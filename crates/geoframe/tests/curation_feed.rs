//! 人が選んだ代表（`photo_curation`）がフィード（`list_photos`・`GET /api/v1/photos`）と状況タブの
//! `db.quality` に反映されること（PR ③ の AC4（フィード分）・AC6）。記録は表へ直接 INSERT する
//! （管理 API の試験は `curation_api.rs`）。合成データだけ（tests/fixtures/curation-catalog.sql）。
//!
//! フィクスチャ: exact グループ 1（写真 1 推奨・2・3。3 は除外）、similar グループ 2（4・5 推奨・6。6 は位置なし）、
//! similar グループ 3（7 推奨・8・9 推奨。7 と 8 は同じ内容の SHA）、グループ外 10・11（位置なし）・12（除外）。

mod common;

use common::{TempDir, build_curation_catalog, get, photos_from, spawn_with_env};
use geoframe::curation::{Record, State, group_state};
use geoframe::db::Catalog;
use geoframe::photos::list_photos;
use rusqlite::Connection;
use serde_json::Value;

struct Env {
    dir: TempDir,
    conn: Connection,
}

fn env() -> Env {
    let dir = TempDir::new("curation-feed");
    let (_path, conn) = build_curation_catalog(dir.path(), "");
    Env { dir, conn }
}

/// 写真 `id` の今の世代（`photo_quality` の行）で、`role` の記録を書く（置き換える）。
fn record(id: i64, role: &str) -> String {
    format!(
        "INSERT OR REPLACE INTO photo_curation(photo_id,display,source_md5,source_mtime,content_sha256,role,decided_at)
           SELECT photo_id,display,source_md5,source_mtime,content_sha256,'{role}','2025-07-20 00:00:00'
             FROM photo_quality WHERE photo_id={id};\n"
    )
}

/// 今の世代と違う（古い）記録。内容の SHA が違うだけで stale になる。
fn stale_record(id: i64, role: &str) -> String {
    format!(
        "INSERT OR REPLACE INTO photo_curation(photo_id,display,source_md5,source_mtime,content_sha256,role,decided_at)
           SELECT photo_id,display,source_md5,source_mtime,'sha-old','{role}','2025-07-20 00:00:00'
             FROM photo_quality WHERE photo_id={id};\n"
    )
}

impl Env {
    fn sql(&self, sql: &str) {
        self.conn.execute_batch(sql).unwrap();
    }

    /// フィードの `(id, quality_recommended)` の列（フィードの並び）。
    fn feed(&self) -> Vec<(i64, bool)> {
        list_photos(&self.conn)
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.quality_recommended))
            .collect()
    }

    fn feed_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.feed().into_iter().map(|(id, _)| id).collect();
        ids.sort_unstable();
        ids
    }

    /// 推奨のある写真の ID（昇順）。
    fn recommended(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .feed()
            .into_iter()
            .filter(|(_, rec)| *rec)
            .map(|(id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("catalog.db")
    }

    /// `GET /api/admin/status` の `db.quality`。
    async fn quality_status(&self) -> Value {
        let catalog = Catalog::open(&self.path()).unwrap();
        let addr = spawn_with_env(self.dir.path(), &[], catalog).await;
        let response = get(addr, "/api/admin/status").await;
        assert_eq!(response.status, 200, "{}", response.text());
        response.json()["db"]["quality"].clone()
    }

    /// グループの状態（世代の合う `photo_quality` の行のメンバーの記録から。`curation::group_state`）。
    fn state(&self, group_id: i64) -> State {
        // グループ 1 は exact、2・3 は similar（フィクスチャ）
        let kind = if group_id == 1 { "exact" } else { "similar" };
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {record} {from} WHERE q.group_kind=?1 AND q.group_id=?2 ORDER BY q.photo_id",
                record = geoframe::curation::member_record(true),
                from = geoframe::curation::members_from(true),
            ))
            .unwrap();
        let records: Vec<Record> = stmt
            .query_map(rusqlite::params![kind, group_id], |row| {
                Ok(match row.get::<_, String>(0)?.as_str() {
                    "representative" => Record::Representative,
                    "reviewed" => Record::Reviewed,
                    "stale" => Record::Stale,
                    _ => Record::None,
                })
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        group_state(&records)
    }

    fn pending(&self) -> i64 {
        geoframe::curation::pending_groups(&self.conn).unwrap()
    }
}

#[test]
fn without_any_record_the_feed_is_the_analysis_and_equals_the_feed_without_the_table() {
    let e = env();
    // 出る写真: 位置あり・除外でない・exact は推奨だけ（2・3 は出ない）。位置なしの 6・11 と除外の 12 も出ない
    assert_eq!(e.feed_ids(), [1, 4, 5, 7, 8, 9, 10]);
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);

    let with_table = serde_json::to_value(list_photos(&e.conn).unwrap()).unwrap();
    e.sql("DROP TABLE photo_curation;");
    assert!(!geoframe::curation::available(&e.conn).unwrap());
    let without_table = serde_json::to_value(list_photos(&e.conn).unwrap()).unwrap();
    // 表の有無で 1 バイトも変わらない（R17。人の記録が無ければ値も不変）
    assert_eq!(with_table, without_table);
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);
}

