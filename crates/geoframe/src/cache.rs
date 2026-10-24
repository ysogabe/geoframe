//! 永続キャッシュ `DATA_DIR/.cache.json`（現行 `store.py` の `load_cache`・`purge_cache_legacy`・
//! `save_cache`・`cache_saver`。棚卸し B0・B7）。
//!
//! # ファイルの形（Python と読み書きの互換）
//!
//! JSON のオブジェクト 1 つ。キーは次の 3 種類だけが現役:
//!
//! | キー | 値 | 書く段 |
//! |---|---|---|
//! | `geo:<緯度3桁>,<経度3桁>` | `{place, elev, muni_cd, pref, city}` | 地名の enrich（T2.5） |
//! | `geocode_osm:[<国コード>:]<検索語>` | 候補の配列 | 地名検索（P3・P4） |
//! | `muni_table` | 市区町村コード→`[都道府県, 市区町村]` | 地名の enrich（T2.5） |
//!
//! 値は [`serde_json::Value`] のまま持つ（`geo:` の中身を型に落とさないので、往復で欠けない）。
//! 書き出しは Python の `json.dumps(ensure_ascii=False, indent=1)` と同じ形（インデント 1、日本語は
//! そのまま）にする。Python が書いたファイルを Rust が、Rust が書いたファイルを Python が、そのまま
//! 読める。**違うのはキーの並びだけ**: Python は挿入順、Rust は辞書順で書く（`serde_json` の
//! `preserve_order` を入れると、他の応答の JSON のキーの並びまで変わるため入れない）。初めて Rust が
//! 保存したときに 1 度だけ並びが変わり、以後は安定する。読むときの並びは問わない。
//!
//! **座標を 3 桁に丸めた値がキーに入るので、ファイルは位置情報を含む**。リポジトリ・テストの実データ・
//! ログに出さない（テストは合成の値だけ）。ログに出すのは件数だけ。
//!
//! # 振る舞い
//!
//! - [`Cache::load`] は、無い・読めない・オブジェクトでないときは空から始める（現行と同じ）。
//! - [`Cache::purge_legacy`] は、起動時に 1 回、現役でないキー（旧 `exif:` `exif2:` `adsbdb:` `airport:`
//!   `intel2:` など）を捨てる。あれば呼び出し側が即保存する。
//! - [`Cache::save_if_due`] は、変更があり、前回の保存から [`SAVE_INTERVAL`] 以上たっているときだけ書く
//!   （SD/eMMC の寿命のため、最短 60 秒間隔）。一時ファイル `.cache.json.tmp` に書いて `rename` で
//!   差し替える（書きかけで落ちても現役のキーを失わない）。失敗したら次の tick で再試行する。
//!   保存の全体は保存用の排他で 1 つずつにする。停止時の [`Cache::save_final`] は、保存スレッドが
//!   I/O の最中ならそれを期限まで待ち、順番が来なければ書かずに省略する（重ねて書かない）。
//! - 凍結（[`Cache::load_frozen`]）は `--dry-run` 用で、`put`・`purge`・保存をしない。
//! - メモリだけ（[`Cache::load_memory_only`]・[`Cache::memory_only`]）は、取り込みが無効（`GEOFRAME_INGEST=0`）の
//!   `serve` の地名検索用で、読み込み（無ければ空）と `put` はするが、**ディスクへは書かない**（`purge`・保存をしない）。
//!   本番の書き手は P5 の切替まで Python なので、`.cache.json` を Rust が書かない。プロセスの中だけで、同じ語を
//!   Nominatim へ何度も問い合わせない。再起動すると消える。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::ingest::LockHold;
use crate::stop::StopFlag;

/// 実書き込みの最短間隔（現行の `CACHE_SAVE_INTERVAL_S`）。
pub const SAVE_INTERVAL: Duration = Duration::from_secs(60);
/// 保存スレッドの起床間隔（現行の `CACHE_SAVER_TICK_S`）。
pub const SAVER_TICK: Duration = Duration::from_secs(10);

