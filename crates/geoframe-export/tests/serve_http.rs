//! T6: `serve` の HTTP（design.md 3.4・6.1・8 章。AC7・AC8・AC9・AC10 の HTTP 側）。`serve::run` を、プロセスの中で 127.0.0.1 の
//! 空きポートに起こす（偽の写真サービス・偽の rclone・偽の Drive だけ。本物の写真サービス 127.0.0.1:8080・rclone・Drive には
//! 触れない）。signal の試験（本物のバイナリ）は serve_cli.rs。
//!
//! 同じプロセスの中で、ほかの試験が偽の rclone を fork している間は、実行枠が一瞬だけ空かない（common/site.rs の「実行枠の、ほかの
//! 試験の fork との相互作用への備え」）。受付を期待するところだけ、`post_retry` で `busy` を少し待つ。

mod common;

use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::drive::*;
use common::http::*;
use common::site::*;
use common::{FakeSite, Reply};
use geoframe_export::cancel::Cancel;
use geoframe_export::logging;
use geoframe_export::serve::{self, Config, Limits, Token};
use geoframe_export::service::Service;
use serde_json::{Value, json};
use tracing_subscriber::fmt::MakeWriter;

const DAY: &str = "2030-03-10";

// ---- ログを受ける（この試験のプロセス全体の購読者。サーバーのスレッドにも届く） ----

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

fn logs() -> String {
    static BUF: OnceLock<LogBuf> = OnceLock::new();
    let buf = BUF.get_or_init(|| {
        let buf = LogBuf::default();
        tracing::subscriber::set_global_default(logging::subscriber(buf.clone()))
            .expect("この試験のプロセスで 1 回だけ入れる");
        buf
    });
    String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned()
}

/// 購読者を先に入れる（ログを読む試験より前のサーバーのログも拾うため）。
fn init_logs() {
    let _ = logs();
}

// ---- サーバー ----

struct Server {
    addr: SocketAddr,
    cancel: Cancel,
    thread: Option<JoinHandle<std::io::Result<u8>>>,
}

impl Server {
    /// 127.0.0.1 の相手だけを許可して起こす。
    fn start(env: &mut Env, site: &FakeSite, token: &str) -> Self {
        Self::start_with(
            env,
            site,
            token,
            vec!["127.0.0.1".parse().unwrap()],
            Limits::default(),
        )
    }

    fn start_with(
        env: &mut Env,
        site: &FakeSite,
        token: &str,
        allow: Vec<IpAddr>,
        limits: Limits,
    ) -> Self {
        init_logs();
        env.settings.feed_base = site.base.clone();
        let cancel = Cancel::new();
        let service = Service::with_cancel(env.settings.clone(), fast_tuning(), cancel.clone())
            .expect("service");
        let config = Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            allow,
            token: Token::parse(token.as_bytes()).expect("試験のトークン"),
            limits,
            handle_signals: false,
        };
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            serve::run(service, config, |addr| {
                let _ = tx.send(addr);
            })
        });
        let addr = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("待ち受けを始めない");
        Self {
            addr,
            cancel,
            thread: Some(thread),
        }
    }

    /// 終了の印を立てて、終わるのを待つ。終了コードを返す。
    fn stop(mut self) -> u8 {
        self.cancel.cancel();
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("serve のスレッド")
            .expect("serve")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn day_body(extra: Value) -> Value {
    let mut v = json!({"from": DAY, "destination": "yamareco"});
    for (k, val) in extra.as_object().cloned().unwrap_or_default() {
        v[k] = val;
    }
    v
}

/// 受付を期待する POST。実行枠の見かけの `busy`（上の説明）だけ、少し待って打ち直す。
fn post_retry(addr: SocketAddr, token: &str, body: &Value) -> Resp {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let r = post_json(addr, token, body);
        if r.status == 409 && r.error() == "busy" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        return r;
    }
}

