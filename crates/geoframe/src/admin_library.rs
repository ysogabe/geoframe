//! 管理画面の「ライブラリ」タブの読み取り API（docs/admin-library/design.md）。
//!
//! - `GET /api/admin/library/trips`: 撮影日時のある写真を 48 時間超の空きで旅に区切った一覧（新しい順）。
//! - `GET /api/admin/library/photos`: 期間・範囲・ID・場所（旧形式の `pref`/`city`/`abroad`、新形式の `country`/`region`/…）・日時なし・
//!   文字（`q`）で絞った写真のページ。各写真に、任意の `location_region`（国・地域・判定方法・表示用のラベル）が付く。
//! - `GET /api/admin/library/points`: 位置のある写真の GeoJSON（地図用）。
//! - `GET /api/admin/library/calendar`: 年・月ごとの件数と、年ごとの代表の写真（日付の眺め方）。
//! - `GET /api/admin/library/places`: 国・地域ごとの件数・範囲・代表の写真（`countries`）、国が決まらない写真（`unknown`）、
//!   旧形式の都道府県・市区町村（`prefs`）と海外・不明（`abroad`）（場所の眺め方。期間 `from`・`to` と地図の範囲 `bbox` で絞れる）。
//!   区分は [`crate::regions::PlaceRules`]（設計「場所の区分の規則」）で 1 つに決まる。
//!
//! 読むのは `photos` 表と、国・地域の分類 `photo_regions` 表（`places`・`trips`・`photos`。表が無い旧 DB では空の派生表を結合し、
//! 表は作らない）。`photos` の絞り込み（`country` など）・文字検索 `q`・応答の `location_region` と、`trips` の地名は、
//! [`crate::regions::PlaceRules`] の式を使う（規則をここで書き直さない）。`photo_curation` などの新しい表が無い DB、
//! `GEOFRAME_INGEST=0` の読み取り専用の起動でも動く。経路は完全一致の `GET` のみで、`server.rs` の
//! `site` が呼ぶ。**更新はしない**（分類の表も API からは作らない・書かない）。
//!
//! クエリは `/api/admin/photos` と同じく `parse_qs` の最初の値で読む（空の値は無かったことになる）。形が違えば
//! 400 `bad_params`（値は応答にもログにも出さない）。SQL の条件は固定の文字列の候補だけから組み、値はすべてバインドする。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};

use crate::admin::{AdminContext, PER_PAGE_MAX, cell, text, thumb_for, truthy};
use crate::photos::{TRIP_GAP_SECONDS, parse_epoch};
use crate::pyquery;
use crate::regions::{self, data};
use crate::server::{AppState, error_response, json_response};

/// `points` が 1 回で返す点の上限。超えたら撮影日時の新しい順に切る。
pub const POINTS_MAX: usize = 20_000;
/// `ids` の個数の上限（重複を除く前）。
const IDS_MAX: usize = 200;
/// `pref`・`city` の文字数の上限。
const NAME_MAX_CHARS: usize = 64;
/// `per` の既定。
const PER_DEFAULT: i64 = 60;
/// 旅の `places` に出す上位の数。
const PLACES_MAX: usize = 3;
/// `photos` の `q` の語数の上限（これ以上は 400）。
const Q_WORDS_MAX: usize = 5;
/// `q` の 1 語ぶんの条件。検索する 6 つ（都道府県・市区町村・地名・ファイル名・撮影日時、場所の区分の検索用文字列）の
/// どれかに部分一致する。場所の検索用文字列は [`crate::regions::PlaceRules::search`]（`pref` のある写真は日本と都道府県の別名、
/// それ以外は有効な分類の検索用文字列。国名と地域名は 1 つの文字列なので、両方に一致する語でも写真は 1 回だけ数える）で、
/// ここには同じ意味の [`regions::PlaceRules::search_like`] を入れる。
/// ユーザーの文字列は入れず、`?` に語ごとのパターンをバインドする（Rust のソースの `ESCAPE '\\'` は、SQL では `ESCAPE '\'`）。
fn q_word_sql(search_like: &str) -> String {
    format!(
        "(COALESCE(pref,'') LIKE ? ESCAPE '\\' OR COALESCE(city,'') LIKE ? ESCAPE '\\' \
         OR COALESCE(place,'') LIKE ? ESCAPE '\\' OR COALESCE(name,'') LIKE ? ESCAPE '\\' \
         OR COALESCE(taken_at,'') LIKE ? ESCAPE '\\' OR {search_like})"
    )
}
/// [`q_word_sql`] の `?` の数（5 列と、[`regions::PlaceRules::search_like`] の分）。
const Q_WORD_PARAMS: usize = 5 + regions::SEARCH_LIKE_PARAMS;

/// 「日時あり」の判定（SQL）。分までの撮影日時の形（`YYYY-MM-DD HH:MM`、16 桁）だけを見る。暦としての正しさは見ない
/// （`imaging::norm_time` が暦を検査しないので、`2025-02-30 10:00` も取り込まれる）。`trips`・`photos`・`points` と
/// `from`・`to` の検査はすべてこの形で揃える。`concat!` で他の定数に埋めるため、リテラルはマクロに 1 つだけ置く。
macro_rules! timed_sql {
    () => {
        "taken_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]'"
    };
}
/// 「日時なし」は [`TIMED`] の否定（NULL・空・形の違う値。`''`・`'x'`・`'2025-06'` など）。
macro_rules! untimed_sql {
    () => {
        concat!("(taken_at IS NULL OR NOT (", timed_sql!(), "))")
    };
}
/// 分までの撮影日時の形をしている。
const TIMED: &str = timed_sql!();
/// 日時なし（[`TIMED`] でない）。
const UNTIMED: &str = untimed_sql!();
/// 海外・不明（`pref` が NULL か空）。`trips` の `places` の「海外・不明」と同じ定義。
const ABROAD: &str = "(pref IS NULL OR pref = '')";
/// 除外していない（`hidden` が NULL・0）。
const VISIBLE: &str = "COALESCE(hidden,0)=0";
/// 日時の順（日時なしは最後）、同じなら ID。
const ORDER: &str = concat!(
    "CASE WHEN ",
    untimed_sql!(),
    " THEN 1 ELSE 0 END, COALESCE(taken_at,''), id"
);
/// 新しい順（日時なしは最後）、同じなら ID の大きい方が先。
const ORDER_NEWEST: &str = concat!(
    "CASE WHEN ",
    untimed_sql!(),
    " THEN 1 ELSE 0 END, COALESCE(taken_at,'') DESC, id DESC"
);

pub(crate) type Params = HashMap<String, String>;

pub(crate) fn query_params(query: Option<&str>) -> Params {
    pyquery::parse_qs_first(query.unwrap_or(""))
}

/// `hidden=1` のときだけ除外を含む（それ以外の値は既定）。
fn include_hidden(params: &Params) -> bool {
    params.get("hidden").is_some_and(|v| v == "1")
}

/// `YYYY-MM-DD HH:MM`（各桁が ASCII の数字）の形。暦としての正しさは見ない（[`TIMED`] と同じ形。画面が旅の
/// `key`・`end` をそのまま渡して、暦として読めない日時の旅も引けるようにする）。
fn parse_minute(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let shaped = b.len() == 16
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b' ',
            13 => *c == b':',
            _ => c.is_ascii_digit(),
        });
    shaped.then_some(s)
}

/// `from`・`to`。どちらも形が合い、両方あるときは `from <= to`（固定長の文字列なので文字列の順が時刻の順）。
fn parse_range(params: &Params) -> Option<(Option<String>, Option<String>)> {
    let read = |name: &str| -> Option<Option<String>> {
        match params.get(name) {
            None => Some(None),
            Some(v) => parse_minute(v).map(|v| Some(v.to_owned())),
        }
    };
    let (from, to) = (read("from")?, read("to")?);
    if let (Some(f), Some(t)) = (&from, &to)
        && f > t
    {
        return None;
    }
    Some((from, to))
}

/// `minlon,minlat,maxlon,maxlat`。有限で、経度は ±180・緯度は ±90、`min <= max`。
fn parse_bbox(s: &str) -> Option<[f64; 4]> {
    let mut out = [0.0; 4];
    let mut parts = s.split(',');
    for slot in &mut out {
        let v: f64 = parts.next()?.parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        *slot = v;
    }
    let [min_lon, min_lat, max_lon, max_lat] = out;
    let ok = parts.next().is_none()
        && [min_lon, max_lon].iter().all(|v| v.abs() <= 180.0)
        && [min_lat, max_lat].iter().all(|v| v.abs() <= 90.0)
        && min_lon <= max_lon
        && min_lat <= max_lat;
    ok.then_some(out)
}