#[test]
fn a_human_representative_is_the_only_recommended_photo_of_its_group() {
    let e = env();
    // similar グループ 2: 5 だけを代表に。4 は出るが推奨ではなくなる。グループ 3 は解析のまま
    e.sql(&record(5, "representative"));
    assert_eq!(e.feed_ids(), [1, 4, 5, 7, 8, 9, 10]);
    assert_eq!(e.recommended(), [1, 5, 7, 9]);

    // 複数選べる（4・5 を代表に）
    e.sql(&record(4, "representative"));
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);
}

#[test]
fn a_human_representative_among_photos_with_the_same_content_picks_one_photo_id() {
    let e = env();
    // 7 と 8 は同じ内容の SHA。8 だけを代表にすると、8 だけが true（写真 ID 単位）。7・9 は解析の推奨を失う
    e.sql(&record(8, "representative"));
    assert_eq!(e.recommended(), [1, 4, 5, 8]);
    assert!(e.feed().iter().any(|(id, rec)| *id == 7 && !*rec));
}

#[test]
fn in_an_exact_group_only_the_chosen_photos_appear() {
    let e = env();
    // 解析の推奨は 1。2 を代表にすると、出るのは 2 だけ（1 は exact の推奨外になって落ちる）
    e.sql(&record(2, "representative"));
    assert_eq!(e.feed_ids(), [2, 4, 5, 7, 8, 9, 10]);
    assert_eq!(e.recommended(), [2, 4, 5, 7, 9]);
    // 2 枚とも代表なら 2 枚とも出る
    e.sql(&record(1, "representative"));
    assert_eq!(e.feed_ids(), [1, 2, 4, 5, 7, 8, 9, 10]);
}

#[test]
fn records_do_not_multiply_or_reorder_the_feed() {
    let e = env();
    let before: Vec<i64> = e.feed().into_iter().map(|(id, _)| id).collect();
    for id in 1..=9 {
        e.sql(&record(id, "reviewed"));
    }
    e.sql(&record(5, "representative"));
    let after: Vec<i64> = e.feed().into_iter().map(|(id, _)| id).collect();
    assert_eq!(before, after);
}

