//! `regions` 段と `photo_regions` 表の試験。境界は合成の長方形（国・地域の ID だけ同梱の名称表のもの）、座標も合成値で、
//! 実際の撮影座標は使わない。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rusqlite::Connection;

use super::classify_tests::{rect, solid, synth};
use super::stage::Regions;
use super::{available, ensure_tables, join, pending};
use crate::cache::Cache;
use crate::config::{Config, Overrides};
use crate::stop::StopFlag;
use crate::sync::{Counts, Mode, Stage, StageCtx, StageError, StageOutcome};
use crate::testutil::TempDir;
use crate::writer::{OpenMode, Writer};

// ---------------------------------------------------------------- 道具

struct Setup {
    _dir: TempDir,
    config: Config,
    writer: Writer,
    cache: Cache,
}

fn setup(with_table: bool) -> Setup {
    let dir = TempDir::new("regions-stage");
    let overrides = Overrides {
        listen: None,
        data_dir: Some(dir.path().to_path_buf()),
    };
    let config = Config::load(|_| None, &overrides).unwrap();
    let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
    writer.call(crate::schema::migrate).unwrap();
    if with_table {
        writer.call(ensure_tables).unwrap();
    }
    let cache = Cache::load(dir.path());
    Setup {
        _dir: dir,
        config,
        writer,
        cache,
    }
}

type Row = (
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
);

impl Setup {
    fn sql(&self, sql: &str) {
        let sql = sql.to_string();
        self.writer.call(move |c| c.execute_batch(&sql)).unwrap();
    }

    /// `lat`・`lon` は SQL のリテラルのまま渡す（文字列・無限大・NULL も入れる）。
    fn photo(&self, id: i64, lat: &str, lon: &str, pref: Option<&str>) {
        let pref = pref.map_or("NULL".to_string(), |p| format!("'{p}'"));
        self.sql(&format!(
            "INSERT INTO photos(id, source, name, lat, lon, pref) VALUES({id}, 'drive', 'p{id}', {lat}, {lon}, {pref})"
        ));
    }

    fn read(&self, mode: Mode) -> Connection {
        let real = crate::db::open_read_only(&self.config.db_path()).unwrap();
        match mode {
            Mode::Apply => real,
            Mode::DryRun => crate::sync::simulation_copy(&real).unwrap(),
        }
    }

    fn run_with(
        &self,
        stage: &Regions,
        mode: Mode,
        read: &Connection,
        writer: &Writer,
        stop: &StopFlag,
    ) -> Result<StageOutcome, StageError> {
        let http = crate::net::agent();
        let ctx = StageCtx {
            mode,
            config: &self.config,
            read,
            writer: (mode == Mode::Apply).then_some(writer),
            cache: &self.cache,
            http: &http,
            stop,
        };
        stage.run(&ctx)
    }

    fn try_run(&self, stage: &Regions) -> Result<StageOutcome, StageError> {
        let read = self.read(Mode::Apply);
        self.run_with(stage, Mode::Apply, &read, &self.writer, &StopFlag::new())
    }

    fn run(&self, stage: &Regions) -> HashMap<&'static str, u64> {
        match self.try_run(stage).unwrap() {
            StageOutcome::Done(counts) => map(&counts),
            other => panic!("{other:?}"),
        }
    }

    fn dry_run(&self, stage: &Regions) -> HashMap<&'static str, u64> {
        let read = self.read(Mode::DryRun);
        match self
            .run_with(stage, Mode::DryRun, &read, &self.writer, &StopFlag::new())
            .unwrap()
        {
            StageOutcome::Done(counts) => map(&counts),
            other => panic!("{other:?}"),
        }
    }

    fn rows(&self) -> Vec<Row> {
        let conn = self.read(Mode::Apply);
        let mut stmt = conn
            .prepare(
                "SELECT photo_id, country_id, region_id, country_method, region_method,
                        classifier_version, search_text FROM photo_regions ORDER BY photo_id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn row_count(&self) -> i64 {
        self.read(Mode::Apply)
            .query_row("SELECT COUNT(*) FROM photo_regions", [], |r| r.get(0))
            .unwrap()
    }
}

fn map(counts: &Counts) -> HashMap<&'static str, u64> {
    counts.0.iter().copied().collect()
}

/// 合成の世界（経度 x, 緯度 y）。ID は同梱の名称表のもの。
///
/// ```text
/// USA : [0,10]x[0,10]   地域 USA-3513: x[0,5]、USA-3514: x[5,10]
/// AUS : [20,30]x[0,10]  地域なし
/// FRA : [40,50]x[0,10]  地域 FRA-1442: x[40,45]
/// JPN : [60,70]x[0,10]  地域なし
/// CAN : [65,75]x[0,10]  （JPN と x[65,70] で重なる = 国が ambiguous）
/// ```
fn world_index() -> Result<super::RegionIndex, super::data::DataError> {
    synth(
        &[
            ("ne:USA", vec![solid(rect(0.0, 0.0, 10.0, 10.0))]),
            ("ne:AUS", vec![solid(rect(20.0, 0.0, 30.0, 10.0))]),
            ("ne:FRA", vec![solid(rect(40.0, 0.0, 50.0, 10.0))]),
            ("ne:JPN", vec![solid(rect(60.0, 0.0, 70.0, 10.0))]),
            ("ne:CAN", vec![solid(rect(65.0, 0.0, 75.0, 10.0))]),
        ],
        &[
            (
                "ne:USA-3513",
                "ne:USA",
                vec![solid(rect(0.0, 0.0, 5.0, 10.0))],
            ),
            (
                "ne:USA-3514",
                "ne:USA",
                vec![solid(rect(5.0, 0.0, 10.0, 10.0))],
            ),
            (
                "ne:FRA-1442",
                "ne:FRA",
                vec![solid(rect(40.0, 0.0, 45.0, 10.0))],
            ),
        ],
    )
}

// (緯度, 経度)
const US1: (&str, &str) = ("5.0", "2.0");
const US2: (&str, &str) = ("5.0", "7.0");
const AUS: (&str, &str) = ("5.0", "25.0");
const FRA_REGION: (&str, &str) = ("5.0", "42.0");
const FRA_ONLY: (&str, &str) = ("5.0", "47.0");
const AMBIGUOUS: (&str, &str) = ("5.0", "67.0");
const SEA: (&str, &str) = ("5.0", "35.0");

