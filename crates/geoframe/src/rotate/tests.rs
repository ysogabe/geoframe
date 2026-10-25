//! E14 回転の単体テスト: 検査の境界、向きと JPEG の設定、失敗の後始末、競合（同期サイクルが `display` を
//! 変えた・行が消えた・書き込みの失敗）、リンクの扱い。Python の `admin_rotate` との突き合わせは
//! `tests/rotate_api.rs`（フィクスチャ）。画像・drive_id は合成。

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::{rotate, rotate_with};
use crate::post::ApiError;
use crate::testutil::{TempDir, capture};
use crate::writer::{OpenMode, Writer};

pub(crate) struct Env {
    pub dir: TempDir,
    pub writer: Writer,
}

/// 4 色の象限の JPEG（左上 赤・右上 緑・左下 青・右下 黄）。
pub(crate) fn jpeg(width: usize, height: usize) -> Vec<u8> {
    let colors: [[u8; 3]; 4] = [[255, 0, 0], [0, 160, 0], [0, 0, 255], [255, 255, 0]];
    let mut pixels = vec![0u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let q = usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2);
            pixels[(y * width + x) * 3..][..3].copy_from_slice(&colors[q]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 90)
        .encode(
            &pixels,
            u16::try_from(width).unwrap(),
            u16::try_from(height).unwrap(),
            jpeg_encoder::ColorType::Rgb,
        )
        .unwrap();
    out
}

/// 寸法と、四隅の目印（角から 4px 内側）の色の名前。
pub(crate) fn measure(bytes: &[u8]) -> ((usize, usize), Vec<&'static str>) {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let mut dec = JpegDecoder::new(ZCursor::new(bytes));
    let px = dec.decode().unwrap();
    let (w, h) = dec.dimensions().unwrap();
    let names = [(4, 4), (w - 5, 4), (4, h - 5), (w - 5, h - 5)]
        .iter()
        .map(|&(x, y)| {
            let p = &px[(y * w + x) * 3..][..3];
            [
                ("red", [255, 0, 0]),
                ("green", [0, 160, 0]),
                ("blue", [0, 0, 255]),
                ("yellow", [255, 255, 0]),
            ]
            .iter()
            .min_by_key(|(_, c)| (0..3).map(|i| (c[i] - i32::from(p[i])).pow(2)).sum::<i32>())
            .unwrap()
            .0
        })
        .collect();
    ((w, h), names)
}

/// JPEG のマーカー（SOI と画像データの前まで）→ (マーカー, 内容)。
fn segments(jpeg: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let marker = jpeg[i + 1];
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        out.push((marker, jpeg[i + 4..i + 2 + len].to_vec()));
        if marker == 0xDA {
            break;
        }
        i += 2 + len;
    }
    out
}

