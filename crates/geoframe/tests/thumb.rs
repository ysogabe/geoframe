//! 一覧用サムネイル `GET /thumb/<id>.jpg?v=<gen>` の結合テスト（B の T1。AC1）。合成データだけを使う
//! （画像はコードから作る。本番のファイルを写さない）。ロック・枠・待ちの上限の細かい動きは
//! `src/thumb/tests.rs` の単体試験が見る。

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{Response, TempDir, build_empty_catalog, get, request, spawn_state};
use geoframe::db::Catalog;
use geoframe::imaging::{THUMB_LIMITS, THUMB_PX, make_thumbnail};
use geoframe::limits::STATIC_FILE_SLOTS;
use geoframe::thumb::{ThumbState, thumb_generation};
use geoframe::writer::{OpenMode, Writer};
use image::ImageEncoder;
use rusqlite::{Connection, params};
use tokio::sync::Semaphore;

// ---- 合成の画像 ----

/// 向き `orientation` と GPS（`GPSLatitudeRef`）を持つ最小の EXIF の APP1 セグメント。
fn exif_segment(orientation: u8) -> Vec<u8> {
    let mut tiff: Vec<u8> = b"MM\0*\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&[0, 2]);
    tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation, 0, 0]);
    tiff.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 38]);
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(tiff.len(), 38);
    tiff.extend_from_slice(&[0, 1]);
    tiff.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]);
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    segment.extend_from_slice(&payload);
    segment
}

