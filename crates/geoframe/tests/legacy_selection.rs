//! 旧 AI 選別の安全化（P4 / T4.3。`geoframe::selection`）の試験。フィード（`photos::list_photos`）と管理画面の集計
//! （`selection::admin_counts`）が、同じ「世代の合う有効なメンバー」と「表示可能な推薦写真」の判定を共有することを、
//! 合成の DB だけで確かめる。外部通信はしない。
//!
//! - 代表（推薦写真）の削除・更新・回転・hidden 化・位置なし・品質側の非表示で、残りの写真が隠れ続けない
//! - 非推薦のメンバー自身の世代が古いときは隠さない
//! - favorite の免除・未判定／同一場面でないグループ・旧 AI 表の行と列の不変
//! - 検索空間（`ai_active_space`）の切替・新しいベクトルは旧選別の抑止集合を変えない
//! - AI の追加表が無い旧 DB（`INGEST=0`）でも動き、管理画面の集計がフィードと一致する

mod common;

use common::{TempDir, build_empty_catalog, legacy_generation_sql};
use geoframe::ai;
use geoframe::ai::index;
use geoframe::ai::settings::Normalization;
use geoframe::ai::space::SpaceIdentity;
use geoframe::photos::list_photos;
use geoframe::selection::admin_counts;
use rusqlite::Connection;

fn photo(id: i64, extra: &str) -> String {
    format!(
        "INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,lat,lon,favorite,hidden)
         VALUES({id},'drive','n{id}','derivatives/p{id}.jpg','m{id}',{id}.0,'2025-06-01 10:0{id}',
                35.0,139.0,0,0);
         {extra}\n"
    )
}

fn group(id: &str, status: &str, same_scene: i64) -> String {
    format!(
        "INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene)
         VALUES('{id}','scene','t','2025-06-10T00:00:00','{status}',{same_scene});\n"
    )
}

fn member(group: &str, id: i64, recommended: i64) -> String {
    format!(
        "INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,local_score)
         VALUES('{group}',{id},{id},{recommended},0.25);\n"
    )
}

fn exact_quality(id: i64, rank: i64, recommended: i64) -> String {
    format!(
        "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,
           group_kind,rank_in_group,recommended,score,algorithm,analyzed_at)
         VALUES({id},'derivatives/p{id}.jpg','m{id}',{id}.0,'sha-{id}',1,'exact',{rank},{recommended},
           0.5,'t','2025-06-10T00:00:00');\n"
    )
}

/// 写真 1（推薦）・2・3（推薦外）が同じ judged・同一場面のグループにあり、全員が有効なメンバー。
fn scene() -> String {
    let mut sql = String::new();
    for id in 1..=3 {
        sql += &photo(id, "");
    }
    sql += &group("g", "judged", 1);
    sql += &member("g", 1, 1);
    sql += &member("g", 2, 0);
    sql += &member("g", 3, 0);
    sql += &legacy_generation_sql(&[1, 2, 3]);
    sql
}

struct Env {
    _dir: TempDir,
    conn: Connection,
}

fn env(rows: &str) -> Env {
    let dir = TempDir::new("legacy-selection");
    let (_path, conn) = build_empty_catalog(dir.path(), rows);
    Env { _dir: dir, conn }
}

impl Env {
    fn sql(&self, sql: &str) {
        self.conn.execute_batch(sql).unwrap();
    }

    fn feed(&self) -> Vec<i64> {
        list_photos(&self.conn)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect()
    }

    fn suppressed(&self) -> i64 {
        admin_counts(&self.conn).unwrap().suppressed
    }
}

#[test]
fn a_valid_group_with_a_shown_representative_suppresses_the_alternates() {
    let e = env(&scene());
    assert_eq!(e.feed(), [1]);
    let counts = admin_counts(&e.conn).unwrap();
    assert_eq!(
        (counts.groups, counts.group_pending, counts.suppressed),
        (1, 0, 2)
    );
}

#[test]
fn the_alternates_come_back_when_the_representative_is_gone_changed_or_not_shown() {
    // 代表を削除 / 更新 / 回転 / hidden 化 / 位置なし / 表示用画像なしにすると、残りは出る（場面全体を消し続けない）
    for (what, change) in [
        ("deleted", "DELETE FROM photos WHERE id=1"),
        ("updated", "UPDATE photos SET md5='new' WHERE id=1"),
        ("mtime", "UPDATE photos SET mtime=999.0 WHERE id=1"),
        (
            "rotated",
            "UPDATE photos SET display='derivatives/p1.r90.jpg', rotation=90 WHERE id=1",
        ),
        ("hidden", "UPDATE photos SET hidden=1 WHERE id=1"),
        (
            "no position",
            "UPDATE photos SET lat=NULL, lon=NULL WHERE id=1",
        ),
        ("no lon", "UPDATE photos SET lon=NULL WHERE id=1"),
        ("no display", "UPDATE photos SET display=NULL WHERE id=1"),
        ("empty display", "UPDATE photos SET display='' WHERE id=1"),
        (
            "quality changed",
            "UPDATE photo_quality_features SET content_sha256='other' WHERE photo_id=1",
        ),
        (
            "quality error",
            "UPDATE photo_quality_features SET error='x' WHERE photo_id=1",
        ),
        (
            "feature row gone",
            "DELETE FROM photo_ai_features WHERE photo_id=1",
        ),
    ] {
        let e = env(&scene());
        e.sql(change);
        let feed = e.feed();
        assert!(feed.contains(&2) && feed.contains(&3), "{what}: {feed:?}");
        assert_eq!(e.suppressed(), 0, "{what}");
    }
}

