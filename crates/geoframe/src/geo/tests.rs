//! `infer`・`enrich` の単体テストと、Python の `infer_locations`・`enrich_rows` との突き合わせ。
//! 地理院はループバックのスタブ（[`super::stub`]）。座標・地名は架空か公開の代表点。実写真由来の値は使わない。

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use super::enrich::{GsiEndpoints, parse_muni_js};
use super::stub::{Kind, Reply, StubGsi};
use super::{Enrich, Infer, naive_seconds};
use crate::backoff::FailureMemo;
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

fn setup_with(tag: &str, env: &[(&str, &str)]) -> Setup {
    let dir = TempDir::new(tag);
    let overrides = Overrides {
        listen: None,
        data_dir: Some(dir.path().to_path_buf()),
    };
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let config = Config::load(|k| env.get(k).cloned(), &overrides).unwrap();
    let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
    writer.call(crate::schema::migrate).unwrap();
    let cache = Cache::load(dir.path());
    Setup {
        _dir: dir,
        config,
        writer,
        cache,
    }
}

fn setup(tag: &str) -> Setup {
    setup_with(tag, &[])
}

/// 行の挿入（`id`・`source`・`name` は固定の形）。`None` は NULL。
fn photo(
    id: i64,
    taken_at: Option<&str>,
    pos: Option<(f64, f64)>,
    loc_source: Option<&str>,
) -> String {
    let lit = |v: Option<String>| v.unwrap_or_else(|| "NULL".to_string());
    format!(
        "INSERT INTO photos(id, source, name, taken_at, lat, lon, loc_source)
         VALUES({id}, 'drive', 'p{id}', {}, {}, {}, {})",
        lit(taken_at.map(|t| format!("'{t}'"))),
        lit(pos.map(|p| format!("{:?}", p.0))),
        lit(pos.map(|p| format!("{:?}", p.1))),
        lit(loc_source.map(|s| format!("'{s}'"))),
    )
}

const TOKYO: (f64, f64) = (35.681236, 139.767125);
const SAPPORO: (f64, f64) = (43.064301, 141.346932);
const PARIS: (f64, f64) = (48.858370, 2.294481);

impl Setup {
    fn sql(&self, sql: &str) {
        let sql = sql.to_string();
        self.writer.call(move |c| c.execute_batch(&sql)).unwrap();
    }

    fn read(&self, mode: Mode) -> Connection {
        let real = crate::db::open_read_only(&self.config.db_path()).unwrap();
        match mode {
            Mode::Apply => real,
            Mode::DryRun => crate::sync::simulation_copy(&real).unwrap(),
        }
    }

    fn run_in(
        &self,
        stage: &dyn Stage,
        mode: Mode,
        read: &Connection,
        stop: &StopFlag,
    ) -> Result<StageOutcome, StageError> {
        let http = crate::net::agent();
        let ctx = StageCtx {
            mode,
            config: &self.config,
            read,
            writer: (mode == Mode::Apply).then_some(&self.writer),
            cache: &self.cache,
            http: &http,
            stop,
        };
        stage.run(&ctx)
    }

    fn run(&self, stage: &dyn Stage) -> Counts {
        let read = self.read(Mode::Apply);
        match self
            .run_in(stage, Mode::Apply, &read, &StopFlag::new())
            .unwrap()
        {
            StageOutcome::Done(counts) => counts,
            other => panic!("{other:?}"),
        }
    }

    fn try_run(&self, stage: &dyn Stage) -> Result<StageOutcome, StageError> {
        let read = self.read(Mode::Apply);
        self.run_in(stage, Mode::Apply, &read, &StopFlag::new())
    }

    /// `--dry-run` で、指定の段を順に流す（同じ写しを共有する）。最後の段の件数を返す。
    fn dry_run(&self, stages: &[&dyn Stage]) -> Counts {
        let read = self.read(Mode::DryRun);
        let mut last = Counts::default();
        for stage in stages {
            last = match self
                .run_in(*stage, Mode::DryRun, &read, &StopFlag::new())
                .unwrap()
            {
                StageOutcome::Done(counts) => counts,
                other => panic!("{other:?}"),
            };
        }
        last
    }

    /// `photos` の全行を、列名 → JSON の値で。
    fn rows(&self) -> Vec<Map<String, Value>> {
        rows_of(&self.read(Mode::Apply))
    }

    fn row(&self, id: i64) -> Map<String, Value> {
        self.rows()
            .into_iter()
            .find(|r| r["id"] == json!(id))
            .unwrap_or_else(|| panic!("行が無い: {id}"))
    }
}

fn rows_of(conn: &Connection) -> Vec<Map<String, Value>> {
    let mut stmt = conn.prepare("SELECT * FROM photos ORDER BY id").unwrap();
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let mut map = Map::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i).unwrap() {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(n) => Value::from(n),
                rusqlite::types::ValueRef::Real(f) => Value::from(f),
                rusqlite::types::ValueRef::Text(t) => {
                    Value::from(String::from_utf8(t.to_vec()).unwrap())
                }
                rusqlite::types::ValueRef::Blob(_) => Value::from("<blob>"),
            };
            map.insert(name.clone(), value);
        }
        out.push(map);
    }
    out
}

fn get(counts: &Counts, name: &str) -> u64 {
    counts
        .0
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("{name} が無い: {counts}"))
        .1
}

/// 待ちなし・記憶なし（Python と同じ「毎サイクル問い合わせ直す」）の `Enrich`。
fn enrich_for(stub: &StubGsi) -> Enrich {
    Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::with_policy(Duration::ZERO, Duration::ZERO, 16),
    )
}

/// 記憶つき（本番の設定）の `Enrich`。
fn enrich_with_memory(stub: &StubGsi) -> Enrich {
    Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::default(),
    )
}

