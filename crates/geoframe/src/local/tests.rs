//! `index_local`・`local_derivatives`・`organize` の単体テスト。写真は合成のバイト列だけ
//! （EXIF は `tests/fixtures/exif-python.json` の合成 JPEG）。Python の `index_local` との列ごとの
//! 突き合わせは `tests/local_index_python.rs`。

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::Engine;
use rusqlite::Connection;

use super::*;
use crate::cache::Cache;
use crate::config::{Config, Overrides};
use crate::geo::Infer;
use crate::stop::StopFlag;
use crate::sync::{Mode, StageResult};
use crate::testutil::TempDir;
use crate::writer::{OpenMode, Writer};

/// 合成 JPEG（8x8）。GPS（12.575, 78.152778）と撮影時刻 `2024-05-06 07:08` 入り。
fn jpeg_gps() -> Vec<u8> {
    fixture_jpeg("typical")
}

/// EXIF の無い合成 JPEG。
fn jpeg_plain() -> Vec<u8> {
    fixture_jpeg("no_exif")
}

fn fixture_jpeg(name: &str) -> Vec<u8> {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/exif-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let case = doc["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(case["jpeg_base64"].as_str().unwrap())
        .unwrap()
}

fn heic() -> Vec<u8> {
    fs::read(format!(
        "{}/tests/fixtures/synthetic_o1.heic",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

struct Setup {
    dir: TempDir,
    config: Arc<Config>,
    writer: Writer,
    cache: Cache,
}

fn setup(tag: &str) -> Setup {
    setup_with(tag, &[])
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
    let config = Arc::new(Config::load(|k| env.get(k).cloned(), &overrides).unwrap());
    let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
    writer.call(crate::schema::migrate).unwrap();
    let cache = Cache::load(dir.path());
    Setup {
        dir,
        config,
        writer,
        cache,
    }
}

impl Setup {
    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// `rel`（`DATA_DIR` からの相対）にファイルを置き、更新時刻を `mtime_s` 秒（+ 123456789 ナノ秒）にする。
    fn put(&self, rel: &str, bytes: &[u8], mtime_s: u64) -> PathBuf {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        set_mtime(&path, mtime_s);
        path
    }

    /// 段に渡す読み取り用の接続。実行は読み取り専用の接続、dry-run は実 DB のメモリ上の写し
    /// （`cli.rs` と同じ）。
    fn read_for(&self, mode: Mode) -> Connection {
        let real = crate::db::open_read_only(&self.config.db_path()).unwrap();
        match mode {
            Mode::Apply => real,
            Mode::DryRun => crate::sync::simulation_copy(&real).unwrap(),
        }
    }

    fn run_with(&self, stage: &dyn Stage, mode: Mode, stop: &StopFlag) -> StageResult {
        let read = self.read_for(mode);
        let http = crate::net::agent();
        let ctx = StageCtx {
            mode,
            config: &self.config,
            read: &read,
            writer: (mode == Mode::Apply).then_some(&self.writer),
            cache: &self.cache,
            http: &http,
            stop,
        };
        match stage.run(&ctx) {
            Ok(StageOutcome::Done(counts)) => StageResult::Done(counts),
            Ok(StageOutcome::Skipped(reason)) => StageResult::Skipped(reason),
            Err(e) => StageResult::Failed(e.to_string()),
        }
    }

    /// `read` を段の読み取り用の接続にして `f` を呼ぶ（dry-run で複数の段が同じ写しを共有するため）。
    fn with_ctx<R>(
        &self,
        mode: Mode,
        read: &Connection,
        stop: &StopFlag,
        f: impl FnOnce(&StageCtx<'_>) -> R,
    ) -> R {
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
        f(&ctx)
    }

    /// 停止の合図が `n` 回目の確認で来るものとして、`index_local` を実行する。確認の回数も返す。
    fn index_stopping_at(&self, n: usize) -> (Result<StageOutcome, StageError>, usize) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let read = self.read_for(Mode::Apply);
        let result = self.with_ctx(Mode::Apply, &read, &StopFlag::new(), |ctx| {
            IndexLocal::run_with(ctx, move || counter.fetch_add(1, Ordering::SeqCst) + 1 >= n)
        });
        (result, calls.load(Ordering::SeqCst))
    }

    fn run(&self, stage: &dyn Stage) -> Counts {
        match self.run_with(stage, Mode::Apply, &StopFlag::new()) {
            StageResult::Done(counts) => counts,
            other => panic!("{other:?}"),
        }
    }

    fn dry_run(&self, stage: &dyn Stage) -> Counts {
        match self.run_with(stage, Mode::DryRun, &StopFlag::new()) {
            StageResult::Done(counts) => counts,
            other => panic!("{other:?}"),
        }
    }

    fn organize(&self, mode: Mode) -> Counts {
        let read = self.read_for(mode);
        let http = crate::net::agent();
        let stop = StopFlag::new();
        let ctx = StageCtx {
            mode,
            config: &self.config,
            read: &read,
            writer: (mode == Mode::Apply).then_some(&self.writer),
            cache: &self.cache,
            http: &http,
            stop: &stop,
        };
        // `geoframe organize` と同じ順序（現行の `organize_photos`: index_local → infer_locations → 移動）。
        // `index_local` は呼び出し側が済ませてある
        Infer.run(&ctx).unwrap();
        organize(&ctx).unwrap()
    }

    fn sql(&self, sql: &'static str) {
        self.writer.call(move |c| c.execute_batch(sql)).unwrap();
    }

    fn rows(&self) -> Vec<Row> {
        let read = crate::db::open_read_only(&self.config.db_path()).unwrap();
        let mut stmt = read
            .prepare(
                "SELECT id, source, path, name, mtime, taken_at, lat, lon, loc_source, display,
                        place, indexed_at, fetched_at FROM photos ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                source: r.get(1)?,
                path: r.get(2)?,
                name: r.get(3)?,
                mtime: r.get(4)?,
                taken_at: r.get(5)?,
                lat: r.get(6)?,
                lon: r.get(7)?,
                loc_source: r.get(8)?,
                display: r.get(9)?,
                place: r.get(10)?,
                indexed_at: r.get(11)?,
                fetched_at: r.get(12)?,
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn row(&self, path: &str) -> Row {
        self.rows()
            .into_iter()
            .find(|r| r.path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("行が無い: {path}"))
    }

    fn paths(&self) -> Vec<String> {
        self.rows().into_iter().filter_map(|r| r.path).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    id: i64,
    source: String,
    path: Option<String>,
    name: String,
    mtime: Option<f64>,
    taken_at: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    loc_source: Option<String>,
    display: Option<String>,
    place: Option<String>,
    indexed_at: Option<String>,
    fetched_at: Option<String>,
}

fn set_mtime(path: &Path, secs: u64) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::new(secs, 123_456_789))
        .unwrap();
}

fn get(counts: &Counts, name: &str) -> u64 {
    counts
        .0
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("{name} が無い: {counts}"))
        .1
}

const T0: u64 = 1_700_000_000;

// ---------------------------------------------------------------- 名前・時刻

#[test]
fn derivativeの名前はpythonのmd5の先頭16桁と同じ() {
    // Python: hashlib.md5(path.encode()).hexdigest()[:16]
    for (path, want) in [
        ("photos/a.heic", "local_e794953465bdae8f.jpg"),
        ("photos/日本語/b.heic", "local_ec54e15052227ec1.jpg"),
        ("photos/sub dir/c.tif", "local_2bb91f1bcb75e793.jpg"),
    ] {
        assert_eq!(local_derivative_name(path), want, "{path}");
        // `derivatives/` に置ける名前の規則を通る
        assert!(crate::datadir::check_file_name(want).is_ok());
    }
}

#[test]
fn mtimeはcpythonのst_mtimeと同じ式の浮動小数() {
    let s = setup("local-mtime");
    let path = s.put("photos/a.jpg", &jpeg_plain(), T0);
    let meta = fs::metadata(path).unwrap();
    // CPython: sec + nsec * 1e-9（double）
    assert_eq!(mtime_seconds(&meta), T0 as f64 + 123_456_789.0 * 1e-9);
    assert_eq!(mtime_seconds(&meta), 1_700_000_000.123_456_8);
}

// ---------------------------------------------------------------- index_local

#[test]
fn 追加は位置と撮影時刻とmtimeを現行と同じ列に入れる() {
    let s = setup("local-add");
    s.put("photos/with_gps.jpg", &jpeg_gps(), T0);
    s.put("photos/no_gps.jpg", &jpeg_plain(), T0 + 5);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "files"), 2);
    assert_eq!(get(&counts, "added"), 2);
    assert_eq!(get(&counts, "updated") + get(&counts, "removed"), 0);

    let rows = s.rows();
    // 走査順（パスの要素の昇順）に id が振られる
    assert_eq!(
        s.paths(),
        ["photos/no_gps.jpg", "photos/with_gps.jpg"],
        "{rows:?}"
    );
    let with_gps = s.row("photos/with_gps.jpg");
    assert_eq!(with_gps.source, "local");
    assert_eq!(with_gps.name, "with_gps.jpg");
    assert_eq!(with_gps.taken_at.as_deref(), Some("2024-05-06 07:08"));
    assert_eq!(with_gps.lat, Some(12.575));
    assert_eq!(with_gps.lon, Some(78.152778));
    assert_eq!(with_gps.loc_source.as_deref(), Some("exif"));
    assert_eq!(with_gps.mtime, Some(1_700_000_000.123_456_8));
    assert_eq!(with_gps.display, None);
    assert_eq!(with_gps.fetched_at, None);
    // `indexed_at` は現地時刻のオフセット無しの ISO（`YYYY-MM-DDTHH:MM:SS`）
    let at = with_gps.indexed_at.unwrap();
    assert_eq!(at.len(), 19, "{at}");
    assert_eq!(&at[10..11], "T", "{at}");

    // EXIF の無い写真は、時刻が空文字（NULL でなく）・位置と loc_source が NULL
    let no_gps = s.row("photos/no_gps.jpg");
    assert_eq!(no_gps.taken_at.as_deref(), Some(""));
    assert_eq!(
        (no_gps.lat, no_gps.lon, no_gps.loc_source),
        (None, None, None)
    );
}

#[test]
fn 変化が無ければ2回目は何も書かない() {
    let s = setup("local-idempotent");
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.put("photos/sub/b.png", &jpeg_plain(), T0); // 中身は JPEG でも拡張子は PNG（対象にする）
    s.run(&IndexLocal);
    let before = s.rows();
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "files"), 2);
    for name in ["added", "updated", "removed", "unreadable"] {
        assert_eq!(get(&counts, name), 0, "{name}");
    }
    assert_eq!(s.rows(), before);
    // dry-run も同じ答え
    let dry = s.dry_run(&IndexLocal);
    assert_eq!(
        get(&dry, "added") + get(&dry, "updated") + get(&dry, "removed"),
        0
    );
}

