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
//! 7. `succeeded`・`conflict`・`failed`・`blocked` を記録する。公開の記録（`published.json`）に書けなかったときは、実行枠を
//!    持ったまま、次のジョブより前に書き直し（[`crate::publish::Published::retry_record`]）、それでも書けなければ `notes` に残す。
//! 8. 作業領域を消す（[`crate::staging::Workspace`] を手放す。成功・失敗・パニックのいずれも）。
//!
//! ジョブ全体の期限は [`JOB_TIMEOUT`]（60 分）。`prepare` と rclone に同じ期限を掛ける（過ぎれば `timeout`）。パニックは
//! `catch_unwind` で受けて `failed`（`internal`）にする。実行枠は、呼び出し側がガードを手放すと外れる（パニックでも）。
//!
//! ## 結果の数
//!
//! `catalog_count` は候補の数、`count` は公開した写真の数（成功のときだけ。`conflict`・`failed` では 0 で、`bytes` も 0）、
//! `excluded` は仕様上の除外（id と理由）、`bytes` は公開した写真のバイト数（manifest は含まない）。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::drive::{Layout, Managed, Target};
use crate::error::{ErrorCode, ExportError};
use crate::feed::FeedClient;
use crate::folder::FolderName;
use crate::image::{ImageClient, Limits, PrepareError, Prepared, prepare};
use crate::jobid::JobId;
use crate::jobs::{BlockedNote, JobRecord, JobStore, Status};
use crate::listing::{Exclusion, Selection};
use crate::lock::SlotGuard;
use crate::publish::{
    ProgressSink, PublishFailure, PublishOutcome, PublishParams, Published, Recovery, Resolution,
    Timing, leftover_incoming_text, publish,
};
use crate::published::PublishedStore;
use crate::rclone::Rclone;
use crate::recovery::Progress;
use crate::request::ExportRequest;
use crate::settings::Settings;
use crate::staging::Workspace;
use crate::sys;

/// ジョブ全体の期限（60 分。design.md 4.1 の手順 11）。
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
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            job_timeout: JOB_TIMEOUT,
            timing: Timing::default(),
            image_limits: Limits::default(),
            panic_in_job: false,
        }
    }
}

/// 流れが使う部品（借用）。
pub struct Context<'a> {
    pub settings: &'a Settings,
    pub store: &'a JobStore,
    pub published: &'a PublishedStore,
    pub tuning: &'a Tuning,
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
        // いま終えたジョブの記録は、同じ時刻の記録が多くても消さない
        self.store
            .prune_keeping(crate::jobs::KEEP_FINISHED, Some(&self.record.job_id));
    }
}

/// 公開の各段階の前に、ジョブの記録へ段階と ID を書く口（[`ProgressSink`]）。書けなければ、その段階の Drive の操作はしない。
pub struct Recorder<'a> {
    handle: &'a mut JobHandle,
}

impl<'a> Recorder<'a> {
    pub fn new(handle: &'a mut JobHandle) -> Self {
        Self { handle }
    }
}

impl ProgressSink for Recorder<'_> {
    fn record(&mut self, progress: &Progress) -> Result<(), ExportError> {
        let before = self.handle.record.progress.replace(progress.clone());
        self.handle.save().inspect_err(|_| {
            // 書けなかったので、記録にある段階は前のまま
            self.handle.record.progress = before;
        })
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

/// 受け付けたジョブを、実行枠を持ったまま最後まで流して、記録を終わりの状態にする。
pub fn execute(ctx: &Context<'_>, handle: &mut JobHandle, _slot: &SlotGuard) {
    let done = match catch_unwind(AssertUnwindSafe(|| run_steps(ctx, handle))) {
        Ok(done) => done,
        Err(_) => Done::failed(ErrorCode::Internal, None),
    };
    handle.finish(done);
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

    let deadline = Instant::now() + ctx.tuning.job_timeout;

    // 手順 2
    let feed = FeedClient::new(&ctx.settings.feed_base);
    let selection = match feed.select(&req.period) {
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
    let prepared = match prepare(
        &images,
        &selection,
        req.destination,
        &workspace,
        Some(deadline),
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
    let rclone = Rclone::new(ctx.settings).with_deadline(Some(deadline));
    let params = PublishParams {
        on_exists: req.on_exists,
        workspace: workspace.path(),
        files: &prepared.files,
        published: ctx.published,
        timing: ctx.tuning.timing,
    };
    let outcome = publish(&rclone, &layout, &params, &mut Recorder::new(handle));
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
            "公開は済んでいます（フォルダの ID {}）が、公開の記録（published.json）に書けていません。\
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
        let mut handle = JobHandle::new(ctx.store.clone(), rec.clone());
        let done = recover_one(ctx, &mut handle);
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
    // 後始末と復旧は期限なし（コマンドごとの期限だけ）
    let rclone = Rclone::new(ctx.settings);
    let recovery = Recovery {
        rclone: &rclone,
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
    let rclone = Rclone::for_dry_run(ctx.settings);
    let inspection = crate::publish::dry_run(&rclone, &layout, ctx.published)?;
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
}