fn tokyo_replies(stub: &StubGsi) {
    stub.set_rgeo(|_, _| {
        Reply::json(r#"{"results":{"muniCd":"13101","lv01Nm":"テスト町一丁目"}}"#)
    });
    stub.set_elev(|_, _| Reply::json(r#"{"elevation":12.5}"#));
    stub.set_muni(Reply::Text(
        "GSI.MUNI_ARRAY[\"13101\"] = '13,東京都,13101,千代田区';".to_string(),
    ));
}

// ---------------------------------------------------------------- 時刻

#[test]
fn 時刻はstrptimeと同じ規則で読む() {
    // 差だけを取るので、基準は何でもよい。1970-01-01 00:00 が 0
    assert_eq!(naive_seconds("1970-01-01 00:00"), Some(0.0));
    assert_eq!(naive_seconds("2024-05-06 07:08"), Some(1_714_979_280.0));
    // 桁が足りなくても通る（strptime と同じ）。日付と時刻の間の空白は 1 文字以上
    assert_eq!(
        naive_seconds("2024-5-6 7:8"),
        naive_seconds("2024-05-06 07:08")
    );
    assert_eq!(
        naive_seconds("2024-05-06   07:08"),
        naive_seconds("2024-05-06 07:08")
    );
    assert_eq!(
        naive_seconds("2024-05-06\t07:08"),
        naive_seconds("2024-05-06 07:08")
    );
    // うるう年
    assert!(naive_seconds("2024-02-29 00:00").is_some());
    assert!(naive_seconds("2023-02-29 00:00").is_none());
    assert!(naive_seconds("1900-02-29 00:00").is_none());
    assert!(naive_seconds("2000-02-29 00:00").is_some());
    // 解釈できないもの
    for bad in [
        "",
        "2024-05-06",
        "2024-05-06 07",
        "2024-05-06 07:08:09",
        "2024-05-06T07:08",
        "2024-05-06 07:08 ",
        " 2024-05-06 07:08",
        "2024-13-06 07:08",
        "2024-00-06 07:08",
        "2024-05-32 07:08",
        "2024-05-00 07:08",
        "2024-04-31 07:08",
        "2024-05-06 24:00",
        "2024-05-06 07:60",
        "0000-01-01 00:00",
        "24-05-06 07:08",
        "20240-05-06 07:08",
        "2024/05/06 07:08",
        "２０２４-０５-０６ ０７:０８",
        "abc",
    ] {
        assert_eq!(naive_seconds(bad), None, "{bad:?}");
    }
    // 暦の境界をまたぐ差
    let a = naive_seconds("2023-12-31 23:59").unwrap();
    let b = naive_seconds("2024-01-01 00:01").unwrap();
    assert_eq!(b - a, 120.0);
    let a = naive_seconds("2024-02-28 12:00").unwrap();
    let b = naive_seconds("2024-03-01 12:00").unwrap();
    assert_eq!(b - a, 2.0 * 86_400.0);
}

// ---------------------------------------------------------------- infer

#[test]
fn 近い順の錨から推定し_窓の境界は含む() {
    let s = setup("geo-infer-window");
    s.sql(&format!(
        "{};{};{};{};{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:15"), None, None), // 900 秒
        photo(3, Some("2024-05-06 22:00"), None, None), // 12 時間ちょうど
        photo(4, Some("2024-05-06 22:01"), None, None), // 12 時間 1 分: 範囲外
        photo(5, Some("2024-05-06 09:59"), None, None), // 前にも後にも効く
        photo(6, None, None, None),
    ));
    let counts = s.run(&Infer);
    assert_eq!(get(&counts, "anchors"), 1);
    assert_eq!(get(&counts, "targets"), 5);
    assert_eq!(get(&counts, "inferred"), 3);
    assert_eq!(get(&counts, "untimed"), 1);
    for (id, dt) in [(2, 900), (3, 43_200), (5, 60)] {
        let row = s.row(id);
        assert_eq!(row["loc_source"], "inferred", "{id}");
        assert_eq!(row["inferred_from"], 1, "{id}");
        assert_eq!(row["infer_dt_s"], dt, "{id}");
        assert_eq!(row["lat"], TOKYO.0, "{id}");
        assert_eq!(row["lon"], TOKYO.1, "{id}");
    }
    assert_eq!(s.row(4)["loc_source"], Value::Null);
    assert_eq!(s.row(6)["loc_source"], Value::Null);
    // 2 回目: 何も変わらない（書かない）
    let again = s.run(&Infer);
    assert_eq!(get(&again, "inferred"), 0);
    assert_eq!(get(&again, "updated"), 0);
    assert_eq!(get(&again, "cancelled"), 0);
    assert_eq!(get(&again, "unchanged"), 3);
}

#[test]
fn 同じ距離の錨はidが小さい方を選ぶ() {
    let s = setup("geo-infer-tie");
    // 後ろの id から先に入れても、選ぶのは id の小さい方
    s.sql(&format!(
        "{};{};{};{}",
        photo(9, Some("2024-05-06 14:00"), Some(PARIS), Some("exif")),
        photo(
            2,
            Some("2024-05-06 12:00"),
            Some(SAPPORO),
            Some("drive_meta")
        ),
        photo(5, Some("2024-05-06 13:00"), None, None), // 2 と 9 から 1 時間ずつ
        photo(3, Some("2024-05-06 12:00"), Some(TOKYO), Some("manual")), // 2 と同じ時刻
    ));
    s.run(&Infer);
    // 距離 3600 の候補は 2（12:00）・3（12:00）・9（14:00）。id が最小の 2
    assert_eq!(s.row(5)["inferred_from"], 2);
    assert_eq!(s.row(5)["lat"], SAPPORO.0);
}

#[test]
fn 時計のずれは推定する写真の時刻にだけ足す() {
    // 補正 -90 分: 10:00 の錨に対し、11:30 の写真は 10:00 とみなされる（差 0）
    let s = setup_with("geo-infer-offset", &[("GEOFRAME_INFER_OFFSET_MIN", "-90")]);
    s.sql(&format!(
        "{};{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 11:30"), None, None),
        photo(3, Some("2024-05-06 10:00"), None, None), // 補正で 08:30（錨の 90 分前）
    ));
    s.run(&Infer);
    assert_eq!(s.row(2)["infer_dt_s"], 0);
    assert_eq!(s.row(3)["infer_dt_s"], 5400);
    // 差の小数部は切り捨て: +0.01 分（0.6 秒）
    let s = setup_with(
        "geo-infer-offset-frac",
        &[("GEOFRAME_INFER_OFFSET_MIN", "0.01")],
    );
    s.sql(&format!(
        "{};{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:10"), None, None), // 600.6 → 600
        photo(3, Some("2024-05-06 09:50"), None, None), // 599.4 → 599
    ));
    s.run(&Infer);
    assert_eq!(s.row(2)["infer_dt_s"], 600);
    assert_eq!(s.row(3)["infer_dt_s"], 599);
}

