//! ジョブ全体の流れ（design.md 4.1 の手順 2〜11）と、止まったジョブの復旧（4.2「復旧」）、`dry_run`。
//!
//! 呼ぶのは、実行枠（[`crate::lock::SlotGuard`]）を**持ったまま**の側だけ（[`execute`]・[`recover_unfinished`] は、枠を持つ印として
//! `&SlotGuard` を受け取る）。サービスの受付と `run` コマンドは [`crate::service`] を通る。
//!
//! ## ジョブの流れ（[`execute`]）
//!
//! 1. 作業領域の親（`staging/`）の残骸を消し、**自分以外の未完了のジョブを復旧する**（[`recover_unfinished`]。復旧は待つことが
//!    あるので、受付の排他の外のここで行う）。復旧で `blocked` が出たら、このジョブは `failed`（`blocked`）で終える。
//! 2. 一覧 API の取得と検査（`feed_unavailable`・`feed_invalid`・`too_many_photos`）。
//! 3. 作成待ちの写真があれば `failed`（`derivative_pending`。Drive にも作業領域にも触れない）。
//! 4. 候補が 0 枚なら `succeeded`・`count: 0`・`uploaded: false`（Drive にも作業領域にも触れない。宛先が既にあっても同じ）。
//! 5. 作業領域を作り、[`crate::image::prepare`]（失敗は、そこまでの数も結果に載せる。全部除外なら `nothing_to_export` で、
//!    Drive は照会しない）。
//! 6. [`crate::publish::publish`]（宛先の状態と `conflict` の判定、公開）。段階はジョブの記録へ書く（[`Recorder`]）。
//! 7. `succeeded`・`conflict`・`failed`・`blocked` を記録する。公開の記録（`published/`）に書けなかったときは、実行枠を
//!    持ったまま、次のジョブより前に書き直し（[`crate::publish::Published::retry_record`]）、それでも書けなければ `notes` に残す。
//! 8. 作業領域を消す（[`crate::staging::Workspace`] を手放す。成功・失敗・パニックのいずれも）。
//!
//! ジョブ全体の期限は [`JOB_TIMEOUT`]（60 分）。`prepare` と Drive の操作に同じ期限を掛ける（過ぎれば `timeout`）。期限は作業の開始
//! （手順 1 の復旧が済んだあと）から数える。期限を過ぎたあとの、安全のための復旧（4.2 の復旧の表）は、Drive の 1 回ごとの操作の期限
//! （`--drive-timeout`）と待ちの回数の上限（[`Timing`]）の中で行い、全体の期限では打ち切らない（打ち切ると、公開の途中の
//! 状態が決まらないまま残る）。
//!
//! ## パニックとキャンセル（SIGINT・SIGTERM）
//!
//! どちらも、Drive の操作を含む段階（[`JobRecord::progress`] が記録にある）まで進んでいたかで分ける。段階が無ければ Drive に
//! 触れていないので、パニックは `failed`（`internal`）、キャンセルは `interrupted`。
//!
//! - **パニック**（[`execute`] が `catch_unwind` で受ける）: 段階が記録にあれば、無条件に `failed` を確定しない（退避の直後なら、
//!   宛先が空のまま復旧の対象から外れる）。**実行枠を持ったまま、その場で復旧の表を当てる**（[`recover_one`]。起動時・各ジョブの
//!   前の復旧と同じ）。根拠は、**ディスクの記録を読み直したもの**（段階と ID）だけで、パニックの時点のメモリの状態は信じない。
//!   記録が無い・読めないときは、Drive の操作も記録の書き込みもせず、そのまま残す（読めない記録は上書きしない。次の起動で
//!   `blocked` として扱われる）。復旧の中でまたパニックしたら、記録を `running` のまま残して次の復旧に渡す。
//! - 一覧の取得の前後でも、キャンセルの印を見る（段階が未記録なので `interrupted`。候補が 0 枚でも `succeeded` にしない）。
//! - **キャンセル**（[`Cancel`] の印。[`crate::cancel`]）: Drive の操作は続けられない（印が立つと、要求の前・再試行の待ち・チャンクの間で止まる。
//!   飛んでいる要求は終わるまで待ち、アップロードのスレッドを全部 join してから失敗を返す）ので、その場で復旧の表は当てられない。段階が記録にあれば、記録を**確定せず `running` のまま**
//!   次の復旧（次のジョブの前・起動時・`recover`）に渡す。印のもとで返ってくる失敗・`blocked`・中断・知らせの付いた成功は、
//!   止めたことの副産物（照合できなかった、`incoming` を片付けられず残りものになった、など）なので、記録に確定しない
//!   （[`cancel_voids`]）。確定するのは、印の前に済んだ `succeeded`（知らせなし）・`conflict` だけ。
//!
//!   パニックを「その場で復旧」、キャンセルを「`running` のまま」と分けた理由: パニックは、プロセスが続く（`serve`）ので、
//!   `running` のまま残すと、次の受付まで GET が `running` を返し続ける。キャンセルはプロセスが終わるところで、Drive の操作を
//!   続けられない。
//!
//!   代償: `running` の記録は、次の復旧まで `running` のまま（`serve` では次の受付か再起動まで。退避の直後なら、その間は
//!   宛先が空）。実行枠は、アップロードのスレッドを全部止めて join し終えるまで外れない。
//!
//! ## 結果の数
//!
//! `catalog_count` は候補の数、`count` は公開した写真の数（成功のときだけ。`conflict`・`failed` では 0 で、`bytes` も 0）、
//! `excluded` は仕様上の除外（id と理由）、`bytes` は公開した写真のバイト数（manifest は含まない）。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::cancel::Cancel;
use crate::drive::http::Retry;
use crate::drive::{Drive, Layout, Managed, Target};
use crate::error::{ErrorCode, ExportError};
use crate::feed::FeedClient;
use crate::folder::FolderName;
use crate::image::{ImageClient, Limits, PrepareError, Prepared, prepare_cancellable};
use crate::jobid::JobId;
use crate::jobs::{BlockedNote, JobRecord, JobStore, ReadOutcome, Status};
use crate::listing::{Exclusion, Selection};
use crate::lock::SlotGuard;
use crate::publish::{
    ProgressSink, PublishFailure, PublishOutcome, PublishParams, Published, Recovery, Resolution,
    Timing, leftover_incoming_text, publish,
};
use crate::published::PublishedStore;
use crate::recovery::{Progress, Stage};
use crate::remote::Remote;
use crate::request::ExportRequest;
use crate::settings::Settings;
use crate::staging::Workspace;
use crate::sys;