#[test]
fn mtimeの差が1秒未満なら更新しない_以上なら更新する() {
    let s = setup("local-tolerance");
    let path = s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    let first = s.row("photos/a.jpg");
    // 0.9 秒ずれても据え置き（現行の `abs(diff) < 1`）
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::new(T0, 123_456_789) + Duration::from_millis(900))
        .unwrap();
    assert_eq!(get(&s.run(&IndexLocal), "updated"), 0);
    assert_eq!(s.row("photos/a.jpg"), first);
    // 1 秒以上なら更新
    set_mtime(&path, T0 + 2);
    assert_eq!(get(&s.run(&IndexLocal), "updated"), 1);
    assert_ne!(s.row("photos/a.jpg").mtime, first.mtime);
}

#[test]
fn 更新は現行と同じ列だけを上書きする_手動でない位置は原本の値に戻る() {
    let s = setup("local-update");
    let path = s.put("photos/a.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    // 推定の位置（手動ではない）に、地名・表示用・お気に入りなどを持たせる
    s.sql(
        "UPDATE photos SET lat=35.0, lon=139.0, loc_source='inferred', place='どこか', elev=3.0,
         display='derivatives/x.jpg', fetched_at='2024-01-01T00:00:00', favorite=1, hidden=1,
         rotation=90, title='題', inferred_from=7, infer_dt_s=8, pref='県', city='市', muni_cd='01'
         WHERE path='photos/a.jpg'",
    );
    set_mtime(&path, T0 + 10);
    fs::write(&path, jpeg_gps()).unwrap();
    set_mtime(&path, T0 + 10);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "updated"), 1);
    assert_eq!(get(&counts, "location_kept"), 0);

    let row = s.row("photos/a.jpg");
    // 手動でない位置は原本の値で上書きされる（現行と同じ）
    assert_eq!(row.lat, Some(12.575));
    assert_eq!(row.loc_source.as_deref(), Some("exif"));
    assert_eq!(row.place, None);
    assert_eq!(row.display, None);
    assert_eq!(row.taken_at.as_deref(), Some("2024-05-06 07:08"));
    // 触らない列
    assert_eq!(row.fetched_at.as_deref(), Some("2024-01-01T00:00:00"));
    let read = crate::db::open_read_only(&s.config.db_path()).unwrap();
    let kept: (
        i64,
        i64,
        i64,
        String,
        i64,
        i64,
        String,
        String,
        String,
        Option<f64>,
    ) = read
        .query_row(
            "SELECT favorite, hidden, rotation, title, inferred_from, infer_dt_s, pref, city,
                    muni_cd, elev FROM photos WHERE path='photos/a.jpg'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        kept,
        (
            1,
            1,
            90,
            "題".into(),
            7,
            8,
            "県".into(),
            "市".into(),
            "01".into(),
            None
        )
    );
    // 同じ行のまま（id が変わらない）
    assert_eq!(s.rows().len(), 1);
}

/// T3.7 ①: 手動の位置（`loc_source='manual'`）は、mtime が変わっても `lat lon loc_source place elev` を
/// 書き換えない。撮影時刻・mtime・表示・indexed_at は今までどおり更新される。
#[test]
fn 更新しても手動の位置と地名は残る() {
    let s = setup("local-update-manual");
    let path = s.put("photos/a.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    s.sql(
        "UPDATE photos SET lat=35.0, lon=139.0, loc_source='manual', place='どこか', elev=3.0,
         display='photos/a.jpg', fetched_at='2024-01-01T00:00:00', favorite=1, hidden=1,
         rotation=90, title='題', inferred_from=7, infer_dt_s=8, pref='県', city='市', muni_cd='01',
         indexed_at='2000-01-01T00:00:00' WHERE path='photos/a.jpg'",
    );
    set_mtime(&path, T0 + 10);
    fs::write(&path, jpeg_gps()).unwrap(); // 原本には別の位置（EXIF）がある
    set_mtime(&path, T0 + 10);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "updated"), 1);
    assert_eq!(get(&counts, "location_kept"), 1);

    let row = s.row("photos/a.jpg");
    assert_eq!(
        (row.loc_source.as_deref(), row.lat, row.lon),
        (Some("manual"), Some(35.0), Some(139.0))
    );
    assert_eq!(row.place.as_deref(), Some("どこか"));
    // 位置以外は今までどおり更新される
    assert_eq!(row.taken_at.as_deref(), Some("2024-05-06 07:08"));
    assert_eq!(row.display, None);
    assert_ne!(row.mtime, Some(T0 as f64 + 0.123456789));
    let read = crate::db::open_read_only(&s.config.db_path()).unwrap();
    let kept: (f64, String, String, String, i64, i64, i64, String, i64, i64) = read
        .query_row(
            "SELECT elev, pref, city, muni_cd, favorite, hidden, rotation, title, inferred_from, infer_dt_s
             FROM photos WHERE path='photos/a.jpg'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        kept,
        (
            3.0,
            "県".into(),
            "市".into(),
            "01".into(),
            1,
            1,
            90,
            "題".into(),
            7,
            8
        )
    );
    // indexed_at は更新される（この行の mtime の変更は索引した）
    let indexed: String = read
        .query_row(
            "SELECT indexed_at FROM photos WHERE path='photos/a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(indexed, "2000-01-01T00:00:00");
}

