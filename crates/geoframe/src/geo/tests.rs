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

use super::enrich::{GsiEndpoints, body_failure, parse_muni_js};
use super::stub::{Kind, Reply, StubGsi};
use super::{Enrich, Infer, naive_seconds, naive_seconds_with_seconds};
use crate::backoff::FailureMemo;
use crate::cache::Cache;
use crate::config::{Config, Overrides};
use crate::net::Failure;
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
    .with_retry_wait(Duration::ZERO)
}

/// 記憶つき（本番の設定）の `Enrich`。
fn enrich_with_memory(stub: &StubGsi) -> Enrich {
    Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::default(),
    )
    .with_retry_wait(Duration::ZERO)
}

/// 失敗の記憶が長く（1 時間）残る `Enrich`。時間切れの行を誤って失敗として覚えると、次のサイクルの `skipped_backoff` に
/// 現れて、確定しない（待ちなしの `enrich_for` では見えない）。
fn enrich_with_long_backoff(stub: &StubGsi) -> Enrich {
    Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::with_policy(Duration::from_secs(3600), Duration::from_secs(3600), 16),
    )
    .with_retry_wait(Duration::ZERO)
}

/// 国内の `n` 行（`id` は 1 から、緯度 36.0 から 0.5 度ずつ違う。キャッシュのキーは重ならない）。
fn domestic_rows(n: i64) -> String {
    (1..=n)
        .map(|i| {
            photo(
                i,
                None,
                Some((36.0 + f64::from(i as i32) / 2.0, 136.0)),
                Some("exif"),
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// `domestic_rows` の `i` 行目の緯度。
fn domestic_lat(i: i64) -> f64 {
    36.0 + f64::from(i as i32) / 2.0
}

/// 地理院への要求のうち、この緯度への、指定の種類の数。
fn requests_for(stub: &StubGsi, kind: Kind, lat: f64) -> usize {
    stub.requests()
        .iter()
        .filter(|(k, a, _)| *k == kind && (*a - lat).abs() < 1e-9)
        .count()
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
    // 日だけは空白埋めを読む（`%d` の ` [1-9]`）。月・時・分に空白埋めは無く、2 桁の日の前の空白（` 06`）も読めない
    assert_eq!(
        naive_seconds("2024-05- 6 07:08"),
        naive_seconds("2024-05-06 07:08")
    );
    assert_eq!(
        naive_seconds("2024-05- 9\t7:8"),
        naive_seconds("2024-05-09 07:08")
    );
    for bad in [
        "2024-05- 06 07:08",
        "2024-05- 16 07:08",
        "2024-05- 0 07:08",
        "2024-05-  6 07:08",
        "2024-05-0 6 07:08",
        "2024- 5-06 07:08",
        "2024-05-06  7: 8",
        "2024-05-06 07: 8",
    ] {
        assert_eq!(naive_seconds(bad), None, "{bad:?}");
    }
    // 区切りは Python の `\s`（U+001C〜001F も含む）。ゼロ幅の空白は含まない
    for sep in [
        "\u{1c}",
        "\u{1f}",
        "\u{a0}",
        "\u{202f}",
        "\u{3000} ",
        "\u{b}",
        "\n",
    ] {
        assert_eq!(
            naive_seconds(&format!("2024-05-06{sep}07:08")),
            naive_seconds("2024-05-06 07:08"),
            "{sep:?}"
        );
    }
    for sep in ["\u{200b}", "\u{180e}"] {
        assert_eq!(naive_seconds(&format!("2024-05-06{sep}07:08")), None);
    }
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

/// 品質解析が使う秒つきの形にも、同じ規則（日の空白埋め・区切り）を当てる。
#[test]
fn 秒つきの時刻にも同じ規則を当てる() {
    assert_eq!(
        naive_seconds_with_seconds("2024-05- 6 07:08:09"),
        naive_seconds_with_seconds("2024-05-06 07:08:09")
    );
    assert_eq!(
        naive_seconds_with_seconds("2024-05- 6 07:08:09").map(|t| t - 9.0),
        naive_seconds("2024-05-06 07:08")
    );
    assert!(naive_seconds_with_seconds("2024-05-06\u{1c}7:8:9").is_some());
    for bad in [
        "2024-05- 06 07:08:09",
        "2024-05-06 07:08",
        "2024-05-06 07:08:60",
        "2024-05-06 07:08: 9",
        "2024-05-06 07:08:09 ",
    ] {
        assert_eq!(naive_seconds_with_seconds(bad), None, "{bad:?}");
    }
}

/// Python の `strptime`（`%Y-%m-%d %H:%M`・`%Y-%m-%d %H:%M:%S`）の結果との突き合わせ。
#[test]
fn pythonのstrptimeと時刻の読み方が一致する() {
    let doc = fixture();
    let samples = doc["time_samples"].as_array().unwrap();
    assert!(samples.len() >= 400, "{}", samples.len());
    let mut readable = 0;
    for sample in samples {
        let text = sample["text"].as_str().unwrap();
        assert_eq!(
            naive_seconds(text),
            sample["minute"].as_f64(),
            "分まで: {text:?}"
        );
        assert_eq!(
            naive_seconds_with_seconds(text),
            sample["second"].as_f64(),
            "秒まで: {text:?}"
        );
        // 写真フィードの `parse_epoch`（T1.2）も同じ規則
        assert_eq!(
            crate::photos::parse_epoch(text).map(|t| t as f64),
            sample["minute"].as_f64(),
            "フィード: {text:?}"
        );
        readable += usize::from(sample["minute"].is_number());
    }
    // 読めるものも読めないものも十分ある
    assert!(readable > 20 && readable < samples.len() - 20, "{readable}");
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

/// 更新 API（E12・E13）を writer 経由で呼ぶ。割り込みの試験用。
fn api_set_loc(writer: &Writer, id: i64, pos: (f64, f64)) {
    let payload = json!({"id": id, "lat": pos.0, "lon": pos.1});
    crate::update::set_loc(payload.as_object().unwrap(), Some(writer)).unwrap();
}

fn api_set_flag(writer: &Writer, id: i64, flags: Value) {
    let mut payload = flags.as_object().unwrap().clone();
    payload.insert("id".into(), json!(id));
    crate::update::set_flag(&payload, Some(writer)).unwrap();
}

#[test]
fn 計画のあとに更新apiで直された位置_お気に入り_除外はinferに失われない() {
    let mut s = setup("geo-infer-update-api");
    s.sql(&format!(
        "{};{};{};{}",
        photo(1, Some("2024-05-06 10:00"), Some(TOKYO), Some("exif")), // 錨
        photo(2, Some("2024-05-06 10:05"), None, None),                // 推定される（Set）
        photo(3, Some("2024-05-12 10:00"), Some(TOKYO), Some("inferred")), // どの錨も窓の外（Clear）
        photo(4, Some("2024-05-06 10:20"), None, None), // 錨 1 から推定される（Set）
    ));
    // 段が計画を読んだあと、writer に依頼を積む直前に、管理画面から更新が入る
    let api = s.writer.clone();
    s.writer = api.clone().interrupt_next_call(move || {
        api_set_loc(&api, 2, SAPPORO);
        api_set_flag(&api, 2, json!({"favorite": true, "hidden": true}));
        api_set_loc(&api, 3, PARIS);
        api_set_flag(&api, 3, json!({"favorite": true}));
        api_set_loc(&api, 1, PARIS); // 錨が手動で動かされる
    });
    let counts = s.run(&Infer);
    // 計画は割り込みの前の状態（3 件が書かれるはずだった）
    assert_eq!(get(&counts, "inferred"), 2);
    assert_eq!(get(&counts, "cancelled"), 1);

    let (p2, p3) = (s.row(2), s.row(3));
    assert_eq!(
        (p2["loc_source"].clone(), p2["lat"].clone()),
        (json!("manual"), json!(SAPPORO.0))
    );
    assert_eq!(
        (p2["favorite"].clone(), p2["hidden"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(p2["inferred_from"], Value::Null);
    assert_eq!(
        (p3["loc_source"].clone(), p3["lat"].clone()),
        (json!("manual"), json!(PARIS.0))
    );
    assert_eq!(p3["favorite"], json!(1));
    assert_eq!(p3["inferred_from"], Value::Null);
    // 錨が計画のときの位置ではなくなったので、古い位置を 4 に写さない
    let p4 = s.row(4);
    assert_eq!(
        (p4["lat"].clone(), p4["loc_source"].clone()),
        (Value::Null, Value::Null),
        "動かされた錨の古い位置を写してはいけない"
    );
    assert_eq!(s.row(1)["lat"], json!(PARIS.0));

    // 次のサイクルが、新しい錨（2 は手動、10:05）から付け直す
    s.writer = s.writer.clone();
    let counts = s.run(&Infer);
    assert_eq!(get(&counts, "inferred"), 1);
    let p4 = s.row(4);
    assert_eq!(p4["loc_source"], "inferred");
    assert_eq!(p4["inferred_from"], json!(2));
    assert_eq!(p4["lat"], json!(SAPPORO.0));
}

#[test]
fn enrichの途中で更新apiが位置を直しても古い座標の地名は書かず_除外は残る() {
    let s = setup("geo-enrich-update-api");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&format!(
        "{};{}",
        photo(1, None, Some(TOKYO), Some("exif")),
        photo(2, None, Some(PARIS), Some("exif")),
    ));
    // 1 の逆ジオの応答を返している最中に、管理画面で 1 の位置が直され、2 が除外される
    let api = s.writer.clone();
    stub.set_elev(move |lat, _| {
        if (lat - TOKYO.0).abs() < 1e-6 {
            api_set_loc(&api, 1, SAPPORO);
            api_set_flag(&api, 2, json!({"hidden": true}));
        }
        Reply::json(r#"{"elevation":1}"#)
    });
    s.run(&enrich_for(&stub));
    let p1 = s.row(1);
    // 古い座標（東京）の地名は、新しい座標（手動）に書かれない
    assert_eq!(p1["place"], Value::Null);
    assert_eq!(
        (p1["lat"].clone(), p1["loc_source"].clone()),
        (json!(SAPPORO.0), json!("manual"))
    );
    assert_eq!(s.row(2)["hidden"], json!(1));
    // 次のサイクルが新しい座標で地名を付ける
    s.run(&enrich_for(&stub));
    assert_ne!(s.row(1)["place"], Value::Null);
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
    assert_eq!(get(&counts, "queried"), 2); // 通信を試した行
    assert_eq!(get(&counts, "resolved"), 1); // うち確定した行
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
    assert_eq!(get(&first, "queried"), 40);
    assert_eq!(get(&first, "resolved"), 0);
    assert_eq!(get(&first, "unresolved"), 40);
    assert_eq!(s.row(100)["place"], Value::Null);
    // 2 回目: 40 件は飛ばして、残りの 5 件と後続を枠に入れる
    let second = s.run(&enrich);
    assert_eq!(get(&second, "skipped_backoff"), 40);
    assert_eq!(get(&second, "unresolved"), 5);
    assert_eq!(get(&second, "queried"), 6);
    assert_eq!(get(&second, "resolved"), 1);
    assert_eq!(s.row(100)["place"], "");
    // 3 回目: 45 件とも飛ばす
    let third = s.run(&enrich);
    assert_eq!(get(&third, "candidates"), 0);
    assert_eq!(get(&third, "skipped_backoff"), 45);
}

/// 座標によらない障害（429）は、再試行のあとすぐ段を中断する。
#[test]
fn 全体の障害は再試行のあと何も確定せずに段を中断し_記憶に入れない() {
    for broken in ["rgeo", "elev"] {
        let s = setup("geo-enrich-outage-global");
        let stub = StubGsi::start();
        tokyo_replies(&stub);
        if broken == "rgeo" {
            stub.set_rgeo(|_, _| Reply::Status(429));
        } else {
            stub.set_elev(|_, _| Reply::Status(429));
        }
        s.cache.put(
            "geo:38.000,138.000",
            json!({"place": "キャッシュ", "elev": 1, "muni_cd": "", "pref": "", "city": ""}),
        );
        s.sql(&format!(
            "{};{};{};{};{};{}",
            photo(1, None, Some(PARIS), Some("exif")), // 国外: 先に確定する
            photo(2, None, Some(TOKYO), Some("exif")), // ここで 429
            photo(3, None, Some((36.0, 136.0)), Some("exif")),
            photo(4, None, Some((37.0, 137.0)), Some("exif")),
            photo(5, None, Some((38.0, 138.0)), Some("exif")), // 通信の要らない行は、止まったあとも済ませる
            photo(6, None, Some((-33.0, 151.0)), Some("exif")),
        ));
        let enrich = enrich_with_memory(&stub);
        let err = s.try_run(&enrich).unwrap_err().to_string();
        assert!(err.contains("通信の障害"), "{broken}: {err}");
        assert!(err.contains("http 429"), "{err}");
        // 件数: 試したのは 1 行、見送ったのは 2 行（URL・座標は含まない）
        assert!(
            err.contains("queried=1") && err.contains("deferred=2"),
            "{err}"
        );
        assert!(
            !err.contains("127.0.0.1") && !err.contains("35.68"),
            "{err}"
        );
        assert_eq!(s.row(1)["place"], "", "{broken}: 国外は済んでいる");
        assert_eq!(s.row(5)["place"], "キャッシュ", "{broken}");
        assert_eq!(s.row(6)["place"], "", "{broken}");
        // 障害の行は確定しない（片方だけ応答があっても、現行のようには確定しない）。見送った行にも問い合わせない
        for id in [2, 3, 4] {
            assert_eq!(s.row(id)["place"], Value::Null, "{broken}/{id}");
        }
        assert!(s.cache.get("geo:35.681,139.767").is_none(), "{broken}");
        // 同じ要求を 1 + 2 回。ほかの座標には進まない
        let (rgeo, elev) = if broken == "rgeo" { (3, 0) } else { (1, 3) };
        assert_eq!(requests_for(&stub, Kind::Rgeo, TOKYO.0), rgeo, "{broken}");
        assert_eq!(requests_for(&stub, Kind::Elev, TOKYO.0), elev, "{broken}");
        assert!(
            stub.requests()
                .iter()
                .filter(|(kind, _, _)| *kind != Kind::Muni)
                .all(|(_, lat, _)| (*lat - TOKYO.0).abs() < 1e-9),
            "{broken}"
        );
        // 直れば、記憶に入っていないので待たずに続きから埋まる
        tokyo_replies(&stub);
        let counts = s.run(&enrich);
        assert_eq!(get(&counts, "skipped_backoff"), 0);
        assert_eq!(get(&counts, "resolved"), 3, "{broken}");
    }
}

/// 座標による障害（5xx・切断・本文が読めない）は、その座標を覚えて飛ばして次へ進み、続けて 3 つの座標で
/// 起きたら全体の障害として中断する。
#[test]
fn 座標による障害は覚えて飛ばし_続けて3つの座標で起きたら段を中断する() {
    let outages: Vec<(&str, Reply, &str)> = vec![
        ("5xx", Reply::Status(503), "http 503"),
        ("切断", Reply::Hangup, "disconnected"),
        ("本文の途中の切断", Reply::Truncated, "disconnected"),
        ("壊れたチャンク", Reply::BadChunk, "body_unreadable"),
        ("途中で終わる gzip", Reply::TruncatedGzip, "body_unreadable"),
    ];
    for (label, outage, word) in outages {
        for broken in ["rgeo", "elev"] {
            let s = setup("geo-enrich-outage-coordinate");
            let stub = StubGsi::start();
            tokyo_replies(&stub);
            let bad = outage.clone();
            if broken == "rgeo" {
                stub.set_rgeo(move |_, _| bad.clone());
            } else {
                stub.set_elev(move |_, _| bad.clone());
            }
            s.cache.put(
                "geo:38.000,138.000",
                json!({"place": "キャッシュ", "elev": 1, "muni_cd": "", "pref": "", "city": ""}),
            );
            // 1 国外 / 2〜6 国内（2・3・4 で続けて失敗して中断。5・6 は見送る）/ 7 国外 / 8 キャッシュにある国内
            let mut rows = vec![photo(1, None, Some(PARIS), Some("exif"))];
            for i in 2..=6 {
                rows.push(photo(i, None, Some((domestic_lat(i), 136.0)), Some("exif")));
            }
            rows.push(photo(7, None, Some((-33.0, 151.0)), Some("exif")));
            rows.push(photo(8, None, Some((38.0, 138.0)), Some("exif")));
            s.sql(&rows.join(";"));
            let enrich = enrich_with_memory(&stub);
            let err = s.try_run(&enrich).unwrap_err().to_string();
            let name = format!("{label}/{broken}");
            assert!(err.contains("通信の障害"), "{name}: {err}");
            assert!(err.contains("3つの座標"), "{name}: {err}");
            assert!(err.contains(word), "{name}: {err}");
            assert!(
                err.contains("queried=3")
                    && err.contains("unresolved=3")
                    && err.contains("deferred=2"),
                "{name}: {err}"
            );
            assert!(
                !err.contains("127.0.0.1") && !err.contains("136.0"),
                "{err}"
            );
            for id in [1, 7] {
                assert_eq!(s.row(id)["place"], "", "{name}: 国外は済んでいる");
            }
            assert_eq!(
                s.row(8)["place"],
                "キャッシュ",
                "{name}: 通信の要らない行は済ませる"
            );
            for id in 2..=6 {
                assert_eq!(s.row(id)["place"], Value::Null, "{name}/{id}");
            }
            // 失敗した座標ごとに、同じ要求を 1 + 2 回。見送った 5・6 には問い合わせない
            for id in 2..=4 {
                let (rgeo, elev) = if broken == "rgeo" { (3, 0) } else { (1, 3) };
                assert_eq!(
                    requests_for(&stub, Kind::Rgeo, domestic_lat(id)),
                    rgeo,
                    "{name}/{id}"
                );
                assert_eq!(
                    requests_for(&stub, Kind::Elev, domestic_lat(id)),
                    elev,
                    "{name}/{id}"
                );
            }
            for id in 5..=6 {
                assert_eq!(
                    requests_for(&stub, Kind::Rgeo, domestic_lat(id)),
                    0,
                    "{name}/{id}"
                );
            }
            // 直ると、失敗した 3 座標は覚えているので飛ばし（30 分以上）、見送った 2 行から埋まる
            tokyo_replies(&stub);
            let counts = s.run(&enrich);
            assert_eq!(get(&counts, "skipped_backoff"), 3, "{name}");
            assert_eq!(get(&counts, "resolved"), 2, "{name}");
            assert_eq!(s.row(5)["place"], "テスト町一丁目", "{name}");
            assert_eq!(s.row(2)["place"], Value::Null, "{name}");
        }
    }
}

/// 先頭だけ座標による障害で、後続は正常: 先頭の座標を覚えて飛ばし、後続は埋まる。続けて失敗しなければ
/// （間に成功する座標があれば）何度失敗しても中断しない。
#[test]
fn 先頭の座標だけ5xxでも後続は埋まり_間に成功があれば中断しない() {
    let s = setup("geo-enrich-outage-head");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    // 緯度 36.5・37.5・38.5（id 1・3・5）だけ 503。ほかは正常
    let bad = [domestic_lat(1), domestic_lat(3), domestic_lat(5)];
    stub.set_rgeo(move |lat, _| {
        if bad.iter().any(|b| (*b - lat).abs() < 1e-9) {
            Reply::Status(503)
        } else {
            Reply::json(r#"{"results":{"lv01Nm":"x"}}"#)
        }
    });
    s.sql(&domestic_rows(7));
    let enrich = enrich_with_memory(&stub);
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "queried"), 7);
    assert_eq!(get(&counts, "resolved"), 4);
    assert_eq!(get(&counts, "unresolved"), 3);
    assert_eq!(get(&counts, "deferred"), 0);
    for id in 1..=7 {
        let bad = id % 2 == 1 && id <= 5;
        assert_eq!(s.row(id)["place"] == Value::Null, bad, "{id}");
    }
    // 先頭の座標には 1 + 2 回だけ要求した
    assert_eq!(requests_for(&stub, Kind::Rgeo, domestic_lat(1)), 3);
    // 次のサイクル: 覚えているので 3 座標は飛ばし、枠を占めない
    stub.clear_requests();
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "candidates"), 0);
    assert_eq!(get(&counts, "skipped_backoff"), 3);
    assert!(stub.requests().is_empty());
}

/// 標高だけの長期障害: 逆ジオが答えていても確定せず（現行は地名だけで確定して `elev` を空に固定する）、
/// 国内の未キャッシュの行は進まない。直れば欠けなく埋まる。
#[test]
fn 標高だけの長期障害は地名だけでは確定せず_直れば欠けなく埋まる() {
    let s = setup("geo-enrich-elev-down");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_elev(|_, _| Reply::Status(503));
    s.sql(&domestic_rows(5));
    let enrich = enrich_with_memory(&stub);
    let err = s.try_run(&enrich).unwrap_err().to_string();
    assert!(
        err.contains("3つの座標") && err.contains("http 503"),
        "{err}"
    );
    // 逆ジオは答えているのに、地名も書かない（elev を NULL に固定しない）
    for id in 1..=5 {
        assert_eq!(s.row(id)["place"], Value::Null, "{id}");
    }
    assert!(s.cache.get("geo:36.500,136.000").is_none());
    assert!(requests_for(&stub, Kind::Rgeo, domestic_lat(1)) >= 1);
    // 標高が直った: 失敗した 3 座標は覚えて飛ばし、残りは埋まる。elev も付く
    tokyo_replies(&stub);
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "skipped_backoff"), 3);
    assert_eq!(get(&counts, "resolved"), 2);
    assert_eq!(s.row(4)["elev"], 12.0);
    assert_eq!(s.row(5)["place"], "テスト町一丁目");
}

/// 一時的な 5xx は再試行で通る。
#[test]
fn 一度だけの5xxは再試行で通り_確定する() {
    let s = setup("geo-enrich-retry");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    stub.set_elev(move |_, _| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::Status(503)
        } else {
            Reply::json(r#"{"elevation":12.5}"#)
        }
    });
    s.sql(&domestic_rows(1));
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(get(&counts, "resolved"), 1);
    assert_eq!(get(&counts, "unresolved"), 0);
    assert_eq!(s.row(1)["elev"], 12.0);
    assert_eq!(stub.count(Kind::Rgeo), 1);
    assert_eq!(stub.count(Kind::Elev), 2);
}

/// 本文を読む失敗の分類（`CallError` の表）。ヘッダーの前なら `Other` の種類も、本文では障害にする。
#[test]
fn 本文が読めない失敗は障害の種類になる() {
    let stub = StubGsi::start();
    for reply in [Reply::BadChunk, Reply::TruncatedGzip] {
        let what = format!("{reply:?}");
        stub.set_rgeo(move |_, _| reply.clone());
        let url = format!(
            "{}/reverse-geocoder/LonLatToAddress?lat=1&lon=2",
            stub.base_url()
        );
        let mut response = crate::net::agent().get(&url).call().unwrap();
        let error = response
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_vec()
            .unwrap_err();
        match (&what[..], &error) {
            ("BadChunk", ureq::Error::Protocol(_)) => {}
            ("TruncatedGzip", ureq::Error::Decompress("gzip", _)) => {}
            _ => panic!("想定外のエラー: {what}"),
        }
        assert_eq!(Failure::from(&error), Failure::Other);
        assert_eq!(body_failure(&error), Failure::BodyUnreadable);
    }
    let io = |kind| ureq::Error::Io(std::io::Error::from(kind));
    for (error, expected) in [
        (io(std::io::ErrorKind::UnexpectedEof), Failure::Disconnected),
        (
            io(std::io::ErrorKind::ConnectionReset),
            Failure::Disconnected,
        ),
        (io(std::io::ErrorKind::TimedOut), Failure::Timeout),
        (
            ureq::Error::Timeout(ureq::Timeout::Global),
            Failure::Timeout,
        ),
        (ureq::Error::Tls("x"), Failure::Tls),
        (
            ureq::Error::Decompress(
                "gzip",
                std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            ),
            Failure::BodyUnreadable,
        ),
        (ureq::Error::TooManyRedirects, Failure::BodyUnreadable),
    ] {
        let failure = body_failure(&error);
        assert_eq!(failure, expected);
        assert!(failure.is_outage());
    }
    // 上限（1 MiB）を超える本文は障害でなく、応答が使えない扱い（座標は覚える）
    let s = setup("geo-enrich-body-limit");
    let big = format!(r#"{{"x":"{}"}}"#, "a".repeat(1 << 20));
    stub.set_rgeo(move |_, _| Reply::Json(big.clone()));
    stub.set_elev(|_, _| Reply::Text("not json".into()));
    s.sql(&domestic_rows(1));
    stub.set_muni(Reply::Text("'13,東京都,13101,千代田区'".into()));
    stub.clear_requests();
    let counts = s.run(&enrich_for(&stub));
    assert_eq!(get(&counts, "unresolved"), 1);
    assert_eq!(stub.count(Kind::Rgeo), 1, "再試行しない");
    assert_eq!(stub.count(Kind::Elev), 1, "再試行しない");
}

#[test]
fn 障害から復旧した次のサイクルで続きから埋まる() {
    let s = setup("geo-enrich-recover");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_elev(|_, _| Reply::Status(429));
    s.sql(&photo(1, None, Some(TOKYO), Some("exif")));
    let enrich = enrich_with_memory(&stub);
    assert!(s.try_run(&enrich).is_err());
    // 復旧: 全体の障害は記憶に入らないので、待たずに問い合わせる
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
    assert!(err.contains("deferred=1"), "{err}");
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
    // 停止の境界: 通信が進んでいる行は、標高・キャッシュ・DB まで済ませてから止まる
    assert_eq!(stub.count(Kind::Rgeo), 1);
    assert_eq!(stub.count(Kind::Elev), 1);
    assert!(s.cache.get("geo:35.681,139.767").is_some());
}

/// 再試行の待ちの途中で停止の合図が来たら、待ちを打ち切って戻る。その行は確定せず、記憶にも入れない。
#[test]
fn 再試行の待ちの途中で停止の合図が来たら打ち切る() {
    let s = setup("geo-enrich-stop-retry");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&domestic_rows(2));
    let stop = StopFlag::new();
    let flag = stop.clone();
    stub.set_rgeo(move |_, _| {
        flag.stop();
        Reply::Status(503)
    });
    let enrich = Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::default(),
    )
    .with_retry_wait(Duration::from_secs(30));
    let read = s.read(Mode::Apply);
    let started = std::time::Instant::now();
    let outcome = s.run_in(&enrich, Mode::Apply, &read, &stop).unwrap();
    let StageOutcome::Done(counts) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "待ちを打ち切る"
    );
    // 最初の要求を出した行は通信済みなので数える（確定しなかった行として）
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(get(&counts, "resolved"), 0);
    assert_eq!(get(&counts, "unresolved"), 1);
    assert_eq!(get(&counts, "deferred"), 0);
    assert_eq!(stub.count(Kind::Rgeo), 1, "再試行に進まない");
    for id in [1, 2] {
        assert_eq!(s.row(id)["place"], Value::Null);
    }
    // 記憶に入っていないので、次のサイクルは待たずに試す
    tokyo_replies(&stub);
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "skipped_backoff"), 0);
    assert_eq!(get(&counts, "resolved"), 2);
}

