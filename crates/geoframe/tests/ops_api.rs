//! 運用タブの管理 API（`GET /api/admin/ops`・`POST /api/admin/ops/{sync,quality-scan,drive-release,ai-retry,
//! ai-release-pause}`。docs/admin-ops/design.md）の結合試験。AC2(a)・AC3・AC4（API 分）・AC5・AC6・AC7。
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{TempDir, build_empty_catalog, post_raw, request, spawn_state};
use geoframe::ai::jobs::{Claimed, JobKey, JobKind, Recheck};
use geoframe::ai::worker::{Clock, Handler, Persist, RunCtx, RunError};
use geoframe::db::Catalog;
use geoframe::drive::{HeldIds, HoldReason, MassDeleteBoard};
use geoframe::ops::{Board, OpKind, Outcome, stages_json};
use geoframe::stop::StopFlag;
use geoframe::sync::{Counts, CycleReport, StageReport, StageResult};
use geoframe::writer::{OpenMode, Writer};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_geoframe");
const PYTHON_SCHEMA: &str = include_str!("fixtures/schema-python.json");
const OPS: &str = "/api/admin/ops";
const POSTS: [&str; 5] = [
    "sync",
    "quality-scan",
    "drive-release",
    "ai-retry",
    "ai-release-pause",
];

/// 各 POST の、形の正しい本文。
fn valid_body(route: &str, token: &str) -> Value {
    match route {
        "drive-release" => json!({ "token": token }),
        "ai-retry" | "ai-release-pause" => json!({ "target": "all" }),
        _ => json!({}),
    }
}

/// 再検査もしない、種類だけを持つ handler（`ai_retry_handlers` に登録のある種類を作る）。
struct Stub(JobKind);

impl Handler for Stub {
    fn kind(&self) -> JobKind {
        self.0
    }
    fn reconcile(&self, _read: &rusqlite::Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        Ok(Vec::new())
    }
    fn run(&self, _ctx: &RunCtx<'_>, _job: &Claimed) -> Result<Persist, RunError> {
        Err(RunError::InputChanged)
    }
    fn recheck(&self, _conn: &rusqlite::Connection, _key: &JobKey) -> rusqlite::Result<Recheck> {
        Ok(Recheck::Requeue)
    }
}

// ================================================================ 同じプロセスの router

struct Server {
    _dir: TempDir,
    /// 試験が DB を直接いじる・見るための接続（書き込み用）。
    db: rusqlite::Connection,
    addr: std::net::SocketAddr,
    board: Arc<Board>,
    mass: Arc<MassDeleteBoard>,
    stop: StopFlag,
    /// `AppState.ai_ops_lock`（試験が握って「別の AI の操作が実行中」を作る）。
    ai_lock: Arc<Mutex<()>>,
}

impl Server {
    /// 掲示板と保留の記憶を持つ `serve`（取り込み有効の形）。
    async fn start() -> Self {
        Self::start_with(true).await
    }

    /// `ingest: false` なら `ops` を入れない（`INGEST=0` の `serve` と同じ）。writer は付けない。
    async fn start_with(ingest: bool) -> Self {
        Self::build(ingest, None, None).await
    }

    /// 掲示板・writer・AI の表（`ai_jobs`・`ai_provider_pauses`）・AI の操作の部品を持つ `serve`。`handlers` は再試行用。
    async fn start_ai(handlers: Vec<Arc<dyn Handler>>) -> Self {
        Self::build(true, Some(handlers), None).await
    }

    /// `start_ai` に、通信しない AI 設定（`ai_json`。`GEOFRAME_AI_CONFIG` のファイル）を足す。再試行が種類ごとに使える。
    async fn start_ai_with_config(handlers: Vec<Arc<dyn Handler>>, ai_json: &str) -> Self {
        Self::build(true, Some(handlers), Some(ai_json)).await
    }

