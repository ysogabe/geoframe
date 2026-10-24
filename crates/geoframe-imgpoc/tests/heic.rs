//! HEICの経路（feature `heic`）の確認。合成の小さなHEIC（`tests/fixtures/`、`scripts/gen_fixtures.py`で作成。
//! 写真ではない）だけを使う。HEVCのデコーダ（libde265のプラグイン等）がシステムに無いと失敗する。
#![cfg(feature = "heic")]

use geoframe_imgpoc::{HeicBackend, IccPolicy, Options, process};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn px(jpeg: &[u8], x: u32, y: u32) -> [u8; 3] {
    image::load_from_memory(jpeg)
        .unwrap()
        .to_rgb8()
        .get_pixel(x, y)
        .0
}

fn has_exif(bytes: &[u8]) -> bool {
    bytes.windows(6).any(|w| w == b"Exif\0\0")
}

fn backends() -> Vec<(&'static str, HeicBackend)> {
    #[allow(unused_mut)]
    let mut v = vec![("libheif", HeicBackend::Libheif)];
    #[cfg(feature = "heic-zune")]
    v.push(("zune", HeicBackend::Zune));
    v
}

#[test]
fn heicはirotを一度だけ適用しexifを残さない() {
    for (name, backend) in backends() {
        let opts = Options {
            heic: backend,
            icc: IccPolicy::Drop,
            ..Options::default()
        };
        let o1 = process(&fixture("synthetic_o1.heic"), None, &opts).unwrap();
        assert_eq!(o1.in_dims, (64, 48), "{name}");
        assert_eq!(o1.out_dims, (64, 48), "{name}");
        // 左が赤・右が青
        assert!(
            px(&o1.jpeg, 5, 24)[0] > 180 && px(&o1.jpeg, 59, 24)[2] > 180,
            "{name}"
        );

        // 向き6（時計回り90度）: libheifがirotを適用して48x64になり、さらにEXIFの向きは当てない
        let o6 = process(&fixture("synthetic_o6.heic"), None, &opts).unwrap();
        assert_eq!(o6.in_dims, (48, 64), "{name}");
        assert_eq!(o6.out_dims, (48, 64), "{name}");
        assert_eq!(o6.exif_orientation, 1, "{name}");
        // 元の左（赤）が上へ回る
        assert!(
            px(&o6.jpeg, 24, 5)[0] > 180 && px(&o6.jpeg, 24, 59)[2] > 180,
            "{name}"
        );

        for out in [&o1, &o6] {
            assert!(!has_exif(&out.jpeg), "{name}");
        }
    }
}

#[test]
fn heicのiccはデコーダから取れる() {
    for (name, backend) in backends() {
        let opts = Options {
            heic: backend,
            icc: IccPolicy::Srgb,
            ..Options::default()
        };
        let out = process(&fixture("synthetic_p3.heic"), None, &opts).unwrap();
        assert!(out.icc_in, "{name}");
        assert!(!has_exif(&out.jpeg), "{name}");
    }
}

#[test]
fn heicでないバイト列は拒否する() {
    let opts = Options {
        heic: HeicBackend::Libheif,
        ..Options::default()
    };
    let mut broken = fixture("synthetic_o1.heic");
    broken.truncate(broken.len() / 2);
    assert!(process(&broken, None, &opts).is_err());
}