#[test]
fn 錨が消えたら取り消し_derivativeの列は消し_地名は残す() {
    let s = setup("geo-infer-cancel");
    s.sql(&format!(
        "{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:05"), None, None),
    ));
    s.run(&Infer);
    s.sql(
        "UPDATE photos SET display='derivatives/x.jpg', place='残る地名', elev=3, muni_cd='13101',
           pref='東京都', city='千代田区', favorite=1 WHERE id=2",
    );
    s.sql("DELETE FROM photos WHERE id=1");
    let counts = s.run(&Infer);
    assert_eq!(get(&counts, "cancelled"), 1);
    let row = s.row(2);
    for column in [
        "lat",
        "lon",
        "loc_source",
        "inferred_from",
        "infer_dt_s",
        "display",
    ] {
        assert_eq!(row[column], Value::Null, "{column}");
    }
    // 地名の列とお気に入りには触らない
    assert_eq!(row["place"], "残る地名");
    assert_eq!(row["elev"], 3.0);
    assert_eq!(row["city"], "千代田区");
    assert_eq!(row["favorite"], 1);
    // 取り消した後は、もう一度回しても何も起きない
    assert_eq!(get(&s.run(&Infer), "cancelled"), 0);
}

#[test]
fn 手動の位置は再推定せず_錨にはなる() {
    let s = setup("geo-infer-manual");
    s.sql(&format!(
        "{};{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("manual")),
        photo(2, Some("2024-05-06 10:01"), Some(PARIS), Some("exif")),
        photo(3, Some("2024-05-06 10:02"), None, None),
    ));
    let before = s.rows();
    let counts = s.run(&Infer);
    assert_eq!(get(&counts, "anchors"), 2);
    assert_eq!(get(&counts, "targets"), 1);
    // 手動と exif の行はそのまま
    assert_eq!(s.rows()[0], before[0]);
    assert_eq!(s.rows()[1], before[1]);
    // 近い方（exif の 2。差 60 秒）で推定される。手動の 1 は 120 秒
    assert_eq!(s.row(3)["inferred_from"], 2);
    // 推定した行は錨にならない（推定が推定を呼ばない）: 錨が 1 と 2 だけのまま
    assert_eq!(get(&s.run(&Infer), "anchors"), 2);
}

#[test]
fn 計画を作ってから書くまでの間に手動になった行は上書きしない() {
    let s = setup("geo-infer-race");
    s.sql(&format!(
        "{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:05"), None, None),
    ));
    let plan = super::infer::plan(&s.read(Mode::Apply), 0.0, 43_200.0).unwrap();
    assert_eq!(plan.ops.len(), 1);
    // 計画のあとに手動で位置が付く
    s.sql(&format!(
        "UPDATE photos SET lat={}, lon={}, loc_source='manual' WHERE id=2",
        SAPPORO.0, SAPPORO.1
    ));
    let ops = plan.ops.clone();
    let committed = s
        .writer
        .call(move |c| super::infer::write_plan(c, &ops, &|| false))
        .unwrap();
    assert!(committed);
    let row = s.row(2);
    assert_eq!(row["loc_source"], "manual");
    assert_eq!(row["lat"], SAPPORO.0);
}

#[test]
fn 値が変わらない行には書かない() {
    let s = setup("geo-infer-nowrite");
    s.sql(&format!(
        "{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:05"), None, None),
    ));
    s.run(&Infer);
    // 更新を数えるトリガー
    s.sql(
        "CREATE TABLE writes(n INTEGER);
           CREATE TRIGGER count_updates AFTER UPDATE ON photos
           BEGIN INSERT INTO writes VALUES(1); END;",
    );
    s.run(&Infer);
    let n: i64 = s
        .read(Mode::Apply)
        .query_row("SELECT COUNT(*) FROM writes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    // 錨が動いたときだけ書く
    s.sql(&format!(
        "UPDATE photos SET lat={} WHERE id=1",
        TOKYO.0 + 0.5
    ));
    s.sql("DELETE FROM writes");
    let counts = s.run(&Infer);
    assert_eq!(get(&counts, "updated"), 1);
    let n: i64 = s
        .read(Mode::Apply)
        .query_row("SELECT COUNT(*) FROM writes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn 書き込みの途中で停止の合図が来たら何も変えない() {
    let s = setup("geo-infer-stop");
    s.sql(&format!(
        "{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:05"), None, None),
    ));
    // 始めから合図が来ている: 計画だけ作って書かず、省略
    let stop = StopFlag::new();
    stop.stop();
    let read = s.read(Mode::Apply);
    let outcome = s.run_in(&Infer, Mode::Apply, &read, &stop).unwrap();
    assert!(matches!(outcome, StageOutcome::Skipped(_)), "{outcome:?}");
    assert_eq!(s.row(2)["loc_source"], Value::Null);
    // COMMIT の直前（最後の確認）で来る: ROLLBACK
    let plan = super::infer::plan(&s.read(Mode::Apply), 0.0, 43_200.0).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let ops = plan.ops.clone();
    let committed = s
        .writer
        .call(move |c| {
            super::infer::write_plan(c, &ops, &move || {
                counter.fetch_add(1, Ordering::SeqCst) >= 1
            })
        })
        .unwrap();
    assert!(!committed);
    assert_eq!(s.row(2)["loc_source"], Value::Null);
}

