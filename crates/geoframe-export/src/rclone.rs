//! rclone の起動（design.md 4.2「rclone の呼び方」、8 章「子プロセス」）。**rclone 固有のものだけ**を持つ（引数の組み立て・
//! 子プロセス・標準エラーの伏せ字・`lsjson` の出力の読み取り）。呼び出し側は、このモジュールの [`Rclone`] を直接使わず、
//! 中立の入口 [`crate::remote::Remote`]（今の中身は [`Rclone`]）を使う。一覧の 1 項目・検査済みの場所・操作の失敗の型は
//! [`crate::remote`]（実装に依らない）。
//!
//! このモジュールは、rclone を**1 回起動して結果を受け取る**ところだけを持つ。何を・どの順で呼ぶか（宛先の状態、公開、
//! 復旧）は [`crate::drive`]・[`crate::publish`]。
//!
//! ## 守っていること
//!
//! - 実行ファイルは `--rclone`、設定ファイルは `--config <--rclone-config>` を**毎回**明示する。
//! - 環境変数は `env_clear()` のあと、[`ENV_KEYS`] の 3 つ（`PATH`＝固定の値、`HOME`・`XDG_CACHE_HOME`＝状態ディレクトリの
//!   `rclone/` の中の専用の場所）だけ。`RCLONE_*` は渡さない（設定ファイルの値を上書きされないため）。
//! - `--drive-client-id`・`--drive-client-secret`・`--drive-token` など、**資格情報を上書きする引数は渡さない**
//!   （専用の client_id の検査［`crate::rclone_conf`］をすり抜けないため）。引数はこのモジュールの関数が固定の部品から組み立て、
//!   起動の直前に [`is_forbidden_flag`] でもう一度確かめる。`-vv`・`--dump*` も同じ検査で断る。
//! - シェルを介さず、引数の配列で渡す。位置の引数の前に `--` を置く。Drive の場所は [`RemoteLoc`]（[`crate::remote`]。検査済みの部品だけで作る）。
//! - 標準入力は閉じる。子プロセスは自分のプロセスグループに入れ、期限を過ぎたらグループごと kill する。
//! - キャンセルの印（[`crate::cancel::Cancel`]。SIGINT・SIGTERM）が立ったら、起動の前なら起動せず、実行中なら期限切れと同じく
//!   グループごと kill して `wait` し、出力のスレッドを `join` してから [`RemoteError::Cancelled`]（[`crate::remote`]）を返す。呼び出し側は、この
//!   関数が返ってから実行枠を手放す（子が残ったまま枠だけ外れて、次の復旧が進行中の `copy`・`moveto` と競合しないため）。
//!   パニックで巻き戻るときも、`Drop` で同じ kill と `wait` をする（[`ChildGuard`]）。
//! - 親が異常終了したとき（SIGKILL・OOM など、印を見られない終わり方）の補助として、子に `PR_SET_PDEATHSIG`（SIGKILL）を付ける。
//!   これは**排他の保証にならない**: 効くのは、子を起動した**スレッド**が終わったときで（プロセスの終了とは限らない。ジョブの
//!   スレッドは起動した子を `wait` し終えてから終わる）、kill されるのは子だけで子孫には及ばず、親が死んだあとで実行枠が
//!   外れるのとのあいだに隙がある。排他は、印を見て kill → `wait` → 枠を手放す、の順で守る。
//! - 標準出力・標準エラーは別のスレッドで読み続ける（パイプが詰まって止まらないように）。標準出力には上限を掛ける。
//! - 標準エラーは、`token`・`Bearer`・`access_token` などを含む行を伏せ字にしてから、最後の数行だけをログ（tracing）に残す
//!   （[`redact_stderr`]）。API の結果・エラーの説明には rclone の出力を載せない（[`crate::error::ExportError`] の説明は固定の文字列）。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cancel::Cancel;
use crate::remote::{
    CopyScope, DIRECTORY_MIME, DRY_RUN_TIMEOUT, Entry, RemoteError, RemoteLoc, is_valid_drive_id,
    normalize_md5, rel_path_ok,
};
use crate::settings::Settings;

/// 子プロセスの `PATH`（固定。rclone は他のコマンドを呼ばない）。
pub const PATH_VALUE: &str = "/usr/local/bin:/usr/bin:/bin";
/// 子プロセスに渡す環境変数の名前（これだけ。試験が、渡した環境がこの 3 つと一致することを確かめる）。
pub const ENV_KEYS: [&str; 3] = ["PATH", "HOME", "XDG_CACHE_HOME"];
/// 標準出力の上限（`lsjson`。2,000 枚の再帰の一覧でも数 MB）。
pub const STDOUT_MAX_BYTES: usize = 32 * 1024 * 1024;
/// 一覧の項目数の上限。
pub const LSJSON_MAX_ENTRIES: usize = 20_000;
/// 標準エラーの 1 行のうち、読み取って機微な語を調べるバイト数の上限。超えた行は、中身を見ずにまるごと伏せ字にする。
const STDERR_LINE_CAP_BYTES: usize = 64 * 1024;
/// ログに残す標準エラーの行数（末尾から）。
pub const STDERR_LOG_LINES: usize = 5;
const STDERR_LINE_MAX_CHARS: usize = 300;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// rclone の終了コード 3（ディレクトリが無い）。
const EXIT_DIR_NOT_FOUND: i32 = 3;

// ---- lsjson の出力 ----

/// rclone の複合 ID（リンク先の ID とショートカットの ID をタブで結んだもの。drive.go の `joinID`）を分ける。
/// どちらの部分も [`is_valid_drive_id`] に合わなければ `None`。タブを含まなければ、そのまま通常の ID。
fn split_composite_id(id: &str) -> Option<(String, Option<String>)> {
    match id.split_once('\t') {
        None => is_valid_drive_id(id).then(|| (id.to_owned(), None)),
        Some((actual, shortcut)) => (is_valid_drive_id(actual) && is_valid_drive_id(shortcut))
            .then(|| (actual.to_owned(), Some(shortcut.to_owned()))),
    }
}

