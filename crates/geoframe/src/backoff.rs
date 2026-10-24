//! 失敗が続く項目を、しばらく飛ばすための記録（P2 / T2.4 の受入条件②）。
//!
//! 現行の `fetch_derivatives`・`local_derivatives` は、取得待ちを `LIMIT n` つきの SQL で選ぶ。恒久的に
//! 変換に失敗する写真（壊れた原本、読めない形式）があると、毎サイクルそれが先頭の枠を埋め、後続の
//! 写真がいつまでも取得されない。スキーマは変えず（Python と DB を共有するため）、**プロセスの中だけ**で
//! 失敗を覚えておく: 失敗した項目は次の再試行の時刻まで飛ばし、時刻は失敗が続くほど倍々に延ばす
//! （上限あり）。成功したら忘れる。プロセスが再起動すれば全部忘れ、もう一度試す（それで十分: 再起動は
//! まれで、1 回の試行は 1 サイクルの枠の一部を使うだけ）。
//!
//! 記録の数にも上限がある（Drive 側で消えた行の記録が残り続けないように、いっぱいになったら
//! 期限が切れたものから落とす）。
//!
//! 時刻は呼び出し側が渡す（試験で時間を進められる）。

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// 1 回目の失敗のあとに飛ばす時間。同期サイクルの間隔（既定 10 分）より長くして、次のサイクルでは
/// 必ず飛ばされるようにする。
pub const BASE_DELAY: Duration = Duration::from_secs(30 * 60);
/// 飛ばす時間の上限。これ以降は半日おきに試す。
pub const MAX_DELAY: Duration = Duration::from_secs(12 * 3600);
/// 覚えておく項目の数の上限。
pub const MAX_ENTRIES: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct Entry {
    /// 続けて失敗した回数。
    failures: u32,
    /// この時刻になるまで飛ばす。
    retry_at: Instant,
}

/// 失敗の記録。`K` は項目を表すキー（Drive の ID と md5、ローカル写真の ID など）。
#[derive(Debug)]
pub struct FailureMemo<K> {
    entries: HashMap<K, Entry>,
    base: Duration,
    max: Duration,
    capacity: usize,
}

impl<K: Hash + Eq + Clone> Default for FailureMemo<K> {
    fn default() -> Self {
        Self::with_policy(BASE_DELAY, MAX_DELAY, MAX_ENTRIES)
    }
}

impl<K: Hash + Eq + Clone> FailureMemo<K> {
    pub fn with_policy(base: Duration, max: Duration, capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            base,
            max,
            capacity: capacity.max(1),
        }
    }

    /// 今は飛ばすべきか（再試行の時刻がまだ先）。
    pub fn is_waiting(&self, key: &K, now: Instant) -> bool {
        self.entries.get(key).is_some_and(|e| e.retry_at > now)
    }

    /// 失敗を記録する。次の再試行は `base * 2^(失敗の回数 - 1)`（`max` が上限）あとになる。
    pub fn record_failure(&mut self, key: K, now: Instant) {
        if !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            self.make_room(now);
        }
        let failures = self
            .entries
            .get(&key)
            .map_or(1, |e| e.failures.saturating_add(1));
        let retry_at = now + self.delay_after(failures);
        self.entries.insert(key, Entry { failures, retry_at });
    }

    /// 成功（または、もう試す必要がなくなった）。記録を消す。
    pub fn record_success(&mut self, key: &K) {
        self.entries.remove(key);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `failures` 回続けて失敗したあとに飛ばす時間。
    fn delay_after(&self, failures: u32) -> Duration {
        // 2 の冪が溢れないよう、指数に上限を付ける（それ以前に `max` で頭打ちになる）
        let exp = failures.saturating_sub(1).min(20);
        self.base
            .checked_mul(1u32 << exp)
            .map_or(self.max, |d| d.min(self.max))
    }

    /// いっぱいなので 1 つ空ける: 再試行の時刻が過ぎたもの（もう飛ばしていない）を全部、無ければ
    /// いちばん早く過ぎるものを 1 つ落とす。
    fn make_room(&mut self, now: Instant) {
        self.entries.retain(|_, e| e.retry_at > now);
        if self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.retry_at)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest {
                self.entries.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn memo() -> FailureMemo<&'static str> {
        FailureMemo::with_policy(10 * MIN, 60 * MIN, 3)
    }

    #[test]
    fn a_failed_key_is_skipped_until_its_retry_time() {
        let mut m = memo();
        let t0 = Instant::now();
        assert!(!m.is_waiting(&"a", t0));
        m.record_failure("a", t0);
        assert!(m.is_waiting(&"a", t0));
        assert!(m.is_waiting(&"a", t0 + 9 * MIN));
        assert!(!m.is_waiting(&"a", t0 + 10 * MIN));
        // 他のキーには影響しない
        assert!(!m.is_waiting(&"b", t0));
    }

    #[test]
    fn the_wait_doubles_with_each_failure_up_to_the_cap() {
        let mut m = memo();
        let t0 = Instant::now();
        let mut now = t0;
        let mut waits = Vec::new();
        for _ in 0..6 {
            m.record_failure("a", now);
            let wait = (0..)
                .map(|i| i * MIN)
                .find(|d| !m.is_waiting(&"a", now + *d))
                .unwrap();
            waits.push(wait.as_secs() / 60);
            now += wait;
        }
        // 10, 20, 40, 60（上限）, 60, 60
        assert_eq!(waits, [10, 20, 40, 60, 60, 60]);
    }

    #[test]
    fn success_forgets_the_key() {
        let mut m = memo();
        let t0 = Instant::now();
        m.record_failure("a", t0);
        m.record_failure("a", t0);
        m.record_success(&"a");
        assert!(!m.is_waiting(&"a", t0));
        assert!(m.is_empty());
        // 忘れたので、次の失敗はまた 1 回目の長さ
        m.record_failure("a", t0);
        assert!(!m.is_waiting(&"a", t0 + 10 * MIN));
    }

    #[test]
    fn the_number_of_entries_is_bounded() {
        let mut m = memo();
        let t0 = Instant::now();
        m.record_failure("a", t0);
        m.record_failure("b", t0 + MIN);
        m.record_failure("c", t0 + 2 * MIN);
        assert_eq!(m.len(), 3);
        // いっぱいで、どれも飛ばしている最中: いちばん早く過ぎる "a" を落とす
        m.record_failure("d", t0 + 3 * MIN);
        assert_eq!(m.len(), 3);
        assert!(!m.is_waiting(&"a", t0 + 3 * MIN));
        assert!(m.is_waiting(&"b", t0 + 3 * MIN));
        assert!(m.is_waiting(&"d", t0 + 3 * MIN));
        // 期限が過ぎたものは先に落ちる（"b"・"c" は過ぎ、"d" は残る）
        m.record_failure("e", t0 + 30 * MIN);
        assert!(m.len() <= 3);
        assert!(m.is_waiting(&"e", t0 + 30 * MIN));
    }

    #[test]
    fn a_huge_number_of_failures_does_not_overflow() {
        let mut m: FailureMemo<u32> = FailureMemo::default();
        let t0 = Instant::now();
        for _ in 0..100 {
            m.record_failure(1, t0);
        }
        assert!(m.is_waiting(&1, t0 + MAX_DELAY - Duration::from_secs(1)));
        assert!(!m.is_waiting(&1, t0 + MAX_DELAY));
    }
}