/// `--dry-run` の写しにも同じ規則が当たり、件数が同じになる。実 DB は変わらない。
#[test]
fn dry_runでも手動の行の位置を残す規則で数える() {
    let s = setup("local-update-manual-dry");
    let m = s.put("photos/m.jpg", &jpeg_plain(), T0);
    let n = s.put("photos/n.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    s.sql("UPDATE photos SET lat=35.0, lon=139.0, loc_source='manual' WHERE path='photos/m.jpg'");
    for path in [&m, &n] {
        fs::write(path, jpeg_gps()).unwrap();
        set_mtime(path, T0 + 10);
    }
    let before = s.rows().len();
    let read = s.read_for(Mode::DryRun);
    let result = s.with_ctx(Mode::DryRun, &read, &StopFlag::new(), |ctx| {
        IndexLocal.run(ctx)
    });
    let Ok(StageOutcome::Done(counts)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(
        (get(&counts, "updated"), get(&counts, "location_kept")),
        (2, 1)
    );
    let lat_of = |path: &str| -> f64 {
        read.query_row("SELECT lat FROM photos WHERE path=?1", [path], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        (lat_of("photos/m.jpg"), lat_of("photos/n.jpg")),
        (35.0, 12.575)
    );
    // 実 DB は変わらない
    assert_eq!(s.rows().len(), before);
    assert_eq!(s.row("photos/m.jpg").lat, Some(35.0));
    assert_eq!(s.row("photos/n.jpg").lat, None);
}

/// 更新 API を writer 経由で呼ぶ（割り込みの試験用）。
fn api_set_loc(writer: &Writer, id: i64, lat: f64, lon: f64) {
    let payload = serde_json::json!({"id": id, "lat": lat, "lon": lon});
    crate::update::set_loc(payload.as_object().unwrap(), Some(writer)).unwrap();
}

fn api_set_flag(writer: &Writer, id: i64, favorite: bool, hidden: bool) {
    let payload = serde_json::json!({"id": id, "favorite": favorite, "hidden": hidden});
    crate::update::set_flag(payload.as_object().unwrap(), Some(writer)).unwrap();
}

/// `(favorite, hidden)`。
fn flags_of(s: &Setup, id: i64) -> Option<(i64, i64)> {
    let read = crate::db::open_read_only(&s.config.db_path()).unwrap();
    read.query_row(
        "SELECT favorite, hidden FROM photos WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

#[test]
fn 計画のあとに更新apiが入っても_お気に入りと除外と手動の位置は残る() {
    let mut s = setup("local-update-api");
    let a = s.put("photos/a.jpg", &jpeg_plain(), T0); // ファイルが変わる（Update）
    s.put("photos/b.jpg", &jpeg_plain(), T0); // 変わらない（計画に入らない）
    let c = s.put("photos/c.jpg", &jpeg_plain(), T0); // 消える（Delete）
    s.run(&IndexLocal);
    let (ida, idb, idc) = (
        s.row("photos/a.jpg").id,
        s.row("photos/b.jpg").id,
        s.row("photos/c.jpg").id,
    );
    set_mtime(&a, T0 + 10);
    fs::write(&a, jpeg_gps()).unwrap();
    set_mtime(&a, T0 + 10);
    fs::remove_file(&c).unwrap();
    // 計画を読んだあと、書く直前に更新 API が入る
    let api = s.writer.clone();
    s.writer = api.clone().interrupt_next_call(move || {
        api_set_flag(&api, ida, true, true);
        api_set_loc(&api, ida, 43.0, 141.0);
        api_set_loc(&api, idb, 48.0, 2.0);
        api_set_flag(&api, idb, true, false);
        api_set_flag(&api, idc, true, true);
    });
    let counts = s.run(&IndexLocal);
    assert_eq!((get(&counts, "updated"), get(&counts, "removed")), (1, 1));

    // 変わらない行: 手動の位置も、お気に入りも残る
    let b = s.row("photos/b.jpg");
    assert_eq!(
        (b.loc_source.as_deref(), b.lat, b.lon),
        (Some("manual"), Some(48.0), Some(2.0))
    );
    assert_eq!(flags_of(&s, idb), Some((1, 0)));
    // 更新される行: お気に入り・除外は索引の SET 句に無いので残る。
    // 位置は、計画のあとに更新 API が手動で直したものが残る（T3.7 ①。書く時点で `manual` なら位置を書かない。
    // 計画の時点では手動でなかったので、件数の `location_kept` には数えない）
    assert_eq!(flags_of(&s, ida), Some((1, 1)));
    let a_row = s.row("photos/a.jpg");
    assert_eq!(
        (a_row.loc_source.as_deref(), a_row.lat, a_row.lon),
        (Some("manual"), Some(43.0), Some(141.0))
    );
    assert_eq!(a_row.taken_at.as_deref(), Some("2024-05-06 07:08"));
    assert_eq!(get(&counts, "location_kept"), 0);
    // 消える行は、お気に入りごと消える（§2.3 ②。T3.7）
    assert_eq!(flags_of(&s, idc), None);
}

#[test]
fn 消えたファイルの行を削除しderivativeだけファイルも消す() {
    let s = setup("local-delete");
    let a = s.put("photos/a.heic", &heic(), T0);
    let b = s.put("photos/b.jpg", &jpeg_gps(), T0);
    s.put("photos/c.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    // a は derivative を持つ。b は原本を display にしている
    fs::create_dir_all(s.root().join("derivatives")).unwrap();
    fs::write(s.root().join("derivatives/local_x.jpg"), b"d").unwrap();
    s.sql(
        "UPDATE photos SET display='derivatives/local_x.jpg' WHERE path='photos/a.heic';
         UPDATE photos SET display='photos/b.jpg' WHERE path='photos/b.jpg'",
    );
    fs::remove_file(&a).unwrap();
    // b は原本が消えても、`photos/` の display は消す対象にならない（原本は元々無いが、他の原本を巻き込まない）
    fs::remove_file(&b).unwrap();
    fs::write(s.root().join("photos/other.txt"), b"keep").unwrap();
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "removed"), 2);
    assert_eq!(get(&counts, "derivative_files_removed"), 1);
    assert_eq!(s.paths(), ["photos/c.jpg"]);
    assert!(!s.root().join("derivatives/local_x.jpg").exists());
    assert!(s.root().join("photos/c.jpg").exists());
    assert!(s.root().join("photos/other.txt").exists());
}

#[test]
fn 危険なdisplayのファイルは消さずに行だけ消す() {
    let s = setup("local-delete-unsafe");
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    fs::write(s.root().join("secret.txt"), b"keep").unwrap();
    fs::create_dir_all(s.root().join("derivatives")).unwrap();
    s.sql("UPDATE photos SET display='derivatives/../secret.txt'");
    fs::remove_file(s.root().join("photos/a.jpg")).unwrap();
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "removed"), 1);
    assert_eq!(get(&counts, "derivative_files_unsafe"), 1);
    assert!(s.rows().is_empty());
    assert!(s.root().join("secret.txt").exists());
}

#[test]
fn gdriveを含むパスと対象外の拡張子は取り込まない() {
    let s = setup("local-gdrive");
    s.put("photos/gdrive/a.jpg", &jpeg_gps(), T0);
    s.put("photos/x/gdrive/deep/b.jpg", &jpeg_gps(), T0);
    s.put("photos/gdrive2/c.jpg", &jpeg_gps(), T0); // 完全一致ではないので対象
    s.put("photos/mygdrive/d.jpg", &jpeg_gps(), T0);
    s.put("photos/e.JPG", &jpeg_gps(), T0); // 大文字の拡張子
    s.put("photos/f.txt", b"x", T0);
    s.put("photos/g.gif", b"x", T0);
    s.put("photos/.hidden/h.jpeg", &jpeg_gps(), T0); // 隠しディレクトリも対象（現行と同じ）
    s.put("photos/.i.jpg", &jpeg_gps(), T0);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "added"), 5);
    assert_eq!(
        s.paths(),
        [
            "photos/.hidden/h.jpeg",
            "photos/.i.jpg",
            "photos/e.JPG",
            "photos/gdrive2/c.jpg",
            "photos/mygdrive/d.jpg",
        ]
    );
}

#[test]
fn 既存のgdrive配下の行は削除される() {
    let s = setup("local-gdrive-delete");
    s.put("photos/gdrive/a.jpg", &jpeg_gps(), T0);
    s.sql(
        "INSERT INTO photos(source, path, name, mtime) VALUES('local','photos/gdrive/a.jpg','a.jpg',1.0)",
    );
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "removed"), 1);
    assert!(s.rows().is_empty());
}