#[test]
fn dry_runは書かず_写しに計画を当てて件数を数える() {
    let s = setup("geo-infer-dry");
    s.sql(&format!(
        "{};{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")),
        photo(2, Some("2024-05-06 10:05"), None, None),
        photo(3, Some("2024-05-06 10:06"), None, None),
    ));
    let before = s.rows();
    let counts = s.dry_run(&[&Infer]);
    assert_eq!(get(&counts, "inferred"), 2);
    assert_eq!(s.rows(), before, "実 DB は変わらない");
    // 後続の段は推定後の状態を見る: 写しの上では enrich の対象が 2 件増える
    let read = s.read(Mode::DryRun);
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: Mode::DryRun,
        config: &s.config,
        read: &read,
        writer: None,
        cache: &s.cache,
        http: &http,
        stop: &stop,
    };
    let stub = StubGsi::start();
    let enrich = enrich_for(&stub);
    let StageOutcome::Done(before_infer) = enrich.run(&ctx).unwrap() else {
        panic!()
    };
    assert_eq!(get(&before_infer, "pending"), 1); // 錨の 1 だけ
    Infer.run(&ctx).unwrap();
    let StageOutcome::Done(after_infer) = enrich.run(&ctx).unwrap() else {
        panic!()
    };
    assert_eq!(get(&after_infer, "pending"), 3);
}

// ---------------------------------------------------------------- enrich

#[test]
fn 国内は逆ジオと標高と自治体表で埋め_同じキーは1回だけ問い合わせる() {
    let s = setup("geo-enrich-basic");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&format!(
        "{};{}",
        photo(1, None, Some(TOKYO), Some("exif")),
        // 3 桁に丸めると同じキー
        photo(
            2,
            None,
            Some((TOKYO.0 + 0.0001, TOKYO.1 + 0.0001)),
            Some("exif")
        ),
    ));
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "candidates"), 2);
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(get(&counts, "cache_hit"), 1);
    for id in [1, 2] {
        let row = s.row(id);
        assert_eq!(row["place"], "テスト町一丁目");
        assert_eq!(row["muni_cd"], "13101");
        assert_eq!(row["pref"], "東京都");
        assert_eq!(row["city"], "千代田区");
        assert_eq!(row["elev"], 12.0); // 12.5 は偶数丸めで 12
    }
    // 逆ジオと標高を 1 回ずつ + muni.js
    assert_eq!(stub.count(Kind::Rgeo), 1);
    assert_eq!(stub.count(Kind::Elev), 1);
    assert_eq!(stub.count(Kind::Muni), 1);
    // キャッシュのキー・値は Python と同じ形
    assert_eq!(
        s.cache.get("geo:35.681,139.767").unwrap(),
        json!({"place": "テスト町一丁目", "elev": 12, "muni_cd": "13101",
               "pref": "東京都", "city": "千代田区"})
    );
    // 市区町村の表もキャッシュに残る（次のサイクルは muni.js を取りに行かない）
    assert!(s.cache.get("muni_table").is_some());
    s.sql(&photo(3, None, Some((36.0, 136.0)), Some("exif")));
    s.run(&enrich_for(&stub));
    assert_eq!(stub.count(Kind::Muni), 1);
}

#[test]
fn 国外は通信せず空で確定する() {
    let s = setup("geo-enrich-foreign");
    let stub = StubGsi::start();
    s.sql(&format!(
        "{};{};{}",
        photo(1, None, Some(PARIS), Some("exif")),
        photo(2, None, Some((19.999, 130.0)), Some("exif")), // 矩形の外（緯度）
        photo(3, None, Some((35.0, 154.6)), Some("exif")),   // 矩形の外（経度）
    ));
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "foreign"), 3);
    assert!(stub.requests().is_empty());
    for id in [1, 2, 3] {
        let row = s.row(id);
        assert_eq!(row["place"], "");
        assert_eq!(row["elev"], Value::Null);
        assert_eq!(row["muni_cd"], "");
        assert_eq!(row["pref"], "");
        assert_eq!(row["city"], "");
    }
    assert_eq!(
        s.cache.get("geo:48.858,2.294").unwrap(),
        json!({"place": "", "elev": null, "muni_cd": "", "pref": "", "city": ""})
    );
    // 確定済みなので、2 回目は対象に入らない
    assert_eq!(get(&s.run(&enrich_for(&stub)), "candidates"), 0);
}