/// `running` でなくなるまで GET で待つ。
fn poll_done(addr: SocketAddr, token: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let r = get(addr, &format!("/v1/exports/{job_id}"), token);
        assert_eq!(r.status, 200, "{}", r.body);
        let v = r.json();
        if v["status"] != "running" {
            return v;
        }
        assert!(Instant::now() < deadline, "ジョブが終わりません: {v}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn three() -> Vec<Value> {
    vec![
        row(1, "2030-03-10 09:00"),
        row(2, "2030-03-10 09:01"),
        row(3, "2030-03-10 10:30"),
    ]
}

// ---- AC7: 認証と送信元 ----

#[test]
fn the_token_is_required_everywhere_except_healthz_and_never_shows_up() {
    const TOKEN: &str = "AC7-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    const WRONG: &str = "AC7-Wrong-Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya8q";
    let mut env = Env::new("http-auth");
    let site = photo_site(DAY, DAY, three());
    let server = Server::start(&mut env, &site, TOKEN);
    let addr = server.addr;
    // 応答（状態行以外のヘッダと本文）に、トークンも、送られてきた誤ったトークンも、その断片も出ない
    let clean = |r: &Resp| {
        for secret in [TOKEN, WRONG, &TOKEN[..20], &WRONG[..20]] {
            assert!(!r.body.contains(secret), "本文にトークンが出ている: {r:?}");
            for (name, value) in &r.headers {
                assert!(
                    !name.contains(secret) && !value.contains(secret),
                    "ヘッダにトークンが出ている: {r:?}"
                );
            }
        }
    };

    // /healthz は認証なし
    let r = http_request(addr, "GET", "/healthz", &[], None);
    assert_eq!((r.status, r.body.as_str()), (200, "ok"));
    clean(&r);

    // トークンなし・違う・形の違う・2 つ → 401（経路の有無によらず）
    let wrong = format!("Bearer {WRONG}");
    let basic = format!("Basic {TOKEN}");
    let bare = TOKEN.to_owned();
    let truncated = format!("Bearer {}", &TOKEN[..TOKEN.len() - 1]);
    let longer = format!("Bearer {TOKEN}x");
    for (method, path, headers) in [
        ("GET", "/v1/exports", vec![]),
        (
            "GET",
            "/v1/exports",
            vec![("Authorization", wrong.as_str())],
        ),
        (
            "GET",
            "/v1/exports",
            vec![("Authorization", basic.as_str())],
        ),
        ("GET", "/v1/exports", vec![("Authorization", bare.as_str())]),
        (
            "GET",
            "/v1/exports",
            vec![("Authorization", truncated.as_str())],
        ),
        (
            "GET",
            "/v1/exports",
            vec![("Authorization", longer.as_str())],
        ),
        ("GET", "/v1/exports", vec![("Authorization", "")]),
        (
            "GET",
            "/v1/exports/00112233445566778899aabbccddeeff",
            vec![],
        ),
        (
            "POST",
            "/v1/exports",
            vec![("Content-Type", "application/json")],
        ),
        ("GET", "/no/such/path", vec![]),
        ("DELETE", "/v1/exports", vec![]),
        ("GET", "/healthz/", vec![]),
    ] {
        let body = (method == "POST").then_some(&b"{}"[..]);
        let r = http_request(addr, method, path, &headers, body);
        assert_eq!(r.status, 401, "{method} {path} {headers:?}: {}", r.body);
        assert_eq!(r.error(), "unauthorized");
        assert_eq!(r.header("www-authenticate"), Some("Bearer"));
        clean(&r);
    }
    // 2 つの Authorization ヘッダは、片方が正しくても断る
    let good = format!("Bearer {TOKEN}");
    let r = http_request(
        addr,
        "GET",
        "/v1/exports",
        &[("Authorization", &good), ("Authorization", &wrong)],
        None,
    );
    assert_eq!(r.status, 401);
    // 何も作られていない
    assert_eq!(job_files(&env), 0);

    // 正しいトークン: 一覧は 200、未知の経路は 404、違うメソッドは 405（どれも JSON）
    let r = get(addr, "/v1/exports", TOKEN);
    assert_eq!((r.status, r.json()), (200, json!({"jobs": []})));
    clean(&r);
    let r = get(addr, "/no/such/path", TOKEN);
    assert_eq!(r.status, 404);
    clean(&r);
    let r = http_request(
        addr,
        "DELETE",
        "/v1/exports",
        &[("Authorization", &good)],
        None,
    );
    assert_eq!((r.status, r.error().as_str()), (405, "method_not_allowed"));
    clean(&r);
    // スキームは大文字小文字を区別しない
    let lower = format!("bearer {TOKEN}");
    let r = http_request(
        addr,
        "GET",
        "/v1/exports",
        &[("Authorization", &lower)],
        None,
    );
    assert_eq!(r.status, 200);

    // 許可した送信元から正しいトークンで送れば受け付ける。`X-Forwarded-For` は見ない
    let r = post_retry(addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["status"], "running");
    assert_eq!(r.json()["job_id"].as_str().map(str::len), Some(32));
    clean(&r);
    // 受け付けた job の記録にも出ない
    let id = r.json()["job_id"].as_str().unwrap().to_owned();
    clean(&get(addr, &format!("/v1/exports/{id}"), TOKEN));
    clean(&post_json(addr, WRONG, &day_body(json!({}))));

    // ログにも出ない（試験用の購読者。要求のヘッダも出ない）。応答は上の `clean` で確かめた
    drop(server);
    let log = logs();
    for secret in [TOKEN, WRONG, &TOKEN[..20], &WRONG[..20]] {
        assert!(!log.contains(secret), "ログにトークンが出ている: {log}");
    }
    assert!(!log.to_lowercase().contains("bearer"), "{log}");
    assert!(!log.to_lowercase().contains("authorization"), "{log}");
    assert!(log.contains("status=401"), "{log}");
    assert!(log.contains("status=202"), "{log}");
    // 経路は型だけ（ジョブ ID は形が合うときだけ）
    assert!(log.contains("path=/v1/exports"), "{log}");
    assert!(!log.contains("/no/such/path"), "{log}");
}

#[test]
fn a_source_outside_the_allow_list_is_refused_before_anything_else() {
    const TOKEN: &str = "AC7-Src-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-source");
    let site = photo_site(DAY, DAY, three());
    // 許可リストには別の IP（文書用の予約アドレス）だけ。この試験の接続は 127.0.0.1 から来る
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["192.0.2.1".parse().unwrap(), "2001:db8::1".parse().unwrap()],
        Limits::default(),
    );
    let addr = server.addr;
    let auth = format!("Bearer {TOKEN}");
    let r = http_request(addr, "GET", "/healthz", &[], None);
    assert_eq!((r.status, r.error().as_str()), (403, "forbidden"));
    for (method, path) in [
        ("GET", "/v1/exports"),
        ("GET", "/no/such/path"),
        ("DELETE", "/v1/exports"),
    ] {
        // トークンが正しくても・無くても 403（認証より前）
        for headers in [vec![], vec![("Authorization", auth.as_str())]] {
            let r = http_request(addr, method, path, &headers, None);
            assert_eq!(r.status, 403, "{method} {path}");
        }
    }
    // ヘッダで送信元を名乗っても通らない（ソケットの相手だけを見る）
    let r = http_request(
        addr,
        "GET",
        "/healthz",
        &[
            ("X-Forwarded-For", "192.0.2.1"),
            ("Forwarded", "for=192.0.2.1"),
            ("X-Real-IP", "192.0.2.1"),
        ],
        None,
    );
    assert_eq!(r.status, 403);
    let r = post_json(addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 403);
    assert_eq!(job_files(&env), 0, "許可外の要求では何も始まらない");
    assert!(env.calls().is_empty());
    drop(server);

    // 許可リストの外にいる相手が `X-Forwarded-For` で許可された送信元を名乗っても通らない、の裏返し:
    // 許可された送信元は、ヘッダが何でも通る
    let mut env = Env::new("http-source-ok");
    let server = Server::start(&mut env, &site, TOKEN);
    let r = http_request(
        server.addr,
        "GET",
        "/healthz",
        &[("X-Forwarded-For", "192.0.2.99")],
        None,
    );
    assert_eq!(r.status, 200);
}

