//! 地名の enrich（段 `enrich`）。現行 `geo.py:enrich_rows`・`muni_table`。
//!
//! # 規則（現行と同じ）
//!
//! - 対象: `lat IS NOT NULL AND place IS NULL`（`place` が NULL なのが「未 enrich」の印。空文字は「確定」）を
//!   `id` 順に **1 サイクル 40 件まで**。
//! - キャッシュのキーは `geo:<緯度3桁>,<経度3桁>`（`{:.3}`。値は `{place, elev, muni_cd, pref, city}`）。
//! - **日本国内**（緯度 20〜46.5、経度 122〜154.5 の矩形）: キャッシュに `place`・`city` を持つ辞書があればそれを使う
//!   （通信しない）。無ければ地理院の逆ジオコーダ（`LonLatToAddress`）と標高 API を呼ぶ。
//!   - `place` ＝ 応答の `results.lv01Nm`（字。都道府県・市区町村は連結しない。別の列）。
//!   - `muni_cd` ＝ `results.muniCd`（数字だけのとき）。その先頭の 0 を落とした文字列で `muni_table`
//!     （地理院 `muni.js` から作る 市区町村コード→[都道府県, 市区町村]）を引いて `pref`・`city`。
//!   - `elev` ＝ 応答の `elevation` が数値のとき、偶数丸め（Python の `round`）で整数。`"-----"` のような文字列は NULL。
//! - **応答が空でも確定する**（`place=''`）。海上や地理院のカバー外は何度問い合わせても埋まらず、確定させないと
//!   上の 40 件の枠を恒久的に占める。「応答あり」は、状態コードが 2xx で本文が JSON として読めたこと。2 つの API の
//!   どちらも応答が無かったとき（本文が壊れている・4xx など）は、キャッシュも DB も更新せず次の行へ進む。
//! - **国外**: 地理院は圏外。オフラインの逆ジオコーダは本番に無いので、`place=''`・`elev=NULL`・`muni_cd=''`・`pref=''`・
//!   `city=''` で確定する（`pip install reverse-geocode` が入った現行だけ国名が付く。本番には入っていない）。
//!   キャッシュは引かずに上書きする。
//! - 通信した行のあとに 0.2 秒待つ（地理院への連続リクエストの抑制）。キャッシュ・国外の行は待たない。
//!
//! # 現行との違い（通信の失敗の扱い。server.md §0・§15）
//!
//! - **通信の障害は段を中断する**（[`crate::net::Failure::is_outage`] と、429・5xx）。何も確定せずに戻り、残りは
//!   次のサイクルで再試行する。現行は障害の行を飛ばして次の行へ進み、障害のたびに最大 8 秒かかる（40 件で
//!   5 分超）。**片方の API だけ障害のとき、現行はもう片方の応答だけで確定してキャッシュする**ので、一時的な不調で
//!   `place=''` が永久に残る。Rust は確定しない。
//! - `muni_table` は、国内で通信が要る行があるときだけ取りに行く（現行は毎サイクルの先頭で、無ければ取りに行く）。
//!   取れなかったとき、現行は空の表で続けて **`pref`・`city` が空のまま確定**する。Rust は取れなければ（通信の障害でも、
//!   解釈できない本文でも）通信が要る行を処理せず、段を失敗にする。国外の行・キャッシュにある行は先に済ませる。
//! - 応答が壊れていて両方の API が応答なしだった行は、現行は毎サイクル 40 件の枠を占める。Rust は、しばらく飛ばす
//!   （[`crate::backoff`]。プロセスの中だけの記録）。
//! - `lon` が NULL（や数値でない）の行は対象にしない。現行は `format` で例外になり、サイクル全体が落ちる
//!   （`cycle()` は `enrich_rows` を `try` で包んでいない）。件数に `unusable` として出す。
//! - 書き込みは `lat`・`lon` が計画を作ったときと同じ行にだけ効く（enrich の最中に位置が変わった行へ、古い座標の
//!   地名を書かない）。
//! - 通信の上限: 逆ジオ・標高の応答は 1 MiB まで、`muni.js` は 8 MiB まで。時間は現行と同じ 8 秒・15 秒。
//!
//! # `--dry-run`
//!
//! 通信しない。1 サイクルで扱う 40 件を、国外・キャッシュで済む・問い合わせるはず（同じキーは 1 件に数える）に
//! 分けて数える。写しには書かない（後続の段が地名を使わない）。キャッシュは凍結されているので何も書かれない。
//!
//! # ログ
//!
//! 件数だけ。座標・地名・URL・応答の本文は出さない。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::{params, types::Value as Sql};
use serde_json::{Map, Value, json};

