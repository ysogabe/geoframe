//! 簡易品質（段 `quality`。P2 / T2.6）。現行 `quality.py` の `quality_cycle`・`quality_backfill`。
//! 詳細は `docs/rust-migration/server.md` §16。
//!
//! # 流れ（現行と同じ。同期サイクルの最後の段）
//!
//! 1. **孤児の特徴量を消す**: 対応する写真が無い（または `display` が空の）`photo_quality_features` の行。消したら
//!    `photo_quality_dirty=1`。
//! 2. **未解析を `GEOFRAME_QUALITY_PER_CYCLE` 件まで解析する**（既定は `GEOFRAME_FETCH_PER_CYCLE`。0 は 1 として
//!    扱う）。未解析は、`display` が空でなく、特徴量の行が無い、または行の `display`・`md5`・`mtime` が今の
//!    写真と違う（`IS` 比較。画像が変わると自動で無効になる）写真。`id` の昇順。SHA-256・pHash・鮮明度・露出を
//!    `photo_quality_features` に upsert して `photo_quality_dirty=1`。画像として読めない写真は `error` を
//!    書いた行になる（**失敗した種類だけ**。パスは書かない）。この行も「解析済み」なので、読めない写真が
//!    毎サイクル枠を占めることはない（画像が変わるまで再解析しない）。
//! 3. **全員分そろってから再構築する**: 未解析が 0 件で `photo_quality_dirty=1` のときだけ、全写真の特徴量から
//!    グルーピングし直し（[`group`]）、`photo_quality` を **`BEGIN IMMEDIATE` の 1 トランザクションで全差し替え**して
//!    `photo_quality_dirty=0` にする。未解析が残る間は前回の結果を保つ。
//!
//! 失敗しても同期は止めない（段の失敗は隔離される）。再構築は読み取りも書き込みも同じトランザクションの中で
//! 行うので、読んだあとに特徴量が変わって食い違うことはない。
//!
//! # `--dry-run`
//!
//! 解析（重い）は実行しない。孤児の件数・未解析の件数・今回解析する件数・再構築をするか（未解析が残らず、
//! 解析か削除かダーティの印がある）を数える。今回解析しなくて再構築になるときは、再構築で書く行数と、
//! 今の `photo_quality` から変わる行数（`changed`）も出す（取り込み済みの本番コピーでは再構築そのものが起きない）。
//!
//! # 特徴量の算法と混在
//!
//! 特徴量の行には算法の印が無い。Python が作った行も、Rust が作った行も、同じく有効として使う。pHash は
//! Python と同じ作り方で、Python の値との距離を計測してある（[`features`]、server.md §16.2）。

mod features;
mod group;
#[cfg(test)]
mod tests;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::datadir::DataDir;
use crate::imaging::ImageError;
use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

pub use features::{Features, Metrics, analyze_bytes, image_metrics, perceptual_hash, sha256_hex};
pub use group::rank_scores;
pub use group::{QualityPhoto, QualityRow, build_quality_rows};

/// `photo_quality.algorithm`。Python の `poc-phash-v1:t3:h12` と区別する（`t3` は時刻の窓 3 分、`h12` は
/// pHash の距離 12）。再構築のたびに全行がこの値になる。
pub const ALGORITHM: &str = "rust-phash-v1:t3:h12";

/// `settings` の、品質の再構築が要る印。
const DIRTY_KEY: &str = "photo_quality_dirty";

/// 停止の合図で中断したときの `Skipped` の理由（定型文）。
const STOPPED: &str = "停止の合図で中断しました";

/// 1 枚の解析（`display` の値から特徴量か、行に書く失敗の文面）。段の本番は [`analyze_one`]。
/// テストが合成の特徴量を返す関数に差し替える。
type Analyzer = fn(&DataDir, &Value) -> Result<Features, String>;

/// 同期サイクルの段 `quality`。
pub struct Quality {
    analyze: Analyzer,
}

impl Quality {
    pub fn new() -> Self {
        Self {
            analyze: analyze_one,
        }
    }

    #[cfg(test)]
    fn with_analyzer(analyze: Analyzer) -> Self {
        Self { analyze }
    }
}

