//! 「HTTP 要求を書き始める」境界での停止の確認（P4 / T4.2。設計は p4-design.md §5「停止」）。
//!
//! 停止の合図（[`StopFlag`]）を `send` の入口と名前解決の前だけで見ていると、名前解決・TCP 接続・TLS ハンドシェイクを
//! 待っている間に来た停止を見逃し、**停止後に**要求が相手へ出てしまう。そこで `ureq` の接続の連鎖
//! （[`Connector`]）の**いちばん外側**に [`GateConnector`] を置き、返す [`Transport`] の `transmit_output`
//! （＝HTTP 要求の最初のバイトを書く呼び出し）で停止を確かめる。TLS は内側のコネクターが `connect` の中で
//! ハンドシェイクまで終えて返すので、この確認は名前解決・接続・TLS の待ちのすべてのあとになる。
//!
//! - **新しい要求の最初の書き込みごとに**確かめる。接続プールの再利用（古い接続を使う要求）でも、同じ要求の
//!   中で接続が作り直された場合でも、各トランスポートの最初の書き込みで確認が走る。
//! - **一度書き始めたら、停止を理由に止めない**。ヘッダー・本文の残りの書き込み・応答の受信・結果の確定は
//!   通常どおり（有償の結果を一律に捨てない）。
//! - 止めたときのエラーは [`StopRequested`]（`ureq::Error::Other`）で、[`super::http`] が固定の
//!   `HttpError::Stopping` に分類する。待ち・停止（pause）・課金の試行には数えない。
//!
//! 要求ごとの停止の合図は、`ureq` の同期呼び出しが走る**呼び出しスレッドのスレッドローカル**に置く
//! （接続プールとコネクターは複数の枠・スレッドで共有されるため、エージェントに合図を持たせられない）。
//! [`arm`] が返す [`Armed`] を持っている間だけ有効で、合図が無い状態での書き込みは**拒否**する（フェイルクローズ）。
//!
//! # `ureq` の unversioned API
//!
//! [`Connector`]・[`Transport`] は `ureq` の semver の対象外。使う箇所は[`super::dest`]の Resolver、
//! [`super::http`] の `Agent::with_parts`、このファイルだけ。`ureq` を更新するときはこの 3 か所のコンパイルと
//! `tests_http.rs` の停止の試験（名前解決中・接続後・書き込み開始後・接続プール）を確認する。

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

use crate::stop::StopFlag;

/// 停止の合図で書き込みを止めた（`ureq::Error::Other` に入る固定の型。文字列は持たない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopRequested;

impl fmt::Display for StopRequested {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stop_requested")
    }
}

impl std::error::Error for StopRequested {}

/// `ureq` のエラーが [`StopRequested`] か。
pub fn is_stop(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Other(inner) => inner.downcast_ref::<StopRequested>().is_some(),
        _ => false,
    }
}

/// 1 回の `send` の間の状態。
struct State {
    stop: StopFlag,
    /// この要求で、すでに書き始めたトランスポート（その後の書き込みは確認しない）。
    writing: Option<u64>,
    started: bool,
}

thread_local! {
    static CURRENT: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 呼び出しスレッドに停止の合図を結ぶ。drop で外れる。
pub struct Armed(());

/// `ureq` を呼ぶ前に結ぶ。入れ子にはしない（`send` の中で `send` はしない）。
pub fn arm(stop: &StopFlag) -> Armed {
    CURRENT.with(|c| {
        let previous = c.borrow_mut().replace(State {
            stop: stop.clone(),
            writing: None,
            started: false,
        });
        debug_assert!(previous.is_none(), "gate armed twice");
    });
    Armed(())
}

impl Armed {
    pub(super) fn started(&self) -> bool {
        CURRENT.with(|c| c.borrow().as_ref().is_some_and(|s| s.started))
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

/// いま結んでいる合図が立っているか（名前解決のあと、接続を始める前の確認用）。結んでいなければ `false`。
pub fn stopped() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|s| s.stop.is_stopped()))
}

