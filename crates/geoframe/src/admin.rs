//! 管理画面の読み取り API（T1.4）: `GET /api/admin/status`（E2）と `GET /api/admin/photos`（E3）。
//!
//! 現行 Python の `app/geoframe/admin.py`（`api_admin_status`・`api_admin_photos`）と
//! `loc_suggest.py:loc_status` を、同じキー・同じ値・同じ型になるように移したもの。どちらも**読み取り
//! だけ**で、Python がバックグラウンドで計算して持つ値は無い（件数はすべて DB から数え直し、ディスク
//! 使用量は Python と同じく 60 秒キャッシュ）。位置候補の列（`db.loc`）は、DB の `photo_loc_clues`・
//! `photo_loc_runs`・`photo_loc_suggestions` と `settings.photo_loc_status`（Python のワーカーが前回の
//! 処理時に書いた Decision の状態）から計算する。Rust は Decision に問い合わせず、その値を書き換えない。
//! ただし Rust が位置の手がかりを判定するときは、`clue_pending` を Rust の保存結果（`ai_location_clues`）が今の
//! 入力に合っていない写真の数にし、`clue_runnable`・`clue_pause` を足す（`ai::location_clue::wait_counts`。
//! **画像ファイルは開かず DB だけを読み**、結果は短い間キャッシュする）。
//!
//! 1回の応答の件数は**1つの読み取りトランザクション**の中で数える（Python は件数ごとにロックを取り直す
//! ので、書き込みが挟まると合計がずれうる。Rust は一貫した断面を返す）。仕様と Python との差は
//! docs/rust-migration/server.md §10。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::types::{Type, ValueRef};
use rusqlite::{Connection, Row, params_from_iter};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::ai::ops::TaskPause;
use crate::ai::settings::TaskKind;
use crate::config::Config;
use crate::photos::parse_epoch;
use crate::sysinfo;

/// `per` の上限（現行の `PER_PAGE_MAX`）。
pub const PER_PAGE_MAX: i64 = 200;

/// 「近い時刻に居た場所」のヒントを出す最大の時間差（秒、7日）。
const HINT_MAX_DT_S: i64 = 7 * 86400;
/// Decision の手がかり判定の合格線（現行の `loc_suggest.CLUE_MIN`）。
const CLUE_MIN: f64 = 0.2;

/// 読み取りに必要な設定。`Config` と解決済みのデータ置き場から作る（読み取りの closure に渡すため `'static`）。
#[derive(Debug, Clone)]
pub struct AdminContext {
    /// `<データ置き場>/derivatives`。表示用画像が無い行で、過去に作った derivative を拾う。
    pub deriv_dir: PathBuf,
    /// 位置推定の段階窓（秒、昇順）。
    pub infer_windows_s: Vec<f64>,
    /// 管理画面の AI の件数が数える `embedding_model`。
    pub embed_model: &'static str,
    /// AI の設定（`ai_config_invalid`・`ai_task_blocks` を作る。応答にはコード・用途名・件数だけを出す）。
    pub ai: crate::ai::settings::AiConfig,
    /// 今の AI 設定の用途ごとの待ちの識別（`ai_waits` を用途に結び直す。ハッシュはメモリの中だけで、応答には出さない）。
    pub ai_waits: crate::ai::waits::Identities,
    /// 設定上、Rust が位置の手がかり（Decision）を判定するとき（取り込みが有効で `location_clue` の用途が使える）の、
    /// Decision のモデルと待ちの件数のキャッシュ。実際のハンドラ登録にはさらに LAN クライアントの生成とデータ置き場の
    /// 解決が要るが、STATUS は設定だけで決める。無ければ `None`（`clue_pending` は旧 Python の行だけで数え、
    /// `clue_runnable`・`clue_pause` は出さない）。
    rust_clue: Option<RustClue>,
    scene_config: Config,
}

impl AdminContext {
    pub fn new(config: &Config, data_root: &Path) -> Self {
        Self {
            deriv_dir: data_root.join("derivatives"),
            infer_windows_s: config.infer_windows_s(),
            embed_model: config.embed_backend.model(),
            ai: config.ai.clone(),
            ai_waits: crate::ai::waits::Identities::from_config(&config.ai),
            rust_clue: rust_clue(config),
            scene_config: config.clone(),
        }
    }
}

impl AdminContext {
    /// 試験用: 待ちの件数のキャッシュの有効時間を変える（`Duration::ZERO` で毎回再計算）。
    #[cfg(test)]
    pub(crate) fn with_clue_ttl(mut self, ttl: Duration) -> Self {
        if let Some(rust) = &mut self.rust_clue {
            rust.cache = Arc::new(ClueCache::new(ttl));
        }
        self
    }
}

/// 設定上 Rust が判定するとき: 取り込みが有効で、`location_clue` の用途が使える（`worker::production_handlers` が
/// ハンドラを登録する設定上の条件。LAN クライアントの生成とデータ置き場を開けることは、ここでは確かめない）。
fn rust_clue(config: &Config) -> Option<RustClue> {
    if !config.ingest {
        return None;
    }
    let gate = config.ai.gate(TaskKind::LocationClue, true).ok()?;
    Some(RustClue {
        model: gate.settings.model.clone(),
        cache: Arc::new(ClueCache::new(CLUE_WAITS_TTL)),
    })
}

/// 待ちの件数を再計算するまでの間隔。STATUS は 5 秒ごとの画面更新や timer から呼ばれる。
const CLUE_WAITS_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct RustClue {
    model: String,
    cache: Arc<ClueCache>,
}

/// 待ちの件数の短いキャッシュ。モデルが変わったら使わない。再計算は 1 つずつ（計算中の別の要求は終わるのを待ち、
/// 出来た値を使う）。停止の有無は値に含めず毎回読む（`ai-jobs release-pause` の効果がすぐ見えるように）。
#[derive(Debug)]
struct ClueCache {
    ttl: Duration,
    slot: Mutex<Option<(String, Instant, crate::ai::location_clue::Waits)>>,
}