impl Default for Quality {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析の対象の 1 枚（`photos` の今の値）。
struct Pending {
    id: i64,
    display: Value,
    md5: Value,
    mtime: Value,
}

/// 1 枚の解析結果（`photo_quality_features` の 1 行ぶん）。
struct FeatureRow {
    pending: Pending,
    result: Result<Features, String>,
}

const PENDING_FROM: &str = "FROM photos p
     LEFT JOIN photo_quality_features f
       ON f.photo_id=p.id AND f.display=p.display
      AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
     WHERE p.display IS NOT NULL AND p.display!='' AND f.photo_id IS NULL";

const ORPHAN_WHERE: &str = "NOT EXISTS (
       SELECT 1 FROM photos p WHERE p.id=photo_quality_features.photo_id
         AND p.display IS NOT NULL AND p.display!='')";

fn count_i64(conn: &Connection, sql: &str) -> rusqlite::Result<i64> {
    conn.query_row(sql, [], |row| row.get(0))
}

pub(crate) fn pending_count(conn: &Connection) -> rusqlite::Result<i64> {
    count_i64(conn, &format!("SELECT COUNT(*) {PENDING_FROM}"))
}

fn orphan_count(conn: &Connection) -> rusqlite::Result<i64> {
    count_i64(
        conn,
        &format!("SELECT COUNT(*) FROM photo_quality_features WHERE {ORPHAN_WHERE}"),
    )
}

fn pending_rows(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<Pending>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT p.id, p.display, p.md5, p.mtime {PENDING_FROM} ORDER BY p.id LIMIT ?1"
    ))?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok(Pending {
            id: row.get(0)?,
            display: row.get(1)?,
            md5: row.get(2)?,
            mtime: row.get(3)?,
        })
    })?;
    rows.collect()
}

fn is_dirty(conn: &Connection) -> rusqlite::Result<bool> {
    let value: Option<Value> = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [DIRTY_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value == Some(Value::Text("1".into())))
}

fn set_dirty(conn: &Connection, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![DIRTY_KEY, value],
    )?;
    Ok(())
}

/// 書き込み用のトランザクション。最初に書き込みの権利を取る（読んでから書く途中で他の書き手に割り込まれて
/// 失敗しない）。
fn immediate(conn: &mut Connection) -> rusqlite::Result<Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// 孤児の特徴量を消す。消した数を返す（消したら再構築の印を立てる）。
fn remove_orphans(conn: &mut Connection) -> rusqlite::Result<i64> {
    let tx = immediate(conn)?;
    let removed = tx.execute(
        &format!("DELETE FROM photo_quality_features WHERE {ORPHAN_WHERE}"),
        [],
    )? as i64;
    if removed > 0 {
        set_dirty(&tx, "1")?;
    }
    tx.commit()?;
    Ok(removed)
}

/// 特徴量の行を upsert して、再構築の印を立てる（1 つのトランザクション）。
fn write_features(
    conn: &mut Connection,
    rows: &[FeatureRow],
    analyzed_at: &str,
) -> rusqlite::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = immediate(conn)?;
    {
        let mut upsert = tx.prepare(
            "INSERT INTO photo_quality_features(
               photo_id,display,source_md5,source_mtime,content_sha256,phash,
               sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(photo_id) DO UPDATE SET
               display=excluded.display,source_md5=excluded.source_md5,
               source_mtime=excluded.source_mtime,content_sha256=excluded.content_sha256,
               phash=excluded.phash,sharpness=excluded.sharpness,luminance=excluded.luminance,
               exposure_score=excluded.exposure_score,black_clip=excluded.black_clip,
               white_clip=excluded.white_clip,contrast=excluded.contrast,error=excluded.error,
               analyzed_at=excluded.analyzed_at",
        )?;
        for row in rows {
            let p = &row.pending;
            match &row.result {
                Ok(f) => upsert.execute(params![
                    p.id,
                    p.display,
                    p.md5,
                    p.mtime,
                    f.sha256,
                    format!("{:016x}", f.phash),
                    f.metrics.sharpness,
                    f.metrics.luminance,
                    f.metrics.exposure_score,
                    f.metrics.black_clip,
                    f.metrics.white_clip,
                    f.metrics.contrast,
                    "",
                    analyzed_at,
                ])?,
                Err(error) => upsert.execute(params![
                    p.id,
                    p.display,
                    p.md5,
                    p.mtime,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    error,
                    analyzed_at,
                ])?,
            };
        }
    }
    set_dirty(&tx, "1")?;
    tx.commit()
}

/// `parse_taken_at`: `YYYY-MM-DD HH:MM:SS`、なければ `YYYY-MM-DD HH:MM`。読めなければ `None`。
fn parse_taken_at(value: &str) -> Option<i64> {
    if value.is_empty() {
        return None;
    }
    crate::geo::naive_seconds_with_seconds(value)
        .or_else(|| crate::geo::naive_seconds(value))
        .map(|s| s as i64)
}