/// `1,2,3`。整数だけ。重複を除く前の個数が [`IDS_MAX`] を超えたら `None`。重複は最初のものだけ残す。
pub(crate) fn parse_ids(s: &str) -> Option<Vec<i64>> {
    let mut out: Vec<i64> = Vec::new();
    let mut count = 0usize;
    for part in s.split(',') {
        count += 1;
        if count > IDS_MAX {
            return None;
        }
        let id: i64 = part.parse().ok()?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Some(out)
}

/// `pref`・`city`。64 文字まで。
fn parse_name(s: &str) -> Option<&str> {
    (s.chars().count() <= NAME_MAX_CHARS).then_some(s)
}

/// `q`。前後の空白を除いた長さが [`NAME_MAX_CHARS`] 文字まで、空白（全角も）で分けた語が [`Q_WORDS_MAX`] 語まで。
/// 外れたら `None`（400）。空になれば `Some(空)`（`q` が無いのと同じ）。`parse_qs` がパーセント解除を済ませている。
fn parse_q(s: &str) -> Option<Vec<&str>> {
    let s = s.trim();
    if s.chars().count() > NAME_MAX_CHARS {
        return None;
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    (words.len() <= Q_WORDS_MAX).then_some(words)
}

/// `LIKE` のパターン（前後を `%` にした部分一致）。`ESCAPE` の文字は `\`。語の `\` を先に `\\` へ、そのあと `%`・`_` を
/// `\%`・`\_` へ置き換える（`%`・`_` を先に直すと、足した `\` がもう一度 `\\` になる）。
fn like_pattern(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// `WHERE` の条件（固定の文字列）とそのバインド値。
#[derive(Default)]
struct Filter {
    conditions: Vec<String>,
    params: Vec<Sql>,
    /// 絞り込みが 1 つでもあるか（`hidden` は数えない）。
    narrowed: bool,
    /// 条件が [`regions::PlaceRules`] の式（都道府県の対応表 `pm`・有効な分類 `r`）を使うか。使うときだけ、読み取りは
    /// `rules.from` で引く（旧形式の絞り込みと文字検索だけなら `photos` のままで、旧形式は変更前と同じ文・同じ結果）。
    rules: bool,
    /// `q` の語のパターン（`LIKE` 用）。分類の表の有無で条件の文が変わるので、読み取りで [`Filter::render`] が展開する。
    words: Vec<String>,
}

impl Filter {
    fn push(&mut self, condition: &'static str, params: impl IntoIterator<Item = Sql>) {
        self.conditions.push(condition.to_owned());
        self.params.extend(params);
    }

    /// [`regions::PlaceRules`] の式を使う条件（`condition` は固定の文字列と規則の式だけで、値は `?` にバインドする）。
    fn push_rule(&mut self, condition: String, params: impl IntoIterator<Item = Sql>) {
        self.conditions.push(condition);
        self.params.extend(params);
        self.rules = true;
        self.narrowed = true;
    }

    fn range(&mut self, from: Option<String>, to: Option<String>) {
        if from.is_some() || to.is_some() {
            // 文字列の比較なので、日時なしや形の違う値が `<=` に紛れ込まないよう、分までの形の行（[`TIMED`]）だけにする
            self.conditions.push(TIMED.to_owned());
            self.narrowed = true;
        }
        if let Some(from) = from {
            self.push("taken_at >= ?", [Sql::Text(from)]);
        }
        if let Some(to) = to {
            self.push("taken_at <= ?", [Sql::Text(to)]);
        }
    }

    /// 位置があり、範囲（両端を含む）に入る写真だけ。
    fn bbox(&mut self, [min_lon, min_lat, max_lon, max_lat]: [f64; 4]) {
        self.push(
            "lat IS NOT NULL AND lon IS NOT NULL AND lon >= ? AND lon <= ? AND lat >= ? AND lat <= ?",
            [min_lon, max_lon, min_lat, max_lat].map(Sql::Real),
        );
        self.narrowed = true;
    }

    /// 語ごとに 6 つのどれかに部分一致、語同士は AND。語はパターンとして持ち、条件は [`Filter::render`] が固定の型を語の数だけ並べる。
    fn words(&mut self, words: &[&str]) {
        self.words.extend(words.iter().map(|w| like_pattern(w)));
        if !words.is_empty() {
            self.narrowed = true;
        }
    }

    fn hidden(&mut self, include: bool) {
        if !include {
            self.conditions.push(VISIBLE.to_owned());
        }
    }

    fn where_clause(&self) -> String {
        if self.conditions.is_empty() {
            "1=1".to_owned()
        } else {
            self.conditions.join(" AND ")
        }
    }

    /// `WHERE` の条件とバインド値。`q` の語を、分類の表の有無に合わせた [`regions::PlaceRules::search_like`] で展開する。
    fn render(&self, rules: &regions::PlaceRules) -> (String, Vec<Sql>) {
        let mut conditions = self.conditions.clone();
        let mut params = self.params.clone();
        for pattern in &self.words {
            conditions.push(q_word_sql(&rules.search_like));
            params.extend(std::iter::repeat_n(
                Sql::Text(pattern.clone()),
                Q_WORD_PARAMS,
            ));
        }
        let clause = if conditions.is_empty() {
            "1=1".to_owned()
        } else {
            conditions.join(" AND ")
        };
        (clause, params)
    }
}

/// 新形式の場所の絞り込みのキー（`country`・`region`・`region_unknown`・`unclassified`・`no_location`）の 1 つが、旧形式のキー
/// （`pref`・`abroad`）と同時に来たら 400。`0`/`1` の旗は、`0` なら無いのと同じ。`1`/`0` 以外の値は 400（`None`）。
fn flag(params: &Params, name: &str) -> Option<bool> {
    match params.get(name).map(String::as_str) {
        None | Some("0") => Some(false),
        Some("1") => Some(true),
        Some(_) => None,
    }
}

/// 国・地域の ID。1〜64 文字の ASCII 英数字と `:_-`（`parse_qs` が空の値を捨てるので、空は無いのと同じ）。
fn parse_place_id(s: &str) -> Option<&str> {
    data::valid_id(s).then_some(s)
}

/// 場所の絞り込み（設計「GET /api/admin/library/photos」）。旧形式（`pref`・`city`・`abroad`）は今の意味のまま、
/// 新形式（`country`／`country&region`／`country&region_unknown=1`／`unclassified=1`／`no_location=1`）は
/// [`regions::PlaceRules`] の式で絞る。2 つの形は混ぜられない。形が違えば `None`（400）。
fn place_filter(params: &Params, filter: &mut Filter, rules: &regions::PlaceRules) -> Option<()> {
    let country: Option<Option<&str>> = params.get("country").map(|v| parse_place_id(v));
    let (region_unknown, unclassified, no_location, abroad) = (
        flag(params, "region_unknown")?,
        flag(params, "unclassified")?,
        flag(params, "no_location")?,
        params.get("abroad").is_some_and(|v| v == "1"),
    );
    // 旧形式のキー。`city` は、`country` と一緒に来たときだけ新形式の一部
    let legacy_pref = params.contains_key("pref") || abroad;
    let new_flag = unclassified || no_location;
    match country {
        Some(country) => {
            let country = country?;
            if legacy_pref || new_flag {
                return None;
            }
            let names = data::names().ok();
            let region = match params.get("region") {
                Some(region) => Some(parse_place_id(region)?),
                None => None,
            };
            if region.is_some() && region_unknown {
                return None;
            }
            // 別の国の地域は 400。名称表に無い ID（文法は正しい）はそのまま 0 件になる
            if let (Some(region), Some(names)) = (region, names)
                && let Some(place) = names.region(region)
                && place.country.as_deref() != Some(country)
            {
                return None;
            }
            filter.push_rule(
                format!("{} = ?", rules.country),
                [Sql::Text(country.to_owned())],
            );
            if let Some(region) = region {
                filter.push_rule(
                    format!("{} = ?", rules.region),
                    [Sql::Text(region.to_owned())],
                );
            }
            if region_unknown {
                filter.push_rule(format!("{} IS NULL", rules.region), []);
            }
            if let Some(city) = params.get("city") {
                // 日本の地域のときだけ: 都道府県名（名称表）と市区町村の両方に一致する
                let pref = (country == data::JAPAN_ID)
                    .then_some(region)
                    .flatten()
                    .and_then(|r| names?.region(r)?.pref.as_deref())?;
                filter.push_rule(
                    "p.pref = ? AND p.city = ?".to_owned(),
                    [
                        Sql::Text(pref.to_owned()),
                        Sql::Text(parse_name(city)?.to_owned()),
                    ],
                );
            }
        }
        None if new_flag || params.contains_key("region") || region_unknown => {
            // `region`・`region_unknown` は `country` が要る。`unclassified`・`no_location` は 1 つだけで、旧形式とも混ぜない
            if params.contains_key("region")
                || region_unknown
                || (unclassified && no_location)
                || legacy_pref
                || params.contains_key("city")
            {
                return None;
            }
            if unclassified {
                filter.push_rule(rules.unclassified.clone(), []);
            } else {
                filter.push_rule(rules.no_location.clone(), []);
            }
        }
        None => {
            // 旧形式（今の画面が送る）。意味は変更前のまま
            if let Some(pref) = params.get("pref") {
                filter.push("pref = ?", [Sql::Text(parse_name(pref)?.to_owned())]);
                filter.narrowed = true;
            }
            if let Some(city) = params.get("city") {
                filter.push("city = ?", [Sql::Text(parse_name(city)?.to_owned())]);
                filter.narrowed = true;
            }
            // 海外・不明（`pref` が NULL か空。位置なしも入る）。`parse_qs` は空の値（`pref=`）を捨てるので、`pref` では絞れない。
            // `pref`・`city` とは同時に受けない
            if abroad {
                if params.contains_key("pref") || params.contains_key("city") {
                    return None;
                }
                filter.push(ABROAD, []);
                filter.narrowed = true;
            }
        }
    }
    Some(())
}

/// `photos` の絞り込み。形が違うか、絞り込みが 1 つも無ければ `None`。
///
/// 場所の式は [`regions::PlaceRules`] のものを使う。式は分類の表の有無に依らず同じで、違うのは `from` だけなので、ここでは
/// 表の有無を引かない（`from` は読み取りで選ぶ）。
fn photos_filter(params: &Params) -> Option<Filter> {
    let rules = regions::PlaceRules::new(true);
    let mut filter = Filter::default();
    let (from, to) = parse_range(params)?;
    filter.range(from, to);
    if let Some(bbox) = params.get("bbox") {
        filter.bbox(parse_bbox(bbox)?);
    }
    if let Some(q) = params.get("q") {
        filter.words(&parse_q(q)?);
    }
    if let Some(ids) = params.get("ids") {
        let ids = parse_ids(ids)?;
        // 個数ぶんの `?` だけを足す（ユーザーの文字列は SQL に入れない）
        let marks = vec!["?"; ids.len()].join(",");
        filter.params.extend(ids.into_iter().map(Sql::Integer));
        filter.conditions.push(format!("id IN ({marks})"));
        filter.narrowed = true;
    }
    place_filter(params, &mut filter, &rules)?;
    if params.get("untimed").is_some_and(|v| v == "1") {
        filter.push(UNTIMED, []);
        filter.narrowed = true;
    }
    filter.hidden(include_hidden(params));
    filter.narrowed.then_some(filter)
}

pub(crate) fn bad_params() -> Response {
    error_response(StatusCode::BAD_REQUEST, "bad_params")
}

/// 読み取りの結果を JSON の応答にする。読み取りの失敗・順番待ちの超過は `/api/admin/photos` と同じ 503。
pub(crate) fn respond<E: std::fmt::Display>(result: Result<Value, E>) -> Response {
    match result {
        Ok(body) => match serde_json::to_vec(&body) {
            Ok(bytes) => json_response(StatusCode::OK, bytes),
            Err(e) => {
                tracing::error!("ライブラリの応答を JSON にできません: {e}");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "内部エラーが発生しました",
                )
            }
        },
        Err(e) => {
            tracing::warn!("ライブラリを読めません: {e}");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "一覧を読み取れません。しばらくしてから再試行してください",
            )
        }
    }
}

// ---------------------------------------------------------------- trips

/// 旅の表紙の候補。
struct Cover {
    id: i64,
    thumb_url: String,
    /// 位置があり除外でない（表紙の優先）。
    preferred: bool,
}

/// 表紙の `thumb_url`。表示ファイルも残った derivative も無ければ空（旅・年・都道府県で共通。画面は代わりの表示を出す）。
/// 世代は `thumb.rs` の lookup と同じ入力（DB の display・md5・mtime）。
fn cover_thumb_url(
    ctx: &AdminContext,
    id: i64,
    display: Option<String>,
    drive_id: Option<String>,
    md5: Option<String>,
    mtime: Option<f64>,
) -> String {
    if thumb_for(ctx, display.as_deref(), drive_id.as_deref()).is_empty() {
        String::new()
    } else {
        crate::thumb::thumb_url(id, display.as_deref(), md5.as_deref(), mtime)
    }
}

/// 年・都道府県の代表の写真を、組ごとに 1 行ずつ取る SQL の並び（旅の表紙と同じ考え方）。除外でない → 位置がある →
/// 新しい（日時なしは最後）→ ID が大きい。除外を含めたときだけ、除外の写真が後ろに回る。
const COVER_ORDER: &str = concat!(
    "COALESCE(hidden,0)<>0, (lat IS NULL OR lon IS NULL), ",
    "CASE WHEN ",
    untimed_sql!(),
    " THEN 1 ELSE 0 END, COALESCE(taken_at,'') DESC, id DESC"
);

/// `key_sql`（組を決める式）ごとの代表の写真 `{id, thumb_url}`。`clause` を満たす写真だけから選ぶ。
fn covers(
    conn: &Connection,
    ctx: &AdminContext,
    key_sql: &str,
    clause: &str,
    params: &[Sql],
) -> rusqlite::Result<HashMap<String, Value>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT k, id, display, drive_id, md5, mtime FROM (
           SELECT {key_sql} AS k, id, display, drive_id, md5, mtime,
                  ROW_NUMBER() OVER (PARTITION BY {key_sql} ORDER BY {COVER_ORDER}) AS rn
             FROM photos WHERE {clause})
          WHERE rn = 1"
    ))?;
    let mut rows = stmt.query(params_from_iter(params.iter()))?;
    let mut out = HashMap::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(1)?;
        let thumb_url =
            cover_thumb_url(ctx, id, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?);
        out.insert(
            row.get::<_, String>(0)?,
            json!({"id": id, "thumb_url": thumb_url}),
        );
    }
    Ok(out)
}