impl ClueCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            slot: Mutex::new(None),
        }
    }

    fn get(
        &self,
        model: &str,
        compute: impl FnOnce() -> rusqlite::Result<crate::ai::location_clue::Waits>,
    ) -> rusqlite::Result<crate::ai::location_clue::Waits> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((cached, at, waits)) = slot.as_ref()
            && cached == model
            && at.elapsed() < self.ttl
        {
            return Ok(*waits);
        }
        let waits = compute()?;
        *slot = Some((model.to_string(), Instant::now(), waits));
        Ok(waits)
    }
}

fn conversion_error(message: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, Type::Null, message.into())
}

/// SQLite の値をそのまま JSON にする（Python は列の型のまま JSON にする）。BLOB は想定外。
fn cell(value: ValueRef<'_>) -> rusqlite::Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(_) => return Err(conversion_error("BLOB は JSON にできません")),
    })
}

/// Python の `bool(値)`: NULL・0・0.0・空の文字列は偽。
fn truthy(value: ValueRef<'_>) -> bool {
    match value {
        ValueRef::Null => false,
        ValueRef::Integer(v) => v != 0,
        ValueRef::Real(v) => v != 0.0,
        ValueRef::Text(v) | ValueRef::Blob(v) => !v.is_empty(),
    }
}

fn count(conn: &Connection, sql: &str) -> rusqlite::Result<i64> {
    conn.query_row(sql, [], |row| row.get(0))
}

fn count_photos(conn: &Connection, condition: &str) -> rusqlite::Result<i64> {
    count(
        conn,
        &format!("SELECT COUNT(*) FROM photos WHERE {condition}"),
    )
}

/// フィルタごとの件数（`Filter::ALL` の順。キーは `Filter::name()`）。
fn filter_counts(conn: &Connection) -> rusqlite::Result<Map<String, Value>> {
    let mut out = Map::new();
    for f in Filter::ALL {
        out.insert(f.name().into(), count_photos(conn, f.condition())?.into());
    }
    Ok(out)
}

// ============================================================
// 状態サマリ
// ============================================================

/// `GET /api/admin/status` の `ok`・`sync`・`db`（`disk`・`server_time` は呼び出し側が足す）。
pub fn status(conn: &Connection, ctx: &AdminContext) -> rusqlite::Result<Map<String, Value>> {
    let scenes = crate::ai::groups::runtime::Prepared::read(conn, &ctx.scene_config);
    // 読み取りトランザクション1つ（drop で ROLLBACK）。途中の書き込みを見せず、件数をそろえる
    let _snapshot = conn.unchecked_transaction()?;

    let ready = count_photos(conn, "display IS NOT NULL AND lat IS NOT NULL")?;
    let pending = count_photos(conn, "lat IS NOT NULL AND display IS NULL")?;
    let total = count(conn, "SELECT COUNT(*) FROM photos")?;

    let mut quality = quality_summary(conn)?;
    quality.extend(quality_coverage(conn)?);

    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    out.insert(
        "sync".into(),
        json!({"ready": ready, "pending": pending, "total": total}),
    );
    out.insert(
        "db".into(),
        json!({
            "total": total,
            "no_loc": count_photos(conn, "lat IS NULL")?,
            "no_display": count_photos(conn, "display IS NULL")?,
            "hidden": count_photos(conn, "COALESCE(hidden,0)=1")?,
            "favorite": count_photos(conn, "COALESCE(favorite,0)=1")?,
            "enrich_pending": count_photos(conn, "lat IS NOT NULL AND place IS NULL")?,
            "no_time": count_photos(conn, "taken_at IS NULL OR taken_at=''")?,
            "filters": filter_counts(conn)?,
            "by_loc_source": group_counts(conn, "loc_source")?,
            "by_source": group_counts(conn, "source")?,
            "infer_buckets": infer_buckets(conn, &ctx.infer_windows_s)?,
            "quality": quality,
            "ai": ai_summary(conn, ctx)?,
            "loc": loc_status(conn, ctx)?,
        }),
    );
    // P4（Rust だけが返す追加フィールド）: AI の設定の誤り・ジョブ・停止・共有の一時待機の集計。あるときだけ足すので、
    // AI の表も設定の誤りも停止も無い DB（Python 版と同じ応答）には 1 つも足さない
    crate::ai::ops::insert_admin_status(
        &mut out,
        conn,
        &ctx.ai,
        &ctx.ai_waits,
        crate::ai::jobs::unix_now(),
    );
    if crate::ai::groups::current(conn) {
        out.insert(
            "ai_scenes".into(),
            crate::ai::groups::runtime::status_summary(&scenes, conn),
        );
    }
    Ok(out)
}

/// 列の値ごとの件数（NULL は空文字のキーに寄せる）。現行の `_group` と同じく、NULL と空文字が両方
/// あるときは**後から来た行**（SQLite の GROUP BY は NULL が先）の件数が残る。
/// `sync` に `mass_delete_hold`（Drive の大量削除の保留。T3.7）を足す。保留が無いときは呼ばない
/// （キーを出さない＝Python の応答と同じ形）。写真の名前・座標は入れない。
pub fn insert_mass_delete_hold(
    status: &mut Map<String, Value>,
    hold: &crate::drive::MassDeleteHold,
) {
    if let Some(Value::Object(sync)) = status.get_mut("sync") {
        sync.insert(
            "mass_delete_hold".into(),
            json!({
                "held": hold.held,
                "reason": hold.reason.as_str(),
                "drive_rows": hold.drive_rows,
                "since": hold.since,
                "checked_at": hold.checked_at,
            }),
        );
    }
}

