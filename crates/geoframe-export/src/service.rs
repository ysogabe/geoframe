//! ジョブの受付と照会の層（`serve`（T6）と `run` コマンドが使う。HTTP・tokio には依存しない同期の層）。
//!
//! [`Service`] は `Clone`（中身は `Arc`）で `Send + Sync`。T6 は、HTTP の要求ごとに次を呼ぶ。
//!
//! | HTTP | 呼ぶもの | 断り |
//! |---|---|---|
//! | `POST /v1/exports`（`dry_run` でない） | [`Service::submit`] → `202 {"job_id", "status"}` | [`SubmitError::Busy`]・[`SubmitError::RequestIdMismatch`] は 409、[`SubmitError::Blocked`]・[`SubmitError::ShuttingDown`] は 503（`blocked`・`shutting_down`）、[`SubmitError::Internal`] は 500 |
//! | `POST /v1/exports`（`dry_run`） | [`Service::dry_run`] → `200` の本文 | [`DryRunError::Busy`] は 409、[`DryRunError::Blocked`]・写真サービス・Drive の失敗は 503 |
//! | `GET /v1/exports/{job_id}` | [`Service::lookup`] → [`Lookup::Found`] は 200、[`Lookup::NotFound`] は 404、[`Lookup::Unreadable`] は 500 | |
//! | `GET /v1/exports` | [`Service::list`] | |
//! | 起動時 | [`Service::startup_recovery`] | |
//!
//! ## 受付（[`Service::submit`]）
//!
//! 1 つの排他（プロセス内の `Mutex`。毒化しても使い続ける）の中で、次を順に行う。プロセス内の排他は、別のプロセス（同じ
//! 状態ディレクトリを使う別の `Service`・CLI の `run`）には効かないので、**実行枠を取った直後と、取れなかったときにも**、
//! `request_id` をもう一度照会する（手順 2）。
//!
//! 1. `request_id` があれば、直近 50 件の記録から探す。同じ値で期間・宛先・`on_exists` が同じなら、**実行枠を取らずに**そのジョブを
//!    返す（実行中でも。[`Submitted::reused`] が真）。違えば [`SubmitError::RequestIdMismatch`]。
//! 2. 実行枠（flock）を取る。取れなければ、もう一度 `request_id` を照会し（その間に別のプロセスが登録していれば、手順 1 と同じに
//!    返す）、無ければ [`SubmitError::Busy`]。取れたら、記録を書く前にもう一度照会する（見つかれば、枠を手放してそのジョブを返す。
//!    二重に登録しない）。
//! 3. 記録の走査で、`blocked`・読めない記録があれば [`SubmitError::Blocked`]（解除するまで、新しい書き出しを断る）。
//! 4. **終了の印**（[`Service::cancel`]）が立っていれば、何も登録せずに [`SubmitError::ShuttingDown`]（HTTP の入口が本文を読んでいる
//!    間に印が立つと、入口の確認と登録の間に隙間ができる。この確認は、排他と実行枠を取ったあと、記録を書く直前にする。これで、
//!    印を見て断った受付は、記録も実行枠も残さない。確認のあとに印が立てば、記録は書かれ、ジョブのスレッドが印を見て止まる
//!    〔記録は次の復旧が片付ける〕）。
//! 5. ジョブの記録（`running`）を書く。書けなければ [`SubmitError::Internal`]（何も始めない）。
//! 6. 別のスレッドで流す（[`crate::flow::execute`]。実行枠のガードはそのスレッドへ移る。終わりの記録を書いてから手放す）。
//!    スレッドを作れなければ、記録を `failed`（`internal`）にして [`SubmitError::Internal`]。
//!
//! ## キャンセル（SIGINT・SIGTERM）と終わりの待ち（T6 から）
//!
//! [`Service::cancel`] が返す [`Cancel`] の印を立てると（[`crate::cancel`]。signal の handler か、tokio のシグナルの受け口から）、
//! 実行中のジョブは、起動している rclone を止めて `wait` し終えてから、記録を確定せずに（Drive の段階が記録にあれば
//! `running` のまま）終わる。止まったジョブの記録は、次の復旧（起動時・次のジョブの前・`recover`）が片付ける。`serve` は、印を
//! 立てたあと、新しい受付を止め、[`Service::wait_idle`] で実行中のジョブのスレッドの終わりを待ってから、プロセスを終える
//! （実行枠はジョブのスレッドが手放す。待たずに終えると、子が残るか、記録が中途のまま終わる）。
//!
//! 止まったジョブの復旧（時間がかかることがある）は、排他の中ではなく、手順 6 のスレッドの最初で行う（[`crate::flow`]）。
//!
//! ## `blocked`
//!
//! 受付と `dry_run` は、`blocked` の記録・読めない記録があれば断る（[`Service::blocking`]）。解除は `geoframe-export recover
//! --resolve <job_id>`（[`crate::recover`]）。サービスが動いている間は、`recover` は実行枠が取れなければ「実行中」で終わる。

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cancel::Cancel;
use crate::error::{ErrorCode, ExportError};
use crate::flow::{self, Context, JobHandle, RecoverReport, Tuning};
use crate::jobid::JobId;
use crate::jobs::{Blocking, JobRecord, JobStore, REQUEST_ID_WINDOW, ReadOutcome, Status};
use crate::lock::{self, Slot, SlotGuard};
use crate::published::PublishedStore;
use crate::request::ExportRequest;
use crate::settings::Settings;