/// 再構築の入力。解析済みで、今の写真と `display`・`md5`・`mtime` が一致し、エラーでない行（`id` の昇順）。
fn load_features(conn: &Connection) -> rusqlite::Result<Vec<QualityPhoto>> {
    let mut stmt = conn.prepare(
        "SELECT p.id,p.display,p.md5,p.mtime,p.taken_at,
                f.content_sha256,f.phash,f.sharpness,f.luminance,
                f.exposure_score,f.black_clip,f.white_clip,f.contrast
           FROM photos p JOIN photo_quality_features f
             ON f.photo_id=p.id AND f.display=p.display
            AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime
          WHERE p.display IS NOT NULL AND p.display!='' AND f.error=''
            AND f.content_sha256 IS NOT NULL AND f.phash IS NOT NULL
            AND f.sharpness IS NOT NULL AND f.luminance IS NOT NULL
            AND f.exposure_score IS NOT NULL AND f.black_clip IS NOT NULL
            AND f.white_clip IS NOT NULL AND f.contrast IS NOT NULL
          ORDER BY p.id",
    )?;
    let mut rows = stmt.query([])?;
    let mut photos = Vec::new();
    while let Some(row) = rows.next()? {
        // `taken_at` が文字列でなければ空として扱う（現行の `row["taken_at"] or ""`）
        let taken_at = match row.get::<_, Value>(4)? {
            Value::Text(s) => s,
            _ => String::new(),
        };
        // 16 進で読めない pHash の行は使わない（Rust は常に 16 桁の 16 進を書く）
        let Ok(phash) = u64::from_str_radix(row.get::<_, String>(6)?.trim(), 16) else {
            continue;
        };
        photos.push(QualityPhoto {
            id: row.get(0)?,
            display: row.get(1)?,
            source_md5: row.get(2)?,
            source_mtime: row.get(3)?,
            captured: parse_taken_at(&taken_at),
            taken_at,
            sha256: row.get(5)?,
            phash,
            metrics: Metrics {
                sharpness: row.get(7)?,
                luminance: row.get(8)?,
                exposure_score: row.get(9)?,
                black_clip: row.get(10)?,
                white_clip: row.get(11)?,
                contrast: row.get(12)?,
            },
        });
    }
    Ok(photos)
}

/// 今の `photo_quality`（比べる列だけ）。`photo_id` の昇順。
fn current_rows(conn: &Connection) -> rusqlite::Result<Vec<QualityRow>> {
    let mut stmt = conn.prepare(
        "SELECT photo_id,display,source_md5,source_mtime,content_sha256,group_id,group_kind,
                rank_in_group,recommended,score,reasons_json,issues_json
           FROM photo_quality ORDER BY photo_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let kind: String = row.get(6)?;
        Ok(QualityRow {
            photo_id: row.get(0)?,
            display: row.get(1)?,
            source_md5: row.get(2)?,
            source_mtime: row.get(3)?,
            content_sha256: row.get(4)?,
            group_id: row.get(5)?,
            kind: if kind == "exact" { "exact" } else { "similar" },
            rank: row.get(7)?,
            recommended: row.get::<_, i64>(8)? != 0,
            score: row.get(9)?,
            reasons_json: row.get(10)?,
            issues_json: row.get(11)?,
        })
    })?;
    rows.collect()
}

/// 再構築の結果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rebuild {
    /// 未解析の残り（0 でないと再構築しない）。
    pub remaining: i64,
    pub rebuilt: bool,
    /// 書いた（`--dry-run` なら書くはずの）行数。
    pub rows: usize,
}

