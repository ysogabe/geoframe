//! 旅行ブログの保存と書き出し（P3 / T3.6）: E15 `POST /api/blog/save` と E16 `POST /api/blog/export`。現行
//! `app/geoframe/travel_blog.py` の `save_blog_draft`・`export_blog` と、その部品（`_validate_manifest`・
//! `_photo_source`・`_export_image`・`_render_markdown`・`_render_html`）を同じ挙動で移したもの（検査は
//! [`crate::blog::validate_manifest`]）。棚卸し S10。
//!
//! # 一次データ
//!
//! 下書き（`DATA_DIR/travel_blog/drafts/<trip_id>.json`）は**利用者が書いた本文で、他から作り直せない**。
//! 書き出し（`DATA_DIR/blog_exports/<trip_id>/`）は下書きと写真から作り直せる派生物（棚卸し §2.3b）。
//! どちらも「失敗しても既存のものを壊さない・消さない」ように、一時ファイルや作業領域に作ってから
//! `rename` で置き換える。
//!
//! # 書けるとき
//!
//! 保存・書き出しは `catalog.db` に書かないが、**writer（取り込みが有効なときだけある）が無いときは書かずに 503**
//! （T3.1 の更新 API と同じ「書けない」扱い。P5 の切替まで本番の書き手は Python で、`GEOFRAME_INGEST=0` の Rust が
//! Python と同じ下書きを書かないため）。写真の行は writer の接続で読む（読み取りだけ。書かない）。`dry_run` は
//! 何も書かず DB も読まないので、writer が無くても答える。
//!
//! # E15 保存
//!
//! `trip_id` → 旅程の存在（404）→ `manifest` の検査（400）→ `published` を `draft` に落とす → 書く。
//! `travel_blog/`・`drafts/` は作り（途中がリンクなら使わない）、`.part` を排他的に新規作成して書き、`fsync`
//! してから `rename`（[`imaging::write_atomic`]）。中身は Python の `json.dumps(ensure_ascii=False, indent=2)` +
//! 改行と同じバイト列（[`pyval::dumps_indent2`]）。
//!
//! # E16 書き出し
//!
//! 検査は保存と同じ。`dry_run` なら枚数だけ返す。それ以外は**プロセスで 1 本だけ**（実行中なら 409）。
//! `blog_exports/.<trip_id>-rs<乱数>/`（作業領域。モード 0700）に `images/photo-<id>.jpg`・`blog-manifest.json`・
//! `index.md`・`index.html` を作ってから、`.<trip_id>.previous` を経由して `blog_exports/<trip_id>/` に置き換える。
//! 途中で失敗しても、前の書き出しは残る（置き換えの 2 回の `rename` の間に失敗したら元に戻す）。
//! 作業領域の残骸（強制終了・電源断で残ったもの）は、次の書き出しの前と起動時（取り込みが有効なとき）に消し、
//! `destination` が無くて `.previous` だけがあれば `.previous` を戻す（Python は `.previous` を消してしまう）。
//!
//! 画像の元は**行の `display`**（derivative か、ブラウザで見られるローカル原本）。`DATA_DIR` の中の通常の
//! ファイルだけを読む。読むのは derivative の回転・取得と同じロック（[`imaging::lock_derivatives`]）の中で、
//! 行を引いてからファイルをメモリに読み切るまで（デコード・縮小・エンコードはロックの外）。
//!
//! # ログ（AC7）
//!
//! 旅程・下書きの中身、`trip_id`、写真の id・ファイル名、パスを出さない。失敗の詳細（定型の語・`ErrorKind`）は
//! [`ApiError::Internal`] に入れ、`post.rs` が定型の行でログにだけ出す。

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::OptionalExtension;
use rusqlite::types::Value as Sql;
use serde_json::{Map, Value, json};

use crate::blog::{self, BlogEnv, BlogError};
use crate::imaging::{self, ImageError};
use crate::post::ApiError;
use crate::pyval::{dumps_indent2, html_escape, is_py_space, truthy_opt};
use crate::writer::Writer;

