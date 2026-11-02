//! 管理画面の「ライブラリ」タブの読み取り API（docs/admin-library/design.md）。
//!
//! - `GET /api/admin/library/trips`: 撮影日時のある写真を 48 時間超の空きで旅に区切った一覧（新しい順）。
//! - `GET /api/admin/library/photos`: 期間・範囲・ID・地名・日時なし・文字（`q`）で絞った写真のページ。
//! - `GET /api/admin/library/points`: 位置のある写真の GeoJSON（地図用）。
//! - `GET /api/admin/library/calendar`: 年・月ごとの件数と、年ごとの代表の写真（日付の眺め方）。
//! - `GET /api/admin/library/places`: 都道府県・市区町村ごとの件数・範囲（都道府県は代表の写真も）と海外・不明の件数
//!   （場所の眺め方。期間 `from`・`to` と地図の範囲 `bbox` で絞れる）。
//!
//! どれも `photos` 表だけを読む（`photo_curation` などの新しい表が無い DB、`GEOFRAME_INGEST=0` の読み取り専用の起動でも
//! 動く）。経路は完全一致の `GET` のみで、`server.rs` の `site` が呼ぶ。更新はしない。
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
/// `q` の 1 語ぶんの条件。検索する 5 列（都道府県・市区町村・地名・ファイル名・撮影日時）のどれかに部分一致する。
/// ユーザーの文字列は入れず、`?` に語ごとのパターンをバインドする（Rust のソースの `ESCAPE '\\'` は、SQL では `ESCAPE '\'`）。
const Q_WORD_SQL: &str = "(COALESCE(pref,'') LIKE ? ESCAPE '\\' OR COALESCE(city,'') LIKE ? ESCAPE '\\' \
     OR COALESCE(place,'') LIKE ? ESCAPE '\\' OR COALESCE(name,'') LIKE ? ESCAPE '\\' \
     OR COALESCE(taken_at,'') LIKE ? ESCAPE '\\')";
/// `Q_WORD_SQL` の `?` の数。
const Q_WORD_PARAMS: usize = 5;

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
}

impl Filter {
    fn push(&mut self, condition: &'static str, params: impl IntoIterator<Item = Sql>) {
        self.conditions.push(condition.to_owned());
        self.params.extend(params);
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

    /// 語ごとに 5 列のどれかに部分一致、語同士は AND。条件は固定の型を語の数だけ並べ、語はパターンとしてバインドする。
    fn words(&mut self, words: &[&str]) {
        for word in words {
            self.conditions.push(Q_WORD_SQL.to_owned());
            let pattern = like_pattern(word);
            self.params
                .extend(std::iter::repeat_n(Sql::Text(pattern), Q_WORD_PARAMS));
        }
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
}

/// `photos` の絞り込み。形が違うか、絞り込みが 1 つも無ければ `None`。
fn photos_filter(params: &Params) -> Option<Filter> {
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
    if params.get("abroad").is_some_and(|v| v == "1") {
        if params.contains_key("pref") || params.contains_key("city") {
            return None;
        }
        filter.push(ABROAD, []);
        filter.narrowed = true;
    }
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
            cover: None,
        }
    }

    fn into_json(self) -> Value {
        let mut places: Vec<((String, String), i64)> = self.places.into_iter().collect();
        // 件数の多い順、同数なら名前の順（応答を安定させる）
        places.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        places.truncate(PLACES_MAX);
        let places: Vec<Value> = places
            .into_iter()
            .map(|((pref, city), count)| json!({"pref": pref, "city": city, "count": count}))
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
        let key = if pref.is_empty() {
            (String::new(), String::new())
        } else {
            (pref, text(row, 5)?)
        };
        *trip.places.entry(key).or_insert(0) += 1;
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
        "trips": trips.into_iter().map(Trip::into_json).collect::<Vec<_>>(),
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
    let clause = filter.where_clause();
    let per = per.clamp(1, PER_PAGE_MAX);
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM photos WHERE {clause}"),
        params_from_iter(filter.params.iter()),
        |row| row.get(0),
    )?;
    let pages = ((total + per - 1) / per).max(1);
    let page = page.clamp(1, pages);