#[test]
fn an_ipv4_mapped_entry_in_the_allow_list_matches_the_ipv4_peer() {
    const TOKEN: &str = "AC7-Map-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-mapped");
    let site = photo_site(DAY, DAY, three());
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["::ffff:127.0.0.1".parse().unwrap()],
        Limits::default(),
    );
    assert_eq!(
        http_request(server.addr, "GET", "/healthz", &[], None).status,
        200
    );
}

/// トークンは小文字 16 進 32 文字にもできる（ジョブ ID と同じ形）。パス・クエリ・メソッド名のどこに入っても、ログには出ない。
#[test]
fn a_token_shaped_like_a_job_id_never_reaches_the_access_log() {
    // 架空の値（ジョブ ID と同じ形。異なる文字が 8 種類以上）
    const TOKEN: &str = "3f9a0c7e1b5d842a6e0f1c3b7a9d5e24";
    const OTHER: &str = "c41e8d27b05a93f6e2d8a1b7405c9e3f";
    let mut env = Env::new("http-log-token");
    let site = photo_site(DAY, DAY, three());
    let server = Server::start(&mut env, &site, TOKEN);
    let addr = server.addr;
    let auth = format!("Bearer {TOKEN}");
    let wrong = format!("Bearer {OTHER}");

    // パスの値（認証あり・なし・違う）、クエリ、メソッド名（拡張メソッド）に、トークンと別の値を入れる
    for headers in [
        vec![("Authorization", auth.as_str())],
        vec![("Authorization", wrong.as_str())],
        vec![],
    ] {
        for secret in [TOKEN, OTHER] {
            for (method, path) in [
                ("GET", format!("/v1/exports/{secret}")),
                ("GET", format!("/v1/exports?token={secret}")),
                ("GET", format!("/v1/exports/{secret}?access_token={secret}")),
                ("GET", format!("/{secret}")),
                (secret, "/v1/exports".to_owned()),
                (secret, format!("/v1/exports/{secret}")),
            ] {
                let _ = http_request(addr, method, &path, &headers, None);
            }
        }
    }
    // 正しい経路の 1 件ずつ（見つかったジョブ ID は出る。見つからなかった値は出ない）
    let r = post_retry(addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 202, "{}", r.body);
    let job_id = r.json()["job_id"].as_str().unwrap().to_owned();
    assert_eq!(
        get(addr, &format!("/v1/exports/{job_id}"), TOKEN).status,
        200
    );
    assert_eq!(
        get(addr, &format!("/v1/exports/{TOKEN}"), TOKEN).status,
        404
    );
    drop(server);

    let log = logs();
    for secret in [TOKEN, OTHER, &TOKEN[..16], &OTHER[..16]] {
        assert!(!log.contains(secret), "ログに要求の値が出ている: {log}");
    }
    // メソッドは既知のものだけ、経路はテンプレート
    assert!(log.contains("method=other"), "{log}");
    assert!(log.contains("method=GET"), "{log}");
    assert!(log.contains("method=POST"), "{log}");
    assert!(log.contains("path=/v1/exports/{job_id}"), "{log}");
    assert!(log.contains("path=other"), "{log}");
    // 受付で作った ID と、照会で見つかった記録の ID は出る
    assert!(
        log.lines()
            .filter(|l| l.contains(&format!("job_id={job_id}")))
            .count()
            >= 2,
        "{log}"
    );
    assert!(log.contains("job_id=-"), "{log}");
}

// ---- AC8・AC9: 実行中の 2 件目と、request_id ----

#[test]
fn while_a_job_runs_a_second_request_and_a_dry_run_get_409_and_the_same_request_id_gets_the_same_job()
 {
    const TOKEN: &str = "AC8-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-busy");
    let site = photo_site(DAY, DAY, three());
    // 写真を送る copy を 30 秒止める（ジョブが実行中のまま）
    env.add_rule("upload", 0, 1, sleep_ms(30_000));
    let server = Server::start(&mut env, &site, TOKEN);
    let addr = server.addr;

    let first = post_retry(addr, TOKEN, &day_body(json!({"request_id": "rid-busy"})));
    assert_eq!(first.status, 202, "{}", first.body);
    let job_id = first.json()["job_id"].as_str().unwrap().to_owned();
    let copy_pid = wait_for_call(&env, "upload", 30);
    assert!(process_exists(copy_pid));

    // GET: 実行中
    let v = get(addr, &format!("/v1/exports/{job_id}"), TOKEN).json();
    assert_eq!(v["status"], "running", "{v}");
    assert_eq!(v["job_id"], job_id.as_str());
    assert_eq!(v["error"], Value::Null);

    // 2 件目（別の要求）は 409 busy。dry_run も 409
    let second = post_json(addr, TOKEN, &day_body(json!({"to": "2030-03-11"})));
    assert_eq!((second.status, second.error().as_str()), (409, "busy"));
    let dry = post_json(addr, TOKEN, &day_body(json!({"dry_run": true})));
    assert_eq!((dry.status, dry.error().as_str()), (409, "busy"));

    // 同じ request_id の再送は、実行中でも同じジョブ（202）。中身が違えば 409 request_id_mismatch
    let again = post_json(addr, TOKEN, &day_body(json!({"request_id": "rid-busy"})));
    assert_eq!(again.status, 202, "{}", again.body);
    assert_eq!(again.json(), json!({"job_id": job_id, "status": "running"}));
    let other = post_json(
        addr,
        TOKEN,
        &day_body(json!({"request_id": "rid-busy", "on_exists": "replace"})),
    );
    assert_eq!(
        (other.status, other.error().as_str()),
        (409, "request_id_mismatch")
    );
    // ジョブは 1 つだけ
    assert_eq!(job_files(&env), 1);
    let list = get(addr, "/v1/exports", TOKEN).json();
    assert_eq!(list["jobs"].as_array().map(Vec::len), Some(1), "{list}");

    // 終わらせる: 印を立てると、新しい受付を閉じ、rclone を止めて待ってから終わる（記録は running のまま次の復旧へ）
    let started = Instant::now();
    assert_eq!(server.stop(), 0);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "止まるのが遅い"
    );
    assert!(
        !process_exists(copy_pid),
        "終わったのに rclone が残っている"
    );
    assert!(slot_is_free(&env.state));
    let store = geoframe_export::jobs::JobStore::open(&env.state).unwrap();
    assert_eq!(
        store.scan().records[0].status,
        geoframe_export::jobs::Status::Running
    );
}

