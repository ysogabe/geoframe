//! 旅行ブログの保存と書き出しの単体試験。Python との突き合わせは `tests/blog_write_api.rs`。ここでは、HTTP から作れない
//! 場面（実行中の 409、停止の合図、置き換えの途中の失敗、作業領域の残骸、パニック）と、部品の振る舞いを確かめる。
//! 合成のデータだけを使う。待ちは合図（チャネル）で同期し、実時間の `sleep` で進み具合を推し量らない。

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc;

use serde_json::{Map, Value, json};

use super::*;
use crate::testutil::{TempDir, capture};
use crate::writer::{OpenMode, Writer};

const TRIP: &str = "sample-trip-a";

/// 書き出しのロックは**プロセスで 1 つ**なので、この節の試験どうしを直列にする（並列に走ると、別の試験の書き出しが
/// 409 になる）。
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Fixture {
    dir: TempDir,
    env: BlogEnv,
    writer: Writer,
    _serial: std::sync::MutexGuard<'static, ()>,
}

fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let pixels = vec![120u8; usize::from(width) * usize::from(height) * 3];
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(&pixels, width, height, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let serial = serial();
        let dir = TempDir::new(tag);
        let root = fs::canonicalize(dir.path()).unwrap();
        let plans = root.join("plans-dir");
        fs::create_dir_all(&plans).unwrap();
        for id in [TRIP, "sample-trip-b"] {
            fs::write(
                plans.join(format!("{id}.json")),
                json!({"trip_id": id, "title": "見本", "segments": []}).to_string(),
            )
            .unwrap();
        }
        fs::create_dir_all(root.join("derivatives")).unwrap();
        for (id, size) in [(1u16, (320, 200)), (2, (200, 320))] {
            fs::write(
                root.join(format!("derivatives/p{id}.jpg")),
                jpeg(size.0, size.1),
            )
            .unwrap();
        }
        let writer = Writer::open(&root.join("catalog.db"), OpenMode::CreateIfMissing).unwrap();
        writer
            .call(|c| {
                c.pragma_update(None, "journal_mode", "WAL")?;
                c.execute_batch(
                    "CREATE TABLE photos (id INTEGER PRIMARY KEY, display TEXT, hidden INTEGER);
                     INSERT INTO photos VALUES (1, 'derivatives/p1.jpg', 0), (2, 'derivatives/p2.jpg', 0),
                                               (3, 'derivatives/p1.jpg', 1);",
                )
            })
            .unwrap();
        let env = BlogEnv::new(Some(&plans), &root);
        Self {
            dir,
            env,
            writer,
            _serial: serial,
        }
    }

    fn root(&self) -> std::path::PathBuf {
        fs::canonicalize(self.dir.path()).unwrap()
    }

    fn export_dir(&self) -> std::path::PathBuf {
        self.root().join("blog_exports")
    }

    fn run(&self, payload: &Value, probe: &dyn Fn(Step) -> Flow) -> Result<Value, ApiError> {
        export_with(
            payload.as_object().unwrap(),
            &self.env,
            &self.root(),
            Some(&self.writer),
            probe,
        )
    }

    fn names(&self, dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|read| {
                read.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

fn manifest(photo_ids: &[&str]) -> Value {
    let images: Vec<Value> = photo_ids
        .iter()
        .map(|id| json!({"photo_id": id, "alt": "a", "caption": "c"}))
        .collect();
    json!({
        "trip_id": TRIP, "status": "draft", "title": "題", "subtitle": "", "summary": "",
        "chapters": [{"id": "d1", "date": "2025-06-06", "title": "章", "location_label": "", "body": "本文", "images": images}],
    })
}

fn request(photo_ids: &[&str]) -> Value {
    json!({"trip_id": TRIP, "manifest": manifest(photo_ids)})
}

fn go(_: Step) -> Flow {
    Flow::Go
}

fn error_of(result: Result<Value, ApiError>) -> ApiError {
    result.expect_err("失敗するはず")
}

#[test]
fn an_export_while_another_is_running_is_409_and_dry_run_and_save_are_not_blocked() {
    let fixture = Fixture::new("blog-write-busy");
    let (staged_tx, staged_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = std::sync::Mutex::new(go_rx);
    std::thread::scope(|scope| {
        let runner = scope.spawn(|| {
            fixture.run(&request(&["1"]), &|step| {
                if step == Step::Staged {
                    // 作業領域まで作った。ここで止まって、別の要求を通す
                    staged_tx.send(()).unwrap();
                    go_rx.lock().unwrap().recv().unwrap();
                }
                Flow::Go
            })
        });
        staged_rx.recv().unwrap();
        match error_of(fixture.run(&request(&["2"]), &go)) {
            ApiError::Busy(message) => assert_eq!(message, BUSY),
            other => panic!("409 のはず: {other:?}"),
        }
        // dry_run はロックを取らない。保存も待たされない
        let mut dry = request(&["2"]);
        dry["dry_run"] = json!(true);
        assert_eq!(fixture.run(&dry, &go).unwrap()["images"], 1);
        let saved = save(
            request(&["2"]).as_object().unwrap(),
            &fixture.env,
            &fixture.root(),
            Some(&fixture.writer),
        );
        assert!(saved.is_ok());
        go_tx.send(()).unwrap();
        assert!(runner.join().unwrap().is_ok());
    });
    // 終わったあとは次の書き出しが通る
    assert!(fixture.run(&request(&["2"]), &go).is_ok());
}

/// 書き出しが写真を読んでいる最中に、回転（取得）が `display` を差し替えて古いファイルを消しても、行とファイルの
/// 組が食い違わない: 書き出しは回転と同じロックの中で「行を引く→ファイルを読む」をするので、回転が済むまで待ち、
/// 済んだあとの新しい行・新しいファイルを読む。
#[test]
fn an_export_waits_for_a_rotation_and_reads_the_row_and_file_it_left_behind() {
    let fixture = Fixture::new("blog-write-rotation");
    let root = fixture.root();
    let guard = imaging::lock_derivatives();
    let (thread, result) = std::thread::scope(|scope| {
        let runner = scope.spawn(|| fixture.run(&request(&["1"]), &go));
        let id = runner.thread().id();
        // 書き出しがロックを待つところまで来た
        assert!(imaging::lock_waiters::wait_until_waiting(
            id,
            || runner.is_finished(),
            std::time::Duration::from_secs(60)
        ));
        // 回転に当たる操作: 新しいファイルを作り、行の `display` を変え、古いファイルを消す
        fs::write(root.join("derivatives/p1.r90.jpg"), jpeg(200, 320)).unwrap();
        fixture
            .writer
            .call(|c| {
                c.execute(
                    "UPDATE photos SET display='derivatives/p1.r90.jpg' WHERE id=1",
                    [],
                )
            })
            .unwrap();
        fs::remove_file(root.join("derivatives/p1.jpg")).unwrap();
        drop(guard);
        (id, runner.join().unwrap())
    });
    let _ = thread;
    result.unwrap();
    let image = fs::read(root.join("blog_exports/sample-trip-a/images/photo-1.jpg")).unwrap();
    let rendered = imaging::render(&image, 0).unwrap();
    // 回したあとのファイル（縦長 200x320）が書き出されている
    assert_eq!(rendered.info.input, (200, 320));
}

#[test]
fn a_stop_signal_removes_the_work_area_and_keeps_the_previous_export() {
    let fixture = Fixture::new("blog-write-stop");
    fixture.run(&request(&["1"]), &go).unwrap();
    fs::write(fixture.export_dir().join(TRIP).join("marker.txt"), "old").unwrap();
    let result = fixture.run(&request(&["1", "2"]), &|step| {
        if step == Step::Image(1) {
            Flow::Stop
        } else {
            Flow::Go
        }
    });
    assert!(matches!(result, Err(ApiError::Internal(_))));
    assert_eq!(fixture.names(&fixture.export_dir()), [TRIP]);
    assert_eq!(
        fs::read_to_string(fixture.export_dir().join(TRIP).join("marker.txt")).unwrap(),
        "old"
    );
}

#[test]
fn a_failure_between_the_two_renames_restores_the_previous_export() {
    let fixture = Fixture::new("blog-write-backed-up");
    fixture.run(&request(&["1"]), &go).unwrap();
    fs::write(fixture.export_dir().join(TRIP).join("marker.txt"), "old").unwrap();
    let result = fixture.run(&request(&["2"]), &|step| {
        if step == Step::BackedUp {
            // 前の書き出しは `.previous` に寄っていて、新しいものはまだ置かれていない
            let export_dir = fixture.export_dir();
            assert!(!export_dir.join(TRIP).exists());
            assert!(export_dir.join(format!(".{TRIP}.previous")).is_dir());
            Flow::Stop
        } else {
            Flow::Go
        }
    });
    assert!(result.is_err());
    assert_eq!(fixture.names(&fixture.export_dir()), [TRIP]);
    assert_eq!(
        fs::read_to_string(fixture.export_dir().join(TRIP).join("marker.txt")).unwrap(),
        "old"
    );
}

#[test]
fn a_panic_in_the_middle_leaves_no_work_area_and_does_not_poison_the_lock() {
    let fixture = Fixture::new("blog-write-panic");
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        fixture.run(&request(&["1"]), &|step| {
            if step == Step::Image(0) {
                panic!("試験用のパニック");
            }
            Flow::Go
        })
    }));
    assert!(outcome.is_err());
    assert_eq!(fixture.names(&fixture.export_dir()), Vec::<String>::new());
    assert!(fixture.run(&request(&["1"]), &go).is_ok());
}