    async fn build(ingest: bool, ai: Option<Vec<Arc<dyn Handler>>>, ai_json: Option<&str>) -> Self {
        let dir = TempDir::new("ops-api");
        let ai_config = ai_json.map(|json| {
            let path = dir.path().join("ai.json");
            fs::write(&path, json).unwrap();
            path.to_str().unwrap().to_owned()
        });
        let env: Vec<(&str, &str)> = ai_config
            .iter()
            .map(|path| ("GEOFRAME_AI_CONFIG", path.as_str()))
            .collect();
        let (path, mut db) = build_empty_catalog(dir.path(), "");
        let mut catalog = Catalog::open(&path).unwrap();
        if ai.is_some() {
            geoframe::ai::migrate(&mut db).unwrap();
            catalog = catalog.with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        }
        let stop = StopFlag::new();
        let board = Arc::new(Board::with_system_clock(stop.clone()));
        let mass = Arc::new(MassDeleteBoard::default());
        let ai_lock = Arc::new(Mutex::new(()));
        let addr = {
            let (board, mass, lock) = (Arc::clone(&board), Arc::clone(&mass), Arc::clone(&ai_lock));
            spawn_state(dir.path(), &env, catalog, move |state| {
                if !ingest {
                    return state;
                }
                let state = state.with_ops(board).with_mass_delete_board(mass);
                match ai {
                    Some(handlers) => {
                        let clock: Clock = Arc::new(|| 1_792_972_800);
                        state.with_ai_ops(handlers, clock, lock)
                    }
                    None => state,
                }
            })
            .await
        };
        Self {
            _dir: dir,
            db,
            addr,
            board,
            mass,
            stop,
            ai_lock,
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
        let response = server.post(route, &valid_body(route, token)).await;
        assert_eq!(response.status, 503, "{route}: {}", response.text());
        assert_eq!(response.json()["ok"], false);
        // 本文の形（400）は掲示板の有無（503）より先
        assert_eq!(server.post_text(route, "not json").await.status, 400);
    }
    for route in ["ai-retry", "ai-release-pause"] {
        assert_eq!(
            server.post(route, &json!({"target": "nope"})).await.status,
            400,
            "{route}"
        );
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
        let response = server.post(route, &valid_body(route, &token)).await;
        assert_eq!(response.status, 503, "{route}: {}", response.text());
    }
    assert!(server.board.snapshot().queued.is_none());
}

#[tokio::test]
async fn after_the_stop_signal_drive_release_is_503_before_any_hold_check() {
    let token = "0123456789abcdef";
    // 保留なし（停止が無ければ 409 no_hold）
    let server = Server::start().await;
    server.stop.stop();
    let response = server
        .post("drive-release", &json!({ "token": token }))
        .await;
    assert_eq!(response.status, 503, "no_hold: {}", response.text());

    // 一覧が空のための保留（停止が無ければ 409 not_releasable）
    let server = Server::start().await;
    server.mass.record_with_ids(
        Some((120, HoldReason::EmptyListing, 120)),
        HeldIds::new((1..=120).collect()),
        "2026-11-02 10:00:00",
    );
    let empty_token = server.hold_token().await;
    server.stop.stop();
    let response = server
        .post("drive-release", &json!({ "token": empty_token }))
        .await;
    assert_eq!(response.status, 503, "not_releasable: {}", response.text());

    // 印の不一致（停止が無ければ 409 hold_changed）
    let server = Server::start().await;
    let current = server.hold_ratio(&[1, 2, 3]).await;
    let first = if current.starts_with('0') { '1' } else { '0' };
    let other = format!("{first}{}", &current[1..]);
    server.stop.stop();
    let response = server
        .post("drive-release", &json!({ "token": other }))
        .await;
    assert_eq!(response.status, 503, "hold_changed: {}", response.text());
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
async fn the_common_entrance_checks_apply_to_all_five_posts() {
    let server = Server::start().await;
    let token = server.hold_ratio(&[1, 2, 3]).await;
    for route in POSTS {
        let ok = valid_body(route, &token).to_string();
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

/// 再検査で SQLite のエラーを返す handler（失敗の文面はパスと表名を含む。応答・履歴へ出てはならない）。
struct FailingRecheck(JobKind);

impl Handler for FailingRecheck {
    fn kind(&self) -> JobKind {
        self.0
    }
    fn reconcile(&self, _read: &rusqlite::Connection, _now: i64) -> rusqlite::Result<Vec<JobKey>> {
        Ok(Vec::new())
    }
    fn run(&self, _ctx: &RunCtx<'_>, _job: &Claimed) -> Result<Persist, RunError> {
        Err(RunError::InputChanged)
    }
    fn recheck(&self, _conn: &rusqlite::Connection, _key: &JobKey) -> rusqlite::Result<Recheck> {
        Err(rusqlite::Error::InvalidParameterName(
            SECRET_TEXT.to_owned(),
        ))
    }
}

/// 通信しない AI 設定（ローカルの `location_clue` と `photo_judge` だけ。LAN の入口は使われない値）。
const LOCAL_AI_JSON: &str = r#"{"version":1,"local_base_url":"http://192.0.2.10:8100",
  "tasks":{"location_clue":{"provider":"local","model":"example-decision"},
           "photo_judge":{"provider":"local","model":"example-judge","service_id":"judge1"}}}"#;

// ---------------------------------------------------------------- AC5: AI の再試行・停止の解除

const AI_ROUTES: [&str; 2] = ["ai-retry", "ai-release-pause"];

impl Server {
    /// `ai_jobs` に 1 行入れる（`input_hash`・版は固定）。
    fn job(&self, kind: &str, target: &str, state: &str) {
        self.db
            .execute(
                "INSERT INTO ai_jobs(kind, target_key, input_hash, processor_version, state, attempts, terminal,
                                     created_at, updated_at)
                 VALUES(?1, ?2, 'h', 'v1', ?3, 5, ?4, 1, 1)",
                rusqlite::params![kind, target, state, i64::from(state == "failed")],
            )
            .unwrap();
    }

    fn pause(&self, scope: &str, subject: &str, task: &str) {
        self.db
            .execute(
                "INSERT INTO ai_provider_pauses(scope, subject, task, reason, created_at)
                 VALUES(?1, ?2, ?3, 'forbidden', 1)",
                rusqlite::params![scope, subject, task],
            )
            .unwrap();
    }

    fn count(&self, sql: &str) -> i64 {
        self.db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// 履歴の先頭（最新）の記録。
    async fn latest(&self) -> Value {
        self.ops().await["history"][0].clone()
    }
}

#[tokio::test]
async fn ai_retry_keeps_failed_jobs_of_unregistered_kinds_and_reports_the_counts() {
    // handler が 1 つも無い（配布バイナリでも AI の設定が無ければこの形）
    let server = Server::start_ai(Vec::new()).await;
    server.job("image_embedding", "a", "failed");
    server.job("image_embedding", "b", "failed");
    server.job("photo_judge", "c", "failed");
    server.job("photo_judge", "d", "pending");
    assert_eq!(server.ops().await["ai"], json!({"retry_kinds": []}));

    let response = server.post("ai-retry", &json!({"target": "all"})).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(
        response.json(),
        json!({
            "ok": true,
            "report": {
                "requeued": 0, "obsolete": 0, "satisfied": 0, "unsupported": 3, "policy_disabled": 0
            }
        })
    );
    // 検査できないものは failed のまま（カウンタも触らない）。待ちのジョブにも触れない
    assert_eq!(
        server.count(
            "SELECT COUNT(*) FROM ai_jobs WHERE state='failed' AND attempts=5 AND terminal=1"
        ),
        3
    );
    assert_eq!(
        server.count("SELECT COUNT(*) FROM ai_jobs WHERE state='pending'"),
        1
    );

    // 種類を指定する
    let response = server
        .post("ai-retry", &json!({"target": {"kind": "photo_judge"}}))
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json()["report"]["unsupported"], 1);

    // 履歴には 2 件（新しい順）。同期ワーカーの待ち・実行中には出ない
    let ops = server.ops().await;
    let history = ops["history"].as_array().unwrap();
    assert_eq!(history.len(), 2, "{ops}");
    assert_eq!(history[0]["kind"], "ai_retry");
    assert_eq!(history[0]["outcome"], "retried");
    assert_eq!(history[0]["report"]["unsupported"], 1);
    assert_eq!(history[1]["report"]["unsupported"], 3);
    assert_eq!(history[0]["stages"], json!([]));
    assert!(history[0]["requested_at"].is_string() && history[0]["finished_at"].is_string());
    assert!(ops["worker"]["queued"].is_null() && ops["worker"]["running"].is_null());
    assert!(server.board.snapshot().queued.is_none());
}

#[tokio::test]
async fn ai_retry_reports_registered_kinds_that_the_settings_do_not_allow_as_policy_disabled() {
    // 登録順に関わらず、`retry_kinds` は JobKind の定義順。AI の設定が無いので、登録済みでも使えない
    let server = Server::start_ai(vec![
        Arc::new(Stub(JobKind::PhotoJudge)),
        Arc::new(Stub(JobKind::ImageEmbedding)),
    ])
    .await;
    assert_eq!(
        server.ops().await["ai"],
        json!({"retry_kinds": ["image_embedding", "photo_judge"]})
    );
    server.job("image_embedding", "a", "failed");
    server.job("location_clue", "b", "failed");
    let response = server.post("ai-retry", &json!({"target": "all"})).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(
        response.json()["report"],
        json!({
            "requeued": 0, "obsolete": 0, "satisfied": 0, "unsupported": 1, "policy_disabled": 1
        })
    );
    assert_eq!(
        server.count("SELECT COUNT(*) FROM ai_jobs WHERE state='failed'"),
        2
    );
}

#[tokio::test]
async fn ai_release_pause_removes_the_pauses_and_records_it() {
    let server = Server::start_ai(Vec::new()).await;
    server.pause("provider", "p1", "image_embedding");
    server.pause("task", "t1", "photo_judge");
    let response = server
        .post("ai-release-pause", &json!({"target": "all"}))
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json(), json!({"ok": true, "released": 2}));
    assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 0);
    let record = server.latest().await;
    assert_eq!(record["kind"], "ai_release_pause");
    assert_eq!(record["outcome"], "released");
    assert_eq!(record["report"], json!({"released": 2}));

