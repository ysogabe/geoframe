//! 実行枠（design.md 4.1 の手順 1、8 章「同時実行」）。`<state-dir>/export.lock` の `flock(LOCK_EX | LOCK_NB)`。
//!
//! サービス（`serve`）と CLI（`run`・`recover`）で共通で、書き出しと `dry_run` はこれを取ってから動く。取れなければ
//! 「実行中」（[`Slot::Busy`]。HTTP は 409、`run` は終了コード 4）。
//!
//! ロックは開いたファイルに付く（`flock` は open file description ごと）。ガード（[`SlotGuard`]）を手放す・プロセスが
//! 終わる・パニックで巻き戻る、のどれでも外れる。ファイルは `O_CLOEXEC`（Rust の既定）で開くので、子プロセスは
//! ロックを引き継がない（親が死んだあとに子がロックを握り続けない）。
//!
//! 同じプロセスの中で 2 回取ると、2 回目は取れない（別のプロセスと同じ振る舞い）。試験はこれで「別のプロセスが持っている」を作る。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, ExportError};
use crate::sys;

/// 実行枠のファイル名（状態ディレクトリの直下）。
pub const LOCK_FILE: &str = "export.lock";

/// 実行枠を持っている印。手放すと（`Drop`）枠が空く。ジョブを流す関数は、この参照を受け取って、枠を持ったまま動くことを
/// 型で示す。
#[derive(Debug)]
pub struct SlotGuard {
    _file: File,
    path: PathBuf,
}

impl SlotGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 実行枠を取ろうとした結果。
#[derive(Debug)]
pub enum Slot {
    Got(SlotGuard),
    /// 別のジョブ（サービスか CLI）が持っている。
    Busy,
}

fn internal(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::Internal, detail)
}

/// 取れれば [`Slot::Got`]、他が持っていれば [`Slot::Busy`]。ファイルが無ければ 0600 で作る（リンクはたどらない）。
/// 開けない・通常のファイルでない・flock が想定外の失敗をしたときは `internal`（「実行中」と取り違えない）。
pub fn try_acquire(state_dir: &Path) -> Result<Slot, ExportError> {
    let path = state_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&path)
        .map_err(|_| internal("実行枠のファイルを開けません"))?;
    let meta = file
        .metadata()
        .map_err(|_| internal("実行枠のファイルを確かめられません"))?;
    if !meta.is_file() || meta.uid() != sys::euid() {
        return Err(internal("実行枠のファイルが通常のファイルではありません"));
    }
    match sys::try_flock_exclusive(&file) {
        Ok(true) => Ok(Slot::Got(SlotGuard { _file: file, path })),
        Ok(false) => Ok(Slot::Busy),
        Err(e) => Err(internal(match e.kind() {
            io::ErrorKind::PermissionDenied => "実行枠の flock が許されません",
            _ => "実行枠の flock に失敗しました",
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-tmp")
            .join(format!("lock-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn only_one_holder_at_a_time_and_dropping_frees_it() {
        let d = dir("one");
        let Slot::Got(a) = try_acquire(&d).unwrap() else {
            panic!("取れるはず")
        };
        assert!(matches!(try_acquire(&d).unwrap(), Slot::Busy));
        assert!(matches!(try_acquire(&d).unwrap(), Slot::Busy));
        drop(a);
        assert!(matches!(try_acquire(&d).unwrap(), Slot::Got(_)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_panic_releases_the_slot() {
        let d = dir("panic");
        let d2 = d.clone();
        let handle = std::thread::spawn(move || {
            let Slot::Got(_guard) = try_acquire(&d2).unwrap() else {
                panic!("取れるはず")
            };
            panic!("試験のパニック");
        });
        assert!(handle.join().is_err());
        assert!(matches!(try_acquire(&d).unwrap(), Slot::Got(_)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_symlink_in_place_of_the_lock_file_is_refused_not_followed() {
        let d = dir("link");
        let target = d.join("elsewhere");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, d.join(LOCK_FILE)).unwrap();
        assert_eq!(
            try_acquire(&d).unwrap_err().code,
            ErrorCode::Internal,
            "リンクは開かない"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
