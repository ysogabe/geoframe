//! 明示されたサービスアカウントの通常ファイルだけを読む。token_uri は使用しない。
use serde_json::Value;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
    /// JSON として読めない、または `client_email`・`private_key` が文字列でない。
    Malformed,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegular => f.write_str("鍵ファイルが通常のファイルではないか大きすぎます"),
            Self::Unreadable(kind) => write!(f, "鍵ファイルを読めません（{kind}）"),
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
/// 他のユーザーに読める権限なら、プロセスで 1 回だけ警告する（値は出さない）。
pub(crate) fn load_service_account(
    path: &Path,
    warned: &AtomicBool,
    context: Context,
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
    if meta.permissions().mode() & 0o077 != 0 && !warned.swap(true, Ordering::SeqCst) {
        match context {
            Context::Drive => tracing::warn!(
                "[drive] 鍵ファイルが所有者以外にも読める権限です（{:03o}）。0600 にしてください",
                meta.permissions().mode() & 0o777
            ),
            Context::Ai => tracing::warn!("[ai] 認証ファイルを0600にしてください"),
        }
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