#[test]
fn the_choice_survives_the_quality_rebuild_that_renumbers_the_groups() {
    let e = env();
    e.sql(&record(5, "representative"));
    e.sql(&record(2, "representative"));
    let before = e.feed();
    // quality-scan の再構築: photo_quality の全行を入れ替え、group_id を振り直す（世代は同じ）
    e.sql(
        "CREATE TEMP TABLE keep AS SELECT * FROM photo_quality;
         DELETE FROM photo_quality;
         INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,
                group_kind,rank_in_group,recommended,score,reasons_json,issues_json,algorithm,analyzed_at)
           SELECT photo_id,display,source_md5,source_mtime,content_sha256,100-group_id,
                  group_kind,rank_in_group,recommended,score,reasons_json,issues_json,algorithm,'2025-08-01T00:00:00'
             FROM keep;
         DROP TABLE keep;",
    );
    assert_eq!(e.feed(), before);
    assert_eq!(e.recommended(), [2, 5, 7, 9]);
}

#[test]
fn a_representative_hidden_after_the_choice_is_not_in_the_feed() {
    let e = env();
    e.sql(&record(2, "representative"));
    e.sql("UPDATE photos SET hidden=1 WHERE id=2;");
    // exact グループは 1 枚も出ない（1 は推奨外、2・3 は除外）
    assert_eq!(e.feed_ids(), [4, 5, 7, 8, 9, 10]);
    // similar の代表を除外にしても、ほかのメンバーは（推奨でないまま）出る
    e.sql(&record(5, "representative"));
    e.sql("UPDATE photos SET hidden=1 WHERE id=5;");
    assert_eq!(e.feed_ids(), [4, 7, 8, 9, 10]);
    assert!(e.feed().iter().any(|(id, rec)| *id == 4 && !*rec));
}

#[test]
fn a_record_for_a_changed_photo_is_ignored() {
    let e = env();
    // 回転・原本の更新を模す: 5 の内容の SHA が変わった（再解析後）。古い代表の記録は無効（stale）
    e.sql(&record(5, "representative"));
    e.sql("UPDATE photo_quality SET content_sha256='sha-new' WHERE photo_id=5;");
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);
    // 写真の世代が `photo_quality` と合わなくなると、品質の行ごと外れる（従来どおり）
    e.sql(&record(4, "representative"));
    e.sql("UPDATE photos SET mtime=999.0 WHERE id=4;");
    assert!(e.feed().iter().any(|(id, rec)| *id == 4 && !*rec));
    assert_eq!(e.recommended(), [1, 5, 7, 9]);
}

/// 設計書の「境界の期待値」の 5 行。各行を、フィクスチャの similar グループ 3（7・8・9。7 と 8 は同じ SHA）か
/// exact グループ 1（1・2・3）に当てて、`effective`（フィードの `quality_recommended`）と状態を固定する。
#[test]
fn the_five_boundary_cases_of_the_design() {
    // 1. 同じ SHA の 2 枚のうち片方だけ代表（他は reviewed）→ 選んだ写真だけ true・confirmed
    let e = env();
    e.sql(&format!(
        "{}{}{}",
        record(7, "representative"),
        record(8, "reviewed"),
        record(9, "reviewed")
    ));
    assert_eq!(e.recommended(), [1, 4, 5, 7]);
    assert_eq!(e.state(3), State::Confirmed);
    assert_eq!(e.state(2), State::Unconfirmed);
    assert_eq!(e.pending(), 2);

    // 2. exact で 2 枚を代表に → 2 枚とも true（フィードに 2 枚）・confirmed
    let e = env();
    e.sql(&format!(
        "{}{}{}",
        record(1, "representative"),
        record(2, "representative"),
        record(3, "reviewed")
    ));
    assert_eq!(e.recommended(), [1, 2, 4, 5, 7, 9]);
    assert_eq!(e.feed_ids(), [1, 2, 4, 5, 7, 8, 9, 10]);
    assert_eq!(e.state(1), State::Confirmed);
    assert_eq!(e.pending(), 2);

    // 3. 代表が stale（回転した。他は reviewed のまま）→ 有効な代表が無いので解析の推奨・recheck
    let e = env();
    e.sql(&format!(
        "{}{}{}UPDATE photo_quality SET content_sha256='sha-rotated' WHERE photo_id=7;",
        record(7, "representative"),
        record(8, "reviewed"),
        record(9, "reviewed")
    ));
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);
    assert_eq!(e.state(3), State::Recheck);
    assert_eq!(e.pending(), 3);

    // 4. 古い世代の representative 行だけ残り、再構築で同じ組に戻った → 解析の推奨・recheck
    let e = env();
    e.sql(&stale_record(7, "representative"));
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);
    assert_eq!(e.state(3), State::Recheck);
    assert_eq!(e.pending(), 3);

    // 5. reviewed だけ（`representatives: null` で確認）→ 解析の推奨・confirmed
    let e = env();
    e.sql(&format!(
        "{}{}{}",
        record(7, "reviewed"),
        record(8, "reviewed"),
        record(9, "reviewed")
    ));
    assert_eq!(e.recommended(), [1, 4, 5, 7, 9]);
    assert_eq!(e.state(3), State::Confirmed);
    assert_eq!(e.pending(), 2);
}

