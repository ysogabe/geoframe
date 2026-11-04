//! `regions` 同期段: `pref` が空の写真の座標を同梱の境界データで分類し、`photo_regions` へ保存する。
//! 設計は docs/international-places/design.md の「同期・初回移行・読み取り専用」。
//!
//! # 流れ（1 サイクル）
//!
//! 1. 候補を読む: `pref` が空（NULL か空文字）で座標が有効、かつ**有効な分類がまだ無い**（分類行が無い・座標が合わない・
//!    分類版が違う）写真。`outside`/`ambiguous` も処理済みとして保存するので、同じ版・同じ座標では再び選ばれない。
//!    無効な座標の写真は候補にしない（毎回枠を占有しない）。
//! 2. 候補が 1 件以上のときだけ [`RegionIndex`] を作り（候補 0 件なら作らない）、サイクルの終わりに手放す。
//! 3. 候補を最大 [`BATCH`] 枚ずつ判定し（同じ座標は 1 回）、writer の 1 つのトランザクションで適用する。適用は**現在の
//!    写真を照合する**（写真が存在し、座標が計画時と同じで、`pref` がまだ空）。条件が変わった行は書かず `stale_input` に数える。
//!
//! 4. 索引を手放したあと、写真が無くなった分類行を短いバッチで消す（掃除。[`CLEANUP_BATCH`]。書き込みの待ちを減らすため判定の後）。
//!
//! 1 サイクルの上限は [`PER_CYCLE`] 枚・[`BUDGET`]。停止の合図・時間の上限は**バッチの前**に見て、適用済みの結果は残し、
//! 残りは次の通常の同期に回す（進めるため、1 バッチ目だけは必ず試す）。[`BUDGET`] は**バッチの境目で見る目安**で、1 バッチの
//! 判定・適用の途中では中断しない（1 バッチは [`BATCH`] 枚で短い）。候補の走査は SQLite の進捗ハンドラで見て、上限を過ぎるか
//! 停止の合図があれば中断する（ただし 1 バッチ目の分の候補が見つかるまでは続け、進めなくなるのを避ける）。
//!
//! # 判定できなかった座標（`failed`）
//!
//! 国と地域の所属が名称表と合わないなど、判定そのものができない座標は、結果として**書かない**ことにすると毎回候補に戻り、
//! ID 順に選ぶので先頭の 1 万枚が恒久的に失敗すると後ろへ進めない。そこで失敗も `country_method`/`region_method` = `failed` の行
//! （入力座標・分類版つき）として保存し、同じ座標・同じ版では選び直さない（版が変われば選び直す）。`failed` の行は有効な分類
//! ではなく、読み取りの [`table::join`] には一致しない（`unclassified` 扱い）。
//!
//! # `--dry-run`
//!
//! 実 DB・写しの DB・永続キャッシュ・画像に書かない。通信しない。表があれば今の行と比べて候補を数え、なければ（写しにも表を作らず）
//! `pref` が空で座標が有効な全件を処理待ちとして数える。どちらもオフラインで判定まで行い、件数を返す。
//!
//! # ログ
//!
//! 件数と固定の文面だけ。写真 ID・座標・地名・パスは出さない（[`StageError`] も件数以外を持たない）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};

use super::data::DataError;
use super::index::{Method, RegionIndex};
use super::search::{classifier_version, search_text};
use super::table;
use crate::stop::StopFlag;
use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

/// 1 サイクルに処理する写真の最大枚数。
pub const PER_CYCLE: usize = 10_000;
/// 1 サイクルの時間の上限（目安。バッチの境目で見る）。
pub const BUDGET: Duration = Duration::from_secs(5);
/// writer に渡す 1 回の適用の最大枚数（1 トランザクション）。
pub const BATCH: usize = 100;
/// 掃除の 1 回の削除の最大行数。
pub const CLEANUP_BATCH: usize = 100;

/// 境界データから索引を作る関数（試験は合成の索引を返す）。
type Loader = Box<dyn Fn() -> Result<RegionIndex, DataError> + Send + Sync>;
/// 開始時刻からの経過時間（試験は時計を差し替える）。進捗ハンドラへ渡すので `Arc`。
type Elapsed = Arc<dyn Fn(Instant) -> Duration + Send + Sync>;