#[test]
fn a_representative_hidden_by_the_quality_side_does_not_hide_the_whole_scene() {
    // 代表（推薦）が品質の exact グループで推薦外 → フィードに出ない写真。場面全体を消さない
    let e = env(&scene());
    e.sql(&exact_quality(1, 2, 0));
    assert_eq!(e.feed(), [2, 3]);
    assert_eq!(e.suppressed(), 0);
    // 品質側で推薦（出る）なら、従来どおり抑止する
    let e = env(&scene());
    e.sql(&exact_quality(1, 1, 1));
    assert_eq!(e.feed(), [1]);
    assert_eq!(e.suppressed(), 2);
}

#[test]
fn an_alternate_with_a_stale_generation_is_not_suppressed_but_the_rest_still_are() {
    let e = env(&scene());
    e.sql("UPDATE photos SET md5='new' WHERE id=2");
    assert_eq!(e.feed(), [1, 2]);
    assert_eq!(e.suppressed(), 1);
    // 特徴量の内容 SHA が今の品質解析と違う場合も同じ
    let e = env(&scene());
    e.sql("UPDATE photo_ai_features SET content_sha256='other' WHERE photo_id=3");
    assert_eq!(e.feed(), [1, 3]);
    assert_eq!(e.suppressed(), 1);
}

#[test]
fn members_without_a_generation_match_never_suppress_anything() {
    // 旧特徴量の行が無い・世代が合わない旧メンバーだけのグループは、何も隠さない（旧: 行があるだけで隠していた）
    let mut sql = String::new();
    for id in 1..=3 {
        sql += &photo(id, "");
    }
    sql += &group("g", "judged", 1);
    sql += &member("g", 1, 1);
    sql += &member("g", 2, 0);
    sql += &member("g", 3, 0);
    let e = env(&sql);
    assert_eq!(e.feed(), [1, 2, 3]);
    let counts = admin_counts(&e.conn).unwrap();
    assert_eq!(
        (counts.groups, counts.group_pending, counts.suppressed),
        (0, 0, 0)
    );
}

#[test]
fn favorites_and_the_other_existing_rules_are_unchanged() {
    let e = env(&scene());
    e.sql("UPDATE photos SET favorite=1 WHERE id=2");
    assert_eq!(e.feed(), [1, 2]); // favorite の免除
    assert_eq!(e.suppressed(), 1); // 現在のフィードで実際に抑止する写真だけ
    // 未判定・同一場面でないグループは推薦外でも出る
    for (status, same_scene) in [("pending", 1), ("error", 1), ("judged", 0)] {
        let mut sql = String::new();
        for id in 1..=3 {
            sql += &photo(id, "");
        }
        sql += &group("g", status, same_scene);
        sql += &member("g", 1, 1);
        sql += &member("g", 2, 0);
        sql += &member("g", 3, 0);
        sql += &legacy_generation_sql(&[1, 2, 3]);
        let e = env(&sql);
        assert_eq!(e.feed(), [1, 2, 3], "{status}/{same_scene}");
        assert_eq!(e.suppressed(), 0, "{status}/{same_scene}");
        let counts = admin_counts(&e.conn).unwrap();
        assert_eq!(counts.groups, 1);
        assert_eq!(counts.group_pending, i64::from(status != "judged"));
    }
    // hidden など他の既存の規則は変えない
    let e = env(&scene());
    e.sql("UPDATE photos SET hidden=1 WHERE id=3");
    assert_eq!(e.feed(), [1]);
}

#[test]
fn the_admin_counts_match_what_the_feed_hides() {
    // 複数のグループ: g1 有効（2 枚抑止）、g2 は代表が削除済み（抑止なし）、g3 は未判定
    let mut sql = String::new();
    for id in 1..=9 {
        sql += &photo(id, "");
    }
    sql += &group("g1", "judged", 1);
    sql += &member("g1", 1, 1);
    sql += &member("g1", 2, 0);
    sql += &member("g1", 3, 0);
    sql += &group("g2", "judged", 1);
    sql += &member("g2", 4, 1);
    sql += &member("g2", 5, 0);
    sql += &group("g3", "pending", 1);
    sql += &member("g3", 6, 1);
    sql += &member("g3", 7, 0);
    sql += &legacy_generation_sql(&[1, 2, 3, 4, 5, 6, 7]);
    sql += "DELETE FROM photos WHERE id=4;\n";
    let e = env(&sql);
    let feed = e.feed();
    let members: Vec<i64> = vec![1, 2, 3, 5, 6, 7];
    let hidden_by_ai = members.iter().filter(|id| !feed.contains(id)).count() as i64;
    assert_eq!(hidden_by_ai, 2);
    let counts = admin_counts(&e.conn).unwrap();
    assert_eq!(counts.suppressed, hidden_by_ai);
    // g1・g2（代表は削除済みだが 5 は有効）・g3 の 3 グループ、うち未判定 1
    assert_eq!((counts.groups, counts.group_pending), (3, 1));
}