use crate::backoff::FailureMemo;
use crate::net::{self, Failure};
use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

/// 現行の `JP_BBOX`（緯度の下限・経度の下限・緯度の上限・経度の上限）。
const JP_BBOX: (f64, f64, f64, f64) = (20.0, 122.0, 46.5, 154.5);
/// 1 サイクルで扱う件数（現行の `LIMIT 40`）。
const PER_CYCLE: usize = 40;
/// 通信した行のあとの待ち（現行の `time.sleep(0.2)`）。
const PAUSE: Duration = Duration::from_millis(200);
/// 逆ジオ・標高の 1 要求の時間（現行の `http_json` の `timeout=8`）。
const API_TIMEOUT: Duration = Duration::from_secs(8);
/// `muni.js` の 1 要求の時間（現行の `timeout=15`）。
const MUNI_TIMEOUT: Duration = Duration::from_secs(15);
/// 逆ジオ・標高の応答の大きさの上限（実物は数百バイト）。
const MAX_API_BYTES: u64 = 1 << 20;
/// `muni.js` の大きさの上限（実物は数百 KB）。
const MAX_MUNI_BYTES: u64 = 8 << 20;
/// `muni.js` の場所（現行の `MUNI_URL`）。
const MUNI_URL: &str = "https://maps.gsi.go.jp/js/muni.js";

/// 通信先。`{lat}`・`{lon}` を埋めるひな形。本番は定数だけ（[`GsiEndpoints::production`]）。
#[derive(Debug, Clone)]
pub struct GsiEndpoints {
    rgeo: String,
    elev: String,
    muni: String,
}

impl GsiEndpoints {
    pub fn production() -> Self {
        Self {
            rgeo: net::GSI_RGEO.to_string(),
            elev: net::GSI_ELEV.to_string(),
            muni: MUNI_URL.to_string(),
        }
    }

    /// 試験用: ループバックのスタブへ向ける。**試験のビルドだけにある**（配布するバイナリにはコードが無い）。
    #[cfg(test)]
    pub(crate) fn stub(base: &str) -> Self {
        Self {
            rgeo: format!("{base}/reverse-geocoder/LonLatToAddress?lat={{lat}}&lon={{lon}}"),
            elev: format!(
                "{base}/general/dem/scripts/getelevation.php?lon={{lon}}&lat={{lat}}&outtype=JSON"
            ),
            muni: format!("{base}/js/muni.js"),
        }
    }
}

/// 同期サイクルの段 `enrich`。
///
/// 応答が壊れている座標を覚えるため、プロセスの間は状態を持つ（[`crate::backoff`]）。
pub struct Enrich {
    endpoints: GsiEndpoints,
    pause: Duration,
    /// 両方の API の応答が無かった座標（`id` と座標）。
    failures: Mutex<FailureMemo<(i64, u64, u64)>>,
}

impl Default for Enrich {
    fn default() -> Self {
        Self::new()
    }
}

impl Enrich {
    pub fn new() -> Self {
        Self::with(GsiEndpoints::production(), PAUSE, FailureMemo::default())
    }

    /// 通信先・待ち・失敗の記憶の方針を指定できる形（試験用。本番は [`Enrich::new`]）。
    pub(crate) fn with(
        endpoints: GsiEndpoints,
        pause: Duration,
        memo: FailureMemo<(i64, u64, u64)>,
    ) -> Self {
        Self {
            endpoints,
            pause,
            failures: Mutex::new(memo),
        }
    }
}

// ---------------------------------------------------------------------------
// 値

/// 1 つの座標の地名（`.cache.json` の `geo:` の値と、`photos` の 5 つの列）。
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    place: String,
    elev: Option<i64>,
    muni_cd: String,
    pref: String,
    city: String,
}