    // もう無い。0 件でも成功で、履歴には残る
    let response = server
        .post("ai-release-pause", &json!({"target": "all"}))
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["released"], 0);
    assert_eq!(server.ops().await["history"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_task_release_without_a_usable_config_is_409_config_unavailable_and_removes_nothing() {
    let server = Server::start_ai(Vec::new()).await;
    server.pause("task", "t1", "photo_judge");
    let response = server
        .post(
            "ai-release-pause",
            &json!({"target": {"task": "photo_judge"}}),
        )
        .await;
    assert_eq!(response.status, 409, "{}", response.text());
    assert_eq!(error_of(&response), "config_unavailable");
    assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 1);
    // 何も消していない拒否は履歴に残さない
    assert_eq!(server.ops().await["history"], json!([]));
}

#[tokio::test]
async fn the_target_must_be_all_or_one_known_name_of_the_right_kind() {
    let server = Server::start_ai(Vec::new()).await;
    server.job("photo_judge", "a", "failed");
    server.pause("task", "t1", "photo_judge");
    let bad_retry = [
        json!({}),
        json!({"target": null}),
        json!({"target": "ALL"}),
        json!({"target": "photo_judge"}),
        json!({"target": ["all"]}),
        json!({"target": {}}),
        json!({"target": {"kind": "nope"}}),
        json!({"target": {"kind": 1}}),
        json!({"target": {"kind": null}}),
        // 用途の名前（JobKind に無い）・別の鍵・余計な鍵
        json!({"target": {"kind": "query_embedding"}}),
        json!({"target": {"task": "photo_judge"}}),
        json!({"target": {"kind": "photo_judge", "task": "photo_judge"}}),
    ];
    let bad_release = [
        json!({}),
        json!({"target": null}),
        json!({"target": "everything"}),
        json!({"target": {"task": "nope"}}),
        json!({"target": {"task": 7}}),
        // 種類の名前（TaskKind に無い）・別の鍵・余計な鍵
        json!({"target": {"task": "location_geocode"}}),
        json!({"target": {"kind": "photo_judge"}}),
        json!({"target": {"task": "photo_judge", "kind": "photo_judge"}}),
    ];
    for (route, bodies) in [
        ("ai-retry", &bad_retry[..]),
        ("ai-release-pause", &bad_release[..]),
    ] {
        for body in bodies {
            let response = server.post(route, body).await;
            assert_eq!(response.status, 400, "{route} {body}: {}", response.text());
            assert_eq!(error_of(&response), "bad_params", "{route} {body}");
        }
    }
    // 何も動かしていない
    assert_eq!(
        server.count("SELECT COUNT(*) FROM ai_jobs WHERE state='failed'"),
        1
    );
    assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 1);
    assert_eq!(server.ops().await["history"], json!([]));
}

