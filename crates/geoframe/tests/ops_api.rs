//! 運用タブの管理 API（`GET /api/admin/ops`・`POST /api/admin/ops/{sync,quality-scan,drive-release}`。
//! docs/admin-ops/design.md の PR ①）の結合試験。AC2(a)・AC3・AC4（API 分）・AC6・AC7（3 本分）。
//!
//! 2 種類の起動を使う:
//!
//! - **同じプロセスの router**（`spawn_state`）。`AppState` に `ops::Board` と `MassDeleteBoard` を入れ、同期ワーカーは
//!   起動しない（受付と GET だけを確かめる）。実行まで進める操作（`take_queued`・`guard().finish`）は試験が掲示板を直接動かす。
//! - **子プロセスの `serve`**（`GEOFRAME_INGEST=1`、Drive の鍵なし）。`tests/ingest_cli.rs` と同じ起動で、本物の同期
//!   ワーカーが要求を処理して履歴に残すところまで確かめる。
//!
//! 期待値は設計書と、試験が入れた合成の値から数えたもので、実装の出力を写していない。

mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TempDir, build_empty_catalog, post_raw, request, spawn_state};
use geoframe::db::Catalog;
use geoframe::drive::{HeldIds, HoldReason, MassDeleteBoard};
use geoframe::ops::{Board, OpKind, Outcome, stages_json};
use geoframe::stop::StopFlag;
use geoframe::sync::{Counts, CycleReport, StageReport, StageResult};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_geoframe");
const PYTHON_SCHEMA: &str = include_str!("fixtures/schema-python.json");
const OPS: &str = "/api/admin/ops";
const POSTS: [&str; 3] = ["sync", "quality-scan", "drive-release"];

// ================================================================ 同じプロセスの router

struct Server {
    _dir: TempDir,
    addr: std::net::SocketAddr,
    board: Arc<Board>,
    mass: Arc<MassDeleteBoard>,
    stop: StopFlag,
}

impl Server {
    /// 掲示板と保留の記憶を持つ `serve`（取り込み有効の形）。
    async fn start() -> Self {
        Self::start_with(true).await
    }

    /// `ingest: false` なら `ops` を入れない（`INGEST=0` の `serve` と同じ）。
    async fn start_with(ingest: bool) -> Self {
        let dir = TempDir::new("ops-api");
        let (path, _db) = build_empty_catalog(dir.path(), "");
        let catalog = Catalog::open(&path).unwrap();
        let stop = StopFlag::new();
        let board = Arc::new(Board::with_system_clock(stop.clone()));
        let mass = Arc::new(MassDeleteBoard::default());
        let addr = {
            let (board, mass) = (Arc::clone(&board), Arc::clone(&mass));
            spawn_state(dir.path(), &[], catalog, move |state| {
                if ingest {
                    state.with_ops(board).with_mass_delete_board(mass)
                } else {
                    state
                }
            })
            .await
        };
        Self {
            _dir: dir,
            addr,
            board,
            mass,
            stop,
        }
    }

    async fn get_ops(&self) -> common::Response {
        common::get(self.addr, OPS).await
    }

    async fn ops(&self) -> Value {
        let response = self.get_ops().await;
        assert_eq!(response.status, 200, "{}", response.text());
        response.json()
    }

    /// CSRF の条件を満たして POST する。
    async fn post(&self, route: &str, body: &Value) -> common::Response {
        self.post_text(route, &body.to_string()).await
    }

    async fn post_text(&self, route: &str, body: &str) -> common::Response {
        self.post_with(route, body, &[]).await
    }

    /// `extra` のヘッダを足す（`Content-Type` を上書きしたいときは `override_type`）。
    async fn post_with(&self, route: &str, body: &str, extra: &[(&str, &str)]) -> common::Response {
        let mut headers = vec![
            ("Host".to_owned(), "localhost".to_owned()),
            ("Content-Length".to_owned(), body.len().to_string()),
            ("Connection".to_owned(), "close".to_owned()),
        ];
        if !extra.iter().any(|(k, _)| *k == "Content-Type") {
            headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
        }
        headers.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        );
        post_raw(
            self.addr,
            &format!("{OPS}/{route}"),
            &headers,
            body.as_bytes(),
        )
        .await
    }

    /// 比率の保留を記録して、その `token` を GET から取る。
    async fn hold_ratio(&self, ids: &[i64]) -> String {
        self.mass.record_with_ids(
            Some((ids.len() as u64, HoldReason::Ratio, 500)),
            HeldIds::new(ids.to_vec()),
            "2026-11-02 10:00:00",
        );
        self.hold_token().await
    }

    async fn hold_token(&self) -> String {
        self.ops().await["hold"]["token"]
            .as_str()
            .expect("hold.token")
            .to_owned()
    }
}

