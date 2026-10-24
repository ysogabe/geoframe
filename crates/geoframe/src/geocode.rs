//! 地名・住所の検索（順ジオコーディング。E4 `GET /api/admin/geocode`）。現行 `geo.py` の
//! `geocode_search`・`_gsi_geocode`・`nominatim_search`。仕様と Python との差は
//! docs/rust-migration/server.md §18。
//!
//! # 規則（現行と同じ）
//!
//! - 検索語は前後の空白（`str.strip()`）を落とし、2 文字（コードポイント）未満は通信せず空にする
//!   （上限は [`MAX_QUERY_LEN`] 文字。現行との違いを参照）。
//! - まず地理院の地名検索 API を引く。**0 件（または失敗）のときだけ** Nominatim を引く。国内の地名は
//!   地理院の結果だけを返し、Nominatim への問い合わせを減らす。
//! - 地理院の結果は先頭の 10 件だけを見て（使えない要素を飛ばした後ろの要素は繰り上がらない）、座標が数値で
//!   2 つ並んだものを `{title, lat, lon, source: "gsi"}` にする。座標の範囲は見ない。`title` が空なら検索語。
//!   **要素・`geometry`・`properties` が真の非オブジェクト、`title` が真の非文字列なら、地理院の結果全体を捨てる**
//!   （現行は `.get`・`.strip` が例外になり、握りつぶして空を返す。それまでに集めた要素も捨てる）。
//! - Nominatim の結果は、`lat`・`lon` を `float()` に通して緯度 −90〜90・経度 −180〜180 に収まるものだけを
//!   `{title: display_name, lat, lon, source: "osm"}` にし、先頭の 10 件を返す。読めない要素は 1 件ずつ飛ばす。
//! - **Nominatim の結果は `.cache.json` のメモリキャッシュに入れる**（キー `geocode_osm:<検索語>`、国コードを
//!   渡したときは `geocode_osm:<国コード>:<検索語>`。0 件も入れる。通信の失敗・配列でない応答は入れない）。
//!   キャッシュの値が配列ならそのまま返す。地理院の結果はキャッシュしない。
//! - **Nominatim は全体で 1 秒 1 件**: ロックを持ったまま、**前の通信が終わった時刻**から 1 秒たつまで待ってから
//!   通信する（失敗でも終わった時刻を記録する）。キャッシュに当たれば待たない。
//! - 通信の失敗（接続・時間切れ・HTTP のエラー・JSON でない・UTF-8 でない）は空にして握りつぶす。
//! - User-Agent は現行の `GeoFrame/0.2`、1 要求 8 秒。
//!
//! # 現行との違い
//!
//! - 応答の本文は**展開後で** 1 MiB まで（現行は上限なし）。超えたら失敗と同じ（空）。gzip の本文も展開後の大きさで
//!   数える（[`crate::net::read_body_limited`]）。
//! - 検索語は前後の空白を落としたあと [`MAX_QUERY_LEN`]（200）文字まで。超えたら上流へ問い合わせずに空を返す
//!   （現行は上限なし）。
//! - `geocode_osm:` のキャッシュは [`CACHE_MAX_ENTRIES`]（5000）件まで。満杯のあとの新しい検索語は結果を返すが
//!   保存しない（現行は上限なし。古いものを捨てない: キャッシュは挿入順を持たず、期限も無い）。既に入っている
//!   語は引き続き当たり、上書きもできる。`geo:` など、ほかのキーには触れない。
//! - 待っている間に、同じ語を別の要求がキャッシュに入れたら、通信せずにそれを返す（現行は待ったあとに必ず通信する）。
//! - 同時に処理する検索は [`MAX_IN_FLIGHT`] 件まで。超える要求は待たせずに断る（現行は無制限に待たせる）。
//! - JSON の `NaN`・`Infinity`・`1e400` のような f64 に収まらない数は、Rust では本文全体が不正な JSON になる
//!   （現行は読めて、非有限の座標は Nominatim では範囲外で飛ばし、地理院では通す）。
//! - `display_name` が文字列でない（数・配列など）ときは、`str()` の結果ではなく検索語を `title` にする。
//! - 失敗のログに検索語を出さない（種類と状態コードだけ）。
//!
//! # ログ
//!
//! 検索語・地名・座標・応答の中身・URL は出さない。失敗の種類（[`crate::net::Failure`]・HTTP の状態コード）だけ。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::cache::Cache;
use crate::net::{self, Failure};
use crate::pyquery::{py_float, py_quote, py_strip};

