//! 管理画面の「選別」タブの API（docs/admin-library/design.md の PR ③）。
//!
//! - `GET /api/admin/curation/groups`: 品質解析のグループ（世代の合う `photo_quality` の行が 2 枚以上）を、状態・種類で
//!   絞って 1 組ずつ返す。
//! - `POST /api/admin/curation/confirm`: グループの代表を人が決める（`representatives: [id…]`）か、解析の推奨のまま
//!   確認済みにする（`representatives: null`）。
//! - `POST /api/admin/curation/reset`: グループの人の記録（stale も）を消す。
//!
//! 読み取りは 1 回の読み取りトランザクションの中で、グループの一覧（ページ）→ メンバーの順に読む。書き込みは
//! `BEGIN IMMEDIATE` の 1 トランザクションで、`(kind, group_id)` のメンバーを読み直して `token`（[`group_token`]）が
//! 画面の見たものと一致するときだけ書く。違えば何も書かずに 409 `group_changed`。判定の順序は `update::set_flag` と同じ
//! （本文の形 400 → writer が無ければ 503 → トランザクションの中で 409 → 400）。
//!
//! 表が無い DB（`INGEST=0` の古い DB）でも `groups` は 200（全員 `curation: null`、状態は `unconfirmed`）。エラーの本文は
//! 固定の符号だけで、ID・SQL・パスを含めない。

use std::sync::Arc;

use axum::response::Response;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, TransactionBehavior, params, params_from_iter};
use serde_json::{Map, Value, json};

use crate::admin::{AdminContext, cell, text, thumb_for, truthy};
use crate::admin_library::{bad_params, parse_ids, query_params, respond};
use crate::curation::{
    self, Member, Record, State, group_state, group_token, member_record, members_from,
};
use crate::photos::FEED_BASE_WHERE;
use crate::post::ApiError;
use crate::pyquery;
use crate::server::AppState;
use crate::update::writer_or_unavailable;
use crate::writer::{WriteError, Writer};

/// `per` の既定と上限。
const PER_DEFAULT: i64 = 20;
const PER_MAX: i64 = 50;

/// 応答・エラーの符号（固定。値は入れない）。
const BAD_PARAMS: &str = "bad_params";
const GROUP_CHANGED: &str = "group_changed";
const NOT_VISIBLE: &str = "representative_not_visible";

// ---------------------------------------------------------------- 1 組を読む

/// 読み出した 1 組。`json` は応答の 1 組、`members` と `selectable` は `confirm` の検査に使う。
struct Group {
    members: Vec<Member>,
    selectable: Vec<bool>,
    json: Value,
}

