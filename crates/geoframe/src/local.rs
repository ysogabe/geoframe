//! ローカル写真の索引（P2 / T2.3）。現行 `drive.py` の `index_local`・`local_derivatives`・
//! `organize_photos` を、同期サイクルの段（[`crate::sync::Stage`]）と `geoframe organize` にしたもの。
//! 詳細は `docs/rust-migration/server.md` §13。
//!
//! # `index_local`（[`IndexLocal`]）
//!
//! `PHOTOS_DIR`（既定 `DATA_DIR/photos`）を走査し、`photos`（`source='local'`）を追加・更新・削除する。
//! 規則は現行と同じ:
//!
//! - 対象は拡張子が [`crate::imaging::EXTS`] のファイル。**パスの要素のどれかが `gdrive`** なら除外する
//!   （旧ミラー先は Drive の行と重複するため）。
//! - **走査順は「パスの要素の列」の昇順**（Python の `sorted(rglob("*"))` と同じ。`a/b` は `a-b` より前）。
//!   新しい行の `id` はこの順に振られる。
//! - 行が無ければ追加（`loc_source` は GPS があれば `exif`、無ければ NULL）。`mtime` が 1 秒以上違えば
//!   更新（`mtime taken_at lat lon loc_source` を上書きし `place elev display` を NULL に戻す。**手動で
//!   直した位置も上書きされる**。inventory.md §2.3）。そうでなければ触らない。
//! - 走査で見つからなかった `source='local'` の行は削除する。`display` が `derivatives/` で始まるときだけ
//!   そのファイルも消す（`photos/` の原本は消さない）。
//! - 画像として読めない写真は飛ばす（行を作らない・更新しない）。
//!
//! 現行との違い（安全側。§0）: シンボリックリンクのディレクトリには降りない（現行の `rglob` も降りない）。
//! ファイルのリンクは、実体が `DATA_DIR` の中にあるときだけ取り込む（現行は外のファイルも読む）。
//! 全ての書き込みは 1 つのトランザクションにまとめる（現行は 1 文ごとのコミット）。ファイルの読み取り
//! （EXIF）は書き込み接続の外で先に済ませる。DB の行を消す前にファイルを消さない。
//!
//! # `local_derivatives`（[`LocalDerivatives`]）
//!
//! 位置があり `display` が未設定のローカル写真に、1 サイクル `GEOFRAME_FETCH_PER_CYCLE` 枚まで表示用の
//! ファイルを用意する。JPEG・PNG は原本をそのまま `display`（= `path`）にし、それ以外（HEIC・TIFF）は
//! `derivatives/local_<md5(path)[:16]>.jpg` を作る。
//!
//! # `geoframe organize`（[`organize`]）
//!
//! 位置の無いローカル写真を `DATA_DIR/_excluded/no_gps/` へ移し、行を削除する。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use rusqlite::{Connection, params};

use crate::datadir::{DataDir, PathError};
use crate::imaging::{self, IccOutcome};
use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

/// 更新とみなす `mtime` の差（秒）。現行の `abs(row.mtime - mtime) < 1` で据え置く。
const MTIME_TOLERANCE_S: f64 = 1.0;

/// `local_derivatives` の名前: `local_<md5(path)[:16]>.jpg`。`path` は DB の `path` 列の文字列そのもの
/// （`DATA_DIR` からの相対、`/` 区切り）の UTF-8 バイト列で、現行の `hashlib.md5(path.encode())` と同じ。
/// 暗号の用途ではなく、名前の互換のためだけ。
pub fn local_derivative_name(path: &str) -> String {
    let digest = Md5::digest(path.as_bytes());
    let mut name = String::from("local_");
    for byte in &digest[..8] {
        name.push_str(&format!("{byte:02x}"));
    }
    name.push_str(".jpg");
    name
}

/// 現行の `p.stat().st_mtime`（`float`）と同じ値。CPython は `sec + nsec * 1e-9`（`double`）で作る。
fn mtime_seconds(meta: &fs::Metadata) -> f64 {
    meta.mtime() as f64 + meta.mtime_nsec() as f64 * 1e-9
}

impl From<PathError> for StageError {
    fn from(e: PathError) -> Self {
        // `PathError` の文面はパスを含まない
        StageError::text(format!("パスの検査に失敗しました: {e}"))
    }
}

// ============================================================
// 走査
// ============================================================

