//! 停止の合図。同期サイクル・キャッシュ保存・AI ワーカーのスレッドと HTTP サーバーが、SIGTERM のあとに
//! 止まるために使う。待ちながら合図を見られる（`wait` は合図が来たらすぐ戻る）ので、周期の待ちが停止を遅らせない。
//!
//! # 停止の共通期限
//!
//! 合図を立てた時刻（[`StopFlag::stop`] の最初の 1 回）が、停止全体の起点。起点から [`SHUTDOWN_BUDGET`]（60 秒）が
//! 共通の期限で（[`StopFlag::deadline`]）、HTTP の終了待ち・タイル掃除・同期・AI・キャッシュ保存・runtime の停止は
//! それぞれ**残り時間**を超えて待たない（[`StopFlag::phase_limit`]）。systemd の `TimeoutStopSec=90s` は、
//! この期限のあとに残る処理（プロセスの終了）の余裕。試験は短い予算（[`StopFlag::with_budget`]）を注入する。

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// SIGTERM から数える停止全体の期限。
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct StopFlag {
    /// 合図の時刻（`None` はまだ合図が来ていない）。
    state: Arc<(Mutex<Option<Instant>>, Condvar)>,
    budget: Duration,
}

impl Default for StopFlag {
    fn default() -> Self {
        Self::with_budget(SHUTDOWN_BUDGET)
    }
}

impl StopFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// 停止全体の期限を差し替えた合図（試験用に短くする）。
    pub fn with_budget(budget: Duration) -> Self {
        Self {
            state: Arc::new((Mutex::new(None), Condvar::new())),
            budget,
        }
    }

    /// 止まるよう合図する（待っているスレッドを起こす）。2 回目以降は起点を変えない。
    pub fn stop(&self) {
        let (lock, cv) = &*self.state;
        lock.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(Instant::now);
        cv.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped_at().is_some()
    }

    /// 合図が来た時刻（停止全体の起点）。
    pub fn stopped_at(&self) -> Option<Instant> {
        *self.state.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 停止全体の期限（合図が来ていなければ `None`）。
    pub fn deadline(&self) -> Option<Instant> {
        self.stopped_at().map(|at| at + self.budget)
    }

    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// 停止の各処理が待ってよい時間: `cap` と、共通期限までの残りの小さいほう。合図の前は `cap`。
    pub fn phase_limit(&self, cap: Duration) -> Duration {
        match self.deadline() {
            Some(deadline) => cap.min(deadline.saturating_duration_since(Instant::now())),
            None => cap,
        }
    }

    /// HTTP の終了待ちの期限（共通期限の 3/4）。残りの 1/4 は AI・同期・キャッシュ保存・runtime の停止に残す。
    pub fn http_limit(&self) -> Duration {
        self.phase_limit(self.budget * 3 / 4)
    }

    /// 最大 `timeout` 待つ。合図が来ていれば（来たときも）`true` で戻る。
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.state;
        let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = cv
            .wait_timeout_while(guard, timeout, |at| at.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        guard.is_some()
    }
}

/// スレッドの終了を最大 `limit` 待つ。終わっていれば `true`（パニックで終わったときは `false`）。
/// 待ち切れなければ `false` で、スレッドはそのまま残る（持っている `LockHold` もプロセスの終了まで残る）。
pub fn join_within(handle: JoinHandle<()>, limit: Duration) -> bool {
    let started = Instant::now();
    while !handle.is_finished() {
        if started.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.join().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_common_deadline_starts_at_the_first_stop_and_bounds_every_phase() {
        let flag = StopFlag::with_budget(Duration::from_millis(400));
        // 合図の前は期限が無い。各処理は自分の上限をそのまま使う
        assert_eq!(flag.deadline(), None);
        assert_eq!(
            flag.phase_limit(Duration::from_secs(10)),
            Duration::from_secs(10)
        );
        flag.stop();
        let first = flag.stopped_at().unwrap();
        // 2 回目の合図で起点は動かない
        std::thread::sleep(Duration::from_millis(30));
        flag.stop();
        assert_eq!(flag.stopped_at(), Some(first));
        assert_eq!(flag.deadline(), Some(first + Duration::from_millis(400)));
        // 各処理の上限は残り時間を超えない（10 秒の上限でも 400ms の予算の残りまで）
        assert!(flag.phase_limit(Duration::from_secs(10)) <= Duration::from_millis(370));
        assert!(flag.phase_limit(Duration::from_millis(5)) <= Duration::from_millis(5));
        // HTTP は予算の 3/4 まで
        assert!(flag.http_limit() <= Duration::from_millis(300));
        // 期限を過ぎたら 0
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(flag.phase_limit(Duration::from_secs(10)), Duration::ZERO);
        // 複製は同じ合図・同じ起点を共有する
        let clone = flag.clone();
        assert_eq!(clone.stopped_at(), Some(first));
    }
}