/// 通信を試した行は、確定してもしなくても待つ（地理院への連続リクエストの抑制）。再試行の前にも待つ。
#[test]
fn 通信を試した行は結果によらず待つ() {
    let pause = Duration::from_millis(150);
    let make = |stub: &StubGsi, retry_wait: Duration| {
        Enrich::with(
            GsiEndpoints::stub(&stub.base_url()),
            pause,
            FailureMemo::default(),
        )
        .with_retry_wait(retry_wait)
    };
    // 両方の応答が使えない行（確定しない）: 以前は待たなかった
    let s = setup("geo-enrich-pause-noanswer");
    let stub = StubGsi::start();
    stub.set_muni(Reply::Text("'13,東京都,13101,千代田区'".into()));
    stub.set_rgeo(|_, _| Reply::Text("broken".into()));
    stub.set_elev(|_, _| Reply::Text("broken".into()));
    s.sql(&domestic_rows(2));
    let started = std::time::Instant::now();
    let counts = s.run(&make(&stub, Duration::ZERO));
    let took = started.elapsed();
    assert_eq!(get(&counts, "queried"), 2);
    assert_eq!(get(&counts, "resolved"), 0);
    assert!(took >= pause * 2, "{took:?}");
    assert!(took < pause * 2 + Duration::from_secs(3), "{took:?}");
    // 障害で確定しなかった行（再試行の待ち 100 ms + 200 ms が先に入る）
    let s = setup("geo-enrich-pause-failed");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_elev(|_, _| Reply::Status(503));
    s.sql(&domestic_rows(2));
    let started = std::time::Instant::now();
    let counts = s.run(&make(&stub, Duration::from_millis(100)));
    let took = started.elapsed();
    assert_eq!(get(&counts, "queried"), 2);
    assert_eq!(get(&counts, "unresolved"), 2);
    let expected = (Duration::from_millis(300) + pause) * 2;
    assert!(took >= expected, "{took:?}");
    assert!(took < expected + Duration::from_secs(3), "{took:?}");
    assert_eq!(stub.count(Kind::Elev), 6, "座標ごとに 1 + 2 回");
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

/// 国内の行が先で、後の国外の行が同じキー（丸めた座標）を空で上書きする: 現行と同じ `id` 順に処理するので、
/// 先の国内の行は問い合わせて確定し、キャッシュは最後に空になる。`--dry-run` も同じ件数になる。
#[test]
fn 国内の行が先なら問い合わせ_後の国外の行が同じキーを空で上書きする() {
    let s = setup("geo-enrich-collision-domestic-first");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&format!(
        "{};{};{}",
        photo(1, None, Some((20.0001, 130.0)), Some("exif")),
        photo(2, None, Some((19.9999, 130.0)), Some("exif")), // 国外・同じキー geo:20.000,130.000
        photo(3, None, Some((20.0002, 130.0)), Some("exif")), // 国内・同じキー: 国外の空に当たる
    ));
    let enrich = enrich_for(&stub);
    let dry = s.dry_run(&[&enrich]);
    assert_eq!(get(&dry, "foreign"), 1);
    assert_eq!(get(&dry, "would_query"), 1);
    assert_eq!(get(&dry, "cache_hit"), 1);
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "foreign"), 1);
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(get(&counts, "cache_hit"), 1);
    assert_eq!(get(&counts, "queried"), get(&dry, "would_query"));
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
    assert_eq!(s.row(2)["place"], "");
    assert_eq!(s.row(3)["place"], "");
    assert_eq!(stub.count(Kind::Rgeo), 1);
    assert_eq!(
        s.cache.get("geo:20.000,130.000").unwrap(),
        json!({"place": "", "elev": null, "muni_cd": "", "pref": "", "city": ""})
    );
}