/// 走査で見つけた取り込み対象のファイル。
struct Found {
    /// `PHOTOS_DIR` からの相対パスの要素（並べ替えのキー。`gdrive` の判定もこれ）。
    parts: Vec<String>,
    /// ファイルの絶対パス（`PHOTOS_DIR` を `canonicalize` したものの下。最後の要素のリンクは辿っていない）。
    abs: PathBuf,
    /// `DATA_DIR` からの相対パス（`/` 区切り。DB の `path` 列に入れる）。
    rel: String,
    name: String,
}

#[derive(Default)]
struct Scan {
    found: Vec<Found>,
    /// 開けなかったサブディレクトリ。
    unreadable_dirs: u64,
    /// 名前が UTF-8 でない（DB に入れられない）エントリ。
    non_utf8: u64,
    /// 実体が `DATA_DIR` の外にあるファイルのリンク。
    unsafe_links: u64,
}

impl Scan {
    fn skipped(&self) -> u64 {
        self.non_utf8 + self.unsafe_links
    }
}

/// 写真の置き場を解決する。`DATA_DIR` の中の実在のディレクトリ（`canonicalize` 済み）。
///
/// 現行の `PHOTOS_DIR.mkdir(exist_ok=True)` と `.resolve()`。dry-run では作らず、無ければ `None`
/// （写真は 0 枚として数える）。`DATA_DIR` そのもの・外を指すときは断る: 前者は `derivatives/` や
/// `tmp/`（EXIF 付きの原本の一時置き場）まで索引してしまい、後者は現行が `relative_to` で落ちる。
fn photos_root(ctx: &StageCtx<'_>, data: &DataDir) -> Result<Option<PathBuf>, StageError> {
    let configured = ctx.config.photos_dir();
    if !ctx.is_dry_run() {
        match fs::create_dir(&configured) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let real = match fs::canonicalize(&configured) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound && ctx.is_dry_run() => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if real == data.root() || !real.starts_with(data.root()) {
        return Err(StageError::fixed(
            "GEOFRAME_PHOTOS_DIR が DATA_DIR の中のサブディレクトリではありません",
        ));
    }
    Ok(Some(real))
}

/// `photos` の下を走査して、取り込み対象のファイルを集める（パスの要素の列の昇順）。
fn scan(data: &DataDir, photos: &Path) -> io::Result<Scan> {
    let mut out = Scan::default();
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(photos.to_path_buf(), Vec::new())];
    while let Some((dir, parts)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            // 一番上が読めないのは失敗。下の階層は飛ばして数える（現行の `rglob` も権限エラーを無視する）
            Err(e) if parts.is_empty() => return Err(e),
            Err(_) => {
                out.unreadable_dirs += 1;
                continue;
            }
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(name) = entry.file_name().into_string() else {
                out.non_utf8 += 1;
                continue;
            };
            // パスの要素のどれかが `gdrive`（ディレクトリにもファイルにも効く。現行の `.parts` の判定）
            if name == "gdrive" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let mut child = parts.clone();
            child.push(name.clone());
            if file_type.is_dir() {
                // リンクのディレクトリは `is_dir()` が偽（`file_type` はリンクを辿らない）なので降りない
                stack.push((path, child));
                continue;
            }
            if !imaging::is_supported_ext(&path) {
                continue;
            }
            let is_file = if file_type.is_file() {
                true
            } else if file_type.is_symlink() {
                // 現行の `is_file()` はリンクを辿る。実体が置き場の外なら読まない
                match (fs::metadata(&path), fs::canonicalize(&path)) {
                    (Ok(meta), Ok(real)) if meta.is_file() => {
                        if !real.starts_with(data.root()) {
                            out.unsafe_links += 1;
                            continue;
                        }
                        true
                    }
                    _ => false, // 切れたリンク・ディレクトリを指すリンク
                }
            } else {
                false
            };
            if !is_file {
                continue;
            }
            let Some(rel) = data.relative(&path) else {
                out.non_utf8 += 1;
                continue;
            };
            out.found.push(Found {
                parts: child,
                abs: path,
                rel,
                name,
            });
        }
    }
    out.found.sort_by(|a, b| a.parts.cmp(&b.parts));
    Ok(out)
}

// ============================================================
// index_local
// ============================================================

/// 同期サイクルの段 `index_local`。
pub struct IndexLocal;

/// DB の既存の行（`path` を持つもの、または `source='local'`）。
struct ExistingRow {
    id: i64,
    source: String,
    mtime: Option<f64>,
    display: Option<String>,
}

