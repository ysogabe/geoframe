//! 簡易品質の試験。合成の画像・合成の特徴量だけを使う（実写真は使わない）。
//!
//! - Python の `quality_cycle`・`quality_backfill` との突き合わせ（`quality-python.json`。特徴量は合成で、
//!   グルーピング・件数の制限・無効化・孤児・「全員分そろってから再構築」・dirty の印を全列で比べる）。
//! - 特徴量の算出（画像から SHA・pHash・指標）は、算法が違ってよいので、決定性と性質を見る。
//! - 段の振る舞い（実ファイルでの解析・失敗の行・`--dry-run`・停止・失敗しても前回の結果を保つ）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;

use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use serde_json::{Map, Value, json};

use super::features::{self, Features, Metrics};
use super::group::{QualityPhoto, build_quality_rows};
use super::{ALGORITHM, Quality, scan_with};
use crate::config::{Config, Overrides};
use crate::datadir::DataDir;
use crate::imaging::Luma;
use crate::stop::StopFlag;
use crate::sync::{Counts, Mode, Stage, StageCtx, StageError, StageOutcome};
use crate::testutil::TempDir;
use crate::writer::{OpenMode, Writer};

// ---------------------------------------------------------------- 道具

struct Setup {
    dir: TempDir,
    config: Config,
    writer: Writer,
    cache: crate::cache::Cache,
}

fn setup(tag: &str) -> Setup {
    let dir = TempDir::new(tag);
    let overrides = Overrides {
        listen: None,
        data_dir: Some(dir.path().to_path_buf()),
    };
    let config = Config::load(|_| None, &overrides).unwrap();
    let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
    writer.call(crate::schema::migrate).unwrap();
    let cache = crate::cache::Cache::load(dir.path());
    Setup {
        dir,
        config,
        writer,
        cache,
    }
}

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

    fn run_with(
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
            .run_with(stage, Mode::Apply, &read, &StopFlag::new())
            .unwrap()
        {
            StageOutcome::Done(counts) => counts,
            other => panic!("{other:?}"),
        }
    }

    fn dry_run(&self, stage: &dyn Stage) -> Counts {
        let read = self.read(Mode::DryRun);
        match self
            .run_with(stage, Mode::DryRun, &read, &StopFlag::new())
            .unwrap()
        {
            StageOutcome::Done(counts) => counts,
            other => panic!("{other:?}"),
        }
    }

    fn scan(&self, analyze: super::Analyzer) -> Result<StageOutcome, StageError> {
        let read = self.read(Mode::Apply);
        let http = crate::net::agent();
        let stop = StopFlag::new();
        let ctx = StageCtx {
            mode: Mode::Apply,
            config: &self.config,
            read: &read,
            writer: Some(&self.writer),
            cache: &self.cache,
            http: &http,
            stop: &stop,
        };
        scan_with(&ctx, analyze)
    }

    fn query<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> T {
        self.writer.call(move |c| f(c)).unwrap()
    }
}

fn get(counts: &Counts, name: &str) -> u64 {
    counts
        .0
        .iter()
        .find(|(k, _)| *k == name)
        .unwrap_or_else(|| panic!("{name} が無い: {counts}"))
        .1
}

fn has(counts: &Counts, name: &str) -> bool {
    counts.0.iter().any(|(k, _)| *k == name)
}

thread_local! {
    /// 合成の特徴量（`display` → 特徴量か失敗の文面）。段は呼び出したスレッドで解析する。
    static FEATS: RefCell<HashMap<String, Result<Features, String>>> = RefCell::new(HashMap::new());
}

fn fake_analyze(_: &DataDir, display: &Sql) -> Result<Features, String> {
    let Sql::Text(key) = display else {
        return Err("display".into());
    };
    FEATS.with(|f| {
        f.borrow()
            .get(key)
            .unwrap_or_else(|| panic!("合成の特徴量が無い: {key}"))
            .clone()
    })
}

fn metrics(sharp: f64) -> Metrics {
    Metrics {
        sharpness: sharp,
        luminance: 120.0,
        exposure_score: 90.0,
        black_clip: 0.01,
        white_clip: 0.01,
        contrast: 120.0,
    }
}

// ---------------------------------------------------------------- Python との突き合わせ