/// 国外の行が先なら、後の国内の行は国外の空のキャッシュに当たる（問い合わせない）。
#[test]
fn 国外の行が先なら_後の国内の行は空のキャッシュに当たる() {
    let s = setup("geo-enrich-collision-foreign-first");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    s.sql(&format!(
        "{};{}",
        photo(1, None, Some((19.9999, 130.0)), Some("exif")),
        photo(2, None, Some((20.0001, 130.0)), Some("exif")),
    ));
    let enrich = enrich_for(&stub);
    let dry = s.dry_run(&[&enrich]);
    assert_eq!(get(&dry, "would_query"), 0);
    assert_eq!(get(&dry, "cache_hit"), 1);
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "cache_hit"), 1);
    assert_eq!(get(&counts, "queried"), 0);
    assert_eq!(s.row(2)["place"], "");
    assert!(stub.requests().is_empty());
}

/// 通信できない間（`muni.js` が取れない・全体の障害）も、先の国内の行を後の行が追い越さない:
/// 同じキーの国外の行は見送り、別のキーの国外の行は済ませる。直れば現行と同じ結果になる。
#[test]
fn 通信できない間も同じキーの行は追い越さない() {
    for cause in ["muni", "429"] {
        let s = setup("geo-enrich-collision-halted");
        let stub = StubGsi::start();
        tokyo_replies(&stub);
        if cause == "muni" {
            stub.set_muni(Reply::Status(503));
        } else {
            stub.set_rgeo(|_, _| Reply::Status(429));
        }
        s.sql(&format!(
            "{};{};{};{}",
            photo(1, None, Some((20.0001, 130.0)), Some("exif")), // 国内: 通信できず確定しない
            photo(2, None, Some((19.9999, 130.0)), Some("exif")), // 国外・同じキー: 追い越さない
            photo(3, None, Some((19.9999, 131.0)), Some("exif")), // 国外・別のキー: 済ませる
            photo(4, None, Some((20.0002, 130.0)), Some("exif")), // 国内・同じキー: 見送る
        ));
        let enrich = enrich_with_memory(&stub);
        let err = s.try_run(&enrich).unwrap_err().to_string();
        assert!(err.contains("deferred="), "{cause}: {err}");
        assert_eq!(s.row(1)["place"], Value::Null, "{cause}");
        assert_eq!(s.row(2)["place"], Value::Null, "{cause}");
        assert_eq!(s.row(3)["place"], "", "{cause}");
        assert_eq!(s.row(4)["place"], Value::Null, "{cause}");
        assert!(s.cache.get("geo:20.000,130.000").is_none(), "{cause}");
        // 直る
        tokyo_replies(&stub);
        s.run(&enrich);
        assert_eq!(
            s.row(1)["place"],
            "テスト町一丁目",
            "{cause}: 先の国内の行は問い合わせた結果"
        );
        assert_eq!(s.row(2)["place"], "", "{cause}");
        assert_eq!(
            s.row(4)["place"],
            "",
            "{cause}: 国外の空に当たる（現行と同じ順）"
        );
    }
}

