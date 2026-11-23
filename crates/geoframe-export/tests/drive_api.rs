//! Drive API の操作の型（`geoframe_export::drive::api::DriveApi`）の試験。相手は、試験のプロセスの中の偽の Drive の HTTP サーバー
//! （`tests/common/fake_drive.rs`。本物の Google には一切つながない）。資格情報・トークン・ログは `drive_auth.rs`・`drive_api_log.rs`。
//!
//! 見るもの: 一覧（全ページ・名前で絞らない・ゴミ箱を除く・上限）、作成・送信の再送で重複したファイルができないこと、
//! 流量制限・429・401・通信の失敗の再試行（回数の上限・`Retry-After`）、resumable の続き・始め直し・別ホストの `Location`、
//! リダイレクトをたどらないこと、期限・キャンセル、完全な削除と中身の更新を呼ばないこと。

mod common;

use std::time::{Duration, Instant};

use common::drive::model::{Kind, ROOT};
use common::drive::*;
use common::fake_drive as fk;
use geoframe_export::cancel::Cancel;
use geoframe_export::drive::api::{DriveApi, Limit, MAX_ENTRIES};
use geoframe_export::drive::http::{ApiError, Deadline, Retry};
use serde_json::{Value, json};

fn fast() -> Retry {
    Retry {
        max_retries: 3,
        base: Duration::from_millis(5),
        cap: Duration::from_millis(200),
    }
}

/// 停止の期限: SIGTERM のあと、飛んでいる要求 1 つの終わりまで待つ。その最長（JSON の上限 + チャンク・multipart の上限。トークンの
/// 取り直しを挟んでも超えない）が、unit の `TimeoutStopSec` の中に収まる（収まらないと、systemd が SIGKILL する前に全スレッドが止まれない）。
#[test]
fn the_per_request_caps_fit_in_the_units_stop_timeout() {
    use geoframe_export::drive::api::{CHUNK_REQUEST_CAP, JSON_REQUEST_CAP};
    let unit = include_str!("../../../deploy/geoframe-export.service");
    let secs: u64 = unit
        .lines()
        .find_map(|l| l.strip_prefix("TimeoutStopSec="))
        .and_then(|v| v.strip_suffix('s'))
        .and_then(|v| v.parse().ok())
        .expect("TimeoutStopSec=<秒>s");
    assert!(CHUNK_REQUEST_CAP <= Duration::from_secs(120));
    assert!(
        CHUNK_REQUEST_CAP + JSON_REQUEST_CAP < Duration::from_secs(secs),
        "{CHUNK_REQUEST_CAP:?} + {JSON_REQUEST_CAP:?} が TimeoutStopSec={secs}s に収まらない"
    );
    // serve が接続を待つ上限（75 秒）も、同じ中に収まる（並行して進む）
    assert!(geoframe_export::serve::SHUTDOWN_WAIT < Duration::from_secs(secs));
}

struct Fx {
    env: Env,
    fake: fk::FakeDrive,
    api: DriveApi,
    yamareco: String,
    work: String,
}

impl Fx {
    fn new(name: &str) -> Self {
        let env = Env::new(name);
        let fake = fk::FakeDrive::start(&env);
        let cred = fk::write_credentials(&env.root.path().join("cred"));
        let api = DriveApi::new(
            geoframe_export::drive::auth::Credentials::load(&cred).expect("credentials"),
            fake.endpoints(),
        )
        .with_retry(fast());
        let d = env.drive();
        let yamareco = d.resolve(&["yamareco"]).unwrap();
        let work = d.resolve(&["geoframe-export-work"]).unwrap();
        Self {
            env,
            fake,
            api,
            yamareco,
            work,
        }
    }

    fn dl(&self) -> Deadline {
        self.api.deadline(Limit::Query)
    }

    fn up(&self) -> Deadline {
        self.api.deadline(Limit::Upload)
    }

    /// トークンの入口を除いた、操作名の並び。
    fn ops(&self) -> Vec<String> {
        self.env
            .ops()
            .into_iter()
            .filter(|o| o != "token")
            .collect()
    }

    /// `phase` の `upload` の要求の数。
    fn uploads(&self, phase: &str) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r["op"] == "upload" && r["phase"] == phase)
            .count()
    }

    /// Drive の `parent` の直下の、ゴミ箱でない `name` の数。
    fn count_named(&self, parent: &str, name: &str) -> usize {
        let d = self.env.drive();
        d.children(parent)
            .iter()
            .filter(|n| n.name == name && !d.is_trashed(&n.id))
            .count()
    }

    /// 手元のファイル（中身を `body` にしたもの）。
    fn local(&self, name: &str, body: &[u8]) -> (std::path::PathBuf, String) {
        let path = self.env.root.path().join(name);
        std::fs::write(&path, body).unwrap();
        (path, model_md5(body))
    }
}

/// 条件が真になるまで待つ（10 秒で諦める）。実時間の決め打ちでなく、進み具合で試験を進める。
fn wait_until(mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "条件が満たされません"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// これまでに受けた、チャンクの送信の要求の数。
fn chunk_requests(fake: &fk::FakeDrive) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r["op"] == "upload" && r["phase"] == "chunk")
        .count()
}

fn model_md5(b: &[u8]) -> String {
    common::drive::model::md5_hex(b)
}

fn file_in(fx: &Fx, parent: &str, name: &str, content: &str) -> String {
    let mut d = fx.env.drive();
    let id = d.new_node(
        parent,
        name,
        Kind::File,
        content.len() as u64,
        Some(content.to_owned()),
        None,
    );
    fx.env.save(&d);
    id
}

fn dir_in(fx: &Fx, parent: &str, name: &str) -> String {
    let mut d = fx.env.drive();
    let id = d.new_node(parent, name, Kind::Dir, 0, None, None);
    fx.env.save(&d);
    id
}

// ---- 一覧 ----

#[test]
fn list_children_returns_every_page_and_never_filters_by_name() {
    let fx = Fx::new("api-list");
    let api = fx.api.clone().with_sizes(2, 8 << 20, 8 << 20);
    for i in 0..5 {
        file_in(&fx, &fx.yamareco, &format!("p{i}.jpg"), "abc");
    }
    let sub = dir_in(&fx, &fx.yamareco, "sub");
    let gdoc = {
        let mut d = fx.env.drive();
        let id = d.new_node(&fx.yamareco, "doc", Kind::Gdoc, 0, None, None);
        let sc = d.new_node(
            &fx.yamareco,
            "link",
            Kind::Shortcut,
            0,
            None,
            Some(sub.clone()),
        );
        fx.env.save(&d);
        (id, sc)
    };
    let entries = api.list_children(&fx.yamareco, fx.dl()).unwrap();
    assert_eq!(entries.len(), 8);
    assert_eq!(
        entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        [
            "p0.jpg", "p1.jpg", "p2.jpg", "p3.jpg", "p4.jpg", "sub", "doc", "link"
        ]
    );
    let p0 = &entries[0];
    assert_eq!(
        (p0.size, p0.is_dir, p0.mime.as_str()),
        (3, false, "image/jpeg")
    );
    assert_eq!(p0.md5.as_deref(), Some(model_md5(b"abc").as_str()));
    assert_eq!(p0.path, "p0.jpg");
    assert_eq!(p0.shortcut_id, None);
    let dir = entries.iter().find(|e| e.id == sub).unwrap();
    assert!(dir.is_dir && dir.size == -1 && dir.md5.is_none());
    // サイズの無い Google 文書は -1・MD5 無し
    let doc = entries.iter().find(|e| e.id == gdoc.0).unwrap();
    assert!(!doc.is_dir && doc.size == -1 && doc.md5.is_none());
    // ショートカットは解決されず 1 件。ショートカットとして見分けられる
    let link = entries.iter().find(|e| e.id == gdoc.1).unwrap();
    assert!(link.is_shortcut() && !link.is_dir);

    // 4 ページ（2 件ずつ）。q は親の ID だけで、名前で絞らない。Authorization が付いている
    let lists: Vec<Value> = fx
        .fake
        .requests()
        .into_iter()
        .filter(|r| r["op"] == "list")
        .collect();
    assert_eq!(lists.len(), 4);
    for (i, l) in lists.iter().enumerate() {
        assert_eq!(
            l["q"],
            format!("'{}' in parents and trashed=false", fx.yamareco)
        );
        assert_eq!(l["has_page_token"], i > 0);
        assert_eq!(l["page_size"], "2");
        assert_eq!(l["authorization"], "ok");
    }
}