fn fixture() -> Value {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/quality-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn spec_to_result(spec: &Value) -> Result<Features, String> {
    if let Some(e) = spec.get("error") {
        return Err(e.as_str().unwrap().to_string());
    }
    let m = &spec["metrics"];
    let f = |k: &str| m[k].as_f64().unwrap();
    Ok(Features {
        sha256: spec["sha256"].as_str().unwrap().to_string(),
        phash: u64::from_str_radix(spec["phash"].as_str().unwrap(), 16).unwrap(),
        metrics: Metrics {
            sharpness: f("sharpness"),
            luminance: f("luminance"),
            exposure_score: f("exposure_score"),
            black_clip: f("black_clip"),
            white_clip: f("white_clip"),
            contrast: f("contrast"),
        },
    })
}

fn sql_to_json(v: Sql) -> Value {
    match v {
        Sql::Null => Value::Null,
        Sql::Integer(i) => json!(i),
        Sql::Real(f) => json!(f),
        Sql::Text(s) => json!(s),
        Sql::Blob(_) => panic!("blob"),
    }
}

/// 表の行を、列名 → 値の JSON にする（`id` の昇順は SQL が決める）。
fn rows_json(conn: &Connection, sql: &str) -> Vec<Value> {
    let mut stmt = conn.prepare(sql).unwrap();
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt
        .query_map([], |row| {
            let mut map = Map::new();
            for (i, name) in names.iter().enumerate() {
                map.insert(name.clone(), sql_to_json(row.get::<_, Sql>(i)?));
            }
            Ok(Value::Object(map))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn pythonのquality_cycleとquality_backfillと全列が一致する() {
    let doc = fixture();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let s = setup(&format!("qfix-{name}"));
        FEATS.with(|f| {
            f.borrow_mut().clear();
            for (display, spec) in case["feats"].as_object().unwrap() {
                f.borrow_mut().insert(display.clone(), spec_to_result(spec));
            }
        });
        for sql in case["photos"].as_array().unwrap() {
            s.sql(sql.as_str().unwrap());
        }
        let mut config = s.config.clone();
        for step in case["steps"].as_array().unwrap() {
            let step_name = step["name"].as_str().unwrap();
            let label = format!("{name}/{step_name}");
            FEATS.with(|f| {
                for (display, spec) in step["feats"].as_object().unwrap() {
                    f.borrow_mut().insert(display.clone(), spec_to_result(spec));
                }
            });
            for sql in step["sql"].as_array().unwrap() {
                s.sql(sql.as_str().unwrap());
            }
            let run = &step["run"];
            let counts = if run["kind"] == "cycle" {
                config.quality_per_cycle = run["limit"].as_u64().unwrap() as u32;
                let stage = Quality::with_analyzer(fake_analyze);
                let read = s.read(Mode::Apply);
                let http = crate::net::agent();
                let stop = StopFlag::new();
                let ctx = StageCtx {
                    mode: Mode::Apply,
                    config: &config,
                    read: &read,
                    writer: Some(&s.writer),
                    cache: &s.cache,
                    http: &http,
                    stop: &stop,
                };
                match stage.run(&ctx).unwrap() {
                    StageOutcome::Done(c) => c,
                    other => panic!("{label}: {other:?}"),
                }
            } else {
                config.quality_per_cycle = run["batch"].as_u64().unwrap() as u32;
                let read = s.read(Mode::Apply);
                let http = crate::net::agent();
                let stop = StopFlag::new();
                let ctx = StageCtx {
                    mode: Mode::Apply,
                    config: &config,
                    read: &read,
                    writer: Some(&s.writer),
                    cache: &s.cache,
                    http: &http,
                    stop: &stop,
                };
                match scan_with(&ctx, fake_analyze).unwrap() {
                    StageOutcome::Done(c) => c,
                    other => panic!("{label}: {other:?}"),
                }
            };
            let py = &step["python"];
            // 戻り値
            let result = &py["result"];
            assert_eq!(
                get(&counts, "analyzed"),
                result["analyzed"].as_u64().unwrap(),
                "{label}: analyzed"
            );
            assert_eq!(
                get(&counts, "remaining"),
                result["remaining"].as_u64().unwrap(),
                "{label}: remaining"
            );
            assert_eq!(
                get(&counts, "rebuilt") == 1,
                result["rebuilt"].as_bool().unwrap(),
                "{label}: rebuilt"
            );
            if run["kind"] == "cycle" {
                assert_eq!(
                    get(&counts, "removed"),
                    result["removed"].as_u64().unwrap(),
                    "{label}: removed"
                );
            }
            // 特徴量の行（時刻の列は比べない。失敗の文面は違ってよいので、失敗かどうかだけ）
            let conn = s.read(Mode::Apply);
            let features = rows_json(
                &conn,
                "SELECT photo_id,display,source_md5,source_mtime,content_sha256,phash,
                        sharpness,luminance,exposure_score,black_clip,white_clip,contrast,
                        error!='' AS failed
                   FROM photo_quality_features ORDER BY photo_id",
            );
            assert_eq!(
                Value::Array(features),
                py["features"],
                "{label}: photo_quality_features"
            );
            // photo_quality の行
            let quality = rows_json(
                &conn,
                "SELECT photo_id,display,source_md5,source_mtime,content_sha256,group_id,
                        group_kind,rank_in_group,recommended,score,reasons_json,issues_json
                   FROM photo_quality ORDER BY photo_id",
            );
            assert_eq!(
                Value::Array(quality),
                py["quality"],
                "{label}: photo_quality"
            );
            // 算法の印は Rust のもの（再構築した行は全部）
            let algorithms: Vec<String> = s.query(|c| {
                c.prepare("SELECT DISTINCT algorithm FROM photo_quality")?
                    .query_map([], |r| r.get(0))?
                    .collect()
            });
            assert!(
                algorithms.iter().all(|a| a == ALGORITHM)
                    || *step["name"].as_str().unwrap() == *"idle",
                "{label}: {algorithms:?}"
            );
            // dirty の印
            let dirty: Option<String> = conn
                .query_row(
                    "SELECT value FROM settings WHERE key='photo_quality_dirty'",
                    [],
                    |r| r.get(0),
                )
                .ok();
            assert_eq!(
                dirty.map_or(Value::Null, Value::from),
                py["dirty"],
                "{label}: photo_quality_dirty"
            );
        }
    }
}

// ---------------------------------------------------------------- グルーピングの単体

fn photo(id: i64, taken_at: &str, sha: &str, phash: u64, sharp: f64) -> QualityPhoto {
    QualityPhoto {
        id,
        display: Sql::Text(format!("derivatives/q{id}.jpg")),
        source_md5: Sql::Null,
        source_mtime: Sql::Null,
        taken_at: taken_at.to_string(),
        captured: super::parse_taken_at(taken_at),
        sha256: sha.to_string(),
        phash,
        metrics: metrics(sharp),
    }
}

#[test]
fn 同じsha256の写真は_算法が違っても同じ指標にそろえて_idの小さい方を推奨にする() {
    // 同一ファイルの 2 枚。算法の違いで指標が食い違っても（Python が作った行と Rust が作った行）、
    // 推奨が算法の差で決まらず、id の小さい方になる
    let a = photo(1, "2024-05-06 10:00", "same", 7, 4.0);
    let b = photo(2, "2024-05-06 10:00", "same", 7, 6.0);
    let rows = build_quality_rows(&[a, b]);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.kind == "exact"));
    assert_eq!(rows[0].score, rows[1].score);
    let rec: Vec<_> = rows
        .iter()
        .filter(|r| r.recommended)
        .map(|r| r.photo_id)
        .collect();
    assert_eq!(rec, [1]);
    // 同じ SHA の pHash も、id が最小の行のものにそろう（別の写真との類似の判定が行ごとにぶれない）
    let a = photo(1, "2024-05-06 10:00", "same", 0, 5.0);
    let b = photo(2, "2024-05-06 10:00", "same", u64::MAX, 5.0);
    let c = photo(3, "2024-05-06 10:01", "other", 0, 5.0);
    let rows = build_quality_rows(&[a, b, c]);
    assert_eq!(rows.len(), 3); // 3 枚とも 1 つのグループ（c は a の pHash と同じ）
}

#[test]
fn pythonのroundと同じ丸め() {
    // Python: round(0.25,1)=0.2, round(0.75,1)=0.8, round(0.05,1)=0.1, round(0.15,1)=0.1,
    //         round(0.45,1)=0.5, round(1.25,1)=1.2, round(1.35,1)=1.4, round(2.5,0)=2
    for (x, expected) in [
        (0.25, 0.2),
        (0.75, 0.8),
        (0.05, 0.1),
        (0.15, 0.1),
        (0.45, 0.5),
        (1.25, 1.2),
        (1.35, 1.4),
        (87.35, 87.3),
        (12.349999999999998, 12.3),
        (99.95, 100.0),
    ] {
        assert_eq!(super::group::round1(x), expected, "{x}");
    }
}

// ---------------------------------------------------------------- 特徴量の算出

fn gradient_jpeg(width: u32, height: u32, quality: u8, shift: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let v = ((x * 255 / width) as u8).wrapping_add(shift);
            let blob = if (x / 40 + y / 40) % 2 == 0 { 60 } else { 0 };
            data.extend_from_slice(&[v, (y * 255 / height) as u8, v.wrapping_add(blob)]);
        }
    }
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, quality);
    enc.encode(
        &data,
        width as u16,
        height as u16,
        jpeg_encoder::ColorType::Rgb,
    )
    .unwrap();
    out
}