impl Entry {
    /// 空で確定する値（国外、応答が空）。
    fn empty() -> Self {
        Self {
            place: String::new(),
            elev: None,
            muni_cd: String::new(),
            pref: String::new(),
            city: String::new(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "place": self.place,
            "elev": self.elev,
            "muni_cd": self.muni_cd,
            "pref": self.pref,
            "city": self.city,
        })
    }

    /// キャッシュの値から。`place`・`city` を持つ辞書だけが有効（現行は `city` だけを見て、`place` が無いと
    /// 添字で例外になる）。ほかのキーは無ければ空。型が違う値は空（`place` が NULL だと「未 enrich」のまま
    /// 毎サイクル枠を占めるので、文字列でなければ空にする）。
    fn from_cache(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        if !map.contains_key("place") || !map.contains_key("city") {
            return None;
        }
        let text = |key: &str| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let elev = map
            .get("elev")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().and_then(round_to_i64)));
        Some(Self {
            place: text("place"),
            elev,
            muni_cd: text("muni_cd"),
            pref: text("pref"),
            city: text("city"),
        })
    }
}

/// Python の `round()`（偶数丸め）の結果を整数に。大きすぎる値は `None`。
fn round_to_i64(value: f64) -> Option<i64> {
    let rounded = value.round_ties_even();
    (rounded.is_finite() && rounded.abs() < 9.0e15).then_some(rounded as i64)
}

/// Python の `f"{x:.3f}"`。
fn cache_key(lat: f64, lon: f64) -> String {
    format!("geo:{lat:.3},{lon:.3}")
}

/// Python の `str(float)` に近い表記（整数値には `.0` を付ける）。URL に入れる座標。
fn py_float(value: f64) -> String {
    let text = format!("{value}");
    if text.contains(['.', 'e', 'n', 'i']) {
        text
    } else {
        format!("{text}.0")
    }
}

fn in_japan(lat: f64, lon: f64) -> bool {
    JP_BBOX.0 <= lat && lat <= JP_BBOX.2 && JP_BBOX.1 <= lon && lon <= JP_BBOX.3
}

/// 市区町村コード（先頭の 0 を落とした文字列）→ (都道府県, 市区町村)。
type MuniTable = BTreeMap<String, (String, String)>;

/// 数字だけの文字列から先頭の 0 を落とす（Python の `str(int(s))`）。
fn strip_zeros(digits: &str) -> String {
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }.to_string()
}

/// `muni.js` から表を作る（現行の `re.finditer(r"'(\d+),([^,]+),(\d+),([^']*)'", text)`）。
/// 市区町村名の全角空白（U+3000）は取り除く。同じコードが重なれば後のもの。
pub(crate) fn parse_muni_js(text: &str) -> MuniTable {
    let mut table = MuniTable::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find('\'') {
        let start = from + offset;
        match muni_entry(&text[start + 1..]) {
            Some((consumed, code, pref, city)) => {
                table.insert(
                    strip_zeros(code),
                    (pref.to_string(), city.replace('\u{3000}', "")),
                );
                from = start + 1 + consumed;
            }
            None => from = start + 1,
        }
    }
    table
}

/// `'` の直後から `数字+ , 非カンマ+ , 数字+ , 非引用符* '` を読む。戻り値は (読んだ長さ, コード, 都道府県, 市区町村)。
fn muni_entry(rest: &str) -> Option<(usize, &str, &str, &str)> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let n = digits(rest);
    if n == 0 || rest.as_bytes().get(n) != Some(&b',') {
        return None;
    }
    let after_first = &rest[n + 1..];
    let comma = after_first.find(',')?;
    if comma == 0 {
        return None; // `[^,]+` は 1 文字以上
    }
    let pref = &after_first[..comma];
    let after_pref = &after_first[comma + 1..];
    let m = digits(after_pref);
    if m == 0 || after_pref.as_bytes().get(m) != Some(&b',') {
        return None;
    }
    let code = &after_pref[..m];
    let after_code = &after_pref[m + 1..];
    let quote = after_code.find('\'')?;
    let city = &after_code[..quote];
    let consumed = n + 1 + comma + 1 + m + 1 + quote + 1;
    Some((consumed, code, pref, city))
}

// ---------------------------------------------------------------------------
// 通信