/// `(kind, group_id)` のメンバーを読んで 1 組にする。世代の合うメンバーが 2 枚に満たなければ `None`
/// （組が消えた・1 枚になった）。`curation` は表の有無（[`curation::available`]）。
fn load_group(
    conn: &Connection,
    ctx: &AdminContext,
    curation: bool,
    kind: &str,
    group_id: i64,
) -> rusqlite::Result<Option<Group>> {
    // 品質以外の述語（フィードに出せるか）は `list_photos` の WHERE と同じ定数。`lon` が NULL の行はフィードが
    // Rust 側で落とすので、ここでも落とす
    let mut stmt = conn.prepare(&format!(
        "SELECT q.photo_id, q.display, q.source_md5, q.source_mtime, q.content_sha256, q.recommended,
                {record}, q.rank_in_group, q.score, q.reasons_json, q.issues_json,
                p.name, p.drive_id, p.md5, p.mtime, p.taken_at, p.place, p.pref, p.city,
                p.lat, p.lon, p.favorite, p.hidden, p.rotation, p.source,
                ({FEED_BASE_WHERE} AND p.lon IS NOT NULL)
           {from}
          WHERE q.group_kind=?1 AND q.group_id=?2
          ORDER BY q.rank_in_group, q.photo_id",
        record = member_record(curation),
        from = members_from(curation),
    ))?;
    let mut rows = stmt.query(params![kind, group_id])?;
    let mut members: Vec<Member> = Vec::new();
    let mut selectable: Vec<bool> = Vec::new();
    // 順位順の行。`effective` は人の代表が決まってから付けるので、いったん JSON の地図に溜める
    let mut items: Vec<Map<String, Value>> = Vec::new();
    let mut first_taken: Option<String> = None;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let record = match row.get::<_, String>(6)?.as_str() {
            "representative" => Record::Representative,
            "reviewed" => Record::Reviewed,
            "stale" => Record::Stale,
            _ => Record::None,
        };
        let recommended = row.get::<_, i64>(5)? != 0;
        members.push(Member {
            photo_id: id,
            display: row.get(1)?,
            source_md5: row.get(2)?,
            source_mtime: row.get(3)?,
            content_sha256: row.get(4)?,
            recommended,
            record,
        });
        selectable.push(truthy(row.get_ref(25)?));
        let display: Option<String> = row.get(1)?;
        let md5: Option<String> = row.get(13)?;
        let thumb = thumb_for(
            ctx,
            display.as_deref(),
            row.get::<_, Option<String>>(12)?.as_deref(),
        );
        let thumb_url = if thumb.is_empty() {
            String::new()
        } else {
            crate::thumb::thumb_url(id, display.as_deref(), md5.as_deref(), row.get(14)?)
        };
        let taken_at = text(row, 15)?;
        if !taken_at.is_empty() && first_taken.as_ref().is_none_or(|t| taken_at < *t) {
            first_taken = Some(taken_at.clone());
        }
        let rotation = match row.get_ref(23)? {
            value if !truthy(value) => Value::from(0),
            value => cell(value)?,
        };
        let json_list = |index: usize| -> rusqlite::Result<Value> {
            let raw: Option<String> = row.get(index)?;
            Ok(raw
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .filter(Value::is_array)
                .unwrap_or_else(|| json!([])))
        };
        let mut item = Map::new();
        item.insert("id".into(), id.into());
        item.insert("name".into(), cell(row.get_ref(11)?)?);
        item.insert("thumb".into(), thumb.into());
        item.insert("thumb_url".into(), thumb_url.into());
        item.insert("taken_at".into(), taken_at.into());
        item.insert("place".into(), text(row, 16)?.into());
        item.insert("pref".into(), text(row, 17)?.into());
        item.insert("city".into(), text(row, 18)?.into());
        item.insert("lat".into(), cell(row.get_ref(19)?)?);
        item.insert("lon".into(), cell(row.get_ref(20)?)?);
        item.insert("favorite".into(), truthy(row.get_ref(21)?).into());
        item.insert("hidden".into(), truthy(row.get_ref(22)?).into());
        item.insert("rotation".into(), rotation);
        item.insert("source".into(), cell(row.get_ref(24)?)?);
        item.insert("rank".into(), cell(row.get_ref(7)?)?);
        item.insert("score".into(), cell(row.get_ref(8)?)?);
        item.insert("reasons".into(), json_list(9)?);
        item.insert("issues".into(), json_list(10)?);
        item.insert("recommended".into(), recommended.into());
        item.insert(
            "curation".into(),
            match record {
                Record::None => Value::Null,
                other => other.as_str().into(),
            },
        );
        item.insert("selectable".into(), (*selectable.last().unwrap()).into());
        items.push(item);
    }
    if members.len() < 2 {
        return Ok(None);
    }
    // 有効な推奨: 有効な人の代表があればそれだけ、無ければ解析の推奨（SQL の `effective_recommended` と同じ）
    let has_pick = members.iter().any(|m| m.record == Record::Representative);
    let mut visible_effective = false;
    for ((item, member), selectable) in items.iter_mut().zip(&members).zip(&selectable) {
        let effective = if has_pick {
            member.record == Record::Representative
        } else {
            member.recommended
        };
        visible_effective |= effective && *selectable;
        item.insert("effective".into(), effective.into());
    }
    let records: Vec<Record> = members.iter().map(|m| m.record).collect();
    let state: State = group_state(&records);
    let json = json!({
        "kind": kind,
        "group_id": group_id,
        "token": group_token(&members),
        "state": state.as_str(),
        "taken_at": first_taken.unwrap_or_default(),
        "count": members.len(),
        // 有効な推奨の写真がすべてフィードに出せない（除外か位置なし）。exact の組ならフィードから消えている
        "warning": if visible_effective { Value::Null } else { Value::from(NOT_VISIBLE) },
        "members": items.into_iter().map(Value::Object).collect::<Vec<_>>(),
    });
    Ok(Some(Group {
        members,
        selectable,
        json,
    }))
}