impl Env {
    pub fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let writer =
            Writer::open(&dir.path().join("catalog.db"), OpenMode::CreateIfMissing).unwrap();
        writer.call(crate::schema::migrate).unwrap();
        fs::create_dir_all(dir.path().join("derivatives")).unwrap();
        let env = Self { dir, writer };
        env.sql(
            "INSERT INTO photos(id, source, drive_id, display, name, md5, fetched_at, rotation)
               VALUES(1, 'drive', 'abc', 'derivatives/abc.jpg', 'a', 'm1', 'f1', NULL);
             INSERT INTO photos(id, source, path, display, name) VALUES(2, 'local', 'p/b.jpg', 'p/b.jpg', 'b');",
        );
        env.put("derivatives/abc.jpg", &jpeg(40, 20));
        env
    }

    pub fn sql(&self, sql: &'static str) {
        self.writer.call(move |c| c.execute_batch(sql)).unwrap();
    }

    pub fn put(&self, rel: &str, bytes: &[u8]) {
        let path = self.dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn read(&self, rel: &str) -> Vec<u8> {
        fs::read(self.path(rel)).unwrap()
    }

    /// `derivatives/` の名前（`.part` を含む）。
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.path("derivatives"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// 行 1 の (display, rotation, md5)。
    pub fn row(&self) -> (Option<String>, Option<i64>, Option<String>) {
        self.writer
            .call(|c| {
                c.query_row(
                    "SELECT display, rotation, md5 FROM photos WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
            })
            .unwrap()
    }

    pub fn rotate(&self, payload: Value) -> Result<Value, ApiError> {
        rotate(
            payload.as_object().unwrap(),
            Some(&self.writer),
            self.dir.path(),
        )
    }

    pub fn rotate_between(&self, payload: Value, between: &dyn Fn()) -> Result<Value, ApiError> {
        rotate_with(
            payload.as_object().unwrap(),
            Some(&self.writer),
            self.dir.path(),
            between,
        )
    }
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn kind(e: &ApiError) -> (&'static str, String) {
    match e {
        ApiError::BadRequest(m) => ("400", m.clone()),
        ApiError::NotFound(m) => ("404", m.clone()),
        ApiError::Busy(m) => ("409", m.clone()),
        ApiError::Unavailable => ("503", String::new()),
        ApiError::Internal(m) => ("500", m.clone()),
    }
}

pub(crate) fn status(result: Result<Value, ApiError>) -> &'static str {
    kind(&result.unwrap_err()).0
}

// ---------------------------------------------------------------- 検査

#[test]
fn deltaは整数の90_180_270_マイナス90だけで_小数と真偽値と文字列は400() {
    let env = Env::new("rotate-delta");
    for bad in [
        json!(0),
        json!(45),
        json!(360),
        json!(-180),
        json!(-270),
        json!(450),
        json!(90.5),
        json!("90"),
        json!(true),
        json!(null),
        json!([90]),
        json!({"a": 90}),
        // Python は通す（90.0 == 90）。Rust は 400（server.md §0）
        json!(90.0),
        json!(-90.0),
        json!(180.0),
    ] {
        let err = env
            .rotate(json!({"id": 1, "delta": bad.clone()}))
            .unwrap_err();
        assert_eq!(
            kind(&err),
            (
                "400",
                "delta は 90 / 180 / 270 / -90 で指定してください".to_string()
            ),
            "{bad}"
        );
    }
    assert_eq!(status(env.rotate(json!({"id": 1}))), "400");
    // 何も変わらない
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(
        env.row(),
        (Some("derivatives/abc.jpg".into()), None, Some("m1".into()))
    );
    for delta in [90, 180, 270, -90] {
        env.rotate(json!({"id": 1, "delta": delta})).unwrap();
    }
}

#[test]
fn idの検査はdeltaより先で_i64を超えるidは404() {
    let env = Env::new("rotate-id");
    let message = |p: Value| kind(&env.rotate(p).unwrap_err());
    assert_eq!(
        message(json!({"delta": 90})).1,
        "id は整数で指定してください"
    );
    assert_eq!(message(json!({"id": 0, "delta": 45})).1, "id が範囲外です");
    assert_eq!(
        message(json!({"id": 1.0, "delta": 90})).1,
        "id は整数で指定してください"
    );
    assert_eq!(
        message(json!({"id": true, "delta": 90})).1,
        "id は整数で指定してください"
    );
    // delta の検査は行の検索より先
    assert_eq!(message(json!({"id": 999, "delta": 45})).0, "400");
    assert_eq!(
        message(json!({"id": 999, "delta": 90})),
        ("404", "id=999 は存在しません".to_string())
    );
    // i64 を超える id は SQL を実行せずに 404（i64::MAX の行に当たらない）
    env.sql("INSERT INTO photos(id, source, drive_id, display, name) VALUES(9223372036854775807, 'drive', 'big', 'derivatives/big.jpg', 'x')");
    env.put("derivatives/big.jpg", &jpeg(40, 20));
    let payload = obj(json!({"delta": 90}));
    let mut payload = payload;
    payload.insert(
        "id".into(),
        serde_json::from_str("9223372036854775808").unwrap(),
    );
    let err = rotate(&payload, Some(&env.writer), env.dir.path()).unwrap_err();
    assert_eq!(kind(&err).0, "404");
    assert_eq!(env.names(), ["abc.jpg", "big.jpg"]);
}

#[test]
fn writerが無ければ検査のあとファイルを作る前に503() {
    let env = Env::new("rotate-no-writer");
    let before = env.names();
    let result = rotate(&obj(json!({"id": 1, "delta": 90})), None, env.dir.path());
    assert!(matches!(result, Err(ApiError::Unavailable)));
    // 400 が先
    let result = rotate(&obj(json!({"id": 1, "delta": 45})), None, env.dir.path());
    assert_eq!(status(result), "400");
    assert_eq!(env.names(), before);
}

#[test]
fn driveの写真でなければ400で_displayが無いか通常のファイルでなければ画像がまだ無いと言う() {
    let env = Env::new("rotate-source");
    let msg = |env: &Env, id: i64| kind(&env.rotate(json!({"id": id, "delta": 90})).unwrap_err());
    assert_eq!(
        msg(&env, 2),
        ("400", "回せるのはDriveの写真だけです".to_string())
    );
    env.sql("UPDATE photos SET drive_id=NULL WHERE id=1");
    assert_eq!(msg(&env, 1).1, "回せるのはDriveの写真だけです");
    env.sql("UPDATE photos SET drive_id='' WHERE id=1");
    assert_eq!(msg(&env, 1).1, "回せるのはDriveの写真だけです");
    env.sql("UPDATE photos SET drive_id='abc', display=NULL WHERE id=1");
    assert_eq!(
        msg(&env, 1),
        ("400", "表示用の画像がまだありません".to_string())
    );
    env.sql("UPDATE photos SET display='' WHERE id=1");
    assert_eq!(msg(&env, 1).1, "表示用の画像がまだありません");
    // ファイルが無い
    env.sql("UPDATE photos SET display='derivatives/none.jpg' WHERE id=1");
    assert_eq!(msg(&env, 1).1, "表示用の画像がまだありません");
    // ディレクトリ・FIFO（開いて止まらない）
    fs::create_dir(env.path("derivatives/dir.jpg")).unwrap();
    env.sql("UPDATE photos SET display='derivatives/dir.jpg' WHERE id=1");
    assert_eq!(msg(&env, 1).1, "表示用の画像がまだありません");
    if std::process::Command::new("mkfifo")
        .arg(env.path("derivatives/fifo.jpg"))
        .status()
        .is_ok_and(|s| s.success())
    {
        env.sql("UPDATE photos SET display='derivatives/fifo.jpg' WHERE id=1");
        assert_eq!(msg(&env, 1).1, "表示用の画像がまだありません");
    }
    // derivatives/<安全な名前> でない display は読まない（DATA_DIR の別の場所・上位・絶対パス）
    for display in [
        "photos/x.jpg",
        "derivatives/../catalog.db",
        "derivatives/sub/x.jpg",
        "/etc/passwd",
        "derivatives/.hidden.jpg",
    ] {
        env.put("photos/x.jpg", &jpeg(40, 20));
        env.writer
            .call({
                let display = display.to_string();
                move |c| c.execute("UPDATE photos SET display=?1 WHERE id=1", [display])
            })
            .unwrap();
        assert_eq!(msg(&env, 1).1, "表示用の画像がまだありません", "{display}");
    }
    assert!(env.path("photos/x.jpg").exists());
}

#[test]
fn 画像でない表示ファイルは500で_何も作らず何も変えない() {
    let env = Env::new("rotate-garbage");
    env.put("derivatives/abc.jpg", b"not an image");
    let err = env.rotate(json!({"id": 1, "delta": 90})).unwrap_err();
    let (code, detail) = kind(&err);
    assert_eq!(code, "500");
    assert!(!detail.contains("abc"), "{detail}");
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(env.row().1, None);
}

// ---------------------------------------------------------------- 正常

#[test]
fn 時計回りに回して名前は回転に従い_4回で元の名前と向きに戻り_旧ファイルは消える() {
    let env = Env::new("rotate-basic");
    let expected = [
        (
            90,
            "derivatives/abc.r90.jpg",
            (20, 40),
            ["blue", "red", "yellow", "green"],
        ),
        (
            180,
            "derivatives/abc.r180.jpg",
            (40, 20),
            ["yellow", "blue", "green", "red"],
        ),
        (
            270,
            "derivatives/abc.r270.jpg",
            (20, 40),
            ["green", "yellow", "red", "blue"],
        ),
        (
            0,
            "derivatives/abc.jpg",
            (40, 20),
            ["red", "green", "blue", "yellow"],
        ),
    ];
    for (rotation, display, size, corners) in expected {
        let out = env.rotate(json!({"id": 1, "delta": 90})).unwrap();
        assert_eq!(
            out,
            json!({"ok": true, "id": 1, "rotation": rotation, "thumb": format!("/{display}")})
        );
        assert_eq!(env.row().0.as_deref(), Some(display));
        assert_eq!(env.row().1, Some(rotation));
        // 旧ファイルは消え、新しいファイルだけが残る（`.part` も残らない）
        assert_eq!(env.names(), [display.strip_prefix("derivatives/").unwrap()]);
        let (got_size, got_corners) = measure(&env.read(display));
        assert_eq!(got_size, size, "{display}");
        assert_eq!(got_corners, corners, "{display}");
    }
    // 反時計回りと半回転・3/4 回転
    for (delta, rotation) in [(-90, 270), (180, 90), (270, 0)] {
        let out = env.rotate(json!({"id": 1, "delta": delta})).unwrap();
        assert_eq!(out["rotation"], rotation);
    }
    assert_eq!(env.names(), ["abc.jpg"]);
}

#[test]
fn 出力はjpegの品質85_4_2_0のベースラインで_exifもgpsも付かない() {
    let env = Env::new("rotate-jpeg");
    // EXIF（向き 6・GPS）と ICC 風の APP2 を持つ入力
    let mut input = jpeg(40, 20);
    let mut exif =
        b"Exif\0\0MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0".to_vec();
    exif.extend_from_slice(b"GPSLATITUDE");
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
    app1.extend_from_slice(&exif);
    input.splice(2..2, app1);
    assert!(input.windows(6).any(|w| w == b"Exif\0\0"));
    env.put("derivatives/abc.jpg", &input);
    env.rotate(json!({"id": 1, "delta": 90})).unwrap();
    let out = env.read("derivatives/abc.r90.jpg");
    // EXIF の向き（6）は当てない: 40x20 を 90° 回して 20x40
    assert_eq!(measure(&out).0, (20, 40));
    assert!(!out.windows(6).any(|w| w == b"Exif\0\0"));
    assert!(!out.windows(11).any(|w| w == b"GPSLATITUDE"));
    let segs = segments(&out);
    let markers: Vec<u8> = segs.iter().map(|(m, _)| *m).collect();
    assert!(!markers.contains(&0xE1), "APP1 が残っている: {markers:x?}");
    assert!(!markers.contains(&0xE2), "ICC が付いている: {markers:x?}");
    // ベースライン（SOF0）。プログレッシブ（SOF2）ではない
    assert!(
        markers.contains(&0xC0) && !markers.contains(&0xC2),
        "{markers:x?}"
    );
    let (_, sof) = segs.iter().find(|(m, _)| *m == 0xC0).unwrap();
    // 精度 8、3 成分、Y のサンプリングが 2x2（4:2:0）で Cb・Cr が 1x1
    assert_eq!(sof[0], 8);
    assert_eq!(sof[5], 3);
    assert_eq!([sof[7], sof[10], sof[13]], [0x22, 0x11, 0x11]);
}

#[test]
fn グレーの画像はグレーのまま回る() {
    let env = Env::new("rotate-gray");
    let mut pixels = vec![0u8; 40 * 20];
    for y in 0..20 {
        for x in 0..40 {
            pixels[y * 40 + x] = if x < 20 { 40 } else { 220 };
        }
    }
    let mut gray = Vec::new();
    jpeg_encoder::Encoder::new(&mut gray, 90)
        .encode(&pixels, 40, 20, jpeg_encoder::ColorType::Luma)
        .unwrap();
    env.put("derivatives/abc.jpg", &gray);
    env.rotate(json!({"id": 1, "delta": 90})).unwrap();
    let out = env.read("derivatives/abc.r90.jpg");
    let segs = segments(&out);
    let (_, sof) = segs.iter().find(|(m, _)| *m == 0xC0).unwrap();
    assert_eq!(sof[5], 1, "グレーは 1 成分のまま");
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    let mut dec = JpegDecoder::new(ZCursor::new(&out));
    let px = dec.decode().unwrap();
    let (w, h) = dec.dimensions().unwrap();
    assert_eq!((w, h), (20, 40));
    // 時計回りに 90°: 左（暗）が上になる
    // （デコーダは RGB で返すので、3 成分とも同じ値）
    assert!(i32::from(px[(5 * w + 10) * 3]) < 100, "上は暗い");
    assert!(i32::from(px[((h - 6) * w + 10) * 3]) > 160, "下は明るい");
}

#[test]
fn 回転が90の倍数でない行_負の行_大きい行は剰余で扱う() {
    let env = Env::new("rotate-weird-rotation");
    for (stored, delta, expect) in [(45, 90, 135), (-90, 90, 0), (1000, -90, 190), (359, 90, 89)] {
        env.writer
            .call(move |c| c.execute("UPDATE photos SET rotation=?1 WHERE id=1", [stored]))
            .unwrap();
        let out = env.rotate(json!({"id": 1, "delta": delta})).unwrap();
        assert_eq!(out["rotation"], expect, "{stored}+{delta}");
        let name = if expect == 0 {
            "abc.jpg".to_string()
        } else {
            format!("abc.r{expect}.jpg")
        };
        assert_eq!(env.names(), [name]);
    }
}

// ---------------------------------------------------------------- 失敗の後始末

#[test]
fn 書き出しに失敗したら新しいファイルも_partも残さず_旧ファイルとdbは元のまま() {
    let env = Env::new("rotate-write-fail");
    let before = env.read("derivatives/abc.jpg");
    // 新しい名前の位置にディレクトリがあって、rename できない
    fs::create_dir(env.path("derivatives/abc.r90.jpg")).unwrap();
    let err = env.rotate(json!({"id": 1, "delta": 90})).unwrap_err();
    assert_eq!(kind(&err).0, "500");
    assert_eq!(env.names(), ["abc.jpg", "abc.r90.jpg"]);
    assert!(env.path("derivatives/abc.r90.jpg").is_dir());
    assert_eq!(env.read("derivatives/abc.jpg"), before);
    assert_eq!(
        env.row(),
        (Some("derivatives/abc.jpg".into()), None, Some("m1".into()))
    );
}

#[test]
fn 書き込み先を作れないときも何も残さない() {
    // 親ディレクトリに書けない（root では効かないので、効くときだけ確かめる）
    let env = Env::new("rotate-readonly");
    let dir = env.path("derivatives");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let writable = fs::File::create(dir.join("probe")).is_ok();
    let result = env.rotate(json!({"id": 1, "delta": 90}));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        let _ = fs::remove_file(dir.join("probe"));
        return; // root: 権限で止められない
    }
    assert_eq!(status(result), "500");
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(env.row().1, None);
}

#[test]
fn dbの更新に失敗したら新しいファイルを消し_旧ファイルとdbは元のまま() {
    let env = Env::new("rotate-db-fail");
    env.sql(
        "CREATE TRIGGER no_rotation BEFORE UPDATE OF rotation ON photos
         BEGIN SELECT RAISE(ABORT, 'blocked'); END",
    );
    let before = env.read("derivatives/abc.jpg");
    let err = env.rotate(json!({"id": 1, "delta": 90})).unwrap_err();
    let (code, detail) = kind(&err);
    assert_eq!(code, "500");
    // 応答に出るのは定型文だけ（ここは内部のログ用の文面だが、ファイル名・drive_id を含まない）
    assert!(!detail.contains("abc"), "{detail}");
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(env.read("derivatives/abc.jpg"), before);
    assert_eq!(
        env.row(),
        (Some("derivatives/abc.jpg".into()), None, Some("m1".into()))
    );
}

#[test]
fn 新しい名前が今のdisplayと同じなら上書きし_dbが失敗したら元の内容に戻す() {
    let env = Env::new("rotate-same-name");
    // display と rotation の食い違い: 90 のはずが abc.jpg を指している。270 を足すと 0（abc.jpg）
    env.sql("UPDATE photos SET rotation=90 WHERE id=1");
    let original = env.read("derivatives/abc.jpg");
    env.sql(
        "CREATE TRIGGER no_rotation BEFORE UPDATE OF rotation ON photos
         BEGIN SELECT RAISE(ABORT, 'blocked'); END",
    );
    assert_eq!(status(env.rotate(json!({"id": 1, "delta": 270}))), "500");
    assert_eq!(
        env.read("derivatives/abc.jpg"),
        original,
        "元の内容に戻っている"
    );
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(env.row().1, Some(90));
    // 成功すれば上書きで、消さない
    env.sql("DROP TRIGGER no_rotation");
    let out = env.rotate(json!({"id": 1, "delta": 270})).unwrap();
    assert_eq!(out["thumb"], "/derivatives/abc.jpg");
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(measure(&env.read("derivatives/abc.jpg")).0, (20, 40));
    assert_eq!(env.row().1, Some(0));
}

#[test]
fn 旧ファイルを消せなくても応答は成功で_ログは固定の文言だけ() {
    let env = Env::new("rotate-old-remove-fail");
    let dir = env.path("derivatives");
    // 新しいファイルを書いたあと・DB を更新する前に、ディレクトリを書き込み不可にする
    // （旧ファイルを消せない）。root では効かないので、効くときだけ確かめる
    let lock_dir = dir.clone();
    let logs = capture::logs_of(|| {
        let result = env.rotate_between(json!({"id": 1, "delta": 90}), &|| {
            fs::set_permissions(&lock_dir, fs::Permissions::from_mode(0o555)).unwrap();
        });
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let out = result.expect("旧ファイルの削除の失敗は成功にする");
        assert_eq!(out["thumb"], "/derivatives/abc.r90.jpg");
    });
    let removed = !env.path("derivatives/abc.jpg").exists();
    if removed {
        return; // root: 権限で止められず、消えた
    }
    // DB は新しいファイルを指す。旧ファイルは残る（孤児。次の回転・取り込みの掃除の対象）
    assert_eq!(env.row().0.as_deref(), Some("derivatives/abc.r90.jpg"));
    assert!(
        logs.contains("古い表示用ファイルを削除できませんでした"),
        "{logs}"
    );
    for secret in ["abc", ".jpg", "derivatives"] {
        assert!(!logs.contains(secret), "{secret}: {logs}");
    }
}

// ---------------------------------------------------------------- 競合

/// 同期サイクルの段が、回転の途中で `display` を NULL にした（推定の取り消し・Drive の md5 の変更）。
#[test]
fn 途中でdisplayがnullになったら何も書かず新しいファイルを消して409() {
    let env = Env::new("rotate-race-null");
    let original = env.read("derivatives/abc.jpg");
    let writer = env.writer.clone();
    let err = env
        .rotate_between(json!({"id": 1, "delta": 90}), &|| {
            writer
                .call(|c| c.execute("UPDATE photos SET display=NULL WHERE id=1", []))
                .unwrap();
            // 新しいファイルは、DB を更新する前にもう書かれている
            assert!(Path::new(&env.path("derivatives/abc.r90.jpg")).exists());
        })
        .unwrap_err();
    assert_eq!(kind(&err).0, "409");
    // display が消えたファイルを指さず、孤児も増えない（取得が同じ名前で作り直す）
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(env.read("derivatives/abc.jpg"), original);
    assert_eq!(env.row(), (None, None, Some("m1".into())));
}

#[test]
fn driveのmd5が変わって索引が表示と回転をリセットしたら409() {
    let env = Env::new("rotate-race-md5");
    let writer = env.writer.clone();
    let err = env
        .rotate_between(json!({"id": 1, "delta": 90}), &|| {
            writer
                .call(|c| {
                    c.execute(
                        "UPDATE photos SET md5='m2', display=NULL, fetched_at=NULL, rotation=0 WHERE id=1",
                        [],
                    )
                })
                .unwrap();
        })
        .unwrap_err();
    assert_eq!(kind(&err).0, "409");
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(env.row(), (None, Some(0), Some("m2".into())));
}

/// 取得が、`display` を NULL にされた同じ写真を、新しい原本から同じ名前で作り直した（同じ `display`・`rotation`
/// に戻る）。`md5`・`fetched_at` が違うので条件が外れ、古い内容を回した結果で上書きしない。
#[test]
fn 同じdisplayに作り直されても_md5とfetched_atが違えば409() {
    let env = Env::new("rotate-race-aba");
    let writer = env.writer.clone();
    let fresh = jpeg(60, 30);
    let (path, fresh_copy) = (env.path("derivatives/abc.jpg"), fresh.clone());
    let err = env
        .rotate_between(json!({"id": 1, "delta": 90}), &|| {
            fs::write(&path, &fresh_copy).unwrap();
            writer
                .call(|c| {
                    c.execute(
                        "UPDATE photos SET md5='m2', fetched_at='f2', display='derivatives/abc.jpg' WHERE id=1",
                        [],
                    )
                })
                .unwrap();
        })
        .unwrap_err();
    assert_eq!(kind(&err).0, "409");
    // 作り直されたファイルはそのまま、回転した内容は残らない
    assert_eq!(env.read("derivatives/abc.jpg"), fresh);
    assert_eq!(env.names(), ["abc.jpg"]);
    assert_eq!(
        env.row(),
        (Some("derivatives/abc.jpg".into()), None, Some("m2".into()))
    );
}

/// 新しい名前が、DB が今指しているファイルと同じ: 消さない（回転が負けても `display` は実在するファイルを指す）。
#[test]
fn dbが今指しているファイルと新しい名前が同じなら_負けても消さない() {
    let env = Env::new("rotate-race-keep");
    // rotation 270 の写真（abc.r270.jpg）を +90 → 新しい名前は abc.jpg
    env.sql("UPDATE photos SET rotation=270, display='derivatives/abc.r270.jpg' WHERE id=1");
    fs::rename(
        env.path("derivatives/abc.jpg"),
        env.path("derivatives/abc.r270.jpg"),
    )
    .unwrap();
    let writer = env.writer.clone();
    let (path, fresh) = (env.path("derivatives/abc.jpg"), jpeg(60, 30));
    let fresh_copy = fresh.clone();
    let err = env
        .rotate_between(json!({"id": 1, "delta": 90}), &|| {
            // 索引が rotation を 0 に戻し、取得が abc.jpg を作った
            fs::write(&path, &fresh_copy).unwrap();
            writer
                .call(|c| {
                    c.execute(
                        "UPDATE photos SET md5='m2', fetched_at='f2', rotation=0, display='derivatives/abc.jpg' WHERE id=1",
                        [],
                    )
                })
                .unwrap();
        })
        .unwrap_err();
    assert_eq!(kind(&err).0, "409");
    assert_eq!(
        env.read("derivatives/abc.jpg"),
        fresh,
        "DB が指すファイルを消さない"
    );
    assert_eq!(env.row().0.as_deref(), Some("derivatives/abc.jpg"));
}

#[test]
fn 途中で行が消えたら404で新しいファイルを消す() {
    let env = Env::new("rotate-race-gone");
    let writer = env.writer.clone();
    let err = env
        .rotate_between(json!({"id": 1, "delta": 90}), &|| {
            writer
                .call(|c| c.execute("DELETE FROM photos WHERE id=1", []))
                .unwrap();
        })
        .unwrap_err();
    assert_eq!(kind(&err), ("404", "id=1 は存在しません".to_string()));
    assert_eq!(env.names(), ["abc.jpg"]);
}

#[test]
fn 同じ写真への回転が同時に来ても1つずつ反映され_孤児が残らない() {
    let env = Arc::new(Env::new("rotate-concurrent"));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let env = Arc::clone(&env);
            std::thread::spawn(move || env.rotate(json!({"id": 1, "delta": 90})).unwrap())
        })
        .collect();
    let mut rotations: Vec<i64> = handles
        .into_iter()
        .map(|h| h.join().unwrap()["rotation"].as_i64().unwrap())
        .collect();
    rotations.sort_unstable();
    // 90 を 6 回: 90, 180, 270, 0, 90, 180（どの順でも、1 回ずつ足されて重ならない）
    assert_eq!(rotations, [0, 90, 90, 180, 180, 270]);
    assert_eq!(env.row().1, Some(180));
    assert_eq!(env.names(), ["abc.r180.jpg"]);
    assert_eq!(measure(&env.read("derivatives/abc.r180.jpg")).0, (40, 20));
}

