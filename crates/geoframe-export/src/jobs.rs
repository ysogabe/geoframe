//! ジョブの記録 `<state-dir>/jobs/<job_id>.json`（design.md 3.3、7 章）と、状態ディレクトリの配置。
//!
//! 書き方は公開の記録と同じ（[`crate::jsonfile::write_json_atomic`]。一時ファイルに書いて `fsync`、`rename`）。読めるのは
//! 「前の完全な記録」か「新しい完全な記録」のどちらかだけ。ファイル名は `<job_id>.json`（`[0-9a-f]{32}`）で、`job_id` は形を
//! 検査してからパスにする（[`crate::jobid::JobId`]）。
//!
//! ## 記録の中身
//!
//! `job_id`・`status`（`running`・`succeeded`・`conflict`・`failed`・`interrupted`・`blocked`）・要求（`from`・`to`・
//! `destination`・`on_exists`・`request_id`）・`started_at`/`finished_at`（`2026-01-12T19:30:00+09:00` のような時差つきの
//! RFC 3339）・`started_unix`（並べ替えと間引きの順を決める数値の時刻。文字列の比較では決めない）・段階（`progress`。
//! [`crate::publish::ProgressSink`] の実装が書く）・`summary`（公開の前に分かった数。起動時の復旧が、止まったジョブを
//! 成功にするときの結果に使う）・`result`（6.1 の形）・`error`（コードだけ。説明は固定の短い文を [`ErrorCode::message`] から
//! 作る。rclone の出力は入れない）・`blocked`（`blocked` にしたときの理由と、`recover` が出す説明）・`resolved_at`
//! （`recover --resolve` で解除した日時）・`notes`（結果に添える知らせ。残った `incoming`、公開の記録に書けなかった、など）。
//!
//! ## 間引き・blocked
//!
//! - 間引くのは、読めて「終わった」と判定できた記録（`succeeded`・`conflict`・`failed`・`interrupted`・解除済みの `blocked`）の
//!   うち、新しいほうから 50 件を超えたものだけ。未完了（`running`）と、解除していない `blocked` は消さない。読めない記録は、
//!   消さず・上書きしない。
//! - **読めない・形の崩れた記録（`<32hex>.json`）、`jobs/` を一覧できない、解除していない `blocked` の記録があれば、次の
//!   書き出しを断る**（[`Scan::blocking`]）。読めない記録は `recover --resolve` では解除できない（記録の中身を信じられない
//!   ため）。オーナーが中身を確かめて、そのファイルを手で退かす。

use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::date::Period;
use crate::error::{ErrorCode, ExportError};
use crate::jobid::JobId;
use crate::jsonfile::{self, ReadError};
use crate::rclone::is_valid_drive_id;
use crate::recovery::{Progress, Stage};
use crate::request::{Destination, ExportRequest, OnExists, RequestId};
use crate::sys;

/// 終わったジョブの記録を残す数。
pub const KEEP_FINISHED: usize = 50;
/// `request_id` を探す範囲（新しいほうから）。
pub const REQUEST_ID_WINDOW: usize = 50;
/// 記録 1 つを読む上限（除外の一覧が 2,000 件でも 200KB ほど）。
pub const RECORD_MAX_BYTES: u64 = 2 * 1024 * 1024;
const VERSION: u64 = 1;
const NOTES_MAX: usize = 32;
const NOTE_MAX_CHARS: usize = 4000;

fn internal(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::Internal, detail)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Succeeded,
    Conflict,
    Failed,
    Interrupted,
    /// 照合できなかった。解除（`resolved_at`）するまで、次の書き出しを断る。
    Blocked,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Conflict => "conflict",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Running,
            Self::Succeeded,
            Self::Conflict,
            Self::Failed,
            Self::Interrupted,
            Self::Blocked,
        ]
        .into_iter()
        .find(|s2| s2.as_str() == s)
    }
}