#[test]
fn 走査順はパスの要素の列の昇順() {
    let s = setup("local-order");
    // 文字列として並べると `a-b.jpg` < `a/b.jpg`（`-` は `/` より前）だが、要素の列では `a` < `a-b.jpg`
    s.put("photos/a-b.jpg", &jpeg_plain(), T0);
    s.put("photos/a/b.jpg", &jpeg_plain(), T0);
    s.put("photos/B.jpg", &jpeg_plain(), T0);
    s.put("photos/日本/z.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    assert_eq!(
        s.paths(),
        [
            "photos/B.jpg",
            "photos/a/b.jpg",
            "photos/a-b.jpg",
            "photos/日本/z.jpg",
        ]
    );
    let ids: Vec<_> = s.rows().iter().map(|r| r.id).collect();
    assert_eq!(ids, [1, 2, 3, 4]);
}

#[test]
fn 読めない写真は飛ばし_既存の行は据え置く() {
    let s = setup("local-unreadable");
    s.put("photos/broken.jpg", b"not an image", T0);
    s.put("photos/ok.jpg", &jpeg_gps(), T0);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "added"), 1);
    assert_eq!(get(&counts, "unreadable"), 1);
    assert_eq!(s.paths(), ["photos/ok.jpg"]);
    // 既に索引済みの写真が壊れても、行は消さず更新もしない（走査では見つけているので `seen` に入る）
    let before = s.row("photos/ok.jpg");
    s.put("photos/ok.jpg", b"broken now", T0 + 100);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "unreadable"), 2);
    assert_eq!((get(&counts, "updated"), get(&counts, "removed")), (0, 0));
    assert_eq!(s.row("photos/ok.jpg"), before);
}

#[test]
fn ディレクトリのリンクには降りず_置き場の外を指すファイルのリンクは読まない() {
    let s = setup("local-symlink");
    let outside = TempDir::new("local-symlink-outside");
    fs::write(outside.path().join("secret.jpg"), jpeg_gps()).unwrap();
    s.put("photos/real.jpg", &jpeg_gps(), T0);
    s.put("inner/target.jpg", &jpeg_gps(), T0);
    fs::create_dir_all(s.root().join("photos")).unwrap();
    // 外のディレクトリ・内のディレクトリへのリンクは降りない
    symlink(outside.path(), s.root().join("photos/outdir")).unwrap();
    symlink(s.root().join("inner"), s.root().join("photos/indir")).unwrap();
    // 外のファイルへのリンクは読まない。内のファイルへのリンクは取り込む
    symlink(
        outside.path().join("secret.jpg"),
        s.root().join("photos/out.jpg"),
    )
    .unwrap();
    symlink(
        s.root().join("inner/target.jpg"),
        s.root().join("photos/in.jpg"),
    )
    .unwrap();
    // 切れたリンクは無視
    symlink(
        s.root().join("nowhere.jpg"),
        s.root().join("photos/broken.jpg"),
    )
    .unwrap();
    let counts = s.run(&IndexLocal);
    assert_eq!(s.paths(), ["photos/in.jpg", "photos/real.jpg"]);
    assert_eq!(get(&counts, "skipped"), 1);
}

