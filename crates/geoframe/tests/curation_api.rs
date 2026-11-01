//! 選別の確認の管理 API（`GET /api/admin/curation/groups`・`POST /api/admin/curation/{confirm,reset}`。
//! docs/admin-library/design.md の PR ③。AC4・AC5（API 分）・AC7（curation 分）・AC9）の結合試験。
//!
//! 合成データは `tests/fixtures/curation-catalog.sql`（冒頭のコメントに構成を書いてある）:
//!
//! - exact グループ 1: 写真 1（推奨）・2・3（除外）。内容の SHA は 3 枚とも同じ。撮影 2025-07-01 09:00〜。
//! - similar グループ 2: 写真 4（推奨）・5（推奨）・6（位置なし）。撮影 2025-07-02 10:00〜。
//! - similar グループ 3: 写真 7（推奨）・8・9（推奨）。7 と 8 は同じ内容の SHA。撮影 2025-07-03 11:00〜。
//! - グループ外: 写真 10（普通）・11（位置なし）・12（除外）。
//!
//! 期待値はこのコメントと行から数えた値で、実装の出力を写していない。

mod common;

use std::time::Duration;

use common::{
    TempDir, build_curation_catalog, build_empty_catalog, curation_catalog_sql, get, post_raw,
    request, spawn_with_env,
};
use geoframe::db::Catalog;
use geoframe::writer::{OpenMode, Writer};
use rusqlite::Connection;
use serde_json::{Value, json};

const JSON_UTF8: &str = "application/json; charset=utf-8";

/// `photo_curation` の 1 行（`decided_at` は形だけ別に検査するので除く）。
type Row = (i64, String, String, Option<String>, Option<f64>, String);

struct Server {
    _dir: TempDir,
    /// 試験が DB を直接いじる・見るための接続（書き込み用）。
    db: Connection,
    addr: std::net::SocketAddr,
}

impl Server {
    /// 表あり・writer ありの普通の起動。
    async fn start() -> Self {
        Self::start_with(true, true, "").await
    }

    async fn start_with(table: bool, writer: bool, then: &str) -> Self {
        let dir = TempDir::new("curation-api");
        let (path, db) = if table {
            build_curation_catalog(dir.path(), then)
        } else {
            build_empty_catalog(dir.path(), &format!("{}\n{then}", curation_catalog_sql()))
        };
        let mut catalog = Catalog::open(&path).unwrap();
        if writer {
            catalog = catalog.with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        }
        let addr = spawn_with_env(dir.path(), &[], catalog).await;
        Self {
            _dir: dir,
            db,
            addr,
        }
    }

    async fn groups(&self, query: &str) -> Value {
        let target = format!("/api/admin/curation/groups?{query}");
        let response = get(self.addr, &target).await;
        assert_eq!(response.status, 200, "{target}: {}", response.text());
        assert!(
            response
                .headers
                .contains(&format!("content-type: {JSON_UTF8}")),
            "{target}"
        );
        assert!(
            response.headers.contains("cache-control: no-store"),
            "{target}"
        );
        let body = response.json();
        assert_eq!(body["ok"], true);
        body
    }

    /// グループ ID の 1 組を読む（状態は問わない）。`ids=` は写真 ID なので、その組のメンバーの写真で引く。
    /// 組の ID は 1（exact。写真 1〜3）・2（写真 4〜6）・3（写真 7〜9）と、振り直したあとの 17・27・37。
    async fn group(&self, group_id: i64) -> Value {
        let base = if group_id >= 10 {
            (group_id - 7) / 10
        } else {
            group_id
        };
        let id = [1, 4, 7][(base - 1) as usize];
        let body = self.groups(&format!("state=all&ids={id}")).await;
        assert_eq!(
            body["groups"].as_array().unwrap().len(),
            1,
            "group_id={group_id}"
        );
        body["groups"][0].clone()
    }

    async fn token(&self, id: i64) -> String {
        self.group(id).await["token"].as_str().unwrap().to_owned()
    }

    /// CSRF の条件を満たして POST する。
    async fn post(&self, route: &str, body: &Value) -> common::Response {
        self.post_text(route, &body.to_string()).await
    }

    async fn post_text(&self, route: &str, body: &str) -> common::Response {
        post_raw(
            self.addr,
            &format!("/api/admin/curation/{route}"),
            &[
                ("Host".into(), "localhost".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ],
            body.as_bytes(),
        )
        .await
    }

    async fn confirm(
        &self,
        kind: &str,
        group_id: i64,
        token: &str,
        reps: Value,
    ) -> common::Response {
        self.post(
            "confirm",
            &json!({"kind": kind, "group_id": group_id, "token": token, "representatives": reps}),
        )
        .await
    }

    async fn reset(&self, kind: &str, group_id: i64, token: &str) -> common::Response {
        self.post(
            "reset",
            &json!({"kind": kind, "group_id": group_id, "token": token}),
        )
        .await
    }

    /// `photo_curation` の全行（写真 ID 順）。
    fn table(&self) -> Vec<Row> {
        table(&self.db)
    }

    async fn feed(&self) -> Vec<(i64, bool)> {
        let response = get(self.addr, "/api/v1/photos").await;
        assert_eq!(response.status, 200);
        let mut out: Vec<(i64, bool)> = response
            .json()
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["id"].as_i64().unwrap(),
                    p["quality_recommended"].as_bool().unwrap(),
                )
            })
            .collect();
        out.sort_unstable();
        out
    }

    async fn quality_status(&self) -> Value {
        let response = get(self.addr, "/api/admin/status").await;
        assert_eq!(response.status, 200, "{}", response.text());
        response.json()["db"]["quality"].clone()
    }
}

