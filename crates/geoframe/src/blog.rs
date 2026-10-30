//! 旅行ブログの読み取り API（E7〜E10。T3.5）: 現行 `app/geoframe/travel_blog.py` の
//! `api_blog_trips`・`api_blog_trip`・`api_blog_candidates`・`api_blog_draft` と、その部品
//! （`list_plans`・`load_plan`・`_segment_candidates`・`generate_manifest`・`_validate_manifest`）を同じ挙動で移したもの。
//! 保存・書き出し（E15・E16）は `blog_write.rs`（T3.6）。ここの [`is_valid_trip_id`]・[`validate_manifest`]・[`PlanSource`] はそちらも使う。
//!
//! # 置き場
//!
//! - 旅程 JSON（git 管理のソース）: 既定ではバイナリに埋め込んだ `web/travel_blog/plans/*.json`
//!   （`embedded.rs`）。`GEOFRAME_BLOG_PLANS_DIR` があればそのディレクトリの `*.json` を読む（[`PlanSource`]）。
//! - 下書き: `DATA_DIR/travel_blog/drafts/<trip_id>.json`、書き出し: `DATA_DIR/blog_exports/<trip_id>/index.html`
//!   （一次データ。ここでは読むだけ）。
//!
//! # 守ること
//!
//! - **正確な位置を返さない**: 候補の SQL は `lat`・`lon`・`pref`・`elev`・`name`（ファイル名）を選ばない。
//!   応答に出す位置は粗い地名（`city`・`place`）か、旅程の `locations` だけ。
//! - 旅程・下書きの中身（地名・本文）と `trip_id` をログに出さない。エラーの本文は Python と同じ定型の文面
//!   （ファイル名は検査済みの `trip_id` + `.json`）。
//! - 日付範囲は **SQLite の文字列比較だけ**（`taken_at>=? AND taken_at<=?`）。時刻の解釈・タイムゾーンの換算はしない
//!   （`timezone` は応答にそのまま出すだけ）。Python と同じ SQL・同じ値の組み立てにして、`"9:00"` が `"10:00"` より
//!   後ろに並ぶことなども含めて同じ結果にする。
//!
//! 旅程・下書きの JSON の型が想定と違うとき（Python は未処理の例外で応答なし）の扱いは server.md §20.8。

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, types::ValueRef};
use serde_json::{Map, Value, json};

use crate::db::{Catalog, CatalogError};
use crate::embedded;
use crate::pyval::{
    DictKey, dict_key, is_py_digit_str, prefix_chars, py_str, py_str_or, round3, truthy,
};

/// 1 日あたりの候補グループの上限。
const MAX_CANDIDATE_GROUPS: usize = 36;
/// 1 日あたり初期案で選ぶ枚数。
const DEFAULT_SELECTED_PER_DAY: usize = 6;
/// 下書きの章の数・1 章の画像数・合計の画像数の上限（`_validate_manifest`）。
const MAX_CHAPTERS: usize = 40;
const MAX_IMAGES_PER_CHAPTER: usize = 100;
/// 章横断の総画像数の上限（`MAX_TOTAL_EXPORT_IMAGES`）。
pub const MAX_TOTAL_EXPORT_IMAGES: usize = 200;
/// 旅程・下書きの JSON を読む大きさの上限（Python は無制限）。下書きは保存時に 64 KiB の本文から作る。
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;

/// 応答に使う定型文（Python の `BlogBadRequest`/`BlogNotFound` の文面）。
const MSG_TRIP_ID: &str = "trip_idが不正です";
const MSG_PLAN_FORMAT: &str = "旅程JSONの形式が不正です";
const MSG_PLAN_MISSING: &str = "旅程が見つかりません";

/// 読み取り API の失敗。本文は定型文だけで、環境の情報（パス・SQL）を載せない。
#[derive(Debug, PartialEq, Eq)]
pub enum BlogError {
    /// 400（Python の `BlogBadRequest`）。文面は Python と同じ。
    BadRequest(String),
    /// 404（Python の `BlogNotFound`）。
    NotFound(String),
    /// DB を読めない・順番待ちが長すぎる（503。Python は 500 か応答なし）。理由はログにだけ出す。
    Unavailable(String),
    /// 想定外（ブロッキング処理の異常終了など。500）。
    Internal,
}

fn bad(message: &str) -> BlogError {
    BlogError::BadRequest(message.to_string())
}

