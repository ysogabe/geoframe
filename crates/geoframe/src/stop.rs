//! 停止の合図。同期サイクルのスレッドとキャッシュ保存のスレッドが、SIGTERM のあとに止まるために使う。
//! 待ちながら合図を見られる（`wait` は合図が来たらすぐ戻る）ので、周期の待ちが停止を遅らせない。

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct StopFlag(Arc<(Mutex<bool>, Condvar)>);

impl StopFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// 止まるよう合図する（待っているスレッドを起こす）。
    pub fn stop(&self) {
        let (lock, cv) = &*self.0;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 最大 `timeout` 待つ。合図が来ていれば（来たときも）`true` で戻る。
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.0;
        let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = cv
            .wait_timeout_while(guard, timeout, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn wait_returns_early_on_stop_and_times_out_otherwise() {
        let flag = StopFlag::new();
        let start = Instant::now();
        assert!(!flag.wait(Duration::from_millis(30)));
        assert!(start.elapsed() >= Duration::from_millis(30));
        assert!(!flag.is_stopped());

        let waiter = {
            let flag = flag.clone();
            std::thread::spawn(move || flag.wait(Duration::from_secs(30)))
        };
        std::thread::sleep(Duration::from_millis(20));
        flag.stop();
        assert!(waiter.join().unwrap());
        assert!(flag.is_stopped());
        // 合図のあとは待たない
        let start = Instant::now();
        assert!(flag.wait(Duration::from_secs(30)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