/// 書き出しの HTML の雛形（現行 `_render_html` の文字列を `{}` の畳みをほどいて写し、差し込み位置を `@@名前@@` にしたもの）。
const HTML_TEMPLATE: &str = include_str!("blog_export.html");

const BUSY: &str = "他のエクスポート処理が実行中です。しばらくしてから再試行してください";

/// SIGTERM のあと、書き出しを画像の切れ目で止める合図（[`request_stop`]）。
static STOPPING: AtomicBool = AtomicBool::new(false);

/// 書き出しは**プロセスで 1 本だけ**（同じ作業領域・`.previous` を奪い合わないため）。
static EXPORT_LOCK: Mutex<()> = Mutex::new(());

/// 試験用の観測: 書き出し（E16）の本処理が今走っているか（ロックを持っているか）。結合試験が、書き出しの処理が
/// 終わったことを、実時間を待たずに合図で確かめるのに使う。本番の処理は使わない。
#[doc(hidden)]
pub fn export_running() -> bool {
    matches!(
        EXPORT_LOCK.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    )
}

/// 停止の合図を出す。実行中の書き出しは、次の画像の前に作業領域を片付けて止まる（前の書き出しは残る）。
pub fn request_stop() {
    STOPPING.store(true, Ordering::SeqCst);
}

/// 書き出しの途中経過（試験が割り込むための印。本番は何もしない）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// ロックを取り、作業領域を作った。
    Staged,
    /// `n` 枚目（0 始まり）の画像を処理する前。
    Image(usize),
    /// 作業領域に全部揃った。置き換えの前。
    Built,
    /// 前の書き出しを `.previous` に寄せた。新しいものを置く前。
    BackedUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    Go,
    /// 作業領域を片付けて止める。
    Stop,
}

fn production_probe(step: Step) -> Flow {
    if matches!(step, Step::Image(_)) && STOPPING.load(Ordering::SeqCst) {
        Flow::Stop
    } else {
        Flow::Go
    }
}

fn from_blog(e: BlogError) -> ApiError {
    match e {
        BlogError::BadRequest(m) => ApiError::BadRequest(m),
        BlogError::NotFound(m) => ApiError::NotFound(m),
        BlogError::Unavailable(reason) => ApiError::Internal(reason),
        BlogError::Internal => ApiError::Internal("旅行ブログの処理が異常終了しました".into()),
    }
}

fn io_err(what: &str, e: &std::io::Error) -> ApiError {
    ApiError::Internal(format!("{what}（{}）", e.kind()))
}

fn image_err(what: &str, e: &ImageError) -> ApiError {
    ApiError::Internal(format!("{what}: {e}"))
}

/// `save_blog_draft` / `export_blog` の入口の検査: `trip_id`（400）→ 旅程（404・400）→ `manifest`（400）。
fn checked(payload: &Map<String, Value>, env: &BlogEnv) -> Result<(String, Value), ApiError> {
    let trip_id = blog::trip_id_of(payload.get("trip_id"))
        .ok_or_else(|| ApiError::BadRequest("trip_idが不正です".into()))?;
    blog::load_plan(env, &trip_id).map_err(from_blog)?;
    let manifest = blog::validate_manifest(
        payload.get("manifest").unwrap_or(&Value::Null),
        Some(&trip_id),
    )
    .map_err(from_blog)?;
    Ok((trip_id, manifest))
}