fn group_counts(conn: &Connection, column: &str) -> rusqlite::Result<Map<String, Value>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {column}, COUNT(*) c FROM photos GROUP BY {column}"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Map::new();
    while let Some(row) = rows.next()? {
        let key = match row.get_ref(0)? {
            ValueRef::Null => String::new(),
            ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
            ValueRef::Integer(i) => i.to_string(),
            ValueRef::Real(f) => format!("{f:?}"),
            ValueRef::Blob(_) => return Err(conversion_error("BLOB は JSON のキーにできません")),
        };
        out.insert(key, Value::from(row.get::<_, i64>(1)?));
    }
    Ok(out)
}

/// 錨との時間差（秒）→ 段階窓のラベル（現行の `geo.infer_bucket`）。`None` は空文字。
pub fn infer_bucket(dt_s: Option<f64>, windows_s: &[f64]) -> String {
    let Some(dt) = dt_s else {
        return String::new();
    };
    for &w in windows_s {
        if dt <= w {
            return if w < 3600.0 {
                format!("±{}分", (w / 60.0).floor())
            } else {
                format!("±{}時間", (w / 3600.0).floor())
            };
        }
    }
    "範囲外".into()
}

fn number(value: ValueRef<'_>) -> rusqlite::Result<Option<f64>> {
    match value {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(i) => Ok(Some(i as f64)),
        ValueRef::Real(f) => Ok(Some(f)),
        _ => Err(conversion_error("数値ではありません")),
    }
}

/// 推定の段階内訳。`infer_dt_s` が NULL（未計測）は `未計測`。
fn infer_buckets(conn: &Connection, windows_s: &[f64]) -> rusqlite::Result<Map<String, Value>> {
    let mut stmt = conn.prepare("SELECT infer_dt_s FROM photos WHERE loc_source='inferred'")?;
    let mut rows = stmt.query([])?;
    let mut buckets: Map<String, Value> = Map::new();
    while let Some(row) = rows.next()? {
        let label = infer_bucket(number(row.get_ref(0)?)?, windows_s);
        let label = if label.is_empty() {
            "未計測".to_string()
        } else {
            label
        };
        let next = buckets.get(&label).and_then(Value::as_i64).unwrap_or(0) + 1;
        buckets.insert(label, Value::from(next));
    }
    Ok(buckets)
}

fn quality_summary(conn: &Connection) -> rusqlite::Result<Map<String, Value>> {
    let (analyzed, recommended, exact_suppressed, similar_alternates) = conn.query_row(
        "SELECT COUNT(*) analyzed,
                COALESCE(SUM(q.recommended),0) recommended,
                COALESCE(SUM(q.group_kind='exact' AND q.recommended=0),0) exact_suppressed,
                COALESCE(SUM(q.group_kind='similar' AND q.recommended=0),0) similar_alternates
           FROM photo_quality q JOIN photos p
             ON p.id=q.photo_id AND p.display=q.display
            AND p.md5 IS q.source_md5 AND p.mtime IS q.source_mtime",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )?;
    let mut out = Map::new();
    out.insert("analyzed".into(), analyzed.into());
    out.insert("recommended".into(), recommended.into());
    out.insert("exact_suppressed".into(), exact_suppressed.into());
    out.insert("similar_alternates".into(), similar_alternates.into());
    Ok(out)
}

fn quality_coverage(conn: &Connection) -> rusqlite::Result<Map<String, Value>> {
    let (features, pending) = conn.query_row(
        "SELECT COUNT(f.photo_id) features, COUNT(*)-COUNT(f.photo_id) pending
           FROM photos p LEFT JOIN photo_quality_features f
             ON f.photo_id=p.id AND f.display=p.display
            AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
          WHERE p.display IS NOT NULL AND p.display!=''",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let mut out = Map::new();
    out.insert("features".into(), features.into());
    out.insert("pending".into(), pending.into());
    Ok(out)
}

fn ai_summary(conn: &Connection, ctx: &AdminContext) -> rusqlite::Result<Value> {
    let (embedded, detailed) = conn.query_row(
        "SELECT COUNT(DISTINCT a.content_sha256) embedded,
                COALESCE(SUM(a.recommended IS NOT NULL),0) detailed
           FROM photo_ai_features a JOIN photos p
             ON p.id=a.photo_id AND p.display=a.display
            AND p.md5 IS a.source_md5 AND p.mtime IS a.source_mtime
          WHERE a.error='' AND a.embedding_model=?1",
        [ctx.embed_model],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let eligible = count(
        conn,
        "SELECT COUNT(DISTINCT f.content_sha256) c
           FROM photos p JOIN photo_quality_features f
             ON f.photo_id=p.id AND f.display=p.display
            AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
          WHERE f.error='' AND f.content_sha256 IS NOT NULL",
    )?;
    // groups・group_pending・suppressed は、フィードと同じ旧選別の有効判定から数える（crate::selection）
    let selection = crate::selection::admin_counts(conn)?;
    Ok(json!({
        "embedded": embedded,
        "detailed": detailed,
        "eligible": eligible,
        "pending": (eligible - embedded).max(0),
        "groups": selection.groups,
        "group_pending": selection.group_pending,
        "suppressed": selection.suppressed,
    }))
}

// ---- 位置候補（`loc_suggest.py:loc_status` と、その下の `_load_targets`・`group_days`・`passes`）----

/// 日をまとめる間隔（秒）。最大の段階窓と同じ（現行の `GROUP_GAP_S`）。
fn group_gap_s(windows_s: &[f64]) -> f64 {
    windows_s.iter().copied().fold(f64::MIN, f64::max)
}

/// 位置なし・撮影時刻ありの写真と、現在の画像に対する Decision の判定。
struct Target {
    id: i64,
    epoch: i64,
    taken_at: String,
    /// 今の画像（表示ファイル・md5・mtime）に対する判定がある。
    classified: bool,
    clue: Option<f64>,
    kind: String,
    clue_error: String,
}

impl Target {
    /// Geminiへ送ってよい写真か（手がかりあり）。
    fn passes(&self) -> bool {
        self.clue_error.is_empty()
            && self
                .clue
                .is_some_and(|clue| clue >= CLUE_MIN || self.kind == "landmark")
    }
}

struct Clue {
    display: Option<String>,
    source_md5: Option<String>,
    source_mtime: Option<f64>,
    clue: Option<f64>,
    kind: String,
    error: String,
}

/// 表示用画像の `derivatives/…` 形の相対パス（現行の `_image_rel`）。表示ファイルが無くても、過去に
/// 作った derivative がディスクに残っていれば拾う。
fn image_rel(ctx: &AdminContext, display: Option<&str>, drive_id: Option<&str>) -> String {
    if let Some(display) = display.filter(|d| !d.is_empty()) {
        return display.to_string();
    }
    match drive_id.filter(|d| !d.is_empty()) {
        Some(id) if ctx.deriv_dir.join(format!("{id}.jpg")).exists() => {
            format!("derivatives/{id}.jpg")
        }
        _ => String::new(),
    }
}

fn load_targets(conn: &Connection, ctx: &AdminContext) -> rusqlite::Result<Vec<Target>> {
    let mut clues: HashMap<i64, Clue> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT photo_id, display, source_md5, source_mtime, clue, kind, error
               FROM photo_loc_clues",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            clues.insert(
                row.get(0)?,
                Clue {
                    display: row.get(1)?,
                    source_md5: row.get(2)?,
                    source_mtime: row.get(3)?,
                    clue: row.get(4)?,
                    kind: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    error: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                },
            );
        }
    }
    let mut stmt = conn.prepare(
        "SELECT id,display,drive_id,md5,mtime,taken_at FROM photos
          WHERE lat IS NULL AND taken_at IS NOT NULL AND taken_at!=''",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let taken_at: String = row.get(5)?;
        let Some(epoch) = parse_epoch(&taken_at) else {
            continue;
        };
        let id: i64 = row.get(0)?;
        let rel = image_rel(
            ctx,
            row.get::<_, Option<String>>(1)?.as_deref(),
            row.get::<_, Option<String>>(2)?.as_deref(),
        );
        let md5: Option<String> = row.get(3)?;
        let mtime: Option<f64> = row.get(4)?;
        let fresh = clues.get(&id).filter(|c| {
            c.display.as_deref() == Some(rel.as_str())
                && c.source_md5 == md5
                && c.source_mtime == mtime
        });
        out.push(Target {
            id,
            epoch,
            taken_at,
            classified: fresh.is_some(),
            clue: fresh.and_then(|c| c.clue),
            kind: fresh.map(|c| c.kind.clone()).unwrap_or_default(),
            clue_error: fresh.map(|c| c.error.clone()).unwrap_or_default(),
        });
    }
    out.sort_by_key(|t| (t.epoch, t.id));
    Ok(out)
}

