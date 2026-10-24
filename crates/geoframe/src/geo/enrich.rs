//! 地名の enrich（段 `enrich`）。現行 `geo.py:enrich_rows`・`muni_table`。
//!
//! # 規則（現行と同じ）
//!
//! - 対象: `lat IS NOT NULL AND place IS NULL`（`place` が NULL なのが「未 enrich」の印。空文字は「確定」）を
//!   `id` 順に **1 サイクル 40 件まで**。
//! - **処理は `id` 順の 1 パス**（現行と同じ）。キャッシュのキーは丸めた座標なので、近い座標の行は同じキーを共有し、
//!   **先に来た行がキャッシュを作る**。国外の行は国内の行が作ったキャッシュを空で上書きするので、国内外を分けて
//!   国内を後回しにすると、先の国内の行が後の国外の行の空のキャッシュに当たって、空の地名で確定してしまう
//!   （例: `(20.0001, 130.0)` と `(19.9999, 130.0)` はどちらも `geo:20.000,130.000`）。
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
//! - 通信を試した行のあとに 0.2 秒待つ（地理院への連続リクエストの抑制）。キャッシュ・国外の行は待たない。
//!
//! # 現行との違い（通信の失敗の扱い。server.md §0・§15）
//!
//! - **通信の障害は、短い待ちで 2 回まで再試行する**（待ちは 0.5 秒、1 秒。停止の合図で打ち切る）。それでも続く
//!   とき、**接続できない（`dns`・`connect`・`tls`）・429** は座標によらない全体の障害なので、すぐ段を中断する。
//!   **5xx・時間切れ・切断・本文が読めない**は座標による場合もあるので、その座標を壊れた座標として
//!   [`crate::backoff`] に入れて（30 分以上飛ばす）**次へ進み**、**連続して 3 つの異なるキー（座標）**で続いたら
//!   全体の障害として中断する（同じ座標の写真が何枚あっても 1 つ。同じキーの後の行は下の「先の行を追い越さない」で
//!   見送るので、数え方としてもキーで数える）。応答が返った座標（確定した・応答が使えなかった）で連続の数は 0 に戻る。中断しても確定した行は残り、
//!   残りは次のサイクルでやる（接続できない・429 の座標は記憶に入れないので、直れば待たずに続く）。現行は障害の行を
//!   飛ばして次へ進み、障害のたびに最大 8 秒かかる（40 件で 5 分超）。
//! - **片方の API だけ障害のとき、確定しない**。現行は応答のあったもう片方だけで確定してキャッシュするので、
//!   一時的な標高の不調で `elev=NULL` が（海上の NULL と見分けがつかないまま）永久に残る。Rust は確定せず、
//!   障害が続く間は国内の未キャッシュの行が進まない代わりに、直れば欠けなく埋まる（server.md §15.4）。
//!   応答は返ったが使えない（4xx・本文が JSON でない）ときは現行と同じく、もう片方の応答で確定する。
//! - **先の行を追い越さない**: 確定していない先の行（座標による障害・応答なし・バックオフ中・段の中断や時間の上限で
//!   止まった行）があるキーは、**サイクルをまたいでも**、同じキーの後の行（国外・キャッシュ・別の座標の国内を問わず）
//!   を見送る。追い越して国外の行が空のキャッシュを作ると、先の国内の行は、直ってもその空に当たって地名が空のまま
//!   永久に確定する。サイクル内で止まった行は `deferred`、前のサイクルから続くバックオフ中の行のキーは、候補を選ぶ
//!   ときに（毎サイクル、バックオフ中の行から作り直す。プロセスの記憶以外に持たない）同じキーの後の行を枠に入れず
//!   `skipped_blocked` に数える。通信の要らない行（別のキーの国外・キャッシュにある行）は、止まったあとも id 順に済ませる。
//! - **段全体の経過時間に上限がある**（[`STAGE_BUDGET`]）。超えたら、通信が要る未確定の行を次のサイクルへ回す
//!   （失敗にしない。件数の `out_of_time=1`）。再試行込みでは 1 行が約 51 秒かかりうるので、上限が無いと
//!   40 行で約 34 分になる（成功扱いなので連続障害の中断も働かない）。
//! - `muni_table` は、国内で通信が要る行があるときだけ取りに行く（現行は毎サイクルの先頭で、無ければ取りに行く）。
//!   取れなかったとき、現行は空の表で続けて **`pref`・`city` が空のまま確定**する。Rust は取れなければ（通信の障害でも、
//!   解釈できない本文でも）通信が要る行を見送り、段を失敗にする。
//! - 応答が壊れていて両方の API が応答なしだった行は、現行は毎サイクル 40 件の枠を占める。Rust は、しばらく飛ばす
//!   （[`crate::backoff`]。プロセスの中だけの記録）。
//! - `lon` が NULL（や数値でない）の行は対象にしない。現行は `format` で例外になり、サイクル全体が落ちる
//!   （`cycle()` は `enrich_rows` を `try` で包んでいない）。件数に `unusable` として出す。
//! - 書き込みは `lat`・`lon` が計画を作ったときと同じ行にだけ効く（enrich の最中に位置が変わった行へ、古い座標の
//!   地名を書かない）。
//! - 通信の上限: 逆ジオ・標高の応答は 1 MiB まで、`muni.js` は 8 MiB まで。時間は現行と同じ 8 秒・15 秒。
//!
//! # 停止の境界
//!
//! 停止の合図は、行と行のあいだ・行のあとの待ち・再試行の待ちで見る。**通信が進んでいる行は、逆ジオ・標高・キャッシュ・
//! DB の更新まで済ませてから止まる**（要求の途中で捨てると、片方だけの応答が残らない代わりに、1 行ぶんの通信が無駄に
//! なる。普通の行は 1 秒未満、再試行込みでは最大で約 51 秒）。再試行の待ちの途中で止まったときは、その行は確定せずに戻る
//! （通信は試したので `queried` と `unresolved` に数える）。**時間の上限**（[`STAGE_BUDGET`]）は、これと違い、次の要求
//! （再試行を含む）を始める前に見る。上限に達した行は確定せず、記憶にも入れない（次のサイクルで先頭から試す）。
//!
//! # `--dry-run`
//!
//! 通信しない。1 サイクルで扱う 40 件を、国外・キャッシュで済む・問い合わせるはずに分けて数える。**実行と同じ id 順で、
//! 書くはずのキャッシュを模擬する**（国外の行が空で上書きしたキー、問い合わせるはずの行が作るキーは、あとの行に
//! とってキャッシュにある）。バックオフ中の行とそのキーの後の行を枠から外す選び方も実行と同じ。写しには書かない（後続の段が地名を使わない）。キャッシュは凍結されているので何も
//! 書かれない。
//!
//! # ログ
//!
//! 件数だけ。座標・地名・URL・応答の本文は出さない。