/// 再構築の条件を満たすなら、グルーピングして `photo_quality` を全差し替えする。**読み取りも書き込みも
/// 同じ `BEGIN IMMEDIATE` の中**。確定の前に停止の合図が来たら何も変えずに戻る（`rebuilt=false`）。
pub(crate) fn rebuild_if_ready(
    conn: &mut Connection,
    analyzed_at: &str,
    stopped: &dyn Fn() -> bool,
) -> rusqlite::Result<Rebuild> {
    let tx = immediate(conn)?;
    let remaining = pending_count(&tx)?;
    if remaining > 0 || !is_dirty(&tx)? {
        return Ok(Rebuild {
            remaining,
            rebuilt: false,
            rows: 0,
        });
    }
    let rows = build_quality_rows(&load_features(&tx)?);
    tx.execute("DELETE FROM photo_quality", [])?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO photo_quality(
               photo_id,display,source_md5,source_mtime,content_sha256,
               group_id,group_kind,rank_in_group,recommended,score,
               reasons_json,issues_json,algorithm,analyzed_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        for r in &rows {
            insert.execute(params![
                r.photo_id,
                r.display,
                r.source_md5,
                r.source_mtime,
                r.content_sha256,
                r.group_id,
                r.kind,
                r.rank,
                i64::from(r.recommended),
                r.score,
                r.reasons_json,
                r.issues_json,
                ALGORITHM,
                analyzed_at,
            ])?;
        }
    }
    set_dirty(&tx, "0")?;
    if stopped() {
        return Ok(Rebuild {
            remaining: 0,
            rebuilt: false,
            rows: 0,
        });
    }
    tx.commit()?;
    Ok(Rebuild {
        remaining: 0,
        rebuilt: true,
        rows: rows.len(),
    })
}

/// `--dry-run` の再構築の見積もり: 書くはずの行数と、今の `photo_quality` から変わる行数（行の追加・削除・
/// 変更。`analyzed_at`・`algorithm` は比べない）。
fn plan_rebuild(conn: &Connection) -> rusqlite::Result<(usize, usize)> {
    let new = build_quality_rows(&load_features(conn)?);
    let current = current_rows(conn)?;
    let by_id: std::collections::HashMap<i64, &QualityRow> =
        current.iter().map(|r| (r.photo_id, r)).collect();
    let new_ids: std::collections::HashSet<i64> = new.iter().map(|r| r.photo_id).collect();
    let changed = new
        .iter()
        .filter(|r| by_id.get(&r.photo_id).is_none_or(|old| old != r))
        .count()
        + current
            .iter()
            .filter(|r| !new_ids.contains(&r.photo_id))
            .count();
    Ok((new.len(), changed))
}

/// ファイルを読んで特徴量にする。失敗は種類だけの文面（パス・ファイル名・画像の内容を含まない）。
fn analyze_one(data_dir: &DataDir, display: &Value) -> Result<Features, String> {
    let Value::Text(rel) = display else {
        return Err("display が文字列ではありません".into());
    };
    let to_text = |e: ImageError| e.to_string();
    let path = data_dir
        .resolve_existing(rel)
        .map_err(ImageError::Path)
        .map_err(to_text)?;
    // 通常のファイルだけ読む（FIFO などで読み取りが止まらないように）
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err("通常のファイルではありません".into()),
        Err(e) => return Err(to_text(ImageError::Io(e.kind()))),
    }
    let bytes = crate::imaging::read_limited(&path).map_err(to_text)?;
    analyze_bytes(&bytes).map_err(to_text)
}

/// 未解析を最大 `limit` 件解析して書く。解析した数・失敗の数・停止で中断したかを返す。
fn analyze_pending(
    ctx: &StageCtx<'_>,
    limit: usize,
    analyze: Analyzer,
) -> Result<(u64, u64, bool), StageError> {
    let pending = pending_rows(ctx.read, limit)?;
    if pending.is_empty() {
        return Ok((0, 0, false));
    }
    let data_dir = DataDir::open(&ctx.config.data_dir)
        .map_err(|e| StageError::text(format!("データ置き場を開けません: {e}")))?;
    let mut rows = Vec::with_capacity(pending.len());
    let mut interrupted = false;
    for p in pending {
        if ctx.stop.is_stopped() {
            interrupted = true;
            break;
        }
        let result = analyze(&data_dir, &p.display);
        rows.push(FeatureRow { pending: p, result });
    }
    let failed = rows.iter().filter(|r| r.result.is_err()).count() as u64;
    let analyzed = rows.len() as u64;
    let analyzed_at = crate::sysinfo::local_isoformat_offset();
    ctx.writer()?
        .call(move |conn| write_features(conn, &rows, &analyzed_at))?;
    Ok((analyzed, failed, interrupted))
}