#[test]
fn a_new_member_or_a_stale_member_makes_a_confirmed_group_recheck() {
    let e = env();
    e.sql(&format!(
        "{}{}{}",
        record(4, "reviewed"),
        record(5, "representative"),
        record(6, "reviewed")
    ));
    assert_eq!(e.state(2), State::Confirmed);
    // 写真が加わった（記録の無いメンバー。10 を 2 のグループへ）
    e.sql(
        "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,
                group_kind,rank_in_group,recommended,score,algorithm,analyzed_at)
           VALUES(10,'derivatives/c10.jpg','md5-c10',100.0,'sha-10',2,'similar',4,0,50.0,'t','2025-08-01T00:00:00');",
    );
    assert_eq!(e.state(2), State::Recheck);
    // 全員 stale は recheck（未確認ではない）
    e.sql("UPDATE photo_quality SET content_sha256=content_sha256||'x' WHERE group_id=2;");
    assert_eq!(e.state(2), State::Recheck);
}

#[test]
fn without_the_table_every_group_is_pending() {
    let e = env();
    e.sql("DROP TABLE photo_curation;");
    assert_eq!(e.pending(), 3);
}

#[tokio::test]
async fn the_endpoint_shows_the_human_choice_and_keeps_the_contract() {
    let e = env();
    e.sql(&record(5, "representative"));
    e.sql(&record(2, "representative"));
    let response = photos_from(&e.path()).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let list = response.json();
    assert_eq!(
        common::violations(&common::validator(), &list),
        Vec::<String>::new()
    );
    let rows = list.as_array().unwrap();
    let rec = |id: i64| -> (bool, String) {
        let row = rows.iter().find(|p| p["id"] == id).unwrap();
        (
            row["quality_recommended"].as_bool().unwrap(),
            row["quality_role"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(rec(5), (true, "recommended".into()));
    assert_eq!(rec(4), (false, "alternate".into()));
    assert_eq!(rec(2), (true, "recommended".into()));
    assert!(rows.iter().all(|p| p["id"] != 1), "exact の推奨外は出ない");
    assert_eq!(rec(10), (false, String::new()));
}

#[tokio::test]
async fn the_status_counts_the_human_choice_and_the_pending_groups() {
    let e = env();
    let q = e.quality_status().await;
    assert_eq!(q["analyzed"], 9);
    assert_eq!(q["recommended"], 5);
    assert_eq!(q["exact_suppressed"], 2);
    assert_eq!(q["similar_alternates"], 2);
    assert_eq!(q["curation_pending"], 3);

    // 5 を代表に: グループ 2 の推奨は 5 だけ（4 が推奨から外れる）
    e.sql(&record(5, "representative"));
    let q = e.quality_status().await;
    assert_eq!(q["recommended"], 4);
    assert_eq!(q["similar_alternates"], 3);
    assert_eq!(q["exact_suppressed"], 2);
    assert_eq!(q["curation_pending"], 3);

    // exact の 2 を代表に: 推奨の数は変わらず（1 → 2）、exact の推奨外も 2 のまま
    e.sql(&record(2, "representative"));
    let q = e.quality_status().await;
    assert_eq!(q["recommended"], 4);
    assert_eq!(q["exact_suppressed"], 2);

    // 全員を確認済みにすると 0
    for id in 1..=9 {
        let role = if id == 2 || id == 5 {
            "representative"
        } else {
            "reviewed"
        };
        e.sql(&record(id, role));
    }
    assert_eq!(e.quality_status().await["curation_pending"], 0);
}

/// 性能の測定（実行: `cargo test --test curation_feed -- --ignored --nocapture`）。合成 6,500 行（うちグループ入り
/// 約 1,000 行・代表の記録 約 100 件）で、`EXPLAIN QUERY PLAN` と `list_photos` の時間を出す。
#[test]
#[ignore = "測定用。結果は docs/admin-library/design.md に記録する"]
fn measure_the_feed_with_6500_rows() {
    let dir = TempDir::new("curation-perf");
    let (_path, conn) = build_curation_catalog(dir.path(), "");
    conn.execute_batch(
        "DELETE FROM photo_curation; DELETE FROM photo_quality; DELETE FROM photos;",
    )
    .unwrap();
    // 6,500 枚。うち 1,000 枚が 2 枚ずつ（500 組。200 組が exact・300 組が similar）、残りはグループ外
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut photo = tx
            .prepare(
                "INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,lat,lon,favorite,hidden)
                 VALUES(?1,'local','n'||?1,'derivatives/p'||?1||'.jpg','m'||?1,?1*1.0,
                        '2025-01-01 00:00:'||printf('%02d',?1%60),35.0,139.0,0,0)",
            )
            .unwrap();
        let mut quality = tx
            .prepare(
                "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,
                        group_kind,rank_in_group,recommended,score,algorithm,analyzed_at)
                 VALUES(?1,'derivatives/p'||?1||'.jpg','m'||?1,?1*1.0,'sha-'||?1,?2,?3,?4,?5,50.0,'t','t')",
            )
            .unwrap();
        for id in 1..=6500_i64 {
            photo.execute([id]).unwrap();
            if id <= 1000 {
                let group = (id + 1) / 2;
                let kind = if group <= 200 { "exact" } else { "similar" };
                let rank = 2 - (id % 2);
                quality
                    .execute(rusqlite::params![
                        id,
                        group,
                        kind,
                        rank,
                        i64::from(rank == 1)
                    ])
                    .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    // 代表の記録 100 件（グループ 1〜100 の 2 枚目を代表に）
    conn.execute_batch(
        "INSERT INTO photo_curation(photo_id,display,source_md5,source_mtime,content_sha256,role,decided_at)
           SELECT photo_id,display,source_md5,source_mtime,content_sha256,'representative','t'
             FROM photo_quality WHERE group_id<=100 AND rank_in_group=2;",
    )
    .unwrap();
    for (label, curation) in [
        ("table absent (legacy SQL)", false),
        ("table present", true),
    ] {
        let sql = geoframe::photos::feed_sql(curation);
        let mut plan = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let lines: Vec<String> = plan
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        println!("{label}: EXPLAIN QUERY PLAN\n  {}", lines.join("\n  "));
        if !curation {
            conn.execute_batch("ALTER TABLE photo_curation RENAME TO photo_curation_off;")
                .unwrap();
        }
        let _ = list_photos(&conn).unwrap();
        let start = std::time::Instant::now();
        let rounds = 20;
        let mut n = 0;
        for _ in 0..rounds {
            n = list_photos(&conn).unwrap().len();
        }
        println!(
            "{label}: list_photos {:.1} ms/round ({n} rows)",
            start.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
        );
        if !curation {
            conn.execute_batch("ALTER TABLE photo_curation_off RENAME TO photo_curation;")
                .unwrap();
        }
    }
}