/// 短すぎる入力は無駄な要求になるので弾く（現行の `GEOCODE_MIN_LEN`。コードポイント数）。
pub const MIN_LEN: usize = 2;
/// 検索語の長さの上限（前後の空白を落としたあとのコードポイント数）。管理画面の地名検索として十分な長さで、
/// 超えるものは上流へ問い合わせずに空を返す（現行にはこの上限が無い。server.md §18）。
pub const MAX_QUERY_LEN: usize = 200;
/// `geocode_osm:` のキャッシュの件数の上限。満杯になったら新しい検索語は保存しない（結果は返す。古いものを
/// 捨てもしない。`geo:` など、ほかのキーには触れない）。実測の 1 件は数百バイト〜数 KB。
pub const CACHE_MAX_ENTRIES: usize = 5000;
/// 返す件数の上限（現行の `GEOCODE_MAX_RESULTS`）。
pub const MAX_RESULTS: usize = 10;
/// Nominatim の要求の最短の間隔（公開サーバーの利用規約。現行の `NOMINATIM_MIN_INTERVAL_S`）。
pub const NOMINATIM_MIN_INTERVAL: Duration = Duration::from_secs(1);
/// 同時に処理する検索の数。超えたら待たせずに断る（[`Geocoder::try_reserve`]）。
pub const MAX_IN_FLIGHT: usize = 8;
/// 1 要求の時間（現行の `urlopen(timeout=8)`。Rust は名前解決から本文の読み切りまで）。
const TIMEOUT: Duration = Duration::from_secs(8);
/// 応答の本文の大きさの上限（実物は数 KB）。
const MAX_BODY_BYTES: u64 = 1 << 20;
/// `geocode_osm:` キャッシュのキーの接頭辞。
const CACHE_PREFIX: &str = "geocode_osm:";
/// 現行の `http_json` の User-Agent。
const USER_AGENT: &str = "GeoFrame/0.2";

/// 地理院の地名検索（`config.py` の `GSI_GEOCODE`）。`{q}` を埋めて使う。
const GSI_GEOCODE: &str = "https://msearch.gsi.go.jp/address-search/AddressSearch?q={q}";
/// Nominatim（`config.py` の `NOMINATIM_SEARCH`）。`{q}`・`{limit}` を埋めて使う。
const NOMINATIM_SEARCH: &str = "https://nominatim.openstreetmap.org/search?q={q}&format=jsonv2&limit={limit}&accept-language=ja";

/// 通信先。ひな形の `{q}`・`{limit}` を埋めて使う。
///
/// # 向け先を変えられるのは、この crate を直接組み込むコードだけ
///
/// 本番のバイナリ（`main.rs`・`cli.rs`）は [`Endpoints::production`]（定数だけ）しか使わず、設定・環境変数・
/// コマンドラインのどれからも通信先を変えられない。結合テスト（別の crate として `geoframe` を使う）が
/// ループバックのスタブへ向けるための [`Endpoints::loopback`] は、**ループバックのアドレス以外を受け付けない**
/// ので、誤って呼んでも外へは出ない。`cfg(test)` の閉じ込めでは結合テストから使えないための形で、
/// `loopback` の呼び出しが本番のコードに無いことは試験で見る。
#[derive(Debug, Clone)]
pub struct Endpoints {
    gsi: String,
    nominatim: String,
}

impl Endpoints {
    pub fn production() -> Self {
        Self {
            gsi: GSI_GEOCODE.to_string(),
            nominatim: NOMINATIM_SEARCH.to_string(),
        }
    }

    /// ループバックのスタブへ向ける（結合テスト用）。ループバック以外のアドレスは `None`。
    pub fn loopback(addr: SocketAddr) -> Option<Self> {
        addr.ip()
            .is_loopback()
            .then(|| Self::at(&format!("http://{addr}")))
    }