/// ジョブ全体の期限（60 分。design.md 4.1 の手順 11）。作業の開始（前のジョブの復旧のあと）から数える。期限を過ぎたあとの
/// 安全のための復旧は、Drive の 1 回ごとの操作の期限と待ちの回数の上限の中で行い、この期限では打ち切らない。
pub const JOB_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// 流れの調整値。本番は [`Tuning::default`]。試験が、待ちや期限を短くするために替える。
#[derive(Debug, Clone)]
pub struct Tuning {
    pub job_timeout: Duration,
    /// 失敗のあと・起動時の復旧の「待って一覧を 2 回取る」（既定 30 秒の待ち × 4 回まで）。
    pub timing: Timing,
    pub image_limits: Limits,
    /// 試験用: ジョブの中でパニックを起こす（`failed`・`internal` になり、実行枠が外れることを確かめる）。
    #[doc(hidden)]
    pub panic_in_job: bool,
    /// 試験用: 公開の途中で、この段階を記録に書いた**直後**にパニックを起こす（`Retiring` のあとの `Publishing` なら、旧フォルダを
    /// 退避した直後。実行枠を持ったまま復旧の表が当たり、公開まで進むことを確かめる）。
    #[doc(hidden)]
    pub panic_after_stage: Option<Stage>,
    /// 試験用: [`Self::panic_after_stage`] のパニックの直前に、ジョブの記録を読めない中身にする（読めない記録で Drive を操作せず、
    /// 上書きもしないことを確かめる）。
    #[doc(hidden)]
    pub corrupt_record_on_panic: bool,
    /// 試験用: Drive の要求の再試行の待ちを替える（待たずに済むように。本番は `None`）。
    #[doc(hidden)]
    pub drive_retry: Option<Retry>,
    /// 試験用: 一覧のページの大きさ・multipart の上限・チャンクの大きさを替える（`None` は本番の値）。
    #[doc(hidden)]
    pub drive_sizes: Option<(u32, u64, u64)>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            job_timeout: JOB_TIMEOUT,
            timing: Timing::default(),
            image_limits: Limits::default(),
            panic_in_job: false,
            panic_after_stage: None,
            corrupt_record_on_panic: false,
            drive_retry: None,
            drive_sizes: None,
        }
    }
}

/// 流れが使う部品（借用）。
pub struct Context<'a> {
    pub settings: &'a Settings,
    /// Drive の操作の入口（キャンセルの印を共有済み。ジョブの期限は使うところで掛ける）。
    pub remote: &'a Remote,
    pub store: &'a JobStore,
    pub published: &'a PublishedStore,
    pub tuning: &'a Tuning,
    /// キャンセルの印（SIGINT・SIGTERM。[`crate::cancel`]）。
    pub cancel: &'a Cancel,
}

/// 1 つのジョブの記録と、それを書き直す口。
pub struct JobHandle {
    store: JobStore,
    pub record: JobRecord,
}

impl JobHandle {
    pub fn new(store: JobStore, record: JobRecord) -> Self {
        Self { store, record }
    }

    /// 今の記録を書く。
    pub fn save(&self) -> Result<(), ExportError> {
        self.store.write(&self.record)
    }

    /// 記録を `failed`（`internal`）で終える（ジョブのスレッドを作れなかったとき）。
    pub fn fail_internal(&mut self) {
        self.finish(Done::failed(ErrorCode::Internal, None));
    }