/// 新しいジョブを始められない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// 実行枠が取れない（別のジョブか CLI の `run` が実行中）。HTTP は 409。
    Busy,
    /// 同じ `request_id` で、期間・宛先・`on_exists` が違う。HTTP は 409。
    RequestIdMismatch,
    /// `blocked` の記録・読めない記録がある。HTTP は 503。
    Blocked(Blocking),
    /// 終了の印が立っていた（記録を書く直前に確かめる）。何も登録していない。HTTP は 503（`shutting_down`）。
    ShuttingDown,
    Internal(ExportError),
}

impl SubmitError {
    /// 本文の `error` に出すコード。
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Busy => ErrorCode::Busy,
            Self::RequestIdMismatch => ErrorCode::RequestIdMismatch,
            Self::Blocked(_) => ErrorCode::Blocked,
            Self::ShuttingDown => ErrorCode::ShuttingDown,
            Self::Internal(e) => e.code,
        }
    }
}

/// 受け付けた（または、同じ `request_id` で受け付け済みの）ジョブ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub job_id: JobId,
    /// 今の状態（新しいジョブは `running`。再送には、そのジョブの今の状態）。
    pub status: Status,
    /// 同じ `request_id` の再送で、新しいジョブを作らなかった。
    pub reused: bool,
}

/// `dry_run` の断り・失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRunError {
    Busy,
    Blocked(Blocking),
    /// 一覧 API・Drive の失敗（`feed_unavailable`・`feed_invalid`・`too_many_photos`・`drive_unavailable`・
    /// `ambiguous_destination`・`internal`）。
    Failed(ExportError),
}

impl DryRunError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Busy => ErrorCode::Busy,
            Self::Blocked(_) => ErrorCode::Blocked,
            Self::Failed(e) => e.code,
        }
    }
}

/// `GET /v1/exports/{job_id}` の結果。
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// 6.1 の形の JSON。
    Found(Value),
    /// 形が違う ID・無い ID。
    NotFound,
    /// 記録が読めない（形が崩れている）。
    Unreadable,
}

/// [`Service::startup_recovery`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupReport {
    /// 実行枠が取れなかった（CLI の `run` が動いている）。その作業領域と記録には触れない。
    Skipped,
    Done {
        /// `staging/` の下から消したものの数。
        cleaned: usize,
        recovery: RecoverReport,
    },
    Failed(ExportError),
}