/// 大きなぼかした塊の絵（自然な写真に近く、pHash の係数が中央値の近くに固まらない）。
fn blob_jpeg(width: u32, height: u32, quality: u8) -> Vec<u8> {
    let blobs = [
        (0.2, 0.3, 0.25, [220.0, 40.0, 40.0]),
        (0.7, 0.4, 0.2, [30.0, 200.0, 60.0]),
        (0.5, 0.8, 0.3, [40.0, 60.0, 230.0]),
        (0.85, 0.85, 0.15, [250.0, 250.0, 30.0]),
    ];
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f64 / width as f64, y as f64 / height as f64);
            let mut rgb = [30.0, 30.0, 30.0];
            for (cx, cy, r, color) in &blobs {
                let d2 = ((fx - cx).powi(2) + (fy - cy).powi(2)) / (r * r);
                let w = (-d2).exp();
                for i in 0..3 {
                    rgb[i] += (color[i] - rgb[i]) * w;
                }
            }
            data.extend(rgb.iter().map(|v| *v as u8));
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, quality)
        .encode(
            &data,
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
    out
}

#[test]
fn 同じ入力なら同じ特徴量() {
    let bytes = gradient_jpeg(640, 480, 85, 0);
    let a = features::analyze_bytes(&bytes).unwrap();
    let b = features::analyze_bytes(&bytes).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.sha256.len(), 64);
    assert_eq!(a.sha256, features::sha256_hex(&bytes));
    // 別のスレッドから呼んでも同じ（並列の順序に依らない）
    let copy = bytes.clone();
    let c = std::thread::spawn(move || features::analyze_bytes(&copy).unwrap())
        .join()
        .unwrap();
    assert_eq!(a, c);
}