// ---- AC9: 202 → succeeded / failed / conflict、request_id、再起動 ----

#[test]
fn a_job_goes_from_202_to_succeeded_and_can_still_be_read_after_a_restart() {
    const TOKEN: &str = "AC9-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-lifecycle");
    let site = photo_site(DAY, DAY, three());
    let server = Server::start(&mut env, &site, TOKEN);
    let addr = server.addr;

    let r = post_retry(addr, TOKEN, &day_body(json!({"request_id": "rid-1"})));
    assert_eq!(r.status, 202, "{}", r.body);
    let job_id = r.json()["job_id"].as_str().unwrap().to_owned();
    // 最初の GET で完了が返ってもよい
    let done = poll_done(addr, TOKEN, &job_id);
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["result"]["count"], 3, "{done}");
    assert_eq!(done["result"]["folder"], DAY);
    assert_eq!(done["request"]["destination"], "yamareco");
    assert_eq!(done["error"], Value::Null);

    // 終わったあとの再送は、新しいジョブを作らずに同じジョブ（今の status）
    let again = post_json(addr, TOKEN, &day_body(json!({"request_id": "rid-1"})));
    assert_eq!(again.status, 202);
    assert_eq!(
        again.json(),
        json!({"job_id": job_id, "status": "succeeded"})
    );

    // 同じ日付をもう一度（既定の on_exists=error）→ 宛先が既にあるので conflict
    let r = post_retry(addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 202, "{}", r.body);
    let second = r.json()["job_id"].as_str().unwrap().to_owned();
    assert_ne!(second, job_id);
    let v = poll_done(addr, TOKEN, &second);
    assert_eq!(v["status"], "conflict", "{v}");
    assert_eq!(v["result"]["conflict"], "exists", "{v}");

    // 一覧: 新しいほうが先
    let list = get(addr, "/v1/exports", TOKEN).json();
    let ids: Vec<&str> = list["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["job_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [second.as_str(), job_id.as_str()], "{list}");
    assert!(
        list["jobs"][0].get("result").is_none(),
        "一覧に結果は載せない"
    );

    // 無い ID・形の違う ID は 404
    for path in [
        "/v1/exports/ffffffffffffffffffffffffffffffff",
        "/v1/exports/not-a-job-id",
        "/v1/exports/..%2f..%2fetc",
        "/v1/exports/00112233445566778899AABBCCDDEEFF",
    ] {
        let r = get(addr, path, TOKEN);
        assert_eq!((r.status, r.error().as_str()), (404, "not_found"), "{path}");
    }

    // serve を止めて起こし直しても、ジョブ ID で引ける。request_id の再送も同じジョブ
    assert_eq!(server.stop(), 0);
    let server = Server::start(&mut env, &site, TOKEN);
    let after = get(server.addr, &format!("/v1/exports/{job_id}"), TOKEN);
    assert_eq!(after.status, 200, "{}", after.body);
    assert_eq!(after.json(), done);
    let again = post_json(
        server.addr,
        TOKEN,
        &day_body(json!({"request_id": "rid-1"})),
    );
    assert_eq!(
        again.json(),
        json!({"job_id": job_id, "status": "succeeded"})
    );
}

#[test]
fn a_failed_job_reports_a_fixed_code_and_never_the_rclone_output() {
    const TOKEN: &str = "AC9-Fail-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-failed");
    // 一覧 API だけで、画像は 404 → 取得に失敗して公開しない
    let site = site_without_images(DAY, DAY, three());
    let server = Server::start(&mut env, &site, TOKEN);
    let r = post_retry(server.addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 202, "{}", r.body);
    let id = r.json()["job_id"].as_str().unwrap().to_owned();
    let v = poll_done(server.addr, TOKEN, &id);
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["error"]["code"].is_string(), "{v}");
    assert!(v["error"]["message"].is_string(), "{v}");
    assert!(!v.to_string().contains("fake failure"));
}

// ---- 503: blocked と、dry_run で写真サービスに届かない ----