    /// 試験用: ループバックのスタブへ向ける。**試験のビルドだけにある**。
    #[cfg(test)]
    pub(crate) fn stub(base: &str) -> Self {
        Self::at(base)
    }

    fn at(base: &str) -> Self {
        Self {
            gsi: format!("{base}/address-search/AddressSearch?q={{q}}"),
            nominatim: format!(
                "{base}/search?q={{q}}&format=jsonv2&limit={{limit}}&accept-language=ja"
            ),
        }
    }
}

/// 時計（Nominatim の 1 秒 1 件の判定と待ち）。本番は実時間、試験は進みを自分で決める偽の時計。
pub(crate) trait Clock: Send + Sync {
    /// 任意の起点からの経過（単調）。
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

struct SystemClock {
    origin: Instant,
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// 通信の失敗（ログに出してよい語だけを持つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// 要求を送って応答のヘッダーを受け取るまで、または本文を読む間の失敗。
    Net(Failure),
    /// 2xx 以外の状態コード。
    Status(u16),
    /// 応答は届いたが使えない（大きすぎる・UTF-8 でない・JSON でない）。
    Bad,
}

impl Fail {
    fn describe(self) -> String {
        match self {
            Self::Net(failure) => failure.as_str().to_string(),
            Self::Status(status) => format!("http {status}"),
            Self::Bad => "bad_response".to_string(),
        }
    }
}

/// 本文を読む最中の `ureq` のエラー。通信の障害の種類はそのまま、それ以外は本文が読めなかったことにする。
fn body_failure(e: &ureq::Error) -> Failure {
    match Failure::from(e) {
        failure if failure.is_outage() => failure,
        _ => Failure::BodyUnreadable,
    }
}

pub struct Geocoder {
    endpoints: Endpoints,
    agent: ureq::Agent,
    clock: Box<dyn Clock>,
    min_interval: Duration,
    /// `geocode_osm:` のキャッシュの件数の上限（本番は [`CACHE_MAX_ENTRIES`]。試験は小さくする）。
    cache_cap: usize,
    /// 前の Nominatim の通信が終わった時刻（[`Clock::now`]）。まだ通信していなければ `None`。
    /// **ロックは待ちと通信のあいだ持ち続ける**（全体で 1 秒 1 件にするため）。
    nominatim_last: Mutex<Option<Duration>>,
    slots: Arc<Semaphore>,
}

impl Default for Geocoder {
    fn default() -> Self {
        Self::production()
    }
}

impl Geocoder {
    pub fn production() -> Self {
        Self::with_parts(
            Endpoints::production(),
            Box::new(SystemClock {
                origin: Instant::now(),
            }),
            MAX_IN_FLIGHT,
        )
    }

    /// 通信先だけを差し替える（実時計・既定の同時数。結合テスト用。[`Endpoints`] を参照）。
    pub fn with_endpoints(endpoints: Endpoints) -> Self {
        Self::with_parts(
            endpoints,
            Box::new(SystemClock {
                origin: Instant::now(),
            }),
            MAX_IN_FLIGHT,
        )
    }

    pub(crate) fn with_parts(
        endpoints: Endpoints,
        clock: Box<dyn Clock>,
        in_flight: usize,
    ) -> Self {
        Self {
            endpoints,
            agent: net::agent(),
            clock,
            min_interval: NOMINATIM_MIN_INTERVAL,
            cache_cap: CACHE_MAX_ENTRIES,
            nominatim_last: Mutex::new(None),
            slots: Arc::new(Semaphore::new(in_flight)),
        }
    }

