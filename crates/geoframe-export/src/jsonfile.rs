//! JSON のファイルをアトミックに書く・上限付きで読む（design.md 7 章）。
//!
//! ジョブの記録（`jobs/<job_id>.json`。T5）と公開の記録（`published.json`。[`crate::published`]）は、同じこの書き方にする:
//! 同じディレクトリの一時ファイル（0600、新規作成）に書いて `fsync` し、`rename` で置き換え、ディレクトリも `fsync` する。
//! 途中で止まっても、読めるのは「前の完全な記録」か「新しい完全な記録」のどちらかだけになる。
//!
//! 読むほうは、リンクをたどらず（`O_NOFOLLOW`）、上限のバイト数までしか読まない。

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `value` を整形して（末尾に改行）`path` へアトミックに書く。`path` の親ディレクトリは存在すること。
pub fn write_json_atomic(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "親ディレクトリがありません"))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ファイル名が不正です"))?;
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');

    let tmp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        // rename を永続にする
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        // rename の前に失敗したときの一時ファイル（自分で作ったもの）だけを片付ける
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// 読めなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// ファイルが無い。
    NotFound,
    /// 開けない・読めない・通常のファイルでない。
    Io,
    /// 上限より大きい。
    TooLarge,
    /// JSON として読めない。
    Syntax,
}

/// `max_bytes` 以下のファイルを JSON として読む。
pub fn read_json(path: &Path, max_bytes: u64) -> Result<Value, ReadError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ReadError::NotFound
            } else {
                ReadError::Io
            }
        })?;
    if !file.metadata().map_err(|_| ReadError::Io)?.is_file() {
        return Err(ReadError::Io);
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ReadError::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| ReadError::Syntax)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    /// 試験の作業用ディレクトリの親（ビルドの出力の下。`/tmp` は使わない）。
    fn unit_tmp() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/unit-tmp")
    }

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = unit_tmp().join(format!("jsonfile-{name}-{}", std::process::id()));
            // 自分で作る名前（プロセス ID つき）。前のものは消さない（残っていれば作成に失敗して気づく）
            std::fs::create_dir_all(&p).unwrap();
            assert_eq!(
                std::fs::read_dir(&p).unwrap().count(),
                0,
                "前の試験の残りがあります"
            );
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_atomically_with_0600_and_leaves_no_temp_file() {
        let d = Dir::new("write");
        let path = d.0.join("a.json");
        write_json_atomic(&path, &json!({"k": 1})).unwrap();
        write_json_atomic(&path, &json!({"k": 2})).unwrap();
        assert_eq!(read_json(&path, 1024).unwrap(), json!({"k": 2}));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = std::fs::read_dir(&d.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["a.json"]);
    }

    #[test]
    fn a_missing_parent_fails_without_creating_anything() {
        let d = Dir::new("noparent");
        let path = d.0.join("nope").join("a.json");
        assert!(write_json_atomic(&path, &json!({})).is_err());
        assert!(!d.0.join("nope").exists());
    }

    #[test]
    fn reading_distinguishes_missing_large_bad_and_links() {
        let d = Dir::new("read");
        let path = d.0.join("a.json");
        assert_eq!(read_json(&path, 10), Err(ReadError::NotFound));
        std::fs::write(&path, b"{\"a\": 1}").unwrap();
        assert_eq!(read_json(&path, 3), Err(ReadError::TooLarge));
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(read_json(&path, 100), Err(ReadError::Syntax));
        let link = d.0.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(read_json(&link, 100), Err(ReadError::Io));
        assert_eq!(read_json(&d.0, 100), Err(ReadError::Io));
    }
}
