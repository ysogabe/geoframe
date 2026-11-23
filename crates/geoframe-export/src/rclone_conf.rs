//! `import-credentials` の取り込み元（rclone の設定ファイル rclone.conf。INI 形式）の読み取り（design.md 4.2・7 章・9 章、
//! docs/note-photo-export/drive-api.md 2 章・6 章）。
//!
//! Drive の操作は Drive API を直接呼ぶ（資格情報は専用の JSON のファイル。[`crate::drive::auth`]）。この読み取りは、これまでの
//! rclone.conf から、専用の `client_id`・`client_secret` と `token` の `refresh_token` を 1 回だけ取り出すためと、再認可のとき
//! （別の機体で `rclone authorize` して得たトークンを一時的な rclone.conf に書いて取り込む）に使う。
//!
//! rclone の共用の client_id は 2026 年中に使えなくなるので、専用の client_id で認可した設定だけを受け付ける。設定ファイルの
//! 指定した remote 名のセクションに `client_id` と `client_secret` が**空でない値で**入っていなければ読まない。あわせて、
//! 設定ファイルとその親ディレクトリの持ち主・権限を確かめる（ファイルは 0600 相当、ディレクトリは 0700 相当）。
//!
//! 設定ファイルの値（トークン・client_id・client_secret を含む）は、結果にもエラーの表示にも残さない。
//! エラーが持つのは、キーの名前と、ファイルかディレクトリかの種類と、行の番号だけ。
//!
//! 読み方は、rclone が書き出す形（`[remote]` と `key = value`）だけを受け付ける。rclone の INI の読み手（goconfig）は
//! `:` 区切り・引用符つきのキーと値・`%(key)s` の展開・同じキーの再指定（後勝ち）なども解釈するので、
//! ここで「入っている」と見た値と rclone が使う値がずれうる。解釈が分かれうる形は、読み違えないように断る
//! （[`SyntaxIssue`]）。
//!
//! 開き方: 親ディレクトリを `O_DIRECTORY | O_NOFOLLOW` で開き、そのハンドルで持ち主・権限を見て、そこから `openat` で
//! 設定ファイルを `O_NOFOLLOW | O_NONBLOCK` で開く（パスを引き直さない。FIFO で止まらない）。
//!
//! root が別のユーザーの設定を取り込むとき（`import-credentials`）は、パスの途中のディレクトリも確かめる。そのときは、**根（`/`）から
//! 1 段ずつ** `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` で開き（[`DirChain`]）、開いたハンドルの `fstat` で各ディレクトリを
//! 見て、同じ親のハンドルから設定ファイルを開く（検査した経路と開いた経路が同じ。途中のシンボリックリンクは断る。`..`・`.`・
//! 空の要素・相対パスも断る）。

use std::ffi::CString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
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
    /// rclone が書き出す形ではなく、読み手によって解釈が分かれうる（`line` は 1 から数えた行の番号。行の中身は持たない）。
    Syntax {
        line: usize,
        issue: SyntaxIssue,
    },
}

/// [`ConfigError::Syntax`] の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxIssue {
    /// セクションの前（またはセクション名が空）の行。
    OutsideSection,
    /// `key = value` の形ではない行（`:` 区切り・引用符つきのキー・`=` が無い行・壊れたセクション行など）。
    BadLine,
    /// 接頭辞の remote のセクションが 2 度ある。
    DuplicateSection,
    /// remote のセクションの中で、同じキーが 2 度ある（後勝ちになる）。
    DuplicateKey,
    /// 専用の値が引用符で始まる、または `%(` を含む（空の引用・展開が読み手によって別の値になる）。
    QuotedOrInterpolated(&'static str),
    /// どのセクションでも、値が引用符で始まる（rclone はこの形で書かない。閉じ忘れると、読み手がその行を丸ごと
    /// エラーに載せることがあり、資格情報が標準エラーに出うる）。DEFAULT の見出しの無いキーも受け付けない。
    QuotedValue,
}

