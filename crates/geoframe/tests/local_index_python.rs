//! `index_local`（ローカル写真の索引）を、Python の `drive.index_local` の結果と列ごとに突き合わせる。
//!
//! 基準は `tools/gen_local_index_fixture.py` が作る `tests/fixtures/local-index-python.json`:
//! 合成の写真の木と操作（追加・更新・削除・手動の位置の書き込み）の列を、Python の `index_local` に
//! 通した各段階の `photos` の全行（`id` を含む全列）。Rust は同じ木と操作を一時の `DATA_DIR` に作り、
//! 同じ段階ごとに `IndexLocal` を通して、全列が一致することを求める。
//!
//! 一致を求めないのは `indexed_at`（実行時刻）の値だけで、代わりに**その段階で書き換えられたか**
//! （`touched`）と形式（`YYYY-MM-DDTHH:MM:SS`）を見る。意図して違える点は Python が索引しない
//! 壊れた GPS の写真で、別のテストで確かめる（`docs/rust-migration/server.md` §0・§13）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine;
use geoframe::cache::Cache;
use geoframe::config::{Config, Overrides};
use geoframe::local::IndexLocal;
use geoframe::stop::StopFlag;
use geoframe::sync::{Mode, Stage, StageCtx, StageOutcome};
use geoframe::writer::{OpenMode, Writer};
use serde_json::{Map, Value};

const SENTINEL: &str = "2000-01-01T00:00:00";

/// Python が索引しない（例外で飛ばす）壊れた GPS の写真。Rust は位置なしで索引する。
const RUST_ONLY: &str = "photos/n_two_components.jpg";

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("geoframe-it-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn doc() -> Value {
    let text = fs::read_to_string(format!(
        "{}/tests/fixtures/local-index-python.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    serde_json::from_str(&text).unwrap()
}

struct Env {
    root: PathBuf,
    config: Config,
    writer: Writer,
    cache: Cache,
}

fn env(root: &Path) -> Env {
    let overrides = Overrides {
        listen: None,
        data_dir: Some(root.to_path_buf()),
    };
    let config = Config::load(|_| None, &overrides).unwrap();
    let writer = Writer::open(&config.db_path(), OpenMode::CreateIfMissing).unwrap();
    writer.call(geoframe::schema::migrate).unwrap();
    Env {
        root: root.to_path_buf(),
        config,
        writer,
        cache: Cache::load(root),
    }
}

impl Env {
    fn index(&self) {
        let read = geoframe::db::open_read_only(&self.config.db_path()).unwrap();
        let http = geoframe::net::agent();
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
        match IndexLocal.run(&ctx).unwrap() {
            StageOutcome::Done(_) => {}
            other => panic!("{other:?}"),
        }
    }

    fn exec(&self, sql: String) {
        self.writer.call(move |c| c.execute_batch(&sql)).unwrap();
    }

    /// `photos` の全行を、列名 → JSON の値で。
    fn rows(&self) -> Vec<Map<String, Value>> {
        let read = geoframe::db::open_read_only(&self.config.db_path()).unwrap();
        let mut stmt = read.prepare("SELECT * FROM photos ORDER BY id").unwrap();
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
                    rusqlite::types::ValueRef::Blob(_) => panic!("blob"),
                };
                map.insert(name.clone(), value);
            }
            out.push(map);
        }
        out
    }
}

fn set_mtime(path: &Path, mtime_ns: i64) {
    let ns = u64::try_from(mtime_ns).unwrap();
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::new(ns / 1_000_000_000, (ns % 1_000_000_000) as u32))
        .unwrap();
}

fn apply_op(env: &Env, op: &Value) {
    let path = op["path"].as_str().map(|p| env.root.join(p));
    match op["op"].as_str().unwrap() {
        "write" => {
            let path = path.unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(op["b64"].as_str().unwrap())
                .unwrap();
            fs::write(&path, bytes).unwrap();
            set_mtime(&path, op["mtime_ns"].as_i64().unwrap());
        }
        "utime" => set_mtime(&path.unwrap(), op["mtime_ns"].as_i64().unwrap()),
        "delete" => fs::remove_file(path.unwrap()).unwrap(),
        "mkfile" => {
            let path = path.unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"derivative").unwrap();
        }
        "sql" => env.exec(op["sql"].as_str().unwrap().to_string()),
        other => panic!("{other}"),
    }
}