const LIVE_PREFIXES: [&str; 2] = ["geo:", "geocode_osm:"];
const LIVE_EXACT: [&str; 1] = ["muni_table"];

fn is_live(key: &str) -> bool {
    LIVE_EXACT.contains(&key) || LIVE_PREFIXES.iter().any(|p| key.starts_with(p))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 変更が無い。
    Clean,
    /// 前回の保存から間隔が足りない。
    TooSoon,
    Saved,
    /// 書けなかった（次の tick で再試行する）。
    Failed,
    /// 別のスレッドが保存の最中で、期限内に順番が来なかったので書かなかった（`save_final` だけ）。
    Skipped,
}

struct State {
    map: Map<String, Value>,
    dirty: bool,
    /// 前回保存した時刻。まだ保存していなければ `None`（最初の保存は間隔を待たない）。
    saved_at: Option<Instant>,
}

/// キャッシュの使い方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 読み書きと保存（取り込みが有効）。
    Persistent,
    /// 読むだけ（`--dry-run`）。
    Frozen,
    /// 読み書きはするが、ディスクへは書かない（取り込みが無効の `serve`）。
    MemoryOnly,
}

pub struct Cache {
    file: PathBuf,
    tmp: PathBuf,
    mode: Mode,
    state: Mutex<State>,
    /// 保存の全体（スナップショット〜一時ファイルへの書き込み〜`fsync`〜`rename`）を 1 つずつにする。
    /// 保存中は `state` の Mutex を放すうえ、一時ファイル名は共通なので、2 つの保存が重なると
    /// truncate・write・rename が競合して更新の欠落や JSON の破損が起こりうる。
    save_lock: Mutex<()>,
}

impl Cache {
    /// `.cache.json` を読む。無い・壊れている・オブジェクトでないときは空から始める。
    pub fn load(data_dir: &Path) -> Self {
        Self::load_with(data_dir, Mode::Persistent)
    }

    /// 読むだけ（`put`・`purge_legacy`・保存はしない）。`--dry-run` 用。
    pub fn load_frozen(data_dir: &Path) -> Self {
        Self::load_with(data_dir, Mode::Frozen)
    }

    /// `.cache.json` を読み、`put` はメモリだけに入れる（`purge_legacy`・保存はしない）。取り込みが無効の `serve` 用。
    pub fn load_memory_only(data_dir: &Path) -> Self {
        Self::load_with(data_dir, Mode::MemoryOnly)
    }

    /// 空で始めるメモリだけのキャッシュ（ファイルを読まない）。
    pub fn memory_only() -> Self {
        Self {
            file: PathBuf::new(),
            tmp: PathBuf::new(),
            mode: Mode::MemoryOnly,
            state: Mutex::new(State {
                map: Map::new(),
                dirty: false,
                saved_at: None,
            }),
            save_lock: Mutex::new(()),
        }
    }

