"""永続キャッシュとカタログDB（server.py L176-L265 / L274-L338 より移動）"""
import json
import os
import sqlite3
import threading
import time

from .config import CACHE_FILE, DB_PATH


# ============================================================
# 永続キャッシュ（地理院の逆ジオコーディング）
# ------------------------------------------------------------
# 読み書きは必ず cache_get / cache_put 経由（_cache への直接アクセスはこの節の中だけ）。
# 実ディスク書き込みは cache_saver スレッドが CACHE_SAVE_INTERVAL_S 間隔で間引く。
# ※毎回全量書き込みするとSD/eMMCの寿命を縮めるため、
#   dirtyフラグ＋デバウンスで「変化があった時だけ・最短60秒間隔」に抑える。
# ============================================================
_cache = {}
_cache_lock = threading.Lock()
_cache_dirty = False
_cache_saved_at = 0.0
_MISS = object()                  # cache_get のミス判定用（None/Falseも正当な値のため）

CACHE_SAVE_INTERVAL_S = 60        # 実書き込みの最短間隔
CACHE_SAVER_TICK_S = 10           # 保存スレッドの起床間隔
# 現役キー。これ以外（exif:/exif2: 等の死蔵レガシーや、削除した表示機能の
# adsbdb:/airport:/intel2:）は起動時に破棄する
CACHE_LIVE_PREFIXES = ("geo:",)
CACHE_LIVE_EXACT = frozenset({"muni_table"})


def load_cache():
    global _cache
    if CACHE_FILE.exists():
        try:
            _cache = json.loads(CACHE_FILE.read_text(encoding="utf-8"))
        except Exception:
            _cache = {}


def purge_cache_legacy():
    """現役でないキーを捨てる（起動時に1回）。旧EXIFキャッシュがファイルの大半を占めるため"""
    global _cache_dirty
    with _cache_lock:
        dead = [k for k in _cache
                if k not in CACHE_LIVE_EXACT and not k.startswith(CACHE_LIVE_PREFIXES)]
        for k in dead:
            del _cache[k]
        if dead:
            _cache_dirty = True
        left = len(_cache)
    if dead:
        print(f"[cache] レガシーキー削除: {len(dead)}件 / 現役{left}件を維持")
    return len(dead)


def cache_get(key, default=None):
    with _cache_lock:
        return _cache.get(key, default)


def cache_put(key, value):
    global _cache_dirty
    with _cache_lock:
        _cache[key] = value
        _cache_dirty = True


def save_cache(force=False):
    """dirtyかつ前回保存から間隔経過時のみ書き込む。失敗しても例外は上げない"""
    global _cache_dirty, _cache_saved_at
    with _cache_lock:
        if not _cache_dirty:
            return False
        if not force and time.time() - _cache_saved_at < CACHE_SAVE_INTERVAL_S:
            return False
        body = json.dumps(_cache, ensure_ascii=False, indent=1)   # ロック下でスナップショット
        _cache_dirty = False
        _cache_saved_at = time.time()
    tmp = CACHE_FILE.with_suffix(".json.tmp")
    try:
        # 書きかけで落ちても現役キーを失わないよう一時ファイル経由で差し替える
        tmp.write_text(body, encoding="utf-8")
        os.replace(tmp, CACHE_FILE)
    except Exception as e:
        with _cache_lock:
            _cache_dirty = True        # 次のtickで再試行
        print(f"[cache] 保存失敗（次回再試行）: {e}")
        return False
    return True


def cache_saver():
    """キャッシュ書き出し専用の軽量スレッド（間引きは save_cache 側で行う）"""
    while True:
        time.sleep(CACHE_SAVER_TICK_S)
        try:
            save_cache()
        except Exception as e:
            print(f"[cache] 保存スレッド異常: {e}")


# ============================================================
# カタログDB
# ============================================================
SCHEMA = """
CREATE TABLE IF NOT EXISTS photos(
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
);
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
"""
_db = None
_db_lock = threading.Lock()