#[test]
fn shaは読んだバイト列のもので_再圧縮した同じ絵はphashが近い() {
    let a = features::analyze_bytes(&blob_jpeg(640, 480, 90)).unwrap();
    let b = features::analyze_bytes(&blob_jpeg(640, 480, 60)).unwrap();
    assert_ne!(a.sha256, b.sha256);
    assert!(
        (a.phash ^ b.phash).count_ones() <= 4,
        "{:016x} {:016x}",
        a.phash,
        b.phash
    );
    // 大きさが違っても同じ絵ならpHashは近い
    let c = features::analyze_bytes(&blob_jpeg(1280, 960, 90)).unwrap();
    assert!(
        (a.phash ^ c.phash).count_ones() <= 4,
        "{:016x} {:016x}",
        a.phash,
        c.phash
    );
}

#[test]
fn 別の絵のphashは遠い() {
    let a = features::analyze_bytes(&blob_jpeg(640, 480, 90)).unwrap();
    let mut data = Vec::new();
    for y in 0..480u32 {
        for x in 0..640u32 {
            let v = if (x / 80 + y / 120) % 2 == 0 { 230 } else { 20 };
            data.extend_from_slice(&[v, v, v]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 90)
        .encode(&data, 640, 480, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    let b = features::analyze_bytes(&jpeg).unwrap();
    assert!((a.phash ^ b.phash).count_ones() > 12);
}

#[test]
fn ぼけた画像は鮮明度が低く_白い画像は白飛びが多い() {
    let sharp = features::analyze_bytes(&gradient_jpeg(640, 480, 90, 0)).unwrap();
    // 同じ絵を 1/8 に縮めて戻したようなぼけ: 低品質で潰す
    let blur = {
        let mut data = vec![0u8; 640 * 480 * 3];
        for (i, p) in data.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let x = (i % 640) as u32;
            let y = (i / 640) as u32;
            let v = (x * 255 / 640) as u8;
            p.copy_from_slice(&[v, (y * 255 / 480) as u8, v]);
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(&data, 640, 480, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        features::analyze_bytes(&out).unwrap()
    };
    assert!(sharp.metrics.sharpness > blur.metrics.sharpness);
    let white = {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(
                &vec![255u8; 64 * 64 * 3],
                64,
                64,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        features::analyze_bytes(&out).unwrap()
    };
    assert!((white.metrics.white_clip - 1.0).abs() < 1e-9);
    assert_eq!(white.metrics.black_clip, 0.0);
    assert!(white.metrics.exposure_score < 50.0);
    assert_eq!(white.metrics.contrast, 0.0);
}

#[test]
fn 小さい画像と極端な縦横比でも解析できる() {
    for (w, h) in [(1u32, 1u32), (8, 8), (9, 9), (3, 200), (200, 3), (2560, 40)] {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 85)
            .encode(
                &vec![128u8; (w * h * 3) as usize],
                w as u16,
                h as u16,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        let f = features::analyze_bytes(&out).unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
        assert!(
            f.metrics.luminance > 100.0 && f.metrics.luminance < 156.0,
            "{w}x{h}"
        );
    }
}

#[test]
fn 画像でないものと壊れたものは失敗にする() {
    assert!(features::analyze_bytes(b"not an image").is_err());
    let mut jpeg = gradient_jpeg(320, 240, 85, 0);
    jpeg.truncate(jpeg.len() / 2);
    assert!(features::analyze_bytes(&jpeg).is_err());
}

#[test]
fn pillowと同じ整数式でグレーにする() {
    // 純粋な赤・緑・青の 1 画素: (R*19595 + G*38470 + B*7471 + 0x8000) >> 16 = 76, 150, 29
    for (rgb, expected) in [([255u8, 0, 0], 76u8), ([0, 255, 0], 150), ([0, 0, 255], 29)] {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgb, 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        let Luma { data, .. } = crate::imaging::decode_luma(&png).unwrap();
        assert_eq!(data, [expected]);
    }
}

use image::ImageEncoder;

// ---------------------------------------------------------------- 段（実ファイル）

fn write_jpeg(s: &Setup, name: &str, jpeg: &[u8]) {
    let dir = s.dir.path().join("derivatives");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), jpeg).unwrap();
}

fn insert_photo(s: &Setup, id: i64, taken_at: &str, display: &str) {
    s.sql(&format!(
        "INSERT INTO photos(id,source,name,taken_at,display,md5,mtime)
         VALUES({id},'drive','p{id}','{taken_at}','{display}','m{id}',{id}.5)"
    ));
}

fn count_of(s: &Setup, sql: &'static str) -> i64 {
    s.query(move |c| c.query_row(sql, [], |r| r.get(0)))
}

#[test]
fn 実ファイルを解析して特徴量と品質の行を作る() {
    let s = setup("quality-real");
    let a = gradient_jpeg(640, 480, 85, 0);
    write_jpeg(&s, "a.jpg", &a);
    write_jpeg(&s, "b.jpg", &a); // 同じバイト列 → 完全重複
    write_jpeg(&s, "c.jpg", &gradient_jpeg(640, 480, 85, 40));
    insert_photo(&s, 1, "2024-05-06 10:00", "derivatives/a.jpg");
    insert_photo(&s, 2, "2024-05-06 10:00", "derivatives/b.jpg");
    insert_photo(&s, 3, "2024-05-06 18:00", "derivatives/c.jpg");
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 3);
    assert_eq!(get(&counts, "failed"), 0);
    assert_eq!(get(&counts, "rebuilt"), 1);
    assert_eq!(get(&counts, "rows"), 2);
    // SHA-256 は derivative のバイト列
    let sha: String = s.query(|c| {
        c.query_row(
            "SELECT content_sha256 FROM photo_quality_features WHERE photo_id=1",
            [],
            |r| r.get(0),
        )
    });
    assert_eq!(sha, features::sha256_hex(&a));
    let rows: Vec<(i64, String, i64, i64)> = s.query(|c| {
        c.prepare(
            "SELECT photo_id,group_kind,recommended,group_id FROM photo_quality ORDER BY photo_id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect()
    });
    assert_eq!(
        rows,
        [
            (1, "exact".to_string(), 1, 1),
            (2, "exact".to_string(), 0, 1)
        ]
    );
    // 解析時刻は +09:00 のような UTC からのずれが付く
    let at: String = s.query(|c| {
        c.query_row(
            "SELECT analyzed_at FROM photo_quality_features LIMIT 1",
            [],
            |r| r.get(0),
        )
    });
    assert_eq!(at.len(), 25, "{at}");
    assert_eq!(
        count_of(
            &s,
            "SELECT COUNT(*) FROM photo_quality WHERE algorithm='rust-phash-v1:t3:h12'"
        ),
        2
    );
    // 次のサイクルは何もしない
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 0);
    assert_eq!(get(&counts, "rebuilt"), 0);
}

#[test]
fn 読めない写真は失敗の行になり_種類だけを書き_毎回やり直さない() {
    let s = setup("quality-fail");
    write_jpeg(&s, "good.jpg", &gradient_jpeg(320, 240, 85, 0));
    write_jpeg(&s, "bad.jpg", b"not an image at all");
    insert_photo(&s, 1, "2024-05-06 10:00", "derivatives/good.jpg");
    insert_photo(&s, 2, "2024-05-06 10:00", "derivatives/bad.jpg");
    insert_photo(
        &s,
        3,
        "2024-05-06 10:00",
        "derivatives/missing-secret-name.jpg",
    );
    insert_photo(&s, 4, "2024-05-06 10:00", "../escape.jpg");
    insert_photo(&s, 5, "2024-05-06 10:00", "/etc/passwd");
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 5);
    assert_eq!(get(&counts, "failed"), 4);
    let errors: Vec<String> = s.query(|c| {
        c.prepare("SELECT error FROM photo_quality_features WHERE error!='' ORDER BY photo_id")?
            .query_map([], |r| r.get(0))?
            .collect()
    });
    assert_eq!(errors.len(), 4);
    for e in &errors {
        // パス・ファイル名は書かない
        assert!(!e.contains("secret"), "{e}");
        assert!(
            !e.contains("bad.jpg") && !e.contains("passwd") && !e.contains("escape"),
            "{e}"
        );
        assert!(e.chars().count() < 100, "{e}");
    }
    // 失敗した行は解析済み: 次のサイクルで再解析しない・グルーピングに入らない
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 0);
    assert_eq!(count_of(&s, "SELECT COUNT(*) FROM photo_quality"), 0);
}