/// 記録した要求（`request_id` を含む）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestInfo {
    pub period: Period,
    pub destination: Destination,
    pub on_exists: OnExists,
    pub request_id: Option<RequestId>,
}

impl RequestInfo {
    pub fn from_request(r: &ExportRequest) -> Self {
        Self {
            period: r.period,
            destination: r.destination,
            on_exists: r.on_exists,
            request_id: r.request_id.clone(),
        }
    }

    /// 期間・宛先・`on_exists` が同じか（`request_id` の再送の判定。`dry_run` は見ない）。
    pub fn same_content(&self, r: &ExportRequest) -> bool {
        self.period == r.period
            && self.destination == r.destination
            && self.on_exists == r.on_exists
    }

    /// 6.1 の `request`（`request_id` は含めない）。
    pub fn to_api_json(&self) -> Value {
        json!({
            "from": self.period.from().to_string(),
            "to": self.period.to().to_string(),
            "destination": self.destination.as_str(),
            "on_exists": self.on_exists.as_str(),
        })
    }

    fn to_file_json(&self) -> Value {
        let mut v = self.to_api_json();
        v["request_id"] = self
            .request_id
            .as_ref()
            .map_or(Value::Null, |r| json!(r.as_str()));
        v
    }

    fn from_file_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k)?.as_str();
        let period = Period::parse(Some(s("from")?), Some(s("to")?)).ok()?;
        let request_id = match v.get("request_id")? {
            Value::Null => None,
            Value::String(id) => Some(RequestId::parse(id)?),
            _ => return None,
        };
        Some(Self {
            period,
            destination: Destination::parse(s("destination")?)?,
            on_exists: OnExists::parse(s("on_exists")?)?,
            request_id,
        })
    }
}

/// `blocked` にした理由と、`recover` が出す説明（Drive の ID を含む。設定・資格情報は含まない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedNote {
    pub reason: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub job_id: JobId,
    pub status: Status,
    pub request: RequestInfo,
    pub started_at: String,
    pub started_unix: i64,
    pub finished_at: Option<String>,
    /// 最後に記録できた段階と ID。
    pub progress: Option<Progress>,
    /// 公開の前に分かった数（[`crate::flow`] の `Counts`）。
    pub summary: Option<Value>,
    /// 6.1 の `result`。
    pub result: Option<Value>,
    pub error: Option<ErrorCode>,
    pub blocked: Option<BlockedNote>,
    pub resolved_at: Option<String>,
    pub notes: Vec<String>,
}

fn progress_json(p: &Progress) -> Value {
    json!({"stage": p.stage.as_str(), "old_id": p.old_id, "incoming_id": p.incoming_id})
}

fn progress_from_json(v: &Value) -> Option<Progress> {
    let id = |k: &str| match v.get(k)? {
        Value::Null => Some(None),
        Value::String(s) => is_valid_drive_id(s).then(|| Some(s.clone())),
        _ => None,
    };
    Some(Progress {
        stage: Stage::parse(v.get("stage")?.as_str()?)?,
        old_id: id("old_id")?,
        incoming_id: id("incoming_id")?,
    })
}

fn is_time_text(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b':' | b'T' | b'+' | b'Z' | b'.'))
}

fn opt_time(v: &Value, key: &str) -> Option<Option<String>> {
    match v.get(key)? {
        Value::Null => Some(None),
        Value::String(s) if is_time_text(s) => Some(Some(s.clone())),
        _ => None,
    }
}

fn opt_object(v: &Value, key: &str) -> Option<Option<Value>> {
    match v.get(key)? {
        Value::Null => Some(None),
        o @ Value::Object(_) => Some(Some(o.clone())),
        _ => None,
    }
}