fn stage(version: &str) -> (Regions, Arc<AtomicUsize>) {
    let loads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&loads);
    let stage = Regions::with_index(version, move || {
        counter.fetch_add(1, Ordering::SeqCst);
        world_index()
    });
    (stage, loads)
}

fn loads(n: &Arc<AtomicUsize>) -> usize {
    n.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------- 表

#[test]
fn the_table_is_created_once_and_not_in_the_python_schema() {
    let s = setup(false);
    let conn = s.read(Mode::Apply);
    // 旧 DB（表なし）でも読める。読み取りから表は作られない
    assert!(!available(&conn).unwrap());
    assert_eq!(pending(&conn).unwrap(), vec!["photo_regions".to_string()]);
    assert!(!available(&s.read(Mode::Apply)).unwrap());
    // 書き込みの起動で作る（2 回目は何も作らない）
    assert_eq!(
        s.writer.call(ensure_tables).unwrap(),
        vec!["photo_regions".to_string()]
    );
    assert!(s.writer.call(ensure_tables).unwrap().is_empty());
    let conn = s.read(Mode::Apply);
    assert!(available(&conn).unwrap());
    assert!(pending(&conn).unwrap().is_empty());
    // 索引もできる
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='photo_regions_place'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
    // `schema::SCHEMA`（Python の fixture と比べる定数）には足していない
    assert!(!crate::schema::SCHEMA.contains("photo_regions"));
}

#[test]
fn the_stage_does_not_create_the_table_and_fails_loudly_in_apply() {
    let s = setup(false);
    s.photo(1, US1.0, US1.1, None);
    let (st, n) = stage("v1");
    let e = s.try_run(&st).expect_err("表が無ければ書かない");
    assert!(e.to_string().contains("photo_regions"), "{e}");
    assert!(!available(&s.read(Mode::Apply)).unwrap());
    assert_eq!(loads(&n), 0);
}

#[test]
fn the_join_accepts_only_rows_with_the_current_coordinates_and_version() {
    let s = setup(true);
    let version = super::classifier_version().unwrap();
    // 1: 一致  2: 座標が変わった  3: 版が違う  4: 座標が文字列  5: 座標が無限大  6: 範囲外  7: 座標が NULL  8: 行が無い
    s.photo(1, "35.5", "139.5", None);
    s.photo(2, "35.5", "139.5", None);
    s.photo(3, "35.5", "139.5", None);
    s.photo(4, "'abc'", "139.5", None);
    s.photo(5, "9e999", "139.5", None);
    s.photo(6, "95.0", "139.5", None);
    s.photo(7, "NULL", "NULL", None);
    s.photo(8, "35.5", "139.5", None);
    let insert = |id: i64, lat: &str, lon: &str, ver: &str| {
        format!(
            "INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_id,search_text,country_method,region_method,classified_at)
             VALUES({id},{lat},{lon},'{ver}','ne:JPN','',  'inside','outside','2026-01-01 00:00:00')"
        )
    };
    s.sql(&insert(1, "35.5", "139.5", version));
    s.sql(&insert(2, "35.6", "139.5", version));
    s.sql(&insert(3, "35.5", "139.5", "old"));
    s.sql(&insert(4, "0", "139.5", version));
    s.sql(&insert(5, "9e999", "139.5", version));
    s.sql(&insert(6, "95.0", "139.5", version));
    s.sql(&insert(7, "0", "0", version));
    let conn = s.read(Mode::Apply);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT p.id FROM photos p {} WHERE r.photo_id IS NOT NULL ORDER BY p.id",
            join("p", "r")
        ))
        .unwrap();
    let valid: Vec<i64> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(valid, vec![1]);
    // JOIN で写真の行は増えない
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM photos p {}", join("p", "r")),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(total, 8);
}

// ---------------------------------------------------------------- 候補

#[test]
fn candidates_are_unplaced_photos_with_valid_coordinates_only() {
    let s = setup(true);
    s.photo(1, US1.0, US1.1, None); // 候補（pref NULL）
    s.photo(2, US2.0, US2.1, Some("")); // 候補（pref 空文字）
    s.photo(3, US1.0, US1.1, Some("東京都")); // pref あり: 対象外
    s.photo(4, "NULL", "NULL", None); // 座標なし
    s.photo(5, "'x'", "1.0", None); // 文字列
    s.photo(6, "9e999", "1.0", None); // 無限大
    s.photo(7, "91.0", "1.0", None); // 範囲外
    s.photo(8, "1.0", "-181.0", None); // 範囲外
    s.photo(9, "-90.0", "180.0", None); // 端は有効
    let (st, n) = stage("v1");
    let c = s.run(&st);
    assert_eq!(c["pending"], 3);
    assert_eq!(c["scan_incomplete"], 0);
    assert_eq!(c["inside"] + c["unresolved"], 3);
    assert_eq!(
        s.rows().iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![1, 2, 9]
    );
    assert_eq!(loads(&n), 1);
    // 無効な座標・pref ありは次のサイクルでも枠を占有しない（候補 0 件 = 索引を作らない）
    let c = s.run(&st);
    assert_eq!(c["pending"], 0);
    assert_eq!(loads(&n), 1);
}

