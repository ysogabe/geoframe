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
//! 残りは次の通常の同期に回す（進めるため、1 バッチ目だけは必ず試す）。
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
use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior, params};

use super::data::DataError;
use super::index::{Method, RegionIndex};
use super::search::{classifier_version, search_text};
use super::table;
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
/// 開始時刻からの経過時間（試験は時計を差し替える）。
type Elapsed = Box<dyn Fn(Instant) -> Duration + Send + Sync>;

/// 候補 1 枚。
#[derive(Debug, Clone, Copy)]
struct Candidate {
    id: i64,
    lat: f64,
    lon: f64,
}

/// 判定の結果（索引を借りない形）。
#[derive(Debug, Clone)]
struct Verdict {
    country_id: Option<String>,
    region_id: Option<String>,
    country: Method,
    region: Method,
    search_text: String,
}

impl Verdict {
    fn inside(&self) -> bool {
        self.country == Method::Inside
    }
}

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
            elapsed: Box::new(|start| start.elapsed()),
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
        self.elapsed = Box::new(elapsed);
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

    /// 候補（先頭から `per_cycle` 枚まで）と、候補の総数。`have_table` が偽（dry-run で表が無い）なら、分類の有無を見ずに
    /// `pref` 空で座標が有効な全件が候補。
    fn select(
        &self,
        conn: &Connection,
        have_table: bool,
        version: &str,
    ) -> rusqlite::Result<(Vec<Candidate>, u64)> {
        let filter = if have_table {
            format!(" AND {}", table::needs_classification("p", version))
        } else {
            String::new()
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT p.id, p.lat, p.lon FROM photos p WHERE {}{filter} ORDER BY p.id",
            table::classifiable("p")
        ))?;
        let mut rows = stmt.query([])?;
        let mut candidates = Vec::new();
        let mut total = 0u64;
        while let Some(row) = rows.next()? {
            total += 1;
            if candidates.len() < self.per_cycle {
                candidates.push(Candidate {
                    id: row.get(0)?,
                    lat: row.get(1)?,
                    lon: row.get(2)?,
                });
            }
        }
        Ok((candidates, total))
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
            let n = writer.call(|conn| {
                conn.execute(
                    "DELETE FROM photo_regions WHERE photo_id IN (
                       SELECT r.photo_id FROM photo_regions r
                        WHERE NOT EXISTS (SELECT 1 FROM photos p WHERE p.id=r.photo_id) LIMIT ?1)",
                    [CLEANUP_BATCH as i64],
                )
            })?;
            if n == 0 {
                break;
            }
            tally.removed += n as u64;
        }
        Ok(())
    }

    /// 判定する（同じ座標は `memo` で 1 回）。座標が無効・国と地域の所属が名称表と合わない結果は `None`。
    fn verdict(
        index: &RegionIndex,
        memo: &mut HashMap<(u64, u64), Option<Verdict>>,
        c: &Candidate,
    ) -> Option<Verdict> {
        memo.entry((c.lat.to_bits(), c.lon.to_bits()))
            .or_insert_with(|| {
                let found = index.classify(c.lat, c.lon).ok()?;
                // 国と地域の所属関係は、検索用文字列を作る名称表で確かめる（合わなければ書かない）
                let text = match found.country_id {
                    Some(country) => search_text(country, found.region_id)?,
                    None => String::new(),
                };
                Some(Verdict {
                    country_id: found.country_id.map(str::to_owned),
                    region_id: found.region_id.map(str::to_owned),
                    country: found.country_method,
                    region: found.region_method,
                    search_text: text,
                })
            })
            .clone()
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
                it.verdict.country.as_str(),
                it.verdict.region.as_str(),
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
        let (candidates, pending) = self.select(ctx.read, have_table, &literal)?;
        tally.pending = pending;

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
            let mut memo: HashMap<(u64, u64), Option<Verdict>> = HashMap::new();
            for (n, chunk) in candidates.chunks(self.batch.max(1)).enumerate() {
                // 1 バッチ目は必ず試す（時間の上限が小さくても進める）
                if n > 0 && (ctx.stop.is_stopped() || self.out_of_time(started)) {
                    break;
                }
                let mut items = Vec::with_capacity(chunk.len());
                for c in chunk {
                    match Self::verdict(&index, &mut memo, c) {
                        Some(verdict) => items.push(Item {
                            candidate: *c,
                            verdict,
                        }),
                        None => tally.failed += 1,
                    }
                }
                if dry_run {
                    for it in &items {
                        if it.verdict.inside() {
                            tally.inside += 1;
                        } else {
                            tally.unresolved += 1;
                        }
                    }
                    continue;
                }
                let writer = ctx.writer()?;
                let version = version.clone();
                let kinds: Vec<bool> = items.iter().map(|it| it.verdict.inside()).collect();
                let applied = writer.call(move |conn| apply(conn, &version, &items));
                match applied {
                    Ok(applied) => {
                        for (applied, inside) in applied.into_iter().zip(kinds) {
                            match (applied, inside) {
                                (false, _) => tally.stale_input += 1,
                                (true, true) => tally.inside += 1,
                                (true, false) => tally.unresolved += 1,
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
                tally.removed = ctx.read.query_row(
                    "SELECT COUNT(*) FROM photo_regions r
                      WHERE NOT EXISTS (SELECT 1 FROM photos p WHERE p.id=r.photo_id)",
                    [],
                    |row| row.get::<_, i64>(0),
                )? as u64;
            }
        } else {
            self.clean_up(ctx, started, &mut tally)?;
        }
        Ok(StageOutcome::Done(tally.counts(dry_run)))
    }
}