/// `dir` が通常のディレクトリであること（リンクは使わない）を確かめ、無ければ作る。
fn ensure_real_dir(dir: &Path) -> Result<(), ApiError> {
    for attempt in 0..2 {
        match fs::symlink_metadata(dir) {
            Ok(meta) if meta.is_dir() => return Ok(()),
            Ok(_) => {
                return Err(ApiError::Internal(
                    "保存先がシンボリックリンクなど通常のディレクトリではありません".into(),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt == 0 => {
                match fs::DirBuilder::new().create(dir) {
                    Ok(()) => {}
                    // 同時に別の保存が作った
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(io_err("保存先を作れません", &e)),
                }
            }
            Err(e) => return Err(io_err("保存先を調べられません", &e)),
        }
    }
    Err(ApiError::Internal("保存先を作れません".into()))
}

/// 下書きの JSON ファイルの中身（Python の `json.dumps(manifest, ensure_ascii=False, indent=2) + "\n"`）。
fn draft_bytes(manifest: &Value) -> Vec<u8> {
    let mut bytes = dumps_indent2(manifest);
    bytes.push(b'\n');
    bytes
}

/// E15 `POST /api/blog/save`。
pub fn save(
    payload: &Map<String, Value>,
    env: &BlogEnv,
    data_root: &Path,
    writer: Option<&Writer>,
) -> Result<Value, ApiError> {
    let (trip_id, mut manifest) = checked(payload, env)?;
    // 保存では `published` を `draft` に落とす（公開の判断は人が書き出しのあとにする）
    if manifest.get("status").and_then(Value::as_str) == Some("published") {
        manifest["status"] = Value::from("draft");
    }
    crate::update::writer_or_unavailable(writer)?;

    ensure_real_dir(&data_root.join("travel_blog"))?;
    ensure_real_dir(&env.draft_dir)?;
    let path = env.draft_dir.join(format!("{trip_id}.json"));
    imaging::write_atomic(&path, &draft_bytes(&manifest))
        .map_err(|e| image_err("下書きを保存できません", &e))?;
    if let Ok(dir) = fs::File::open(&env.draft_dir) {
        // ディレクトリの記録も確定する（電源断で `rename` が消えないように）。失敗しても保存は済んでいる
        let _ = dir.sync_all();
    }
    Ok(json!({
        "ok": true,
        "trip_id": trip_id,
        "saved": format!("travel_blog/drafts/{trip_id}.json"),
    }))
}

// ---- E16 書き出し ---------------------------------------------------------------------------

/// 写真の id（`isdigit` が真の文字列）を行の `id` にする（Python の `int()`）。Unicode の十進の数字も読み、
/// `i64` に収まらないもの、上付き文字など `int()` が読めないもの（Python は `ValueError`・`OverflowError` で
/// 500）は `None`＝実在しない行と同じ扱い（400）。
fn photo_row_id(photo_id: &str) -> Option<i64> {
    photo_id.chars().try_fold(0i64, |acc, c| {
        let digit = crate::pyquery::decimal_digit(c)?;
        acc.checked_mul(10)?.checked_add(i64::from(digit))
    })
}

/// 現行 `_photo_source` + 画像ファイルの読み込み。行を引いて、`display` が指す `DATA_DIR` の中の通常の
/// ファイルをメモリに読み切るまでを、derivative の回転・取得と同じロックの中で行う（読む途中に回転が
/// `display` を差し替えて古いファイルを消しても、行とファイルの組が食い違わない）。
fn read_source(
    writer: &Writer,
    data_root: &Path,
    photo_id: &str,
    row_id: Option<i64>,
) -> Result<Vec<u8>, ApiError> {
    let no_display = || ApiError::BadRequest(format!("写真#{photo_id}の表示用画像がありません"));
    let no_file = || ApiError::BadRequest(format!("写真#{photo_id}のファイルを確認できません"));
    let Some(row_id) = row_id else {
        return Err(no_display());
    };
    let _files = imaging::lock_derivatives();
    let display: Option<Sql> = writer
        .call(move |conn| {
            conn.query_row(
                "SELECT display FROM photos WHERE id=?1 AND COALESCE(hidden,0)=0",
                [row_id],
                |r| r.get::<_, Sql>(0),
            )
            .optional()
        })
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let display = match display {
        Some(Sql::Text(text)) if !text.is_empty() => text,
        _ => return Err(no_display()),
    };
    // 現行は `(DATA_DIR / display).resolve()`。絶対パスの `display` は `DATA_DIR` の外になる。リンクを辿った
    // 先が `DATA_DIR` の中の通常のファイルでなければ読まない
    let path = fs::canonicalize(data_root.join(&display)).map_err(|_| no_file())?;
    if !path.starts_with(data_root) {
        return Err(no_file());
    }
    match fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        _ => return Err(no_file()),
    }
    imaging::read_limited(&path).map_err(|e| image_err("写真を読み込めません", &e))
}

/// 作業領域。成功するまで（[`Staging::disarm`]）、落とされたら中身ごと消す。
struct Staging {
    path: PathBuf,
    armed: bool,
}

impl Staging {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_entry(&self.path);
        }
    }
}

