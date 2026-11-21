//! `serve`: tailnet 向けの HTTP の入口（design.md 3.4・6.1・8 章。T6）。トークン・送信元の制限・本文の検査・接続の上限・ジョブの API。
//!
//! ## 状態コードの対応
//!
//! | 要求 | 結果 | 状態 | 本文 |
//! |---|---|---|---|
//! | すべて | 許可していない送信元（`/healthz` も） | 403 | `{"error":"forbidden"}` |
//! | `/healthz` 以外 | `Authorization: Bearer` が無い・違う | 401（`WWW-Authenticate: Bearer`） | `{"error":"unauthorized"}` |
//! | `GET /healthz` | | 200 | `ok`（text/plain） |
//! | `POST /v1/exports` | `Content-Type` が `application/json` でない | 400 | `{"error":"invalid_content_type"}` |
//! | | 本文が 4KiB を超える | 413 | `{"error":"payload_too_large"}` |
//! | | 本文を 10 秒で読み切れない | 408 | `{"error":"request_timeout"}` |
//! | | JSON でない・オブジェクトでない・`dry_run` の型違い | 400 | `{"error":"invalid_body"}` |
//! | | 未知のキー | 400 | `{"error":"unknown_field"}` |
//! | | 同じキーが 2 回以上（値が違っても同じでも。`null` も） | 400 | `{"error":"duplicate_field"}`（最後の値で上書きしない） |
//! | | 値の誤り | 400 | `{"error":"invalid_date"}` など（[`crate::error::ErrorCode`]。キーの型違いも、そのキーのコード） |
//! | | 受け付けた（同じ `request_id` の再送も） | 202 | `{"job_id","status"}` |
//! | | 実行中（実行枠が取れない）・`request_id` の中身違い | 409 | `{"error":"busy"}`・`{"error":"request_id_mismatch"}` |
//! | | `blocked` の記録がある | 503 | `{"error":"blocked"}` |
//! | | 内部エラー | 500 | `{"error":"internal"}` など |
//! | | `dry_run: true` | 200 | 6.1 の形（[`crate::service::Service::dry_run`]） |
//! | | `dry_run` で実行中 | 409 | `{"error":"busy"}` |
//! | | `dry_run` で `blocked`・写真サービス・Drive の失敗 | 503 | `{"error":"blocked"}`・`{"error":"feed_unavailable"}` など |
//! | | 終了の印が立った後 | 503 | `{"error":"shutting_down"}` |
//! | `GET /v1/exports/{job_id}` | あった・形が違う / 無い・読めない | 200 / 404（`not_found`）/ 500（`internal`） | 6.1 の形 |
//! | `GET /v1/exports` | | 200 | `{"jobs":[{"job_id","status","request",…}]}`（新しい順。直近 50 件） |
//! | 上のどれでもない経路・メソッド | | 404（`not_found`）・405（`method_not_allowed`） | |
//!
//! 応答の `error` は固定のコードだけ。rclone の出力・資格情報・トークン・受け取った値は載せない。
//!
//! ## 守り
//!
//! - **送信元**はソケットの接続相手のアドレスで判定する（`X-Forwarded-For` などのヘッダは見ない）。許可リストも相手も、IPv4-mapped
//!   IPv6（`::ffff:a.b.c.d`）は IPv4 にそろえてから比べる（`::` に待ち受けると、IPv4 の相手がその形で見えるため）。
//! - **認証**は 403 の次（許可外は、トークンを見る前に断る）。`/healthz` だけは認証しない。門は経路の振り分けの外側にあるので、
//!   未知の経路・違うメソッドも、許可外は 403、トークン無しは 401 になる（経路の有無を漏らさない）。
//! - **接続**は、許可した送信元から同時に [`Limits::max_connections`]（既定 8）本まで。accept の直後にソケットの相手で分け、
//!   許可外の相手は、別の小さな枠（[`Limits::max_denied_connections`]、既定 2）で受け、短い期限（[`Limits::denied_header_timeout`]、
//!   既定 2 秒）でヘッダを読んで 403 を返して閉じる（許可した送信元の枠を使わない。別の枠も埋まっていれば、何も返さずに閉じる）。
//!   許可した送信元の枠が埋まっている間は、その接続を 1 本だけ accept して持ち、枠が空くまで次を accept しない（超えた分は、
//!   503 ではなく、OS の待ち行列で待たされる）。1 接続 1 要求（keep-alive しない）。ヘッダの読み取りは [`Limits::header_timeout`]
//!   （10 秒。hyper の `header_read_timeout`）、本文の読み取りは [`Limits::body_timeout`]（10 秒）。本文は [`Limits::max_body`]
//!   （4KiB）まで。
//! - **ログ**は、起動・終了・起動時の復旧の結果と、アクセスログ（送信元・メソッド〔`GET`・`POST`・それ以外は `other`〕・経路の
//!   テンプレート〔`/healthz`・`/v1/exports`・`/v1/exports/{job_id}`・`other`〕・ジョブ ID〔照会で見つかった記録と、受付で作った
//!   ものだけ。なければ `-`〕・状態コード・所要時間）だけ。ヘッダ・クエリ・本文・トークン・要求の文字列そのもの（パスの値・
//!   メソッド名）は出さない。トークンは、小文字 16 進 32 文字（ジョブ ID と同じ形）にもでき、メソッド名・パス・クエリのどこにも
//!   入れられるので、要求の文字列は記録しない。
//!
//! ## 起動と終了
//!
//! 1. `--listen`・トークンのファイルを検査する（満たさなければ終了コード 2。[`check_listen`]・[`token`]）。`--listen` は、tailnet の
//!    アドレス帯（IPv4 は 100.64.0.0/10、IPv6 は fd7a:115c:a1e0::/48）かループバックだけ。
//! 2. SIGINT・SIGTERM を tokio で受ける（`handle_signals` が真のとき。受けたら [`Cancel::cancel`]）。**起動時の復旧より前**に入れる
//!    （復旧中に既定の動作で親だけが終わると、rclone が残る）。`run` と違い、libc の handler は入れない
//!    （[`crate::app`]。印を立てる先は同じ [`Cancel`]）。
//! 3. [`Service::startup_recovery`] を済ませてから待ち受ける。待ち受けながら終わるまで 503 にする案もあるが、復旧は実行枠を
//!    取るので、その間の受付は 409（`busy`）になり、呼び出し側が「別のジョブが実行中」と取り違える。接続できない（拒否）なら
//!    呼び出し側は再試行するだけで済み、systemd も起動の完了を待たない（`Type=simple`）。復旧中に印が立てば、待ち受けずに終わる
//!    （復旧が子〔rclone〕を回収して実行枠を手放すまでは、終わらない）。
//!    **帰結**: 止まったジョブ（Drive の段階を記録した `running`）があると、本番の待ち（Drive の反映を 30 秒ずつ最大 4 回）のぶん、
//!    再起動のあと最大で数分、待ち受けない（呼び出し側には接続拒否に見える。`/healthz` も答えない）。呼び出し側は、接続を拒否
//!    されたら間隔を空けて再試行する。unit の起動の完了を `/healthz` で見張るなら、その間は未起動に見える（T7）。
//! 4. 終了（印が立つ）では、新しい受付を閉じ、①接続のタスクが終わるのを待ち（要求の途中だったものは返事をする。**この待ちだけが
//!    [`Limits::shutdown_wait`] の期限つき**。超えたら接続を落とし、終了コード 1）、②受付・`dry_run`・起動時の復旧の
//!    ブロッキング処理が終わるのを待ち、③ [`Service::wait_idle`] でジョブのスレッドの終わり（rclone を止めて `wait` し終え、実行枠を
//!    手放すまで）を待ってから終わる。②③は**期限なし**（「HTTP の打ち切りの期限」と「後始末の完了の条件」は別。印が立てば rclone は
//!    止まるので、後始末は有限の時間で終わる前提）。どの経路（復旧の最中の印・待ち受けの失敗・通常の停止）でも、この後始末を
//!    経てから返る。そのあと tokio のランタイムは、短い待ち（[`RUNTIME_DROP_WAIT`]）で破棄する（残ったブロッキング処理があっても、
//!    無限に待たない）。止まったジョブの記録は確定せず、次の起動の復旧が片付ける。
//!    本文の読み取りの最中に印が立っても、受付は、実行枠を取ったあと記録を書く直前にもう一度印を確かめ、立っていれば何も
//!    登録せずに 503（`shutting_down`。[`crate::service::SubmitError::ShuttingDown`]）にする。

