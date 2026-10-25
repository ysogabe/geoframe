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
INSERT INTO "photos" VALUES(1,'drive','drive-1',NULL,'derivatives/p001.jpg','IMG_0001.jpg','m1',10.0,'2025-06-06 08:59',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(2,'drive','drive-2',NULL,'derivatives/p002.jpg','IMG_0002.jpg','m2',20.0,'2025-06-06 09:00',35.1,139.1,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(3,'drive','drive-3',NULL,'derivatives/p003.jpg','IMG_0003.jpg','m3',30.0,'2025-06-06 09:00',35.1,139.1,'inferred',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(4,'drive','drive-4',NULL,'derivatives/p004.jpg','IMG_0004.jpg','m4',40.0,'2025-06-06 09:00',35.1,139.1,'drive_meta',NULL,NULL,NULL,NULL,NULL,'説明つき',NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(5,'drive','drive-5',NULL,'derivatives/p005.jpg','IMG_0005.jpg','m5',50.0,'2025-06-06 09:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,'題つき',NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(6,'drive','drive-6',NULL,'derivatives/p006.jpg','IMG_0006.jpg','m6',60.0,'2025-06-06 09:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,'題つき',NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(7,'drive','drive-7',NULL,'derivatives/p007.jpg','IMG_0007.jpg','m7',70.0,'2025-06-06 09:00',35.1,139.1,'manual',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,1,0,0);
INSERT INTO "photos" VALUES(8,'drive','drive-8',NULL,'derivatives/p008.jpg','IMG_0008.jpg','m8',80.0,'2025-06-06 09:01',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,1,0);
INSERT INTO "photos" VALUES(9,'drive','drive-9',NULL,NULL,'IMG_0009.jpg','m9',90.0,'2025-06-06 09:01',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(10,'drive','drive-10',NULL,'','IMG_0010.jpg','m10',100.0,'2025-06-06 09:01',35.1,139.1,'',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(11,'drive','drive-11',NULL,'derivatives/p011.jpg','IMG_0011.jpg','m11',110.0,'2025-06-06 12:30',35.1,139.1,'inferred',NULL,NULL,'サンプル町',NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(12,'drive','drive-12',NULL,'derivatives/p012.jpg','IMG_0012.jpg','m12',120.0,'2025-06-06 12:30',35.1,139.1,'exif',NULL,NULL,'同名',NULL,NULL,NULL,NULL,NULL,NULL,'架空県','同名',0,0,0);
INSERT INTO "photos" VALUES(13,'drive','drive-13',NULL,'derivatives/p013.jpg','IMG_0013.jpg','m13',130.0,'2025-06-06 17:59',35.1,139.1,'exif',NULL,NULL,'場所だけ',NULL,NULL,NULL,NULL,NULL,NULL,'架空県','',0,NULL,0);
INSERT INTO "photos" VALUES(14,'drive','drive-14',NULL,'derivatives/p014.jpg','IMG_0014.jpg','m14',140.0,'2025-06-06 18:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',' ',0,0,0);
INSERT INTO "photos" VALUES(15,'drive','drive-15',NULL,'derivatives/p015.jpg','IMG_0015.jpg','m15',150.0,'2025-06-06 18:01',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(16,'drive','drive-16',NULL,'derivatives/p016.jpg','IMG_0016.jpg','m16',160.0,'2025-06-06 18:00:30',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(17,'drive','drive-17',NULL,'\derivatives\win.jpg','IMG_0017.jpg','m17',170.0,'2025-06-06 10:10',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(18,'drive','drive-18',NULL,'../escape.jpg','IMG_0018.jpg','m18',180.0,'2025-06-06 10:11',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(19,'drive','drive-19',NULL,'.hidden.jpg','IMG_0019.jpg','m19',190.0,'2025-06-06 10:12',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(20,'drive','drive-20',NULL,'a/../b.jpg','IMG_0020.jpg','m20',200.0,'2025-06-06 10:13',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(21,'drive','drive-21',NULL,'/lead/slash.jpg','IMG_0021.jpg','m21',210.0,'2025-06-06 10:14',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(22,'drive','drive-22',NULL,'a/./b.jpg','IMG_0022.jpg','m22',220.0,'2025-06-06 10:15',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(23,'drive','drive-23',NULL,'derivatives/p023.jpg','IMG_0023.jpg','m23',230.0,'2025-06-06 10:16',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(30,'drive','drive-30',NULL,'derivatives/p030.jpg','IMG_0030.jpg','m30',300.0,'2025-06-07 00:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(31,'drive','drive-31',NULL,'derivatives/p031.jpg','IMG_0031.jpg','m31',310.0,'2025-06-07 23:59',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,1,0,0);
INSERT INTO "photos" VALUES(32,'drive','drive-32',NULL,'derivatives/p032.jpg','IMG_0032.jpg','m32',320.0,'2025-06-08 00:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(40,'drive','drive-40',NULL,'derivatives/p040.jpg','IMG_0040.jpg','m40',400.0,'2025-06-09 09:30',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(41,'drive','drive-41',NULL,'derivatives/p041.jpg','IMG_0041.jpg','m41',410.0,'2025-06-09 12:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(42,'drive','drive-42',NULL,'derivatives/p042.jpg','IMG_0042.jpg','m42',420.0,'2025-06-09 9:30',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(50,'drive','drive-50',NULL,'derivatives/p050.jpg','IMG_0050.jpg','m50',500.0,'2025-06-10 21:59',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(51,'drive','drive-51',NULL,'derivatives/p051.jpg','IMG_0051.jpg','m51',510.0,'2025-06-10 22:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(52,'drive','drive-52',NULL,'derivatives/p052.jpg','IMG_0052.jpg','m52',520.0,'2025-06-10 23:59',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(53,'drive','drive-53',NULL,'derivatives/p053.jpg','IMG_0053.jpg','m53',530.0,'2025-06-10 23:59:30',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(54,'drive','drive-54',NULL,'derivatives/p054.jpg','IMG_0054.jpg','m54',540.0,'2025-06-11 00:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(55,'drive','drive-55',NULL,'derivatives/p055.jpg','IMG_0055.jpg','m55',550.0,'2025-06-11 01:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(56,'drive','drive-56',NULL,'derivatives/p056.jpg','IMG_0056.jpg','m56',560.0,'2025-06-11 01:01',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(60,'drive','drive-60',NULL,'derivatives/p060.jpg','IMG_0060.jpg','m60',600.0,NULL,35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(61,'drive','drive-61',NULL,'derivatives/p061.jpg','IMG_0061.jpg','m61',610.0,'',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(100,'drive','drive-100',NULL,'derivatives/p100.jpg','IMG_0100.jpg','m100',1000.0,'2025-06-12 10:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(101,'drive','drive-101',NULL,'derivatives/p101.jpg','IMG_0101.jpg','m101',1010.0,'2025-06-12 10:01',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(102,'drive','drive-102',NULL,'derivatives/p102.jpg','IMG_0102.jpg','m102',1020.0,'2025-06-12 10:02',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(103,'drive','drive-103',NULL,'derivatives/p103.jpg','IMG_0103.jpg','m103',1030.0,'2025-06-12 10:03',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(104,'drive','drive-104',NULL,'derivatives/p104.jpg','IMG_0104.jpg','m104',1040.0,'2025-06-12 10:04',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(105,'drive','drive-105',NULL,'derivatives/p105.jpg','IMG_0105.jpg','m105',1050.0,'2025-06-12 10:05',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(106,'drive','drive-106',NULL,'derivatives/p106.jpg','IMG_0106.jpg','m106',1060.0,'2025-06-12 10:06',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(107,'drive','drive-107',NULL,'derivatives/p107.jpg','IMG_0107.jpg','m107',1070.0,'2025-06-12 10:07',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(108,'drive','drive-108',NULL,'derivatives/p108.jpg','IMG_0108.jpg','m108',1080.0,'2025-06-12 10:08',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(109,'drive','drive-109',NULL,'derivatives/p109.jpg','IMG_0109.jpg','m109',1090.0,'2025-06-12 10:09',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(110,'drive','drive-110',NULL,'derivatives/p110.jpg','IMG_0110.jpg','m110',1100.0,'2025-06-12 10:10',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(111,'drive','drive-111',NULL,'derivatives/p111.jpg','IMG_0111.jpg','m111',1110.0,'2025-06-12 10:11',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(112,'drive','drive-112',NULL,'derivatives/p112.jpg','IMG_0112.jpg','m112',1120.0,'2025-06-12 10:12',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(113,'drive','drive-113',NULL,'derivatives/p113.jpg','IMG_0113.jpg','m113',1130.0,'2025-06-12 10:13',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(114,'drive','drive-114',NULL,'derivatives/p114.jpg','IMG_0114.jpg','m114',1140.0,'2025-06-12 10:14',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(115,'drive','drive-115',NULL,'derivatives/p115.jpg','IMG_0115.jpg','m115',1150.0,'2025-06-12 10:15',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(116,'drive','drive-116',NULL,'derivatives/p116.jpg','IMG_0116.jpg','m116',1160.0,'2025-06-12 10:16',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(117,'drive','drive-117',NULL,'derivatives/p117.jpg','IMG_0117.jpg','m117',1170.0,'2025-06-12 10:17',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(118,'drive','drive-118',NULL,'derivatives/p118.jpg','IMG_0118.jpg','m118',1180.0,'2025-06-12 10:18',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(119,'drive','drive-119',NULL,'derivatives/p119.jpg','IMG_0119.jpg','m119',1190.0,'2025-06-12 10:19',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(120,'drive','drive-120',NULL,'derivatives/p120.jpg','IMG_0120.jpg','m120',1200.0,'2025-06-12 10:20',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',1,0,0);
INSERT INTO "photos" VALUES(121,'drive','drive-121',NULL,'derivatives/p121.jpg','IMG_0121.jpg','m121',1210.0,'2025-06-12 10:21',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(122,'drive','drive-122',NULL,'derivatives/p122.jpg','IMG_0122.jpg','m122',1220.0,'2025-06-12 10:22',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(123,'drive','drive-123',NULL,'derivatives/p123.jpg','IMG_0123.jpg','m123',1230.0,'2025-06-12 10:23',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(124,'drive','drive-124',NULL,'derivatives/p124.jpg','IMG_0124.jpg','m124',1240.0,'2025-06-12 10:24',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(125,'drive','drive-125',NULL,'derivatives/p125.jpg','IMG_0125.jpg','m125',1250.0,'2025-06-12 10:25',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(126,'drive','drive-126',NULL,'derivatives/p126.jpg','IMG_0126.jpg','m126',1260.0,'2025-06-12 10:26',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(127,'drive','drive-127',NULL,'derivatives/p127.jpg','IMG_0127.jpg','m127',1270.0,'2025-06-12 10:27',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(128,'drive','drive-128',NULL,'derivatives/p128.jpg','IMG_0128.jpg','m128',1280.0,'2025-06-12 10:28',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(129,'drive','drive-129',NULL,'derivatives/p129.jpg','IMG_0129.jpg','m129',1290.0,'2025-06-12 10:29',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(130,'drive','drive-130',NULL,'derivatives/p130.jpg','IMG_0130.jpg','m130',1300.0,'2025-06-12 10:30',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(131,'drive','drive-131',NULL,'derivatives/p131.jpg','IMG_0131.jpg','m131',1310.0,'2025-06-12 10:31',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(132,'drive','drive-132',NULL,'derivatives/p132.jpg','IMG_0132.jpg','m132',1320.0,'2025-06-12 10:32',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(133,'drive','drive-133',NULL,'derivatives/p133.jpg','IMG_0133.jpg','m133',1330.0,'2025-06-12 10:33',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(134,'drive','drive-134',NULL,'derivatives/p134.jpg','IMG_0134.jpg','m134',1340.0,'2025-06-12 10:34',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(135,'drive','drive-135',NULL,'derivatives/p135.jpg','IMG_0135.jpg','m135',1350.0,'2025-06-12 10:35',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(136,'drive','drive-136',NULL,'derivatives/p136.jpg','IMG_0136.jpg','m136',1360.0,'2025-06-12 10:36',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(137,'drive','drive-137',NULL,'derivatives/p137.jpg','IMG_0137.jpg','m137',1370.0,'2025-06-12 10:37',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(138,'drive','drive-138',NULL,'derivatives/p138.jpg','IMG_0138.jpg','m138',1380.0,'2025-06-12 10:38',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(139,'drive','drive-139',NULL,'derivatives/p139.jpg','IMG_0139.jpg','m139',1390.0,'2025-06-12 10:39',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(140,'drive','drive-140',NULL,'derivatives/p140.jpg','IMG_0140.jpg','m140',1400.0,'2025-06-12 10:40',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県','見本市',0,0,0);
INSERT INTO "photos" VALUES(141,'drive','drive-141',NULL,'derivatives/p141.jpg','IMG_0141.jpg','m141',1410.0,'2025-06-12 10:41',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(142,'drive','drive-142',NULL,'derivatives/p142.jpg','IMG_0142.jpg','m142',1420.0,'2025-06-12 10:42',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(143,'drive','drive-143',NULL,'derivatives/p143.jpg','IMG_0143.jpg','m143',1430.0,'2025-06-12 10:43',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,1,0,0);
INSERT INTO "photos" VALUES(144,'drive','drive-144',NULL,'derivatives/p144.jpg','IMG_0144.jpg','m144',1440.0,'2025-06-12 10:44',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,1,0,0);
INSERT INTO "photos" VALUES(150,'drive','drive-150',NULL,'derivatives/p150.jpg','IMG_0150.jpg','m150',1500.0,'2025-06-12 10:44',35.1,139.1,'exif',NULL,NULL,NULL,NULL,'同じ分の別の写真',NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(200,'drive','drive-200',NULL,'derivatives/p200.jpg','IMG_0200.jpg','m200',2000.0,'2025-06-13 09:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
INSERT INTO "photos" VALUES(201,'drive','drive-201',NULL,'derivatives/p201.jpg','IMG_0201.jpg','m201',2010.0,'2025-06-13 10:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,1,0,0);
INSERT INTO "photos" VALUES(202,'drive','drive-202',NULL,'derivatives/p202.jpg','IMG_0202.jpg','m202',2020.0,'2025-06-13 11:00',35.1,139.1,'exif',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'架空県',NULL,0,0,0);
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
