//! `catalog.db` のスキーマの作成と移行（現行 Python の `store.py:db()` と同じ結果にする）。
//!
//! # いつ動くか
//!
//! 取り込みが有効（`GEOFRAME_INGEST=1`）のときだけ、writer（[`crate::writer`]）の上で起動時に 1 回
//! [`migrate`] を実行する。無効のとき（既定）や `--dry-run` では**一切書かない**（読み取りの
//! [`pending`] で「やるとしたら何か」を数えるだけ）。
//!
//! # Python の移行との対応（docs/rust-migration/inventory.md §2.4）
//!
//! | # | 内容 | Rust |
//! |---|---|---|
//! | M0 | `journal_mode=WAL`、`SCHEMA` の `CREATE … IF NOT EXISTS` | 同じ（[`SCHEMA`] は Python の `SCHEMA` に M1・M2 の列を足した定義） |
//! | M1 | `photos` に `muni_cd pref city`。**追加したときだけ** `UPDATE photos SET place=NULL` | 同じ。**既に列がある DB では決して起きない** |
//! | M2 | `photos` に `favorite hidden rotation` | 同じ（M1 とは別。`place=NULL` を誘発しない） |
//! | M3 | `photos.infer_dt_s`（`SCHEMA` にも入っている） | 同じ |
//! | M4〜M6 | `photo_ai_features`・`photo_ai_groups`・`photo_ai_group_members` の列 | 同じ（`SCHEMA` にも入っている） |
//! | M7 | `INSERT OR IGNORE INTO photo_embeddings … FROM photo_ai_features WHERE error=''`、**毎起動** | 同じ（毎起動。行が無ければ書き込みは起きない） |
//!
//! Python は `ALTER` の失敗（`OperationalError`）をすべて握りつぶすが、Rust は**列があるかを
//! `PRAGMA table_info` で先に確かめ**、無い列だけを足す。ロックなどの本当の失敗は握りつぶさず返す。
//! 全体を 1 つの `BEGIN IMMEDIATE` に入れるので、途中で落ちても中途半端な移行にならない。
//!
//! # 空の DB から作るとき
//!
//! `photos` の定義には、Python では `ALTER` で後から足される `muni_cd pref city favorite hidden rotation`
//! を**最初から含めてある**。しかも `sqlite_master.sql` が、Python が `ALTER` した結果と**1 文字も違わない**
//! ように書いてある（`ALTER TABLE ADD COLUMN` は元の文の末尾の `)` の前に `\n, 列定義` を挿す。
//! 定義の終わりの形はその結果の複製）。テスト `fresh_database_matches_the_python_one` が、Python が
//! 空の置き場から作った DB（`tools/gen_schema_fixture.py` の出力）と、`sqlite_master` の全行・各表の
//! `table_xinfo`・索引を比べる。Python の `SCHEMA` を変えたらそのテストが落ちる。
//!
//! 本番の DB は古い版から `ALTER` を重ねてきたので、列の**並び**は空の DB と違いうる（例: `infer_dt_s` が
//! 末尾）。移行はその並びを変えない（列の有無だけを見る）。
//!
//! # ログ
//!
//! 出すのは表名・列名・件数だけ。

use rusqlite::{Connection, TransactionBehavior};