#[test]
fn results_are_stored_for_every_branch_and_not_selected_again() {
    let s = setup(true);
    let cases = [
        (
            1,
            US1,
            Some("ne:USA"),
            Some("ne:USA-3513"),
            "inside",
            "inside",
        ),
        (
            2,
            US2,
            Some("ne:USA"),
            Some("ne:USA-3514"),
            "inside",
            "inside",
        ),
        (3, AUS, Some("ne:AUS"), None, "inside", "outside"),
        (
            4,
            FRA_REGION,
            Some("ne:FRA"),
            Some("ne:FRA-1442"),
            "inside",
            "inside",
        ),
        (5, FRA_ONLY, Some("ne:FRA"), None, "inside", "outside"),
        (6, AMBIGUOUS, None, None, "ambiguous", "ambiguous"),
        (7, SEA, None, None, "outside", "outside"),
    ];
    for (id, (lat, lon), ..) in cases {
        s.photo(id, lat, lon, None);
    }
    let (st, n) = stage("v1");
    let c = s.run(&st);
    assert_eq!(c["pending"], 7);
    assert_eq!(c["inside"], 5);
    assert_eq!(c["unresolved"], 2);
    assert_eq!(c["stale_input"], 0);
    assert_eq!(c["failed"], 0);
    assert_eq!(c["deferred"], 0);
    let rows = s.rows();
    assert_eq!(rows.len(), 7);
    for (&(id, _, country, region, cm, rm), row) in cases.iter().zip(&rows) {
        assert_eq!(row.0, id);
        assert_eq!(row.1.as_deref(), country, "photo {id}");
        assert_eq!(row.2.as_deref(), region, "photo {id}");
        assert_eq!((row.3.as_str(), row.4.as_str()), (cm, rm), "photo {id}");
        assert_eq!(row.5, "v1");
    }
    // 検索用文字列: 国→地域の名前。未判定は空
    assert!(rows[0].6.contains("アメリカ") && rows[0].6.contains("マサチューセッツ"));
    assert!(rows[2].6.contains("オーストラリア"));
    assert_eq!(rows[5].6, "");
    assert_eq!(rows[6].6, "");
    // 未解決（outside・ambiguous）を含めて、同じ版・同じ座標では再選択しない
    let c = s.run(&st);
    assert_eq!(c["pending"], 0);
    assert_eq!(loads(&n), 1, "2 回目は索引を作らない");
    assert_eq!(s.rows(), rows);
}

#[test]
fn a_changed_coordinate_or_a_new_version_is_classified_again() {
    let s = setup(true);
    s.photo(1, US1.0, US1.1, None);
    s.photo(2, AUS.0, AUS.1, None);
    let (st, _) = stage("v1");
    assert_eq!(s.run(&st)["pending"], 2);
    // 座標が変わった写真だけが候補になる
    s.sql("UPDATE photos SET lat=5.0, lon=7.0 WHERE id=1");
    let c = s.run(&st);
    assert_eq!(c["pending"], 1);
    let rows = s.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].2.as_deref(), Some("ne:USA-3514"));
    // 版が上がると全件が候補。行は入れ替わる（増えない）
    let (v2, n2) = stage("v2");
    let c = s.run(&v2);
    assert_eq!(c["pending"], 2);
    assert_eq!(loads(&n2), 1);
    let rows = s.rows();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.5 == "v2"));
    // 古い版で動く段は、また全件を分類し直す（版は『今の同梱版』と同じ時だけ有効）
    assert_eq!(s.run(&st)["pending"], 2);
    // pref が入った（enrich が地理院の結果を書いた）写真は候補から外れ、分類行は残る（読む側が pref を優先する）
    s.sql("UPDATE photos SET pref='東京都' WHERE id=2");
    assert_eq!(s.run(&st)["pending"], 0);
    assert_eq!(s.row_count(), 2);
}

#[test]
fn photos_at_the_same_coordinate_are_classified_together() {
    let s = setup(true);
    for id in 1..=5 {
        s.photo(id, US1.0, US1.1, None);
    }
    let (st, n) = stage("v1");
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"]), (5, 5));
    assert_eq!(loads(&n), 1);
    assert!(
        s.rows()
            .iter()
            .all(|r| r.2.as_deref() == Some("ne:USA-3513"))
    );
}

// ---------------------------------------------------------------- 適用の照合

fn apply_with_interruption(
    s: &Setup,
    st: &Regions,
    interrupt: impl FnOnce(&Writer) + Send + 'static,
) -> HashMap<&'static str, u64> {
    let other = s.writer.clone();
    let writer = s.writer.interrupt_next_call(move || interrupt(&other));
    let read = s.read(Mode::Apply);
    match s
        .run_with(st, Mode::Apply, &read, &writer, &StopFlag::new())
        .unwrap()
    {
        StageOutcome::Done(c) => map(&c),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_change_between_the_plan_and_the_apply_is_not_written() {
    let s = setup(true);
    for id in 1..=4 {
        s.photo(id, US1.0, US1.1, None);
    }
    let (st, _) = stage("v1");
    let c = apply_with_interruption(&s, &st, |w| {
        // 1: 座標が変わる  2: 削除される  3: pref が埋まる  4: 変わらない
        w.call(|c| {
            c.execute_batch(
                "UPDATE photos SET lat=5.0, lon=25.0 WHERE id=1;
                 DELETE FROM photos WHERE id=2;
                 UPDATE photos SET pref='東京都' WHERE id=3;",
            )
        })
        .unwrap();
    });
    assert_eq!(c["pending"], 4);
    assert_eq!(c["stale_input"], 3);
    assert_eq!(c["inside"], 1);
    assert_eq!(s.rows().iter().map(|r| r.0).collect::<Vec<_>>(), vec![4]);
    // 条件が変わった行は次のサイクルの候補（座標が変わった 1 だけ。削除・pref ありは対象外）
    let c = s.run(&st);
    assert_eq!(c["pending"], 1);
    let rows = s.rows();
    assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 4]);
    assert_eq!(rows[0].1.as_deref(), Some("ne:AUS"));
}

#[test]
fn a_stale_old_row_is_never_valid_after_the_coordinate_moves() {
    let s = setup(true);
    s.photo(1, US1.0, US1.1, None);
    let (st, _) = stage("v1");
    s.run(&st);
    // 座標が動く。再分類の前でも、有効な分類として JOIN されない（版は fake の "v1" なので今の同梱版でもない）
    s.sql("UPDATE photos SET lon=25.0 WHERE id=1");
    let conn = s.read(Mode::Apply);
    let valid: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM photos p {} WHERE r.photo_id IS NOT NULL",
                join("p", "r")
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(valid, 0);
}

#[test]
fn an_apply_failure_keeps_earlier_batches_and_resumes_next_cycle() {
    let s = setup(true);
    for id in 1..=4 {
        s.photo(id, US1.0, US1.1, None);
    }
    s.sql(
        "CREATE TRIGGER boom BEFORE INSERT ON photo_regions WHEN NEW.photo_id=3
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    );
    let (st, _) = stage("v1");
    let st = st.with_limits(100, Duration::from_secs(60), 2);
    // 1〜2 のバッチは確定し、3〜4 のバッチが失敗する
    let e = s.try_run(&st).expect_err("適用に失敗");
    assert!(e.to_string().contains("failed=2"), "{e}");
    assert_eq!(s.rows().iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 2]);
    s.sql("DROP TRIGGER boom");
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"]), (2, 2));
    assert_eq!(s.row_count(), 4);
}

