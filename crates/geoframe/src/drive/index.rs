//! 同期サイクルの段 `index_drive`（現行 `drive.py:index_drive`）。Drive の一覧の `imageMediaMetadata` だけで、
//! ダウンロードせずに `photos`（`source='drive'`）を追加・更新・削除する。
//!
//! # 規則（現行と同じ）
//!
//! - 一覧は `'<folder>' in parents and trashed=false`（サブフォルダは辿らない）で、MIME が [`super::DRIVE_MIMES`]
//!   のものだけ。
//! - **追加**: `drive_id` の行が無いとき。`source='drive'`・`name`・`md5`・`taken_at`（Drive の時刻を
//!   [`imaging::norm_time`] に通す。無ければ空文字）・`lat`/`lon`（`latitude`/`longitude`。独立に取る）・
//!   `loc_source`（`lat` があれば `drive_meta`、無ければ NULL）・`indexed_at`。新しい行の `id` は一覧の順に振られる。
//! - **更新**: `drive_id` の行があって `md5` が違うとき。`name md5 taken_at lat lon loc_source` を上書きし、
//!   `place elev display fetched_at` を NULL、`rotation=0`、`indexed_at` を更新する。**ただし `loc_source='manual'`
//!   の行は `lat lon loc_source place elev` を書かない**（T3.7。ユーザーの決定。現行は手動の位置も上書きして失う。
//!   inventory.md §2.3 ①）。`md5` が同じなら何もしない（`name` が変わっていても）。
//!   `favorite hidden title inferred_from infer_dt_s muni_cd pref city` は触らない。
//! - **削除**: `source='drive'` の行で、一覧に無いもの。`display` の derivative も消す。
//!
//! # 現行との違い（安全側。server.md §0）
//!
//! - **一覧が全ページ取れたときだけ**計画を作る。途中で失敗したら DB に触れない（現行も例外で同じ。差ではない）。
//! - 一覧が **0 件**で、Drive の行が残っているときは、**削除しない**（共有を外した・フォルダを取り違えた
//!   ときも Drive は空の一覧を成功として返すので、全行の削除は、お気に入りや手動の位置まで消す）。
//! - **0 件以外でも、消す行が多すぎるとき**（消す件数が [`MASS_DELETE_MIN`] 件を超え、かつ Drive の行の
//!   [`MASS_DELETE_PERCENT`]% を超える。例: 6,535 件から 1 件だけ見える一覧）は、**削除だけを保留**して件数と
//!   警告を出す（追加・更新は通常どおり。`mass_delete_withheld`）。保留を外すのは**一度きりの明示的な操作**
//!   （管理画面の運用タブの承認＝`Allow::Only`、または `serve` が動いていないときの
//!   `geoframe sync --allow-mass-delete`）だけで、環境変数・設定・unit では与えられない（`serve` の定期実行は
//!   常に保留する）。同じ操作が 0 件の一覧の保留も外す。
//! - 書き込みは 1 つのトランザクション（追加・更新・削除をまとめる。現行は 1 文ごとのコミット）。
//! - 消す derivative の名前は `derivatives/<安全な名前>` だけ（現行は `DATA_DIR / display` を無検査で消す）。
//! - 一覧に `id` が無い（現行は例外で全体が失敗）・名前が無い（現行は途中で例外）ファイルは、前者は一覧の失敗、
//!   後者は索引に入れず（`malformed`）、行は「一覧にある」ものとして残す。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, params};

use super::{DriveError, DriveShared, RemoteFile, Session};
use crate::datadir::DataDir;
use crate::imaging;
use crate::local::remove_derivatives;
use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

/// 鍵が無いときの `Skipped` の理由（定型文）。
pub(super) const NO_KEY: &str = "Drive の鍵が無いので飛ばします";
/// 認証に失敗したときの理由。
pub(super) const AUTH_FAILED: &str = "Drive の認証に失敗したので飛ばします";
/// 停止の合図で中断したとき。
const STOPPED: &str = "停止の合図で中断しました";

/// 書き込みの途中で停止の合図を見る間隔（操作の件数）。
const STOP_CHECK_EVERY: usize = 256;

/// 消す件数がこれを**超え**、かつ Drive の行の [`MASS_DELETE_PERCENT`]% を**超える**と、削除を保留する。
/// 小さな写真集（数十件）の入れ替えでは止めず、手作業で 50 件以上まとめて消すことは普通ない、という判断。
pub const MASS_DELETE_MIN: usize = 50;
/// 上の割合（%）。6,535 件なら 1,307 件を超える削除が対象。1 件だけ見える一覧のような取り違えを止める。
pub const MASS_DELETE_PERCENT: usize = 20;

/// 大量削除を保留している理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// 消す行が [`MASS_DELETE_MIN`] 件を超え、かつ Drive の行の [`MASS_DELETE_PERCENT`]% を超える。
    Ratio,
    /// 一覧が 0 件で、Drive の行が残っている。
    EmptyListing,
}

impl HoldReason {
    /// `/api/admin/status` の `sync.mass_delete_hold.reason` の値。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ratio => "ratio",
            Self::EmptyListing => "empty_listing",
        }
    }
}

/// 保留した行の ID の集合（昇順・重複なし）。**メモリの中だけ**で、応答にもログにも出さない。
///
/// `Debug` は件数だけ（`HeldIds(len=120)`）で、`Serialize` は付けない（`{:?}` で千件の ID が journal に流れないため）。
/// `Arc` なので、保留の記憶を読むたびの複製は軽い。
#[derive(Clone, PartialEq, Eq, Default)]
pub struct HeldIds(Arc<[i64]>);

impl HeldIds {
    /// 昇順に並べて重複を除く。
    pub fn new(mut ids: Vec<i64>) -> Self {
        ids.sort_unstable();
        ids.dedup();
        Self(ids.into())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, id: i64) -> bool {
        self.0.binary_search(&id).is_ok()
    }

    /// 昇順の ID（印の計算と、集合に限った削除の判定に使う）。
    pub fn as_slice(&self) -> &[i64] {
        &self.0
    }
}

impl fmt::Debug for HeldIds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HeldIds(len={})", self.0.len())
    }
}

/// `index_drive` が大量削除の保留をどう扱うか。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Allow {
    /// 保留する（`serve` の定期実行）。
    #[default]
    No,
    /// 保留を外して消す（`serve` が動いていないときの `geoframe sync --allow-mass-delete` の 1 回だけ）。
    Yes,
    /// 承認した行の集合に限って消す（運用コンソールの「保留の解除」）。消える行がすべて集合の中のときだけ消し、
    /// 集合に無い行が 1 件でもあれば保留に戻す。取り直した一覧が空のときも集合に関係なく保留に戻す。
    Only(HeldIds),
}