#[test]
fn 応答が空でも確定し_通信しないキャッシュの行は待たない() {
    let s = setup("geo-enrich-empty");
    let stub = StubGsi::start();
    stub.set_muni(Reply::Text(
        "GSI.MUNI_ARRAY[\"1\"] = '13,東京都,1,x';".into(),
    ));
    // 海上など: results が空、標高は文字列
    stub.set_rgeo(|_, _| Reply::json("{}"));
    stub.set_elev(|_, _| Reply::json(r#"{"elevation":"-----","hsrc":"-----"}"#));
    s.sql(&photo(1, None, Some((25.0, 125.0)), Some("exif")));
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "queried"), 1);
    let row = s.row(1);
    assert_eq!(row["place"], "");
    assert_eq!(row["elev"], Value::Null);
    // 2 回目は問い合わせない
    stub.clear_requests();
    s.sql("UPDATE photos SET place=NULL WHERE id=1");
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "cache_hit"), 1);
    assert!(stub.requests().is_empty());
}

#[test]
fn 通信するのは国内の未キャッシュの行だけで_待ちはその行のあとだけ() {
    let s = setup("geo-enrich-pause");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&format!(
        "{};{};{}",
        photo(1, None, Some(PARIS), Some("exif")),
        photo(2, None, Some(TOKYO), Some("exif")),
        photo(3, None, Some((TOKYO.0 + 0.0001, TOKYO.1)), Some("exif")),
    ));
    let pause = Duration::from_millis(150);
    let enrich = Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        pause,
        FailureMemo::default(),
    );
    let started = std::time::Instant::now();
    s.run(&enrich);
    let took = started.elapsed();
    // 通信した行は 1 行だけ。待ちは 1 回（国外・キャッシュの行は待たない）
    assert!(took >= pause, "{took:?}");
    assert!(took < pause * 4, "{took:?}");
}

#[test]
fn 片方だけ応答があればその分で確定し_両方なければ確定しない() {
    let s = setup("geo-enrich-partial");
    let stub = StubGsi::start();
    stub.set_muni(Reply::Text(
        "GSI.MUNI_ARRAY[\"1\"] = '13,東京都,1,x';".into(),
    ));
    // 逆ジオは 404（応答なし）、標高だけ成功
    stub.set_rgeo(|lat, _| {
        if lat == 36.0 {
            Reply::Status(404)
        } else {
            Reply::Text("<html>".into())
        }
    });
    stub.set_elev(|lat, _| {
        if lat == 36.0 {
            Reply::json(r#"{"elevation":5.0}"#)
        } else {
            Reply::Status(404)
        }
    });
    s.sql(&format!(
        "{};{}",
        photo(1, None, Some((36.0, 136.0)), Some("exif")),
        photo(2, None, Some((37.0, 137.0)), Some("exif")),
    ));
    let enrich = enrich_for(&stub);
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(get(&counts, "unresolved"), 1);
    // 1: 標高だけで確定（place は空）
    assert_eq!(s.row(1)["place"], "");
    assert_eq!(s.row(1)["elev"], 5.0);
    // 2: 両方とも応答なし → NULL のまま・キャッシュなし
    assert_eq!(s.row(2)["place"], Value::Null);
    assert!(s.cache.get("geo:37.000,137.000").is_none());
    // 次のサイクルで問い合わせ直す（記憶なしの設定）
    stub.clear_requests();
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "unresolved"), 1);
    assert_eq!(stub.count(Kind::Rgeo), 1);
}

#[test]
fn 応答が壊れた座標は記憶してしばらく飛ばし_後続を枠に入れる() {
    let s = setup("geo-enrich-memo");
    let stub = StubGsi::start();
    stub.set_muni(Reply::Text(
        "GSI.MUNI_ARRAY[\"1\"] = '13,東京都,1,x';".into(),
    ));
    // 緯度 36 の座標は両方とも壊れた応答、それ以外は空で確定
    stub.set_rgeo(|lat, _| {
        if (36.0..37.0).contains(&lat) {
            Reply::Text("broken".into())
        } else {
            Reply::json("{}")
        }
    });
    stub.set_elev(|lat, _| {
        if (36.0..37.0).contains(&lat) {
            Reply::Text("broken".into())
        } else {
            Reply::json("{}")
        }
    });
    // 壊れた座標が 40 件より多い（記憶が無いと、恒久的に枠を占めて後続が処理されない）
    let mut sql = String::new();
    for i in 0..45 {
        sql.push_str(&photo(
            i + 1,
            None,
            Some((36.0 + f64::from(i as i32) / 100.0, 136.0)),
            Some("exif"),
        ));
        sql.push(';');
    }
    sql.push_str(&photo(100, None, Some((38.0, 138.0)), Some("exif")));
    s.sql(&sql);
    let enrich = enrich_with_memory(&stub);
    let first = s.run(&enrich);
    assert_eq!(get(&first, "candidates"), 40);
    assert_eq!(get(&first, "unresolved"), 40);
    assert_eq!(s.row(100)["place"], Value::Null);
    // 2 回目: 40 件は飛ばして、残りの 5 件と後続を枠に入れる
    let second = s.run(&enrich);
    assert_eq!(get(&second, "skipped_backoff"), 40);
    assert_eq!(get(&second, "unresolved"), 5);
    assert_eq!(get(&second, "queried"), 1);
    assert_eq!(s.row(100)["place"], "");
    // 3 回目: 45 件とも飛ばす
    let third = s.run(&enrich);
    assert_eq!(get(&third, "candidates"), 0);
    assert_eq!(get(&third, "skipped_backoff"), 45);
}