/// 候補の走査で、SQLite が進捗ハンドラを呼ぶ間隔（仮想機械の命令数）。
const PROGRESS_OPS: i32 = 1_000;

/// 候補 1 枚。
#[derive(Debug, Clone, Copy)]
pub(super) struct Candidate {
    id: i64,
    lat: f64,
    lon: f64,
}

/// 判定の結果の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// 国が 1 つに決まった。
    Inside,
    /// 国が `outside`/`ambiguous`（保存はする）。
    Unresolved,
    /// 判定できなかった（印だけ保存する）。
    Failed,
}

/// 判定の結果（索引を借りない形）。
#[derive(Debug, Clone)]
struct Verdict {
    country_id: Option<String>,
    region_id: Option<String>,
    /// DB に保存する判定方法（`failed` は判定できなかった印）。
    country: &'static str,
    region: &'static str,
    search_text: String,
    outcome: Outcome,
}

impl Verdict {
    /// 判定できなかった座標の印。国・地域・検索用文字列は空。
    fn failed() -> Self {
        Self {
            country_id: None,
            region_id: None,
            country: FAILED,
            region: FAILED,
            search_text: String::new(),
            outcome: Outcome::Failed,
        }
    }
}

/// 判定できなかった印の保存値（[`table::SCHEMA`] の CHECK と同じ）。
const FAILED: &str = "failed";

/// 適用する 1 行。
struct Item {
    candidate: Candidate,
    verdict: Verdict,
}

#[derive(Default)]
struct Tally {
    pending: u64,
    inside: u64,
    unresolved: u64,
    stale_input: u64,
    removed: u64,
    failed: u64,
    /// 候補の走査を時間の上限か停止で打ち切ったら 1（`pending` は数えた分までの下限）。
    scan_incomplete: u64,
}

impl Tally {
    fn handled(&self) -> u64 {
        self.inside + self.unresolved + self.stale_input + self.failed
    }

    fn counts(&self, dry_run: bool) -> Counts {
        let deferred = self.pending.saturating_sub(self.handled());
        let mut v = vec![
            ("pending", self.pending),
            ("inside", self.inside),
            ("unresolved", self.unresolved),
        ];
        if !dry_run {
            v.push(("stale_input", self.stale_input));
        }
        v.extend([
            ("removed", self.removed),
            ("failed", self.failed),
            ("deferred", deferred),
            ("scan_incomplete", self.scan_incomplete),
        ]);
        Counts(v)
    }
}

/// `regions` 段。
pub struct Regions {
    /// 分類版。`None` は同梱の版（[`classifier_version`]）。
    version: Option<String>,
    loader: Loader,
    per_cycle: usize,
    budget: Duration,
    batch: usize,
    elapsed: Elapsed,
    progress_ops: i32,
}

impl Default for Regions {
    fn default() -> Self {
        Self::new()
    }
}

impl Regions {
    /// 本番の値（同梱の境界データ・[`PER_CYCLE`]・[`BUDGET`]・[`BATCH`]）。
    pub fn new() -> Self {
        Self {
            version: None,
            loader: Box::new(RegionIndex::load),
            per_cycle: PER_CYCLE,
            budget: BUDGET,
            batch: BATCH,
            elapsed: Arc::new(|start| start.elapsed()),
            progress_ops: PROGRESS_OPS,
        }
    }