/// 書き込む 1 行分（追加・更新に共通）。
#[derive(Clone)]
struct Entry {
    rel: String,
    name: String,
    mtime: f64,
    taken_at: String,
    lat: Option<f64>,
    lon: Option<f64>,
    loc_source: Option<&'static str>,
}

enum Op {
    Insert(Entry),
    Update(Entry),
}

impl Op {
    fn clone_for_write(&self) -> Op {
        match self {
            Op::Insert(e) => Op::Insert(e.clone()),
            Op::Update(e) => Op::Update(e.clone()),
        }
    }
}

/// 計画の結果（書き込む前）。
#[derive(Default)]
struct Plan {
    ops: Vec<Op>,
    /// 消す行 `(id, display)`。
    deletes: Vec<(i64, Option<String>)>,
    files: u64,
    unreadable: u64,
}

impl Plan {
    fn added(&self) -> u64 {
        self.ops
            .iter()
            .filter(|op| matches!(op, Op::Insert(_)))
            .count() as u64
    }
    fn updated(&self) -> u64 {
        self.ops
            .iter()
            .filter(|op| matches!(op, Op::Update(_)))
            .count() as u64
    }
}

fn load_existing(
    conn: &Connection,
) -> rusqlite::Result<(HashMap<String, ExistingRow>, Vec<ExistingRow>)> {
    let mut by_path = HashMap::new();
    let mut local_without_path = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, source, path, mtime, display FROM photos
         WHERE path IS NOT NULL OR source = 'local'",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let existing = ExistingRow {
            id: row.get(0)?,
            source: row.get(1)?,
            // 型が想定外（文字列など）の mtime は「変わった」とみなす（現行は例外で落ちる）
            mtime: row.get::<_, Option<f64>>(3).unwrap_or(None),
            display: row.get(4)?,
        };
        match row.get::<_, Option<String>>(2)? {
            Some(path) => {
                by_path.insert(path, existing);
            }
            None => local_without_path.push(existing),
        }
    }
    Ok((by_path, local_without_path))
}

impl IndexLocal {
    /// 計画を作る。ファイルは読むが、DB にもファイルにも書かない。`stop` の合図が来たら `None`
    /// （走査が途中の `seen` で削除を決めてはいけない）。
    fn plan(
        ctx: &StageCtx<'_>,
        data: &DataDir,
        photos: &Path,
    ) -> Result<Option<(Plan, Scan)>, StageError> {
        let scan = scan(data, photos)?;
        let (mut existing, local_without_path) = load_existing(ctx.read)?;
        let mut plan = Plan {
            files: scan.found.len() as u64,
            ..Plan::default()
        };
        let mut seen: HashSet<&str> = HashSet::with_capacity(scan.found.len());
        for found in &scan.found {
            if ctx.stop.is_stopped() {
                return Ok(None);
            }
            seen.insert(&found.rel);
            let Ok(meta) = fs::metadata(&found.abs) else {
                plan.unreadable += 1;
                continue;
            };
            let mtime = mtime_seconds(&meta);
            let known = existing.get(&found.rel);
            if let Some(row) = known
                && row
                    .mtime
                    .is_some_and(|old| (old - mtime).abs() < MTIME_TOLERANCE_S)
            {
                continue;
            }
            // 画像として読めないものは飛ばす（現行の「読み込み失敗」）。EXIF が無いのは失敗ではない
            let Ok(info) = imaging::extract_exif(&found.abs) else {
                plan.unreadable += 1;
                continue;
            };
            let (lat, lon) = match (info.lat, info.lon) {
                (Some(lat), Some(lon)) => (Some(lat), Some(lon)),
                _ => (None, None),
            };
            let entry = Entry {
                rel: found.rel.clone(),
                name: found.name.clone(),
                mtime,
                taken_at: info.taken_at,
                lat,
                lon,
                loc_source: lat.map(|_| "exif"),
            };
            plan.ops.push(if known.is_some() {
                Op::Update(entry)
            } else {
                Op::Insert(entry)
            });
        }
        // 走査で見つからなかった `source='local'` の行（`path` が NULL の行も、現行は消す）
        for (path, row) in existing.drain() {
            if row.source == "local" && !seen.contains(path.as_str()) {
                plan.deletes.push((row.id, row.display));
            }
        }
        for row in local_without_path {
            plan.deletes.push((row.id, row.display));
        }
        plan.deletes.sort_by_key(|(id, _)| *id);
        Ok(Some((plan, scan)))
    }
}