#[test]
fn 通信の障害は何も確定せずに段を中断する() {
    let outages: Vec<(&str, Reply)> = vec![
        ("5xx", Reply::Status(503)),
        ("429", Reply::Status(429)),
        ("切断", Reply::Hangup),
        ("本文の途中の切断", Reply::Truncated),
    ];
    for (label, outage) in outages {
        for broken in ["rgeo", "elev"] {
            let s = setup("geo-enrich-outage");
            let stub = StubGsi::start();
            tokyo_replies(&stub);
            let bad = outage.clone();
            if broken == "rgeo" {
                stub.set_rgeo(move |_, _| bad.clone());
            } else {
                stub.set_elev(move |_, _| bad.clone());
            }
            s.sql(&format!(
                "{};{};{}",
                photo(1, None, Some(PARIS), Some("exif")), // 国外: 先に確定する
                photo(2, None, Some(TOKYO), Some("exif")),
                photo(3, None, Some((36.0, 136.0)), Some("exif")),
            ));
            let err = s.try_run(&enrich_for(&stub)).unwrap_err().to_string();
            assert!(err.contains("通信の障害"), "{label}/{broken}: {err}");
            // URL・座標を含まない
            assert!(!err.contains("127.0.0.1"), "{err}");
            assert!(!err.contains("35.68"), "{err}");
            assert_eq!(s.row(1)["place"], "", "{label}/{broken}: 国外は済んでいる");
            // 障害の行は確定しない（片方だけ応答があっても、現行のようには確定しない）
            assert_eq!(s.row(2)["place"], Value::Null, "{label}/{broken}");
            assert!(
                s.cache.get("geo:35.681,139.767").is_none(),
                "{label}/{broken}"
            );
            // 中断したので、3 の行は問い合わせていない
            assert_eq!(s.row(3)["place"], Value::Null);
            assert!(
                stub.requests()
                    .iter()
                    .all(|(_, lat, _)| (*lat - 36.0).abs() > 1e-9),
                "{label}/{broken}"
            );
        }
    }
}

#[test]
fn 障害から復旧した次のサイクルで続きから埋まる() {
    let s = setup("geo-enrich-recover");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_elev(|_, _| Reply::Status(503));
    s.sql(&photo(1, None, Some(TOKYO), Some("exif")));
    let enrich = enrich_with_memory(&stub);
    assert!(s.try_run(&enrich).is_err());
    // 復旧: 障害は記憶に入らない（写真の失敗ではない）ので、待たずに問い合わせる
    stub.set_elev(|_, _| Reply::json(r#"{"elevation":12.5}"#));
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
}

#[test]
fn muni_jsが取れなければ通信が要る行は処理せず_国外とキャッシュの行は済ませる() {
    let s = setup("geo-enrich-muni-fail");
    let stub = StubGsi::start(); // muni.js は 404
    tokyo_replies(&stub);
    stub.set_muni(Reply::Status(404));
    s.cache.put(
        "geo:36.000,136.000",
        json!({"place": "キャッシュ", "elev": 1, "muni_cd": "", "pref": "", "city": ""}),
    );
    s.sql(&format!(
        "{};{};{}",
        photo(1, None, Some(PARIS), Some("exif")),
        photo(2, None, Some(TOKYO), Some("exif")),
        photo(3, None, Some((36.0, 136.0)), Some("exif")),
    ));
    let err = s.try_run(&enrich_for(&stub)).unwrap_err().to_string();
    assert!(err.contains("muni.js"), "{err}");
    assert_eq!(s.row(1)["place"], "");
    assert_eq!(s.row(3)["place"], "キャッシュ");
    assert_eq!(s.row(2)["place"], Value::Null);
    // 逆ジオ・標高には問い合わせていない（pref/city 抜きで確定させない）
    assert_eq!(stub.count(Kind::Rgeo), 0);
    // 解釈できない本文でも同じ（表が空）
    stub.set_muni(Reply::Text("<html>moved</html>".into()));
    let err = s.try_run(&enrich_for(&stub)).unwrap_err().to_string();
    assert!(err.contains("muni.js"), "{err}");
    assert_eq!(stub.count(Kind::Rgeo), 0);
    // 取れるようになれば埋まる
    stub.set_muni(Reply::Text(
        "GSI.MUNI_ARRAY[\"13101\"] = '13,東京都,13101,千代田区';".into(),
    ));
    s.run(&enrich_for(&stub));
    assert_eq!(s.row(2)["city"], "千代田区");
}

#[test]
fn 市区町村の表はキャッシュにあれば取りに行かない() {
    let s = setup("geo-enrich-muni-cached");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_muni(Reply::Status(500));
    s.cache
        .put("muni_table", json!({"13101": ["東京都", "千代田区"]}));
    s.sql(&photo(1, None, Some(TOKYO), Some("exif")));
    s.run(&enrich_for(&stub));
    assert_eq!(stub.count(Kind::Muni), 0);
    assert_eq!(s.row(1)["pref"], "東京都");
}

#[test]
fn lonが数値でない行は対象にせず件数に出す() {
    let s = setup("geo-enrich-unusable");
    let stub = StubGsi::start();
    s.sql(&format!(
        "{};{}",
        "INSERT INTO photos(id, source, name, lat, lon, loc_source) VALUES(1, 'drive', 'a', 35.0, NULL, 'exif')",
        "INSERT INTO photos(id, source, name, lat, lon, loc_source) VALUES(2, 'drive', 'b', 'x', 'y', 'exif')",
    ));
    s.sql(&photo(3, None, Some(PARIS), Some("exif")));
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "unusable"), 2);
    assert_eq!(get(&counts, "candidates"), 1);
    assert_eq!(s.row(3)["place"], "");
    assert_eq!(s.row(1)["place"], Value::Null);
}

#[test]
fn キャッシュの値が壊れていれば使わず_型の違う値は空にする() {
    let s = setup("geo-enrich-badcache");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    // place が無い辞書・辞書でない値は無効（問い合わせ直す）。place が null のものは空で確定する
    s.cache.put("geo:35.100,139.100", json!({"city": "x"}));
    s.cache.put("geo:35.200,139.200", json!("文字列"));
    s.cache.put(
        "geo:35.300,139.300",
        json!({"place": null, "city": null, "elev": "x", "muni_cd": 5}),
    );
    s.sql(&format!(
        "{};{};{}",
        photo(1, None, Some((35.1, 139.1)), Some("exif")),
        photo(2, None, Some((35.2, 139.2)), Some("exif")),
        photo(3, None, Some((35.3, 139.3)), Some("exif")),
    ));
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "queried"), 2);
    assert_eq!(get(&counts, "cache_hit"), 1);
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
    assert_eq!(s.row(3)["place"], "");
    assert_eq!(s.row(3)["elev"], Value::Null);
    assert_eq!(s.row(3)["muni_cd"], "");
}