#[test]
fn a_blocked_record_and_an_unreachable_photo_service_answer_503_with_a_fixed_code() {
    use geoframe_export::jobid::JobId;
    use geoframe_export::jobs::{BlockedNote, JobRecord, JobStore, Status};
    const TOKEN: &str = "S503-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";

    // dry_run で、写真サービスが一覧 API を返さない（登録のない経路は 404）→ 503 feed_unavailable
    let mut env = Env::new("http-503-feed");
    let dead = FakeSite::start(Vec::new());
    let server = Server::start(&mut env, &dead, TOKEN);
    let r = post_json(server.addr, TOKEN, &day_body(json!({"dry_run": true})));
    assert_eq!(
        (r.status, r.error().as_str()),
        (503, "feed_unavailable"),
        "{}",
        r.body
    );
    assert_eq!(r.json().as_object().map(|o| o.len()), Some(1));
    drop(server);

    // blocked の記録がある間は、受付も dry_run も 503 blocked（解除するまで）。記録そのものは GET で引ける
    let mut env = Env::new("http-503-blocked");
    let site = photo_site(DAY, DAY, three());
    let store = JobStore::open(&env.state).unwrap();
    let mut b = JobRecord::new(JobId::parse(&format!("{:032x}", 8)).unwrap(), &day());
    settle(&mut b, Status::Blocked);
    b.started_ns = 1;
    b.blocked = Some(BlockedNote {
        reason: "unobservable".into(),
        description: "d".into(),
    });
    store.write(&b).unwrap();
    let server = Server::start(&mut env, &site, TOKEN);
    for body in [day_body(json!({})), day_body(json!({"dry_run": true}))] {
        // 実行枠の見かけの `busy`（ほかの試験の fork。冒頭の説明）は、少し待って打ち直す
        let r = post_retry(server.addr, TOKEN, &body);
        assert_eq!((r.status, r.error().as_str()), (503, "blocked"), "{body}");
    }
    let v = get(server.addr, &format!("/v1/exports/{:032x}", 8), TOKEN).json();
    assert_eq!(v["status"], "blocked", "{v}");
    assert_eq!(job_files(&env), 1, "断った要求は記録を作らない");
    assert!(env.calls().is_empty(), "blocked の間は Drive に触れない");
}

// ---- AC10: dry_run ----

#[test]
fn a_dry_run_returns_200_and_only_reads_the_drive() {
    const TOKEN: &str = "AC10-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-dry");
    let site = photo_site(DAY, DAY, three());
    let server = Server::start(&mut env, &site, TOKEN);
    let deadline = Instant::now() + Duration::from_secs(5);
    let r = loop {
        let r = post_json(server.addr, TOKEN, &day_body(json!({"dry_run": true})));
        if r.status != 409 || Instant::now() >= deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(r.status, 200, "{}", r.body);
    let v = r.json();
    assert_eq!(v["folder"], DAY, "{v}");
    assert_eq!(v["destination"], "yamareco");
    assert_eq!(v["candidate_count"], 3);
    assert_eq!(v["target"], "absent");
    assert_eq!(v["derivative_pending"], 0);
    // Drive には何も書かない: 呼ばれたのは lsjson（と管理の確認の cat）だけ。記録も作らない
    let used = env.ops_used();
    assert!(!used.is_empty(), "Drive に照会していない");
    assert!(
        used.iter().all(|c| c == "list" || c == "read"),
        "書く操作が呼ばれた: {used:?}"
    );
    assert_eq!(job_files(&env), 0);
    // dry_run: false は、ふつうの受付
    let r = post_retry(server.addr, TOKEN, &day_body(json!({"dry_run": false})));
    assert_eq!(r.status, 202, "{}", r.body);
}

// ---- 本文の検査 ----

