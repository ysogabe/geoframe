//! 記録の形式の固定（`tests/fixtures/records/`）: ジョブの記録と公開の記録の実例を、今のコードが読めること、今の書き手が
//! 同じバイト列を書くこと。Drive の操作の実装を替えても、記録の形式は変わらない（前のバイナリが読める）ことの証拠にする。
//! 実例は、今のコードに実際に書かせたもの（作り方は `tests/fixtures/records/README.md`）。値は全部、架空。

mod common;

use std::path::{Path, PathBuf};

use common::TempDir;
use geoframe_export::error::ErrorCode;
use geoframe_export::folder::FolderName;
use geoframe_export::jobid::JobId;
use geoframe_export::jobs::{JobRecord, JobStore, ReadOutcome, Status};
use geoframe_export::published::{Loaded, PublishedStore};
use geoframe_export::recovery::Stage;
use geoframe_export::request::Destination;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/records")
}

/// 実例のジョブの記録を、状態ディレクトリの `jobs/<job_id>.json` に置いて、本物の読み手で読む。
struct Jobs {
    _dir: TempDir,
    store: JobStore,
}

impl Jobs {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let store = JobStore::open(&state).unwrap();
        for entry in std::fs::read_dir(fixtures().join("jobs"))
            .unwrap()
            .flatten()
        {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            let id = v["job_id"].as_str().unwrap();
            std::fs::write(store.record_path(&JobId::parse(id).unwrap()), text).unwrap();
        }
        Self { _dir: dir, store }
    }

    /// 実例（ファイル名の前半）の記録を読む。
    fn read(&self, case: &str) -> JobRecord {
        let text = std::fs::read_to_string(fixtures().join(format!("jobs/{case}.json"))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let id = JobId::parse(v["job_id"].as_str().unwrap()).unwrap();
        match self.store.read(&id) {
            ReadOutcome::Found(r) => *r,
            other => panic!("{case}: 読めません: {other:?}"),
        }
    }
}

const CASES: [&str; 10] = [
    "succeeded_new_folder",
    "succeeded_replace",
    "conflict_exists",
    "failed_fetch",
    "failed_drive_auth_invalid",
    "interrupted_before_drive",
    "blocked_stage_inconsistent",
    "running_uploading",
    "running_publishing_with_digests",
    "running_trashing",
];

#[test]
fn every_job_record_example_is_readable_and_the_current_writer_gives_the_same_bytes() {
    let jobs = Jobs::new("record-format-jobs");
    for case in CASES {
        let rec = jobs.read(case);
        // 今の書き手が、実例と同じバイト列（キーの順・整形・末尾の改行）を書く
        let again = TempDir::new(&format!("record-format-again-{case}"));
        let store = JobStore::open(again.path()).unwrap();
        store.write(&rec).unwrap();
        let written = std::fs::read_to_string(store.record_path(&rec.job_id)).unwrap();
        let example =
            std::fs::read_to_string(fixtures().join(format!("jobs/{case}.json"))).unwrap();
        assert_eq!(written, example, "{case}");
    }
    // 全部まとめて走査しても、読めない記録は無い
    let scan = jobs.store.scan();
    assert!(scan.unreadable.is_empty(), "{:?}", scan.unreadable);
    assert!(!scan.dir_unreadable);
    assert_eq!(scan.records.len(), CASES.len());
}

#[test]
fn succeeded_records_carry_the_result_the_summary_and_the_verified_md5s() {
    let jobs = Jobs::new("record-format-succeeded");

    let new = jobs.read("succeeded_new_folder");
    assert_eq!(new.status, Status::Succeeded);
    assert_eq!(new.request.destination, Destination::Yamareco);
    assert_eq!(
        new.request.request_id.as_ref().map(|r| r.as_str()),
        Some("req-1")
    );
    assert_eq!(new.error, None);
    assert!(new.is_finished());
    let result = new.result.as_ref().unwrap();
    assert_eq!(result["folder"], "2030-03-10");
    assert_eq!(result["count"], 3);
    assert_eq!(result["uploaded"], true);
    assert_eq!(result["replaced"], false);
    assert_eq!(new.summary.as_ref().unwrap()["catalog_count"], 3);
    let p = new.progress.as_ref().unwrap();
    assert_eq!(p.stage, Stage::Publishing);
    assert_eq!(
        (p.old_id.as_deref(), p.incoming_id.as_deref()),
        (None, Some("fid0005"))
    );
    let digests = p.digests.as_ref().unwrap();
    assert_eq!(digests.len(), 4);
    assert_eq!(digests.last().unwrap().path, "manifest.json");

    let replace = jobs.read("succeeded_replace");
    assert_eq!(replace.status, Status::Succeeded);
    assert_eq!(replace.result.as_ref().unwrap()["replaced"], true);
    assert_eq!(replace.result.as_ref().unwrap()["existing_files"], 2);
    let p = replace.progress.as_ref().unwrap();
    assert_eq!(p.stage, Stage::Trashing);
    assert_eq!(
        (p.old_id.as_deref(), p.incoming_id.as_deref()),
        (Some("fid0004"), Some("fid0008"))
    );
}