/// 失敗の文面にパスと SQLite のメッセージを入れた、合成の段の結果。
const SECRET_TEXT: &str = "/srv/geoframe/data/catalog.db: no such table: secret_photos";

fn failing_report() -> CycleReport {
    CycleReport {
        stages: vec![
            StageReport {
                name: "index_local",
                result: StageResult::Done(Counts(vec![("indexed", 3)])),
            },
            StageReport {
                name: "index_drive",
                result: StageResult::Failed(SECRET_TEXT.to_owned()),
            },
            StageReport {
                name: "fetch_derivatives",
                result: StageResult::Skipped("Drive の鍵がありません"),
            },
        ],
        photos: None,
    }
}

fn error_of(response: &common::Response) -> String {
    let body = response.json();
    assert_eq!(body["ok"], false, "{}", response.text());
    body["error"].as_str().expect("error").to_owned()
}

// ---------------------------------------------------------------- AC6: GET

#[tokio::test]
async fn get_is_200_no_store_json_with_an_empty_board_and_head_and_post_are_403() {
    let server = Server::start().await;
    let response = server.get_ops().await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(
        response.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["ingest"], true);
    // 既定の同期の間隔（分）
    assert!(body["poll_minutes"].as_f64().unwrap() > 0.0, "{body}");
    assert_eq!(body["worker"]["running"], Value::Null);
    assert_eq!(body["worker"]["queued"], Value::Null);
    assert_eq!(body["worker"]["last_cycle"], Value::Null);
    assert_eq!(body["hold"], Value::Null);
    assert_eq!(body["ai"], json!({"retry_kinds": []}));
    assert_eq!(body["history"], json!([]));

    // HEAD・POST は 403（`guard`）。完全一致以外（末尾 `/`・接尾辞）も 403
    let head = request(server.addr, "HEAD", OPS, &[("Host", "localhost")]).await;
    assert_eq!(head.status, 403);
    let post = post_raw(
        server.addr,
        OPS,
        &[
            ("Host".into(), "localhost".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), "2".into()),
            ("Connection".into(), "close".into()),
        ],
        b"{}",
    )
    .await;
    assert_eq!(post.status, 403);
    for target in ["/api/admin/ops/", "/api/admin/opsX"] {
        let response = request(server.addr, "GET", target, &[("Host", "localhost")]).await;
        assert_eq!(response.status, 403, "{target}");
    }
    // 掲示板は何も受け付けていない
    assert!(server.board.snapshot().queued.is_none());
}

#[tokio::test]
async fn without_ingest_get_says_so_and_every_post_is_503() {
    let server = Server::start_with(false).await;
    let response = server.get_ops().await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(
        response.json(),
        json!({
            "ok": true, "ingest": false, "worker": null, "hold": null, "ai": null, "history": []
        })
    );
    let token = "0123456789abcdef";
    for route in POSTS {
        let body = if route == "drive-release" {
            json!({ "token": token })
        } else {
            json!({})
        };
        let response = server.post(route, &body).await;
        assert_eq!(response.status, 503, "{route}: {}", response.text());
        assert_eq!(response.json()["ok"], false);
        // 本文の形（400）は掲示板の有無（503）より先
        assert_eq!(server.post_text(route, "not json").await.status, 400);
    }
    assert_eq!(
        server
            .post("drive-release", &json!({"token": "nope"}))
            .await
            .status,
        400
    );
}