/// `lsjson` の出力を読む。形が違えば `None`（呼び出し側は `drive_unavailable` などの失敗にする）。
///
/// 形: 配列。各項目は `Path`(文字列)・`Name`(文字列、`Path` の最後の部品)・`Size`(整数、フォルダは -1。**ファイルも -1 になりうる**:
/// `--drive-show-all-gdocs` で出るネイティブの Google 文書など、サイズが分からないもの)・
/// `MimeType`(文字列。フォルダは `inode/directory`)・`IsDir`(真偽)・`ID`(文字列、空でない。解決されたショートカットは複合 ID)。
/// `--hash` を付けた一覧では、ファイルに `Hashes`(`{"md5": "<16 進 32 文字>"}`。rclone v1.76.0 の `ListJSONItem.Hashes`。
/// キーは小文字の `md5`、MD5 を持たないファイルでは空・省略）が付く。形が違う `Hashes` は MD5 なし（`None`）として読む
/// （一覧全体は断らない。MD5 を期待する照合が一式に無いファイルとして断る）。`ModTime` は読まない。サイズ不明（負）のファイルは受け付け、中身の照合（[`crate::drive::Expected::matches`]）が
/// 「一式に無いファイル」として扱う。
pub fn parse_lsjson(bytes: &[u8]) -> Option<Vec<Entry>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let items = value.as_array()?;
    if items.len() > LSJSON_MAX_ENTRIES {
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let o = item.as_object()?;
        let path = o.get("Path")?.as_str()?;
        let name = o.get("Name")?.as_str()?;
        let mime = o.get("MimeType")?.as_str()?;
        let raw_id = o.get("ID")?.as_str()?;
        let size = o.get("Size")?.as_i64()?;
        let is_dir = o.get("IsDir")?.as_bool()?;
        let (id, shortcut_id) = split_composite_id(raw_id)?;
        let md5 = o
            .get("Hashes")
            .and_then(Value::as_object)
            .and_then(|h| h.get("md5"))
            .and_then(Value::as_str)
            .and_then(normalize_md5);
        if !rel_path_ok(path)
            || path.rsplit('/').next() != Some(name)
            || is_dir != (mime == DIRECTORY_MIME)
        {
            return None;
        }
        out.push(Entry {
            path: path.to_owned(),
            name: name.to_owned(),
            size,
            mime: mime.to_owned(),
            is_dir,
            id,
            shortcut_id,
            md5: if is_dir { None } else { md5 },
        });
    }
    Some(out)
}

// ---- 標準エラーの伏せ字 ----

/// この文字列（小文字）を含む行は、まるごと伏せ字にする。
const SENSITIVE_MARKERS: [&str; 6] = [
    "token",
    "bearer",
    "access_token",
    "secret",
    "authorization",
    "client_id",
];

/// 標準エラーを**読みながら**行ごとに伏せ字にして、最後の数行だけを残す。
///
/// 先に末尾のバイト列だけを切り出してから検査すると、長い行の先頭にある `Bearer` などが切れて、残りの部分（資格情報の本体）が
/// 伏せ字にならずに出てしまう。そこで、行の終わり（`\n`）が来るたびに、**その行の全体**を調べてから残す。1 行が
/// [`STDERR_LINE_CAP_BYTES`] を超えたら、先頭だけを持って、その行はまるごと伏せ字にする（持っていない部分に何があるか分からないため）。
/// どんなバイト列の区切りで [`StderrTail::push`] に渡しても、結果は同じ。
struct StderrTail {
    keep: usize,
    lines: VecDeque<String>,
    current: Vec<u8>,
    overflowed: bool,
}

impl StderrTail {
    fn new(keep: usize) -> Self {
        Self {
            keep,
            lines: VecDeque::new(),
            current: Vec::new(),
            overflowed: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for chunk in bytes.split_inclusive(|b| *b == b'\n') {
            let (body, ended) = match chunk.split_last() {
                Some((b'\n', rest)) => (rest, true),
                _ => (chunk, false),
            };
            let room = STDERR_LINE_CAP_BYTES.saturating_sub(self.current.len());
            if body.len() > room {
                self.overflowed = true;
            }
            self.current
                .extend_from_slice(&body[..body.len().min(room)]);
            if ended {
                self.end_line();
            }
        }
    }

    fn end_line(&mut self) {
        let bytes = std::mem::take(&mut self.current);
        let overflowed = std::mem::take(&mut self.overflowed);
        let Some(line) = redact_line(&bytes, overflowed) else {
            return;
        };
        self.lines.push_back(line);
        while self.lines.len() > self.keep {
            self.lines.pop_front();
        }
    }

    /// 最後の行（改行で終わっていないもの）も含めて、` / ` でつなぐ。
    fn finish(mut self) -> String {
        if !self.current.is_empty() || self.overflowed {
            self.end_line();
        }
        self.lines.into_iter().collect::<Vec<_>>().join(" / ")
    }
}

/// 1 行を、ログに残せる形にする。空の行は `None`。
fn redact_line(bytes: &[u8], overflowed: bool) -> Option<String> {
    if overflowed {
        return Some(REDACTED.to_owned());
    }
    let text = String::from_utf8_lossy(bytes);
    let line = text.trim_end_matches('\r');
    if line.trim().is_empty() {
        return None;
    }
    let lower = line.to_lowercase();
    Some(if SENSITIVE_MARKERS.iter().any(|m| lower.contains(m)) {
        REDACTED.to_owned()
    } else {
        line.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(STDERR_LINE_MAX_CHARS)
            .collect()
    })
}

const REDACTED: &str = "[伏せ字]";

/// 標準エラーを、ログに残せる形にする: 行ごとに、機微な語（`token`・`Bearer`・`access_token` ほか。大文字小文字を問わない）を
/// 含む行を伏せ字にし、制御文字を空白にし、長い行を切り、最後の `max_lines` 行だけを ` / ` でつなぐ。
/// 子プロセスの出力は、読みながら同じ処理をする（`StderrTail`）。
pub fn redact_stderr(raw: &[u8], max_lines: usize) -> String {
    let mut tail = StderrTail::new(max_lines);
    tail.push(raw);
    tail.finish()
}

// ---- 子プロセスの結果 ----

/// 起動した rclone の標準出力。
#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
}

