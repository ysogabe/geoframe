//! HEICを外部コマンド（libheifの`heif-convert`、新しい版では`heif-dec`）へ任せる候補。
//!
//! コマンドにHEICをJPEGへ変換させ、そのJPEGを通常の経路で読む。画素のJPEG再圧縮が1回増える
//! （品質は`-q 95`）。向きはコマンドがirot/imirを適用して書くので、再度は当てない。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Decoded, Options, decode_jpeg};
use image::metadata::Orientation;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 一時ファイルの掃除（失敗しても無視）
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub fn decode(
    cmd: &str,
    bytes: &[u8],
    path: Option<&Path>,
    opts: &Options,
) -> Result<Decoded, String> {
    let dir = std::env::temp_dir();
    let id = format!(
        "gfimgpoc-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let mut cleanup = Cleanup(Vec::new());
    let input = match path {
        Some(p) => p.to_path_buf(),
        None => {
            let p = dir.join(format!("{id}.heic"));
            std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
            cleanup.0.push(p.clone());
            p
        }
    };
    let out = dir.join(format!("{id}.jpg"));
    cleanup.0.push(out.clone());
    // 複数の最上位画像があるとコマンドは`<name>-1.jpg`のように書く
    cleanup.0.push(dir.join(format!("{id}-1.jpg")));

    let status = Command::new(cmd)
        .args(["-q", "95"])
        .arg(&input)
        .arg(&out)
        .output()
        .map_err(|e| format!("{cmd}を起動できません: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "{cmd}が失敗しました（終了コード{:?}）",
            status.status.code()
        ));
    }
    let jpeg = std::fs::read(&out)
        .or_else(|_| std::fs::read(dir.join(format!("{id}-1.jpg"))))
        .map_err(|e| format!("変換結果を読めません: {e}"))?;
    let mut decoded = decode_jpeg(&jpeg, opts)?;
    decoded.orientation = Orientation::NoTransforms;
    Ok(decoded)
}