// ---------------------------------------------------------------- GET groups

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateFilter {
    Unconfirmed,
    Confirmed,
    All,
}

/// `groups` の絞り込み（固定の候補とバインドだけ）。
struct Query {
    state: StateFilter,
    kind: Option<&'static str>,
    ids: Option<Vec<i64>>,
    page: i64,
    per: i64,
}

fn parse_query(query: Option<&str>) -> Option<Query> {
    let params = query_params(query);
    let state = match params.get("state").map(String::as_str) {
        None | Some("unconfirmed") => StateFilter::Unconfirmed,
        Some("confirmed") => StateFilter::Confirmed,
        Some("all") => StateFilter::All,
        Some(_) => return None,
    };
    let kind = match params.get("kind").map(String::as_str) {
        None | Some("all") => None,
        Some("exact") => Some("exact"),
        Some("similar") => Some("similar"),
        Some(_) => return None,
    };
    let ids = match params.get("ids") {
        None => None,
        Some(v) => Some(parse_ids(v)?),
    };
    let int = |name: &str, default: i64| match params.get(name) {
        None => Some(default),
        Some(v) => pyquery::py_int(v),
    };
    Some(Query {
        state,
        kind,
        ids,
        page: int("page", 1)?,
        per: int("per", PER_DEFAULT)?,
    })
}

/// `GET /api/admin/curation/groups`。
pub async fn groups(state: &AppState, query: Option<&str>) -> Response {
    let Some(query) = parse_query(query) else {
        return bad_params();
    };
    let ctx = Arc::clone(&state.admin);
    respond(
        state
            .catalog
            .read(move |conn| build_groups(conn, &ctx, &query))
            .await,
    )
}