// ---------------------------------------------------------------- リンク・DATA_DIR の外

#[test]
fn リンクのdisplayは辿らず_外のファイルは読まず消さない() {
    let env = Env::new("rotate-link");
    let outside = TempDir::new("rotate-outside");
    let victim = outside.path().join("victim.jpg");
    fs::write(&victim, jpeg(40, 20)).unwrap();
    fs::remove_file(env.path("derivatives/abc.jpg")).unwrap();
    symlink(&victim, env.path("derivatives/abc.jpg")).unwrap();
    let err = env.rotate(json!({"id": 1, "delta": 90})).unwrap_err();
    assert_eq!(
        kind(&err),
        ("400", "表示用の画像がまだありません".to_string())
    );
    assert!(victim.exists());
    assert_eq!(fs::read(&victim).unwrap(), jpeg(40, 20));
    assert_eq!(env.names(), ["abc.jpg"]);
    // 切れたリンクも同じ
    fs::remove_file(env.path("derivatives/abc.jpg")).unwrap();
    symlink(
        outside.path().join("none.jpg"),
        env.path("derivatives/abc.jpg"),
    )
    .unwrap();
    assert_eq!(status(env.rotate(json!({"id": 1, "delta": 90}))), "400");
}

#[test]
fn derivativesがリンクなら使わない() {
    let env = Env::new("rotate-dir-link");
    let outside = TempDir::new("rotate-outside-dir");
    fs::write(outside.path().join("abc.jpg"), jpeg(40, 20)).unwrap();
    fs::remove_dir_all(env.path("derivatives")).unwrap();
    symlink(outside.path(), env.path("derivatives")).unwrap();
    assert_eq!(status(env.rotate(json!({"id": 1, "delta": 90}))), "400");
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        1,
        "外のディレクトリに何も作らない"
    );
}