/// 保留している削除（`/api/admin/status` の `sync.mass_delete_hold`。T3.7）。写真の名前・座標は持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MassDeleteHold {
    /// 保留している（消さなかった）行の数。直近の確認での数。
    pub held: u64,
    pub reason: HoldReason,
    /// そのときの Drive の行の数。
    pub drive_rows: u64,
    /// 保留が続き始めた時刻（`YYYY-MM-DD HH:MM:SS`。連続して保留している間は変わらない）。
    pub since: String,
    /// 直近に保留を確かめた時刻（同）。
    pub checked_at: String,
    /// 保留した行（消そうとした行）の ID。メモリの中だけ。応答には出さない（`/api/admin/status` の項目は手書きの 5 つ）。
    /// `Debug` は件数だけ。
    pub ids: HeldIds,
}

/// 大量削除の保留の記憶。**プロセスの中だけ**（再起動で消え、次のサイクルでまた付く）。`index_drive` が一覧を
/// 読んで計画を作るたびに書き直し、管理画面の状態（`/api/admin/status`）が読む。DB には書かない。
///
/// - 保留があれば `Some`、計画が保留なしだったら `None` に戻る（`geoframe sync --allow-mass-delete` か運用タブの解除で消したあと、
///   または Drive の一覧が元に戻ったあとの、**この `serve` の次のサイクル**）。
/// - 鍵が無い・認証に失敗・一覧の取得に失敗・停止の合図で中断したサイクルは何も確かめていないので、直前の記憶を変えない
///   （`checked_at` が古いままなので、画面では「最後に確かめた時刻」で分かる）。
/// - 取り込みが無効（`GEOFRAME_INGEST=0`）の `serve` には、この記憶自体が無い（`index_drive` が動かない）。
#[derive(Debug, Default)]
pub struct MassDeleteBoard {
    hold: std::sync::Mutex<Option<MassDeleteHold>>,
}

impl MassDeleteBoard {
    /// 今の保留（無ければ `None`）。
    pub fn current(&self) -> Option<MassDeleteHold> {
        self.hold
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 計画を作った結果を記録する（`index_drive` が呼ぶ。結合試験も使う）。`observed` は保留した `(行数, 理由, Drive の行数)`、無ければ保留なし。
    pub fn record(&self, observed: Option<(u64, HoldReason, u64)>, now: &str) {
        self.record_with_ids(observed, HeldIds::default(), now);
    }

    /// [`MassDeleteBoard::record`] に、保留した行の ID の集合を添える（`index_drive` は常にこちら）。
    pub fn record_with_ids(
        &self,
        observed: Option<(u64, HoldReason, u64)>,
        ids: HeldIds,
        now: &str,
    ) {
        let mut guard = self
            .hold
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = observed.map(|(held, reason, drive_rows)| MassDeleteHold {
            held,
            reason,
            drive_rows,
            // 連続して保留している間は、保留が始まった時刻のまま
            since: guard
                .as_ref()
                .map_or_else(|| now.to_string(), |old| old.since.clone()),
            checked_at: now.to_string(),
            ids,
        });
    }
}

/// 同期サイクルの段 `index_drive`。
pub struct IndexDrive {
    shared: Arc<DriveShared>,
    /// 大量削除の保留の扱い。
    allow: Allow,
    /// 解除前の写しの空き容量の調べ方（試験で差し替える）。
    free: fn(&Path) -> io::Result<u64>,
}

impl IndexDrive {
    pub fn new(shared: Arc<DriveShared>) -> Self {
        Self::with_allow(shared, Allow::No)
    }

    /// 大量削除の保留を外した段（`serve` が動いていないときの `geoframe sync --allow-mass-delete` の 1 回だけ）。
    pub fn allowing_mass_delete(shared: Arc<DriveShared>) -> Self {
        Self::with_allow(shared, Allow::Yes)
    }

    /// 承認した行の集合に限って消す段（運用コンソールの「保留の解除」）。[`Allow::Only`]。
    pub fn allowing_only(shared: Arc<DriveShared>, ids: HeldIds) -> Self {
        Self::with_allow(shared, Allow::Only(ids))
    }

    pub(crate) fn with_allow(shared: Arc<DriveShared>, allow: Allow) -> Self {
        Self {
            shared,
            allow,
            free: free_bytes,
        }
    }

    /// 空き容量の調べ方を差し替える（試験用）。
    #[cfg(test)]
    pub(super) fn with_free_space(mut self, free: fn(&Path) -> io::Result<u64>) -> Self {
        self.free = free;
        self
    }
}

/// DB の `md5` 列の値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Md5Cell {
    Null,
    Text(String),
    /// 文字列でも NULL でもない（想定外）。「変わった」とみなす。
    Other,
}

impl Md5Cell {
    /// 現行の `row["md5"] == f.get("md5Checksum")`（`None == None` は等しい）。
    fn same_as(&self, remote: &Option<String>) -> bool {
        match (self, remote) {
            (Self::Null, None) => true,
            (Self::Text(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

pub(super) struct ExistingRow {
    id: i64,
    source: String,
    drive_id: Option<String>,
    md5: Md5Cell,
    display: Option<String>,
    /// `loc_source='manual'`（手で直した位置。md5 が変わっても位置を書き換えない）。
    manual: bool,
}

/// 書き込む 1 行分（追加・更新に共通）。
#[derive(Clone, PartialEq)]
pub(super) struct Entry {
    pub drive_id: String,
    pub name: String,
    pub md5: Option<String>,
    pub taken_at: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

// 名前・撮影時刻・座標をログやパニックの表示に出さない（`{:?}` で出るのは ID の有無だけ）
impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("has_md5", &self.md5.is_some())
            .field("has_location", &self.lat.is_some())
            .finish_non_exhaustive()
    }
}

impl Entry {
    /// 現行の `"drive_meta" if lat is not None else None`。
    fn loc_source(&self) -> Option<&'static str> {
        self.lat.map(|_| "drive_meta")
    }
}

#[derive(Clone, PartialEq)]
pub(super) enum Op {
    Insert(Entry),
    Update(Entry),
}

impl fmt::Debug for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Insert(e) => f.debug_tuple("Insert").field(e).finish(),
            Self::Update(e) => f.debug_tuple("Update").field(e).finish(),
        }
    }
}