    /// 同時に処理できる検索の空きを 1 つ取る。空きが無ければ `None`（呼び出し側が断る）。処理が終わるまで持つ。
    pub fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.slots).try_acquire_owned().ok()
    }

    /// 現行の `geocode_search`。候補（`{title, lat, lon, source}`）の配列を返す。通信しない入力（短すぎる語）や
    /// 失敗は空。呼び出し側は**ブロッキングのスレッド**で呼ぶ（通信と待ちがある）。
    pub fn search(&self, cache: &Cache, query: &str) -> Vec<Value> {
        let Some(q) = clean_query(query) else {
            return Vec::new();
        };
        let items = self.gsi_geocode(q);
        if !items.is_empty() {
            return items;
        }
        self.nominatim_search(cache, q, "")
    }

    fn gsi_geocode(&self, q: &str) -> Vec<Value> {
        let url = self.endpoints.gsi.replace("{q}", &py_quote(q));
        match self.get_json(&url) {
            Ok(data) => parse_gsi(&data, q),
            Err(fail) => {
                tracing::warn!("[geo] 地名検索失敗（地理院）: {}", fail.describe());
                Vec::new()
            }
        }
    }

    /// 現行の `nominatim_search`。`countrycode`（ISO 3166-1 の 2 文字）を渡すとその国に絞る。
    /// 検索語は呼び出し側が整える（ここでは `strip` しない）。
    pub fn nominatim_search(&self, cache: &Cache, q: &str, countrycode: &str) -> Vec<Value> {
        let cc = normalize_country(countrycode);
        let key = if cc.is_empty() {
            format!("{CACHE_PREFIX}{q}")
        } else {
            format!("{CACHE_PREFIX}{cc}:{q}")
        };
        if let Some(Value::Array(hit)) = cache.get(&key) {
            return hit;
        }
        // ロックは、待ちから通信・キャッシュへの保存まで持ち続ける（全体で 1 秒 1 件にするため。保存の前に放すと、
        // 待っていた同じ語の要求がキャッシュを見逃して、もう一度問い合わせてしまう）
        let mut last = self
            .nominatim_last
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // 待っている間に、別の要求が同じ語を入れたかもしれない
        if let Some(Value::Array(hit)) = cache.get(&key) {
            return hit;
        }
        if let Some(at) = *last
            && let Some(wait) = (at + self.min_interval).checked_sub(self.clock.now())
            && !wait.is_zero()
        {
            self.clock.sleep(wait);
        }
        let mut url = self
            .endpoints
            .nominatim
            .replace("{q}", &py_quote(q))
            .replace("{limit}", &MAX_RESULTS.to_string());
        if !cc.is_empty() {
            url.push_str("&countrycodes=");
            url.push_str(&cc);
        }
        let data = self.get_json(&url);
        *last = Some(self.clock.now());
        let list = match data {
            Ok(Value::Array(list)) => list,
            // 想定外の形式（エラー応答など）はキャッシュしない
            Ok(_) => return Vec::new(),
            Err(fail) => {
                tracing::warn!("[geo] 地名検索失敗（Nominatim）: {}", fail.describe());
                return Vec::new();
            }
        };
        let mut out: Vec<Value> = list
            .iter()
            .filter_map(|item| parse_osm_item(item, q))
            .collect();
        out.truncate(MAX_RESULTS);
        if !cache.put_capped(
            &key,
            Value::Array(out.clone()),
            CACHE_PREFIX,
            self.cache_cap,
        ) {
            tracing::warn!(
                "[geo] 地名検索のキャッシュが満杯です（{} 件）。保存しません",
                self.cache_cap
            );
        }
        out
    }

    /// GET して JSON にする。現行の `http_json`: 状態コードが 2xx 以外、本文が UTF-8 でない・JSON でないときは失敗。
    fn get_json(&self, url: &str) -> Result<Value, Fail> {
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .config()
            .timeout_global(Some(TIMEOUT))
            .build()
            .call()
            .map_err(|e| Fail::Net(Failure::from(&e)))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Fail::Status(status));
        }
        // 上限は展開後の大きさに効かせる（`net::read_body_limited`。gzip の本文は圧縮時の大きさでは測れない）
        let bytes =
            net::read_body_limited(response.body_mut(), MAX_BODY_BYTES).map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => Fail::Bad,
                other => Fail::Net(body_failure(&other)),
            })?;
        let text = String::from_utf8(bytes).map_err(|_| Fail::Bad)?;
        serde_json::from_str(&text).map_err(|_| Fail::Bad)
    }
}

