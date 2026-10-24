//! `index_local`・`local_derivatives`・`organize` の単体テスト。写真は合成のバイト列だけ
//! （EXIF は `tests/fixtures/exif-python.json` の合成 JPEG）。Python の `index_local` との列ごとの
//! 突き合わせは `tests/local_index_python.rs`。

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine;

use super::*;
use crate::cache::Cache;
use crate::config::{Config, Overrides};
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

    fn run_with(&self, stage: &dyn Stage, mode: Mode, stop: &StopFlag) -> StageResult {
        let read = crate::db::open_read_only(&self.config.db_path()).unwrap();
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
        let read = crate::db::open_read_only(&self.config.db_path()).unwrap();
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
fn 更新は現行と同じ列だけを上書きし手動の位置も戻す() {
    let s = setup("local-update");
    let path = s.put("photos/a.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    // 手動で位置を直し、地名・表示用・お気に入りなどを持たせる
    s.sql(
        "UPDATE photos SET lat=35.0, lon=139.0, loc_source='manual', place='どこか', elev=3.0,
         display='derivatives/x.jpg', fetched_at='2024-01-01T00:00:00', favorite=1, hidden=1,
         rotation=90, title='題', inferred_from=7, infer_dt_s=8, pref='県', city='市', muni_cd='01'
         WHERE path='photos/a.jpg'",
    );
    set_mtime(&path, T0 + 10);
    fs::write(&path, jpeg_gps()).unwrap();
    set_mtime(&path, T0 + 10);
    let counts = s.run(&IndexLocal);
    assert_eq!(get(&counts, "updated"), 1);

    let row = s.row("photos/a.jpg");
    // 上書きされる（inventory.md §2.3: 手動の位置も戻る）
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
    let counts = s.run(&LocalDerivatives);
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
    let again = s.run(&LocalDerivatives);
    assert_eq!((get(&again, "original"), get(&again, "made")), (0, 0));
}

#[test]
fn 位置の無い写真と他のsourceの行は対象にしない() {
    let s = setup("local-deriv-filter");
    s.put("photos/nogps.heic", &heic(), T0);
    s.run(&IndexLocal);
    s.sql(
        "INSERT INTO photos(source, drive_id, name, lat, lon) VALUES('drive','d1','x.jpg',1.0,2.0)",
    );
    let counts = s.run(&LocalDerivatives);
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
    assert_eq!(get(&s.run(&LocalDerivatives), "original"), 2);
    assert!(s.row("photos/1.jpg").display.is_some());
    assert!(s.row("photos/2.jpg").display.is_some());
    assert!(s.row("photos/3.jpg").display.is_none());
    assert_eq!(get(&s.run(&LocalDerivatives), "original"), 1);
    assert!(s.row("photos/3.jpg").display.is_some());
}

#[test]
fn 変換に失敗した写真は数えて続ける() {
    let s = setup("local-deriv-fail");
    // 画像として読めるのは索引の時点だけ。後から壊れた HEIC
    index_with_location(&s, &[("photos/a.heic", heic()), ("photos/b.heic", heic())]);
    fs::write(s.root().join("photos/a.heic"), b"broken").unwrap();
    let counts = s.run(&LocalDerivatives);
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
    let counts = s.run(&LocalDerivatives);
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
    let counts = s.dry_run(&LocalDerivatives);
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
fn 位置が付きうる写真はt2_5まで移さない() {
    let s = setup("local-organize-held");
    // 撮影時刻つきで位置の無い写真と、錨になる写真がある → 推定で位置が付きうるので移さない
    let mut with_time = jpeg_plain();
    with_time.clear();
    with_time.extend(fixture_jpeg("time_digitized_only")); // 時刻だけあって GPS が無い
    s.put("photos/timed.jpg", &with_time, T0);
    s.put("photos/anchor.jpg", &jpeg_gps(), T0);
    s.put("photos/untimed.jpg", &jpeg_plain(), T0);
    s.run(&IndexLocal);
    assert_eq!(
        s.row("photos/timed.jpg").taken_at.as_deref(),
        Some("2023-01-02 03:04")
    );
    let dry = s.organize(Mode::DryRun);
    assert_eq!(
        (get(&dry, "targets"), get(&dry, "held_back_until_infer")),
        (1, 1)
    );
    let counts = s.organize(Mode::Apply);
    assert_eq!(get(&counts, "moved"), 1);
    assert_eq!(get(&counts, "held_back_until_infer"), 1);
    assert_eq!(s.paths(), ["photos/anchor.jpg", "photos/timed.jpg"]);
    // 錨が無ければ時刻つきでも移す（位置は付かない）
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