/// 書き込み: 追加・更新・削除を 1 つのトランザクションで。
fn apply_plan(
    writer: &crate::writer::Writer,
    now: String,
    ops: &[Op],
    delete_ids: Vec<i64>,
) -> Result<(), StageError> {
    // `Writer::call` の閉包は `'static`。計画は読み取り専用で使い回す（件数・ログ用）ので、書く分を写す
    let ops: Vec<Op> = ops.iter().map(Op::clone_for_write).collect();
    writer.call(move |conn| {
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO photos(source, path, name, mtime, taken_at, lat, lon, loc_source, indexed_at)
                 VALUES('local', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            let mut update = tx.prepare(
                "UPDATE photos SET mtime=?1, taken_at=?2, lat=?3, lon=?4, loc_source=?5,
                 place=NULL, elev=NULL, display=NULL, indexed_at=?6 WHERE path=?7",
            )?;
            for op in &ops {
                match op {
                    Op::Insert(e) => insert.execute(params![
                        e.rel, e.name, e.mtime, e.taken_at, e.lat, e.lon, e.loc_source, now
                    ])?,
                    Op::Update(e) => update.execute(params![
                        e.mtime, e.taken_at, e.lat, e.lon, e.loc_source, now, e.rel
                    ])?,
                };
            }
            let mut delete = tx.prepare("DELETE FROM photos WHERE id=?1")?;
            for id in &delete_ids {
                delete.execute([id])?;
            }
        }
        tx.commit()
    })?;
    Ok(())
}

/// 消した行の derivative を消す（行を消したあと）。`display` が `derivatives/` で始まるものだけ。
/// 戻り値は `(消した数, 安全でない display で消さなかった数, 消せなかった数)`。
fn remove_derivatives(
    data: &DataDir,
    deletes: &[(i64, Option<String>)],
    dry_run: bool,
) -> (u64, u64, u64) {
    let (mut removed, mut unsafe_display, mut failed) = (0, 0, 0);
    for display in deletes.iter().filter_map(|(_, d)| d.as_deref()) {
        if !display.starts_with("derivatives/") {
            continue; // `photos/` の原本は消さない
        }
        let Ok(path) = data.derivative_file(display) else {
            unsafe_display += 1;
            continue;
        };
        if dry_run {
            // 消すはずの数（無いファイルは数えない）
            if fs::symlink_metadata(&path).is_ok() {
                removed += 1;
            }
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => failed += 1,
        }
    }
    (removed, unsafe_display, failed)
}

/// 段の結果の件数（名前は固定。ログ・`--dry-run` の出力に載る）。
fn index_counts(plan: &Plan, scan: &Scan, removed_files: (u64, u64, u64)) -> Counts {
    Counts(vec![
        ("files", plan.files),
        ("added", plan.added()),
        ("updated", plan.updated()),
        ("removed", plan.deletes.len() as u64),
        ("unreadable", plan.unreadable),
        ("skipped", scan.skipped()),
        ("unreadable_dirs", scan.unreadable_dirs),
        ("derivative_files_removed", removed_files.0),
        ("derivative_files_unsafe", removed_files.1),
        ("derivative_files_failed", removed_files.2),
    ])
}

impl Stage for IndexLocal {
    fn name(&self) -> &'static str {
        "index_local"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        let data = DataDir::open(&ctx.config.data_dir)?;
        let Some(photos) = photos_root(ctx, &data)? else {
            // dry-run で置き場がまだ無い: 写真は 0 枚
            return Ok(StageOutcome::Done(index_counts(
                &Plan::default(),
                &Scan::default(),
                (0, 0, 0),
            )));
        };
        let Some((plan, scan)) = Self::plan(ctx, &data, &photos)? else {
            return Ok(StageOutcome::Skipped("停止の合図で中断しました"));
        };
        let mut files_removed = (0, 0, 0);
        if ctx.is_dry_run() {
            files_removed = remove_derivatives(&data, &plan.deletes, true);
        } else if !plan.ops.is_empty() || !plan.deletes.is_empty() {
            let writer = ctx.writer()?;
            // 現行は `now` を段の先頭で 1 回だけ作り、全行に使う
            let now = crate::sysinfo::local_isoformat();
            let ids = plan.deletes.iter().map(|(id, _)| *id).collect();
            // 行を消して確定してから、そのファイルを消す（先にファイルを消して失敗すると、行だけが残る）
            apply_plan(writer, now, &plan.ops, ids)?;
            files_removed = remove_derivatives(&data, &plan.deletes, false);
        }
        Ok(StageOutcome::Done(index_counts(
            &plan,
            &scan,
            files_removed,
        )))
    }
}