fn build_groups(conn: &Connection, ctx: &AdminContext, query: &Query) -> rusqlite::Result<Value> {
    // 件数・ページ・メンバーが同じ時点の DB から出るように、1 つの読み取りトランザクションにまとめる
    let _snapshot = conn.unchecked_transaction()?;
    let curation = curation::available(conn)?;
    let mut conditions = String::new();
    let mut bound: Vec<Sql> = Vec::new();
    if let Some(kind) = query.kind {
        conditions.push_str(" AND q.group_kind=?");
        bound.push(Sql::Text(kind.to_owned()));
    }
    let mut having = String::new();
    match query.state {
        StateFilter::Unconfirmed => {
            having.push_str(&format!(" AND {}", curation::not_confirmed(curation)));
        }
        StateFilter::Confirmed => {
            having.push_str(&format!(" AND {}", curation::all_confirmed(curation)));
        }
        StateFilter::All => {}
    }
    if let Some(ids) = &query.ids {
        // 個数ぶんの `?` だけを足す（ユーザーの文字列は SQL に入れない）
        let marks = vec!["?"; ids.len()].join(",");
        having.push_str(&format!(" AND SUM(q.photo_id IN ({marks}))>0"));
        bound.extend(ids.iter().copied().map(Sql::Integer));
    }
    let from = members_from(curation);
    let grouped = format!(
        "{from} WHERE 1=1{conditions}
          GROUP BY q.group_kind, q.group_id
         HAVING COUNT(*)>=2{having}"
    );
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM (SELECT 1 {grouped})"),
        params_from_iter(bound.iter()),
        |row| row.get(0),
    )?;
    let per = query.per.clamp(1, PER_MAX);
    let pages = ((total + per - 1) / per).max(1);
    let page = query.page.clamp(1, pages);
    let mut paged = bound.clone();
    paged.push(Sql::Integer(per));
    paged.push(Sql::Integer((page - 1) * per));
    let keys: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT q.group_kind, q.group_id {grouped}
          ORDER BY COALESCE(MIN(NULLIF(p.taken_at,'')),'') DESC, q.group_kind, q.group_id
          LIMIT ? OFFSET ?"
        ))?;
        stmt.query_map(params_from_iter(paged.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let mut groups = Vec::with_capacity(keys.len());
    for (kind, group_id) in keys {
        if let Some(group) = load_group(conn, ctx, curation, &kind, group_id)? {
            groups.push(group.json);
        }
    }
    Ok(json!({
        "ok": true,
        "page": page,
        "per": per,
        "pages": pages,
        "total": total,
        "groups": groups,
    }))
}

// ---------------------------------------------------------------- POST confirm・reset

/// 本文の `kind`・`group_id`・`token`。
struct Target {
    kind: &'static str,
    group_id: i64,
    token: String,
}

fn bad_request() -> ApiError {
    ApiError::BadRequest(BAD_PARAMS.into())
}

fn parse_target(payload: &Map<String, Value>) -> Result<Target, ApiError> {
    let kind = match payload.get("kind").and_then(Value::as_str) {
        Some("exact") => "exact",
        Some("similar") => "similar",
        _ => return Err(bad_request()),
    };
    let group_id = payload
        .get("group_id")
        .and_then(Value::as_i64)
        .filter(|id| *id >= 1)
        .ok_or_else(bad_request)?;
    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| t.len() == 16 && t.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
        .ok_or_else(bad_request)?
        .to_owned();
    Ok(Target {
        kind,
        group_id,
        token,
    })
}

/// `representatives`。キーが無ければ 400（`null` は明示したときだけ）。`null` は人の代表を持たず確認済み。
/// 配列は 1 件以上、各要素は 1 以上の整数（`as_i64` で取れるもの。`1.0`・負数・0・真偽値は 400）、重複なし。
fn parse_representatives(payload: &Map<String, Value>) -> Result<Option<Vec<i64>>, ApiError> {
    match payload.get("representatives") {
        None => Err(bad_request()),
        Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return Err(bad_request());
            }
            let mut ids: Vec<i64> = Vec::with_capacity(items.len());
            let mut seen = std::collections::HashSet::with_capacity(items.len());
            for item in items {
                let id = item
                    .as_i64()
                    .filter(|id| *id >= 1)
                    .ok_or_else(bad_request)?;
                if !seen.insert(id) {
                    return Err(bad_request());
                }
                ids.push(id);
            }
            Ok(Some(ids))
        }
        Some(_) => Err(bad_request()),
    }
}

/// writer の失敗を `ApiError` にする。待ち切れなかった BUSY は 503（500 にしない）。
fn write_error(e: WriteError) -> ApiError {
    if let WriteError::Query(rusqlite::Error::SqliteFailure(failure, _)) = &e
        && matches!(
            failure.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return ApiError::Unavailable;
    }
    e.into()
}

/// トランザクションの中の結果（`Err` は SQL の失敗だけ。入力の違反はここで区別して、外で `ApiError` にする）。
enum Outcome {
    /// 組が消えた・組み替わった・メンバーの内容が変わった（409）。
    Changed,
    /// `representatives` がメンバーの部分集合でない（400）。
    NotSubset,
    /// 代表がフィードに出せない写真を含む（400）。
    NotVisible,
    /// `photo_curation` 表が無い（書き込み側の起動時に作るので、通常は起きない）。何も書かずに 503。
    NoTable,
    Done(Value),
}

