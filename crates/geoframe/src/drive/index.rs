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
//!   `geoframe sync --allow-mass-delete` だけで、環境変数・設定・unit では与えられない（`serve` の定期実行は
//!   常に保留する）。同じ操作が 0 件の一覧の保留も外す。
//! - 書き込みは 1 つのトランザクション（追加・更新・削除をまとめる。現行は 1 文ごとのコミット）。
//! - 消す derivative の名前は `derivatives/<安全な名前>` だけ（現行は `DATA_DIR / display` を無検査で消す）。
//! - 一覧に `id` が無い（現行は例外で全体が失敗）・名前が無い（現行は途中で例外）ファイルは、前者は一覧の失敗、
//!   後者は索引に入れず（`malformed`）、行は「一覧にある」ものとして残す。

use std::collections::{HashMap, HashSet};
use std::fmt;
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
}

/// 大量削除の保留の記憶。**プロセスの中だけ**（再起動で消え、次のサイクルでまた付く）。`index_drive` が一覧を
/// 読んで計画を作るたびに書き直し、管理画面の状態（`/api/admin/status`）が読む。DB には書かない。
///
/// - 保留があれば `Some`、計画が保留なしだったら `None` に戻る（`geoframe sync --allow-mass-delete` で消したあと、
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
        });
    }
}

/// 同期サイクルの段 `index_drive`。
pub struct IndexDrive {
    shared: Arc<DriveShared>,
    /// 大量削除の保留を外す（`geoframe sync --allow-mass-delete` のときだけ true）。
    allow_mass_delete: bool,
}

impl IndexDrive {
    pub fn new(shared: Arc<DriveShared>) -> Self {
        Self {
            shared,
            allow_mass_delete: false,
        }
    }

    /// 大量削除の保留を外した段（`geoframe sync --allow-mass-delete` の 1 回だけ）。
    pub fn allowing_mass_delete(shared: Arc<DriveShared>) -> Self {
        Self {
            shared,
            allow_mass_delete: true,
        }
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

/// 一覧と DB の行から計画を作る（副作用なし。実行と dry-run が同じ関数を通る）。`allow_mass_delete` は
/// 削除の保留（0 件の一覧・大量削除）を外す。
pub(super) fn plan(
    existing: &[ExistingRow],
    files: &[RemoteFile],
    allow_mass_delete: bool,
) -> DrivePlan {
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
    if allow_mass_delete {
        plan.deletes = doomed;
        plan.deletes.sort_by_key(|(id, _)| *id);
    } else if files.is_empty() && !doomed.is_empty() {
        // 空の一覧は成功として返ることがある（共有を外した・フォルダの取り違え）。全行の削除はしない
        plan.deletes_withheld = doomed.len() as u64;
    } else if is_mass_delete(doomed.len(), drive_rows) {
        // 0 件でなくても、ほとんどの行が消える一覧（一部しか見えない共有・フォルダの取り違え）は疑う
        plan.mass_delete_withheld = doomed.len() as u64;
    } else {
        plan.deletes = doomed;
        plan.deletes.sort_by_key(|(id, _)| *id);
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
    Counts(vec![
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
    ])
}

/// `DriveError` を段の失敗の文面にする（固定の語と状態コードだけ）。
pub(super) fn stage_error(e: DriveError) -> StageError {
    StageError::text(format!("Drive: {e}"))
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
        let plan = plan(&existing, &listing.files, self.allow_mass_delete);
        if !ctx.is_dry_run() {
            // 一覧を全部読めて計画ができたので、保留の有無を管理画面に見せる（T3.7）。
            // 失敗・省略したサイクルはここに来ないので、直前の記憶のまま
            self.shared
                .board
                .record(plan.hold(), &crate::sysinfo::local_timestamp());
        }
        if plan.mass_delete_withheld > 0 {
            // 件数だけ（名前は出さない）。保留はサイクルごとに警告する
            tracing::warn!(
                "[drive] 消す行が多すぎます（Drive の行 {} 件のうち {} 件。一覧は {} 件）。削除は保留します。\
                 意図した削除なら `geoframe sync --allow-mass-delete` を一度だけ実行してください",
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
            // 行を消して確定してから、そのファイルを消す（先にファイルを消して失敗すると、行だけが残る）
            let committed = writer
                .call(move |conn| write_plan(conn, &now, &ops, &ids, &stopped))
                .map_err(StageError::from)?;
            if !committed {
                return Ok(StageOutcome::Skipped(STOPPED));
            }
            files_removed = remove_derivatives(&data, &plan.deletes, false);
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