#[test]
fn the_body_is_checked_before_anything_starts() {
    const TOKEN: &str = "BODY-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-body");
    let site = photo_site(DAY, DAY, three());
    let server = Server::start(&mut env, &site, TOKEN);
    let addr = server.addr;
    let auth = format!("Bearer {TOKEN}");

    // Content-Type
    for ct in [
        None,
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
        Some("application/jsonx"),
    ] {
        let mut headers = vec![("Authorization", auth.as_str())];
        if let Some(ct) = ct {
            headers.push(("Content-Type", ct));
        }
        let body = serde_json::to_vec(&day_body(json!({}))).unwrap();
        let r = http_request(addr, "POST", "/v1/exports", &headers, Some(&body));
        assert_eq!(
            (r.status, r.error().as_str()),
            (400, "invalid_content_type"),
            "{ct:?}"
        );
    }

    // 形・値の誤り（400。固定のコード）
    for (body, code) in [
        (
            json!({"from": "2030-02-30", "destination": "yamareco"}),
            "invalid_date",
        ),
        (json!({"destination": "yamareco"}), "invalid_date"),
        (
            json!({"from": 20300310, "destination": "yamareco"}),
            "invalid_date",
        ),
        (
            json!({"from": DAY, "to": "2030-03-09", "destination": "yamareco"}),
            "invalid_range",
        ),
        (
            json!({"from": DAY, "to": "2030-03-24", "destination": "yamareco"}),
            "invalid_range",
        ),
        (
            json!({"from": DAY, "destination": "x"}),
            "invalid_destination",
        ),
        (json!({"from": DAY}), "invalid_destination"),
        (
            json!({"from": DAY, "destination": "yamareco", "on_exists": "append"}),
            "invalid_on_exists",
        ),
        (
            json!({"from": DAY, "destination": "yamareco", "request_id": "a/b"}),
            "invalid_request_id",
        ),
        (
            json!({"from": DAY, "destination": "yamareco", "dry_run": "yes"}),
            "invalid_body",
        ),
        // 未知のキー
        (
            json!({"from": DAY, "destination": "yamareco", "path": "/etc"}),
            "unknown_field",
        ),
        (
            json!({"from": DAY, "destination": "yamareco", "Dry_Run": true}),
            "unknown_field",
        ),
    ] {
        let r = post_json(addr, TOKEN, &body);
        assert_eq!((r.status, r.error()), (400, code.to_owned()), "{body}");
        assert_eq!(
            r.json().as_object().map(|o| o.len()),
            Some(1),
            "本文は error だけ: {}",
            r.body
        );
    }
    // 重複したキーは 400（最後の値で上書きしない）。ジョブも rclone も作らない
    let before = job_files(&env);
    for raw in [
        &br#"{"from":"2030-03-10","destination":"yamareco","dry_run":true,"dry_run":false}"#[..],
        br#"{"from":"2030-03-10","destination":"yamareco","dry_run":false,"dry_run":true}"#,
        br#"{"from":"2030-03-10","from":"2030-03-11","destination":"yamareco"}"#,
        br#"{"from":"2030-03-10","destination":"yamareco","request_id":null,"request_id":"r-1"}"#,
        br#"{"from":"2030-03-10","destination":"yamareco","dry_run":true,"dry\u005frun":false}"#,
    ] {
        let r = post_raw(addr, TOKEN, raw);
        assert_eq!(
            (r.status, r.error().as_str()),
            (400, "duplicate_field"),
            "{}",
            String::from_utf8_lossy(raw)
        );
    }
    assert_eq!(job_files(&env), before);
    assert!(env.calls().is_empty());
    for raw in [
        &b""[..],
        b"[]",
        b"null",
        b"\"x\"",
        b"{",
        b"{\"from\":",
        b"\xff\xfe",
    ] {
        let r = post_raw(addr, TOKEN, raw);
        assert_eq!(
            (r.status, r.error().as_str()),
            (400, "invalid_body"),
            "{raw:?}"
        );
    }

    // 4KiB: ちょうどは通り、超えると 413（読み切らずに断る。Content-Length でも chunked でも）
    let base = serde_json::to_string(&day_body(json!({}))).unwrap();
    let padded = |n: usize| format!("{base}{}", " ".repeat(n - base.len()));
    let r = post_raw(addr, TOKEN, padded(4096).as_bytes());
    assert_eq!(r.status, 202, "{}", r.body);
    let big = padded(4097);
    let r = post_raw(addr, TOKEN, big.as_bytes());
    assert_eq!((r.status, r.error().as_str()), (413, "payload_too_large"));
    let mut s = connect(addr);
    send_request(
        &mut s,
        "POST",
        "/v1/exports",
        &[
            ("Authorization", &auth),
            ("Content-Type", "application/json"),
            ("Transfer-Encoding", "chunked"),
        ],
        None,
    );
    use std::io::Write;
    let chunk = "x".repeat(5000);
    // 相手が先に断って閉じると、あとの書き込みは失敗しうる（応答は読める）
    let _ = s.write_all(format!("{:x}\r\n{chunk}\r\n0\r\n\r\n", chunk.len()).as_bytes());
    let r = read_response(&mut s);
    assert_eq!((r.status, r.error().as_str()), (413, "payload_too_large"));

    // 断った要求は、記録も rclone も作らない（上の 4096 バイトの 1 件だけが、ジョブになった）
    assert_eq!(job_files(&env), 1);
}

#[test]
fn a_body_that_never_arrives_is_cut_by_the_read_deadline() {
    const TOKEN: &str = "SLOW-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-slowbody");
    let site = photo_site(DAY, DAY, three());
    let limits = Limits {
        body_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let auth = format!("Bearer {TOKEN}");
    let mut s = connect(server.addr);
    // 50 バイトと言って 5 バイトだけ送り、黙る
    use std::io::Write;
    s.write_all(
        format!(
            "POST /v1/exports HTTP/1.1\r\nHost: t\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: 50\r\n\r\n{{\"fro"
        )
        .as_bytes(),
    )
    .unwrap();
    let started = Instant::now();
    let r = read_response(&mut s);
    assert_eq!((r.status, r.error().as_str()), (408, "request_timeout"));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(job_files(&env), 0);
}

// ---- 接続の上限と、ヘッダの期限 ----

#[test]
fn silent_connections_hold_a_slot_only_until_the_header_deadline() {
    const TOKEN: &str = "CONN-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-conn");
    let site = photo_site(DAY, DAY, three());
    let limits = Limits {
        max_connections: 2,
        header_timeout: Duration::from_millis(800),
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let addr = server.addr;

    // 何も送らない接続で枠（2）を使い切る
    let mut a = connect(addr);
    let mut b = connect(addr);
    let t0 = Instant::now();
    // 3 本目は、枠が空くまで（OS の待ち行列で）待たされる
    let mut c = connect(addr);
    send_request(&mut c, "GET", "/healthz", &[], None);
    c.set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut buf = [0u8; 1];
    let early = std::io::Read::read(&mut c, &mut buf);
    assert!(
        matches!(&early, Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
        "枠が空く前に応答が来た: {early:?}"
    );
    // ヘッダの期限で a・b が切られ、枠が空いて、3 本目に答える
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let r = read_response(&mut c);
    assert_eq!(r.status, 200);
    assert!(
        t0.elapsed() >= Duration::from_millis(600),
        "期限より前に枠が空いた: {:?}",
        t0.elapsed()
    );
    assert!(t0.elapsed() < Duration::from_secs(8), "{:?}", t0.elapsed());
    // 黙っていた a・b は閉じられている（読むと EOF かエラー。タイムアウトではない）
    for s in [&mut a, &mut b] {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut sink = Vec::new();
        match std::io::Read::read_to_end(s, &mut sink) {
            Ok(_) => {}
            Err(e) => assert!(
                !matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
                "切られていない: {e}"
            ),
        }
        assert!(!String::from_utf8_lossy(&sink).contains("200 OK"));
    }
}

#[test]
fn a_header_sent_in_dribbles_is_cut_by_the_deadline_as_a_whole() {
    const TOKEN: &str = "DRIB-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-dribble");
    let site = photo_site(DAY, DAY, three());
    let limits = Limits {
        header_timeout: Duration::from_millis(700),
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let mut s = connect(server.addr);
    use std::io::Write;
    let started = Instant::now();
    // 期限の途中で、少しずつ送り続ける（1 バイトごとの無通信ではなく、ヘッダ全体に期限が掛かる）
    s.write_all(b"GET /healthz HTTP/1.1\r\n").unwrap();
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        if s.write_all(b"X-A: b\r\n").is_err() {
            break;
        }
        if started.elapsed() > Duration::from_secs(5) {
            break;
        }
    }
    let mut sink = Vec::new();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = std::io::Read::read_to_end(&mut s, &mut sink);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "期限で切れない: {:?}",
        started.elapsed()
    );
    assert!(!String::from_utf8_lossy(&sink).contains("200 OK"));
}