#[test]
fn リンクでデータ置き場の外を指す写真は読まない() {
    let s = setup("quality-link");
    let outside = TempDir::new("quality-outside");
    fs::write(outside.path().join("o.jpg"), gradient_jpeg(64, 64, 85, 0)).unwrap();
    fs::create_dir_all(s.dir.path().join("derivatives")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("o.jpg"),
        s.dir.path().join("derivatives/link.jpg"),
    )
    .unwrap();
    insert_photo(&s, 1, "2024-05-06 10:00", "derivatives/link.jpg");
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "failed"), 1);
    assert_eq!(
        count_of(
            &s,
            "SELECT COUNT(*) FROM photo_quality_features WHERE content_sha256 IS NOT NULL"
        ),
        0
    );
}

#[test]
fn 件数の制限は_idの順に解析し_0は1として扱う() {
    let s = setup("quality-limit");
    for i in 1..=5 {
        write_jpeg(&s, &format!("{i}.jpg"), &gradient_jpeg(64, 64, 85, i as u8));
        insert_photo(&s, i, "2024-05-06 10:00", &format!("derivatives/{i}.jpg"));
    }
    let mut s = s;
    s.config.quality_per_cycle = 0;
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 1);
    assert_eq!(get(&counts, "remaining"), 4);
    assert_eq!(get(&counts, "rebuilt"), 0);
    let ids: Vec<i64> = s.query(|c| {
        c.prepare("SELECT photo_id FROM photo_quality_features")?
            .query_map([], |r| r.get(0))?
            .collect()
    });
    assert_eq!(ids, [1]);
    s.config.quality_per_cycle = 2;
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 2);
    assert_eq!(get(&counts, "pending"), 4);
}

