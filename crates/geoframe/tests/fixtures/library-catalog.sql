-- ライブラリ API（tests/library_api.rs）の合成データ。実在の場所・人・端末とは無関係な作り物。
-- スキーマは photo-feed-catalog.sql（common::build_library_catalog が行を消してから流す）。ここは photos の行だけ。
--
-- 旅（ここでの hidden=0 の写真）。時刻差は直前の写真との差:
--   旅 A  id 1..10   2025-03-01 09:00 〜 2025-03-03 19:30。10 分（1→2）・1 時間 30 分（2→3）・8 時間（3→4）・
--                    ちょうど 48 時間（6→7: 03-01 19:00 → 03-03 19:00）はどれも同じ旅。位置なし 4・5・6。
--                    hidden=1 の id 11（03-04 08:00）は hidden=1 を付けたときだけ旅 A に入る。
--   旅 B  id 12・14  2025-03-12 19:30 〜 20:00。旅 A の終わりから 9 日（→ 別の旅）。先頭の id 12 は位置なし。
--                    hidden=1 の id 13（19:35）は位置があるが除外。
--   旅 C  id 15・16  2025-03-14 20:01 〜 20:02。旅 B の終わりからちょうど 48 時間 + 1 分（→ 別の旅）。全部位置なし。
--   旅 D  id 17・18  2025-05-01 12:00〜12:05。除外だけの旅（hidden=1 のときだけ現れる）。
--   旅 E  id 19・20  2025-06-01 08:00（同じ分。id 順）。
--   日時なし id 21（NULL）・22（空文字）・23（NULL。除外）。
INSERT INTO photos(id, source, drive_id, path, display, name, md5, mtime, taken_at, lat, lon, loc_source,
                   place, pref, city, favorite, hidden, rotation) VALUES
  (1, 'local', NULL, 'local/l01.jpg', 'derivatives/l01.jpg', 'IMG_L01.jpg', 'md5-l01', 10.0, '2025-03-01 09:00', 34.1, 135.1, 'exif', '試験区', '架空県', '試験市', 0, 0, 0),
  (2, 'drive', 'drive-l02', NULL, 'derivatives/l02.jpg', 'IMG_L02.jpg', 'md5-l02', 20.0, '2025-03-01 09:10', 34.2, 135.2, 'exif', '試験区', '架空県', '試験市', 1, 0, 0),
  (3, 'local', NULL, 'local/l03.jpg', 'derivatives/l03.jpg', 'IMG_L03.jpg', 'md5-l03', 30.0, '2025-03-01 10:40', 34.3, 135.3, 'exif', '試験区', '架空県', '試験市', 0, 0, 90),
  (4, 'drive', 'drive-l04', NULL, 'derivatives/l04.jpg', 'IMG_L04.jpg', 'md5-l04', 40.0, '2025-03-01 18:40', NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (5, 'local', NULL, 'local/l05.jpg', 'derivatives/l05.jpg', 'IMG_L05.jpg', 'md5-l05', 50.0, '2025-03-01 18:50', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (6, 'drive', 'drive-l06', NULL, 'derivatives/l06.jpg', 'IMG_L06.jpg', 'md5-l06', 60.0, '2025-03-01 19:00', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (7, 'local', NULL, 'local/l07.jpg', 'derivatives/l07.jpg', 'IMG_L07.jpg', 'md5-l07', 70.0, '2025-03-03 19:00', 34.4, 135.4, 'exif', '試験区', '架空県', '試験市', 0, 0, 0),
  (8, 'drive', 'drive-l08', NULL, 'derivatives/l08.jpg', 'IMG_L08.jpg', 'md5-l08', 80.0, '2025-03-03 19:10', 35.1, 137.1, 'exif', '試験区', '架空県', '見本町', 0, 0, 0),
  (9, 'local', NULL, 'local/l09.jpg', 'derivatives/l09.jpg', 'IMG_L09.jpg', 'md5-l09', 90.0, '2025-03-03 19:20', 35.2, 137.2, 'exif', '試験区', '架空県', '見本町', 0, 0, 0),
  (10, 'drive', 'drive-l10', NULL, 'derivatives/l10.jpg', 'IMG_L10.jpg', 'md5-l10', 100.0, '2025-03-03 19:30', 35.9, 139.1, 'exif', '試験区', '試験県', 'サンプル市', 0, 0, 0),
  (11, 'local', NULL, 'local/l11.jpg', 'derivatives/l11.jpg', 'IMG_L11.jpg', 'md5-l11', 110.0, '2025-03-04 08:00', 34.5, 135.5, 'exif', '試験区', '架空県', '試験市', 0, 1, 0),
  (12, 'drive', 'drive-l12', NULL, 'derivatives/l12.jpg', 'IMG_L12.jpg', 'md5-l12', 120.0, '2025-03-12 19:30', NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (13, 'local', NULL, 'local/l13.jpg', 'derivatives/l13.jpg', 'IMG_L13.jpg', 'md5-l13', 130.0, '2025-03-12 19:35', 34.95, 136.2, 'exif', '試験区', '架空県', '見本町', 0, 1, 0),
  (14, 'drive', 'drive-l14', NULL, 'derivatives/l14.jpg', 'IMG_L14.jpg', 'md5-l14', 140.0, '2025-03-12 20:00', 34.9, 136.1, 'exif', '試験区', '架空県', '試験市', 0, 0, 0),
  (15, 'drive', 'drive-l15', NULL, NULL, 'IMG_L15.jpg', 'md5-l15', 150.0, '2025-03-14 20:01', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (16, 'drive', 'drive-l16', NULL, 'derivatives/l16.jpg', 'IMG_L16.jpg', 'md5-l16', 160.0, '2025-03-14 20:02', NULL, NULL, NULL, NULL, '', '', 0, 0, 0),
  (17, 'local', NULL, 'local/l17.jpg', 'derivatives/l17.jpg', 'IMG_L17.jpg', 'md5-l17', 170.0, '2025-05-01 12:00', 34.6, 138.5, 'exif', '試験区', '試験県', 'サンプル市', 0, 1, 0),
  (18, 'drive', 'drive-l18', NULL, 'derivatives/l18.jpg', 'IMG_L18.jpg', 'md5-l18', 180.0, '2025-05-01 12:05', 34.7, 138.6, 'exif', '試験区', '試験県', 'サンプル市', 0, 1, 0),
  (19, 'local', NULL, 'local/l19.jpg', 'derivatives/l19.jpg', 'IMG_L19.jpg', 'md5-l19', 190.0, '2025-06-01 08:00', 35.5, 139.5, 'exif', '試験区', '試験県', 'サンプル市', 0, 0, 0),
  (20, 'drive', 'drive-l20', NULL, 'derivatives/l20.jpg', 'IMG_L20.jpg', 'md5-l20', 200.0, '2025-06-01 08:00', 35.6, 139.6, 'exif', '試験区', '試験県', 'サンプル市', 0, 0, 0),
  (21, 'local', NULL, 'local/l21.jpg', 'derivatives/l21.jpg', 'IMG_L21.jpg', 'md5-l21', 210.0, NULL, 35.0, 136.0, 'exif', '試験区', '架空県', '試験市', 0, 0, 0),
  (22, 'drive', 'drive-l22', NULL, NULL, 'IMG_L22.jpg', 'md5-l22', 220.0, '', NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, 0),
  (23, 'local', NULL, 'local/l23.jpg', 'derivatives/l23.jpg', 'IMG_L23.jpg', 'md5-l23', 230.0, NULL, 35.05, 136.05, 'exif', '試験区', '架空県', '見本町', 0, 1, 0);