/// 空の DB から作る定義。`store.py` の `SCHEMA` と同じ文面で、`photos` だけ M1・M2 の列を含む
/// （上の「空の DB から作るとき」）。Python 側のコメントも含めて同一にしてある。
pub const SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS photos(
  id INTEGER PRIMARY KEY,
  source TEXT NOT NULL,             -- 'drive' | 'local'
  drive_id TEXT UNIQUE,             -- DriveファイルID
  path TEXT UNIQUE,                 -- ローカル原本の相対パス（drive行はNULL）
  display TEXT,                     -- 表示用ファイル（derivative等）の相対パス
  name TEXT NOT NULL,
  md5 TEXT, mtime REAL,
  taken_at TEXT,                    -- 'YYYY-MM-DD HH:MM'
  lat REAL, lon REAL,
  loc_source TEXT,                  -- 'exif' | 'drive_meta' | 'inferred' | 'manual' | NULL
  inferred_from INTEGER,
  infer_dt_s INTEGER,               -- 推定に使った錨との時間差（秒）。loc_source='inferred'時のみ
  place TEXT, elev REAL,
  title TEXT, ai_caption TEXT,      -- Phase3(Gemini)用
  indexed_at TEXT, fetched_at TEXT
, muni_cd TEXT, pref TEXT, city TEXT, favorite INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0, rotation INTEGER DEFAULT 0);
CREATE INDEX IF NOT EXISTS idx_photos_time ON photos(taken_at);
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT);
-- 写真品質解析はDrive/ローカル索引から再生成できる派生データなので、photos本体と分離する。
-- source_md5/source_mtime/displayまで一致するときだけAPIが採用し、原本やderivativeの更新後に
-- 古い解析結果が誤適用されないようにする。
CREATE TABLE IF NOT EXISTS photo_quality(
  photo_id INTEGER PRIMARY KEY,
  display TEXT NOT NULL,
  source_md5 TEXT,
  source_mtime REAL,
  content_sha256 TEXT NOT NULL,
  group_id INTEGER NOT NULL,
  group_kind TEXT NOT NULL CHECK(group_kind IN ('exact','similar')),
  rank_in_group INTEGER NOT NULL,
  recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
  score REAL NOT NULL,
  reasons_json TEXT NOT NULL DEFAULT '[]',
  issues_json TEXT NOT NULL DEFAULT '[]',
  algorithm TEXT NOT NULL,
  analyzed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_photo_quality_group ON photo_quality(group_kind, group_id);
-- 重い画像読み込みを新規・更新写真だけに限定するための特徴量キャッシュ。
-- photo_quality は「全写真の特徴量が最新」の時点だけ、この表から原子的に再生成する。
CREATE TABLE IF NOT EXISTS photo_quality_features(
  photo_id INTEGER PRIMARY KEY,
  display TEXT NOT NULL,
  source_md5 TEXT,
  source_mtime REAL,
  content_sha256 TEXT,
  phash TEXT,
  sharpness REAL,
  luminance REAL,
  exposure_score REAL,
  black_clip REAL,
  white_clip REAL,
  contrast REAL,
  error TEXT NOT NULL DEFAULT '',
  analyzed_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_photo_quality_features_sha
  ON photo_quality_features(content_sha256);
-- Gemini Embeddingと比較評価の永続化。embeddingはfloat32 little-endianのBLOB。
-- 品質解析と同じく、写真のdisplay/md5/mtimeが一致する行だけ検索APIが採用する。
CREATE TABLE IF NOT EXISTS photo_ai_features(
  photo_id INTEGER PRIMARY KEY,
  display TEXT NOT NULL,
  source_md5 TEXT,
  source_mtime REAL,
  content_sha256 TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dim INTEGER NOT NULL,
  embedding BLOB NOT NULL,
  subject TEXT NOT NULL DEFAULT '',
  pet_state TEXT NOT NULL DEFAULT '',
  eyes TEXT NOT NULL DEFAULT '',
  face_crop TEXT NOT NULL DEFAULT '',
  horizon_status TEXT NOT NULL DEFAULT '',
  horizon_score INTEGER,
  subject_sharpness INTEGER,
  aesthetic_score INTEGER,
  keep_score INTEGER,
  recommended INTEGER CHECK(recommended IS NULL OR recommended IN (0,1)),
  reasons_json TEXT NOT NULL DEFAULT '[]',
  concerns_json TEXT NOT NULL DEFAULT '[]',
  judge_model TEXT,
  analyzed_at TEXT NOT NULL,
  error TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_photo_ai_features_sha
  ON photo_ai_features(content_sha256);
-- embeddingのモデル別キャッシュ（画像の中身のSHA-256単位）。photo_ai_featuresは今のモデルの
-- 1行だけを持つので、モデルを切り替えて戻したときに計算・課金をやり直さないためにここへ残す。
CREATE TABLE IF NOT EXISTS photo_embeddings(
  content_sha256 TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dim INTEGER NOT NULL,
  embedding BLOB NOT NULL,
  analyzed_at TEXT NOT NULL,
  PRIMARY KEY(content_sha256, embedding_model)
);
CREATE TABLE IF NOT EXISTS photo_ai_groups(
  group_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('scene','pet')),
  summary TEXT NOT NULL DEFAULT '',
  min_similarity REAL,
  max_similarity REAL,
  avg_similarity REAL,
  algorithm TEXT NOT NULL,
  analyzed_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'judged',
  same_scene INTEGER NOT NULL DEFAULT 1,
  judge_version TEXT NOT NULL DEFAULT '',
  judge_model TEXT NOT NULL DEFAULT '',
  shortlist_json TEXT NOT NULL DEFAULT '[]',
  error TEXT NOT NULL DEFAULT '',
  attempts INTEGER NOT NULL DEFAULT 0,
  judge_order TEXT NOT NULL DEFAULT '',
  presented_json TEXT NOT NULL DEFAULT '[]'
);
CREATE TABLE IF NOT EXISTS photo_ai_group_members(
  group_id TEXT NOT NULL,
  photo_id INTEGER NOT NULL,
  rank_in_group INTEGER NOT NULL,
  recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
  shortlisted INTEGER NOT NULL DEFAULT 1,
  local_score REAL,
  PRIMARY KEY(group_id, photo_id)
);
CREATE INDEX IF NOT EXISTS idx_photo_ai_group_members_photo
  ON photo_ai_group_members(photo_id);
-- 位置なし写真の撮影地候補（loc_suggest.py）。photos.lat/lon には書かず、管理画面で人が
-- 確認してSAVEしたときだけ manual として photos に入る。どれも再生成できる派生データ。
-- Decisionによる「撮影地の手がかり」判定。display/md5/mtimeが一致する行だけ使う。
-- 判定に失敗した写真も error を入れて記録し（手がかり無し扱い）、その日の推定を止めない。
CREATE TABLE IF NOT EXISTS photo_loc_clues(
  photo_id INTEGER PRIMARY KEY,
  display TEXT NOT NULL,
  source_md5 TEXT,
  source_mtime REAL,
  clue REAL,
  kind TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL,
  analyzed_at TEXT NOT NULL,
  error TEXT NOT NULL DEFAULT ''
);
-- Geminiへの問い合わせ1回分（同じ日に撮った写真のまとまり単位）。run_id は
-- プロンプト版・モデル・送った写真の組から決まり、同じ組なら再問い合わせしない。
CREATE TABLE IF NOT EXISTS photo_loc_runs(
  run_id TEXT PRIMARY KEY,
  day TEXT NOT NULL,
  model TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  member_ids_json TEXT NOT NULL,
  sent_ids_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending','done','error')),
  attempts INTEGER NOT NULL DEFAULT 0,
  answer_json TEXT NOT NULL DEFAULT '',
  error TEXT NOT NULL DEFAULT '',
  analyzed_at TEXT NOT NULL
);
-- 写真ごとの候補。送った写真は写真ごとの推定（scope='photo'）、同じ日の残りの写真は
-- その日の推定（scope='day'）。lat/lon は地名をNominatimで引いた代表点で、撮影地点ではない。
CREATE TABLE IF NOT EXISTS photo_loc_suggestions(
  photo_id INTEGER PRIMARY KEY,
  run_id TEXT NOT NULL,
  scope TEXT NOT NULL CHECK(scope IN ('photo','day')),
  place_name TEXT NOT NULL,
  day_place_name TEXT NOT NULL DEFAULT '',
  geocode_query TEXT NOT NULL DEFAULT '',
  precision TEXT NOT NULL DEFAULT 'unknown',
  confidence TEXT NOT NULL DEFAULT 'unknown',
  evidence_json TEXT NOT NULL DEFAULT '[]',
  lat REAL, lon REAL,
  geocode_title TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL,
  analyzed_at TEXT NOT NULL
);
"#;

/// `SCHEMA` が作る表（Python の作成順）。
pub const TABLES: &[&str] = &[
    "photos",
    "settings",
    "photo_quality",
    "photo_quality_features",
    "photo_ai_features",
    "photo_embeddings",
    "photo_ai_groups",
    "photo_ai_group_members",
    "photo_loc_clues",
    "photo_loc_runs",
    "photo_loc_suggestions",
];

/// `SCHEMA` が作る索引（`CREATE INDEX`。主キー・UNIQUE の自動索引は表に付いてくる）。
pub const INDEXES: &[&str] = &[
    "idx_photos_time",
    "idx_photo_quality_group",
    "idx_photo_quality_features_sha",
    "idx_photo_ai_features_sha",
    "idx_photo_ai_group_members_photo",
];

/// 1 つの表に足す列（`ALTER TABLE … ADD COLUMN <名前> <定義>`）。
struct AddColumns {
    table: &'static str,
    columns: &'static [(&'static str, &'static str)],
    /// この列を足したら `UPDATE photos SET place=NULL`（再 enrich）する。M1 だけ。
    resets_place: bool,
}

/// M1〜M6 の列。Python の `db()` のループと同じ表・同じ順。
const ADD_COLUMNS: &[AddColumns] = &[
    // M1: 自治体情報列。足したら地名を再解決させる
    AddColumns {
        table: "photos",
        columns: &[("muni_cd", "TEXT"), ("pref", "TEXT"), ("city", "TEXT")],
        resets_place: true,
    },
    // M2: 再生スケジューラ用フラグと手動回転。M1 とは別立て（place=NULL を誘発させない）
    AddColumns {
        table: "photos",
        columns: &[
            ("favorite", "INTEGER DEFAULT 0"),
            ("hidden", "INTEGER DEFAULT 0"),
            ("rotation", "INTEGER DEFAULT 0"),
        ],
        resets_place: false,
    },
    // M3: 位置推定の段階窓で錨との時間差を残す列（これも place=NULL を誘発させない）
    AddColumns {
        table: "photos",
        columns: &[("infer_dt_s", "INTEGER")],
        resets_place: false,
    },
    // M4: AI 評価規則 v3
    AddColumns {
        table: "photo_ai_features",
        columns: &[
            ("horizon_status", "TEXT NOT NULL DEFAULT ''"),
            ("horizon_score", "INTEGER"),
        ],
        resets_place: false,
    },
    // M5: 自動 AI パイプライン用の進捗状態
    AddColumns {
        table: "photo_ai_groups",
        columns: &[
            ("status", "TEXT NOT NULL DEFAULT 'judged'"),
            ("same_scene", "INTEGER NOT NULL DEFAULT 1"),
            ("judge_version", "TEXT NOT NULL DEFAULT ''"),
            ("judge_model", "TEXT NOT NULL DEFAULT ''"),
            ("shortlist_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("error", "TEXT NOT NULL DEFAULT ''"),
            ("attempts", "INTEGER NOT NULL DEFAULT 0"),
            ("judge_order", "TEXT NOT NULL DEFAULT ''"),
            ("presented_json", "TEXT NOT NULL DEFAULT '[]'"),
        ],
        resets_place: false,
    },
    // M6
    AddColumns {
        table: "photo_ai_group_members",
        columns: &[
            ("shortlisted", "INTEGER NOT NULL DEFAULT 1"),
            ("local_score", "REAL"),
        ],
        resets_place: false,
    },
];

/// M7: `photo_embeddings` 導入前の embedding をキャッシュへ写す（既にあれば何もしない）。
const BACKFILL_EMBEDDINGS: &str = "INSERT OR IGNORE INTO photo_embeddings(
                         content_sha256,embedding_model,embedding_dim,embedding,analyzed_at)
                       SELECT content_sha256,embedding_model,embedding_dim,embedding,analyzed_at
                       FROM photo_ai_features WHERE error=''";

/// 移行が実際にやったこと（件数・名前だけ）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migration {
    /// 新しく作った表・索引の名前。
    pub created: Vec<String>,
    /// 足した列（`表.列`）。
    pub added_columns: Vec<String>,
    /// M1 で `place` を NULL に戻した（全行を再 enrich させる）。
    pub places_reset: bool,
    /// M7 で `photo_embeddings` に写した行数。
    pub embeddings_copied: usize,
}

impl Migration {
    /// 何も変えなかった。
    pub fn is_noop(&self) -> bool {
        self.created.is_empty()
            && self.added_columns.is_empty()
            && !self.places_reset
            && self.embeddings_copied == 0
    }
}

/// DB に無い（作る・足す必要がある）もの。読み取りだけで数える（`--dry-run`）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// 無い表・索引の名前。
    pub missing_objects: Vec<String>,
    /// 既存の表に無い列（`表.列`）。
    pub missing_columns: Vec<String>,
    /// M1 の列が足りない（足すと全行の `place` が NULL に戻る）。
    pub would_reset_places: bool,
    /// M7 で `photo_embeddings` に写される行数。
    pub embeddings_to_copy: i64,
}