/// 引数に入れてはいけない flag（資格情報の上書き、ログに秘密が出うるもの、設定の二重指定）。
pub fn is_forbidden_flag(arg: &str) -> bool {
    const PREFIXES: [&str; 14] = [
        "--drive-client",
        "--drive-token",
        "--drive-service-account",
        "--drive-impersonate",
        "--drive-scope",
        "--drive-env-auth",
        "--drive-auth",
        "--drive-root-folder-id",
        "--dump",
        "--verbose",
        "--config-",
        "--rc",
        "--password",
        "--use-json-log",
    ];
    if PREFIXES.iter().any(|p| arg.starts_with(p)) {
        return true;
    }
    // `-v`・`-vv`・`-vvv` と、`-q` の連なりなど（短い flag の束）。位置の引数は `--` の後ろにあるので、ここには来ない
    arg.starts_with('-')
        && !arg.starts_with("--")
        && arg.len() > 1
        && arg[1..].bytes().all(|b| b == b'v')
}

/// rclone の呼び出し口。`Clone` できる（期限だけを替えた複製を作れる）。
#[derive(Debug, Clone)]
pub struct Rclone {
    program: PathBuf,
    config: PathBuf,
    home: PathBuf,
    cache: PathBuf,
    copy_timeout: Duration,
    timeout: Duration,
    deadline: Option<Instant>,
    cancel: Cancel,
}

impl Rclone {
    /// 設定（`--rclone`・`--rclone-config`・`--state-dir`・2 つの期限）から作る。
    pub fn new(settings: &Settings) -> Self {
        let base = settings.state_dir.join("rclone");
        Self {
            program: settings.rclone.clone(),
            config: settings.rclone_config.clone(),
            home: base.join("home"),
            cache: base.join("cache"),
            copy_timeout: settings.rclone_copy_timeout,
            timeout: settings.rclone_timeout,
            deadline: None,
            cancel: Cancel::new(),
        }
    }

    /// `dry_run` 用: 照会の期限を 30 秒にする（design.md 6.1）。`copy` は使わない。
    pub fn for_dry_run(settings: &Settings) -> Self {
        let mut r = Self::new(settings);
        r.timeout = r.timeout.min(DRY_RUN_TIMEOUT);
        r.copy_timeout = r.timeout;
        r
    }

    /// 期限（コマンドごとの期限）を指定して作る（試験用）。
    #[doc(hidden)]
    pub fn with_timeouts(mut self, copy: Duration, other: Duration) -> Self {
        self.copy_timeout = copy;
        self.timeout = other;
        self
    }

    /// キャンセルの印（SIGINT・SIGTERM）を共有する。立つと、起動の前なら起動せず、実行中なら子のグループを kill して `wait` し、
    /// [`RemoteError::Cancelled`] を返す。
    pub fn with_cancel(mut self, cancel: Cancel) -> Self {
        self.cancel = cancel;
        self
    }

    /// キャンセルの印が立っているか（復旧の待ちが、眠らずに止まるために見る）。
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// ジョブ全体の期限を掛けた複製。過ぎたあとの起動は `JobDeadline`、実行中に過ぎれば kill して `JobDeadline`。
    pub fn with_deadline(&self, deadline: Option<Instant>) -> Self {
        let mut r = self.clone();
        r.deadline = deadline;
        r
    }

    // ---- 引数の組み立て（固定の部品だけ。試験が flag の中身を確かめる） ----

