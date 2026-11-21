//! rclone の設定ファイル（INI 形式）の起動時の検査（design.md 4.2・7 章・9 章）。
//!
//! rclone の共用の client_id は 2026 年中に使えなくなるので、このサービスは専用の client_id で認可した設定だけを使う。
//! 設定ファイルの接頭辞の remote 名のセクションに `client_id` と `client_secret` が**空でない値で**入っていなければ起動しない。
//! あわせて、設定ファイルとその親ディレクトリが実行ユーザーの持ち物で、グループ・その他から読めないことを確かめる
//! （ファイルは 0600 相当、ディレクトリは 0700 相当）。
//!
//! 設定ファイルの値（トークン・client_id・client_secret を含む）は、検査の結果にもエラーの表示にも残さない。
//! 持つのは、キーの名前と、ファイルかディレクトリかの種類だけ。

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// 読む大きさの上限（rclone の設定は数 KB。巨大なファイルを読まない）。
pub const CONFIG_MAX_BYTES: u64 = 64 * 1024;

/// 専用の値が入っているべきキー。
const REQUIRED_KEYS: [&str; 2] = ["client_id", "client_secret"];

/// 持ち物・権限を確かめた対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    File,
    Directory,
}

impl Target {
    const fn text(self) -> &'static str {
        match self {
            Self::File => "設定ファイル",
            Self::Directory => "設定ファイルの親ディレクトリ",
        }
    }
}

/// 検査に通らなかった理由。値（パス・設定の中身）は含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// 開けない・読めない（種類だけ。OS のメッセージにパスが入りうるので持たない）。
    Unreadable(Target, io::ErrorKind),
    /// 通常のファイル（親はディレクトリ）でない。シンボリックリンクも含む。
    WrongKind(Target),
    /// 実行ユーザーの持ち物でない。
    WrongOwner(Target),
    /// グループ・その他から読める、または書ける（ファイルは 0600、ディレクトリは 0700 相当にする）。
    TooPermissive(Target),
    TooLarge,
    NotUtf8,
    /// 接頭辞の remote 名のセクションが無い。
    MissingSection,
    /// セクションに必要なキーが無い。
    MissingKey(&'static str),
    /// キーはあるが値が空。
    EmptyValue(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(t, kind) => write!(f, "{}を読めません（{kind}）", t.text()),
            Self::WrongKind(t) => write!(f, "{}が通常のファイル・ディレクトリではありません", t.text()),
            Self::WrongOwner(t) => write!(f, "{}が実行ユーザーの持ち物ではありません", t.text()),
            Self::TooPermissive(Target::File) => {
                f.write_str("設定ファイルの権限が広すぎます（0600 にしてください）")
            }
            Self::TooPermissive(Target::Directory) => {
                f.write_str("設定ファイルの親ディレクトリの権限が広すぎます（0700 にしてください）")
            }
            Self::TooLarge => write!(f, "設定ファイルが {CONFIG_MAX_BYTES} バイトを超えています"),
            Self::NotUtf8 => f.write_str("設定ファイルが UTF-8 ではありません"),
            Self::MissingSection => f.write_str(
                "設定ファイルに、接頭辞の remote 名のセクションがありません（専用の client_id で認可した設定を使ってください）",
            ),
            Self::MissingKey(k) => write!(
                f,
                "設定ファイルの remote のセクションに {k} がありません（専用の client_id で認可し直してください）"
            ),
            Self::EmptyValue(k) => write!(
                f,
                "設定ファイルの remote のセクションの {k} が空です（専用の client_id で認可し直してください）"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// 実行ユーザーの実効 UID。
pub fn effective_uid() -> u32 {
    // SAFETY: `geteuid` は引数を取らず、失敗しない。
    unsafe { libc::geteuid() }
}

/// 実行ユーザー（実効 UID）に対する検査。
pub fn check_rclone_config(path: &Path, remote: &str) -> Result<(), ConfigError> {
    check_rclone_config_for_uid(path, remote, effective_uid())
}

/// 持ち主を `uid` として検査する（試験で「別のユーザーの持ち物」を作るため。通常は [`check_rclone_config`]）。
///
/// 順序: 親ディレクトリ → ファイルの持ち主・権限（開いたハンドルで見る。確かめた後に差し替えられない）→ 中身。
/// 親やファイルが安全でなければ、中身は読まない。
pub fn check_rclone_config_for_uid(path: &Path, remote: &str, uid: u32) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(ConfigError::WrongKind(Target::Directory))?;
    let dir = std::fs::metadata(parent)
        .map_err(|e| ConfigError::Unreadable(Target::Directory, e.kind()))?;
    if !dir.is_dir() {
        return Err(ConfigError::WrongKind(Target::Directory));
    }
    check_owner_and_mode(Target::Directory, dir.uid(), dir.mode(), uid)?;

    // シンボリックリンクはたどらない（O_NOFOLLOW）。持ち主・権限は開いたハンドルで見る
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => ConfigError::WrongKind(Target::File),
            _ => ConfigError::Unreadable(Target::File, e.kind()),
        })?;
    let meta = file
        .metadata()
        .map_err(|e| ConfigError::Unreadable(Target::File, e.kind()))?;
    if !meta.is_file() {
        return Err(ConfigError::WrongKind(Target::File));
    }
    check_owner_and_mode(Target::File, meta.uid(), meta.mode(), uid)?;
    if meta.len() > CONFIG_MAX_BYTES {
        return Err(ConfigError::TooLarge);
    }
    let text = read_limited(&mut file)?;
    check_section(&text, remote)
}