    /// 終わりの状態を書く（書けなければもう 1 回試し、それでも書けなければログに残す。`running` のまま残った記録は、
    /// 次の復旧が片付ける）。そのあと、終わったジョブの記録を間引く。
    fn finish(&mut self, done: Done) {
        self.record.status = done.status;
        self.record.result = done.result;
        self.record.error = done.error;
        if done.blocked.is_some() {
            self.record.blocked = done.blocked;
        }
        self.record.notes.extend(done.notes);
        self.record.finished_at = Some(sys::now_rfc3339());
        if self.save().is_err() && self.save().is_err() {
            tracing::error!(
                job = %self.record.job_id,
                "ジョブの終わりの記録を書けませんでした（次の復旧が片付けます）"
            );
        }
        self.log_completion();
        // いま終えたジョブの記録は、同じ時刻の記録が多くても消さない
        self.store
            .prune_keeping(crate::jobs::KEEP_FINISHED, Some(&self.record.job_id));
    }

    /// ジョブの完了ログ（design.md 7 章）: ジョブ ID、期間、宛先、枚数、経過時間、結果の status と error の code。
    /// トークン・資格情報・Drive の応答の本文は載せない（記録にもそれらは無い）。`execute` と復旧（`recover_unfinished`）の
    /// どちらの終わりも、ここを通る（CLI の `run` でも `serve` でも出る）。経過時間は、記録した開始からの秒数（復旧が終えた
    /// ジョブでは、止まっていた時間を含む）。
    fn log_completion(&self) {
        let r = &self.record;
        let count = r
            .result
            .as_ref()
            .and_then(|v| v.get("count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let elapsed_secs =
            sys::unix_now_nanos().saturating_sub(r.started_ns).max(0) / 1_000_000_000;
        tracing::info!(
            job = %r.job_id,
            from = %r.request.period.from(),
            to = %r.request.period.to(),
            destination = r.request.destination.as_str(),
            count,
            elapsed_secs,
            status = r.status.as_str(),
            error = r.error.map_or("none", ErrorCode::as_str),
            "書き出しのジョブが終わりました"
        );
    }
}

/// 公開の各段階の前に、ジョブの記録へ段階と ID を書く口（[`ProgressSink`]）。書けなければ、その段階の Drive の操作はしない。
pub struct Recorder<'a> {
    handle: &'a mut JobHandle,
    panic_after: Option<Stage>,
    corrupt_on_panic: bool,
}

impl<'a> Recorder<'a> {
    pub fn new(handle: &'a mut JobHandle) -> Self {
        Self {
            handle,
            panic_after: None,
            corrupt_on_panic: false,
        }
    }

    /// 試験用: パニックの直前に、記録のファイルを読めない中身にする（[`Tuning::corrupt_record_on_panic`]）。
    fn corrupt_on_panic(mut self, on: bool) -> Self {
        self.corrupt_on_panic = on;
        self
    }

    /// 試験用: `stage` を書いた直後にパニックする（[`Tuning::panic_after_stage`]）。
    fn panic_after(mut self, stage: Option<Stage>) -> Self {
        self.panic_after = stage;
        self
    }
}

impl ProgressSink for Recorder<'_> {
    fn record(&mut self, progress: &Progress) -> Result<(), ExportError> {
        let before = self.handle.record.progress.replace(progress.clone());
        self.handle.save().inspect_err(|_| {
            // 書けなかったので、記録にある段階は前のまま
            self.handle.record.progress = before;
        })?;
        if self.panic_after == Some(progress.stage) {
            if self.corrupt_on_panic {
                let _ = std::fs::write(
                    self.handle.store.record_path(&self.handle.record.job_id),
                    b"{broken",
                );
            }
            panic!("試験: 段階 {} を記録した直後のパニック", progress.stage);
        }
        Ok(())
    }
}

// ---- 結果の数 ----

/// 結果（6.1 の `result`）に入る数。
#[derive(Debug, Clone, PartialEq)]
pub struct Counts {
    folder: String,
    catalog_count: usize,
    count: usize,
    bytes: u64,
    excluded: Vec<(i64, String)>,
    undated: u64,
    invalid_time: u64,
}

fn excluded_pairs(list: &[Exclusion]) -> Vec<(i64, String)> {
    list.iter()
        .map(|e| (e.id, e.reason.as_str().to_owned()))
        .collect()
}

impl Counts {
    fn empty(folder: &str) -> Self {
        Self {
            folder: folder.to_owned(),
            catalog_count: 0,
            count: 0,
            bytes: 0,
            excluded: Vec::new(),
            undated: 0,
            invalid_time: 0,
        }
    }

    /// 一覧 API だけで分かる数（公開した数はまだ無い）。
    fn from_selection(folder: &str, s: &Selection) -> Self {
        Self {
            catalog_count: s.candidate_count(),
            excluded: excluded_pairs(&s.exclusions()),
            undated: s.undated,
            invalid_time: s.invalid_time,
            ..Self::empty(folder)
        }
    }

    fn from_prepared(p: &Prepared) -> Self {
        Self {
            folder: p.folder.to_string(),
            catalog_count: p.catalog_count,
            count: p.count,
            bytes: p.photo_bytes,
            excluded: excluded_pairs(&p.excluded),
            undated: p.undated_in_catalog,
            invalid_time: p.invalid_time_in_catalog,
        }
    }