/// 計画（書き込む前）。
#[derive(Default, PartialEq)]
pub(super) struct DrivePlan {
    /// 一覧の順（`id` の採番順）。
    pub ops: Vec<Op>,
    /// 消す行 `(id, display)`。
    pub deletes: Vec<(i64, Option<String>)>,
    pub remote: u64,
    pub unchanged: u64,
    /// 名前が無くて索引に入れなかったファイル。
    pub malformed: u64,
    /// 一覧が空だったので消さなかった行の数。
    pub deletes_withheld: u64,
    /// 消す行が多すぎるので（[`MASS_DELETE_MIN`]・[`MASS_DELETE_PERCENT`]）消さなかった行の数。
    pub mass_delete_withheld: u64,
    /// 計画を作った時点の、DB の Drive の行の数（警告の文面に使う）。
    pub drive_rows: u64,
    /// 更新する行のうち、計画を作った時点で `loc_source='manual'` だったもの（位置と地名を書かない。T3.7）。
    /// 書く時点の判定は SQL の条件で行うので、計画のあとに手動の位置が付いた行はこの数に入らないが守られる。
    pub location_kept: u64,
    /// 保留した（消さなかった）行の ID。保留なしなら空。保留の記憶（[`MassDeleteHold::ids`]）に入れる。
    pub held_ids: HeldIds,
    /// [`Allow::Only`] のときだけ: 承認された行の数。
    pub approved: Option<u64>,
}

// 計画の中身（名前・座標を持つ `ops`）を出さず、件数だけを出す
impl fmt::Debug for DrivePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrivePlan")
            .field("ops", &self.ops.len())
            .field("deletes", &self.deletes.len())
            .field("remote", &self.remote)
            .field("unchanged", &self.unchanged)
            .field("malformed", &self.malformed)
            .field("deletes_withheld", &self.deletes_withheld)
            .field("mass_delete_withheld", &self.mass_delete_withheld)
            .field("location_kept", &self.location_kept)
            .field("held_ids", &self.held_ids)
            .field("approved", &self.approved)
            .finish()
    }
}

/// 消す行の数が、保留する大きさか。
fn is_mass_delete(doomed: usize, drive_rows: usize) -> bool {
    doomed > MASS_DELETE_MIN && doomed * 100 > drive_rows * MASS_DELETE_PERCENT
}