/// 撮影時刻順に、`gap_s` を超える空きか日付の変わり目で区切った、各まとまりの「手がかりあり」の有無。
fn days_with_clue(targets: &[Target], gap_s: f64) -> (usize, usize) {
    let (mut days, mut with_clue) = (0, 0);
    let mut first: Option<&Target> = None;
    let mut last_epoch = 0;
    let mut group_passes = false;
    let date = |t: &Target| t.taken_at.chars().take(10).collect::<String>();
    for target in targets {
        let starts_new = match first {
            None => true,
            Some(first) => {
                (target.epoch - last_epoch) as f64 > gap_s || date(target) != date(first)
            }
        };
        if starts_new {
            if first.is_some() && group_passes {
                with_clue += 1;
            }
            days += 1;
            first = Some(target);
            group_passes = false;
        }
        group_passes |= target.passes();
        last_epoch = target.epoch;
    }
    if group_passes {
        with_clue += 1;
    }
    (days, with_clue)
}

fn loc_status(conn: &Connection, ctx: &AdminContext) -> rusqlite::Result<Value> {
    // Python のワーカーが前回の処理時に書いた Decision の状態。無い・壊れている・オブジェクトでない
    // ときは空（Python は JSON が壊れていれば空、オブジェクトでなければ 500）
    let last: Map<String, Value> = match conn.query_row(
        "SELECT value FROM settings WHERE key='photo_loc_status'",
        [],
        |row| match row.get_ref(0)? {
            ValueRef::Text(t) => Ok(serde_json::from_slice::<Value>(t).ok()),
            _ => Ok(None),
        },
    ) {
        Ok(Some(Value::Object(map))) => map,
        Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => Map::new(),
        Err(e) => return Err(e),
    };
    let targets = load_targets(conn, ctx)?;
    let (days, with_clue) = days_with_clue(&targets, group_gap_s(&ctx.infer_windows_s));
    let suggested = count(
        conn,
        "SELECT COUNT(*) c FROM photo_loc_suggestions s JOIN photos p
           ON p.id=s.photo_id WHERE p.lat IS NULL",
    )?;
    let mut runs: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT status, COUNT(*) c FROM photo_loc_runs GROUP BY status")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            runs.insert(row.get(0)?, row.get(1)?);
        }
    }
    // 待ち: Rust が判定するときは、Rust の判定が今の入力の世代に対して保存されていない写真の数（旧 Python の
    // `photo_loc_clues` の行があっても数える）。Rust が判定しないとき（AI 無効・用途なし・取り込み無効）は、
    // 旧 Python の行の世代一致で数える従来の値のまま。`clue_runnable` は timer 用で、そのうち今 Decision が動けば
    // 進む件数（Provider の停止中は 0。品質待ち・期限前の後回しは数えない）
    let mut clue = Map::new();
    let clue_pending = match &ctx.rust_clue {
        Some(rust) => {
            let waits = rust.cache.get(&rust.model, || {
                crate::ai::location_clue::wait_counts(
                    conn,
                    &rust.model,
                    crate::ai::jobs::unix_now(),
                )
            })?;
            let (runnable, pause) =
                match crate::ai::ops::task_pause(conn, &ctx.ai, TaskKind::LocationClue) {
                    TaskPause::None => (waits.runnable, ""),
                    TaskPause::Paused(paused) => (0, paused.reason.as_str()),
                    // 停止の表を読めないとき、送信側は送らない
                    TaskPause::Unknown => (0, "unknown"),
                };
            clue.insert("clue_runnable".into(), json!(runnable));
            clue.insert("clue_pause".into(), json!(pause));
            waits.pending
        }
        None => targets.iter().filter(|t| !t.classified).count(),
    };
    let mut loc = json!({
        "decision": last.get("decision").cloned().unwrap_or_else(|| json!("")),
        "last_at": last.get("at").cloned().unwrap_or_else(|| json!("")),
        "targets": targets.len(),
        "clue_pending": clue_pending,
        "clue_passed": targets.iter().filter(|t| t.classified && t.passes()).count(),
        "days": days,
        "days_with_clue": with_clue,
        "runs_done": runs.get("done").copied().unwrap_or(0),
        "runs_error": runs.get("error").copied().unwrap_or(0),
        "suggested": suggested,
    });
    if let Value::Object(map) = &mut loc {
        map.extend(clue);
    }
    Ok(loc)
}