fn table(db: &Connection) -> Vec<Row> {
    let mut stmt = db
        .prepare(
            "SELECT photo_id, role, display, source_md5, source_mtime, content_sha256
               FROM photo_curation ORDER BY photo_id",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// グループの JSON から (写真 ID, フィールド) の列を取る。
fn members(group: &Value, field: &str) -> Vec<(i64, Value)> {
    group["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["id"].as_i64().unwrap(), m[field].clone()))
        .collect()
}

fn ids_where(group: &Value, field: &str) -> Vec<i64> {
    members(group, field)
        .into_iter()
        .filter(|(_, v)| v == &json!(true))
        .map(|(id, _)| id)
        .collect()
}

fn keys(body: &Value) -> Vec<(String, i64)> {
    body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["kind"].as_str().unwrap().to_owned(),
                g["group_id"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn ids(items: &[i64]) -> Value {
    json!(items)
}

fn error_of(response: &common::Response) -> String {
    response.json()["error"].as_str().unwrap().to_owned()
}

fn recommended_ids(feed: &[(i64, bool)]) -> Vec<i64> {
    feed.iter().filter(|(_, r)| *r).map(|(id, _)| *id).collect()
}

fn feed_ids(feed: &[(i64, bool)]) -> Vec<i64> {
    feed.iter().map(|(id, _)| *id).collect()
}

/// 仕様（design.md の `token`）から独立に求めた印。メンバーを写真 ID 昇順に、1 枚を
/// `<id>\t<display>\t<md5>\t<mtime>\t<sha>\t<推奨 0/1>\t<記録>` にして改行でつなぎ、SHA-256 の先頭 16 hex。
fn expected_token(rows: &[(i64, &str, &str, &str, &str, u8, &str)]) -> String {
    let text = rows
        .iter()
        .map(|(id, display, md5, mtime, sha, rec, record)| {
            format!("{id}\t{display}\t{md5}\t{mtime}\t{sha}\t{rec}\t{record}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------- GET groups

#[tokio::test]
async fn groups_lists_the_three_groups_newest_first_with_their_members() {
    let server = Server::start().await;
    let body = server.groups("").await;
    assert_eq!(
        (&body["page"], &body["per"], &body["pages"], &body["total"]),
        (&json!(1), &json!(20), &json!(1), &json!(3))
    );
    // 並びは撮影日時（グループの最小）の降順
    assert_eq!(
        keys(&body),
        [
            ("similar".to_owned(), 3),
            ("similar".to_owned(), 2),
            ("exact".to_owned(), 1)
        ]
    );
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|g| g["taken_at"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["2025-07-03 11:00", "2025-07-02 10:00", "2025-07-01 09:00"]
    );
    assert_eq!(
        groups
            .iter()
            .map(|g| g["count"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [3, 3, 3]
    );
    for g in groups {
        assert_eq!(g["state"], "unconfirmed");
        assert_eq!(g["warning"], Value::Null);
        let token = g["token"].as_str().unwrap();
        assert_eq!(token.len(), 16);
        assert!(
            token
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        );
        for m in g["members"].as_array().unwrap() {
            assert_eq!(m["curation"], Value::Null);
            assert!(m["reasons"].is_array() && m["issues"].is_array());
        }
    }
    // メンバーは順位順。点数・推奨・有効な推奨（人の記録が無ければ解析の推奨）・選べるか
    let exact = &groups[2];
    assert_eq!(
        members(exact, "id")
            .into_iter()
            .map(|(_, v)| v)
            .collect::<Vec<_>>(),
        [json!(1), json!(2), json!(3)]
    );
    assert_eq!(
        members(exact, "rank")
            .into_iter()
            .map(|(_, v)| v)
            .collect::<Vec<_>>(),
        [json!(1), json!(2), json!(3)]
    );
    assert_eq!(
        members(exact, "score")
            .into_iter()
            .map(|(_, v)| v)
            .collect::<Vec<_>>(),
        [json!(87.5), json!(84.5), json!(81.5)]
    );
    assert_eq!(ids_where(exact, "recommended"), [1]);
    assert_eq!(ids_where(exact, "effective"), [1]);
    // 写真 3 は除外なので代表に選べない
    assert_eq!(ids_where(exact, "selectable"), [1, 2]);
    assert_eq!(ids_where(exact, "hidden"), [3]);
    let m1 = &exact["members"][0];
    assert_eq!(m1["name"], "IMG_C01.jpg");
    assert_eq!(m1["thumb"], "/derivatives/c01.jpg");
    assert!(
        m1["thumb_url"]
            .as_str()
            .unwrap()
            .starts_with("/thumb/1.jpg?v=")
    );
    assert_eq!(m1["taken_at"], "2025-07-01 09:00");
    assert_eq!((&m1["lat"], &m1["lon"]), (&json!(35.1), &json!(139.1)));
    assert_eq!(
        (&m1["place"], &m1["pref"], &m1["city"]),
        (&json!("試験区"), &json!("架空県"), &json!("試験市"))
    );
    assert_eq!(
        (
            &m1["favorite"],
            &m1["hidden"],
            &m1["rotation"],
            &m1["source"]
        ),
        (&json!(false), &json!(false), &json!(0), &json!("local"))
    );
    // 写真 6 は位置なし、写真 9 は推奨
    let similar2 = &groups[1];
    assert_eq!(ids_where(similar2, "recommended"), [4, 5]);
    assert_eq!(ids_where(similar2, "selectable"), [4, 5]);
    assert_eq!(members(similar2, "lat")[2], (6, Value::Null));
    assert_eq!(ids_where(&groups[0], "recommended"), [7, 9]);
    assert_eq!(ids_where(&groups[0], "selectable"), [7, 8, 9]);
}

#[tokio::test]
async fn every_member_carries_loc_source_in_groups_confirm_and_reset() {
    let server = Server::start().await;
    // 写真 1 は EXIF、写真 2 は手動、写真 3 は NULL（空文字で返る）
    server
        .db
        .execute_batch(
            "UPDATE photos SET loc_source='exif' WHERE id=1;
             UPDATE photos SET loc_source='manual' WHERE id=2;
             UPDATE photos SET loc_source=NULL WHERE id=3;",
        )
        .unwrap();
    let expected = [(1, json!("exif")), (2, json!("manual")), (3, json!(""))];
    let exact = server.group(1).await;
    assert_eq!(members(&exact, "loc_source"), expected);
    // メンバーの鍵の集合（画面が読む形）
    let keys: std::collections::BTreeSet<&str> = exact["members"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys.into_iter().collect::<Vec<_>>(),
        [
            "city",
            "curation",
            "effective",
            "favorite",
            "hidden",
            "id",
            "issues",
            "lat",
            "loc_source",
            "lon",
            "name",
            "place",
            "pref",
            "rank",
            "reasons",
            "recommended",
            "rotation",
            "score",
            "selectable",
            "source",
            "taken_at",
            "thumb",
            "thumb_url"
        ]
    );
    // confirm・reset の応答の組も同じ列を持つ
    let token = exact["token"].as_str().unwrap();
    let confirmed = server.confirm("exact", 1, token, Value::Null).await;
    assert_eq!(confirmed.status, 200, "{}", confirmed.text());
    assert_eq!(members(&confirmed.json()["group"], "loc_source"), expected);
    let token = server.token(1).await;
    let reset = server.reset("exact", 1, &token).await;
    assert_eq!(reset.status, 200, "{}", reset.text());
    assert_eq!(members(&reset.json()["group"], "loc_source"), expected);
}

#[tokio::test]
async fn the_token_follows_the_documented_recipe() {
    let server = Server::start().await;
    let exact = server.group(1).await;
    assert_eq!(
        exact["token"],
        expected_token(&[
            (
                1,
                "derivatives/c01.jpg",
                "md5-c01",
                "10.0",
                "sha-e",
                1,
                "none"
            ),
            (
                2,
                "derivatives/c02.jpg",
                "md5-c02",
                "20.0",
                "sha-e",
                0,
                "none"
            ),
            (
                3,
                "derivatives/c03.jpg",
                "md5-c03",
                "30.0",
                "sha-e",
                0,
                "none"
            ),
        ])
        .as_str()
    );
    // 人の記録（代表・確認済み）も印に入る
    let token = exact["token"].as_str().unwrap();
    assert_eq!(
        server.confirm("exact", 1, token, ids(&[2])).await.status,
        200
    );
    assert_eq!(
        server.token(1).await,
        expected_token(&[
            (
                1,
                "derivatives/c01.jpg",
                "md5-c01",
                "10.0",
                "sha-e",
                1,
                "reviewed"
            ),
            (
                2,
                "derivatives/c02.jpg",
                "md5-c02",
                "20.0",
                "sha-e",
                0,
                "representative"
            ),
            (
                3,
                "derivatives/c03.jpg",
                "md5-c03",
                "30.0",
                "sha-e",
                0,
                "reviewed"
            ),
        ])
    );
}

#[tokio::test]
async fn the_filters_and_the_paging_are_per_group() {
    let server = Server::start().await;
    let exact = server.groups("kind=exact").await;
    assert_eq!(
        (keys(&exact), exact["total"].clone()),
        (vec![("exact".to_owned(), 1)], json!(1))
    );
    let similar = server.groups("kind=similar").await;
    assert_eq!(
        keys(&similar),
        [("similar".to_owned(), 3), ("similar".to_owned(), 2)]
    );
    assert_eq!(server.groups("kind=all&state=all").await["total"], 3);
    // 確認済みはまだ無い
    let confirmed = server.groups("state=confirmed").await;
    assert_eq!(
        (confirmed["total"].clone(), keys(&confirmed)),
        (json!(0), vec![])
    );
    assert_eq!(confirmed["pages"], 1);

    // ページ: 1 ページ 1 組
    let page2 = server.groups("per=1&page=2").await;
    assert_eq!(
        (
            page2["page"].clone(),
            page2["pages"].clone(),
            page2["total"].clone()
        ),
        (json!(2), json!(3), json!(3))
    );
    assert_eq!(keys(&page2), [("similar".to_owned(), 2)]);
    // 範囲外は丸める
    assert_eq!(
        keys(&server.groups("per=1&page=99").await),
        [("exact".to_owned(), 1)]
    );
    assert_eq!(server.groups("per=1&page=0").await["page"], 1);
    assert_eq!(server.groups("per=1&page=-5").await["page"], 1);
    assert_eq!(server.groups("per=0").await["per"], 1);
    assert_eq!(server.groups("per=500").await["per"], 50);
    assert_eq!(server.groups("").await["per"], 20);
}

#[tokio::test]
async fn ids_narrow_to_the_groups_of_those_photos() {
    let server = Server::start().await;
    let by = |q: &str| format!("state=all&ids={q}");
    // 写真 5 → グループ 2。位置なしの写真 6 も同じグループ
    assert_eq!(
        keys(&server.groups(&by("5")).await),
        [("similar".to_owned(), 2)]
    );
    assert_eq!(
        keys(&server.groups(&by("6")).await),
        [("similar".to_owned(), 2)]
    );
    // 2 つのグループ、同じグループの重複は 1 組
    assert_eq!(
        keys(&server.groups(&by("1,8,7")).await),
        [("similar".to_owned(), 3), ("exact".to_owned(), 1)]
    );
    // グループ外の写真・無い写真は何も返さない
    for id in ["10", "11", "999"] {
        assert_eq!(server.groups(&by(id)).await["total"], 0, "{id}");
    }
    // 絞り込みは状態・種類と掛け合わせる
    assert_eq!(server.groups("state=confirmed&ids=5").await["total"], 0);
    assert_eq!(
        server.groups("kind=exact&state=all&ids=5").await["total"],
        0
    );
    // 個数の上限は 200（重複を除く前）
    let n = |count: usize| {
        (1..=count)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    assert_eq!(server.groups(&by(&n(200))).await["total"], 3);
    for bad in [n(201), vec!["1"; 201].join(",")] {
        let response = get(
            server.addr,
            &format!("/api/admin/curation/groups?ids={bad}"),
        )
        .await;
        assert_eq!(response.status, 400);
        assert_eq!(response.json(), json!({"ok": false, "error": "bad_params"}));
    }
}

#[tokio::test]
async fn bad_queries_are_400_with_a_fixed_code() {
    let server = Server::start().await;
    for query in [
        "state=bogus",
        "kind=bogus",
        "state=Unconfirmed",
        "ids=1,a",
        "ids=1,",
        "ids=1.5",
        "page=x",
        "per=1.5",
    ] {
        let response = get(server.addr, &format!("/api/admin/curation/groups?{query}")).await;
        assert_eq!(response.status, 400, "{query}");
        assert_eq!(
            response.json(),
            json!({"ok": false, "error": "bad_params"}),
            "{query}"
        );
        assert!(response.headers.contains("cache-control: no-store"));
    }
}

#[tokio::test]
async fn the_warning_marks_a_group_whose_effective_photos_cannot_appear_in_the_feed() {
    let server = Server::start().await;
    // 推奨の写真 1 つ（写真 1）だけが除外 → 警告。写真 2 は出せるが推奨ではない
    server
        .db
        .execute_batch("UPDATE photos SET hidden=1 WHERE id=1;")
        .unwrap();
    let exact = server.group(1).await;
    assert_eq!(exact["warning"], "representative_not_visible");
    assert_eq!(ids_where(&exact, "selectable"), [2]);
    // 2 枚の推奨のうち 1 枚だけ除外 → 警告なし。2 枚とも出せなければ警告
    server
        .db
        .execute_batch("UPDATE photos SET hidden=1 WHERE id=4;")
        .unwrap();
    assert_eq!(server.group(2).await["warning"], Value::Null);
    server
        .db
        .execute_batch("UPDATE photos SET lat=NULL WHERE id=5;")
        .unwrap();
    assert_eq!(
        server.group(2).await["warning"],
        "representative_not_visible"
    );
    // 緯度があって経度が無い行も、フィードが落とすので選べない
    server
        .db
        .execute_batch("UPDATE photos SET lon=NULL WHERE id=7;")
        .unwrap();
    let similar3 = server.group(3).await;
    assert_eq!(ids_where(&similar3, "selectable"), [8, 9]);
    assert_eq!(similar3["warning"], Value::Null);
}

// ---------------------------------------------------------------- AC4: confirm・reset

#[tokio::test]
async fn confirming_a_human_representative_reaches_the_groups_the_feed_and_the_status() {
    let server = Server::start().await;
    // 前: 解析の推奨だけ
    let before = server.feed().await;
    assert_eq!(feed_ids(&before), [1, 4, 5, 7, 8, 9, 10]);
    assert_eq!(recommended_ids(&before), [1, 4, 5, 7, 9]);
    let status = server.quality_status().await;
    assert_eq!(
        (
            &status["recommended"],
            &status["exact_suppressed"],
            &status["similar_alternates"],
            &status["curation_pending"]
        ),
        (&json!(5), &json!(2), &json!(2), &json!(3))
    );

    let token = server.token(2).await;
    let response = server.confirm("similar", 2, &token, ids(&[5])).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert!(response.headers.contains("cache-control: no-store"));
    let body = response.json();
    assert_eq!(body["ok"], true);
    let group = &body["group"];
    assert_eq!(group["state"], "confirmed");
    assert_eq!(ids_where(group, "recommended"), [4, 5]);
    // 人の代表があるグループでは、有効な推奨はその写真だけ
    assert_eq!(ids_where(group, "effective"), [5]);
    assert_eq!(
        members(group, "curation"),
        [
            (4, json!("reviewed")),
            (5, json!("representative")),
            (6, json!("reviewed"))
        ]
    );
    assert_ne!(group["token"], token.as_str());
    // 次の groups も同じ
    assert_eq!(&server.group(2).await, group);

    // 表: 世代の 4 列は photo_quality の行の値。`decided_at` は UTC の `YYYY-MM-DD HH:MM:SS`
    assert_eq!(
        server.table(),
        [
            (
                4,
                "reviewed".into(),
                "derivatives/c04.jpg".into(),
                Some("md5-c04".into()),
                Some(40.0),
                "sha-04".into()
            ),
            (
                5,
                "representative".into(),
                "derivatives/c05.jpg".into(),
                Some("md5-c05".into()),
                Some(50.0),
                "sha-05".into()
            ),
            (
                6,
                "reviewed".into(),
                "derivatives/c06.jpg".into(),
                Some("md5-c06".into()),
                Some(60.0),
                "sha-06".into()
            ),
        ]
    );
    let decided: Vec<String> = server
        .db
        .prepare("SELECT decided_at FROM photo_curation")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let utc_now: String = server
        .db
        .query_row("SELECT datetime('now')", [], |row| row.get(0))
        .unwrap();
    for at in &decided {
        let b = at.as_bytes();
        assert_eq!(b.len(), 19, "{at}");
        assert!(
            b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b' ',
                13 | 16 => *c == b':',
                _ => c.is_ascii_digit(),
            }),
            "{at}"
        );
        assert_eq!(&at[..10], &utc_now[..10], "UTC の日付");
    }

    // 確認済みの絞り込みに出て、未確認からは消える
    let confirmed = server.groups("state=confirmed").await;
    assert_eq!(keys(&confirmed), [("similar".to_owned(), 2)]);
    assert_eq!(server.groups("state=unconfirmed").await["total"], 2);

    // フィード: 写真 5 だけが推奨（4 は出るが推奨でない）。similar なので出る枚数は変わらない
    let after = server.feed().await;
    assert_eq!(feed_ids(&after), feed_ids(&before));
    assert_eq!(recommended_ids(&after), [1, 5, 7, 9]);
    // 状況タブ: 推奨は 4（5 → 4）、similar の推奨外は 3（4・6・8）、未確認は 2 組
    let status = server.quality_status().await;
    assert_eq!(
        (
            &status["recommended"],
            &status["exact_suppressed"],
            &status["similar_alternates"],
            &status["curation_pending"]
        ),
        (&json!(4), &json!(2), &json!(3), &json!(2))
    );
    assert_eq!(status["recommended"], recommended_ids(&after).len());
}

#[tokio::test]
async fn in_an_exact_group_only_the_chosen_photo_is_in_the_feed() {
    let server = Server::start().await;
    let token = server.token(1).await;
    let response = server.confirm("exact", 1, &token, ids(&[2])).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json()["group"]["state"], "confirmed");
    let feed = server.feed().await;
    // 写真 1 は消え、選んだ写真 2 が出る。3 は除外
    assert_eq!(feed_ids(&feed), [2, 4, 5, 7, 8, 9, 10]);
    assert_eq!(recommended_ids(&feed), [2, 4, 5, 7, 9]);
    let status = server.quality_status().await;
    assert_eq!(status["recommended"], 5);
    assert_eq!(status["exact_suppressed"], 2);

    // 2 枚を代表にすると 2 枚とも出る
    let token = server.token(1).await;
    assert_eq!(
        server
            .confirm("exact", 1, &token, ids(&[1, 2]))
            .await
            .status,
        200
    );
    let feed = server.feed().await;
    assert_eq!(feed_ids(&feed), [1, 2, 4, 5, 7, 8, 9, 10]);
    assert_eq!(recommended_ids(&feed), [1, 2, 4, 5, 7, 9]);
    assert_eq!(server.quality_status().await["recommended"], 6);

    // 確定のあとで代表を除外にすると、フィードから消え、警告が出る（既存の穴。design.md のリスクの表）
    server
        .db
        .execute_batch("UPDATE photos SET hidden=1 WHERE id IN (1,2);")
        .unwrap();
    let feed = server.feed().await;
    assert!(feed_ids(&feed).iter().all(|id| ![1, 2, 3].contains(id)));
    assert_eq!(
        server.group(1).await["warning"],
        "representative_not_visible"
    );
}

#[tokio::test]
async fn the_choice_survives_a_rebuild_of_photo_quality_that_renumbers_the_groups() {
    let server = Server::start().await;
    let token = server.token(2).await;
    assert_eq!(
        server.confirm("similar", 2, &token, ids(&[5])).await.status,
        200
    );
    let token = server.token(1).await;
    assert_eq!(
        server.confirm("exact", 1, &token, ids(&[2])).await.status,
        200
    );
    let feed_before = server.feed().await;
    let status_before = server.quality_status().await;

    // `quality-scan` の再構築と同じ DELETE + INSERT。group_id を振り直す
    server
        .db
        .execute_batch(
            "CREATE TEMP TABLE bk AS SELECT * FROM photo_quality;
             DELETE FROM photo_quality;
             INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id,
                                       group_kind, rank_in_group, recommended, score, reasons_json, issues_json,
                                       algorithm, analyzed_at)
               SELECT photo_id, display, source_md5, source_mtime, content_sha256, group_id*10+7,
                      group_kind, rank_in_group, recommended, score, reasons_json, issues_json,
                      algorithm, analyzed_at FROM bk;
             DROP TABLE bk;",
        )
        .unwrap();

    assert_eq!(server.feed().await, feed_before);
    assert_eq!(
        server.quality_status().await["recommended"],
        status_before["recommended"]
    );
    let confirmed = server.groups("state=confirmed").await;
    assert_eq!(
        keys(&confirmed),
        [("similar".to_owned(), 27), ("exact".to_owned(), 17)]
    );
    assert_eq!(ids_where(&confirmed["groups"][0], "effective"), [5]);
    assert_eq!(ids_where(&confirmed["groups"][1], "effective"), [2]);
    // 古い group_id はもう無い。そこへの確定は組が消えたものとして 409
    let response = server.confirm("similar", 2, &token, ids(&[4])).await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "group_changed".into())
    );
    // 新しい group_id と読み直した印なら通る
    let token = server.token(27).await;
    assert_eq!(
        server
            .confirm("similar", 27, &token, ids(&[4]))
            .await
            .status,
        200
    );
    assert_eq!(ids_where(&server.group(27).await, "effective"), [4]);
}

#[tokio::test]
async fn confirming_without_representatives_keeps_the_analysis_and_a_human_choice_can_be_dropped() {
    let server = Server::start().await;
    let feed_before = server.feed().await;
    let token = server.token(3).await;
    // `reviewed` だけ（人の代表を持たず確認済み）。推奨は解析のまま
    let response = server.confirm("similar", 3, &token, Value::Null).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let group = &response.json()["group"];
    assert_eq!(group["state"], "confirmed");
    assert_eq!(ids_where(group, "effective"), [7, 9]);
    assert_eq!(
        members(group, "curation"),
        [
            (7, json!("reviewed")),
            (8, json!("reviewed")),
            (9, json!("reviewed"))
        ]
    );
    assert_eq!(server.feed().await, feed_before);
    assert_eq!(
        server
            .table()
            .iter()
            .map(|r| r.1.as_str())
            .collect::<Vec<_>>(),
        ["reviewed"; 3]
    );

    // 人の代表があるグループを `null` で確認すると、人の代表は外れて解析の推奨に戻る（画面は代表を入れて送る）
    let token = server.token(2).await;
    assert_eq!(
        server.confirm("similar", 2, &token, ids(&[5])).await.status,
        200
    );
    let token = server.token(2).await;
    let kept = server.confirm("similar", 2, &token, ids(&[5])).await;
    assert_eq!(ids_where(&kept.json()["group"], "effective"), [5]);
    let token = server.token(2).await;
    let dropped = server.confirm("similar", 2, &token, Value::Null).await;
    assert_eq!(dropped.status, 200);
    assert_eq!(ids_where(&dropped.json()["group"], "effective"), [4, 5]);
    assert_eq!(dropped.json()["group"]["state"], "confirmed");
}

#[tokio::test]
async fn reset_goes_back_to_the_analysis_and_unconfirmed_and_removes_stale_records_too() {
    let server = Server::start().await;
    let feed_before = server.feed().await;
    let status_before = server.quality_status().await;
    let token = server.token(1).await;
    assert_eq!(
        server.confirm("exact", 1, &token, ids(&[2])).await.status,
        200
    );
    assert_ne!(server.feed().await, feed_before);

    let token = server.token(1).await;
    let response = server.reset("exact", 1, &token).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let group = &response.json()["group"];
    assert_eq!(group["state"], "unconfirmed");
    assert_eq!(ids_where(group, "effective"), [1]);
    assert!(members(group, "curation").iter().all(|(_, v)| v.is_null()));
    assert_eq!(server.table(), []);
    assert_eq!(server.feed().await, feed_before);
    assert_eq!(server.quality_status().await, status_before);

    // 世代が合わない記録（stale）も消える。他のグループの記録には触れない
    server
        .db
        .execute_batch(
            "INSERT INTO photo_curation(photo_id, display, source_md5, source_mtime, content_sha256, role, decided_at)
               VALUES (4, 'derivatives/c04.jpg', 'md5-c04', 40.0, 'sha-old', 'representative', '2025-07-20 00:00:00'),
                      (7, 'derivatives/c07.jpg', 'md5-c07', 70.0, 'sha-s3dup', 'reviewed', '2025-07-20 00:00:00');",
        )
        .unwrap();
    let group = server.group(2).await;
    assert_eq!(group["state"], "recheck");
    assert_eq!(members(&group, "curation")[0], (4, json!("stale")));
    // 古い代表は有効な代表ではないので、解析の推奨のまま
    assert_eq!(ids_where(&group, "effective"), [4, 5]);
    let token = group["token"].as_str().unwrap();
    assert_eq!(server.reset("similar", 2, token).await.status, 200);
    assert_eq!(server.table().iter().map(|r| r.0).collect::<Vec<_>>(), [7]);
    assert_eq!(server.group(2).await["state"], "unconfirmed");
}

#[tokio::test]
async fn representatives_that_cannot_appear_in_the_feed_are_refused_and_nothing_is_written() {
    let server = Server::start().await;
    // 除外の写真 3・位置なしの写真 6
    for (kind, group_id, reps) in [
        ("exact", 1, vec![3]),
        ("similar", 2, vec![6]),
        ("exact", 1, vec![1, 3]),
        ("similar", 2, vec![5, 6]),
    ] {
        let token = server.token(group_id).await;
        let response = server.confirm(kind, group_id, &token, ids(&reps)).await;
        assert_eq!(response.status, 400, "{reps:?}");
        assert_eq!(
            response.json(),
            json!({"ok": false, "error": "representative_not_visible"})
        );
        assert_eq!(server.table(), []);
    }
    // 緯度があって経度が無い行
    server
        .db
        .execute_batch("UPDATE photos SET lon=NULL WHERE id=4;")
        .unwrap();
    let token = server.token(2).await;
    let response = server.confirm("similar", 2, &token, ids(&[4])).await;
    assert_eq!(
        (response.status, error_of(&response)),
        (400, "representative_not_visible".into())
    );
    // 表示用画像が無い行（photo_quality と世代が合わなくなるのでグループから外れる）はそもそも選べない
    assert_eq!(server.table(), []);
    // 除外を外す・位置を付けると選べる
    server
        .db
        .execute_batch("UPDATE photos SET hidden=0 WHERE id=3;")
        .unwrap();
    let token = server.token(1).await;
    assert_eq!(
        server.confirm("exact", 1, &token, ids(&[3])).await.status,
        200
    );
    assert_eq!(
        server
            .feed()
            .await
            .iter()
            .filter(|(id, _)| (1..=3).contains(id))
            .count(),
        1
    );
}

#[tokio::test]
async fn representatives_must_be_members_of_the_group() {
    let server = Server::start().await;
    let token = server.token(2).await;
    for reps in [vec![10], vec![1], vec![5, 7], vec![999]] {
        let response = server.confirm("similar", 2, &token, ids(&reps)).await;
        assert_eq!(response.status, 400, "{reps:?}");
        assert_eq!(response.json(), json!({"ok": false, "error": "bad_params"}));
        assert_eq!(server.table(), []);
    }
}

// ---------------------------------------------------------------- AC5

#[tokio::test]
async fn a_photo_added_to_a_confirmed_group_makes_it_recheck_and_unconfirmed_again() {
    let server = Server::start().await;
    let token = server.token(3).await;
    assert_eq!(
        server
            .confirm("similar", 3, &token, Value::Null)
            .await
            .status,
        200
    );
    assert_eq!(server.group(3).await["state"], "confirmed");
    assert_eq!(server.groups("state=unconfirmed&per=1").await["total"], 2);
    assert_eq!(
        keys(&server.groups("state=confirmed").await),
        [("similar".to_owned(), 3)]
    );

    // 再解析で 4 枚目（写真 13）が同じグループに入った
    server
        .db
        .execute_batch(
            "INSERT INTO photos(id, source, name, display, md5, mtime, taken_at, lat, lon, place, pref, city, favorite, hidden)
               VALUES (13, 'local', 'IMG_C13.jpg', 'derivatives/c13.jpg', 'md5-c13', 130.0, '2025-07-03 11:03', 35.3, 139.3, '試験区', '架空県', '試験市', 0, 0);
             INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id, group_kind, rank_in_group, recommended, score, algorithm, analyzed_at)
               VALUES (13, 'derivatives/c13.jpg', 'md5-c13', 130.0, 'sha-13', 3, 'similar', 4, 0, 60.5, 't', '2025-07-10T00:00:00');",
        )
        .unwrap();
    let group = server.group(3).await;
    assert_eq!(group["state"], "recheck");
    assert_eq!(group["count"], 4);
    assert_eq!(members(&group, "curation")[3], (13, Value::Null));
    // 未確認の絞り込みに出て、確認済みからは外れる
    let unconfirmed = server.groups("state=unconfirmed").await;
    assert_eq!(
        keys(&unconfirmed),
        [
            ("similar".to_owned(), 3),
            ("similar".to_owned(), 2),
            ("exact".to_owned(), 1)
        ]
    );
    assert_eq!(server.groups("state=confirmed").await["total"], 0);
    assert_eq!(server.quality_status().await["curation_pending"], 3);
    // 確認し直すと確認済みに戻る
    let token = group["token"].as_str().unwrap();
    let response = server.confirm("similar", 3, token, Value::Null).await;
    assert_eq!(response.json()["group"]["state"], "confirmed");
}

#[tokio::test]
async fn a_changed_generation_makes_the_record_stale_and_the_group_recheck() {
    let server = Server::start().await;
    let token = server.token(3).await;
    assert_eq!(
        server.confirm("similar", 3, &token, ids(&[7])).await.status,
        200
    );
    assert_eq!(ids_where(&server.group(3).await, "effective"), [7]);
    // 写真 7 の原本が更新され、再解析された（photos の mtime と photo_quality の世代が変わった）
    server
        .db
        .execute_batch(
            "UPDATE photos SET mtime=71.5 WHERE id=7;
             UPDATE photo_quality SET source_mtime=71.5 WHERE photo_id=7;",
        )
        .unwrap();
    let group = server.group(3).await;
    assert_eq!(group["state"], "recheck");
    assert_eq!(
        members(&group, "curation"),
        [
            (7, json!("stale")),
            (8, json!("reviewed")),
            (9, json!("reviewed"))
        ]
    );
    // 古い代表は効かない: 解析の推奨に戻る
    assert_eq!(ids_where(&group, "effective"), [7, 9]);
    assert_eq!(
        keys(&server.groups("state=unconfirmed&ids=7").await),
        [("similar".to_owned(), 3)]
    );
    assert_eq!(server.quality_status().await["curation_pending"], 3);
    // 記録そのものは残っている（消さない）
    assert_eq!(server.table().len(), 3);
}

#[tokio::test]
async fn a_stale_token_is_a_409_and_changes_nothing_in_four_ways() {
    // 1. メンバーの集合が違う（写真が加わった）
    let server = Server::start().await;
    let old = server.token(3).await;
    server
        .db
        .execute_batch(
            "INSERT INTO photos(id, source, name, display, md5, mtime, taken_at, lat, lon, favorite, hidden)
               VALUES (13, 'local', 'IMG_C13.jpg', 'derivatives/c13.jpg', 'md5-c13', 130.0, '2025-07-03 11:03', 35.3, 139.3, 0, 0);
             INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id, group_kind, rank_in_group, recommended, score, algorithm, analyzed_at)
               VALUES (13, 'derivatives/c13.jpg', 'md5-c13', 130.0, 'sha-13', 3, 'similar', 4, 0, 60.5, 't', '2025-07-10T00:00:00');",
        )
        .unwrap();
    assert_conflict(&server, "similar", 3, &old, ids(&[7])).await;

    // 2. 内容の SHA は同じで、更新時刻だけ違う（写真 8 は 7 と同じ SHA）
    let server = Server::start().await;
    let old = server.token(3).await;
    server
        .db
        .execute_batch(
            "UPDATE photos SET mtime=80.5 WHERE id=8;
             UPDATE photo_quality SET source_mtime=80.5 WHERE photo_id=8;",
        )
        .unwrap();
    assert_conflict(&server, "similar", 3, &old, ids(&[7])).await;

    // 3. 別のタブが先に代表を変えた
    let server = Server::start().await;
    let tab_a = server.token(2).await;
    let tab_b = server.token(2).await;
    assert_eq!(tab_a, tab_b);
    assert_eq!(
        server.confirm("similar", 2, &tab_b, ids(&[5])).await.status,
        200
    );
    let after_b = server.table();
    assert_conflict(&server, "similar", 2, &tab_a, ids(&[4])).await;
    assert_eq!(server.table(), after_b);
    assert_eq!(ids_where(&server.group(2).await, "effective"), [5]);

    // 4. reset のあとの古い印
    let server = Server::start().await;
    let confirmed = {
        let token = server.token(1).await;
        let response = server.confirm("exact", 1, &token, ids(&[2])).await;
        response.json()["group"]["token"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(server.reset("exact", 1, &confirmed).await.status, 200);
    assert_conflict(&server, "exact", 1, &confirmed, ids(&[2])).await;
    let response = server.reset("exact", 1, &confirmed).await;
    assert_eq!(
        (response.status, error_of(&response)),
        (409, "group_changed".into())
    );
    assert_eq!(server.table(), []);
}

/// `token` が古い/違うとき、`confirm`・`reset` とも 409 `group_changed` で表が変わらない。
async fn assert_conflict(server: &Server, kind: &str, group_id: i64, token: &str, reps: Value) {
    let before = server.table();
    let response = server.confirm(kind, group_id, token, reps).await;
    assert_eq!(response.status, 409, "{}", response.text());
    assert_eq!(
        response.json(),
        json!({"ok": false, "error": "group_changed"})
    );
    let response = server.reset(kind, group_id, token).await;
    assert_eq!(response.status, 409, "{}", response.text());
    assert_eq!(
        response.json(),
        json!({"ok": false, "error": "group_changed"})
    );
    assert_eq!(server.table(), before);
}

#[tokio::test]
async fn an_unknown_group_or_a_made_up_token_is_a_409() {
    let server = Server::start().await;
    // 形は正しいが合わない印・無いグループ・種類違い・グループ外の ID
    assert_conflict(&server, "similar", 2, "0000000000000000", ids(&[5])).await;
    let token = server.token(2).await;
    assert_conflict(&server, "similar", 999, &token, ids(&[5])).await;
    assert_conflict(&server, "exact", 2, &token, ids(&[5])).await;
    assert_conflict(&server, "similar", 10, &token, ids(&[5])).await;
}

#[tokio::test]
async fn malformed_bodies_are_400_before_anything_else_and_write_nothing() {
    let server = Server::start().await;
    let token = server.token(2).await;
    let good = |reps: Value| json!({"kind": "similar", "group_id": 2, "token": token, "representatives": reps});
    let mut bodies: Vec<(String, Value)> = Vec::new();
    for reps in [
        json!([]),
        json!([1.0]),
        json!([5.5]),
        json!([-1]),
        json!([0]),
        json!([true]),
        json!(["5"]),
        json!([null]),
        json!([5, 5]),
        json!([18446744073709551615u64]),
        json!(5),
        json!("5"),
        json!(true),
        json!({}),
    ] {
        bodies.push((format!("representatives={reps}"), good(reps)));
    }
    // キーが無い（`null` は明示したときだけ）
    bodies.push((
        "representatives が無い".into(),
        json!({"kind": "similar", "group_id": 2, "token": token}),
    ));
    for (what, patch) in [
        ("kind", json!({"kind": "other"})),
        ("kind が数", json!({"kind": 1})),
        ("kind が無い", json!({"kind": null})),
        ("group_id=0", json!({"group_id": 0})),
        ("group_id=-1", json!({"group_id": -1})),
        ("group_id=2.0", json!({"group_id": 2.0})),
        ("group_id が文字", json!({"group_id": "2"})),
        ("group_id が真偽値", json!({"group_id": true})),
        (
            "group_id が u64 超",
            json!({"group_id": 18446744073709551615u64}),
        ),
        ("token が大文字", json!({"token": token.to_uppercase()})),
        ("token が短い", json!({"token": &token[..15]})),
        ("token が長い", json!({"token": format!("{token}0")})),
        ("token が数", json!({"token": 1234567890123456u64})),
        ("token が null", json!({"token": null})),
    ] {
        let mut body = good(Value::Null);
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        bodies.push((what.into(), body));
    }
    for (what, body) in bodies {
        for route in ["confirm", "reset"] {
            // `reset` は `representatives` を見ない
            let ignored = route == "reset" && what.starts_with("representatives");
            let response = server.post(route, &body).await;
            if ignored {
                // 本文の形が合うので通る（印は合う）。表は次の試験のために戻す
                assert_eq!(response.status, 200, "{route} {what}");
                continue;
            }
            assert_eq!(response.status, 400, "{route} {what}: {}", response.text());
            assert_eq!(
                response.json(),
                json!({"ok": false, "error": "bad_params"}),
                "{route} {what}"
            );
            assert_eq!(server.table(), [], "{route} {what}");
        }
    }
    // JSON でない・オブジェクトでない
    for text in ["not json", "[]", "1", "null"] {
        assert_eq!(
            server.post_text("confirm", text).await.status,
            400,
            "{text}"
        );
    }
    assert_eq!(server.table(), []);
}

#[tokio::test]
async fn an_unknown_token_value_never_echoes_into_the_error() {
    let server = Server::start().await;
    let response = server
        .post(
            "confirm",
            &json!({"kind": "similar", "group_id": 2, "token": "SECRET-TOKEN-xyz", "representatives": [5]}),
        )
        .await;
    assert_eq!(response.status, 400);
    assert!(!response.text().contains("SECRET"));
    let response = server
        .confirm("similar", 2, "0123456789abcdef", ids(&[5, 999]))
        .await;
    // 組は合わない（印が違う）ので 409。ID の一覧を含めない
    assert_eq!(response.status, 409);
    assert!(!response.text().contains("999"));
}

// ---------------------------------------------------------------- AC7

#[tokio::test]
async fn csrf_checks_apply_to_confirm_and_reset() {
    let server = Server::start().await;
    let token = server.token(2).await;
    let body = json!({"kind": "similar", "group_id": 2, "token": token, "representatives": [5]})
        .to_string();
    for route in ["confirm", "reset"] {
        let send = |content_type: Option<&'static str>, origin: Option<&'static str>| {
            let mut headers: Vec<(String, String)> = vec![
                ("Host".into(), "localhost".into()),
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ];
            if let Some(ct) = content_type {
                headers.push(("Content-Type".into(), ct.into()));
            }
            if let Some(origin) = origin {
                headers.push(("Origin".into(), origin.into()));
            }
            let target = format!("/api/admin/curation/{route}");
            let addr = server.addr;
            let body = body.clone();
            async move { post_raw(addr, &target, &headers, body.as_bytes()).await }
        };
        for (what, response) in [
            ("text/plain", send(Some("text/plain"), None).await),
            (
                "form",
                send(Some("application/x-www-form-urlencoded"), None).await,
            ),
            ("Content-Type が無い", send(None, None).await),
            (
                "別の Origin",
                send(Some("application/json"), Some("http://attacker.example")).await,
            ),
            (
                "ポートの違う Origin",
                send(Some("application/json"), Some("http://localhost:8080")).await,
            ),
        ] {
            assert_eq!(response.status, 403, "{route} {what}");
            assert!(response.headers.contains("cache-control: no-store"));
        }
        assert_eq!(server.table(), [], "{route}");
    }
    // 同じ Origin は通る
    let ok = post_raw(
        server.addr,
        "/api/admin/curation/confirm",
        &[
            ("Host".into(), "localhost".into()),
            ("Origin".into(), "http://localhost".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ],
        body.as_bytes(),
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.text());
}

#[tokio::test]
async fn without_a_writer_a_well_formed_post_is_503_and_a_malformed_one_is_400() {
    let server = Server::start_with(true, false, "").await;
    let token = server.token(2).await;
    let response = server.confirm("similar", 2, &token, ids(&[5])).await;
    assert_eq!(response.status, 503, "{}", response.text());
    assert_eq!(response.json()["ok"], false);
    assert_eq!(server.reset("similar", 2, &token).await.status, 503);
    // 503 でも、「確認済み」だけでも、何も書かない。`null` も 503
    assert_eq!(
        server
            .confirm("similar", 2, &token, Value::Null)
            .await
            .status,
        503
    );
    assert_eq!(server.table(), []);
    // 判定の順序: 本文の形（400）が writer の有無（503）より先
    let bad = server
        .post(
            "confirm",
            &json!({"kind": "similar", "group_id": 2, "token": token}),
        )
        .await;
    assert_eq!(bad.status, 400);
    // 読み取りは writer が無くても動く
    assert_eq!(server.groups("").await["total"], 3);
}

#[tokio::test]
async fn an_oversized_body_gets_413_and_nothing_is_written() {
    let server = Server::start().await;
    let token = server.token(2).await;
    let filler = "5,".repeat(40_000);
    let body = format!(
        r#"{{"kind":"similar","group_id":2,"token":"{token}","representatives":[{filler}5]}}"#
    );
    assert!(body.len() > 64 * 1024);
    for route in ["confirm", "reset"] {
        let response = server.post_text(route, &body).await;
        assert_eq!(response.status, 413, "{route}");
    }
    assert_eq!(server.table(), []);
}

#[tokio::test]
async fn groups_work_on_a_database_without_the_curation_table() {
    for writer in [false, true] {
        let server = Server::start_with(false, writer, "").await;
        let tables: i64 = server
            .db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='photo_curation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        let body = server.groups("").await;
        assert_eq!(body["total"], 3);
        assert_eq!(
            keys(&body),
            [
                ("similar".to_owned(), 3),
                ("similar".to_owned(), 2),
                ("exact".to_owned(), 1)
            ]
        );
        for g in body["groups"].as_array().unwrap() {
            assert_eq!(g["state"], "unconfirmed");
            assert_eq!(g["token"].as_str().unwrap().len(), 16);
            for m in g["members"].as_array().unwrap() {
                assert_eq!(m["curation"], Value::Null);
                assert_eq!(m["effective"], m["recommended"]);
            }
        }
        // 表が無い印は、表があって記録が無い DB と同じ（記録の見え方はどちらも none）
        let with_table = Server::start().await;
        assert_eq!(server.token(1).await, with_table.token(1).await);
        assert_eq!(server.groups("state=confirmed").await["total"], 0);
        assert_eq!(server.groups("state=all&per=1").await["total"], 3);
        // 状況タブの未確認の数もすべてのグループ
        assert_eq!(server.quality_status().await["curation_pending"], 3);
        // 書き込みは表が無ければ何もしない（writer があっても起動時に作る前提。ここでは 503）
        let token = server.token(2).await;
        let response = server.confirm("similar", 2, &token, ids(&[5])).await;
        assert_eq!(response.status, 503);
        assert_eq!(
            server
                .db
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name='photo_curation'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn only_get_on_the_exact_path_serves_groups_and_only_post_on_the_two_paths_writes() {
    let server = Server::start().await;
    // groups への POST・HEAD は 403
    for method in ["POST", "HEAD"] {
        let response = request(
            server.addr,
            method,
            "/api/admin/curation/groups",
            &[("Host", "localhost")],
        )
        .await;
        assert_eq!(response.status, 403, "{method}");
    }
    let body = br#"{"kind":"similar","group_id":2}"#;
    let post = post_raw(
        server.addr,
        "/api/admin/curation/groups",
        &[
            ("Host".into(), "localhost".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ],
        body,
    )
    .await;
    assert_eq!(post.status, 403);
    // 完全一致だけ
    for target in [
        "/api/admin/curation/groupsX",
        "/api/admin/curation/groups/",
        "/api/admin/curation/groups/x",
        "/api/admin/curation",
        "/api/admin/curation/",
        "/api/admin/Curation/groups",
        "/api/admin/curation/Groups",
        "/api/admin/curation/gro%75ps",
        "/api/admin/curation/confirm",
        "/api/admin/curation/reset",
    ] {
        let response = get(server.addr, target).await;
        assert_eq!(response.status, 403, "{target}");
        assert!(!response.text().contains("\"groups\""), "{target}");
    }
    // confirm・reset への GET・HEAD は 403、似た名前への POST も 403
    for method in ["GET", "HEAD"] {
        for route in ["confirm", "reset"] {
            let response = request(
                server.addr,
                method,
                &format!("/api/admin/curation/{route}"),
                &[("Host", "localhost")],
            )
            .await;
            assert_eq!(response.status, 403, "{method} {route}");
        }
    }
    for route in ["confirmX", "confirm/x", "resets"] {
        let response = server.post_text(route, "{}").await;
        assert_eq!(response.status, 403, "{route}");
    }
    // クエリ付き・先頭の `//` は同じ経路
    assert_eq!(
        get(server.addr, "/api/admin/curation/groups?x=1")
            .await
            .status,
        200
    );
    assert_eq!(
        get(server.addr, "//api/admin/curation/groups").await.status,
        200
    );
}

// ---------------------------------------------------------------- AC9

#[tokio::test]
async fn the_status_count_equals_the_unconfirmed_total_in_every_situation() {
    async fn assert_same(server: &Server, expected: i64, what: &str) {
        let pending = server.quality_status().await["curation_pending"].clone();
        let total = server.groups("state=unconfirmed&per=1").await["total"].clone();
        assert_eq!(pending, total, "{what}");
        assert_eq!(pending, expected, "{what}");
    }
    let server = Server::start().await;
    assert_same(&server, 3, "最初").await;
    let token = server.token(2).await;
    assert_eq!(
        server.confirm("similar", 2, &token, ids(&[5])).await.status,
        200
    );
    assert_same(&server, 2, "代表を決めた").await;
    let token = server.token(3).await;
    assert_eq!(
        server
            .confirm("similar", 3, &token, Value::Null)
            .await
            .status,
        200
    );
    let token = server.token(1).await;
    assert_eq!(
        server.confirm("exact", 1, &token, ids(&[1])).await.status,
        200
    );
    assert_same(&server, 0, "全部確認済み").await;
    // stale・写真が加わった（再確認）も数える
    server
        .db
        .execute_batch(
            "UPDATE photos SET mtime=41.5 WHERE id=4;
             UPDATE photo_quality SET source_mtime=41.5 WHERE photo_id=4;",
        )
        .unwrap();
    assert_same(&server, 1, "世代が変わった").await;
    server
        .db
        .execute_batch(
            "INSERT INTO photos(id, source, name, display, md5, mtime, taken_at, lat, lon, favorite, hidden)
               VALUES (13, 'local', 'IMG_C13.jpg', 'derivatives/c13.jpg', 'md5-c13', 130.0, '2025-07-03 11:03', 35.3, 139.3, 0, 0);
             INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id, group_kind, rank_in_group, recommended, score, algorithm, analyzed_at)
               VALUES (13, 'derivatives/c13.jpg', 'md5-c13', 130.0, 'sha-13', 3, 'similar', 4, 0, 60.5, 't', '2025-07-10T00:00:00');",
        )
        .unwrap();
    assert_same(&server, 2, "写真が加わった").await;
    // 1 枚しか有効でなくなったグループは数えない
    server
        .db
        .execute_batch("DELETE FROM photo_quality WHERE photo_id IN (8, 9, 13);")
        .unwrap();
    assert_same(&server, 1, "グループが消えた").await;
    // 表が無い DB
    let server = Server::start_with(false, false, "").await;
    assert_same(&server, 3, "表なし").await;
}

// ---------------------------------------------------------------- 同時の書き込み

#[tokio::test]
async fn a_confirm_racing_with_a_photo_quality_replacement_never_leaves_a_half_written_table() {
    // 別の接続が `BEGIN IMMEDIATE` で `photo_quality` を入れ替えている間、`confirm` は待つ（書き込みの順番を取る）。
    // 解放後は、入れ替わった結果を見て 409（組が変わった）か 200 になり、表は常に 1 組まるごと一貫している
    for (replacement, expected) in [
        // group_id を振り直した → 組が消えた
        ("group_id*10+7", 409),
        // 同じ行を入れ直しただけ → 同じ印のまま通る
        ("group_id", 200),
    ] {
        let server = Server::start().await;
        let token = server.token(2).await;
        server.db.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let addr = server.addr;
        let body =
            json!({"kind": "similar", "group_id": 2, "token": token, "representatives": [5]})
                .to_string();
        let request = tokio::spawn(async move {
            post_raw(
                addr,
                "/api/admin/curation/confirm",
                &[
                    ("Host".into(), "localhost".into()),
                    ("Content-Type".into(), "application/json".into()),
                    ("Content-Length".into(), body.len().to_string()),
                    ("Connection".into(), "close".into()),
                ],
                body.as_bytes(),
            )
            .await
        });
        // 要求が writer に届いて、順番待ちになるまで
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!request.is_finished(), "ロックを持っている間は終わらない");
        server
            .db
            .execute_batch(&format!(
                "CREATE TEMP TABLE bk AS SELECT * FROM photo_quality;
                 DELETE FROM photo_quality;
                 INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id,
                                           group_kind, rank_in_group, recommended, score, reasons_json,
                                           issues_json, algorithm, analyzed_at)
                   SELECT photo_id, display, source_md5, source_mtime, content_sha256, {replacement},
                          group_kind, rank_in_group, recommended, score, reasons_json, issues_json,
                          algorithm, analyzed_at FROM bk;
                 DROP TABLE bk;
                 COMMIT;"
            ))
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(20), request)
            .await
            .expect("解放後に終わる")
            .unwrap();
        assert_eq!(
            response.status,
            expected,
            "{replacement}: {}",
            response.text()
        );
        let table = server.table();
        if expected == 409 {
            assert_eq!(response.json()["error"], "group_changed");
            assert_eq!(table, [], "組が変わったので何も書かない");
        } else {
            // 組まるごと（3 枚）。代表は 1 枚、残りは確認済み
            assert_eq!(table.iter().map(|r| r.0).collect::<Vec<_>>(), [4, 5, 6]);
            assert_eq!(
                table.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(),
                ["reviewed", "representative", "reviewed"]
            );
        }
        // 表は常に一貫している（部分的な組が無い）
        let per_group: Vec<i64> = server
            .db
            .prepare(
                "SELECT COUNT(*) FROM photo_curation c JOIN photo_quality q ON q.photo_id=c.photo_id GROUP BY q.group_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(per_group.iter().all(|n| *n == 3), "{per_group:?}");
    }
}