struct Trip {
    start: String,
    end: String,
    count: i64,
    noloc: i64,
    places: HashMap<(String, String), i64>,
    /// 国・地域（ID）ごとの件数。国が決まらない写真は数えない。地域が決まらなければ地域の ID は `None`。
    regions: HashMap<PlaceIds, i64>,
    cover: Option<Cover>,
}

impl Trip {
    fn new(taken_at: &str) -> Self {
        Self {
            start: taken_at.to_owned(),
            end: taken_at.to_owned(),
            count: 0,
            noloc: 0,
            places: HashMap::new(),
            regions: HashMap::new(),
            cover: None,
        }
    }

    fn into_json(self, names: Option<&data::NameTable>) -> Value {
        let mut places: Vec<((String, String), i64)> = self.places.into_iter().collect();
        // 件数の多い順、同数なら名前の順（応答を安定させる）
        places.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        places.truncate(PLACES_MAX);
        let places: Vec<Value> = places
            .into_iter()
            .map(|((pref, city), count)| json!({"pref": pref, "city": city, "count": count}))
            .collect();
        // 国・地域の上位。件数の多い順、同数なら国名・地域名（地域不明が先）、それも同じなら ID（応答を安定させる）。
        // 名前は並べ替えには借りて比べ、表示用の項目は上位だけ作る（旅ごとに国・地域は数百になりうる）
        fn name_of<'a>(names: Option<&'a data::NameTable>, id: &'a str, region: bool) -> &'a str {
            names
                .and_then(|n| if region { n.region(id) } else { n.country(id) })
                .map_or(id, |p| p.name.as_str())
        }
        let mut regions: Vec<(PlaceIds, i64)> = self.regions.into_iter().collect();
        regions.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| name_of(names, a.0.0, false).cmp(name_of(names, b.0.0, false)))
                .then_with(|| {
                    let region = |ids: PlaceIds| ids.1.map(|r| name_of(names, r, true));
                    region(a.0).cmp(&region(b.0))
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        regions.truncate(PLACES_MAX);
        let regions: Vec<Value> = regions
            .into_iter()
            .filter_map(|(ids, count)| {
                let mut map = region_fields(names, Some(ids.0), ids.1)?;
                map.insert("count".to_owned(), json!(count));
                Some(Value::Object(map))
            })
            .collect();
        let cover = self
            .cover
            .map(|c| json!({"id": c.id, "thumb_url": c.thumb_url}));
        json!({
            "key": self.start,
            "start": self.start,
            "end": self.end,
            "count": self.count,
            "noloc": self.noloc,
            "places": places,
            "regions": regions,
            "cover": cover,
        })
    }
}

/// `GET /api/admin/library/trips`。
pub async fn trips(state: &AppState, query: Option<&str>) -> Response {
    let hidden = include_hidden(&query_params(query));
    let ctx = Arc::clone(&state.admin);
    respond(
        state
            .catalog
            .read(move |conn| build_trips(conn, &ctx, hidden))
            .await,
    )
}

fn build_trips(
    conn: &Connection,
    ctx: &AdminContext,
    with_hidden: bool,
) -> rusqlite::Result<Value> {
    // 旅と日時なしの件数が同じ時点の DB から出るように、1 つの読み取りにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let visible = if with_hidden {
        ""
    } else {
        " AND COALESCE(hidden,0)=0"
    };
    // 「日時あり」は `photos`・`points` と同じ形の判定（[`TIMED`]）。暦として読めない日時もここに入る
    let mut stmt = conn.prepare(&format!(
        "SELECT id, taken_at, lat, lon, pref, city, hidden, display, drive_id, md5, mtime
           FROM photos WHERE {TIMED}{visible}
          ORDER BY taken_at, id"
    ))?;
    // 国・地域は `places` と同じ規則。`pref` のある写真（規則 1）は日本と `pref` の対応（[`regions::pref_region`]）、
    // 無い写真は有効な分類の式で引く（国が決まる写真だけ。旅の走査に分類の結合を掛けない）
    let rules = regions::PlaceRules::new(regions::available(conn)?);
    let classified = classified_places(conn, &rules, "1=1", &[])?;
    let mut rows = stmt.query([])?;
    let mut trips: Vec<Trip> = Vec::new();
    let mut last: Option<i64> = None;
    // 直前の行が暦として読めない日時だったときの、その文字列
    let mut unreadable: Option<String> = None;
    while let Some(row) = rows.next()? {
        let taken_at = text(row, 1)?;
        // 形は合うが暦として読めない（`2025-02-30 10:00` など）行は落とさず、旅にする。`photos::assign_trips` と同じく
        // 前後の読める行とは別の旅で（`last` を空にして、空きを測る相手を無くす。読める行が来たらその時刻で置き直す）、
        // 直前の旅が同じ文字列の「読めない日時の旅」ならそこに足す（同じ `key`・`start`・`end` の旅を 2 つ作らない。
        // `ORDER BY taken_at, id` なので同じ文字列は隣り合う）
        let epoch = parse_epoch(&taken_at);
        let starts_new_trip = match epoch {
            Some(e) => last.is_none_or(|l| e - l > TRIP_GAP_SECONDS),
            None => unreadable.as_deref() != Some(taken_at.as_str()),
        };
        if starts_new_trip {
            trips.push(Trip::new(&taken_at));
        }
        last = epoch;
        unreadable = epoch.is_none().then(|| taken_at.clone());
        let trip = trips.last_mut().expect("直前で足した");
        trip.end.clone_from(&taken_at);
        trip.count += 1;
        let located = !matches!(row.get_ref(2)?, rusqlite::types::ValueRef::Null)
            && !matches!(row.get_ref(3)?, rusqlite::types::ValueRef::Null);
        if !located {
            trip.noloc += 1;
        }
        let pref = text(row, 4)?;
        let place = if pref.is_empty() {
            classified.get(&row.get::<_, i64>(0)?).copied()
        } else {
            Some((data::JAPAN_ID, regions::pref_region(&pref)))
        };
        let key = if pref.is_empty() {
            (String::new(), String::new())
        } else {
            (pref, text(row, 5)?)
        };
        *trip.places.entry(key).or_insert(0) += 1;
        if let Some(place) = place {
            *trip.regions.entry(place).or_insert(0) += 1;
        }
        // 表紙は、位置があり除外でない最初の写真。無ければ先頭
        let preferred = located && !truthy(row.get_ref(6)?);
        if trip
            .cover
            .as_ref()
            .is_none_or(|c| !c.preferred && preferred)
        {
            let id: i64 = row.get(0)?;
            trip.cover = Some(Cover {
                id,
                thumb_url: cover_thumb_url(
                    ctx,
                    id,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ),
                preferred,
            });
        }
    }
    drop(rows);
    drop(stmt);
    let names = data::names().ok();
    trips.sort_by(|a, b| b.start.cmp(&a.start));
    let untimed: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM photos WHERE {UNTIMED}{}",
            if with_hidden {
                ""
            } else {
                " AND COALESCE(hidden,0)=0"
            }
        ),
        [],
        |row| row.get(0),
    )?;
    Ok(json!({
        "ok": true,
        "trips": trips.into_iter().map(|t| t.into_json(names)).collect::<Vec<_>>(),
        "untimed": untimed,
    }))
}

// ---------------------------------------------------------------- photos

/// `GET /api/admin/library/photos`。
pub async fn photos(state: &AppState, query: Option<&str>) -> Response {
    let params = query_params(query);
    let int = |name: &str, default: i64| match params.get(name) {
        None => Some(default),
        Some(v) => pyquery::py_int(v),
    };
    let (Some(page), Some(per)) = (int("page", 1), int("per", PER_DEFAULT)) else {
        return bad_params();
    };
    let Some(filter) = photos_filter(&params) else {
        return bad_params();
    };
    let ctx = Arc::clone(&state.admin);
    respond(
        state
            .catalog
            .read(move |conn| build_photos(conn, &ctx, &filter, page, per))
            .await,
    )
}

fn build_photos(
    conn: &Connection,
    ctx: &AdminContext,
    filter: &Filter,
    page: i64,
    per: i64,
) -> rusqlite::Result<Value> {
    // 件数とページが同じ時点の DB から出るように、1 つの読み取りにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let rules = regions::PlaceRules::new(regions::available(conn)?);
    let (clause, params) = filter.render(&rules);
    let per = per.clamp(1, PER_PAGE_MAX);
    // 条件が国・地域の式を使うときだけ結合する（旧形式の絞り込みと文字検索は `photos` だけで引く）
    let from = if filter.rules {
        &rules.from
    } else {
        "photos p"
    };
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {from} WHERE {clause}"),
        params_from_iter(params.iter()),
        |row| row.get(0),
    )?;
    let pages = ((total + per - 1) / per).max(1);
    let page = page.clamp(1, pages);

    let mut stmt = conn.prepare(&format!(
        "SELECT id, name, display, drive_id, taken_at, lat, lon, loc_source,
                place, pref, city, favorite, hidden, rotation, source, md5, mtime
           FROM {from} WHERE {clause} ORDER BY {ORDER} LIMIT ? OFFSET ?"
    ))?;
    let mut bound = params;
    bound.push(Sql::Integer(per));
    bound.push(Sql::Integer((page - 1) * per));
    let mut rows = stmt.query(params_from_iter(bound.iter()))?;
    let mut items = Vec::new();
    let mut prefs = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let display: Option<String> = row.get(2)?;
        let thumb = thumb_for(
            ctx,
            display.as_deref(),
            row.get::<_, Option<String>>(3)?.as_deref(),
        );
        // 世代は `thumb.rs` の lookup と同じ入力（DB の display・md5・mtime）
        let thumb_url = if thumb.is_empty() {
            String::new()
        } else {
            crate::thumb::thumb_url(
                id,
                display.as_deref(),
                row.get::<_, Option<String>>(15)?.as_deref(),
                row.get::<_, Option<f64>>(16)?,
            )
        };
        let rotation = match row.get_ref(13)? {
            value if !truthy(value) => Value::from(0),
            value => cell(value)?,
        };
        prefs.push((id, text(row, 9)?));
        items.push(json!({
            "id": id,
            "name": cell(row.get_ref(1)?)?,
            "thumb": thumb,
            "thumb_url": thumb_url,
            "taken_at": text(row, 4)?,
            "lat": cell(row.get_ref(5)?)?,
            "lon": cell(row.get_ref(6)?)?,
            "loc_source": text(row, 7)?,
            "place": text(row, 8)?,
            "pref": text(row, 9)?,
            "city": text(row, 10)?,
            "favorite": truthy(row.get_ref(11)?),
            "hidden": truthy(row.get_ref(12)?),
            "rotation": rotation,
            "source": cell(row.get_ref(14)?)?,
        }));
    }
    drop(rows);
    drop(stmt);
    // 国・地域は、このページの写真だけ規則の式で引く（無効・旧版・`failed` の分類は結合に入らず、国が決まらない写真は NULL）
    let located = locations(conn, &rules, &prefs)?;
    for (item, (id, _)) in items.iter_mut().zip(&prefs) {
        // 国が決まらない写真には付けない（任意の項目）
        if let Some(location) = located.get(id) {
            item["location_region"] = location.clone();
        }
    }
    Ok(json!({
        "ok": true,
        "page": page,
        "per": per,
        "pages": pages,
        "total": total,
        "items": items,
    }))
}