fn bad_plan() -> BlogError {
    bad(MSG_PLAN_FORMAT)
}

/// `trip_id` の形（`^[a-z0-9][a-z0-9-]{0,79}$`）。
pub fn is_valid_trip_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=80).contains(&bytes.len())
        && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
        && bytes[1..]
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// `_trip_id(value)`: `str(value or "")` が形に合えばその文字列。
pub fn trip_id_of(value: Option<&Value>) -> Option<String> {
    let id = py_str_or(value, "");
    is_valid_trip_id(&id).then_some(id)
}

/// 旅程 JSON の置き場。
#[derive(Clone, Debug)]
pub enum PlanSource {
    /// バイナリに埋め込んだもの（既定）。
    Embedded,
    /// `GEOFRAME_BLOG_PLANS_DIR` のディレクトリ。
    Dir(PathBuf),
}

/// 旅行ブログが読む場所。複製は安い。
#[derive(Clone, Debug)]
pub struct BlogEnv {
    pub plans: PlanSource,
    /// `DATA_DIR/travel_blog/drafts`
    pub draft_dir: PathBuf,
    /// `DATA_DIR/blog_exports`
    pub export_dir: PathBuf,
}

impl BlogEnv {
    /// `data_root` はリンクを解決したデータ置き場。
    pub fn new(plans_dir: Option<&Path>, data_root: &Path) -> Self {
        Self {
            plans: plans_dir.map_or(PlanSource::Embedded, |dir| {
                PlanSource::Dir(dir.to_path_buf())
            }),
            draft_dir: data_root.join("travel_blog").join("drafts"),
            export_dir: data_root.join("blog_exports"),
        }
    }

    fn draft_path(&self, trip_id: &str) -> PathBuf {
        self.draft_dir.join(format!("{trip_id}.json"))
    }
}

/// 読んだファイルの中身。読めない（無い・ディレクトリ・大きすぎる・UTF-8 でない・JSON でない）は `None`。
fn read_json_file(path: &Path) -> Option<Value> {
    let meta = fs::metadata(path).ok()?;
    // FIFO などを開いて待たない
    if !meta.is_file() || meta.len() > MAX_JSON_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_json(&bytes)
}

