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
INSERT INTO "photo_ai_features" VALUES(1,'derivatives/1.jpg','m1',1.0,'0000000000000000000000000000000000000000000000000000000000000001','gemini-embedding-2',3,X'0000803F0000000000000000','dog','active','open','good','level',95,80,80,60,0,'invalid','{"not":"list"}',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(2,'derivatives/2.jpg','m2',1.0,'0000000000000000000000000000000000000000000000000000000000000002','gemini-embedding-2',3,X'A4707D3FCDCC4C3D00000000','dog','active','open','good','level',95,80,80,90,1,'["合成の理由"]','[]',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(3,'derivatives/3.jpg','m3',1.0,'0000000000000000000000000000000000000000000000000000000000000003','gemini-embedding-2',3,X'000000000000803F00000000','dog','active','open','good','level',95,80,80,NULL,NULL,'invalid','[]',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(4,'derivatives/4.jpg','m4',1.0,'0000000000000000000000000000000000000000000000000000000000000004','gemini-embedding-2',3,X'00000000000000000000803F','dog','active','open','good','level',95,80,80,95,NULL,'invalid','[]',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(5,'derivatives/5.jpg','m5',1.0,'0000000000000000000000000000000000000000000000000000000000000005','gemini-embedding-2',3,X'000080BF0000000000000000','dog','active','open','good','level',95,80,80,0,NULL,'invalid','[]',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(6,'derivatives/6.jpg','m6',1.0,'0000000000000000000000000000000000000000000000000000000000000006','gemini-embedding-2',3,X'0000003F0000003F00000000','dog','active','open','good','level',95,80,80,NULL,NULL,'invalid','[]',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(7,'derivatives/7.jpg','m7',1.0,'0000000000000000000000000000000000000000000000000000000000000007','gemini-embedding-2',3,X'000000000000803F00000000','dog','active','open','good','level',95,80,80,NULL,NULL,'invalid','[]',NULL,'fixture','');
INSERT INTO "photo_ai_features" VALUES(8,'derivatives/8.jpg','m8',1.0,'0000000000000000000000000000000000000000000000000000000000000008','gemini-embedding-2',3,X'00000000000000000000803F','dog','active','open','good','level',95,80,80,NULL,NULL,'invalid','[]',NULL,'fixture','');
CREATE TABLE photo_ai_group_members(
  group_id TEXT NOT NULL,
  photo_id INTEGER NOT NULL,
  rank_in_group INTEGER NOT NULL,
  recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
  shortlisted INTEGER NOT NULL DEFAULT 1,
  local_score REAL,
  PRIMARY KEY(group_id, photo_id)
);
INSERT INTO "photo_ai_group_members" VALUES('scene-a',1,1,0,1,NULL);
INSERT INTO "photo_ai_group_members" VALUES('scene-a',2,2,1,1,NULL);
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
INSERT INTO "photo_ai_groups" VALUES('scene-a','scene','合成場面',0.95,0.95,0.95,'fixture','fixture','judged',1,'','','[]','',0,'','[]');
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
INSERT INTO "photo_quality" VALUES(3,'derivatives/3.jpg','m3',1.0,'0000000000000000000000000000000000000000000000000000000000000003',2,'similar',3,1,45.0,'[]','[]','fixture','fixture');
INSERT INTO "photo_quality" VALUES(5,'derivatives/5.jpg','m5',1.0,'0000000000000000000000000000000000000000000000000000000000000005',2,'similar',5,0,0.0,'[]','[]','fixture','fixture');
INSERT INTO "photo_quality" VALUES(7,'derivatives/7.jpg','m7',1.0,'0000000000000000000000000000000000000000000000000000000000000007',3,'exact',7,0,99.0,'[]','[]','fixture','fixture');
INSERT INTO "photo_quality" VALUES(8,'derivatives/8.jpg','m8',1.0,'0000000000000000000000000000000000000000000000000000000000000008',3,'exact',8,1,30.0,'[]','[]','fixture','fixture');
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
INSERT INTO "photo_quality_features" VALUES(1,'derivatives/1.jpg','m1',1.0,'0000000000000000000000000000000000000000000000000000000000000001',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(2,'derivatives/2.jpg','m2',1.0,'0000000000000000000000000000000000000000000000000000000000000002',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(3,'derivatives/3.jpg','m3',1.0,'0000000000000000000000000000000000000000000000000000000000000003',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(4,'derivatives/4.jpg','m4',1.0,'0000000000000000000000000000000000000000000000000000000000000004',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(5,'derivatives/5.jpg','m5',1.0,'0000000000000000000000000000000000000000000000000000000000000005',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(6,'derivatives/6.jpg','m6',1.0,'0000000000000000000000000000000000000000000000000000000000000006',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(7,'derivatives/7.jpg','m7',1.0,'0000000000000000000000000000000000000000000000000000000000000007',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
INSERT INTO "photo_quality_features" VALUES(8,'derivatives/8.jpg','m8',1.0,'0000000000000000000000000000000000000000000000000000000000000008',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','fixture');
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
INSERT INTO "photos" VALUES(1,'drive',NULL,NULL,'derivatives/1.jpg','synthetic-1.jpg','m1',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,'見本町',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(2,'drive',NULL,NULL,'derivatives/2.jpg','synthetic-2.jpg','m2',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,1,0,0);
INSERT INTO "photos" VALUES(3,'drive',NULL,NULL,'derivatives/3.jpg','synthetic-3.jpg','m3',1.0,NULL,35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(4,'drive',NULL,NULL,'derivatives/4.jpg','synthetic-4.jpg','m4',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,1,0);
INSERT INTO "photos" VALUES(5,'drive',NULL,NULL,'derivatives/5.jpg','synthetic-5.jpg','m5',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(6,'drive',NULL,NULL,'derivatives/6.jpg','synthetic-6.jpg','m6',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(7,'drive',NULL,NULL,'derivatives/7.jpg','synthetic-7.jpg','m7',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(8,'drive',NULL,NULL,'derivatives/8.jpg','synthetic-8.jpg','m8',1.0,'2026-01-01 10:00',35.0,139.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
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
