BEGIN TRANSACTION;
CREATE TABLE photo_ai_features(
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
CREATE TABLE photo_ai_group_members(
  group_id TEXT NOT NULL,
  photo_id INTEGER NOT NULL,
  rank_in_group INTEGER NOT NULL,
  recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
  shortlisted INTEGER NOT NULL DEFAULT 1,
  local_score REAL,
  PRIMARY KEY(group_id, photo_id)
);
INSERT INTO "photo_ai_group_members" VALUES('ai-1',6,1,1,1,0.8);
INSERT INTO "photo_ai_group_members" VALUES('ai-1',7,2,0,1,0.5);
INSERT INTO "photo_ai_group_members" VALUES('ai-1',8,3,0,1,NULL);
INSERT INTO "photo_ai_group_members" VALUES('ai-2',9,1,0,1,0.6);
CREATE TABLE photo_ai_groups(
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
INSERT INTO "photo_ai_groups" VALUES('ai-1','scene','',NULL,NULL,NULL,'test','2025-06-10T00:00:00','judged',1,'','','[]','',0,'','[]');
INSERT INTO "photo_ai_groups" VALUES('ai-2','scene','',NULL,NULL,NULL,'test','2025-06-10T00:00:00','pending',1,'','','[]','',0,'','[]');
CREATE TABLE photo_embeddings(
  content_sha256 TEXT NOT NULL,
  embedding_model TEXT NOT NULL,
  embedding_dim INTEGER NOT NULL,
  embedding BLOB NOT NULL,
  analyzed_at TEXT NOT NULL,
  PRIMARY KEY(content_sha256, embedding_model)
);
CREATE TABLE photo_loc_clues(
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
CREATE TABLE photo_loc_runs(
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
CREATE TABLE photo_loc_suggestions(
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
CREATE TABLE photo_quality(
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
INSERT INTO "photo_quality" VALUES(2,'derivatives/p02.jpg','md5-2',NULL,'sha',1,'similar',1,1,0.9,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(3,'derivatives/p03.jpg','md5-3',NULL,'sha',1,'similar',2,0,0.4,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(4,'derivatives/p04.jpg','md5-4',NULL,'sha',2,'exact',1,1,0.8,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(5,'derivatives/p05.jpg','md5-5',NULL,'sha',2,'exact',2,0,0.7,'[]','[]','test','2025-06-10T00:00:00');
CREATE TABLE photo_quality_features(
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
CREATE TABLE photos(
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
INSERT INTO "photos" VALUES(1,'drive',NULL,NULL,'derivatives/p01.jpg','IMG_0001.jpg','md5-1',NULL,'2025-06-01 10:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(2,'drive',NULL,NULL,'derivatives/p02.jpg','IMG_0002.jpg','md5-2',NULL,'2025-06-01 12:00',35.2,139.2,'inferred',NULL,NULL,'サンプル町',12.5,NULL,NULL,NULL,'2025-06-02T09:30:00',NULL,'架空県','見本市',1,0,0);
INSERT INTO "photos" VALUES(3,'drive',NULL,NULL,'derivatives/p03.jpg','IMG_0003.jpg','md5-3',NULL,'2025-06-01 12:01',35.2,139.2,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'2025-06-02T09:30:01',NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(4,'drive',NULL,NULL,'derivatives/p04.jpg','IMG_0004.jpg','md5-4',NULL,'2025-06-05 10:00',35.3,139.3,'exif',NULL,NULL,NULL,3.0,'夏の海',NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(5,'drive',NULL,NULL,'derivatives/p05.jpg','IMG_0005.jpg','md5-5',NULL,'2025-06-05 10:00',35.3,139.3,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(6,'drive',NULL,NULL,'derivatives/p06.jpg','IMG_0006.jpg','md5-6',NULL,'2025-06-05 11:00',35.4,139.4,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(7,'drive',NULL,NULL,'derivatives/p07.jpg','IMG_0007.jpg','md5-7',NULL,'2025-06-05 11:01',35.4,139.4,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(8,'drive',NULL,NULL,'derivatives/p08.jpg','IMG_0008.jpg','md5-8',NULL,'2025-06-05 11:02',35.4,139.4,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,1,0,0);
INSERT INTO "photos" VALUES(9,'drive',NULL,NULL,'derivatives/p09.jpg','IMG_0009.jpg','md5-9',NULL,'2025-06-05 11:03',35.5,139.5,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(10,'drive',NULL,NULL,'derivatives/p10.jpg','IMG_0010.jpg','md5-10',NULL,'',35.0,139.0,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(11,'drive',NULL,NULL,'derivatives/p11.jpg','IMG_0011.jpg','md5-11',NULL,'2025-06-01 10:00',35.0,139.0,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,1,0);
INSERT INTO "photos" VALUES(12,'drive',NULL,NULL,NULL,'IMG_0012.jpg','md5-12',NULL,'2025-06-01 10:00',35.0,139.0,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(13,'drive',NULL,NULL,'derivatives/p13.jpg','IMG_0013.jpg','md5-13',NULL,'2025-06-01 10:00',NULL,NULL,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);
CREATE INDEX idx_photos_time ON photos(taken_at);
CREATE INDEX idx_photo_quality_group ON photo_quality(group_kind, group_id);
CREATE INDEX idx_photo_quality_features_sha
  ON photo_quality_features(content_sha256);
CREATE INDEX idx_photo_ai_features_sha
  ON photo_ai_features(content_sha256);
CREATE INDEX idx_photo_ai_group_members_photo
  ON photo_ai_group_members(photo_id);
COMMIT;