#[test]
fn trashed_files_and_folders_are_not_listed() {
    let fx = Fx::new("api-trashed");
    let keep = file_in(&fx, &fx.yamareco, "keep.jpg", "k");
    let gone = file_in(&fx, &fx.yamareco, "gone.jpg", "g");
    let folder = dir_in(&fx, &fx.yamareco, "old");
    let inner = file_in(&fx, &folder, "inner.jpg", "i");
    let mut d = fx.env.drive();
    d.node_mut_pub(&gone).unwrap().trashed = true;
    d.node_mut_pub(&folder).unwrap().trashed = true;
    fx.env.save(&d);
    let names: Vec<String> = fx
        .api
        .list_children(&fx.yamareco, fx.dl())
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(names, std::slice::from_ref(&keep));
    // ゴミ箱の中のフォルダの中身は、ゴミ箱の中のものとして見える（`get` の trashed は真）
    assert!(fx.api.get(&inner, fx.dl()).unwrap().trashed);
    assert!(fx.api.get(&folder, fx.dl()).unwrap().trashed);
    assert!(!fx.api.get(&keep, fx.dl()).unwrap().trashed);
    assert_eq!(fx.api.list_children(&folder, fx.dl()).unwrap().len(), 0);
}

#[test]
fn a_repeating_page_token_fails_instead_of_looping() {
    let fx = Fx::new("api-stuck");
    let api = fx.api.clone().with_sizes(1, 8 << 20, 8 << 20);
    for i in 0..3 {
        file_in(&fx, &fx.yamareco, &format!("p{i}.jpg"), "a");
    }
    fx.env.add_rule("list", 0, 100, fk::repeat_page_token());
    assert_eq!(
        api.list_children(&fx.yamareco, fx.dl()),
        Err(ApiError::TooManyPages)
    );
    // 同じトークンが 2 度目に返ったところで止まる
    assert_eq!(fx.env.op_count("list"), 2);
}

#[test]
fn more_than_the_entry_limit_fails() {
    // 本番の上限は rclone 版と同じ 20000 件（件数が多いと偽のサーバーが遅いので、試験では上限を下げて確かめる）
    assert_eq!(MAX_ENTRIES, 20_000);
    let fx = Fx::new("api-limit");
    let api = fx
        .api
        .clone()
        .with_sizes(10, 8 << 20, 8 << 20)
        .with_max_entries(25);
    let mut d = fx.env.drive();
    for i in 0..25 {
        d.new_node(&fx.yamareco, &format!("f{i}"), Kind::File, 1, None, None);
    }
    fx.env.save(&d);
    // ちょうど上限は通る
    assert_eq!(api.list_children(&fx.yamareco, fx.dl()).unwrap().len(), 25);
    let mut d = fx.env.drive();
    d.new_node(&fx.yamareco, "one-more", Kind::File, 1, None, None);
    fx.env.save(&d);
    assert_eq!(
        api.list_children(&fx.yamareco, fx.dl()),
        Err(ApiError::TooManyPages)
    );
    assert_eq!(
        api.list_tree(&fx.yamareco, fx.dl()),
        Err(ApiError::TooManyPages)
    );
}

#[test]
fn list_tree_builds_relative_paths_breadth_first_and_skips_the_trash() {
    let fx = Fx::new("api-tree");
    let a = dir_in(&fx, &fx.yamareco, "a");
    let b = dir_in(&fx, &a, "b");
    file_in(&fx, &a, "x.jpg", "x");
    file_in(&fx, &b, "y.jpg", "yy");
    file_in(&fx, &fx.yamareco, "top.json", "{}");
    let t = dir_in(&fx, &fx.yamareco, "trash");
    let mut d = fx.env.drive();
    d.node_mut_pub(&t).unwrap().trashed = true;
    fx.env.save(&d);
    let tree = fx.api.list_tree(&fx.yamareco, fx.dl()).unwrap();
    let paths: Vec<&str> = tree.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["a", "top.json", "a/b", "a/x.jpg", "a/b/y.jpg"]);
    let y = tree.iter().find(|e| e.path == "a/b/y.jpg").unwrap();
    assert_eq!((y.name.as_str(), y.size), ("y.jpg", 2));
}

#[test]
fn list_tree_rejects_slashes_in_names_and_too_deep_trees() {
    let fx = Fx::new("api-tree-bad");
    file_in(&fx, &fx.yamareco, "a/b.jpg", "x");
    assert_eq!(
        fx.api.list_tree(&fx.yamareco, fx.dl()),
        Err(ApiError::BadResponse)
    );

    let fx = Fx::new("api-tree-deep");
    let mut parent = fx.yamareco.clone();
    let mut d = fx.env.drive();
    for i in 0..40 {
        parent = d.new_node(&parent, &format!("d{i}"), Kind::Dir, 0, None, None);
    }
    fx.env.save(&d);
    assert_eq!(
        fx.api.list_tree(&fx.yamareco, fx.dl()),
        Err(ApiError::TooManyPages)
    );
}

#[test]
fn get_and_read_by_id() {
    let fx = Fx::new("api-get");
    let id = file_in(&fx, &fx.yamareco, "manifest.json", "{\"a\":1}");
    let m = fx.api.get(&id, fx.dl()).unwrap();
    assert_eq!(m.entry.name, "manifest.json");
    assert_eq!(m.entry.size, 7);
    assert_eq!(m.parents, std::slice::from_ref(&fx.yamareco));
    assert!(!m.trashed);
    assert_eq!(fx.api.read(&id, 100, fx.dl()).unwrap(), b"{\"a\":1}");
    // 上限ちょうどは通り、1 バイト超えたら失敗
    assert_eq!(fx.api.read(&id, 7, fx.dl()).unwrap().len(), 7);
    assert_eq!(fx.api.read(&id, 6, fx.dl()), Err(ApiError::TooLarge));
    assert_eq!(fx.api.get("nope", fx.dl()), Err(ApiError::NotFound));
    // フォルダは読めない（403）。形の違う ID は、送らずに断る
    assert_eq!(
        fx.api.read(&fx.yamareco, 10, fx.dl()),
        Err(ApiError::Status(403))
    );
    let before = fx.env.ops().len();
    for bad in ["", "a b", "a'b", "a\\b", "../x"] {
        assert!(
            matches!(fx.api.get(bad, fx.dl()), Err(ApiError::Rejected(_))),
            "{bad:?}"
        );
        assert!(matches!(
            fx.api.list_children(bad, fx.dl()),
            Err(ApiError::Rejected(_))
        ));
    }
    assert_eq!(fx.env.ops().len(), before);
}

#[test]
fn generate_ids_returns_distinct_valid_ids() {
    let fx = Fx::new("api-ids");
    let ids = fx.api.generate_ids(5, fx.dl()).unwrap();
    assert_eq!(ids.len(), 5);
    let set: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(set.len(), 5);
    assert!(fx.api.generate_ids(0, fx.dl()).is_err());
    assert!(fx.api.generate_ids(1001, fx.dl()).is_err());
}

// ---- フォルダの作成 ----

#[test]
fn create_folder_takes_an_id_first_and_posts_with_it() {
    let fx = Fx::new("api-mkdir");
    let id = fx.api.create_folder(&fx.work, "incoming", fx.dl()).unwrap();
    assert_eq!(fx.ops(), ["generate_ids", "create_folder"]);
    assert_eq!(fx.env.mutating_ops(), ["create_folder"]);
    let d = fx.env.drive();
    let n = d.node(&id).unwrap();
    assert_eq!(
        (n.name.as_str(), n.parent.as_str(), n.kind),
        ("incoming", fx.work.as_str(), Kind::Dir)
    );
    for bad in ["", "a/b", "a\nb"] {
        assert!(matches!(
            fx.api.create_folder(&fx.work, bad, fx.dl()),
            Err(ApiError::Rejected(_))
        ));
    }
}