impl SyntaxIssue {
    const fn text(self) -> &'static str {
        match self {
            Self::OutsideSection => "セクションの外にある行",
            Self::BadLine => "`key = value` の形ではない行",
            Self::DuplicateSection => "remote のセクションが 2 度ある",
            Self::DuplicateKey => "remote のセクションで同じキーが 2 度ある",
            Self::QuotedValue => {
                "値が引用符で始まる行（専用の設定は rclone が書く `key = value` の形だけを受け付ける）"
            }
            Self::QuotedOrInterpolated(_) => {
                "client_id・client_secret の値が引用符で始まるか `%(` を含む"
            }
        }
    }
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
                f.write_str(
                    "設定ファイルの親ディレクトリ（root が取り込むときは、その上の各ディレクトリも）の権限が広すぎます（親は 0700、上はグループ・その他が書けない形にしてください）",
                )
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
            Self::Syntax { line, issue } => write!(
                f,
                "設定ファイルの {line} 行目を読めません（{}。rclone が書き出す `[remote]` と `key = value` の形にしてください）",
                issue.text()
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

/// 設定ファイルを安全に開いて、中身の文字列を返す。`any_owner` が真なら、持ち主は問わない（権限は問う）。
fn read_config_text(path: &Path, uid: u32, any_owner: bool) -> Result<String, ConfigError> {
    // 実行ユーザーが本当に root のときだけ（試験の「root のつもり」では、鎖の検査をしない）
    read_config_text_with(path, uid, any_owner, any_owner && effective_uid() == 0)
}

/// [`read_config_text`] の、パスの途中の検査（`chain`）を呼び出し側が決める形。
///
/// `chain` が真なら、**根（`/`）から 1 段ずつ**開いたハンドルで読む（[`DirChain`]）: パスを引き直して検査しないので、検査した
/// ディレクトリと実際に開いたファイルの経路がずれない。途中にシンボリックリンクがあれば断り、`..`・`.`・空の要素を含むパスと、
/// 絶対でないパスも断る。
fn read_config_text_with(
    path: &Path,
    uid: u32,
    any_owner: bool,
    chain: bool,
) -> Result<String, ConfigError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(ConfigError::WrongKind(Target::Directory))?;
    let name = path
        .file_name()
        .ok_or(ConfigError::WrongKind(Target::File))?;

    // 親ディレクトリ自身がシンボリックリンクなら断る（O_NOFOLLOW）。持ち主・権限は開いたハンドルで見る
    let (dir, chain) = if chain {
        let c = DirChain::open_file_parent(path)?;
        (
            c.parent
                .try_clone()
                .map_err(|e| ConfigError::Unreadable(Target::Directory, e.kind()))?,
            Some(c),
        )
    } else {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|e| match e.raw_os_error() {
                // O_NOFOLLOW でリンクを開こうとすると、ELOOP か ENOTDIR（O_DIRECTORY と合わせたとき）になる
                Some(libc::ELOOP | libc::ENOTDIR) => ConfigError::WrongKind(Target::Directory),
                _ => ConfigError::Unreadable(Target::Directory, e.kind()),
            })?;
        (dir, None)
    };
    let dir_meta = dir
        .metadata()
        .map_err(|e| ConfigError::Unreadable(Target::Directory, e.kind()))?;
    if !dir_meta.is_dir() {
        return Err(ConfigError::WrongKind(Target::Directory));
    }
    check_owner_and_mode(
        Target::Directory,
        dir_meta.uid(),
        dir_meta.mode(),
        uid,
        any_owner,
    )?;

    // 同じハンドルの下から開く（パスを引き直さない）。シンボリックリンクはたどらない。FIFO などで待たないよう O_NONBLOCK で
    // 開き、通常のファイルだと確かめてから読む（通常のファイルの読み取りに O_NONBLOCK は影響しない）
    let mut file = open_at_nofollow(&dir, name.as_bytes())?;
    let meta = file
        .metadata()
        .map_err(|e| ConfigError::Unreadable(Target::File, e.kind()))?;
    if !meta.is_file() {
        return Err(ConfigError::WrongKind(Target::File));
    }
    check_owner_and_mode(Target::File, meta.uid(), meta.mode(), uid, any_owner)?;
    // root が、別のユーザーの持ち物の設定を取り込むとき（`import-credentials`）は、パスの途中のディレクトリも確かめる。
    // 途中のディレクトリを書き換えられるユーザーが、パスの先を別のファイルに差し替えて、root に読ませられないように。
    // 検査するのは、**ファイルを開いた経路そのもの**（根から 1 段ずつ開いたハンドル）。パスを引き直して検査すると、途中のリンクを
    // 解決した先を検査して、リンクを置いたディレクトリが検査から外れる
    if let Some(chain) = &chain {
        chain.check(meta.uid())?;
    }
    if meta.len() > CONFIG_MAX_BYTES {
        return Err(ConfigError::TooLarge);
    }
    read_limited(&mut file)
}