/// 国内の先の行が障害で確定しないとき、同じキーの国外の後の行は追い越して空のキャッシュを作ってはいけない:
/// 先の行は、直ったあとに問い合わせた結果で確定する（単発の 503）。
#[test]
fn 単発の障害で止まった国内の行を同じキーの国外の行が追い越さない() {
    let s = setup("geo-enrich-collision-outage");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_elev(|_, _| Reply::Status(503));
    s.sql(&format!(
        "{};{};{};{}",
        photo(1, None, Some((20.0001, 130.0)), Some("exif")), // 国内: 503 で確定しない
        photo(2, None, Some((19.9999, 130.0)), Some("exif")), // 国外・同じキー: 追い越さない
        photo(3, None, Some((20.0002, 130.0)), Some("exif")), // 国内・同じキー: 追い越さない
        photo(4, None, Some((19.9999, 131.0)), Some("exif")), // 国外・別のキー: 済ませる
    ));
    let enrich = Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::with_policy(Duration::from_secs(3600), Duration::from_secs(3600), 16),
    )
    .with_retry_wait(Duration::ZERO);
    let first = s.run(&enrich);
    assert_eq!(get(&first, "unresolved"), 1);
    assert_eq!(get(&first, "deferred"), 2);
    assert_eq!(get(&first, "foreign"), 1);
    for id in [1, 2, 3] {
        assert_eq!(s.row(id)["place"], Value::Null, "{id}");
    }
    assert_eq!(s.row(4)["place"], "");
    assert!(
        s.cache.get("geo:20.000,130.000").is_none(),
        "空のキャッシュを作らない"
    );
    // バックオフ中の次のサイクル: 先の行を枠から外すだけでなく、同じキーの後の行も枠に入れない
    let second = s.run(&enrich);
    assert_eq!(get(&second, "skipped_backoff"), 1);
    assert_eq!(get(&second, "skipped_blocked"), 2);
    assert_eq!(get(&second, "candidates"), 0);
    assert!(s.cache.get("geo:20.000,130.000").is_none());
    // 直って、バックオフが明ける: 先の行が問い合わせた結果で確定し、後の行はその順で続く
    tokyo_replies(&stub);
    enrich.advance_memo_clock(Duration::from_secs(3601));
    let third = s.run(&enrich);
    assert_eq!(get(&third, "resolved"), 1);
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
    assert_eq!(s.row(2)["place"], "");
    assert_eq!(s.row(3)["place"], "", "国外の空に当たる（現行と同じ順）");
}