mod token;

use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, HttpBody};
use axum::extract::{Request, State};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use crate::app::EXIT_USAGE;
use crate::cancel::Cancel;
use crate::error::ErrorCode;
use crate::flow::Tuning;
use crate::jobid::JobId;
use crate::request::RawRequest;
use crate::service::{DryRunError, Lookup, Service, StartupReport, SubmitError};
use crate::settings::Settings;

pub use token::{
    MAX_LEN as TOKEN_MAX_LEN, MIN_LEN as TOKEN_MIN_LEN, Token, TokenError, load_token,
    load_token_for_uid,
};

/// 同時に受け付ける接続の数（design.md 6.1）。
pub const MAX_CONNECTIONS: usize = 8;
/// 本文の上限（バイト。design.md 6.1）。
pub const MAX_BODY: usize = 4096;
/// ヘッダ・本文の読み取りの期限（design.md 6.1）。
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 許可していない送信元の接続の同時数（design.md 6.1）。許可した送信元の枠（[`MAX_CONNECTIONS`]）とは別に数える。
pub const MAX_DENIED_CONNECTIONS: usize = 2;
/// 許可していない送信元の、ヘッダの読み取りの期限（403 を返すだけなので、短い）。
pub const DENIED_HEADER_TIMEOUT: Duration = Duration::from_secs(2);
/// 終了のとき、**接続**（HTTP）の終わりを待つ上限。超えたら、接続のタスクを落として、終了コード 1 にする。
///
/// これは「HTTP の打ち切りの期限」だけを決める。後始末（ブロッキング処理・ジョブのスレッドが、子の rclone を止めて回収し、
/// 実行枠を手放すまで）の完了は、この期限と関係なく、**必ず待つ**（[`run`]。期限なし）。印が立てば rclone は止まるので、後始末は
/// 有限の時間で終わる。systemd の既定の停止の待ち（`TimeoutStopSec` の既定は 90 秒）より前に出す。T7 の unit は、これより
/// 長い `TimeoutStopSec`（例: 120 秒）を置く。
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(75);
/// 後始末を待っている間、状況をログに出す間隔。
const CLEANUP_LAP: Duration = Duration::from_secs(10);
/// 後始末が済んだあとの、tokio のランタイムの破棄の待ち（数えていない残りがあっても、無限に待たない）。
const RUNTIME_DROP_WAIT: Duration = Duration::from_secs(5);