impl Pending {
    pub fn is_noop(&self) -> bool {
        self.missing_objects.is_empty()
            && self.missing_columns.is_empty()
            && !self.would_reset_places
            && self.embeddings_to_copy == 0
    }
}

/// `table` の列名（無い表なら空）。`table` は定数だけを渡す。
fn column_names(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    stmt.query_map([], |row| row.get::<_, String>(1))?.collect()
}

fn object_exists(conn: &Connection, kind: &str, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2",
        [kind, name],
        |_| Ok(()),
    )
    .map(|()| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        other => Err(other),
    })
}

/// スキーマを作り、足りない列を足す（M0〜M7）。書き込み用の接続で呼ぶ。
///
/// 全体を 1 つの `BEGIN IMMEDIATE` に入れる。**既存の本番相当の DB では何も書かない**（`is_noop()`）。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Migration> {
    // M0: WAL。既に WAL なら何も起きない（トランザクションの外でないと変えられない）
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut done = Migration::default();

    for table in TABLES {
        if !object_exists(&tx, "table", table)? {
            done.created.push((*table).to_string());
        }
    }
    for index in INDEXES {
        if !object_exists(&tx, "index", index)? {
            done.created.push((*index).to_string());
        }
    }
    // `IF NOT EXISTS` なので、あるものには何も起きない（スキーマのバージョンも変わらない）
    tx.execute_batch(SCHEMA)?;

    for add in ADD_COLUMNS {
        let present = column_names(&tx, add.table)?;
        let mut added = false;
        for (name, definition) in add.columns {
            if present.iter().any(|p| p == name) {
                continue;
            }
            tx.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {name} {definition}",
                add.table
            ))?;
            done.added_columns.push(format!("{}.{name}", add.table));
            added = true;
        }
        if added && add.resets_place {
            // 次のサイクルで全行を再 enrich させる（地理院を大量に叩く）。列を**足したときだけ**
            tx.execute("UPDATE photos SET place=NULL", [])?;
            done.places_reset = true;
        }
    }

    done.embeddings_copied = tx.execute(BACKFILL_EMBEDDINGS, [])?;
    tx.commit()?;
    Ok(done)
}