fn parse_json(bytes: &[u8]) -> Option<Value> {
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

impl PlanSource {
    /// `*.json` のファイル名（バイト順）。Python の `sorted(PLAN_DIR.glob("*.json"))`。
    /// ディレクトリが無い・読めないときは空。
    fn names(&self) -> Vec<String> {
        match self {
            PlanSource::Embedded => embedded::plan_files()
                .iter()
                .map(|f| f.name.to_string())
                .collect(),
            PlanSource::Dir(dir) => {
                let mut names = Vec::new();
                match fs::read_dir(dir) {
                    Ok(read) => {
                        for entry in read.flatten() {
                            if let Ok(name) = entry.file_name().into_string()
                                && name.ends_with(".json")
                            {
                                names.push(name);
                            }
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!("旅程のディレクトリを読めません: {:?}", e.kind());
                        }
                    }
                }
                names.sort();
                names
            }
        }
    }

    /// `PLAN_DIR/<name>` が存在するか（`Path.exists()`。ディレクトリでも真）。
    fn exists(&self, name: &str) -> bool {
        match self {
            PlanSource::Embedded => embedded::plan_files().iter().any(|f| f.name == name),
            PlanSource::Dir(dir) => dir.join(name).exists(),
        }
    }

    /// 読んで JSON にする。読めなければ `None`。
    fn read(&self, name: &str) -> Option<Value> {
        match self {
            PlanSource::Embedded => embedded::plan_files()
                .iter()
                .find(|f| f.name == name)
                .and_then(|f| parse_json(f.data)),
            PlanSource::Dir(dir) => read_json_file(&dir.join(name)),
        }
    }
}

/// Python の `len(x or [])`。長さを持たない型（数・真偽）は `None`（Python は `TypeError` → 一覧から外す）。
fn len_or_empty(value: Option<&Value>) -> Option<usize> {
    match value {
        None | Some(Value::Null) => Some(0),
        Some(Value::Array(a)) => Some(a.len()),
        Some(Value::Object(o)) => Some(o.len()),
        Some(Value::String(s)) => Some(s.chars().count()),
        Some(Value::Bool(false)) => Some(0),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => Some(0),
        Some(_) => None,
    }
}

/// E7 の 1 件。形が合わない旅程は `None`（一覧から外す）。
fn plan_summary(env: &BlogEnv, plan: &Value) -> Option<Value> {
    let plan = plan.as_object()?;
    let trip_id = trip_id_of(plan.get("trip_id"))?;
    let empty = Map::new();
    let date_range = match plan.get("date_range") {
        Some(v) if truthy(v) => v.as_object()?,
        _ => &empty,
    };
    let title = match plan.get("title") {
        Some(v) if truthy(v) => py_str(v),
        _ => trip_id.clone(),
    };
    let days = len_or_empty(plan.get("segments"))?;
    Some(json!({
        "trip_id": trip_id,
        "title": title,
        "subtitle": py_str_or(plan.get("subtitle"), ""),
        "start": date_range.get("start").cloned().unwrap_or_else(|| json!("")),
        "end": date_range.get("end").cloned().unwrap_or_else(|| json!("")),
        "days": days,
        "has_draft": env.draft_path(&trip_id).exists(),
        "has_export": env.export_dir.join(&trip_id).join("index.html").exists(),
    }))
}

/// E7 `GET /api/blog/trips`。読めない・形が合わない旅程は一覧から外す。
pub fn api_trips(env: &BlogEnv) -> Value {
    let items: Vec<Value> = env
        .plans
        .names()
        .iter()
        .filter_map(|name| env.plans.read(name))
        .filter_map(|plan| plan_summary(env, &plan))
        .collect();
    json!({"ok": true, "items": items})
}

/// `load_plan`: 検査 → 無ければ 404 → 読めなければ 400 → `trip_id` が違う・`segments` が配列でなければ 400。
pub fn load_plan(env: &BlogEnv, trip_id: &str) -> Result<Value, BlogError> {
    if !is_valid_trip_id(trip_id) {
        return Err(bad(MSG_TRIP_ID));
    }
    let name = format!("{trip_id}.json");
    if !env.plans.exists(&name) {
        return Err(BlogError::NotFound(MSG_PLAN_MISSING.to_string()));
    }
    let Some(plan) = env.plans.read(&name) else {
        return Err(BlogError::BadRequest(format!(
            "JSONを読み込めません: {name}"
        )));
    };
    let ok = plan.as_object().is_some_and(|o| {
        o.get("trip_id").and_then(Value::as_str) == Some(trip_id)
            && o.get("segments").is_some_and(Value::is_array)
    });
    if ok { Ok(plan) } else { Err(bad_plan()) }
}

/// E8 `GET /api/blog/trip`。
pub fn api_trip(env: &BlogEnv, trip_id: &str) -> Result<Value, BlogError> {
    Ok(json!({"ok": true, "plan": load_plan(env, trip_id)?}))
}

// ---- 候補 -----------------------------------------------------------------------------------

/// 候補の SQL。**`lat`・`lon`・`pref`・`elev`・`name` を選ばない**（応答に位置・ファイル名を載せないことを構造で守る）。
const CANDIDATE_SQL: &str = "SELECT id, display, taken_at, loc_source, place, city,
                                    title, ai_caption, favorite
                             FROM photos
                             WHERE COALESCE(hidden,0)=0 AND taken_at>=?1 AND taken_at<=?2
                                   AND display IS NOT NULL
                             ORDER BY taken_at, id";

struct Row {
    id: i64,
    display: String,
    taken_at: Option<String>,
    loc_source: Option<String>,
    place: Option<String>,
    city: Option<String>,
    title: Option<String>,
    ai_caption: Option<String>,
    favorite: bool,
}

/// Python の `bool(value)`（SQLite の値）。
fn sql_truthy(value: ValueRef<'_>) -> bool {
    match value {
        ValueRef::Null => false,
        ValueRef::Integer(i) => i != 0,
        ValueRef::Real(f) => f != 0.0,
        ValueRef::Text(t) | ValueRef::Blob(t) => !t.is_empty(),
    }
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        id: row.get(0)?,
        display: row.get(1)?,
        taken_at: row.get(2)?,
        loc_source: row.get(3)?,
        place: row.get(4)?,
        city: row.get(5)?,
        title: row.get(6)?,
        ai_caption: row.get(7)?,
        favorite: sql_truthy(row.get_ref(8)?),
    })
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|v| !v.is_empty())
}