#[test]
fn a_lost_create_is_resent_with_the_same_id_and_never_duplicates() {
    for (label, effect) in [
        ("drop", fk::do_then_drop()),
        ("fail", do_then_fail()),
        ("cut", fk::cut_response()),
    ] {
        let fx = Fx::new(&format!("api-mkdir-lost-{label}"));
        fx.env.add_rule("create_folder", 0, 1, effect);
        let id = fx.api.create_folder(&fx.work, "incoming", fx.dl()).unwrap();
        assert_eq!(fx.count_named(&fx.work, "incoming"), 1, "{label}");
        assert!(fx.env.drive().node(&id).is_some());
        // ID を取り直さない（generate_ids は 1 回）。再送は同じ ID で、409 のあとの照合は get
        assert_eq!(
            fx.ops(),
            ["generate_ids", "create_folder", "create_folder", "get"],
            "{label}"
        );
        let posts: Vec<_> = fx
            .fake
            .requests()
            .into_iter()
            .filter(|r| r["op"] == "create_folder")
            .map(|r| r["new_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(posts, [id.clone(), id], "{label}");
    }
}

#[test]
fn a_create_that_failed_before_doing_anything_is_resent_and_made_once() {
    let fx = Fx::new("api-mkdir-500");
    fx.env.add_rule("create_folder", 0, 2, fail(1));
    let id = fx.api.create_folder(&fx.work, "incoming", fx.dl()).unwrap();
    assert_eq!(fx.count_named(&fx.work, "incoming"), 1);
    assert_eq!(fx.env.op_count("create_folder"), 3);
    assert_eq!(fx.env.op_count("generate_ids"), 1);
    assert!(fx.env.drive().node(&id).is_some());
}

/// 409 のあとの照合で、別のものだった（別の親・名前・中身）なら成功にしない。
#[test]
fn a_conflict_on_the_id_is_only_success_when_the_existing_one_matches() {
    let fx = Fx::new("api-mkdir-mismatch");
    // 作ってから応答を失い、再送までの間に、作られたフォルダを別の名前に変える
    let api = fx.api.clone().with_retry(Retry {
        max_retries: 3,
        base: Duration::from_millis(400),
        cap: Duration::from_millis(400),
    });
    fx.env.add_rule("create_folder", 0, 1, fk::do_then_drop());
    let env_dir = fx.env.fake_dir.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = stop.clone();
    let t = std::thread::spawn(move || {
        while !s2.load(std::sync::atomic::Ordering::SeqCst) {
            let mut d = model::Drive::load(&env_dir);
            if let Some(n) = d.nodes.iter_mut().find(|n| n.name == "incoming") {
                n.name = "renamed".into();
                d.save(&env_dir);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let r = api.create_folder(&fx.work, "incoming", fx.dl());
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    t.join().unwrap();
    assert_eq!(r, Err(ApiError::Mismatch));
}

// ---- 送信（multipart） ----

#[test]
fn upload_sends_a_small_file_with_a_preissued_id() {
    let fx = Fx::new("api-up-small");
    let body = b"hello photo bytes".to_vec();
    let (path, md5) = fx.local("a.jpg", &body);
    let id = fx
        .api
        .upload(&fx.work, "a.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    assert_eq!(fx.ops(), ["generate_ids", "upload"]);
    let d = fx.env.drive();
    let n = d.node(&id).unwrap();
    assert_eq!(
        (n.name.as_str(), n.parent.as_str(), n.size),
        ("a.jpg", fx.work.as_str(), 17)
    );
    assert_eq!(n.effective_md5(), md5);
    assert_eq!(fx.fake.blob(&id).unwrap(), body);
    let up = &fx.fake.requests()[2];
    assert_eq!(
        (
            up["op"].as_str(),
            up["phase"].as_str(),
            up["authorization"].as_str()
        ),
        (Some("upload"), Some("multipart"), Some("ok"))
    );
}

#[test]
fn upload_refuses_before_any_request_when_the_local_file_differs() {
    let fx = Fx::new("api-up-local");
    let (path, md5) = fx.local("a.jpg", b"abc");
    let wrong_md5 = model_md5(b"zzz");
    assert!(matches!(
        fx.api
            .upload(&fx.work, "a.jpg", &path, &wrong_md5, 3, fx.up()),
        Err(ApiError::Rejected(_))
    ));
    assert!(matches!(
        fx.api.upload(&fx.work, "a.jpg", &path, &md5, 4, fx.up()),
        Err(ApiError::Rejected(_))
    ));
    assert!(matches!(
        fx.api.upload(&fx.work, "a.jpg", &path, "xyz", 3, fx.up()),
        Err(ApiError::Rejected(_))
    ));
    assert!(matches!(
        fx.api.upload(&fx.work, "a/b.jpg", &path, &md5, 3, fx.up()),
        Err(ApiError::Rejected(_))
    ));
    let missing = fx.env.root.path().join("missing.jpg");
    assert!(matches!(
        fx.api.upload(&fx.work, "a.jpg", &missing, &md5, 3, fx.up()),
        Err(ApiError::Io(_))
    ));
    assert_eq!(fx.env.ops(), Vec::<String>::new());
}

#[test]
fn a_lost_upload_response_is_resent_with_the_same_id_and_never_duplicates() {
    for (label, effect) in [("drop", fk::do_then_drop()), ("fail", do_then_fail())] {
        let fx = Fx::new(&format!("api-up-lost-{label}"));
        let body = b"photo".to_vec();
        let (path, md5) = fx.local("a.jpg", &body);
        fx.env.add_rule("upload", 0, 1, effect);
        let id = fx
            .api
            .upload(&fx.work, "a.jpg", &path, &md5, 5, fx.up())
            .unwrap();
        assert_eq!(fx.count_named(&fx.work, "a.jpg"), 1, "{label}");
        assert_eq!(
            fx.ops(),
            ["generate_ids", "upload", "upload", "get"],
            "{label}"
        );
        let ids: Vec<_> = fx
            .fake
            .requests()
            .into_iter()
            .filter(|r| r["op"] == "upload")
            .map(|r| r["new_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, [id.clone(), id], "{label}");
    }
}

#[test]
fn a_conflict_with_a_different_md5_is_not_success() {
    let fx = Fx::new("api-up-mismatch");
    let api = fx.api.clone().with_retry(Retry {
        max_retries: 3,
        base: Duration::from_millis(400),
        cap: Duration::from_millis(400),
    });
    let (path, md5) = fx.local("a.jpg", b"photo");
    fx.env.add_rule("upload", 0, 1, fk::do_then_drop());
    let env_dir = fx.env.fake_dir.clone();
    let t = std::thread::spawn(move || {
        for _ in 0..300 {
            let mut d = model::Drive::load(&env_dir);
            if let Some(id) = d
                .nodes
                .iter()
                .find(|n| n.name == "a.jpg")
                .map(|n| n.id.clone())
            {
                d.set_md5(&id, &"0".repeat(32));
                d.save(&env_dir);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let r = api.upload(&fx.work, "a.jpg", &path, &md5, 5, fx.up());
    t.join().unwrap();
    assert_eq!(r, Err(ApiError::Mismatch));
}

// ---- 送信（resumable） ----

fn big(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

/// 256KiB のチャンク・100KiB を超えたら resumable、の API。
fn small_chunks(fx: &Fx) -> DriveApi {
    fx.api.clone().with_sizes(1000, 100 * 1024, 256 * 1024)
}

#[test]
fn a_large_file_is_sent_in_chunks_with_a_resumable_session() {
    let fx = Fx::new("api-resumable");
    let api = small_chunks(&fx);
    let body = big(1024 * 1024 + 17);
    let (path, md5) = fx.local("big.jpg", &body);
    let id = api
        .upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    assert_eq!(fx.fake.blob(&id).unwrap(), body);
    assert_eq!(fx.uploads("init"), 1);
    assert_eq!(fx.uploads("chunk"), 5);
    assert_eq!(fx.uploads("status"), 0);
    assert_eq!(fx.uploads("multipart"), 0);
    assert_eq!(fx.env.op_count("generate_ids"), 1);
    let ranges: Vec<String> = fx
        .fake
        .requests()
        .iter()
        .filter(|r| r["phase"] == "chunk")
        .map(|r| r["content_range"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ranges[0], "bytes 0-262143/1048593");
    assert_eq!(ranges[4], "bytes 1048576-1048592/1048593");
}

#[test]
fn an_interrupted_chunk_resumes_from_the_received_position() {
    let fx = Fx::new("api-resume-cut");
    // 512KiB のチャンク。2 つ目のチャンクを 300000 バイトまで受けたところで切る（残るのは 256KiB の倍数まで）
    let api = fx.api.clone().with_sizes(1000, 100 * 1024, 512 * 1024);
    let body = big(1_300_000);
    let (path, md5) = fx.local("big.jpg", &body);
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 1, 1, fk::cut_after(300_000));
    let id = api
        .upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    assert_eq!(fx.fake.blob(&id).unwrap(), body);
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 1);
    assert_eq!(fx.uploads("init"), 1, "セッションは始め直さない");
    assert_eq!(fx.uploads("status"), 1);
    let ranges: Vec<String> = fx
        .fake
        .requests()
        .iter()
        .filter(|r| r["phase"] == "chunk")
        .map(|r| r["content_range"].as_str().unwrap().to_owned())
        .collect();
    // 0〜 → 524288〜（切れた）→ 受け取り済みの 786432 から
    assert!(ranges[0].starts_with("bytes 0-"), "{ranges:?}");
    assert!(ranges[1].starts_with("bytes 524288-"), "{ranges:?}");
    assert!(ranges[2].starts_with("bytes 786432-"), "{ranges:?}");
}

#[test]
fn a_lost_session_is_started_again_with_the_same_id() {
    let fx = Fx::new("api-resume-lost");
    let api = small_chunks(&fx);
    let body = big(800_000);
    let (path, md5) = fx.local("big.jpg", &body);
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 1, 1, fk::lose_session());
    let id = api
        .upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    assert_eq!(fx.fake.blob(&id).unwrap(), body);
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 1);
    let inits: Vec<String> = fx
        .fake
        .requests()
        .iter()
        .filter(|r| r["phase"] == "init")
        .map(|r| r["new_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(inits, [id.clone(), id], "同じ ID で始め直す");
    assert_eq!(fx.env.op_count("generate_ids"), 1);
}

#[test]
fn a_final_chunk_whose_response_was_lost_is_found_complete_by_the_status_query() {
    let fx = Fx::new("api-resume-final-lost");
    let api = small_chunks(&fx);
    let body = big(300_000);
    let (path, md5) = fx.local("big.jpg", &body);
    // 2 つ目（最後）のチャンクは受けて、応答を返さずに切る
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 1, 1, fk::do_then_drop());
    let id = api
        .upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 1);
    assert_eq!(fx.fake.blob(&id).unwrap(), body);
    assert_eq!(fx.uploads("status"), 1);
}

#[test]
fn a_308_that_never_advances_stops_at_the_retry_limit_instead_of_the_deadline() {
    let fx = Fx::new("api-resume-stuck");
    let api = small_chunks(&fx);
    let body = big(600_000);
    let (path, md5) = fx.local("big.jpg", &body);
    // 受け取り済みの位置（Range）が付かない 308 が続く
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 0, 1000, fk::status(308));
    let r = api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up());
    assert_eq!(r, Err(ApiError::BadResponse));
    // 最初の 1 回 + 再試行の回数（3）。期限（45 分）まで送り続けない
    assert_eq!(fx.uploads("chunk"), 4);
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 0);
}

#[test]
fn chunk_failures_stop_after_the_retry_limit() {
    let fx = Fx::new("api-resume-giveup");
    let api = small_chunks(&fx);
    let body = big(600_000);
    let (path, md5) = fx.local("big.jpg", &body);
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 0, 1000, fk::drop_connection());
    let r = api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up());
    assert!(matches!(r, Err(ApiError::Net(_))), "{r:?}");
    // 最初の 1 回 + 再試行の回数（3）
    assert_eq!(fx.uploads("chunk"), 4);
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 0);
}

#[test]
fn a_session_location_on_another_host_is_never_used() {
    for (label, location) in [("other port", None), ("other host name", Some("localhost"))] {
        let fx = Fx::new(&format!("api-location-{}", label.replace(' ', "-")));
        let sink = fk::Sink::start();
        let api = small_chunks(&fx);
        let body = big(300_000);
        let (path, md5) = fx.local("big.jpg", &body);
        let target = match location {
            None => format!(
                "http://127.0.0.1:{}/upload/drive/v3/files?upload_id=x",
                sink.addr.port()
            ),
            Some(host) => format!(
                "http://{host}:{}/upload/drive/v3/files?upload_id=sess-0001",
                fx.fake.url().rsplit(':').next().unwrap()
            ),
        };
        fx.env.add_rule("upload", 0, 1, fk::bad_location(&target));
        let r = api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up());
        assert!(matches!(r, Err(ApiError::Rejected(_))), "{label}: {r:?}");
        assert_eq!(sink.connections(), 0, "{label}");
        assert_eq!(fx.uploads("chunk"), 0, "{label}");
        assert_eq!(fx.count_named(&fx.work, "big.jpg"), 0, "{label}");
    }
}

// ---- 移動・ゴミ箱 ----

#[test]
fn move_keeps_the_id_and_changes_parent_and_name_in_one_call() {
    let fx = Fx::new("api-move");
    let incoming = dir_in(&fx, &fx.work, "incoming-x");
    file_in(&fx, &incoming, "a.jpg", "a");
    fx.api
        .move_to(&incoming, &fx.work, &fx.yamareco, "2030-03-10", fx.dl())
        .unwrap();
    assert_eq!(fx.ops(), ["move"]);
    let d = fx.env.drive();
    let n = d.node(&incoming).unwrap();
    assert_eq!(
        (n.parent.as_str(), n.name.as_str()),
        (fx.yamareco.as_str(), "2030-03-10")
    );
    assert_eq!(d.children(&incoming).len(), 1, "中身はそのまま");
    // 移動元と移動先が同じ・形の違う引数は、送らずに断る
    for r in [
        fx.api
            .move_to(&incoming, &fx.yamareco, &fx.yamareco, "x", fx.dl()),
        fx.api
            .move_to(&incoming, &fx.work, &fx.yamareco, "a/b", fx.dl()),
        fx.api.move_to("a b", &fx.work, &fx.yamareco, "x", fx.dl()),
    ] {
        assert!(matches!(r, Err(ApiError::Rejected(_))));
    }
    assert_eq!(fx.ops(), ["move"]);
}

#[test]
fn a_move_whose_response_was_lost_is_checked_with_get_and_not_resent() {
    let fx = Fx::new("api-move-lost");
    let id = dir_in(&fx, &fx.work, "incoming-x");
    fx.env.add_rule("move", 0, 1, fk::do_then_drop());
    fx.api
        .move_to(&id, &fx.work, &fx.yamareco, "2030-03-10", fx.dl())
        .unwrap();
    // 再送の前に get で見て、済んでいたので送らない
    assert_eq!(fx.ops(), ["move", "get"]);
    assert_eq!(fx.env.drive().node(&id).unwrap().parent, fx.yamareco);
}

#[test]
fn a_move_that_failed_with_an_unknown_result_is_checked_with_get_and_never_resent() {
    // 5xx・通信断は、Drive 側で済んだか分からない。待っている間に場所や中身が変わっているかもしれないので、API の層は自分で再送せず、
    // get で見て、目的の状態でなければ失敗として返す（再送するかは、場所と中身を照合し直せる上位が決める）
    for (label, effect) in [
        ("500", fail(1)),
        ("503", fk::status(503)),
        ("drop", fk::drop_connection()),
    ] {
        let fx = Fx::new("api-move-500");
        let id = dir_in(&fx, &fx.work, "incoming-x");
        fx.env.add_rule("move", 0, u64::MAX, effect);
        let r = fx
            .api
            .move_to(&id, &fx.work, &fx.yamareco, "2030-03-10", fx.dl());
        assert!(
            matches!(r, Err(ApiError::Status(500 | 503) | ApiError::Net(_))),
            "{label}: {r:?}"
        );
        assert_eq!(fx.ops(), ["move", "get"], "{label}: 再送しない");
        assert_eq!(fx.env.drive().node(&id).unwrap().parent, fx.work, "{label}");
    }
}

#[test]
fn a_trash_that_failed_with_an_unknown_result_is_checked_with_get_and_never_resent() {
    for (label, effect) in [
        ("500", fail(1)),
        ("503", fk::status(503)),
        ("drop", fk::drop_connection()),
    ] {
        let fx = Fx::new("api-trash-500");
        let id = dir_in(&fx, &fx.work, "old");
        fx.env.add_rule("trash", 0, u64::MAX, effect);
        let r = fx.api.trash(&id, fx.dl());
        assert!(
            matches!(r, Err(ApiError::Status(500 | 503) | ApiError::Net(_))),
            "{label}: {r:?}"
        );
        assert_eq!(fx.ops(), ["trash", "get"], "{label}: 再送しない");
        assert!(!fx.env.drive().node(&id).unwrap().trashed, "{label}");
    }
}

#[test]
fn a_throttled_move_or_trash_is_waited_for_and_resent_after_checking_the_state() {
    // 429・流量制限の 403 は、処理されていないと分かる。待って再送してよい（その前に状態を見る）
    for effect in [fk::rate_limit_403(), fk::too_many_requests(0)] {
        let fx = Fx::new("api-move-throttled");
        let id = dir_in(&fx, &fx.work, "incoming-x");
        fx.env.add_rule("move", 0, 1, effect.clone());
        fx.api
            .move_to(&id, &fx.work, &fx.yamareco, "2030-03-10", fx.dl())
            .unwrap();
        assert_eq!(fx.ops(), ["move", "get", "move"], "{effect}");
        assert_eq!(fx.env.drive().node(&id).unwrap().name, "2030-03-10");

        let fx = Fx::new("api-trash-throttled");
        let old = dir_in(&fx, &fx.work, "old");
        fx.env.add_rule("trash", 0, 1, effect.clone());
        fx.api.trash(&old, fx.dl()).unwrap();
        assert_eq!(fx.ops(), ["trash", "get", "trash"], "{effect}");
        assert!(fx.env.drive().node(&old).unwrap().trashed);
    }
}

#[test]
fn a_401_before_a_move_or_trash_checks_the_state_before_the_resend() {
    let fx = Fx::new("api-move-401");
    let id = dir_in(&fx, &fx.work, "incoming-x");
    fx.env.add_rule("move", 0, 1, fk::unauthorized());
    fx.api
        .move_to(&id, &fx.work, &fx.yamareco, "2030-03-10", fx.dl())
        .unwrap();
    assert_eq!(fx.ops(), ["move", "get", "move"]);
    assert_eq!(fx.fake.tokens_issued(), 2);

    let fx = Fx::new("api-trash-401");
    let old = dir_in(&fx, &fx.work, "old");
    fx.env.add_rule("trash", 0, 1, fk::unauthorized());
    fx.api.trash(&old, fx.dl()).unwrap();
    assert_eq!(fx.ops(), ["trash", "get", "trash"]);
    assert_eq!(fx.fake.tokens_issued(), 2);
}

#[test]
fn a_401_does_not_reset_the_retry_count_of_a_move() {
    // 429 を 3 回（再試行の上限）→ 401 → 取り直し → 429 が続く。取り直しで回数が初期化されると、さらに 3 回送ってしまう
    let fx = Fx::new("api-401-count");
    let id = dir_in(&fx, &fx.work, "x");
    fx.env.add_rule("move", 0, 3, fk::too_many_requests(0));
    fx.env.add_rule("move", 3, 1, fk::unauthorized());
    fx.env
        .add_rule("move", 4, u64::MAX, fk::too_many_requests(0));
    let r = fx.api.move_to(&id, &fx.work, &fx.yamareco, "n", fx.dl());
    assert_eq!(r, Err(ApiError::RateLimited));
    // 429 ×3 → 401 → 429（上限）。初期化されていれば 8 回になる
    assert_eq!(fx.env.op_count("move"), 5);
}

#[test]
fn a_401_does_not_reset_the_no_resend_rule_for_a_later_unknown_failure() {
    // 401 → トークンの取り直し → 再送 → 500。取り直しで確認が初期化されて、さらに再送することはない
    for op in ["move", "trash"] {
        let fx = Fx::new("api-401-then-500");
        let id = dir_in(&fx, &fx.work, "x");
        fx.env.add_rule(op, 0, 1, fk::unauthorized());
        fx.env.add_rule(op, 1, 1, fail(1));
        let r = if op == "move" {
            fx.api.move_to(&id, &fx.work, &fx.yamareco, "n", fx.dl())
        } else {
            fx.api.trash(&id, fx.dl())
        };
        assert!(matches!(r, Err(ApiError::Status(500))), "{op}: {r:?}");
        assert_eq!(fx.ops(), [op, "get", op, "get"], "{op}");
        assert_eq!(fx.env.op_count(op), 2, "{op}");
    }
}

#[test]
fn a_rejected_move_is_success_only_when_already_in_the_target_state() {
    let fx = Fx::new("api-move-4xx");
    let id = dir_in(&fx, &fx.work, "incoming-x");
    // 403（流量制限でない）で断られ、状態は元のまま → 失敗
    fx.env.add_rule("move", 0, 1, fk::status(403));
    assert_eq!(
        fx.api.move_to(&id, &fx.work, &fx.yamareco, "n", fx.dl()),
        Err(ApiError::Status(403))
    );
    assert_eq!(fx.env.drive().node(&id).unwrap().parent, fx.work);
    // 親が違うものを外す要求は、偽の Drive が 404 にする（元の親でないものを指定した移動）
    assert_eq!(
        fx.api.move_to(&id, &fx.yamareco, &fx.work, "n", fx.dl()),
        Err(ApiError::NotFound)
    );
    assert_eq!(
        fx.api.move_to("zzz", &fx.work, &fx.yamareco, "n", fx.dl()),
        Err(ApiError::NotFound)
    );
}

#[test]
fn trash_moves_to_the_trash_and_can_be_repeated() {
    let fx = Fx::new("api-trash");
    let id = dir_in(&fx, &fx.work, "old");
    let inner = file_in(&fx, &id, "a.jpg", "a");
    fx.api.trash(&id, fx.dl()).unwrap();
    assert_eq!(fx.env.mutating_ops(), ["trash"]);
    let d = fx.env.drive();
    assert!(d.node(&id).unwrap().trashed);
    assert!(d.node(&inner).is_some(), "完全には消えない");
    assert!(d.trashed.contains(&id));
    assert_eq!(fx.api.list_children(&fx.work, fx.dl()).unwrap().len(), 0);
    assert!(fx.api.get(&id, fx.dl()).unwrap().trashed);
    fx.api.trash(&id, fx.dl()).unwrap();
    assert_eq!(fx.api.trash("zzz", fx.dl()), Err(ApiError::NotFound));
}

#[test]
fn a_trash_whose_response_was_lost_is_checked_with_get() {
    let fx = Fx::new("api-trash-lost");
    let id = dir_in(&fx, &fx.work, "old");
    fx.env.add_rule("trash", 0, 1, fk::do_then_drop());
    fx.api.trash(&id, fx.dl()).unwrap();
    assert_eq!(fx.ops(), ["trash", "get"]);
    assert!(fx.env.drive().node(&id).unwrap().trashed);
}

// ---- 再試行 ----

#[test]
fn rate_limits_and_server_errors_are_retried_then_succeed() {
    let fx = Fx::new("api-retry-ok");
    for effect in [
        fk::rate_limit_403(),
        fk::too_many_requests(0),
        fk::status(503),
        fk::drop_connection(),
        fk::cut_response(),
    ] {
        let fx2 = Fx::new("api-retry-ok-each");
        fx2.env.add_rule("list", 0, 2, effect.clone());
        assert!(
            fx2.api.list_children(&fx2.yamareco, fx2.dl()).is_ok(),
            "{effect}"
        );
        assert_eq!(fx2.env.op_count("list"), 3, "{effect}");
    }
    let _ = fx;
}

#[test]
fn a_rate_limit_403_is_recognised_wherever_the_reason_is_in_the_body() {
    // 理由が 1 件目でない・errors が空で本文の文言にだけある、でも流量制限として再試行する。権限の不足の 403 は再試行しない
    let retried = [
        fk::forbidden_with_reasons(&["backendError", "userRateLimitExceeded"]),
        fk::forbidden_with_reasons(&["rateLimitExceeded"]),
        fk::status_with_body(
            403,
            r#"{"error":{"code":403,"message":"User Rate Limit Exceeded. userRateLimitExceeded","errors":[]}}"#,
        ),
    ];
    for effect in retried {
        let fx = Fx::new("api-403-rate");
        fx.env.add_rule("list", 0, 1, effect.clone());
        assert!(
            fx.api.list_children(&fx.yamareco, fx.dl()).is_ok(),
            "{effect}"
        );
        assert_eq!(fx.env.op_count("list"), 2, "{effect}");
    }
    let not_retried = [
        fk::forbidden_with_reasons(&["insufficientFilePermissions"]),
        fk::forbidden_with_reasons(&["forbidden", "dailyLimitExceeded"]),
        fk::status_with_body(403, "<html>forbidden</html>"),
    ];
    for effect in not_retried {
        let fx = Fx::new("api-403-perm");
        fx.env.add_rule("list", 0, 100, effect.clone());
        assert_eq!(
            fx.api.list_children(&fx.yamareco, fx.dl()),
            Err(ApiError::Status(403)),
            "{effect}"
        );
        assert_eq!(fx.env.op_count("list"), 1, "{effect}");
    }
}

#[test]
fn retries_stop_at_the_limit() {
    type Check = fn(&ApiError) -> bool;
    let cases: [(Value, Check); 4] = [
        (fk::rate_limit_403(), |e| *e == ApiError::RateLimited),
        (fk::too_many_requests(0), |e| *e == ApiError::RateLimited),
        (fk::status(503), |e| *e == ApiError::Status(503)),
        (fk::drop_connection(), |e| matches!(e, ApiError::Net(_))),
    ];
    for (effect, check) in cases {
        let fx = Fx::new("api-retry-limit");
        fx.env.add_rule("list", 0, 1000, effect.clone());
        let r = fx.api.list_children(&fx.yamareco, fx.dl());
        assert!(check(r.as_ref().unwrap_err()), "{effect}: {r:?}");
        // 最初の 1 回 + 再試行の回数（3）
        assert_eq!(fx.env.op_count("list"), 4, "{effect}");
    }
}

#[test]
fn other_4xx_are_not_retried() {
    for (code, want) in [
        (400u16, ApiError::Status(400)),
        (403, ApiError::Status(403)),
        (404, ApiError::NotFound),
    ] {
        let fx = Fx::new("api-4xx");
        fx.env.add_rule("list", 0, 1000, fk::status(code));
        assert_eq!(fx.api.list_children(&fx.yamareco, fx.dl()), Err(want));
        assert_eq!(fx.env.op_count("list"), 1, "{code}");
    }
}

#[test]
fn retry_after_is_honoured_and_capped() {
    let fx = Fx::new("api-retry-after");
    let api = fx.api.clone().with_retry(Retry {
        max_retries: 3,
        base: Duration::from_millis(5),
        cap: Duration::from_secs(5),
    });
    fx.env.add_rule("list", 0, 1, fk::too_many_requests(1));
    let start = Instant::now();
    api.list_children(&fx.yamareco, fx.dl()).unwrap();
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(950), "{took:?}");
    assert!(took < Duration::from_secs(4), "{took:?}");

    // 上限（cap）を超える Retry-After は、上限で切る
    let fx = Fx::new("api-retry-after-cap");
    fx.env.add_rule("list", 0, 1, fk::too_many_requests(3600));
    let start = Instant::now();
    fx.api.list_children(&fx.yamareco, fx.dl()).unwrap();
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(190), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn a_401_refreshes_the_token_once_and_retries() {
    let fx = Fx::new("api-401");
    fx.env.add_rule("list", 0, 1, fk::unauthorized());
    fx.api.list_children(&fx.yamareco, fx.dl()).unwrap();
    assert_eq!(fx.fake.tokens_issued(), 2);
    assert_eq!(fx.env.op_count("list"), 2);

    // 失効したトークン（サーバー側で無効にした）も、取り直して通る
    fx.fake.revoke_tokens();
    fx.api.list_children(&fx.yamareco, fx.dl()).unwrap();
    assert_eq!(fx.fake.tokens_issued(), 3);

    // 取り直しても 401 なら、もう 1 度は取り直さない
    let fx = Fx::new("api-401-twice");
    fx.env.add_rule("list", 0, 1000, fk::unauthorized());
    assert_eq!(
        fx.api.list_children(&fx.yamareco, fx.dl()),
        Err(ApiError::Status(401))
    );
    assert_eq!(fx.fake.tokens_issued(), 2);
    assert_eq!(fx.env.op_count("list"), 2);
}

#[test]
fn redirects_are_not_followed() {
    let fx = Fx::new("api-redirect");
    let sink = fk::Sink::start();
    for code in [301u16, 302, 303, 307, 308] {
        fx.env.add_rule(
            "list",
            0,
            1,
            fk::redirect(code, &format!("http://127.0.0.1:{}/x", sink.addr.port())),
        );
        assert_eq!(
            fx.api.list_children(&fx.yamareco, fx.dl()),
            Err(ApiError::Status(code))
        );
    }
    assert_eq!(fx.env.op_count("list"), 5, "再試行も追従もしない");
    assert_eq!(sink.connections(), 0);
}

#[test]
fn a_garbage_body_is_a_bad_response_not_an_empty_listing() {
    let fx = Fx::new("api-garbage");
    fx.env.add_rule("list", 0, 1, garbage("not json"));
    assert_eq!(
        fx.api.list_children(&fx.yamareco, fx.dl()),
        Err(ApiError::BadResponse)
    );
    fx.env.add_rule("get", 0, 1, garbage("{\"id\":1}"));
    assert_eq!(
        fx.api.get(&fx.yamareco, fx.dl()),
        Err(ApiError::BadResponse)
    );
}

// ---- 期限・キャンセル ----

#[test]
fn the_operation_deadline_and_the_job_deadline_are_told_apart() {
    let fx = Fx::new("api-deadline");
    fx.env.add_rule("list", 0, 1000, sleep_ms(1500));
    let api = fx
        .api
        .clone()
        .with_timeouts(Duration::from_secs(60), Duration::from_millis(300));
    let start = Instant::now();
    assert_eq!(
        api.list_children(&fx.yamareco, api.deadline(Limit::Query)),
        Err(ApiError::Timeout)
    );
    assert!(
        start.elapsed() < Duration::from_millis(1400),
        "{:?}",
        start.elapsed()
    );

    let job = api.with_job_deadline(Some(Instant::now() + Duration::from_millis(250)));
    let start = Instant::now();
    assert_eq!(
        job.list_children(&fx.yamareco, job.deadline(Limit::Query)),
        Err(ApiError::JobDeadline)
    );
    assert!(
        start.elapsed() < Duration::from_millis(1400),
        "{:?}",
        start.elapsed()
    );

    // エラーコードの対応: 操作の期限は rclone_timeout、ジョブ全体は timeout
    let op = geoframe_export::remote::RemoteError::from(ApiError::Timeout);
    assert_eq!(
        op.into_op_error().code,
        geoframe_export::error::ErrorCode::RcloneTimeout
    );
    let jb = geoframe_export::remote::RemoteError::from(ApiError::JobDeadline);
    assert_eq!(
        jb.into_op_error().code,
        geoframe_export::error::ErrorCode::Timeout
    );
}

#[test]
fn a_deadline_cuts_a_retry_wait_short() {
    let fx = Fx::new("api-deadline-wait");
    let api = fx.api.clone().with_retry(Retry {
        max_retries: 5,
        base: Duration::from_secs(1),
        cap: Duration::from_secs(30),
    });
    fx.env.add_rule("list", 0, 1000, fk::too_many_requests(60));
    let start = Instant::now();
    let dl = Deadline::operation(Instant::now() + Duration::from_millis(500));
    assert_eq!(api.list_children(&fx.yamareco, dl), Err(ApiError::Timeout));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(fx.env.op_count("list"), 1);
}

#[test]
fn a_deadline_that_has_passed_sends_nothing() {
    let fx = Fx::new("api-deadline-past");
    let past = Deadline::operation(Instant::now() - Duration::from_secs(1));
    assert_eq!(
        fx.api.list_children(&fx.yamareco, past),
        Err(ApiError::Timeout)
    );
    let past = Deadline::job(Instant::now() - Duration::from_secs(1));
    assert_eq!(fx.api.get(&fx.yamareco, past), Err(ApiError::JobDeadline));
    assert_eq!(fx.env.ops(), Vec::<String>::new());
}

#[test]
fn a_cancelled_api_sends_nothing_and_a_cancel_during_a_retry_wait_stops_it() {
    let fx = Fx::new("api-cancel");
    let cancel = Cancel::new();
    let api = fx.api.clone().with_cancel(cancel.clone());
    cancel.cancel();
    assert_eq!(
        api.list_children(&fx.yamareco, fx.dl()),
        Err(ApiError::Cancelled)
    );
    assert_eq!(
        api.create_folder(&fx.work, "x", fx.dl()),
        Err(ApiError::Cancelled)
    );
    assert_eq!(fx.env.ops(), Vec::<String>::new());
    assert_eq!(
        geoframe_export::remote::RemoteError::from(ApiError::Cancelled),
        geoframe_export::remote::RemoteError::Cancelled
    );

    // 再試行の待ちの最中のキャンセル
    let fx = Fx::new("api-cancel-wait");
    let cancel = Cancel::new();
    let api = fx
        .api
        .clone()
        .with_cancel(cancel.clone())
        .with_retry(Retry {
            max_retries: 5,
            base: Duration::from_secs(20),
            cap: Duration::from_secs(30),
        });
    fx.env.add_rule("list", 0, 1000, fk::status(503));
    let c2 = cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c2.cancel();
    });
    let start = Instant::now();
    assert_eq!(
        api.list_children(&fx.yamareco, api.deadline(Limit::Query)),
        Err(ApiError::Cancelled)
    );
    t.join().unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_cancel_between_chunks_stops_the_upload() {
    let fx = Fx::new("api-cancel-chunk");
    let cancel = Cancel::new();
    let api = small_chunks(&fx).with_cancel(cancel.clone());
    let body = big(1_000_000);
    let (path, md5) = fx.local("big.jpg", &body);
    // 2 つ目のチャンクの応答を遅らせ、その要求が記録されてから（応答の前に）キャンセルする
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 1, 1, sleep_ms(400));
    let fake = &fx.fake;
    let r = std::thread::scope(|s| {
        let h = s.spawn(|| {
            wait_until(|| chunk_requests(fake) >= 2);
            cancel.cancel();
        });
        let r = api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up());
        h.join().unwrap();
        r
    });
    assert_eq!(r, Err(ApiError::Cancelled));
    assert_eq!(fx.uploads("chunk"), 2, "3 つ目は送らない");
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 0);
}