/// [`migrate`] がやるはずのことを、書かずに数える。読み取り専用の接続でよい。
pub fn pending(conn: &Connection) -> rusqlite::Result<Pending> {
    let mut out = Pending::default();
    for table in TABLES {
        if !object_exists(conn, "table", table)? {
            out.missing_objects.push((*table).to_string());
        }
    }
    for index in INDEXES {
        if !object_exists(conn, "index", index)? {
            out.missing_objects.push((*index).to_string());
        }
    }
    for add in ADD_COLUMNS {
        let present = column_names(conn, add.table)?;
        if present.is_empty() {
            continue; // 表ごと無い（上の missing_objects に入っている）
        }
        for (name, _) in add.columns {
            if !present.iter().any(|p| p == name) {
                out.missing_columns.push(format!("{}.{name}", add.table));
                out.would_reset_places |= add.resets_place;
            }
        }
    }
    if object_exists(conn, "table", "photo_ai_features")?
        && object_exists(conn, "table", "photo_embeddings")?
    {
        let features = column_names(conn, "photo_ai_features")?;
        if ["content_sha256", "embedding_model", "error"]
            .iter()
            .all(|c| features.iter().any(|f| f == c))
        {
            // INSERT OR IGNORE が実際に挿入する行数（主キー (sha, model) の重複を 1 つに数える）
            out.embeddings_to_copy = conn.query_row(
                "SELECT COUNT(*) FROM (
                   SELECT DISTINCT f.content_sha256, f.embedding_model
                   FROM photo_ai_features f
                   WHERE f.error='' AND NOT EXISTS (
                     SELECT 1 FROM photo_embeddings e
                     WHERE e.content_sha256=f.content_sha256
                       AND e.embedding_model=f.embedding_model))",
                [],
                |row| row.get(0),
            )?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use serde_json::{Value, json};

    const PYTHON_SCHEMA: &str = include_str!("../tests/fixtures/schema-python.json");

    fn fresh(dir: &TempDir) -> Connection {
        let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
        migrate(&mut conn).unwrap();
        conn
    }

    /// `sqlite_master`（作成順）。
    fn objects(conn: &Connection) -> Vec<Value> {
        let mut stmt = conn
            .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |r| {
            Ok(json!({
                "type": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "tbl_name": r.get::<_, String>(2)?,
                "sql": r.get::<_, Option<String>>(3)?,
            }))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// 各表の `table_xinfo` と索引（`gen_schema_fixture.py` と同じ形）。
    fn tables(conn: &Connection) -> Value {
        let mut out = serde_json::Map::new();
        for o in objects(conn) {
            if o["type"] != "table" {
                continue;
            }
            let name = o["name"].as_str().unwrap().to_string();
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_xinfo({name})"))
                .unwrap();
            let columns: Vec<Value> = stmt
                .query_map([], |r| {
                    Ok(json!({
                        "cid": r.get::<_, i64>(0)?,
                        "name": r.get::<_, String>(1)?,
                        "type": r.get::<_, String>(2)?,
                        "notnull": r.get::<_, i64>(3)?,
                        "dflt_value": r.get::<_, Option<String>>(4)?,
                        "pk": r.get::<_, i64>(5)?,
                        "hidden": r.get::<_, i64>(6)?,
                    }))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let mut stmt = conn.prepare(&format!("PRAGMA index_list({name})")).unwrap();
            let mut indexes: Vec<(String, Value)> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .map(|(ix, unique, origin, partial)| {
                    let mut stmt = conn.prepare(&format!("PRAGMA index_info({ix})")).unwrap();
                    let cols: Vec<String> = stmt
                        .query_map([], |r| r.get::<_, String>(2))
                        .unwrap()
                        .map(Result::unwrap)
                        .collect();
                    (
                        ix.clone(),
                        json!({"name": ix, "unique": unique, "origin": origin,
                               "partial": partial, "columns": cols}),
                    )
                })
                .collect();
            indexes.sort_by(|a, b| a.0.cmp(&b.0));
            let indexes: Vec<Value> = indexes.into_iter().map(|(_, v)| v).collect();
            out.insert(name, json!({"columns": columns, "indexes": indexes}));
        }
        Value::Object(out)
    }

    /// Python が空の置き場から作った DB（フィクスチャ）の `sqlite_master` を、そのまま流して DB を作る。
    /// 本番の DB と同じ系統（`ALTER` で列が足された形）の、全移行が済んだスキーマ。
    fn python_shaped(dir: &TempDir) -> Connection {
        let fixture: Value = serde_json::from_str(PYTHON_SCHEMA).unwrap();
        let conn = Connection::open(dir.path().join("catalog.db")).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        for o in fixture["objects"].as_array().unwrap() {
            if let Some(sql) = o["sql"].as_str() {
                conn.execute_batch(sql).unwrap();
            }
        }
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn fresh_database_matches_the_python_one() {
        let dir = TempDir::new("schema-fresh");
        let conn = fresh(&dir);
        let fixture: Value = serde_json::from_str(PYTHON_SCHEMA).unwrap();
        // 表・索引の集合と、`sql` の文字列まで（photos も含めて）一致する
        assert_eq!(Value::Array(objects(&conn)), fixture["objects"]);
        // 列（名前・型・NOT NULL・既定値・主キー・並び）と索引（列・一意性）
        assert_eq!(tables(&conn), fixture["tables"]);
    }

    #[test]
    fn fresh_database_creates_everything_and_is_wal() {
        let dir = TempDir::new("schema-created");
        let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
        let done = migrate(&mut conn).unwrap();
        assert_eq!(done.created.len(), TABLES.len() + INDEXES.len());
        // 列は最初から全部あるので ALTER は 1 つも起きない（M1 の place=NULL も起きない）
        assert!(done.added_columns.is_empty(), "{done:?}");
        assert!(!done.places_reset);
        assert_eq!(done.embeddings_copied, 0);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        // 読み取り層が要求する表・列がそろっている
        crate::db::check_schema(&conn).unwrap();
    }

    /// 本番相当のスキーマ（Python が全移行を終えた形）では、移行は何もしない。
    #[test]
    fn migration_is_a_no_op_on_a_production_shaped_database() {
        let dir = TempDir::new("schema-noop");
        let mut conn = python_shaped(&dir);
        // place が NULL の行と、埋まっている行（再 enrich の印）。embedding も 1 件（M7 の対象は既に写してある）
        conn.execute_batch(
            "INSERT INTO photos(source,drive_id,name,lat,lon,place) VALUES
               ('drive','a','n1',1.0,2.0,NULL),
               ('drive','b','n2',1.0,2.0,'p'),
               ('drive','c','n3',NULL,NULL,NULL);
             INSERT INTO photo_ai_features(photo_id,display,content_sha256,embedding_model,
               embedding_dim,embedding,analyzed_at) VALUES (1,'d','sha','m',1,x'00','t');
             INSERT INTO photo_embeddings VALUES ('sha','m',1,x'00','t');",
        )
        .unwrap();
        let null_places = count(&conn, "SELECT COUNT(*) FROM photos WHERE place IS NULL");
        let before_objects = objects(&conn);
        let before_version = count(&conn, "PRAGMA schema_version");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let file = dir.path().join("catalog.db");
        let before_bytes = std::fs::read(&file).unwrap();
        let before_changes = conn.total_changes();

        // dry-run 相当は先に、何もないと答える
        assert!(pending(&conn).unwrap().is_noop());
        let done = migrate(&mut conn).unwrap();

        assert!(done.is_noop(), "{done:?}");
        assert_eq!(objects(&conn), before_objects);
        assert_eq!(count(&conn, "PRAGMA schema_version"), before_version);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM photos WHERE place IS NULL"),
            null_places
        );
        // 1 行も書いていない（M7 の INSERT OR IGNORE も 0 行）
        assert_eq!(conn.total_changes(), before_changes);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), before_bytes);
        // 毎起動やっても同じ
        assert!(migrate(&mut conn).unwrap().is_noop());
    }

    /// 旧形（M1 の列が無い）の DB では、Python と同じく列を足して全行の place を NULL に戻す。
    #[test]
    fn migration_from_the_old_shape_adds_columns_and_resets_places_once() {
        let dir = TempDir::new("schema-old");
        let mut conn = Connection::open(dir.path().join("catalog.db")).unwrap();
        // 旧版: muni_cd pref city favorite hidden rotation infer_dt_s が無い photos と、
        // AI 表の追加列が無い旧形
        conn.execute_batch(
            "CREATE TABLE photos(
               id INTEGER PRIMARY KEY, source TEXT NOT NULL, drive_id TEXT UNIQUE, path TEXT UNIQUE,
               display TEXT, name TEXT NOT NULL, md5 TEXT, mtime REAL, taken_at TEXT, lat REAL, lon REAL,
               loc_source TEXT, inferred_from INTEGER, place TEXT, elev REAL, title TEXT,
               ai_caption TEXT, indexed_at TEXT, fetched_at TEXT);
             CREATE TABLE photo_ai_groups(group_id TEXT PRIMARY KEY, kind TEXT NOT NULL,
               summary TEXT NOT NULL DEFAULT '', min_similarity REAL, max_similarity REAL,
               avg_similarity REAL, algorithm TEXT NOT NULL, analyzed_at TEXT NOT NULL);
             INSERT INTO photos(source,drive_id,name,place) VALUES ('drive','a','n','p1'),('drive','b','n','p2');
             INSERT INTO photo_ai_groups VALUES ('g','scene','',NULL,NULL,NULL,'a','t');",
        )
        .unwrap();
        let pending_before = pending(&conn).unwrap();
        assert!(pending_before.would_reset_places);
        assert!(
            pending_before
                .missing_columns
                .contains(&"photos.muni_cd".to_string())
        );

        let done = migrate(&mut conn).unwrap();
        assert!(done.places_reset);
        for column in [
            "photos.muni_cd",
            "photos.favorite",
            "photos.rotation",
            "photos.infer_dt_s",
            "photo_ai_groups.status",
            "photo_ai_groups.presented_json",
        ] {
            assert!(done.added_columns.contains(&column.to_string()), "{column}");
        }
        // 既存行の place は全部 NULL（次のサイクルで再 enrich される）。既存の group は judged のまま
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM photos WHERE place IS NULL"),
            2
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM photo_ai_groups WHERE status='judged'"
            ),
            1
        );
        // 足した列の既定値
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM photos WHERE favorite=0 AND hidden=0 AND rotation=0"
            ),
            2
        );
        crate::db::check_schema(&conn).unwrap();

        // 2 回目は何もしない。ここで place を埋め直しても、もう NULL に戻らない
        conn.execute("UPDATE photos SET place='filled'", [])
            .unwrap();
        assert!(migrate(&mut conn).unwrap().is_noop());
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM photos WHERE place IS NULL"),
            0
        );
    }

    /// M1 の列があって M2 の列だけ無い DB では、place は NULL に戻さない（M1 とは別立て）。
    #[test]
    fn adding_the_flag_columns_alone_does_not_reset_places() {
        let dir = TempDir::new("schema-m2");
        let mut conn = python_shaped(&dir);
        conn.execute_batch(
            "INSERT INTO photos(source,drive_id,name,place) VALUES ('drive','a','n','p1');
             ALTER TABLE photos DROP COLUMN rotation;
             ALTER TABLE photos DROP COLUMN hidden;
             ALTER TABLE photos DROP COLUMN infer_dt_s;",
        )
        .unwrap();
        let p = pending(&conn).unwrap();
        assert!(!p.would_reset_places);
        assert_eq!(
            p.missing_columns,
            ["photos.hidden", "photos.rotation", "photos.infer_dt_s"]
        );
        let done = migrate(&mut conn).unwrap();
        assert!(!done.places_reset);
        assert_eq!(done.added_columns.len(), 3);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM photos WHERE place='p1'"),
            1
        );
    }

    /// M7 は毎起動。写されていない embedding だけを写し、2 回目は何もしない。dry-run の数も合う。
    #[test]
    fn embeddings_backfill_copies_only_the_missing_ones() {
        let dir = TempDir::new("schema-m7");
        let mut conn = python_shaped(&dir);
        conn.execute_batch(
            "INSERT INTO photo_ai_features(photo_id,display,content_sha256,embedding_model,
               embedding_dim,embedding,analyzed_at,error) VALUES
               (1,'d','sha1','m',1,x'01','t',''),
               (2,'d','sha1','m',1,x'01','t',''),
               (3,'d','sha2','m',1,x'02','t',''),
               (4,'d','sha3','m',1,x'03','t','failed');
             INSERT INTO photo_embeddings VALUES ('sha2','m',1,x'02','t');",
        )
        .unwrap();
        // sha1（photo 1・2 は同じ鍵）だけが写される。sha2 は既にある。sha3 は error 付きで対象外
        assert_eq!(pending(&conn).unwrap().embeddings_to_copy, 1);
        let done = migrate(&mut conn).unwrap();
        assert_eq!(done.embeddings_copied, 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM photo_embeddings"), 2);
        assert!(migrate(&mut conn).unwrap().is_noop());
        assert_eq!(pending(&conn).unwrap().embeddings_to_copy, 0);
    }

    #[test]
    fn pending_on_an_empty_database_lists_everything() {
        let dir = TempDir::new("schema-pending-empty");
        let conn = Connection::open(dir.path().join("catalog.db")).unwrap();
        let p = pending(&conn).unwrap();
        assert_eq!(p.missing_objects.len(), TABLES.len() + INDEXES.len());
        assert!(p.missing_columns.is_empty()); // 表ごと作るので、足す列としては数えない
        assert!(!p.would_reset_places);
    }

    #[test]
    fn pending_works_on_a_read_only_connection() {
        let dir = TempDir::new("schema-pending-ro");
        let conn = fresh(&dir);
        drop(conn);
        let ro = crate::db::open_read_only(&dir.path().join("catalog.db")).unwrap();
        assert!(pending(&ro).unwrap().is_noop());
    }
}