def db():
    global _db
    if _db is None:
        # 新規チェックアウト直後は、Git管理外のデータディレクトリがまだ存在しない。
        # SQLiteへ接続する前に作成し、初回起動とテストをそのまま実行できるようにする。
        DB_PATH.parent.mkdir(parents=True, exist_ok=True)
        _db = sqlite3.connect(DB_PATH, check_same_thread=False)
        _db.row_factory = sqlite3.Row
        _db.execute("PRAGMA journal_mode=WAL")
        _db.executescript(SCHEMA)
        # スキーマ移行: 自治体情報列（無ければ追加し、地名を再解決させる）
        migrated = False
        for col in ("muni_cd TEXT", "pref TEXT", "city TEXT"):
            try:
                _db.execute(f"ALTER TABLE photos ADD COLUMN {col}")
                migrated = True
            except sqlite3.OperationalError:
                pass
        if migrated:
            _db.execute("UPDATE photos SET place=NULL")   # 次サイクルで再enrich
            _db.commit()
        # スキーマ移行: 再生スケジューラ用フラグ列。
        # ※上の migrated ループとは別立てにする（place=NULL の再enrichを誘発させないため）
        for col in ("favorite INTEGER DEFAULT 0", "hidden INTEGER DEFAULT 0"):
            try:
                _db.execute(f"ALTER TABLE photos ADD COLUMN {col}")
                _db.commit()
            except sqlite3.OperationalError:
                pass
        # スキーマ移行: 位置推定の段階窓で「錨とどれだけ離れていたか」を残す列。
        # ※これも1つ目の migrated ループには入れないこと（place=NULL の
        #   再enrichが全行に走り、地理院APIを1400回以上叩いてしまうため）。
        #   既存行は NULL のまま＝次回 infer_locations() で埋まる
        try:
            _db.execute("ALTER TABLE photos ADD COLUMN infer_dt_s INTEGER")
            _db.commit()
        except sqlite3.OperationalError:
            pass
        # AI評価規則v3: 風景・建築写真の水平評価。既存DBには列だけを安全に追加し、
        # 過去評価は空欄のまま（次回Gemini再評価で埋める）。
        for col in ("horizon_status TEXT NOT NULL DEFAULT ''", "horizon_score INTEGER"):
            try:
                _db.execute(f"ALTER TABLE photo_ai_features ADD COLUMN {col}")
                _db.commit()
            except sqlite3.OperationalError:
                pass
        # 自動AIパイプライン用の進捗状態。既存PoCグループは完成済みとして扱う。
        for col in (
            "status TEXT NOT NULL DEFAULT 'judged'",
            "same_scene INTEGER NOT NULL DEFAULT 1",
            "judge_version TEXT NOT NULL DEFAULT ''",
            "judge_model TEXT NOT NULL DEFAULT ''",
            "shortlist_json TEXT NOT NULL DEFAULT '[]'",
            "error TEXT NOT NULL DEFAULT ''",
            "attempts INTEGER NOT NULL DEFAULT 0",
            # Geminiに見せた並び（rank=順位どおり、shuffled=並べ替え、''=記録前）と写真IDの順
            "judge_order TEXT NOT NULL DEFAULT ''",
            "presented_json TEXT NOT NULL DEFAULT '[]'",
        ):
            try:
                _db.execute(f"ALTER TABLE photo_ai_groups ADD COLUMN {col}")
                _db.commit()
            except sqlite3.OperationalError:
                pass
        for col in ("shortlisted INTEGER NOT NULL DEFAULT 1", "local_score REAL"):
            try:
                _db.execute(f"ALTER TABLE photo_ai_group_members ADD COLUMN {col}")
                _db.commit()
            except sqlite3.OperationalError:
                pass
        # photo_embeddings導入前のembeddingをキャッシュへ写す（既にあれば何もしない）。
        _db.execute("""INSERT OR IGNORE INTO photo_embeddings(
                         content_sha256,embedding_model,embedding_dim,embedding,analyzed_at)
                       SELECT content_sha256,embedding_model,embedding_dim,embedding,analyzed_at
                       FROM photo_ai_features WHERE error=''""")
        _db.commit()
    return _db


def dbx(sql, args=()):
    with _db_lock:
        cur = db().execute(sql, args)
        db().commit()
        return cur


def dbq(sql, args=()):
    with _db_lock:
        return db().execute(sql, args).fetchall()


def dbm(sql, rows):
    """同じSQLを1トランザクションでまとめて実行する。"""
    with _db_lock:
        conn = db()
        cur = conn.executemany(sql, rows)
        conn.commit()
        return cur


def replace_photo_quality(rows):
    """品質結果を一瞬で差し替え、APIから中間状態が見えないようにする。"""
    insert_sql = """INSERT INTO photo_quality(
        photo_id, display, source_md5, source_mtime, content_sha256,
        group_id, group_kind, rank_in_group, recommended, score,
        reasons_json, issues_json, algorithm, analyzed_at
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)"""
    with _db_lock:
        conn = db()
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("DELETE FROM photo_quality")
            conn.executemany(insert_sql, rows)
            conn.commit()
        except Exception:
            conn.rollback()
            raise