    fn common_args(&self, sub: &str, skip_shortcuts: bool) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec![sub.into(), "--config".into(), self.config.clone().into()];
        for s in [
            "--ask-password=false",
            "--retries",
            "3",
            "--low-level-retries",
            "10",
            "--transfers",
            "4",
            "--checkers",
            "8",
            "--cache-dir",
        ] {
            a.push(s.into());
        }
        a.push(self.cache.clone().into());
        if skip_shortcuts {
            a.push("--drive-skip-shortcuts".into());
        }
        a
    }

    fn finish_args(mut a: Vec<OsString>, positional: Vec<OsString>) -> Vec<OsString> {
        a.push("--".into());
        a.extend(positional);
        a
    }

    #[doc(hidden)]
    pub fn lsjson_args(
        &self,
        loc: &RemoteLoc,
        recursive: bool,
        skip_shortcuts: bool,
        hash: bool,
    ) -> Vec<OsString> {
        // 設定ファイルに `skip_shortcuts`・`skip_gdocs` があっても結果が変わらないよう、毎回どちらも明示する。
        // ショートカットを除かない一覧は `=false`（設定ファイルで省略が有効でも、解決されたショートカットが一覧に出る）。
        // `--drive-show-all-gdocs` は、エクスポートできない Google のファイル（フォーム等）も一覧に出す
        // （付けないと、宛先と同じ名前のフォームが「無い」ように見える）。そうしたファイルとネイティブ文書は `Size: -1`。
        let mut a = self.common_args("lsjson", false);
        a.push(
            if skip_shortcuts {
                "--drive-skip-shortcuts"
            } else {
                "--drive-skip-shortcuts=false"
            }
            .into(),
        );
        // リンク切れのショートカットを省く設定があっても、一覧に出す（ショートカットとして分類したいため）
        if !skip_shortcuts {
            a.push("--drive-skip-dangling-shortcuts=false".into());
        }
        a.push("--drive-skip-gdocs=false".into());
        a.push("--drive-show-all-gdocs".into());
        if recursive {
            a.push("-R".into());
        }
        // 中身の照合用: ファイルごとの MD5（Drive はバイナリのファイルに MD5 を持つ）を `Hashes.md5` に出す
        if hash {
            a.push("--hash".into());
            a.push("--hash-type".into());
            a.push("MD5".into());
        }
        Self::finish_args(a, vec![loc.as_str().into()])
    }

    #[doc(hidden)]
    pub fn cat_args(&self, loc: &RemoteLoc, count: u64) -> Vec<OsString> {
        let mut a = self.common_args("cat", true);
        a.push("--count".into());
        a.push(count.to_string().into());
        Self::finish_args(a, vec![loc.as_str().into()])
    }

    #[doc(hidden)]
    pub fn mkdir_args(&self, loc: &RemoteLoc) -> Vec<OsString> {
        let a = self.common_args("mkdir", true);
        Self::finish_args(a, vec![loc.as_str().into()])
    }

    #[doc(hidden)]
    pub fn copy_args(&self, src: &Path, dst: &RemoteLoc, scope: CopyScope) -> Vec<OsString> {
        let mut a = self.common_args("copy", true);
        a.push("--immutable".into());
        match scope {
            CopyScope::PhotosOnly => {
                a.push("--exclude".into());
                a.push("/manifest.json".into());
            }
            CopyScope::ManifestOnly => {
                a.push("--include".into());
                a.push("/manifest.json".into());
            }
        }
        Self::finish_args(a, vec![src.as_os_str().to_owned(), dst.as_str().into()])
    }

    #[doc(hidden)]
    pub fn moveto_args(&self, src: &RemoteLoc, dst: &RemoteLoc) -> Vec<OsString> {
        let mut a = self.common_args("moveto", true);
        a.push("--immutable".into());
        Self::finish_args(a, vec![src.as_str().into(), dst.as_str().into()])
    }

    #[doc(hidden)]
    pub fn purge_args(&self, loc: &RemoteLoc) -> Vec<OsString> {
        let mut a = self.common_args("purge", true);
        a.push("--drive-use-trash=true".into());
        Self::finish_args(a, vec![loc.as_str().into()])
    }

    // ---- コマンド ----

    /// `lsjson`。`skip_shortcuts` が偽のときは、ショートカットも（rclone の既定の扱いで）一覧に出る。`hash` が真なら、
    /// ファイルの MD5 も出す（`--hash --hash-type MD5`。中身の照合に使う一覧だけ）。
    /// 場所が無ければ `NotFound`、出力が決められた形でなければ `BadOutput`。
    pub fn lsjson(
        &self,
        loc: &RemoteLoc,
        recursive: bool,
        skip_shortcuts: bool,
        hash: bool,
    ) -> Result<Vec<Entry>, RemoteError> {
        let out = self.run(
            "lsjson",
            self.lsjson_args(loc, recursive, skip_shortcuts, hash),
            self.timeout,
            STDOUT_MAX_BYTES,
        )?;
        parse_lsjson(&out.stdout).ok_or(RemoteError::BadOutput)
    }

    /// `cat`。先頭の `max_bytes + 1` バイトまでを読む（`max_bytes` を超えるかどうかは、返った長さで分かる）。
    pub fn cat(&self, loc: &RemoteLoc, max_bytes: u64) -> Result<Vec<u8>, RemoteError> {
        let count = max_bytes.saturating_add(1);
        let cap = usize::try_from(count).unwrap_or(usize::MAX);
        self.run("cat", self.cat_args(loc, count), self.timeout, cap)
            .map(|o| o.stdout)
    }

    pub fn mkdir(&self, loc: &RemoteLoc) -> Result<(), RemoteError> {
        self.run("mkdir", self.mkdir_args(loc), self.timeout, 64 * 1024)
            .map(|_| ())
    }

    /// 作業領域（ローカルのディレクトリ。絶対パス）を `dst` へ送る。期限は `--rclone-copy-timeout`。
    pub fn copy(&self, src: &Path, dst: &RemoteLoc, scope: CopyScope) -> Result<(), RemoteError> {
        if !src.is_absolute() {
            return Err(RemoteError::Spawn);
        }
        self.run(
            "copy",
            self.copy_args(src, dst, scope),
            self.copy_timeout,
            64 * 1024,
        )
        .map(|_| ())
    }

    pub fn moveto(&self, src: &RemoteLoc, dst: &RemoteLoc) -> Result<(), RemoteError> {
        self.run(
            "moveto",
            self.moveto_args(src, dst),
            self.timeout,
            64 * 1024,
        )
        .map(|_| ())
    }

    /// `purge --drive-use-trash=true`（Drive のゴミ箱へ移す）。
    pub fn purge(&self, loc: &RemoteLoc) -> Result<(), RemoteError> {
        self.run("purge", self.purge_args(loc), self.timeout, 64 * 1024)
            .map(|_| ())
    }

    // ---- 起動 ----

    fn ensure_private_dir(path: &Path) -> Result<(), RemoteError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| RemoteError::Spawn)
    }

    fn run(
        &self,
        sub: &'static str,
        args: Vec<OsString>,
        timeout: Duration,
        stdout_cap: usize,
    ) -> Result<Output, RemoteError> {
        // 引数の最後の検査（資格情報の上書き・詳細なログの flag は、どの経路でも渡さない）
        for arg in args.iter().take_while(|a| a.as_encoded_bytes() != b"--") {
            match arg.to_str() {
                Some(s) if !is_forbidden_flag(s) => {}
                _ => return Err(RemoteError::Spawn),
            }
        }
        if self.cancel.is_cancelled() {
            return Err(RemoteError::Cancelled);
        }
        // 期限: コマンドごとの期限と、ジョブ全体の期限の短いほう
        let mut limit = timeout;
        let mut by_job_deadline = false;
        if let Some(d) = self.deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(RemoteError::JobDeadline);
            }
            if left < limit {
                limit = left;
                by_job_deadline = true;
            }
        }
        Self::ensure_private_dir(&self.home)?;
        Self::ensure_private_dir(&self.cache)?;

        let mut command = Command::new(&self.program);
        command
            .args(&args)
            .env_clear()
            .env("PATH", PATH_VALUE)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.cache)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id();
            // SAFETY: fork のあと exec の前に走る。`prctl`・`getppid` は async-signal-safe で、メモリの確保も、ロックも取らない。
            unsafe {
                command.pre_exec(move || die_with_parent(parent));
            }
        }
        let child = command.spawn().map_err(|_| RemoteError::Spawn)?;
        let mut guard = ChildGuard::new(child);
        let stdout = guard.child.stdout.take().map(|r| read_head(r, stdout_cap));
        let stderr = guard.child.stderr.take().map(read_tail);

        let started = Instant::now();
        // 直接の子の終了と、出力のパイプの完了は別々に見る。子が先に終わっても、同じグループの子孫がパイプを握ったまま、
        // あるいはパイプを閉じて動き続けていることがある。後始末（グループの停止・子の回収・パイプの読み切り）が済むまで、
        // キャンセルと期限を見続ける。
        let mut status: Option<std::process::ExitStatus> = None;
        let mut cause = Interrupt::None;
        loop {
            if status.is_none() {
                match child_exited(guard.child.id()) {
                    Ok(true) => {
                        // 直接の子は終わって、まだ回収していない（ゾンビ。PID・PGID は再利用されない）。回収する前に、
                        // 子孫をグループごと止める。そのあとで `wait` する
                        guard.kill_group();
                        match guard.child.wait() {
                            Ok(st) => {
                                guard.reaped = true;
                                status = Some(st);
                            }
                            Err(_) => {
                                cause = Interrupt::Failed;
                                break;
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(_) => {
                        cause = Interrupt::Failed;
                        break;
                    }
                }
            }
            if status.is_some() && readers_done(&stdout, &stderr) {
                break;
            }
            if self.cancel.is_cancelled() {
                cause = Interrupt::Cancelled;
                break;
            }
            if started.elapsed() >= limit {
                cause = Interrupt::TimedOut;
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        if cause != Interrupt::None {
            // 期限切れ・キャンセル（または待てない）。子と、その子孫をまとめて止めて、`wait` し終えてから先へ進む
            guard.kill_and_wait();
        }
        // パイプを読み切るのを待つ。グループを止めたので、すぐ終わる。グループを抜けた子孫がパイプを握り続けていても、
        // 無限には待たない（猶予のあとは読み手を手放す）
        let grace = Instant::now() + READER_GRACE;
        let (stdout_bytes, truncated) = collect_reader(stdout, grace).unwrap_or_default();
        let stderr_text = collect_reader(stderr, grace).unwrap_or_default();
        let status = if cause == Interrupt::None {
            status
        } else {
            None
        };
        let cancelled = cause == Interrupt::Cancelled;
        let timed_out = cause == Interrupt::TimedOut;

        let result = match status {
            None if cancelled => Err(RemoteError::Cancelled),
            Some(status) if status.success() => {
                if truncated {
                    Err(RemoteError::BadOutput)
                } else {
                    Ok(Output {
                        stdout: stdout_bytes,
                    })
                }
            }
            Some(status) => match status.code() {
                Some(EXIT_DIR_NOT_FOUND) => Err(RemoteError::NotFound),
                Some(code) => Err(RemoteError::Exit(code)),
                None => Err(RemoteError::Exit(-1)),
            },
            None if timed_out && by_job_deadline => Err(RemoteError::JobDeadline),
            None if timed_out => Err(RemoteError::Timeout),
            None => Err(RemoteError::Exit(-1)),
        };
        if let Err(e) = &result {
            // 失敗のときは、コマンド名と失敗の分類（終了コード・期限・キャンセル）と、標準エラーの行数だけを残す。標準エラーの
            // 中身はログにも API の結果にも出さない（伏せ字の語に当たらない行にも、Drive の ID やパスが入るため）
            tracing::warn!(
                command = sub,
                failure = ?e,
                stderr_lines = stderr_text.lines().count(),
                "rclone が失敗しました"
            );
        }
        result
    }
}

/// 実行を途中で切り上げた理由。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    None,
    Cancelled,
    TimedOut,
    /// 子の状態を調べられない・回収できない
    Failed,
}

/// パイプを読み切るのを待つ猶予（グループを止めたあと）。
const READER_GRACE: Duration = Duration::from_secs(2);

/// 直接の子が終わったか（回収はしない。`waitid(WNOWAIT)`）。終わっていれば、子はゾンビのまま残り、PID も PGID も使われ続ける。
fn child_exited(pid: u32) -> std::io::Result<bool> {
    // SAFETY: 全ビット 0 の siginfo_t は有効な値。
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: 自分が起動した子の PID への waitid(2)。`WNOWAIT` で回収しない。`info` は書き込み先として有効。
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if r == 0 {
            // SAFETY: waitid が成功したあとの siginfo_t（待つ子が無ければ si_pid は 0 のまま）。
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// 読み手のスレッドがすべて終わった（読み手が無い場合も含む）。
fn readers_done<A, B>(a: &Option<JoinHandle<A>>, b: &Option<JoinHandle<B>>) -> bool {
    a.as_ref().is_none_or(|h| h.is_finished()) && b.as_ref().is_none_or(|h| h.is_finished())
}

/// 読み手の結果を受け取る。`grace` までに終わらなければ手放す（スレッドは、パイプが閉じたときに自分で終わる）。
fn collect_reader<T>(handle: Option<JoinHandle<T>>, grace: Instant) -> Option<T> {
    let handle = handle?;
    while !handle.is_finished() {
        if Instant::now() >= grace {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    handle.join().ok()
}

/// 起動した子。手放すとき（正常な終わりでも、パニックの巻き戻りでも）まだ `wait` していなければ、プロセスグループごと
/// kill して `wait` する（子が残ったまま、実行枠だけが外れないように）。
struct ChildGuard {
    child: Child,
    /// 子のプロセスグループ（`process_group(0)` で、子の PID と同じ）。
    pgid: Option<i32>,
    /// `wait` 済み（子が終わって回収した）。
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        let pgid = i32::try_from(child.id()).ok();
        Self {
            child,
            pgid,
            reaped: false,
        }
    }

    /// 子のプロセスグループ全体へ SIGKILL。直接の子を回収する前にだけ呼ぶ（回収すると PID が再利用されうるため）。
    fn kill_group(&self) {
        if self.reaped {
            return;
        }
        if let Some(pgid) = self.pgid {
            // SAFETY: 自分が作ったプロセスグループ（`process_group(0)`）への kill(2) だけ
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    fn kill_and_wait(&mut self) {
        if self.reaped {
            return;
        }
        self.kill_group();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

/// 子が、親（このプロセス）が死んだときに SIGKILL を受けるようにする（`pre_exec`。上の「守っていること」の補助。排他の保証ではない）。
/// 起動の途中で親がすでにいなければ（`getppid` が変わっていれば）、起動を失敗させる。
#[cfg(target_os = "linux")]
fn die_with_parent(parent: u32) -> std::io::Result<()> {
    // SAFETY: `prctl`・`getppid` は async-signal-safe。
    unsafe {
        if libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGKILL as libc::c_ulong,
            0,
            0,
            0,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if u32::try_from(libc::getppid()).ok() != Some(parent) {
            return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    Ok(())
}

/// 先頭の `cap` バイトまでを読み、残りは読み捨てる（パイプを詰まらせない）。超えたら真を返す。
fn read_head<R: Read + Send + 'static>(mut r: R, cap: usize) -> JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut truncated = false;
        let mut buf = [0u8; 64 * 1024];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            let room = cap.saturating_sub(out.len());
            if n > room {
                truncated = true;
            }
            out.extend_from_slice(&buf[..n.min(room)]);
        }
        (out, truncated)
    })
}

/// 読み続けて（パイプを詰まらせない）、行ごとに伏せ字にした最後の [`STDERR_LOG_LINES`] 行を返す（[`StderrTail`]）。
fn read_tail<R: Read + Send + 'static>(mut r: R) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut tail = StderrTail::new(STDERR_LOG_LINES);
        let mut buf = [0u8; 16 * 1024];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            tail.push(&buf[..n]);
        }
        tail.finish()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::RemotePath;
    use serde_json::json;

    fn settings() -> Settings {
        Settings::from_raw(&crate::settings::RawSettings {
            rclone: Some("/x/rclone".into()),
            rclone_config: Some("/x/state/rclone/rclone.conf".into()),
            state_dir: Some("/x/state".into()),
            ..Default::default()
        })
        .unwrap()
    }

    fn strs(a: &[OsString]) -> Vec<&str> {
        a.iter().map(|s| s.to_str().unwrap()).collect()
    }

    fn loc(s: &str) -> RemoteLoc {
        let mut parts = s.split('/');
        let base = RemotePath::parse(parts.next().unwrap()).unwrap();
        parts.fold(RemoteLoc::of(&base), |l, c| l.child(c).unwrap())
    }

    #[test]
    fn every_command_names_config_and_ends_flags_with_dashes() {
        let r = Rclone::new(&settings());
        let l = loc("gdrive:p/q");
        let all: Vec<Vec<OsString>> = vec![
            r.lsjson_args(&l, true, true, false),
            r.lsjson_args(&l, false, false, false),
            r.cat_args(&l, 10),
            r.mkdir_args(&l),
            r.copy_args(Path::new("/w/ws"), &l, CopyScope::PhotosOnly),
            r.copy_args(Path::new("/w/ws"), &l, CopyScope::ManifestOnly),
            r.moveto_args(&l, &loc("gdrive:p/r")),
            r.purge_args(&l),
        ];
        for args in &all {
            let s = strs(args);
            let dashes = s.iter().position(|a| *a == "--").expect("-- がある");
            let (flags, positional) = s.split_at(dashes);
            // 設定ファイルは毎回明示する
            let ci = flags.iter().position(|a| *a == "--config").unwrap();
            assert_eq!(flags[ci + 1], "/x/state/rclone/rclone.conf");
            assert!(flags.contains(&"--ask-password=false"));
            for pair in [
                ["--retries", "3"],
                ["--low-level-retries", "10"],
                ["--transfers", "4"],
                ["--checkers", "8"],
            ] {
                let i = flags.iter().position(|a| *a == pair[0]).expect(pair[0]);
                assert_eq!(flags[i + 1], pair[1]);
            }
            // 位置の引数は `--` の後ろだけ。flag の部分に、場所のような文字列は無い
            assert!(
                !flags.iter().skip(1).any(|a| a.contains("gdrive:")),
                "{flags:?}"
            );
            assert!(positional.len() >= 2);
            // 禁止の flag は、組み立てた引数にも無い
            for f in flags {
                assert!(!is_forbidden_flag(f), "{f}");
            }
        }
    }

    #[test]
    fn flags_for_each_command_match_the_design() {
        let r = Rclone::new(&settings());
        let l = loc("gdrive:p/q");
        let has = |a: &[OsString], f: &str| strs(a).contains(&f);
        // Drive の操作には --drive-skip-shortcuts。ショートカットも見たい照会だけは `=false` を明示する（設定ファイルで
        // skip_shortcuts が有効でも、解決されたショートカットが一覧に出る）
        assert!(has(
            &r.lsjson_args(&l, false, true, false),
            "--drive-skip-shortcuts"
        ));
        assert!(!has(
            &r.lsjson_args(&l, false, false, false),
            "--drive-skip-shortcuts"
        ));
        assert!(has(
            &r.lsjson_args(&l, false, false, false),
            "--drive-skip-shortcuts=false"
        ));
        assert!(!has(
            &r.lsjson_args(&l, false, true, false),
            "--drive-skip-shortcuts=false"
        ));
        assert!(has(
            &r.lsjson_args(&l, false, false, false),
            "--drive-skip-dangling-shortcuts=false"
        ));
        // 一覧は毎回、エクスポートできない Google のファイルも出し、gdocs を除く設定を打ち消す
        for skip in [true, false] {
            for recursive in [true, false] {
                let a = r.lsjson_args(&l, recursive, skip, false);
                assert!(has(&a, "--drive-show-all-gdocs"));
                assert!(has(&a, "--drive-skip-gdocs=false"));
            }
        }
        // MD5 は、頼んだ一覧だけに付ける（`--hash --hash-type MD5`。付けないと `Hashes` が出ない）
        let hashed_args = r.lsjson_args(&l, true, true, true);
        let hashed = strs(&hashed_args);
        let i = hashed.iter().position(|a| *a == "--hash").expect("--hash");
        assert_eq!(hashed[i + 1], "--hash-type");
        assert_eq!(hashed[i + 2], "MD5");
        assert!(!has(&r.lsjson_args(&l, true, true, false), "--hash"));
        assert!(!has(&r.lsjson_args(&l, true, true, false), "--hash-type"));
        assert!(!has(&r.mkdir_args(&l), "--drive-show-all-gdocs"));
        for a in [
            r.cat_args(&l, 1),
            r.mkdir_args(&l),
            r.copy_args(Path::new("/w"), &l, CopyScope::PhotosOnly),
            r.moveto_args(&l, &l),
            r.purge_args(&l),
        ] {
            assert!(has(&a, "--drive-skip-shortcuts"));
        }
        // copy・moveto は --immutable、purge は --drive-use-trash=true
        assert!(has(
            &r.copy_args(Path::new("/w"), &l, CopyScope::PhotosOnly),
            "--immutable"
        ));
        assert!(has(&r.moveto_args(&l, &l), "--immutable"));
        assert!(has(&r.purge_args(&l), "--drive-use-trash=true"));
        assert!(!has(&r.lsjson_args(&l, true, true, false), "--immutable"));
        // 再帰の指定
        assert!(has(&r.lsjson_args(&l, true, true, false), "-R"));
        assert!(!has(&r.lsjson_args(&l, false, true, false), "-R"));
        // manifest の分け方
        let photos = r.copy_args(Path::new("/w"), &l, CopyScope::PhotosOnly);
        let manifest = r.copy_args(Path::new("/w"), &l, CopyScope::ManifestOnly);
        assert!(
            strs(&photos)
                .windows(2)
                .any(|w| w == ["--exclude", "/manifest.json"])
        );
        assert!(
            strs(&manifest)
                .windows(2)
                .any(|w| w == ["--include", "/manifest.json"])
        );
        // cat は読む量を決める
        assert!(
            strs(&r.cat_args(&l, 7))
                .windows(2)
                .any(|w| w == ["--count", "7"])
        );
    }

    #[test]
    fn forbidden_flags_are_refused() {
        for bad in [
            "--drive-client-id",
            "--drive-client-id=x",
            "--drive-client-secret",
            "--drive-token",
            "--drive-token=x",
            "--drive-service-account-file",
            "--drive-impersonate",
            "--drive-env-auth",
            "--dump",
            "--dump=headers",
            "--dump-bodies",
            "-v",
            "-vv",
            "-vvv",
            "--verbose",
            "--config-x",
            "--rc",
            "--rc-addr",
            "--password-command",
            "--use-json-log",
        ] {
            assert!(is_forbidden_flag(bad), "{bad}");
        }
        for ok in [
            "--config",
            "--drive-skip-shortcuts",
            "--drive-use-trash=true",
            "-R",
            "--",
            "--immutable",
            "--cache-dir",
        ] {
            assert!(!is_forbidden_flag(ok), "{ok}");
        }
    }

    #[test]
    fn stderr_is_redacted_per_line_and_trimmed_to_the_tail() {
        let raw = b"line one\nERROR : token expired\nAuthorization: Bearer abc\nBEARER xyz\n\"access_token\":\"s\"\nclient_secret=z\n\nplain tail\n";
        let out = redact_stderr(raw, 10);
        for secret in ["token expired", "abc", "xyz", "\"s\"", "z\n"] {
            assert!(!out.contains(secret), "{out}");
        }
        assert!(out.contains("line one"));
        assert!(out.ends_with("plain tail"));
        assert_eq!(out.matches("[伏せ字]").count(), 5);
        // 末尾の行だけ
        let many: String = (0..20).map(|i| format!("l{i}\n")).collect();
        assert_eq!(redact_stderr(many.as_bytes(), 3), "l17 / l18 / l19");
        // 大文字小文字を問わない・制御文字は空白・長い行は切る
        assert_eq!(redact_stderr(b"ToKeN\n", 3), "[伏せ字]");
        assert_eq!(redact_stderr(b"a\x1b[31mb\n", 3), "a [31mb");
        let long = "x".repeat(1000);
        assert_eq!(
            redact_stderr(long.as_bytes(), 1).chars().count(),
            STDERR_LINE_MAX_CHARS
        );
        // UTF-8 でないバイトがあっても落ちない
        assert!(!redact_stderr(&[0xff, 0xfe, b'\n'], 1).is_empty());
        assert_eq!(redact_stderr(b"", 3), "");
    }

    #[test]
    fn a_long_line_is_redacted_as_a_whole_before_the_tail_is_kept() {
        // 先頭に Bearer がある、とても長い行（末尾だけを先に切り出す作りだと、先頭が落ちて残りが出る）
        let secret = "SECRETSECRETSECRET";
        let mut raw = format!("Authorization: Bearer {secret}").into_bytes();
        raw.extend(std::iter::repeat_n(b'x', 3 * STDERR_LINE_CAP_BYTES));
        raw.extend_from_slice(secret.as_bytes());
        raw.extend_from_slice(b"\nERROR : plain tail\n");
        let out = redact_stderr(&raw, 5);
        assert!(
            !out.contains(secret) && !out.contains("xxxx"),
            "{}",
            &out[..out.len().min(200)]
        );
        assert_eq!(out, "[伏せ字] / ERROR : plain tail");
        // 長いが機微な語を含まない行も、中身を見ずに伏せ字にする（持っていない部分に何があるか分からない）
        let mut raw = vec![b'y'; 2 * STDERR_LINE_CAP_BYTES];
        raw.extend_from_slice(b"\nok\n");
        assert_eq!(redact_stderr(&raw, 5), "[伏せ字] / ok");
        // 区切りの位置が変わっても同じ結果（1 バイトずつ・不揃いに渡す）
        let text = b"one\r\nERROR : token=abc\n\nBearer zzz tail\nlast line without newline";
        let whole = redact_stderr(text, 10);
        for step in [1usize, 2, 3, 7] {
            let mut t = StderrTail::new(10);
            for c in text.chunks(step) {
                t.push(c);
            }
            assert_eq!(t.finish(), whole, "step={step}");
        }
        assert_eq!(
            whole,
            "one / [伏せ字] / [伏せ字] / last line without newline"
        );
        assert!(!whole.contains("abc") && !whole.contains("zzz"));
        // 最後の数行だけを残す
        let many: String = (0..100).map(|i| format!("l{i}\n")).collect();
        let mut t = StderrTail::new(3);
        for c in many.as_bytes().chunks(5) {
            t.push(c);
        }
        assert_eq!(t.finish(), "l97 / l98 / l99");
    }

    fn entry(path: &str, id: &str, dir: bool) -> serde_json::Value {
        json!({
            "Path": path,
            "Name": path.rsplit('/').next().unwrap(),
            "Size": if dir { -1 } else { 10 },
            "MimeType": if dir { "inode/directory" } else { "image/jpeg" },
            "ModTime": "2030-01-01T00:00:00.000Z",
            "IsDir": dir,
            "ID": id,
        })
    }

    #[test]
    fn lsjson_output_shape_is_checked() {
        let good = json!([entry("a", "id1", true), entry("a/b.jpg", "id2", false)]);
        let parsed = parse_lsjson(good.to_string().as_bytes()).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "id1");
        assert!(parsed[0].is_dir);
        assert_eq!(parsed[1].path, "a/b.jpg");
        assert_eq!(parsed[1].name, "b.jpg");
        assert_eq!(parsed[1].size, 10);
        assert_eq!(parse_lsjson(b"[]").unwrap(), vec![]);
        assert_eq!(parse_lsjson(b"[\n]\n").unwrap(), vec![]);

        let bad_cases: Vec<serde_json::Value> = vec![
            json!({"a": 1}),
            json!([1]),
            json!([{"Path": "a"}]),
            // ID が無い・空・形が違う
            json!([{"Path":"a","Name":"a","Size":1,"MimeType":"x/y","IsDir":false}]),
            json!([entry("a", "", false)]),
            json!([entry("a", "bad id", false)]),
            json!([entry("a", "bad/id", false)]),
            // Name が Path の最後と違う
            json!([{"Path":"a/b","Name":"c","Size":1,"MimeType":"x/y","IsDir":false,"ID":"i"}]),
            // パスの形
            json!([entry("/a", "i", false)]),
            json!([entry("a/", "i", false)]),
            json!([entry("../a", "i", false)]),
            json!([entry("a//b", "i", false)]),
            json!([entry("", "i", false)]),
            // 型が違う
            json!([{"Path":"a","Name":"a","Size":"1","MimeType":"x/y","IsDir":false,"ID":"i"}]),
            json!([{"Path":"a","Name":"a","Size":1,"MimeType":"x/y","IsDir":"no","ID":"i"}]),
            json!([{"Path":"a","Name":"a","Size":1,"MimeType":5,"IsDir":false,"ID":"i"}]),
            // IsDir と MimeType が食い違う、ファイルの Size が負
            json!([{"Path":"a","Name":"a","Size":-1,"MimeType":"inode/directory","IsDir":false,"ID":"i"}]),
            json!([{"Path":"a","Name":"a","Size":-1,"MimeType":"x/y","IsDir":true,"ID":"i"}]),
            // 複合 ID の形（どちらかが空・3 つ・形が違う）
            json!([entry("a", "i\t", false)]),
            json!([entry("a", "\ti", false)]),
            json!([entry("a", "i\tj\tk", false)]),
            json!([entry("a", "i\tb d", false)]),
        ];
        for bad in bad_cases {
            assert!(parse_lsjson(bad.to_string().as_bytes()).is_none(), "{bad}");
        }
        assert!(parse_lsjson(b"not json").is_none());
        assert!(parse_lsjson(b"").is_none());
    }

    #[test]
    fn md5_is_read_from_hashes_and_a_malformed_value_means_no_md5() {
        let md5 = "0123456789abcdef0123456789ABCDEF";
        let with = |hashes: serde_json::Value| {
            let mut e = entry("a.jpg", "i", false);
            e["Hashes"] = hashes;
            parse_lsjson(json!([e]).to_string().as_bytes()).expect("一覧は断らない")
        };
        // 小文字にそろえる
        assert_eq!(
            with(json!({"md5": md5}))[0].md5.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        // 付いていない・空（MD5 を持たないファイル）・形が違うものは MD5 なし
        assert_eq!(
            parse_lsjson(json!([entry("a.jpg", "i", false)]).to_string().as_bytes()).unwrap()[0]
                .md5,
            None
        );
        for bad in [
            json!({}),
            json!({"md5": ""}),
            json!({"md5": "xyz"}),
            json!({"md5": "0123456789abcdef0123456789abcde"}),
            json!({"md5": "0123456789abcdef0123456789abcdef0"}),
            json!({"md5": 5}),
            // キーは小文字の `md5`（rclone の `hash.MD5.String()`）。別の名前は読まない
            json!({"MD5": md5}),
            json!({"sha1": md5}),
            json!("md5"),
        ] {
            assert_eq!(with(bad.clone())[0].md5, None, "{bad}");
        }
        // フォルダには付けない
        let mut d = entry("d", "i", true);
        d["Hashes"] = json!({"md5": md5});
        assert_eq!(
            parse_lsjson(json!([d]).to_string().as_bytes()).unwrap()[0].md5,
            None
        );
    }

    #[test]
    fn unknown_sizes_and_composite_ids_are_read_without_rejecting_the_whole_listing() {
        // ネイティブの Google 文書・エクスポートできないファイル（--drive-show-all-gdocs）: ファイルで Size が -1
        let doc = json!({"Path":"memo","Name":"memo","Size":-1,"MimeType":"application/vnd.google-apps.document","IsDir":false,"ID":"idD"});
        // 解決されたショートカット: リンク先の ID とショートカットの ID をタブで結んだ複合 ID
        let resolved_dir = json!({"Path":"link","Name":"link","Size":-1,"MimeType":"inode/directory","IsDir":true,"ID":"idT\tidS"});
        let resolved_file = json!({"Path":"pic","Name":"pic","Size":5,"MimeType":"image/jpeg","IsDir":false,"ID":"idT2\tidS2"});
        // リンク切れ: MIME タイプが .dangling、ID はショートカット自身
        let dangling = json!({"Path":"gone","Name":"gone","Size":0,"MimeType":"application/vnd.google-apps.shortcut.dangling","IsDir":false,"ID":"idG"});
        let plain = entry("a.jpg", "idP", false);
        let all = json!([doc, resolved_dir, resolved_file, dangling, plain]);
        let got = parse_lsjson(all.to_string().as_bytes()).expect("全体を断らない");
        assert_eq!(got.len(), 5);
        assert_eq!((got[0].size, got[0].is_shortcut()), (-1, false));
        assert_eq!(got[1].id, "idT");
        assert_eq!(got[1].shortcut_id.as_deref(), Some("idS"));
        assert!(got[1].is_shortcut() && got[1].is_dir);
        assert!(got[2].is_shortcut());
        assert_eq!(got[3].shortcut_id, None);
        assert!(
            got[3].is_shortcut(),
            "リンク切れも、ショートカットとして数える"
        );
        assert!(!got[4].is_shortcut() && got[4].shortcut_id.is_none());
    }
}
