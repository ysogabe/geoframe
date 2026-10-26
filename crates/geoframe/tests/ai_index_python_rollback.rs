//! Python への切り戻しの確認（P4 / T4.3）: Rust が追加した表（空間・ベクトル・ジョブ）と有効な検索空間のポインタが
//! ある合成 DB を、**現行の Python の読み取り・AI の周期の一部**（`api_photos`・`api_ai_status`・
//! `_remove_noncanonical_embeddings`・`_load_photos`・`ai_search._valid_rows`）に渡しても、手動の位置・お気に入り・
//! 非表示・回転、旧の有償結果が変わらず、Rust の追加表・ポインタにも Python は触れないことを確かめる。
//! 世代の合う旧選別では、Python のフィードと Rust のフィードの顔ぶれが一致する（安全化は世代が古い・代表が無い
//! ときだけ差が出る）。
//!
//! 外部通信はしない（Embedding・Gemini の呼び出しは含めない）。合成のデータだけで、Python は `-I`（隔離）で
//! 起動し、データ置き場は一時ディレクトリ。Python（`GEOFRAME_TEST_PYTHON`、既定 `python3`）か Pillow が無い環境では
//! 何もしない。

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::TempDir;
use geoframe::ai;
use geoframe::ai::index;
use geoframe::ai::settings::Normalization;
use geoframe::ai::space::SpaceIdentity;
use rusqlite::Connection;
use serde_json::Value;

fn app_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../app")
}

fn python() -> String {
    std::env::var("GEOFRAME_TEST_PYTHON").unwrap_or_else(|_| "python3".to_string())
}

/// Python で `script` を実行し、標準出力の JSON を返す。使えない環境なら `None`。
fn run_python(data_dir: &Path, script: &str) -> Option<Value> {
    let full = format!(
        "import sys, json\nsys.path.insert(0, {app:?})\n{script}",
        app = app_dir().to_str().unwrap()
    );
    let output = Command::new(python())
        .args(["-I", "-c", &full])
        .env_clear()
        .env("GEOFRAME_DATA_DIR", data_dir)
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("python を起動できない: {e}"),
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        if err.contains("ModuleNotFoundError") || err.contains("ImportError") {
            return None;
        }
        panic!("python が失敗した: {err}");
    }
    let last = String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .unwrap_or("")
        .to_string();
    Some(serde_json::from_str(&last).unwrap_or_else(|e| panic!("{e}: {last}")))
}