#[tokio::test]
async fn the_ai_posts_share_the_common_entrance_checks() {
    let server = Server::start_ai(Vec::new()).await;
    server.pause("task", "t1", "photo_judge");
    for route in AI_ROUTES {
        let ok = valid_body(route, "").to_string();
        let response = server
            .post_with(route, &ok, &[("Content-Type", "text/plain")])
            .await;
        assert_eq!(response.status, 403, "{route} content-type");
        let response = server
            .post_with(route, &ok, &[("Origin", "http://evil.example")])
            .await;
        assert_eq!(response.status, 403, "{route} origin");
        let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(64 * 1024 + 1));
        assert_eq!(server.post_text(route, &big).await.status, 413, "{route}");
        for bad in ["not json", "", "[]", "12", "{\"target\":"] {
            assert_eq!(
                server.post_text(route, bad).await.status,
                400,
                "{route}: {bad:?}"
            );
        }
    }
    // 断られた要求は何も動かさない。同じ Origin は通る
    assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 1);
    assert_eq!(server.ops().await["history"], json!([]));
    let response = server
        .post_with(
            "ai-release-pause",
            &valid_body("ai-release-pause", "").to_string(),
            &[("Origin", "http://localhost")],
        )
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
}

#[tokio::test]
async fn without_a_writer_or_after_the_stop_signal_the_ai_posts_are_503_and_change_nothing() {
    // 掲示板はあるが writer が無い
    let server = Server::start().await;
    for route in AI_ROUTES {
        let response = server.post(route, &valid_body(route, "")).await;
        assert_eq!(response.status, 503, "{route}: {}", response.text());
    }
    // writer があっても、停止の合図のあとは 503（行は動かさない）
    let server = Server::start_ai(Vec::new()).await;
    server.job("image_embedding", "a", "failed");
    server.pause("task", "t1", "photo_judge");
    server.stop.stop();
    for route in AI_ROUTES {
        let response = server.post(route, &valid_body(route, "")).await;
        assert_eq!(response.status, 503, "{route}: {}", response.text());
    }
    assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 1);
    assert_eq!(server.ops().await["history"], json!([]));
}

