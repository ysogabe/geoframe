//! 位置の推定（段 `infer`）。現行 `geo.py:infer_locations`。
//!
//! # 規則（現行と同じ）
//!
//! - **錨**: `loc_source IN ('exif','drive_meta','manual')` で `taken_at` が空でない写真のうち、時刻が解釈できるもの。
//!   `inferred` は錨にしない（推定が推定を呼ばない）。`manual` は錨になる（手動で 1 枚直すと、同じ旅の近い時刻の
//!   未解決の写真に波及する）。
//! - **対象**: `loc_source IS NULL OR loc_source = 'inferred'` の写真。`manual`・`exif`・`drive_meta` は再推定しない。
//! - 対象の時刻に `GEOFRAME_INFER_OFFSET_MIN`（カメラ時計のずれ）を**1 回だけ**足し、時刻の差が最小の錨を選ぶ。
//!   差が最大の窓（[`crate::config::Config::infer_widest_s`]。既定は 12 時間）以内なら `lat lon` を錨のものにし、
//!   `loc_source='inferred'`・`inferred_from`＝錨の `id`・`infer_dt_s`＝差の秒（切り捨て）にする。
//!   段階窓（15 分→2 時間→12 時間）は「差を含む最小の窓」に振り分けるのと同値なので、1 回の最近傍探索で済む。
//!   窓のラベルは `infer_dt_s` から表示側が計算する（`loc_source` は常に `inferred`）。
//! - **同順位**（差が同じ錨が複数）は、**`id` が小さい方**（現行は `ORDER BY` 無しの全件走査で、`min` が最初の
//!   最小を返す。実際は `rowid` 順）。順序を `ORDER BY id` に固定する。
//! - **取り消し**: 錨が消えた・窓から外れた・時刻が読めなくなった対象で `lat` が入っているものは、
//!   `lat lon loc_source inferred_from infer_dt_s display` を NULL にする（`display` を NULL にするので、Drive の写真は
//!   `fetch_derivatives` が取り直す）。**derivative のファイルは消さない**。
//! - **地名の列（`place elev muni_cd pref city`）には触らない**（現行と同じ）。錨が別の写真に変わって位置が動いても、
//!   取り消して別の場所に推定し直しても、`place` は古いまま残る。位置が動いたときに地名を直すかは P3 で決める
//!   （server.md §0）。
//!
//! # 現行との違い
//!
//! - 値が変わらない行には書かない（現行は毎サイクル全対象に同じ値を `UPDATE` する）。最終の行は同じ。
//!   `--dry-run` の件数（`updated`・`cancelled`）が「ほぼ 0」と読めるのは、これがあるため。
//! - 書き込みは 1 つのトランザクション。`UPDATE` は `loc_source IS NULL OR 'inferred'` のままの行にだけ効く
//!   （計画を作ってから書くまでの間に、手動で位置が付いた行を上書きしない）。

use rusqlite::{Connection, params, types::Value};

use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

use super::naive_seconds;

/// 同期サイクルの段 `infer`。
pub struct Infer;

/// 停止の合図で中断したときの `Skipped` の理由（定型文）。
const STOPPED: &str = "停止の合図で中断しました";
/// 書き込みの途中で停止の合図を見る間隔（操作の件数）。
const STOP_CHECK_EVERY: usize = 256;

struct Anchor {
    /// naive な日時の通算秒。
    t: f64,
    id: i64,
    /// 錨の値をそのまま写す（現行は Python の値をそのまま `UPDATE` に渡す）。
    lat: Value,
    lon: Value,
}