#[tokio::test]
async fn the_body_has_neither_failure_text_nor_held_ids() {
    let server = Server::start().await;
    // 履歴の 1 件（要求）と、直近の定期サイクル。どちらにも失敗した段を入れる
    let report = failing_report();
    server.board.enqueue(OpKind::Sync, None, None).unwrap();
    let request = server.board.take_queued().unwrap();
    server
        .board
        .guard(request.id)
        .finish(Outcome::Partial, stages_json(&report), None);
    server.board.start_cycle();
    server
        .board
        .cycle_guard(Duration::from_secs(600))
        .finish_cycle(&report);
    // 保留した行の ID（応答のどこにも出てはいけない）
    let ids = [8_675_309_i64, 8_675_310, 8_675_311];
    server.hold_ratio(&ids).await;

    let response = server.get_ops().await;
    let text = response.text().to_owned();
    for needle in [
        "catalog.db",
        "secret_photos",
        "no such table",
        "/srv/",
        "8675309",
        "8675310",
        "8675311",
    ] {
        assert!(!text.contains(needle), "{needle} が出ている: {text}");
    }
    let body = response.json();
    // 失敗は結果の語だけ。固定の理由（`Skipped`）は出る
    let stages = &body["history"][0]["stages"];
    assert_eq!(stages[1]["name"], "index_drive");
    assert_eq!(stages[1]["result"], "failed");
    assert!(stages[1].get("reason").is_none(), "{stages}");
    assert_eq!(stages[2]["reason"], "Drive の鍵がありません");
    assert_eq!(body["history"][0]["outcome"], "partial");
    assert_eq!(body["worker"]["last_cycle"]["outcome"], "partial");
    // 保留は 7 つの項目だけ（ID の列も、`ids` の鍵も無い）
    let hold = body["hold"].as_object().unwrap();
    let mut keys: Vec<_> = hold.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "checked_at",
            "drive_rows",
            "held",
            "reason",
            "releasable",
            "since",
            "token"
        ]
    );
    assert_eq!(hold["held"], 3);
    assert_eq!(hold["reason"], "ratio");
    assert_eq!(hold["drive_rows"], 500);
    assert_eq!(hold["releasable"], true);
    assert_eq!(hold["token"].as_str().unwrap().len(), 16);
}

// ---------------------------------------------------------------- 受付・busy・too_soon

#[tokio::test]
async fn sync_and_quality_scan_are_accepted_as_queued_requests() {
    let server = Server::start().await;
    let response = server.post("sync", &json!({})).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["request"]["kind"], "sync");
    assert_eq!(body["request"]["state"], "queued");
    let id = body["request"]["id"].as_u64().unwrap();
    assert!(id >= 1);
    // GET に待ちとして見える
    let ops = server.ops().await;
    assert_eq!(ops["worker"]["queued"]["id"], id);
    assert_eq!(ops["worker"]["queued"]["kind"], "sync");

    // 待ちは 1 つだけ: 別の種類も 409 busy
    let response = server.post("quality-scan", &json!({})).await;
    assert_eq!(response.status, 409);
    assert_eq!(error_of(&response), "busy");

    // ワーカーが取り出して終えたあと、品質解析の一括は受け付ける
    let request = server.board.take_queued().unwrap();
    assert_eq!(request.id, id);
    server
        .board
        .guard(id)
        .finish(Outcome::Done, Vec::new(), None);
    let response = server.post("quality-scan", &json!({})).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let body = response.json();
    assert_eq!(body["request"]["kind"], "quality_scan");
    assert_eq!(body["request"]["state"], "queued");
    assert!(body["request"]["id"].as_u64().unwrap() > id);
}

#[tokio::test]
async fn a_second_request_of_the_same_kind_is_busy_while_one_is_waiting_or_running() {
    let server = Server::start().await;
    assert_eq!(server.post("sync", &json!({})).await.status, 200);
    // 待ちがある
    let response = server.post("sync", &json!({})).await;
    assert_eq!((response.status, error_of(&response)), (409, "busy".into()));
    // 実行中（取り出した直後）でも同じ種類は受け付けない
    let request = server.board.take_queued().unwrap();
    let response = server.post("sync", &json!({})).await;
    assert_eq!((response.status, error_of(&response)), (409, "busy".into()));
    // 別の種類は 1 つ待てる。2 つ目は busy
    assert_eq!(server.post("quality-scan", &json!({})).await.status, 200);
    let response = server.post("quality-scan", &json!({})).await;
    assert_eq!((response.status, error_of(&response)), (409, "busy".into()));
    let ops = server.ops().await;
    assert_eq!(ops["worker"]["running"]["kind"], "sync");
    assert_eq!(ops["worker"]["queued"]["kind"], "quality_scan");
    server
        .board
        .guard(request.id)
        .finish(Outcome::Done, Vec::new(), None);
}