#[test]
fn 書き込みは座標が変わった行や地名が付いた行に効かない() {
    let s = setup("geo-enrich-guard");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&photo(1, None, Some(TOKYO), Some("exif")));
    // 逆ジオの応答を返している最中に、管理画面で位置が直されて地名が消えた、という競合
    let target = Arc::new(s.writer.clone());
    let race = Arc::clone(&target);
    stub.set_elev(move |_, _| {
        race.call(|c| {
            c.execute(
                "UPDATE photos SET lat=43.0, lon=141.0, loc_source='manual', place=NULL WHERE id=1",
                [],
            )
        })
        .unwrap();
        Reply::json(r#"{"elevation":1}"#)
    });
    s.run(&enrich_for(&stub));
    // 古い座標の地名は書かれない（次のサイクルで新しい座標を enrich する）
    let row = s.row(1);
    assert_eq!(row["place"], Value::Null);
    assert_eq!(row["lat"], 43.0);
}

#[test]
fn 一サイクルは40件まで() {
    let s = setup("geo-enrich-limit");
    let stub = StubGsi::start();
    let mut sql = String::new();
    for i in 0..45 {
        sql.push_str(&photo(
            i + 1,
            None,
            Some((48.0 + f64::from(i as i32) / 100.0, 2.0)),
            Some("exif"),
        ));
        sql.push(';');
    }
    s.sql(&sql);
    let first = s.run(&enrich_for(&stub));
    assert_eq!(get(&first, "candidates"), 40);
    assert_eq!(get(&first, "pending"), 45);
    let filled: usize = s
        .rows()
        .iter()
        .filter(|r| r["place"] != Value::Null)
        .count();
    assert_eq!(filled, 40);
    // id 順に 40 件
    assert_eq!(s.row(40)["place"], "");
    assert_eq!(s.row(41)["place"], Value::Null);
    let second = s.run(&enrich_for(&stub));
    assert_eq!(get(&second, "candidates"), 5);
    assert_eq!(get(&second, "pending"), 5);
}

#[test]
fn 停止の合図が来たら次の行へ進まない() {
    let s = setup("geo-enrich-stop");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&format!(
        "{};{}",
        photo(1, None, Some(TOKYO), Some("exif")),
        photo(2, None, Some((36.0, 136.0)), Some("exif")),
    ));
    let stop = StopFlag::new();
    let flag = stop.clone();
    // 1 件目の逆ジオに答えたあとで合図が来る → 待ちのところで止まり、2 件目には進まない
    stub.set_rgeo(move |_, _| {
        flag.stop();
        Reply::json(r#"{"results":{"lv01Nm":"x"}}"#)
    });
    let enrich = Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::from_secs(30),
        FailureMemo::default(),
    );
    let read = s.read(Mode::Apply);
    let started = std::time::Instant::now();
    let outcome = s.run_in(&enrich, Mode::Apply, &read, &stop).unwrap();
    assert!(matches!(outcome, StageOutcome::Done(_)));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "待ちを打ち切る"
    );
    assert_eq!(s.row(1)["place"], "x");
    assert_eq!(s.row(2)["place"], Value::Null);
}

#[test]
fn dry_runは通信せず_問い合わせる件数とキャッシュで済む件数を数える() {
    let s = setup("geo-enrich-dry");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.cache.put(
        "geo:36.000,136.000",
        json!({"place": "キャッシュ", "elev": 1, "muni_cd": "", "pref": "", "city": ""}),
    );
    s.sql(&format!(
        "{};{};{};{};{}",
        photo(1, None, Some(PARIS), Some("exif")),
        photo(2, None, Some(TOKYO), Some("exif")),
        photo(3, None, Some((TOKYO.0 + 0.0001, TOKYO.1)), Some("exif")), // 2 と同じキー
        photo(4, None, Some((36.0, 136.0)), Some("exif")),
        photo(5, None, Some((37.0, 137.0)), Some("exif")),
    ));
    let before = s.rows();
    let counts = s.dry_run(&[&enrich_for(&stub)]);
    assert_eq!(get(&counts, "candidates"), 5);
    assert_eq!(get(&counts, "foreign"), 1);
    assert_eq!(get(&counts, "would_query"), 2); // 2 と 5（3 は 2 の問い合わせで済む）
    assert_eq!(get(&counts, "cache_hit"), 2); // 3 と 4
    assert!(counts.0.iter().all(|(name, _)| *name != "queried"));
    assert!(
        stub.requests().is_empty(),
        "dry-run は地理院に問い合わせない"
    );
    assert_eq!(s.rows(), before);
    // キャッシュは凍結されない Cache でも、dry-run は put しない
    assert!(s.cache.get("geo:48.858,2.294").is_none());
}

// ---------------------------------------------------------------- muni.js・書式

#[test]
fn muni_jsの解析は手書きで_pythonの正規表現と同じ表になる() {
    let doc = fixture();
    let samples = doc["muni_js"].as_array().unwrap();
    assert!(samples.len() >= 4);
    for sample in samples {
        let text = sample["text"].as_str().unwrap();
        let want = sample["table"].as_object().unwrap();
        let got = parse_muni_js(text);
        let got: Map<String, Value> = got
            .iter()
            .map(|(code, (pref, city))| (code.clone(), json!([pref, city])))
            .collect();
        assert_eq!(&got, want, "{text:?}");
    }
}