/// 本番の値は [`Limits::default`]。試験が短くする。
#[derive(Debug, Clone)]
pub struct Limits {
    /// 許可した送信元の接続の同時数。
    pub max_connections: usize,
    /// ヘッダを読み終えるまでの期限（接続してから、または前の応答のあとから。1 接続 1 要求なので、接続してから）。
    pub header_timeout: Duration,
    /// 本文を読み終えるまでの期限（ヘッダを読んでから）。
    pub body_timeout: Duration,
    pub max_body: usize,
    /// 許可していない送信元の接続の同時数（超えた分は、何も返さずに閉じる）。
    pub max_denied_connections: usize,
    /// 許可していない送信元の、ヘッダを読み終えるまでの期限。
    pub denied_header_timeout: Duration,
    /// 終了のとき、接続の終わりを待つ上限（印を見てから数える。後始末の完了の待ちには掛からない。[`SHUTDOWN_WAIT`]）。
    pub shutdown_wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            header_timeout: READ_TIMEOUT,
            body_timeout: READ_TIMEOUT,
            max_body: MAX_BODY,
            max_denied_connections: MAX_DENIED_CONNECTIONS,
            denied_header_timeout: DENIED_HEADER_TIMEOUT,
            shutdown_wait: SHUTDOWN_WAIT,
        }
    }
}

/// `--listen` を受け付けない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenError {
    /// `0.0.0.0`・`::`（すべてのインターフェースへの待ち受け）。IPv4-mapped の `::ffff:0.0.0.0` も同じ。
    Unspecified,
    /// ポートが 0（OS 任せ）。
    ZeroPort,
    /// マルチキャスト・ブロードキャスト（具体的な 1 つのアドレスではない）。
    NotUnicast,
    /// Tailscale の tailnet のアドレス帯でも、ループバックでもない（LAN の私的アドレス・グローバルアドレス・リンクローカルなど）。
    NotTailnet,
}

impl std::fmt::Display for ListenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unspecified => {
                "0.0.0.0・:: には待ち受けられません。tailnet の具体的な IP を指定してください"
            }
            Self::ZeroPort => "ポートを具体的に指定してください（0 は不可）",
            Self::NotUnicast => "マルチキャスト・ブロードキャストのアドレスは使えません",
            Self::NotTailnet => {
                "tailnet のアドレス帯（100.64.0.0/10・fd7a:115c:a1e0::/48）かループバック以外には待ち受けられません"
            }
        })
    }
}

/// Tailscale の tailnet のアドレス帯: IPv4 は CGNAT の 100.64.0.0/10、IPv6 は fd7a:115c:a1e0::/48（`ip` は IPv4-mapped を IPv4 に
/// そろえたもの）。
fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 0x40
        }
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// `--listen` の検査（design.md 3.4）。tailnet の具体的なアドレス（[`is_tailnet`]）とポートだけを受け付ける。LAN の私的アドレス
/// （`192.168.x.x`・`10.x.x.x`・`172.16.0.0/12`）・グローバルアドレスなど、tailnet でないアドレスは、起動しない。
///
/// **ループバック（`127.0.0.0/8`・`::1`）は許す**: 同じホストの中からしか届かず、ネットワークには出ないので、本番でも害が
/// 無い（unit が tailnet の IP を渡す運用は変わらない）。試験が空きポートで起こすのにも要る。
pub fn check_listen(addr: SocketAddr) -> Result<(), ListenError> {
    let ip = addr.ip().to_canonical();
    if ip.is_unspecified() {
        return Err(ListenError::Unspecified);
    }
    if addr.port() == 0 {
        return Err(ListenError::ZeroPort);
    }
    let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
    if ip.is_multicast() || broadcast {
        return Err(ListenError::NotUnicast);
    }
    if !ip.is_loopback() && !is_tailnet(ip) {
        return Err(ListenError::NotTailnet);
    }
    Ok(())
}

/// 待ち受けの設定。
#[derive(Debug)]
pub struct Config {
    pub listen: SocketAddr,
    /// 送信元の許可リスト（IPv4-mapped は IPv4 にそろえて比べる）。
    pub allow: Vec<IpAddr>,
    pub token: Token,
    pub limits: Limits,
    /// 真なら、SIGINT・SIGTERM を tokio で受けて [`Cancel::cancel`] を呼ぶ（バイナリの `serve`）。プロセス内の試験は偽にして、
    /// [`Cancel::cancel`] で止める。
    pub handle_signals: bool,
}

/// コマンドの入口（[`crate::app`] から）。終了コードを返す: 0 終了の印で止まった・2 引数の誤り（`--listen`・トークン）・
/// 1 それ以外の失敗（待ち受けられない・終了のときに時間内に終わらない）。
pub fn serve(
    settings: &Settings,
    listen: SocketAddr,
    token_file: &Path,
    allow: &[IpAddr],
    cancel: &Cancel,
    handle_signals: bool,
    err: &mut dyn Write,
) -> u8 {
    if let Err(e) = check_listen(listen) {
        let _ = writeln!(err, "geoframe-export: 起動しません: --listen: {e}");
        return EXIT_USAGE;
    }
    let token = match load_token(token_file) {
        Ok(t) => t,
        Err(e) => {
            // パスは出さない（`--token-file` の値は端末にも journal にも残さない）
            let _ = writeln!(err, "geoframe-export: 起動しません: --token-file: {e}");
            return EXIT_USAGE;
        }
    };
    let service = match Service::with_cancel(settings.clone(), Tuning::default(), cancel.clone()) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "geoframe-export: 起動しません: {e}");
            return 1;
        }
    };
    let config = Config {
        listen,
        allow: allow.to_vec(),
        token,
        limits: Limits::default(),
        handle_signals,
    };
    match run(service, config, |addr| {
        tracing::info!(listen = %addr, "書き出しの HTTP の入口を始めました");
    }) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "geoframe-export: serve が失敗しました: {}", e.kind());
            1
        }
    }
}