/// 1 回の問い合わせの失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallError {
    /// 要求を送って応答のヘッダーを受け取るまで、または本文を読む間の失敗（[`Failure`]）。
    Net(Failure),
    /// 状態コード（2xx 以外）。
    Status(u16),
    /// 応答は届いたが使えない（本文が JSON でない・大きすぎる・4xx）。要求ごとに決まった結果になりうる。
    Bad,
}

impl CallError {
    /// 通信の障害か（相手・回線の不調。要求の中身のせいではない）。
    fn is_outage(self) -> bool {
        match self {
            Self::Net(failure) => failure.is_outage(),
            Self::Status(status) => status == 429 || status >= 500,
            Self::Bad => false,
        }
    }

    /// ログ・エラーに出してよい語（URL・本文を含まない）。
    fn describe(self) -> String {
        match self {
            Self::Net(failure) => failure.as_str().to_string(),
            Self::Status(status) => format!("http {status}"),
            Self::Bad => "bad_response".to_string(),
        }
    }
}

fn fetch(
    ctx: &StageCtx<'_>,
    url: &str,
    timeout: Duration,
    limit: u64,
) -> Result<Vec<u8>, CallError> {
    let mut response = ctx
        .http
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(|e| CallError::Net(Failure::from(&e)))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(if status == 429 || status >= 500 {
            CallError::Status(status)
        } else {
            CallError::Bad
        });
    }
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => CallError::Bad,
            other => CallError::Net(Failure::from(&other)),
        })
}

fn fetch_json(ctx: &StageCtx<'_>, url: &str) -> Result<Value, CallError> {
    let bytes = fetch(ctx, url, API_TIMEOUT, MAX_API_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|_| CallError::Bad)
}

/// 逆ジオコーダの応答を `entry` に反映する（現行の `data["results"]` の読み方）。
fn apply_rgeo(entry: &mut Entry, data: &Value, muni: &MuniTable) {
    let Some(results) = data.get("results").and_then(Value::as_object) else {
        return; // 空（`{}`）・形が違う: 空で確定する
    };
    if let Some(place) = results.get("lv01Nm").and_then(Value::as_str) {
        entry.place = place.to_string();
    }
    let code = match results.get("muniCd") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => String::new(),
    };
    if !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit()) {
        if let Some((pref, city)) = muni.get(&strip_zeros(&code)) {
            entry.pref = pref.clone();
            entry.city = city.clone();
        }
        entry.muni_cd = code;
    }
}

/// 標高 API の応答を `entry` に反映する。
fn apply_elev(entry: &mut Entry, data: &Value) {
    if let Some(Value::Number(n)) = data.get("elevation") {
        entry.elev = n.as_i64().or_else(|| n.as_f64().and_then(round_to_i64));
    }
}

// ---------------------------------------------------------------------------
// 段

#[derive(Debug, Clone, Copy)]
struct Candidate {
    id: i64,
    lat: f64,
    lon: f64,
}

impl Candidate {
    fn memo_key(&self) -> (i64, u64, u64) {
        (self.id, self.lat.to_bits(), self.lon.to_bits())
    }
}

/// 数値の `lat`・`lon` を持つ、地名が未設定の写真（`lon` が NULL・文字列の行は対象にしない）。
const USABLE: &str = "lat IS NOT NULL AND lon IS NOT NULL AND place IS NULL
    AND typeof(lat) IN ('real','integer') AND typeof(lon) IN ('real','integer')";

#[derive(Default)]
struct Tally {
    foreign: u64,
    cache_hit: u64,
    queried: u64,
    unresolved: u64,
    would_query: u64,
}