#[test]
fn キャッシュのキーはpythonの書式と同じ() {
    let doc = fixture();
    let samples = doc["float_keys"].as_array().unwrap();
    assert!(samples.len() > 50);
    for sample in samples {
        let x = sample["x"].as_f64().unwrap();
        assert_eq!(format!("{x:.3}"), sample["key"].as_str().unwrap(), "{x:?}");
    }
}

// ---------------------------------------------------------------- Python との突き合わせ

fn fixture() -> Value {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/geo-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn reply_of(value: &Value) -> Reply {
    if let Some(status) = value.get("status") {
        Reply::Status(status.as_u64().unwrap() as u16)
    } else if let Some(text) = value.get("text") {
        Reply::Text(text.as_str().unwrap().to_string())
    } else {
        Reply::Json(serde_json::to_string(&value["json"]).unwrap())
    }
}

/// フィクスチャの応答表（座標 → 応答）で答えるスタブにする。
fn script_stub(stub: &StubGsi, case: &Value) {
    let table = |kind: &str| -> Vec<(f64, f64, Reply)> {
        case["gsi"][kind]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| {
                        (
                            e["lat"].as_f64().unwrap(),
                            e["lon"].as_f64().unwrap(),
                            reply_of(&e["reply"]),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for (kind, entries) in [("rgeo", table("rgeo")), ("elev", table("elev"))] {
        let find = move |lat: f64, lon: f64| {
            entries
                .iter()
                .find(|(a, b, _)| *a == lat && *b == lon)
                .map_or_else(|| Reply::json("{}"), |(_, _, r)| r.clone())
        };
        if kind == "rgeo" {
            stub.set_rgeo(find);
        } else {
            stub.set_elev(find);
        }
    }
}

fn geo_cache(cache: &Cache, expected: &Map<String, Value>) -> Map<String, Value> {
    // Python が持つ `geo:` キーに対応するものを取り出す（Rust が余計なキーを持たないことも見る）
    let mut got = Map::new();
    for key in expected.keys() {
        if let Some(v) = cache.get(key) {
            got.insert(key.clone(), v);
        }
    }
    got
}

#[test]
fn pythonのinfer_locationsとenrich_rowsと全列が一致する() {
    let doc = fixture();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    let mut steps_checked = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let env: Vec<(String, String)> = case["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let env_refs: Vec<(&str, &str)> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let s = setup_with(&format!("geo-py-{name}"), &env_refs);
        let stub = StubGsi::start();
        script_stub(&stub, case);
        if let Some(muni) = case.get("muni").filter(|m| m.is_object()) {
            s.cache.put("muni_table", muni.clone());
        }
        let enrich = enrich_for(&stub);
        for step in case["steps"].as_array().unwrap() {
            let step_name = step["name"].as_str().unwrap();
            let label = format!("{name}/{step_name}");
            if let Some(pre) = step.get("cache").and_then(Value::as_object) {
                for (key, value) in pre {
                    s.cache.put(key, value.clone());
                }
            }
            for statement in step["sql"].as_array().unwrap() {
                s.sql(statement.as_str().unwrap());
            }
            stub.clear_requests();
            let mut inferred = None;
            for what in step["run"].as_array().unwrap() {
                match what.as_str().unwrap() {
                    "infer" => inferred = Some(get(&s.run(&Infer), "inferred")),
                    "enrich" => {
                        s.run(&enrich);
                    }
                    other => panic!("{other}"),
                }
            }
            let python = &step["python"];
            if !python["inferred"].is_null() {
                assert_eq!(
                    inferred.map(|n| json!(n)),
                    Some(python["inferred"].clone()),
                    "{label}: 新しく位置が付いた数"
                );
            }
            // photos の全列
            let want: Vec<Map<String, Value>> = python["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_object().unwrap().clone())
                .collect();
            let got = s.rows();
            assert_eq!(got.len(), want.len(), "{label}: 行数");
            for (g, w) in got.iter().zip(&want) {
                assert_eq!(g, w, "{label}: id={}", w["id"]);
            }
            // 地理院への要求（種類・座標・順序）
            let want_requests: Vec<(String, f64, f64)> = python["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    (
                        r[0].as_str().unwrap().to_string(),
                        r[1].as_f64().unwrap(),
                        r[2].as_f64().unwrap(),
                    )
                })
                .collect();
            let got_requests: Vec<(String, f64, f64)> = stub
                .requests()
                .into_iter()
                .filter(|(kind, _, _)| *kind != Kind::Muni)
                .map(|(kind, lat, lon)| {
                    let kind = if kind == Kind::Rgeo { "rgeo" } else { "elev" };
                    (kind.to_string(), lat, lon)
                })
                .collect();
            assert_eq!(got_requests, want_requests, "{label}: 地理院への要求");
            // .cache.json の geo: キー
            let want_cache = python["cache"].as_object().unwrap();
            assert_eq!(
                &geo_cache(&s.cache, want_cache),
                want_cache,
                "{label}: キャッシュ"
            );
            steps_checked += 1;
        }
    }
    assert!(steps_checked >= 20, "{steps_checked}");
}

// ---------------------------------------------------------------- 段の並び

#[test]
fn サイクルの段の順序は推定のあとに地名() {
    let names: Vec<_> = crate::sync::default_stages()
        .iter()
        .map(|s| s.name())
        .collect();
    let infer = names.iter().position(|n| *n == "infer").unwrap();
    let enrich = names.iter().position(|n| *n == "enrich").unwrap();
    assert_eq!(enrich, infer + 1);
    assert!(Path::new(env!("CARGO_MANIFEST_DIR")).exists());
}