#[test]
fn leftovers_are_swept_before_the_next_export_without_touching_others() {
    let fixture = Fixture::new("blog-write-sweep");
    let export_dir = fixture.export_dir();
    let outside = fixture.root().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "keep").unwrap();
    for stale in [
        ".sample-trip-a-rs0123456789abcdef",
        ".sample-trip-b-rsfedcba9876543210",
    ] {
        fs::create_dir_all(export_dir.join(stale).join("images")).unwrap();
        fs::write(export_dir.join(stale).join("images/x.jpg"), "x").unwrap();
    }
    // Python の作業領域（`mkdtemp`）、リンク、通常のファイル、形の違う名前は触らない
    fs::create_dir_all(export_dir.join(".sample-trip-a-ab12cd34")).unwrap();
    std::os::unix::fs::symlink(
        &outside,
        export_dir.join(".sample-trip-a-rs1111111111111111"),
    )
    .unwrap();
    fs::write(export_dir.join(".sample-trip-a-rs2222222222222222"), "file").unwrap();
    fs::create_dir_all(export_dir.join(".sample-trip-a-rs12")).unwrap();
    fs::create_dir_all(export_dir.join(".Bad_Id-rs3333333333333333")).unwrap();
    // 前の書き出しが残っていて、`.previous` も残っている（置き換えは済んだが消す前に止まった）
    fs::create_dir_all(export_dir.join(TRIP)).unwrap();
    fs::create_dir_all(export_dir.join(format!(".{TRIP}.previous"))).unwrap();

    fixture.run(&request(&["1"]), &go).unwrap();
    assert_eq!(
        fixture.names(&export_dir),
        [
            ".Bad_Id-rs3333333333333333",
            ".sample-trip-a-ab12cd34",
            ".sample-trip-a-rs1111111111111111",
            ".sample-trip-a-rs12",
            ".sample-trip-a-rs2222222222222222",
            TRIP
        ]
    );
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn a_previous_export_is_restored_when_the_destination_is_missing_even_if_the_export_fails() {
    let fixture = Fixture::new("blog-write-restore");
    let export_dir = fixture.export_dir();
    let backup = export_dir.join(format!(".{TRIP}.previous"));
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("index.html"), "前の書き出し").unwrap();
    // 隠れた写真を含むので、この書き出しは 400 で失敗する
    let result = fixture.run(&request(&["1", "3"]), &go);
    assert!(matches!(result, Err(ApiError::BadRequest(_))));
    assert_eq!(fixture.names(&export_dir), [TRIP]);
    assert_eq!(
        fs::read_to_string(export_dir.join(TRIP).join("index.html")).unwrap(),
        "前の書き出し"
    );
}