/// 起動時の復旧を済ませ、待ち受けて、印が立つまで受け付ける。`on_ready` は、待ち受けを始めた直後に、実際のアドレスで呼ぶ
/// （試験が空きポートを知る。`--listen` はポート 0 を断るが、ここは [`check_listen`] を通さない下位の入口なので、0 でもよい）。
/// 印が立ったら、上の「起動と終了」の順に待ってから返る。0 なら、接続もジョブも、時間内に止まった。1 は、接続が
/// [`Limits::shutdown_wait`] までに終わらず落とした（このときも、ジョブの子は回収してから返る）。
///
/// **どの経路でも**（起動時の復旧の最中に印が立った場合・待ち受けられなかった場合・通常の停止）、返る前に、ブロッキング処理
/// （受付・`dry_run`・起動時の復旧）とジョブのスレッドの終わりを、期限なしで待つ。印が立てば rclone は止まり、ジョブも復旧も
/// 子を回収して実行枠を手放すので、この待ちは有限の時間で終わる（その前提で、HTTP の打ち切りの期限とは分けてある）。
pub fn run(service: Service, config: Config, on_ready: impl FnOnce(SocketAddr)) -> io::Result<u8> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let inflight = Arc::new(Inflight::default());
    let outcome = runtime.block_on(run_async(
        service.clone(),
        config,
        Arc::clone(&inflight),
        on_ready,
    ));

    // 後始末（子の回収と実行枠の解放）。ここへ来た時点で終わる途中なので、印を立てる（待ち受けの失敗で返ってきた場合も、
    // ジョブが残っていれば止める）
    service.cancel().cancel();
    // 先にブロッキング処理（受付がジョブのスレッドを作りうる）、そのあとジョブのスレッド
    while !inflight.wait_idle(CLEANUP_LAP) {
        tracing::warn!("受付・復旧のブロッキング処理の終わりを待っています");
    }
    while !service.wait_idle(CLEANUP_LAP) {
        tracing::warn!("ジョブのスレッド（rclone の回収）の終わりを待っています");
    }
    // 数えていない残りがあっても、ランタイムの破棄で無限に待たない
    runtime.shutdown_timeout(RUNTIME_DROP_WAIT);

    match outcome {
        Ok(0) => {
            tracing::info!("終わりました");
            Ok(0)
        }
        Ok(code) => {
            tracing::error!(
                "接続が時間内に終わらなかったので落としました（ジョブの子は回収しました）"
            );
            Ok(code)
        }
        Err(e) => Err(e),
    }
}

/// ブロッキング処理（受付・`dry_run`・起動時の復旧）の数。相手が切れてハンドラの future が落ちても、`spawn_blocking` の中身は
/// 続くので、接続とは別に数える（数える守りはクロージャの中へ移す）。
#[derive(Default)]
struct Inflight {
    count: Mutex<usize>,
    idle: Condvar,
}

struct InflightGuard(Arc<Inflight>);

impl Inflight {
    fn enter(self: &Arc<Self>) -> InflightGuard {
        *self.count.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        InflightGuard(Arc::clone(self))
    }

    /// 0 になるまで待つ。`timeout` までに 0 になれば `true`。
    fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut n = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        while *n > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            n = self
                .idle
                .wait_timeout(n, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        *self.0.count.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.idle.notify_all();
    }
}

struct App {
    service: Service,
    token: Token,
    /// IPv4-mapped は IPv4 にそろえてある。
    allow: Vec<IpAddr>,
    cancel: Cancel,
    limits: Limits,
    inflight: Arc<Inflight>,
}

impl App {
    /// 同期の層（I/O をする）を、ブロッキングのスレッドで動かす。`None` はパニック。
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let guard = self.inflight.enter();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            f()
        })
        .await
        .ok()
    }
}