/// 国と地域の ID（名称表の ID。国が決まれば `Some`、地域が決まらなければ地域は `None`）。
type PlaceIds = (&'static str, Option<&'static str>);

/// `pref` が空で、有効な分類で国が決まる写真の `{写真の ID: (国, 地域)}`。`extra` は `photos`（別名 `p`）への追加の条件
/// （`params` をバインドする）。分類の式は [`regions::PlaceRules::outside_pref`]。名称表に無い ID（版が合っていれば起きない）は
/// 飛ばす。**`pref` のある写真は含めない**（規則 1。[`regions::pref_region`]）。
fn classified_places(
    conn: &Connection,
    rules: &regions::PlaceRules,
    extra: &str,
    params: &[Sql],
) -> rusqlite::Result<HashMap<i64, PlaceIds>> {
    let mut out = HashMap::new();
    let Ok(names) = data::names() else {
        return Ok(out);
    };
    let outside = &rules.outside_pref;
    let mut stmt = conn.prepare(&format!(
        "SELECT p.id, {country}, {region} FROM {from} WHERE NOT {has_pref} AND {extra} AND {country} IS NOT NULL",
        country = outside.country,
        region = outside.region,
        from = outside.from,
        has_pref = rules.has_pref,
    ))?;
    let mut rows = stmt.query(params_from_iter(params.iter()))?;
    while let Some(row) = rows.next()? {
        let country: String = row.get(1)?;
        let region: Option<String> = row.get(2)?;
        let Some(country) = names.country(&country) else {
            continue;
        };
        let region = region
            .as_deref()
            .and_then(|r| names.region(r))
            .map(|r| r.id.as_str());
        out.insert(row.get(0)?, (country.id.as_str(), region));
    }
    Ok(out)
}

/// `ids`（このページの写真）の `location_region`（国が決まる写真だけ。決まらない写真は含めない）。`prefs` は
/// `(写真の ID, pref)` で、`pref` のある写真は規則 1（日本と `pref` の対応）、それ以外は有効な分類。
fn locations(
    conn: &Connection,
    rules: &regions::PlaceRules,
    prefs: &[(i64, String)],
) -> rusqlite::Result<HashMap<i64, Value>> {
    let names = data::names().ok();
    let mut out = HashMap::new();
    let mut classified_ids = Vec::new();
    for (id, pref) in prefs {
        if pref.is_empty() {
            classified_ids.push(Sql::Integer(*id));
        } else if let Some(mut map) =
            region_fields(names, Some(data::JAPAN_ID), regions::pref_region(pref))
        {
            // `pref` のある写真は地理院の地名が正
            map.insert("method".to_owned(), json!("pref"));
            out.insert(*id, Value::Object(map));
        }
    }
    if classified_ids.is_empty() {
        return Ok(out);
    }
    let extra = format!("p.id IN ({})", vec!["?"; classified_ids.len()].join(","));
    for (id, (country, region)) in classified_places(conn, rules, &extra, &classified_ids)? {
        if let Some(mut map) = region_fields(names, Some(country), region) {
            // 境界データの分類
            map.insert("method".to_owned(), json!("inside"));
            out.insert(id, Value::Object(map));
        }
    }
    Ok(out)
}

/// 国・地域の表示用の項目 `{country_id, country, region_id, region, label}`（`label` は「国・地域」。地域が無ければ国だけ）。
/// 名称表に無い ID（同梱データが読めないときなど）は ID を名前にする。国が無ければ `None`。
fn region_fields(
    names: Option<&data::NameTable>,
    country: Option<&str>,
    region: Option<&str>,
) -> Option<serde_json::Map<String, Value>> {
    let country = country?;
    let name = |place: Option<&data::Place>, id: &str| {
        place.map_or_else(|| id.to_owned(), |p| p.name.clone())
    };
    let country_name = name(names.and_then(|n| n.country(country)), country);
    let region_name = region.map(|id| name(names.and_then(|n| n.region(id)), id));
    let label = match &region_name {
        Some(region) => format!("{country_name}・{region}"),
        None => country_name.clone(),
    };
    let mut map = serde_json::Map::new();
    map.insert("country_id".to_owned(), json!(country));
    map.insert("country".to_owned(), json!(country_name));
    map.insert("region_id".to_owned(), json!(region));
    map.insert("region".to_owned(), json!(region_name));
    map.insert("label".to_owned(), json!(label));
    Some(map)
}

// ---------------------------------------------------------------- points

/// `GET /api/admin/library/points`。
pub async fn points(state: &AppState, query: Option<&str>) -> Response {
    let params = query_params(query);
    let Some((from, to)) = parse_range(&params) else {
        return bad_params();
    };
    let mut filter = Filter::default();
    filter.push("lat IS NOT NULL AND lon IS NOT NULL", []);
    filter.range(from, to);
    filter.hidden(include_hidden(&params));
    respond(
        state
            .catalog
            .read(move |conn| build_points(conn, &filter, POINTS_MAX))
            .await,
    )
}

/// 新しい順に並んだ `rows`（`max + 1` 件まで読んだもの）を `max` 件に切る。切ったら `true`。
fn truncate_points<T>(mut rows: Vec<T>, max: usize) -> (Vec<T>, bool) {
    let truncated = rows.len() > max;
    rows.truncate(max);
    (rows, truncated)
}

fn build_points(conn: &Connection, filter: &Filter, max: usize) -> rusqlite::Result<Value> {
    let _snapshot = conn.unchecked_transaction()?;
    let clause = filter.where_clause();
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM photos WHERE {clause}"),
        params_from_iter(filter.params.iter()),
        |row| row.get(0),
    )?;
    // 新しい順（日時なしは最後）。1 件多く読んで、超えたかを知る
    let mut stmt = conn.prepare(&format!(
        "SELECT id, lat, lon FROM photos WHERE {clause}
          ORDER BY {ORDER_NEWEST} LIMIT ?"
    ))?;
    let mut bound = filter.params.clone();
    bound.push(Sql::Integer(
        i64::try_from(max).unwrap_or(i64::MAX).saturating_add(1),
    ));
    let rows = stmt
        .query_map(params_from_iter(bound.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (rows, truncated) = truncate_points(rows, max);
    let features: Vec<Value> = rows
        .into_iter()
        .map(|(id, lat, lon)| {
            json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [lon, lat]},
                "properties": {"id": id},
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "truncated": truncated,
        "total": total,
        "type": "FeatureCollection",
        "features": features,
    }))
}

// ---------------------------------------------------------------- calendar・places

/// `WHERE` に足す除外の条件（`hidden=1` でなければ除外を除く）。
fn visible_clause(with_hidden: bool) -> String {
    if with_hidden {
        String::new()
    } else {
        format!(" AND {VISIBLE}")
    }
}

/// `GET /api/admin/library/calendar`。
pub async fn calendar(state: &AppState, query: Option<&str>) -> Response {
    let hidden = include_hidden(&query_params(query));
    let ctx = Arc::clone(&state.admin);
    respond(
        state
            .catalog
            .read(move |conn| build_calendar(conn, &ctx, hidden))
            .await,
    )
}

/// 年・月ごとの件数。日時ありの形（[`TIMED`]）の行を `substr(taken_at,1,4)`・`substr(taken_at,1,7)` で数える
/// （暦として読めない値も形が合えばそのまま出る）。年は新しい順、月は古い順。年には代表の写真 `cover` が付く（月には付けない）。
fn build_calendar(
    conn: &Connection,
    ctx: &AdminContext,
    with_hidden: bool,
) -> rusqlite::Result<Value> {
    // 年・月・日時なしの件数が同じ時点の DB から出るように、1 つの読み取りにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let visible = visible_clause(with_hidden);
    let count_by = |len: usize| -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT substr(taken_at,1,{len}) AS k, COUNT(*) FROM photos
              WHERE {TIMED}{visible} GROUP BY k ORDER BY k"
        ))?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    };
    let years = count_by(4)?;
    let months = count_by(7)?;
    let mut year_covers = covers(
        conn,
        ctx,
        "substr(taken_at,1,4)",
        &format!("{TIMED}{visible}"),
        &[],
    )?;
    let mut by_year: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for (key, count) in &months {
        // `key` は `YYYY-MM`（TIMED の形なので 7 文字の ASCII）
        by_year
            .entry(&key[..4])
            .or_default()
            .push(json!({"month": &key[5..], "count": count}));
    }
    let years: Vec<Value> = years
        .iter()
        .rev()
        .map(|(year, count)| {
            json!({
                "year": year,
                "count": count,
                "cover": year_covers.remove(year.as_str()),
                "months": by_year.remove(year.as_str()).unwrap_or_default(),
            })
        })
        .collect();
    let untimed: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM photos WHERE {UNTIMED}{visible}"),
        [],
        |row| row.get(0),
    )?;
    Ok(json!({"ok": true, "years": years, "untimed": untimed}))
}

/// `places` の絞り込み。`from`・`to`・`bbox` の形が違えば `None`（400）。`bbox` があるときは位置のある写真だけが残る
/// （件数・表紙・範囲と `abroad` のすべてに掛かる）。
fn places_filter(params: &Params) -> Option<Filter> {
    let mut filter = Filter::default();
    let (from, to) = parse_range(params)?;
    filter.range(from, to);
    if let Some(bbox) = params.get("bbox") {
        filter.bbox(parse_bbox(bbox)?);
    }
    filter.hidden(include_hidden(params));
    Some(filter)
}

/// `GET /api/admin/library/places`。
pub async fn places(state: &AppState, query: Option<&str>) -> Response {
    let Some(filter) = places_filter(&query_params(query)) else {
        return bad_params();
    };
    let ctx = Arc::clone(&state.admin);
    respond(
        state
            .catalog
            .read(move |conn| build_places(conn, &ctx, &filter))
            .await,
    )
}

/// 写真の範囲 `[min_lon, min_lat, max_lon, max_lat]`。
type Bounds = [f64; 4];

/// `(pref, city)` ごとの集計。
struct PlaceRow {
    pref: String,
    city: String,
    count: i64,
    /// 位置のある写真だけの範囲。位置のある写真が無ければ `None`。
    bounds: Option<Bounds>,
}