/// ディレクトリなら中身ごと、それ以外（ファイル・リンク）はそのものを消す。リンクは辿らない。無ければ成功。
fn remove_entry(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// 作業領域の名前: `.<trip_id>-rs<16 桁の 16 進>`（Python の `mkdtemp` の `.<trip_id>-<8 文字>` とは別の形。
/// Python が同じ置き場で動いていても、その作業領域を残骸と取り違えない）。
fn staging_name(trip_id: &str) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut random = [0u8; 8];
    let _ = SystemRandom::new().fill(&mut random);
    format!(".{trip_id}-rs{:016x}", u64::from_be_bytes(random))
}

/// `name` が [`staging_name`] の形か（`.<有効な trip_id>-rs<16 桁の 16 進>`）。
fn is_staging_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some(split) = rest.len().checked_sub(3 + 16) else {
        return false;
    };
    if !rest.is_char_boundary(split) {
        return false;
    }
    let (id, tail) = rest.split_at(split);
    let Some(hex) = tail.strip_prefix("-rs") else {
        return false;
    };
    blog::is_valid_trip_id(id) && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `.<trip_id>.previous` の `trip_id`。
fn previous_trip_id(name: &str) -> Option<&str> {
    let id = name.strip_prefix('.')?.strip_suffix(".previous")?;
    blog::is_valid_trip_id(id).then_some(id)
}

/// 掃除してよいディレクトリか。リンクを辿らずに見て、通常のディレクトリだけ true。無ければ false（黙って飛ばす）。
/// リンク・ファイルなど通常のディレクトリでないものは、飛ばした旨を定型の文でログに出す（パスは出さない）。
fn is_sweepable_dir(dir: &Path) -> bool {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        _ => {
            tracing::warn!(
                "[blog] 書き出し・下書きの置き場の一部がリンクなどで通常のディレクトリではないので、掃除を飛ばしました"
            );
            false
        }
    }
}

/// 置き場の残骸の片付け。`export_dir` が通常のディレクトリのときだけ。
/// - 自分の形の作業領域（通常のディレクトリ）を消す。
/// - `.<trip_id>.previous` は、`<trip_id>/` が無ければ戻し（置き換えの途中で止まった）、あれば消す。
///
/// 戻り値は（消した作業領域の数、戻した・消した `.previous` の数）。名前はログに出さない。
fn sweep_exports(export_dir: &Path) -> (usize, usize) {
    if !is_sweepable_dir(export_dir) {
        return (0, 0);
    }
    let Ok(read) = fs::read_dir(export_dir) else {
        return (0, 0);
    };
    let (mut staged, mut previous) = (0, 0);
    for entry in read.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = entry.path();
        if is_staging_name(&name) {
            if entry.file_type().is_ok_and(|t| t.is_dir()) && fs::remove_dir_all(&path).is_ok() {
                staged += 1;
            }
        } else if let Some(id) = previous_trip_id(&name) {
            let destination = export_dir.join(id);
            let done = if exists(&destination) {
                remove_entry(&path).is_ok()
            } else {
                fs::rename(&path, &destination).is_ok()
            };
            if done {
                previous += 1;
            }
        }
    }
    (staged, previous)
}