/// 検索語を整える（前後の空白を落とす）。2 文字未満と [`MAX_QUERY_LEN`] 文字超は `None`（通信しない）。
pub fn clean_query(raw: &str) -> Option<&str> {
    let q = py_strip(raw);
    let len = q.chars().take(MAX_QUERY_LEN + 1).count();
    (MIN_LEN..=MAX_QUERY_LEN).contains(&len).then_some(q)
}

/// 現行の国コードの正規化: 前後の空白を落として小文字にし、ASCII の英字 2 文字ならそれ、でなければ空。
fn normalize_country(countrycode: &str) -> String {
    let cc = py_strip(countrycode).to_lowercase();
    if cc.chars().count() == 2 && cc.chars().all(|c| c.is_ascii_alphabetic()) {
        cc
    } else {
        String::new()
    }
}

fn candidate(title: &str, q: &str, lat: f64, lon: f64, source: &str) -> Value {
    let title = if title.is_empty() { q } else { title };
    json!({"title": title, "lat": lat, "lon": lon, "source": source})
}

/// Python の真偽（`bool(x)`）。
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `isinstance(x, (int, float))` が真のもの（bool も int）を `float(x)` にする。
fn py_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

enum Gsi {
    Keep(Value),
    /// この要素だけ飛ばす。
    Skip,
    /// 現行なら例外になる要素。地理院の結果全体を捨てる。
    Abort,
}

/// 現行の `_gsi_geocode` の本文の解釈。
fn parse_gsi(data: &Value, q: &str) -> Vec<Value> {
    let Value::Array(list) = data else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for feat in list.iter().take(MAX_RESULTS) {
        match gsi_item(feat, q) {
            Gsi::Keep(item) => out.push(item),
            Gsi::Skip => {}
            Gsi::Abort => {
                tracing::warn!("[geo] 地名検索失敗（地理院）: 応答の形が想定外です");
                return Vec::new();
            }
        }
    }
    out
}

/// `x or {}` を辿って `.get` を呼ぶ。偽の値は `{}` 扱い、真の非オブジェクトは `.get` が `AttributeError`（`Err`）。
fn object_or_empty<'a>(
    v: Option<&'a Value>,
    empty: &'a Map<String, Value>,
) -> Result<&'a Map<String, Value>, ()> {
    match v {
        Some(v) if truthy(v) => v.as_object().ok_or(()),
        _ => Ok(empty),
    }
}

/// 現行は `(feat or {}).get("geometry") or {}` ... と順に辿る。
fn gsi_item(feat: &Value, q: &str) -> Gsi {
    let empty = Map::new();
    let Ok(feat) = object_or_empty(Some(feat), &empty) else {
        return Gsi::Abort;
    };
    let Ok(geometry) = object_or_empty(feat.get("geometry"), &empty) else {
        return Gsi::Abort;
    };
    let Some(Value::Array(coords)) = geometry.get("coordinates") else {
        return Gsi::Skip;
    };
    let [lon, lat] = coords.as_slice() else {
        return Gsi::Skip;
    };
    let (Some(lon), Some(lat)) = (py_number(lon), py_number(lat)) else {
        return Gsi::Skip;
    };
    let Ok(props) = object_or_empty(feat.get("properties"), &empty) else {
        return Gsi::Abort;
    };
    let title = match props.get("title") {
        Some(Value::String(s)) => py_strip(s),
        Some(other) if truthy(other) => return Gsi::Abort, // `.strip()` が無い
        _ => "",
    };
    Gsi::Keep(candidate(title, q, lat, lon, "gsi"))
}

/// Nominatim の 1 件。`float(item["lat"])` が `TypeError`・`KeyError`・`ValueError` になる要素と、範囲外は飛ばす。
fn parse_osm_item(item: &Value, q: &str) -> Option<Value> {
    let item = item.as_object()?;
    let lat = osm_coordinate(item.get("lat")?)?;
    let lon = osm_coordinate(item.get("lon")?)?;
    if !((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
        return None;
    }
    let title = match item.get("display_name") {
        Some(Value::String(s)) => py_strip(s),
        _ => "",
    };
    Some(candidate(title, q, lat, lon, "osm"))
}

fn osm_coordinate(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => py_float(s),
        other => py_number(other),
    }
}

#[cfg(test)]
pub(crate) mod stub;
#[cfg(test)]
mod tests;