fn check_owner_and_mode(
    target: Target,
    owner: u32,
    mode: u32,
    uid: u32,
) -> Result<(), ConfigError> {
    if owner != uid {
        return Err(ConfigError::WrongOwner(target));
    }
    if mode & 0o077 != 0 {
        return Err(ConfigError::TooPermissive(target));
    }
    Ok(())
}

fn read_limited(file: &mut File) -> Result<String, ConfigError> {
    let mut bytes = Vec::new();
    file.take(CONFIG_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ConfigError::Unreadable(Target::File, e.kind()))?;
    if bytes.len() as u64 > CONFIG_MAX_BYTES {
        return Err(ConfigError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::NotUtf8)
}

/// `[remote]` のセクションに、必須のキーが空でない値で入っているか。値は返さない。
/// 同じセクションが 2 度あれば、後のものが前のものを上書きする（行ごとに最後の値を採る）。
fn check_section(text: &str, remote: &str) -> Result<(), ConfigError> {
    let mut in_section = false;
    let mut found = false;
    // 各必須キーの、最後に見た値が空でないか（None = まだ無い）
    let mut state: [Option<bool>; 2] = [None, None];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = inner.trim() == remote;
            found |= in_section;
            continue;
        }
        if !in_section {
            continue;
        }
        // `key = value`。= の無い行・形の崩れた行は読み飛ばす（行の中身はエラーに写さない）
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(i) = REQUIRED_KEYS.iter().position(|k| *k == key.trim()) {
            state[i] = Some(!value.trim().is_empty());
        }
    }
    if !found {
        return Err(ConfigError::MissingSection);
    }
    for (key, state) in REQUIRED_KEYS.iter().zip(state) {
        match state {
            None => return Err(ConfigError::MissingKey(key)),
            Some(false) => return Err(ConfigError::EmptyValue(key)),
            Some(true) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "[gdrive]\ntype = drive\nclient_id = A\nclient_secret = B\ntoken = {}\n";

    #[test]
    fn a_section_with_both_values_passes() {
        assert_eq!(check_section(GOOD, "gdrive"), Ok(()));
        // 空白・コメント・別セクション・順序は問わない
        let text =
            "# x\n[other]\nclient_id =\n\n[ gdrive ]\n; c\n  client_secret=B  \nclient_id\t= A\n";
        assert_eq!(check_section(text, "gdrive"), Ok(()));
    }

    #[test]
    fn missing_or_empty_values_are_refused_by_key_name() {
        assert_eq!(
            check_section("", "gdrive"),
            Err(ConfigError::MissingSection)
        );
        assert_eq!(
            check_section("[other]\nclient_id = A\nclient_secret = B\n", "gdrive"),
            Err(ConfigError::MissingSection)
        );
        assert_eq!(
            check_section("[gdrive]\nclient_secret = B\n", "gdrive"),
            Err(ConfigError::MissingKey("client_id"))
        );
        assert_eq!(
            check_section("[gdrive]\nclient_id = A\n", "gdrive"),
            Err(ConfigError::MissingKey("client_secret"))
        );
        assert_eq!(
            check_section("[gdrive]\nclient_id =\nclient_secret = B\n", "gdrive"),
            Err(ConfigError::EmptyValue("client_id"))
        );
        assert_eq!(
            check_section("[gdrive]\nclient_id = A\nclient_secret =   \n", "gdrive"),
            Err(ConfigError::EmptyValue("client_secret"))
        );
        // 別のセクションの値は数えない（remote 名が部分一致でも）
        assert_eq!(
            check_section("[gdrive2]\nclient_id = A\nclient_secret = B\n", "gdrive"),
            Err(ConfigError::MissingSection)
        );
        // セクションの外（先頭）の値も数えない
        assert_eq!(
            check_section(
                "client_id = A\nclient_secret = B\n[gdrive]\ntype = drive\n",
                "gdrive"
            ),
            Err(ConfigError::MissingKey("client_id"))
        );
    }

    #[test]
    fn a_later_empty_assignment_overrides_an_earlier_value() {
        let text = "[gdrive]\nclient_id = A\nclient_secret = B\nclient_id =\n";
        assert_eq!(
            check_section(text, "gdrive"),
            Err(ConfigError::EmptyValue("client_id"))
        );
    }

    #[test]
    fn messages_name_keys_and_kinds_only() {
        for e in [
            ConfigError::MissingSection,
            ConfigError::MissingKey("client_id"),
            ConfigError::EmptyValue("client_secret"),
            ConfigError::TooPermissive(Target::File),
            ConfigError::TooPermissive(Target::Directory),
            ConfigError::WrongOwner(Target::File),
            ConfigError::WrongKind(Target::Directory),
            ConfigError::TooLarge,
            ConfigError::NotUtf8,
            ConfigError::Unreadable(Target::File, io::ErrorKind::NotFound),
        ] {
            assert!(!e.to_string().is_empty());
        }
    }
}