/// 下書きの書きかけ（`<trip_id>.json.<pid>.<16 桁>.part`）を消す。戻り値は消した数。
///
/// `read_dir` はリンクを辿るので、`travel_blog/` か `drafts/` が外へのリンクだと、列挙の前に確かめないと
/// リンク先の別の場所の通常ファイルを消してしまう。**両方を** `symlink_metadata` で（リンクを辿らずに）見て、
/// 通常のディレクトリでなければ何もしない（`None`。無いときも `None`＝消すものが無い）。
fn sweep_draft_parts(data_root: &Path) -> Option<usize> {
    let travel_blog = data_root.join("travel_blog");
    let draft_dir = travel_blog.join("drafts");
    if !is_sweepable_dir(&travel_blog) || !is_sweepable_dir(&draft_dir) {
        return None;
    }
    let read = fs::read_dir(&draft_dir).ok()?;
    let mut removed = 0;
    for entry in read.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let is_part = name.ends_with(".part")
            && name
                .split_once(".json.")
                .is_some_and(|(id, _)| blog::is_valid_trip_id(id));
        if is_part
            && entry.file_type().is_ok_and(|t| t.is_file())
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    Some(removed)
}

/// 起動時の掃除（取り込みが有効なとき。`.ingest.lock` を取ったあと）。強制終了・電源断で残った書き出しの作業領域と
/// 下書きの書きかけを片付け、置き換えの途中で止まった書き出しを元に戻す。件数だけをログに出す。
/// 掃除の対象の親（`blog_exports/`・`travel_blog/`・`drafts/`）がリンクなどで通常のディレクトリでなければ、
/// その中は掃除しない（リンク先の別の場所のファイルを消さないため）。
pub fn sweep_leftovers(data_root: &Path) {
    let _export = EXPORT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (staged, previous) = sweep_exports(&data_root.join("blog_exports"));
    let parts = sweep_draft_parts(data_root).unwrap_or(0);
    if staged + previous + parts > 0 {
        tracing::info!(
            "[blog] 書き出しの作業領域{staged}件・置き換え途中の書き出し{previous}件・下書きの書きかけ{parts}件を片付けました"
        );
    }
}

/// `fsync` できなくても書き出しは済んでいるので、失敗は無視する。
fn sync_dir(dir: &Path) {
    if let Ok(file) = fs::File::open(dir) {
        let _ = file.sync_all();
    }
}

fn json_str<'a>(obj: &'a Map<String, Value>, key: &str) -> &'a str {
    obj.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `map[key]` が配列なら、そのうちオブジェクトの要素（検査済みの下書きでは全部がオブジェクト）。
fn items<'a>(map: &'a Map<String, Value>, key: &str) -> Vec<&'a Map<String, Value>> {
    map.get(key)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

/// 現行 `_render_markdown`。
fn render_markdown(manifest: &Map<String, Value>) -> String {
    let mut lines: Vec<String> = vec![format!("# {}", json_str(manifest, "title")), String::new()];
    for key in ["subtitle", "summary"] {
        let text = json_str(manifest, key);
        if !text.is_empty() {
            lines.push(text.to_string());
            lines.push(String::new());
        }
    }
    for chapter in items(manifest, "chapters") {
        lines.push(format!(
            "## {} {}",
            json_str(chapter, "date"),
            json_str(chapter, "title")
        ));
        lines.push(String::new());
        lines.push(json_str(chapter, "body").to_string());
        lines.push(String::new());
        for image in items(chapter, "images") {
            lines.push(format!(
                "![{}]({})",
                json_str(image, "alt"),
                json_str(image, "src")
            ));
            lines.push(String::new());
            let caption = json_str(image, "caption");
            if !caption.is_empty() {
                lines.push(format!("_{caption}_"));
                lines.push(String::new());
            }
        }
    }
    let joined = lines.join("\n");
    format!("{}\n", joined.trim_end_matches(is_py_space))
}