/// 3 件目の障害で段を中断した次のサイクルでも、バックオフ中の行と同じキーの後の行は追い越さない。直ったあとは
/// 現行（Python）の `first` と同じ地名・キャッシュに収束する。
#[test]
fn 障害で中断した次のサイクルも追い越さず_直れば現行と同じ地名に収束する() {
    let doc = fixture();
    let case = doc["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "enrich_key_collision")
        .unwrap();
    let step = &case["steps"][0];
    assert_eq!(step["name"], "first");
    let s = setup("geo-enrich-collision-converge");
    let stub = StubGsi::start();
    script_stub(&stub, case);
    s.cache.put("muni_table", case["muni"].clone());
    for (key, value) in step["cache"].as_object().unwrap() {
        s.cache.put(key, value.clone());
    }
    for statement in step["sql"].as_array().unwrap() {
        s.sql(statement.as_str().unwrap());
    }
    // 障害の間: 問い合わせる座標（緯度 20.0001 の 3 つ）がすべて 503
    let healthy_rgeo = {
        let table = case["gsi"]["rgeo"].clone();
        move |lat: f64, lon: f64| {
            table
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["lat"].as_f64() == Some(lat) && e["lon"].as_f64() == Some(lon))
                .map_or_else(|| Reply::json("{}"), |e| reply_of(&e["reply"]))
        }
    };
    stub.set_rgeo(|_, _| Reply::Status(503));
    // バックオフ中を保つため長くする（明けるのは時計を進めたとき）。3 つの座標で続けて障害になるので、1 つ目の障害の行（id=1）が止まった次のサイクルに
    // 国外の行（id=2）が入り込める条件になる
    let enrich = Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::with_policy(Duration::from_secs(3600), Duration::from_secs(3600), 64),
    )
    .with_retry_wait(Duration::ZERO);
    // 追加の行: id=1 の座標のほかに、障害の座標を 2 つ足して中断させる（国内・別のキー）
    s.sql(&format!(
        "{};{}",
        photo(11, None, Some((21.0, 130.0)), Some("exif")),
        photo(12, None, Some((22.0, 130.0)), Some("exif")),
    ));
    let first = s.try_run(&enrich);
    // id=1 の障害と id=9 の障害は続く。id=11・12 でも障害になるので、3 つ目の座標で中断する
    let err = first.unwrap_err().to_string();
    assert!(err.contains("続けて3つの座標"), "{err}");
    // 追い越されていない
    for id in [1, 2, 3, 9, 10] {
        assert_eq!(s.row(id)["place"], Value::Null, "第 1 サイクル: {id}");
    }
    assert!(s.cache.get("geo:20.000,130.000").is_none());
    // 次のサイクル（バックオフ中）: 先の行 id=1 は飛ばされるが、同じキーの id=2・3 は枠に入らない
    let second = s.try_run(&enrich);
    let _ = second; // 中断の有無はここでは問わない（残りの行で再び障害が続くことがある）
    for id in [1, 2, 3] {
        assert_eq!(s.row(id)["place"], Value::Null, "第 2 サイクル: {id}");
    }
    assert!(
        s.cache.get("geo:20.000,130.000").is_none(),
        "空のキャッシュを作らない"
    );
    // 直る
    stub.set_rgeo(healthy_rgeo);
    enrich.advance_memo_clock(Duration::from_secs(3601));
    for _ in 0..3 {
        let _ = s.try_run(&enrich);
        enrich.advance_memo_clock(Duration::from_secs(3601));
    }
    let want_rows = step["python"]["rows"].as_array().unwrap();
    let got = s.rows();
    for want in want_rows {
        let id = want["id"].as_i64().unwrap();
        let row = got.iter().find(|r| r["id"] == json!(id)).unwrap();
        for column in ["place", "elev", "muni_cd", "pref", "city"] {
            assert_eq!(row[column], want[column], "id={id} {column}");
        }
    }
    let want_cache = step["python"]["cache"].as_object().unwrap();
    let got_cache = geo_cache(&s.cache, want_cache);
    for (key, want) in want_cache {
        assert_eq!(got_cache.get(key), Some(want), "{key}");
    }
}