// ---------------------------------------------------------------- 掃除

#[test]
fn rows_of_deleted_photos_are_removed_in_short_batches() {
    let s = setup(true);
    for id in 1..=3 {
        s.photo(id, US1.0, US1.1, None);
    }
    let (st, _) = stage("v1");
    s.run(&st);
    // 250 行の孤児（写真が無い）を足す。掃除は 100 行ずつ
    s.sql(
        "WITH RECURSIVE n(i) AS (SELECT 1000 UNION ALL SELECT i+1 FROM n WHERE i<1249)
         INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
         SELECT i, 1.0, 1.0, 'v1', 'outside', 'outside', '2026-01-01 00:00:00' FROM n;",
    );
    assert_eq!(s.row_count(), 253);
    s.sql("DELETE FROM photos WHERE id=2");
    let c = s.run(&st);
    assert_eq!(c["removed"], 251);
    assert_eq!(c["pending"], 0);
    assert_eq!(s.rows().iter().map(|r| r.0).collect::<Vec<_>>(), vec![1, 3]);
}

#[test]
fn cleanup_stops_at_the_cycle_limit_and_continues_next_time() {
    let s = setup(true);
    s.sql(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<350)
         INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
         SELECT i, 1.0, 1.0, 'v1', 'outside', 'outside', '2026-01-01 00:00:00' FROM n;",
    );
    let (st, _) = stage("v1");
    let st = st.with_limits(200, Duration::from_secs(60), 100);
    assert_eq!(s.run(&st)["removed"], 200);
    assert_eq!(s.row_count(), 150);
    assert_eq!(s.run(&st)["removed"], 150);
    assert_eq!(s.row_count(), 0);
}

#[test]
fn the_dry_run_cleanup_count_follows_the_cycle_limit_like_apply() {
    let s = setup(true);
    s.sql(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<350)
         INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
         SELECT i, 1.0, 1.0, 'v1', 'outside', 'outside', '2026-01-01 00:00:00' FROM n;",
    );
    // 孤児 350 行、1 サイクル 250 行（1 回の削除 100 行の倍数ではない）: dry-run も実行も 250 行
    let (st, _) = stage("v1");
    let st = st.with_limits(250, Duration::from_secs(60), 100);
    assert_eq!(s.dry_run(&st)["removed"], 250);
    assert_eq!(s.row_count(), 350, "dry-run は消さない");
    assert_eq!(s.run(&st)["removed"], 250);
    assert_eq!(s.row_count(), 100);
    // 上限より少なければ、あるだけ
    assert_eq!(s.dry_run(&st)["removed"], 100);
    assert_eq!(s.run(&st)["removed"], 100);
    assert_eq!(s.row_count(), 0);
}

// ---------------------------------------------------------------- 判定できなかった座標（failed）

/// 国 `ne:ZZZ`（名称表に無い）の領域 x[80,90] を足した世界。そこに入る座標は国は決まるが名称表と合わず、判定できない。
fn broken_world() -> Result<super::RegionIndex, super::data::DataError> {
    synth(
        &[
            ("ne:USA", vec![solid(rect(0.0, 0.0, 10.0, 10.0))]),
            ("ne:ZZZ", vec![solid(rect(80.0, 0.0, 90.0, 10.0))]),
        ],
        &[],
    )
}

fn broken_stage(version: &str) -> Regions {
    Regions::with_index(version, broken_world)
}

const BAD: (&str, &str) = ("5.0", "85.0");

fn valid_join_ids(s: &Setup) -> Vec<i64> {
    let conn = s.read(Mode::Apply);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT p.id FROM photos p {} WHERE r.photo_id IS NOT NULL ORDER BY p.id",
            join("p", "r")
        ))
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn an_unclassifiable_coordinate_is_recorded_as_failed_and_not_selected_again() {
    let s = setup(true);
    s.photo(1, BAD.0, BAD.1, None);
    s.photo(2, BAD.0, BAD.1, None); // 同じ座標
    s.photo(3, US1.0, US1.1, None);
    // 読み取りの JOIN は同梱の分類版で照合するので、この試験は同梱の版で保存する
    let version = super::classifier_version().unwrap();
    let st = broken_stage(version);
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["failed"]), (3, 1, 2));
    assert_eq!(c["deferred"], 0);
    let rows = s.rows();
    let failed: Vec<_> = rows.iter().filter(|r| r.3 == "failed").collect();
    assert_eq!(failed.len(), 2);
    for r in failed {
        // 国・地域・検索用文字列は空、版は今の版、判定方法は国も地域も failed
        assert_eq!((r.1.as_deref(), r.2.as_deref()), (None, None));
        assert_eq!(
            (r.4.as_str(), r.5.as_str(), r.6.as_str()),
            ("failed", version, "")
        );
    }
    // 読み取り側は failed の行を有効な分類として引かない（= 国が決まらない `unclassified`）
    assert_eq!(valid_join_ids(&s), vec![3]);
    // 候補ではなくなり、分類待ちにも数えない
    let c = s.run(&st);
    assert_eq!((c["pending"], c["failed"], c["inside"]), (0, 0, 0));
    assert_eq!(super::pending_photos(&s.read(Mode::Apply)).unwrap(), 0);
    // dry-run も同じ（失敗した行は候補に戻らない）
    assert_eq!(s.dry_run(&st)["pending"], 0);
}

