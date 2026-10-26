//! 外部 AI 要求の枠（P4 / T4.2。設計は p4-design.md §4.2・§5「背景 1 枠と対話 1 枠」）。
//!
//! 背景の枠と対話の枠は**独立**で、それぞれ同時に 1 件まで。対話の要求が背景の要求に待たされることはない。
//! 対話の枠自体が使用中なら、短い待ち（呼び出し側が決める。HTTP の受付では 0）のあと [`LaneError::Busy`]（定型 503 に
//! 当たる）を返し、HTTP のワーカーを塞がない。対話の要求が枠を待っている間は、**新しい背景の要求は始めない**
//! （待っている対話を新しい背景より先にする。実行中の背景は止めない）。
//!
//! 枠は [`Permit`] で持ち、drop（panic を含む）で必ず返る。

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::stop::StopFlag;

/// 背景の枠を待つとき、停止の合図を確かめる間隔。
const STOP_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// AI ワーカーの背景ジョブ。
    Background,
    /// 利用者を待たせる要求（検索語の Embedding）。
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneError {
    /// 枠が使用中（定型 503 に当たる）。
    Busy,
    /// 停止の合図が来ている。
    Stopping,
}

#[derive(Default)]
struct State {
    background: bool,
    interactive: bool,
    /// 対話の枠を待っている要求の数。
    interactive_waiting: u32,
}

#[derive(Default)]
pub struct Lanes {
    state: Mutex<State>,
    changed: Condvar,
}

/// 枠の保持。drop で返る。
pub struct Permit {
    lanes: Arc<Lanes>,
    lane: Lane,
}

impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permit").field("lane", &self.lane).finish()
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self
            .lanes
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match self.lane {
            Lane::Background => state.background = false,
            Lane::Interactive => state.interactive = false,
        }
        drop(state);
        self.lanes.changed.notify_all();
    }
}

impl Lanes {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 待たずに取る。使用中なら [`LaneError::Busy`]。背景は、対話が枠を待っている間も `Busy`。
    pub fn try_acquire(self: &Arc<Self>, lane: Lane, stop: &StopFlag) -> Result<Permit, LaneError> {
        self.acquire(lane, Duration::ZERO, stop)
    }

    /// 最大 `wait` 待って取る。`wait` が 0 なら待たない。
    pub fn acquire(
        self: &Arc<Self>,
        lane: Lane,
        wait: Duration,
        stop: &StopFlag,
    ) -> Result<Permit, LaneError> {
        let deadline = Instant::now() + wait;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut waiting = false;
        let result = loop {
            if stop.is_stopped() {
                break Err(LaneError::Stopping);
            }
            let free = match lane {
                Lane::Interactive => !state.interactive,
                // 待っている対話があるうちは、新しい背景を始めない
                Lane::Background => !state.background && state.interactive_waiting == 0,
            };
            if free {
                match lane {
                    Lane::Interactive => state.interactive = true,
                    Lane::Background => state.background = true,
                }
                break Ok(Permit {
                    lanes: Arc::clone(self),
                    lane,
                });
            }
            let now = Instant::now();
            if now >= deadline {
                break Err(LaneError::Busy);
            }
            if lane == Lane::Interactive && !waiting {
                waiting = true;
                state.interactive_waiting += 1;
            }
            let slice = (deadline - now).min(STOP_POLL);
            state = self
                .changed
                .wait_timeout(state, slice)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        };
        if waiting {
            state.interactive_waiting -= 1;
            drop(state);
            // 待っていた対話が去ったので、待たされていた背景を起こす
            self.changed.notify_all();
        }
        result
    }

    #[cfg(test)]
    pub fn interactive_waiting(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .interactive_waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn lanes_are_independent_and_interactive_busy_is_immediate() {
        let lanes = Lanes::new();
        let stop = StopFlag::new();
        let bg = lanes.try_acquire(Lane::Background, &stop).unwrap();
        // 背景が塞がっていても、対話は取れる
        let ia = lanes.try_acquire(Lane::Interactive, &stop).unwrap();
        // 対話の枠自体が使用中なら、待たずに Busy
        let started = Instant::now();
        assert_eq!(
            lanes.try_acquire(Lane::Interactive, &stop).unwrap_err(),
            LaneError::Busy
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(
            lanes.try_acquire(Lane::Background, &stop).unwrap_err(),
            LaneError::Busy
        );
        drop(ia);
        drop(bg);
        // 返ればまた取れる（panic した持ち主でも返る）
        let again = thread::spawn({
            let lanes = Arc::clone(&lanes);
            let stop = stop.clone();
            move || {
                let _p = lanes.try_acquire(Lane::Background, &stop).unwrap();
                panic!("boom");
            }
        })
        .join();
        assert!(again.is_err());
        assert!(lanes.try_acquire(Lane::Background, &stop).is_ok());
        assert!(lanes.try_acquire(Lane::Interactive, &stop).is_ok());
    }

    #[test]
    fn a_waiting_interactive_goes_before_a_new_background() {
        let lanes = Lanes::new();
        let stop = StopFlag::new();
        let held = lanes.try_acquire(Lane::Interactive, &stop).unwrap();
        let (tx, rx) = mpsc::channel();
        let waiter = {
            let (lanes, stop, tx) = (Arc::clone(&lanes), stop.clone(), tx.clone());
            thread::spawn(move || {
                let p = lanes
                    .acquire(Lane::Interactive, Duration::from_secs(5), &stop)
                    .unwrap();
                tx.send("interactive").unwrap();
                thread::sleep(Duration::from_millis(100));
                drop(p);
            })
        };
        while lanes.interactive_waiting() == 0 {
            thread::sleep(Duration::from_millis(5));
        }
        let background = {
            let (lanes, stop) = (Arc::clone(&lanes), stop.clone());
            thread::spawn(move || {
                let p = lanes
                    .acquire(Lane::Background, Duration::from_secs(5), &stop)
                    .unwrap();
                tx.send("background").unwrap();
                drop(p);
            })
        };
        thread::sleep(Duration::from_millis(100));
        // 対話が待っている間、背景は始まらない
        assert!(rx.try_recv().is_err());
        drop(held);
        // 待っていた対話が枠を取ったあとは、対話の実行中でも背景は独立に始まる（どちらも進む）
        let mut done = [
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        ];
        done.sort_unstable();
        assert_eq!(done, ["background", "interactive"]);
        waiter.join().unwrap();
        background.join().unwrap();
    }

    #[test]
    fn stop_ends_the_wait_and_refuses_new_permits() {
        let lanes = Lanes::new();
        let stop = StopFlag::new();
        let _held = lanes.try_acquire(Lane::Background, &stop).unwrap();
        let waiter = {
            let (lanes, stop) = (Arc::clone(&lanes), stop.clone());
            thread::spawn(move || lanes.acquire(Lane::Background, Duration::from_secs(30), &stop))
        };
        thread::sleep(Duration::from_millis(60));
        stop.stop();
        assert_eq!(waiter.join().unwrap().unwrap_err(), LaneError::Stopping);
        assert_eq!(
            lanes.try_acquire(Lane::Interactive, &stop).unwrap_err(),
            LaneError::Stopping
        );
    }
}