/// 左が赤・右が青の RGB の JPEG。`orientation` が `Some` なら、向きと GPS を持つ EXIF（APP1）を付ける。
fn jpeg(w: u32, h: u32, orientation: Option<u8>) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..h {
        for x in 0..w {
            pixels.extend_from_slice(if x < w / 2 {
                &[255, 0, 0]
            } else {
                &[0, 0, 255]
            });
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(
            &pixels,
            u16::try_from(w).unwrap(),
            u16::try_from(h).unwrap(),
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
    match orientation {
        Some(o) => {
            let mut with = out[..2].to_vec();
            with.extend_from_slice(&exif_segment(o));
            with.extend_from_slice(&out[2..]);
            with
        }
        None => out,
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            &vec![200u8; (w * h * 3) as usize],
            w,
            h,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// 寸法だけ 10000x10000 に書き換えた PNG（画素は 4x4 分しか無い）。
fn png_with_huge_header() -> Vec<u8> {
    let mut data = png(4, 4);
    data[16..20].copy_from_slice(&10_000u32.to_be_bytes());
    data[20..24].copy_from_slice(&10_000u32.to_be_bytes());
    let crc = crc32(&data[12..29]);
    data[29..33].copy_from_slice(&crc.to_be_bytes());
    data
}

fn dims(jpeg: &[u8]) -> (usize, usize) {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let mut dec = JpegDecoder::new(ZCursor::new(jpeg));
    dec.decode_headers().unwrap();
    dec.dimensions().unwrap()
}

fn color_at(jpeg: &[u8], x: usize, y: usize) -> [u8; 3] {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let mut dec = JpegDecoder::new(ZCursor::new(jpeg));
    let pixels = dec.decode().unwrap();
    let (w, _) = dec.dimensions().unwrap();
    let i = (y * w + x) * 3;
    [pixels[i], pixels[i + 1], pixels[i + 2]]
}

/// APP1（`0xFFE1`）のセグメントの数（開始から SOS まで）。
fn app1_segments(jpeg: &[u8]) -> usize {
    let mut pos = 2;
    let mut count = 0;
    while pos + 4 <= jpeg.len() && jpeg[pos] == 0xFF && jpeg[pos + 1] != 0xDA {
        if jpeg[pos + 1] == 0xE1 {
            count += 1;
        }
        pos += 2 + usize::from(u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]));
    }
    count
}

// ---- サイト ----

fn generation(display: Option<&str>, md5: &str, mtime: f64) -> String {
    thumb_generation(display, Some(md5), Some(mtime)).hex()
}

struct Site {
    _tmp: TempDir,
    data: PathBuf,
    db: Connection,
    addr: std::net::SocketAddr,
    thumbs: Arc<ThumbState>,
    slots: Arc<Semaphore>,
}

impl Site {
    async fn start(writer: bool) -> Self {
        Self::start_with(writer, Arc::new(ThumbState::new())).await
    }

    async fn start_with(writer: bool, thumbs: Arc<ThumbState>) -> Self {
        let tmp = TempDir::new("thumb");
        let data = fs::canonicalize(tmp.path()).unwrap();
        let (path, db) = build_empty_catalog(&data, "");
        let mut catalog = Catalog::open(&path).unwrap();
        if writer {
            catalog = catalog.with_writer(Writer::open(&path, OpenMode::MustExist).unwrap());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let state_thumbs = Arc::clone(&thumbs);
        let addr = spawn_state(&data, &[], catalog, move |state| {
            tx.send(Arc::clone(&state.static_slots)).unwrap();
            state.with_thumbs(state_thumbs)
        })
        .await;
        Self {
            _tmp: tmp,
            data,
            db,
            addr,
            thumbs,
            slots: rx.recv().unwrap(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.data.join(rel)
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn add_photo(
        &self,
        id: i64,
        display: Option<&str>,
        drive_id: Option<&str>,
        md5: &str,
        mtime: f64,
    ) {
        self.db
            .execute(
                "INSERT INTO photos(id, source, drive_id, display, name, md5, mtime)
                 VALUES(?1, 'drive', ?2, ?3, 'n.jpg', ?4, ?5)",
                params![id, drive_id, display, md5, mtime],
            )
            .unwrap();
    }

    /// 写真 1 枚（`derivatives/p<id>.jpg` に 1200x800 の JPEG）を足し、世代を返す。
    fn add_standard(&self, id: i64, orientation: Option<u8>) -> String {
        let display = format!("derivatives/p{id}.jpg");
        self.put(&display, &jpeg(1200, 800, orientation));
        self.add_photo(id, Some(&display), None, "md5-a", 10.0);
        generation(Some(&display), "md5-a", 10.0)
    }

    async fn get(&self, id: i64, v: &str) -> Response {
        get(self.addr, &format!("/thumb/{id}.jpg?v={v}")).await
    }

    async fn send(&self, method: &str, target: &str, extra: &[(&str, &str)]) -> Response {
        let mut headers = vec![("Host", "localhost")];
        headers.extend_from_slice(extra);
        request(self.addr, method, target, &headers).await
    }

    fn thumbs_dir_exists(&self) -> bool {
        self.path("thumbs").exists()
    }

    fn thumb_files(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.path("thumbs")) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn counting_renderer(delay: Duration) -> Arc<ThumbState> {
    Arc::new(ThumbState::with_renderer(Arc::new(move |src: &Path| {
        std::thread::sleep(delay);
        make_thumbnail(src, THUMB_PX, &THUMB_LIMITS)
    })))
}

// ---- 生成・キャッシュ・ETag ----

#[tokio::test(flavor = "multi_thread")]
async fn generates_a_small_jpeg_without_exif_and_caches_it() {
    let site = Site::start(true).await;
    // 向き 6（右へ 90 度）と GPS を持つ 1200x800 → 縦長の 320 以下
    let v = site.add_standard(1, Some(6));
    let response = site.get(1, &v).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("image/jpeg"));
    let (w, h) = dims(&response.body);
    assert_eq!(h, 320);
    assert!((212..=214).contains(&w), "{w}");
    assert_eq!(
        response.header("content-length"),
        Some(response.body.len().to_string().as_str())
    );
    // 向きが当たっている（左の赤が上へ）
    assert!(color_at(&response.body, w / 2, 20)[0] > 170);
    assert!(color_at(&response.body, w / 2, h - 20)[2] > 170);
    // EXIF（GPS を含む）を持ち込まない
    assert_eq!(app1_segments(&response.body), 0);
    assert!(!response.body.windows(6).any(|w| w == b"Exif\0\0"));
    // ヘッダ
    assert_eq!(response.header("etag"), Some(format!("\"{v}\"").as_str()));
    assert_eq!(
        response.header("cache-control"),
        Some("private, max-age=604800")
    );
    assert_eq!(
        response.header("cross-origin-resource-policy"),
        Some("same-origin")
    );
    assert!(response.header("last-modified").is_some());
    // thumbs/<id>-<gen>.jpg ができる
    let cached = site.path(&format!("thumbs/1-{v}.jpg"));
    assert_eq!(fs::read(&cached).unwrap(), response.body);
    assert_eq!(site.thumb_files(), [format!("1-{v}.jpg")]);
    // 2 回目はファイルを返す（生成しない）
    let again = site.get(1, &v).await;
    assert_eq!(
        (again.status, again.body.as_slice()),
        (200, response.body.as_slice())
    );
    assert_eq!(site.thumbs.renders(), 1);
    // HEAD はキャッシュがあれば本文なしで返す
    let head = site.send("HEAD", &format!("/thumb/1.jpg?v={v}"), &[]).await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(
        head.header("content-length"),
        Some(response.body.len().to_string().as_str())
    );
    assert_eq!(head.header("etag"), response.header("etag"));
    assert_eq!(site.thumbs.renders(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_admin_photos_thumb_url_is_versioned_and_can_be_fetched() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    // 表示用画像の無い写真は、`thumb`・`thumb_url` とも空
    site.add_photo(2, None, None, "md5-b", 11.0);
    let (status, body) = {
        let response = get(site.addr, "/api/admin/photos?per=10").await;
        (response.status, response.json())
    };
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().unwrap();
    let find = |id: i64| items.iter().find(|i| i["id"] == id).unwrap();
    let url = find(1)["thumb_url"].as_str().unwrap();
    assert_eq!(url, format!("/thumb/1.jpg?v={v}"));
    let hex = url.rsplit("?v=").next().unwrap();
    assert!(hex.len() == 12 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    assert_eq!(find(1)["thumb"], "/derivatives/p1.jpg");
    assert_eq!(find(2)["thumb"], "");
    assert_eq!(find(2)["thumb_url"], "");
    // その URL を GET すると 200 でサムネイルができる
    let response = get(site.addr, url).await;
    assert_eq!(response.status, 200);
    assert_eq!(site.thumb_files(), [format!("1-{v}.jpg")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_matching_if_none_match_gets_304_with_the_validators() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    let first = site.get(1, &v).await;
    assert_eq!(first.status, 200);
    let etag = format!("\"{v}\"");
    for header in [
        etag.clone(),
        format!("W/{etag}"),
        format!("\"x\", {etag}"),
        "*".into(),
    ] {
        let response = site
            .send(
                "GET",
                &format!("/thumb/1.jpg?v={v}"),
                &[("If-None-Match", &header)],
            )
            .await;
        assert_eq!(response.status, 304, "{header}");
        assert!(response.body.is_empty());
        assert_eq!(response.header("etag"), Some(etag.as_str()));
        assert_eq!(
            response.header("cache-control"),
            Some("private, max-age=604800")
        );
    }
    let other = site
        .send(
            "GET",
            &format!("/thumb/1.jpg?v={v}"),
            &[("If-None-Match", "\"another\"")],
        )
        .await;
    assert_eq!(other.status, 200);
    assert_eq!(site.thumbs.renders(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_display_or_mtime_makes_a_new_generation_and_removes_the_old_file() {
    let site = Site::start(true).await;
    let v1 = site.add_standard(1, None);
    let first = site.get(1, &v1).await;
    assert_eq!(first.status, 200);
    assert_eq!(site.thumb_files(), [format!("1-{v1}.jpg")]);
    // 別の写真のファイル（消してはいけない）
    let v2 = site.add_standard(2, None);
    assert_eq!(site.get(2, &v2).await.status, 200);

    // 原本が更新された（mtime が変わる）: 古い版では作らない。新しい版で作り直し、古いファイルが消える
    site.db
        .execute("UPDATE photos SET mtime=11.5 WHERE id=1", [])
        .unwrap();
    let v1b = generation(Some("derivatives/p1.jpg"), "md5-a", 11.5);
    assert_ne!(v1, v1b);
    // 古い版の URL は、今の世代のキャッシュが無いので 404（生成しない）
    assert_eq!(site.get(1, &v1).await.status, 404);
    assert_eq!(site.thumbs.renders(), 2);
    let second = site.get(1, &v1b).await;
    assert_eq!(second.status, 200);
    assert_eq!(second.header("etag"), Some(format!("\"{v1b}\"").as_str()));
    assert_eq!(site.thumbs.renders(), 3);
    assert_eq!(
        site.thumb_files(),
        [format!("1-{v1b}.jpg"), format!("2-{v2}.jpg")]
    );

    // 回転された（display が別のファイルになる）: 同じく作り直して古いファイルを消す
    site.put("derivatives/p1.r90.jpg", &jpeg(800, 1200, None));
    site.db
        .execute(
            "UPDATE photos SET display='derivatives/p1.r90.jpg' WHERE id=1",
            [],
        )
        .unwrap();
    let v1c = generation(Some("derivatives/p1.r90.jpg"), "md5-a", 11.5);
    let third = site.get(1, &v1c).await;
    assert_eq!(third.status, 200);
    assert_eq!(dims(&third.body).1, 320);
    assert_eq!(
        site.thumb_files(),
        [format!("1-{v1c}.jpg"), format!("2-{v2}.jpg")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_v_never_generates_but_a_cached_current_generation_is_returned() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    let wrong = if v.starts_with('0') {
        "111111111111"
    } else {
        "000000000000"
    };
    let response = site.get(1, wrong).await;
    assert_eq!(response.status, 404);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());
    // 今の世代のキャッシュができた後は、古い v でもそれを返す
    assert_eq!(site.get(1, &v).await.status, 200);
    let stale = site.get(1, wrong).await;
    assert_eq!(stale.status, 200);
    assert_eq!(stale.header("etag"), Some(format!("\"{v}\"").as_str()));
    assert_eq!(site.thumbs.renders(), 1);
}

// ---- 経路の検証 ----

#[tokio::test(flavor = "multi_thread")]
async fn only_plain_numeric_ids_with_a_valid_v_are_accepted() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    let ok = v.clone();
    for target in [
        format!("/thumb/abc.jpg?v={ok}"),
        format!("/thumb/0.jpg?v={ok}"),
        format!("/thumb/01.jpg?v={ok}"),
        format!("/thumb/1.png?v={ok}"),
        format!("/thumb/1?v={ok}"),
        format!("/thumb/1.jpg/?v={ok}"),
        format!("/thumb/a/1.jpg?v={ok}"),
        format!("/thumb/1/1.jpg?v={ok}"),
        format!("/thumb/1.jpg/x?v={ok}"),
        format!("/thumb/-1.jpg?v={ok}"),
        format!("/thumb/1234567890123456789.jpg?v={ok}"),
        format!("/thumb/99999999999999999.jpg?v={ok}"),
        "/thumb/".to_string(),
        "/thumb".to_string(),
        "/thumb/1.jpg".to_string(),
        "/thumb/1.jpg?v=".to_string(),
        "/thumb/1.jpg?v=ABCDEF012345".to_string(),
        format!("/thumb/1.jpg?v={}", &ok[..11]),
        format!("/thumb/1.jpg?v={ok}0"),
        "/thumb/1.jpg?w=0123456789ab".to_string(),
    ] {
        let response = site.send("GET", &target, &[]).await;
        assert_eq!(response.status, 404, "{target}");
    }
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());
    // 正しい形は通る（`%31` は正規化で `1`）
    assert_eq!(
        site.send("GET", &format!("/thumb/%31.jpg?v={v}"), &[])
            .await
            .status,
        200
    );
    assert_eq!(
        site.send("GET", &format!("//thumb/1.jpg?v={v}"), &[])
            .await
            .status,
        200
    );
    // 範囲外の ID を呑み込んで別の写真を返さない
    assert_eq!(site.get(2, &v).await.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn photos_without_a_usable_source_are_404() {
    let site = Site::start(true).await;
    let v = "0123456789ab";
    // 表示用画像の無い写真（display も drive_id も無い）
    site.add_photo(1, None, None, "m", 1.0);
    assert_eq!(site.get(1, &generation(None, "m", 1.0)).await.status, 404);
    // display はあるがファイルが無い
    site.add_photo(2, Some("derivatives/missing.jpg"), None, "m", 1.0);
    assert_eq!(
        site.get(2, &generation(Some("derivatives/missing.jpg"), "m", 1.0))
            .await
            .status,
        404
    );
    // ディレクトリ
    fs::create_dir_all(site.path("derivatives/dir.jpg")).unwrap();
    site.add_photo(3, Some("derivatives/dir.jpg"), None, "m", 1.0);
    assert_eq!(
        site.get(3, &generation(Some("derivatives/dir.jpg"), "m", 1.0))
            .await
            .status,
        403
    );
    assert_eq!(site.get(404, v).await.status, 404);
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn display_less_drive_photos_use_their_derivative_when_the_id_is_safe() {
    let site = Site::start(true).await;
    site.put("derivatives/drv_1-a.jpg", &jpeg(640, 480, None));
    site.add_photo(1, None, Some("drv_1-a"), "m", 1.0);
    let v = generation(None, "m", 1.0);
    let response = site.get(1, &v).await;
    assert_eq!(response.status, 200);
    assert_eq!(dims(&response.body).0, 320);
    // 区切りを含む drive_id は使わない
    site.put("derivatives/x.jpg", &jpeg(64, 48, None));
    site.add_photo(2, None, Some("../derivatives/x"), "m", 1.0);
    assert_eq!(site.get(2, &generation(None, "m", 1.0)).await.status, 404);
    assert_eq!(site.thumbs.renders(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_display_that_points_outside_the_areas_is_404_and_never_returns_the_database() {
    let site = Site::start(true).await;
    site.put("tmp/x", b"not-a-db");
    let cases = [
        "catalog.db",
        "../x",
        "tmp/x",
        "derivatives/../catalog.db",
        "/etc/passwd",
        "thumbs/1-0123456789ab.jpg",
        "blog_exports/a.jpg",
        "derivatives/.hidden.jpg",
    ];
    for (n, display) in cases.iter().enumerate() {
        let id = n as i64 + 1;
        site.add_photo(id, Some(display), None, "m", 1.0);
        let v = generation(Some(display), "m", 1.0);
        for target in [format!("/thumb/{id}.jpg?v={v}")] {
            let response = site.send("GET", &target, &[]).await;
            assert_eq!(response.status, 404, "{display}");
            assert!(
                !response.body.starts_with(b"SQLite format 3"),
                "{display}: DB を返した"
            );
            assert!(!response.body.starts_with(b"not-a-db"), "{display}");
        }
    }
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn links_that_leave_the_area_are_403() {
    let site = Site::start(true).await;
    let outside = TempDir::new("thumb-outside");
    fs::write(outside.path().join("secret.jpg"), jpeg(64, 48, None)).unwrap();
    // 領域の外（外部）を指すリンク
    fs::create_dir_all(site.path("derivatives")).unwrap();
    symlink(
        outside.path().join("secret.jpg"),
        site.path("derivatives/link.jpg"),
    )
    .unwrap();
    // データ置き場の中でも領域の外（catalog.db）を指すリンク
    symlink("../catalog.db", site.path("derivatives/tocat.jpg")).unwrap();
    // 外へ出るディレクトリのリンクの先
    symlink(outside.path(), site.path("derivatives/dirlink")).unwrap();
    for (id, display) in [
        (1, "derivatives/link.jpg"),
        (2, "derivatives/tocat.jpg"),
        (3, "derivatives/dirlink/secret.jpg"),
    ] {
        site.add_photo(id, Some(display), None, "m", 1.0);
        let response = site.get(id, &generation(Some(display), "m", 1.0)).await;
        assert_eq!(response.status, 403, "{display}");
        assert!(!response.body.starts_with(b"SQLite format 3"));
    }
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());

    // キャッシュのファイルが外を指すリンクでも返さない
    let v = site.add_standard(4, None);
    fs::create_dir_all(site.path("thumbs")).unwrap();
    symlink(
        outside.path().join("secret.jpg"),
        site.path(&format!("thumbs/4-{v}.jpg")),
    )
    .unwrap();
    assert_eq!(site.get(4, &v).await.status, 403);
    assert_eq!(site.thumbs.renders(), 0);
    assert_eq!(
        fs::read(outside.path().join("secret.jpg")).unwrap(),
        jpeg(64, 48, None)
    );
}

// ---- 失敗 ----

#[tokio::test(flavor = "multi_thread")]
async fn a_broken_jpeg_is_404_and_is_not_decoded_a_second_time() {
    let site = Site::start(true).await;
    let good = jpeg(600, 400, None);
    site.put("derivatives/broken.jpg", &good[..good.len() / 2]);
    site.add_photo(1, Some("derivatives/broken.jpg"), None, "m", 1.0);
    let v = generation(Some("derivatives/broken.jpg"), "m", 1.0);
    for _ in 0..3 {
        let response = site.get(1, &v).await;
        assert_eq!(response.status, 404);
        assert_eq!(response.header("cache-control"), Some("no-store"));
    }
    assert_eq!(site.thumbs.renders(), 1, "2 回目以降は復号しない");
    assert!(!site.thumbs_dir_exists());
    // 写真が変われば（世代が変われば）もう一度試す
    site.db
        .execute("UPDATE photos SET mtime=2.0 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        site.get(1, &generation(Some("derivatives/broken.jpg"), "m", 2.0))
            .await
            .status,
        404
    );
    assert_eq!(site.thumbs.renders(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_inputs_are_404_without_a_full_decode() {
    let site = Site::start(true).await;
    // 寸法だけ大きいヘッダの PNG（本体は 4x4 分しか無い）
    site.put("derivatives/huge.png", &png_with_huge_header());
    site.add_photo(1, Some("derivatives/huge.png"), None, "m", 1.0);
    // ファイルが大きすぎる（疎なファイル。読まずに断る）
    fs::create_dir_all(site.path("derivatives")).unwrap();
    fs::File::create(site.path("derivatives/big.jpg"))
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    site.add_photo(2, Some("derivatives/big.jpg"), None, "m", 1.0);
    for (id, display) in [(1, "derivatives/huge.png"), (2, "derivatives/big.jpg")] {
        let v = generation(Some(display), "m", 1.0);
        for _ in 0..2 {
            let response = site.get(id, &v).await;
            assert_eq!(response.status, 404, "{display}");
            assert_eq!(response.header("cache-control"), Some("no-store"));
        }
    }
    assert_eq!(site.thumbs.renders(), 2, "それぞれ 1 回だけ（記憶される）");
    assert!(!site.thumbs_dir_exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_writer_nothing_is_generated_and_the_original_is_returned() {
    let site = Site::start(false).await;
    let v = site.add_standard(1, None);
    let original = fs::read(site.path("derivatives/p1.jpg")).unwrap();
    let response = site.get(1, &v).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("image/jpeg"));
    assert_eq!(response.body, original);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert!(response.header("etag").is_none());
    assert!(response.header("last-modified").is_none());
    // 条件付き要求でも 304 にしない
    let conditional = site
        .send(
            "GET",
            &format!("/thumb/1.jpg?v={v}"),
            &[
                ("If-None-Match", &format!("\"{v}\"")),
                ("If-Modified-Since", "Sun, 01 Jun 2099 00:00:00 GMT"),
            ],
        )
        .await;
    assert_eq!(conditional.status, 200);
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists(), "thumbs/ も作らない");

    // PNG の原本は PNG として返す。拡張子が jpg/jpeg/png でないものは返さない
    site.put("derivatives/q.png", &png(40, 30));
    site.add_photo(2, Some("derivatives/q.png"), None, "m", 1.0);
    let q = site
        .get(2, &generation(Some("derivatives/q.png"), "m", 1.0))
        .await;
    assert_eq!(
        (q.status, q.header("content-type")),
        (200, Some("image/png"))
    );
    site.put("derivatives/h.heic", b"heic-bytes");
    site.add_photo(3, Some("derivatives/h.heic"), None, "m", 1.0);
    assert_eq!(
        site.get(3, &generation(Some("derivatives/h.heic"), "m", 1.0))
            .await
            .status,
        404
    );
    // 古い v は、writer が無くても 404
    assert_eq!(site.get(1, "000000000000").await.status, 404);
    // キャッシュが既にあれば、それを返す（作らない）
    let cached = jpeg(10, 5, None);
    site.put(&format!("thumbs/1-{v}.jpg"), &cached);
    let hit = site.get(1, &v).await;
    assert_eq!((hit.status, hit.body.as_slice()), (200, cached.as_slice()));
    assert_eq!(hit.header("etag"), Some(format!("\"{v}\"").as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn when_the_thumbnail_cannot_be_written_the_original_is_returned_and_retries_wait() {
    let site = Site::start(true).await;
    // thumbs/ がリンクだと書かない（書き先はリンク先ではなく、書けない扱い）
    let elsewhere = TempDir::new("thumb-link-target");
    symlink(elsewhere.path(), site.path("thumbs")).unwrap();
    let v1 = site.add_standard(1, None);
    let v2 = site.add_standard(2, None);
    let original = fs::read(site.path("derivatives/p1.jpg")).unwrap();
    let response = site.get(1, &v1).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, original);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert!(response.header("etag").is_none());
    assert_eq!(site.thumbs.renders(), 1);
    // 60 秒は全体で生成を試みない（別の写真も元の画像）
    let other = site.get(2, &v2).await;
    assert_eq!(other.status, 200);
    assert_eq!(
        other.body,
        fs::read(site.path("derivatives/p2.jpg")).unwrap()
    );
    assert_eq!(site.get(1, &v1).await.status, 200);
    assert_eq!(site.thumbs.renders(), 1);
    assert_eq!(
        fs::read_dir(elsewhere.path()).unwrap().count(),
        0,
        "リンク先に書かない"
    );
}

// ---- 権限・メソッド ----

#[tokio::test(flavor = "multi_thread")]
async fn cross_site_requests_are_403_and_do_not_generate() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    for value in ["cross-site", "same-site"] {
        let response = site
            .send(
                "GET",
                &format!("/thumb/1.jpg?v={v}"),
                &[("Sec-Fetch-Site", value)],
            )
            .await;
        assert_eq!(response.status, 403, "{value}");
    }
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());
    // 同一オリジン・アドレスバー・ヘッダ無しは通る
    for value in ["same-origin", "none"] {
        let response = site
            .send(
                "GET",
                &format!("/thumb/1.jpg?v={v}"),
                &[("Sec-Fetch-Site", value)],
            )
            .await;
        assert_eq!(response.status, 200, "{value}");
    }
    // キャッシュがあっても cross-site には返さない
    let response = site
        .send(
            "GET",
            &format!("/thumb/1.jpg?v={v}"),
            &[("Sec-Fetch-Site", "cross-site")],
        )
        .await;
    assert_eq!(response.status, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn head_never_generates() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    let head = site.send("HEAD", &format!("/thumb/1.jpg?v={v}"), &[]).await;
    assert_eq!(head.status, 404);
    assert!(head.body.is_empty());
    assert_eq!(site.thumbs.renders(), 0);
    assert!(!site.thumbs_dir_exists());
    // POST は既存の決まりどおり 403
    let post = site.send("POST", &format!("/thumb/1.jpg?v={v}"), &[]).await;
    assert_eq!(post.status, 403);
    assert!(!site.thumbs_dir_exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_static_file_budget_is_503_for_cached_thumbnails() {
    let site = Site::start(true).await;
    let v = site.add_standard(1, None);
    assert_eq!(site.get(1, &v).await.status, 200);
    let held = Arc::clone(&site.slots)
        .try_acquire_many_owned(STATIC_FILE_SLOTS as u32)
        .unwrap();
    let busy = site.get(1, &v).await;
    assert_eq!(busy.status, 503);
    assert_eq!(busy.header("retry-after"), Some("5"));
    drop(held);
    assert_eq!(site.get(1, &v).await.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_concurrent_requests_for_one_photo_generate_once() {
    let site = Site::start_with(true, counting_renderer(Duration::from_millis(150))).await;
    let v = site.add_standard(1, None);
    let addr = site.addr;
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let target = format!("/thumb/1.jpg?v={v}");
            tokio::spawn(async move { get(addr, &target).await })
        })
        .collect();
    let mut bodies = Vec::new();
    for task in tasks {
        let response = task.await.unwrap();
        assert_eq!(response.status, 200);
        bodies.push(response.body);
    }
    assert_eq!(site.thumbs.renders(), 1);
    assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(site.thumb_files(), [format!("1-{v}.jpg")]);
    assert_eq!(site.thumbs.lock_table_len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn derivatives_keep_their_headers() {
    // `/derivatives/` の応答は変えない（ETag も Cache-Control も付かない）
    let site = Site::start(true).await;
    site.add_standard(1, None);
    let response = site.send("GET", "/derivatives/p1.jpg", &[]).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("image/jpeg"));
    assert!(response.header("etag").is_none());
    assert!(response.header("cache-control").is_none());
    assert!(response.header("cross-origin-resource-policy").is_none());
    assert!(response.header("last-modified").is_some());
}