#[test]
fn a_failed_coordinate_is_retried_when_the_version_or_the_coordinate_changes() {
    let s = setup(true);
    s.photo(1, BAD.0, BAD.1, None);
    s.photo(2, BAD.0, BAD.1, None);
    assert_eq!(s.run(&broken_stage("v1"))["failed"], 2);
    assert_eq!(s.run(&broken_stage("v1"))["pending"], 0);
    // 版が上がれば選び直す（直った索引なら分類できる）
    let (st, _) = stage("v2");
    s.sql("UPDATE photos SET lat=5.0, lon=2.0 WHERE id=2");
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["unresolved"]), (2, 1, 1));
    let rows = s.rows();
    assert!(rows.iter().all(|r| r.5 == "v2"), "{rows:?}");
    // 座標が動いた写真は、版が同じでも選び直す
    let s2 = setup(true);
    s2.photo(1, BAD.0, BAD.1, None);
    assert_eq!(s2.run(&broken_stage("v1"))["failed"], 1);
    s2.sql("UPDATE photos SET lat=5.0, lon=2.0 WHERE id=1");
    let c = s2.run(&broken_stage("v1"));
    assert_eq!((c["pending"], c["inside"]), (1, 1));
}

#[test]
fn permanent_failures_at_the_front_do_not_block_the_photos_behind_them() {
    let s = setup(true);
    // 先頭 5 枚が恒久的に判定できない。1 サイクル 5 枚でも、次のサイクルでは後ろへ進む
    for id in 1..=5 {
        s.photo(id, BAD.0, BAD.1, None);
    }
    for id in 6..=8 {
        s.photo(id, US1.0, US1.1, None);
    }
    let st = broken_stage("v1").with_limits(5, Duration::from_secs(60), 2);
    let c = s.run(&st);
    assert_eq!(
        (c["pending"], c["failed"], c["inside"], c["deferred"]),
        (8, 5, 0, 3)
    );
    let c = s.run(&st);
    assert_eq!(
        (c["pending"], c["failed"], c["inside"], c["deferred"]),
        (3, 0, 3, 0)
    );
    assert_eq!(s.run(&st)["pending"], 0);
}

#[test]
fn failed_rows_are_dropped_when_a_photo_changes_before_the_apply() {
    let s = setup(true);
    s.photo(1, BAD.0, BAD.1, None);
    s.photo(2, BAD.0, BAD.1, None);
    // 判定と適用のあいだに写真 2 の座標が動く: failed の行も照合で捨て、stale_input に数える
    let st = broken_stage("v1");
    let c = apply_with_interruption(&s, &st, |w| {
        w.call(|c| c.execute("UPDATE photos SET lat=6.0 WHERE id=2", []))
            .unwrap();
    });
    assert_eq!((c["failed"], c["stale_input"]), (1, 1));
    assert_eq!(s.rows().iter().map(|r| r.0).collect::<Vec<_>>(), vec![1]);
}

#[test]
fn a_dry_run_counts_failures_without_writing() {
    let s = setup(true);
    s.photo(1, BAD.0, BAD.1, None);
    s.photo(2, US1.0, US1.1, None);
    let c = s.dry_run(&broken_stage("v1"));
    assert_eq!((c["pending"], c["inside"], c["failed"]), (2, 1, 1));
    assert_eq!(s.row_count(), 0);
}

#[test]
fn the_failed_mark_is_a_valid_value_of_the_table_but_other_values_are_not() {
    let s = setup(true);
    let insert = |m: &str| {
        let m = m.to_string();
        s.writer.call(move |c| {
            c.execute(
                "INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
                 VALUES(1,1.0,1.0,'v',?1,?1,'2026-01-01 00:00:00')",
                [m],
            )
        })
    };
    assert!(insert("nonsense").is_err());
    assert!(insert("failed").is_ok());
}

// ---------------------------------------------------------------- 上限・停止・再開

#[test]
fn the_per_cycle_limit_leaves_the_rest_for_later() {
    let s = setup(true);
    for id in 1..=12 {
        s.photo(id, US1.0, US1.1, None);
    }
    let (st, _) = stage("v1");
    let st = st.with_limits(5, Duration::from_secs(60), 2);
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (12, 5, 7));
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (7, 5, 2));
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (2, 2, 0));
    assert_eq!(s.row_count(), 12);
    assert_eq!(s.run(&st)["pending"], 0);
}

#[test]
fn the_time_limit_is_checked_before_each_batch_but_the_first_batch_always_runs() {
    let s = setup(true);
    for id in 1..=7 {
        s.photo(id, US1.0, US1.1, None);
    }
    let (st, _) = stage("v1");
    // 経過時間が最初から上限を過ぎている: 1 バッチ（3 枚）だけ適用して残りは次回
    let st = st
        .with_limits(100, Duration::from_secs(5), 3)
        .with_clock(|_| Duration::from_secs(6));
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (7, 3, 4));
    assert_eq!(s.row_count(), 3);
    // 時間の上限に余裕があれば 1 サイクルで終わる（残りの 4 枚）
    let (st, _) = stage("v1");
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (4, 4, 0));
    assert_eq!(s.row_count(), 7);
}

#[test]
fn a_stop_signal_keeps_applied_batches_and_the_rest_resumes() {
    let s = setup(true);
    for id in 1..=7 {
        s.photo(id, US1.0, US1.1, None);
    }
    let stop = StopFlag::new();
    let signal = stop.clone();
    let (st, n) = stage("v1");
    let st = st.with_limits(100, Duration::from_secs(60), 3);
    // 1 つ目のバッチを書く直前に停止の合図が立つ。書き込みは最後まで済み、次のバッチの前で止まる
    let writer = s.writer.interrupt_next_call(move || signal.stop());
    let read = s.read(Mode::Apply);
    let StageOutcome::Done(c) = s.run_with(&st, Mode::Apply, &read, &writer, &stop).unwrap() else {
        panic!("完了のはず");
    };
    let c = map(&c);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (7, 3, 4));
    assert_eq!(s.row_count(), 3);
    assert_eq!(loads(&n), 1);
    // 再起動のあと（新しい段・停止の合図なし）に残りを処理する
    let (st, _) = stage("v1");
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"]), (4, 4));
    assert_eq!(s.row_count(), 7);
}

#[test]
fn a_stop_before_the_run_does_not_build_the_index() {
    let s = setup(true);
    s.photo(1, US1.0, US1.1, None);
    let stop = StopFlag::new();
    stop.stop();
    let (st, n) = stage("v1");
    let read = s.read(Mode::Apply);
    let StageOutcome::Done(c) = s
        .run_with(&st, Mode::Apply, &read, &s.writer, &stop)
        .unwrap()
    else {
        panic!("完了のはず");
    };
    assert_eq!(map(&c)["pending"], 1);
    assert_eq!(loads(&n), 0);
    assert_eq!(s.row_count(), 0);
}