/// トランスポート `id` がこの要求で書く許可。最初の書き込みのときだけ停止を確かめる。
fn permit_write(id: u64) -> Result<(), ureq::Error> {
    CURRENT.with(|c| {
        let mut state = c.borrow_mut();
        let Some(state) = state.as_mut() else {
            // 合図が結ばれていない書き込みは出さない
            return Err(ureq::Error::Other(Box::new(StopRequested)));
        };
        if state.writing == Some(id) {
            return Ok(());
        }
        if state.stop.is_stopped() {
            return Err(ureq::Error::Other(Box::new(StopRequested)));
        }
        state.writing = Some(id);
        // 最初の write 試行からは、実バイト数が不明でも保守的に送信済み。
        state.started = true;
        Ok(())
    })
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 連鎖のいちばん外側に置くコネクター。トランスポートを包むだけで、接続はしない。
#[derive(Debug, Default)]
pub struct GateConnector;

impl Connector<Box<dyn Transport>> for GateConnector {
    type Out = GateTransport;

    fn connect(
        &self,
        _details: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(GateTransport::new))
    }
}

/// 最初の書き込みの前に停止を確かめるトランスポート。ほかは内側へそのまま渡す。
pub struct GateTransport {
    id: u64,
    inner: Box<dyn Transport>,
}

impl GateTransport {
    fn new(inner: Box<dyn Transport>) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            inner,
        }
    }
}

impl fmt::Debug for GateTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GateTransport").finish_non_exhaustive()
    }
}

impl Transport for GateTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        if amount > 0 {
            permit_write(self.id)?;
        }
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.inner.await_input(timeout)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

/// 試験だけ: 内側のコネクターが接続（TCP・TLS）を終えて返したあと、ゲートの前で呼ぶ処理を差し込む
/// （「接続・TLS の待ちの間に停止が来た」順序を固定するため）。本番の構築口には無い。
#[cfg(test)]
pub struct HookConnector(pub std::sync::Arc<dyn Fn() + Send + Sync>);

#[cfg(test)]
impl fmt::Debug for HookConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookConnector").finish_non_exhaustive()
    }
}

#[cfg(test)]
impl Connector<Box<dyn Transport>> for HookConnector {
    type Out = Box<dyn Transport>;

    fn connect(
        &self,
        _details: &ConnectionDetails,
        chained: Option<Box<dyn Transport>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        if chained.is_some() {
            (self.0)();
        }
        Ok(chained)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ureq::unversioned::transport::LazyBuffers;

    /// 書き込みの回数を数えるだけのトランスポート。
    struct Counting {
        buffers: LazyBuffers,
        writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl fmt::Debug for Counting {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Counting")
        }
    }

    impl Transport for Counting {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }
        fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
            Ok(false)
        }
        fn is_open(&mut self) -> bool {
            true
        }
    }

    fn timeout() -> NextTimeout {
        NextTimeout::default()
    }

    /// 1 本のトランスポート（プールされた接続に当たる）を要求ごとに使い回す。要求ごとの最初の書き込みで
    /// 停止が確かめられ、書き始めたあとは停止しても残りの書き込みが通る。
    #[test]
    fn each_requests_first_write_is_checked_even_on_a_reused_transport() {
        let writes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut transport = GateTransport::new(Box::new(Counting {
            buffers: LazyBuffers::new(64, 64),
            writes: writes.clone(),
        }));
        let write = |t: &mut GateTransport| t.transmit_output(1, timeout());

        // 1 つ目の要求: 書き始めたあとに停止が来ても、残りは通る
        let stop = StopFlag::new();
        {
            let _armed = arm(&stop);
            assert!(write(&mut transport).is_ok());
            stop.stop();
            assert!(write(&mut transport).is_ok());
            assert!(write(&mut transport).is_ok());
        }
        assert_eq!(writes.load(Ordering::SeqCst), 3);

        // 2 つ目の要求（同じトランスポート）: 停止済みなら最初の書き込みで止まる
        {
            let _armed = arm(&stop);
            assert!(is_stop(&write(&mut transport).unwrap_err()));
        }
        assert_eq!(writes.load(Ordering::SeqCst), 3);

        // 3 つ目の要求: 停止していなければ通る。別のトランスポートでも最初の書き込みで確かめる
        let fresh = StopFlag::new();
        let mut other = GateTransport::new(Box::new(Counting {
            buffers: LazyBuffers::new(64, 64),
            writes: writes.clone(),
        }));
        {
            let _armed = arm(&fresh);
            assert!(write(&mut transport).is_ok());
            fresh.stop();
            assert!(write(&mut transport).is_ok());
            // 同じ要求の中で作り直された接続（別のトランスポート）の最初の書き込みは確かめられる
            assert!(is_stop(&write(&mut other).unwrap_err()));
        }
        // 合図が結ばれていない書き込みは出さない
        assert!(is_stop(&write(&mut transport).unwrap_err()));
        assert!(!stopped());
    }
}