// ---- ディスク（`admin.disk_info`）----

/// ディレクトリ配下のファイル数と合計バイト数（再帰、現行の `_dir_usage`）。`os.walk` と同じく、
/// ディレクトリへのシンボリックリンクは辿らず、走査中に消えたファイル・読めないディレクトリは無視する。
fn dir_usage(path: &Path) -> Value {
    if !path.exists() {
        return json!({"files": 0, "bytes": 0, "exists": false});
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
                continue;
            }
            // シンボリックリンクは先を見る（先がディレクトリなら数えない。壊れていれば無視）
            let metadata = if kind.is_symlink() {
                fs::metadata(entry.path())
            } else {
                entry.metadata()
            };
            if let Ok(m) = metadata
                && !m.is_dir()
            {
                files += 1;
                bytes += m.len();
            }
        }
    }
    json!({"files": files, "bytes": bytes, "exists": true})
}

/// 小数1桁に丸める（Python の `round(x, 1)`: 厳密な値に対する偶数丸め。`format!` の `{:.1}` と同じ）。
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// `disk`（ブロッキング。ディレクトリを全走査するので、呼び出し側が 60 秒キャッシュして
/// `spawn_blocking` で呼ぶ）。
pub fn disk_info(data_root: &Path, tile_cache_max_mb: i64) -> io::Result<Value> {
    let usage = sysinfo::disk_usage(data_root)?;
    let used_pct = if usage.total > 0 {
        Value::from(round1(usage.used as f64 / usage.total as f64 * 100.0))
    } else {
        Value::from(0)
    };
    Ok(json!({
        "total_bytes": usage.total,
        "used_bytes": usage.used,
        "free_bytes": usage.free,
        "used_pct": used_pct,
        "dirs": {
            "derivatives": dir_usage(&data_root.join("derivatives")),
            "tile_cache": dir_usage(&data_root.join("tile_cache")),
        },
        "tile_cache_max_mb": tile_cache_max_mb,
        "measured_at": sysinfo::local_timestamp(),
    }))
}

// ============================================================
// 写真一覧（ページング）
// ============================================================

/// `filter` の値。未知の値は `all`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    NoLoc,
    Fav,
    Hidden,
    Inferred,
    Manual,
    Suggest,
}

impl Filter {
    /// 管理画面のチップの並び（`db.filters` のキーの順）。
    pub const ALL: [Filter; 7] = [
        Self::All,
        Self::NoLoc,
        Self::Suggest,
        Self::Inferred,
        Self::Manual,
        Self::Fav,
        Self::Hidden,
    ];

    pub fn parse(name: &str) -> Self {
        match name {
            "noloc" => Self::NoLoc,
            "fav" => Self::Fav,
            "hidden" => Self::Hidden,
            "inferred" => Self::Inferred,
            "manual" => Self::Manual,
            "suggest" => Self::Suggest,
            _ => Self::All,
        }
    }

