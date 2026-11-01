-- 選別（photo_curation）の試験用の合成データ（tests/curation_feed.rs）。実在の場所・人・端末とは無関係な作り物。
-- スキーマは photo-feed-catalog.sql（common::build_curation_catalog が行を消してから流す）。ここは photos と photo_quality の行だけ。
-- 人の記録（photo_curation）は試験が足す。世代（display・md5・mtime・内容の SHA）は photos と photo_quality で一致している。
--
--   exact   グループ 1: 写真 1（推奨）・2・3。3 は除外（hidden=1）。内容は 3 枚とも同じ SHA。
--   similar グループ 2: 写真 4（推奨）・5（推奨）・6。6 は位置なし。
--   similar グループ 3: 写真 7（推奨）・8・9（推奨）。7 と 8 は同じ内容の SHA（photo_id が別）。
--   グループ外: 写真 10（普通）・11（位置なし）・12（除外）。
INSERT INTO photos(id, source, name, display, md5, mtime, taken_at, lat, lon, place, pref, city, favorite, hidden) VALUES
  (1, 'local', 'IMG_C01.jpg', 'derivatives/c01.jpg', 'md5-c01', 10.0, '2025-07-01 09:00', 35.1, 139.1, '試験区', '架空県', '試験市', 0, 0),
  (2, 'local', 'IMG_C02.jpg', 'derivatives/c02.jpg', 'md5-c02', 20.0, '2025-07-01 09:01', 35.1, 139.1, '試験区', '架空県', '試験市', 0, 0),
  (3, 'local', 'IMG_C03.jpg', 'derivatives/c03.jpg', 'md5-c03', 30.0, '2025-07-01 09:02', 35.1, 139.1, '試験区', '架空県', '試験市', 0, 1),
  (4, 'local', 'IMG_C04.jpg', 'derivatives/c04.jpg', 'md5-c04', 40.0, '2025-07-02 10:00', 35.2, 139.2, '試験区', '架空県', '試験市', 0, 0),
  (5, 'local', 'IMG_C05.jpg', 'derivatives/c05.jpg', 'md5-c05', 50.0, '2025-07-02 10:01', 35.2, 139.2, '試験区', '架空県', '試験市', 0, 0),
  (6, 'local', 'IMG_C06.jpg', 'derivatives/c06.jpg', 'md5-c06', 60.0, '2025-07-02 10:02', NULL, NULL, NULL, NULL, NULL, 0, 0),
  (7, 'local', 'IMG_C07.jpg', 'derivatives/c07.jpg', 'md5-c07', 70.0, '2025-07-03 11:00', 35.3, 139.3, '試験区', '架空県', '試験市', 0, 0),
  (8, 'local', 'IMG_C08.jpg', 'derivatives/c08.jpg', 'md5-c08', 80.0, '2025-07-03 11:01', 35.3, 139.3, '試験区', '架空県', '試験市', 0, 0),
  (9, 'local', 'IMG_C09.jpg', 'derivatives/c09.jpg', 'md5-c09', 90.0, '2025-07-03 11:02', 35.3, 139.3, '試験区', '架空県', '試験市', 0, 0),
  (10, 'local', 'IMG_C10.jpg', 'derivatives/c10.jpg', 'md5-c10', 100.0, '2025-07-04 12:00', 35.4, 139.4, '試験区', '架空県', '試験市', 0, 0),
  (11, 'local', 'IMG_C11.jpg', 'derivatives/c11.jpg', 'md5-c11', 110.0, '2025-07-04 12:01', NULL, NULL, NULL, NULL, NULL, 0, 0),
  (12, 'local', 'IMG_C12.jpg', 'derivatives/c12.jpg', 'md5-c12', 120.0, '2025-07-04 12:02', 35.4, 139.4, '試験区', '架空県', '試験市', 0, 1);
INSERT INTO photo_quality(photo_id, display, source_md5, source_mtime, content_sha256, group_id, group_kind, rank_in_group, recommended, score, algorithm, analyzed_at) VALUES
  (1, 'derivatives/c01.jpg', 'md5-c01', 10.0, 'sha-e', 1, 'exact', 1, 1, 87.5, 't', '2025-07-10T00:00:00'),
  (2, 'derivatives/c02.jpg', 'md5-c02', 20.0, 'sha-e', 1, 'exact', 2, 0, 84.5, 't', '2025-07-10T00:00:00'),
  (3, 'derivatives/c03.jpg', 'md5-c03', 30.0, 'sha-e', 1, 'exact', 3, 0, 81.5, 't', '2025-07-10T00:00:00'),
  (4, 'derivatives/c04.jpg', 'md5-c04', 40.0, 'sha-04', 2, 'similar', 1, 1, 78.5, 't', '2025-07-10T00:00:00'),
  (5, 'derivatives/c05.jpg', 'md5-c05', 50.0, 'sha-05', 2, 'similar', 2, 1, 75.5, 't', '2025-07-10T00:00:00'),
  (6, 'derivatives/c06.jpg', 'md5-c06', 60.0, 'sha-06', 2, 'similar', 3, 0, 72.5, 't', '2025-07-10T00:00:00'),
  (7, 'derivatives/c07.jpg', 'md5-c07', 70.0, 'sha-s3dup', 3, 'similar', 1, 1, 69.5, 't', '2025-07-10T00:00:00'),
  (8, 'derivatives/c08.jpg', 'md5-c08', 80.0, 'sha-s3dup', 3, 'similar', 2, 0, 66.5, 't', '2025-07-10T00:00:00'),
  (9, 'derivatives/c09.jpg', 'md5-c09', 90.0, 'sha-09', 3, 'similar', 3, 1, 63.5, 't', '2025-07-10T00:00:00');
