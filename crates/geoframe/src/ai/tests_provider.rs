//! Discovery・Decision・共有の一時待機の試験。スタブの local-model-lab（ループバック）・偽の DNS・合成の画像／DB だけを
//! 使い、実 LAN・外部 AI・クラウドへは出ない。
//!
//! - Discovery: `available` を尊重（false・lease が 0 でない／null・未知の state・矛盾は送信 0）、
//!   `url`／`local_url`／`endpoint` へは接続しない、契約エラーは固定の停止
//! - Decision: health（ready＋image）、systemone の形・上限・状態コードの分類
//! - ワーカー: 入口全体の待ちと、サービス単体の待ちの区別（別 Provider・別サービスは進む）、再起動後も待つ、
//!   永続停止との区別、送信後の失敗の分類（反復／待ち）、停止後の送信 0 と有効な応答の確定、
//!   `before_probe` はクラウド枠を使わない

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::decision::{self, Answer, DecisionRequest, Question, QuestionKind};
use super::http::Deadline;
use super::image_input::{ImageProfile, PreparedImage, fixtures};
use super::jobs::{ErrorCode, InputCheck, JobKind, JobState};
use super::pause::{PauseReason, PauseScope};
use super::provider::{LanClient, ProviderError};
use super::tests_http::{Captured, FakeDns, Stub, lab_http, respond, stub};
use super::tests_settings::variant;
use super::tests_worker::{Env, Fake, env, env_with, key, persist_result};
use super::waits::{WaitReason, WaitScope};
use super::worker::Handler;
use crate::stop::StopFlag;

// ---------------------------------------------------------------- スタブの local-model-lab

type PostHook = Box<dyn Fn(&mut LabState) + Send>;

struct LabState {
    catalog_available: bool,
    state: &'static str,
    svc_available: bool,
    lease: Value,
    health: Value,
    /// 上書き（状態コード＋本文）。
    catalog_raw: Option<(u16, Vec<u8>)>,
    service_raw: Option<(u16, Vec<u8>)>,
    post: (u16, String, Vec<u8>),
    posts: usize,
    /// POST を受けたときの副作用（状態を変える・停止の合図を立てるなど）。
    on_post: Option<PostHook>,
}

struct Lab {
    stub: Stub,
    state: Arc<Mutex<LabState>>,
    /// url／local_url／endpoint が指す罠（ここへの接続は 0 でなければならない）。
    trap: Stub,
    dns: Arc<FakeDns>,
}

fn good_reply() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": "example-decision", "usage": {"input_tokens": 1},
        "answers": {"clue": {"noul": 0.8}, "kind": {"choice": "a", "confidence": 0.9}}
    }))
    .unwrap()
}

fn new_lab() -> Lab {
    let trap = stub(|s, _| respond(s, "200 OK", "", b"{}"));
    let trap_addr = trap.addr;
    let state = Arc::new(Mutex::new(LabState {
        catalog_available: true,
        state: "running",
        svc_available: true,
        lease: json!(0),
        health: json!({"status": "ready", "model": "example-decision", "max_input_tokens": 4096,
                       "modalities": ["text", "image"], "authentication": false}),
        catalog_raw: None,
        service_raw: None,
        post: (200, String::new(), good_reply()),
        posts: 0,
        on_post: None,
    }));
    let shared = Arc::clone(&state);
    let server = stub(move |stream, req: &Captured| {
        let mut st = shared.lock().unwrap();
        let line = req.request_line.as_str();
        let mut json = |status: &str, v: &Value| {
            respond(stream, status, "", &serde_json::to_vec(v).unwrap());
        };
        if line.starts_with("GET /llm/catalog ") {
            if let Some((code, body)) = &st.catalog_raw {
                return respond(stream, &format!("{code} X"), "", body);
            }
            let v = json!({"schema": 1, "revision": "r", "services": [{
                "id": "decision", "title": "t", "description": "d",
                "url": format!("http://{trap_addr}/trap"), "local_url": format!("http://{trap_addr}/trap"),
                "models": [], "limits": [], "reason": "free text",
                "apis": [
                    {"method": "POST", "path": "/v1/systemone", "model": "example-decision"},
                    {"method": "GET", "path": "/v1/models"},
                    {"method": "GET", "path": "/health"}],
                "available": st.catalog_available}]});
            json("200 OK", &v);
        } else if line.starts_with("GET /llm/services/decision ") {
            if let Some((code, body)) = &st.service_raw {
                return respond(stream, &format!("{code} X"), "", body);
            }
            let v = json!({"id": "decision", "state": st.state, "endpoint": format!("{trap_addr}"),
                           "available": st.svc_available, "reason": "free text",
                           "lease_count": st.lease, "generated_at": "2026-01-01T00:00:00Z"});
            json("200 OK", &v);
        } else if line.starts_with("GET /llm-decision/health ") {
            let v = st.health.clone();
            json("200 OK", &v);
        } else if line.starts_with("POST /llm-decision/v1/systemone ") {
            st.posts += 1;
            if let Some(hook) = st.on_post.take() {
                hook(&mut st);
                st.on_post = Some(hook);
            }
            let (code, headers, body) = st.post.clone();
            drop(st);
            respond(stream, &format!("{code} X"), &headers, &body);
        } else {
            respond(stream, "404 Not Found", "", b"{}");
        }
    });
    let dns = FakeDns::new();
    Lab {
        stub: server,
        state,
        trap,
        dns,
    }
}

