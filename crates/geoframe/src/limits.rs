//! 接続とファイル配信の上限。認証の無い LAN 内サーバーなので、読み取らない・遅いクライアントが
//! ソケットとファイル記述子を握り続けて、`/healthz` や `/api/v1/photos` まで止めないようにする。
//! 値と理由は docs/rust-migration/server.md §9.3。
//!
//! - **同時に配るデータ置き場のファイル数**（[`STATIC_FILE_SLOTS`]）: [`crate::statics::serve_data`] が
//!   ファイルを開く前に取り、応答の本文が捨てられるまで持つ。空きが無ければ待たずに 503。
//! - **接続の総数**（[`MAX_CONNECTIONS`]）と**送信の停滞**（[`STALL_TIMEOUT`]）・**無通信**
//!   （[`IDLE_TIMEOUT`]）の打ち切り: [`LimitedListener`] が受け付けた接続を [`LimitedIo`] で包む。
//!   送信が進まないクライアントを待つとき hyper は本文を読みに来ない（ソケットの書き込み待ち）ので、
//!   本文の側では測れず、hyper の下（IO）で測る。

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::serve::Listener;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Sleep, sleep};

/// データ置き場のファイルを同時に配る上限。1件ごとにファイル記述子を1つ握るので、
/// 接続数の上限と合わせて記述子の予算（server.md §6）に収める。
pub const STATIC_FILE_SLOTS: usize = 32;

/// 同時に受け付ける接続の上限。超えたぶんは accept せず、OS の待ち行列で待たせる。
/// 記述子の予算: 256（接続）＋32（配信のファイル）＋SQLite（API 4 本・ヘルス 1 本と `-wal`/`-shm`）
/// ＋標準入出力で 300 前後。systemd の既定（`LimitNOFILE=1024`）に十分収まる。
pub const MAX_CONNECTIONS: usize = 256;

/// 送信が進まない（クライアントが読まない）まま待つ上限。超えたら接続を切る。
/// 手元の LAN で大きな原本（数十 MB）を1回の送信に使う時間より十分長く、読まない相手に
/// 握らせ続ける時間としては短い値。
pub const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 読み書きのどちらも無い接続を待つ上限。keep-alive の待機と、リクエストのヘッダを
/// 少しずつしか送らない相手（slowloris）を切る。応答の生成（DB の順番待ちは最大 10 秒）より長く取る。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// [`LimitedListener`] の設定。本番は [`ConnLimits::default`]、テストは短い値を渡す。
#[derive(Clone, Copy, Debug)]
pub struct ConnLimits {
    pub max_connections: usize,
    pub stall_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for ConnLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            stall_timeout: STALL_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
        }
    }
}

/// 接続の総数を数え、受け付けた接続に停滞・無通信の打ち切りを付ける [`Listener`]。
pub struct LimitedListener {
    inner: TcpListener,
    limits: ConnLimits,
    slots: Arc<Semaphore>,
}

impl LimitedListener {
    pub fn new(inner: TcpListener, limits: ConnLimits) -> Self {
        Self {
            inner,
            slots: Arc::new(Semaphore::new(limits.max_connections)),
            limits,
        }
    }
}

impl Listener for LimitedListener {
    type Io = LimitedIo<TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        // 枠が空くまで accept しない（接続の総数の上限）。枠は接続の IO が捨てられるまで持つ
        let permit = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .expect("セマフォは閉じない");
        let (stream, addr) = Listener::accept(&mut self.inner).await;
        (LimitedIo::new(stream, self.limits, Some(permit)), addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// 送信の停滞と無通信を打ち切る IO。
///
/// - **書き込み・flush・shutdown** が `Pending` のまま `stall_timeout` を超えたら `TimedOut`。
///   進捗（`Ready`）があれば測り直す。
/// - **読み取り**が `Pending` のまま `idle_timeout` を超え、かつその間に書き込みの進捗も無ければ
///   `TimedOut`。大きなファイルを遅い相手へ送っている最中（書き込みは進んでいる）は切らない。
pub struct LimitedIo<T> {
    inner: T,
    stall_timeout: Duration,
    idle_timeout: Duration,
    write_timer: Option<Pin<Box<Sleep>>>,
    read_timer: Option<Pin<Box<Sleep>>>,
    last_write: Instant,
    _permit: Option<OwnedSemaphorePermit>,
}

impl<T> LimitedIo<T> {
    pub fn new(inner: T, limits: ConnLimits, permit: Option<OwnedSemaphorePermit>) -> Self {
        Self {
            inner,
            stall_timeout: limits.stall_timeout,
            idle_timeout: limits.idle_timeout,
            write_timer: None,
            read_timer: None,
            last_write: Instant::now(),
            _permit: permit,
        }
    }
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, what.to_owned())
}

/// 書き込み系が `Pending` を返したとき: 期限を張り（無ければ）、過ぎていればエラーにする。
fn stall_check<R>(
    timer: &mut Option<Pin<Box<Sleep>>>,
    limit: Duration,
    cx: &mut Context<'_>,
) -> Poll<io::Result<R>> {
    let timer = timer.get_or_insert_with(|| Box::pin(sleep(limit)));
    match timer.as_mut().poll(cx) {
        Poll::Ready(()) => Poll::Ready(Err(timed_out("送信が進みません"))),
        Poll::Pending => Poll::Pending,
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for LimitedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Pending => {
                let idle = this.idle_timeout;
                let timer = this.read_timer.get_or_insert_with(|| Box::pin(sleep(idle)));
                // 期限が来ても、直近に書き込みの進捗があれば切らずに張り直す
                while timer.as_mut().poll(cx).is_ready() {
                    if this.last_write.elapsed() < idle {
                        timer.as_mut().reset(tokio::time::Instant::now() + idle);
                    } else {
                        return Poll::Ready(Err(timed_out("無通信です")));
                    }
                }
                Poll::Pending
            }
            ready => {
                this.read_timer = None;
                ready
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for LimitedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Pending => stall_check(&mut this.write_timer, this.stall_timeout, cx),
            ready => {
                this.write_timer = None;
                this.last_write = Instant::now();
                ready
            }
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_write_vectored(cx, bufs) {
            Poll::Pending => stall_check(&mut this.write_timer, this.stall_timeout, cx),
            ready => {
                this.write_timer = None;
                this.last_write = Instant::now();
                ready
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Pending => stall_check(&mut this.write_timer, this.stall_timeout, cx),
            ready => {
                this.write_timer = None;
                ready
            }
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_shutdown(cx) {
            Poll::Pending => stall_check(&mut this.write_timer, this.stall_timeout, cx),
            ready => {
                this.write_timer = None;
                ready
            }
        }
    }
}