/// `indexed_at` を捨てて `touched` にする（Python 側の `normalize` と同じ）。
fn normalize(rows: Vec<Map<String, Value>>) -> Vec<Map<String, Value>> {
    rows.into_iter()
        .map(|mut row| {
            let at = row.remove("indexed_at").unwrap();
            if let Some(at) = at.as_str().filter(|a| *a != SENTINEL) {
                // 形式: オフセット無しの `YYYY-MM-DDTHH:MM:SS`
                let b = at.as_bytes();
                assert_eq!(b.len(), 19, "{at}");
                assert!(b[10] == b'T' && b[4] == b'-' && b[13] == b':', "{at}");
            }
            row.insert("touched".into(), Value::from(at != SENTINEL));
            row
        })
        .collect()
}

#[test]
fn index_localの全列はpythonと一致する() {
    let tmp = TempRoot::new("local-index");
    let env = env(&tmp.0);
    let doc = doc();
    let steps = doc["steps"].as_array().unwrap();
    assert!(steps.len() >= 4);
    let mut compared_rows = 0;
    for step in steps {
        let name = step["name"].as_str().unwrap();
        env.exec(format!("UPDATE photos SET indexed_at='{SENTINEL}'"));
        for op in step["ops"].as_array().unwrap() {
            // Python が索引しない写真は、この突き合わせでは置かない（別のテストで確かめる）
            if op["path"] == RUST_ONLY {
                continue;
            }
            apply_op(&env, op);
        }
        env.exec(format!(
            "UPDATE photos SET indexed_at='{SENTINEL}' WHERE indexed_at IS NULL"
        ));
        env.index();

        let python = &step["python"];
        let want: Vec<&Map<String, Value>> = python["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_object().unwrap())
            .collect();
        let got = normalize(env.rows());
        assert_eq!(got.len(), want.len(), "{name}: 行数");
        for (got, want) in got.iter().zip(&want) {
            let id = want["id"].clone();
            // 列の集合も一致する（Python の `SELECT *` と Rust の `SELECT *`）
            let got_keys: Vec<_> = got.keys().collect();
            let want_keys: Vec<_> = want.keys().collect();
            assert_eq!(got_keys, want_keys, "{name} id={id}: 列");
            for key in want.keys() {
                assert_eq!(got[key], want[key], "{name} id={id} 列 {key}");
            }
            compared_rows += 1;
        }
        // 消えるはずのファイル・残るはずのファイル
        let exists: HashMap<_, _> = python["exists"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_bool().unwrap()))
            .collect();
        for (path, python_exists) in exists {
            if path == RUST_ONLY {
                continue;
            }
            assert_eq!(
                env.root.join(&path).exists(),
                python_exists,
                "{name}: {path} の有無"
            );
        }
    }
    assert!(compared_rows > 40, "比べた行が少なすぎる: {compared_rows}");
}

#[test]
fn 壊れたgpsの写真はpythonと違い位置なしで索引する() {
    let tmp = TempRoot::new("local-index-broken-gps");
    let env = env(&tmp.0);
    let doc = doc();
    // 基準のフィクスチャの中の、壊れた GPS の写真の書き込みを使う
    let op = doc["steps"][0]["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["path"] == RUST_ONLY)
        .expect("フィクスチャに壊れた GPS の写真が無い");
    // Python はこの写真を索引していない
    let python_paths: Vec<_> = doc["steps"][0]["python"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_string())
        .collect();
    assert!(!python_paths.contains(&RUST_ONLY.to_string()));

    apply_op(&env, op);
    env.index();
    let rows = env.rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["path"], RUST_ONLY);
    assert_eq!(row["taken_at"], "2024-05-06 07:08");
    assert_eq!(row["lat"], Value::Null);
    assert_eq!(row["lon"], Value::Null);
    assert_eq!(row["loc_source"], Value::Null);
}