impl JobRecord {
    /// 受け付けたばかりの記録（`running`。段階なし）。
    pub fn new(job_id: JobId, request: &ExportRequest) -> Self {
        let now = sys::unix_now();
        Self {
            job_id,
            status: Status::Running,
            request: RequestInfo::from_request(request),
            started_at: sys::rfc3339_local(now),
            started_unix: now,
            finished_at: None,
            progress: None,
            summary: None,
            result: None,
            error: None,
            blocked: None,
            resolved_at: None,
            notes: Vec::new(),
        }
    }

    /// 未完了（実行中、または実行中に止まった）。
    pub fn is_unfinished(&self) -> bool {
        self.status == Status::Running
    }

    /// 解除していない `blocked`。
    pub fn is_blocking(&self) -> bool {
        self.status == Status::Blocked && self.resolved_at.is_none()
    }

    /// 終わった（間引いてよい）。未完了と、解除していない `blocked` は含まない。
    pub fn is_finished(&self) -> bool {
        !self.is_unfinished() && !self.is_blocking()
    }

    /// 並べ替えの鍵（新しいほうが大きい。同値は `job_id`）。
    fn order_key(&self) -> (i64, &str) {
        (self.started_unix, self.job_id.as_str())
    }

    pub fn to_file_json(&self) -> Value {
        json!({
            "version": VERSION,
            "job_id": self.job_id.as_str(),
            "status": self.status.as_str(),
            "request": self.request.to_file_json(),
            "started_at": self.started_at,
            "started_unix": self.started_unix,
            "finished_at": self.finished_at,
            "progress": self.progress.as_ref().map(progress_json),
            "summary": self.summary,
            "result": self.result,
            "error": self.error.map(|c| json!({"code": c.as_str()})),
            "blocked": self.blocked.as_ref().map(|b| json!({"reason": b.reason, "description": b.description})),
            "resolved_at": self.resolved_at,
            "notes": self.notes,
        })
    }

    /// 記録を読み戻す。形が崩れていれば `None`。`expect` はファイル名の `job_id`（中の値と一致すること）。
    pub fn from_file_json(v: &Value, expect: &JobId) -> Option<Self> {
        if v.get("version")?.as_u64()? != VERSION {
            return None;
        }
        let job_id = JobId::parse(v.get("job_id")?.as_str()?)?;
        if &job_id != expect {
            return None;
        }
        let error = match v.get("error")? {
            Value::Null => None,
            e => Some(ErrorCode::parse(e.get("code")?.as_str()?)?),
        };
        let blocked = match v.get("blocked")? {
            Value::Null => None,
            b => {
                let reason = b.get("reason")?.as_str()?;
                let description = b.get("description")?.as_str()?;
                if reason.len() > 64 || description.chars().count() > 8 * NOTE_MAX_CHARS {
                    return None;
                }
                Some(BlockedNote {
                    reason: reason.to_owned(),
                    description: description.to_owned(),
                })
            }
        };
        let progress = match v.get("progress")? {
            Value::Null => None,
            p => Some(progress_from_json(p)?),
        };
        let notes_json = v.get("notes")?.as_array()?;
        if notes_json.len() > NOTES_MAX {
            return None;
        }
        let mut notes = Vec::new();
        for n in notes_json {
            let n = n.as_str()?;
            if n.chars().count() > NOTE_MAX_CHARS {
                return None;
            }
            notes.push(n.to_owned());
        }
        let started_at = v.get("started_at")?.as_str()?;
        if !is_time_text(started_at) {
            return None;
        }
        Some(Self {
            job_id,
            status: Status::parse(v.get("status")?.as_str()?)?,
            request: RequestInfo::from_file_json(v.get("request")?)?,
            started_at: started_at.to_owned(),
            started_unix: v.get("started_unix")?.as_i64()?,
            finished_at: opt_time(v, "finished_at")?,
            progress,
            summary: opt_object(v, "summary")?,
            result: opt_object(v, "result")?,
            error,
            blocked,
            resolved_at: opt_time(v, "resolved_at")?,
            notes,
        })
    }