/// [`Service::begin`] の使い方（受付と、`run` コマンド）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `submit`: `request_id` を照会し、記録を書く直前に終了の印を確かめる。
    Submit,
    /// `run_foreground`: `request_id` は見ない。終了の印は、ジョブの中（[`crate::flow`]）で見る。
    Foreground,
}

/// [`Service::begin`] の結果。
enum Began {
    Started(Box<(SlotGuard, JobHandle)>),
    /// 別のプロセスが、先に同じ `request_id` を登録していた（実行枠は手放した）。
    Duplicate(Submitted),
}

/// ジョブのスレッドの数を、手放すとき（パニックでも）に減らす。
struct RunningGuard(Arc<Inner>);

impl RunningGuard {
    /// 減らす（スレッドを作れなかったとき）。
    fn release(self) {}
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let (count, cv) = &self.0.running;
        *count.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        cv.notify_all();
    }
}

struct Inner {
    settings: Settings,
    store: JobStore,
    published: PublishedStore,
    tuning: Tuning,
    cancel: Cancel,
    gate: Mutex<()>,
    /// 動いているジョブのスレッドの数（[`Service::wait_idle`]）。
    running: (Mutex<usize>, Condvar),
}

impl Inner {
    fn context(&self) -> Context<'_> {
        Context {
            settings: &self.settings,
            store: &self.store,
            published: &self.published,
            tuning: &self.tuning,
            cancel: &self.cancel,
        }
    }
}

#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

impl Service {
    /// 状態ディレクトリの `jobs/`・`staging/` を用意する（状態ディレクトリは、あるものとする）。
    pub fn new(settings: Settings) -> Result<Self, ExportError> {
        Self::with_tuning(settings, Tuning::default())
    }

    /// 調整値を替える（試験用。本番は [`Service::new`]）。
    pub fn with_tuning(settings: Settings, tuning: Tuning) -> Result<Self, ExportError> {
        Self::with_cancel(settings, tuning, Cancel::new())
    }