#[test]
fn the_startup_sweep_restores_and_cleans_and_reports_only_counts() {
    let fixture = Fixture::new("blog-write-startup");
    let export_dir = fixture.export_dir();
    let drafts = fixture.root().join("travel_blog/drafts");
    fs::create_dir_all(&drafts).unwrap();
    fs::create_dir_all(export_dir.join(".sample-trip-a-rs0123456789abcdef")).unwrap();
    fs::create_dir_all(export_dir.join(format!(".{TRIP}.previous"))).unwrap();
    fs::write(
        export_dir.join(format!(".{TRIP}.previous/index.html")),
        "old",
    )
    .unwrap();
    fs::create_dir_all(export_dir.join("sample-trip-b")).unwrap();
    fs::create_dir_all(export_dir.join(".sample-trip-b.previous")).unwrap();
    fs::write(drafts.join("sample-trip-a.json"), "{}").unwrap();
    fs::write(
        drafts.join("sample-trip-a.json.123.0123456789abcdef.part"),
        "半端",
    )
    .unwrap();
    fs::write(drafts.join("notes.part"), "keep").unwrap();
    fs::write(drafts.join("sample-trip-a.json.tmp"), "keep").unwrap();
    let logs = capture::logs_of(|| sweep_leftovers(&fixture.root()));
    assert!(logs.contains("作業領域1件"), "{logs}");
    assert!(logs.contains("書き出し2件"), "{logs}");
    assert!(logs.contains("下書きの書きかけ1件"), "{logs}");
    assert!(!logs.contains("sample-trip"), "名前を出さない: {logs}");
    assert_eq!(
        fixture.names(&export_dir),
        ["sample-trip-a", "sample-trip-b"]
    );
    assert_eq!(
        fs::read_to_string(export_dir.join(TRIP).join("index.html")).unwrap(),
        "old"
    );
    assert_eq!(
        fixture.names(&drafts),
        ["notes.part", "sample-trip-a.json", "sample-trip-a.json.tmp"]
    );
}