    /// 応答の `filter` に返す、解決後の名前（受け取った文字列は返さない）。
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::NoLoc => "noloc",
            Self::Fav => "fav",
            Self::Hidden => "hidden",
            Self::Inferred => "inferred",
            Self::Manual => "manual",
            Self::Suggest => "suggest",
        }
    }

    fn condition(self) -> &'static str {
        match self {
            Self::All => "1=1",
            Self::NoLoc => "lat IS NULL",
            Self::Fav => "COALESCE(favorite,0)=1",
            Self::Hidden => "COALESCE(hidden,0)=1",
            Self::Inferred => "loc_source='inferred'",
            Self::Manual => "loc_source='manual'",
            // 位置なしのうち、AI の撮影地候補がある写真
            Self::Suggest => "lat IS NULL AND id IN (SELECT photo_id FROM photo_loc_suggestions)",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Hint {
    label: String,
    lat: Value,
    lon: Value,
    taken_at: String,
    dt_s: i64,
}

#[derive(Debug, Serialize)]
pub struct Suggest {
    place_name: Value,
    day_place_name: Value,
    scope: Value,
    precision: Value,
    confidence: Value,
    evidence: Value,
    lat: Value,
    lon: Value,
    geocode_title: Value,
    model: Value,
}

/// 一覧の1枚。フィールドの並びは現行の `api_admin_photos` と同じ。
#[derive(Debug, Serialize)]
pub struct AdminPhoto {
    id: i64,
    name: Value,
    thumb: String,
    taken_at: String,
    lat: Value,
    lon: Value,
    loc_source: String,
    infer_dt_s: Value,
    infer_bucket: String,
    place: String,
    pref: String,
    city: String,
    fav: bool,
    hidden: bool,
    rotation: Value,
    source: Value,
    /// 外側が `None`: キーなし。`Some(None)`: `null`（どのフィルタでも位置なしの行で、近い写真が無い・7日より離れている）。
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<Option<Hint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggest: Option<Suggest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scene: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct PhotosPage {
    ok: bool,
    pub page: i64,
    pub per: i64,
    pub pages: i64,
    pub total: i64,
    filter: &'static str,
    pub items: Vec<AdminPhoto>,
}

/// 一覧用サムネイルの URL（現行の `_thumb_for`）。表示ファイルがあればそれ、無くても過去に作った
/// derivative が残っていれば拾う。Drive へ取りに行くことはしない。
fn thumb_for(ctx: &AdminContext, display: Option<&str>, drive_id: Option<&str>) -> String {
    if let Some(display) = display.filter(|d| !d.is_empty()) {
        return format!("/{display}");
    }
    match drive_id.filter(|d| !d.is_empty()) {
        Some(id) if ctx.deriv_dir.join(format!("{id}.jpg")).exists() => {
            format!("/derivatives/{id}.jpg")
        }
        _ => String::new(),
    }
}

struct Anchor {
    epoch: i64,
    label: String,
    lat: Value,
    lon: Value,
    taken_at: String,
}

/// 位置あり写真（EXIF・Drive由来）の (時刻, 地名)。時刻順（同時刻は SQLite の走査順のまま）。
fn time_anchors(conn: &Connection) -> rusqlite::Result<Vec<Anchor>> {
    let mut stmt = conn.prepare(
        "SELECT taken_at, lat, lon, place, pref, city FROM photos
          WHERE lat IS NOT NULL AND taken_at != ''
            AND loc_source IN ('exif','drive_meta')",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let taken_at: String = row.get(0)?;
        let Some(epoch) = parse_epoch(&taken_at) else {
            continue;
        };
        // 並びは都道府県・市区町村・町名（SELECT の 4・5・3 列目）
        let label = [4, 5, 3]
            .iter()
            .map(|&i| {
                row.get::<_, Option<String>>(i)
                    .map(Option::unwrap_or_default)
            })
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Anchor {
            epoch,
            label,
            lat: cell(row.get_ref(1)?)?,
            lon: cell(row.get_ref(2)?)?,
            taken_at,
        });
    }
    out.sort_by_key(|a| a.epoch);
    Ok(out)
}

/// 撮影時刻が最も近い位置あり写真の場所（DB には書かない表示ヒント）。同じ差のときは時刻の早いほう。
/// 時刻が読めない・7日より離れている・錨が無いときは `None`。
fn nearest_hint(anchors: &[Anchor], taken_at: &str) -> Option<Hint> {
    let epoch = parse_epoch(taken_at)?;
    let best = anchors.iter().min_by_key(|a| (a.epoch - epoch).abs())?;
    let dt_s = (best.epoch - epoch).abs();
    if dt_s > HINT_MAX_DT_S {
        return None;
    }
    Some(Hint {
        label: best.label.clone(),
        lat: best.lat.clone(),
        lon: best.lon.clone(),
        taken_at: best.taken_at.clone(),
        dt_s,
    })
}

/// 位置なし写真の AI 撮影地候補（画面に出すだけで、DB の位置にはしない）。
fn suggestions(conn: &Connection, ids: &[i64]) -> rusqlite::Result<HashMap<i64, Suggest>> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let marks = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT photo_id, place_name, day_place_name, scope, precision, confidence,
                evidence_json, lat, lon, geocode_title, model
           FROM photo_loc_suggestions WHERE photo_id IN ({marks})"
    ))?;
    let mut rows = stmt.query(params_from_iter(ids))?;
    // 画像ファイルは開かない（一覧の GET で行ごとにファイルを読んで SHA を取らない）。履歴の読込みも 1 回。
    let mut filter = crate::ai::location_suggest::CandidateFilter::new(conn)?;
    while let Some(row) = rows.next()? {
        if !filter.current(conn, row.get(0)?)? {
            continue;
        }
        let evidence = match row.get_ref(6)? {
            ValueRef::Text(t) => match serde_json::from_slice::<Value>(t) {
                Ok(list @ Value::Array(_)) => list,
                _ => json!([]),
            },
            _ => json!([]),
        };
        out.insert(
            row.get(0)?,
            Suggest {
                place_name: cell(row.get_ref(1)?)?,
                day_place_name: cell(row.get_ref(2)?)?,
                scope: cell(row.get_ref(3)?)?,
                precision: cell(row.get_ref(4)?)?,
                confidence: cell(row.get_ref(5)?)?,
                evidence,
                lat: cell(row.get_ref(7)?)?,
                lon: cell(row.get_ref(8)?)?,
                geocode_title: cell(row.get_ref(9)?)?,
                model: cell(row.get_ref(10)?)?,
            },
        );
    }
    Ok(out)
}

/// 位置の補完（近い時刻の位置のヒント・AI の撮影地候補・由来の段階）の入出力。一覧（`photos`）と
/// AI 検索（`ai::search_view`）が共有する。入力は DB から読んだ値で、`no_loc` は行を読むときの `lat IS NULL`
/// （JSON にしたあとの値ではない）。
#[derive(Debug)]
pub(crate) struct LocFields {
    pub id: i64,
    pub taken_at: String,
    pub no_loc: bool,
    pub loc_source: String,
    pub infer_dt_s: Option<f64>,
    /// 出力。外側が `None`: キーなし（位置ありの行・錨が1枚も無い）。`Some(None)`: `null`。
    pub hint: Option<Option<Hint>>,
    /// 出力。候補が無い・古いときは `None`（キーなし）。
    pub suggest: Option<Suggest>,
    /// 出力。`loc_source='inferred'` のときだけ段階のラベル、ほかは空文字。
    pub infer_bucket: String,
}