    /// 試験用: 索引の作り方と分類版を差し替える（合成の索引・版の更新の試験）。
    #[cfg(test)]
    pub(super) fn with_index(
        version: &str,
        loader: impl Fn() -> Result<RegionIndex, DataError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            version: Some(version.to_string()),
            loader: Box::new(loader),
            ..Self::new()
        }
    }

    /// 試験用: 上限を差し替える。
    #[cfg(test)]
    pub(super) fn with_limits(mut self, per_cycle: usize, budget: Duration, batch: usize) -> Self {
        self.per_cycle = per_cycle;
        self.budget = budget;
        self.batch = batch;
        self
    }

    /// 試験用: 時計を差し替える。
    #[cfg(test)]
    pub(super) fn with_clock(
        mut self,
        elapsed: impl Fn(Instant) -> Duration + Send + Sync + 'static,
    ) -> Self {
        self.elapsed = Arc::new(elapsed);
        self
    }

    /// 試験用: 進捗ハンドラを呼ぶ間隔（命令数）を小さくして、小さな DB でも候補の走査を中断できるようにする。
    #[cfg(test)]
    pub(super) fn with_progress_ops(mut self, ops: i32) -> Self {
        self.progress_ops = ops;
        self
    }

    fn version(&self) -> Result<String, StageError> {
        match &self.version {
            Some(v) => Ok(v.clone()),
            None => classifier_version()
                .map(str::to_string)
                .map_err(|_| StageError::fixed("同梱の境界データの分類版を得られません")),
        }
    }

    fn out_of_time(&self, started: Instant) -> bool {
        (self.elapsed)(started) >= self.budget
    }

    /// 候補（先頭から `per_cycle` 枚まで）と、数えた候補の総数と、走査を途中で打ち切ったか。`have_table` が偽（dry-run で表が無い）なら、
    /// 分類の有無を見ずに `pref` 空で座標が有効な全件が候補。
    ///
    /// 走査は進捗ハンドラで見る。停止の合図があるか、時間の上限を過ぎたら中断し、そこまでの候補（ID 順の先頭側）と、そこまでで数えた
    /// 総数（下限）を返す。ただし 1 バッチ目の分（[``batch``]・`per_cycle` の小さい方）の候補が揃うまでは中断しない。
    /// 候補があるのに毎回走査で時間切れになり、一度も進めなくなるのを避けるため。
    pub(super) fn select(
        &self,
        conn: &Connection,
        have_table: bool,
        version: &str,
        started: Instant,
        stop: &StopFlag,
    ) -> rusqlite::Result<(Vec<Candidate>, u64, bool)> {
        let filter = if have_table {
            format!(" AND {}", table::needs_classification("p", version))
        } else {
            String::new()
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT p.id, p.lat, p.lon FROM photos p WHERE {}{filter} ORDER BY p.id",
            table::classifiable("p")
        ))?;
        let collected = Arc::new(AtomicUsize::new(0));
        let _guard = ProgressGuard::set(conn, self.progress_ops, {
            let (collected, stop, elapsed) =
                (Arc::clone(&collected), stop.clone(), self.elapsed.clone());
            let (budget, first) = (self.budget, self.batch.max(1).min(self.per_cycle));
            move || {
                stop.is_stopped()
                    || (collected.load(Ordering::Relaxed) >= first && elapsed(started) >= budget)
            }
        })?;
        let mut rows = stmt.query([])?;
        let mut candidates = Vec::new();
        let mut total = 0u64;
        loop {
            match rows.next() {
                Ok(Some(row)) => {
                    total += 1;
                    if candidates.len() < self.per_cycle {
                        candidates.push(Candidate {
                            id: row.get(0)?,
                            lat: row.get(1)?,
                            lon: row.get(2)?,
                        });
                        collected.store(candidates.len(), Ordering::Relaxed);
                    }
                }
                Ok(None) => return Ok((candidates, total, false)),
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == ErrorCode::OperationInterrupted =>
                {
                    return Ok((candidates, total, true));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 写真が無くなった分類行を消す（Apply）。`removed` に数える。
    fn clean_up(
        &self,
        ctx: &StageCtx<'_>,
        started: Instant,
        tally: &mut Tally,
    ) -> Result<(), StageError> {
        let writer = ctx.writer()?;
        let mut first = true;
        while (tally.removed as usize) < self.per_cycle {
            if !first && (ctx.stop.is_stopped() || self.out_of_time(started)) {
                break;
            }
            first = false;
            let limit = CLEANUP_BATCH.min(self.per_cycle - tally.removed as usize) as i64;
            let n = writer.call(move |conn| {
                conn.execute(
                    "DELETE FROM photo_regions WHERE photo_id IN (
                       SELECT r.photo_id FROM photo_regions r
                        WHERE NOT EXISTS (SELECT 1 FROM photos p WHERE p.id=r.photo_id) LIMIT ?1)",
                    [limit],
                )
            })?;
            if n == 0 {
                break;
            }
            tally.removed += n as u64;
        }
        Ok(())
    }

    /// 判定する（同じ座標は `memo` で 1 回）。座標が無効・国と地域の所属が名称表と合わない結果は [`Verdict::failed`]。
    fn verdict(
        index: &RegionIndex,
        memo: &mut HashMap<(u64, u64), Verdict>,
        c: &Candidate,
    ) -> Verdict {
        memo.entry((c.lat.to_bits(), c.lon.to_bits()))
            .or_insert_with(|| {
                let Ok(found) = index.classify(c.lat, c.lon) else {
                    return Verdict::failed();
                };
                // 国と地域の所属関係は、検索用文字列を作る名称表で確かめる（合わなければ判定できなかった印だけ書く）
                let text = match found.country_id {
                    Some(country) => match search_text(country, found.region_id) {
                        Some(text) => text,
                        None => return Verdict::failed(),
                    },
                    None => String::new(),
                };
                Verdict {
                    country_id: found.country_id.map(str::to_owned),
                    region_id: found.region_id.map(str::to_owned),
                    country: found.country_method.as_str(),
                    region: found.region_method.as_str(),
                    search_text: text,
                    outcome: if found.country_method == Method::Inside {
                        Outcome::Inside
                    } else {
                        Outcome::Unresolved
                    },
                }
            })
            .clone()
    }
}

/// 候補の走査の間だけ SQLite の進捗ハンドラを付ける（共有の読み取り接続に残さない）。
struct ProgressGuard<'c>(&'c Connection);