/// 現行 `_paragraphs`: 改行で分け、空白だけの行を捨て、`<p>` で包む。
fn paragraphs(text: &str) -> String {
    text.split('\n')
        .filter(|p| !p.chars().all(is_py_space))
        .map(|p| format!("<p>{}</p>", html_escape(p)))
        .collect()
}

/// 雛形の `@@名前@@` を、`values` の値で 1 回の走査で置き換える（差し込んだ文字列の中の `@@…@@` は見ない）。
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 1024);
    let mut rest = template;
    while let Some(start) = rest.find("@@") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("@@") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match values.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => {
                out.push_str(v);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("@@");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 現行 `_render_html`。
fn render_html(manifest: &Map<String, Value>) -> String {
    let mut chapters = String::new();
    for chapter in items(manifest, "chapters") {
        let figures: String = items(chapter, "images")
            .into_iter()
            .map(|image| {
                format!(
                    "<figure><img loading=\"lazy\" src=\"{}\" alt=\"{}\"><figcaption>{}</figcaption></figure>",
                    html_escape(json_str(image, "src")),
                    html_escape(json_str(image, "alt")),
                    html_escape(json_str(image, "caption")),
                )
            })
            .collect();
        chapters.push_str(&format!(
            "<article><div class=\"eyebrow\">{} · {}</div><h2>{}</h2>{}<div class=\"gallery\">{figures}</div></article>",
            html_escape(json_str(chapter, "date")),
            html_escape(json_str(chapter, "location_label")),
            html_escape(json_str(chapter, "title")),
            paragraphs(json_str(chapter, "body")),
        ));
    }
    let title = html_escape(json_str(manifest, "title"));
    let subtitle = html_escape(json_str(manifest, "subtitle"));
    let summary = paragraphs(json_str(manifest, "summary"));
    fill(
        HTML_TEMPLATE,
        &[
            ("TITLE", &title),
            ("SUBTITLE", &subtitle),
            ("SUMMARY", &summary),
            ("CHAPTERS", &chapters),
        ],
    )
}

/// 作業領域に書き出す一式を作る。`probe` は試験用の割り込み口。
fn build(
    staging: &Path,
    manifest: &Value,
    writer: &Writer,
    data_root: &Path,
    probe: &dyn Fn(Step) -> Flow,
) -> Result<(), ApiError> {
    let image_dir = staging.join("images");
    fs::create_dir(&image_dir).map_err(|e| io_err("作業領域を作れません", &e))?;
    let mut exported = manifest.clone();
    let mut done: HashSet<String> = HashSet::new();
    let mut index = 0usize;
    let Some(Value::Array(chapters)) = exported.get_mut("chapters") else {
        return Err(ApiError::Internal("chaptersがありません".into()));
    };
    for chapter in chapters {
        let Some(Value::Array(images)) = chapter.get_mut("images") else {
            continue;
        };
        for image in images {
            if probe(Step::Image(index)) == Flow::Stop {
                return Err(ApiError::Internal(
                    "停止の合図を受けたので書き出しを中断しました".into(),
                ));
            }
            index += 1;
            let photo_id = image
                .get("photo_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let row_id = photo_row_id(&photo_id);
            // 現行はファイル名を `int(photo_id)` で作る（`007` と `7` は同じ名前で、2 回目は作り直さない）
            let name = match row_id {
                Some(id) => format!("photo-{id}.jpg"),
                None => format!("photo-{photo_id}.jpg"),
            };
            if !done.contains(&name) {
                let bytes = read_source(writer, data_root, &photo_id, row_id)?;
                let rendered = imaging::render_export(&bytes)
                    .map_err(|e| image_err("写真を書き出せません", &e))?;
                drop(bytes);
                imaging::write_atomic(&image_dir.join(&name), &rendered.jpeg)
                    .map_err(|e| image_err("写真を保存できません", &e))?;
                done.insert(name.clone());
            }
            image["src"] = Value::from(format!("images/{name}"));
        }
    }
    exported["status"] = Value::from("reviewed");
    let write = |file: &str, bytes: &[u8]| {
        imaging::write_atomic(&staging.join(file), bytes)
            .map_err(|e| image_err("書き出しのファイルを保存できません", &e))
    };
    write("blog-manifest.json", &draft_bytes(&exported))?;
    let top = exported.as_object().cloned().unwrap_or_default();
    write("index.md", render_markdown(&top).as_bytes())?;
    write("index.html", render_html(&top).as_bytes())?;
    sync_dir(&image_dir);
    sync_dir(staging);
    Ok(())
}

/// 作業領域を `destination` に置き換える。前のものは `backup`（`.<trip_id>.previous`）に寄せ、置き換えに
/// 失敗したら元に戻す。
fn swap(
    export_dir: &Path,
    staging: &Path,
    destination: &Path,
    backup: &Path,
    probe: &dyn Fn(Step) -> Flow,
) -> Result<(), ApiError> {
    let had_previous = exists(destination);
    if had_previous {
        fs::rename(destination, backup).map_err(|e| io_err("前の書き出しを退避できません", &e))?;
    }
    let result = if probe(Step::BackedUp) == Flow::Stop {
        Err(ApiError::Internal(
            "停止の合図を受けたので書き出しを中断しました".into(),
        ))
    } else {
        fs::rename(staging, destination).map_err(|e| io_err("書き出しを置き換えられません", &e))
    };
    if let Err(e) = result {
        if had_previous {
            // 置けなかった: 前の書き出しを元に戻す
            let _ = fs::rename(backup, destination);
        }
        return Err(e);
    }
    sync_dir(export_dir);
    if had_previous && remove_entry(backup).is_err() {
        // 置き換えは済んでいる。次の書き出し・起動時の掃除が消す
        tracing::warn!("[blog] 前の書き出しの退避先を消せませんでした");
    }
    Ok(())
}

/// E16 `POST /api/blog/export`。
pub fn export(
    payload: &Map<String, Value>,
    env: &BlogEnv,
    data_root: &Path,
    writer: Option<&Writer>,
) -> Result<Value, ApiError> {
    export_with(payload, env, data_root, writer, &production_probe)
}

pub(crate) fn export_with(
    payload: &Map<String, Value>,
    env: &BlogEnv,
    data_root: &Path,
    writer: Option<&Writer>,
    probe: &dyn Fn(Step) -> Flow,
) -> Result<Value, ApiError> {
    let (trip_id, manifest) = checked(payload, env)?;
    let selected: usize = manifest
        .as_object()
        .map(|m| items(m, "chapters"))
        .unwrap_or_default()
        .iter()
        .map(|c| {
            c.get("images")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum();
    if truthy_opt(payload.get("dry_run")) {
        return Ok(json!({"ok": true, "dry_run": true, "trip_id": trip_id, "images": selected}));
    }
    let writer = crate::update::writer_or_unavailable(writer)?;

    // ファイルを伴う本処理はプロセスで 1 本だけ（dry_run はここより前に返すので待たされない）
    let _running = match EXPORT_LOCK.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return Err(ApiError::Busy(BUSY.into())),
    };
    let export_dir = &env.export_dir;
    ensure_real_dir(export_dir)?;
    sweep_exports(export_dir);

    let staging_path = export_dir.join(staging_name(&trip_id));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging_path)
        .map_err(|e| io_err("作業領域を作れません", &e))?;
    let mut staging = Staging {
        path: staging_path,
        armed: true,
    };
    probe(Step::Staged);
    build(&staging.path, &manifest, writer, data_root, probe)?;
    probe(Step::Built);

    let destination = export_dir.join(&trip_id);
    let backup = export_dir.join(format!(".{trip_id}.previous"));
    swap(export_dir, &staging.path, &destination, &backup, probe)?;
    staging.disarm();
    Ok(json!({
        "ok": true,
        "trip_id": trip_id,
        "images": selected,
        "path": format!("blog_exports/{trip_id}"),
        "url": format!("/blog_exports/{trip_id}/index.html"),
    }))
}

#[cfg(test)]
mod tests;