#[test]
fn a_broken_data_set_fails_with_a_fixed_message() {
    let s = setup(true);
    s.photo(1, "12.345678", "98.765432", None);
    let st = Regions::with_index("v1", || {
        Err(super::data::DataError::new(
            "12.345678 98.765432 の読み込み失敗",
        ))
    });
    let e = s.try_run(&st).expect_err("索引を作れない");
    let text = e.to_string();
    assert!(
        !text.contains("12.345") && !text.contains("98.765"),
        "{text}"
    );
    assert_eq!(s.row_count(), 0);
}

// ---------------------------------------------------------------- dry-run

#[test]
fn dry_run_counts_like_apply_and_writes_nothing() {
    let s = setup(true);
    s.photo(1, US1.0, US1.1, None);
    s.photo(2, SEA.0, SEA.1, None);
    s.photo(3, AMBIGUOUS.0, AMBIGUOUS.1, None);
    s.photo(4, US2.0, US2.1, Some("東京都"));
    let (st, n) = stage("v1");
    s.sql("INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
           VALUES(900,1.0,1.0,'v1','outside','outside','2026-01-01 00:00:00')");
    let before = s.rows();
    let dry = s.dry_run(&st);
    assert_eq!(dry["pending"], 3);
    assert_eq!((dry["inside"], dry["unresolved"]), (1, 2));
    assert_eq!(dry["removed"], 1, "消すはずの孤児を数える");
    assert!(!dry.contains_key("stale_input"));
    assert_eq!(loads(&n), 1);
    assert_eq!(s.rows(), before, "dry-run は表を書かない");
    // 実行すると dry-run が数えたとおりになる
    let run = s.run(&st);
    assert_eq!(
        (
            run["pending"],
            run["inside"],
            run["unresolved"],
            run["removed"]
        ),
        (3, 1, 2, 1)
    );
    // 済んだあとの dry-run は処理待ち 0
    assert_eq!(s.dry_run(&st)["pending"], 0);
}

#[test]
fn dry_run_without_the_table_counts_everything_and_creates_no_table() {
    let s = setup(false);
    s.photo(1, US1.0, US1.1, None);
    s.photo(2, AUS.0, AUS.1, Some(""));
    s.photo(3, SEA.0, SEA.1, None);
    s.photo(4, "'bad'", "1.0", None);
    s.photo(5, FRA_ONLY.0, FRA_ONLY.1, Some("大阪府"));
    let (st, n) = stage("v1");
    let read = s.read(Mode::DryRun);
    let StageOutcome::Done(c) = s
        .run_with(&st, Mode::DryRun, &read, &s.writer, &StopFlag::new())
        .unwrap()
    else {
        panic!("完了のはず");
    };
    let c = map(&c);
    assert_eq!(c["pending"], 3);
    assert_eq!((c["inside"], c["unresolved"]), (2, 1));
    assert_eq!(c["removed"], 0);
    assert_eq!(loads(&n), 1);
    // メモリ上の写しにも、実 DB にも表を作らない
    assert!(!available(&read).unwrap());
    assert!(!available(&s.read(Mode::Apply)).unwrap());
}

#[test]
fn dry_run_with_no_candidates_does_not_build_the_index() {
    let s = setup(true);
    s.photo(1, US1.0, US1.1, Some("東京都"));
    let (st, n) = stage("v1");
    assert_eq!(s.dry_run(&st)["pending"], 0);
    assert_eq!(loads(&n), 0);
}

// ---------------------------------------------------------------- 候補の走査の上限

/// 写真 1〜`done` は有効な分類が済み、`done+1`〜`done+todo` が候補。
fn photos_with_leading_classified(s: &Setup, done: i64, todo: i64) {
    s.sql(&format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{})
         INSERT INTO photos(id, source, name, lat, lon, pref) SELECT i, 'drive', 'p'||i, 5.0, 2.0, NULL FROM n;
         INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
         SELECT id, lat, lon, 'v1', 'outside', 'outside', '2026-01-01 00:00:00' FROM photos WHERE id<={done};",
        done + todo
    ));
}

#[test]
fn the_time_limit_also_cuts_the_candidate_scan_after_the_first_batch_is_found() {
    let s = setup(true);
    photos_with_leading_classified(&s, 40, 20);
    let (st, _) = stage("v1");
    // 経過時間は最初から上限を過ぎている。走査は 1 バッチ目（3 枚）の候補が揃うまで続き、そこで中断する
    let st = st
        .with_limits(100, Duration::from_secs(5), 3)
        .with_clock(|_| Duration::from_secs(6))
        .with_progress_ops(1);
    let c = s.run(&st);
    assert_eq!(c["inside"], 3, "{c:?}");
    assert!(c["pending"] < 20, "走査が打ち切られていない: {c:?}");
    // 打ち切ったので、pending は下限であることを件数で示す
    assert_eq!(c["scan_incomplete"], 1, "{c:?}");
    assert_eq!(s.row_count(), 43);
    // 時間に余裕があれば、残りは次の通常の同期で全部処理する
    let (st, _) = stage("v1");
    let st = st
        .with_limits(100, Duration::from_secs(60), 3)
        .with_progress_ops(1);
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"], c["deferred"]), (17, 17, 0));
    assert_eq!(c["scan_incomplete"], 0, "{c:?}");
    assert_eq!(s.row_count(), 60);
}

#[test]
fn a_dry_run_also_reports_an_incomplete_scan() {
    let s = setup(true);
    photos_with_leading_classified(&s, 40, 20);
    let (st, _) = stage("v1");
    let cut = st
        .with_limits(100, Duration::from_secs(5), 3)
        .with_clock(|_| Duration::from_secs(6))
        .with_progress_ops(1);
    let c = s.dry_run(&cut);
    assert_eq!(c["scan_incomplete"], 1, "{c:?}");
    assert!(c["pending"] < 20, "{c:?}");
    assert_eq!(s.row_count(), 40, "dry-run は書かない");
    let (st, _) = stage("v1");
    let c = s.dry_run(&st);
    assert_eq!((c["pending"], c["scan_incomplete"]), (20, 0), "{c:?}");
}