#[test]
fn a_cancel_stops_every_parallel_upload_and_all_threads_return() {
    let fx = Fx::new("api-cancel-parallel");
    let cancel = Cancel::new();
    let api = small_chunks(&fx).with_cancel(cancel.clone());
    let body = big(800_000);
    let files: Vec<_> = (0..4)
        .map(|i| fx.local(&format!("p{i}.jpg"), &body))
        .collect();
    // どのチャンクも 300ms かかる（3 チャンクで 900ms）。4 本とも最初のチャンクの要求が出たところでキャンセルする
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 0, 1000, sleep_ms(300));
    let start = Instant::now();
    let fake = &fx.fake;
    let results: Vec<Result<String, ApiError>> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .iter()
            .enumerate()
            .map(|(i, (path, md5))| {
                let api = api.clone();
                let work = fx.work.clone();
                let dl = fx.up();
                s.spawn(move || api.upload(&work, &format!("p{i}.jpg"), path, md5, 800_000, dl))
            })
            .collect();
        // 4 本とも、最初のチャンクの要求が出て、応答を待っているところ
        wait_until(|| chunk_requests(fake) >= 4);
        cancel.cancel();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in &results {
        assert_eq!(*r, Err(ApiError::Cancelled), "{results:?}");
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    for i in 0..4 {
        assert_eq!(fx.count_named(&fx.work, &format!("p{i}.jpg")), 0);
    }
}