#[test]
fn the_production_limits_are_the_documented_ones() {
    let l = Limits::default();
    assert_eq!(l.max_connections, 8);
    assert_eq!(l.header_timeout, Duration::from_secs(10));
    assert_eq!(l.body_timeout, Duration::from_secs(10));
    assert_eq!(l.max_body, 4096);
    assert_eq!(l.max_denied_connections, 2);
    assert_eq!(l.denied_header_timeout, Duration::from_secs(2));
    assert_eq!(l.shutdown_wait, Duration::from_secs(75));
}

// ---- 許可外の接続は、許可した送信元の枠を使わない ----

/// 許可リストにあるのは 127.0.0.1 だけ。127.0.0.2（同じホストの別のループバックのアドレス）から来る接続は許可外。
#[test]
fn outside_sources_use_their_own_small_pool_and_never_the_allowed_slots() {
    const TOKEN: &str = "POOL-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-denied-pool");
    let site = photo_site(DAY, DAY, three());
    let limits = Limits {
        max_connections: 2,
        max_denied_connections: 2,
        denied_header_timeout: Duration::from_secs(4),
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let addr = server.addr;
    let t0 = Instant::now();

    // 許可外の、何も送らない接続で、許可外の枠（2）を使い切る
    let mut a = connect_from(addr, "127.0.0.2");
    let mut b = connect_from(addr, "127.0.0.2");
    // 枠が埋まったあとの許可外は、何も返さずに閉じられる（待たされない）
    let mut c = connect_from(addr, "127.0.0.2");
    c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut buf = [0u8; 16];
    match std::io::Read::read(&mut c, &mut buf) {
        Ok(0) => {}
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("枠を超えた許可外の接続が閉じられていない: {other:?}"),
    }
    // 許可した送信元は、その間も待たされない（許可外の 3 本が、許可した枠 2 を使っていれば、ここで待たされる）
    for _ in 0..3 {
        let r = http_request(addr, "GET", "/healthz", &[], None);
        assert_eq!((r.status, r.body.as_str()), (200, "ok"));
    }
    assert!(
        t0.elapsed() < Duration::from_secs(3),
        "許可した送信元が待たされた: {:?}",
        t0.elapsed()
    );
    // 許可した送信元の枠が埋まっていても、許可外は自分の枠で 403 を返される（枠が空いたあとで）
    for s in [&mut a, &mut b] {
        // 黙っていた許可外の接続は、許可外のヘッダの期限（4 秒）で切られる
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(s, &mut sink);
        assert!(!String::from_utf8_lossy(&sink).contains("200 OK"));
    }
    assert!(t0.elapsed() >= Duration::from_secs(3), "期限より前に切れた");
    let mut d = connect_from(addr, "127.0.0.2");
    send_request(&mut d, "GET", "/healthz", &[], None);
    let r = read_response(&mut d);
    assert_eq!((r.status, r.error().as_str()), (403, "forbidden"));
    // 許可外の 403 は、トークンが正しくても同じ（認証より前）
    let auth = format!("Bearer {TOKEN}");
    let mut d = connect_from(addr, "127.0.0.2");
    send_request(
        &mut d,
        "GET",
        "/v1/exports",
        &[("Authorization", &auth)],
        None,
    );
    assert_eq!(read_response(&mut d).status, 403);
    assert_eq!(job_files(&env), 0);
}

// ---- 終了: 接続の打ち切りの期限と、後始末の完了は別 ----

/// 接続の期限（`shutdown_wait`）が 0 でも、親は、子の rclone を止めて回収し、実行枠を手放してから返る。
#[test]
fn stopping_returns_only_after_the_child_is_reaped_even_with_no_time_left_for_connections() {
    const TOKEN: &str = "STOP-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-stop-reap");
    let site = photo_site(DAY, DAY, three());
    env.add_rule("upload", 0, 1, sleep_ms(30_000));
    let limits = Limits {
        shutdown_wait: Duration::ZERO,
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let r = post_retry(server.addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 202, "{}", r.body);
    let copy_pid = wait_for_call(&env, "upload", 30);
    assert!(process_exists(copy_pid));

    let started = Instant::now();
    // 接続の期限が 0 なので、終了コードは 0 か 1（接続のタスクの片付けが間に合ったか）。確かめるのは、返った時点の状態
    let code = server.stop();
    assert!(code <= 1, "{code}");
    assert!(
        !process_exists(copy_pid),
        "返ったのに、子の rclone が残っている（回収されていない）"
    );
    assert!(slot_is_free(&env.state), "返ったのに、実行枠が外れていない");
    assert!(started.elapsed() < Duration::from_secs(20));
    let store = geoframe_export::jobs::JobStore::open(&env.state).unwrap();
    assert_eq!(
        store.scan().records[0].status,
        geoframe_export::jobs::Status::Running
    );
}