/// 根（`/`）から 1 段ずつ開いたディレクトリの連なり。**パスを引き直さない**: 各段を、前の段のハンドルから
/// `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` で開き、開いたハンドルの `fstat` で持ち主・権限を控える。途中にシンボリックリンクが
/// あれば断る。最後の段（設定ファイルの親）のハンドルから、同じ連なりのまま設定ファイルを開く。
pub struct DirChain {
    /// 最後の段（設定ファイルの親ディレクトリ）のハンドル。
    parent: File,
    /// 親より上の各段（根から順）の（持ち主, モード）。
    ancestors: Vec<(u32, u32)>,
}

/// 絶対パスを、根からの各要素に分ける。`..`・`.`・空の要素（`//`・末尾の `/`）・相対パスは断る。`/` だけなら要素なし。
fn absolute_elements(path: &Path) -> Result<Vec<&[u8]>, ConfigError> {
    let bytes = path.as_os_str().as_bytes();
    let rest = bytes
        .strip_prefix(b"/")
        .ok_or(ConfigError::WrongKind(Target::Directory))?;
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    rest.split(|b| *b == b'/')
        .map(|c| {
            if c.is_empty() || c == b"." || c == b".." {
                Err(ConfigError::WrongKind(Target::Directory))
            } else {
                Ok(c)
            }
        })
        .collect()
}

impl DirChain {
    /// `dir`（絶対パス）を、根から 1 段ずつ開く。
    pub fn open(dir: &Path) -> Result<Self, ConfigError> {
        Self::open_elements(&absolute_elements(dir)?)
    }

    /// 設定ファイル `path`（絶対パス）の親までを、根から 1 段ずつ開く（ファイル名の要素は開かない）。
    fn open_file_parent(path: &Path) -> Result<Self, ConfigError> {
        let mut elements = absolute_elements(path)?;
        // ファイルの名前（最後の要素）は、親のハンドルから別に開く
        elements.pop().ok_or(ConfigError::WrongKind(Target::File))?;
        Self::open_elements(&elements)
    }

    fn open_elements(elements: &[&[u8]]) -> Result<Self, ConfigError> {
        let unreadable = |e: io::Error| ConfigError::Unreadable(Target::Directory, e.kind());
        let mut current = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open("/")
            .map_err(unreadable)?;
        let mut metas = vec![current.metadata().map_err(unreadable)?];
        for element in elements {
            let c_name =
                CString::new(*element).map_err(|_| ConfigError::WrongKind(Target::Directory))?;
            // SAFETY: `current` は開いたままのディレクトリの FD、`c_name` は NUL で終わる文字列。戻り値の FD はこの関数の中で
            // 1 度だけ `File` に包み、閉じる責任を `File` に移す。
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let e = io::Error::last_os_error();
                return Err(match e.raw_os_error() {
                    // O_NOFOLLOW でリンクを開こうとすると、ELOOP か ENOTDIR（O_DIRECTORY と合わせたとき）になる
                    Some(libc::ELOOP | libc::ENOTDIR) => ConfigError::WrongKind(Target::Directory),
                    _ => unreadable(e),
                });
            }
            // SAFETY: `fd` は上で開いたばかりの、他に持ち主のいない FD。
            current = unsafe { File::from_raw_fd(fd) };
            metas.push(current.metadata().map_err(unreadable)?);
        }
        // 最後の段（親）は、別の検査（持ち主・権限）を受けるので、ここでは親より上だけを控える
        metas.pop();
        let ancestors = metas
            .into_iter()
            .map(|m| {
                if m.is_dir() {
                    Ok((m.uid(), m.mode()))
                } else {
                    Err(ConfigError::WrongKind(Target::Directory))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            parent: current,
            ancestors,
        })
    }

    /// 親より上の各ディレクトリを確かめる（近い順）。各ディレクトリは、持ち主が root か `file_owner`（設定ファイルの持ち主）で、
    /// グループ・その他が書き込めないこと（root の持ち物のスティッキービット付き〔`/tmp` など〕は、書き込めてもよい。他人の持ち物を
    /// 消せない）。親ディレクトリそのものは、[`read_credentials`] が別に検査する（持ち主・権限）。
    pub fn check(&self, file_owner: u32) -> Result<(), ConfigError> {
        for (uid, mode) in self.ancestors.iter().rev() {
            if *uid != 0 && *uid != file_owner {
                return Err(ConfigError::WrongOwner(Target::Directory));
            }
            let sticky_root = mode & 0o1000 != 0 && *uid == 0;
            if mode & 0o022 != 0 && !sticky_root {
                return Err(ConfigError::TooPermissive(Target::Directory));
            }
        }
        Ok(())
    }
}