impl DrivePlan {
    /// 削除を保留したなら `(保留した行数, 理由, Drive の行数)`。
    fn hold(&self) -> Option<(u64, HoldReason, u64)> {
        if self.mass_delete_withheld > 0 {
            Some((
                self.mass_delete_withheld,
                HoldReason::Ratio,
                self.drive_rows,
            ))
        } else if self.deletes_withheld > 0 {
            Some((
                self.deletes_withheld,
                HoldReason::EmptyListing,
                self.drive_rows,
            ))
        } else {
            None
        }
    }

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

pub(super) fn load_existing(conn: &Connection) -> rusqlite::Result<Vec<ExistingRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, source, drive_id, md5, display, loc_source = 'manual' FROM photos
         WHERE drive_id IS NOT NULL OR source = 'drive'",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let md5 = match row.get_ref(3)? {
            ValueRef::Null => Md5Cell::Null,
            ValueRef::Text(t) => Md5Cell::Text(String::from_utf8_lossy(t).into_owned()),
            _ => Md5Cell::Other,
        };
        out.push(ExistingRow {
            id: row.get(0)?,
            source: row.get(1)?,
            // 型が想定外の `drive_id` は、一覧のどれとも一致しない（現行は `WHERE drive_id=?` に当たらない）
            drive_id: row.get::<_, Option<String>>(2).unwrap_or(None),
            md5,
            display: row.get::<_, Option<String>>(4).unwrap_or(None),
            manual: row
                .get::<_, Option<bool>>(5)
                .unwrap_or(None)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// 一覧と DB の行から計画を作る（副作用なし。実行と dry-run が同じ関数を通る）。`allow` は削除の保留
/// （0 件の一覧・大量削除）の扱いで、**最初に分岐する**: [`Allow::Yes`] は全部消す、[`Allow::Only`] は承認した集合に
/// 収まるときだけ消す、[`Allow::No`] は保留する。
pub(super) fn plan(existing: &[ExistingRow], files: &[RemoteFile], allow: &Allow) -> DrivePlan {
    let by_drive_id: HashMap<&str, &ExistingRow> = existing
        .iter()
        .filter_map(|row| row.drive_id.as_deref().map(|id| (id, row)))
        .collect();
    let drive_rows = existing.iter().filter(|row| row.source == "drive").count();
    let mut plan = DrivePlan {
        remote: files.len() as u64,
        drive_rows: drive_rows as u64,
        ..DrivePlan::default()
    };
    let mut remote_ids: HashSet<&str> = HashSet::with_capacity(files.len());
    for file in files {
        remote_ids.insert(&file.id);
        let Some(name) = &file.name else {
            plan.malformed += 1;
            continue;
        };
        let known = by_drive_id.get(file.id.as_str());
        if known.is_some_and(|row| row.md5.same_as(&file.md5)) {
            plan.unchanged += 1;
            continue;
        }
        let entry = Entry {
            drive_id: file.id.clone(),
            name: name.clone(),
            md5: file.md5.clone(),
            taken_at: imaging::norm_time(file.time.as_deref().unwrap_or("")),
            lat: file.lat,
            lon: file.lon,
        };
        if known.is_some_and(|row| row.manual) {
            plan.location_kept += 1;
        }
        plan.ops.push(if known.is_some() {
            Op::Update(entry)
        } else {
            Op::Insert(entry)
        });
    }
    // 一覧に無い Drive の行（`drive_id` が NULL の `source='drive'` も、現行は `not in remote` で消す）
    let doomed: Vec<(i64, Option<String>)> = existing
        .iter()
        .filter(|row| {
            row.source == "drive"
                && row
                    .drive_id
                    .as_deref()
                    .is_none_or(|id| !remote_ids.contains(id))
        })
        .map(|row| (row.id, row.display.clone()))
        .collect();
    let doomed_ids = || HeldIds::new(doomed.iter().map(|(id, _)| *id).collect());
    match allow {
        Allow::Yes => {
            plan.deletes = doomed;
            plan.deletes.sort_by_key(|(id, _)| *id);
        }
        Allow::Only(ids) => {
            plan.approved = Some(ids.len() as u64);
            if files.is_empty() && !doomed.is_empty() {
                // 承認は「一覧に何かあった」状態に対するもの。空の一覧は状況の変化なので、集合に関係なく保留に戻す
                plan.deletes_withheld = doomed.len() as u64;
                plan.held_ids = doomed_ids();
            } else if doomed.iter().all(|(id, _)| ids.contains(*id)) {
                // 承認した集合に収まる（一部が一覧に戻っていれば、残りだけ）
                plan.deletes = doomed;
                plan.deletes.sort_by_key(|(id, _)| *id);
            } else {
                // 承認していない行が 1 件でもあれば、件数が同じでも消さない
                plan.mass_delete_withheld = doomed.len() as u64;
                plan.held_ids = doomed_ids();
            }
        }
        Allow::No => {
            if files.is_empty() && !doomed.is_empty() {
                // 空の一覧は成功として返ることがある（共有を外した・フォルダの取り違え）。全行の削除はしない
                plan.deletes_withheld = doomed.len() as u64;
                plan.held_ids = doomed_ids();
            } else if is_mass_delete(doomed.len(), drive_rows) {
                // 0 件でなくても、ほとんどの行が消える一覧（一部しか見えない共有・フォルダの取り違え）は疑う
                plan.mass_delete_withheld = doomed.len() as u64;
                plan.held_ids = doomed_ids();
            } else {
                plan.deletes = doomed;
                plan.deletes.sort_by_key(|(id, _)| *id);
            }
        }
    }
    plan
}

/// 計画を `conn` に 1 つのトランザクションで書く。確定したら `true`。**確定の前に**停止の合図が来たら
/// （始めるとき・[`STOP_CHECK_EVERY`] 件ごと・`COMMIT` の直前）何も変えずに `false`（`ROLLBACK`）。
/// writer の接続と、dry-run の写しが同じ関数を通る。
pub(super) fn write_plan(
    conn: &Connection,
    now: &str,
    ops: &[Op],
    delete_ids: &[i64],
    stopped: &dyn Fn() -> bool,
) -> rusqlite::Result<bool> {
    if stopped() {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO photos(source, drive_id, name, md5, taken_at, lat, lon, loc_source, indexed_at)
             VALUES('drive', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        // `loc_source='manual'`（手で直した位置）の行は、位置と地名（`lat lon loc_source place elev`）を書かない
        // （T3.7）。判定は書く時点の行で行う（SQLite の `UPDATE` は右辺をすべて更新前の行で評価する）ので、
        // 計画を読んでから書くまでの間に更新 API が位置を直した行も守られる
        let mut update = tx.prepare(
            "UPDATE photos SET name=?1, md5=?2, taken_at=?3,
             lat=CASE WHEN loc_source = 'manual' THEN lat ELSE ?4 END,
             lon=CASE WHEN loc_source = 'manual' THEN lon ELSE ?5 END,
             loc_source=CASE WHEN loc_source = 'manual' THEN loc_source ELSE ?6 END,
             place=CASE WHEN loc_source = 'manual' THEN place END,
             elev=CASE WHEN loc_source = 'manual' THEN elev END,
             display=NULL, fetched_at=NULL, rotation=0, indexed_at=?7
             WHERE drive_id=?8",
        )?;
        let mut delete = tx.prepare("DELETE FROM photos WHERE id=?1")?;
        let steps = ops.len() + delete_ids.len();
        for i in 0..steps {
            if i % STOP_CHECK_EVERY == STOP_CHECK_EVERY - 1 && stopped() {
                return Ok(false); // `tx` は確定せずに drop される（ROLLBACK）
            }
            match ops.get(i) {
                Some(Op::Insert(e)) => insert.execute(params![
                    e.drive_id,
                    e.name,
                    e.md5,
                    e.taken_at,
                    e.lat,
                    e.lon,
                    e.loc_source(),
                    now
                ])?,
                Some(Op::Update(e)) => update.execute(params![
                    e.name,
                    e.md5,
                    e.taken_at,
                    e.lat,
                    e.lon,
                    e.loc_source(),
                    now,
                    e.drive_id
                ])?,
                None => delete.execute([delete_ids[i - ops.len()]])?,
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

fn counts(plan: &DrivePlan, pages: u32, removed_files: (u64, u64, u64)) -> Counts {
    let mut counts = Counts(vec![
        ("remote", plan.remote),
        ("pages", u64::from(pages)),
        ("added", plan.added()),
        ("updated", plan.updated()),
        ("removed", plan.deletes.len() as u64),
        ("unchanged", plan.unchanged),
        ("location_kept", plan.location_kept),
        ("malformed", plan.malformed),
        ("deletes_withheld", plan.deletes_withheld),
        ("mass_delete_withheld", plan.mass_delete_withheld),
        ("derivative_files_removed", removed_files.0),
        ("derivative_files_unsafe", removed_files.1),
        ("derivative_files_failed", removed_files.2),
    ]);
    // 承認した集合に限った解除（`Allow::Only`）のときだけ: 承認した件数と、実際に消した件数
    if let Some(approved) = plan.approved {
        counts.0.push(("approved", approved));
        counts.0.push(("deleted", plan.deletes.len() as u64));
    }
    counts
}

/// `DriveError` を段の失敗の文面にする（固定の語と状態コードだけ）。
pub(super) fn stage_error(e: DriveError) -> StageError {
    StageError::text(format!("Drive: {e}"))
}

/// 解除前の DB の写し（`tmp/catalog-before-release.db`）の名前。次の解除まで 1 つだけ残る。
const BACKUP_NAME: &str = "catalog-before-release.db";
/// 写しの書き込み中の名前。成功したときだけ [`BACKUP_NAME`] に `rename` する。
const BACKUP_PART_NAME: &str = ".catalog-before-release.db.part";
/// 写しを作るのに要る空き容量は、`catalog.db` の大きさのこの割合（%）。
const BACKUP_FREE_PERCENT: u64 = 120;

/// 解除前の写しを作れなかった理由（パスや SQLite の文面は持たない。固定の文だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackupError {
    /// `tmp/` を用意できない・古い `.part` を消せない。
    Prepare,
    /// 空き容量が `catalog.db` の 1.2 倍に足りない（または調べられない）。
    NoSpace,
    /// `VACUUM INTO` が失敗した。
    Copy,
    /// 完成した写しを置き換えられなかった。
    Install,
}

impl BackupError {
    fn message(self) -> &'static str {
        match self {
            Self::Prepare => "解除前の DB の写しを置く場所を用意できないので、削除しません",
            Self::NoSpace => "解除前の DB の写しを作る空き容量が足りないので、削除しません",
            Self::Copy => "解除前の DB の写しを作れなかったので、削除しません",
            Self::Install => "解除前の DB の写しを置けなかったので、削除しません",
        }
    }
}

/// 解除前の写しの置き場所。
#[derive(Debug, Clone)]
pub(super) struct BackupPaths {
    dir: PathBuf,
    db: PathBuf,
}

impl BackupPaths {
    fn part(&self) -> PathBuf {
        self.dir.join(BACKUP_PART_NAME)
    }

    fn done(&self) -> PathBuf {
        self.dir.join(BACKUP_NAME)
    }
}

/// `dir` の空き容量（バイト）。
fn free_bytes(dir: &Path) -> io::Result<u64> {
    crate::sysinfo::disk_usage(dir).map(|usage| usage.free)
}

/// 空き容量の見積もりの基準にする `catalog.db` の大きさ（バイト）。ファイル長と、接続から見える論理サイズ
/// （`page_count` × `page_size`。WAL にまだ書き戻していない増加分を含む）の大きいほう。
fn database_bytes(conn: &Connection, file_len: u64) -> rusqlite::Result<u64> {
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let logical = u64::try_from(pages)
        .unwrap_or(0)
        .saturating_mul(u64::try_from(page_size).unwrap_or(0));
    Ok(file_len.max(logical))
}

/// `.part` を空のファイルとして、本 DB と同じ権限で作る（作った時点から、見えてよい人だけが読める）。
/// 権限は umask で狭まることがあるので、作ったあとに確かめ、違えば（まだ空のうちに）合わせる。
fn create_part(part: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(part)?;
    let actual = file.metadata()?.permissions().mode() & 0o7777;
    if actual != mode {
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        if file.metadata()?.permissions().mode() & 0o7777 != mode {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
    }
    Ok(())
}

/// 大量削除の解除の直前に、`catalog.db` の写し `tmp/catalog-before-release.db` を作る。
///
/// 残っている `.part` を消す → 空き容量が `catalog.db`（WAL の増加分を含む論理サイズ）の 1.2 倍あるか確かめる →
/// `.part` を本 DB と同じ権限の空ファイルとして先に作る → トランザクションの外で `VACUUM INTO ?1`（パスは束縛。
/// 空のファイルには書ける）で `.part` に書く → 成功したら `rename`（前回の写しをここで初めて置き換える）。
/// 失敗したら `.part` を消し、前回の写しは残す。呼び出し側は失敗したら削除しない。
/// `conn` は writer の接続で、トランザクションの外（自動コミット）であること。
pub(super) fn backup_before_release(
    conn: &Connection,
    paths: &BackupPaths,
    free: &dyn Fn(&Path) -> io::Result<u64>,
) -> Result<(), BackupError> {
    let part = paths.part();
    match std::fs::remove_file(&part) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(BackupError::Prepare),
    }
    let db_meta = std::fs::metadata(&paths.db).map_err(|_| BackupError::Prepare)?;
    let size = database_bytes(conn, db_meta.len()).map_err(|_| BackupError::Prepare)?;
    let needed = size.saturating_mul(BACKUP_FREE_PERCENT) / 100;
    match free(&paths.dir) {
        Ok(available) if available >= needed => {}
        _ => return Err(BackupError::NoSpace),
    }
    let Some(part_text) = part.to_str() else {
        return Err(BackupError::Copy);
    };
    if create_part(&part, db_meta.permissions().mode() & 0o7777).is_err() {
        let _ = std::fs::remove_file(&part);
        return Err(BackupError::Prepare);
    }
    if conn.execute("VACUUM INTO ?1", [part_text]).is_err() {
        let _ = std::fs::remove_file(&part);
        return Err(BackupError::Copy);
    }
    if std::fs::rename(&part, paths.done()).is_err() {
        let _ = std::fs::remove_file(&part);
        return Err(BackupError::Install);
    }
    Ok(())
}

/// 削除の書き込みの結果（writer の依頼の中で決まる）。
enum Written {
    /// 確定した。
    Committed,
    /// 停止の合図で、何も変えずに終えた。
    Stopped,
    /// 写しを作れなかったので、何も書かなかった。
    BackupFailed(BackupError),
}

impl IndexDrive {
    pub(super) fn run_with(
        &self,
        ctx: &StageCtx<'_>,
        stopped: impl Fn() -> bool + Send + 'static,
    ) -> Result<StageOutcome, StageError> {
        self.shared.set_auth_failed(false);
        let key = &ctx.config.drive_key;
        if !key.exists() {
            return Ok(StageOutcome::Skipped(NO_KEY));
        }
        let session = Session::new(&self.shared, ctx.http, ctx.stop, key);
        // 現行の `drive_service()`（鍵の読み込み）にあたる。ここで失敗したら Drive の段をまとめて飛ばす
        match session.token() {
            Ok(_) => {}
            Err(DriveError::Stopped) => return Ok(StageOutcome::Skipped(STOPPED)),
            Err(e) if e.is_auth() => {
                tracing::warn!("[drive] 認証エラー: {e}");
                self.shared.set_auth_failed(true);
                return Ok(StageOutcome::Skipped(AUTH_FAILED));
            }
            Err(e) => return Err(stage_error(e)),
        }
        // 一覧の取得（ネットワークを読むだけ。dry-run でも行う）。途中で失敗したら DB には触れない
        let listing = match session
            .resolve_folder(&ctx.config.drive_folder)
            .and_then(|folder| session.list_folder(&folder))
        {
            Ok(listing) => listing,
            Err(DriveError::Stopped) => return Ok(StageOutcome::Skipped(STOPPED)),
            Err(e) => return Err(stage_error(e)),
        };
        let existing = load_existing(ctx.read)?;
        let plan = plan(&existing, &listing.files, &self.allow);
        // `Only`（解除）で保留が消える計画のときは、削除が確定してから消す（下）。写し・書き込みが失敗したときに、
        // 承認した保留の記憶（解除のボタン）まで消えないように。それ以外は計画の直後に記録する
        let clears_after_commit = matches!(self.allow, Allow::Only(_)) && plan.hold().is_none();
        if !ctx.is_dry_run() && !clears_after_commit {
            // 一覧を全部読めて計画ができたので、保留の有無を管理画面に見せる（T3.7）。
            // 失敗・省略したサイクルはここに来ないので、直前の記憶のまま
            self.shared.board.record_with_ids(
                plan.hold(),
                plan.held_ids.clone(),
                &crate::sysinfo::local_timestamp(),
            );
        }
        if plan.mass_delete_withheld > 0 {
            // 件数だけ（名前は出さない）。保留はサイクルごとに警告する
            tracing::warn!(
                "[drive] 消す行が多すぎます（Drive の行 {} 件のうち {} 件。一覧は {} 件）。削除は保留します。\
                 意図した削除なら管理画面の運用タブから解除してください（`serve` が動いていないときは `geoframe sync --allow-mass-delete` を一度だけ実行）",
                plan.drive_rows,
                plan.mass_delete_withheld,
                plan.remote
            );
        }
        if plan.deletes_withheld > 0 {
            tracing::warn!(
                "[drive] 一覧が 0 件でした。Drive の行 {} 件は削除しません（共有・フォルダの設定を確認してください）",
                plan.deletes_withheld
            );
        }
        if stopped() {
            return Ok(StageOutcome::Skipped(STOPPED));
        }
        let data = DataDir::open(&ctx.config.data_dir)?;
        let mut files_removed = (0, 0, 0);
        let changed = !plan.ops.is_empty() || !plan.deletes.is_empty();
        let ids: Vec<i64> = plan.deletes.iter().map(|(id, _)| *id).collect();
        if ctx.is_dry_run() {
            // 実 DB の写し（`ctx.read`）に計画を適用して、後続の段に索引後の状態を見せる。実行と同じ書き込み関数
            if changed {
                let now = crate::sysinfo::local_isoformat();
                write_plan(ctx.read, &now, &plan.ops, &ids, &|| false).map_err(|e| {
                    StageError::text(format!("dry-run の写しに計画を適用できません: {e}"))
                })?;
            }
            files_removed = remove_derivatives(&data, &plan.deletes, true);
        } else if changed {
            let writer = ctx.writer()?;
            let now = crate::sysinfo::local_isoformat();
            let ops = plan.ops.clone();
            // `Only` で消す行があるときだけ、削除の直前に DB の写しを作る（定期サイクルが毎回 DB 全体を写さないため）
            let free = self.free;
            let backup = if matches!(self.allow, Allow::Only(_)) && !ids.is_empty() {
                let tmp = data
                    .tmp_dir()
                    .map_err(|_| StageError::fixed(BackupError::Prepare.message()))?;
                Some(BackupPaths {
                    dir: tmp,
                    db: ctx.config.db_path(),
                })
            } else {
                None
            };
            // 行を消して確定してから、そのファイルを消す（先にファイルを消して失敗すると、行だけが残る）。
            // 写しと書き込みは同じ依頼の中（間に別の writer の依頼を挟まない）
            let written = writer
                .call(move |conn| {
                    if let Some(paths) = &backup {
                        if stopped() {
                            return Ok(Written::Stopped);
                        }
                        if let Err(e) = backup_before_release(conn, paths, &free) {
                            return Ok(Written::BackupFailed(e));
                        }
                    }
                    Ok(if write_plan(conn, &now, &ops, &ids, &stopped)? {
                        Written::Committed
                    } else {
                        Written::Stopped
                    })
                })
                .map_err(StageError::from)?;
            match written {
                Written::Committed => {}
                Written::Stopped => return Ok(StageOutcome::Skipped(STOPPED)),
                // 保留の記憶には触らない（承認した保留が残る）
                Written::BackupFailed(e) => return Err(StageError::fixed(e.message())),
            }
            files_removed = remove_derivatives(&data, &plan.deletes, false);
        }
        if clears_after_commit && !ctx.is_dry_run() {
            // 削除が確定した（または消す行・書く操作が無かった）ので、承認した保留を消す
            self.shared
                .board
                .record(None, &crate::sysinfo::local_timestamp());
        }
        if let (Allow::Only(_), false) = (&self.allow, ctx.is_dry_run()) {
            // 件数だけ（名前・ID は出さない）
            tracing::warn!(
                "[drive] 保留の解除を実行しました（承認 {} 件・削除 {} 件・保留に戻した {} 件）",
                plan.approved.unwrap_or(0),
                plan.deletes.len(),
                plan.mass_delete_withheld + plan.deletes_withheld
            );
        }
        Ok(StageOutcome::Done(counts(
            &plan,
            listing.pages,
            files_removed,
        )))
    }
}

impl Stage for IndexDrive {
    fn name(&self) -> &'static str {
        "index_drive"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        let stop = ctx.stop.clone();
        self.run_with(ctx, move || stop.is_stopped())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn row(id: i64) -> ExistingRow {
        ExistingRow {
            id,
            source: "drive".into(),
            drive_id: Some(format!("d{id}")),
            md5: Md5Cell::Text("m".into()),
            display: Some(format!("derivatives/d{id}.jpg")),
            manual: false,
        }
    }

    fn file(id: i64) -> RemoteFile {
        RemoteFile {
            id: format!("d{id}"),
            name: Some("n".into()),
            md5: Some("m".into()),
            time: None,
            lat: None,
            lon: None,
        }
    }

    /// Drive の行 `1..=rows` と、一覧に残る `keep`（ID の配列）。
    fn world(rows: i64, keep: &[i64]) -> (Vec<ExistingRow>, Vec<RemoteFile>) {
        (
            (1..=rows).map(row).collect(),
            keep.iter().copied().map(file).collect(),
        )
    }

    fn ids(range: std::ops::RangeInclusive<i64>) -> HeldIds {
        HeldIds::new(range.collect())
    }

    fn deleted_ids(plan: &DrivePlan) -> Vec<i64> {
        plan.deletes.iter().map(|(id, _)| *id).collect()
    }

    #[test]
    fn only_deletes_when_every_doomed_row_was_approved() {
        // 100 件から 1 件だけ見える。消える 2..=100 をすべて承認している
        let (existing, files) = world(100, &[1]);
        let plan = plan(&existing, &files, &Allow::Only(ids(2..=100)));
        assert_eq!(deleted_ids(&plan), (2..=100).collect::<Vec<_>>());
        assert_eq!((plan.deletes_withheld, plan.mass_delete_withheld), (0, 0));
        assert_eq!(plan.approved, Some(99));
        assert!(plan.hold().is_none());
        assert!(plan.held_ids.is_empty());
        let names: Vec<&str> = counts(&plan, 1, (0, 0, 0))
            .0
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert!(names.ends_with(&["approved", "deleted"]), "{names:?}");
    }

    #[test]
    fn only_deletes_just_the_rest_when_some_rows_came_back() {
        // 承認した 2..=100 のうち 50 が一覧に戻った: 残りの 98 件だけ消す
        let (existing, files) = world(100, &[1, 50]);
        let plan = plan(&existing, &files, &Allow::Only(ids(2..=100)));
        let expected: Vec<i64> = (2..=100).filter(|id| *id != 50).collect();
        assert_eq!(deleted_ids(&plan), expected);
        assert_eq!(plan.approved, Some(99));
        let counts = counts(&plan, 1, (0, 0, 0));
        let get = |name: &str| counts.0.iter().find(|(n, _)| *n == name).unwrap().1;
        assert_eq!(
            (get("approved"), get("deleted"), get("removed")),
            (99, 98, 98)
        );
    }

    #[test]
    fn only_withholds_everything_when_one_doomed_row_was_not_approved() {
        // 承認は 2..=99。100 も消えようとしている（件数は違っても、集合に無い行が 1 件でもあれば全部保留）
        let (existing, files) = world(100, &[1]);
        let plan = plan(&existing, &files, &Allow::Only(ids(2..=99)));
        assert!(plan.deletes.is_empty());
        assert_eq!((plan.mass_delete_withheld, plan.deletes_withheld), (99, 0));
        // 保留に戻した集合は、消えようとした行すべて（新しい印のもと）
        assert_eq!(plan.held_ids, ids(2..=100));
        assert_eq!(plan.hold(), Some((99, HoldReason::Ratio, 100)));
        // 件数が同じでも、別の行なら消さない
        let (existing, files) = world(100, &[1]);
        let other = HeldIds::new((3..=100).chain([1000]).collect());
        let plan = super::plan(&existing, &files, &Allow::Only(other));
        assert!(plan.deletes.is_empty());
        assert_eq!(plan.mass_delete_withheld, 99);
    }

    #[test]
    fn only_withholds_below_the_ratio_too_when_a_row_is_outside_the_set() {
        // 20 件だけ消える（No なら閾値未満で消す）が、承認した集合に無い行があるので保留に戻す
        let (existing, files) = world(100, &(1..=80).collect::<Vec<_>>());
        let no = plan(&existing, &files, &Allow::No);
        assert_eq!(no.deletes.len(), 20);
        let only = plan(&existing, &files, &Allow::Only(ids(82..=100)));
        assert!(only.deletes.is_empty());
        assert_eq!(only.mass_delete_withheld, 20);
        assert_eq!(only.held_ids, ids(81..=100));
        // 収まっていれば、閾値未満でも承認どおりに消す
        let only = plan(&existing, &files, &Allow::Only(ids(81..=100)));
        assert_eq!(only.deletes.len(), 20);
    }

    #[test]
    fn only_with_an_empty_listing_goes_back_to_the_hold_whatever_the_set_is() {
        let (existing, files) = world(100, &[]);
        let plan = plan(&existing, &files, &Allow::Only(ids(1..=100)));
        assert!(plan.deletes.is_empty());
        assert_eq!((plan.deletes_withheld, plan.mass_delete_withheld), (100, 0));
        assert_eq!(plan.hold(), Some((100, HoldReason::EmptyListing, 100)));
        assert_eq!(plan.held_ids, ids(1..=100));
        // 空の一覧でも消す行が無ければ、何も保留しない
        let plan = super::plan(&[], &[], &Allow::Only(ids(1..=3)));
        assert!(plan.hold().is_none() && plan.deletes.is_empty());
    }

    #[test]
    fn only_with_nothing_doomed_deletes_nothing_and_clears_the_hold() {
        let (existing, files) = world(100, &(1..=100).collect::<Vec<_>>());
        let plan = plan(&existing, &files, &Allow::Only(ids(2..=100)));
        assert!(plan.deletes.is_empty() && plan.hold().is_none());
        assert_eq!(plan.approved, Some(99));
    }

    #[test]
    fn yes_and_no_behave_as_the_flag_did() {
        let (existing, files) = world(100, &[1]);
        // No: 比率で保留（消す行の ID を覚える）
        let held = plan(&existing, &files, &Allow::No);
        assert!(held.deletes.is_empty());
        assert_eq!(held.mass_delete_withheld, 99);
        assert_eq!(held.held_ids, ids(2..=100));
        assert_eq!(held.approved, None);
        // Yes: 全部消す
        let all = plan(&existing, &files, &Allow::Yes);
        assert_eq!(deleted_ids(&all), (2..=100).collect::<Vec<_>>());
        assert_eq!((all.mass_delete_withheld, all.deletes_withheld), (0, 0));
        assert!(all.held_ids.is_empty());
        // 空の一覧: No は保留、Yes は全部消す
        let (existing, files) = world(3, &[]);
        let no = plan(&existing, &files, &Allow::No);
        assert_eq!((no.deletes.len(), no.deletes_withheld), (0, 3));
        assert_eq!(no.held_ids, ids(1..=3));
        assert_eq!(plan(&existing, &files, &Allow::Yes).deletes.len(), 3);
        // 閾値未満の削除は No でも通る
        let (existing, files) = world(100, &(1..=50).collect::<Vec<_>>());
        assert_eq!(plan(&existing, &files, &Allow::No).deletes.len(), 50);
        // approved / deleted は Only のときだけ
        let names: Vec<&str> = counts(&held, 1, (0, 0, 0))
            .0
            .iter()
            .map(|(n, _)| *n)
            .collect();
        assert!(!names.contains(&"approved") && !names.contains(&"deleted"));
    }

    #[test]
    fn held_ids_are_sorted_unique_and_debug_shows_only_the_count() {
        let held = HeldIds::new(vec![30, 10, 20, 10]);
        assert_eq!(held.as_slice(), &[10, 20, 30]);
        assert_eq!(held.len(), 3);
        assert!(held.contains(20) && !held.contains(21));
        assert_eq!(format!("{held:?}"), "HeldIds(len=3)");
        assert!(HeldIds::default().is_empty());
        let allow = Allow::Only(HeldIds::new((500_000..500_100).collect()));
        let shown = format!("{allow:?}");
        assert!(
            shown.contains("len=100") && !shown.contains("500000"),
            "{shown}"
        );
        let (existing, files) = world(100, &[1]);
        let shown = format!("{:?}", plan(&existing, &files, &Allow::No));
        assert!(shown.contains("len=99"), "{shown}");
    }

    // 解除前の写し -----------------------------------------------------------------

    struct BackupEnv {
        dir: TempDir,
        conn: Connection,
        paths: BackupPaths,
    }

    fn backup_env(tag: &str) -> BackupEnv {
        let dir = TempDir::new(tag);
        let db = dir.path().join("catalog.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE photos (id INTEGER PRIMARY KEY, loc_source TEXT);
             INSERT INTO photos(loc_source) VALUES ('manual'), ('drive_meta');",
        )
        .unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        BackupEnv {
            dir,
            conn,
            paths: BackupPaths { dir: tmp, db },
        }
    }

    fn plenty(_: &Path) -> io::Result<u64> {
        Ok(u64::MAX)
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn copy_rows(path: &Path) -> Vec<String> {
        let copy =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut stmt = copy
            .prepare("SELECT loc_source FROM photos ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn the_copy_is_written_to_part_then_renamed_and_replaces_the_previous_one() {
        let e = backup_env("backup-ok");
        // 前回の写しと、中断の残りの `.part`
        std::fs::write(e.paths.done(), b"old").unwrap();
        std::fs::write(e.paths.part(), b"stale").unwrap();
        backup_before_release(&e.conn, &e.paths, &plenty).unwrap();
        assert_eq!(names_in(&e.paths.dir), [BACKUP_NAME]);
        assert_eq!(copy_rows(&e.paths.done()), ["manual", "drive_meta"]);
        let _ = &e.dir;
    }

    #[test]
    fn a_short_disk_stops_before_writing_and_keeps_the_previous_copy() {
        let e = backup_env("backup-space");
        std::fs::write(e.paths.done(), b"old").unwrap();
        let size = std::fs::metadata(&e.paths.db).unwrap().len();
        // 1.2 倍にあと 1 バイト足りない
        let short = move |_: &Path| Ok(size * 120 / 100 - 1);
        assert_eq!(
            backup_before_release(&e.conn, &e.paths, &short),
            Err(BackupError::NoSpace)
        );
        // ちょうど足りれば作る
        let exact = move |_: &Path| Ok(size * 120 / 100);
        // 調べられないときも作らない（安全側）
        let unknown = |_: &Path| Err(io::Error::from(io::ErrorKind::Other));
        std::fs::write(e.paths.done(), b"old").unwrap();
        assert_eq!(
            backup_before_release(&e.conn, &e.paths, &unknown),
            Err(BackupError::NoSpace)
        );
        assert_eq!(std::fs::read(e.paths.done()).unwrap(), b"old");
        assert_eq!(names_in(&e.paths.dir), [BACKUP_NAME]);
        backup_before_release(&e.conn, &e.paths, &exact).unwrap();
        assert_eq!(copy_rows(&e.paths.done()).len(), 2);
    }

    #[test]
    fn a_part_that_cannot_be_created_stops_before_copying_and_keeps_the_previous_copy() {
        let e = backup_env("backup-inway");
        std::fs::write(e.paths.done(), b"old").unwrap();
        // 空き容量の確認の最中に `.part` が現れた状況（`create_new` は既存のファイルを断る）を作る
        let part = e.paths.part();
        let meddle = move |_: &Path| {
            std::fs::write(&part, b"in the way").unwrap();
            Ok(u64::MAX)
        };
        assert_eq!(
            backup_before_release(&e.conn, &e.paths, &meddle),
            Err(BackupError::Prepare)
        );
        assert_eq!(std::fs::read(e.paths.done()).unwrap(), b"old");
        assert_eq!(names_in(&e.paths.dir), [BACKUP_NAME]);
    }

    #[test]
    fn a_failing_copy_removes_the_part_and_keeps_the_previous_copy() {
        let e = backup_env("backup-fail");
        std::fs::write(e.paths.done(), b"old").unwrap();
        // `VACUUM` はトランザクションの中では動かない
        e.conn.execute_batch("BEGIN").unwrap();
        assert_eq!(
            backup_before_release(&e.conn, &e.paths, &plenty),
            Err(BackupError::Copy)
        );
        e.conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(std::fs::read(e.paths.done()).unwrap(), b"old");
        assert_eq!(names_in(&e.paths.dir), [BACKUP_NAME]);
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn the_copy_has_the_same_permissions_as_the_database() {
        for mode in [0o600, 0o640, 0o644] {
            let e = backup_env("backup-mode");
            std::fs::set_permissions(&e.paths.db, std::fs::Permissions::from_mode(mode)).unwrap();
            backup_before_release(&e.conn, &e.paths, &plenty).unwrap();
            assert_eq!(mode_of(&e.paths.done()), mode, "{mode:o}");
            assert_eq!(copy_rows(&e.paths.done()).len(), 2);
        }
    }

    #[test]
    fn the_part_is_created_empty_with_the_requested_permissions_whatever_the_umask() {
        let dir = TempDir::new("backup-create-part");
        // 0o666 は普通の umask（022 など）で狭まる: 作ったあとに合わせる
        for mode in [0o600, 0o640, 0o666] {
            let part = dir.path().join(format!("part-{mode:o}"));
            create_part(&part, mode).unwrap();
            assert_eq!(mode_of(&part), mode, "{mode:o}");
            assert_eq!(std::fs::metadata(&part).unwrap().len(), 0);
            // 既にあるものは断る
            assert!(create_part(&part, mode).is_err());
        }
    }

    #[test]
    fn the_space_estimate_includes_what_the_wal_has_not_written_back() {
        let e = backup_env("backup-wal");
        e.conn
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE blob (b BLOB);",
            )
            .unwrap();
        e.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let file_len = std::fs::metadata(&e.paths.db).unwrap().len();
        let before = database_bytes(&e.conn, file_len).unwrap();
        for _ in 0..50 {
            e.conn
                .execute("INSERT INTO blob VALUES (zeroblob(65536))", [])
                .unwrap();
        }
        // 本体のファイルは増えず、WAL だけが増える
        assert_eq!(std::fs::metadata(&e.paths.db).unwrap().len(), file_len);
        let logical = database_bytes(&e.conn, file_len).unwrap();
        assert!(logical > before + 50 * 65536 / 2, "{before} -> {logical}");
        // ファイル長の 1.2 倍だけでは足りず、論理サイズの 1.2 倍なら作れる
        let file_only = move |_: &Path| Ok(file_len * 120 / 100);
        assert_eq!(
            backup_before_release(&e.conn, &e.paths, &file_only),
            Err(BackupError::NoSpace)
        );
        let enough = move |_: &Path| Ok(logical * 120 / 100);
        backup_before_release(&e.conn, &e.paths, &enough).unwrap();
        // ファイル長が論理サイズより大きいとき（空きページがあるなど）は、ファイル長のほうを使う
        assert_eq!(database_bytes(&e.conn, u64::MAX / 2).unwrap(), u64::MAX / 2);
    }

    #[test]
    fn a_leftover_part_that_cannot_be_removed_stops_before_anything() {
        let e = backup_env("backup-stuck");
        std::fs::write(e.paths.done(), b"old").unwrap();
        // 空でないディレクトリは `remove_file` で消せない（誰で動かしても同じ）
        std::fs::create_dir_all(e.paths.part().join("inner")).unwrap();
        assert_eq!(
            backup_before_release(&e.conn, &e.paths, &plenty),
            Err(BackupError::Prepare)
        );
        assert_eq!(std::fs::read(e.paths.done()).unwrap(), b"old");
    }

    #[test]
    fn backup_errors_say_nothing_about_paths() {
        for e in [
            BackupError::Prepare,
            BackupError::NoSpace,
            BackupError::Copy,
            BackupError::Install,
        ] {
            let text = e.message();
            assert!(!text.contains('/') && !text.contains(".db"), "{text}");
        }
    }
}