fn blob(value: f32) -> Vec<u8> {
    vec![value; 768]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

/// 合成の DB: 手動の位置（1）・お気に入り（2）・非表示（3）・回転（4）、旧の同一場面グループ（5 推薦・6・7 推薦外。
/// 全員が世代の合う旧特徴量）、Rust の追加表・有効な検索空間のポインタ。
fn build(dir: &Path) -> PathBuf {
    let path = dir.join("catalog.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    geoframe::schema::migrate(&mut conn).unwrap();
    ai::migrate(&mut conn).unwrap();
    for id in 1..=7_i64 {
        conn.execute(
            "INSERT INTO photos(id,source,name,display,md5,mtime,taken_at,lat,lon,loc_source,favorite,hidden,rotation)
             VALUES(?1,'drive','n',?2,?3,?4,?5,35.0,139.0,?6,?7,?8,?9)",
            rusqlite::params![
                id,
                format!("derivatives/p{id}.jpg"),
                format!("m{id}"),
                id as f64,
                format!("2025-06-01 10:{:02}", id),
                if id == 1 { "manual" } else { "exif" },
                i64::from(id == 2),
                i64::from(id == 3),
                if id == 4 { 90 } else { 0 },
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO photo_quality_features(photo_id,display,source_md5,source_mtime,content_sha256,
                phash,sharpness,luminance,exposure_score,black_clip,white_clip,contrast,error,analyzed_at)
             VALUES(?1,?2,?3,?4,?5,?6,10.0,100.0,0.9,0.0,0.0,50.0,'','t')",
            rusqlite::params![
                id,
                format!("derivatives/p{id}.jpg"),
                format!("m{id}"),
                id as f64,
                format!("sha-{id}"),
                format!("{:016x}", id),
            ],
        )
        .unwrap();
    }
    // 旧の有償結果（5・6・7）。keep_score などの判定も持つ
    for id in 5..=7_i64 {
        conn.execute(
            "INSERT INTO photo_ai_features(photo_id,display,source_md5,source_mtime,content_sha256,
               embedding_model,embedding_dim,embedding,keep_score,recommended,judge_model,analyzed_at,error)
             VALUES(?1,?2,?3,?4,?5,'gemini-embedding-2',768,?6,?7,?8,'judge','t','')",
            rusqlite::params![
                id,
                format!("derivatives/p{id}.jpg"),
                format!("m{id}"),
                id as f64,
                format!("sha-{id}"),
                blob(0.01 * id as f32),
                70 + id,
                i64::from(id == 5),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO photo_embeddings VALUES(?1,'gemini-embedding-2',768,?2,'t')",
            rusqlite::params![format!("sha-{id}"), blob(0.01 * id as f32)],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO photo_ai_groups(group_id,kind,algorithm,analyzed_at,status,same_scene)
           VALUES('g','scene','t','t','judged',1);
         INSERT INTO photo_ai_group_members(group_id,photo_id,rank_in_group,recommended,local_score)
           VALUES('g',5,1,1,0.9),('g',6,2,0,0.5),('g',7,3,0,0.4);",
    )
    .unwrap();
    // Rust の追加物: 空間・ベクトル・有効な検索空間のポインタ・ジョブ
    let space = SpaceIdentity::new(
        "space-1",
        "shared-1",
        "model-a",
        "rev-1",
        "img-v1",
        "txt-v1",
        3,
        Normalization::L2,
    )
    .unwrap();
    index::register_space(&mut conn, &space, 0).unwrap();
    for id in 1..=7 {
        index::store_vector(
            &mut conn,
            &space,
            &format!("sha-{id}"),
            "sent",
            &[1.0, id as f32, 0.0],
            0,
        )
        .unwrap();
    }
    let snapshot = ai::activation::snapshot_targets(&conn, &space).unwrap();
    assert!(
        ai::activation::try_activate(&mut conn, &snapshot, &space, 0.95)
            .unwrap()
            .published()
    );
    ai::jobs::enqueue(&mut conn, &index::image_embedding_key(&space, "sha-99"), 1).unwrap();
    path
}

fn dump(conn: &Connection, tables: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
            .unwrap();
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
}

const RUST_TABLES: [&str; 4] = [
    "ai_embedding_spaces",
    "ai_embedding_vectors",
    "ai_jobs",
    "ai_request_usage",
];
const LEGACY_TABLES: [&str; 5] = [
    "photo_ai_features",
    "photo_embeddings",
    "photo_ai_groups",
    "photo_ai_group_members",
    "photo_quality_features",
];

const SCRIPT: &str = r#"
from geoframe import ai_pipeline, ai_search, photos
removed = ai_pipeline._remove_noncanonical_embeddings()
loaded = ai_pipeline._load_photos()
feed = photos.api_photos()
ids = [p["id"] for p in feed]
status = ai_search.api_ai_status()
rows = ai_search._valid_rows(True)
print(json.dumps({"removed": removed, "loaded": len(loaded), "feed": ids,
                  "status": {k: status[k] for k in ("photos", "eligible", "groups", "other_model_embeddings")},
                  "valid_rows": sorted(r["id"] for r in rows)}))
"#;

#[test]
fn python_reads_and_cycles_a_database_with_the_rust_tables_and_the_active_pointer() {
    let dir = TempDir::new("py-rollback");
    let path = build(dir.path());
    let rust_feed: Vec<i64> = {
        let conn = Connection::open(&path).unwrap();
        geoframe::photos::list_photos(&conn)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect()
    };
    let (rust_before, legacy_before, photos_before, pointer_before) = {
        let conn = Connection::open(&path).unwrap();
        (
            dump(&conn, &RUST_TABLES),
            dump(&conn, &LEGACY_TABLES),
            dump(&conn, &["photos"]),
            dump(&conn, &["settings"]),
        )
    };
    assert!(pointer_before.iter().any(|r| r.contains("ai_active_space")));

    let Some(out) = run_python(dir.path(), SCRIPT) else {
        eprintln!("Python（または Pillow）が使えないので切り戻しの試験を省く");
        return;
    };

    // Python の読み取りは、Rust の追加表があっても動く。世代の合う旧選別なので、フィードの顔ぶれは Rust と同じ
    let python_feed: Vec<i64> = out["feed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    let mut a = python_feed.clone();
    let mut b = rust_feed.clone();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
    assert_eq!(a, [1, 2, 4, 5]); // 3 は非表示、6・7 は旧選別で抑止
    assert_eq!(out["removed"], 0); // 有効な旧結果は掃除されない
    assert_eq!(out["status"]["eligible"], 7);
    assert_eq!(out["valid_rows"], serde_json::json!([5, 6, 7]));

    // Python が触れたあとも: 旧の有償結果・手動の位置・お気に入り・非表示・回転は不変、Rust の追加表・ポインタも不変
    let conn = Connection::open(&path).unwrap();
    assert_eq!(dump(&conn, &LEGACY_TABLES), legacy_before);
    assert_eq!(dump(&conn, &["photos"]), photos_before);
    assert_eq!(dump(&conn, &RUST_TABLES), rust_before);
    assert_eq!(
        dump(&conn, &["settings"])[..pointer_before.len()],
        pointer_before[..]
    );
    let (lat, loc, fav, hidden, rotation): (f64, String, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT lat FROM photos WHERE id=1),(SELECT loc_source FROM photos WHERE id=1),
                    (SELECT favorite FROM photos WHERE id=2),(SELECT hidden FROM photos WHERE id=3),
                    (SELECT rotation FROM photos WHERE id=4)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        (lat, loc.as_str(), fav, hidden, rotation),
        (35.0, "manual", 1, 1, 90)
    );
    let keep: i64 = conn
        .query_row(
            "SELECT keep_score FROM photo_ai_features WHERE photo_id=6",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(keep, 76);

    // Python の周期のあとの DB を Rust がまた読める（Python→Rust の再切替）。新しい空間のポインタも有効なまま
    let rust_again: Vec<i64> = geoframe::photos::list_photos(&conn)
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(rust_again.len(), rust_feed.len());
    let loaded = ai::search::load_active_index(&conn).unwrap();
    assert_eq!(loaded.index.unwrap().len(), 7);
}