/// 範囲を 1 つにまとめる（`None` は無視する）。
fn union_bounds(a: Option<Bounds>, b: Option<Bounds>) -> Option<Bounds> {
    match (a, b) {
        (Some(a), Some(b)) => Some([
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]),
        (a, b) => a.or(b),
    }
}

/// 国・地域・区分ごとの件数と範囲と代表の写真。
#[derive(Clone)]
struct Tally {
    count: i64,
    bounds: Option<Bounds>,
    /// 日本の地域だけ: 境界データで入った（`pref` 空の）写真の数。
    boundary: i64,
    /// 代表の写真の鍵（[`COVER_KEY`] の最大。大きいほど代表にふさわしい）。
    best: Option<String>,
    /// 位置のある写真の経度の全部（日付変更線をまたぐかの確認用）。通常の範囲が [`FIT_SPAN_FREE`] を超える組だけ
    /// [`fill_longitudes`] が入れる。それ以外と、`pref` のある写真を含む組（件数が多い）は `None`。
    lons: Option<Vec<f64>>,
}

impl Default for Tally {
    fn default() -> Self {
        Self {
            count: 0,
            bounds: None,
            boundary: 0,
            best: None,
            lons: Some(Vec::new()),
        }
    }
}

impl Tally {
    fn add(&mut self, other: &Tally, boundary: bool) {
        self.count += other.count;
        self.bounds = union_bounds(self.bounds, other.bounds);
        if boundary {
            self.boundary += other.count;
        }
        if self.best < other.best {
            self.best.clone_from(&other.best);
        }
        self.lons = match (self.lons.take(), &other.lons) {
            (Some(mut a), Some(b)) => {
                a.extend_from_slice(b);
                Some(a)
            }
            _ => None,
        };
    }

    /// 代表の写真の ID（鍵の末尾の 20 桁）。
    fn cover_id(&self) -> Option<i64> {
        let key = self.best.as_deref()?;
        key.get(key.len().checked_sub(COVER_KEY_ID_DIGITS)?..)?
            .parse()
            .ok()
    }
}

/// 集計の 1 行（国・地域・`pref`/`city`・場所不明の別で分けた組）。
struct PlaceGroup {
    /// 国の ID。決まらない写真（`unclassified`・`no_location`）は `None`。
    country: Option<String>,
    /// 地域の ID。地域が決まらない写真は `None`。
    region: Option<String>,
    /// `pref` のある写真だけ: `(pref, city)`。
    pref: Option<(String, String)>,
    /// `no_location`（`pref` が空で座標が無効）。
    no_location: bool,
    tally: Tally,
    /// `pref` のある写真だけ: 旧 `prefs` の bbox（数値の座標の写真の MIN/MAX。変更前と同じ値で、互換のため範囲外の値も
    /// 含む。文字列の座標は、変更前は読み取りの失敗になっていたので除く）。新しい応答の範囲 `tally.bounds` は有効な座標の写真だけ。
    legacy_bounds: Option<Bounds>,
}

/// 組の代表の写真を、集計の中で `MAX` だけで選ぶための鍵。[`COVER_ORDER`]（除外でない → 位置がある → 日時がある → 新しい →
/// ID が大きい）と同じ順位が、文字列として大きいほど上位になるよう、先頭から 3 つの 0/1、日時、ID（20 桁の 0 埋め）を並べる。
/// 日時のない写真は、日時の文字列が任意の長さなので 16 進にして `-` で区切る（短い方が小さくなる）。日時のある写真は
/// 固定長の形（[`TIMED`]）。日時のある写真と日時のない写真は先頭の 0/1 で分かれるので、日時の部分を比べ合うことはない。
/// 窓関数の並べ替えをせず、件数の集計と同じ 1 回の走査で代表が得られる。
const COVER_KEY: &str = concat!(
    "CASE WHEN ",
    timed_sql!(),
    " THEN printf('%d%d1%s-%020d', COALESCE(hidden,0)=0, lat IS NOT NULL AND lon IS NOT NULL, taken_at, p.id) ",
    "ELSE printf('%d%d0%s-%020d', COALESCE(hidden,0)=0, lat IS NOT NULL AND lon IS NOT NULL, ",
    "hex(COALESCE(taken_at,'')), p.id) END"
);
/// [`COVER_KEY`] の末尾の ID の桁数。
const COVER_KEY_ID_DIGITS: usize = 20;

/// 代表の写真の `{id, thumb_url}`。`ids` の写真の表示ファイルは 1 回の読み取りでまとめて引く。
fn cover_values(
    conn: &Connection,
    ctx: &AdminContext,
    ids: &[i64],
) -> rusqlite::Result<HashMap<i64, Value>> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(500) {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, display, drive_id, md5, mtime FROM photos WHERE id IN ({})",
            vec!["?"; chunk.len()].join(",")
        ))?;
        let mut rows = stmt.query(params_from_iter(chunk.iter()))?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let thumb_url =
                cover_thumb_url(ctx, id, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?);
            out.insert(id, json!({"id": id, "thumb_url": thumb_url}));
        }
    }
    Ok(out)
}

/// 通常の範囲の経度の幅がこれ以内なら、空きが戻る間（最大 → 最小）が必ず最大の空きなので、日付変更線をまたがない。
const FIT_SPAN_FREE: f64 = 180.0;

/// 経度の列から、写真の経度を円周上で覆う最小の区間が日付変更線（±180）をまたぐか。
/// 隣り合う経度の間（円周上で最後の 1 つは最大から最小へ戻る間）の最大の空きが、最小の区間の外側になる。
/// 空きが戻る間（最大 → 最小）にあるか、同じ大きさなら、区間は通常の `[最小, 最大]`。それ以外はまたぐ。
fn spans_dateline(lons: &[f64]) -> bool {
    let mut lons = lons.to_vec();
    lons.sort_by(f64::total_cmp);
    let (Some(first), Some(last)) = (lons.first().copied(), lons.last().copied()) else {
        return false;
    };
    let wrap = 360.0 - (last - first);
    let inner = lons.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    inner > wrap
}

/// 範囲が地図に出せる値か: 4 つとも有限で、緯度 ±90・経度 ±180 以内、最小 ≤ 最大。
fn valid_bounds(b: &Bounds) -> bool {
    let [w, s, e, n] = *b;
    [w, s, e, n].iter().all(|v| v.is_finite())
        && (-180.0..=180.0).contains(&w)
        && (-180.0..=180.0).contains(&e)
        && (-90.0..=90.0).contains(&s)
        && (-90.0..=90.0).contains(&n)
        && w <= e
        && s <= n
}

/// 件数・範囲・代表の写真 `{count, cover, bbox, fit_bbox}`（国・地域・地域不明・場所不明で同じ形）。
/// `fit_bbox` は地図の自動 fit 用で、写真の経度を円周上で覆う最小の区間が日付変更線をまたぐなら `null`。
/// 通常の範囲の経度の幅が [`FIT_SPAN_FREE`] 以内ならその範囲そのまま。それより広いときは、経度が全部そろっていれば
/// （[`fill_longitudes`]）最小の区間で確かめ、`pref` のある写真を含む（経度をそろえていない）ときは確かめずに `null`（自動 fit を省く側に倒す。
/// 日本の `pref` の写真が経度で半周以上に広がるのは、座標の誤りがあるときだけ）。
/// 範囲は有効な座標の写真だけから作る（`pref` のある写真も同じ。無効な座標は範囲に入れない）が、念のため `fit_bbox` は
/// [`valid_bounds`] を満たさなければ `null`（不正な bbox を地図に渡さない）。
fn tally_json(tally: &Tally, cover: Option<Value>) -> serde_json::Map<String, Value> {
    let fit = tally.bounds.filter(|b| {
        valid_bounds(b)
            && (b[2] - b[0] <= FIT_SPAN_FREE
                || tally.lons.as_deref().is_some_and(|l| !spans_dateline(l)))
    });
    let mut out = serde_json::Map::new();
    out.insert("count".to_owned(), json!(tally.count));
    out.insert("cover".to_owned(), cover.unwrap_or(Value::Null));
    out.insert("bbox".to_owned(), bounds_json(tally.bounds));
    out.insert("fit_bbox".to_owned(), bounds_json(fit));
    out
}

/// `group_concat` した経度の文字列（無ければ空）を数値の列にする。
fn parse_longitudes(list: Option<String>) -> Vec<f64> {
    list.iter()
        .flat_map(|s| s.split(',').filter_map(|v| v.parse::<f64>().ok()))
        .collect()
}