/// `dir`（設定ファイルの親ディレクトリ）より上の、パスの途中の各ディレクトリを確かめる（root が取り込むとき）。根から 1 段ずつ
/// 開いたハンドルで見る（[`DirChain`]。パスを引き直したり、リンクを解決したりしない）: 途中にシンボリックリンクがあれば断り、
/// `..`・空の要素・相対パスも断る。各ディレクトリの条件は [`DirChain::check`]。
pub fn check_ancestors(dir: &Path, file_owner: u32) -> Result<(), ConfigError> {
    DirChain::open(dir)?.check(file_owner)
}

/// 取り込み（[`read_credentials`]）が取り出す 3 つの値。`Debug` は値を伏せる。
pub struct ImportedCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

impl std::fmt::Debug for ImportedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ImportedCredentials { .. }")
    }
}

/// rclone の設定ファイルの `[remote]` から、`client_id`・`client_secret`・`token` の `refresh_token` を取り出す
/// （`import-credentials`。docs/note-photo-export/drive-api.md 2 章）。形の検査は [`check_section`]。
///
/// 持ち主: 設定ファイルはサービスのユーザーの持ち物で、取り込みは root が sudo で 1 回だけ動かす（持ち主が実行ユーザーと違う）。
/// そこで **実行ユーザーが root のときは、持ち主を問わない**（権限は 0600／0700 相当を問う）。root でなければ、持ち主は実行ユーザー。
/// 値は、エラーにも表示にも残さない。
pub fn read_credentials(path: &Path, remote: &str) -> Result<ImportedCredentials, ConfigError> {
    read_credentials_for_uid(path, remote, effective_uid())
}

/// [`read_credentials`] の、実行ユーザーを `uid` にする形（試験用）。
pub fn read_credentials_for_uid(
    path: &Path,
    remote: &str,
    uid: u32,
) -> Result<ImportedCredentials, ConfigError> {
    let text = read_config_text(path, uid, uid == 0)?;
    credentials_from(&text, remote)
}

fn credentials_from(text: &str, remote: &str) -> Result<ImportedCredentials, ConfigError> {
    check_section(text, remote)?;
    // 形は検査済み（セクションは 1 つで、キーは重複せず、`key = value`）。値を拾う
    let mut in_section = false;
    let (mut id, mut secret, mut token) = (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = inner.trim() == remote;
            continue;
        }
        if !in_section || line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_owned();
        match key.trim() {
            "client_id" => id = Some(value),
            "client_secret" => secret = Some(value),
            "token" => token = Some(value),
            _ => {}
        }
    }
    let token = token.ok_or(ConfigError::MissingKey("token"))?;
    let refresh_token = serde_json::from_str::<serde_json::Value>(&token)
        .ok()
        .and_then(|v| v.get("refresh_token")?.as_str().map(str::to_owned))
        .filter(|t| !t.is_empty())
        .ok_or(ConfigError::EmptyValue("refresh_token"))?;
    Ok(ImportedCredentials {
        client_id: id.ok_or(ConfigError::MissingKey("client_id"))?,
        client_secret: secret.ok_or(ConfigError::MissingKey("client_secret"))?,
        refresh_token,
    })
}

