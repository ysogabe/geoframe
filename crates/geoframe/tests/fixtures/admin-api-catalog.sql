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
INSERT INTO "photo_ai_features" VALUES(1,'derivatives/p01.jpg','m1',10.0,'sha-a','gemini-embedding-2',768,X'00','','','','','',NULL,NULL,NULL,NULL,1,'[]','[]',NULL,'2025-06-10T00:00:00','');
INSERT INTO "photo_ai_features" VALUES(2,'derivatives/p02.jpg','m2',20.0,'sha-a','gemini-embedding-2',768,X'00','','','','','',NULL,NULL,NULL,NULL,NULL,'[]','[]',NULL,'2025-06-10T00:00:00','');
INSERT INTO "photo_ai_features" VALUES(6,'derivatives/p06.jpg','m6',60.0,'sha-b','gemini-embedding-2',768,X'00','','','','','',NULL,NULL,NULL,NULL,0,'[]','[]',NULL,'2025-06-10T00:00:00','');
INSERT INTO "photo_ai_features" VALUES(7,'derivatives/p07.jpg','m7',70.0,'sha-c','embeddinggemma-2',768,X'00','','','','','',NULL,NULL,NULL,NULL,1,'[]','[]',NULL,'2025-06-10T00:00:00','');
INSERT INTO "photo_ai_features" VALUES(8,'derivatives/p08.jpg','stale',80.0,'sha-e','gemini-embedding-2',768,X'00','','','','','',NULL,NULL,NULL,NULL,1,'[]','[]',NULL,'2025-06-10T00:00:00','');
INSERT INTO "photo_ai_features" VALUES(16,'derivatives/p16.jpg','m16',160.0,'sha-d','gemini-embedding-2',768,X'00','','','','','',NULL,NULL,NULL,NULL,1,'[]','[]',NULL,'2025-06-10T00:00:00','boom');
CREATE TABLE photo_ai_group_members(
  group_id TEXT NOT NULL,
  photo_id INTEGER NOT NULL,
  rank_in_group INTEGER NOT NULL,
  recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
  shortlisted INTEGER NOT NULL DEFAULT 1,
  local_score REAL,
  PRIMARY KEY(group_id, photo_id)
);
INSERT INTO "photo_ai_group_members" VALUES('ai-1',6,1,1,1,NULL);
INSERT INTO "photo_ai_group_members" VALUES('ai-1',7,2,0,1,NULL);
INSERT INTO "photo_ai_group_members" VALUES('ai-1',8,3,0,1,NULL);
INSERT INTO "photo_ai_group_members" VALUES('ai-2',9,1,0,1,NULL);
INSERT INTO "photo_ai_group_members" VALUES('ai-3',10,1,0,1,NULL);
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
INSERT INTO "photo_ai_groups" VALUES('ai-3','scene','',NULL,NULL,NULL,'test','2025-06-10T00:00:00','judged',0,'','','[]','',0,'','[]');
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
INSERT INTO "photo_loc_clues" VALUES(16,'derivatives/p16.jpg','m16',160.0,0.9,'landmark','test','2025-06-10T00:00:00','');
INSERT INTO "photo_loc_clues" VALUES(17,'derivatives/d-has-deriv.jpg','m17',170.0,0.3,'street','test','2025-06-10T00:00:00','');
INSERT INTO "photo_loc_clues" VALUES(19,'derivatives/p19.jpg','m19',190.0,0.1,'street','test','2025-06-10T00:00:00','');
INSERT INTO "photo_loc_clues" VALUES(23,'derivatives/p23.jpg','m23',230.0,0.05,'landmark','test','2025-06-10T00:00:00','');
INSERT INTO "photo_loc_clues" VALUES(24,'derivatives/p24.jpg','m24',240.0,0.9,'street','test','2025-06-10T00:00:00','x');
INSERT INTO "photo_loc_clues" VALUES(25,'derivatives/p25.jpg','old',250.0,0.9,'street','test','2025-06-10T00:00:00','');
INSERT INTO "photo_loc_clues" VALUES(26,'derivatives/p26.jpg','m26',260.0,0.5,'street','test','2025-06-10T00:00:00','');
INSERT INTO "photo_loc_clues" VALUES(27,'derivatives/p27.jpg','m27',270.0,NULL,'','test','2025-06-10T00:00:00','unavailable');
INSERT INTO "photo_loc_clues" VALUES(28,'derivatives/other.jpg','m28',280.0,0.9,'street','test','2025-06-10T00:00:00','');
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
INSERT INTO "photo_loc_runs" VALUES('loc-a','2025-06-01','test','loc-v2','[]','[]','done',0,'','','2025-06-10T00:00:00');
INSERT INTO "photo_loc_runs" VALUES('loc-b','2025-06-01','test','loc-v2','[]','[]','done',0,'','','2025-06-10T00:00:00');
INSERT INTO "photo_loc_runs" VALUES('loc-c','2025-06-01','test','loc-v2','[]','[]','error',0,'','','2025-06-10T00:00:00');
INSERT INTO "photo_loc_runs" VALUES('loc-d','2025-06-01','test','loc-v2','[]','[]','pending',0,'','','2025-06-10T00:00:00');
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
INSERT INTO "photo_loc_suggestions" VALUES(4,'loc-a','photo','位置のある写真の候補','','','city','low','[]',13.5,23.5,'Located','sample-model','2025-06-10T00:00:00');
INSERT INTO "photo_loc_suggestions" VALUES(16,'loc-a','photo','サンプル寺院','架空の街','','landmark','medium','["看板の文字", "建物の形"]',10.5,20.5,'Sample Temple','sample-model','2025-06-10T00:00:00');
INSERT INTO "photo_loc_suggestions" VALUES(18,'loc-a','day','架空の街','架空の街','','city','low','not json',NULL,NULL,'','sample-model','2025-06-10T00:00:00');
INSERT INTO "photo_loc_suggestions" VALUES(21,'loc-a','day','架空の地方','','','region','unknown','{"a": 1}',11.5,21.5,'Sample Region','sample-model','2025-06-10T00:00:00');
INSERT INTO "photo_loc_suggestions" VALUES(26,'loc-a','photo','サンプル広場','','','district','high','["x"]',12.5,22.5,'Sample Square','sample-model','2025-06-10T00:00:00');
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
INSERT INTO "photo_quality" VALUES(1,'derivatives/p01.jpg','m1',10.0,'sha',1,'similar',1,1,0.9,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(2,'derivatives/p02.jpg','m2',20.0,'sha',1,'similar',2,0,0.4,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(6,'derivatives/p06.jpg','m6',60.0,'sha',2,'exact',1,1,0.8,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(7,'derivatives/p07.jpg','m7',70.0,'sha',2,'exact',2,0,0.7,'[]','[]','test','2025-06-10T00:00:00');
INSERT INTO "photo_quality" VALUES(8,'derivatives/p08.jpg','stale',80.0,'sha',3,'exact',1,0,0.5,'[]','[]','test','2025-06-10T00:00:00');
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
INSERT INTO "photo_quality_features" VALUES(1,'derivatives/p01.jpg','m1',10.0,'sha-a',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(2,'derivatives/p02.jpg','m2',20.0,'sha-a',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(6,'derivatives/p06.jpg','m6',60.0,'sha-b',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(7,'derivatives/p07.jpg','m7',70.0,'sha-c',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(8,'derivatives/p08.jpg','stale',80.0,'sha-e',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(9,'derivatives/p09.jpg','m9',90.0,'sha-f',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'failed','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(10,'derivatives/p10.jpg','m10',100.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
INSERT INTO "photo_quality_features" VALUES(16,'derivatives/p16.jpg','m16',160.0,'sha-d',NULL,NULL,NULL,NULL,NULL,NULL,NULL,'','2025-06-10T00:00:00');
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
INSERT INTO "photos" VALUES(1,'local',NULL,'local/p01.jpg','derivatives/p01.jpg','IMG_0001.jpg','m1',10.0,'2025-06-01 10:00',35.1,139.1,'exif',NULL,NULL,'サンプル町',NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',1,0,90);
INSERT INTO "photos" VALUES(2,'drive','drive-2',NULL,'derivatives/p02.jpg','IMG_0002.jpg','m2',20.0,'2025-06-01 12:00',35.2,139.2,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,NULL);
INSERT INTO "photos" VALUES(3,'drive','drive-3',NULL,'derivatives/p03.jpg','IMG_0003.jpg','m3',30.0,'2025-06-03 09:00',35.3,139.3,'drive_meta',NULL,NULL,'公園前',NULL,NULL,NULL,NULL,NULL,NULL,'架空県','北見本',0,1,0);
INSERT INTO "photos" VALUES(4,'local',NULL,'local/p04.jpg','derivatives/p04.jpg','IMG_0004.jpg','m4',40.0,'2025-06-10 15:30',35.4,139.4,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(5,'drive','d-orphan',NULL,NULL,'IMG_0005.jpg','m5',50.0,'2025-06-20 08:00',35.5,139.5,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,270);
INSERT INTO "photos" VALUES(6,'drive','drive-6',NULL,'derivatives/p06.jpg','IMG_0006.jpg','m6',60.0,'2025-06-01 10:10',35.1,139.1,'inferred',NULL,600,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(7,'drive','drive-7',NULL,'derivatives/p07.jpg','IMG_0007.jpg','m7',70.0,'2025-06-01 10:20',35.1,139.1,'inferred',NULL,900,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(8,'drive','drive-8',NULL,'derivatives/p08.jpg','IMG_0008.jpg','m8',80.0,'2025-06-01 11:00',35.1,139.1,'inferred',NULL,901,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(9,'drive','drive-9',NULL,'derivatives/p09.jpg','IMG_0009.jpg','m9',90.0,'2025-06-01 13:00',35.1,139.1,'inferred',NULL,7200,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(10,'drive','drive-10',NULL,'derivatives/p10.jpg','IMG_0010.jpg','m10',100.0,'2025-06-01 14:00',35.1,139.1,'inferred',NULL,7201,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(11,'drive','drive-11',NULL,'derivatives/p11.jpg','IMG_0011.jpg','m11',110.0,'2025-06-02 00:00',35.1,139.1,'inferred',NULL,43200,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(12,'drive','drive-12',NULL,'derivatives/p12.jpg','IMG_0012.jpg','m12',120.0,'2025-06-02 01:00',35.1,139.1,'inferred',NULL,43201,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(13,'drive','drive-13',NULL,'derivatives/p13.jpg','IMG_0013.jpg','m13',130.0,'2025-06-02 02:00',35.1,139.1,'inferred',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(14,'drive','drive-14',NULL,'derivatives/p14.jpg','IMG_0014.jpg','m14',140.0,'2025-06-04 10:00',35.6,139.6,'manual',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,1,0,180);
INSERT INTO "photos" VALUES(15,'drive','drive-15',NULL,'derivatives/p15.jpg','IMG_0015.jpg','m15',150.0,'',35.7,139.7,'manual',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,1,1,0);
INSERT INTO "photos" VALUES(16,'drive','drive-16',NULL,'derivatives/p16.jpg','IMG_0016.jpg','m16',160.0,'2025-06-01 10:05',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(17,'drive','d-has-deriv',NULL,NULL,'IMG_0017.jpg','m17',170.0,'2025-06-01 12:30',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(18,'drive','d-no-deriv',NULL,NULL,'IMG_0018.jpg','m18',180.0,'2025-06-03 09:30',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(19,'drive','drive-19',NULL,'derivatives/p19.jpg','IMG_0019.jpg','m19',190.0,'2025-07-30 10:00',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(20,'drive','drive-20',NULL,'derivatives/p20.jpg','IMG_0020.jpg','m20',200.0,'x',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(21,'drive','drive-21',NULL,'derivatives/p21.jpg','IMG_0021.jpg','m21',210.0,'',NULL,NULL,'',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(22,'drive','drive-22',NULL,'derivatives/p22.jpg','IMG_0022.jpg','m22',220.0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(23,'drive','drive-23',NULL,'derivatives/p23.jpg','IMG_0023.jpg','m23',230.0,'2025-06-01 10:30',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(24,'drive','drive-24',NULL,'derivatives/p24.jpg','IMG_0024.jpg','m24',240.0,'2025-06-01 10:40',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(25,'drive','drive-25',NULL,'derivatives/p25.jpg','IMG_0025.jpg','m25',250.0,'2025-06-01 10:50',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(26,'drive','drive-26',NULL,'derivatives/p26.jpg','IMG_0026.jpg','m26',260.0,'2025-06-02 03:00',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(27,'drive','drive-27',NULL,'derivatives/p27.jpg','IMG_0027.jpg','m27',270.0,'2025-06-01 23:30',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(28,'drive','drive-28',NULL,'derivatives/p28.jpg','IMG_0028.jpg','m28',280.0,'2025-06-02 00:30',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(29,'drive','drive-29',NULL,'derivatives/p29.jpg','IMG_0029.jpg','m29',290.0,'2025-06-03 00:10',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
INSERT INTO "photos" VALUES(30,'drive','drive-30',NULL,'derivatives/p30.jpg','IMG_0030.jpg','m30',300.0,'2025-06-03 22:00',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,0,0);
CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);
INSERT INTO "settings" VALUES('photo_loc_status','{"decision": "ready", "at": "2025-06-20T10:00:00+09:00", "extra": 1}');
CREATE INDEX idx_photos_time ON photos(taken_at);
CREATE INDEX idx_photo_quality_group ON photo_quality(group_kind, group_id);
CREATE INDEX idx_photo_quality_features_sha
  ON photo_quality_features(content_sha256);
CREATE INDEX idx_photo_ai_features_sha
  ON photo_ai_features(content_sha256);
CREATE INDEX idx_photo_ai_group_members_photo
  ON photo_ai_group_members(photo_id);
COMMIT;