impl LocFields {
    pub(crate) fn new(
        id: i64,
        taken_at: String,
        no_loc: bool,
        loc_source: String,
        infer_dt_s: Option<f64>,
    ) -> Self {
        Self {
            id,
            taken_at,
            no_loc,
            loc_source,
            infer_dt_s,
            hint: None,
            suggest: None,
            infer_bucket: String::new(),
        }
    }
}

/// 錨（位置あり写真）を読む必要があるか: 位置なしの行が1つでもあるときだけ。
fn needs_anchors(rows: &[LocFields]) -> bool {
    rows.iter().any(|r| r.no_loc)
}

/// 位置なしの行の `suggestions` を引く ID の束の大きさ（SQLite の変数の上限に当たらないように分ける）。
const SUGGEST_CHUNK: usize = 500;

/// `rows` に由来の段階・ヒント・撮影地候補を付ける。`rows` の `id` は重複させない。ヒントは位置なしの行にだけ付き
/// （錨が1枚でもあるときに付き、近い写真が無ければ `null`）、錨は位置なしの行があるときだけ読む。候補は位置なしの
/// ID をまとめて引く。
pub(crate) fn decorate_locations(
    conn: &Connection,
    windows_s: &[f64],
    rows: &mut [LocFields],
) -> rusqlite::Result<()> {
    decorate_with(conn, windows_s, rows, time_anchors)
}

fn decorate_with(
    conn: &Connection,
    windows_s: &[f64],
    rows: &mut [LocFields],
    load_anchors: impl FnOnce(&Connection) -> rusqlite::Result<Vec<Anchor>>,
) -> rusqlite::Result<()> {
    for row in rows.iter_mut() {
        row.infer_bucket = if row.loc_source == "inferred" {
            infer_bucket(row.infer_dt_s, windows_s)
        } else {
            String::new()
        };
    }
    let anchors = if needs_anchors(rows) {
        load_anchors(conn)?
    } else {
        Vec::new()
    };
    if !anchors.is_empty() {
        for row in rows.iter_mut().filter(|r| r.no_loc) {
            row.hint = Some(nearest_hint(&anchors, &row.taken_at));
        }
    }
    let no_loc_ids: Vec<i64> = rows.iter().filter(|r| r.no_loc).map(|r| r.id).collect();
    let mut found = HashMap::new();
    for chunk in no_loc_ids.chunks(SUGGEST_CHUNK) {
        found.extend(suggestions(conn, chunk)?);
    }
    for row in rows.iter_mut() {
        row.suggest = found.remove(&row.id);
    }
    Ok(())
}

fn text(row: &Row<'_>, index: usize) -> rusqlite::Result<String> {
    Ok(row.get::<_, Option<String>>(index)?.unwrap_or_default())
}