use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;
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
/// 通信を試した行のあとの待ち（現行の `time.sleep(0.2)`。現行は確定した行のあとだけ）。
const PAUSE: Duration = Duration::from_millis(200);
/// 通信の障害のあとの再試行の回数（最初の 1 回のあとに）。
const RETRIES: u32 = 2;
/// 再試行の前の待ち。1 回目はこれ、2 回目はその 2 倍。
const RETRY_WAIT: Duration = Duration::from_millis(500);
/// 座標による障害（5xx・時間切れ・切断・本文が読めない）が、別の座標でも続けてこの数になったら、全体の障害として
/// 段を中断する。
const OUTAGE_STREAK: usize = 3;
/// 段全体の経過時間の上限。超えたら、通信が要る未確定の行は次のサイクルへ回す（確定した行は残る）。
/// 普通の行は 2 回の要求で 1 秒未満（40 件で 1 分に届かない）。再試行つきで「2 回時間切れ→3 回目成功」が両方の
/// API で続く行は約 51 秒かかるので、これが 40 件続いた場合（約 34 分）に歯止めをかける。要求の途中では切らず、
/// 次の要求（再試行を含む）を始める前に見るので、実際の最大は上限＋進行中の 1 要求（8 秒）＋再試行の待ち（1 秒）。
const STAGE_BUDGET: Duration = Duration::from_secs(120);
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
    retry_wait: Duration,
    /// 段全体の経過時間の上限（[`STAGE_BUDGET`]）。
    budget: Duration,
    /// 両方の API の応答が無かった座標（`id` と座標）。
    failures: Mutex<FailureMemo<(i64, u64, u64)>>,
    /// 失敗の記憶が見る時計の進み（試験用。実時間を待たずにバックオフを明けさせる）。
    #[cfg(test)]
    memo_skew: Mutex<Duration>,
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
            retry_wait: RETRY_WAIT,
            budget: STAGE_BUDGET,
            failures: Mutex::new(memo),
            #[cfg(test)]
            memo_skew: Mutex::new(Duration::ZERO),
        }
    }

    /// 失敗の記憶が見る「今」。本番は実時間そのもの。
    fn memo_now(&self) -> Instant {
        let now = Instant::now();
        #[cfg(test)]
        let now = now + *self.memo_skew.lock().unwrap_or_else(|e| e.into_inner());
        now
    }

    /// 失敗の記憶の時計を進める（試験用。実時間を待たずに、バックオフが明けた状態にする）。
    #[cfg(test)]
    pub(crate) fn advance_memo_clock(&self, by: Duration) {
        *self.memo_skew.lock().unwrap_or_else(|e| e.into_inner()) += by;
    }

    /// 再試行の前の待ちを変える（試験用。本番は [`RETRY_WAIT`]）。
    #[cfg(test)]
    pub(crate) fn with_retry_wait(mut self, retry_wait: Duration) -> Self {
        self.retry_wait = retry_wait;
        self
    }

    /// 段全体の経過時間の上限を変える（試験用。本番は [`STAGE_BUDGET`]）。
    #[cfg(test)]
    pub(crate) fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
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
///
/// # 本文を読む失敗の分類
///
/// 状態コードが 2xx のあと、本文を読む最中の失敗（`read_to_vec`）。ヘッダーの前の失敗は [`Failure`] の表（`net.rs`）。
///
/// | 失敗 | `CallError` | 障害か |
/// | --- | --- | --- |
/// | 上限（1 MiB・8 MiB）を超えた（`BodyExceedsLimit`） | `Bad` | 障害でない（応答の中身の問題） |
/// | 時間切れ・切断・TLS など、[`Failure::from`] が障害にするもの | `Net(その種類)` | 障害 |
/// | それ以外すべて（`Protocol`＝不正なチャンク長、`Decompress`＝途中で終わる gzip など） | `Net(`[`Failure::BodyUnreadable`]`)` | 障害 |
///
/// 数百バイトの本文が読めないのは、座標の中身でなく回線・相手の不調と言えるので、Drive の 403 の理由の本文
/// （`ReasonUnreadable`）と同じく障害にする。ただし座標による場合（特定の座標の応答だけが壊れる）もありうるので、
/// 地理院の段では「座標による障害」として扱う（[`CallError::is_global`]。連続して複数の座標で続いたら中断する）。
/// ヘッダーの前の `Protocol`（応答の頭が壊れている）は `Other` のままで障害にしない（`net.rs` の表）。
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

    /// 座標によらない全体の障害か（名前解決・接続・TLS・流量制限）。そうでない障害（5xx・時間切れ・切断・
    /// 本文が読めない）は、特定の座標の応答だけがそうなることもある。
    fn is_global(self) -> bool {
        match self {
            Self::Net(failure) => matches!(failure, Failure::Dns | Failure::Connect | Failure::Tls),
            Self::Status(status) => status == 429,
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

/// 本文を読む最中の `ureq` のエラーを、障害の種類にする（`BodyExceedsLimit` は呼び出し側が先に取り除く）。
pub(super) fn body_failure(e: &ureq::Error) -> Failure {
    match Failure::from(e) {
        failure if failure.is_outage() => failure,
        _ => Failure::BodyUnreadable,
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
    // 上限は展開後の大きさに効かせる（`net::read_body_limited`）
    net::read_body_limited(response.body_mut(), limit).map_err(|e| match e {
        ureq::Error::BodyExceedsLimit(_) => CallError::Bad,
        other => CallError::Net(body_failure(&other)),
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
    /// 通信を試した行（最初の要求を出した時点で数える。成否・途中の停止によらない）。
    queried: u64,
    /// うち、確定した行。
    resolved: u64,
    /// うち、確定しなかった行（応答なし・障害・停止の合図・時間の上限で打ち切った行）。`queried = resolved + unresolved`。
    unresolved: u64,
    /// 通信できない間・時間の上限のあと・同じキーの先の行が未確定で、見送った行（通信していない）。
    deferred: u64,
    would_query: u64,
}

/// 1 サイクルの処理の途中の状態。
struct Pass {
    tally: Tally,
    /// このサイクルで先の行が確定しなかったキー（障害・応答なし・見送り・時間の上限）。同じキーの後の行は、
    /// 先の行を追い越さないよう見送る。前のサイクルから続くもの（バックオフ中の行）は、候補を選ぶ段階で
    /// 同じキーの後の行ごと枠から外す。
    blocked: HashSet<String>,
    /// 通信を続けられなくなった理由。以降の通信が要る行は見送り、通信の要らない行は済ませる。
    halted: Option<StageError>,
    /// 段全体の経過時間の上限に達した。`halted` と同じく通信が要る行は見送るが、失敗ではない。
    out_of_time: bool,
    /// この時刻を過ぎたら新しい要求を始めない。
    deadline: Instant,
    /// 市区町村の表。最初に通信が要る行が来たときに用意する。
    muni: Option<MuniTable>,
    /// 座標による障害（5xx・時間切れ・切断・本文が読めない）で、続けて確定できなかった**異なるキー**。
    /// 応答が返った座標（確定した・応答が使えなかった）で空に戻る。
    failed_keys: HashSet<String>,
    /// `--dry-run` で、実行なら書くキャッシュのキー（国外の行が上書きするキー、問い合わせる行が作るキー）。
    simulated: HashSet<String>,
}

/// 1 つの座標の問い合わせの結果。
enum Lookup {
    /// 確定する値。
    Answered(Entry),
    /// 両方の API の応答が使えなかった（本文が壊れている・4xx など）。障害ではない。
    NoAnswer,
    /// どちらかが通信の障害のまま再試行が尽きた。確定しない。
    Failed(CallError),
    /// 再試行の待ちの途中で停止の合図が来た。
    Stopped,
    /// 段全体の経過時間の上限に達した（次の要求を始める前）。
    OutOfTime,
}

/// 再試行つきの 1 回の JSON の取得の結果。
enum Call {
    Json(Value),
    /// 応答は届いたが使えない（障害ではない）。
    Unusable,
    Failed(CallError),
    Stopped,
    OutOfTime,
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
        // 現行の `LIMIT 40`。失敗が続く座標（バックオフ中）は枠に入れずに飛ばすが、その座標は**確定していない
        // 先の行**なので、同じキーの後の行も枠に入れない（先に空のキャッシュを作って追い越すため）。
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut blocked: HashSet<String> = HashSet::new();
        let mut skipped_backoff = 0u64;
        let mut skipped_blocked = 0u64;
        {
            let mut stmt = ctx.read.prepare(&format!(
                "SELECT id, lat, lon FROM photos WHERE {USABLE} ORDER BY id"
            ))?;
            let mut rows = stmt.query([])?;
            let memo = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            let now = self.memo_now();
            while candidates.len() < PER_CYCLE {
                let Some(row) = rows.next()? else { break };
                let candidate = Candidate {
                    id: row.get(0)?,
                    lat: row.get(1)?,
                    lon: row.get(2)?,
                };
                let key = cache_key(candidate.lat, candidate.lon);
                if memo.is_waiting(&candidate.memo_key(), now) {
                    skipped_backoff += 1;
                    blocked.insert(key);
                    continue;
                }
                if blocked.contains(&key) {
                    skipped_blocked += 1;
                    continue;
                }
                candidates.push(candidate);
            }
        }
        let mut pass = Pass {
            tally: Tally::default(),
            blocked: HashSet::new(),
            halted: None,
            out_of_time: false,
            deadline: Instant::now() + self.budget,
            muni: None,
            failed_keys: HashSet::new(),
            simulated: HashSet::new(),
        };
        self.process(ctx, &candidates, &mut pass)?;
        let tally = &pass.tally;
        let counts = if ctx.is_dry_run() {
            Counts(vec![
                ("candidates", candidates.len() as u64),
                ("foreign", tally.foreign),
                ("cache_hit", tally.cache_hit),
                ("would_query", tally.would_query),
                ("unresolved", tally.unresolved),
                ("skipped_backoff", skipped_backoff),
                ("skipped_blocked", skipped_blocked),
                ("unusable", unusable),
                ("pending", pending),
            ])
        } else {
            Counts(vec![
                ("candidates", candidates.len() as u64),
                ("foreign", tally.foreign),
                ("cache_hit", tally.cache_hit),
                ("queried", tally.queried),
                ("resolved", tally.resolved),
                ("unresolved", tally.unresolved),
                ("deferred", tally.deferred),
                ("skipped_backoff", skipped_backoff),
                ("skipped_blocked", skipped_blocked),
                ("out_of_time", u64::from(pass.out_of_time)),
                ("unusable", unusable),
                ("pending", pending),
            ])
        };
        match pass.halted {
            None => Ok(StageOutcome::Done(counts)),
            // 件数は失敗の文面に載せる（途中までは確定している）
            Some(e) => Err(StageError::text(format!("{e}（{counts}）"))),
        }
    }
}

impl Enrich {
    /// 候補を `id` 順に 1 行ずつ処理する（現行の `for r in dbq(...)` と同じ順序）。DB の書き込みの失敗は `Err`。
    /// 通信の障害・`muni.js` の失敗は `pass.halted` に残し、確定した行はそのまま残す。
    fn process(
        &self,
        ctx: &StageCtx<'_>,
        candidates: &[Candidate],
        pass: &mut Pass,
    ) -> Result<(), StageError> {
        let dry_run = ctx.is_dry_run();
        for candidate in candidates {
            if ctx.stop.is_stopped() {
                return Ok(());
            }
            let key = cache_key(candidate.lat, candidate.lon);
            // 先の行がまだ確定していないキーは追い越さない（国外の行の空の上書きが先に入ると、先の国内の行が
            // 空で確定してしまう）
            if pass.blocked.contains(&key) {
                pass.tally.deferred += 1;
                continue;
            }
            if !in_japan(candidate.lat, candidate.lon) {
                pass.tally.foreign += 1;
                if dry_run {
                    pass.simulated.insert(key);
                } else {
                    // 国外はキャッシュを引かずに上書きする（現行と同じ）
                    let entry = Entry::empty();
                    ctx.cache.put(&key, entry.to_json());
                    store(ctx, candidate, &entry)?;
                }
                continue;
            }
            let cached = ctx.cache.get(&key).as_ref().and_then(Entry::from_cache);
            if dry_run {
                if cached.is_some() || pass.simulated.contains(&key) {
                    pass.tally.cache_hit += 1;
                } else {
                    pass.simulated.insert(key);
                    pass.tally.would_query += 1;
                }
                continue;
            }
            if let Some(entry) = cached {
                pass.tally.cache_hit += 1;
                store(ctx, candidate, &entry)?;
                continue;
            }
            if pass.halted.is_some() || pass.out_of_time {
                pass.tally.deferred += 1;
                pass.blocked.insert(key);
                continue;
            }
            if self.query_row(ctx, candidate, key, pass)?.is_break() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// 通信が要る 1 行を処理する。`Break` は停止の合図。
    fn query_row(
        &self,
        ctx: &StageCtx<'_>,
        candidate: &Candidate,
        key: String,
        pass: &mut Pass,
    ) -> Result<ControlFlow<()>, StageError> {
        let Pass {
            tally,
            blocked,
            halted,
            out_of_time,
            deadline,
            muni,
            failed_keys,
            ..
        } = pass;
        if Instant::now() >= *deadline {
            // 上限のあとは通信が要る行を始めない。先の行として、同じキーの後の行も追い越させない
            *out_of_time = true;
            tally.deferred += 1;
            blocked.insert(key);
            return Ok(ControlFlow::Continue(()));
        }
        let table = match muni {
            Some(table) => table,
            None => match self.muni_table(ctx) {
                Ok(table) => muni.insert(table),
                Err(e) => {
                    // 表が無いまま pref・city 抜きで確定させない。以降の通信が要る行は見送る
                    *halted = Some(e);
                    tally.deferred += 1;
                    blocked.insert(key);
                    return Ok(ControlFlow::Continue(()));
                }
            },
        };
        // 最初の要求を出した時点で「通信を試した」。再試行の待ちで止まった行も数える
        tally.queried += 1;
        match self.lookup(ctx, candidate, table, *deadline) {
            Lookup::Answered(entry) => {
                // 空でも確定として記録し、同じ座標を二度と問い合わせない
                ctx.cache.put(&key, entry.to_json());
                store(ctx, candidate, &entry)?;
                tally.resolved += 1;
                failed_keys.clear();
                self.forget_failure(candidate);
            }
            Lookup::NoAnswer => {
                // 両方の API の応答が使えなかった: 確定せず、次の行へ。この座標はしばらく飛ばす。先の行が確定して
                // いないので、同じキーの後の行は追い越させない
                tally.unresolved += 1;
                failed_keys.clear();
                self.remember_failure(candidate);
                blocked.insert(key);
            }
            Lookup::Failed(error) => {
                tally.unresolved += 1;
                if error.is_global() {
                    // 座標によらない障害: この座標は覚えない（直れば、待たずに試す）
                    *halted = Some(outage_error(error, None));
                } else {
                    // 座標による障害かもしれない: 覚えてしばらく飛ばし、後続を通す。続けて数えるのは**異なるキー**
                    // （同じキーの後の行は下で見送るので、同じ座標の写真が何枚あっても 1 つ）
                    self.remember_failure(candidate);
                    failed_keys.insert(key.clone());
                    if failed_keys.len() >= OUTAGE_STREAK {
                        *halted = Some(outage_error(error, Some(failed_keys.len())));
                    }
                }
                blocked.insert(key);
            }
            Lookup::OutOfTime => {
                // 要求の途中で上限に達した行: 確定せず、記憶にも入れない（次のサイクルで先頭から試す）
                tally.unresolved += 1;
                *out_of_time = true;
                blocked.insert(key);
                return Ok(ControlFlow::Continue(()));
            }
            Lookup::Stopped => {
                tally.unresolved += 1;
                return Ok(ControlFlow::Break(()));
            }
        }
        // 地理院への連続リクエストを抑制する。通信を試した行は結果によらず待つ。停止の合図が来たら待たずに戻る
        if ctx.stop.wait(self.pause) {
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    }

    /// 座標の失敗を記憶に入れる（しばらく飛ばす）。
    fn remember_failure(&self, candidate: &Candidate) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_failure(candidate.memo_key(), self.memo_now());
    }

    fn forget_failure(&self, candidate: &Candidate) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_success(&candidate.memo_key());
    }

    /// 逆ジオコーダと標高 API を呼ぶ。
    ///
    /// - どちらかが通信の障害のまま再試行が尽きたら `Failed`（もう片方に応答があっても確定しない。逆ジオが障害なら
    ///   標高は呼ばない）。
    /// - 両方の応答が使えなければ `NoAnswer`。片方だけ使えれば、その分で確定する（現行と同じ）。
    fn lookup(
        &self,
        ctx: &StageCtx<'_>,
        candidate: &Candidate,
        muni: &MuniTable,
        deadline: Instant,
    ) -> Lookup {
        let fill = |template: &str| {
            template
                .replace("{lat}", &py_float(candidate.lat))
                .replace("{lon}", &py_float(candidate.lon))
        };
        let mut entry = Entry::empty();
        let mut answered = false;
        match self.call_json(ctx, &fill(&self.endpoints.rgeo), deadline) {
            Call::Json(data) => {
                answered = true;
                apply_rgeo(&mut entry, &data, muni);
            }
            Call::Unusable => {}
            Call::Failed(e) => return Lookup::Failed(e),
            Call::Stopped => return Lookup::Stopped,
            Call::OutOfTime => return Lookup::OutOfTime,
        }
        match self.call_json(ctx, &fill(&self.endpoints.elev), deadline) {
            Call::Json(data) => {
                answered = true;
                apply_elev(&mut entry, &data);
            }
            Call::Unusable => {}
            Call::Failed(e) => return Lookup::Failed(e),
            Call::Stopped => return Lookup::Stopped,
            Call::OutOfTime => return Lookup::OutOfTime,
        }
        if answered {
            Lookup::Answered(entry)
        } else {
            Lookup::NoAnswer
        }
    }

    /// 1 つの URL を取る。通信の障害は [`RETRIES`] 回まで、短い待ちを置いて取り直す（待ちは停止の合図で打ち切る）。
    /// 要求（再試行を含む）を始める前に `deadline` を見る。
    fn call_json(&self, ctx: &StageCtx<'_>, url: &str, deadline: Instant) -> Call {
        let mut retried = 0;
        loop {
            if Instant::now() >= deadline {
                return Call::OutOfTime;
            }
            match fetch_json(ctx, url) {
                Ok(data) => return Call::Json(data),
                Err(e) if e.is_outage() => {
                    if retried >= RETRIES {
                        return Call::Failed(e);
                    }
                    retried += 1;
                    if ctx.stop.wait(self.retry_wait * retried) {
                        return Call::Stopped;
                    }
                }
                Err(_) => return Call::Unusable,
            }
        }
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

/// 段を中断する理由。URL・座標を含まない。`streak` は座標による障害が続いた座標の数。
fn outage_error(error: CallError, streak: Option<usize>) -> StageError {
    match streak {
        None => StageError::text(format!(
            "地理院への問い合わせが通信の障害で中断しました（{}）",
            error.describe()
        )),
        Some(n) => StageError::text(format!(
            "地理院への問い合わせが、続けて{n}つの座標で通信の障害になったので中断しました（{}）",
            error.describe()
        )),
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