impl Lab {
    fn client(&self) -> LanClient {
        LanClient::from_http(lab_http(self.stub.addr, &self.dns))
    }

    fn set(&self, f: impl FnOnce(&mut LabState)) {
        f(&mut self.state.lock().unwrap());
    }

    fn posts(&self) -> usize {
        self.state.lock().unwrap().posts
    }

    fn requests(&self) -> usize {
        self.stub.requests.lock().unwrap().len()
    }
}

fn image() -> PreparedImage {
    PreparedImage::from_bytes(fixtures::jpeg(32, 24), ImageProfile::Decision).unwrap()
}

fn questions() -> Vec<Question> {
    vec![
        Question {
            name: "clue".into(),
            instructions: "合成の質問 1".into(),
            kind: QuestionKind::Noul,
        },
        Question {
            name: "kind".into(),
            instructions: "合成の質問 2".into(),
            kind: QuestionKind::Choice(vec![("a".into(), "A".into()), ("b".into(), "B".into())]),
        },
    ]
}

fn request() -> DecisionRequest {
    DecisionRequest::new("example-decision", "合成の状況", questions(), vec![image()]).unwrap()
}

fn session(client: &LanClient) -> super::http::Session {
    client.background(&StopFlag::new()).unwrap()
}

const MODEL: &str = "example-decision";

// ---------------------------------------------------------------- Discovery・health（直接）

#[test]
fn a_ready_lab_is_probed_and_asked_and_the_advertised_urls_are_never_contacted() {
    let lab = new_lab();
    let client = lab.client();
    let s = session(&client);
    decision::probe(&s, MODEL).unwrap();
    assert_eq!(lab.posts(), 0);
    let reply = decision::ask(&s, &request(), &Deadline::after(Duration::from_secs(5))).unwrap();
    assert_eq!(lab.posts(), 1);
    assert_eq!(reply.answers[0], ("clue".to_string(), Answer::Noul(0.8)));
    // catalog の url・local_url と service の endpoint は読むだけで使わない
    assert_eq!(lab.trap.connections.load(Ordering::SeqCst), 0);
    // 要求の形（送ったもの）
    let requests = lab.stub.requests.lock().unwrap();
    let paths: Vec<&str> = requests.iter().map(|r| r.request_line.as_str()).collect();
    assert_eq!(
        paths,
        [
            "GET /llm/catalog HTTP/1.1",
            "GET /llm/services/decision HTTP/1.1",
            "GET /llm-decision/health HTTP/1.1",
            "POST /llm-decision/v1/systemone HTTP/1.1"
        ]
    );
}