async fn run_async(
    service: Service,
    config: Config,
    inflight: Arc<Inflight>,
    on_ready: impl FnOnce(SocketAddr),
) -> io::Result<u8> {
    let Config {
        listen,
        allow,
        token,
        limits,
        handle_signals,
    } = config;
    let cancel = service.cancel().clone();
    if handle_signals {
        spawn_signal_task(cancel.clone())?;
    }

    // 起動時の復旧（実行枠を取る。済んでから待ち受ける。上の「起動と終了」の 3）。ここの待ちに期限は掛けない（子を回収して
    // 実行枠を手放すまで終わらない。印が立てば rclone は止まるので、有限）
    let recovery = {
        let service = service.clone();
        let guard = inflight.enter();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            service.startup_recovery()
        })
        .await
    };
    match recovery {
        Ok(StartupReport::Skipped) => {
            tracing::info!("run が動いているので、起動時の復旧を見送りました");
        }
        Ok(StartupReport::Done { cleaned, recovery }) => {
            tracing::info!(
                cleaned,
                recovered = recovery.recovered,
                blocked = recovery.blocked,
                cancelled = recovery.cancelled,
                "起動時の復旧を済ませました"
            );
        }
        Ok(StartupReport::Failed(e)) => {
            tracing::error!(code = %e.code, detail = e.detail, "起動時の復旧に失敗しました");
        }
        Err(_) => tracing::error!("起動時の復旧が異常終了しました"),
    }
    if cancel.is_cancelled() {
        return Ok(0);
    }

    let listener = TcpListener::bind(listen).await?;
    on_ready(listener.local_addr()?);

    let app = Arc::new(App {
        service: service.clone(),
        token,
        allow: allow.iter().map(IpAddr::to_canonical).collect(),
        cancel: cancel.clone(),
        limits: limits.clone(),
        inflight: Arc::clone(&inflight),
    });
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let denied_slots = Arc::new(Semaphore::new(limits.max_denied_connections));
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut connections: JoinSet<()> = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // 許可した送信元の接続で、枠が空くのを待っているもの（accept した直後に相手で分けるので、枠が埋まっている間は、
    // 1 本だけ accept して持つ。持っている間は accept しない）
    let mut waiting: Option<(tokio::net::TcpStream, SocketAddr)> = None;

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                if cancel.is_cancelled() {
                    break;
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            permit = Arc::clone(&slots).acquire_owned(), if waiting.is_some() => {
                if let (Ok(permit), Some((stream, peer))) = (permit, waiting.take()) {
                    spawn_connection(&mut connections, &app, &stop_rx, stream, peer, permit, limits.header_timeout);
                }
            }
            accepted = listener.accept(), if waiting.is_none() => match accepted {
                Ok((stream, peer)) => {
                    if app.allow.contains(&peer.ip().to_canonical()) {
                        match Arc::clone(&slots).try_acquire_owned() {
                            Ok(permit) => spawn_connection(&mut connections, &app, &stop_rx, stream, peer, permit, limits.header_timeout),
                            Err(_) => waiting = Some((stream, peer)),
                        }
                    } else {
                        // 許可外は、許可した接続の枠を使わない。別の小さな枠が埋まっていれば、何も返さずに閉じる
                        match Arc::clone(&denied_slots).try_acquire_owned() {
                            Ok(permit) => spawn_connection(&mut connections, &app, &stop_rx, stream, peer, permit, limits.denied_header_timeout),
                            Err(_) => drop(stream),
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(kind = %e.kind(), "接続を受け付けられません");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }

    // 終わり: 新しい受付を閉じる → 接続のタスク（ここだけが期限つき）。ブロッキング処理とジョブのスレッドの完了は、
    // 返ったあとの [`run`] が、期限なしで待つ
    tracing::info!("終了の印を受けました。受付を閉じて、実行中の処理の終わりを待ちます");
    drop(listener);
    drop(waiting);
    let _ = stop_tx.send(true);
    let joined = tokio::time::timeout(limits.shutdown_wait, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    if !joined {
        // 接続のタスクは落とす（中のブロッキング処理は続くので、[`Inflight`] が数えている）
        connections.abort_all();
    }
    Ok(u8::from(!joined))
}

fn spawn_connection(
    connections: &mut JoinSet<()>,
    app: &Arc<App>,
    stop: &watch::Receiver<bool>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    permit: tokio::sync::OwnedSemaphorePermit,
    header_timeout: Duration,
) {
    let app = Arc::clone(app);
    let stop = stop.clone();
    connections.spawn(async move {
        let _permit = permit;
        serve_connection(app, stream, peer, stop, header_timeout).await;
    });
}

/// SIGINT・SIGTERM を、[`Cancel::cancel`] を呼ぶ受け口にする。登録はこの関数の中で済む（呼んだ直後から有効）。
fn spawn_signal_task(cancel: Cancel) -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;
    tokio::spawn(async move {
        loop {
            let got = tokio::select! {
                s = int.recv() => s,
                s = term.recv() => s,
            };
            if got.is_none() {
                break;
            }
            // 2 回目以降も、印を立てるだけ（立った印は下りない）
            cancel.cancel();
        }
    });
    Ok(())
}

async fn serve_connection(
    app: Arc<App>,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    mut stop: watch::Receiver<bool>,
    header_timeout: Duration,
) {
    let service = TowerToHyperService::new(router(Arc::clone(&app), peer.ip().to_canonical()));
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .keep_alive(false)
        .max_buf_size(16 * 1024)
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    let result = tokio::select! {
        r = connection.as_mut() => r,
        _ = async { let _ = stop.wait_for(|stopped| *stopped).await.map(|_| ()); } => {
            // 終わりの印: 要求の途中なら返事をしてから閉じる
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    };
    if let Err(e) = result {
        // ヘッダの期限・切断・不正な要求。中身（ヘッダ・本文）は出さない
        tracing::debug!(
            incomplete = e.is_incomplete_message(),
            timeout = e.is_timeout(),
            "接続を閉じました"
        );
    }
}

type Gate = (Arc<App>, IpAddr);

fn router(app: Arc<App>, peer: IpAddr) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/exports", get(list_jobs).post(create_job))
        .route("/v1/exports/{job_id}", get(show_job))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(Arc::clone(&app))
        // 門はいちばん外側（経路の振り分けより前）。403 → 401 の順
        .layer(middleware::from_fn_with_state((app, peer), gate))
}

/// 送信元の許可 → 認証（`/healthz` 以外）→ 振り分け。アクセスログもここで出す。出すのは、送信元・メソッド（既知のものだけ）・
/// 経路の型・ジョブ ID（記録が実際にあったとき）・状態コード・所要時間だけ。ヘッダ・クエリ・本文・要求の値そのものは出さない
/// （トークンは、メソッド名にもパスにもクエリにも入れられるので、要求の文字列をそのまま記録しない）。
async fn gate(State((app, peer)): State<Gate>, req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = method_label(req.method());
    let label = route_label(req.uri().path());
    let response = if !app.allow.contains(&peer) {
        error(StatusCode::FORBIDDEN, "forbidden")
    } else if req.uri().path() == "/healthz" || authorized(&app.token, req.headers()) {
        next.run(req).await
    } else {
        let mut r = error(StatusCode::UNAUTHORIZED, "unauthorized");
        r.headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        r
    };
    let job_id = response
        .extensions()
        .get::<LoggedJob>()
        .map_or("-", |j| j.0.as_str());
    tracing::info!(
        peer = %peer,
        method = %method,
        path = %label,
        job_id = %job_id,
        status = response.status().as_u16(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "要求"
    );
    response
}

/// ハンドラが応答に付ける、アクセスログに出すジョブ ID。付けるのは、照会で**実際に見つかった**記録の ID と、受付で**作った**
/// ID だけ（要求に書かれた文字列は付けない）。
#[derive(Clone)]
struct LoggedJob(String);

fn with_job(mut response: Response, id: &JobId) -> Response {
    response
        .extensions_mut()
        .insert(LoggedJob(id.as_str().to_owned()));
    response
}

/// アクセスログのメソッド。既知の `GET`・`POST` だけ。ほかは `other`（メソッド名には、トークンと同じ文字を使えるので、
/// 要求の値をそのまま記録しない）。
fn method_label(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "GET",
        axum::http::Method::POST => "POST",
        _ => "other",
    }
}

/// アクセスログの経路。型だけ（ジョブ ID も含めない。トークンに許す文字〔小文字 16 進 32 文字を含む〕は、ジョブ ID の形と
/// 区別できないので、パスの値は記録しない）。
fn route_label(path: &str) -> &'static str {
    match path {
        "/healthz" => "/healthz",
        "/v1/exports" => "/v1/exports",
        _ => match path.strip_prefix("/v1/exports/") {
            Some(rest) if !rest.is_empty() && !rest.contains('/') => "/v1/exports/{job_id}",
            _ => "other",
        },
    }
}

/// `Authorization` が 1 つだけあり、`Bearer <トークン>` が合う。
fn authorized(token: &Token, headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    match (values.next(), values.next()) {
        (Some(v), None) => token.matches_authorization(v.as_bytes()),
        _ => false,
    }
}

// ---- 応答 ----

fn json_response(status: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    r.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// `{"error": <固定のコード>}`。
fn error(status: StatusCode, code: &str) -> Response {
    json_response(status, &json!({"error": code}))
}

fn internal() -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal.as_str(),
    )
}

async fn healthz() -> Response {
    let mut r = Response::new(Body::from("ok"));
    r.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found")
}

async fn method_not_allowed() -> Response {
    error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
}

// ---- ジョブの API ----

async fn list_jobs(State(app): State<Arc<App>>) -> Response {
    let service = app.service.clone();
    match app.blocking(move || service.list()).await {
        Some(Ok(jobs)) => json_response(StatusCode::OK, &json!({ "jobs": jobs })),
        _ => internal(),
    }
}

async fn show_job(State(app): State<Arc<App>>, req: Request) -> Response {
    let Some(id) = req
        .uri()
        .path()
        .strip_prefix("/v1/exports/")
        .and_then(JobId::parse)
    else {
        return error(StatusCode::NOT_FOUND, "not_found");
    };
    let service = app.service.clone();
    let id_for_log = id.clone();
    match app.blocking(move || service.lookup(id.as_str())).await {
        // 見つかった記録の ID だけをログに出す
        Some(Lookup::Found(v)) => with_job(json_response(StatusCode::OK, &v), &id_for_log),
        Some(Lookup::NotFound) => error(StatusCode::NOT_FOUND, "not_found"),
        Some(Lookup::Unreadable) | None => internal(),
    }
}

async fn create_job(State(app): State<Arc<App>>, req: Request) -> Response {
    if app.cancel.is_cancelled() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ShuttingDown.as_str(),
        );
    }
    let (parts, body) = req.into_parts();
    if !is_json(&parts.headers) {
        return error(StatusCode::BAD_REQUEST, "invalid_content_type");
    }
    let max = app.limits.max_body;
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > max) {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
    }
    let bytes = match tokio::time::timeout(app.limits.body_timeout, read_body(body, max)).await {
        Err(_) => return error(StatusCode::REQUEST_TIMEOUT, "request_timeout"),
        Ok(Err(BodyError::TooLarge)) => {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
        }
        Ok(Err(BodyError::Read)) => return error(StatusCode::BAD_REQUEST, "invalid_body"),
        Ok(Ok(bytes)) => bytes,
    };
    let request =
        match parse_body(&bytes).and_then(|raw| raw.validate().map_err(|code| code.as_str())) {
            Ok(r) => r,
            Err(code) => return error(StatusCode::BAD_REQUEST, code),
        };

    // 本文の読み取り中に印が立ったら、始めない（読み取りの前の確認だけだと、202 を返したのに動かないジョブができる）
    if app.cancel.is_cancelled() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ShuttingDown.as_str(),
        );
    }
    let service = app.service.clone();
    if request.dry_run {
        return match app.blocking(move || service.dry_run(&request)).await {
            Some(Ok(v)) => json_response(StatusCode::OK, &v),
            Some(Err(e)) => {
                let status = match e {
                    DryRunError::Busy => StatusCode::CONFLICT,
                    DryRunError::Blocked(_) | DryRunError::Failed(_) => {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                };
                error(status, e.code().as_str())
            }
            None => internal(),
        };
    }
    match app.blocking(move || service.submit(&request)).await {
        Some(Ok(s)) => with_job(
            json_response(
                StatusCode::ACCEPTED,
                &json!({"job_id": s.job_id.as_str(), "status": s.status.as_str()}),
            ),
            &s.job_id,
        ),
        Some(Err(e)) => {
            let status = match e {
                SubmitError::Busy | SubmitError::RequestIdMismatch => StatusCode::CONFLICT,
                SubmitError::Blocked(_) | SubmitError::ShuttingDown => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                SubmitError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error(status, e.code().as_str())
        }
        None => internal(),
    }
}

/// `Content-Type` が `application/json`（`; charset=utf-8` などの付属は許す。大文字小文字は区別しない）。
fn is_json(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    let (Some(v), None) = (values.next(), values.next()) else {
        return false;
    };
    let Ok(text) = v.to_str() else {
        return false;
    };
    let media = text.split(';').next().unwrap_or("").trim();
    media.eq_ignore_ascii_case("application/json")
}

enum BodyError {
    TooLarge,
    Read,
}

/// 本文を `max` バイトまで読む（超えたら、残りを読まずに断る）。期限は呼び出し側が掛ける。
async fn read_body(mut body: Body, max: usize) -> Result<Vec<u8>, BodyError> {
    let mut out = Vec::new();
    loop {
        let frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
        match frame {
            None => return Ok(out),
            Some(Err(_)) => return Err(BodyError::Read),
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    if out.len() + data.len() > max {
                        return Err(BodyError::TooLarge);
                    }
                    out.extend_from_slice(&data);
                }
            }
        }
    }
}