impl Stage for Enrich {
    fn name(&self) -> &'static str {
        "enrich"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        let count = |sql: String| ctx.read.query_row(&sql, [], |row| row.get::<_, i64>(0));
        let pending = count(format!("SELECT COUNT(*) FROM photos WHERE {USABLE}"))? as u64;
        let unusable = count(
            "SELECT COUNT(*) FROM photos WHERE lat IS NOT NULL AND place IS NULL
             AND NOT (typeof(lat) IN ('real','integer') AND typeof(lon) IN ('real','integer'))"
                .to_string(),
        )? as u64;
        // 現行の `LIMIT 40`。失敗が続く座標は飛ばして、後続を枠に入れる
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut skipped_backoff = 0u64;
        {
            let mut stmt = ctx.read.prepare(&format!(
                "SELECT id, lat, lon FROM photos WHERE {USABLE} ORDER BY id"
            ))?;
            let mut rows = stmt.query([])?;
            let memo = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            while candidates.len() < PER_CYCLE {
                let Some(row) = rows.next()? else { break };
                let candidate = Candidate {
                    id: row.get(0)?,
                    lat: row.get(1)?,
                    lon: row.get(2)?,
                };
                if memo.is_waiting(&candidate.memo_key(), now) {
                    skipped_backoff += 1;
                    continue;
                }
                candidates.push(candidate);
            }
        }
        let mut tally = Tally::default();
        let mut misses: Vec<Candidate> = Vec::new();
        // 通信の要らない行（国外・キャッシュ）を先に済ませる
        let mut dry_keys: Vec<String> = Vec::new();
        for candidate in &candidates {
            if ctx.stop.is_stopped() {
                break;
            }
            let key = cache_key(candidate.lat, candidate.lon);
            if !in_japan(candidate.lat, candidate.lon) {
                tally.foreign += 1;
                if !ctx.is_dry_run() {
                    // 国外はキャッシュを引かずに上書きする（現行と同じ）
                    let entry = Entry::empty();
                    ctx.cache.put(&key, entry.to_json());
                    store(ctx, candidate, &entry)?;
                }
                continue;
            }
            match ctx.cache.get(&key).as_ref().and_then(Entry::from_cache) {
                Some(entry) => {
                    tally.cache_hit += 1;
                    if !ctx.is_dry_run() {
                        store(ctx, candidate, &entry)?;
                    }
                }
                None if ctx.is_dry_run() => {
                    // 同じキーの 2 件目以降は、1 件目の問い合わせで済む
                    if dry_keys.contains(&key) {
                        tally.cache_hit += 1;
                    } else {
                        dry_keys.push(key);
                        tally.would_query += 1;
                    }
                }
                None => misses.push(*candidate),
            }
        }
        let outcome = if misses.is_empty() {
            Ok(())
        } else {
            self.query_all(ctx, &misses, &mut tally)
        };
        let query_name = if ctx.is_dry_run() {
            "would_query"
        } else {
            "queried"
        };
        let counts = Counts(vec![
            ("candidates", candidates.len() as u64),
            ("foreign", tally.foreign),
            ("cache_hit", tally.cache_hit),
            (
                query_name,
                if ctx.is_dry_run() {
                    tally.would_query
                } else {
                    tally.queried
                },
            ),
            ("unresolved", tally.unresolved),
            ("skipped_backoff", skipped_backoff),
            ("unusable", unusable),
            ("pending", pending),
        ]);
        match outcome {
            Ok(()) => Ok(StageOutcome::Done(counts)),
            // 件数は失敗の文面に載せる（途中までは確定している）
            Err(e) => Err(StageError::text(format!("{e}（{counts}）"))),
        }
    }
}