#[test]
fn the_scan_keeps_going_until_the_first_batch_exists_even_when_out_of_time() {
    // 分類済みの写真が 200 枚続いても、候補が 1 枚でもあれば進む（毎回走査だけで時間切れにならない）
    let s = setup(true);
    photos_with_leading_classified(&s, 200, 2);
    let (st, _) = stage("v1");
    let st = st
        .with_limits(100, Duration::from_secs(5), 100)
        .with_clock(|_| Duration::from_secs(60))
        .with_progress_ops(1);
    let c = s.run(&st);
    assert_eq!(c["inside"], 2, "{c:?}");
}

#[test]
fn a_stop_signal_interrupts_the_candidate_scan_and_the_read_connection_stays_usable() {
    let s = setup(true);
    photos_with_leading_classified(&s, 40, 20);
    let stop = StopFlag::new();
    stop.stop();
    let (st, n) = stage("v1");
    let st = st
        .with_limits(100, Duration::from_secs(60), 3)
        .with_progress_ops(1);
    let read = s.read(Mode::Apply);
    let StageOutcome::Done(c) = s
        .run_with(&st, Mode::Apply, &read, &s.writer, &stop)
        .unwrap()
    else {
        panic!("完了のはず");
    };
    let c = map(&c);
    assert_eq!((c["inside"], c["unresolved"]), (0, 0), "{c:?}");
    assert_eq!(c["scan_incomplete"], 1, "{c:?}");
    assert_eq!(loads(&n), 0);
    assert_eq!(s.row_count(), 40);
    // 進捗ハンドラは残らない: 同じ接続で次の問い合わせが最後まで進む
    let n: i64 = read
        .query_row("SELECT COUNT(*) FROM photos p WHERE p.id>0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 60);
}

// ---------------------------------------------------------------- ログ（AC12）

#[test]
fn logs_and_errors_carry_only_counts_and_fixed_reasons() {
    let s = setup(true);
    // 件数と紛れない値・実在の国の名前にあたる座標
    s.photo(777777, "5.123456", "2.654321", None);
    s.photo(888888, "5.0", "25.0", None);
    let (st, _) = stage("v1");
    let stop = StopFlag::new();
    let http = crate::net::agent();
    let read = s.read(Mode::Apply);
    let logs = crate::testutil::capture::logs_of(|| {
        let ctx = StageCtx {
            mode: Mode::Apply,
            config: &s.config,
            read: &read,
            writer: Some(&s.writer),
            cache: &s.cache,
            http: &http,
            stop: &stop,
        };
        let stages: Vec<Box<dyn Stage>> = vec![Box::new(st)];
        let report = crate::sync::run_cycle(&stages, &ctx);
        assert_eq!(report.failed(), 0);
        // 失敗の経路（索引を作れない）も
        s.photo(999999, "5.5", "2.5", None);
        let broken: Vec<Box<dyn Stage>> = vec![Box::new(Regions::with_index("v1", || {
            Err(super::data::DataError::new("5.5 2.5 ne:USA アメリカ"))
        }))];
        let report = crate::sync::run_cycle(&broken, &ctx);
        assert_eq!(report.failed(), 1);
    });
    assert!(logs.contains("regions"), "{logs}");
    assert!(logs.contains("pending=2"), "{logs}");
    for needle in [
        "777777",
        "888888",
        "999999",
        "5.123",
        "2.654",
        "25.0",
        "5.5",
        "ne:USA",
        "ne:AUS",
        "アメリカ",
        "オーストラリア",
        "p777777",
    ] {
        assert!(!logs.contains(needle), "ログに {needle} が出ている: {logs}");
    }
}

#[test]
fn pending_photos_counts_what_the_stage_would_classify() {
    // 表が無い旧 DB: pref 空で座標が有効な全件
    let s = setup(false);
    s.photo(1, "35.0", "139.0", None);
    s.photo(2, "35.0", "139.0", Some("東京都"));
    s.photo(3, "'x'", "1.0", None);
    assert_eq!(super::pending_photos(&s.read(Mode::Apply)).unwrap(), 1);
    // 表があり、今の分類版で分類済みなら 0。座標が動けば 1
    let s = setup(true);
    s.photo(1, "5.0", "2.0", None);
    assert_eq!(super::pending_photos(&s.read(Mode::Apply)).unwrap(), 1);
    s.run(&Regions::new());
    assert_eq!(super::pending_photos(&s.read(Mode::Apply)).unwrap(), 0);
    s.sql("UPDATE photos SET lon=7.0 WHERE id=1");
    assert_eq!(super::pending_photos(&s.read(Mode::Apply)).unwrap(), 1);
}

/// 段は通信もキャッシュも使わない（dry-run が通信 0 で永続キャッシュに書かないことの、構造の確認）。`StageCtx` の
/// `http`・`cache` を段のコードが参照していないこと。
#[test]
fn the_stage_never_touches_the_network_or_the_cache() {
    let source = include_str!("stage.rs");
    for needle in [".http", ".cache", "ureq", "std::net", "TcpStream"] {
        assert!(
            !source.contains(needle),
            "stage.rs が {needle} を使っている"
        );
    }
}

// ---------------------------------------------------------------- 同梱データ

#[test]
fn the_bundled_data_classifies_public_landmarks_in_the_default_stage() {
    let s = setup(true);
    // 公開の地点（エッフェル塔・シアトルのスペースニードル）と、pref のある写真
    s.photo(1, "48.858370", "2.294481", None);
    s.photo(2, "47.620500", "-122.349300", Some(""));
    s.photo(3, "35.681236", "139.767125", Some("東京都"));
    let st = Regions::new();
    assert_eq!(st.name(), "regions");
    let c = s.run(&st);
    assert_eq!((c["pending"], c["inside"]), (2, 2));
    let rows = s.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1.as_deref(), Some("ne:FRA"));
    assert_eq!(rows[1].1.as_deref(), Some("ne:USA"));
    assert_eq!(rows[0].5, super::classifier_version().unwrap());
    // 同梱の版で保存した行は、読み取り側の有効な分類として JOIN される
    let conn = s.read(Mode::Apply);
    let valid: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM photos p {} WHERE r.photo_id IS NOT NULL",
                join("p", "r")
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(valid, 2);
    assert_eq!(s.run(&st)["pending"], 0);
}