#[test]
fn switching_the_search_space_never_changes_the_legacy_suppression() {
    let mut e = env(&scene());
    let before = e.feed();
    let counts_before = admin_counts(&e.conn).unwrap();
    // 新しい空間を作り、ベクトルを入れて、有効にして、外しても、旧選別の結果は同じ
    ai::migrate(&mut e.conn).unwrap();
    let space = SpaceIdentity::new(
        "s",
        "shared-1",
        "model-a",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    index::register_space(&mut e.conn, &space, 0).unwrap();
    for (sha, v) in [
        ("sha-1", [1.0, 0.0, 0.0]),
        ("sha-2", [0.0, 1.0, 0.0]),
        ("sha-3", [0.0, 0.0, 1.0]),
    ] {
        index::store_vector(&mut e.conn, &space, sha, "sent", &v, 0).unwrap();
        assert_eq!(e.feed(), before);
    }
    let snapshot = ai::activation::snapshot_targets(&e.conn, &space).unwrap();
    assert!(
        ai::activation::try_activate(&mut e.conn, &snapshot, &space, 0.95)
            .unwrap()
            .published()
    );
    assert_eq!(e.feed(), before);
    assert_eq!(admin_counts(&e.conn).unwrap(), counts_before);
    assert!(ai::activation::deactivate(&mut e.conn).unwrap());
    assert_eq!(e.feed(), before);
    assert_eq!(admin_counts(&e.conn).unwrap(), counts_before);
}

#[test]
fn the_legacy_tables_are_only_read() {
    let e = env(&scene());
    let tables = [
        "photos",
        "photo_ai_features",
        "photo_embeddings",
        "photo_ai_groups",
        "photo_ai_group_members",
        "photo_quality",
        "photo_quality_features",
    ];
    let dump = |conn: &Connection| -> Vec<String> {
        let mut out = Vec::new();
        for table in tables {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let n = stmt.column_count();
            let rows = stmt
                .query_map([], |r| {
                    (0..n)
                        .map(|i| {
                            r.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|v| format!("{table}:{}", v.join("|")))
                })
                .unwrap();
            out.extend(rows.map(Result::unwrap));
        }
        out
    };
    let before = dump(&e.conn);
    let changes = e.conn.total_changes();
    e.feed();
    admin_counts(&e.conn).unwrap();
    assert_eq!(e.conn.total_changes(), changes);
    assert_eq!(dump(&e.conn), before);
}

/// AI の追加表が無い旧 DB（`REQUIRED_SCHEMA` だけ）を読み取り専用で読んでも、安全化の SQL は動く。
#[test]
fn the_safe_selection_runs_on_an_old_database_without_any_ai_tables() {
    let dir = TempDir::new("legacy-old-db");
    let path = dir.path().join("catalog.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        for (table, columns) in geoframe::db::REQUIRED_SCHEMA {
            conn.execute_batch(&format!("CREATE TABLE {table} ({})", columns.join(", ")))
                .unwrap();
        }
        conn.execute_batch(&format!(
            "{}{}INSERT INTO photo_ai_groups(group_id,status,same_scene) VALUES('g','judged',1);
             INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,local_score)
               VALUES('g',1,1,1,0.5);
             INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,local_score)
               VALUES('g',2,2,0,0.5);",
            photo(1, ""),
            photo(2, "")
        ))
        .unwrap();
        conn.execute_batch(&legacy_generation_sql_untyped(&[1, 2]))
            .unwrap();
    }
    let ro = geoframe::db::open_read_only(&path).unwrap();
    assert!(!table_exists(&ro, "ai_embedding_spaces"));
    let ids: Vec<i64> = list_photos(&ro)
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, [1]);
    let counts = admin_counts(&ro).unwrap();
    assert_eq!((counts.groups, counts.suppressed), (1, 1));
    assert!(!table_exists(&ro, "ai_embedding_spaces"));
    let status = index::status(&ro).unwrap();
    assert_eq!(status.active, geoframe::ai::index::ActiveState::NoTables);
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

/// `REQUIRED_SCHEMA` の最小の表（列の型なし・NOT NULL なし）への世代の行。
fn legacy_generation_sql_untyped(ids: &[i64]) -> String {
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    format!(
        "INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,error)
           SELECT id,display,md5,mtime,'sha-'||id,'' FROM photos WHERE id IN ({list});
         INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,
                embedding_model,error)
           SELECT id,display,md5,mtime,'sha-'||id,'legacy-model','' FROM photos WHERE id IN ({list});"
    )
}