    fn from_prepare_error(folder: &str, e: &PrepareError) -> Self {
        Self {
            catalog_count: e.catalog_count,
            excluded: excluded_pairs(&e.excluded),
            undated: e.undated_in_catalog,
            invalid_time: e.invalid_time_in_catalog,
            ..Self::empty(folder)
        }
    }

    /// 記録（`summary`）に残す形。
    fn to_value(&self) -> Value {
        json!({
            "folder": self.folder,
            "catalog_count": self.catalog_count,
            "count": self.count,
            "bytes": self.bytes,
            "excluded": self.excluded_json(),
            "undated_in_catalog": self.undated,
            "invalid_time_in_catalog": self.invalid_time,
        })
    }

    fn from_value(v: &Value) -> Option<Self> {
        let excluded = v
            .get("excluded")?
            .as_array()?
            .iter()
            .map(|e| {
                Some((
                    e.get("id")?.as_i64()?,
                    e.get("reason")?.as_str()?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            folder: v.get("folder")?.as_str()?.to_owned(),
            catalog_count: usize::try_from(v.get("catalog_count")?.as_u64()?).ok()?,
            count: usize::try_from(v.get("count")?.as_u64()?).ok()?,
            bytes: v.get("bytes")?.as_u64()?,
            excluded,
            undated: v.get("undated_in_catalog")?.as_u64()?,
            invalid_time: v.get("invalid_time_in_catalog")?.as_u64()?,
        })
    }

    fn excluded_json(&self) -> Vec<Value> {
        self.excluded
            .iter()
            .map(|(id, reason)| json!({"id": id, "reason": reason}))
            .collect()
    }

    /// 6.1 の `result`。`published` が偽のとき（`conflict`・`failed`・0 枚）は、公開した数ではないので `count`・`bytes` は 0。
    fn result(
        &self,
        published: bool,
        replaced: bool,
        existing_files: u64,
        conflict: Option<&str>,
    ) -> Value {
        let mut m = Map::new();
        m.insert("folder".into(), json!(self.folder));
        m.insert("catalog_count".into(), json!(self.catalog_count));
        m.insert(
            "count".into(),
            json!(if published { self.count } else { 0 }),
        );
        m.insert("uploaded".into(), json!(published));
        m.insert("replaced".into(), json!(published && replaced));
        m.insert(
            "bytes".into(),
            json!(if published { self.bytes } else { 0 }),
        );
        m.insert("excluded".into(), Value::Array(self.excluded_json()));
        m.insert("undated_in_catalog".into(), json!(self.undated));
        m.insert("invalid_time_in_catalog".into(), json!(self.invalid_time));
        m.insert("existing_files".into(), json!(existing_files));
        if let Some(reason) = conflict {
            m.insert("conflict".into(), json!(reason));
        }
        Value::Object(m)
    }
}

// ---- 終わり方 ----

/// ジョブの終わり方（記録に書く内容）。
struct Done {
    status: Status,
    result: Option<Value>,
    error: Option<ErrorCode>,
    blocked: Option<BlockedNote>,
    notes: Vec<String>,
}

impl Done {
    fn new(status: Status) -> Self {
        Self {
            status,
            result: None,
            error: None,
            blocked: None,
            notes: Vec::new(),
        }
    }

    fn succeeded(result: Value) -> Self {
        Self {
            result: Some(result),
            ..Self::new(Status::Succeeded)
        }
    }

    fn conflict(result: Value) -> Self {
        Self {
            result: Some(result),
            ..Self::new(Status::Conflict)
        }
    }

    fn failed(code: ErrorCode, result: Option<Value>) -> Self {
        Self {
            result,
            error: Some(code),
            ..Self::new(Status::Failed)
        }
    }

    fn interrupted() -> Self {
        Self::new(Status::Interrupted)
    }

    fn blocked(note: BlockedNote) -> Self {
        Self {
            error: Some(ErrorCode::Blocked),
            blocked: Some(note),
            ..Self::new(Status::Blocked)
        }
    }

    fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }
}

// ---- ジョブの流れ ----

/// 受け付けたジョブを、実行枠を持ったまま最後まで流して、記録を終わりの状態にする。パニックとキャンセルの扱いは、
/// モジュールの説明（「パニックとキャンセル」）のとおり。
pub fn execute(ctx: &Context<'_>, handle: &mut JobHandle, _slot: &SlotGuard) {
    let done = match catch_unwind(AssertUnwindSafe(|| run_steps(ctx, handle))) {
        Ok(done) => done,
        Err(_) => match after_panic(ctx, handle) {
            Some(done) => done,
            None => return,
        },
    };
    if cancel_voids(ctx.cancel, &done) {
        hand_over_cancelled(ctx, handle);
        return;
    }
    handle.finish(done);
}

/// キャンセルの印が立っているときに、記録に**確定しない**結果か。止めたことの副産物は確定しない: 照合できなかった
/// （`blocked`）、失敗、中断（`incoming` のゴミ箱への移動が止まって、「手作業の片付けが要る残りもの」になる。次の復旧なら自動で
/// 消せる）、知らせの付いた成功（旧フォルダのゴミ箱への移動が止まって、旧フォルダが残る）。印の前に済んだ `succeeded`（知らせなし）・
/// `conflict` は、そのまま記録する。確定しなくても害は無い（復旧の表は、記録した段階から同じ結果を出す）。
fn cancel_voids(cancel: &Cancel, done: &Done) -> bool {
    cancel.is_cancelled()
        && match done.status {
            Status::Succeeded => !done.notes.is_empty(),
            Status::Conflict => false,
            Status::Failed | Status::Blocked | Status::Interrupted | Status::Running => true,
        }
}

/// 記録に、Drive の操作を含む段階があるか。**ディスクの記録**で判定する（メモリの状態は、パニックの時点で信じない）。読めない・
/// 無いときは、あるものとして扱う（確定の書き込みで、読めない記録を上書きしない）。
fn stage_on_disk(ctx: &Context<'_>, handle: &JobHandle) -> bool {
    match ctx.store.read(&handle.record.job_id) {
        ReadOutcome::Found(rec) => rec.progress.is_some(),
        ReadOutcome::NotFound | ReadOutcome::Unreadable => true,
    }
}

/// パニックのあとの終わり方。根拠は**ディスクの記録を読み直したもの**だけ（メモリの状態は、パニックの時点で信じない）。
///
/// - 読み直せた `running` の記録で、段階が無ければ `failed`（`internal`）。
/// - 段階があれば、その記録（段階と ID）を `handle` に取り込み、実行枠を持ったまま復旧の表を当てる（`failed` を無条件に確定すると、
///   退避の直後なら、宛先が空のまま復旧の対象から外れる）。
/// - 記録が無い・読めない・`running` でないときは、Drive の操作も記録の書き込みもしない（`None`。読めない記録は上書きせず、
///   次の起動で `blocked` として扱われる）。
///
/// 復旧の中でまたパニックしたら `None`（記録は `running` のまま、次の復旧に渡す）。
fn after_panic(ctx: &Context<'_>, handle: &mut JobHandle) -> Option<Done> {
    let disk = match ctx.store.read(&handle.record.job_id) {
        ReadOutcome::Found(rec) if rec.status == Status::Running => *rec,
        ReadOutcome::Found(_) => {
            tracing::error!(
                job = %handle.record.job_id,
                "ジョブの中でパニックしました（記録は、すでに終わりの状態です。触れません）"
            );
            return None;
        }
        ReadOutcome::NotFound | ReadOutcome::Unreadable => {
            tracing::error!(
                job = %handle.record.job_id,
                "ジョブの中でパニックしましたが、記録を読み直せません（Drive にも記録にも触れません。次の起動で扱います）"
            );
            return None;
        }
    };
    handle.record = disk;
    if handle.record.progress.is_none() {
        return Some(Done::failed(ErrorCode::Internal, None));
    }
    tracing::error!(
        job = %handle.record.job_id,
        "ジョブの中でパニックしました。記録した段階から、復旧の表を当てます"
    );
    match catch_unwind(AssertUnwindSafe(|| recover_one(ctx, handle))) {
        Ok(done) => Some(done),
        Err(_) => {
            tracing::error!(
                job = %handle.record.job_id,
                "パニックの後の復旧でもパニックしました（記録は running のまま、次の復旧に渡します）"
            );
            None
        }
    }
}

/// キャンセルされたジョブの記録。段階が無ければ（Drive に触れていない）`interrupted`、あれば確定せず `running` のまま、
/// 次の復旧に渡す。
fn hand_over_cancelled(ctx: &Context<'_>, handle: &mut JobHandle) {
    if stage_on_disk(ctx, handle) {
        tracing::warn!(
            job = %handle.record.job_id,
            "キャンセルされました。記録は running のまま、次の復旧に渡します"
        );
    } else {
        handle.finish(Done::interrupted());
    }
}

fn run_steps(ctx: &Context<'_>, handle: &mut JobHandle) -> Done {
    if ctx.tuning.panic_in_job {
        panic!("試験: ジョブの中のパニック");
    }
    let req = handle.record.request.clone();
    let own_id = handle.record.job_id.clone();
    let folder = match FolderName::for_period(&req.period) {
        Ok(f) => f,
        Err(e) => return Done::failed(e.code, None),
    };

    // 手順 1: 作業領域の残骸と、止まったジョブの復旧（枠を持っているので触ってよい）
    if ctx.store.clean_staging().is_err() {
        return Done::failed(ErrorCode::Internal, None);
    }
    let report = recover_unfinished(ctx, Some(&own_id));
    if report.blocked {
        return Done::failed(ErrorCode::Blocked, None);
    }
    if report.cancelled {
        // キャンセルされた（確定しない。`execute` が、印を見て記録を渡す）
        return Done::failed(ErrorCode::Internal, None);
    }

    let deadline = Instant::now() + ctx.tuning.job_timeout;

    // 手順 2（一覧の取得の前後で、キャンセルの印を見る。段階は未記録なので、`execute` が `interrupted` にする。印が立った
    // あとの取得の結果で、`succeeded`（0 枚）や `failed` を確定しない）
    if ctx.cancel.is_cancelled() {
        return Done::interrupted();
    }
    let feed = FeedClient::new(&ctx.settings.feed_base);
    let selected = feed.select(&req.period);
    if ctx.cancel.is_cancelled() {
        return Done::interrupted();
    }
    let selection = match selected {
        Ok(s) => s,
        Err(e) => return Done::failed(e.code, None),
    };
    let counts = Counts::from_selection(folder.as_str(), &selection);
    // 手順 3
    if selection.pending_count() > 0 {
        return Done::failed(
            ErrorCode::DerivativePending,
            Some(counts.result(false, false, 0, None)),
        );
    }
    // 手順 4
    if selection.candidate_count() == 0 {
        return Done::succeeded(counts.result(false, false, 0, None));
    }

    // 手順 5
    let workspace = match Workspace::create(ctx.store.staging_dir(), own_id.as_str()) {
        Ok(w) => w,
        Err(e) => return Done::failed(e.code, Some(counts.result(false, false, 0, None))),
    };
    let images = ImageClient::with_limits(&ctx.settings.feed_base, ctx.tuning.image_limits);
    let prepared = match prepare_cancellable(
        &images,
        &selection,
        req.destination,
        &workspace,
        Some(deadline),
        ctx.cancel,
    ) {
        Ok(p) => p,
        Err(e) => {
            let counts = Counts::from_prepare_error(folder.as_str(), &e);
            return Done::failed(e.code(), Some(counts.result(false, false, 0, None)));
        }
    };
    let counts = Counts::from_prepared(&prepared);
    // 公開の前に数を記録に残す（止まったジョブを復旧が成功にするときの結果に使う。最初の段階の記録と一緒に書かれる）
    handle.record.summary = Some(counts.to_value());
    if Instant::now() >= deadline {
        return Done::failed(
            ErrorCode::Timeout,
            Some(counts.result(false, false, 0, None)),
        );
    }

    // 手順 6〜7
    let layout = match Layout::new(ctx.settings, req.destination, folder, own_id) {
        Ok(l) => l,
        Err(e) => return Done::failed(e.code, Some(counts.result(false, false, 0, None))),
    };
    let remote = ctx.remote.with_deadline(Some(deadline));
    // ジョブの始めに、宛先の親と作業用のフォルダが（起動後に動かされていないか）フォルダ・ゴミ箱の中でないことを確かめる。
    // 候補が 0 枚のとき（上で終えている）は Drive に触れない
    if let Err(e) = Drive::new(&remote, &layout).check_roots() {
        return Done::failed(e.code, Some(counts.result(false, false, 0, None)));
    }
    let params = PublishParams {
        on_exists: req.on_exists,
        workspace: workspace.path(),
        files: &prepared.files,
        published: ctx.published,
        timing: ctx.tuning.timing,
    };
    let outcome = publish(
        &remote,
        &layout,
        &params,
        &mut Recorder::new(handle)
            .panic_after(ctx.tuning.panic_after_stage)
            .corrupt_on_panic(ctx.tuning.corrupt_record_on_panic),
    );
    let done = match outcome {
        Ok(PublishOutcome::Published(p)) => {
            let mut notes = Vec::new();
            settle_published(ctx, &p, &layout, &mut notes);
            Done::succeeded(counts.result(true, p.replaced, p.existing_files, None))
                .with_notes(notes)
        }
        Ok(PublishOutcome::Conflict {
            reason,
            existing_files,
        }) => Done::conflict(counts.result(false, false, existing_files, Some(reason.as_str()))),
        Err(f) => failure_done(&layout, f, &counts),
    };
    // 手順 8: 作業領域を消す（結果を記録に書く前に）
    drop(workspace);
    done
}

fn failure_done(layout: &Layout, f: PublishFailure, counts: &Counts) -> Done {
    if let Some(info) = f.blocked {
        return Done::blocked(BlockedNote {
            reason: info.reason.as_str().to_owned(),
            description: info.describe(layout),
        });
    }
    let mut notes = Vec::new();
    if f.leftover_incoming {
        notes.push(leftover_incoming_text(
            layout,
            f.leftover_incoming_id.as_deref(),
        ));
    }
    Done::failed(f.error.code, Some(counts.result(false, false, 0, None))).with_notes(notes)
}

/// 公開できたあとの始末: 公開の記録に書けていなければ、実行枠を持ったまま書き直し（次のジョブより前）、それでも書けなければ
/// 知らせに残す。旧フォルダが `replaced/` に残っていれば、それも知らせる。
fn settle_published(ctx: &Context<'_>, p: &Published, layout: &Layout, notes: &mut Vec<String>) {
    if !p.recorded && p.retry_record(ctx.published).is_err() {
        notes.push(format!(
            "公開は済んでいます（フォルダの ID {}）が、公開の記録（published/ の下の、この宛先のファイル）に書けていません。\
             次の replace は、このフォルダを管理しているものとして扱えず、conflict（not_managed）になります。",
            p.folder_id
        ));
    }
    if let Some(text) = p.old_left_text(layout) {
        notes.push(text);
    }
}

// ---- 止まったジョブの復旧 ----

/// [`recover_unfinished`] の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecoverReport {
    /// 片付けたジョブの数。
    pub recovered: usize,
    /// `blocked` になった（そこで止める。後ろの未完了は、解除のあとの復旧に任せる）。
    pub blocked: bool,
    /// キャンセルの印が立った（そこで止める。そのジョブの記録は確定せず `running` のまま、後ろの未完了にも触れない）。
    pub cancelled: bool,
}

