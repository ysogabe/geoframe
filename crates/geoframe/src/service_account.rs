//! 明示されたサービスアカウントの通常ファイルだけを読む。token_uri は使用しない。
use serde_json::Value;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fmt, fs,
    io::{self, Read},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
pub(crate) const MAX_KEY_BYTES: u64 = 64 << 10;
#[derive(Clone, Copy)]
pub(crate) enum Context {
    Drive,
    Ai,
}
/// 鍵ファイルの失敗の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// 通常のファイルではない（ディレクトリなど）、または大きすぎる。
    NotRegular,
    /// 読めない（`ErrorKind` だけを持つ）。
    Unreadable(io::ErrorKind),
    /// 権限がゆるい（所有者以外にも読める）か、所有者が実行ユーザーと違う。AI のクラウド鍵だけ。
    Insecure,
    /// JSON として読めない、または `client_email`・`private_key` が文字列でない。
    Malformed,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegular => f.write_str("鍵ファイルが通常のファイルではないか大きすぎます"),
            Self::Unreadable(kind) => write!(f, "鍵ファイルを読めません（{kind}）"),
            Self::Insecure => f.write_str(
                "鍵ファイルの権限がゆるいか、所有者が実行ユーザーと違います（0600 で実行ユーザーの所有にしてください）",
            ),
            Self::Malformed => {
                f.write_str("鍵ファイルが JSON として不正か、必要な項目がありません")
            }
        }
    }
}

/// 鍵ファイルから読む 2 つの値。
pub(crate) struct ServiceAccount {
    pub(crate) email: String,
    pub(crate) private_key: String,
}

/// 鍵ファイルを読む。通常のファイルだけ（`DRIVE_KEY.exists()` と同じくリンクは辿る）。
/// 他のユーザーに読める権限のとき、Drive はプロセスで 1 回だけ警告して使う（値は出さない）。
/// AI のクラウド鍵は、権限がゆるい（`mode & 0o077`）か所有者が実行ユーザーと違えば、読まずに
/// [`KeyError::Insecure`] で拒否する（開いた後の fstat で判定する）。
pub(crate) fn load_service_account(
    path: &Path,
    warned: &AtomicBool,
    context: Context,
) -> Result<ServiceAccount, KeyError> {
    load_as(path, warned, context, crate::sysinfo::effective_uid())
}

fn load_as(
    path: &Path,
    warned: &AtomicBool,
    context: Context,
    run_uid: u32,
) -> Result<ServiceAccount, KeyError> {
    // 開く前に種類を見る（FIFO を開くと、書き手が来るまで止まる）。リンクは辿る
    let before = fs::metadata(path).map_err(|e| KeyError::Unreadable(e.kind()))?;
    if !before.is_file() {
        return Err(KeyError::NotRegular);
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| KeyError::Unreadable(e.kind()))?;
    let meta = file
        .metadata()
        .map_err(|e| KeyError::Unreadable(e.kind()))?;
    if !meta.is_file() || meta.len() > MAX_KEY_BYTES {
        return Err(KeyError::NotRegular);
    }
    let loose = meta.permissions().mode() & 0o077 != 0;
    match context {
        Context::Ai if loose || meta.uid() != run_uid => return Err(KeyError::Insecure),
        Context::Drive if loose && !warned.swap(true, Ordering::SeqCst) => tracing::warn!(
            "[drive] 鍵ファイルが所有者以外にも読める権限です（{:03o}）。0600 にしてください",
            meta.permissions().mode() & 0o777
        ),
        _ => {}
    }
    let mut text = String::new();
    file.take(MAX_KEY_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| KeyError::Unreadable(e.kind()))?;
    if text.len() as u64 > MAX_KEY_BYTES {
        return Err(KeyError::NotRegular);
    }
    let json: Value = serde_json::from_str(&text).map_err(|_| KeyError::Malformed)?;
    let field = |name: &str| {
        json.get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(KeyError::Malformed)
    };
    Ok(ServiceAccount {
        email: field("client_email")?,
        private_key: field("private_key")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const KEY: &str = r#"{"client_email":"a@example.invalid","private_key":"x"}"#;

    fn write(dir: &TempDir, name: &str, mode: u32) -> std::path::PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, KEY).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        p
    }
    fn me() -> u32 {
        crate::sysinfo::effective_uid()
    }
    fn ai(p: &Path, uid: u32) -> Result<ServiceAccount, KeyError> {
        load_as(p, &AtomicBool::new(false), Context::Ai, uid)
    }

    #[test]
    fn ai_key_must_be_owner_only_and_owned_by_the_running_user() {
        let dir = TempDir::new("sa-strict");
        assert!(ai(&write(&dir, "ok", 0o600), me()).is_ok());
        assert!(ai(&write(&dir, "ro", 0o400), me()).is_ok());
        for mode in [0o644, 0o640, 0o604, 0o660, 0o666] {
            let p = write(&dir, &format!("m{mode:o}"), mode);
            assert_eq!(ai(&p, me()).err(), Some(KeyError::Insecure), "{mode:o}");
        }
        // 所有者が実行ユーザーと違う（別 uid を実行ユーザーに見立てる）。
        let p = write(&dir, "other-owner", 0o600);
        assert_eq!(ai(&p, me().wrapping_add(1)).err(), Some(KeyError::Insecure));
    }

    #[test]
    fn ai_key_through_a_link_is_judged_on_the_target() {
        let dir = TempDir::new("sa-link");
        let strict = write(&dir, "strict", 0o600);
        let loose = write(&dir, "loose", 0o644);
        let (a, b) = (dir.path().join("to-strict"), dir.path().join("to-loose"));
        std::os::unix::fs::symlink(&strict, &a).unwrap();
        std::os::unix::fs::symlink(&loose, &b).unwrap();
        // リンクは辿る（設計文書の方針）。判定は実体の権限で行う。
        assert!(ai(&a, me()).is_ok());
        assert_eq!(ai(&b, me()).err(), Some(KeyError::Insecure));
    }

    #[test]
    fn drive_key_with_loose_permissions_still_loads_and_warns_once() {
        let dir = TempDir::new("sa-drive");
        let p = write(&dir, "loose", 0o644);
        let warned = AtomicBool::new(false);
        assert!(load_as(&p, &warned, Context::Drive, me().wrapping_add(1)).is_ok());
        assert!(warned.load(Ordering::SeqCst));
    }
}