#[test]
fn unavailable_states_never_post_and_report_the_right_scope() {
    let lab = new_lab();
    let client = lab.client();
    let entrance = |reason| Err(ProviderError::wait(WaitScope::Entrance, reason));
    let service = |reason| Err(ProviderError::wait(WaitScope::Service, reason));
    let probe = || decision::probe(&session(&client), MODEL);

    lab.set(|l| l.lease = json!(2));
    assert_eq!(probe(), entrance(WaitReason::Leased));
    // lease の変更中（null）。available=true と言っていても送らない
    lab.set(|l| l.lease = Value::Null);
    assert_eq!(probe(), entrance(WaitReason::Leased));
    lab.set(|l| l.lease = json!(0));
    for state in ["starting", "stopping", "stopped", "failed", "unknown"] {
        lab.set(|l| l.state = state);
        assert_eq!(probe(), service(WaitReason::NotReady), "{state}");
    }
    lab.set(|l| l.state = "running");
    // 上流の available=false（収集が古いなど）を自己流に上書きしない
    lab.set(|l| l.svc_available = false);
    assert_eq!(probe(), service(WaitReason::NotReady));
    lab.set(|l| {
        l.svc_available = true;
        l.catalog_available = false;
    });
    assert_eq!(probe(), service(WaitReason::NotReady));
    lab.set(|l| l.catalog_available = true);
    // health が loading／modalities が null は一時的な待ち
    lab.set(|l| l.health = json!({"status": "loading", "model": null, "modalities": null}));
    assert_eq!(probe(), service(WaitReason::NotReady));
    lab.set(|l| l.health = json!({"status": "ready", "model": MODEL, "modalities": null}));
    assert_eq!(probe(), service(WaitReason::NotReady));
    assert_eq!(lab.posts(), 0);
    // 戻れば送れる
    lab.set(|l| {
        l.health = json!({"status": "ready", "model": MODEL, "modalities": ["text", "image"]});
    });
    assert_eq!(probe(), Ok(()));
    assert_eq!(lab.trap.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn contract_violations_are_fixed_errors_and_never_post() {
    let lab = new_lab();
    let client = lab.client();
    let probe = || decision::probe(&session(&client), MODEL);
    let catalog_contract = Err(ProviderError::contract(true));
    let service_contract = Err(ProviderError::contract(false));

    lab.set(|l| l.catalog_raw = Some((200, b"<html>not json</html>".to_vec())));
    assert_eq!(probe(), catalog_contract);
    lab.set(|l| {
        l.catalog_raw = Some((
            200,
            br#"{"services": [{"id": "other", "available": true, "apis": []}]}"#.to_vec(),
        ))
    });
    assert_eq!(
        probe(),
        Err(ProviderError::Pause(
            PauseScope::Task,
            PauseReason::ModelNotFound
        ))
    );
    // 必要な API が catalog に無い
    lab.set(|l| {
        l.catalog_raw = Some((
            200,
            br#"{"services": [{"id": "decision", "available": true, "apis": [{"method": "GET", "path": "/health"}]}]}"#.to_vec(),
        ));
    });
    assert_eq!(probe(), service_contract);
    lab.set(|l| l.catalog_raw = None);
    // service: 未知の state・id の不一致・必須項目の欠落
    lab.set(|l| {
        l.service_raw = Some((
            200,
            br#"{"id":"decision","state":"melting","available":true,"lease_count":0}"#.to_vec(),
        ));
    });
    assert_eq!(probe(), service_contract);
    lab.set(|l| {
        l.service_raw = Some((
            200,
            br#"{"id":"other","state":"running","available":true,"lease_count":0}"#.to_vec(),
        ));
    });
    assert_eq!(probe(), service_contract);
    lab.set(|l| {
        l.service_raw = Some((
            200,
            br#"{"id":"decision","state":"running","available":true}"#.to_vec(),
        ));
    });
    assert_eq!(probe(), service_contract);
    // service の 404 は用途の model_not_found、catalog の 404 は Provider 全体
    lab.set(|l| l.service_raw = Some((404, b"{}".to_vec())));
    assert_eq!(
        probe(),
        Err(ProviderError::Pause(
            PauseScope::Task,
            PauseReason::ModelNotFound
        ))
    );
    lab.set(|l| l.service_raw = Some((503, b"{}".to_vec())));
    assert_eq!(
        probe(),
        Err(ProviderError::wait(WaitScope::Entrance, WaitReason::Busy))
    );
    lab.set(|l| l.service_raw = None);
    lab.set(|l| l.catalog_raw = Some((404, b"{}".to_vec())));
    assert_eq!(probe(), catalog_contract);
    lab.set(|l| l.catalog_raw = Some((401, b"{}".to_vec())));
    assert_eq!(
        probe(),
        Err(ProviderError::Pause(
            PauseScope::Provider,
            PauseReason::Unauthorized
        ))
    );
    lab.set(|l| l.catalog_raw = None);
    // health: text だけ（画像なし）・別のモデル
    lab.set(|l| l.health = json!({"status": "ready", "model": MODEL, "modalities": ["text"]}));
    assert_eq!(probe(), service_contract);
    lab.set(|l| l.health = json!({"status": "ready", "model": "another", "modalities": ["image"]}));
    assert_eq!(
        probe(),
        Err(ProviderError::Pause(
            PauseScope::Task,
            PauseReason::ModelNotFound
        ))
    );
    assert_eq!(lab.posts(), 0);
    assert_eq!(lab.trap.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn post_status_codes_and_bodies_are_classified() {
    let lab = new_lab();
    let client = lab.client();
    let ask = |code: u16, headers: &str, body: &[u8]| {
        lab.set(|l| l.post = (code, headers.to_string(), body.to_vec()));
        decision::ask(
            &session(&client),
            &request(),
            &Deadline::after(Duration::from_secs(5)),
        )
    };
    assert!(ask(200, "", &good_reply()).is_ok());
    assert_eq!(
        ask(413, "", b"{}"),
        Err(ProviderError::Permanent(ErrorCode::InputTooLarge))
    );
    assert_eq!(
        ask(422, "", b"{}"),
        Err(ProviderError::Permanent(ErrorCode::InputInvalid))
    );
    assert_eq!(
        ask(429, "Retry-After: 300\r\n", b"{}"),
        Err(ProviderError::Wait(super::waits::Wait {
            scope: WaitScope::Entrance,
            reason: WaitReason::RateLimited,
            retry_after_secs: Some(300)
        }))
    );
    assert_eq!(
        ask(401, "", b"{}"),
        Err(ProviderError::Pause(
            PauseScope::Provider,
            PauseReason::Unauthorized
        ))
    );
    assert_eq!(
        ask(403, "", b"{}"),
        Err(ProviderError::Pause(
            PauseScope::Provider,
            PauseReason::Forbidden
        ))
    );
    assert_eq!(
        ask(404, "", b"{}"),
        Err(ProviderError::Pause(
            PauseScope::Task,
            PauseReason::ModelNotFound
        ))
    );
    assert_eq!(
        ask(502, "", b"{}"),
        Err(ProviderError::Transient(ErrorCode::ServerError))
    );
    assert_eq!(
        ask(504, "", b"{}"),
        Err(ProviderError::Transient(ErrorCode::ServerError))
    );
    assert_eq!(
        ask(302, "Location: http://10.255.255.1/x\r\n", b""),
        Err(ProviderError::contract(true))
    );
    // 契約に合わない 2xx
    assert_eq!(ask(200, "", b"not json"), Err(ProviderError::BadResponse));
    assert_eq!(
        ask(200, "", br#"{"model":"m","answers":{}}"#),
        Err(ProviderError::BadResponse)
    );
    assert_eq!(lab.trap.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn an_oversized_request_is_a_permanent_error_with_nothing_received() {
    let lab = new_lab();
    let client = lab.client();
    // 8 MiB を超える JSON（画像 4 枚の base64）
    let mut padded = fixtures::png(32, 32, None);
    let iend = padded.split_off(padded.len() - 12);
    let mut chunk = (2_500_000u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"IDAT");
    chunk.extend(std::iter::repeat_n(0u8, 2_500_000));
    chunk.extend_from_slice(&[0; 4]);
    padded.extend(chunk);
    padded.extend(iend);
    let image = PreparedImage::from_bytes(padded, ImageProfile::Decision).unwrap();
    let request = DecisionRequest::new(MODEL, "s", questions(), vec![image; 4]).unwrap();
    assert!(request.to_json().is_err());
    let before = lab.requests();
    assert_eq!(
        decision::ask(
            &session(&client),
            &request,
            &Deadline::after(Duration::from_secs(5))
        ),
        Err(ProviderError::Permanent(ErrorCode::InputTooLarge))
    );
    assert_eq!((lab.requests(), lab.posts()), (before, 0));
}

// ---------------------------------------------------------------- ワーカーとの統合

fn clue_handler(lab: &Lab, request: DecisionRequest) -> Arc<Fake> {
    let client = lab.client();
    Fake::new(JobKind::LocationClue, move |ctx, job| {
        decision::run(ctx, &client, &request)?;
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    })
}

/// 同じ入口の別サービス（`example-embed-svc`）を使う用途。Discovery は呼ばない fake。
fn embed_handler() -> Arc<Fake> {
    Fake::ok(JobKind::ImageEmbedding)
}

/// 別の Provider（クラウド）の用途。
fn judge_handler() -> Arc<Fake> {
    Fake::ok(JobKind::PhotoJudge)
}

fn all_keys(e: &Env, handlers: &[&Arc<Fake>]) {
    for h in handlers {
        let kind = h.kind();
        e.enqueue(&key(kind, "t"));
    }
}

fn defer_count(e: &Env, kind: JobKind) -> i64 {
    let k = kind.as_str();
    e.writer()
        .call(move |c| {
            c.query_row(
                "SELECT defer_count FROM ai_jobs WHERE kind=?1 AND target_key='t'",
                [k],
                |r| r.get(0),
            )
        })
        .unwrap()
}

fn wait_rows(e: &Env) -> Vec<(String, String)> {
    e.writer()
        .call(|c| {
            let mut stmt =
                c.prepare("SELECT service, reason FROM ai_provider_waits ORDER BY service")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .unwrap()
}

fn pause_rows(e: &Env) -> Vec<(String, String)> {
    e.writer()
        .call(|c| {
            let mut stmt =
                c.prepare("SELECT scope, reason FROM ai_provider_pauses ORDER BY scope")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .unwrap()
}

#[test]
fn a_ready_provider_completes_the_job_after_discovery() {
    let e = env("prov-ok");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    e.enqueue(&key(JobKind::LocationClue, "t"));
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Succeeded);
    assert_eq!((clue.runs(), lab.posts()), (1, 1));
    assert_eq!(wait_rows(&e), []);
}

#[test]
fn a_lease_blocks_the_whole_entrance_but_not_another_provider() {
    let e = env("prov-lease");
    let lab = new_lab();
    let (clue, embed, judge) = (
        clue_handler(&lab, request()),
        embed_handler(),
        judge_handler(),
    );
    all_keys(&e, &[&clue, &embed, &judge]);
    lab.set(|l| l.lease = json!(1));
    let stop = StopFlag::new();
    // clue が先に入口全体の待ちを作る（lease）。同じ入口の embed は claim の前に飛ばされ、別 Provider の judge は進む
    e.cycle(
        &[Arc::clone(&clue), Arc::clone(&embed), Arc::clone(&judge)],
        &stop,
    );
    let row = e.row(JobKind::LocationClue, "t").unwrap();
    assert_eq!(row.state, JobState::Deferred);
    assert_eq!(row.code, ErrorCode::Unavailable.as_str());
    assert_eq!(row.attempts, 0, "失敗に数えない");
    assert!(
        row.next >= super::tests_worker::T0 + 60,
        "ジョブの次回も遅らせる"
    );
    assert_eq!(lab.posts(), 0, "lease 中は POST 0");
    assert_eq!(embed.runs(), 0, "同じ入口の他の用途は claim の前に飛ばす");
    assert_ne!(e.state(JobKind::ImageEmbedding, "t"), JobState::Succeeded);
    assert_eq!(judge.runs(), 1, "別 Provider は巻き込まない");
    assert_eq!(e.state(JobKind::PhotoJudge, "t"), JobState::Succeeded);
    assert_eq!(wait_rows(&e).len(), 1);
    assert_eq!(wait_rows(&e)[0].1, "leased");

    // 待ちの間は何度周期が回っても Provider へ連打しない（新しいワーカー＝再起動でも）
    let requests = lab.requests();
    e.advance(30);
    e.cycle(
        &[Arc::clone(&clue), Arc::clone(&embed), Arc::clone(&judge)],
        &stop,
    );
    assert_eq!(lab.requests(), requests);
    assert_eq!((clue.runs(), embed.runs()), (1, 0));

    // lease が戻り、待ちの終わりを過ぎれば、両方進む
    lab.set(|l| l.lease = json!(0));
    e.advance(40);
    e.cycle(
        &[Arc::clone(&clue), Arc::clone(&embed), Arc::clone(&judge)],
        &stop,
    );
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Succeeded);
    assert_eq!(e.state(JobKind::ImageEmbedding, "t"), JobState::Succeeded);
    assert_eq!(lab.posts(), 1);
}

#[test]
fn a_single_service_outage_does_not_stop_the_other_services_on_the_entrance() {
    let e = env("prov-service");
    let lab = new_lab();
    let (clue, embed, judge) = (
        clue_handler(&lab, request()),
        embed_handler(),
        judge_handler(),
    );
    all_keys(&e, &[&clue, &embed, &judge]);
    lab.set(|l| l.state = "stopped");
    e.cycle(
        &[Arc::clone(&clue), Arc::clone(&embed), Arc::clone(&judge)],
        &StopFlag::new(),
    );
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Deferred);
    assert_eq!(lab.posts(), 0);
    // Decision だけが止まっている。同じ入口の別サービスも別 Provider も進む
    assert_eq!(e.state(JobKind::ImageEmbedding, "t"), JobState::Succeeded);
    assert_eq!(e.state(JobKind::PhotoJudge, "t"), JobState::Succeeded);
    let rows = wait_rows(&e);
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].0, "", "サービス単体の待ち（入口全体ではない）");
    assert_eq!(rows[0].1, "not_ready");
    // 戻れば再開
    lab.set(|l| l.state = "running");
    e.advance(61);
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Succeeded);
}

#[test]
fn unreachable_and_rate_limits_wait_for_the_whole_entrance() {
    // 到達不能: 閉じたポート
    let e = env("prov-unreachable");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    let embed = embed_handler();
    all_keys(&e, &[&clue, &embed]);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = closed.local_addr().unwrap();
    drop(closed);
    lab.dns.script("lab.test", vec![vec![dead]]);
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Deferred);
    assert_eq!(e.row(JobKind::LocationClue, "t").unwrap().attempts, 0);
    assert_eq!(embed.runs(), 0);
    assert_eq!(wait_rows(&e)[0].1, "unreachable");

    // 429（Retry-After 300 秒）: その分だけ待つ。上限 3600 秒に収める
    let e = env("prov-429");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    let embed = embed_handler();
    all_keys(&e, &[&clue, &embed]);
    lab.set(|l| l.post = (429, "Retry-After: 300\r\n".to_string(), b"{}".to_vec()));
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    let row = e.row(JobKind::LocationClue, "t").unwrap();
    assert_eq!(row.code, ErrorCode::RateLimited.as_str());
    assert_eq!(row.attempts, 0);
    assert!(row.next >= super::tests_worker::T0 + 300);
    assert_eq!(embed.runs(), 0, "429 は入口全体");
    assert_eq!(wait_rows(&e)[0].1, "rate_limited");
    // 300 秒の途中ではまだ待つ、過ぎれば進む
    e.advance(200);
    e.cycle(&[Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(embed.runs(), 0);
    e.advance(101);
    e.cycle(&[Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(embed.runs(), 1);
}

#[test]
fn permanent_pauses_are_not_confused_with_temporary_waits() {
    for (status, scope, reason) in [
        (401u16, "provider", "unauthorized"),
        (403, "provider", "forbidden"),
        (404, "task", "model_not_found"),
    ] {
        let e = env(&format!("prov-pause-{status}"));
        let lab = new_lab();
        let clue = clue_handler(&lab, request());
        e.enqueue(&key(JobKind::LocationClue, "t"));
        lab.set(|l| l.post = (status, String::new(), b"{}".to_vec()));
        e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
        assert_eq!(
            pause_rows(&e),
            [(scope.to_string(), reason.to_string())],
            "{status}"
        );
        assert_eq!(wait_rows(&e), [], "{status}: 一時待機ではない");
        let row = e.row(JobKind::LocationClue, "t").unwrap();
        assert_eq!((row.state, row.attempts), (JobState::Deferred, 0));
        // 停止中は再起動しても送らない（時間が経っても）
        let posts = lab.posts();
        e.advance(86_400);
        e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
        assert_eq!(lab.posts(), posts);
    }
    // catalog の契約違反は Provider 全体の停止（POST 0）
    let e = env("prov-pause-contract");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    e.enqueue(&key(JobKind::LocationClue, "t"));
    lab.set(|l| l.catalog_raw = Some((200, b"garbage".to_vec())));
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(
        pause_rows(&e),
        [("provider".to_string(), "contract_mismatch".to_string())]
    );
    assert_eq!(lab.posts(), 0);
}

#[test]
fn a_failure_after_sending_is_judged_by_a_second_discovery() {
    // 利用できるのに同じジョブだけが 5xx: そのジョブの反復失敗（Provider は待たない）
    let e = env("prov-repeated");
    let lab = new_lab();
    let (clue, embed) = (clue_handler(&lab, request()), embed_handler());
    all_keys(&e, &[&clue, &embed]);
    lab.set(|l| l.post = (500, String::new(), b"{}".to_vec()));
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    let row = e.row(JobKind::LocationClue, "t").unwrap();
    assert_eq!(row.state, JobState::Deferred);
    assert_eq!(row.code, ErrorCode::ServerError.as_str());
    assert_eq!(defer_count(&e, JobKind::LocationClue), 1);
    assert_eq!(wait_rows(&e), [], "Provider 全体は待たない");
    assert_eq!(
        e.state(JobKind::ImageEmbedding, "t"),
        JobState::Succeeded,
        "他の用途は進む"
    );

    // 送信の直後に lease が始まった（502）: ジョブの不良ではないので失敗に数えず、入口全体を待つ
    let e = env("prov-lease-after-send");
    let lab = new_lab();
    let (clue, embed) = (clue_handler(&lab, request()), embed_handler());
    all_keys(&e, &[&clue, &embed]);
    // POST の応答（502）と同時に lease が始まる
    lab.set(|l| {
        l.post = (502, String::new(), b"{}".to_vec());
        l.on_post = Some(Box::new(|st| st.lease = json!(1)));
    });
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    let row = e.row(JobKind::LocationClue, "t").unwrap();
    assert_eq!(row.attempts, 0);
    assert_eq!(
        defer_count(&e, JobKind::LocationClue),
        0,
        "反復失敗に数えない"
    );
    assert_eq!(row.code, ErrorCode::Unavailable.as_str());
    assert_eq!(embed.runs(), 0, "入口全体の待ち");
}

#[test]
fn permanent_input_errors_fail_only_that_job() {
    let e = env("prov-422");
    let lab = new_lab();
    let (clue, embed) = (clue_handler(&lab, request()), embed_handler());
    all_keys(&e, &[&clue, &embed]);
    lab.set(|l| l.post = (422, String::new(), b"{}".to_vec()));
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Failed);
    assert_eq!(e.state(JobKind::ImageEmbedding, "t"), JobState::Succeeded);
    assert_eq!(wait_rows(&e), []);
    assert_eq!(pause_rows(&e), []);
}

#[test]
fn waits_do_not_touch_succeeded_results_and_survive_a_restart() {
    let e = env("prov-succeeded");
    let lab = new_lab();
    let (clue, embed) = (clue_handler(&lab, request()), embed_handler());
    e.enqueue(&key(JobKind::ImageEmbedding, "t"));
    e.cycle(&[Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(e.state(JobKind::ImageEmbedding, "t"), JobState::Succeeded);
    assert_eq!((embed.runs(), e.results()), (1, 1));
    // 入口全体の待ちが付いても、成功済みは再送も再計算もされない
    e.enqueue(&key(JobKind::LocationClue, "t"));
    lab.set(|l| l.lease = json!(3));
    e.cycle(&[Arc::clone(&clue), Arc::clone(&embed)], &StopFlag::new());
    assert_eq!(e.state(JobKind::ImageEmbedding, "t"), JobState::Succeeded);
    assert_eq!((embed.runs(), e.results()), (1, 1));
    // 待ちは DB に残る。新しいワーカーでも期限までは送らない
    let requests = lab.requests();
    e.cycle(&[Arc::clone(&clue)], &StopFlag::new());
    assert_eq!(lab.requests(), requests);
}

#[test]
fn stop_prevents_discovery_and_posts_but_a_valid_inflight_answer_is_committed() {
    // 停止後は Discovery も POST もしない
    let e = env("prov-stop-before");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    e.enqueue(&key(JobKind::LocationClue, "t"));
    let stop = StopFlag::new();
    stop.stop();
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert_eq!((lab.requests(), clue.runs()), (0, 0));

    // 送信済みの要求の応答を待っている間に停止が来ても、有効な応答は確定される
    let e = env("prov-stop-inflight");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    e.enqueue(&key(JobKind::LocationClue, "t"));
    let stop = StopFlag::new();
    let hook_stop = stop.clone();
    lab.set(|l| l.on_post = Some(Box::new(move |_| hook_stop.stop())));
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert!(stop.is_stopped());
    assert_eq!(lab.posts(), 1);
    assert_eq!(e.state(JobKind::LocationClue, "t"), JobState::Succeeded);
    assert_eq!(e.results(), 1);
}

/// 名前解決の最中に停止が来ても、要求は出ず、待ち・停止（pause）・失敗の試行回数のどれにも数えない
/// （停止は固定の Shutdown の後回しになるだけ）。旧実装は停止後に Discovery の GET を出した。
#[test]
fn a_stop_during_name_resolution_is_not_counted_as_a_wait_pause_or_attempt() {
    let e = env("prov-stop-dns");
    let lab = new_lab();
    let clue = clue_handler(&lab, request());
    e.enqueue(&key(JobKind::LocationClue, "t"));
    let stop = StopFlag::new();
    let hook_stop = stop.clone();
    lab.dns.on_lookup(move || hook_stop.stop());
    e.cycle(&[Arc::clone(&clue)], &stop);
    assert!(stop.is_stopped());
    assert_eq!((clue.runs(), lab.requests(), lab.posts()), (1, 0, 0));
    assert_eq!(lab.stub.bytes.load(Ordering::SeqCst), 0);
    let row = e.row(JobKind::LocationClue, "t").unwrap();
    assert_ne!(row.state, JobState::Failed);
    assert_eq!((row.attempts, row.interruptions), (0, 0));
    assert_eq!(wait_rows(&e), []);
    assert_eq!(pause_rows(&e), []);
}

/// 画素データのあとにメタデータを足した JPEG は準備済み入力にならず、Decision のスタブには何も届かない。
#[test]
fn an_image_with_trailing_metadata_never_reaches_the_lab() {
    let lab = new_lab();
    let client = lab.client();
    let _ = &client;
    let base = fixtures::jpeg(32, 24);
    for tail in [
        fixtures::segment(0xE1, b"Exif\0\0II*\0"),
        fixtures::segment(0xFE, b"hidden"),
    ] {
        let bad = fixtures::before_eoi(base.clone(), &tail);
        assert_eq!(
            PreparedImage::from_bytes(bad, ImageProfile::Decision).unwrap_err(),
            super::image_input::ImageError::HasMetadata
        );
    }
    // EOI の後ろに足したデータ
    let mut after = base.clone();
    after.extend_from_slice(b"trailing");
    assert_eq!(
        PreparedImage::from_bytes(after, ImageProfile::Decision).unwrap_err(),
        super::image_input::ImageError::Malformed
    );
    // 切れた JPEG（SOS の直後）も同じ
    let sos = fixtures::marker_positions(&base, 0xDA)[0];
    let cut =
        base[..sos + 2 + usize::from(u16::from_be_bytes([base[sos + 2], base[sos + 3]]))].to_vec();
    assert!(PreparedImage::from_bytes(cut, ImageProfile::Decision).is_err());
    assert_eq!(
        (
            lab.requests(),
            lab.stub.connections.load(Ordering::SeqCst),
            lab.stub.bytes.load(Ordering::SeqCst)
        ),
        (0, 0, 0)
    );
}

#[test]
fn probing_never_reserves_the_cloud_budget_but_sending_does() {
    let e = env("prov-budget");
    // クラウドの用途（photo_judge）。Discovery 相当の before_probe を何度呼んでも枠は減らない
    let probe_only = Fake::new(JobKind::PhotoJudge, |ctx, job| {
        for _ in 0..3 {
            ctx.before_probe()?;
        }
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    });
    let usage = |e: &Env| -> i64 {
        e.writer()
            .call(|c| c.query_row("SELECT COUNT(*) FROM ai_request_usage", [], |r| r.get(0)))
            .unwrap()
    };
    e.enqueue(&key(JobKind::PhotoJudge, "t"));
    e.cycle(&[Arc::clone(&probe_only)], &StopFlag::new());
    assert_eq!(e.state(JobKind::PhotoJudge, "t"), JobState::Succeeded);
    assert_eq!(usage(&e), 0, "before_probe は送信枠を使わない");

    // 送信の直前の確認（before_send）だけが枠を予約する
    let e = env("prov-budget-send");
    let send = Fake::new(JobKind::PhotoJudge, |ctx, job| {
        ctx.before_probe()?;
        ctx.before_send()?;
        Ok(persist_result(
            job.key.target_key.clone(),
            InputCheck::Current,
        ))
    });
    e.enqueue(&key(JobKind::PhotoJudge, "t"));
    e.cycle(&[Arc::clone(&send)], &StopFlag::new());
    assert_eq!(e.state(JobKind::PhotoJudge, "t"), JobState::Succeeded);
    assert!(usage(&e) > 0);
}

#[test]
fn the_config_with_a_dns_name_builds_a_lan_client_and_other_configs_do_not() {
    // 本番の作り方（設定の local_base_url から）。DNS 名・IP のどちらも作れる
    let e = env("prov-client");
    assert!(LanClient::new(&e.config.ai).is_some());
    let named = env_with(
        "prov-client-name",
        &variant("http://192.0.2.10:8100", "http://lab.example:8100"),
        true,
    );
    assert!(LanClient::new(&named.config.ai).is_some());
    // 設定が不正・LAN の用途が無いなら作れない
    assert!(LanClient::new(&env_with("prov-client-bad", "{", true).config.ai).is_none());
}