    fn load_with(data_dir: &Path, mode: Mode) -> Self {
        let file = data_dir.join(".cache.json");
        let map = match fs::read_to_string(&file) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(map)) => map,
                _ => {
                    // 中身は出さない（位置を含む）
                    tracing::warn!(".cache.json を読めないので空から始めます");
                    Map::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(e) => {
                tracing::warn!(".cache.json を読めないので空から始めます: {}", e.kind());
                Map::new()
            }
        };
        Self {
            tmp: data_dir.join(".cache.json.tmp"),
            file,
            mode,
            state: Mutex::new(State {
                map,
                dirty: false,
                saved_at: None,
            }),
            save_lock: Mutex::new(()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn len(&self) -> usize {
        self.state().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 全体の写し（試験用）。
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Map<String, Value> {
        self.state().map.clone()
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.state().map.get(key).cloned()
    }

    pub fn put(&self, key: &str, value: Value) {
        if self.mode == Mode::Frozen {
            return;
        }
        let mut state = self.state();
        state.map.insert(key.to_string(), value);
        state.dirty = true;
    }

    /// `prefix` で始まるキーが `max` 件に達していて、`key` がまだ無いときは入れずに `false`。それ以外は
    /// [`Cache::put`] と同じ（上書きは件数が増えないので常に入る）。ほかの名前空間（`geo:` など）は数えず、
    /// 捨てもしない。検索語のように、利用者の入力で件数が増えるキーの総量を抑えるのに使う。
    pub fn put_capped(&self, key: &str, value: Value, prefix: &str, max: usize) -> bool {
        if self.mode == Mode::Frozen {
            return true;
        }
        let mut state = self.state();
        if !state.map.contains_key(key)
            && state.map.keys().filter(|k| k.starts_with(prefix)).count() >= max
        {
            return false;
        }
        state.map.insert(key.to_string(), value);
        state.dirty = true;
        true
    }

    /// 現役でないキーの数（捨てずに数えるだけ）。
    pub fn legacy_count(&self) -> usize {
        self.state().map.keys().filter(|k| !is_live(k)).count()
    }

    /// 現役でないキーを捨てる。捨てた数を返す。
    pub fn purge_legacy(&self) -> usize {
        if self.mode != Mode::Persistent {
            return 0;
        }
        let mut state = self.state();
        let before = state.map.len();
        state.map.retain(|k, _| is_live(k));
        let removed = before - state.map.len();
        if removed > 0 {
            state.dirty = true;
        }
        removed
    }

    /// 変更があり、前回の保存から [`SAVE_INTERVAL`] 以上たっているときだけ書く。
    pub fn save_if_due(&self, now: Instant) -> SaveOutcome {
        self.save_with(now, false, None, |body| self.write_atomically(body))
    }

    /// 間隔を待たずに書く（変更があるときだけ）。起動時の旧キー破棄のあと。ほかの保存が終わるまで待つ。
    pub fn save_now(&self) -> SaveOutcome {
        self.save_with(Instant::now(), true, None, |body| {
            self.write_atomically(body)
        })
    }

    /// 停止時の最後の保存。ほかの保存（保存スレッドが I/O の最中に止まりきらなかった場合）の終了を
    /// `wait` まで待ち、それでも順番が来なければ**書かずに** [`SaveOutcome::Skipped`] を返す。
    /// 無期限には待たない（停止が終わらなくなる）。重ねて書くよりは、この回の更新を捨てるほうを選ぶ。
    pub fn save_final(&self, wait: Duration) -> SaveOutcome {
        self.save_with(Instant::now(), true, Some(wait), |body| {
            self.write_atomically(body)
        })
    }

    /// 保存の本体。`wait` が `None` なら排他を無期限に待つ。`write` は本文を書く処理（試験で遅延や
    /// 失敗を差し込めるよう、呼び出し側から渡す。本番の呼び出しは [`Cache::write_atomically`] を渡す）。
    fn save_with(
        &self,
        now: Instant,
        force: bool,
        wait: Option<Duration>,
        write: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> SaveOutcome {
        if self.mode != Mode::Persistent {
            return SaveOutcome::Clean;
        }
        let Some(_exclusive) = self.lock_save(wait) else {
            tracing::warn!(
                "別の保存が終わらないので、キャッシュの最後の保存を省略します（直近の更新は失われます）"
            );
            return SaveOutcome::Skipped;
        };
        let body = {
            let mut state = self.state();
            if !state.dirty {
                return SaveOutcome::Clean;
            }
            if !force
                && state
                    .saved_at
                    .is_some_and(|at| now.saturating_duration_since(at) < SAVE_INTERVAL)
            {
                return SaveOutcome::TooSoon;
            }
            // ロックの下でスナップショットを取る（書いている間に put されても失わない）
            let body = to_python_json(&state.map);
            state.dirty = false;
            state.saved_at = Some(now);
            body
        };
        match write(&body) {
            Ok(()) => SaveOutcome::Saved,
            Err(e) => {
                self.state().dirty = true; // 次の tick で再試行
                tracing::warn!("キャッシュの保存に失敗しました（次回再試行）: {}", e.kind());
                SaveOutcome::Failed
            }
        }
    }

    /// 保存の排他を取る。`wait` が `Some` なら、その時間までしか待たず、取れなければ `None`。
    fn lock_save(&self, wait: Option<Duration>) -> Option<MutexGuard<'_, ()>> {
        let Some(wait) = wait else {
            return Some(
                self.save_lock
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
        };
        let deadline = Instant::now() + wait;
        loop {
            match self.save_lock.try_lock() {
                Ok(guard) => return Some(guard),
                Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 一時ファイルに書いて差し替える。書きかけで落ちても現役のキーを失わない。
    fn write_atomically(&self, body: &[u8]) -> std::io::Result<()> {
        let mut file = fs::File::create(&self.tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp, &self.file)
    }
}

/// Python の `json.dumps(obj, ensure_ascii=False, indent=1)` と同じ形（末尾の改行は付けない）。
fn to_python_json(map: &Map<String, Value>) -> Vec<u8> {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    map.serialize(&mut serializer)
        .expect("JSON を直列化できません");
    out
}

/// 保存スレッド（現行の `cache_saver`）。[`SAVER_TICK`] ごとに起きて [`Cache::save_if_due`] を呼ぶ。
///
/// 停止の合図で終わるだけで、**最後の保存はしない**。最後の保存は、キャッシュに書く側（同期サイクルの
/// スレッド）が止まったあとに呼び出し側が [`Cache::save_final`] で行う（`cli::stop_ingest_threads`）。
/// 同じ合図で保存側が先に最後の保存を終えると、あとから書く側が `put` した更新が失われるため。
///
/// `lock` は保存スレッドが終わるまで持つ（停止の期限を超えて I/O の最中に残っても、その間は
/// `.ingest.lock` が外れず、別の `geoframe` が同じ置き場に書き始めない）。
pub fn spawn_saver(
    cache: Arc<Cache>,
    stop: StopFlag,
    lock: LockHold,
) -> std::thread::JoinHandle<()> {
    spawn_saver_with(cache, stop, SAVER_TICK, lock)
}

fn spawn_saver_with(
    cache: Arc<Cache>,
    stop: StopFlag,
    tick: Duration,
    lock: LockHold,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("cache-saver".into())
        .spawn(move || {
            let _lock = lock;
            while !stop.wait(tick) {
                cache.save_if_due(Instant::now());
            }
        })
        .expect("キャッシュ保存スレッドを起動できません")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use serde_json::json;

    fn write_file(dir: &TempDir, text: &str) {
        fs::write(dir.path().join(".cache.json"), text).unwrap();
    }

    fn read_file(dir: &TempDir) -> String {
        fs::read_to_string(dir.path().join(".cache.json")).unwrap()
    }

    // 合成の値だけ。座標らしいキーも、実在の場所とは無関係な丸めの値
    const SYNTH: &str = r#"{
 "geo:10.000,20.000": {
  "city": "テスト市",
  "elev": 12.5,
  "muni_cd": "99999",
  "place": "テスト県テスト市",
  "pref": "テスト県"
 },
 "geocode_osm:jp:テスト": [],
 "muni_table": {
  "99999": [
   "テスト県",
   "テスト市"
  ]
 }
}"#;

    #[test]
    fn put_capped_limits_one_namespace_only() {
        let cache = Cache::memory_only();
        cache.put("geo:1.000,2.000", json!({}));
        assert!(cache.put_capped("n:a", json!(1), "n:", 2));
        assert!(cache.put_capped("n:b", json!(2), "n:", 2));
        // 満杯: 新しいキーは入れない。上書きは入る。ほかの名前空間は数えない・捨てない
        assert!(!cache.put_capped("n:c", json!(3), "n:", 2));
        assert!(cache.put_capped("n:a", json!(9), "n:", 2));
        assert_eq!(cache.get("n:a"), Some(json!(9)));
        assert_eq!(cache.get("n:c"), None);
        cache.put("geo:3.000,4.000", json!({}));
        assert_eq!(cache.len(), 4);
        assert!(cache.get("geo:1.000,2.000").is_some());
    }

    #[test]
    fn missing_or_broken_files_start_empty() {
        let dir = TempDir::new("cache-empty");
        assert!(Cache::load(dir.path()).is_empty());
        for bad in ["", "not json", "[1,2]", "\"x\"", "null", "{\"a\":"] {
            write_file(&dir, bad);
            assert!(Cache::load(dir.path()).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn python_formatted_file_round_trips_byte_for_byte() {
        let dir = TempDir::new("cache-roundtrip");
        write_file(&dir, SYNTH);
        let cache = Cache::load(dir.path());
        assert_eq!(cache.len(), 3);
        assert_eq!(
            cache.get("geo:10.000,20.000").unwrap()["pref"],
            json!("テスト県")
        );
        // 変更がなければ書かない
        assert_eq!(cache.save_now(), SaveOutcome::Clean);
        cache.put("geo:10.000,20.000", cache.get("geo:10.000,20.000").unwrap());
        assert_eq!(cache.save_now(), SaveOutcome::Saved);
        // Python の indent=1・ensure_ascii=False と同じ形（Rust はキーを辞書順に書く。この合成データは
        // 辞書順で書いてある）
        assert_eq!(read_file(&dir), SYNTH);
        assert!(!dir.path().join(".cache.json.tmp").exists());
    }

    #[test]
    fn output_matches_python_json_dumps() {
        // `json.dumps({"b": [], "a": {}, "c": [1, 2.5, "あ/\"\n"], "d": None, "e": True}, ensure_ascii=False, indent=1)`
        // の出力（Python 3.13 で確認した固定値）。キーは辞書順に入れる
        let python = "{\n \"a\": {},\n \"b\": [],\n \"c\": [\n  1,\n  2.5,\n  \"あ/\\\"\\n\"\n ],\n \"d\": null,\n \"e\": true\n}";
        let mut map = Map::new();
        map.insert("a".into(), json!({}));
        map.insert("b".into(), json!([]));
        map.insert("c".into(), json!([1, 2.5, "あ/\"\n"]));
        map.insert("d".into(), Value::Null);
        map.insert("e".into(), json!(true));
        assert_eq!(String::from_utf8(to_python_json(&map)).unwrap(), python);
        assert_eq!(to_python_json(&Map::new()), b"{}");
    }

    #[test]
    fn legacy_keys_are_purged_and_live_ones_kept() {
        let dir = TempDir::new("cache-purge");
        write_file(
            &dir,
            r#"{"exif:a": 1, "exif2:b": {}, "adsbdb:c": [], "airport:d": 0, "intel2:e": "x",
                "geo:1.000,2.000": {}, "geocode_osm:q": [], "muni_table": {}, "geocodex": 1}"#,
        );
        let cache = Cache::load(dir.path());
        assert_eq!(cache.legacy_count(), 6); // geocodex も現役のどれにも前方一致しない
        assert_eq!(cache.purge_legacy(), 6);
        assert_eq!(cache.len(), 3);
        // 破棄したら即保存できる（間隔を待たない）
        assert_eq!(cache.save_now(), SaveOutcome::Saved);
        let again = Cache::load(dir.path());
        assert_eq!(again.len(), 3);
        assert!(again.get("geo:1.000,2.000").is_some());
        assert_eq!(again.purge_legacy(), 0);
        assert_eq!(again.save_now(), SaveOutcome::Clean);
    }

    #[test]
    fn saves_are_debounced_to_one_per_interval() {
        let dir = TempDir::new("cache-debounce");
        let cache = Cache::load(dir.path());
        let t0 = Instant::now();
        // 最初の保存は間隔を待たない（現行の `_cache_saved_at = 0.0`）
        cache.put("geo:1", json!(1));
        assert_eq!(cache.save_if_due(t0), SaveOutcome::Saved);
        // 変更があっても 60 秒未満は書かない
        cache.put("geo:2", json!(2));
        assert_eq!(
            cache.save_if_due(t0 + Duration::from_secs(59)),
            SaveOutcome::TooSoon
        );
        assert!(!read_file(&dir).contains("geo:2"));
        assert_eq!(
            cache.save_if_due(t0 + Duration::from_secs(60)),
            SaveOutcome::Saved
        );
        assert!(read_file(&dir).contains("geo:2"));
        // 変更が無ければ、時間がたっても書かない
        assert_eq!(
            cache.save_if_due(t0 + Duration::from_secs(600)),
            SaveOutcome::Clean
        );
    }

    #[test]
    fn a_failed_save_is_retried() {
        let dir = TempDir::new("cache-fail");
        let cache = Cache::load(dir.path());
        cache.put("geo:1", json!(1));
        // 書き込み先のディレクトリを無くして失敗させる
        let missing = dir.path().join("gone");
        let broken = Cache {
            file: missing.join(".cache.json"),
            tmp: missing.join(".cache.json.tmp"),
            mode: Mode::Persistent,
            state: Mutex::new(State {
                map: cache.state().map.clone(),
                dirty: true,
                saved_at: None,
            }),
            save_lock: Mutex::new(()),
        };
        assert_eq!(broken.save_now(), SaveOutcome::Failed);
        assert!(broken.state().dirty, "失敗したら次の tick で再試行する");
        fs::create_dir_all(&missing).unwrap();
        assert_eq!(broken.save_now(), SaveOutcome::Saved);
    }

    #[test]
    fn frozen_cache_never_writes() {
        let dir = TempDir::new("cache-frozen");
        write_file(&dir, r#"{"exif:x": 1, "geo:1": 2}"#);
        let cache = Cache::load_frozen(dir.path());
        assert_eq!(cache.legacy_count(), 1); // 数えられる
        assert_eq!(cache.purge_legacy(), 0);
        cache.put("geo:2", json!(3));
        assert_eq!(cache.save_now(), SaveOutcome::Clean);
        assert_eq!(read_file(&dir), r#"{"exif:x": 1, "geo:1": 2}"#);
        assert!(cache.get("geo:2").is_none());
    }

    #[test]
    fn memory_only_cache_reads_and_puts_but_never_writes_the_disk() {
        let dir = TempDir::new("cache-memory");
        write_file(&dir, r#"{"exif:x": 1, "geo:1": 2}"#);
        let cache = Cache::load_memory_only(dir.path());
        assert_eq!(cache.get("geo:1"), Some(json!(2)));
        // 旧キーの破棄はしない（Python が書き手の間は、ファイルにも手を付けない）
        assert_eq!(cache.purge_legacy(), 0);
        assert_eq!(cache.legacy_count(), 1);
        cache.put("geocode_osm:ab", json!([]));
        assert_eq!(cache.get("geocode_osm:ab"), Some(json!([])));
        assert_eq!(cache.save_now(), SaveOutcome::Clean);
        assert_eq!(cache.save_if_due(Instant::now()), SaveOutcome::Clean);
        assert_eq!(
            cache.save_final(Duration::from_millis(10)),
            SaveOutcome::Clean
        );
        assert_eq!(read_file(&dir), r#"{"exif:x": 1, "geo:1": 2}"#);
        assert!(!dir.path().join(".cache.json.tmp").exists());

        // ファイルを読まない空のもの。置き場が無くても書こうとしない
        let empty = Cache::memory_only();
        assert!(empty.is_empty());
        empty.put("geo:1", json!(1));
        assert_eq!(empty.get("geo:1"), Some(json!(1)));
        assert_eq!(empty.save_now(), SaveOutcome::Clean);
    }

    #[test]
    fn saver_thread_saves_on_tick_but_leaves_the_final_save_to_the_caller() {
        let dir = TempDir::new("cache-saver");
        let cache = Arc::new(Cache::load(dir.path()));
        let stop = StopFlag::new();
        let handle = spawn_saver_with(
            Arc::clone(&cache),
            stop.clone(),
            Duration::from_millis(10),
            LockHold::for_test(dir.path()),
        );
        cache.put("geo:1", json!(1));
        for _ in 0..200 {
            if dir.path().join(".cache.json").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(read_file(&dir).contains("geo:1"));
        // 間隔の内側の変更は、停止してもスレッドは保存しない（書く側が止まったあとに呼び出し側が保存する）
        cache.put("geo:2", json!(2));
        stop.stop();
        handle.join().unwrap();
        assert!(!read_file(&dir).contains("geo:2"));
        assert_eq!(cache.save_now(), SaveOutcome::Saved);
        assert!(read_file(&dir).contains("geo:2"));
    }

    #[test]
    fn the_final_save_does_not_overlap_a_stuck_save_and_is_skipped_after_the_deadline() {
        use std::sync::mpsc;
        let dir = TempDir::new("cache-overlap");
        let cache = Arc::new(Cache::load(dir.path()));
        cache.put("geo:1", json!(1));
        // 保存スレッドが I/O の最中で固まっている状態（書き込み処理に待ちを差し込む）
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let stuck = {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || {
                cache.save_with(Instant::now(), false, None, |body| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    cache.write_atomically(body)
                })
            })
        };
        entered_rx.recv().unwrap();
        // 固まっている間に書き手が更新し、停止側が最後の保存を試みる
        cache.put("geo:2", json!(2));
        let started = Instant::now();
        assert_eq!(
            cache.save_final(Duration::from_millis(100)),
            SaveOutcome::Skipped
        );
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(
            !dir.path().join(".cache.json").exists()
                && !dir.path().join(".cache.json.tmp").exists(),
            "排他の外では何も書かない"
        );
        // 固まりが解けたら、保存は 1 つずつ終わり、省略された分も次の保存で残る
        release_tx.send(()).unwrap();
        assert_eq!(stuck.join().unwrap(), SaveOutcome::Saved);
        assert_eq!(cache.save_final(Duration::from_secs(5)), SaveOutcome::Saved);
        let text = read_file(&dir);
        assert!(text.contains("geo:1") && text.contains("geo:2"), "{text}");
        serde_json::from_str::<Value>(&text).expect("JSON が壊れていない");
        assert!(!dir.path().join(".cache.json.tmp").exists());
    }

    #[test]
    fn the_final_save_waits_for_a_save_that_finishes_in_time() {
        use std::sync::mpsc;
        let dir = TempDir::new("cache-wait");
        let cache = Arc::new(Cache::load(dir.path()));
        cache.put("geo:1", json!(1));
        let (entered_tx, entered_rx) = mpsc::channel();
        let slow = {
            let cache = Arc::clone(&cache);
            std::thread::spawn(move || {
                cache.save_with(Instant::now(), false, None, |body| {
                    entered_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(150));
                    cache.write_atomically(body)
                })
            })
        };
        entered_rx.recv().unwrap();
        cache.put("geo:2", json!(2));
        assert_eq!(cache.save_final(Duration::from_secs(5)), SaveOutcome::Saved);
        assert_eq!(slow.join().unwrap(), SaveOutcome::Saved);
        let text = read_file(&dir);
        assert!(text.contains("geo:1") && text.contains("geo:2"), "{text}");
    }
}