#[test]
fn 再構築が失敗しても前回の結果を保ち_dirtyも戻らない() {
    let s = setup("quality-keep");
    write_jpeg(&s, "a.jpg", &gradient_jpeg(64, 64, 85, 0));
    insert_photo(&s, 1, "2024-05-06 10:00", "derivatives/a.jpg");
    s.sql(&format!(
        "INSERT INTO photo_quality(photo_id,display,source_md5,source_mtime,content_sha256,group_id,
           group_kind,rank_in_group,recommended,score,reasons_json,issues_json,algorithm,analyzed_at)
         VALUES(9,'x',NULL,NULL,'{}',1,'exact',1,1,50.0,'[]','[]','old','t')",
        "a".repeat(64)
    ));
    // 全差し替えの挿入を必ず失敗させる
    s.sql("CREATE TRIGGER fail_insert BEFORE INSERT ON photo_quality BEGIN SELECT RAISE(ABORT,'x'); END");
    s.sql("INSERT INTO photos(id,source,name,taken_at,display,md5,mtime) VALUES(2,'drive','p2','2024-05-06 10:00','derivatives/a.jpg','m2',2.5)");
    let read = s.read(Mode::Apply);
    let result = s.run_with(&Quality::new(), Mode::Apply, &read, &StopFlag::new());
    // 2 枚とも同じバイト列 → 完全重複の行を挿入しようとして失敗する
    assert!(result.is_err());
    assert_eq!(count_of(&s, "SELECT COUNT(*) FROM photo_quality"), 1);
    assert_eq!(
        count_of(
            &s,
            "SELECT COUNT(*) FROM photo_quality WHERE algorithm='old'"
        ),
        1
    );
    // 特徴量は残り、dirty のまま。直ったら次のサイクルで再構築される
    assert_eq!(
        count_of(&s, "SELECT COUNT(*) FROM photo_quality_features"),
        2
    );
    let dirty: String = s.query(|c| {
        c.query_row(
            "SELECT value FROM settings WHERE key='photo_quality_dirty'",
            [],
            |r| r.get(0),
        )
    });
    assert_eq!(dirty, "1");
    s.sql("DROP TRIGGER fail_insert");
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "rebuilt"), 1);
    assert_eq!(
        count_of(
            &s,
            "SELECT COUNT(*) FROM photo_quality WHERE algorithm='old'"
        ),
        0
    );
}