/// `_photo_src`。
fn photo_src(display: &str) -> String {
    let display = display.replace('\\', "/");
    let display = display.trim_start_matches('/');
    if display.is_empty() || display.starts_with('.') || format!("/{display}/").contains("/../") {
        return String::new();
    }
    format!("/{display}")
}

/// `_location_label`: `[city, place]` の空でないものを重複なしで ` ` で繋ぎ、前後の空白を落とす。
/// 空なら `fallback`。
fn location_label(row: &Row, fallback: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in [non_empty(&row.city), non_empty(&row.place)]
        .into_iter()
        .flatten()
    {
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    let label = crate::pyquery::py_strip(&parts.join(" ")).to_string();
    if label.is_empty() {
        fallback.to_string()
    } else {
        label
    }
}

/// `_base_score`。足す順序も Python と同じ（浮動小数点の結果を同じにする）。
fn base_score(row: &Row) -> (f64, Vec<&'static str>) {
    let mut score = 0.20_f64;
    let mut reasons = vec!["旅程日の撮影時刻と一致"];
    if !row.display.is_empty() {
        score += 0.25;
        reasons.push("表示用画像を取得済み");
    }
    match row.loc_source.as_deref() {
        Some("exif" | "drive_meta" | "manual") => {
            score += 0.25;
            reasons.push("位置情報あり");
        }
        Some("inferred") => {
            score += 0.12;
            reasons.push("周辺写真から位置を推定");
        }
        _ => {}
    }
    if row.favorite {
        score += 0.25;
        reasons.push("お気に入り");
    }
    if non_empty(&row.title).is_some() || non_empty(&row.ai_caption).is_some() {
        score += 0.05;
        reasons.push("説明メタデータあり");
    }
    (score.min(1.0), reasons)
}

/// 同じ分に撮られた写真の 1 枚。
struct Photo {
    id: i64,
    src: String,
    captured_at: String,
    location_label: String,
    timezone_source: String,
    score: f64,
    reasons: Vec<&'static str>,
    favorite: bool,
    alt: Value,
    caption: String,
}

/// 候補（グループの代表）。
pub struct Candidate {
    photo: Photo,
    group_id: String,
    count: usize,
    alternates: Vec<Value>,
    selected: bool,
}

impl Candidate {
    fn to_json(&self) -> Value {
        let p = &self.photo;
        json!({
            "photo_id": p.id.to_string(),
            "src": p.src,
            "captured_at": p.captured_at,
            "location_label": p.location_label,
            "timezone_source": p.timezone_source,
            "score": p.score,
            "selection_reason": p.reasons,
            "favorite": p.favorite,
            "alt": p.alt,
            "caption": p.caption,
            "duplicate_group_id": self.group_id,
            "duplicate_count": self.count,
            "alternates": self.alternates,
            "selected": self.selected,
        })
    }
}

/// 日ごと（旅程の 1 区間）の候補。
pub struct Chapter {
    segment_id: Value,
    date: String,
    title: Value,
    timezone: Value,
    location_label: String,
    photo_count: usize,
    duplicate_groups: usize,
    candidates: Vec<Candidate>,
}

impl Chapter {
    fn to_json(&self) -> Value {
        json!({
            "segment_id": self.segment_id,
            "date": self.date,
            "title": self.title,
            "timezone": self.timezone,
            "location_label": self.location_label,
            "photo_count": self.photo_count,
            "duplicate_groups": self.duplicate_groups,
            "candidates": self.candidates.iter().map(Candidate::to_json).collect::<Vec<_>>(),
        })
    }
}

/// Python の `round()`（偶数丸め）で整数にする。
fn round_even(x: f64) -> usize {
    x.round_ties_even() as usize
}

/// `_evenly_spaced`: 残す位置（元の順序のまま、`limit` 個まで）。お気に入りは等間隔の抜き取りから漏らさない。
fn evenly_spaced(favorites: &[bool], limit: usize) -> Vec<usize> {
    let n = favorites.len();
    if n <= limit {
        return (0..n).collect();
    }
    if limit <= 1 {
        return vec![0];
    }
    let mut keep: BTreeSet<usize> = (0..limit)
        .map(|i| round_even((i * (n - 1)) as f64 / (limit - 1) as f64))
        .collect();
    keep.extend(
        favorites
            .iter()
            .enumerate()
            .filter(|(_, fav)| **fav)
            .map(|(i, _)| i),
    );
    (0..n).filter(|i| keep.contains(i)).take(limit).collect()
}

/// `segment.get("locations") or []` の先頭 2 つを「・」で繋いだ文字列（`"・".join(locations[:2])`）。
/// 文字列でない要素・配列でも文字列でもない値は Python が `TypeError` で落ちる形。
fn location_fallback(value: Option<&Value>) -> Result<String, BlogError> {
    match value {
        Some(v) if truthy(v) => match v {
            Value::Array(items) => {
                let mut parts = Vec::new();
                for item in items.iter().take(2) {
                    parts.push(item.as_str().ok_or_else(bad_plan)?);
                }
                Ok(parts.join("・"))
            }
            // `"abc"[:2]` は `"ab"`、`"・".join("ab")` は `"a・b"`
            Value::String(s) => Ok(s
                .chars()
                .take(2)
                .map(String::from)
                .collect::<Vec<_>>()
                .join("・")),
            _ => Err(bad_plan()),
        },
        _ => Ok(String::new()),
    }
}

/// `_segment_candidates`。
fn segment_candidates(
    conn: &Connection,
    segment: &Map<String, Value>,
) -> rusqlite::Result<Result<Chapter, BlogError>> {
    let date = py_str_or(segment.get("date"), "");
    let start = format!("{date} {}", py_str_or(segment.get("start_local"), "00:00"));
    let end = format!("{date} {}", py_str_or(segment.get("end_local"), "23:59"));
    let mut stmt = conn.prepare(CANDIDATE_SQL)?;
    let rows: Vec<Row> = stmt
        .query_map([&start, &end], row_from)?
        .collect::<rusqlite::Result<_>>()?;

    let fallback = match location_fallback(segment.get("locations")) {
        Ok(f) => f,
        Err(e) => return Ok(Err(e)),
    };
    let segment_title = segment.get("title").filter(|v| truthy(v));

    // DB の時刻精度は分。同一分の連写を重複候補グループとして扱う（隣り合う同じ値だけ）
    let mut groups: Vec<Vec<Photo>> = Vec::new();
    let mut current_key: Option<String> = None;
    for row in &rows {
        let key =
            non_empty(&row.taken_at).map_or_else(|| format!("photo-{}", row.id), str::to_string);
        if current_key.as_deref() != Some(&key) {
            groups.push(Vec::new());
            current_key = Some(key);
        }
        let (score, reasons) = base_score(row);
        let alt = if let Some(t) = non_empty(&row.title) {
            json!(t)
        } else if let Some(c) = non_empty(&row.ai_caption) {
            json!(c)
        } else if let Some(t) = segment_title {
            t.clone()
        } else {
            json!("旅の写真")
        };
        groups.last_mut().expect("直前に追加した").push(Photo {
            id: row.id,
            src: photo_src(&row.display),
            captured_at: row.taken_at.clone().unwrap_or_default(),
            location_label: location_label(row, &fallback),
            timezone_source: non_empty(&row.loc_source).unwrap_or("unknown").to_string(),
            score: round3(score),
            reasons,
            favorite: row.favorite,
            alt,
            caption: row.ai_caption.clone().unwrap_or_default(),
        });
    }

    let segment_id = segment.get("id").cloned().unwrap_or(Value::Null);
    let group_count = groups.len();
    let mut representatives: Vec<(Photo, usize, Vec<Value>, String)> = Vec::new();
    for (number, mut group) in groups.into_iter().enumerate() {
        // 点の高い順、同点は id の小さい順（安定ソート）
        group.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
        let alternates: Vec<Value> = group
            .iter()
            .skip(1)
            .take(3)
            .map(|p| {
                json!({
                    "photo_id": p.id.to_string(),
                    "src": p.src,
                    "captured_at": p.captured_at,
                    "score": p.score,
                    "alt": p.alt,
                })
            })
            .collect();
        let count = group.len();
        let group_id = format!("{}-g{}", py_str(&segment_id), number + 1);
        let top = group.into_iter().next().expect("グループは空でない");
        representatives.push((top, count, alternates, group_id));
    }

    let favorites: Vec<bool> = representatives.iter().map(|r| r.0.favorite).collect();
    let keep = evenly_spaced(&favorites, MAX_CANDIDATE_GROUPS);
    let kept: Vec<_> = representatives
        .into_iter()
        .enumerate()
        .filter(|(i, _)| keep.contains(i))
        .map(|(_, r)| r)
        .collect();

    let mut selected: BTreeSet<usize> = BTreeSet::new();
    if !kept.is_empty() {
        let wanted = DEFAULT_SELECTED_PER_DAY.min(kept.len());
        let denom = (wanted - 1).max(1) as f64;
        selected = (0..wanted)
            .map(|i| round_even((i * (kept.len() - 1)) as f64 / denom))
            .collect();
        selected.extend(
            kept.iter()
                .enumerate()
                .filter(|(_, r)| r.0.favorite)
                .map(|(i, _)| i),
        );
    }
    let candidates = kept
        .into_iter()
        .enumerate()
        .map(|(i, (photo, count, alternates, group_id))| Candidate {
            photo,
            group_id,
            count,
            alternates,
            selected: selected.contains(&i),
        })
        .collect();

    let date_title = Value::String(date.clone());
    Ok(Ok(Chapter {
        segment_id,
        title: segment_title.cloned().unwrap_or(date_title),
        timezone: segment
            .get("timezone")
            .filter(|v| truthy(v))
            .cloned()
            .unwrap_or_else(|| json!("")),
        date,
        location_label: fallback,
        photo_count: rows.len(),
        duplicate_groups: group_count,
        candidates,
    }))
}

/// 旅程の `segments` の各要素（オブジェクト）。そうでなければ 400（Python は未処理の例外）。
fn segments_of(plan: &Value) -> Result<Vec<&Map<String, Value>>, BlogError> {
    plan.get("segments")
        .and_then(Value::as_array)
        .ok_or_else(bad_plan)?
        .iter()
        .map(|s| s.as_object().ok_or_else(bad_plan))
        .collect()
}

/// 旅程の全区間の候補。`Ok(Err(..))` は旅程の形の問題（400）、`Err(..)` は DB。
fn chapters(conn: &Connection, plan: &Value) -> rusqlite::Result<Result<Vec<Chapter>, BlogError>> {
    let segments = match segments_of(plan) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let mut out = Vec::with_capacity(segments.len());
    for segment in segments {
        match segment_candidates(conn, segment)? {
            Ok(chapter) => out.push(chapter),
            Err(e) => return Ok(Err(e)),
        }
    }
    Ok(Ok(out))
}

/// `plan.get("title") or plan["trip_id"]`（`plan` は `load_plan` を通ったもの）。
fn plan_title(plan: &Value) -> Value {
    match plan.get("title") {
        Some(v) if truthy(v) => v.clone(),
        _ => plan.get("trip_id").cloned().unwrap_or(Value::Null),
    }
}

/// E9 `api_blog_candidates` の本体。
pub fn candidates_json(
    conn: &Connection,
    plan: &Value,
) -> rusqlite::Result<Result<Value, BlogError>> {
    let chapters = match chapters(conn, plan)? {
        Ok(c) => c,
        Err(e) => return Ok(Err(e)),
    };
    Ok(Ok(json!({
        "ok": true,
        "trip_id": plan.get("trip_id").cloned().unwrap_or(Value::Null),
        "title": plan_title(plan),
        "photo_count": chapters.iter().map(|c| c.photo_count).sum::<usize>(),
        "chapters": chapters.iter().map(Chapter::to_json).collect::<Vec<_>>(),
        "privacy": {"exact_gps_in_response": false},
    })))
}

// ---- 初期案 ---------------------------------------------------------------------------------

/// `_outline_body`。
fn outline_body(segment: &Map<String, Value>, photo_count: usize) -> Result<String, BlogError> {
    let activities: &[Value] = match segment.get("activities") {
        Some(v) if truthy(v) => v.as_array().ok_or_else(bad_plan)?,
        _ => &[],
    };
    let mut parts = Vec::with_capacity(activities.len());
    for activity in activities {
        let activity = activity.as_object().ok_or_else(bad_plan)?;
        let time = activity
            .get("time")
            .map_or_else(|| "時刻未確認".to_string(), py_str);
        let title = activity.get("title").map(py_str).unwrap_or_default();
        parts.push(format!("{time} {title}"));
    }
    let outline = parts.join("、");
    let title = segment.get("title").map(py_str).unwrap_or_default();
    Ok(format!(
        "しおりでは「{title}」を予定。写真DBにはこの日の写真が{photo_count}枚あります。予定の流れは {outline}。実際に訪れた場所、時刻、感想は写真を見ながら追記してください。[要確認]"
    ))
}

/// `generate_manifest` の本体（候補から作る初期案）。
fn manifest_json(conn: &Connection, plan: &Value) -> rusqlite::Result<Result<Value, BlogError>> {
    let chapters = match chapters(conn, plan) {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Ok(Err(e)),
        Err(e) => return Err(e),
    };
    let segments = match segments_of(plan) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    // `{s.get("id"): s for s in segments}`: キーは Python の `dict` と同じ等価性（`5` と `5.0`、`1` と
    // `True` は同じキー）で、同じキーなら後のものが残る。配列・オブジェクトの id は Python が `TypeError`。
    let mut by_id: HashMap<DictKey, &Map<String, Value>> = HashMap::new();
    for segment in &segments {
        let Some(key) = id_key(segment.get("id")) else {
            return Ok(Err(bad_plan()));
        };
        by_id.insert(key, segment);
    }
    let mut out = Vec::with_capacity(chapters.len());
    for chapter in &chapters {
        let Some(segment) = id_key(Some(&chapter.segment_id)).and_then(|k| by_id.get(&k).copied())
        else {
            return Ok(Err(bad_plan()));
        };
        let images: Vec<Value> = chapter
            .candidates
            .iter()
            .filter(|c| c.selected)
            .map(|c| {
                let p = &c.photo;
                let caption = if p.caption.is_empty() {
                    format!(
                        "{}（{}）[要確認]",
                        py_str(&chapter.title),
                        prefix_chars(&p.captured_at.chars().skip(11).collect::<String>(), 5)
                    )
                } else {
                    p.caption.clone()
                };
                json!({
                    "photo_id": p.id.to_string(),
                    "src": p.src,
                    "alt": p.alt,
                    "caption": caption,
                    "score": p.score,
                    "selection_reason": p.reasons,
                })
            })
            .collect();
        let body = match outline_body(segment, chapter.photo_count) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        out.push(json!({
            "id": chapter.segment_id,
            "date": chapter.date,
            "title": chapter.title,
            "location_label": chapter.location_label,
            "body": body,
            "images": images,
        }));
    }
    Ok(Ok(json!({
        "schema_version": "1.0",
        "trip_id": plan.get("trip_id").cloned().unwrap_or(Value::Null),
        "title": plan_title(plan),
        "subtitle": match plan.get("subtitle") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!(""),
        },
        "status": "draft",
        "summary": "旅程と写真の撮影時刻から生成した下書きです。公開前に事実と文章を確認してください。",
        "chapters": out,
    })))
}

