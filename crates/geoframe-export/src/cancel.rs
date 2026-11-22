//! キャンセルの印（SIGINT・SIGTERM。design.md 8 章「同時実行」、4.1 の手順 11）。
//!
//! 親（`run`・`serve`）が signal で終わると、別のプロセスグループに入れた rclone だけが残り、実行枠（flock）だけが外れて、次の
//! ジョブの復旧が、進行中の `copy`・`moveto` と競合しうる。それを避けるため、signal は**その場で終わらず、印を立てるだけ**にし、
//! 実行中の子（rclone）のプロセスグループを止めて `wait` し終えるまで実行枠を持ってから終える（[`crate::rclone::Rclone`]）。
//!
//! - [`Cancel`] は同期の印（`Arc<AtomicBool>`。`Clone` で共有）。[`Cancel::cancel`] で立てる。立った印は下ろさない。
//! - [`install_signal_handlers`] は、SIGINT・SIGTERM の handler を入れる（印を立てるだけ。async-signal-safe）。
//!   バイナリの `main` だけが、[`crate::app::run_command_cancellable`] の `handle_signals` を真にして呼ばせる（試験のプロセスは
//!   並列で 1 つの印を共有できないので、プロセス内の試験は呼ばない）。`run` の前に入る。`serve` は入れず、tokio のシグナルで受けて
//!   [`Cancel::cancel`] を呼ぶ（[`crate::serve`]。どちらでも、立った印の見方は同じ）。
//!
//! 印は下ろせない。2 回目以降の signal も、印を立てるだけ（子を止めて `wait` し終えるまで、枠を持ったまま待つ。印を見ない待ちは、
//! 一覧 API の取得〔30 秒〕と写真 1 枚の取得〔60 秒〕だけで、どちらにも上限がある）。
//!
//! 立った印は、次のところで見る。見つけたら、取りかかっているジョブは、記録を**確定せずに**（Drive の段階を記録していれば
//! `running` のまま）次の復旧に渡す（[`crate::flow`]）。
//!
//! - [`crate::rclone::Rclone`]: 起動の前と、待ちのループの中（20ms ごと）。起動していれば、グループごと kill して `wait` し、
//!   出力を読むスレッドを `join` してから [`crate::rclone::RunError::Cancelled`] を返す。
//! - [`crate::publish::Recovery::settle_observe`]: 待ちの代わりに印を見る（30 秒 × 回数を眠らない）。
//! - [`crate::image::prepare_cancellable`]: 1 枚ごと。

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// キャンセルの印。`Clone` した複製は同じ印を指す。
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// 印を立てる（下ろせない）。どのスレッドからでもよい。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// handler が立てる印（[`install_signal_handlers`] が `Arc` を 1 つ漏らして置く）。
static HANDLER_TARGET: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn on_signal(_sig: libc::c_int) {
    // async-signal-safe なのは、静的な AtomicPtr の読みと、AtomicBool への書きだけ
    let p = HANDLER_TARGET.load(Ordering::SeqCst);
    if !p.is_null() {
        // SAFETY: `p` は `Arc::into_raw` で作ったもので、漏らしたまま（解放しない）なので、生きている。
        unsafe { (*p).store(true, Ordering::SeqCst) };
    }
}

/// SIGINT・SIGTERM を、`cancel` の印を立てる handler にする（以後、この 2 つの signal ではプロセスは終わらない）。
/// `SA_RESTART` を付ける（読み書きの途中の中断で、標準ライブラリの呼び出しが失敗しないように）。何度呼んでもよく、最後に
/// 渡した印だけが立つ（漏らす `Arc` は 1 回の呼び出しにつき 1 つ）。
pub fn install_signal_handlers(cancel: &Cancel) -> io::Result<()> {
    let raw = Arc::into_raw(Arc::clone(&cancel.0)).cast_mut();
    HANDLER_TARGET.store(raw, Ordering::SeqCst);
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: 0 で初期化した `sigaction` に、handler・空の mask・`SA_RESTART` を入れて渡すだけ。
        let rc = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut())
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_shares_the_mark_and_it_never_goes_down() {
        let a = Cancel::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
        assert!(a.is_cancelled());
    }
}