/// 未完了（`running` のまま残った）ジョブの記録を、古いほうから、表（4.2）のとおりに片付ける。実行枠を持っているときだけ
/// 呼ぶ（起動時と各ジョブの前）。`skip` は、いま自分が流すジョブ。いったん `blocked` になったジョブは、自動では動かさない
/// （`running` の記録だけを見る）。`blocked` になったら、そこで止める。
pub fn recover_unfinished(ctx: &Context<'_>, skip: Option<&JobId>) -> RecoverReport {
    let mut report = RecoverReport::default();
    let scan = ctx.store.scan();
    for rec in scan.unfinished() {
        if skip == Some(&rec.job_id) {
            continue;
        }
        if ctx.cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let mut handle = JobHandle::new(ctx.store.clone(), rec.clone());
        let done = recover_one(ctx, &mut handle);
        if cancel_voids(ctx.cancel, &done) {
            // 止めたことの副産物（照合できなかった、など）は確定しない。記録は `running` のまま、次の復旧に任せる
            report.cancelled = true;
            break;
        }
        let blocked = done.status == Status::Blocked;
        handle.finish(done);
        report.recovered += 1;
        if blocked {
            report.blocked = true;
            break;
        }
    }
    report
}

fn recover_one(ctx: &Context<'_>, handle: &mut JobHandle) -> Done {
    // 段階の記録が無ければ、Drive に触れる前に止まった
    let Some(progress) = handle.record.progress.clone() else {
        return Done::interrupted();
    };
    let rec = handle.record.clone();
    let stuck = |reason: &str| {
        Done::blocked(BlockedNote {
            reason: reason.to_owned(),
            description: "記録から Drive のパスを作れませんでした。記録を確かめてください"
                .to_owned(),
        })
    };
    let Ok(folder) = FolderName::for_period(&rec.request.period) else {
        return stuck("internal");
    };
    let Ok(layout) = Layout::new(
        ctx.settings,
        rec.request.destination,
        folder,
        rec.job_id.clone(),
    ) else {
        return stuck("internal");
    };
    // 後始末と復旧は期限なし（コマンドごとの期限と、待ちの回数の上限だけ。ジョブ全体の期限では打ち切らない）
    let remote = ctx.remote.with_deadline(None);
    let recovery = Recovery {
        remote: &remote,
        layout: &layout,
        published: ctx.published,
        expected: None,
        timing: ctx.tuning.timing,
    };
    match recovery.run(&progress, true, &mut Recorder::new(handle)) {
        Ok(Resolution::Interrupted {
            leftover_incoming,
            leftover_incoming_id,
        }) => {
            let notes = if leftover_incoming {
                vec![leftover_incoming_text(
                    &layout,
                    leftover_incoming_id.as_deref(),
                )]
            } else {
                Vec::new()
            };
            Done::interrupted().with_notes(notes)
        }
        Ok(Resolution::Published(p)) => {
            let counts = rec
                .summary
                .as_ref()
                .and_then(Counts::from_value)
                .unwrap_or_else(|| Counts::empty(layout.folder().as_str()));
            let mut notes = Vec::new();
            settle_published(ctx, &p, &layout, &mut notes);
            Done::succeeded(counts.result(true, p.replaced, p.existing_files, None))
                .with_notes(notes)
        }
        Err(info) => Done::blocked(BlockedNote {
            reason: info.reason.as_str().to_owned(),
            description: info.describe(&layout),
        }),
    }
}