    /// `GET /v1/exports/{job_id}` と、`run` の標準出力の形（6.1）。解除済みの `blocked` には `resolved_at`、知らせがあれば
    /// `notes` を足す（どちらも無ければキーを出さない）。
    pub fn to_api_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("job_id".into(), json!(self.job_id.as_str()));
        m.insert("status".into(), json!(self.status.as_str()));
        m.insert("request".into(), self.request.to_api_json());
        m.insert("started_at".into(), json!(self.started_at));
        m.insert("finished_at".into(), json!(self.finished_at));
        m.insert("result".into(), self.result.clone().unwrap_or(Value::Null));
        m.insert("error".into(), error_json(self.error));
        if let Some(t) = &self.resolved_at {
            m.insert("resolved_at".into(), json!(t));
        }
        if !self.notes.is_empty() {
            m.insert("notes".into(), json!(self.notes));
        }
        Value::Object(m)
    }

    /// `GET /v1/exports` の 1 件（`job_id`・`status`・`request`・時刻だけ）。
    pub fn to_list_json(&self) -> Value {
        json!({
            "job_id": self.job_id.as_str(),
            "status": self.status.as_str(),
            "request": self.request.to_api_json(),
            "started_at": self.started_at,
            "finished_at": self.finished_at,
        })
    }
}

/// `error`（コードと固定の短い説明）。
pub fn error_json(code: Option<ErrorCode>) -> Value {
    code.map_or(
        Value::Null,
        |c| json!({"code": c.as_str(), "message": c.message()}),
    )
}

/// 次の書き出しを断る理由（記録の走査で見つかったもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocking {
    /// 解除していない `blocked` のジョブ。
    Job { job_id: JobId, reason: String },
    /// 読めない・形の崩れた記録（ファイル名）。
    UnreadableRecord(String),
    /// `jobs/` を一覧できない。
    UnreadableDir,
}

impl Blocking {
    /// 表示用の説明（Drive の ID・パスは含まない）。
    pub fn text(&self) -> String {
        match self {
            Self::Job { job_id, reason } => format!(
                "ジョブ {job_id} が blocked のままです（{reason}）。Drive を確かめて、geoframe-export recover --resolve {job_id} で解除してください"
            ),
            Self::UnreadableRecord(name) => format!(
                "ジョブの記録 jobs/{name} を読めません。中身を確かめて、そのファイルを手で退かしてください（recover --resolve では解除できません）"
            ),
            Self::UnreadableDir => "ジョブの記録のディレクトリ jobs/ を一覧できません".to_owned(),
        }
    }
}

/// `jobs/` の走査の結果。
#[derive(Debug, Default)]
pub struct Scan {
    /// 読めた記録。新しいほうが先。
    pub records: Vec<JobRecord>,
    /// 読めない記録のファイル名（`<32hex>.json`）。
    pub unreadable: Vec<String>,
    pub dir_unreadable: bool,
}

impl Scan {
    /// 次の書き出しを断る理由。なければ `None`。
    pub fn blocking(&self) -> Option<Blocking> {
        if self.dir_unreadable {
            return Some(Blocking::UnreadableDir);
        }
        if let Some(name) = self.unreadable.first() {
            return Some(Blocking::UnreadableRecord(name.clone()));
        }
        self.records
            .iter()
            .find(|r| r.is_blocking())
            .map(|r| Blocking::Job {
                job_id: r.job_id.clone(),
                reason: r
                    .blocked
                    .as_ref()
                    .map_or_else(|| "理由の記録なし".to_owned(), |b| b.reason.clone()),
            })
    }

    /// 未完了の記録（古いほうが先）。
    pub fn unfinished(&self) -> Vec<&JobRecord> {
        let mut v: Vec<&JobRecord> = self.records.iter().filter(|r| r.is_unfinished()).collect();
        v.reverse();
        v
    }