#[test]
fn 停止の合図で解析を打ち切り_再構築しない() {
    let s = setup("quality-stop");
    for i in 1..=3 {
        write_jpeg(&s, &format!("{i}.jpg"), &gradient_jpeg(64, 64, 85, i as u8));
        insert_photo(&s, i, "2024-05-06 10:00", &format!("derivatives/{i}.jpg"));
    }
    let stop = StopFlag::new();
    stop.stop();
    let read = s.read(Mode::Apply);
    let StageOutcome::Done(counts) = s
        .run_with(&Quality::new(), Mode::Apply, &read, &stop)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(get(&counts, "analyzed"), 0);
    assert_eq!(get(&counts, "interrupted"), 1);
    assert!(!has(&counts, "rebuilt"));
    assert_eq!(
        count_of(&s, "SELECT COUNT(*) FROM photo_quality_features"),
        0
    );
}

#[test]
fn dry_runは解析せず件数だけを数え_何も書かない() {
    let s = setup("quality-dry");
    for i in 1..=3 {
        write_jpeg(&s, &format!("{i}.jpg"), &gradient_jpeg(64, 64, 85, i as u8));
        insert_photo(&s, i, "2024-05-06 10:00", &format!("derivatives/{i}.jpg"));
    }
    let mut s = s;
    s.config.quality_per_cycle = 2;
    let counts = s.dry_run(&Quality::new());
    assert_eq!(get(&counts, "pending"), 3);
    assert_eq!(get(&counts, "would_analyze"), 2);
    assert_eq!(get(&counts, "remaining"), 1);
    assert_eq!(get(&counts, "would_rebuild"), 0);
    assert!(!has(&counts, "rows"));
    s.config.quality_per_cycle = 10;
    let counts = s.dry_run(&Quality::new());
    assert_eq!(get(&counts, "would_analyze"), 3);
    assert_eq!(get(&counts, "would_rebuild"), 1);
    assert!(!has(&counts, "rows")); // 解析の結果が要るので見積もれない
    assert_eq!(
        count_of(&s, "SELECT COUNT(*) FROM photo_quality_features"),
        0
    );
    // 全部解析したあと: 何もしない（dirty でなければ再構築しない）
    s.run(&Quality::new());
    let counts = s.dry_run(&Quality::new());
    assert_eq!(get(&counts, "pending"), 0);
    assert_eq!(get(&counts, "orphans"), 0);
    assert_eq!(get(&counts, "would_rebuild"), 0);
    // dirty にすると、再構築で書く行数と、今から変わる行数（変わらないので 0）
    s.sql("UPDATE settings SET value='1' WHERE key='photo_quality_dirty'");
    let counts = s.dry_run(&Quality::new());
    assert_eq!(get(&counts, "would_rebuild"), 1);
    assert_eq!(get(&counts, "rows"), 3);
    assert_eq!(get(&counts, "changed"), 0);
    // 孤児（写真が消えた）が見える
    s.sql("DELETE FROM photos WHERE id=1");
    let counts = s.dry_run(&Quality::new());
    assert_eq!(get(&counts, "orphans"), 1);
    assert_eq!(get(&counts, "changed"), 3); // 3 枚の完全重複 → 2 枚（行が変わる・消える）
    assert_eq!(
        count_of(&s, "SELECT COUNT(*) FROM photo_quality_features"),
        3
    );
}