fn outcome(result: Result<Outcome, WriteError>) -> Result<Value, ApiError> {
    match result.map_err(write_error)? {
        Outcome::NoTable => Err(ApiError::Unavailable),
        Outcome::Changed => Err(ApiError::Busy(GROUP_CHANGED.into())),
        Outcome::NotSubset => Err(bad_request()),
        Outcome::NotVisible => Err(ApiError::BadRequest(NOT_VISIBLE.into())),
        Outcome::Done(group) => Ok(json!({"ok": true, "group": group})),
    }
}

/// 組を読み、2 枚以上あって `token` が一致するときだけ `Some`。
fn checked_group(
    conn: &Connection,
    ctx: &AdminContext,
    target: &Target,
) -> rusqlite::Result<Option<Group>> {
    Ok(load_group(conn, ctx, true, target.kind, target.group_id)?
        .filter(|g| g.json["token"] == target.token.as_str()))
}

/// `POST /api/admin/curation/confirm`。
pub fn confirm(
    payload: &Map<String, Value>,
    writer: Option<&Writer>,
    ctx: Arc<AdminContext>,
) -> Result<Value, ApiError> {
    let target = parse_target(payload)?;
    let representatives = parse_representatives(payload)?;
    let writer = writer_or_unavailable(writer)?;
    outcome(writer.call(move |conn| {
        // `Writer::call` はトランザクションを張らない。`quality-scan` の再構築（別の接続の `BEGIN IMMEDIATE`）と
        // 競合しても、読んでから書く途中で `SQLITE_BUSY_SNAPSHOT` にならないよう、最初から書き込みの順番を取る
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !curation::available(&tx)? {
            return Ok(Outcome::NoTable);
        }
        let Some(group) = checked_group(&tx, &ctx, &target)? else {
            return Ok(Outcome::Changed);
        };
        if let Some(ids) = &representatives {
            let member_ids: std::collections::HashSet<i64> =
                group.members.iter().map(|m| m.photo_id).collect();
            if !ids.iter().all(|id| member_ids.contains(id)) {
                return Ok(Outcome::NotSubset);
            }
            let visible: std::collections::HashSet<i64> = group
                .members
                .iter()
                .zip(&group.selectable)
                .filter(|(_, ok)| **ok)
                .map(|(m, _)| m.photo_id)
                .collect();
            if !ids.iter().all(|id| visible.contains(id)) {
                return Ok(Outcome::NotVisible);
            }
        }
        delete_records(&tx, &group)?;
        {
            // 世代の 4 列は `photo_quality` の行の値（読んだ同じトランザクションの中）
            let mut insert = tx.prepare(
                "INSERT INTO photo_curation(photo_id, display, source_md5, source_mtime, content_sha256, role, decided_at)
                 SELECT photo_id, display, source_md5, source_mtime, content_sha256, ?1,
                        strftime('%Y-%m-%d %H:%M:%S','now')
                   FROM photo_quality WHERE photo_id=?2",
            )?;
            let chosen: std::collections::HashSet<i64> = representatives
                .iter()
                .flatten()
                .copied()
                .collect();
            for member in &group.members {
                let representative = chosen.contains(&member.photo_id);
                let role = if representative {
                    "representative"
                } else {
                    "reviewed"
                };
                insert.execute(params![role, member.photo_id])?;
            }
        }
        let Some(after) = load_group(&tx, &ctx, true, target.kind, target.group_id)? else {
            return Ok(Outcome::Changed);
        };
        tx.commit()?;
        Ok(Outcome::Done(after.json))
    }))
}

/// `POST /api/admin/curation/reset`。メンバーの人の記録（世代が合わないものも）を消すだけ。
pub fn reset(
    payload: &Map<String, Value>,
    writer: Option<&Writer>,
    ctx: Arc<AdminContext>,
) -> Result<Value, ApiError> {
    let target = parse_target(payload)?;
    let writer = writer_or_unavailable(writer)?;
    outcome(writer.call(move |conn| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !curation::available(&tx)? {
            return Ok(Outcome::NoTable);
        }
        let Some(group) = checked_group(&tx, &ctx, &target)? else {
            return Ok(Outcome::Changed);
        };
        delete_records(&tx, &group)?;
        let Some(after) = load_group(&tx, &ctx, true, target.kind, target.group_id)? else {
            return Ok(Outcome::Changed);
        };
        tx.commit()?;
        Ok(Outcome::Done(after.json))
    }))
}