/// 区間の `id`（`dict` のキー）。無ければ `None`（`dict.get` の `None`）。
fn id_key(id: Option<&Value>) -> Option<DictKey> {
    dict_key(id.unwrap_or(&Value::Null))
}

// ---- 下書きの検査 ---------------------------------------------------------------------------

/// `_validate_manifest`: 形を検査し、文字列を切り詰め、`photo_id` を文字列にした複製を返す。
/// 知らないキーはそのまま残る。T3.6 の保存・書き出しも使う。
pub fn validate_manifest(
    value: &Value,
    expected_trip_id: Option<&str>,
) -> Result<Value, BlogError> {
    let Some(object) = value.as_object() else {
        return Err(bad("manifestはJSONオブジェクトにしてください"));
    };
    let trip_id = trip_id_of(object.get("trip_id")).ok_or_else(|| bad(MSG_TRIP_ID))?;
    if let Some(expected) = expected_trip_id
        && !expected.is_empty()
        && trip_id != expected
    {
        return Err(bad("trip_idが一致しません"));
    }
    if !matches!(
        object.get("status").and_then(Value::as_str),
        Some("draft" | "reviewed" | "published")
    ) {
        return Err(bad("statusが不正です"));
    }
    match object.get("chapters") {
        Some(Value::Array(chapters)) if chapters.len() <= MAX_CHAPTERS => {}
        _ => return Err(bad("chaptersが不正です")),
    }
    let mut clean = object.clone();
    set_text(&mut clean, "title", 200, "");
    set_text(&mut clean, "subtitle", 300, "");
    set_text(&mut clean, "summary", 4000, "");
    let mut total_images = 0usize;
    let Some(Value::Array(chapters)) = clean.get_mut("chapters") else {
        return Err(bad("chaptersが不正です"));
    };
    for chapter in chapters {
        let Some(chapter) = chapter.as_object_mut() else {
            return Err(bad("chapterの形式が不正です"));
        };
        if !chapter.get("images").is_some_and(Value::is_array) {
            return Err(bad("chapterの形式が不正です"));
        }
        set_text(chapter, "id", 100, "");
        set_text(chapter, "date", 10, "");
        set_text(chapter, "title", 200, "");
        set_text(chapter, "location_label", 300, "");
        set_text(chapter, "body", 20000, "");
        let Some(Value::Array(images)) = chapter.get_mut("images") else {
            return Err(bad("chapterの形式が不正です"));
        };
        if images.len() > MAX_IMAGES_PER_CHAPTER {
            return Err(bad("1章あたりの画像が多すぎます"));
        }
        total_images += images.len();
        if total_images > MAX_TOTAL_EXPORT_IMAGES {
            return Err(BlogError::BadRequest(format!(
                "画像の合計が多すぎます（上限{MAX_TOTAL_EXPORT_IMAGES}枚）"
            )));
        }
        for image in images {
            let Some(image) = image.as_object_mut() else {
                return Err(bad("画像のphoto_idが不正です"));
            };
            if !is_py_digit_str(&py_str_or(image.get("photo_id"), "")) {
                return Err(bad("画像のphoto_idが不正です"));
            }
            let photo_id = image.get("photo_id").map(py_str).unwrap_or_default();
            image.insert("photo_id".into(), Value::String(photo_id));
            set_text(image, "alt", 500, "旅の写真");
            set_text(image, "caption", 2000, "");
        }
    }
    Ok(Value::Object(clean))
}