    /// キャンセルの印を渡す形（`run`・`serve` の入口。signal の handler と同じ印を共有する）。
    pub fn with_cancel(
        settings: Settings,
        tuning: Tuning,
        cancel: Cancel,
    ) -> Result<Self, ExportError> {
        let store = JobStore::open(&settings.state_dir)?;
        let published = PublishedStore::in_state_dir(&settings.state_dir);
        Ok(Self {
            inner: Arc::new(Inner {
                settings,
                store,
                published,
                tuning,
                cancel,
                gate: Mutex::new(()),
                running: (Mutex::new(0), Condvar::new()),
            }),
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// キャンセルの印（立てると、実行中のジョブは rclone を止めて、記録を確定せずに終わる。上の「キャンセル」）。
    pub fn cancel(&self) -> &Cancel {
        &self.inner.cancel
    }

    /// 動いているジョブのスレッドが無くなるまで待つ。`timeout` までに無くなれば `true`。
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let (count, cv) = &self.inner.running;
        let deadline = Instant::now() + timeout;
        let mut n = count.lock().unwrap_or_else(PoisonError::into_inner);
        while *n > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            n = cv
                .wait_timeout(n, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// 次の書き出しを断る理由（`blocked`・読めない記録）。なければ `None`。
    pub fn blocking(&self) -> Option<Blocking> {
        self.inner.store.scan().blocking()
    }

    fn acquire(&self) -> Result<SlotGuard, ExportError> {
        match lock::try_acquire(&self.inner.settings.state_dir)? {
            Slot::Got(guard) => Ok(guard),
            Slot::Busy => Err(ExportError::from(ErrorCode::Busy)),
        }
    }

    /// 起動時の復旧（`serve` の起動で 1 回）。実行枠が取れたときだけ、`staging/` の残骸を消し、未完了のジョブを表（4.2）の
    /// とおりに片付ける。取れなければ（CLI の `run` が動いている）何にも触れない。
    pub fn startup_recovery(&self) -> StartupReport {
        let guard = match self.acquire() {
            Ok(g) => g,
            Err(e) if e.code == ErrorCode::Busy => return StartupReport::Skipped,
            Err(e) => return StartupReport::Failed(e),
        };
        let ctx = self.inner.context();
        let cleaned = match ctx.store.clean_staging() {
            Ok(n) => n,
            Err(e) => return StartupReport::Failed(e),
        };
        let recovery = flow::recover_unfinished(&ctx, None);
        drop(guard);
        StartupReport::Done { cleaned, recovery }
    }

    /// 書き出しを受け付ける（上の「受付」）。`req.dry_run` は見ない（T6 は `dry_run` を [`Service::dry_run`] へ回す）。
    pub fn submit(&self, req: &ExportRequest) -> Result<Submitted, SubmitError> {
        self.submit_with_hook(req, &|| {})
    }

    /// [`Service::submit`] の、最初の `request_id` の照会と実行枠を取る間に `between` を呼ぶ形（別のプロセスが、その間に同じ
    /// `request_id` を登録した場合を、試験で決まった順に起こすため。本番は [`Service::submit`]）。
    #[doc(hidden)]
    pub fn submit_with_hook(
        &self,
        req: &ExportRequest,
        between: &dyn Fn(),
    ) -> Result<Submitted, SubmitError> {
        let _gate = self
            .inner
            .gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = self.find_by_request_id(req)? {
            return Ok(found);
        }
        between();
        let (guard, handle) = match self.begin(req, Mode::Submit)? {
            Began::Started(started) => *started,
            Began::Duplicate(found) => return Ok(found),
        };
        let job_id = handle.record.job_id.clone();
        let fallback = (handle.record.clone(), self.inner.store.clone());
        let inner = Arc::clone(&self.inner);
        // 動いているジョブの数（`wait_idle`）。スレッドの終わりで（パニックでも）減らす
        *self
            .inner
            .running
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner) += 1;
        let spawned = std::thread::Builder::new()
            .name("export-job".to_owned())
            .spawn({
                let inner = Arc::clone(&inner);
                move || {
                    let _running = RunningGuard(Arc::clone(&inner));
                    // `_running` より後に宣言する: パニックでも、実行枠を手放してから数を減らす（`wait_idle` が戻ったとき、
                    // 実行枠はもう空いている）
                    let guard = guard;
                    let mut handle = handle;
                    let ctx = inner.context();
                    flow::execute(&ctx, &mut handle, &guard);
                    // 終わりの記録を書いてから、実行枠を手放す
                    drop(guard);
                }
            });
        match spawned {
            Ok(_) => Ok(Submitted {
                job_id,
                status: Status::Running,
                reused: false,
            }),
            Err(_) => {
                RunningGuard(inner).release();
                let (record, store) = fallback;
                JobHandle::new(store, record).fail_internal();
                Err(SubmitError::Internal(ExportError::new(
                    ErrorCode::Internal,
                    "ジョブのスレッドを作れません",
                )))
            }
        }
    }

    /// `request_id` の重複の確認（直近 50 件）。見つかれば、実行枠を取らずに返す。
    fn find_by_request_id(&self, req: &ExportRequest) -> Result<Option<Submitted>, SubmitError> {
        let Some(rid) = &req.request_id else {
            return Ok(None);
        };
        let scan = self.inner.store.scan();
        let found = scan
            .recent(REQUEST_ID_WINDOW)
            .iter()
            .find(|r| r.request.request_id.as_ref() == Some(rid));
        match found {
            None => Ok(None),
            Some(r) if r.request.same_content(req) => Ok(Some(Submitted {
                job_id: r.job_id.clone(),
                status: r.status,
                reused: true,
            })),
            Some(_) => Err(SubmitError::RequestIdMismatch),
        }
    }

    /// 実行枠を取り、`blocked` でないことを確かめ、ジョブの記録（`running`）を書く。`request_id` は、取れなかったときと、
    /// 取れた直後（記録を書く前）に、もう一度照会する（別のプロセスが、最初の照会のあとに登録しているかもしれない）。
    fn begin(&self, req: &ExportRequest, mode: Mode) -> Result<Began, SubmitError> {
        let dedupe = mode == Mode::Submit;
        let guard = match self.acquire() {
            Ok(g) => g,
            Err(e) if e.code == ErrorCode::Busy => {
                if dedupe && let Some(found) = self.find_by_request_id(req)? {
                    return Ok(Began::Duplicate(found));
                }
                return Err(SubmitError::Busy);
            }
            Err(e) => return Err(SubmitError::Internal(e)),
        };
        if dedupe && let Some(found) = self.find_by_request_id(req)? {
            return Ok(Began::Duplicate(found));
        }
        if let Some(b) = self.inner.store.scan().blocking() {
            return Err(SubmitError::Blocked(b));
        }
        // 記録を書く直前（上の「受付」の 4）
        if mode == Mode::Submit && self.inner.cancel.is_cancelled() {
            return Err(SubmitError::ShuttingDown);
        }
        let record = JobRecord::new(JobId::generate().map_err(SubmitError::Internal)?, req);
        self.inner
            .store
            .write(&record)
            .map_err(SubmitError::Internal)?;
        Ok(Began::Started(Box::new((
            guard,
            JobHandle::new(self.inner.store.clone(), record),
        ))))
    }

    /// 同じ実行枠で、呼び出したスレッドの上で流す（`run` コマンド）。終わった記録を返す。`request_id` は見ない。
    pub fn run_foreground(&self, req: &ExportRequest) -> Result<JobRecord, SubmitError> {
        let Began::Started(started) = self.begin(req, Mode::Foreground)? else {
            unreachable!("request_id を見ないので、重複にはならない")
        };
        let (guard, mut handle) = *started;
        let ctx = self.inner.context();
        flow::execute(&ctx, &mut handle, &guard);
        Ok(handle.record)
    }

    /// `dry_run`（実行枠を取る。復旧はしない）。Drive は読み取りだけ。
    pub fn dry_run(&self, req: &ExportRequest) -> Result<Value, DryRunError> {
        let guard = self.acquire().map_err(|e| match e.code {
            ErrorCode::Busy => DryRunError::Busy,
            _ => DryRunError::Failed(e),
        })?;
        if let Some(b) = self.inner.store.scan().blocking() {
            return Err(DryRunError::Blocked(b));
        }
        flow::dry_run(&self.inner.context(), req, &guard).map_err(DryRunError::Failed)
    }

    /// ジョブの照会。ID は形を検査してからファイルを開く。
    pub fn lookup(&self, job_id: &str) -> Lookup {
        let Some(id) = JobId::parse(job_id) else {
            return Lookup::NotFound;
        };
        match self.inner.store.read(&id) {
            ReadOutcome::Found(rec) => Lookup::Found(rec.to_api_json()),
            ReadOutcome::NotFound => Lookup::NotFound,
            ReadOutcome::Unreadable => Lookup::Unreadable,
        }
    }

    /// 直近のジョブの一覧（新しいほうが先。`job_id`・`status`・`request`・時刻だけ）。`jobs/` を一覧できなければ `internal`。
    pub fn list(&self) -> Result<Vec<Value>, ExportError> {
        let scan = self.inner.store.scan();
        if scan.dir_unreadable {
            return Err(ExportError::new(
                ErrorCode::Internal,
                "ジョブの記録を一覧できません",
            ));
        }
        Ok(scan
            .recent(REQUEST_ID_WINDOW)
            .iter()
            .map(JobRecord::to_list_json)
            .collect())
    }
}