impl<'c> ProgressGuard<'c> {
    fn set(
        conn: &'c Connection,
        ops: i32,
        handler: impl FnMut() -> bool + Send + 'static,
    ) -> rusqlite::Result<Self> {
        conn.progress_handler(ops, Some(handler))?;
        Ok(Self(conn))
    }
}

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

/// 1 バッチを書く。写真が存在し、座標が計画時と同じで、`pref` がまだ空の行だけ UPSERT する。各行を書いたか（`false` は条件が
/// 変わったので捨てた）を返す。
fn apply(conn: &mut Connection, version: &str, items: &[Item]) -> rusqlite::Result<Vec<bool>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = {
        let mut stmt = tx.prepare(&format!(
            "INSERT INTO photo_regions(photo_id, input_lat, input_lon, classifier_version, country_id, region_id,
                                       search_text, country_method, region_method, classified_at)
             SELECT p.id, p.lat, p.lon, ?4, ?5, ?6, ?7, ?8, ?9, strftime('%Y-%m-%d %H:%M:%S','now')
               FROM photos p
              WHERE p.id=?1 AND p.lat=?2 AND p.lon=?3 AND {}
             ON CONFLICT(photo_id) DO UPDATE SET
               input_lat=excluded.input_lat, input_lon=excluded.input_lon,
               classifier_version=excluded.classifier_version,
               country_id=excluded.country_id, region_id=excluded.region_id,
               search_text=excluded.search_text,
               country_method=excluded.country_method, region_method=excluded.region_method,
               classified_at=excluded.classified_at",
            table::classifiable("p")
        ))?;
        let mut applied = Vec::with_capacity(items.len());
        for it in items {
            let n = stmt.execute(params![
                it.candidate.id,
                it.candidate.lat,
                it.candidate.lon,
                version,
                it.verdict.country_id,
                it.verdict.region_id,
                it.verdict.search_text,
                it.verdict.country,
                it.verdict.region,
            ])?;
            applied.push(n > 0);
        }
        applied
    };
    tx.commit()?;
    Ok(applied)
}