/// 受付・`dry_run` のブロッキング処理が終わっていなければ、接続の期限（0）が過ぎても、返らない（期限なしで待つ）。ブロッキング処理は
/// 写真サービスの応答待ち（1.5 秒）で止まっている。
#[test]
fn stopping_waits_for_an_inflight_blocking_task_even_after_the_connection_deadline() {
    const TOKEN: &str = "WAIT-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-stop-inflight");
    // 一覧 API の応答を 1.5 秒遅らせる（`dry_run` が、その間ブロッキングのスレッドで待つ）
    let site = FakeSite::start(vec![(
        "/api/admin/export/photos".to_owned(),
        Reply::Delayed {
            wait: Duration::from_millis(1500),
            then: Box::new(Reply::json(
                serde_json::to_vec(&listing(DAY, DAY, three())).unwrap(),
            )),
        },
    )]);
    let limits = Limits {
        shutdown_wait: Duration::ZERO,
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    // dry_run を送る（応答は待たない）。実行枠の見かけの `busy`（ほかの試験の fork。冒頭の説明）で断られたら、打ち直す
    let auth = format!("Bearer {TOKEN}");
    let body = serde_json::to_vec(&day_body(json!({"dry_run": true}))).unwrap();
    let mut s;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        s = connect(server.addr);
        send_request(
            &mut s,
            "POST",
            "/v1/exports",
            &[
                ("Authorization", &auth),
                ("Content-Type", "application/json"),
            ],
            Some(&body),
        );
        // 写真サービスに届けば、受け付けられている（busy なら届かない）
        let sent = Instant::now() + Duration::from_millis(500);
        while site.request_lines().is_empty() && Instant::now() < sent {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !site.request_lines().is_empty() || Instant::now() > deadline {
            break;
        }
    }
    // 写真サービスに届くまで待つ（ブロッキング処理が、応答待ちに入っている）
    let deadline = Instant::now() + Duration::from_secs(10);
    while site.request_lines().is_empty() {
        assert!(
            Instant::now() < deadline,
            "dry_run が写真サービスに届かない"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let seen = Instant::now();
    let code = server.stop();
    assert!(code <= 1, "{code}");
    assert!(
        seen.elapsed() >= Duration::from_millis(1200),
        "ブロッキング処理が終わる前に返った: {:?}",
        seen.elapsed()
    );
    assert!(seen.elapsed() < Duration::from_secs(20));
    drop(s);
}

/// ジョブのスレッドが（写真サービスの応答待ちなどで）終わるのに時間がかかっても、接続の期限（0）が過ぎても、返らない。スレッドは
/// tokio のランタイムの外の素のスレッドなので、ランタイムの破棄は待たない。返る条件は、ジョブのスレッドの終わり（子の回収と
/// 実行枠の解放を済ませた状態）だけ。
#[test]
fn stopping_waits_for_a_slow_job_thread_even_after_the_connection_deadline() {
    const TOKEN: &str = "JOBT-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-stop-jobthread");
    // 一覧 API はすぐ、表示用 JPEG は 1.5 秒遅れて返す（ジョブのスレッドが、写真の取得で待つ）
    let mut routes = vec![(
        "/api/admin/export/photos".to_owned(),
        Reply::json(serde_json::to_vec(&listing(DAY, DAY, three())).unwrap()),
    )];
    for (i, p) in three().iter().enumerate() {
        routes.push((
            format!("/derivatives/p{}.jpg", p["id"].as_i64().unwrap()),
            Reply::Delayed {
                wait: Duration::from_millis(1500),
                then: Box::new(Reply::jpeg(jpeg(64, 48, i as u8))),
            },
        ));
    }
    let site = FakeSite::start(routes);
    let limits = Limits {
        shutdown_wait: Duration::ZERO,
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let r = post_retry(server.addr, TOKEN, &day_body(json!({})));
    assert_eq!(r.status, 202, "{}", r.body);
    // 写真の取得が始まるまで待つ
    let deadline = Instant::now() + Duration::from_secs(10);
    while !site
        .request_lines()
        .iter()
        .any(|l| l.contains("/derivatives/"))
    {
        assert!(Instant::now() < deadline, "写真の取得が始まらない");
        std::thread::sleep(Duration::from_millis(5));
    }
    let seen = Instant::now();
    let code = server.stop();
    assert!(code <= 1, "{code}");
    assert!(
        seen.elapsed() >= Duration::from_millis(1200),
        "ジョブのスレッドが終わる前に返った: {:?}",
        seen.elapsed()
    );
    assert!(slot_is_free(&env.state), "返ったのに、実行枠が外れていない");
    assert!(seen.elapsed() < Duration::from_secs(20));
}

/// 本文を読み切らない接続が残っていても、接続の期限（ここでは 0.3 秒）で落として、返る（本文の期限が 30 秒でも、待たない）。
/// 打ち切ったことは、終了コード 1 で分かる。
#[test]
fn a_connection_stalled_in_its_body_is_dropped_at_the_connection_deadline() {
    const TOKEN: &str = "STAL-Tk-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0Ya";
    let mut env = Env::new("http-stop-stalled");
    let site = photo_site(DAY, DAY, three());
    let limits = Limits {
        body_timeout: Duration::from_secs(30),
        shutdown_wait: Duration::from_millis(300),
        ..Limits::default()
    };
    let server = Server::start_with(
        &mut env,
        &site,
        TOKEN,
        vec!["127.0.0.1".parse().unwrap()],
        limits,
    );
    let auth = format!("Bearer {TOKEN}");
    let mut s = connect(server.addr);
    use std::io::Write;
    s.write_all(
        format!(
            "POST /v1/exports HTTP/1.1\r\nHost: t\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: 50\r\n\r\n{{\"fro"
        )
        .as_bytes(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    assert_eq!(server.stop(), 1, "接続を落としたのに、0 を返した");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(job_files(&env), 0);
    drop(s);
}
