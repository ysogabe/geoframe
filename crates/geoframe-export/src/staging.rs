//! 作業領域 `<staging>/<job_id>/`（design.md 4.1 の手順 5・7、8 章）。
//!
//! ジョブごとに新しく作る（0700）。中のファイルは、既にあるものを開かず・リンクをたどらずに、新規作成（`O_EXCL`・
//! `O_NOFOLLOW`、0600）だけで書く。期間（複数日）のサブフォルダも 0700 で作る。パスは [`Workspace::write_new`] が
//! 決められた形（`manifest.json`、`HHMM00_<id>.jpg`、`YYYY-MM-DD/HHMM00_<id>.jpg`）にだけ通す。
//!
//! 作業領域は、作った [`Workspace`] を手放すとき（成功・失敗・パニックのいずれも）に消える。`staging` の親
//! （`StateDirectory/staging/`）は、起動時の検査で作ってあるものとして、ここでは作らない。

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, ExportError};

fn internal(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::Internal, detail)
}

/// `[A-Za-z0-9_-]{1,64}`（ジョブ ID と日付フォルダ・ファイル名の部品）。
fn is_safe_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 作業領域に置いてよい相対パスか。
fn is_allowed_rel(rel: &str) -> bool {
    let ok_file = |name: &str| {
        name == "manifest.json"
            || name.strip_suffix(".jpg").is_some_and(|stem| {
                stem.split_once('_').is_some_and(|(time, id)| {
                    time.len() == 6
                        && time.bytes().all(|b| b.is_ascii_digit())
                        && !id.is_empty()
                        && id.len() <= 19
                        && id.bytes().all(|b| b.is_ascii_digit())
                })
            })
    };
    match rel.split_once('/') {
        None => ok_file(rel),
        Some((dir, name)) => {
            crate::folder::is_valid_folder_name(dir)
                && dir.len() == 10
                && ok_file(name)
                && name != "manifest.json"
        }
    }
}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// `<staging>/<job_id>/` を 0700 で新しく作る。既にあれば失敗（前のジョブの残りを使い回さない）。
    pub fn create(staging: &Path, job_id: &str) -> Result<Self, ExportError> {
        if !is_safe_name(job_id) {
            return Err(internal("ジョブ ID が作業領域の名前にできません"));
        }
        let root = staging.join(job_id);
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|_| internal("作業領域を作れません"))?;
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// 新しいファイルを書く（既にあれば失敗。リンクはたどらない）。`rel` は `/` 区切りの相対パス。
    pub fn write_new(&self, rel: &str, bytes: &[u8]) -> Result<(), ExportError> {
        if !is_allowed_rel(rel) {
            return Err(internal("作業領域に置けないパスです"));
        }
        let path = self.root.join(rel);
        if let Some((dir, _)) = rel.split_once('/') {
            self.ensure_dir(dir)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&path)
            .map_err(|_| internal("作業領域にファイルを作れません"))?;
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .map_err(|_| internal("作業領域に書けません"))
    }

    /// 日付のサブフォルダ（0700）。自分が作ったものだけを使い、リンクや通常のファイルなら失敗する。
    fn ensure_dir(&self, name: &str) -> Result<(), ExportError> {
        let path = self.root.join(name);
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = std::fs::symlink_metadata(&path)
                    .map_err(|_| internal("作業領域のフォルダを確かめられません"))?;
                if meta.is_dir() {
                    Ok(())
                } else {
                    Err(internal("作業領域のフォルダがフォルダではありません"))
                }
            }
            Err(_) => Err(internal("作業領域にフォルダを作れません")),
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // 作った作業領域だけを消す（`remove_dir_all` はリンクをたどらない）。残っても、起動時の掃除が消す
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_planned_shapes_are_allowed() {
        for ok in [
            "manifest.json",
            "091100_101.jpg",
            "000000_1.jpg",
            "2030-03-10/091100_101.jpg",
        ] {
            assert!(is_allowed_rel(ok), "{ok}");
        }
        for bad in [
            "",
            "../x.jpg",
            "a.jpg",
            "09110_101.jpg",
            "091100_x.jpg",
            "091100_101.jpg/",
            "/091100_101.jpg",
            "2030-03-10/manifest.json",
            "2030-03-10-11/091100_101.jpg",
            "2030-03-10/2030-03-11/091100_101.jpg",
            "x/091100_101.jpg",
            "2030-03-10/../091100_101.jpg",
            "091100_101.JPG",
            "manifest.json/x",
        ] {
            assert!(!is_allowed_rel(bad), "{bad:?}");
        }
    }
}