/// バックオフ中の国内の行と同じキーの国外の行は、枠に入れず、国外の空のキャッシュを作らない。
#[test]
fn バックオフ中の国内の行と同じキーの国外の行は枠に入れない() {
    let s = setup("geo-enrich-collision-backoff");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_rgeo(|lat, _| {
        if (lat - 20.0001).abs() < 1e-9 {
            Reply::Text("broken".into())
        } else {
            Reply::json(r#"{"results":{"muniCd":"13101","lv01Nm":"テスト町一丁目"}}"#)
        }
    });
    stub.set_elev(|lat, _| {
        if (lat - 20.0001).abs() < 1e-9 {
            Reply::Text("broken".into())
        } else {
            Reply::json(r#"{"elevation":12.5}"#)
        }
    });
    s.sql(&format!(
        "{};{};{}",
        photo(1, None, Some((20.0001, 130.0)), Some("exif")), // 国内: 応答なし（NoAnswer）でバックオフに入る
        photo(2, None, Some((19.9999, 130.0)), Some("exif")), // 国外・同じキー
        photo(3, None, Some((36.0, 136.0)), Some("exif")),    // 別のキー: 進む
    ));
    let enrich = Enrich::with(
        GsiEndpoints::stub(&stub.base_url()),
        Duration::ZERO,
        FailureMemo::with_policy(Duration::from_secs(3600), Duration::from_secs(3600), 16),
    )
    .with_retry_wait(Duration::ZERO);
    let dry = s.dry_run(&[&enrich]);
    assert_eq!(get(&dry, "skipped_blocked"), 0);
    let first = s.run(&enrich);
    assert_eq!(get(&first, "unresolved"), 1);
    assert_eq!(
        get(&first, "deferred"),
        1,
        "NoAnswer の行と同じキーの国外の行"
    );
    assert_eq!(get(&first, "resolved"), 1);
    assert_eq!(s.row(2)["place"], Value::Null);
    let second = s.run(&enrich);
    assert_eq!(get(&second, "skipped_backoff"), 1);
    assert_eq!(get(&second, "skipped_blocked"), 1);
    assert_eq!(get(&second, "foreign"), 0);
    assert_eq!(s.row(2)["place"], Value::Null);
    assert!(s.cache.get("geo:20.000,130.000").is_none());
    // `--dry-run`（バックオフ中の記憶は同じプロセスのものを使う）も同じ枠の選び方をする
    let dry = s.dry_run(&[&enrich]);
    assert_eq!(get(&dry, "skipped_backoff"), 1);
    assert_eq!(get(&dry, "skipped_blocked"), 1);
    assert_eq!(get(&dry, "foreign"), 0);
    // 応答が返るようになり、バックオフが明ける
    tokyo_replies(&stub);
    enrich.advance_memo_clock(Duration::from_secs(3601));
    let third = s.run(&enrich);
    assert_eq!(get(&third, "resolved"), 1);
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
    assert_eq!(s.row(2)["place"], "");
}

/// 中断の判定は異なるキーで数える: 同じ座標の写真が 3 行障害でも、中断せず、その後の正常な座標が進む。
#[test]
fn 同じ座標の写真が何枚あっても障害は1つの座標として数える() {
    let s = setup("geo-enrich-streak-same-key");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_rgeo(|lat, _| {
        if (lat - 36.0).abs() < 0.01 {
            Reply::Status(503)
        } else {
            Reply::json(r#"{"results":{"muniCd":"13101","lv01Nm":"テスト町一丁目"}}"#)
        }
    });
    s.sql(&format!(
        "{};{};{};{}",
        photo(1, None, Some((36.0, 136.0)), Some("exif")),
        photo(2, None, Some((36.0, 136.0)), Some("exif")), // 同じ座標
        photo(3, None, Some((36.0001, 136.0)), Some("exif")), // 同じキー
        photo(4, None, Some((37.0, 137.0)), Some("exif")), // 正常な座標
    ));
    let enrich = enrich_with_memory(&stub);
    let counts = s.run(&enrich); // 中断しない（Ok）
    assert_eq!(get(&counts, "unresolved"), 1);
    assert_eq!(get(&counts, "deferred"), 2);
    assert_eq!(get(&counts, "resolved"), 1);
    assert_eq!(s.row(4)["place"], "テスト町一丁目");
    assert_eq!(
        requests_for(&stub, Kind::Rgeo, 36.0),
        3,
        "同じ座標は 1 + 2 回だけ"
    );
}

/// 段全体の経過時間の上限を超えたら、通信が要る未確定の行は次のサイクルへ回す（失敗にしない）。同じキーの後の行は
/// 追い越さず、通信の要らない行は済ませる。要求の途中では打ち切らず、次の要求を始める前に見る。
///
/// 時間切れの行は**失敗の記憶に入れない**。長い（1 時間の）バックオフの `Enrich` を使い回し、応答を戻したあとの
/// **直後のサイクル**で `skipped_backoff=0` のまま確定することで確かめる（記憶に入れると、その行と同じキーの後の行が
/// 1 時間飛ばされる）。ここは「上限のあとで要求を始めなかった行」（要求の途中で上限に達した行は次の試験）。
#[test]
fn 段の経過時間の上限で打ち切り_残りは次のサイクルへ回す() {
    let s = setup("geo-enrich-budget");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    // 2 行目（緯度 37）の標高だけが 600 ms かかる。上限は 500 ms: 2 行目は最初の要求を始めた時点では上限の内側なので
    // 確定し、その次の 3 行目を始める前に上限を過ぎている。遅い環境でも 1 行目・2 行目が上限の内側で始まるよう、
    // 上限は待ちに対して余裕を持たせる。
    stub.set_elev(|lat, _| {
        if (lat - 37.0).abs() < 0.01 {
            std::thread::sleep(Duration::from_millis(600));
        }
        Reply::json(r#"{"elevation":12.5}"#)
    });
    s.sql(&format!(
        "{};{};{};{};{};{}",
        photo(1, None, Some((36.0, 136.0)), Some("exif")),
        photo(2, None, Some((37.0, 137.0)), Some("exif")),
        photo(3, None, Some((20.0001, 130.0)), Some("exif")), // 上限のあと: 始めない
        photo(4, None, Some((19.9999, 130.0)), Some("exif")), // 国外・同じキー: 追い越さない
        photo(5, None, Some((-33.0, 151.0)), Some("exif")),   // 国外・別のキー: 済ませる
        photo(6, None, Some((38.0, 138.0)), Some("exif")),    // 上限のあと: 始めない
    ));
    let enrich = enrich_with_long_backoff(&stub).with_budget(Duration::from_millis(500));
    let started = std::time::Instant::now();
    let first = s.run(&enrich); // Ok: 失敗にしない
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(get(&first, "out_of_time"), 1);
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
    assert_eq!(s.row(2)["place"], "テスト町一丁目");
    assert_eq!(s.row(3)["place"], Value::Null);
    assert_eq!(
        s.row(4)["place"],
        Value::Null,
        "同じキーの国外の行は追い越さない"
    );
    assert_eq!(s.row(5)["place"], "", "通信の要らない行は済ませる");
    assert_eq!(s.row(6)["place"], Value::Null);
    assert!(s.cache.get("geo:20.000,130.000").is_none());
    assert_eq!(get(&first, "resolved"), 2);
    assert_eq!(
        get(&first, "unresolved"),
        0,
        "要求を始めなかった行は失敗でない"
    );
    assert_eq!(
        get(&first, "deferred"),
        3,
        "3 行目・同じキーの 4 行目・6 行目"
    );
    assert_eq!(
        get(&first, "queried"),
        get(&first, "resolved") + get(&first, "unresolved")
    );
    // 応答が戻り、同じ `Enrich`（長いバックオフ）で直ちに次のサイクル: 時間切れの行は記憶に入っていないので飛ばされず、
    // 現行と同じ順の結果になる。ここで見たいのは記憶なので、2 回目は上限に当たらないようにする（`with_budget` は
    // 失敗の記憶を持ったまま上限だけを替える。遅い環境で 2 回目まで打ち切られて不安定になるのを避ける）
    stub.set_elev(|_, _| Reply::json(r#"{"elevation":12.5}"#));
    let enrich = enrich.with_budget(Duration::from_secs(3600));
    let second = s.run(&enrich);
    assert_eq!(get(&second, "skipped_backoff"), 0);
    assert_eq!(get(&second, "skipped_blocked"), 0);
    assert_eq!(get(&second, "out_of_time"), 0);
    assert_eq!(s.row(3)["place"], "テスト町一丁目");
    assert_eq!(s.row(4)["place"], "");
    assert_eq!(s.row(6)["place"], "テスト町一丁目");
}

/// 再試行込みの 1 行が長引いても、上限で次の要求（再試行）に進まない。要求の途中で上限に達した行も失敗の記憶に
/// 入れない（長いバックオフの `Enrich` を使い回した直後のサイクルで、`skipped_backoff=0` のまま確定する）。
#[test]
fn 上限のあとは再試行に進まない() {
    let s = setup("geo-enrich-budget-retry");
    let stub = StubGsi::start();
    tokyo_replies(&stub);
    stub.set_rgeo(|_, _| {
        std::thread::sleep(Duration::from_millis(600));
        Reply::Status(503)
    });
    s.sql(&domestic_rows(1));
    // 1 回目の要求の終わりで上限を過ぎている（遅い環境でも要求を始める前には上限に達しないよう、待ちに対して余裕を持たせる）
    let enrich = enrich_with_long_backoff(&stub).with_budget(Duration::from_millis(400));
    // 上限が先に見られる行頭ではなく、要求の最中に上限に達するように、1 行目だけは始める
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "out_of_time"), 1);
    assert_eq!(get(&counts, "queried"), 1);
    assert_eq!(get(&counts, "unresolved"), 1);
    assert_eq!(stub.count(Kind::Rgeo), 1, "再試行に進まない");
    assert_eq!(s.row(1)["place"], Value::Null);
    // 記憶に入っていないので、同じ `Enrich` の次のサイクルは待たずに試し、確定する
    tokyo_replies(&stub);
    let enrich = enrich.with_budget(Duration::from_secs(3600)); // 2 回目は上限に当たらない（記憶は引き継ぐ）
    let counts = s.run(&enrich);
    assert_eq!(get(&counts, "skipped_backoff"), 0);
    assert_eq!(get(&counts, "out_of_time"), 0);
    assert_eq!(get(&counts, "resolved"), 1);
    assert_eq!(s.row(1)["place"], "テスト町一丁目");
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
    // Python が持つ `geo:` キーに対応するものを取り出す。Rust が余計なキーを持たないことは、呼び出し側が
    // キャッシュの件数で見る
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
            // `--dry-run` は、同じ `id` 順でキャッシュを模擬して、Python が問い合わせる回数と同じ数を数える
            // （書かず、通信せず、模擬のキャッシュは実のキャッシュに残らない）
            if step["run"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w == "enrich")
            {
                let stages: Vec<&dyn Stage> = step["run"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| match w.as_str().unwrap() {
                        "infer" => &Infer as &dyn Stage,
                        _ => &enrich as &dyn Stage,
                    })
                    .collect();
                let rows_before = s.rows();
                let cache_before = s.cache.len();
                let dry = s.dry_run(&stages);
                let python_queries = step["python"]["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|r| r[0] == "rgeo")
                    .count() as u64;
                assert_eq!(get(&dry, "would_query"), python_queries, "{label}: dry-run");
                assert!(stub.requests().is_empty(), "{label}: dry-run は通信しない");
                assert_eq!(s.rows(), rows_before, "{label}: dry-run は書かない");
                assert_eq!(
                    s.cache.len(),
                    cache_before,
                    "{label}: dry-run はキャッシュを書かない"
                );
            }
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
            // 余計なキーが無い（`geo:` のほかは、先に入れた `muni_table` だけ）
            assert_eq!(
                s.cache.len(),
                want_cache.len() + usize::from(case.get("muni").is_some_and(Value::is_object)),
                "{label}: キャッシュの件数"
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