#[test]
fn failed_conflict_and_interrupted_records_keep_their_error_codes() {
    let jobs = Jobs::new("record-format-ends");

    let conflict = jobs.read("conflict_exists");
    assert_eq!(conflict.status, Status::Conflict);
    assert_eq!(conflict.error, None);
    assert_eq!(conflict.result.as_ref().unwrap()["conflict"], "exists");
    assert_eq!(conflict.result.as_ref().unwrap()["uploaded"], false);

    let failed = jobs.read("failed_fetch");
    assert_eq!(failed.status, Status::Failed);
    assert_eq!(failed.error, Some(ErrorCode::FetchFailed));
    assert_eq!(failed.result.as_ref().unwrap()["count"], 0);

    // まだどこからも書かない綴り。前のバイナリが記録を読めて、blocked にならないこと
    let auth = jobs.read("failed_drive_auth_invalid");
    assert_eq!(auth.status, Status::Failed);
    assert_eq!(auth.error, Some(ErrorCode::DriveAuthInvalid));
    assert!(auth.is_finished() && !auth.is_blocking());
    // 前の版でも、この記録は未完了でも解除待ちでもない（次の書き出しを止めない）
    assert!(!auth.is_unfinished());

    let interrupted = jobs.read("interrupted_before_drive");
    assert_eq!(interrupted.status, Status::Interrupted);
    assert_eq!(interrupted.error, None);
    assert!(interrupted.finished_at.is_some());
    assert!(interrupted.progress.is_none());
}

#[test]
fn blocked_and_unfinished_records_keep_the_stage_and_the_ids_recovery_needs() {
    let jobs = Jobs::new("record-format-stuck");

    let blocked = jobs.read("blocked_stage_inconsistent");
    assert_eq!(blocked.status, Status::Blocked);
    assert!(blocked.is_blocking());
    assert_eq!(blocked.error, Some(ErrorCode::Blocked));
    let note = blocked.blocked.as_ref().unwrap();
    assert_eq!(note.reason, "stage_inconsistent");
    assert!(note.description.contains("publishing"));
    assert_eq!(blocked.progress.as_ref().unwrap().stage, Stage::Publishing);
    assert_eq!(
        blocked.progress.as_ref().unwrap().incoming_id.as_deref(),
        Some("fid-gone")
    );

    let uploading = jobs.read("running_uploading");
    assert!(uploading.is_unfinished());
    let p = uploading.progress.as_ref().unwrap();
    assert_eq!(p.stage, Stage::Uploading);
    assert_eq!(
        (p.old_id.as_deref(), p.incoming_id.as_deref()),
        (None, Some("fid0010"))
    );
    assert!(p.digests.is_none(), "MD5 は検証のあとの段階から");

    let publishing = jobs.read("running_publishing_with_digests");
    let p = publishing.progress.as_ref().unwrap();
    assert_eq!(p.stage, Stage::Publishing);
    assert_eq!(
        (p.old_id.as_deref(), p.incoming_id.as_deref()),
        (Some("fid0003"), Some("fid0010"))
    );
    let d = p.digests.as_ref().unwrap();
    assert_eq!(
        d.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["090000_1.jpg", "manifest.json"]
    );
    assert_eq!(d[0].md5, "0123456789abcdef0123456789abcdef");

    let trashing = jobs.read("running_trashing");
    assert!(trashing.is_unfinished());
    assert_eq!(trashing.progress.as_ref().unwrap().stage, Stage::Trashing);
}