    /// 新しいほうから `n` 件（`request_id` を探す範囲）。
    pub fn recent(&self, n: usize) -> &[JobRecord] {
        &self.records[..self.records.len().min(n)]
    }
}

/// 状態ディレクトリの `jobs/`・`staging/`（どちらも 0700）。
#[derive(Debug, Clone)]
pub struct JobStore {
    jobs: PathBuf,
    staging: PathBuf,
}

/// 実在するディレクトリ（リンクでない）で、なければ 0700 で作る。
fn ensure_dir(path: &Path, what: &'static str) -> Result<(), ExportError> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && m.uid() == sys::euid() => Ok(()),
        Ok(_) => Err(internal(what)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| internal(what)),
        Err(_) => Err(internal(what)),
    }
}

impl JobStore {
    /// `<state-dir>/jobs/`・`<state-dir>/staging/` を（無ければ 0700 で）用意する。状態ディレクトリ自体は、あるものとする。
    pub fn open(state_dir: &Path) -> Result<Self, ExportError> {
        let state_ok = std::fs::metadata(state_dir).is_ok_and(|m| m.is_dir());
        if !state_ok {
            return Err(internal("状態ディレクトリがありません"));
        }
        let jobs = state_dir.join("jobs");
        let staging = state_dir.join("staging");
        ensure_dir(&jobs, "jobs/ を用意できません")?;
        ensure_dir(&staging, "staging/ を用意できません")?;
        Ok(Self { jobs, staging })
    }

    /// 作業領域の親（`StateDirectory/staging/`）。
    pub fn staging_dir(&self) -> &Path {
        &self.staging
    }

    fn path_of(&self, id: &JobId) -> PathBuf {
        self.jobs.join(format!("{}.json", id.as_str()))
    }

    /// 記録のパス（`recover` の案内に出す）。
    pub fn record_path(&self, id: &JobId) -> PathBuf {
        self.path_of(id)
    }

    /// 記録をアトミックに書く。
    pub fn write(&self, rec: &JobRecord) -> Result<(), ExportError> {
        jsonfile::write_json_atomic(&self.path_of(&rec.job_id), &rec.to_file_json())
            .map_err(|_| internal("ジョブの記録を書けません"))
    }

    pub fn read(&self, id: &JobId) -> ReadOutcome {
        match jsonfile::read_json(&self.path_of(id), RECORD_MAX_BYTES) {
            Ok(v) => JobRecord::from_file_json(&v, id)
                .map_or(ReadOutcome::Unreadable, |r| ReadOutcome::Found(Box::new(r))),
            Err(ReadError::NotFound) => ReadOutcome::NotFound,
            Err(_) => ReadOutcome::Unreadable,
        }
    }