#[tokio::test]
async fn drive_reading_requests_are_too_soon_right_after_one_finished() {
    let server = Server::start().await;
    let token = server.hold_ratio(&[11, 12, 13]).await;
    assert_eq!(server.post("sync", &json!({})).await.status, 200);
    let request = server.board.take_queued().unwrap();
    server
        .board
        .guard(request.id)
        .finish(Outcome::Done, Vec::new(), None);

    // 前回の終了から 60 秒以内: Drive を読む 2 つ（同期・保留の解除）は too_soon
    let response = server.post("sync", &json!({})).await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "too_soon".into())
    );
    let response = server
        .post("drive-release", &json!({ "token": token }))
        .await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "too_soon".into())
    );
    // 品質解析の一括は Drive を読まないので受け付ける
    assert_eq!(server.post("quality-scan", &json!({})).await.status, 200);
    assert!(server.ops().await["worker"]["queued"]["kind"] == "quality_scan");
}

#[tokio::test]
async fn after_the_stop_signal_every_post_is_503() {
    let server = Server::start().await;
    let token = server.hold_ratio(&[1, 2, 3]).await;
    server.stop.stop();
    for route in POSTS {
        let body = if route == "drive-release" {
            json!({ "token": token })
        } else {
            json!({})
        };
        let response = server.post(route, &body).await;
        assert_eq!(response.status, 503, "{route}: {}", response.text());
    }
    assert!(server.board.snapshot().queued.is_none());
}

// ---------------------------------------------------------------- AC4（API 分）: drive-release

#[tokio::test]
async fn drive_release_accepts_only_the_token_of_the_current_hold() {
    let server = Server::start().await;
    let ids = [1001_i64, 1002, 1003, 1004];
    let token = server.hold_ratio(&ids).await;

    // 違う印（形は正しい）は 409 hold_changed。何も入らない
    // 先頭の 1 文字だけ変えた、形は正しい別の印
    let first = if token.starts_with('0') { '1' } else { '0' };
    let other = format!("{first}{}", &token[1..]);
    let response = server
        .post("drive-release", &json!({ "token": other }))
        .await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "hold_changed".into())
    );
    assert!(server.board.snapshot().queued.is_none());

    // 正しい印: 200。受け付けた要求と承認した件数
    let response = server
        .post("drive-release", &json!({ "token": token }))
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["request"]["kind"], "drive_release");
    assert_eq!(body["request"]["state"], "queued");
    assert_eq!(body["approved"], 4);
    // ID は応答に出ない
    assert!(!response.text().contains("1001"), "{}", response.text());
    // 要求には印と承認した行の集合が入る（ワーカーだけが読む）
    let queued = server.board.snapshot().queued.expect("待ちに入っている");
    assert_eq!(queued.kind, OpKind::DriveRelease);
    let request = server.board.take_queued().unwrap();
    assert_eq!(request.token.as_deref(), Some(token.as_str()));
    assert_eq!(request.ids.as_ref().unwrap().as_slice(), ids);
    assert!(!format!("{request:?}").contains("1001"));
}

#[tokio::test]
async fn drive_release_without_a_hold_is_no_hold_and_a_changed_hold_is_hold_changed() {
    let server = Server::start().await;
    let token = "0123456789abcdef";
    // 記録が無い
    let response = server
        .post("drive-release", &json!({ "token": token }))
        .await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "no_hold".into())
    );

    // 画面が見た保留が、同じ件数・同じ理由のまま行の集合だけ変わった: 印が変わるので 409 hold_changed
    let old = server.hold_ratio(&[1, 2, 3]).await;
    server.mass.record_with_ids(
        Some((3, HoldReason::Ratio, 500)),
        HeldIds::new(vec![1, 2, 4]),
        "2026-11-02 10:10:00",
    );
    let new = server.hold_token().await;
    assert_ne!(old, new, "行の集合が違えば印も違う");
    let response = server.post("drive-release", &json!({ "token": old })).await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "hold_changed".into())
    );

    // 保留が消えたあとは no_hold
    server.mass.record(None, "2026-11-02 10:20:00");
    assert_eq!(server.ops().await["hold"], Value::Null);
    let response = server.post("drive-release", &json!({ "token": new })).await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "no_hold".into())
    );
    assert!(server.board.snapshot().queued.is_none());
}