/// 起動時の掃除は、掃除する場所の親（`travel_blog/`・`drafts/`・`blog_exports/`）がリンクなら何もしない:
/// `read_dir` はリンクを辿るので、確かめないと、外のディレクトリの通常ファイルや作業領域の形のものを消してしまう。
/// 外のものは何も変わらず、ログは定型文で名前を含まない。
#[test]
fn the_startup_sweep_does_not_follow_a_linked_parent() {
    let fixture = Fixture::new("blog-write-startup-links");
    let root = fixture.root();
    let part = "sample-trip-a.json.123.0123456789abcdef.part";
    // 外のディレクトリ: 下書きの書きかけの形・書き出しの作業領域と `.previous` の形のもの
    let outside = root.join("outside");
    fs::create_dir_all(outside.join(".sample-trip-a-rs0123456789abcdef")).unwrap();
    fs::create_dir_all(outside.join(format!(".{TRIP}.previous"))).unwrap();
    fs::write(outside.join(part), "外").unwrap();
    let before = fixture.names(&outside);
    let skipped = |logs: &str| {
        assert!(logs.contains("掃除を飛ばしました"), "{logs}");
        assert!(!logs.contains("sample-trip"), "名前を出さない: {logs}");
        assert!(!logs.contains("outside"), "パスを出さない: {logs}");
    };

    // `travel_blog` が外へのリンク（`drafts` は外にある）
    fs::create_dir_all(outside.join("drafts")).unwrap();
    fs::write(outside.join("drafts").join(part), "外").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("travel_blog")).unwrap();
    let logs = capture::logs_of(|| sweep_leftovers(&root));
    skipped(&logs);
    assert!(outside.join("drafts").join(part).is_file());
    fs::remove_file(root.join("travel_blog")).unwrap();

    // `travel_blog` は通常のディレクトリで、`drafts` が外へのリンク
    fs::create_dir_all(root.join("travel_blog")).unwrap();
    fs::remove_dir_all(outside.join("drafts")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("travel_blog/drafts")).unwrap();
    let logs = capture::logs_of(|| sweep_leftovers(&root));
    skipped(&logs);
    assert!(outside.join(part).is_file());
    fs::remove_file(root.join("travel_blog/drafts")).unwrap();

    // `blog_exports` が外へのリンク
    std::os::unix::fs::symlink(&outside, root.join("blog_exports")).unwrap();
    let logs = capture::logs_of(|| sweep_leftovers(&root));
    skipped(&logs);
    fs::remove_file(root.join("blog_exports")).unwrap();

    assert_eq!(fixture.names(&outside), before);
    assert!(outside.join(".sample-trip-a-rs0123456789abcdef").is_dir());
    assert!(outside.join(format!(".{TRIP}.previous")).is_dir());
    // 通常のディレクトリなら、同じ場所の同じ形のものを掃除する（試験が効いていることの確認）
    let drafts = root.join("travel_blog/drafts");
    fs::create_dir_all(&drafts).unwrap();
    fs::write(drafts.join(part), "半端").unwrap();
    sweep_leftovers(&root);
    assert!(!drafts.join(part).exists());
}