def replace_photo_ai_catalog(features, groups, members):
    """AI検索用の特徴量・評価・グループを1トランザクションで差し替える。

    PoCレポートの途中状態が検索APIから見えないことを優先し、3表を同時に更新する。
    favorite/hiddenはphotos本体の列なので、この処理では一切変更しない。
    """
    feature_sql = """INSERT INTO photo_ai_features(
        photo_id, display, source_md5, source_mtime, content_sha256,
        embedding_model, embedding_dim, embedding,
        subject, pet_state, eyes, face_crop, horizon_status, horizon_score,
        subject_sharpness,
        aesthetic_score, keep_score, recommended, reasons_json, concerns_json,
        judge_model, analyzed_at, error
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"""
    group_sql = """INSERT INTO photo_ai_groups(
        group_id, kind, summary, min_similarity, max_similarity, avg_similarity,
        algorithm, analyzed_at
      ) VALUES(?,?,?,?,?,?,?,?)"""
    member_sql = """INSERT INTO photo_ai_group_members(
        group_id, photo_id, rank_in_group, recommended
      ) VALUES(?,?,?,?)"""
    with _db_lock:
        conn = db()
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("DELETE FROM photo_ai_group_members")
            conn.execute("DELETE FROM photo_ai_groups")
            conn.execute("DELETE FROM photo_ai_features")
            conn.executemany(feature_sql, features)
            conn.executemany(group_sql, groups)
            conn.executemany(member_sql, members)
            conn.commit()
        except Exception:
            conn.rollback()
            raise


def replace_photo_ai_groups(groups, members):
    """自動パイプラインが計算した最新グループ集合だけを原子的に差し替える。"""
    group_sql = """INSERT INTO photo_ai_groups(
        group_id,kind,summary,min_similarity,max_similarity,avg_similarity,
        algorithm,analyzed_at,status,same_scene,judge_version,judge_model,
        shortlist_json,error,attempts,judge_order,presented_json
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"""
    member_sql = """INSERT INTO photo_ai_group_members(
        group_id,photo_id,rank_in_group,recommended,shortlisted,local_score
      ) VALUES(?,?,?,?,?,?)"""
    with _db_lock:
        conn = db()
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("DELETE FROM photo_ai_group_members")
            conn.execute("DELETE FROM photo_ai_groups")
            conn.executemany(group_sql, groups)
            conn.executemany(member_sql, members)
            conn.commit()
        except Exception:
            conn.rollback()
            raise


def complete_photo_ai_group(group_id, same_scene, summary, member_recommendations,
                            feature_judgments, judge_model, judge_version, analyzed_at,
                            judge_order="", presented_ids=()):
    """Gemini比較結果をグループ・表示判定・写真詳細へまとめて確定する。"""
    feature_sql = """UPDATE photo_ai_features SET
        subject=?,pet_state=?,eyes=?,face_crop=?,horizon_status=?,horizon_score=?,
        subject_sharpness=?,aesthetic_score=?,keep_score=?,recommended=?,
        reasons_json=?,concerns_json=?,judge_model=?,analyzed_at=?
      WHERE photo_id=?"""
    with _db_lock:
        conn = db()
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute("""UPDATE photo_ai_groups SET
                status='judged',same_scene=?,summary=?,judge_model=?,judge_version=?,
                error='',analyzed_at=?,judge_order=?,presented_json=? WHERE group_id=?""",
                         (int(bool(same_scene)), summary, judge_model, judge_version,
                          analyzed_at, judge_order, json.dumps(list(presented_ids)), group_id))
            conn.executemany(
                "UPDATE photo_ai_group_members SET recommended=? WHERE group_id=? AND photo_id=?",
                [(int(bool(value)), group_id, pid)
                 for pid, value in member_recommendations.items()])
            conn.executemany(feature_sql, feature_judgments)
            conn.commit()
        except Exception:
            conn.rollback()
            raise


def setting_get(key, default=None):
    r = dbq("SELECT value FROM settings WHERE key=?", (key,))
    return r[0]["value"] if r else default


def setting_set(key, value):
    """value=None で該当キーを削除する（＝未設定に戻す）"""
    if value is None:
        dbx("DELETE FROM settings WHERE key=?", (key,))
    else:
        dbx("""INSERT INTO settings(key, value) VALUES(?, ?)
               ON CONFLICT(key) DO UPDATE SET value=excluded.value""", (key, value))