// ============================================================
// local_derivatives
// ============================================================

/// 同期サイクルの段 `local_derivatives`。
pub struct LocalDerivatives;

impl Stage for LocalDerivatives {
    fn name(&self) -> &'static str {
        "local_derivatives"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        // 現行は `ORDER BY` なしの `LIMIT`（実際は rowid 順）。順序を固定する
        let rows: Vec<(i64, String)> = {
            let mut stmt = ctx.read.prepare(
                "SELECT id, path FROM photos
                 WHERE source='local' AND path IS NOT NULL AND lat IS NOT NULL AND display IS NULL
                 ORDER BY id LIMIT ?1",
            )?;
            stmt.query_map([ctx.config.fetch_per_cycle], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?
        };
        let (mut original, mut made, mut failed, mut icc_dropped) = (0u64, 0u64, 0u64, 0u64);
        if rows.is_empty() {
            return Ok(StageOutcome::Done(Counts(vec![
                ("original", 0),
                ("made", 0),
                ("failed", 0),
                ("icc_dropped", 0),
            ])));
        }
        if ctx.is_dry_run() {
            for (_, path) in &rows {
                if imaging::is_browser_displayable_ext(Path::new(path)) {
                    original += 1;
                } else {
                    made += 1;
                }
            }
            return Ok(StageOutcome::Done(Counts(vec![
                ("original", original),
                ("made", made),
            ])));
        }
        let data = DataDir::open(&ctx.config.data_dir)?;
        let writer = ctx.writer()?;
        for (id, path) in rows {
            if ctx.stop.is_stopped() {
                break;
            }
            // DB の `path` は信用しない（`DATA_DIR` の外へ出るものは読まない）
            let Ok(src) = data.resolve_existing(&path) else {
                failed += 1;
                continue;
            };
            let display = if imaging::is_browser_displayable_ext(Path::new(&path)) {
                // ブラウザがそのまま表示できるので原本を使う
                original += 1;
                path
            } else {
                match imaging::store_derivative(&data, &src, &local_derivative_name(&path), 0) {
                    Ok(stored) => {
                        if stored.info.icc == IccOutcome::Dropped {
                            icc_dropped += 1;
                        }
                        made += 1;
                        stored.display
                    }
                    Err(e) => {
                        // `ImageError` の文面はパス・ファイル名を含まない
                        tracing::debug!("[local] derivative 失敗（id={id}）: {e}");
                        failed += 1;
                        continue;
                    }
                }
            };
            let now = crate::sysinfo::local_isoformat();
            writer.call(move |conn| {
                conn.execute(
                    "UPDATE photos SET display=?1, fetched_at=?2 WHERE id=?3",
                    params![display, now, id],
                )
            })?;
        }
        Ok(StageOutcome::Done(Counts(vec![
            ("original", original),
            ("made", made),
            ("failed", failed),
            ("icc_dropped", icc_dropped),
        ])))
    }
}

// ============================================================
// organize
// ============================================================

/// `organize` の移動先（`DATA_DIR` からの相対）。
pub const EXCLUDED_DIR: &str = "_excluded/no_gps";

/// 移動の対象を選ぶ SQL（`lat IS NULL` のローカル写真）。
///
/// 現行の `organize_photos` は `index_local` → **`infer_locations`** → `lat IS NULL` の順で、撮影時刻が
/// 近い位置つきの写真（錨）から位置が付く写真は移さない。Rust の `infer` 段は T2.5 まで何もしないので、
/// そのまま移すと現行なら残る写真まで移してしまう。そこで T2.5 までは**推定の対象になりうる写真を
/// 移さない**: 撮影時刻が空の写真（推定できない）か、錨になりうる写真（`exif`・`drive_meta`・`manual`
/// で時刻つき）が 1 枚も無いときだけ移す。現行の移動対象の部分集合。T2.5 で `infer` 段のあとに
/// この条件を外す。
const CANDIDATES_SQL: &str = "
    SELECT id, path FROM photos
    WHERE source = 'local' AND lat IS NULL
      AND (taken_at IS NULL OR taken_at = ''
           OR NOT EXISTS (SELECT 1 FROM photos a
                          WHERE a.loc_source IN ('exif', 'drive_meta', 'manual')
                            AND a.taken_at IS NOT NULL AND a.taken_at != ''))
    ORDER BY id";