#[test]
fn 新しい名前の位置に置かれたリンクは辿らず_リンク自体を置き換える() {
    let env = Env::new("rotate-dest-link");
    let outside = TempDir::new("rotate-outside-dest");
    let victim = outside.path().join("victim.txt");
    fs::write(&victim, b"keep me").unwrap();
    symlink(&victim, env.path("derivatives/abc.r90.jpg")).unwrap();
    // 切れたリンクの先も作らせない
    let created = outside.path().join("created");
    symlink(&created, env.path("derivatives/abc.r90.jpg.part")).unwrap();
    env.rotate(json!({"id": 1, "delta": 90})).unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"keep me");
    assert!(!created.exists());
    let meta = fs::symlink_metadata(env.path("derivatives/abc.r90.jpg")).unwrap();
    assert!(meta.is_file() && !meta.is_symlink());
    // 自分のものでない `.part` のリンクには触らない
    assert_eq!(env.names(), ["abc.r90.jpg", "abc.r90.jpg.part"]);
}

#[test]
fn drive_idが名前に使えない行は回さない() {
    let env = Env::new("rotate-bad-drive-id");
    for bad in ["../x", "a/b", "a b", ".hidden", "あ"] {
        env.writer
            .call({
                let bad = bad.to_string();
                move |c| c.execute("UPDATE photos SET drive_id=?1 WHERE id=1", [bad])
            })
            .unwrap();
        let err = env.rotate(json!({"id": 1, "delta": 90})).unwrap_err();
        let (code, detail) = kind(&err);
        assert_eq!(code, "500", "{bad}");
        assert!(!detail.contains(bad), "{detail}");
    }
    assert_eq!(env.names(), ["abc.jpg"]);
    assert!(!env.path("x.r90.jpg").exists());
}