#[tokio::test]
async fn an_empty_listing_hold_is_not_releasable() {
    let server = Server::start().await;
    server.mass.record_with_ids(
        Some((120, HoldReason::EmptyListing, 120)),
        HeldIds::new((1..=120).collect()),
        "2026-11-02 10:00:00",
    );
    let ops = server.ops().await;
    assert_eq!(ops["hold"]["reason"], "empty_listing");
    assert_eq!(ops["hold"]["releasable"], false);
    // 正しい印でも解除しない
    let token = ops["hold"]["token"].as_str().unwrap().to_owned();
    let response = server
        .post("drive-release", &json!({ "token": token }))
        .await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "not_releasable".into())
    );
    assert!(server.board.snapshot().queued.is_none());
}

#[tokio::test]
async fn a_malformed_token_is_400_bad_params_whatever_the_hold() {
    let server = Server::start().await;
    let token = server.hold_ratio(&[1, 2, 3]).await;
    let upper = token.to_uppercase();
    let cases = [
        json!({}),
        json!({"token": null}),
        json!({"token": 1_234_567_890_123_456_u64}),
        json!({"token": ["0123456789abcdef"]}),
        json!({"token": ""}),
        json!({"token": "0123456789abcde"}),
        json!({"token": "0123456789abcdef0"}),
        json!({"token": "0123456789abcdeg"}),
        json!({"token": "0123456789ABCDEF"}),
        json!({"token": format!("{token} ")}),
    ];
    for body in cases {
        let response = server.post("drive-release", &body).await;
        assert_eq!(response.status, 400, "{body}: {}", response.text());
        assert_eq!(error_of(&response), "bad_params", "{body}");
    }
    // 大文字は別の入力（印そのものは小文字の hex）
    if upper != token {
        assert_eq!(
            server
                .post("drive-release", &json!({ "token": upper }))
                .await
                .status,
            400
        );
    }
    // 保留が無くても、形が先に断られる
    server.mass.record(None, "2026-11-02 10:30:00");
    assert_eq!(
        server
            .post("drive-release", &json!({"token": "zz"}))
            .await
            .status,
        400
    );
    assert!(server.board.snapshot().queued.is_none());
}

// ---------------------------------------------------------------- AC7: 入口の検査