#[test]
fn 一括解析は全部解析してから再構築し_待ちが減らなければ止まる() {
    let s = setup("quality-scan");
    for i in 1..=5 {
        write_jpeg(&s, &format!("{i}.jpg"), &gradient_jpeg(64, 64, 85, i as u8));
        insert_photo(&s, i, "2024-05-06 10:00", &format!("derivatives/{i}.jpg"));
    }
    let mut s = s;
    s.config.quality_per_cycle = 2;
    let StageOutcome::Done(counts) = s.scan(super::analyze_one).unwrap() else {
        panic!()
    };
    assert_eq!(get(&counts, "analyzed"), 5);
    assert_eq!(get(&counts, "remaining"), 0);
    assert_eq!(get(&counts, "rebuilt"), 1);
    // 待ちが減らない解析（書いた行が有効にならない）は無限に回さず失敗にする
    let s = setup("quality-scan-stuck");
    insert_photo(&s, 1, "2024-05-06 10:00", "derivatives/x.jpg");
    s.sql("CREATE TRIGGER no_valid AFTER INSERT ON photo_quality_features BEGIN DELETE FROM photo_quality_features WHERE photo_id=NEW.photo_id; END");
    FEATS.with(|f| {
        f.borrow_mut().insert(
            "derivatives/x.jpg".into(),
            Ok(Features {
                sha256: "a".repeat(64),
                phash: 0,
                metrics: metrics(1.0),
            }),
        )
    });
    assert!(s.scan(fake_analyze).is_err());
}

#[test]
fn 位置の無い写真も解析し_displayの無い写真は解析しない() {
    let s = setup("quality-nolocation");
    write_jpeg(&s, "a.jpg", &gradient_jpeg(64, 64, 85, 0));
    s.sql("INSERT INTO photos(id,source,name,taken_at,display,lat,lon) VALUES(1,'drive','p1','2024-05-06 10:00','derivatives/a.jpg',NULL,NULL)");
    s.sql("INSERT INTO photos(id,source,name,taken_at,display) VALUES(2,'drive','p2','2024-05-06 10:00',NULL)");
    s.sql("INSERT INTO photos(id,source,name,taken_at,display) VALUES(3,'drive','p3','2024-05-06 10:00','')");
    let counts = s.run(&Quality::new());
    assert_eq!(get(&counts, "analyzed"), 1);
}

#[test]
fn 時刻の秒つきの形も読む() {
    assert_eq!(
        super::parse_taken_at("2024-05-06 10:00:30"),
        super::parse_taken_at("2024-05-06 10:00").map(|t| t + 30)
    );
    for bad in [
        "",
        "x",
        "2024-05-06 10:00:60",
        "2024-05-06 10:00:5x",
        "2024-02-30 10:00:00",
    ] {
        assert_eq!(super::parse_taken_at(bad), None, "{bad:?}");
    }
    assert!(super::parse_taken_at("2024-5-6 1:2:3").is_some());
    // 日だけは空白埋めを読む（`strptime` の `%d`）。`geo::naive_seconds` と同じ規則（geo のテストが Python と突き合わせる）
    assert_eq!(
        super::parse_taken_at("2024-05- 6 10:00:30"),
        super::parse_taken_at("2024-05-06 10:00:30")
    );
    assert!(super::parse_taken_at("2024-05- 6 10:00").is_some());
    assert_eq!(super::parse_taken_at("2024-05- 06 10:00:30"), None);
}