/// `obj[key] = str(obj.get(key) or default)[:max]`。
fn set_text(obj: &mut Map<String, Value>, key: &str, max: usize, default: &str) {
    let text = prefix_chars(&py_str_or(obj.get(key), default), max);
    obj.insert(key.to_string(), Value::String(text));
}

// ---- 下書き ---------------------------------------------------------------------------------

/// 保存済みの下書きを読んで検査する。無ければ `Ok(None)`。`exists()` が真で読めなければ 400。
/// 保存済みの下書きは旅程が（もう）無くても返す（Python は `load_plan` を呼ばない）。
pub fn saved_draft(env: &BlogEnv, trip_id: &str) -> Result<Option<Value>, BlogError> {
    if !is_valid_trip_id(trip_id) {
        return Err(bad(MSG_TRIP_ID));
    }
    let path = env.draft_path(trip_id);
    if !path.exists() {
        return Ok(None);
    }
    let Some(value) = read_json_file(&path) else {
        return Err(BlogError::BadRequest(format!(
            "JSONを読み込めません: {trip_id}.json"
        )));
    };
    validate_manifest(&value, Some(trip_id)).map(Some)
}

// ---- 呼び出し口（非同期。server.rs が使う） --------------------------------------------------

fn unavailable(e: &CatalogError) -> BlogError {
    BlogError::Unavailable(e.to_string())
}