#[test]
fn 画素は回した元の画像と近く_再圧縮の差だけが残る() {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    fn decode(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
        let mut dec = JpegDecoder::new(ZCursor::new(bytes));
        let px = dec.decode().unwrap();
        let (w, h) = dec.dimensions().unwrap();
        (w, h, px)
    }
    // なだらかな色の変化（自然画像の代わり）
    let (w, h) = (200usize, 130usize);
    let mut pixels = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            pixels[(y * w + x) * 3..][..3].copy_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x + y) * 255 / (w + h)) as u8,
            ]);
        }
    }
    let mut source = Vec::new();
    jpeg_encoder::Encoder::new(&mut source, 92)
        .encode(&pixels, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    let (_, _, decoded) = decode(&source);
    let env = Env::new("rotate-pixels");
    for delta in [90_i32, 180, 270, -90] {
        env.put("derivatives/abc.jpg", &source);
        env.sql("UPDATE photos SET display='derivatives/abc.jpg', rotation=NULL WHERE id=1");
        let out = env.rotate(json!({"id": 1, "delta": delta})).unwrap();
        let name = out["thumb"]
            .as_str()
            .unwrap()
            .trim_start_matches('/')
            .to_string();
        let (ow, oh, got) = decode(&env.read(&name));
        let swapped = delta != 180;
        assert_eq!((ow, oh), if swapped { (h, w) } else { (w, h) }, "{delta}");
        // 元の画素 (x, y) の行き先: 時計回り 90° は (h-1-y, x)、180° は (w-1-x, h-1-y)、反時計回り 90° は (y, w-1-x)
        let quarter = delta.rem_euclid(360);
        let mut total = 0u64;
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = match quarter {
                    90 => (h - 1 - y, x),
                    180 => (w - 1 - x, h - 1 - y),
                    _ => (y, w - 1 - x),
                };
                for c in 0..3 {
                    let a = i32::from(decoded[(y * w + x) * 3 + c]);
                    let b = i32::from(got[(dy * ow + dx) * 3 + c]);
                    total += u64::from((a - b).unsigned_abs());
                }
            }
        }
        let mean = total as f64 / (w * h * 3) as f64;
        assert!(mean < 3.0, "delta {delta}: 平均の差 {mean}");
    }
}