// ---- 呼び出しの形 ----

#[test]
fn the_api_never_deletes_and_never_updates_file_contents() {
    let fx = Fx::new("api-shape");
    let api = small_chunks(&fx);
    let body = big(500_000);
    let (path, md5) = fx.local("big.jpg", &body);
    let (small, small_md5) = fx.local("s.jpg", b"s");
    let folder = fx.api.create_folder(&fx.work, "incoming", fx.dl()).unwrap();
    api.upload(&folder, "big.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    api.upload(&folder, "s.jpg", &small, &small_md5, 1, fx.up())
        .unwrap();
    fx.api.list_tree(&fx.work, fx.dl()).unwrap();
    let m = fx.api.list_children(&folder, fx.dl()).unwrap();
    fx.api.read(&m[0].id, 1_000_000, fx.dl()).unwrap();
    fx.api
        .move_to(&folder, &fx.work, &fx.yamareco, "2030-03-10", fx.dl())
        .unwrap();
    fx.api.trash(&folder, fx.dl()).unwrap();

    let requests = fx.fake.requests();
    assert!(!requests.is_empty());
    for r in &requests {
        let method = r["method"].as_str().unwrap();
        assert!(["GET", "POST", "PATCH", "PUT"].contains(&method), "{r}");
        assert_ne!(r["op"], "unsupported", "{r}");
        // PATCH は /drive/v3/files/<ID> だけ（`PATCH /upload/…` は中身の更新）。PUT は resumable のセッションだけ
        match method {
            "PATCH" => assert_eq!(r["route"], "files.update", "{r}"),
            "PUT" => assert_eq!(r["route"], "upload.session", "{r}"),
            _ => {}
        }
        // Authorization は、トークンの入口と resumable のセッションの URL を除くすべてに、有効なトークンで付く
        let expect_auth = !(r["op"] == "token" || r["route"] == "upload.session");
        if expect_auth {
            assert_eq!(r["authorization"], "ok", "{r}");
        } else {
            assert_eq!(r["authorization"], "missing", "{r}");
        }
        if r["op"] == "list" {
            let q = r["q"].as_str().unwrap();
            assert!(
                q.starts_with('\'') && q.ends_with("' in parents and trashed=false"),
                "{q}"
            );
            assert!(!q.contains("name"), "名前で絞らない: {q}");
        }
    }
    // 中立の操作名の記録が、1 段目の道具でそのまま読める
    let mut used = fx.env.ops_used();
    used.retain(|o| o != "token");
    assert_eq!(
        used,
        [
            "create_folder",
            "generate_ids",
            "list",
            "move",
            "read",
            "trash",
            "upload"
        ]
    );
    assert_eq!(fx.env.op_count("create_folder"), 1);
    assert_eq!(fx.env.op_count("token"), 1, "トークンは 1 回だけ取る");
}

#[test]
fn read_only_operations_leave_no_mutation_in_the_record() {
    let fx = Fx::new("api-readonly");
    let id = file_in(&fx, &fx.yamareco, "manifest.json", "{}");
    fx.api.list_children(&fx.yamareco, fx.dl()).unwrap();
    fx.api.list_tree(&fx.yamareco, fx.dl()).unwrap();
    fx.api.get(&id, fx.dl()).unwrap();
    fx.api.read(&id, 100, fx.dl()).unwrap();
    fx.api.generate_ids(2, fx.dl()).unwrap();
    // `dry_run` が読むだけであることの検査（`assert_no_mutations`）が、HTTP の記録でも使える
    fx.env.assert_no_mutations();
    assert_eq!(fx.env.op_count("create_folder"), 0);
    let _ = (ROOT, json!(null));
}

// ---- 形の壊れた応答を「空」として読まない ----

const EMPTY_PAGE: &str = r#"{"kind":"drive#fileList","incompleteSearch":false,"files":[]}"#;

#[test]
fn a_well_formed_empty_page_is_the_only_empty_listing() {
    let fx = Fx::new("api-list-empty-ok");
    fx.env.add_rule("list", 0, 1, garbage(EMPTY_PAGE));
    assert_eq!(fx.api.list_children(&fx.work, fx.dl()).unwrap(), []);
    // `incompleteSearch` が無い応答（偽の応答の古い形）も、`kind`・`files` が正しければ受ける
    let fx = Fx::new("api-list-empty-bare");
    fx.env.add_rule(
        "list",
        0,
        1,
        garbage(r#"{"kind":"drive#fileList","files":[]}"#),
    );
    assert_eq!(fx.api.list_children(&fx.work, fx.dl()).unwrap(), []);
}

#[test]
fn a_malformed_listing_is_a_failure_and_never_an_empty_one() {
    for body in [
        "",
        "null",
        "[]",
        "{}",
        r#""text""#,
        r#"{"files":[]}"#,
        r#"{"kind":"drive#fileList"}"#,
        r#"{"kind":"drive#fileList","files":null}"#,
        r#"{"kind":"drive#fileList","files":{}}"#,
        r#"{"kind":"drive#fileList","files":"x"}"#,
        r#"{"kind":"drive#file","files":[]}"#,
        r#"{"kind":null,"files":[]}"#,
        r#"{"kind":"drive#fileList","incompleteSearch":"no","files":[]}"#,
        r#"{"kind":"drive#fileList","files":[1]}"#,
        r#"{"kind":"drive#fileList","files":[{"id":"a"}]}"#,
    ] {
        let fx = Fx::new("api-list-malformed");
        fx.env.add_rule("list", 0, u64::MAX, garbage(body));
        assert_eq!(
            fx.api.list_children(&fx.work, fx.dl()),
            Err(ApiError::BadResponse),
            "{body}"
        );
        assert_eq!(fx.env.op_count("list"), 1, "{body}: 再試行しない");
        assert_eq!(
            fx.api.list_tree(&fx.work, fx.dl()),
            Err(ApiError::BadResponse),
            "{body}"
        );
    }
}

#[test]
fn an_incomplete_search_is_a_failed_query() {
    let fx = Fx::new("api-list-incomplete");
    fx.env.add_rule(
        "list",
        0,
        1,
        garbage(r#"{"kind":"drive#fileList","incompleteSearch":true,"files":[]}"#),
    );
    assert_eq!(
        fx.api.list_children(&fx.work, fx.dl()),
        Err(ApiError::Incomplete)
    );
    // 呼び出し側（ジョブのエラー）では、照会の失敗（drive_unavailable）
    use geoframe_export::error::ErrorCode;
    use geoframe_export::remote::RemoteError;
    assert_eq!(
        RemoteError::from(ApiError::Incomplete)
            .into_query_error()
            .code,
        ErrorCode::DriveUnavailable
    );
}

#[test]
fn a_malformed_later_page_fails_the_whole_listing() {
    let fx = Fx::new("api-list-malformed-page2");
    for i in 0..3 {
        file_in(&fx, &fx.work, &format!("f{i}.jpg"), "x");
    }
    let api = fx.api.clone().with_sizes(1, 100 * 1024, 256 * 1024);
    // 2 ページ目が、形の壊れた応答（空のページに見えるもの）
    fx.env.add_rule("list", 1, 1, garbage("null"));
    assert_eq!(
        api.list_children(&fx.work, fx.dl()),
        Err(ApiError::BadResponse)
    );
}

#[test]
fn a_get_response_must_be_an_object_for_the_requested_id() {
    let fx = Fx::new("api-get-shape");
    let id = dir_in(&fx, &fx.work, "x");
    for body in [
        "null".to_owned(),
        "[]".to_owned(),
        "{}".to_owned(),
        r#"{"id":"fidOTHER","name":"x","mimeType":"application/vnd.google-apps.folder","trashed":false}"#
            .to_owned(),
    ] {
        fx.env.add_rule("get", 0, 1, garbage(&body));
        assert_eq!(
            fx.api.get(&id, fx.dl()),
            Err(ApiError::BadResponse),
            "{body}"
        );
    }
    assert!(fx.api.get(&id, fx.dl()).is_ok());
}

#[test]
fn every_list_request_asks_for_the_kind_and_the_incomplete_flag() {
    let fx = Fx::new("api-list-fields");
    fx.api.list_children(&fx.work, fx.dl()).unwrap();
    let fields = fx
        .fake
        .requests()
        .iter()
        .find(|r| r["op"] == "list")
        .map(|r| r["fields"].as_str().unwrap().to_owned())
        .unwrap();
    assert!(fields.starts_with("kind,incompleteSearch,"), "{fields}");
}

// ---- resumable のチャンクの再試行の方針（共通の方針と同じ） ----

#[test]
fn a_rate_limited_chunk_waits_then_resumes_from_the_received_position() {
    // 流量制限の 403 は、以前はチャンクで即失敗していた。共通の再試行の方針に従い、待って、位置を問い合わせて続ける
    for effect in [
        fk::rate_limit_403(),
        fk::too_many_requests(0),
        fk::status(503),
    ] {
        let fx = Fx::new("api-chunk-throttled");
        let api = small_chunks(&fx);
        let body = big(600_000);
        let (path, md5) = fx.local("big.jpg", &body);
        fx.fake
            .add_rule_contains("upload", "phase:chunk", 1, 1, effect.clone());
        let id = api
            .upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up())
            .unwrap_or_else(|e| panic!("{effect}: {e:?}"));
        assert_eq!(fx.fake.blob(&id).unwrap(), body, "{effect}");
        assert_eq!(fx.count_named(&fx.work, "big.jpg"), 1, "{effect}");
        assert_eq!(fx.uploads("init"), 1, "{effect}");
        assert_eq!(
            fx.uploads("status"),
            1,
            "待ったあとに位置を問い合わせる: {effect}"
        );
        assert_eq!(fx.uploads("chunk"), 4, "{effect}");
    }
}

#[test]
fn a_chunk_honours_retry_after_and_caps_it() {
    let fx = Fx::new("api-chunk-retry-after");
    let api = small_chunks(&fx).with_retry(Retry {
        max_retries: 3,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(700),
    });
    let body = big(600_000);
    let (path, md5) = fx.local("big.jpg", &body);
    // Retry-After 1 秒は、上限（700ms）で切る。指数の待ち（1ms）より、はっきり長く待つ
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 1, 1, fk::too_many_requests(1));
    let start = Instant::now();
    api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up())
        .unwrap();
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(650), "{took:?}");
    assert_eq!(fx.uploads("status"), 1);
}