const ALL_UNLOCATED_SQL: &str =
    "SELECT COUNT(*) FROM photos WHERE source = 'local' AND lat IS NULL";

/// 移動先の名前。`name` が既にあれば `<stem> (2).<ext>`, `(3)`… と探す。
fn destination_candidates(name: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    std::iter::once(name.to_string()).chain((2..=1000).map(move |n| format!("{stem} ({n}){ext}")))
}

/// `src` を `dest_dir` の下へ**上書きせずに**移す。成功したら移し先の名前の有無だけ返す。
/// ハードリンクを作ってから元を消す（`link` は既にあるときに失敗するので、同名のファイルを壊さない）。
/// リンクを作れない FS では、無いことを確かめてから `rename` する。
fn move_without_overwrite(src: &Path, dest_dir: &Path, name: &str) -> io::Result<()> {
    for candidate in destination_candidates(name) {
        let dest = dest_dir.join(candidate);
        match fs::hard_link(src, &dest) {
            Ok(()) => return fs::remove_file(src),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // リンクを作れない（FS の都合）: 無いことを確かめてから rename
                if fs::symlink_metadata(&dest).is_ok() {
                    continue;
                }
                return fs::rename(src, &dest);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "移動先の名前が見つかりません",
    ))
}

/// 移動先 `DATA_DIR/_excluded/no_gps` を用意する。階層を 1 つずつ見て、リンクを経由していたら
/// （`DATA_DIR` の外へ出るおそれがあるので）何も作らずに断る。
fn excluded_dir(data: &DataDir) -> Result<PathBuf, StageError> {
    let mut dir = data.root().to_path_buf();
    for part in EXCLUDED_DIR.split('/') {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(StageError::fixed(
                    "移動先の _excluded/no_gps がディレクトリではない（リンクを含む）",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&dir)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(dir)
}

/// `geoframe organize` の本体（`index_local` を済ませたあとで呼ぶ）。位置の無い写真を `_excluded/no_gps/` へ
/// 移して行を削除する。`--dry-run` は件数だけ。
pub fn organize(ctx: &StageCtx<'_>) -> Result<Counts, StageError> {
    let data = DataDir::open(&ctx.config.data_dir)?;
    let candidates: Vec<(i64, String)> = {
        let mut stmt = ctx.read.prepare(CANDIDATES_SQL)?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let unlocated: i64 = ctx
        .read
        .query_row(ALL_UNLOCATED_SQL, [], |row| row.get(0))?;
    let held_back = (unlocated as u64).saturating_sub(candidates.len() as u64);
    if ctx.is_dry_run() || candidates.is_empty() {
        return Ok(Counts(vec![
            ("targets", candidates.len() as u64),
            ("held_back_until_infer", held_back),
        ]));
    }
    let writer = ctx.writer()?;
    let dest_dir = excluded_dir(&data)?;
    let (mut moved, mut gone, mut skipped, mut failed) = (0u64, 0u64, 0u64, 0u64);
    for (id, path) in candidates {
        if ctx.stop.is_stopped() {
            break;
        }
        let entry = match data.entry_path(&path) {
            Ok(entry) => Some(entry),
            Err(PathError::Io(io::ErrorKind::NotFound)) => None,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let meta = entry.as_deref().map(fs::symlink_metadata);
        match meta {
            // ファイルが無い: 現行と同じく行だけ消す
            None => gone += 1,
            Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => gone += 1,
            Some(Err(_)) => {
                failed += 1;
                continue;
            }
            // 通常のファイルだけ移す（リンク・ディレクトリは動かさない）
            Some(Ok(meta)) if meta.is_file() => {
                let name = Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let src = entry.as_deref().unwrap_or(Path::new(""));
                if move_without_overwrite(src, &dest_dir, name).is_err() {
                    failed += 1;
                    continue;
                }
                moved += 1;
            }
            Some(Ok(_)) => {
                skipped += 1;
                continue;
            }
        }
        writer.call(move |conn| conn.execute("DELETE FROM photos WHERE id=?1", [id]))?;
    }
    Ok(Counts(vec![
        ("targets", moved + gone + skipped + failed),
        ("moved", moved),
        ("row_only_removed", gone),
        ("skipped", skipped),
        ("failed", failed),
        ("held_back_until_infer", held_back),
    ]))
}

#[cfg(test)]
mod tests;