#[test]
fn 置き場がdata_dirの外やdata_dirそのものなら断る() {
    for value in ["..", ".", "/tmp"] {
        let s = setup_with("local-bad-root", &[("GEOFRAME_PHOTOS_DIR", value)]);
        match s.run_with(&IndexLocal, Mode::Apply, &StopFlag::new()) {
            StageResult::Failed(message) => {
                assert!(
                    message.contains("GEOFRAME_PHOTOS_DIR"),
                    "{value}: {message}"
                );
            }
            other => panic!("{value}: {other:?}"),
        }
        assert!(s.rows().is_empty());
    }
    // DATA_DIR の中のサブディレクトリなら別名でもよい
    let s = setup_with("local-alt-root", &[("GEOFRAME_PHOTOS_DIR", "pics/in")]);
    fs::create_dir_all(s.root().join("pics")).unwrap();
    s.put("pics/in/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    assert_eq!(s.paths(), ["pics/in/a.jpg"]);
}

#[test]
fn 置き場が無ければ作る_dry_runでは作らない() {
    let s = setup("local-mkdir");
    assert!(!s.root().join("photos").exists());
    let dry = s.dry_run(&IndexLocal);
    assert_eq!(get(&dry, "files"), 0);
    assert!(!s.root().join("photos").exists());
    s.run(&IndexLocal);
    assert!(s.root().join("photos").is_dir());
}

#[test]
fn dry_runは件数だけ出して何も書かない() {
    let s = setup("local-dry");
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.put("photos/b.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    let before = s.rows();
    // 1 枚更新・1 枚追加・1 枚削除の状況を作る
    s.put("photos/a.jpg", &jpeg_plain(), T0 + 50);
    s.put("photos/c.jpg", &jpeg_gps(), T0);
    fs::remove_file(s.root().join("photos/b.jpg")).unwrap();
    s.sql("UPDATE photos SET display='derivatives/d.jpg' WHERE path='photos/b.jpg'");
    fs::create_dir_all(s.root().join("derivatives")).unwrap();
    fs::write(s.root().join("derivatives/d.jpg"), b"d").unwrap();
    let rows_before_dry = s.rows();
    let counts = s.dry_run(&IndexLocal);
    assert_eq!(
        (
            get(&counts, "added"),
            get(&counts, "updated"),
            get(&counts, "removed"),
            get(&counts, "derivative_files_removed")
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(s.rows(), rows_before_dry);
    assert!(s.root().join("derivatives/d.jpg").exists());
    assert_ne!(before, rows_before_dry);
}

#[test]
fn 停止の合図が来たら何も書かず削除もしない() {
    let s = setup("local-stop");
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    fs::remove_file(s.root().join("photos/a.jpg")).unwrap();
    s.put("photos/b.jpg", &jpeg_gps(), T0);
    let stop = StopFlag::new();
    stop.stop();
    let result = s.run_with(&IndexLocal, Mode::Apply, &stop);
    assert!(matches!(result, StageResult::Skipped(_)), "{result:?}");
    // 走査が途中のまま「見つからなかった」行を消さない
    assert_eq!(s.paths(), ["photos/a.jpg"]);
}

#[test]
fn ログに出す文面にパスやファイル名は入らない() {
    let s = setup("local-log");
    s.put("photos/secret-name.jpg", b"broken", T0);
    let counts = s.run(&IndexLocal);
    assert!(!counts.to_string().contains("secret"), "{counts}");
    let err = StageError::from(PathError::Escapes);
    assert!(!err.to_string().contains('/'), "{err}");
}

// ---------------------------------------------------------------- local_derivatives

fn index_with_location(s: &Setup, files: &[(&str, Vec<u8>)]) {
    for (rel, bytes) in files {
        s.put(rel, bytes, T0);
    }
    s.run(&IndexLocal);
    // 合成の HEIC・PNG には GPS が無いので、位置ありにして対象にする
    s.sql("UPDATE photos SET lat=35.0, lon=139.0, loc_source='manual' WHERE lat IS NULL");
}

#[test]
fn jpegは原本をdisplayにしheicはderivativeを作る() {
    let s = setup("local-deriv");
    index_with_location(
        &s,
        &[
            ("photos/a.heic", heic()),
            ("photos/b.JPG", jpeg_gps()),
            ("photos/c.png", jpeg_plain()),
        ],
    );
    let counts = s.run(&LocalDerivatives::new());
    assert_eq!(get(&counts, "original"), 2);
    assert_eq!(get(&counts, "made"), 1);
    assert_eq!(get(&counts, "failed"), 0);

    let heic_row = s.row("photos/a.heic");
    assert_eq!(
        heic_row.display.as_deref(),
        Some("derivatives/local_e794953465bdae8f.jpg")
    );
    assert!(
        s.root()
            .join("derivatives/local_e794953465bdae8f.jpg")
            .is_file()
    );
    // 原本の相対パスがそのまま display（大文字の拡張子も）
    assert_eq!(
        s.row("photos/b.JPG").display.as_deref(),
        Some("photos/b.JPG")
    );
    assert_eq!(
        s.row("photos/c.png").display.as_deref(),
        Some("photos/c.png")
    );
    for row in s.rows() {
        let at = row.fetched_at.unwrap();
        assert_eq!((at.len(), &at[10..11]), (19, "T"), "{at}");
    }
    // 2 回目は何もしない
    let again = s.run(&LocalDerivatives::new());
    assert_eq!((get(&again, "original"), get(&again, "made")), (0, 0));
}

#[test]
fn derivativeの作成中に更新apiが入っても_位置とお気に入りは残る() {
    let mut s = setup("local-deriv-update-api");
    index_with_location(
        &s,
        &[("photos/a.heic", heic()), ("photos/b.JPG", jpeg_gps())],
    );
    let (ida, idb) = (s.row("photos/a.heic").id, s.row("photos/b.JPG").id);
    let api = s.writer.clone();
    s.writer = api.clone().interrupt_next_call(move || {
        api_set_loc(&api, ida, 43.0, 141.0);
        api_set_flag(&api, ida, true, true);
        api_set_flag(&api, idb, false, true);
    });
    let counts = s.run(&LocalDerivatives::new());
    assert_eq!(get(&counts, "made") + get(&counts, "original"), 2);
    let a = s.row("photos/a.heic");
    assert_eq!(
        (a.loc_source.as_deref(), a.lat),
        (Some("manual"), Some(43.0))
    );
    assert!(a.display.is_some());
    assert_eq!(flags_of(&s, ida), Some((1, 1)));
    assert_eq!(flags_of(&s, idb), Some((0, 1)));
    assert!(s.row("photos/b.JPG").display.is_some());
}

#[test]
fn 位置の無い写真と他のsourceの行は対象にしない() {
    let s = setup("local-deriv-filter");
    s.put("photos/nogps.heic", &heic(), T0);
    s.run(&IndexLocal);
    s.sql(
        "INSERT INTO photos(source, drive_id, name, lat, lon) VALUES('drive','d1','x.jpg',1.0,2.0)",
    );
    let counts = s.run(&LocalDerivatives::new());
    assert_eq!(get(&counts, "made") + get(&counts, "original"), 0);
    assert!(!s.root().join("derivatives").exists());
    assert!(s.rows().iter().all(|r| r.display.is_none()));
}

#[test]
fn 一度に作る枚数はfetch_per_cycleまで_idの順() {
    let s = setup_with("local-deriv-limit", &[("GEOFRAME_FETCH_PER_CYCLE", "2")]);
    index_with_location(
        &s,
        &[
            ("photos/1.jpg", jpeg_gps()),
            ("photos/2.jpg", jpeg_gps()),
            ("photos/3.jpg", jpeg_gps()),
        ],
    );
    assert_eq!(get(&s.run(&LocalDerivatives::new()), "original"), 2);
    assert!(s.row("photos/1.jpg").display.is_some());
    assert!(s.row("photos/2.jpg").display.is_some());
    assert!(s.row("photos/3.jpg").display.is_none());
    assert_eq!(get(&s.run(&LocalDerivatives::new()), "original"), 1);
    assert!(s.row("photos/3.jpg").display.is_some());
}

#[test]
fn 変換に失敗した写真は数えて続ける() {
    let s = setup("local-deriv-fail");
    // 画像として読めるのは索引の時点だけ。後から壊れた HEIC
    index_with_location(&s, &[("photos/a.heic", heic()), ("photos/b.heic", heic())]);
    fs::write(s.root().join("photos/a.heic"), b"broken").unwrap();
    let counts = s.run(&LocalDerivatives::new());
    assert_eq!((get(&counts, "failed"), get(&counts, "made")), (1, 1));
    assert!(s.row("photos/a.heic").display.is_none());
    assert!(s.row("photos/b.heic").display.is_some());
    // 壊れた写真の `.part` は残らない
    let leftovers: Vec<_> = fs::read_dir(s.root().join("derivatives"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn 失敗が続く写真は枠を塞がず_後続の写真が作られる() {
    // T2.4 の受入条件②（local_derivatives にも同じ仕組み）。1 サイクル 2 枚、先頭の 3 枚が恒久的に失敗する
    let s = setup_with("local-deriv-block", &[("GEOFRAME_FETCH_PER_CYCLE", "2")]);
    index_with_location(
        &s,
        &[
            ("photos/a1.heic", heic()),
            ("photos/a2.heic", heic()),
            ("photos/a3.heic", heic()),
            ("photos/b1.jpg", jpeg_gps()),
            ("photos/b2.jpg", jpeg_gps()),
            ("photos/b3.jpg", jpeg_gps()),
        ],
    );
    for name in ["a1", "a2", "a3"] {
        fs::write(s.root().join(format!("photos/{name}.heic")), b"broken").unwrap();
    }
    let stage = LocalDerivatives::new();
    let shown = |s: &Setup| {
        s.rows()
            .iter()
            .filter(|r| r.display.is_some())
            .map(|r| r.path.clone().unwrap())
            .collect::<Vec<_>>()
    };
    // 1 回目: id の若い a1・a2 を試して失敗
    let counts = s.run(&stage);
    assert_eq!((get(&counts, "failed"), get(&counts, "original")), (2, 0));
    assert!(shown(&s).is_empty());
    // 2 回目: a1・a2 を飛ばし、a3（失敗）と b1
    let counts = s.run(&stage);
    assert_eq!(get(&counts, "skipped_backoff"), 2);
    assert_eq!((get(&counts, "failed"), get(&counts, "original")), (1, 1));
    // 3 回目: 失敗中の 3 枚を飛ばして b2・b3
    let counts = s.run(&stage);
    assert_eq!(get(&counts, "skipped_backoff"), 3);
    assert_eq!(get(&counts, "original"), 2);
    assert_eq!(
        shown(&s),
        ["photos/b1.jpg", "photos/b2.jpg", "photos/b3.jpg"]
    );
    // 4 回目: 失敗中のものだけが残り、何も試さない
    let counts = s.run(&stage);
    assert_eq!((get(&counts, "failed"), get(&counts, "made")), (0, 0));
    assert_eq!(get(&counts, "skipped_backoff"), 3);
    // ファイルが更新されれば（mtime が変わる）すぐ試す
    s.put("photos/a1.heic", &heic(), T0 + 100);
    s.run(&IndexLocal);
    s.sql("UPDATE photos SET lat=35.0, lon=139.0, loc_source='manual' WHERE lat IS NULL");
    let counts = s.run(&stage);
    assert_eq!((get(&counts, "made"), get(&counts, "failed")), (1, 0));
    assert!(s.row("photos/a1.heic").display.is_some());
}

#[test]
fn dry_runは失敗を覚えない() {
    let s = setup("local-deriv-dry-memo");
    index_with_location(&s, &[("photos/a.heic", heic())]);
    fs::write(s.root().join("photos/a.heic"), b"broken").unwrap();
    let stage = LocalDerivatives::new();
    // dry-run は変換しないので失敗も分からないが、`resolve_existing` の失敗（存在しない）は数える。覚えない
    fs::remove_file(s.root().join("photos/a.heic")).unwrap();
    assert_eq!(get(&s.dry_run(&stage), "failed"), 1);
    assert_eq!(get(&s.dry_run(&stage), "failed"), 1);
    assert!(!s.dry_run(&stage).to_string().contains("skipped_backoff"));
}

#[test]
fn dbのpathが置き場の外を指しても読まない() {
    let s = setup("local-deriv-escape");
    let outside = TempDir::new("local-deriv-escape-out");
    fs::write(outside.path().join("x.heic"), heic()).unwrap();
    symlink(outside.path(), s.root().join("link")).unwrap();
    s.sql(
        "INSERT INTO photos(source, path, name, lat, lon) VALUES
         ('local','../x.heic','x.heic',1.0,2.0),
         ('local','link/x.heic','x.heic',1.0,2.0),
         ('local','/etc/passwd.heic','x.heic',1.0,2.0)",
    );
    let counts = s.run(&LocalDerivatives::new());
    assert_eq!(get(&counts, "failed"), 3);
    assert!(s.rows().iter().all(|r| r.display.is_none()));
    assert!(!s.root().join("derivatives").exists());
}

#[test]
fn derivativeのdry_runは作らず件数だけ() {
    let s = setup("local-deriv-dry");
    index_with_location(
        &s,
        &[("photos/a.heic", heic()), ("photos/b.jpg", jpeg_gps())],
    );
    let counts = s.dry_run(&LocalDerivatives::new());
    assert_eq!((get(&counts, "original"), get(&counts, "made")), (1, 1));
    assert!(s.rows().iter().all(|r| r.display.is_none()));
    assert!(!s.root().join("derivatives").exists());
}

// ---------------------------------------------------------------- organize

#[test]
fn organizeは位置の無い写真を退避して行を消す() {
    let s = setup("local-organize");
    s.put("photos/a/no_gps.jpg", &jpeg_plain(), T0);
    s.put("photos/b/no_gps.jpg", &jpeg_plain(), T0 + 1); // 同名（移動先で衝突する）
    s.put("photos/with_gps.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    // 位置ありの写真（錨）があるので、撮影時刻の無い写真だけが対象
    let counts = s.organize(Mode::Apply);
    assert_eq!(get(&counts, "moved"), 2, "{counts}");
    assert_eq!(s.paths(), ["photos/with_gps.jpg"]);
    let dest = s.root().join("_excluded/no_gps");
    let mut names: Vec<_> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // 同名は上書きせず別名にする
    assert_eq!(names, ["no_gps (2).jpg", "no_gps.jpg"]);
    assert!(!s.root().join("photos/a/no_gps.jpg").exists());
    assert!(s.root().join("photos/with_gps.jpg").exists());
}

#[test]
fn 退避先に同名があっても上書きしない() {
    let s = setup("local-organize-collide");
    s.put("photos/x.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    fs::create_dir_all(s.root().join("_excluded/no_gps")).unwrap();
    fs::write(s.root().join("_excluded/no_gps/x.jpg"), b"previous").unwrap();
    s.organize(Mode::Apply);
    assert_eq!(
        fs::read(s.root().join("_excluded/no_gps/x.jpg")).unwrap(),
        b"previous"
    );
    assert!(s.root().join("_excluded/no_gps/x (2).jpg").is_file());
    assert!(s.rows().is_empty());
}

#[test]
fn 推定で位置が付く写真は移さない() {
    let s = setup("local-organize-inferred");
    // 錨（GPS・撮影時刻 2024-05-06 07:08）と、位置の無い 3 枚: 錨の 22 分後・13 時間後・撮影時刻なし
    s.put("photos/anchor.jpg", &jpeg_gps(), T0);
    s.put("photos/near.jpg", &fixture_jpeg("time_digitized_only"), T0);
    s.put(
        "photos/far.jpg",
        &fixture_jpeg("time_datetime_ifd0_only"),
        T0,
    );
    s.put("photos/untimed.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    s.sql("UPDATE photos SET taken_at='2024-05-06 07:30' WHERE path='photos/near.jpg'");
    s.sql("UPDATE photos SET taken_at='2024-05-06 20:09' WHERE path='photos/far.jpg'");
    let dry = s.organize(Mode::DryRun);
    // 移すのは窓（12 時間）の外の 1 枚と、撮影時刻のない 1 枚。推定で位置が付く 1 枚と錨は残す
    assert_eq!(
        (
            get(&dry, "targets"),
            get(&dry, "moved"),
            get(&dry, "failed")
        ),
        (2, 2, 0)
    );
    assert!(
        dry.0
            .iter()
            .all(|(name, _)| *name != "held_back_until_infer")
    );
    // dry-run は何も動かさない
    assert_eq!(s.paths().len(), 4);
    let counts = s.organize(Mode::Apply);
    assert_eq!(get(&counts, "moved"), 2);
    assert_eq!(s.paths(), ["photos/anchor.jpg", "photos/near.jpg"]);
    let near = s.row("photos/near.jpg");
    assert_eq!(near.loc_source.as_deref(), Some("inferred"));
    assert_eq!(near.lat, Some(12.575));
    // 錨が無くなれば、推定は取り消され、次の organize で移る（現行と同じ順序: 索引→推定→移動）
    fs::remove_file(s.root().join("photos/anchor.jpg")).unwrap();
    s.run(&IndexLocal);
    assert_eq!(get(&s.organize(Mode::Apply), "moved"), 1);
    assert!(s.rows().is_empty());
}

#[test]
fn organizeのdry_runは何も動かさない() {
    let s = setup("local-organize-dry");
    s.put("photos/a.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    let counts = s.organize(Mode::DryRun);
    assert_eq!(get(&counts, "targets"), 1);
    assert!(s.root().join("photos/a.jpg").exists());
    assert!(!s.root().join("_excluded").exists());
    assert_eq!(s.paths(), ["photos/a.jpg"]);
}

#[test]
fn ファイルが無い行は行だけ消し_リンクは動かさない() {
    let s = setup("local-organize-edge");
    s.put("photos/gone.jpg", &jpeg_plain(), T0);
    s.put("photos/target.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    fs::remove_file(s.root().join("photos/gone.jpg")).unwrap();
    // 索引のあとに差し替えられたリンク（実体は置き場の中）
    fs::remove_file(s.root().join("photos/target.jpg")).unwrap();
    s.put("photos/real.bin", b"x", T0);
    symlink(
        s.root().join("photos/real.bin"),
        s.root().join("photos/target.jpg"),
    )
    .unwrap();
    // 索引を通さず organize だけを呼ぶ（行が残っている状態）
    let counts = s.organize(Mode::Apply);
    assert_eq!(get(&counts, "row_only_removed"), 1, "{counts}");
    assert_eq!(get(&counts, "skipped"), 1, "{counts}");
    assert_eq!(get(&counts, "moved"), 0);
    assert_eq!(s.paths(), ["photos/target.jpg"]);
    assert!(s.root().join("photos/real.bin").exists());
}

#[test]
fn 退避先がリンクを経由するなら動かさない() {
    let s = setup("local-organize-link");
    let outside = TempDir::new("local-organize-link-out");
    s.put("photos/a.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    symlink(outside.path(), s.root().join("_excluded")).unwrap();
    let read = crate::db::open_read_only(&s.config.db_path()).unwrap();
    let http = crate::net::agent();
    let stop = StopFlag::new();
    let ctx = StageCtx {
        mode: Mode::Apply,
        config: &s.config,
        read: &read,
        writer: Some(&s.writer),
        cache: &s.cache,
        http: &http,
        stop: &stop,
    };
    assert!(organize(&ctx).is_err());
    assert!(s.root().join("photos/a.jpg").exists());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert_eq!(s.paths(), ["photos/a.jpg"]);
}

// ---------------------------------------------------------------- レビュー指摘（T2.3 の再レビュー）

/// `DATA_DIR` の下のファイルを `(相対パス, サイズ)` で（DB のファイルは除く）。
fn tree(root: &Path) -> Vec<(String, u64)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                out.push((format!("{rel}/"), 0));
                walk(root, &entry.path(), out);
            } else if !rel.starts_with("catalog.db") {
                out.push((rel, meta.len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// 写真を 1 枚索引して derivative の行を作り、写真のディレクトリごと消した状態にする。
fn setup_with_vanished_photos(tag: &str) -> Setup {
    let s = setup(tag);
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    s.sql("UPDATE photos SET display='derivatives/d.jpg'");
    fs::create_dir_all(s.root().join("derivatives")).unwrap();
    fs::write(s.root().join("derivatives/d.jpg"), b"d").unwrap();
    fs::remove_dir_all(s.root().join("photos")).unwrap();
    s
}

#[test]
fn 写真の置き場が無くてもdry_runは削除を数え_実行と同じ件数になる() {
    let dry_side = setup_with_vanished_photos("local-vanished-dry");
    let rows = dry_side.rows();
    let files = tree(dry_side.root());
    let dry = dry_side.dry_run(&IndexLocal);
    assert_eq!(
        (
            get(&dry, "files"),
            get(&dry, "removed"),
            get(&dry, "derivative_files_removed")
        ),
        (0, 1, 1),
        "{dry}"
    );
    // dry-run は何も変えない（置き場も作らない）
    assert_eq!(dry_side.rows(), rows);
    assert_eq!(tree(dry_side.root()), files);
    assert!(!dry_side.root().join("photos").exists());

    // 同じ状態に実行すると、同じ件数。置き場は作られ、行と derivative が消える
    let apply_side = setup_with_vanished_photos("local-vanished-apply");
    let applied = apply_side.run(&IndexLocal);
    assert_eq!(applied, dry);
    assert!(apply_side.root().join("photos").is_dir());
    assert!(apply_side.rows().is_empty());
    assert!(!apply_side.root().join("derivatives/d.jpg").exists());
}

#[test]
fn 写真が0枚で停止の合図が来ていても行を消さない() {
    let s = setup("local-stop-empty");
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    s.sql("UPDATE photos SET display='derivatives/d.jpg'");
    fs::create_dir_all(s.root().join("derivatives")).unwrap();
    fs::write(s.root().join("derivatives/d.jpg"), b"d").unwrap();
    fs::remove_file(s.root().join("photos/a.jpg")).unwrap(); // 置き場はあるが 0 枚
    let before = s.rows();
    let stop = StopFlag::new();
    stop.stop();
    let result = s.run_with(&IndexLocal, Mode::Apply, &stop);
    assert!(matches!(result, StageResult::Skipped(_)), "{result:?}");
    assert_eq!(s.rows(), before);
    assert!(s.root().join("derivatives/d.jpg").exists());
}

/// 停止の合図が 1 回目、2 回目、… の確認で来たとき、どの時点でも「何も変えない」か「全部やった」
/// のどちらかになる（途中までの削除・書き込みが残らない）。
#[test]
fn 停止の合図はどの確認点で来ても確定の前なら何も変えない() {
    let build = |tag: &str| {
        let s = setup(tag);
        s.put("photos/old.jpg", &jpeg_gps(), T0);
        s.put("photos/keep.jpg", &jpeg_gps(), T0);
        s.run(&IndexLocal);
        s.sql("UPDATE photos SET display='derivatives/old.jpg' WHERE path='photos/old.jpg'");
        fs::create_dir_all(s.root().join("derivatives")).unwrap();
        fs::write(s.root().join("derivatives/old.jpg"), b"d").unwrap();
        fs::remove_file(s.root().join("photos/old.jpg")).unwrap(); // 削除 1
        s.put("photos/keep.jpg", &jpeg_plain(), T0 + 50); // 更新 1
        s.put("photos/new1.jpg", &jpeg_gps(), T0); // 追加 2
        s.put("photos/new2.jpg", &jpeg_gps(), T0);
        s
    };
    let mut skipped = 0;
    for n in 1..=40 {
        let s = build(&format!("local-stop-sweep-{n}"));
        let (rows, files) = (s.rows(), tree(s.root()));
        let (result, calls) = s.index_stopping_at(n);
        match result.unwrap() {
            StageOutcome::Skipped(_) => {
                skipped += 1;
                assert_eq!(s.rows(), rows, "n={n}: 行が変わった");
                assert_eq!(tree(s.root()), files, "n={n}: ファイルが変わった");
                assert_eq!(calls, n, "n={n}: 合図の後にも確認が続いた");
            }
            StageOutcome::Done(counts) => {
                assert_eq!(
                    (
                        get(&counts, "added"),
                        get(&counts, "updated"),
                        get(&counts, "removed")
                    ),
                    (2, 1, 1),
                    "n={n}"
                );
                assert!(!s.root().join("derivatives/old.jpg").exists(), "n={n}");
                break;
            }
        }
    }
    // 確認点: 走査（ディレクトリ 1）・写真ごと（3）・計画の完了・書き込みの始め・COMMIT の直前
    assert_eq!(skipped, 7);
}

#[test]
fn 書き込みの途中でも停止の合図が来たら戻して確定しない() {
    let s = setup("local-stop-write");
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.run(&IndexLocal);
    let before = s.rows();
    let entry = |i: usize| Entry {
        rel: format!("photos/gen{i}.jpg"),
        name: format!("gen{i}.jpg"),
        mtime: 1.0,
        taken_at: String::new(),
        lat: None,
        lon: None,
        loc_source: None,
    };
    // 300 件の追加と既存の行の削除。1 回目（始め）は通り、2 回目（256 件目）で合図が来る
    let ops: Vec<Op> = (0..300).map(|i| Op::Insert(entry(i))).collect();
    let id = before[0].id;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let committed = apply_plan(
        &s.writer,
        "2024-01-01T00:00:00".into(),
        &ops,
        vec![id],
        move || counter.fetch_add(1, Ordering::SeqCst) + 1 >= 2,
    )
    .unwrap();
    assert!(!committed);
    assert_eq!(s.rows(), before);
    // 合図が来なければ全部確定する
    let committed = apply_plan(
        &s.writer,
        "2024-01-01T00:00:00".into(),
        &ops,
        vec![id],
        || false,
    )
    .unwrap();
    assert!(committed);
    assert_eq!(s.rows().len(), 300);
}

#[test]
fn 置き場の親がdata_dirの外なら_作る前に断り外には何も作らない() {
    let outside = TempDir::new("local-photos-outside");
    // 絶対パスで、まだ無い外のディレクトリ
    let absolute = outside.path().join("newdir");
    // `..` を経由する
    let unique = format!("geoframe-photos-escape-{}", std::process::id());
    let dotdot = format!("../{unique}");
    // 外へのリンクを経由する
    let via_link = "link/newdir".to_string();
    for (value, created) in [
        (absolute.to_str().unwrap().to_string(), absolute.clone()),
        (dotdot, outside.path().join("never")), // 下で別に確かめる
        (via_link, outside.path().join("newdir")),
    ] {
        for mode in [Mode::Apply, Mode::DryRun] {
            let s = setup_with("local-photos-escape", &[("GEOFRAME_PHOTOS_DIR", &value)]);
            symlink(outside.path(), s.root().join("link")).unwrap();
            let escaped = s.root().join("..").join(&unique);
            let result = s.run_with(&IndexLocal, mode, &StopFlag::new());
            match result {
                StageResult::Failed(message) => {
                    assert!(message.contains("GEOFRAME_PHOTOS_DIR"), "{message}");
                }
                other => panic!("{value} {mode:?}: {other:?}"),
            }
            assert!(!created.exists(), "{value} {mode:?}: 外にできた");
            assert!(!escaped.exists(), "{value} {mode:?}: 外にできた");
            assert!(s.rows().is_empty());
        }
    }
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn 置き場の親が無ければ作らずに失敗する() {
    // 作るのは最後の 1 階層だけ（現行の `mkdir` と同じ）
    let s = setup_with("local-photos-noparent", &[("GEOFRAME_PHOTOS_DIR", "a/b")]);
    let result = s.run_with(&IndexLocal, Mode::Apply, &StopFlag::new());
    assert!(matches!(result, StageResult::Failed(_)), "{result:?}");
    assert!(!s.root().join("a").exists());
}

// ---------------------------------------------------------------- dry-run の後続段

/// 索引済みの位置ありの HEIC が 1 枚あるところへ、新しい位置ありの JPEG と位置なしの HEIC を足した状態。
fn sequence_state(tag: &str) -> Setup {
    let s = setup(tag);
    index_with_location(&s, &[("photos/h.heic", heic())]);
    s.put("photos/a.jpg", &jpeg_gps(), T0);
    s.put("photos/n.heic", &heic(), T0);
    s
}

/// `index_local` → `local_derivatives` → `organize` を 1 回ずつ。dry-run は 3 つが同じ写しを共有する。
fn run_sequence(s: &Setup, mode: Mode) -> (Counts, Counts, Counts) {
    let read = s.read_for(mode);
    let stop = StopFlag::new();
    s.with_ctx(mode, &read, &stop, |ctx| {
        let done = |outcome: StageOutcome| match outcome {
            StageOutcome::Done(counts) => counts,
            other => panic!("{other:?}"),
        };
        let index = done(IndexLocal.run(ctx).unwrap());
        let derivatives = done(LocalDerivatives::new().run(ctx).unwrap());
        let organized = organize(ctx).unwrap();
        (index, derivatives, organized)
    })
}

#[test]
fn dry_runの後続段は索引後の状態で数え_実行と同じ件数になる() {
    let dry_side = sequence_state("local-seq-dry");
    let (rows, files) = (dry_side.rows(), tree(dry_side.root()));
    let dry = run_sequence(&dry_side, Mode::DryRun);
    // 何も変えない
    assert_eq!(dry_side.rows(), rows);
    assert_eq!(tree(dry_side.root()), files);

    let apply_side = sequence_state("local-seq-apply");
    let applied = run_sequence(&apply_side, Mode::Apply);
    assert_eq!(dry.0, applied.0, "index_local");
    assert_eq!(dry.1, applied.1, "local_derivatives");
    assert_eq!(dry.2, applied.2, "organize");
    // 索引で増える行が後続の対象になっている（索引前の状態なら 0 件になる数）
    assert_eq!(get(&dry.0, "added"), 2);
    assert_eq!((get(&dry.1, "original"), get(&dry.1, "made")), (1, 1));
    assert_eq!((get(&dry.2, "targets"), get(&dry.2, "moved")), (1, 1));
}

#[test]
fn derivativeのdry_runもパスを検査し_存在しないものはfailedに数える() {
    let s = setup("local-deriv-dry-missing");
    s.sql(
        "INSERT INTO photos(source, path, name, lat, lon) VALUES
         ('local','photos/missing.jpg','missing.jpg',1.0,2.0),
         ('local','../x.heic','x.heic',1.0,2.0)",
    );
    let dry = s.dry_run(&LocalDerivatives::new());
    assert_eq!(
        (
            get(&dry, "original"),
            get(&dry, "made"),
            get(&dry, "failed")
        ),
        (0, 0, 2),
        "{dry}"
    );
    assert_eq!(s.run(&LocalDerivatives::new()), dry);
}

#[test]
fn organizeのdry_runも実行と同じ分類で数える() {
    let s = setup("local-organize-dry-classes");
    s.put("photos/gone.jpg", &jpeg_plain(), T0);
    s.put("photos/link.jpg", &jpeg_plain(), T0);
    s.put("photos/move.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    fs::remove_file(s.root().join("photos/gone.jpg")).unwrap();
    fs::remove_file(s.root().join("photos/link.jpg")).unwrap();
    s.put("photos/real.bin", b"x", T0);
    symlink(
        s.root().join("photos/real.bin"),
        s.root().join("photos/link.jpg"),
    )
    .unwrap();
    let files = tree(s.root());
    let dry = s.organize(Mode::DryRun);
    assert_eq!(tree(s.root()), files);
    assert!(!s.root().join("_excluded").exists());
    assert_eq!(s.organize(Mode::Apply), dry);
    assert_eq!(
        (
            get(&dry, "moved"),
            get(&dry, "row_only_removed"),
            get(&dry, "skipped")
        ),
        (1, 1, 1)
    );
}

// ---------------------------------------------------------------- organize の途中失敗

/// 子ディレクトリのパーミッションを、drop で（パニックしても）戻す。
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn 元の削除に失敗したら退避先のリンクを取り消し_再実行で増えない() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("local-organize-ro");
    s.put("photos/ro/x.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    let ro = s.root().join("photos/ro");
    let _restore = Restore(ro.clone());
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::File::create(ro.join("probe")).is_ok() {
        return; // root など、パーミッションが効かない環境では確かめられない
    }
    let dest = s.root().join("_excluded/no_gps");
    for _ in 0..3 {
        let counts = s.organize(Mode::Apply);
        assert_eq!((get(&counts, "failed"), get(&counts, "moved")), (1, 0));
        assert_eq!(s.paths(), ["photos/ro/x.jpg"]);
        assert!(ro.join("x.jpg").exists());
        // リンクを残さない（残ると `(2)`・`(3)` と増える）
        assert!(names(&dest).is_empty(), "{:?}", names(&dest));
    }
    // 権限を直せば、同じ名前で移せる
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    let counts = s.organize(Mode::Apply);
    assert_eq!(get(&counts, "moved"), 1);
    assert_eq!(names(&dest), ["x.jpg"]);
    assert!(s.rows().is_empty());
}

#[test]
fn 取り消せずに残ったリンクは重ねずに再利用する() {
    let s = setup("local-organize-leftover");
    let src = s.put("photos/x.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    // 前回、リンクを作ったあとで元を消せず、リンクの取り消しにも失敗した状態
    let dest = s.root().join("_excluded/no_gps");
    fs::create_dir_all(&dest).unwrap();
    fs::hard_link(&src, dest.join("x.jpg")).unwrap();
    let counts = s.organize(Mode::Apply);
    assert_eq!((get(&counts, "moved"), get(&counts, "failed")), (1, 0));
    assert_eq!(names(&dest), ["x.jpg"]);
    assert!(!src.exists());
    assert!(s.rows().is_empty());
}

#[test]
fn 同名の別のファイルがあれば別名にし_残ったリンクは別名の側で再利用する() {
    let s = setup("local-organize-leftover-2");
    let src = s.put("photos/x.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    let dest = s.root().join("_excluded/no_gps");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("x.jpg"), b"previous").unwrap();
    fs::hard_link(&src, dest.join("x (2).jpg")).unwrap();
    s.organize(Mode::Apply);
    assert_eq!(names(&dest), ["x (2).jpg", "x.jpg"]);
    assert_eq!(fs::read(dest.join("x.jpg")).unwrap(), b"previous");
    assert!(!src.exists());
}

// ---------------------------------------------------------------- 大量の変更（計測）

/// 大量の追加・更新・削除の所要時間と、停止の合図への応答を測る（`cargo test --release
/// --lib -- --ignored --nocapture 大量`）。結果は docs/rust-migration/server.md §13.12 に記録した。
#[test]
#[ignore = "計測用（件数が多く、時間がかかる）"]
fn 大量の変更のトランザクションの所要時間と停止への応答を測る() {
    const N: usize = 10_000;
    let s = setup("local-bulk");
    let entry = |i: usize, mtime: f64| Entry {
        rel: format!("photos/{i:06}.jpg"),
        name: format!("{i:06}.jpg"),
        mtime,
        taken_at: "2024-05-06 07:08".into(),
        lat: Some(12.5),
        lon: Some(78.1),
        loc_source: Some("exif"),
    };
    let now = || "2024-01-01T00:00:00".to_string();
    let timed = |label: &str, ops: Vec<Op>, ids: Vec<i64>| {
        let start = Instant::now();
        assert!(apply_plan(&s.writer, now(), &ops, ids, || false).unwrap());
        eprintln!("{label}: {:?}", start.elapsed());
    };
    timed(
        "10000 件の追加",
        (0..N).map(|i| Op::Insert(entry(i, 1.0))).collect(),
        vec![],
    );
    timed(
        "10000 件の更新",
        (0..N).map(|i| Op::Update(entry(i, 9.0))).collect(),
        vec![],
    );
    // 停止への応答: 更新の途中で合図を出し、合図から戻るまで
    let flag = Arc::new(AtomicBool::new(false));
    let raised: Arc<std::sync::Mutex<Option<Instant>>> = Arc::default();
    let setter = {
        let (flag, raised) = (Arc::clone(&flag), Arc::clone(&raised));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(3));
            *raised.lock().unwrap() = Some(Instant::now());
            flag.store(true, Ordering::SeqCst);
        })
    };
    let ops: Vec<Op> = (0..N).map(|i| Op::Update(entry(i, 20.0))).collect();
    let checker = Arc::clone(&flag);
    let committed = apply_plan(&s.writer, now(), &ops, vec![], move || {
        checker.load(Ordering::SeqCst)
    })
    .unwrap();
    let returned = Instant::now();
    setter.join().unwrap();
    let raised = raised.lock().unwrap().unwrap();
    eprintln!(
        "停止の合図 → 戻るまで: {:?}（確定した: {committed}）",
        returned.saturating_duration_since(raised)
    );
    assert!(!committed);
    assert_eq!(s.rows().len(), N);
    assert!(s.rows().iter().all(|r| r.mtime == Some(9.0)));
    let ids: Vec<i64> = s.rows().iter().map(|r| r.id).collect();
    timed("10000 件の削除", vec![], ids);
    assert!(s.rows().is_empty());
}