// ---------------------------------------------------------------- 計測

/// 計測（設計「テスト計画」: 1 万件程度の合成写真で 1 サイクルの時間と writer 待ちを測る）。通常の `cargo test` では回さない:
///
/// ```text
/// cargo test --release -p geoframe --lib regions::stage_tests::measure -- --ignored --nocapture
/// ```
///
/// 座標は決まった乱数の合成値（実際の撮影座標は使わない）。`writer` 待ちは、段が動いている間に別のスレッドが小さな書き込みを
/// 繰り返し、その応答時間（= 他の書き手が 1 バッチ分待たされる時間）を測る。
#[test]
#[ignore = "計測。単独で回す"]
fn measure_one_cycle_of_ten_thousand_photos() {
    use std::time::Instant;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }
    // 国の頂点の近傍の点（陸地寄り）を半分、世界一様の点を半分
    let index = super::RegionIndex::load().unwrap();
    let verts: Vec<[f64; 2]> = index
        .polygons()
        .countries
        .rings()
        .flatten()
        .copied()
        .collect();
    let mut rng = Lcg(7);
    let points: Vec<(f64, f64)> = (0..10_000)
        .map(|i| {
            if i % 2 == 0 {
                (rng.next() * 180.0 - 90.0, rng.next() * 360.0 - 180.0)
            } else {
                let v = verts[(rng.next() * verts.len() as f64) as usize];
                (
                    (v[1] + (rng.next() - 0.5) * 0.2).clamp(-90.0, 90.0),
                    (v[0] + (rng.next() - 0.5) * 0.2).clamp(-180.0, 180.0),
                )
            }
        })
        .collect();
    drop((index, verts));

    let s = setup(true);
    s.writer
        .call(move |conn| {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO photos(id, source, name, lat, lon, pref) VALUES(?1, 'drive', 'p', ?2, ?3, NULL)",
                )?;
                for (i, (lat, lon)) in points.iter().enumerate() {
                    stmt.execute(rusqlite::params![i as i64 + 1, lat, lon])?;
                }
            }
            tx.commit()
        })
        .unwrap();

    // 段が動く間、別のスレッドが小さな書き込みを繰り返して待ち時間を測る
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = {
        let (writer, stop) = (s.writer.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut waits = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                let t = Instant::now();
                writer
                    .call(|c| c.execute("UPDATE photos SET favorite=favorite WHERE id=1", []))
                    .unwrap();
                waits.push(t.elapsed());
                std::thread::sleep(Duration::from_micros(200));
            }
            waits
        })
    };
    let st = Regions::new();
    let t = Instant::now();
    let c = s.run(&st);
    let cycle = t.elapsed();
    stop.store(true, Ordering::SeqCst);
    let mut waits = probe.join().unwrap();
    waits.sort();
    let pct = |p: f64| waits[((waits.len() - 1) as f64 * p) as usize];
    println!(
        "1 サイクル（1 万枚、索引の作成を含む）: {:.0} ms",
        cycle.as_secs_f64() * 1e3
    );
    println!("件数: {c:?}");
    println!(
        "writer 待ち（{} 回）: 中央値 {:.2} ms、99% {:.2} ms、最大 {:.2} ms",
        waits.len(),
        pct(0.5).as_secs_f64() * 1e3,
        pct(0.99).as_secs_f64() * 1e3,
        waits.last().unwrap().as_secs_f64() * 1e3
    );
    // 2 回目（候補 0 件）と dry-run
    let t = Instant::now();
    let c2 = s.run(&st);
    println!(
        "2 回目（候補 0 件、索引なし）: {:.1} ms {c2:?}",
        t.elapsed().as_secs_f64() * 1e3
    );
    s.sql("DELETE FROM photo_regions");
    let t = Instant::now();
    let d = s.dry_run(&st);
    println!(
        "dry-run（1 万枚）: {:.0} ms {d:?}",
        t.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(c["pending"], 10_000);
    assert_eq!(c["failed"], 0);
}

/// 計測（大規模 DB の候補選択）。写真 `n` 枚（`pref` 空・座標有効）のうち、先頭の `classified` 枚は有効な分類が済み、残りが候補。
///
/// ```text
/// cargo test --release -p geoframe --lib regions::stage_tests::measure_candidate_scan -- --ignored --nocapture
/// ```
#[test]
#[ignore = "計測。単独で回す"]
fn measure_candidate_scan() {
    use std::time::Instant;
    for (label, n, classified) in [
        ("20 万枚: 全件が候補（初回移行）", 200_000i64, 0i64),
        ("20 万枚: 15 万枚が分類済み、5 万枚が候補", 200_000, 150_000),
        ("20 万枚: 全件が分類済み（定常状態）", 200_000, 200_000),
        ("100 万枚: 全件が分類済み（定常状態）", 1_000_000, 1_000_000),
    ] {
        let s = setup(true);
        s.writer
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute_batch(&format!(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{n})
                     INSERT INTO photos(id, source, name, lat, lon, pref, md5, mtime, taken_at)
                     SELECT i, 'drive', 'p'||i, (i%1700)/10.0-85.0, (i%3500)/10.0-175.0, NULL, 'x'||i, 1.0, '2026-01-01 00:00'
                       FROM n;
                     INSERT INTO photo_regions(photo_id,input_lat,input_lon,classifier_version,country_method,region_method,classified_at)
                     SELECT id, lat, lon, 'v1', 'outside', 'outside', '2026-01-01 00:00:00' FROM photos WHERE id<={classified};"
                ))?;
                tx.commit()
            })
            .unwrap();
        let (st, _) = stage("v1");
        let read = s.read(Mode::Apply);
        let t = Instant::now();
        let (c, total, _) = st
            .select(&read, true, "'v1'", Instant::now(), &StopFlag::new())
            .unwrap();
        println!(
            "{label}: 候補走査 {:.0} ms（候補 {} 枚、総数 {total}）",
            t.elapsed().as_secs_f64() * 1e3,
            c.len()
        );
        let t = Instant::now();
        let c = s.run(&st);
        println!(
            "  1 サイクル（索引は合成）: {:.0} ms {c:?}",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
}