#[tokio::test]
async fn the_common_entrance_checks_apply_to_all_three_posts() {
    let server = Server::start().await;
    let token = server.hold_ratio(&[1, 2, 3]).await;
    for route in POSTS {
        let ok = if route == "drive-release" {
            json!({ "token": token }).to_string()
        } else {
            "{}".to_owned()
        };
        // Content-Type の不一致
        let response = server
            .post_with(route, &ok, &[("Content-Type", "text/plain")])
            .await;
        assert_eq!(response.status, 403, "{route} content-type");
        // Origin の不一致（ホストが違う・ポートが違う）
        for origin in ["http://evil.example", "http://localhost:1"] {
            let response = server.post_with(route, &ok, &[("Origin", origin)]).await;
            assert_eq!(response.status, 403, "{route} {origin}");
        }
        // 本文が 64 KiB を超える
        let filler = "x".repeat(64 * 1024 + 1);
        let big = format!(r#"{{"pad":"{filler}"}}"#);
        assert_eq!(server.post_text(route, &big).await.status, 413, "{route}");
        // JSON でない・オブジェクトでない
        for bad in ["not json", "", "[]", "\"x\"", "12", "{\"token\":"] {
            assert_eq!(
                server.post_text(route, bad).await.status,
                400,
                "{route}: {bad:?}"
            );
        }
        // Content-Length が無い
        let response = post_raw(
            server.addr,
            &format!("{OPS}/{route}"),
            &[
                ("Host".into(), "localhost".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Connection".into(), "close".into()),
            ],
            b"",
        )
        .await;
        assert_eq!(response.status, 400, "{route} content-length");
    }
    // 断られた要求は何も入れない。同じ Origin は通る
    assert!(server.board.snapshot().queued.is_none());
    let response = server
        .post_with("sync", "{}", &[("Origin", "http://localhost")])
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
}

// ================================================================ 子プロセスの serve

/// 取り込みを動かすための環境変数（実効 UID 1000・存在しない Drive の鍵）。
fn ingest_env(root: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("GEOFRAME_TEST_EUID", "1000".into()),
        ("GEOFRAME_INGEST", "1".into()),
        (
            "GEOFRAME_DRIVE_KEY",
            root.join("no_such_drive_key.json").to_str().unwrap().into(),
        ),
    ]
}

struct Child1 {
    child: Child,
    port: u16,
    log: PathBuf,
}

impl Child1 {
    fn start(root: &Path, data: &Path, extra: &[(&str, &str)]) -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log = root.join("serve.log");
        let out = fs::File::create(&log).unwrap();
        let child = Command::new(BIN)
            .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
            .args(["--data-dir", data.to_str().unwrap()])
            .env_clear()
            .envs(ingest_env(root))
            .envs(extra.iter().copied())
            .stdout(Stdio::from(out.try_clone().unwrap()))
            .stderr(Stdio::from(out))
            .spawn()
            .unwrap();
        let mut me = Self { child, port, log };
        me.wait_healthy();
        me
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn wait_healthy(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "serve が終了しました: {}",
                self.log()
            );
            if http(self.port, "GET", "/healthz", None).is_some_and(|r| r.0 == 200) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("serve が /healthz に答えません: {}", self.log());
    }

    fn get_ops(&self) -> Value {
        let (status, _, body) = http(self.port, "GET", OPS, None).expect("GET /api/admin/ops");
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    fn post(&self, route: &str, body: &Value) -> (u16, Value) {
        let (status, _, text) = http(
            self.port,
            "POST",
            &format!("{OPS}/{route}"),
            Some(&body.to_string()),
        )
        .expect("POST");
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// 履歴に `id` の記録が現れる（終了している）まで最長 `secs` 秒待つ。
    fn wait_history(&self, id: u64, secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let ops = self.get_ops();
            if let Some(record) = ops["history"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
            {
                return record.clone();
            }
            assert!(
                Instant::now() < deadline,
                "要求 {id} が終わりません: {ops}\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 起動直後の定期サイクルが終わる（`last_cycle` が出る）まで待つ。
    fn wait_first_cycle(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let ops = self.get_ops();
            if !ops["worker"]["last_cycle"].is_null() {
                return ops["worker"]["last_cycle"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "起動直後のサイクルが終わりません: {ops}\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Child1 {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(15);
            while self.child.try_wait().ok().flatten().is_none() {
                if Instant::now() > deadline {
                    let _ = self.child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = self.child.wait();
    }
}

/// 最小の同期 HTTP クライアント。`(状態コード, ヘッダ, 本文)`。
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> Option<(u16, String, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(body) = body {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
    } else {
        request.push_str("\r\n");
    }
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let status = head.split(' ').nth(1)?.parse().ok()?;
    Some((status, head.to_lowercase(), body.to_owned()))
}

/// 本番相当（Python が全移行を終えた形）のスキーマの空の DB。
fn production_shaped_db(data: &Path) -> rusqlite::Connection {
    fs::create_dir_all(data).unwrap();
    let conn = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    let fixture: Value = serde_json::from_str(PYTHON_SCHEMA).unwrap();
    for object in fixture["objects"].as_array().unwrap() {
        if let Some(sql) = object["sql"].as_str() {
            conn.execute_batch(sql).unwrap();
        }
    }
    conn
}

/// 合成の JPEG（EXIF なし。`value` で画素が変わる）。
fn solid_jpeg(value: u8) -> Vec<u8> {
    let mut data = Vec::new();
    for y in 0..48u32 {
        for x in 0..64u32 {
            let v = value.wrapping_add((x * 3 + y) as u8);
            data.extend_from_slice(&[v, v.wrapping_mul(2), 255 - v]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 85)
        .encode(&data, 64, 48, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    out
}

fn stage<'a>(record: &'a Value, name: &str) -> &'a Value {
    record["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("段 {name} が無い: {record}"))
}

/// AC2(a): 本物の同期ワーカーが `sync` の要求を処理して、履歴に段ごとの結果を残す。
#[test]
fn a_sync_request_runs_the_stages_in_a_real_serve_and_lands_in_the_history() {
    let dir = TempDir::new("ops-serve-sync");
    let data = dir.path().join("data");
    drop(production_shaped_db(&data));
    let server = Child1::start(dir.path(), &data, &[]);
    // 起動直後の定期サイクルが済んでから要求する（定期サイクルの実行中でも入るが、結果を決まった形で見るため）
    let cycle = server.wait_first_cycle();
    assert_eq!(cycle["kind"], "cycle");
    assert_eq!(cycle["id"], 0);

    let (status, body) = server.post("sync", &json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["request"]["kind"], "sync");
    assert_eq!(body["request"]["state"], "queued");
    let id = body["request"]["id"].as_u64().unwrap();

    let record = server.wait_history(id, 20);
    assert_eq!(record["kind"], "sync");
    assert_eq!(record["outcome"], "done", "{record}");
    assert!(record["requested_at"].is_string());
    assert!(record["started_at"].is_string());
    assert!(record["finished_at"].is_string());
    // 鍵が無いので Drive の段は通信せずに飛ばす。ローカル写真の段は走る
    let drive = stage(&record, "index_drive");
    assert_eq!(drive["result"], "skipped", "{record}");
    assert!(drive["reason"].is_string());
    assert_eq!(stage(&record, "index_local")["result"], "done", "{record}");
    // 終わったあとは実行中も待ちも無い。直近の定期サイクルは別に残る
    let ops = server.get_ops();
    assert_eq!(ops["ingest"], true);
    assert!(ops["worker"]["running"].is_null(), "{ops}");
    assert!(ops["worker"]["queued"].is_null(), "{ops}");
    assert_eq!(ops["worker"]["last_cycle"]["kind"], "cycle");
    assert!(ops["worker"]["next_cycle_at"].is_string(), "{ops}");

    // 続けて同期を送ると、前回の Drive を読む要求の終了から 60 秒以内なので too_soon
    let (status, body) = server.post("sync", &json!({}));
    assert_eq!(
        (status, body["error"].as_str()),
        (409, Some("too_soon")),
        "{body}"
    );
    // 履歴にも、応答にも、環境の情報は出ない
    let text = server.get_ops().to_string();
    assert!(!text.contains("catalog.db"), "{text}");
    assert!(!text.contains(dir.path().to_str().unwrap()), "{text}");
}

/// AC3: `GEOFRAME_QUALITY_PER_CYCLE=1` の `serve` で、定期サイクルは 1 枚だけ解析する。品質解析の一括は残りを全部解析して
/// 再構築する。
#[test]
fn a_quality_scan_request_analyzes_what_the_periodic_cycle_left() {
    let dir = TempDir::new("ops-serve-quality");
    let data = dir.path().join("data");
    let conn = production_shaped_db(&data);
    fs::create_dir_all(data.join("derivatives")).unwrap();
    // 位置も撮影時刻も無い 3 枚（`infer`・`enrich` は何もしない）。表示用の画像は有効な合成 JPEG
    for n in 0..3u8 {
        fs::write(
            data.join(format!("derivatives/q{n}.jpg")),
            solid_jpeg(n * 40),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO photos(source,drive_id,name,display) VALUES ('drive',?1,?2,?3)",
            rusqlite::params![
                format!("drive-q{n}"),
                format!("q{n}"),
                format!("derivatives/q{n}.jpg")
            ],
        )
        .unwrap();
    }
    drop(conn);

    let server = Child1::start(dir.path(), &data, &[("GEOFRAME_QUALITY_PER_CYCLE", "1")]);
    let cycle = server.wait_first_cycle();
    let quality = stage(&cycle, "quality");
    assert_eq!(quality["result"], "done", "{cycle}");
    assert_eq!(quality["counts"]["analyzed"], 1, "{cycle}");

    // 定期サイクルは 1 枚だけ解析した: 未解析が 2 枚残り、特徴量の行は 1 枚ぶん
    let db = rusqlite::Connection::open(data.join("catalog.db")).unwrap();
    let features = |db: &rusqlite::Connection| -> i64 {
        db.query_row("SELECT COUNT(*) FROM photo_quality_features", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(features(&db), 1);

    let (status, body) = server.post("quality-scan", &json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["request"]["kind"], "quality_scan");
    let id = body["request"]["id"].as_u64().unwrap();
    let record = server.wait_history(id, 20);
    assert_eq!(record["kind"], "quality_scan");
    assert_eq!(record["outcome"], "done", "{record}");
    // 段は `quality` の 1 つだけ。残りの 2 枚を解析し、未解析は 0、再構築した
    assert_eq!(record["stages"].as_array().unwrap().len(), 1, "{record}");
    let counts = &stage(&record, "quality")["counts"];
    assert!(counts["analyzed"].as_u64().unwrap() >= 2, "{record}");
    assert_eq!(counts["failed"], 0, "{record}");
    assert_eq!(counts["remaining"], 0, "{record}");
    assert_eq!(counts["rebuilt"], 1, "{record}");
    assert_eq!(features(&db), 3);
}