fn delete_records(conn: &Connection, group: &Group) -> rusqlite::Result<()> {
    let mut delete = conn.prepare("DELETE FROM photo_curation WHERE photo_id=?1")?;
    for member in &group.members {
        delete.execute(params![member.photo_id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn the_target_needs_a_kind_a_positive_group_id_and_a_16_hex_token() {
        let ok = payload(json!({"kind": "exact", "group_id": 3, "token": "0123456789abcdef"}));
        let target = parse_target(&ok).unwrap();
        assert_eq!((target.kind, target.group_id), ("exact", 3));
        for bad in [
            json!({"kind": "other", "group_id": 3, "token": "0123456789abcdef"}),
            json!({"group_id": 3, "token": "0123456789abcdef"}),
            json!({"kind": "exact", "group_id": 0, "token": "0123456789abcdef"}),
            json!({"kind": "exact", "group_id": -1, "token": "0123456789abcdef"}),
            json!({"kind": "exact", "group_id": 1.0, "token": "0123456789abcdef"}),
            json!({"kind": "exact", "group_id": true, "token": "0123456789abcdef"}),
            json!({"kind": "exact", "group_id": u64::MAX, "token": "0123456789abcdef"}),
            json!({"kind": "exact", "group_id": 3, "token": "0123456789ABCDEF"}),
            json!({"kind": "exact", "group_id": 3, "token": "0123456789abcde"}),
            json!({"kind": "exact", "group_id": 3, "token": "0123456789abcdeff"}),
            json!({"kind": "exact", "group_id": 3, "token": 1234567890123456_u64}),
            json!({"kind": "exact", "group_id": 3}),
        ] {
            assert!(
                matches!(parse_target(&payload(bad.clone())), Err(ApiError::BadRequest(m)) if m == BAD_PARAMS),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_representatives_key_is_required_and_null_is_explicit() {
        let parse = |v: Value| parse_representatives(&payload(v));
        assert_eq!(
            parse(json!({"representatives": [1, 2]})).unwrap(),
            Some(vec![1, 2])
        );
        assert_eq!(parse(json!({"representatives": null})).unwrap(), None);
        for bad in [
            json!({}),
            json!({"representatives": []}),
            json!({"representatives": [1, 1]}),
            json!({"representatives": [1.0]}),
            json!({"representatives": [-1]}),
            json!({"representatives": [0]}),
            json!({"representatives": [true]}),
            json!({"representatives": ["1"]}),
            json!({"representatives": [u64::MAX]}),
            json!({"representatives": 1}),
            json!({"representatives": "1"}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_busy_database_is_a_503_and_other_failures_are_500() {
        let busy = |code| {
            WriteError::Query(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ))
        };
        assert!(matches!(
            write_error(busy(rusqlite::ffi::SQLITE_BUSY)),
            ApiError::Unavailable
        ));
        assert!(matches!(
            write_error(busy(rusqlite::ffi::SQLITE_LOCKED)),
            ApiError::Unavailable
        ));
        assert!(matches!(
            write_error(busy(rusqlite::ffi::SQLITE_CONSTRAINT)),
            ApiError::Internal(_)
        ));
    }

    #[test]
    fn the_query_takes_known_values_only() {
        let q = |s: &str| parse_query(Some(s));
        assert!(q("").is_some());
        assert!(q("state=all&kind=exact&page=2&per=5&ids=1,2").is_some());
        for bad in ["state=x", "kind=x", "ids=1,a", "ids=1,", "page=x", "per=x"] {
            assert!(q(bad).is_none(), "{bad}");
        }
        let many = vec!["1"; 201].join(",");
        assert!(q(&format!("ids={many}")).is_none());
    }
}