    /// `jobs/` の全部を読む。`<32hex>.json` の形の名前だけを記録として扱う（一時ファイルなどは無視）。
    pub fn scan(&self) -> Scan {
        let mut scan = Scan::default();
        let Ok(dir) = std::fs::read_dir(&self.jobs) else {
            scan.dir_unreadable = true;
            return scan;
        };
        let mut ids = Vec::new();
        for entry in dir {
            let Ok(entry) = entry else {
                scan.dir_unreadable = true;
                continue;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name.strip_suffix(".json").and_then(JobId::parse) else {
                continue;
            };
            ids.push(id);
        }
        for id in ids {
            match self.read(&id) {
                ReadOutcome::Found(rec) => scan.records.push(*rec),
                // 一覧のあとで消された（間引き）ときは、無いものとして扱う
                ReadOutcome::NotFound => {}
                ReadOutcome::Unreadable => scan.unreadable.push(format!("{id}.json")),
            }
        }
        scan.unreadable.sort();
        scan.records
            .sort_by(|a, b| b.order_key().cmp(&a.order_key()));
        scan
    }

    /// 終わった記録を、新しいほうから [`KEEP_FINISHED`] 件だけ残す。未完了・解除していない `blocked`・読めない記録は消さない。
    /// 消せなくても失敗にはしない（次の機会にまた試す）。
    pub fn prune(&self) {
        self.prune_keeping(KEEP_FINISHED);
    }

    #[doc(hidden)]
    pub fn prune_keeping(&self, keep: usize) {
        let scan = self.scan();
        let finished = scan.records.iter().filter(|r| r.is_finished());
        for (i, rec) in finished.enumerate() {
            if i >= keep {
                let _ = std::fs::remove_file(self.path_of(&rec.job_id));
            }
        }
    }

    /// `staging/` の直下を消す（起動時の残骸の掃除。実行枠を取ったときだけ呼ぶ）。消すのは `staging/` の中だけで、
    /// リンクはたどらない。`staging/` がリンク・ディレクトリでないときは何もせず失敗にする。
    pub fn clean_staging(&self) -> Result<usize, ExportError> {
        let meta = std::fs::symlink_metadata(&self.staging)
            .map_err(|_| internal("staging/ を確かめられません"))?;
        if !meta.is_dir() {
            return Err(internal("staging/ がディレクトリではありません"));
        }
        let dir =
            std::fs::read_dir(&self.staging).map_err(|_| internal("staging/ を一覧できません"))?;
        let mut removed = 0;
        for entry in dir {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let done = if is_dir {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if done.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[derive(Debug)]
pub enum ReadOutcome {
    Found(Box<JobRecord>),
    NotFound,
    /// 読めない・形が崩れている。
    Unreadable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RawRequest;
    use std::collections::HashSet;

    fn unit_tmp() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/unit-tmp")
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = unit_tmp().join(format!("jobs-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            assert_eq!(std::fs::read_dir(&p).unwrap().count(), 0);
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn request(request_id: Option<&str>) -> ExportRequest {
        RawRequest {
            from: Some("2030-03-10".into()),
            to: Some("2030-03-11".into()),
            destination: Some("yamareco".into()),
            request_id: request_id.map(str::to_owned),
            ..RawRequest::default()
        }
        .validate()
        .unwrap()
    }

    fn id(n: u32) -> JobId {
        JobId::parse(&format!("{n:032x}")).unwrap()
    }

    fn record(n: u32, status: Status, unix: i64) -> JobRecord {
        let mut r = JobRecord::new(id(n), &request(None));
        r.status = status;
        r.started_unix = unix;
        r
    }

    #[test]
    fn a_record_round_trips_through_its_file_form() {
        let mut r = JobRecord::new(id(1), &request(Some("req-1")));
        r.status = Status::Blocked;
        r.finished_at = Some("2030-01-01T00:00:00+09:00".into());
        r.progress = Some(Progress {
            stage: Stage::Retiring,
            old_id: Some("oldid1".into()),
            incoming_id: Some("inc1".into()),
        });
        r.summary = Some(json!({"catalog_count": 3}));
        r.result = Some(json!({"count": 2}));
        r.error = Some(ErrorCode::Blocked);
        r.blocked = Some(BlockedNote {
            reason: "stage_inconsistent".into(),
            description: "説明".into(),
        });
        r.resolved_at = Some("2030-01-02T00:00:00+09:00".into());
        r.notes = vec!["知らせ".into()];
        let back = JobRecord::from_file_json(&r.to_file_json(), &id(1)).unwrap();
        assert_eq!(back, r);
        assert!(!back.is_blocking(), "解除済み");
        assert!(back.is_finished());
        // ほかの job_id のファイル名で読むと、形が崩れた記録として断る
        assert!(JobRecord::from_file_json(&r.to_file_json(), &id(2)).is_none());
    }

    #[test]
    fn bad_shapes_are_refused() {
        let good = record(1, Status::Running, 1).to_file_json();
        assert!(JobRecord::from_file_json(&good, &id(1)).is_some());
        for (key, value) in [
            ("version", json!(2)),
            ("status", json!("done")),
            ("started_unix", json!("x")),
            ("started_at", json!("rm -rf")),
            ("error", json!({"code": "interrupted"})),
            (
                "progress",
                json!({"stage": "x", "old_id": null, "incoming_id": null}),
            ),
            (
                "progress",
                json!({"stage": "uploading", "old_id": "../x", "incoming_id": null}),
            ),
            (
                "request",
                json!({"from": "2030-02-30", "to": "2030-02-30", "destination": "yamareco", "on_exists": "error", "request_id": null}),
            ),
            ("result", json!([1])),
            ("notes", json!([1])),
        ] {
            let mut v = good.clone();
            v[key] = value;
            assert!(JobRecord::from_file_json(&v, &id(1)).is_none(), "{key}");
        }
        let mut v = good.clone();
        v.as_object_mut().unwrap().remove("notes");
        assert!(JobRecord::from_file_json(&v, &id(1)).is_none());
    }

    #[test]
    fn the_api_form_has_the_documented_keys_and_hides_the_request_id() {
        let mut r = JobRecord::new(id(1), &request(Some("secret-id")));
        let v = r.to_api_json();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "job_id",
                "status",
                "request",
                "started_at",
                "finished_at",
                "result",
                "error"
            ]
        );
        assert_eq!(v["request"]["to"], "2030-03-11");
        assert!(!v.to_string().contains("secret-id"));
        r.status = Status::Failed;
        r.error = Some(ErrorCode::NothingToExport);
        r.notes = vec!["n".into()];
        let v = r.to_api_json();
        assert_eq!(v["error"]["code"], "nothing_to_export");
        assert!(
            v["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty())
        );
        assert_eq!(v["notes"][0], "n");
        assert_eq!(r.to_list_json().as_object().unwrap().len(), 5);
    }

    #[test]
    fn pruning_keeps_the_newest_finished_and_every_unfinished_or_blocked_record() {
        let d = Dir::new("prune");
        let store = JobStore::open(&d.0).unwrap();
        // 終わったもの 60 件（古いほうから 1..=60）
        for n in 1..=60u32 {
            store
                .write(&record(n, Status::Succeeded, i64::from(n)))
                .unwrap();
        }
        // いちばん古い未完了と、解除していない blocked、解除済みの blocked
        store.write(&record(100, Status::Running, 0)).unwrap();
        store.write(&record(101, Status::Blocked, 0)).unwrap();
        let mut resolved = record(102, Status::Blocked, 0);
        resolved.resolved_at = Some("2030-01-01T00:00:00+09:00".into());
        store.write(&resolved).unwrap();
        // 読めない記録
        std::fs::write(d.0.join("jobs").join(format!("{}.json", id(103))), b"{x").unwrap();
        // 無関係な名前
        std::fs::write(d.0.join("jobs").join("notes.txt"), b"x").unwrap();

        store.prune();
        let scan = store.scan();
        let kept: HashSet<String> = scan.records.iter().map(|r| r.job_id.to_string()).collect();
        // 新しいほうの 50 件（11..=60）。解除済みの blocked（古い）は終わった側に数えるので、50 件に入らず消える
        for n in 11..=60u32 {
            assert!(kept.contains(&id(n).to_string()), "{n}");
        }
        for n in 1..=10u32 {
            assert!(!kept.contains(&id(n).to_string()), "{n}");
        }
        assert!(kept.contains(&id(100).to_string()), "未完了は消さない");
        assert!(kept.contains(&id(101).to_string()), "blocked は消さない");
        assert!(!kept.contains(&id(102).to_string()), "解除済みは間引く");
        assert_eq!(scan.unreadable.len(), 1, "読めない記録は消さない");
        assert!(d.0.join("jobs/notes.txt").exists());
    }

    #[test]
    fn a_scan_orders_by_the_numeric_time_then_the_job_id() {
        let d = Dir::new("order");
        let store = JobStore::open(&d.0).unwrap();
        store.write(&record(1, Status::Succeeded, 5)).unwrap();
        store.write(&record(2, Status::Succeeded, 9)).unwrap();
        store.write(&record(3, Status::Succeeded, 9)).unwrap();
        let ids: Vec<String> = store
            .scan()
            .records
            .iter()
            .map(|r| r.job_id.to_string())
            .collect();
        assert_eq!(
            ids,
            [id(3).to_string(), id(2).to_string(), id(1).to_string()]
        );
    }

    #[test]
    fn blocking_comes_from_blocked_unreadable_or_a_missing_directory() {
        let d = Dir::new("blocking");
        let store = JobStore::open(&d.0).unwrap();
        assert_eq!(store.scan().blocking(), None);
        store.write(&record(1, Status::Running, 1)).unwrap();
        store.write(&record(2, Status::Failed, 2)).unwrap();
        assert_eq!(store.scan().blocking(), None);
        let mut b = record(3, Status::Blocked, 3);
        b.blocked = Some(BlockedNote {
            reason: "unobservable".into(),
            description: String::new(),
        });
        store.write(&b).unwrap();
        assert!(matches!(
            store.scan().blocking(),
            Some(Blocking::Job { reason, .. }) if reason == "unobservable"
        ));
        b.resolved_at = Some("2030-01-01T00:00:00+09:00".into());
        store.write(&b).unwrap();
        assert_eq!(store.scan().blocking(), None);
        std::fs::write(d.0.join("jobs").join(format!("{}.json", id(4))), b"").unwrap();
        assert!(matches!(
            store.scan().blocking(),
            Some(Blocking::UnreadableRecord(n)) if n == format!("{}.json", id(4))
        ));
        std::fs::remove_dir_all(d.0.join("jobs")).unwrap();
        assert_eq!(store.scan().blocking(), Some(Blocking::UnreadableDir));
    }

    #[test]
    fn staging_cleanup_removes_only_what_is_under_staging_and_follows_no_links() {
        let d = Dir::new("staging");
        let store = JobStore::open(&d.0).unwrap();
        let outside = d.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"x").unwrap();
        let stage = store.staging_dir();
        std::fs::create_dir_all(stage.join("job1/2030-03-10")).unwrap();
        std::fs::write(stage.join("job1/2030-03-10/a.jpg"), b"x").unwrap();
        std::fs::write(stage.join("stray.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, stage.join("link")).unwrap();
        assert_eq!(store.clean_staging().unwrap(), 3);
        assert_eq!(std::fs::read_dir(stage).unwrap().count(), 0);
        assert!(outside.join("keep.txt").exists(), "リンクの先は消さない");
    }

    #[test]
    fn a_staging_that_is_a_link_is_refused() {
        let d = Dir::new("staginglink");
        let store = JobStore::open(&d.0).unwrap();
        let real = d.0.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("keep.txt"), b"x").unwrap();
        std::fs::remove_dir(store.staging_dir()).unwrap();
        std::os::unix::fs::symlink(&real, store.staging_dir()).unwrap();
        assert!(store.clean_staging().is_err());
        assert!(real.join("keep.txt").exists());
        assert!(
            JobStore::open(&d.0).is_err(),
            "リンクの staging/ は使わない"
        );
    }

    #[test]
    fn same_content_ignores_the_request_id_and_dry_run() {
        let info = RequestInfo::from_request(&request(Some("a")));
        assert!(info.same_content(&request(Some("b"))));
        let other = RawRequest {
            from: Some("2030-03-10".into()),
            to: Some("2030-03-11".into()),
            destination: Some("yamareco".into()),
            on_exists: Some("replace".into()),
            ..RawRequest::default()
        }
        .validate()
        .unwrap();
        assert!(!info.same_content(&other));
    }
}