/// `GET /api/admin/photos`。`page`・`per` は `int()` を通した値（桁あふれは飽和）で、ここで丸める:
/// `per` は 1〜[`PER_PAGE_MAX`]、`page` は 1〜総ページ数。
pub fn photos(
    conn: &Connection,
    ctx: &AdminContext,
    page: i64,
    per: i64,
    filter: Filter,
) -> rusqlite::Result<PhotosPage> {
    let prepared = crate::ai::groups::runtime::Prepared::read(conn, &ctx.scene_config);
    let _snapshot = conn.unchecked_transaction()?;
    let condition = filter.condition();
    let per = per.clamp(1, PER_PAGE_MAX);
    let total = count_photos(conn, condition)?;
    let pages = ((total + per - 1) / per).max(1);
    let page = page.clamp(1, pages);

    let mut stmt = conn.prepare(&format!(
        "SELECT id, name, display, drive_id, taken_at, lat, lon, loc_source, infer_dt_s,
                place, pref, city, favorite, hidden, rotation, source
           FROM photos WHERE {condition}
          ORDER BY CASE WHEN taken_at IS NULL OR taken_at='' THEN 1 ELSE 0 END, taken_at, id
          LIMIT ?1 OFFSET ?2"
    ))?;
    let mut rows = stmt.query([per, (page - 1) * per])?;
    let mut items = Vec::new();
    // 補完（ヒント・候補・由来）の入力。行を読むときの DB の値で作る
    let mut loc = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let loc_source = text(row, 7)?;
        let infer_dt_s = cell(row.get_ref(8)?)?;
        loc.push(LocFields::new(
            id,
            text(row, 4)?,
            matches!(row.get_ref(5)?, ValueRef::Null),
            loc_source.clone(),
            // 段階を出すのは由来が推定の行だけ（ほかの行の infer_dt_s の型は見ない）
            if loc_source == "inferred" {
                number(row.get_ref(8)?)?
            } else {
                None
            },
        ));
        let rotation = match row.get_ref(14)? {
            value if !truthy(value) => Value::from(0),
            value => cell(value)?,
        };
        items.push(AdminPhoto {
            id,
            name: cell(row.get_ref(1)?)?,
            thumb: thumb_for(
                ctx,
                row.get::<_, Option<String>>(2)?.as_deref(),
                row.get::<_, Option<String>>(3)?.as_deref(),
            ),
            taken_at: text(row, 4)?,
            lat: cell(row.get_ref(5)?)?,
            lon: cell(row.get_ref(6)?)?,
            infer_bucket: String::new(),
            loc_source,
            infer_dt_s,
            place: text(row, 9)?,
            pref: text(row, 10)?,
            city: text(row, 11)?,
            fav: truthy(row.get_ref(12)?),
            hidden: truthy(row.get_ref(13)?),
            rotation,
            source: cell(row.get_ref(15)?)?,
            hint: None,
            suggest: None,
            scene: None,
        });
    }
    drop(rows);
    drop(stmt);

    // 場面の注記は、このページの写真の分だけ保存済みの値から読む（ベクトルも他の写真も読まない）
    let mut scenes = prepared
        .and_then(|p| p.annotations_in(conn, &items.iter().map(|i| i.id).collect::<Vec<_>>()))
        .unwrap_or_default();
    for item in &mut items {
        item.scene = scenes.remove(&item.id).map(|a| {
            json!({"state":a.state.as_str(),"overlap":a.overlap.as_str(),"rank":a.rank,"score":a.score,"representative":a.representative,"shortlisted":a.shortlisted})
        });
    }

    // ヒントは、どのフィルタでも位置なしの行（DB の `lat IS NULL`）にだけ付く。錨が1枚でもあるときに付き
    // （近い写真が無ければ null）、ページ内に位置なしの行が無ければ錨も読まない
    decorate_locations(conn, &ctx.infer_windows_s, &mut loc)?;
    for (item, fields) in items.iter_mut().zip(loc) {
        item.infer_bucket = fields.infer_bucket;
        item.hint = fields.hint;
        item.suggest = fields.suggest;
    }
    Ok(PhotosPage {
        ok: true,
        page,
        per,
        pages,
        total,
        filter: filter.name(),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn located(id: i64) -> LocFields {
        LocFields::new(id, "2025-01-01 10:00".into(), false, "exif".into(), None)
    }

    #[test]
    fn anchors_are_read_only_when_a_row_has_no_location() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::schema::SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO photos(id, source, name, taken_at, lat, lon, loc_source, city)
               VALUES (1, 'local', 'anchor', '2025-01-01 10:00', 35.0, 139.0, 'exif', '東京');",
        )
        .unwrap();
        let reads = std::cell::Cell::new(0);
        let load = |conn: &Connection| {
            reads.set(reads.get() + 1);
            time_anchors(conn)
        };
        // 位置ありの行だけ・行が無い: 読まない
        let mut rows = vec![located(1), located(2)];
        assert!(!needs_anchors(&rows));
        decorate_with(&conn, &[900.0], &mut rows, load).unwrap();
        decorate_with(&conn, &[900.0], &mut [], load).unwrap();
        assert_eq!(reads.get(), 0);
        assert!(rows.iter().all(|r| r.hint.is_none() && r.suggest.is_none()));
        // 位置なしの行が混じる: 1 回だけ読み、位置なしの行にだけヒントが付く
        let mut rows = vec![
            located(1),
            LocFields::new(3, "2025-01-01 10:05".into(), true, String::new(), None),
            LocFields::new(4, "2025-02-01 10:05".into(), true, String::new(), None),
        ];
        assert!(needs_anchors(&rows));
        decorate_with(&conn, &[900.0], &mut rows, load).unwrap();
        assert_eq!(reads.get(), 1);
        assert!(rows[0].hint.is_none());
        assert_eq!(rows[1].hint.as_ref().unwrap().as_ref().unwrap().dt_s, 300);
        assert!(matches!(rows[2].hint, Some(None))); // 7 日より離れている
    }

    #[test]
    fn bucket_labels_follow_the_windows() {
        let default = [900.0, 7200.0, 43200.0];
        for (dt, expected) in [
            (None, ""),
            (Some(0.0), "±15分"),
            (Some(900.0), "±15分"),
            (Some(901.0), "±2時間"),
            (Some(7200.0), "±2時間"),
            (Some(7201.0), "±12時間"),
            (Some(43200.0), "±12時間"),
            (Some(43201.0), "範囲外"),
            (Some(-1.0), "±15分"),
        ] {
            assert_eq!(infer_bucket(dt, &default), expected, "{dt:?}");
        }
        // 1時間未満は分、それ以上は時間（端数は切り捨て）
        assert_eq!(infer_bucket(Some(1.0), &[30.0]), "±0分");
        assert_eq!(infer_bucket(Some(1.0), &[3599.0]), "±59分");
        assert_eq!(infer_bucket(Some(1.0), &[3600.0]), "±1時間");
        assert_eq!(infer_bucket(Some(1.0), &[60000.0]), "±16時間");
        assert_eq!(infer_bucket(Some(1.0), &[]), "範囲外");
    }

    #[test]
    fn all_lists_every_filter_in_chip_order() {
        let names: Vec<_> = Filter::ALL.iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            [
                "all", "noloc", "suggest", "inferred", "manual", "fav", "hidden"
            ]
        );
        for f in Filter::ALL {
            assert_eq!(Filter::parse(f.name()), f);
        }
    }

    #[test]
    fn unknown_filters_fall_back_to_all_and_names_round_trip() {
        for name in [
            "all", "noloc", "fav", "hidden", "inferred", "manual", "suggest",
        ] {
            assert_eq!(Filter::parse(name).name(), name);
        }
        for unknown in ["", "ALL", "Fav", "bogus", "fav ", "<script>"] {
            assert_eq!(Filter::parse(unknown), Filter::All, "{unknown:?}");
        }
    }

    #[test]
    fn rounding_matches_python_round_to_one_digit() {
        // Python の round(x, 1): 厳密な二進値に対する偶数丸め
        assert_eq!(round1(0.25), 0.2);
        assert_eq!(round1(0.35), 0.3); // 0.35 は二進で 0.34999…
        assert_eq!(round1(12.345), 12.3);
        assert_eq!(round1(99.95), 100.0); // 99.95 は二進で 99.9500000000000028…
        assert_eq!(round1(0.0), 0.0);
    }
}