impl Enrich {
    /// 通信が要る行を順に処理する。通信の障害・`muni.js` の失敗は `Err`（確定した行はそのまま残る）。
    fn query_all(
        &self,
        ctx: &StageCtx<'_>,
        misses: &[Candidate],
        tally: &mut Tally,
    ) -> Result<(), StageError> {
        let muni = self.muni_table(ctx)?;
        for candidate in misses {
            if ctx.stop.is_stopped() {
                break;
            }
            let key = cache_key(candidate.lat, candidate.lon);
            // 先の行が同じキーを確定していれば、それを使う（現行も 2 件目はキャッシュに当たる）
            if let Some(entry) = ctx.cache.get(&key).as_ref().and_then(Entry::from_cache) {
                tally.cache_hit += 1;
                store(ctx, candidate, &entry)?;
                continue;
            }
            match self.lookup(ctx, candidate, &muni) {
                Err(e) => {
                    return Err(StageError::text(format!(
                        "地理院への問い合わせが通信の障害で中断しました（{}）",
                        e.describe()
                    )));
                }
                Ok(None) => {
                    // 両方の API の応答が無かった: 確定せず、次の行へ。この座標はしばらく飛ばす
                    tally.unresolved += 1;
                    self.failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .record_failure(candidate.memo_key(), Instant::now());
                }
                Ok(Some(entry)) => {
                    // 空でも確定として記録し、同じ座標を二度と問い合わせない
                    ctx.cache.put(&key, entry.to_json());
                    store(ctx, candidate, &entry)?;
                    tally.queried += 1;
                    self.failures
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .record_success(&candidate.memo_key());
                    // 地理院への連続リクエストを抑制する。停止の合図が来たら待たずに戻る
                    if ctx.stop.wait(self.pause) {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// 逆ジオコーダと標高 API を呼ぶ。どちらかが通信の障害なら `Err`。両方の応答が無ければ `Ok(None)`。
    /// 片方だけ応答があれば、その分で確定する（現行と同じ）。
    fn lookup(
        &self,
        ctx: &StageCtx<'_>,
        candidate: &Candidate,
        muni: &MuniTable,
    ) -> Result<Option<Entry>, CallError> {
        let fill = |template: &str| {
            template
                .replace("{lat}", &py_float(candidate.lat))
                .replace("{lon}", &py_float(candidate.lon))
        };
        let mut entry = Entry::empty();
        let mut answered = false;
        match fetch_json(ctx, &fill(&self.endpoints.rgeo)) {
            Ok(data) => {
                answered = true;
                apply_rgeo(&mut entry, &data, muni);
            }
            Err(e) if e.is_outage() => return Err(e),
            Err(_) => {}
        }
        match fetch_json(ctx, &fill(&self.endpoints.elev)) {
            Ok(data) => {
                answered = true;
                apply_elev(&mut entry, &data);
            }
            Err(e) if e.is_outage() => return Err(e),
            Err(_) => {}
        }
        Ok(answered.then_some(entry))
    }

    /// 市区町村の表。キャッシュ（`muni_table`）にあればそれを使い、無ければ `muni.js` から作って保存する。
    fn muni_table(&self, ctx: &StageCtx<'_>) -> Result<MuniTable, StageError> {
        if let Some(Value::Object(map)) = ctx.cache.get("muni_table") {
            let table = table_from_cache(&map);
            if !table.is_empty() {
                return Ok(table);
            }
        }
        let bytes =
            fetch(ctx, &self.endpoints.muni, MUNI_TIMEOUT, MAX_MUNI_BYTES).map_err(|e| {
                StageError::text(format!(
                    "muni.js を取得できないので、地名を付けられません（{}）",
                    e.describe()
                ))
            })?;
        // 現行は `decode("utf-8", errors="replace")`
        let table = parse_muni_js(&String::from_utf8_lossy(&bytes));
        if table.is_empty() {
            return Err(StageError::fixed(
                "muni.js から自治体の表を作れないので、地名を付けられません",
            ));
        }
        let map: Map<String, Value> = table
            .iter()
            .map(|(code, (pref, city))| (code.clone(), json!([pref, city])))
            .collect();
        ctx.cache.put("muni_table", Value::Object(map));
        tracing::info!("[geo] 自治体テーブル取得: {}件", table.len());
        Ok(table)
    }
}

fn table_from_cache(map: &Map<String, Value>) -> MuniTable {
    map.iter()
        .filter_map(|(code, value)| {
            let items = value.as_array()?;
            Some((
                code.clone(),
                (
                    items.first()?.as_str()?.to_string(),
                    items.get(1)?.as_str()?.to_string(),
                ),
            ))
        })
        .collect()
}

/// 1 行に 5 つの列を書く。`lat`・`lon` が変わった行・すでに地名が付いた行には効かない。
fn store(ctx: &StageCtx<'_>, candidate: &Candidate, entry: &Entry) -> Result<(), StageError> {
    let (id, lat, lon) = (candidate.id, candidate.lat, candidate.lon);
    let entry = entry.clone();
    ctx.writer()?.call(move |conn| {
        conn.execute(
            "UPDATE photos SET place=?1, elev=?2, muni_cd=?3, pref=?4, city=?5
             WHERE id=?6 AND lat=?7 AND lon=?8 AND place IS NULL",
            params![
                entry.place,
                entry.elev.map_or(Sql::Null, Sql::Integer),
                entry.muni_cd,
                entry.pref,
                entry.city,
                id,
                lat,
                lon
            ],
        )
    })?;
    Ok(())
}