// ---- 公開の記録 ----

struct Published {
    _dir: TempDir,
    store: PublishedStore,
}

impl Published {
    /// 実例（`published/<case>/`）を、状態ディレクトリの `published/` に置く。
    fn new(tag: &str, case: &str) -> Self {
        let dir = TempDir::new(&format!("record-format-published-{tag}-{case}"));
        let store = PublishedStore::in_state_dir(dir.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        for entry in std::fs::read_dir(fixtures().join("published").join(case))
            .unwrap()
            .flatten()
        {
            std::fs::copy(entry.path(), store.dir().join(entry.file_name())).unwrap();
        }
        Self { _dir: dir, store }
    }
}

fn day() -> FolderName {
    FolderName::parse("2030-03-10").unwrap()
}

fn record_of(
    tag: &str,
    case: &str,
    dest: Destination,
) -> geoframe_export::published::PublishedRecord {
    let p = Published::new(tag, case);
    match p.store.load(dest, &day()) {
        Loaded::Record(r) => *r,
        other => panic!("{case}: 読めません: {other:?}"),
    }
}

#[test]
fn every_published_record_example_is_readable_and_the_current_writer_gives_the_same_bytes() {
    for (case, dest, file) in [
        (
            "succeeded_new_folder",
            Destination::Yamareco,
            "yamareco-2030-03-10.json",
        ),
        (
            "succeeded_replace",
            Destination::Yamareco,
            "yamareco-2030-03-10.json",
        ),
        (
            "with_previous",
            Destination::Yamareco,
            "yamareco-2030-03-10.json",
        ),
        (
            "without_files",
            Destination::TravelJournal,
            "travel-journal-2030-03-10.json",
        ),
    ] {
        let rec = record_of("w", case, dest);
        let fresh = TempDir::new(&format!("record-format-published-again-{case}"));
        let store = PublishedStore::in_state_dir(fresh.path());
        store.upsert(rec).unwrap();
        let written = std::fs::read_to_string(store.dir().join(file)).unwrap();
        let example =
            std::fs::read_to_string(fixtures().join("published").join(case).join(file)).unwrap();
        assert_eq!(written, example, "{case}");
    }
}

#[test]
fn published_records_carry_the_folder_the_job_and_the_md5_of_every_file() {
    let new = record_of("t", "succeeded_new_folder", Destination::Yamareco);
    assert_eq!(new.folder_id, "fid0005");
    assert_eq!(new.job_id.as_str(), "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1");
    assert!(new.previous.is_none());
    let files = new.files.as_ref().unwrap();
    assert_eq!(files.len(), 4);
    assert!(files.iter().any(|f| f.path == "manifest.json"));
    assert!(files.iter().all(|f| f.md5.len() == 32));

    let replaced = record_of("t", "succeeded_replace", Destination::Yamareco);
    assert_eq!(replaced.folder_id, "fid0008");
    assert_eq!(replaced.job_id.as_str(), "a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2");

    // 旧フォルダの退避が残っている形
    let pending = record_of("t", "with_previous", Destination::Yamareco);
    assert_eq!(pending.folder_id, "fid0020");
    let prev = pending.previous.as_ref().unwrap();
    assert_eq!(prev.folder_id, "fid0007");
    assert_eq!(prev.files.len(), 2);
    assert_eq!(prev.files[1].path, "manifest.json");

    // MD5 を取れなかった形（replace は断る）
    let none = record_of("t", "without_files", Destination::TravelJournal);
    assert_eq!(none.folder_id, "fid0021");
    assert!(none.files.is_none() && none.previous.is_none());
}

#[test]
fn a_record_named_for_another_destination_is_not_read() {
    // 中身の種類がファイル名と違うものは読めない（別の宛先の記録の取り違えを防ぐ）
    let p = Published::new("x", "without_files");
    assert!(matches!(
        p.store.load(Destination::Yamareco, &day()),
        Loaded::Absent
    ));
    std::fs::copy(
        p.store.dir().join("travel-journal-2030-03-10.json"),
        p.store.dir().join("yamareco-2030-03-10.json"),
    )
    .unwrap();
    assert!(matches!(
        p.store.load(Destination::Yamareco, &day()),
        Loaded::Unreadable
    ));
}