impl Stage for Regions {
    fn name(&self) -> &'static str {
        "regions"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        let started = Instant::now();
        let dry_run = ctx.is_dry_run();
        let version = self.version()?;
        let literal = table::version_literal(&version)
            .ok_or_else(|| StageError::fixed("分類版の形式が不正です"))?;
        let have_table = table::available(ctx.read)?;
        if !dry_run && !have_table {
            // 表は書き込みの起動時に作る。黙って作らない
            return Err(StageError::fixed(
                "photo_regions 表がありません（書き込みの起動時に作られます）",
            ));
        }
        let mut tally = Tally::default();

        // 1. 候補
        let (candidates, pending, cut) =
            self.select(ctx.read, have_table, &literal, started, ctx.stop)?;
        tally.pending = pending;
        tally.scan_incomplete = u64::from(cut);
        if cut {
            tracing::info!(
                "[regions] 候補の走査を時間の上限か停止で打ち切りました（数えた分 {} 枚。残りは次回）",
                pending
            );
        }

        // 2. 判定と適用（候補が無ければ索引を作らない。作った索引はこの関数の終わりに手放す）
        if !candidates.is_empty() && !ctx.stop.is_stopped() {
            let built = Instant::now();
            let index = (self.loader)()
                .map_err(|_| StageError::fixed("同梱の境界データから索引を作れません"))?;
            tracing::info!(
                "[regions] 境界データの索引を作りました（{} ms、候補 {} 枚）",
                built.elapsed().as_millis(),
                candidates.len()
            );
            let mut memo: HashMap<(u64, u64), Verdict> = HashMap::new();
            for (n, chunk) in candidates.chunks(self.batch.max(1)).enumerate() {
                // 1 バッチ目は必ず試す（時間の上限が小さくても進める）
                if n > 0 && (ctx.stop.is_stopped() || self.out_of_time(started)) {
                    break;
                }
                let items: Vec<Item> = chunk
                    .iter()
                    .map(|c| Item {
                        candidate: *c,
                        verdict: Self::verdict(&index, &mut memo, c),
                    })
                    .collect();
                if dry_run {
                    for it in &items {
                        match it.verdict.outcome {
                            Outcome::Inside => tally.inside += 1,
                            Outcome::Unresolved => tally.unresolved += 1,
                            Outcome::Failed => tally.failed += 1,
                        }
                    }
                    continue;
                }
                let writer = ctx.writer()?;
                let version = version.clone();
                let kinds: Vec<Outcome> = items.iter().map(|it| it.verdict.outcome).collect();
                let applied = writer.call(move |conn| apply(conn, &version, &items));
                match applied {
                    Ok(applied) => {
                        for (applied, outcome) in applied.into_iter().zip(kinds) {
                            match (applied, outcome) {
                                (false, _) => tally.stale_input += 1,
                                (true, Outcome::Inside) => tally.inside += 1,
                                (true, Outcome::Unresolved) => tally.unresolved += 1,
                                (true, Outcome::Failed) => tally.failed += 1,
                            }
                        }
                    }
                    Err(e) => {
                        tally.failed += kinds.len() as u64;
                        // 適用できなかった分は次のサイクルでまた候補になる
                        return Err(StageError::text(format!(
                            "{e}（{}）",
                            tally.counts(dry_run)
                        )));
                    }
                }
            }
            drop(memo);
            drop(index);
        }

        // 3. 掃除（索引を手放したあと。dry-run は消さず、消すはずの行を数える）
        if dry_run {
            if have_table {
                // 実行（Apply）と同じ 1 サイクルの上限で数える（孤児が上限より多くても、実行が消す数を超えない）
                tally.removed = ctx.read.query_row(
                    "SELECT COUNT(*) FROM (SELECT 1 FROM photo_regions r
                      WHERE NOT EXISTS (SELECT 1 FROM photos p WHERE p.id=r.photo_id) LIMIT ?1)",
                    [self.per_cycle as i64],
                    |row| row.get::<_, i64>(0),
                )? as u64;
            }
        } else {
            self.clean_up(ctx, started, &mut tally)?;
        }
        Ok(StageOutcome::Done(tally.counts(dry_run)))
    }
}