    let mut stmt = conn.prepare(&format!(
        "SELECT id, name, display, drive_id, taken_at, lat, lon, loc_source,
                place, pref, city, favorite, hidden, rotation, source, md5, mtime
           FROM photos WHERE {clause} ORDER BY {ORDER} LIMIT ? OFFSET ?"
    ))?;
    let mut bound = filter.params.clone();
    bound.push(Sql::Integer(per));
    bound.push(Sql::Integer((page - 1) * per));
    let mut rows = stmt.query(params_from_iter(bound.iter()))?;
    let mut items = Vec::new();
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
    Ok(json!({
        "ok": true,
        "page": page,
        "per": per,
        "pages": pages,
        "total": total,
        "items": items,
    }))
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

/// 都道府県・市区町村ごとの件数・範囲（`pref` が空でない行）と、海外・不明（`pref` が NULL か空。位置なしも入る）の件数。
/// `filter`（期間・範囲・除外）は件数・都道府県の代表の写真・範囲と海外・不明のすべてに掛かる。範囲があれば、その中に
/// 写真の無い都道府県・市区町村は出ず、海外・不明は位置のある写真だけを数える（範囲が位置のある写真だけを残すため）。
/// 並びは件数の多い順、同数なら名前の順。`city` が空の行は `city: ""` の 1 項目にする。
fn build_places(conn: &Connection, ctx: &AdminContext, filter: &Filter) -> rusqlite::Result<Value> {
    // 件数・代表の写真・海外・不明が同じ時点の DB から出るように、1 つの読み取りにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let clause = filter.where_clause();
    // 範囲は位置のある写真だけの MIN/MAX（集計関数は NULL を飛ばすので、位置の無い行は CASE で NULL にする）
    let mut stmt = conn.prepare(&format!(
        "SELECT CAST(pref AS TEXT), CAST(COALESCE(city,'') AS TEXT), COUNT(*),
                MIN(CASE WHEN lat IS NOT NULL AND lon IS NOT NULL THEN lon END),
                MIN(CASE WHEN lat IS NOT NULL AND lon IS NOT NULL THEN lat END),
                MAX(CASE WHEN lat IS NOT NULL AND lon IS NOT NULL THEN lon END),
                MAX(CASE WHEN lat IS NOT NULL AND lon IS NOT NULL THEN lat END)
           FROM photos WHERE NOT {ABROAD} AND {clause} GROUP BY 1, 2"
    ))?;
    let rows = stmt
        .query_map(params_from_iter(filter.params.iter()), |row| {
            let corners = [
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
            ];
            let bounds = match corners {
                [Some(w), Some(s), Some(e), Some(n)] => Some([w, s, e, n]),
                _ => None,
            };
            Ok(PlaceRow {
                pref: row.get(0)?,
                city: row.get(1)?,
                count: row.get(2)?,
                bounds,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut pref_covers = covers(
        conn,
        ctx,
        "CAST(pref AS TEXT)",
        &format!("NOT {ABROAD} AND {clause}"),
        &filter.params,
    )?;
    let abroad: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM photos WHERE {ABROAD} AND {clause}"),
        params_from_iter(filter.params.iter()),
        |row| row.get(0),
    )?;
    let mut prefs = group_places(rows);
    for pref in &mut prefs {
        let name = pref["pref"].as_str().unwrap_or_default();
        pref["cover"] = pref_covers.remove(name).unwrap_or(Value::Null);
    }
    Ok(json!({"ok": true, "prefs": prefs, "abroad": abroad}))
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
    fn q_narrows_and_binds_five_patterns_per_word() {
        let f = photos_filter(&params("q=%E3%81%82+b")).unwrap();
        assert_eq!(f.conditions.iter().filter(|c| *c == Q_WORD_SQL).count(), 2);
        assert_eq!(f.params.len(), 2 * Q_WORD_PARAMS);
        assert!(f.narrowed);
        // 空白だけの q は無いのと同じ（絞り込みにならない）
        assert!(photos_filter(&params("q=%20%E3%80%80")).is_none());
        assert!(photos_filter(&params("q=%20&pref=x")).is_some());
        assert!(photos_filter(&params("q=a+b+c+d+e+f")).is_none());
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
}