// ---- dry_run ----

/// `dry_run`（design.md 6.1 の 200 の形）。実行枠を持ったまま呼ぶ。画像は取らず、Drive は `lsjson`（と管理の確認の `cat`）
/// だけを呼ぶ。候補が 0 枚でも、宛先の状態は照会する（読み取りだけ。実際の書き出しは 0 枚なら Drive に触れない）。
///
/// `days` は manifest の `days` と同じ形（日付ごとの `count`・`first`・`last`・`ids`。候補の全部）。`managed` は、宛先が
/// `exists` のとき、このサービスが書いたフォルダか（書いたが中身が変わっているものも `true`。`replace` では `modified` の
/// `conflict` になる）。それ以外は `null`。
pub fn dry_run(
    ctx: &Context<'_>,
    req: &ExportRequest,
    _slot: &SlotGuard,
) -> Result<Value, ExportError> {
    let selection = FeedClient::new(&ctx.settings.feed_base).select(&req.period)?;
    let folder = FolderName::for_period(&req.period)?;
    // 照会だけなので、ジョブ ID は Drive のパスには使わない（`incoming/`・`replaced/` には触れない）
    let layout = Layout::new(
        ctx.settings,
        req.destination,
        folder.clone(),
        JobId::generate()?,
    )?;
    let remote = ctx.remote.for_dry_run();
    let inspection = crate::publish::dry_run(&remote, &layout, ctx.published)?;
    let (target, managed) = match &inspection.target {
        Target::Absent => ("absent", Value::Null),
        Target::Exists { .. } => (
            "exists",
            match inspection.managed {
                Some(Managed::Managed | Managed::Modified) => json!(true),
                Some(Managed::NotManaged) => json!(false),
                None => Value::Null,
            },
        ),
    };
    let mut days = Map::new();
    for (date, list) in selection.by_day() {
        days.insert(
            date.to_string(),
            json!({
                "count": list.len(),
                "first": list.first().map(|c| c.time.to_string()),
                "last": list.last().map(|c| c.time.to_string()),
                "ids": list.iter().map(|c| c.id).collect::<Vec<_>>(),
            }),
        );
    }
    Ok(json!({
        "folder": folder.as_str(),
        "destination": req.destination.as_str(),
        "candidate_count": selection.candidate_count(),
        "days": days,
        "target": target,
        "managed": managed,
        "existing_files": inspection.existing_files,
        "excluded": {"not_derivative": selection.exclusions().len()},
        "derivative_pending": selection.pending_count(),
        "undated_in_catalog": selection.undated,
        "invalid_time_in_catalog": selection.invalid_time,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_survive_the_record_and_hide_the_unpublished_numbers() {
        let c = Counts {
            folder: "2030-03-10".into(),
            catalog_count: 5,
            count: 4,
            bytes: 1234,
            excluded: vec![(7, "has_metadata".into())],
            undated: 2,
            invalid_time: 1,
        };
        assert_eq!(Counts::from_value(&c.to_value()), Some(c.clone()));
        let published = c.result(true, true, 3, None);
        assert_eq!(published["count"], 4);
        assert_eq!(published["bytes"], 1234);
        assert_eq!(published["uploaded"], true);
        assert_eq!(published["replaced"], true);
        assert_eq!(published["existing_files"], 3);
        assert!(published.get("conflict").is_none());
        let conflict = c.result(false, true, 3, Some("exists"));
        assert_eq!(conflict["count"], 0);
        assert_eq!(conflict["bytes"], 0);
        assert_eq!(conflict["uploaded"], false);
        assert_eq!(conflict["replaced"], false);
        assert_eq!(conflict["conflict"], "exists");
        assert_eq!(conflict["catalog_count"], 5);
        assert_eq!(conflict["excluded"][0]["id"], 7);
        let keys: Vec<&str> = published
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "folder",
                "catalog_count",
                "count",
                "uploaded",
                "replaced",
                "bytes",
                "excluded",
                "undated_in_catalog",
                "invalid_time_in_catalog",
                "existing_files"
            ]
        );
    }

    /// 書く側（[`Done`] の全部の作り方）が作る記録は、[`JobRecord::from_file_json`] の整合の検査に通る（外すと、正しい記録が
    /// 読めない記録になって、全部の書き出しが止まる）。
    #[test]
    fn every_way_of_finishing_a_job_leaves_a_readable_record() {
        use crate::jobs::ReadOutcome;
        use crate::request::RawRequest;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-tmp")
            .join(format!("flow-finish-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = JobStore::open(&dir).unwrap();
        let req = RawRequest {
            from: Some("2030-03-10".into()),
            destination: Some("yamareco".into()),
            ..RawRequest::default()
        }
        .validate()
        .unwrap();
        let counts = Counts {
            folder: "2030-03-10".into(),
            catalog_count: 3,
            count: 2,
            bytes: 10,
            excluded: vec![(7, "has_metadata".into())],
            undated: 0,
            invalid_time: 0,
        };
        let note = || BlockedNote {
            reason: "unobservable".into(),
            description: "説明".into(),
        };
        let dones: Vec<(&str, Done)> = vec![
            (
                "公開",
                Done::succeeded(counts.result(true, false, 0, None))
                    .with_notes(vec!["知らせ".into()]),
            ),
            (
                "0 枚",
                Done::succeeded(Counts::empty("2030-03-10").result(false, false, 0, None)),
            ),
            (
                "復旧の成功（数は不明）",
                Done::succeeded(Counts::empty("2030-03-10").result(true, true, 0, None)),
            ),
            (
                "conflict",
                Done::conflict(counts.result(false, false, 2, Some("not_managed"))),
            ),
            (
                "失敗（数つき）",
                Done::failed(
                    ErrorCode::FetchFailed,
                    Some(counts.result(false, false, 0, None)),
                ),
            ),
            ("失敗（数なし）", Done::failed(ErrorCode::Internal, None)),
            (
                "blocked で止められた",
                Done::failed(ErrorCode::Blocked, None),
            ),
            (
                "中断",
                Done::interrupted().with_notes(vec!["incoming が残っています".into()]),
            ),
            ("blocked", Done::blocked(note())),
        ];
        for (n, (what, done)) in dones.into_iter().enumerate() {
            let id = JobId::parse(&format!("{:032x}", n + 1)).unwrap();
            let mut handle = JobHandle::new(store.clone(), JobRecord::new(id.clone(), &req));
            handle.save().unwrap();
            handle.finish(done);
            assert!(
                matches!(store.read(&id), ReadOutcome::Found(_)),
                "{what}: 終えた記録が読めない"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