/// ブロッキングの処理を専用のスレッドで行う。
async fn blocking<T, F>(f: F) -> Result<T, BlogError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, BlogError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| BlogError::Internal)?
}

pub async fn trips(env: Arc<BlogEnv>) -> Result<Value, BlogError> {
    blocking(move || Ok(api_trips(&env))).await
}

pub async fn trip(env: Arc<BlogEnv>, trip_id: String) -> Result<Value, BlogError> {
    blocking(move || api_trip(&env, &trip_id)).await
}

pub async fn candidates(
    env: Arc<BlogEnv>,
    catalog: &Catalog,
    trip_id: String,
) -> Result<Value, BlogError> {
    let plan = blocking(move || load_plan(&env, &trip_id)).await?;
    catalog
        .read(move |conn| candidates_json(conn, &plan))
        .await
        .map_err(|e| unavailable(&e))?
}

pub async fn draft(
    env: Arc<BlogEnv>,
    catalog: &Catalog,
    trip_id: String,
) -> Result<Value, BlogError> {
    let (env2, id2) = (Arc::clone(&env), trip_id.clone());
    if let Some(manifest) = blocking(move || saved_draft(&env2, &id2)).await? {
        return Ok(json!({"ok": true, "source": "saved", "manifest": manifest}));
    }
    let plan = blocking(move || load_plan(&env, &trip_id)).await?;
    let manifest = catalog
        .read(move |conn| manifest_json(conn, &plan))
        .await
        .map_err(|e| unavailable(&e))??;
    Ok(json!({"ok": true, "source": "generated", "manifest": manifest}))
}