const KEYS: [&str; 6] = [
    "from",
    "to",
    "destination",
    "on_exists",
    "dry_run",
    "request_id",
];

/// 本文のオブジェクト。未知のキー・重複したキーは、**最初に見つけた 1 つ**を理由として持つ（本文の最後まで読む。途中で
/// 打ち切ると、JSON の文法の誤りと区別できない）。重複は、エスケープを戻したあとのキーで比べる（`"dry_run"` と `"dry\u005frun"`
/// は同じキー）。値が `null` のキーも、1 つと数える。
struct Strict(Result<Map<String, Value>, &'static str>);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visit;

        impl<'de> Visitor<'de> for Visit {
            type Value = Strict;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
                let mut object = Map::new();
                let mut problem: Option<&'static str> = None;
                while let Some(key) = map.next_key::<String>()? {
                    let value = map.next_value::<Value>()?;
                    if !KEYS.contains(&key.as_str()) {
                        problem.get_or_insert("unknown_field");
                    } else if object.contains_key(&key) {
                        problem.get_or_insert("duplicate_field");
                    } else {
                        object.insert(key, value);
                    }
                }
                Ok(Strict(problem.map_or(Ok(object), Err)))
            }
        }

        deserializer.deserialize_map(Visit)
    }
}

/// 本文（JSON）を [`RawRequest`] にする。未知のキー・重複したキー・オブジェクトでない本文・型の違いは、固定のコードで断る。
/// `null` は、キーが無いのと同じに扱う（必須の `from`・`destination` は、そのあとの検査で `invalid_date`・`invalid_destination`）。
/// 型の違いは、そのキーの検査のコード（`from`・`to` は `invalid_date`、`dry_run` は `invalid_body`）。
fn parse_body(bytes: &[u8]) -> Result<RawRequest, &'static str> {
    let Ok(Strict(checked)) = serde_json::from_slice::<Strict>(bytes) else {
        return Err("invalid_body");
    };
    let object = checked?;
    let text = |key: &str, code: ErrorCode| text_field(&object, key, code);
    let from = text("from", ErrorCode::InvalidDate)?;
    let to = text("to", ErrorCode::InvalidDate)?;
    let destination = text("destination", ErrorCode::InvalidDestination)?;
    let on_exists = text("on_exists", ErrorCode::InvalidOnExists)?;
    let request_id = text("request_id", ErrorCode::InvalidRequestId)?;
    let dry_run = match object.get("dry_run") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("invalid_body"),
    };
    Ok(RawRequest {
        from,
        to,
        destination,
        on_exists,
        dry_run,
        request_id,
    })
}