// 鎖を握ったまま POST を待つのが目的。サーバー側は `try_lock` で待たないので、デッドロックしない
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_second_ai_operation_is_409_busy_while_one_holds_the_lock() {
    let server = Server::start_ai(Vec::new()).await;
    server.job("image_embedding", "a", "failed");
    server.pause("task", "t1", "photo_judge");
    {
        // 別の AI の操作が実行中（鎖を握る）。再試行も停止の解除も、種類を問わず 409
        let _held = server.ai_lock.lock().unwrap();
        for route in AI_ROUTES {
            let response = server.post(route, &valid_body(route, "")).await;
            assert_eq!(response.status, 409, "{route}: {}", response.text());
            assert_eq!(error_of(&response), "busy", "{route}");
        }
        assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 1);
        assert_eq!(server.ops().await["history"], json!([]));
    }
    // 鎖が外れれば通る（失敗の記録も、鎖の残りも無い）
    for route in AI_ROUTES {
        let response = server.post(route, &valid_body(route, "")).await;
        assert_eq!(response.status, 200, "{route}: {}", response.text());
    }
}

/// 鎖は `spawn_blocking` の閉包の中で取る。1 つ目の接続が切れても、書き込みが終わるまで鎖は外れない。
#[tokio::test]
async fn the_lock_stays_held_after_the_client_disconnects_until_the_write_finishes() {
    let server = Server::start_ai(Vec::new()).await;
    server.pause("provider", "p1", "image_embedding");
    // 別の接続が書き込みの鎖を握り、writer の依頼を待たせる
    server.db.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let addr = server.addr;
    let body = json!({"target": "all"}).to_string();
    let first = tokio::spawn(async move {
        post_raw(
            addr,
            &format!("{OPS}/ai-release-pause"),
            &[
                ("Host".into(), "localhost".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ],
            body.as_bytes(),
        )
        .await
    });
    // 要求が閉包に入り、writer を待つまで
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.ai_lock.try_lock().is_ok() {
        assert!(Instant::now() < deadline, "鎖が取られない");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 接続を切る
    first.abort();
    let _ = first.await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // 終わるまでは、再試行も停止の解除も busy
    for route in AI_ROUTES {
        let response = server.post(route, &valid_body(route, "")).await;
        assert_eq!(response.status, 409, "{route}: {}", response.text());
        assert_eq!(error_of(&response), "busy", "{route}");
    }
    assert!(server.ai_lock.try_lock().is_err());
    // 書き込みが通ると、切れた接続の分も最後まで走って履歴に残り、鎖が外れる
    server.db.execute_batch("COMMIT;").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.ai_lock.try_lock().is_err() {
        assert!(Instant::now() < deadline, "鎖が外れない");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(server.count("SELECT COUNT(*) FROM ai_provider_pauses"), 0);
    let record = server.latest().await;
    assert_eq!(record["kind"], "ai_release_pause");
    assert_eq!(record["report"], json!({"released": 1}));
    let response = server.post("ai-retry", &valid_body("ai-retry", "")).await;
    assert_eq!(response.status, 200, "{}", response.text());
}

#[tokio::test]
async fn a_failed_write_is_a_fixed_500_and_a_failed_record_without_the_database_text() {
    let server = Server::start_ai(Vec::new()).await;
    server.job("image_embedding", "a", "failed");
    server
        .db
        .execute_batch("DROP TABLE ai_jobs; DROP TABLE ai_provider_pauses;")
        .unwrap();
    for route in AI_ROUTES {
        let response = server.post(route, &valid_body(route, "")).await;
        assert_eq!(response.status, 500, "{route}: {}", response.text());
        let text = response.text().to_owned();
        for needle in [
            "ai_jobs",
            "ai_provider_pauses",
            "no such table",
            "catalog.db",
            "sqlite",
        ] {
            assert!(!text.to_lowercase().contains(needle), "{route}: {text}");
        }
        assert_eq!(response.json()["ok"], false);
    }
    // 履歴には失敗として 2 件（再試行は済んだ分の集計つき。ここでは 0）
    let ops = server.ops().await;
    let history = ops["history"].as_array().unwrap();
    assert_eq!(history.len(), 2, "{ops}");
    assert_eq!(history[0]["kind"], "ai_release_pause");
    assert_eq!(history[0]["outcome"], "failed");
    assert_eq!(history[1]["kind"], "ai_retry");
    assert_eq!(history[1]["outcome"], "failed");
    assert_eq!(history[1]["report"]["requeued"], 0);
    let text = ops.to_string();
    for needle in [
        "ai_jobs",
        "ai_provider_pauses",
        "no such table",
        "catalog.db",
    ] {
        assert!(!text.contains(needle), "{needle}: {text}");
    }
}

#[tokio::test]
async fn a_failure_in_a_later_kind_keeps_the_earlier_kinds_commit_and_records_the_partial_counts() {
    // JobKind の定義順は ImageEmbedding, LocationClue, LocationSuggest, LocationGeocode, PhotoJudge。
    // 先の location_clue は再検査で Requeue（コミット済み）、後の photo_judge の再検査が SQLite のエラーで失敗する
    let server = Server::start_ai_with_config(
        vec![
            Arc::new(Stub(JobKind::LocationClue)),
            Arc::new(FailingRecheck(JobKind::PhotoJudge)),
        ],
        LOCAL_AI_JSON,
    )
    .await;
    assert_eq!(
        server.ops().await["ai"],
        json!({"retry_kinds": ["location_clue", "photo_judge"]})
    );
    server.job("location_clue", "a", "failed");
    server.job("location_clue", "b", "failed");
    server.job("photo_judge", "c", "failed");

    let response = server.post("ai-retry", &json!({"target": "all"})).await;
    assert_eq!(response.status, 500, "{}", response.text());
    assert_eq!(response.json()["ok"], false);
    for needle in ["secret_photos", "catalog.db", "/srv/", "no such table"] {
        assert!(!response.text().contains(needle), "{}", response.text());
    }

    // 先の種類はコミット済み（pending に戻り、カウンタも新しい枠）。後の種類は failed のまま
    assert_eq!(
        server.count(
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='location_clue' AND state='pending'
                AND attempts=0 AND terminal=0"
        ),
        2
    );
    assert_eq!(
        server.count(
            "SELECT COUNT(*) FROM ai_jobs WHERE kind='photo_judge' AND state='failed'
                AND attempts=5 AND terminal=1"
        ),
        1
    );

    // 履歴には失敗として 1 件。済んだ種類までの集計（requeued=2）が残り、失敗の文面は出ない
    let ops = server.ops().await;
    let history = ops["history"].as_array().unwrap();
    assert_eq!(history.len(), 1, "{ops}");
    assert_eq!(history[0]["kind"], "ai_retry");
    assert_eq!(history[0]["outcome"], "failed");
    assert_eq!(history[0]["report"]["requeued"], 2, "{ops}");
    for needle in ["secret_photos", "catalog.db", "/srv/", "no such table"] {
        assert!(!ops.to_string().contains(needle), "{needle}: {ops}");
    }
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

/// AC5（配線）: 本物の `serve` で、AI の再試行・停止の解除が通り、履歴に残る。AI の設定が無いので再試行できる種類は無く、
/// 失敗したジョブも停止も無い（件数は全部 0）。
#[test]
fn the_ai_posts_work_in_a_real_serve_and_land_in_the_history() {
    let dir = TempDir::new("ops-serve-ai");
    let data = dir.path().join("data");
    drop(production_shaped_db(&data));
    let server = Child1::start(dir.path(), &data, &[]);
    server.wait_first_cycle();
    assert_eq!(server.get_ops()["ai"], json!({"retry_kinds": []}));

    let (status, body) = server.post("ai-retry", &json!({"target": "all"}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["report"],
        json!({"requeued": 0, "obsolete": 0, "satisfied": 0, "unsupported": 0, "policy_disabled": 0})
    );
    let (status, body) = server.post("ai-release-pause", &json!({"target": "all"}));
    assert_eq!((status, &body["released"]), (200, &json!(0)), "{body}");
    let (status, body) = server.post(
        "ai-release-pause",
        &json!({"target": {"task": "photo_judge"}}),
    );
    assert_eq!(
        (status, body["error"].as_str()),
        (409, Some("config_unavailable")),
        "{body}"
    );
    let ops = server.get_ops();
    let kinds: Vec<_> = ops["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["kind"].as_str().unwrap(), r["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        kinds,
        [("ai_release_pause", "released"), ("ai_retry", "retried")],
        "{ops}"
    );
}

/// AC5（配線）: 本物の `serve` に、通信しない AI 設定（ローカルの Decision だけ）を渡すと、`serve` が運用タブの再試行用に
/// handler を登録する（`with_ai_ops` が抜けると `retry_kinds` が空のままになる）。失敗したジョブは何も作らないので、
/// 再試行は全て 0 件で通る。
#[test]
fn a_real_serve_registers_the_retry_handlers_of_the_configured_local_tasks() {
    let dir = TempDir::new("ops-serve-ai-handlers");
    let data = dir.path().join("data");
    drop(production_shaped_db(&data));
    let ai = dir.path().join("ai.json");
    fs::write(
        &ai,
        r#"{"version":1,"local_base_url":"http://192.0.2.10:8100",
            "tasks":{"location_clue":{"provider":"local","model":"example-decision"}}}"#,
    )
    .unwrap();
    let server = Child1::start(
        dir.path(),
        &data,
        &[("GEOFRAME_AI_CONFIG", ai.to_str().unwrap())],
    );
    server.wait_first_cycle();
    assert_eq!(
        server.get_ops()["ai"],
        json!({"retry_kinds": ["location_clue"]})
    );

    let (status, body) = server.post("ai-retry", &json!({"target": {"kind": "location_clue"}}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["report"],
        json!({"requeued": 0, "obsolete": 0, "satisfied": 0, "unsupported": 0, "policy_disabled": 0})
    );
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
    // 特徴量の行のうち、写真の表示画像・原本の MD5・mtime と一致するもの（`QUALITY_JOIN` と同じ条件、NULL は `IS`）
    let features = |db: &rusqlite::Connection| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM photos p JOIN photo_quality_features f
               ON f.photo_id=p.id AND f.display=p.display
              AND f.source_md5 IS p.md5 AND f.source_mtime IS p.mtime",
            [],
            |r| r.get(0),
        )
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