/// 対象の写真の、今の値（変わらないなら書かないための比較用）。
struct Current {
    lat: Value,
    lon: Value,
    loc_source: Option<String>,
    inferred_from: Value,
    infer_dt_s: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Op {
    Set {
        id: i64,
        lat: Value,
        lon: Value,
        from: i64,
        dt_s: i64,
    },
    Clear {
        id: i64,
    },
}

/// 計画と件数。
#[derive(Debug, Default)]
pub(super) struct Plan {
    pub ops: Vec<Op>,
    /// 錨の数（時刻が解釈できたもの）。
    pub anchors: u64,
    /// 対象（`loc_source` が NULL か `inferred`）の数。
    pub targets: u64,
    /// 新しく位置が付く（今は `lat` が無い。現行の戻り値 `n`）。
    pub inferred: u64,
    /// すでに推定済みで、錨や差が変わる（位置が動くものを含む）。
    pub updated: u64,
    /// すでに推定済みで、値が変わらない（書かない）。
    pub unchanged: u64,
    /// 推定を取り消す。
    pub cancelled: u64,
    /// 時刻が無い・解釈できない対象（推定できない）。
    pub untimed: u64,
}

impl Plan {
    fn counts(&self) -> Counts {
        Counts(vec![
            ("anchors", self.anchors),
            ("targets", self.targets),
            ("inferred", self.inferred),
            ("updated", self.updated),
            ("unchanged", self.unchanged),
            ("cancelled", self.cancelled),
            ("untimed", self.untimed),
        ])
    }
}

fn text(value: Value) -> Option<String> {
    match value {
        Value::Text(s) => Some(s),
        _ => None,
    }
}

/// 計画を作る（DB は読むだけ）。
pub(super) fn plan(conn: &Connection, offset_s: f64, widest_s: f64) -> rusqlite::Result<Plan> {
    let mut anchors: Vec<Anchor> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, taken_at, lat, lon FROM photos
             WHERE loc_source IN ('exif','drive_meta','manual') AND taken_at != ''
             ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            // 時刻が文字列でない・解釈できない錨は使わない（現行の `t_epoch` が `None`）
            let Some(t) = text(row.get(1)?).as_deref().and_then(naive_seconds) else {
                continue;
            };
            anchors.push(Anchor {
                t,
                id: row.get(0)?,
                lat: row.get(2)?,
                lon: row.get(3)?,
            });
        }
    }
    let mut plan = Plan {
        anchors: anchors.len() as u64,
        ..Plan::default()
    };
    let mut stmt = conn.prepare(
        "SELECT id, taken_at, lat, lon, loc_source, inferred_from, infer_dt_s FROM photos
         WHERE loc_source IS NULL OR loc_source = 'inferred'
         ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        plan.targets += 1;
        let id: i64 = row.get(0)?;
        let time = text(row.get(1)?).as_deref().and_then(naive_seconds);
        let current = Current {
            lat: row.get(2)?,
            lon: row.get(3)?,
            loc_source: text(row.get(4)?),
            inferred_from: row.get(5)?,
            infer_dt_s: row.get(6)?,
        };
        if time.is_none() {
            plan.untimed += 1;
        }
        // 最も近い錨。同順位は先（`id` が小さい方）。`<` なので、後から来た同じ差の錨は選ばない
        let best = time.and_then(|time| {
            let te = time + offset_s; // オフセットの加算は 1 回だけ
            let mut best: Option<(&Anchor, f64)> = None;
            for anchor in &anchors {
                let d = (anchor.t - te).abs();
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((anchor, d));
                }
            }
            // 最大の窓に入らなければ推定しない
            best.filter(|(_, d)| *d <= widest_s)
        });
        let located = !matches!(current.lat, Value::Null);
        match best {
            Some((anchor, d)) => {
                // `int(dt)`: 差は 0 以上なので切り捨て
                let dt_s = d as i64;
                if !located {
                    plan.inferred += 1;
                } else if current.loc_source.as_deref() == Some("inferred")
                    && current.lat == anchor.lat
                    && current.lon == anchor.lon
                    && current.inferred_from == Value::Integer(anchor.id)
                    && current.infer_dt_s == Value::Integer(dt_s)
                {
                    plan.unchanged += 1;
                    continue;
                } else {
                    plan.updated += 1;
                }
                plan.ops.push(Op::Set {
                    id,
                    lat: anchor.lat.clone(),
                    lon: anchor.lon.clone(),
                    from: anchor.id,
                    dt_s,
                });
            }
            // 錨が消えた等 → 推定を取り消し
            None if located => {
                plan.cancelled += 1;
                plan.ops.push(Op::Clear { id });
            }
            None => {}
        }
    }
    Ok(plan)
}

/// 計画を `conn` に 1 つのトランザクションで書く。確定したら `true`。確定の前に停止の合図が来たら
/// （始めるとき・[`STOP_CHECK_EVERY`] 件ごと・`COMMIT` の直前）何も変えずに `false`（`ROLLBACK`）。
/// writer の接続と、dry-run の写し（[`crate::sync::simulation_copy`]）が同じ関数を通る。
pub(super) fn write_plan(
    conn: &Connection,
    ops: &[Op],
    stopped: &dyn Fn() -> bool,
) -> rusqlite::Result<bool> {
    if ops.is_empty() {
        return Ok(true);
    }
    if stopped() {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut set = tx.prepare(
            "UPDATE photos SET lat=?1, lon=?2, loc_source='inferred', inferred_from=?3, infer_dt_s=?4
             WHERE id=?5 AND (loc_source IS NULL OR loc_source='inferred')",
        )?;
        let mut clear = tx.prepare(
            "UPDATE photos SET lat=NULL, lon=NULL, loc_source=NULL, inferred_from=NULL,
             infer_dt_s=NULL, display=NULL
             WHERE id=?1 AND (loc_source IS NULL OR loc_source='inferred')",
        )?;
        for (i, op) in ops.iter().enumerate() {
            if i % STOP_CHECK_EVERY == STOP_CHECK_EVERY - 1 && stopped() {
                return Ok(false); // `tx` は確定せずに drop される（ROLLBACK）
            }
            match op {
                Op::Set {
                    id,
                    lat,
                    lon,
                    from,
                    dt_s,
                } => set.execute(params![lat, lon, from, dt_s, id])?,
                Op::Clear { id } => clear.execute([id])?,
            };
        }
    }
    if stopped() {
        tx.rollback()?;
        return Ok(false);
    }
    tx.commit()?;
    Ok(true)
}

impl Stage for Infer {
    fn name(&self) -> &'static str {
        "infer"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        let plan = plan(
            ctx.read,
            ctx.config.infer_offset_s(),
            ctx.config.infer_widest_s(),
        )?;
        let stop = ctx.stop.clone();
        let stopped = move || stop.is_stopped();
        if ctx.is_dry_run() {
            // 実 DB の写し（`ctx.read`）に計画を当てて、後続の段に推定後の状態を見せる。実行と同じ書き込み関数
            write_plan(ctx.read, &plan.ops, &|| false).map_err(|e| {
                StageError::text(format!("dry-run の写しに計画を適用できません: {e}"))
            })?;
        } else if !plan.ops.is_empty() {
            let ops = plan.ops.clone();
            let committed = ctx
                .writer()?
                .call(move |conn| write_plan(conn, &ops, &stopped))?;
            if !committed {
                return Ok(StageOutcome::Skipped(STOPPED));
            }
        }
        Ok(StageOutcome::Done(plan.counts()))
    }
}