#[test]
fn nothing_is_written_without_a_writer_but_dry_run_answers() {
    let fixture = Fixture::new("blog-write-no-writer");
    let payload = request(&["1"]);
    let object = payload.as_object().unwrap();
    assert!(matches!(
        save(object, &fixture.env, &fixture.root(), None),
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(
        export_with(object, &fixture.env, &fixture.root(), None, &go),
        Err(ApiError::Unavailable)
    ));
    let mut dry = object.clone();
    dry.insert("dry_run".into(), json!(true));
    assert_eq!(
        export(&dry, &fixture.env, &fixture.root(), None).unwrap()["images"],
        1
    );
    // 入力の検査（400・404）は 503 より先
    let mut bad = object.clone();
    bad.insert("trip_id".into(), json!("no-such-trip"));
    assert!(matches!(
        save(&bad, &fixture.env, &fixture.root(), None),
        Err(ApiError::NotFound(_))
    ));
    assert!(!fixture.root().join("travel_blog").exists());
    assert!(!fixture.root().join("blog_exports").exists());
}

#[test]
fn links_are_never_followed_when_saving_or_exporting() {
    let fixture = Fixture::new("blog-write-links");
    let root = fixture.root();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    // 保存先の途中がリンク
    std::os::unix::fs::symlink(&outside, root.join("travel_blog")).unwrap();
    let payload = request(&[]);
    let result = save(
        payload.as_object().unwrap(),
        &fixture.env,
        &root,
        Some(&fixture.writer),
    );
    assert!(matches!(result, Err(ApiError::Internal(_))));
    assert_eq!(fixture.names(&outside), Vec::<String>::new());
    fs::remove_file(root.join("travel_blog")).unwrap();
    // 下書きのファイルそのものがリンク: リンクを置き換え、先は書き換えない
    fs::create_dir_all(root.join("travel_blog/drafts")).unwrap();
    fs::write(outside.join("target.json"), "keep").unwrap();
    std::os::unix::fs::symlink(
        outside.join("target.json"),
        root.join(format!("travel_blog/drafts/{TRIP}.json")),
    )
    .unwrap();
    save(
        payload.as_object().unwrap(),
        &fixture.env,
        &root,
        Some(&fixture.writer),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(outside.join("target.json")).unwrap(),
        "keep"
    );
    let saved = root.join(format!("travel_blog/drafts/{TRIP}.json"));
    assert!(fs::symlink_metadata(&saved).unwrap().is_file());
    // 書き出し先がリンク
    std::os::unix::fs::symlink(&outside, root.join("blog_exports")).unwrap();
    assert!(matches!(
        fixture.run(&request(&["1"]), &go),
        Err(ApiError::Internal(_))
    ));
    assert_eq!(fixture.names(&outside), ["target.json"]);
}

#[test]
fn a_failed_save_keeps_the_existing_draft_and_leaves_no_part_file() {
    let fixture = Fixture::new("blog-write-save-fail");
    let root = fixture.root();
    let payload = request(&[]);
    let object = payload.as_object().unwrap();
    save(object, &fixture.env, &root, Some(&fixture.writer)).unwrap();
    let path = root.join(format!("travel_blog/drafts/{TRIP}.json"));
    let before = fs::read(&path).unwrap();
    // 置き換え先が空でないディレクトリだと `rename` が失敗する経路は、既存のファイルでは作れないので、
    // 書けないディレクトリで失敗させる（root では効かないので飛ばす）
    use std::os::unix::fs::PermissionsExt;
    let drafts = root.join("travel_blog/drafts");
    fs::set_permissions(&drafts, fs::Permissions::from_mode(0o500)).unwrap();
    let writable_anyway = fs::File::create(drafts.join("probe")).is_ok();
    let mut changed = object.clone();
    changed["manifest"]["title"] = json!("別の題");
    let result = save(&changed, &fixture.env, &root, Some(&fixture.writer));
    fs::set_permissions(&drafts, fs::Permissions::from_mode(0o700)).unwrap();
    if writable_anyway {
        let _ = fs::remove_file(drafts.join("probe"));
        return;
    }
    assert!(matches!(result, Err(ApiError::Internal(_))));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fixture.names(&drafts), [format!("{TRIP}.json")]);
}