/// `dir` の下の `name`（1 つの名前）を、シンボリックリンクをたどらず、待たずに開く。
fn open_at_nofollow(dir: &File, name: &[u8]) -> Result<File, ConfigError> {
    let c_name = CString::new(name).map_err(|_| ConfigError::WrongKind(Target::File))?;
    // SAFETY: `dir` は開いたままのディレクトリの FD、`c_name` は NUL で終わる文字列。戻り値の FD はこの関数の中で
    // 1 度だけ `File` に包み、閉じる責任を `File` に移す。
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::ELOOP) => ConfigError::WrongKind(Target::File),
            _ => ConfigError::Unreadable(Target::File, e.kind()),
        });
    }
    // SAFETY: `fd` は上で開いたばかりの、他に持ち主のいない FD。
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn check_owner_and_mode(
    target: Target,
    owner: u32,
    mode: u32,
    uid: u32,
    any_owner: bool,
) -> Result<(), ConfigError> {
    if owner != uid && !any_owner {
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

fn syntax(line: usize, issue: SyntaxIssue) -> ConfigError {
    ConfigError::Syntax { line, issue }
}

/// キーとして受け付ける形（rclone のオプション名: 英数字と `_`・`-`・`.`、先頭は英数字か `_`）。
/// `:`・引用符・空白を含むものは受け付けない（読み手によって、区切りやキーの範囲が変わる）。
fn is_plain_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

/// `[remote]` のセクションに、必須のキーが空でない値で入っているか。値は返さない。
///
/// 受け付けるのは、空行・`#`/`;` のコメント・`[name]`・`key = value` だけ。それ以外の行、セクションの前の行、
/// remote のセクションが 2 度あること、そのセクションの中で同じキーが 2 度あることは、固定のエラーで断る
/// （最初の `=` より前を、そのまま平文のキーとして扱える形に限る）。
fn check_section(text: &str, remote: &str) -> Result<(), ConfigError> {
    let mut seen_section = false;
    let mut in_section = false;
    let mut found = false;
    let mut keys: Vec<&str> = Vec::new();
    // 各必須キーの、値が空でないか（None = まだ無い）
    let mut state: [Option<bool>; 2] = [None, None];
    for (index, line) in text.lines().enumerate() {
        let no = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = inner.trim();
            if name.is_empty() {
                return Err(syntax(no, SyntaxIssue::OutsideSection));
            }
            seen_section = true;
            in_section = name == remote;
            if in_section {
                if found {
                    return Err(syntax(no, SyntaxIssue::DuplicateSection));
                }
                found = true;
            }
            continue;
        }
        if !seen_section {
            return Err(syntax(no, SyntaxIssue::OutsideSection));
        }
        // `key = value`（キーは平文。`:` 区切りや引用符つきのキーは断る）。どのセクションの行にも同じ形を求める
        // （rclone はこの形でしか書かない。ほかの形は、ほかの読み手と解釈が分かれる）
        let Some((key, value)) = line.split_once('=') else {
            return Err(syntax(no, SyntaxIssue::BadLine));
        };
        let key = key.trim();
        if !is_plain_key(key) {
            return Err(syntax(no, SyntaxIssue::BadLine));
        }
        if !in_section && value.trim().starts_with(['"', '`']) {
            return Err(syntax(no, SyntaxIssue::QuotedValue));
        }
        if !in_section {
            continue;
        }
        if keys.contains(&key) {
            return Err(syntax(no, SyntaxIssue::DuplicateKey));
        }
        keys.push(key);
        if let Some(i) = REQUIRED_KEYS.iter().position(|k| *k == key) {
            let value = value.trim();
            if value.starts_with(['"', '`']) || value.contains("%(") {
                return Err(syntax(
                    no,
                    SyntaxIssue::QuotedOrInterpolated(REQUIRED_KEYS[i]),
                ));
            }
            state[i] = Some(!value.is_empty());
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

    /// 試験用: root が取り込むときと同じ読み方（根から 1 段ずつ開いて、パスの途中も確かめる。持ち主は問わない）で、`[remote]` の
    /// 3 つの値を取り出す。実行ユーザーが root でなくても、この読み方を通せる。本番のビルドには入れない（持ち主の検査を省く経路を
    /// 残さない）。
    fn read_credentials_via_chain(
        path: &Path,
        remote: &str,
    ) -> Result<ImportedCredentials, ConfigError> {
        let text = read_config_text_with(path, effective_uid(), true, true)?;
        credentials_from(&text, remote)
    }

    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// `dir/rclone/rclone.conf`（親は 0700、ファイルは 0600）。
    fn write_config(dir: &Path, content: &[u8]) -> std::path::PathBuf {
        let parent = dir.join("rclone");
        std::fs::create_dir_all(&parent).unwrap();
        set_mode(&parent, 0o700);
        let path = parent.join("rclone.conf");
        std::fs::write(&path, content).unwrap();
        set_mode(&path, 0o600);
        path
    }

    const REFRESH: &str = "1//REFRESH-TOKEN-MARKER-33aa";

    fn conf_text(remote: &str) -> String {
        format!(
            "[{remote}]\ntype = drive\nclient_id = CID-MARK\nclient_secret = SECRET-MARK\nscope = drive\n\
             token = {{\"access_token\":\"ya29.X\",\"token_type\":\"Bearer\",\"refresh_token\":\"{REFRESH}\",\"expiry\":\"2030-01-01T00:00:00+09:00\"}}\n"
        )
    }

    #[test]
    fn a_section_with_both_values_passes() {
        assert_eq!(check_section(GOOD, "gdrive"), Ok(()));
        // 空白・コメント・別セクション・順序は問わない
        let text =
            "# x\n[other]\nclient_id =\n\n[ gdrive ]\n; c\n  client_secret=B  \nclient_id\t= A\n";
        assert_eq!(check_section(text, "gdrive"), Ok(()));
        // ほかのセクションでも、引用符で始まる値（閉じ忘れを含む）は断る
        for text in [
            "[other]\ntoken = `{unclosed\n[gdrive]\nclient_id = A\nclient_secret = B\n",
            "[gdrive]\nclient_id = A\nclient_secret = B\n[other]\ntoken = \"x\n",
        ] {
            assert!(
                matches!(
                    check_section(text, "gdrive"),
                    Err(ConfigError::Syntax {
                        issue: SyntaxIssue::QuotedValue,
                        ..
                    })
                ),
                "{text}"
            );
        }
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
        // セクションの外（先頭）の行は、数えないどころか断る（goconfig は DEFAULT セクションに入れる）
        assert_eq!(
            check_section(
                "client_id = A\nclient_secret = B\n[gdrive]\ntype = drive\n",
                "gdrive"
            ),
            Err(syntax(1, SyntaxIssue::OutsideSection))
        );
    }

    fn syntax_issue(text: &str) -> Option<(usize, SyntaxIssue)> {
        match check_section(text, "gdrive") {
            Err(ConfigError::Syntax { line, issue }) => Some((line, issue)),
            _ => None,
        }
    }

    #[test]
    fn re_assigning_a_key_is_refused_whatever_the_order() {
        // 値あり → 空（後勝ちで共用の client_id に戻る）も、空 → 値ありも、同じ値の再指定も断る
        for text in [
            "[gdrive]\nclient_id = A\nclient_secret = B\nclient_id =\n",
            "[gdrive]\nclient_id =\nclient_secret = B\nclient_id = A\n",
            "[gdrive]\nclient_id = A\nclient_secret = B\nclient_secret = B\n",
            "[gdrive]\nclient_id = A\nclient_secret = B\ntype = drive\ntype = drive\n",
        ] {
            assert!(
                matches!(syntax_issue(text), Some((_, SyntaxIssue::DuplicateKey))),
                "{text:?}"
            );
        }
        assert_eq!(
            syntax_issue("[gdrive]\nclient_id = A\nclient_secret = B\nclient_id =\n"),
            Some((4, SyntaxIssue::DuplicateKey))
        );
        // ほかのセクションの同じキーは別物
        assert_eq!(
            check_section(
                "[other]\nclient_id = A\nclient_id = B\n[gdrive]\nclient_id = A\nclient_secret = B\n",
                "gdrive"
            ),
            Ok(())
        );
        // remote のセクションが 2 度ある（値の上書きの抜け道になる）
        assert_eq!(
            syntax_issue("[gdrive]\nclient_id = A\nclient_secret = B\n[gdrive]\nclient_id =\n"),
            Some((4, SyntaxIssue::DuplicateSection))
        );
        assert_eq!(
            syntax_issue("[gdrive]\nclient_id = A\nclient_secret = B\n[ gdrive ]\n"),
            Some((4, SyntaxIssue::DuplicateSection))
        );
    }

    #[test]
    fn quoted_and_interpolated_required_values_are_refused() {
        for value in [
            "\"\"",
            "\"A\"",
            "\"\"\"\"\"\"",
            "`A`",
            "``",
            "%(client_secret)s",
            "prefix%(x)ssuffix",
        ] {
            let text = format!("[gdrive]\nclient_id = {value}\nclient_secret = B\n");
            assert_eq!(
                syntax_issue(&text),
                Some((2, SyntaxIssue::QuotedOrInterpolated("client_id"))),
                "{value}"
            );
        }
        // 途中の引用符・パーセントは通常の文字（Google の client_id にも入りうる文字だけ）
        assert_eq!(
            check_section(
                "[gdrive]\nclient_id = a\"b%c\nclient_secret = B\n",
                "gdrive"
            ),
            Ok(())
        );
    }

    #[test]
    fn syntax_other_than_plain_key_equals_value_is_refused() {
        for (text, line) in [
            // `:` 区切り（goconfig は `=` と同じに読む）
            ("[gdrive]\nclient_id: A\nclient_secret = B\n", 2),
            ("[gdrive]\nclient_id = A\nclient_secret:B\n", 3),
            ("[gdrive]\nclient_id:A = x\n", 2),
            // 引用符で始まるキー
            ("[gdrive]\n\"client_id\" = A\n", 2),
            ("[gdrive]\n`client_id` = A\n", 2),
            ("[gdrive]\n\"\"\"client_id\"\"\" = A\n", 2),
            // = が無い・キーが空・キーに空白や記号
            ("[gdrive]\nclient_id\n", 2),
            ("[gdrive]\n= A\n", 2),
            ("[gdrive]\nclient id = A\n", 2),
            ("[gdrive]\n-\n", 2),
            ("[gdrive]\n- = A\n", 2),
            // 壊れたセクション行・継続のように見える行
            ("[gdrive\nclient_id = A\n", 1),
            ("[gdrive]\nclient_id = A\\\n  more\n", 3),
            // ほかのセクションの中でも同じ形を求める
            (
                "[other]\nweird: line\n[gdrive]\nclient_id = A\nclient_secret = B\n",
                2,
            ),
        ] {
            let got = syntax_issue(text);
            assert!(
                matches!(got, Some((l, SyntaxIssue::BadLine | SyntaxIssue::OutsideSection)) if l == line),
                "{text:?} -> {got:?}"
            );
        }
        // セクションの前の行（コメントと空行は除く）、空のセクション名、BOM
        assert_eq!(
            syntax_issue("type = drive\n[gdrive]\nclient_id = A\nclient_secret = B\n"),
            Some((1, SyntaxIssue::OutsideSection))
        );
        assert_eq!(
            syntax_issue("[]\nclient_id = A\n"),
            Some((1, SyntaxIssue::OutsideSection))
        );
        assert!(syntax_issue("\u{feff}[gdrive]\nclient_id = A\nclient_secret = B\n").is_some());
        // コメント・空行・値の中の `:`・`=`・引用符（token の JSON など）は通る
        assert_eq!(
            check_section(
                "# c\n\n; d\n[gdrive]\nclient_id = A\nclient_secret = B\ntoken = {\"a\":\"b=c\"}\n",
                "gdrive"
            ),
            Ok(())
        );
    }

    #[test]
    fn syntax_errors_name_the_line_but_never_the_content() {
        let e = check_section(
            "[gdrive]\nclient_id = A\nSECRET-LINE-CONTENT: x\n",
            "gdrive",
        )
        .unwrap_err();
        let text = format!("{e} {e:?}");
        assert!(text.contains("3 行目"), "{text}");
        assert!(!text.contains("SECRET-LINE-CONTENT"), "{text}");
    }

    #[test]
    fn owner_and_mode_are_checked_for_both_targets() {
        for target in [Target::File, Target::Directory] {
            assert_eq!(
                check_owner_and_mode(target, 1000, 0o600, 1000, false),
                Ok(())
            );
            assert_eq!(
                check_owner_and_mode(target, 1001, 0o600, 1000, false),
                Err(ConfigError::WrongOwner(target))
            );
            for mode in [0o640, 0o604, 0o660, 0o666, 0o701, 0o710] {
                assert_eq!(
                    check_owner_and_mode(target, 1000, mode, 1000, false),
                    Err(ConfigError::TooPermissive(target)),
                    "{mode:o}"
                );
            }
            // 持ち主が違うときは、権限より先に持ち主を断る
            assert_eq!(
                check_owner_and_mode(target, 0, 0o666, 1000, false),
                Err(ConfigError::WrongOwner(target))
            );
        }
    }

    #[test]
    fn messages_name_keys_and_kinds_only() {
        for e in [
            ConfigError::MissingSection,
            ConfigError::MissingKey("client_id"),
            ConfigError::EmptyValue("client_secret"),
            syntax(3, SyntaxIssue::BadLine),
            syntax(1, SyntaxIssue::OutsideSection),
            syntax(2, SyntaxIssue::DuplicateSection),
            syntax(2, SyntaxIssue::DuplicateKey),
            syntax(2, SyntaxIssue::QuotedOrInterpolated("client_id")),
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

    /// root が取り込むときの読み方（根から 1 段ずつ開いたハンドルで、パスの途中も確かめる）を、root でない試験からも通す。
    /// 検査したパスと、開いたファイルの経路が同じ（途中のリンクを解決した先を検査して、リンクを置いたディレクトリが検査から
    /// 外れることがない）。
    #[test]
    fn the_file_is_opened_through_the_checked_chain_and_a_link_in_the_middle_is_refused() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let me = effective_uid();
        // 検査は、根から下の全部のディレクトリを見る。この試験を置く場所の上に、グループが書き込めるディレクトリがあると、どんな読み方でも
        // 通らない（環境による）。そこで、上の道が検査を通る置き場所を選ぶ（なければ、成功することの確認だけを省く）
        let candidates = [std::env::temp_dir(), std::path::PathBuf::from("/tmp")];
        let root = candidates.iter().find_map(|c| {
            let real = std::fs::canonicalize(c).ok()?;
            check_ancestors(&real, me).ok()?;
            (std::fs::metadata(&real).ok()?.mode() & 0o022 == 0
                || std::fs::metadata(&real).ok()?.mode() & 0o1000 != 0)
                .then_some(real)
        });
        let (dir, usable) = match root {
            Some(root) => {
                let p = root.join(format!("export-import-chain-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir_all(&p).unwrap();
                set_mode(&p, 0o700);
                (p, true)
            }
            None => {
                eprintln!("上の道が検査を通る置き場所が無いので、成功することの確認は省く");
                (
                    {
                        let p = std::env::temp_dir()
                            .join(format!("export-import-chain-x-{}", std::process::id()));
                        let _ = std::fs::remove_dir_all(&p);
                        std::fs::create_dir_all(&p).unwrap();
                        set_mode(&p, 0o700);
                        std::fs::canonicalize(p).unwrap()
                    },
                    false,
                )
            }
        };
        // 実際のパス（途中のリンクを含まない形）で渡す
        let base = dir.clone();
        let conf = write_config(&base, conf_text("gdrive").as_bytes());
        // 何も問題がなければ読める
        if usable {
            let got = read_credentials_via_chain(&conf, "gdrive").unwrap();
            assert_eq!(got.refresh_token, REFRESH);
        }

        // 途中のディレクトリ（`base` の下の `up`）がシンボリックリンクなら、行き先が同じ中身でも断る
        let up = base.join("up");
        symlink(&base, &up).unwrap();
        let via_link = up.join("rclone").join("rclone.conf");
        assert!(via_link.exists());
        assert_eq!(
            read_credentials_via_chain(&via_link, "gdrive").unwrap_err(),
            ConfigError::WrongKind(Target::Directory)
        );
        // 親ディレクトリそのものがリンクでも断る
        let alias = base.join("alias");
        symlink(base.join("rclone"), &alias).unwrap();
        assert_eq!(
            read_credentials_via_chain(&alias.join("rclone.conf"), "gdrive").unwrap_err(),
            ConfigError::WrongKind(Target::Directory)
        );
        // 途中のディレクトリを、グループが書き込める → 断る（その下の親ディレクトリは 0700 のまま）
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert_eq!(
            read_credentials_via_chain(&conf, "gdrive").unwrap_err(),
            ConfigError::TooPermissive(Target::Directory)
        );
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        if usable {
            assert!(read_credentials_via_chain(&conf, "gdrive").is_ok());
        }
        // `..` や空の要素を含むパスは断る
        for bad in [
            format!("{}/rclone/../rclone/rclone.conf", base.display()),
            format!("{}//rclone/rclone.conf", base.display()),
            format!("{}/./rclone/rclone.conf", base.display()),
        ] {
            assert_eq!(
                read_credentials_via_chain(std::path::Path::new(&bad), "gdrive").unwrap_err(),
                ConfigError::WrongKind(Target::Directory),
                "{bad}"
            );
        }
        // ファイルがシンボリックリンクなら断る（ファイル名の要素も、リンクをたどらない）
        let real = base.join("rclone").join("rclone.conf");
        let swapped = base.join("rclone").join("other.conf");
        symlink(&real, &swapped).unwrap();
        assert_eq!(
            read_credentials_via_chain(&swapped, "gdrive").unwrap_err(),
            ConfigError::WrongKind(Target::File)
        );
        if usable {
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}