#[test]
fn a_rate_limited_chunk_stops_at_the_retry_limit_as_rate_limited() {
    let fx = Fx::new("api-chunk-throttle-giveup");
    let api = small_chunks(&fx);
    let body = big(600_000);
    let (path, md5) = fx.local("big.jpg", &body);
    fx.fake
        .add_rule_contains("upload", "phase:chunk", 0, 1000, fk::rate_limit_403());
    let r = api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up());
    assert_eq!(r, Err(ApiError::RateLimited));
    assert_eq!(fx.uploads("chunk"), 4);
    assert_eq!(fx.count_named(&fx.work, "big.jpg"), 0);
}

#[test]
fn a_chunk_refused_for_a_permission_reason_is_not_retried() {
    let fx = Fx::new("api-chunk-403-perm");
    let api = small_chunks(&fx);
    let body = big(600_000);
    let (path, md5) = fx.local("big.jpg", &body);
    fx.fake.add_rule_contains(
        "upload",
        "phase:chunk",
        0,
        1000,
        fk::forbidden_with_reasons(&["insufficientFilePermissions"]),
    );
    let r = api.upload(&fx.work, "big.jpg", &path, &md5, body.len() as u64, fx.up());
    assert_eq!(r, Err(ApiError::Status(403)));
    assert_eq!(fx.uploads("chunk"), 1);
}