#[test]
fn errors_and_logs_never_carry_names_ids_or_paths() {
    let fixture = Fixture::new("blog-write-no-leak");
    fs::write(fixture.root().join("derivatives/p1.jpg"), b"not an image").unwrap();
    let logs = capture::logs_of(|| {
        let ApiError::Internal(detail) = error_of(fixture.run(&request(&["1"]), &go)) else {
            panic!("500 のはず")
        };
        for secret in [
            TRIP,
            "p1.jpg",
            "derivatives",
            "blog_exports",
            "写真#1",
            "本文",
        ] {
            assert!(
                !detail.contains(secret),
                "{secret} が詳細に出ている: {detail}"
            );
        }
    });
    assert_eq!(logs, "");
    // 400 の文面は Python と同じで、写真の id だけを含む
    let ApiError::BadRequest(message) = error_of(fixture.run(&request(&["3"]), &go)) else {
        panic!("400 のはず")
    };
    assert_eq!(message, "写真#3の表示用画像がありません");
    assert_eq!(fixture.names(&fixture.export_dir()), Vec::<String>::new());
}

#[test]
fn photo_row_id_reads_decimal_digits_like_int() {
    assert_eq!(photo_row_id("12"), Some(12));
    assert_eq!(photo_row_id("012"), Some(12));
    assert_eq!(photo_row_id("٣"), Some(3));
    assert_eq!(photo_row_id("１２"), Some(12));
    assert_eq!(photo_row_id("9223372036854775807"), Some(i64::MAX));
    assert_eq!(photo_row_id("9223372036854775808"), None);
    assert_eq!(photo_row_id("²"), None);
    assert_eq!(photo_row_id(""), Some(0));
}

#[test]
fn staging_names_are_recognised_exactly() {
    assert!(is_staging_name(".sample-trip-a-rs0123456789abcdef"));
    assert!(is_staging_name(".a-rs0123456789abcdef"));
    assert!(!is_staging_name(".sample-trip-a-rs0123456789abcde"));
    assert!(!is_staging_name(".sample-trip-a-rs0123456789ABCDEF"));
    assert!(!is_staging_name("sample-trip-a-rs0123456789abcdef"));
    assert!(!is_staging_name(".-rs0123456789abcdef"));
    assert!(!is_staging_name(".sample-trip-a-ab12cd34"));
    assert!(!is_staging_name(
        ".日本語日本語日本語日本語日本語日本語日本語"
    ));
    assert_eq!(
        previous_trip_id(".sample-trip-a.previous"),
        Some("sample-trip-a")
    );
    assert_eq!(previous_trip_id(".Bad.previous"), None);
    assert_eq!(previous_trip_id("sample-trip-a.previous"), None);
}

#[test]
fn fill_does_not_rescan_inserted_text() {
    let out = fill(
        "<a>@@TITLE@@</a><b>@@BODY@@</b>@@NONE@@",
        &[("TITLE", "@@BODY@@"), ("BODY", "x@@TITLE@@")],
    );
    assert_eq!(out, "<a>@@BODY@@</a><b>x@@TITLE@@</b>@@NONE@@");
    // 閉じていない印は、そのまま残す
    assert_eq!(fill("a@@b", &[]), "a@@b");
}

#[test]
fn paragraphs_and_markdown_follow_python_whitespace_rules() {
    assert_eq!(
        paragraphs("一\n\x1c\n \t\n二\u{85}\n\u{3000}\n<&>'\""),
        "<p>一</p><p>二\u{85}</p><p>&lt;&amp;&gt;&#x27;&quot;</p>"
    );
    let manifest: Map<String, Value> = serde_json::from_value(json!({
        "title": "題", "subtitle": "", "summary": "",
        "chapters": [{"date": "d", "title": "t", "body": "末尾\n \u{3000}\x1c\n", "images": []}],
    }))
    .unwrap();
    assert_eq!(render_markdown(&manifest), "# 題\n\n## d t\n\n末尾\n");
}