/// 日付変更線をまたぐかの確認に要る経度を、通常の範囲が [`FIT_SPAN_FREE`] を超える国の、`pref` の無い組だけ取り出して `lons` に
/// 入れる。通常の範囲が狭い組は経度を見ずに済む（国の範囲の中の地域も狭い）ので、ふつうは何も引かない（追加の文を実行しない）。
/// 国が決まらない写真の経度は、集計の文が一緒に取る。条件は集計と同じ期間・範囲・除外。
fn fill_longitudes(
    conn: &Connection,
    rules: &regions::PlaceRules,
    filter: &Filter,
    groups: &mut [PlaceGroup],
) -> rusqlite::Result<()> {
    let mut bounds: BTreeMap<&str, Option<Bounds>> = BTreeMap::new();
    for g in groups.iter().filter(|g| !g.no_location) {
        if let Some(country) = g.country.as_deref() {
            let entry = bounds.entry(country).or_default();
            *entry = union_bounds(*entry, g.tally.bounds);
        }
    }
    let countries: Vec<String> = bounds
        .iter()
        .filter(|(_, b)| b.is_some_and(|b| b[2] - b[0] > FIT_SPAN_FREE))
        .map(|(c, _)| (*c).to_owned())
        .collect();
    if countries.is_empty() {
        return Ok(());
    }
    let outside = &rules.outside_pref;
    let mut params = filter.params.clone();
    params.extend(countries.iter().cloned().map(Sql::Text));
    let mut stmt = conn.prepare(&format!(
        "SELECT {country}, {region}, group_concat(lon, ',')
           FROM {from} WHERE NOT {has_pref} AND ({clause}) AND {valid} AND {country} IN ({marks}) GROUP BY 1, 2",
        country = outside.country,
        region = outside.region,
        from = outside.from,
        has_pref = rules.has_pref,
        clause = filter.where_clause(),
        valid = regions::valid_coordinates("p"),
        marks = vec!["?"; countries.len()].join(","),
    ))?;
    let mut lons: HashMap<(Option<String>, Option<String>), Vec<f64>> = stmt
        .query_map(params_from_iter(params.iter()), |row| {
            Ok(((row.get(0)?, row.get(1)?), parse_longitudes(row.get(2)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for g in groups
        .iter_mut()
        .filter(|g| g.pref.is_none() && !g.no_location)
    {
        if g.country.as_ref().is_some_and(|c| countries.contains(c)) {
            let key = (g.country.clone(), g.region.clone());
            g.tally.lons = Some(lons.remove(&key).unwrap_or_default());
        }
    }
    Ok(())
}

/// 名称表の 1 件（表示名と検索別名）。表に無い ID は ID を表示名にする（版が合っていれば起きない）。
fn names_json(map: &mut serde_json::Map<String, Value>, id: &str, place: Option<&data::Place>) {
    map.insert("id".to_owned(), json!(id));
    map.insert(
        "name".to_owned(),
        json!(place.map_or(id, |p| p.name.as_str())),
    );
    map.insert(
        "aliases".to_owned(),
        json!(place.map_or_else(Vec::new, |p| p.search.clone())),
    );
}

/// 件数の多い順、同数なら表示名、それも同じなら ID の順。
fn sort_places(items: &mut [(String, i64, serde_json::Map<String, Value>)]) {
    items.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.2["name"].as_str().cmp(&b.2["name"].as_str()))
            .then_with(|| a.0.cmp(&b.0))
    });
}

/// 範囲の 4 列（西・南・東・北の順）を `first` 列目から読む。1 つでも NULL なら `None`。
fn bounds_at(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Option<Bounds>> {
    let corners = [
        row.get::<_, Option<f64>>(first)?,
        row.get::<_, Option<f64>>(first + 1)?,
        row.get::<_, Option<f64>>(first + 2)?,
        row.get::<_, Option<f64>>(first + 3)?,
    ];
    Ok(match corners {
        [Some(w), Some(s), Some(e), Some(n)] => Some([w, s, e, n]),
        _ => None,
    })
}

/// 集計の列（[`COVER_KEY`] を含む `COUNT(*)`・範囲 4 つ・代表の鍵）を `first` 列目から読む。
fn tally_at(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Tally> {
    Ok(Tally {
        count: row.get(first)?,
        bounds: bounds_at(row, first + 1)?,
        boundary: 0,
        best: row.get(first + 5)?,
        lons: None,
    })
}

/// 場所の眺め方。国ごと・地域ごとの件数・範囲・代表の写真（`countries`）、国が決まらない写真（`unknown`）、旧形式の都道府県・
/// 市区町村（`prefs`）と海外・不明（`abroad`）。区分は [`regions::PlaceRules`]（設計「場所の区分の規則」）で 1 つに決まる。
///
/// `filter`（期間・範囲・除外）は、すべての項目の件数・代表の写真・範囲に先に掛かる。範囲があれば、その中に写真の無い
/// 国・地域は出ず、座標のない写真（`no_location`）も入らない。並びは件数の多い順、同数なら表示名の順。
/// 旧形式の `prefs`（`pref` のある写真を `pref`/`city` で数えたもの）と `abroad`（`pref` が空の写真の数）は、変更前と
/// 同じ意味のまま、新しい集計と同じ走査から取る。
///
/// 分類の表が無い DB（`classification_available` が偽）では、`pref` のある写真だけが日本として出て、残りは国が決まらない
/// 写真になる。表は作らず、書かない。
fn build_places(conn: &Connection, ctx: &AdminContext, filter: &Filter) -> rusqlite::Result<Value> {
    // 件数・代表の写真・分類待ちが同じ時点の DB から出るように、1 つの読み取りにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let classification_available = regions::available(conn)?;
    let rules = regions::PlaceRules::new(classification_available);
    let clause = filter.where_clause();
    // 範囲は位置のある写真だけの MIN/MAX（集計関数は NULL を飛ばすので、位置の無い行は CASE で NULL にする）。
    // 組ごとの代表の写真の鍵も同じ走査で `MAX` を取る。集計は 2 つに分ける:
    // 規則 1（`pref` のある写真）は `pref`/`city` で数えるだけ（国は日本、地域は名称表の都道府県の対応で付ける。SQL の
    // `rules.region` が使う対応表と同じ名称表）、規則 2〜4 は分類の式で数える。1 回の走査で 5 つの鍵を並べ替えるより軽い
    // 範囲に入れる写真（座標が有効な写真だけ。無効な座標の値は範囲に入れない）。旧 `prefs` の bbox だけは変更前と同じ
    // （数値の座標の写真の範囲。範囲外の値も含む）を、`pref` の組の追加の列で取る
    let aggregates = |located: &str| {
        format!(
            "COUNT(*),
        MIN(CASE WHEN {located} THEN lon END), MIN(CASE WHEN {located} THEN lat END),
        MAX(CASE WHEN {located} THEN lon END), MAX(CASE WHEN {located} THEN lat END),
        MAX({COVER_KEY})"
        )
    };
    let names = data::names().ok();
    let mut groups: Vec<PlaceGroup> = Vec::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT CAST(p.pref AS TEXT), CAST(COALESCE(p.city,'') AS TEXT), 0, {aggregates},
        MIN(CASE WHEN {legacy} THEN lon END), MIN(CASE WHEN {legacy} THEN lat END),
        MAX(CASE WHEN {legacy} THEN lon END), MAX(CASE WHEN {legacy} THEN lat END)
           FROM photos p WHERE {has_pref} AND {clause} GROUP BY 1, 2",
        aggregates = aggregates(&regions::valid_coordinates("p")),
        legacy = "typeof(lat) IN ('real','integer') AND typeof(lon) IN ('real','integer')",
        has_pref = rules.has_pref,
    ))?;
    let pref_groups = stmt
        .query_map(params_from_iter(filter.params.iter()), |row| {
            let pref: String = row.get(0)?;
            let region = names
                .and_then(|n| n.region_of_pref(&pref))
                .map(|r| r.id.clone());
            Ok(PlaceGroup {
                country: Some(data::JAPAN_ID.to_owned()),
                region,
                pref: Some((pref, row.get(1)?)),
                no_location: false,
                tally: tally_at(row, 3)?,
                legacy_bounds: bounds_at(row, 9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    groups.extend(pref_groups);
    // `pref` の無い写真だけの文なので、都道府県の対応表を結合しない式（`outside_pref`）を使う
    let outside = &rules.outside_pref;
    let valid = regions::valid_coordinates("p");
    let mut stmt = conn.prepare(&format!(
        "SELECT {country}, {region}, {no_location}, {aggregates},
                group_concat(CASE WHEN {country} IS NULL AND {valid} THEN lon END, ',')
           FROM {from} WHERE NOT {has_pref} AND {clause} GROUP BY 1, 2, 3",
        aggregates = aggregates(&valid),
        valid = valid,
        country = outside.country,
        region = outside.region,
        no_location = outside.no_location,
        has_pref = rules.has_pref,
        from = outside.from,
    ))?;
    let other_groups = stmt
        .query_map(params_from_iter(filter.params.iter()), |row| {
            let mut tally = tally_at(row, 3)?;
            // 国が決まらない写真（座標は有効）だけ、経度を全部持つ（件数は少なく、組の範囲が広いことが多い）
            if row.get::<_, Option<String>>(0)?.is_none() && !row.get::<_, bool>(2)? {
                tally.lons = Some(parse_longitudes(row.get(9)?));
            }
            Ok(PlaceGroup {
                country: row.get(0)?,
                region: row.get(1)?,
                pref: None,
                no_location: row.get(2)?,
                tally,
                legacy_bounds: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    groups.extend(other_groups);
    fill_longitudes(conn, &rules, filter, &mut groups)?;

    // 旧形式: `pref` のある写真の `pref`/`city`、`pref` が空の写真の数。`pref` ごとの代表の写真の鍵
    let mut abroad = 0;
    let mut pref_rows: Vec<PlaceRow> = Vec::new();
    let mut pref_best: HashMap<&str, &Option<String>> = HashMap::new();
    for g in &groups {
        match &g.pref {
            Some((pref, city)) => {
                pref_rows.push(PlaceRow {
                    pref: pref.clone(),
                    city: city.clone(),
                    count: g.tally.count,
                    bounds: g.legacy_bounds,
                });
                let best = pref_best.entry(pref).or_insert(&g.tally.best);
                if **best < g.tally.best {
                    *best = &g.tally.best;
                }
            }
            None => abroad += g.tally.count,
        }
    }

    // 国・地域・場所不明ごとの件数と範囲と代表の写真の鍵
    let mut by_region: BTreeMap<(String, Option<String>), Tally> = BTreeMap::new();
    let mut unknown = [Tally::default(), Tally::default()];
    for g in &groups {
        match &g.country {
            Some(country) => by_region
                .entry((country.clone(), g.region.clone()))
                .or_default()
                .add(&g.tally, g.pref.is_none()),
            None => unknown[usize::from(g.no_location)].add(&g.tally, false),
        }
    }
    let mut countries: BTreeMap<&str, Vec<(&Option<String>, &Tally)>> = BTreeMap::new();
    for ((country, region), tally) in &by_region {
        countries.entry(country).or_default().push((region, tally));
    }
    // 国ごとの合計（経度の配列を持つ組があるので、1 回だけ作って使い回す）
    let totals: BTreeMap<&str, Tally> = countries
        .iter()
        .map(|(country, parts)| {
            let total = parts.iter().fold(Tally::default(), |mut acc, (_, t)| {
                acc.add(t, false);
                acc
            });
            (*country, total)
        })
        .collect();

    // 代表の写真の表示ファイルを、必要な写真だけまとめて引く
    let mut wanted: Vec<i64> = pref_best
        .values()
        .filter_map(|b| {
            Tally {
                best: (*b).clone(),
                ..Tally::default()
            }
            .cover_id()
        })
        .collect();
    wanted.extend(by_region.values().filter_map(Tally::cover_id));
    wanted.extend(totals.values().filter_map(Tally::cover_id));
    wanted.extend(unknown.iter().filter_map(Tally::cover_id));
    wanted.sort_unstable();
    wanted.dedup();
    let covers = cover_values(conn, ctx, &wanted)?;
    let cover_of = |t: &Tally| t.cover_id().and_then(|id| covers.get(&id)).cloned();

    let mut prefs = group_places(pref_rows);
    for pref in &mut prefs {
        let name = pref["pref"].as_str().unwrap_or_default().to_owned();
        let best = pref_best.get(name.as_str()).and_then(|b| (*b).clone());
        pref["cover"] = cover_of(&Tally {
            best,
            ..Tally::default()
        })
        .unwrap_or(Value::Null);
    }

    let mut country_items: Vec<(String, i64, serde_json::Map<String, Value>)> = Vec::new();
    for (country, parts) in &countries {
        let total = &totals[country];
        let mut map = serde_json::Map::new();
        names_json(&mut map, country, names.and_then(|n| n.country(country)));
        map.extend(tally_json(total, cover_of(total)));
        let mut region_items: Vec<(String, i64, serde_json::Map<String, Value>)> = Vec::new();
        let mut region_unknown = Tally::default();
        for (region, tally) in parts {
            match region {
                None => region_unknown = (*tally).clone(),
                Some(id) => {
                    let place = names.and_then(|n| n.region(id));
                    let mut rmap = serde_json::Map::new();
                    names_json(&mut rmap, id, place);
                    rmap.extend(tally_json(tally, cover_of(tally)));
                    // 日本の地域だけ: `pref`（都道府県名。画面は旧 `prefs` のこの名前の行から市区町村を引く。ここには
                    // 市区町村を複製しない）と、境界データで入った写真の数
                    if let Some(pref) = place.and_then(|p| p.pref.as_deref()) {
                        rmap.insert("pref".to_owned(), json!(pref));
                        rmap.insert("boundary_only".to_owned(), json!(tally.boundary));
                    }
                    region_items.push((id.clone(), tally.count, rmap));
                }
            }
        }
        sort_places(&mut region_items);
        map.insert(
            "regions".to_owned(),
            Value::Array(
                region_items
                    .into_iter()
                    .map(|(_, _, m)| Value::Object(m))
                    .collect(),
            ),
        );
        map.insert(
            "region_unknown".to_owned(),
            Value::Object(tally_json(&region_unknown, cover_of(&region_unknown))),
        );
        country_items.push(((*country).to_owned(), total.count, map));
    }
    sort_places(&mut country_items);

    let unclassified = tally_json(&unknown[0], cover_of(&unknown[0]));
    // 座標が無い（無効な）写真に範囲は無い
    let no_location = tally_json(
        &Tally {
            bounds: None,
            ..unknown[1].clone()
        },
        cover_of(&unknown[1]),
    );
    let pending = regions::pending_photos(conn)?;
    Ok(json!({
        "ok": true,
        "prefs": prefs,
        "abroad": abroad,
        "classification_available": classification_available,
        "classification_pending": pending,
        "countries": country_items.into_iter().map(|(_, _, m)| Value::Object(m)).collect::<Vec<_>>(),
        "unknown": {"unclassified": unclassified, "no_location": no_location},
    }))
}

/// 都道府県の件数・範囲と市区町村の `(名前, 件数, 範囲)`。
type PrefGroup = (
    String,
    i64,
    Option<Bounds>,
    Vec<(String, i64, Option<Bounds>)>,
);

/// 範囲の JSON（無ければ `null`）。
fn bounds_json(bounds: Option<Bounds>) -> Value {
    bounds.map_or(Value::Null, |b| json!(b))
}

/// 行を都道府県ごとにまとめ、件数の多い順（同数は名前の順）に並べる。代表の写真 `cover` は呼び出し側が足す。
fn group_places(rows: Vec<PlaceRow>) -> Vec<Value> {
    let mut prefs: BTreeMap<String, Vec<(String, i64, Option<Bounds>)>> = BTreeMap::new();
    for row in rows {
        prefs
            .entry(row.pref)
            .or_default()
            .push((row.city, row.count, row.bounds));
    }
    let mut prefs: Vec<PrefGroup> = prefs
        .into_iter()
        .map(|(pref, mut cities)| {
            cities.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let total = cities.iter().map(|c| c.1).sum();
            let bounds = cities.iter().fold(None, |acc, c| union_bounds(acc, c.2));
            (pref, total, bounds, cities)
        })
        .collect();
    prefs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    prefs
        .into_iter()
        .map(|(pref, count, bounds, cities)| {
            let cities: Vec<Value> = cities
                .into_iter()
                .map(|(city, count, bounds)| {
                    json!({"city": city, "count": count, "bbox": bounds_json(bounds)})
                })
                .collect();
            json!({
                "pref": pref,
                "count": count,
                "cover": Value::Null,
                "bbox": bounds_json(bounds),
                "cities": cities,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(query: &str) -> Params {
        pyquery::parse_qs_first(query)
    }

    #[test]
    fn truncate_points_keeps_the_first_max_and_reports_the_cut() {
        let (rows, cut) = truncate_points(vec![5, 4, 3], 3);
        assert_eq!((rows, cut), (vec![5, 4, 3], false));
        let (rows, cut) = truncate_points(vec![5, 4, 3, 2], 3);
        assert_eq!((rows, cut), (vec![5, 4, 3], true));
        let (rows, cut) = truncate_points(Vec::<i32>::new(), 3);
        assert_eq!((rows, cut), (vec![], false));
        let (rows, cut) = truncate_points(vec![1, 2], 0);
        assert_eq!((rows, cut), (vec![], true));
    }

    #[test]
    fn minutes_must_be_sixteen_characters_in_the_timed_shape() {
        assert!(parse_minute("2025-06-01 10:00").is_some());
        // 形だけを見る（暦の正しさは見ない。`taken_at` 自体も暦を検査しない）
        for shaped in [
            "2025-02-30 10:00",
            "2025-06-01 24:00",
            "2025-06-01 10:60",
            "2025-13-45 99:99",
        ] {
            assert!(parse_minute(shaped).is_some(), "{shaped:?}");
        }
        for bad in [
            "",
            "2025-06-01",
            "2025-6-1 10:00",
            "2025-06-01T10:00",
            "2025-06-01 10:00:00",
            "２０２５-06-01 10:00",
            "2025-06-01  10:0",
        ] {
            assert!(parse_minute(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn range_needs_ordered_ends() {
        assert!(parse_range(&params("from=2025-06-01+10:00&to=2025-06-01+10:00")).is_some());
        assert!(parse_range(&params("from=2025-06-01+10:01&to=2025-06-01+10:00")).is_none());
        assert!(parse_range(&params("from=x")).is_none());
        // 形だけ: 暦が無い日時は通り、形の違うものは通らない
        assert!(parse_range(&params("from=2025-02-30+10:00&to=2025-02-30+10:00")).is_some());
        assert!(parse_range(&params("from=2025-6-1+10:00")).is_none());
        assert!(parse_range(&params("to=2025-06-01T10:00")).is_none());
        assert_eq!(parse_range(&params("")), Some((None, None)));
    }

    #[test]
    fn bbox_is_four_finite_ordered_numbers_in_range() {
        assert_eq!(
            parse_bbox("135,34,140,36"),
            Some([135.0, 34.0, 140.0, 36.0])
        );
        assert_eq!(
            parse_bbox("-180,-90,180,90"),
            Some([-180.0, -90.0, 180.0, 90.0])
        );
        for bad in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "1,2,3,",
            "a,2,3,4",
            "nan,2,3,4",
            "1,2,inf,4",
            "140,34,135,36",
            "135,36,140,34",
            "-181,0,0,1",
            "0,0,181,1",
            "0,-91,1,0",
            "0,0,1,91",
        ] {
            assert!(parse_bbox(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn ids_are_integers_capped_before_deduplication() {
        assert_eq!(parse_ids("3,1,3,2"), Some(vec![3, 1, 2]));
        let many = |n: usize| vec!["7"; n].join(",");
        assert_eq!(parse_ids(&many(IDS_MAX)), Some(vec![7]));
        assert!(parse_ids(&many(IDS_MAX + 1)).is_none());
        for bad in ["", "1,", ",1", "1,,2", "a", "1.5", "1;2", " 1"] {
            assert!(parse_ids(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn names_are_capped_by_characters() {
        assert!(parse_name(&"あ".repeat(NAME_MAX_CHARS)).is_some());
        assert!(parse_name(&"あ".repeat(NAME_MAX_CHARS + 1)).is_none());
    }

    fn place(pref: &str, city: &str, count: i64, bounds: Option<Bounds>) -> PlaceRow {
        PlaceRow {
            pref: pref.to_owned(),
            city: city.to_owned(),
            count,
            bounds,
        }
    }

    #[test]
    fn places_are_grouped_by_pref_and_sorted_by_count_then_name() {
        let rows = vec![
            place("B県", "い市", 2, Some([1.0, 1.0, 2.0, 2.0])),
            place("A県", "", 1, None),
            place("A県", "う市", 1, Some([10.0, 20.0, 11.0, 21.0])),
            place("A県", "あ市", 1, Some([9.0, 22.0, 10.5, 23.0])),
            place("C県", "え市", 4, None),
        ];
        let out = group_places(rows);
        let prefs: Vec<&str> = out.iter().map(|p| p["pref"].as_str().unwrap()).collect();
        // C 4、A と B は 3 と 2
        assert_eq!(prefs, ["C県", "A県", "B県"]);
        let cities: Vec<&str> = out[1]["cities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["city"].as_str().unwrap())
            .collect();
        assert_eq!(cities, ["", "あ市", "う市"]);
        // 都道府県の範囲は市区町村の範囲の合併（位置の無い市区町村は無視）。位置のある写真が無ければ null
        assert_eq!(out[1]["bbox"], json!([9.0, 20.0, 11.0, 23.0]));
        assert_eq!(out[1]["cities"][0]["bbox"], Value::Null);
        assert_eq!(out[0]["bbox"], Value::Null);
        assert_eq!(out[2]["bbox"], json!([1.0, 1.0, 2.0, 2.0]));
    }

    #[test]
    fn dateline_is_crossed_only_when_the_largest_gap_is_inside() {
        // 178 と -179 は円周上で 3 度の差 = またぐ
        assert!(spans_dateline(&[178.0, -179.0]));
        assert!(spans_dateline(&[-179.0, -170.0, 170.0, 179.0]));
        // 空きが 80 度ずつ、戻る間が 120 度 = またがない
        assert!(!spans_dateline(&[-100.0, -20.0, 60.0, 140.0]));
        // 戻る間と同じ大きさの空きは、通常の区間（またがない）
        assert!(!spans_dateline(&[-90.0, 0.0, 90.0]));
        assert!(!spans_dateline(&[]));
        assert!(!spans_dateline(&[10.0]));
        assert!(!spans_dateline(&[135.0, 140.0, 139.7]));
    }

    /// `where_` を満たす写真の ID を、`order` の並びで返す（`photos` の別名は `p`）。
    fn ids_ordered_by(conn: &Connection, where_: &str, order: &str) -> Vec<i64> {
        conn.prepare(&format!(
            "SELECT p.id FROM photos p WHERE {where_} ORDER BY {order}"
        ))
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn the_cover_key_ranks_photos_the_way_cover_order_does() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE photos(id INTEGER PRIMARY KEY, taken_at TEXT, lat REAL, lon REAL, hidden INTEGER);",
        )
        .unwrap();
        // 除外（NULL・0・1）・位置（無し・あり・片方だけ）・日時（無し・空・形の違う値・他の接頭辞になる値・分までの形・
        // 形の後ろに続く値・同じ日時）を組み合わせた行。同じ組み合わせを 2 枚ずつ（同じ日時・日時なし同士・位置なし同士の ID 順）
        let takens = [
            "NULL",
            "''",
            "'x'",
            "'xy'",
            "'2025-06'",
            "'2025-06-1'",
            "'2025-06-01 10:00'",
            "'2025-06-01 10:00:00'",
            "'2025-06-01 10:01'",
            "'2025-07-01 09:00'",
            "'あ'",
        ];
        let mut id = 0;
        let mut rows = String::new();
        for hidden in ["NULL", "0", "1"] {
            for pos in ["NULL, NULL", "35.0, 135.0", "35.0, NULL", "NULL, 135.0"] {
                for taken in takens {
                    for _ in 0..2 {
                        id += 1;
                        rows += &format!("INSERT INTO photos VALUES({id},{taken},{pos},{hidden});");
                    }
                }
            }
        }
        conn.execute_batch(&rows).unwrap();
        // 全 ID を `COVER_ORDER` の並びと `COVER_KEY DESC` の並びで並べて、同じ並びになる
        let key_desc = format!("{COVER_KEY} DESC");
        for (label, where_) in [
            ("全部", "1=1"),
            ("除外だけ", "COALESCE(hidden,0)<>0"),
            ("除外でないだけ", "COALESCE(hidden,0)=0"),
            ("位置なしだけ", "(lat IS NULL OR lon IS NULL)"),
            ("日時なしだけ", untimed_sql!()),
            ("日時ありだけ", timed_sql!()),
            (
                "除外の位置なし日時なし",
                "COALESCE(hidden,0)<>0 AND (lat IS NULL OR lon IS NULL) AND taken_at IS NULL",
            ),
        ] {
            let expected = ids_ordered_by(&conn, where_, COVER_ORDER);
            assert!(!expected.is_empty(), "{label}");
            assert_eq!(
                ids_ordered_by(&conn, where_, &key_desc),
                expected,
                "{label}"
            );
        }
        // 各組の最大（`MAX(COVER_KEY)`）は、窓関数で選んだ 1 枚と同じ写真（組は ID の余りで分ける）
        for modulus in [1, 3, 7] {
            let by_window: Vec<(i64, i64)> = conn
                .prepare(&format!(
                    "SELECT k, id FROM (SELECT id % {modulus} AS k, id,
                        ROW_NUMBER() OVER (PARTITION BY id % {modulus} ORDER BY {COVER_ORDER}) AS rn FROM photos)
                      WHERE rn = 1 ORDER BY k"
                ))
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let by_key: Vec<(i64, i64)> = conn
                .prepare(&format!(
                    "SELECT id % {modulus}, MAX({COVER_KEY}) FROM photos p GROUP BY 1 ORDER BY 1"
                ))
                .unwrap()
                .query_map([], |row| {
                    let key: String = row.get(1)?;
                    let tally = Tally {
                        best: Some(key),
                        ..Tally::default()
                    };
                    Ok((row.get(0)?, tally.cover_id().unwrap()))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(by_key, by_window, "modulus {modulus}");
        }
        // 除外しかない組（全部が除外）でも、位置・日時・ID の順で代表が決まる
        let hidden_only = ids_ordered_by(&conn, "COALESCE(hidden,0)<>0", COVER_ORDER);
        let best: String = conn
            .query_row(
                &format!("SELECT MAX({COVER_KEY}) FROM photos p WHERE COALESCE(hidden,0)<>0"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        let best = Tally {
            best: Some(best),
            ..Tally::default()
        };
        assert_eq!(best.cover_id(), hidden_only.first().copied());
    }

    #[test]
    fn q_is_trimmed_split_on_any_space_and_capped() {
        assert_eq!(parse_q("  松本　城 \u{3000}"), Some(vec!["松本", "城"]));
        assert_eq!(parse_q(" \u{3000} "), Some(vec![]));
        assert_eq!(parse_q("a b c d e"), Some(vec!["a", "b", "c", "d", "e"]));
        assert!(parse_q("a b c d e f").is_none());
        // 長さは前後の空白を除いたあとで数える
        let long = "あ".repeat(NAME_MAX_CHARS);
        assert!(parse_q(&format!(" {long} ")).is_some());
        assert!(parse_q(&format!("{long}あ")).is_none());
    }

    #[test]
    fn like_patterns_escape_the_backslash_first_then_percent_and_underscore() {
        assert_eq!(like_pattern("abc"), "%abc%");
        assert_eq!(like_pattern("50%_off"), "%50\\%\\_off%");
        assert_eq!(like_pattern("a\\%"), "%a\\\\\\%%");
        assert_eq!(like_pattern("\\"), "%\\\\%");
    }

    #[test]
    fn q_narrows_and_binds_six_patterns_per_word() {
        // 分類の表がある DB とない DB のどちらでも、語ごとに同じ数の `?` を持つ
        for available in [true, false] {
            let rules = regions::PlaceRules::new(available);
            let word = q_word_sql(&rules.search_like);
            let f = photos_filter(&params("q=%E3%81%82+b")).unwrap();
            let (clause, bound) = f.render(&rules);
            assert_eq!(clause.matches(&word).count(), 2);
            assert_eq!(bound.len(), 2 * Q_WORD_PARAMS);
            assert_eq!(
                bound
                    .iter()
                    .filter(|v| **v == Sql::Text("%あ%".to_owned()))
                    .count(),
                Q_WORD_PARAMS
            );
            assert!(!clause.contains("%あ%"), "値は SQL に入れない");
            // 文として正しく、`?` の数がバインドする値の数と同じ（`photos p` だけで引ける）
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE photos(id INTEGER PRIMARY KEY, lat REAL, lon REAL, pref TEXT, city TEXT,
                   place TEXT, name TEXT, taken_at TEXT, hidden INTEGER);",
            )
            .unwrap();
            if available {
                conn.execute_batch(regions::SCHEMA).unwrap();
            }
            let stmt = conn
                .prepare(&format!("SELECT COUNT(*) FROM photos p WHERE {clause}"))
                .unwrap();
            assert_eq!(stmt.parameter_count(), bound.len(), "table={available}");
        }
        assert!(photos_filter(&params("q=%E3%81%82+b")).unwrap().narrowed);
        // 検索は `photos` だけで引く（国・地域の式を使わない）
        assert!(!photos_filter(&params("q=a")).unwrap().rules);
        // 空白だけの q は無いのと同じ（絞り込みにならない）
        assert!(photos_filter(&params("q=%20%E3%80%80")).is_none());
        assert!(photos_filter(&params("q=%20&pref=x")).is_some());
        assert!(photos_filter(&params("q=a+b+c+d+e+f")).is_none());
    }

    #[test]
    fn the_rule_expressions_do_not_depend_on_the_table() {
        // `photos_filter` は式を表の有無に依らず作る（`from` だけが違う）。式が変わったらここで気づく
        let (a, b) = (
            regions::PlaceRules::new(true),
            regions::PlaceRules::new(false),
        );
        assert_eq!(a.country, b.country);
        assert_eq!(a.region, b.region);
        assert_eq!(a.no_location, b.no_location);
        assert_eq!(a.unclassified, b.unclassified);
        assert_eq!(a.search, b.search);
        assert_ne!(a.from, b.from);
    }

    #[test]
    fn photos_need_a_narrowing_filter() {
        assert!(photos_filter(&params("")).is_none());
        assert!(photos_filter(&params("hidden=1&page=2&per=5")).is_none());
        assert!(photos_filter(&params("untimed=0")).is_none());
        assert!(photos_filter(&params("untimed=1")).is_some());
        assert!(photos_filter(&params("abroad=0")).is_none());
        assert!(photos_filter(&params("abroad=1")).is_some());
        // 海外・不明は pref・city と同時に受けない（空の値は parse_qs が捨てるので、あっても無いのと同じ）
        assert!(photos_filter(&params("abroad=1&pref=x")).is_none());
        assert!(photos_filter(&params("abroad=1&city=x")).is_none());
        assert!(photos_filter(&params("abroad=1&pref=")).is_some());
        assert!(photos_filter(&params("abroad=0&pref=x")).is_some());
        assert!(photos_filter(&params("pref=%E6%9E%B6%E7%A9%BA%E7%9C%8C")).is_some());
        assert!(photos_filter(&params("ids=1,2&from=bad")).is_none());
    }

    /// 場所の絞り込みの形。旧形式だけなら規則の結合は要らず、新形式は要る。
    #[test]
    fn place_queries_come_in_two_shapes_that_do_not_mix() {
        let japan = data::JAPAN_ID;
        let tokyo = data::names()
            .unwrap()
            .region_of_pref("東京都")
            .unwrap()
            .id
            .clone();
        let ok = |query: &str| photos_filter(&params(query));
        // 旧形式: 今の画面が送る形。規則の結合は要らない（変更前と同じ文）
        for query in [
            "pref=a",
            "pref=a&city=b",
            "city=b",
            "abroad=1",
            "abroad=0&pref=a",
            // 新形式の旗の 0 は、無いのと同じ
            "pref=a&unclassified=0&no_location=0&region_unknown=0",
        ] {
            let f = ok(query).unwrap_or_else(|| panic!("{query}"));
            assert!(!f.rules, "{query}");
        }
        // 新形式
        for query in [
            format!("country={japan}"),
            format!("country={japan}&region={tokyo}"),
            format!("country={japan}&region={tokyo}&city=x"),
            format!("country={japan}&region_unknown=1"),
            "country=ne:FRA".to_owned(),
            "country=ne:FRA&region=ne:FRA-1".to_owned(),
            "country=ne:FRA&region_unknown=1".to_owned(),
            // 文法が正しい未知の ID は受ける（0 件になる）
            "country=ne:ZZZ&region=ne:ZZZ-1".to_owned(),
            "country=ne:FRA&region=unknown-id".to_owned(),
            "unclassified=1".to_owned(),
            "no_location=1".to_owned(),
            "unclassified=1&no_location=0".to_owned(),
            format!("country={}", "a".repeat(64)),
        ] {
            let f = ok(&query).unwrap_or_else(|| panic!("{query}"));
            assert!(f.rules && f.narrowed, "{query}");
        }
        // 400
        let fr_region = data::names()
            .unwrap()
            .regions
            .iter()
            .find(|r| r.country.as_deref() == Some("ne:FRA"))
            .unwrap()
            .id
            .clone();
        for query in [
            // 地域だけ・地域不明だけ
            format!("region={tokyo}"),
            "region_unknown=1".to_owned(),
            // 国と地域・地域不明の同時、国と区分の同時、区分どうし
            format!("country={japan}&region={tokyo}&region_unknown=1"),
            "unclassified=1&no_location=1".to_owned(),
            format!("country={japan}&unclassified=1"),
            format!("country={japan}&no_location=1"),
            "unclassified=1&region_unknown=1".to_owned(),
            // 別の国の地域
            format!("country={japan}&region={fr_region}"),
            format!("country=ne:FRA&region={tokyo}"),
            format!("country=ne:ZZZ&region={tokyo}"),
            // 旧形式との混用
            format!("country={japan}&pref=a"),
            format!("country={japan}&abroad=1"),
            "unclassified=1&pref=a".to_owned(),
            "no_location=1&abroad=1".to_owned(),
            "unclassified=1&city=a".to_owned(),
            // 新形式の `city` は日本の地域のときだけ
            format!("country={japan}&city=x"),
            format!("country={japan}&region_unknown=1&city=x"),
            format!("country=ne:FRA&region={fr_region}&city=x"),
            "country=ne:FRA&city=x".to_owned(),
            format!("country={japan}&region=unknown-id&city=x"),
            // ID の文法
            "country=ne%3BFRA".to_owned(),
            format!("country={}", "a".repeat(65)),
            "country=%E6%97%A5%E6%9C%AC".to_owned(),
            format!("country={japan}&region=a%20b"),
            // 旗は 0/1 だけ
            "unclassified=2".to_owned(),
            "no_location=true".to_owned(),
            format!("country={japan}&region_unknown=yes"),
            // 64 文字を超える `city`
            format!("country={japan}&region={tokyo}&city={}", "あ".repeat(65)),
        ] {
            assert!(ok(&query).is_none(), "{query}");
        }
        // 都道府県名は名称表のもの、市区町村はバインドされる
        let f = ok(&format!("country={japan}&region={tokyo}&city=x")).unwrap();
        assert!(f.params.contains(&Sql::Text("東京都".to_owned())));
        assert!(f.params.contains(&Sql::Text("x".to_owned())));
        assert!(f.conditions.iter().all(|c| !c.contains("'x'")));
    }
}