fn text_field(
    object: &Map<String, Value>,
    key: &str,
    code: ErrorCode,
) -> Result<Option<String>, &'static str> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(code.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn listen_needs_a_concrete_address_and_port() {
        for bad in ["0.0.0.0:9100", "[::]:9100", "[::ffff:0.0.0.0]:9100"] {
            assert_eq!(
                check_listen(sa(bad)),
                Err(ListenError::Unspecified),
                "{bad}"
            );
        }
        assert_eq!(check_listen(sa("127.0.0.1:0")), Err(ListenError::ZeroPort));
        for bad in ["224.0.0.1:9100", "255.255.255.255:9100", "[ff02::1]:9100"] {
            assert_eq!(check_listen(sa(bad)), Err(ListenError::NotUnicast), "{bad}");
        }
        // tailnet の帯の中（端も）とループバック（試験用）
        for ok in [
            "100.64.0.0:9100",
            "100.64.0.1:9100",
            "100.96.1.2:9100",
            "100.127.255.255:9100",
            "[fd7a:115c:a1e0::1]:9100",
            "[fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff]:9100",
            "127.0.0.1:9100",
            "127.1.2.3:9100",
            "[::1]:9100",
            "[::ffff:127.0.0.1]:9100",
            "[::ffff:100.64.0.1]:9100",
        ] {
            assert_eq!(check_listen(sa(ok)), Ok(()), "{ok}");
        }
    }

    #[test]
    fn listen_refuses_addresses_outside_the_tailnet_and_loopback() {
        for bad in [
            // LAN の私的アドレス
            "192.168.1.10:9100",
            "192.168.0.1:9100",
            "10.0.0.5:9100",
            "10.255.255.254:9100",
            "172.16.0.1:9100",
            "172.31.255.254:9100",
            // 文書用の予約アドレス・リンクローカル・グローバル
            "192.0.2.1:9100",
            "198.51.100.7:9100",
            "203.0.113.9:9100",
            "169.254.1.1:9100",
            "198.18.0.1:9100",
            "[2001:db8::1]:9100",
            "[fe80::1]:9100",
            "[fd00::1]:9100",
            "[2002::1]:9100",
            // tailnet の帯のすぐ外
            "[fd7a:115c:a1e1::1]:9100",
            "[fd7a:115b:a1e0::1]:9100",
            "[fd7b:115c:a1e0::1]:9100",
            // IPv4-mapped は IPv4 として判定する
            "[::ffff:192.168.1.10]:9100",
        ] {
            assert_eq!(check_listen(sa(bad)), Err(ListenError::NotTailnet), "{bad}");
        }
    }

    #[test]
    fn bodies_are_parsed_strictly() {
        let ok = parse_body(
            br#"{"from":"2030-03-10","to":null,"destination":"yamareco","dry_run":true,"request_id":"r-1"}"#,
        )
        .unwrap();
        assert_eq!(ok.from.as_deref(), Some("2030-03-10"));
        assert_eq!(ok.to, None);
        assert!(ok.dry_run);
        assert_eq!(ok.request_id.as_deref(), Some("r-1"));
        for (body, code) in [
            (&b""[..], "invalid_body"),
            (b"[]", "invalid_body"),
            (b"\"x\"", "invalid_body"),
            (b"{", "invalid_body"),
            (br#"{"from":"2030-03-10","extra":1}"#, "unknown_field"),
            (br#"{"From":"2030-03-10"}"#, "unknown_field"),
            (br#"{"from":20300310}"#, "invalid_date"),
            (br#"{"from":"2030-03-10","to":5}"#, "invalid_date"),
            (br#"{"destination":["yamareco"]}"#, "invalid_destination"),
            (br#"{"on_exists":false}"#, "invalid_on_exists"),
            (br#"{"request_id":1}"#, "invalid_request_id"),
            (br#"{"dry_run":"true"}"#, "invalid_body"),
            (br#"{"dry_run":1}"#, "invalid_body"),
            // 重複したキー（最後の値で上書きしない）
            (br#"{"dry_run":true,"dry_run":false}"#, "duplicate_field"),
            (br#"{"dry_run":false,"dry_run":true}"#, "duplicate_field"),
            (
                br#"{"from":"2030-03-10","from":"2030-03-11"}"#,
                "duplicate_field",
            ),
            (br#"{"from":"2030-03-10","from":null}"#, "duplicate_field"),
            (br#"{"to":null,"to":"2030-03-11"}"#, "duplicate_field"),
            (
                br#"{"dry_run":true,"dry\u005frun":false}"#,
                "duplicate_field",
            ),
            (
                br#"{"request_id":"a","destination":"b","request_id":"a"}"#,
                "duplicate_field",
            ),
            // 未知のキーと重複の両方があるときは、先に見つけたほう
            (br#"{"x":1,"from":"a","from":"b"}"#, "unknown_field"),
            (br#"{"from":"a","from":"b","x":1}"#, "duplicate_field"),
            (br#"{"x":1,"x":2}"#, "unknown_field"),
            // 文法の誤りは、重複より先に invalid_body
            (br#"{"from":"a","from":"b""#, "invalid_body"),
        ] {
            assert_eq!(
                parse_body(body).map(|_| ()),
                Err(code),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        // 必須のキーが無いのは、検査（validate）が決まったコードで断る
        let raw = parse_body(b"{}").unwrap();
        assert_eq!(raw.validate(), Err(ErrorCode::InvalidDate));
    }

    #[test]
    fn the_access_log_names_only_known_methods_and_route_templates() {
        use axum::http::Method;
        let id = "00112233445566778899aabbccddeeff";
        assert_eq!(route_label("/healthz"), "/healthz");
        assert_eq!(route_label("/v1/exports"), "/v1/exports");
        // ジョブ ID の形でも、パスの値は出さない（トークンに許す文字は、ジョブ ID の形を含む）
        assert_eq!(
            route_label(&format!("/v1/exports/{id}")),
            "/v1/exports/{job_id}"
        );
        assert_eq!(
            route_label("/v1/exports/abc%0aTOKEN"),
            "/v1/exports/{job_id}"
        );
        for other in [
            "/v1/exports/",
            "/v1/exports/a/b",
            "/anything/else",
            "/healthz/",
            "*",
            "",
        ] {
            assert_eq!(route_label(other), "other", "{other}");
        }
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::POST), "POST");
        for other in [Method::DELETE, Method::OPTIONS, Method::HEAD] {
            assert_eq!(method_label(&other), "other");
        }
        let token_like = Method::from_bytes(id.as_bytes()).unwrap();
        assert_eq!(method_label(&token_like), "other");
    }

    #[test]
    fn content_types_are_matched_by_the_media_type_only() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(CONTENT_TYPE, HeaderValue::from_str(v).unwrap());
            m
        };
        assert!(is_json(&h("application/json")));
        assert!(is_json(&h("Application/JSON; charset=utf-8")));
        for bad in [
            "text/plain",
            "application/jsonx",
            "application/x-www-form-urlencoded",
            "",
        ] {
            assert!(!is_json(&h(bad)), "{bad}");
        }
        assert!(!is_json(&HeaderMap::new()));
    }
}