fn apply(ctx: &StageCtx<'_>, analyze: Analyzer) -> Result<StageOutcome, StageError> {
    let limit = limit_of(ctx);
    let pending_at_start = pending_count(ctx.read)?;
    let removed = ctx.writer()?.call(remove_orphans)? as u64;
    let (analyzed, failed, interrupted) = analyze_pending(ctx, limit, analyze)?;
    let mut counts = vec![
        ("removed", removed),
        ("pending", pending_at_start as u64),
        ("analyzed", analyzed),
        ("failed", failed),
    ];
    if interrupted || ctx.stop.is_stopped() {
        counts.push(("interrupted", 1));
        return Ok(StageOutcome::Done(Counts(counts)));
    }
    let stop = ctx.stop.clone();
    let analyzed_at = crate::sysinfo::local_isoformat_offset();
    let outcome = ctx
        .writer()?
        .call(move |conn| rebuild_if_ready(conn, &analyzed_at, &move || stop.is_stopped()))?;
    counts.push(("remaining", outcome.remaining as u64));
    counts.push(("rebuilt", u64::from(outcome.rebuilt)));
    counts.push(("rows", outcome.rows as u64));
    Ok(StageOutcome::Done(Counts(counts)))
}

fn limit_of(ctx: &StageCtx<'_>) -> usize {
    (ctx.config.quality_per_cycle as usize).max(1)
}

/// `all` は一括解析（`quality-scan`）: 未解析を全部解析する。
fn dry_run(ctx: &StageCtx<'_>, all: bool) -> Result<StageOutcome, StageError> {
    let limit = limit_of(ctx);
    let orphans = orphan_count(ctx.read)?;
    let pending = pending_count(ctx.read)?;
    let analyze = if all {
        pending
    } else {
        pending.min(limit as i64)
    };
    let remaining = pending - analyze;
    let dirty = is_dirty(ctx.read)?;
    let would_rebuild = remaining == 0 && (dirty || analyze > 0 || orphans > 0);
    let mut counts = vec![
        ("orphans", orphans as u64),
        ("pending", pending as u64),
        ("would_analyze", analyze as u64),
        ("remaining", remaining as u64),
        ("would_rebuild", u64::from(would_rebuild)),
    ];
    // 今回解析しないときは特徴量が確定しているので、再構築の結果を見積もれる
    if would_rebuild && analyze == 0 {
        let (rows, changed) = plan_rebuild(ctx.read)?;
        counts.push(("rows", rows as u64));
        counts.push(("changed", changed as u64));
    }
    Ok(StageOutcome::Done(Counts(counts)))
}

impl Stage for Quality {
    fn name(&self) -> &'static str {
        "quality"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        if ctx.is_dry_run() {
            dry_run(ctx, false)
        } else {
            apply(ctx, self.analyze)
        }
    }
}

/// `geoframe quality-scan`（現行の `server.py --quality-scan` の `quality_backfill`）: 孤児を消し、未解析が
/// なくなるまで `GEOFRAME_QUALITY_PER_CYCLE` 件ずつ解析し、再構築する。`--dry-run` は同期の段と同じ件数。
pub fn scan(ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
    scan_with(ctx, analyze_one)
}

fn scan_with(ctx: &StageCtx<'_>, analyze: Analyzer) -> Result<StageOutcome, StageError> {
    if ctx.is_dry_run() {
        return dry_run(ctx, true);
    }
    let limit = limit_of(ctx);
    let removed = ctx.writer()?.call(remove_orphans)? as u64;
    let (mut analyzed, mut failed) = (0_u64, 0_u64);
    loop {
        if ctx.stop.is_stopped() {
            return Ok(StageOutcome::Skipped(STOPPED));
        }
        let before = pending_count(ctx.read)?;
        let (n, f, interrupted) = analyze_pending(ctx, limit, analyze)?;
        analyzed += n;
        failed += f;
        if interrupted {
            return Ok(StageOutcome::Skipped(STOPPED));
        }
        if n == 0 {
            break;
        }
        // 書いたのに待ちが減らない（行が有効にならない）なら、無限に回さずに失敗にする
        let after = pending_count(ctx.read)?;
        if after >= before {
            return Err(StageError::fixed(
                "解析結果を書いても未解析が減りません（中断します）",
            ));
        }
        tracing::info!(
            "[quality] 一括解析: 解析 {analyzed} 件（失敗 {failed} 件、残り {after} 件）"
        );
    }
    let stop = ctx.stop.clone();
    let analyzed_at = crate::sysinfo::local_isoformat_offset();
    let outcome = ctx
        .writer()?
        .call(move |conn| rebuild_if_ready(conn, &analyzed_at, &move || stop.is_stopped()))?;
    Ok(StageOutcome::Done(Counts(vec![
        ("removed", removed),
        ("analyzed", analyzed),
        ("failed", failed),
        ("remaining", outcome.remaining as u64),
        ("rebuilt", u64::from(outcome.rebuilt)),
        ("rows", outcome.rows as u64),
    ])))
}
