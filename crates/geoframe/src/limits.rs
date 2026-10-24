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
use std::time::Duration;

use axum::serve::Listener;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, Sleep, sleep, sleep_until};

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
/// - **読み取り**が `Pending` のまま、**最後に読み書きが進んだ時刻**から `idle_timeout` を超えたら
///   `TimedOut`（期限は「最後の進捗＋`idle_timeout`」で、タイマーが切れるたびに延ばさない）。
///   大きなファイルを遅い相手へ送っている最中（書き込みは進んでいる）は切らない。
pub struct LimitedIo<T> {
    inner: T,
    stall_timeout: Duration,
    idle_timeout: Duration,
    write_timer: Option<Pin<Box<Sleep>>>,
    read_timer: Option<Pin<Box<Sleep>>>,
    /// 最後に読み取り・書き込みのどちらかが進んだ（`Ready` を返した）時刻。無通信の期限の起点。
    last_progress: Instant,
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
            last_progress: Instant::now(),
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
                let last_progress = this.last_progress;
                // 期限は「最後の進捗＋idle」。書き込みが進んでいれば期限が先へ動くので、そのぶんだけ
                // 張り直す（「今＋idle」で延ばすと、最後の送信から最大 2×idle 残ってしまう）
                let timer = this
                    .read_timer
                    .get_or_insert_with(|| Box::pin(sleep_until(last_progress + idle)));
                while timer.as_mut().poll(cx).is_ready() {
                    let deadline = this.last_progress + idle;
                    if deadline > Instant::now() {
                        timer.as_mut().reset(deadline);
                    } else {
                        return Poll::Ready(Err(timed_out("無通信です")));
                    }
                }
                Poll::Pending
            }
            ready => {
                this.read_timer = None;
                this.last_progress = Instant::now();
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
                this.last_progress = Instant::now();
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
                this.last_progress = Instant::now();
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex, split};

    fn limits(idle_ms: u64) -> ConnLimits {
        ConnLimits {
            max_connections: 1,
            stall_timeout: Duration::from_secs(3600),
            idle_timeout: Duration::from_millis(idle_ms),
        }
    }

    /// 読み取りで待つ側が、`idle` 経過後に切られるまでの時刻（開始からの経過）を測る。
    /// `write_at` に書き込みの進捗を起こす（`None` なら何も書かない）。
    async fn cut_at(idle_ms: u64, write_at: Option<u64>) -> Duration {
        let (a, _peer) = duplex(64);
        let (mut reader, mut writer) = split(LimitedIo::new(a, limits(idle_ms), None));
        let start = Instant::now();
        let waiting = tokio::spawn(async move {
            let mut buf = [0u8; 8];
            let err = reader.read(&mut buf).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            Instant::now()
        });
        if let Some(ms) = write_at {
            sleep(Duration::from_millis(ms)).await;
            writer.write_all(b"x").await.unwrap();
        }
        waiting.await.unwrap() - start
    }

    #[tokio::test(start_paused = true)]
    async fn silent_connection_is_cut_after_the_idle_timeout() {
        let at = cut_at(1000, None).await;
        assert!(
            at >= Duration::from_millis(1000) && at <= Duration::from_millis(1005),
            "{at:?}"
        );
    }

    /// 最後に書き込みが進んだ時刻＋idle で切れる。以前は「タイマーが切れた時刻＋idle」で延ばしていたので、
    /// 300ms に送ると 2000ms まで残った（最後の送信から 1700ms）。
    #[tokio::test(start_paused = true)]
    async fn the_deadline_is_the_last_progress_plus_idle() {
        let at = cut_at(1000, Some(300)).await;
        assert!(
            at >= Duration::from_millis(1300) && at <= Duration::from_millis(1305),
            "{at:?}"
        );
    }

    /// 進捗が続く間は切れず、止まってから idle で切れる。
    #[tokio::test(start_paused = true)]
    async fn steady_writes_keep_the_connection_open() {
        let (a, _peer) = duplex(64);
        let (mut reader, mut writer) = split(LimitedIo::new(a, limits(1000), None));
        let start = Instant::now();
        let waiting = tokio::spawn(async move {
            let mut buf = [0u8; 8];
            reader.read(&mut buf).await.unwrap_err();
            Instant::now()
        });
        for _ in 0..5 {
            sleep(Duration::from_millis(600)).await;
            writer.write_all(b"x").await.unwrap();
        }
        let at = waiting.await.unwrap() - start;
        // 最後の送信は 3000ms
        assert!(
            at >= Duration::from_millis(4000) && at <= Duration::from_millis(4005),
            "{at:?}"
        );
    }
}
