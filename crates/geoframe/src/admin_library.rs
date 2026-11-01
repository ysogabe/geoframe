//! 管理画面の「ライブラリ」タブの読み取り API（docs/admin-library/design.md）。
//!
//! - `GET /api/admin/library/trips`: 撮影日時のある写真を 48 時間超の空きで旅に区切った一覧（新しい順）。
//! - `GET /api/admin/library/photos`: 期間・範囲・ID・地名・日時なしで絞った写真のページ。
//! - `GET /api/admin/library/points`: 位置のある写真の GeoJSON（地図用）。
//! - `GET /api/admin/library/calendar`: 年・月ごとの件数（日付の眺め方）。
//! - `GET /api/admin/library/places`: 都道府県・市区町村ごとの件数と海外・不明の件数（場所の眺め方）。
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

type Params = HashMap<String, String>;

fn query_params(query: Option<&str>) -> Params {
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
fn parse_ids(s: &str) -> Option<Vec<i64>> {
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
        let [min_lon, min_lat, max_lon, max_lat] = parse_bbox(bbox)?;
        filter.push(
            "lat IS NOT NULL AND lon IS NOT NULL AND lon >= ? AND lon <= ? AND lat >= ? AND lat <= ?",
            [min_lon, max_lon, min_lat, max_lat].map(Sql::Real),
        );
        filter.narrowed = true;
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

fn bad_params() -> Response {
    error_response(StatusCode::BAD_REQUEST, "bad_params")
}

/// 読み取りの結果を JSON の応答にする。読み取りの失敗・順番待ちの超過は `/api/admin/photos` と同じ 503。
fn respond<E: std::fmt::Display>(result: Result<Value, E>) -> Response {
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
            let display: Option<String> = row.get(7)?;
            let md5: Option<String> = row.get(9)?;
            let thumb = thumb_for(
                ctx,
                display.as_deref(),
                row.get::<_, Option<String>>(8)?.as_deref(),
            );
            let id: i64 = row.get(0)?;
            let thumb_url = if thumb.is_empty() {
                String::new()
            } else {
                crate::thumb::thumb_url(id, display.as_deref(), md5.as_deref(), row.get(10)?)
            };
            trip.cover = Some(Cover {
                id,
                thumb_url,
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
fn visible_clause(with_hidden: bool) -> &'static str {
    if with_hidden {
        ""
    } else {
        " AND COALESCE(hidden,0)=0"
    }
}

/// `GET /api/admin/library/calendar`。
pub async fn calendar(state: &AppState, query: Option<&str>) -> Response {
    let hidden = include_hidden(&query_params(query));
    respond(
        state
            .catalog
            .read(move |conn| build_calendar(conn, hidden))
            .await,
    )
}

/// 年・月ごとの件数。日時ありの形（[`TIMED`]）の行を `substr(taken_at,1,4)`・`substr(taken_at,1,7)` で数える
/// （暦として読めない値も形が合えばそのまま出る）。年は新しい順、月は古い順。
fn build_calendar(conn: &Connection, with_hidden: bool) -> rusqlite::Result<Value> {
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

/// `GET /api/admin/library/places`。
pub async fn places(state: &AppState, query: Option<&str>) -> Response {
    let hidden = include_hidden(&query_params(query));
    respond(
        state
            .catalog
            .read(move |conn| build_places(conn, hidden))
            .await,
    )
}

/// 都道府県・市区町村ごとの件数（`pref` が空でない行）と、海外・不明（`pref` が NULL か空。位置なしも入る）の件数。
/// 並びは件数の多い順、同数なら名前の順。`city` が空の行は `city: ""` の 1 項目にする。
fn build_places(conn: &Connection, with_hidden: bool) -> rusqlite::Result<Value> {
    let _snapshot = conn.unchecked_transaction()?;
    let visible = visible_clause(with_hidden);
    let mut stmt = conn.prepare(&format!(
        "SELECT CAST(pref AS TEXT), CAST(COALESCE(city,'') AS TEXT), COUNT(*) FROM photos
          WHERE NOT {ABROAD}{visible} GROUP BY 1, 2"
    ))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let abroad: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM photos WHERE {ABROAD}{visible}"),
        [],
        |row| row.get(0),
    )?;
    Ok(json!({"ok": true, "prefs": group_places(rows), "abroad": abroad}))
}

/// 都道府県の件数と市区町村の `(名前, 件数)`。
type PrefGroup = (String, i64, Vec<(String, i64)>);

/// `(pref, city, count)` を都道府県ごとにまとめ、件数の多い順（同数は名前の順）に並べる。
fn group_places(rows: Vec<(String, String, i64)>) -> Vec<Value> {
    let mut prefs: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    for (pref, city, count) in rows {
        prefs.entry(pref).or_default().push((city, count));
    }
    let mut prefs: Vec<PrefGroup> = prefs
        .into_iter()
        .map(|(pref, mut cities)| {
            cities.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let total = cities.iter().map(|c| c.1).sum();
            (pref, total, cities)
        })
        .collect();
    prefs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    prefs
        .into_iter()
        .map(|(pref, count, cities)| {
            let cities: Vec<Value> = cities
                .into_iter()
                .map(|(city, count)| json!({"city": city, "count": count}))
                .collect();
            json!({"pref": pref, "count": count, "cities": cities})
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

    #[test]
    fn places_are_grouped_by_pref_and_sorted_by_count_then_name() {
        let rows = vec![
            ("B県".to_owned(), "い市".to_owned(), 2),
            ("A県".to_owned(), "".to_owned(), 1),
            ("A県".to_owned(), "う市".to_owned(), 1),
            ("A県".to_owned(), "あ市".to_owned(), 1),
            ("C県".to_owned(), "え市".to_owned(), 4),
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
