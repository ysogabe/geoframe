//! `crates` の Rust のコードが Python を起動しないことの検査（P5b / T5.6a。受け入れ条件 AC7）。
//!
//! Python 版をリポジトリから外したあとに、試験や build.rs がまた Python を呼ぶようになると、CI の Rust のジョブに
//! Python の導入が戻る。文字列で `python` を探すだけでは、変数に入れた名前（`let exe = ..; Command::new(exe)`）、
//! `Command` の別名、`sh -c "python .."` を見逃す。そこで**プロセスを起動する箇所そのもの**を管理する。
//!
//! - `crates` 以下の `.rs` を字句に分け（コメントは捨て、文字列は 1 つの塊にする）、`Command::new(` の形の呼び出し
//!   （`use .. process::Command as X` や `type X = ..Command;` の別名、`<std::process::Command>::new(`・
//!   `<Command as X>::new(` のような山括弧の修飾パスを含む）を全部数える。最初の引数が文字列なら
//!   その文字列、そうでなければ式のまま（`BIN`、`std::env::current_exe().unwrap()` など）を「起動するもの」とする。
//!   `<_ as X>::new(` のように型が Command か読めない形は、未対応の起動構文として失敗にする。
//! - 数えた結果を `ALLOWED`（ファイル・起動するもの・件数）と**完全に**照合する。リストに無い起動が増えても、
//!   件数が増えても減っても失敗にする。増えたときは、何を起動するかを確かめてからリストに足す。
//!   式のままの起動はリストに式そのものを書いた場所だけ許す。
//! - 次は許可リストと無関係に失敗にする: 起動するものが python・pypy（`python3.x`・`pypy3.x`・パス付きを含む）、
//!   起動する関数の中に python の実行ファイル名の文字列がある（`let exe = "python"`、`env` に渡す引数など）、
//!   shell・`env` などを起動する関数の中や、shell に渡す `-c` の次の引数に、python・pypy の実行ファイル名の語を含む
//!   命令文がある（`sh -c "pypy3 -V"`、`cd x && python3 y.py`）、`tokio::process`・`CommandExt`・`exec*`・
//!   `posix_spawn` など `Command::new` 以外の起動の道具を使う。python の実行ファイル名の判定は `runs_python` の
//!   1 か所で、直接の起動・wrapper・`-c` のどれも同じ判定を使う。
//!   起動の道具の検査は、API を使う構文（`use` の import、`x::y` のパス、`name(` の呼び出し、`.pre_exec(`）だけを
//!   見る。同じ名前の変数・フィールド（`let subprocess = 42;`）は対象にしない。
//!   `use` の木は階層を追う（`use libc::{system};`・`use crate::{process, tokio::fs};`）。`tokio` 配下の `process`、
//!   `libc` の `system`・`popen`（グループ・別名・glob を含む）、`Command::new::<T>(..)` の型の引数つきの呼び出しも、
//!   通常の呼び出しと同じ検査をする。
//! - **底の検査**（起動の構文に頼らない）: `crates` 以下の全 `.rs` の**すべての文字列リテラル**（通常・生・`b".."`・
//!   `c".."`・`br#".."#` など。エスケープ `\x70ython` を戻したもの）を `runs_python` で判定し、python・pypy を
//!   実行ファイルとして読めるものがあれば、起動の箇所かどうかに関係なく失敗にする。上の構文の追跡が漏れても、
//!   python の名前が文字列として書いてあれば、ここで捕まる。誤検知（基準データの JSON のキー `"python"`、Python 版との
//!   比較の失敗の説明文など）は `LITERAL_ALLOWED`（ファイル・文字列の中身・理由）で個別に除く。使われなくなった項目は
//!   失敗にする。この試験ファイル自身は、印の行より後（検出器の入力の例と許可リスト）だけ文字列の検査から外す。
//!
//! 限界（捕まえられない書き方）: 文字列を実行時に組み立てる（`format!("py{}", ..)`、`concat!`・`+` での連結、
//! `char` や `u8` の並びから作る名前）、shell の引用の連結（`p""ython`）、変数展開（`PY=py; ${PY}thon`）、
//! 別の関数や別のファイルから渡る変数や引数、マクロの中で作る `Command`、`Command` を返す関数の呼び出し側
//! （`make().spawn()`）、別の名前での再公開を 2 段以上重ねた別名、`<T as Trait>::new(` で Command 以外の型の名前を
//! 通して Command を作る関連型。底の検査は文字列の中身しか見ない。基準データの JSON のキー `"python"` は、添字
//! （`case["python"]`）としての出現が**すべて**のファイルだけ許す（`Allow::IndexKey`）。添字でない `"python"` が 1 つでも入れば
//! 失敗にする。説明文（`Allow::Exact`）の許可は、その文章そのものにしか効かない。`exec` は試験の補助関数の名前
//! （`env.exec(..)`）と区別できないので、`CommandExt` を glob で import して `.exec()` を呼ぶ書き方は import の検査
//! （`process::*`・`CommandExt`）に頼る。式のままの起動は許可リストの式が何を指すかを人が確かめる前提。
//! コメントの中の説明は、起動するコードではないので対象にしない。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// 許可する起動（`crates` からの相対パス、起動するもの、件数）。起動するものは、文字列なら `Lit`、
/// 文字列でない式のままなら `Expr`（字句を詰めて並べた形）。
const ALLOWED: &[(&str, Exe, usize)] = &[
    // 本番の書き込みの前に、Python 版のサービスの稼働を `systemctl show` で確かめる（読み取りだけ）。
    ("geoframe/src/guard.rs", Exe::Expr("path"), 1),
    // 試験用の「状態を返す」代役。引数の `-c` の文字列は試験の中の固定の短い文だけ。
    ("geoframe/src/guard.rs", Exe::Lit("/bin/sh"), 1),
    // 名前付きパイプ（FIFO）を作って「通常ファイルでない」ものの扱いを確かめる。
    ("geoframe/src/cache.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/src/cli.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/src/rotate/tests.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/src/ai/tests_settings.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/src/drive/tests.rs", Exe::Lit("mkfifo"), 1),
    (
        "geoframe/src/ai/location_clue/tests.rs",
        Exe::Lit("mkfifo"),
        1,
    ),
    ("geoframe/src/ai/cloud/auth/tests.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/src/ai/groups/tests.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/src/tiles/tests.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/tests/blog_api.rs", Exe::Lit("mkfifo"), 1),
    ("geoframe/tests/static_files.rs", Exe::Lit("mkfifo"), 1),
    // 試験の実行ファイル自身を子プロセスとして 1 つの試験だけ動かす（環境変数のプロキシを使わない確認）。
    (
        "geoframe/src/ai/tests_http.rs",
        Exe::Expr("std::env::current_exe().unwrap()"),
        1,
    ),
    (
        "geoframe/src/ai/cloud/tests.rs",
        Exe::Expr("std::env::current_exe().unwrap()"),
        1,
    ),
    // 管理画面の描画を Node で確かめる（Node が無ければ省く）。
    ("geoframe/tests/admin_ui_ai_waits.rs", Exe::Lit("node"), 1),
    // ビルドした geoframe の実行ファイル（`CARGO_BIN_EXE_geoframe`）を動かす CLI の試験と、終了信号を送る `kill`。
    ("geoframe/tests/ingest_cli.rs", Exe::Expr("BIN"), 4),
    ("geoframe/tests/ingest_cli.rs", Exe::Lit("kill"), 1),
];

/// 他のコマンドを起動するコマンド。引数に python があれば python を起動するのと同じなので、
/// これを起動する関数の中の文字列を調べる。
const WRAPPERS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "ksh", "fish", "env", "xargs", "nohup", "nice", "timeout",
    "setsid", "sudo", "doas", "time", "busybox", "exec", "command", "stdbuf", "taskset", "chroot",
    "script", "flock", "ionice", "strace", "ltrace",
];

/// `Command::new` 以外の起動の道具。これらは許可リストにも載せず、使ったら失敗にする。
/// 起動用の API を使う構文（`use` の import、`x::y` のパス、`name(` の呼び出し、`.name(` のメソッド呼び出し）
/// だけを見る。同じ名前の変数・フィールド（`let subprocess = 42;`）は対象にしない。
const OTHER_LAUNCHERS: &[&str] = &[
    "CommandExt",
    "execv",
    "execve",
    "execvp",
    "execvpe",
    "execl",
    "execle",
    "execlp",
    "fexecve",
    "posix_spawn",
    "posix_spawnp",
    "async_process",
    "duct",
    "xshell",
    "subprocess",
    "cmd_lib",
];

/// `CommandExt` のメソッド。トレイトを glob で import していても、`.name(` で呼べば失敗にする。
/// （`exec` は試験の補助関数の名前に使われているので入れない。限界に書く。）
const LAUNCHER_METHODS: &[&str] = &["pre_exec"];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Exe<S = &'static str> {
    Lit(S),
    Expr(S),
}

impl<S: fmt::Debug + fmt::Display> fmt::Display for Exe<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exe::Lit(s) => write!(f, "文字列 {s:?}"),
            Exe::Expr(s) => write!(f, "式 {s}"),
        }
    }
}

impl Exe<&'static str> {
    fn owned(&self) -> Exe<String> {
        match self {
            Exe::Lit(s) => Exe::Lit((*s).to_string()),
            Exe::Expr(s) => Exe::Expr((*s).to_string()),
        }
    }
}

// ---- 字句 ----

#[derive(Debug, PartialEq)]
enum Kind {
    Ident(String),
    Str(String),
    Punct(char),
}

#[derive(Debug)]
struct Tok {
    kind: Kind,
    line: usize,
}

/// Rust の字句への粗い分割。コメントは捨てる。文字列（生文字列も）は中身ごと 1 つの `Str` にする。
/// 文字リテラルとライフタイムは区別して捨てる。完全な構文解析ではない。
fn lex(src: &str) -> Vec<Tok> {
    let c: Vec<char> = src.chars().collect();
    let at = |i: usize| c.get(i).copied();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && at(i + 1) == Some('/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && at(i + 1) == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < c.len() && depth > 0 {
                if c[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    i += 2;
                } else {
                    if c[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if ch == '"' {
            let start_line = line;
            let (text, next) = read_string(&c, i + 1, &mut line);
            out.push(Tok {
                kind: Kind::Str(text),
                line: start_line,
            });
            i = next;
        } else if ch == '\'' {
            // 文字リテラル（'x'、'\n'、'"'）は捨てる。ライフタイム（'a）は印だけ読み飛ばす
            if at(i + 1) == Some('\\') {
                let mut j = i + 3;
                while j < c.len() && c[j] != '\'' {
                    j += if c[j] == '\\' { 2 } else { 1 };
                }
                i = j + 1;
            } else if at(i + 2) == Some('\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let ident: String = c[start..i].iter().collect();
            if matches!(ident.as_str(), "r" | "br" | "cr") && matches!(at(i), Some('#' | '"')) {
                let start_line = line;
                if let Some((text, next)) = read_raw(&c, i, &mut line) {
                    out.push(Tok {
                        kind: Kind::Str(text),
                        line: start_line,
                    });
                    i = next;
                    continue;
                }
            }
            out.push(Tok {
                kind: Kind::Ident(ident),
                line,
            });
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok {
                kind: Kind::Ident(c[start..i].iter().collect()),
                line,
            });
        } else {
            out.push(Tok {
                kind: Kind::Punct(ch),
                line,
            });
            i += 1;
        }
    }
    out
}

/// `"` の次から、閉じる `"` までを読む。中身（エスケープは戻さない）と、閉じた次の位置を返す。
fn read_string(c: &[char], mut i: usize, line: &mut usize) -> (String, usize) {
    let start = i;
    while i < c.len() && c[i] != '"' {
        if c[i] == '\\' && i + 1 < c.len() {
            if c[i + 1] == '\n' {
                *line += 1;
            }
            i += 2;
        } else {
            if c[i] == '\n' {
                *line += 1;
            }
            i += 1;
        }
    }
    let raw: String = c[start..i.min(c.len())].iter().collect();
    (unescape(&raw), i + 1)
}

/// 通常の文字列（`"`・`b".."`・`c".."`）のエスケープを戻す（`\x70ython`・`\u{70}ython`・`\n`・行末の `\`）。
/// 読めないエスケープはそのまま残す。生文字列には使わない。
fn unescape(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let hex = |from: usize, to: usize| -> Option<char> {
        let digits: String = c.get(from..to)?.iter().collect();
        char::from_u32(u32::from_str_radix(&digits, 16).ok()?)
    };
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '\\' || i + 1 >= c.len() {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let simple = match c[i + 1] {
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '0' => Some('\0'),
            '\\' | '\'' | '"' => Some(c[i + 1]),
            _ => None,
        };
        if let Some(ch) = simple {
            out.push(ch);
            i += 2;
        } else if c[i + 1] == '\n' {
            // 行末の `\` は、次の行の先頭の空白までを読み飛ばす
            i += 2;
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
        } else if c[i + 1] == 'x'
            && let Some(ch) = hex(i + 2, i + 4)
        {
            out.push(ch);
            i += 4;
        } else if c[i + 1] == 'u'
            && c.get(i + 2) == Some(&'{')
            && let Some(close) = (i + 3..c.len()).find(|&k| c[k] == '}')
            && let Some(ch) = hex(i + 3, close)
        {
            out.push(ch);
            i = close + 1;
        } else {
            out.push('\\');
            i += 1;
        }
    }
    out
}

/// `r` の次（`#` か `"`）から生文字列を読む。生文字列でなければ `None`（`r#ident` など）。
fn read_raw(c: &[char], mut i: usize, line: &mut usize) -> Option<(String, usize)> {
    let mut hashes = 0;
    while c.get(i) == Some(&'#') {
        hashes += 1;
        i += 1;
    }
    if c.get(i) != Some(&'"') {
        return None;
    }
    i += 1;
    let start = i;
    while i < c.len() {
        if c[i] == '"' && (0..hashes).all(|k| c.get(i + 1 + k) == Some(&'#')) {
            let text = c[start..i].iter().collect();
            return Some((text, i + 1 + hashes));
        }
        if c[i] == '\n' {
            *line += 1;
        }
        i += 1;
    }
    None
}

// ---- 解析 ----

fn ident(t: &[Tok], i: usize) -> Option<&str> {
    match t.get(i).map(|t| &t.kind) {
        Some(Kind::Ident(s)) => Some(s),
        _ => None,
    }
}

fn string(t: &[Tok], i: usize) -> Option<&str> {
    match t.get(i).map(|t| &t.kind) {
        Some(Kind::Str(s)) => Some(s),
        _ => None,
    }
}

fn punct(t: &[Tok], i: usize, p: char) -> bool {
    matches!(t.get(i).map(|t| &t.kind), Some(Kind::Punct(c)) if *c == p)
}

fn is_path_sep(t: &[Tok], i: usize) -> bool {
    punct(t, i, ':') && punct(t, i + 1, ':')
}

/// `start` から始まる 1 つ目の引数（深さ 0 の `,` か閉じ括弧の手前まで）の範囲の終わり。
fn arg_end(t: &[Tok], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while let Some(tok) = t.get(i) {
        if let Kind::Punct(p) = tok.kind {
            match p {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    if depth == 0 {
                        return i;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => return i,
                _ => {}
            }
        }
        i += 1;
    }
    i
}

fn text_of(t: &[Tok]) -> String {
    t.iter()
        .map(|t| match &t.kind {
            Kind::Ident(s) => s.clone(),
            Kind::Str(s) => format!("{s:?}"),
            Kind::Punct(c) => c.to_string(),
        })
        .collect()
}

/// python の実行ファイルの名前（`python`・`python3`・`python3.12`・`/usr/bin/python3`・`pypy3`）か。
fn is_python_exe(s: &str) -> bool {
    let base = s
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    ["python", "pypy"].iter().any(|p| {
        base.strip_prefix(p).is_some_and(|rest| {
            rest.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == 'w')
        })
    })
}

/// 文字列が python の実行ファイルを指す・起動するか。文字列全体が実行ファイルの名前（`python3`・`pypy3`・
/// `/usr/bin/python3.12`）の場合と、shell の命令文（`cd x && pypy3 y.py`、`PY=python3 ./run`）の語のどれかが
/// そうである場合。直接起動・wrapper の引数・shell の `-c` の検査はすべてこの 1 つの判定を使う。
fn runs_python(s: &str) -> bool {
    is_python_exe(s)
        || s.split(|c: char| !(c.is_alphanumeric() || "._/-+".contains(c)))
            .any(is_python_exe)
}

fn is_wrapper(exe: &str) -> bool {
    let base = exe.rsplit('/').next().unwrap_or("");
    WRAPPERS.contains(&base)
}

/// `Command` の別名（`use ..process::Command as X;`、`type X = ..Command;`）。`known` は既知の名前。
fn aliases_in(t: &[Tok], known: &BTreeSet<String>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut last_use = None;
    for i in 0..t.len() {
        match ident(t, i) {
            Some("use") => last_use = Some(i),
            Some(name) if known.contains(name) && ident(t, i + 1) == Some("as") => {
                let in_process = last_use.is_some_and(|u| {
                    (u..i).any(|k| ident(t, k) == Some("process")) && !punct_between(t, u, i, ';')
                });
                if let Some(alias) = ident(t, i + 2)
                    && (in_process || name != "Command")
                {
                    found.insert(alias.to_string());
                }
            }
            Some("type") => {
                if let (Some(alias), true) = (ident(t, i + 1), punct(t, i + 2, '=')) {
                    let mut k = i + 3;
                    while k < t.len() && !punct(t, k, ';') {
                        if ident(t, k).is_some_and(|n| known.contains(n)) {
                            found.insert(alias.to_string());
                        }
                        k += 1;
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn punct_between(t: &[Tok], from: usize, to: usize, p: char) -> bool {
    (from..to).any(|k| punct(t, k, p))
}

struct Site {
    line: usize,
    exe: Exe<String>,
}

#[derive(Default)]
struct Analysis {
    /// `Command::new(..)` の呼び出し。
    sites: Vec<Site>,
    /// 許可リストと無関係に失敗にする箇所（行、理由）。
    problems: Vec<(usize, String)>,
    /// python・pypy を実行ファイルとして読める文字列リテラル（行、エスケープを戻した中身、添字 `x["python"]` か）。
    /// 起動の箇所かどうかに関係なく、実際の `crates` の検査では許可リストに無ければ失敗にする。
    literals: Vec<(usize, String, bool)>,
}

/// 位置 `i` を含む関数の字句の範囲（見つからなければ前後の少しの範囲）。
fn enclosing_fn(t: &[Tok], i: usize) -> (usize, usize) {
    if let Some(f) = (0..i).rev().find(|&k| ident(t, k) == Some("fn"))
        && let Some(open) = (f..t.len()).find(|&k| punct(t, k, '{'))
    {
        let mut depth = 0usize;
        for (k, tok) in t.iter().enumerate().skip(open) {
            match tok.kind {
                Kind::Punct('{') => depth += 1,
                Kind::Punct('}') => {
                    depth -= 1;
                    if depth == 0 {
                        if k >= i {
                            return (f, k);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    (i.saturating_sub(40), (i + 40).min(t.len()))
}

/// 添字（`case["python"]`、`x[..]["python"]`）として使われている文字列か。
fn is_index(t: &[Tok], i: usize) -> bool {
    punct(t, i.wrapping_sub(1), '[')
        && punct(t, i + 1, ']')
        && i >= 2
        && (matches!(t[i - 2].kind, Kind::Ident(_)) || punct(t, i - 2, ')') || punct(t, i - 2, ']'))
}

/// 位置 `i` の識別子が、`use` の文（`;` まで）の中にあるか。
fn in_use_statement(t: &[Tok], i: usize) -> bool {
    for k in (0..i).rev() {
        match &t[k].kind {
            Kind::Ident(s) if s == "use" => return true,
            // `use a::{b, c}` の `}` では止めない
            Kind::Punct(';') => return false,
            Kind::Punct('{') if !is_path_sep(t, k.wrapping_sub(2)) => return false,
            _ => {}
        }
    }
    false
}

/// `use` の木（`use a::{b, c::{d as e, *}};`）を 1 本ずつの経路に開いたもの。
struct UsePath {
    /// 先頭から順の経路（`self` は親の名前そのものとして畳む）。
    segs: Vec<String>,
    /// 末尾が `*`。
    glob: bool,
    /// `as X` が付いている。
    alias: bool,
}

/// `start` から `end`（`;` の位置）までの `use` の木 1 本を、`prefix` の下の経路に開いて `out` に足す。
/// 読んだ次の位置を返す（必ず 1 つ以上進む）。
fn use_tree(
    t: &[Tok],
    start: usize,
    end: usize,
    prefix: &[String],
    out: &mut Vec<UsePath>,
) -> usize {
    let mut segs = prefix.to_vec();
    let mut i = start;
    if is_path_sep(t, i) {
        i += 2;
    }
    while i < end {
        match &t[i].kind {
            Kind::Ident(s) if s != "as" => {
                if !(s == "self" && !segs.is_empty()) {
                    segs.push(s.clone());
                }
                i += 1;
            }
            Kind::Punct('*') => {
                out.push(UsePath {
                    segs,
                    glob: true,
                    alias: false,
                });
                return i + 1;
            }
            Kind::Punct('{') => {
                i += 1;
                while i < end && !punct(t, i, '}') {
                    let before = i;
                    i = use_tree(t, i, end, &segs, out);
                    if punct(t, i, ',') {
                        i += 1;
                    }
                    if i == before {
                        i += 1;
                    }
                }
                return i + 1;
            }
            _ => break,
        }
        if is_path_sep(t, i) {
            i += 2;
        } else {
            break;
        }
    }
    let alias = ident(t, i) == Some("as");
    if alias {
        i += 2;
    }
    out.push(UsePath {
        segs,
        glob: false,
        alias,
    });
    i.max(start + 1)
}

/// `use` の文 1 つ分（`use` の位置から）の、開いた経路。
fn use_paths(t: &[Tok], use_at: usize) -> Vec<UsePath> {
    let end = (use_at..t.len())
        .find(|&k| punct(t, k, ';'))
        .unwrap_or(t.len());
    let mut out = Vec::new();
    use_tree(t, use_at + 1, end, &[], &mut out);
    out
}

/// `use` の経路 1 本が、起動の道具に届くものなら、その理由。階層を追うので、同じ文の別の枝の名前
/// （`use crate::{process, tokio::fs};`）では誤検知しない。
fn use_path_problem(p: &UsePath) -> Option<String> {
    let last = p.segs.last().map(String::as_str);
    if p.segs
        .windows(2)
        .any(|w| w[0] == "tokio" && w[1] == "process")
    {
        return Some("`tokio::process` は使えない".to_string());
    }
    if last == Some("tokio") && (p.glob || p.alias) {
        return Some(
            "`tokio` の glob import・別名は使えない（`tokio::process` に届く）".to_string(),
        );
    }
    if last == Some("libc") && (p.glob || p.alias) {
        return Some(
            "`libc` の glob import・別名は使えない（`system`・`popen` に届く）".to_string(),
        );
    }
    if p.segs.iter().any(|s| s == "libc") && matches!(last, Some("system" | "popen")) {
        return Some("`libc::system`・`popen` は使えない".to_string());
    }
    if p.glob && last == Some("process") {
        return Some(
            "`process::*` の glob import は使えない（起動の道具が見えなくなる）".to_string(),
        );
    }
    None
}

/// `new` の位置 `i` から、呼び出しの `(` の位置。`new::<&str>(` の型の引数は読み飛ばす。呼び出しでなければ `None`。
fn call_open(t: &[Tok], i: usize) -> Option<usize> {
    if punct(t, i + 1, '(') {
        return Some(i + 1);
    }
    if !(is_path_sep(t, i + 1) && punct(t, i + 3, '<')) {
        return None;
    }
    let mut depth = 0usize;
    for k in i + 3..(i + 3 + 80).min(t.len()) {
        match t[k].kind {
            Kind::Punct('<') => depth += 1,
            Kind::Punct('>') => {
                depth -= 1;
                if depth == 0 {
                    return punct(t, k + 1, '(').then_some(k + 1);
                }
            }
            Kind::Punct(';' | '{' | '}') => return None,
            _ => {}
        }
    }
    None
}

/// `Command::new` 以外の起動の道具を使う構文なら、その理由。名前が同じだけの変数・フィールドは対象にしない。
fn launcher_use(t: &[Tok], i: usize, name: &str) -> Option<String> {
    let listed = OTHER_LAUNCHERS.contains(&name);
    let method = LAUNCHER_METHODS.contains(&name);
    if !listed && !method {
        return None;
    }
    let after_dot = i > 0 && punct(t, i - 1, '.');
    let called = punct(t, i + 1, '(');
    let path = is_path_sep(t, i + 1) || (i >= 2 && is_path_sep(t, i - 2));
    let used = if method {
        after_dot && called
    } else {
        in_use_statement(t, i) || path || (called && !after_dot)
    };
    used.then(|| format!("`{name}` は使えない（Command::new 以外の起動の道具）"))
}

/// `X::new(` の `X` にあたる字句（`new` の 3 つ前）の読み取りの結果。
enum Receiver {
    /// `Command`（またはその別名）の `::new`。値は式の始まりの位置。
    Command(usize),
    /// `Command` ではない（`Vec::<T>::new`、`<String>::new` など）。
    Other,
    /// 型を読み取れない `<..>::new(`。
    Unsupported(usize),
}

fn receiver(t: &[Tok], j: usize, names: &BTreeSet<String>) -> Receiver {
    match &t[j].kind {
        Kind::Ident(n) if names.contains(n) => Receiver::Command(j),
        Kind::Punct('>') => {
            // 山括弧で囲んだ型の修飾パス（`<Command>::new`、`<std::process::Command as X>::new`）。
            let mut depth = 0usize;
            let mut open = None;
            for k in (j.saturating_sub(80)..=j).rev() {
                match t[k].kind {
                    Kind::Punct('>') => depth += 1,
                    Kind::Punct('<') => {
                        depth -= 1;
                        if depth == 0 {
                            open = Some(k);
                            break;
                        }
                    }
                    Kind::Punct(';' | '{' | '}') => break,
                    _ => {}
                }
            }
            let Some(open) = open else {
                return Receiver::Unsupported(j);
            };
            // `Vec::<u8>::new` の `::<..>` は型の引数で、修飾パスではない
            if open >= 2 && is_path_sep(t, open - 2) {
                return Receiver::Other;
            }
            // `as` の手前の型の、一番外側の最後の識別子
            let (mut nest, mut last) = (0usize, None);
            for tok in &t[open + 1..j] {
                match &tok.kind {
                    Kind::Punct('<' | '(' | '[') => nest += 1,
                    Kind::Punct('>' | ')' | ']') => nest = nest.saturating_sub(1),
                    Kind::Ident(s) if nest == 0 && s == "as" => break,
                    Kind::Ident(s) if nest == 0 => last = Some(s.as_str()),
                    _ => {}
                }
            }
            match last {
                Some(n) if names.contains(n) => Receiver::Command(open),
                Some("_") | None => Receiver::Unsupported(open),
                Some(_) => Receiver::Other,
            }
        }
        _ => Receiver::Other,
    }
}

/// 1 つのファイルの解析。`outer` は他のファイルで見つかった `Command` の別名。
fn analyze(text: &str, outer: &BTreeSet<String>) -> Analysis {
    let t = lex(text);
    let mut names = outer.clone();
    names.insert("Command".to_string());
    names.extend(aliases_in(&t, &names));
    let mut a = Analysis::default();

    for i in 0..t.len() {
        let Some(name) = ident(&t, i) else { continue };

        // 起動の道具（`Command::new` 以外）
        if let Some(why) = launcher_use(&t, i, name) {
            a.problems.push((t[i].line, why));
        }
        if name == "tokio" && is_path_sep(&t, i + 1) && ident(&t, i + 3) == Some("process") {
            a.problems
                .push((t[i].line, "`tokio::process` は使えない".to_string()));
        }
        if name == "libc"
            && is_path_sep(&t, i + 1)
            && matches!(ident(&t, i + 3), Some("system" | "popen"))
        {
            a.problems
                .push((t[i].line, "`libc::system`・`popen` は使えない".to_string()));
        }
        // 自分で `system`・`popen` を宣言する（`extern "C" { fn system(..); }`）
        if name == "fn"
            && matches!(ident(&t, i + 1), Some("system" | "popen"))
            && punct(&t, i + 2, '(')
        {
            a.problems.push((
                t[i].line,
                "libc の `system`・`popen` を自分で宣言している".to_string(),
            ));
        }
        // `use` の木は階層を追う（グループ・別名・glob）
        if name == "use" {
            for p in use_paths(&t, i) {
                if let Some(why) = use_path_problem(&p) {
                    a.problems.push((t[i].line, why));
                }
            }
        }
    }

    // 起動の構文に頼らない底: python と読める文字列リテラルは、どこにあっても記録する
    for (i, tok) in t.iter().enumerate() {
        if let Kind::Str(s) = &tok.kind
            && runs_python(s)
        {
            a.literals.push((tok.line, s.clone(), is_index(&t, i)));
        }
    }

    // Command::new(..)（`Command::new`、`<Command>::new`、`<std::process::Command as X>::new`、
    // `Command::new::<&str>(..)`）
    for i in 3..t.len() {
        if ident(&t, i) != Some("new") || !is_path_sep(&t, i - 2) {
            continue;
        }
        let start = match receiver(&t, i - 3, &names) {
            Receiver::Command(start) => start,
            Receiver::Other => continue,
            Receiver::Unsupported(at) => {
                a.problems.push((
                    t[at].line,
                    "未対応の起動構文（`<..>::new(` の型が Command かどうか読めない）".to_string(),
                ));
                continue;
            }
        };
        let Some(open) = call_open(&t, i) else {
            a.problems.push((
                t[start].line,
                "`Command::new` を呼び出さずに使っている（関数として渡している）".to_string(),
            ));
            continue;
        };
        let first = open + 1;
        let end = arg_end(&t, first);
        let arg = &t[first..end];
        let exe = match arg {
            [
                Tok {
                    kind: Kind::Str(s), ..
                },
            ] => Exe::Lit(s.clone()),
            [
                Tok {
                    kind: Kind::Punct('&'),
                    ..
                },
                Tok {
                    kind: Kind::Str(s), ..
                },
            ] => Exe::Lit(s.clone()),
            _ => Exe::Expr(text_of(arg)),
        };
        let line = t[start].line;
        if let Exe::Lit(s) = &exe {
            if is_python_exe(s) {
                a.problems
                    .push((line, format!("python を起動している: {s:?}")));
            }
            if is_wrapper(s) {
                let (from, to) = enclosing_fn(&t, i);
                for k in from..to {
                    if string(&t, k).is_some_and(runs_python) {
                        a.problems.push((
                            t[k].line,
                            format!("{s:?} を起動する関数に python と読める文字列がある"),
                        ));
                    }
                }
            }
        }
        // 起動する関数の中に python の実行ファイルの名前の文字列がある
        // （`let exe = "python"; Command::new(exe)`）
        let (from, to) = enclosing_fn(&t, i);
        for k in from..to {
            if string(&t, k).is_some_and(is_python_exe) && !is_index(&t, k) {
                a.problems.push((
                    t[k].line,
                    "起動する関数に python の実行ファイルの名前の文字列がある".to_string(),
                ));
            }
        }
        // 変数・定数に入れた名前（関数の外の `const PY = "python3"` を含む）
        if let Exe::Expr(x) = &exe {
            let var = x.trim_start_matches('&');
            if !var.is_empty() && var.chars().all(|c| c.is_alphanumeric() || c == '_') {
                for k in 0..t.len() {
                    // `NAME = ..;` と `const NAME: &str = ..;`（`==` と `NAME::` は除く）
                    let defines = (punct(&t, k + 1, '=') && !punct(&t, k + 2, '='))
                        || (punct(&t, k + 1, ':') && !punct(&t, k + 2, ':'));
                    if ident(&t, k) == Some(var) && defines {
                        let mut m = k + 2;
                        while m < t.len() && !punct(&t, m, ';') {
                            if string(&t, m).is_some_and(is_python_exe) {
                                a.problems.push((
                                    t[m].line,
                                    format!("`{var}` に python の実行ファイルの名前が入っている"),
                                ));
                            }
                            m += 1;
                        }
                    }
                }
            }
        }
        a.sites.push(Site { line, exe });
    }

    // shell に文字列を実行させる印（`"-c", "cmd"`、`.arg("-c").arg("cmd")`）の次の引数に python がある
    // （どこで shell を起動しても）
    for i in 0..t.len() {
        if !string(&t, i).is_some_and(is_shell_command_flag) {
            continue;
        }
        let from = if punct(&t, i + 1, ',') {
            i + 2
        } else if punct(&t, i + 1, ')')
            && punct(&t, i + 2, '.')
            && matches!(ident(&t, i + 3), Some("arg" | "args"))
            && punct(&t, i + 4, '(')
        {
            i + 5
        } else {
            continue;
        };
        let to = arg_end(&t, from);
        if (from..to).any(|k| string(&t, k).is_some_and(runs_python)) {
            a.problems.push((
                t[i].line,
                "shell の `-c` に渡す文字列に python がある".to_string(),
            ));
        }
    }
    a.problems.sort();
    a.problems.dedup();
    a
}

/// `-c`・`-lc`・`-ec` など、shell に文字列を実行させる印。
fn is_shell_command_flag(s: &str) -> bool {
    s.len() <= 4
        && s.starts_with('-')
        && s.contains('c')
        && s[1..].chars().all(|c| c.is_ascii_alphabetic())
}

// ---- 以降の文字列は検出器の入力の例と説明文なので、文字列の検査から外す ----

/// 文字列の許可リストの 1 項目。
#[derive(Debug)]
enum Allow {
    /// 文字列の中身が完全に一致する。
    Exact(&'static str),
    /// 中身がこれで始まる（長い説明文だけ。ほかの項目には使わない）。
    Starts(&'static str),
    /// 中身が完全に一致し、そのファイルの出現が**すべて**添字（`case["python"]`）のときだけ許す。
    /// 基準データの JSON のキー用。同じファイルに添字でない `"python"` が 1 つでも入れば失敗にする。
    IndexKey(&'static str),
}

/// python・pypy を実行ファイルとして読める文字列リテラルのうち、起動するものではないと確かめたもの
/// （`crates` からの相対パス、文字列）。起動の箇所かどうかに関係なく、ここに無い文字列があれば失敗にする。
/// 項目が使われなくなったら失敗にする。
const LITERAL_ALLOWED: &[(&str, Allow)] = &[
    // 基準データ（Python 版が出した値を固めた JSON）のキー `"python"`。添字で読むだけで、起動しない。
    ("geoframe/src/drive/tests.rs", Allow::IndexKey("python")),
    ("geoframe/src/geo/tests.rs", Allow::IndexKey("python")),
    ("geoframe/src/imaging/tests.rs", Allow::IndexKey("python")),
    ("geoframe/src/quality/tests.rs", Allow::IndexKey("python")),
    ("geoframe/tests/exif_python.rs", Allow::IndexKey("python")),
    (
        "geoframe/tests/jpeg_eoi_python.rs",
        Allow::IndexKey("python"),
    ),
    (
        "geoframe/tests/local_index_python.rs",
        Allow::Exact("python"),
    ),
    (
        "geoframe/tests/orientation_python.rs",
        Allow::Exact("python"),
    ),
    // テストデータの SQL（`place_name` に入れる合成の地名）。
    (
        "geoframe/src/ai/location_suggest/runtime/tests.rs",
        Allow::Exact(
            "SELECT COUNT(*) FROM photo_loc_suggestions WHERE place_name='synthetic Python rewrite'",
        ),
    ),
    (
        "geoframe/src/ai/location_suggest/runtime/tests.rs",
        Allow::Exact("UPDATE photo_loc_suggestions SET place_name='synthetic Python rewrite'"),
    ),
    // 起動の安全確認の説明文（Python 版のサービスが稼働中のとき書き込みを断る理由）。
    (
        "geoframe/src/guard.rs",
        Allow::Exact(
            "Python 版（{PYTHON_UNIT}）が非稼働と確認できない（状態: {state}）ので、書き込みを始めません。二重に書くと Python のダウンロードと .cache.json を壊します",
        ),
    ),
    (
        "geoframe/src/guard.rs",
        Allow::Exact(
            "Python 版（{PYTHON_UNIT}）の状態を取得できない（{why}）ので、書き込みを始めません。非稼働と確認できないものは通しません",
        ),
    ),
    // `geoframe` の使い方の表示（Python 版の稼働確認の説明を含む。長いので始まりだけで照合する）。
    (
        "geoframe/src/cli.rs",
        Allow::Starts("使い方:\n  geoframe --version"),
    ),
    // Python 版との比較の試験の、失敗時の説明文・表示名（Python は比べる相手の名前で、起動しない）。
    (
        "geoframe/src/ai/tests.rs",
        Allow::Exact("Python と同じスキーマの再現には ai_jobs を入れない"),
    ),
    (
        "geoframe/src/drive/tests.rs",
        Allow::Exact("{column}: Python と同じ値なら決定による差ではない"),
    ),
    (
        "geoframe/src/drive/tests.rs",
        Allow::Exact("{name}: Python は idF から動けない"),
    ),
    (
        "geoframe/src/quality/tests.rs",
        Allow::Exact("{name}: Python が作った photo_quality"),
    ),
    (
        "geoframe/tests/blog_api.rs",
        Allow::Exact("{target}: 応答が Python と違う"),
    ),
    (
        "geoframe/tests/blog_write_api.rs",
        Allow::Exact("{name} は Python と同じになった。decided から外す"),
    ),
    (
        "geoframe/tests/csrf_python.rs",
        Allow::Exact("Python は {python}: {:?}"),
    ),
    (
        "geoframe/tests/csrf_python.rs",
        Allow::Exact("Python は {python}: {headers:?}"),
    ),
    (
        "geoframe/tests/csrf_python.rs",
        Allow::Exact(
            "host: {} 件（Python が通す {python_allows}、HTTP 層が先に 400 にして Python が通すもの {http_layer_only}）",
        ),
    ),
    (
        "geoframe/tests/csrf_python.rs",
        Allow::Exact("{name}: Python は書かない（{py_status}）のに Rust が書いた（{status}）"),
    ),
    (
        "geoframe/tests/csrf_python.rs",
        Allow::Exact(
            "{name}: {} 件（Python が通す {}、HTTP 層が先に 400 にして Python が通すもの {}、両方が断る 400 {}、既知の差 {}）。HTTP 層が断る値のバイト: {:02x?}",
        ),
    ),
    (
        "geoframe/tests/csrf_python.rs",
        Allow::Exact("{name}: 書き込み（Python {py_status}、Rust {status}）"),
    ),
    (
        "geoframe/tests/exif_python.rs",
        Allow::Exact("未知の Python 例外のケース: {other}"),
    ),
    (
        "geoframe/tests/jpeg_eoi_python.rs",
        Allow::Exact("{name}: 表の差が Python と同じになった"),
    ),
    (
        "geoframe/tests/jpeg_eoi_python.rs",
        Allow::Exact("{name}（Python: {}）"),
    ),
    (
        "geoframe/tests/local_index_python.rs",
        Allow::Exact("{name} {column}: Python と同じ値なら決定による差ではない"),
    ),
    (
        "geoframe/tests/orientation_python.rs",
        Allow::Exact("{name}: Python が回していない"),
    ),
    (
        "geoframe/tests/rotate_api.rs",
        Allow::Exact("{name} は Python と同じになった。known_differences から外す"),
    ),
    (
        "geoframe/tests/update_api.rs",
        Allow::Exact("{name} は Python と同じになった。known_differences から外す"),
    ),
    (
        "geoframe/tests/update_api.rs",
        Allow::Exact("{name}: Python の結果"),
    ),
    // この試験ファイル自身のうち、検出器の本体の文字列。`is_python_exe` の接頭辞と、失敗の説明文。
    (
        "geoframe/tests/no_python.rs",
        Allow::Exact("`{var}` に python の実行ファイルの名前が入っている"),
    ),
    ("geoframe/tests/no_python.rs", Allow::Exact("pypy")),
    ("geoframe/tests/no_python.rs", Allow::Exact("python")),
    (
        "geoframe/tests/no_python.rs",
        Allow::Exact("python を起動している: {s:?}"),
    ),
    (
        "geoframe/tests/no_python.rs",
        Allow::Exact("shell の `-c` に渡す文字列に python がある"),
    ),
    (
        "geoframe/tests/no_python.rs",
        Allow::Exact("{s:?} を起動する関数に python と読める文字列がある"),
    ),
    (
        "geoframe/tests/no_python.rs",
        Allow::Exact("起動する関数に python の実行ファイルの名前の文字列がある"),
    ),
];
// ---- 実際の `crates` の検査 ----

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `crates` 以下の全 `.rs`（`crates` からの相対パス、中身）。
fn crate_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    assert!(files.len() > 50, "{} files", files.len());
    files
        .iter()
        .map(|f| {
            let rel = f
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, std::fs::read_to_string(f).unwrap())
        })
        .collect()
}

#[test]
fn process_launches_under_crates_match_the_allow_list_and_none_is_python() {
    let texts = crate_sources();

    // Command の別名は、別のファイルで再公開されて使われることがあるので、全ファイルから集める
    let mut aliases = BTreeSet::from(["Command".to_string()]);
    for _ in 0..2 {
        for (_, text) in &texts {
            let found = aliases_in(&lex(text), &aliases);
            aliases.extend(found);
        }
    }

    let mut problems = Vec::new();
    let mut found: BTreeMap<(String, Exe<String>), Vec<usize>> = BTreeMap::new();
    for (rel, text) in &texts {
        let analysis = analyze(text, &aliases);
        for (line, why) in analysis.problems {
            problems.push(format!("{rel}:{line} {why}"));
        }
        for site in analysis.sites {
            found
                .entry((rel.clone(), site.exe))
                .or_default()
                .push(site.line);
        }
    }
    assert!(
        problems.is_empty(),
        "Python を起動するコードがある:\n{}",
        problems.join("\n")
    );

    let mut allowed: BTreeMap<(String, Exe<String>), usize> = BTreeMap::new();
    for (file, exe, n) in ALLOWED {
        let key = ((*file).to_string(), exe.owned());
        assert!(
            allowed.insert(key, *n).is_none(),
            "許可リストの重複: {file} {exe}"
        );
    }
    let mut diffs = Vec::new();
    for ((file, exe), lines) in &found {
        match allowed.get(&(file.clone(), exe.clone())) {
            None => diffs.push(format!(
                "{file}:{lines:?} 許可リストに無い起動（{exe}）。何を起動するか確かめてから ALLOWED に足すこと"
            )),
            Some(n) if *n != lines.len() => diffs.push(format!(
                "{file}:{lines:?} {exe} の件数が許可リストと違う（許可 {n}、実際 {}）。確かめて ALLOWED を直すこと",
                lines.len()
            )),
            Some(_) => {}
        }
    }
    for ((file, exe), n) in &allowed {
        if !found.contains_key(&(file.clone(), exe.clone())) {
            diffs.push(format!(
                "{file} {exe} は起動されていない（許可 {n}）。ALLOWED から外すこと"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "起動箇所が許可リストと合わない:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn the_allow_list_never_names_python() {
    for (file, exe, n) in ALLOWED {
        assert!(*n > 0, "{file} {exe}");
        if let Exe::Lit(s) = exe {
            assert!(
                !runs_python(s) && !s.to_ascii_lowercase().contains("python"),
                "{file} {exe}"
            );
        }
    }
}

/// この試験ファイル自身で、文字列の検査から外す範囲の始まりの行（この行より後の文字列は、検出器の入力の例と
/// 失敗の説明文）。この行より前の文字列は他のファイルと同じ扱いで、`LITERAL_ALLOWED` に書いたものだけ許す。
const SELF_FILE: &str = "geoframe/tests/no_python.rs";
const SELF_EXEMPT_FROM: &str =
    "// ---- 以降の文字列は検出器の入力の例と説明文なので、文字列の検査から外す ----";

/// 許可リストの項目 `allow` が、文字列 `lit`（そのファイルの出現が `all_index` ならすべて添字）を許すか。
fn allows(allow: &Allow, lit: &str, all_index: bool) -> bool {
    match allow {
        Allow::Exact(s) => *s == lit,
        Allow::Starts(s) => lit.starts_with(s),
        Allow::IndexKey(s) => *s == lit && all_index,
    }
}

#[test]
fn no_string_literal_under_crates_reads_as_python_outside_the_allow_list() {
    // (ファイル, 文字列の中身) → (行, 添字か)
    let mut hits: BTreeMap<(String, String), Vec<(usize, bool)>> = BTreeMap::new();
    for (rel, text) in crate_sources() {
        let exempt_from = if rel == SELF_FILE {
            text.lines()
                .position(|l| l == SELF_EXEMPT_FROM)
                .map_or_else(|| panic!("{SELF_FILE} に外す範囲の印の行が無い"), |n| n + 1)
        } else {
            usize::MAX
        };
        for (line, s, index) in analyze(&text, &BTreeSet::new()).literals {
            if line < exempt_from {
                hits.entry((rel.clone(), s))
                    .or_default()
                    .push((line, index));
            }
        }
    }

    let mut used = vec![false; LITERAL_ALLOWED.len()];
    let mut diffs = Vec::new();
    for ((file, lit), at) in &hits {
        let all_index = at.iter().all(|(_, index)| *index);
        let entry = LITERAL_ALLOWED
            .iter()
            .position(|(f, allow)| f == file && allows(allow, lit, all_index));
        match entry {
            Some(n) => used[n] = true,
            None => diffs.push(format!(
                "{file}:{:?} python・pypy を実行ファイルとして読める文字列 {lit:?}（添字 `x[\"python\"]` でない出現: {:?}）。起動する文字列でないと確かめてから LITERAL_ALLOWED に足すこと",
                at.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
                at.iter().filter(|(_, i)| !*i).map(|(l, _)| *l).collect::<Vec<_>>(),
            )),
        }
    }
    for (n, (file, allow)) in LITERAL_ALLOWED.iter().enumerate() {
        if !used[n] {
            diffs.push(format!(
                "{file} {allow:?} は文字列の許可リストにあるが使われていない。LITERAL_ALLOWED から外すこと"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "python と読める文字列が許可リストと合わない:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn the_literal_allow_list_keeps_prefix_matching_to_one_entry_and_index_keys_to_python() {
    let starts = LITERAL_ALLOWED
        .iter()
        .filter(|(_, a)| matches!(a, Allow::Starts(_)))
        .count();
    assert!(
        starts <= 1,
        "前置きの照合は長い使い方の表示 1 つだけ（{starts}）"
    );
    for (file, allow) in LITERAL_ALLOWED {
        if let Allow::IndexKey(s) = allow {
            assert_eq!(*s, "python", "{file}");
        }
    }
}

// ---- 検出器の回帰試験 ----

/// 失敗にする箇所の行（同じ行に理由が複数あっても 1 つ）。
fn problems(src: &str) -> Vec<usize> {
    let lines: BTreeSet<usize> = analyze(src, &BTreeSet::new())
        .problems
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    lines.into_iter().collect()
}

fn sites(src: &str) -> Vec<(usize, String)> {
    analyze(src, &BTreeSet::new())
        .sites
        .into_iter()
        .map(|s| (s.line, s.exe.to_string()))
        .collect()
}

/// python と読める文字列リテラルの行。
fn literals(src: &str) -> Vec<usize> {
    let lines: BTreeSet<usize> = analyze(src, &BTreeSet::new())
        .literals
        .into_iter()
        .map(|(line, _, _)| line)
        .collect();
    lines.into_iter().collect()
}

#[test]
fn the_detector_reads_past_a_turbofish_after_new() {
    // `new` の後の `::<..>` を読み飛ばして、通常の呼び出しと同じ検査をする
    assert_eq!(
        problems("std::process::Command::new::<&str>(\"python\").status();"),
        [1]
    );
    assert_eq!(problems("Command::new::<&str>(\"python3\");"), [1]);
    assert_eq!(problems("Command::new::<Vec<u8>>(\"pypy3\");"), [1]);
    assert_eq!(problems("<Command>::new::<&str>(\"python\");"), [1]);
    assert_eq!(
        problems("let exe = \"python\";\nCommand::new::<&str>(exe);"),
        [1]
    );
    assert_eq!(
        problems("Command::new::<&str>(\"sh\").args([\"-c\", \"python -V\"]);"),
        [1]
    );
    assert_eq!(
        sites("Command::new::<&str>(\"mkfifo\");\nstd::process::Command::new::<&Path>(BIN);"),
        [
            (1, "文字列 \"mkfifo\"".to_string()),
            (2, "式 BIN".to_string())
        ]
    );
    // 関数として渡すだけ（呼び出しの `(` が続かない）のも、読めない起動として失敗にする
    assert_eq!(problems("let f = Command::new;\nf(\"python\");"), [1]);
    // Command でない型の `new` は対象外
    assert_eq!(
        problems("let a = Vec::<u8>::new(); let b = String::new(); let c = Vec::new;"),
        Vec::<usize>::new()
    );
}

#[test]
fn the_detector_follows_the_libc_use_tree_to_system_and_popen() {
    for src in [
        "use libc::{system};\nfn f() { unsafe { system(c\"python -V\".as_ptr()); } }",
        "use libc::{c_int, popen};",
        "use libc::{system as run};",
        "use libc::{self, system};",
        "use ::libc::{c_int, system};",
        "use foo::{bar, libc::{system}};",
        "use libc::system;",
        "use libc::*;",
        "use libc as l;",
        "use libc::{self as l};",
        "extern \"C\" { fn system(cmd: *const i8) -> i32; }",
        "extern \"C\" { pub fn popen(c: *const i8, m: *const i8) -> *mut u8; }",
    ] {
        assert!(!problems(src).is_empty(), "{src}");
    }
    // 起動と関係ない libc の使い方は誤検知しない
    for src in [
        "use libc::{O_NONBLOCK, c_int};",
        "use libc::O_NOFOLLOW;",
        "use libc::{self, O_NOFOLLOW};",
        "use libc;\nlet f = libc::O_NONBLOCK;",
        "o.custom_flags(libc::O_NONBLOCK);",
        "let system = 1; let popen = system + 1;",
        "struct S { system: u8 }",
    ] {
        assert_eq!(problems(src), Vec::<usize>::new(), "{src}");
    }
}

#[test]
fn the_detector_follows_the_use_tree_to_tokio_process_only() {
    for src in [
        "use tokio::process;",
        "use tokio::{fs, process};",
        "use tokio::{fs, process::Command};",
        "use tokio::process::{Command as C};",
        "use tokio::{io::{self}, process::{Command, Child}};",
        "use crate::tokio::process::Command;",
        "use tokio::*;",
        "use tokio as t;",
        "use tokio::{self as t};",
    ] {
        assert!(!problems(src).is_empty(), "{src}");
    }
    // 同じ文の別の枝の `process` は、`tokio` 配下ではない
    for src in [
        "use crate::{process, tokio::fs};",
        "use crate::{tokio::fs, process::Command};",
        "use tokio::{fs, io};\nuse std::process::Stdio;",
        "use tokio::fs::File;\nuse crate::process;",
        "use std::{process::Stdio, thread};",
        "use tokio::{self};",
    ] {
        assert_eq!(problems(src), Vec::<usize>::new(), "{src}");
    }
}

#[test]
fn the_bottom_check_reads_every_kind_of_string_literal_as_a_possible_executable() {
    // 起動の構文に頼らず、文字列そのものを見る（どの種類の文字列でも）
    for (src, line) in [
        ("let a = \"python\";", 1),
        ("let a = \"/usr/bin/python3.12\";", 1),
        ("let a = \"cd x && python3 y.py\";", 1),
        ("let a = r#\"python\"#;", 1),
        ("let a = b\"python\";", 1),
        ("let a = br#\"pypy3 -V\"#;", 1),
        ("let a = c\"python -V\";", 1),
        ("let a = cr\"pypy\";", 1),
        ("x;\nlet a = c\"python -V\".as_ptr();", 2),
        // エスケープ・行末の `\` で組み立てた名前も戻して読む（行末の `\` は次の行の空白まで詰める）
        ("let a = \"\\x70ython\";", 1),
        ("let a = \"py\\u{74}hon -V\";", 1),
        ("let a = \"\\npython3\";", 1),
        ("let a = \"py\\\n        thon\";", 1),
    ] {
        assert_eq!(literals(src), [line], "{src}");
    }
    // 起動の構文のどれにも当たらない形でも、底で捕まる
    for src in [
        "use libc::{system};\nfn f() { unsafe { system(c\"python -V\".as_ptr()); } }",
        "let f = Command::new;\nf(\"python\");",
        "std::process::Command::new::<&str>(\"python\").status();",
        "let s = b\"python\"; run(s);",
    ] {
        assert!(!literals(src).is_empty(), "{src}");
    }
    // python と関係ない文字列は誤検知しない
    for src in [
        "let a = \"mkfifo\";",
        "let a = b\"pythonic\";",
        "let a = c\"exif-python.json\";",
        "let a = \"local-index-python.json\";",
        "let a = \"python_exe\";",
        "let a = \"/tmp/python/bin\";",
        "let a = r#\"{\"name\": \"pypy-fixtures\"}\"#;",
        "// \"python\"\n/* b\"python\" */",
    ] {
        assert_eq!(literals(src), Vec::<usize>::new(), "{src}");
    }
}

#[test]
fn an_index_key_allowance_covers_only_files_where_every_python_literal_is_an_index() {
    let key = Allow::IndexKey("python");
    let all_index = |src: &str| {
        let found = analyze(src, &BTreeSet::new()).literals;
        assert!(!found.is_empty(), "{src}");
        found.iter().all(|(_, _, index)| *index)
    };
    // 基準データの JSON のキーとしての添字だけなら許す
    let json = "let w = &case[\"python\"];\nlet r = row[\"python\"].as_str();\nlet d = doc[\"steps\"][0][\"python\"][\"rows\"];";
    assert!(all_index(json));
    assert!(allows(&key, "python", all_index(json)));
    // 同じファイルに添字でない "python" が 1 つでも入れば許さない（起動に使う形を底で止める）
    for src in [
        "let w = &case[\"python\"];\nlet exe = \"python\";",
        "let w = &case[\"python\"];\nrun(\"python\");",
        "let w = &case[\"python\"];\nlet exe = [\"python\"];",
        "let exe = b\"python\";",
    ] {
        assert!(!all_index(src), "{src}");
        assert!(!allows(&key, "python", all_index(src)), "{src}");
    }
    // 完全に一致しない文字列には使えない
    assert!(!allows(&key, "python3", true));
    assert!(!allows(&key, "python -V", true));
    // Exact・Starts の照合
    assert!(allows(&Allow::Exact("a python b"), "a python b", false));
    assert!(!allows(&Allow::Exact("a python"), "a python b", false));
    assert!(allows(&Allow::Starts("使い方:"), "使い方: python", false));
}
#[test]
fn the_detector_finds_python_launched_in_the_ways_a_string_search_misses() {
    // 変数に入れた名前
    assert_eq!(
        problems("let exe = \"python\";\nCommand::new(exe).status();"),
        [1]
    );
    // 定数（関数の外）に入れた名前
    assert_eq!(
        problems("const PY: &str = \"python3\";\nfn f() {\n    Command::new(PY).status();\n}"),
        [1]
    );
    // Command の別名
    assert_eq!(
        problems("use std::process::Command as Process;\nProcess::new(\"python\").status();"),
        [2]
    );
    assert_eq!(
        problems("use std::process::{self, Command as P};\nP::new(\"python3\");"),
        [2]
    );
    assert_eq!(
        problems("type P = std::process::Command;\nP::new(\"python3\");"),
        [2]
    );
    // shell を経由する
    assert_eq!(
        problems("Command::new(\"sh\").args([\"-c\", \"python -V\"]).status();"),
        [1]
    );
    assert_eq!(
        problems(
            "let mut c = Command::new(\"/bin/bash\");\nlet script = \"cd x && python3 y.py\";\nc.arg(\"-c\").arg(script);"
        ),
        [2]
    );
    assert_eq!(
        problems("fn f() { let a = [\"-lc\", &format!(\"exec python {}\", 1)]; }"),
        [1]
    );
    // 他のコマンドに包ませる
    assert_eq!(
        problems("Command::new(\"env\").arg(\"python3\").status();"),
        [1]
    );
    // 直接
    assert_eq!(problems("Command::new(\"python3\");"), [1]);
    assert_eq!(problems("std::process::Command::new(\"python\");"), [1]);
    assert_eq!(
        problems("x\nCommand::new(\n    \"/usr/bin/python3.12\",\n)\n.status();"),
        [2, 3]
    );
    assert_eq!(problems("Command::new(&\"pypy3\");"), [1]);
    assert_eq!(problems("Command::new(r#\"python\"#);"), [1]);
    // Command::new 以外の起動の道具
    assert_eq!(problems("use std::os::unix::process::CommandExt;"), [1]);
    assert_eq!(
        problems("let c = tokio::process::Command::new(\"x\");"),
        [1]
    );
    assert_eq!(problems("unsafe { libc::execv(p, a) };"), [1]);
    assert_eq!(problems("unsafe { libc::system(c) };"), [1]);
    assert_eq!(problems("use tokio::{fs, process::Command};"), [1]);
    assert_eq!(problems("use std::os::unix::process::*;"), [1]);
    assert_eq!(problems("c.pre_exec(|| Ok(()));"), [1]);
    assert_eq!(
        problems("use duct::cmd;\nlet _ = duct::cmd(\"x\", [\"y\"]);"),
        [1, 2]
    );
    assert_eq!(problems("let s = subprocess::Exec::cmd(\"x\");"), [1]);
    assert_eq!(problems("posix_spawn(p, f, a, e);"), [1]);
}

#[test]
fn the_detector_finds_launches_through_a_qualified_type_path() {
    // `<型>::new` の形でも起動として数え、python なら失敗にする
    assert_eq!(
        problems("<std::process::Command>::new(\"python\").status();"),
        [1]
    );
    assert_eq!(
        sites("<std::process::Command>::new(\"python\").status();"),
        [(1, "文字列 \"python\"".to_string())]
    );
    assert_eq!(problems("<Command>::new(\"python3\");"), [1]);
    assert_eq!(problems("<Command as Foo>::new(\"pypy3\");"), [1]);
    assert_eq!(
        problems("use std::process::Command as P;\n<P>::new(\"python\");"),
        [2]
    );
    assert_eq!(
        sites("a;\n<std::process::Command as Foo>::new(BIN);\n<Command>::new(\"kill\");"),
        [
            (2, "式 BIN".to_string()),
            (3, "文字列 \"kill\"".to_string())
        ]
    );
    // 型を読めない形は未対応の起動構文として失敗にする
    assert_eq!(problems("<_ as Foo>::new(\"x\");"), [1]);
    assert_eq!(problems("x >::new(\"x\");"), [1]);
    // Command でない型の `new`・ターボフィッシュは起動として数えない
    assert_eq!(
        sites("let a = <String>::new(); let b = <Vec<Command>>::new();"),
        []
    );
    assert_eq!(
        sites("let a = Vec::<Command>::new(); let b = HashMap::<K, Command>::new();"),
        []
    );
    assert_eq!(problems("let a = Vec::<u8>::new();"), Vec::<usize>::new());
}

#[test]
fn the_detector_finds_python_run_through_a_shell_by_any_executable_name() {
    for cmd in [
        "pypy3 -V",
        "pypy -c 1",
        "pypy3.10 x.py",
        "/opt/pypy/bin/pypy3 x.py",
        "cd x && pypy3 y.py",
        "FOO=1 /usr/bin/python3.12 y.py",
        "PY=python3; $PY y",
        "echo $(command -v python3)",
        "exec ./venv/bin/python -m x",
    ] {
        let src = format!("Command::new(\"/bin/sh\").args([\"-c\", {cmd:?}]);");
        assert_eq!(problems(&src), [1], "{cmd}");
    }
    assert_eq!(
        problems("Command::new(\"bash\").arg(\"-c\").arg(\"pypy3 -V\").status();"),
        [1]
    );
    assert_eq!(
        problems("Command::new(\"sh\").args([\"-lc\", \"cd x; pypy3 y\"]);"),
        [1]
    );
    // 関数の別の場所に書いた命令文でも、wrapper を起動していれば同じ判定
    assert_eq!(
        problems(
            "fn f() {\n    let s = \"pypy3 -V\";\n    Command::new(\"/bin/sh\").args([\"-c\", s]);\n}"
        ),
        [2]
    );
    // python と関係ない命令文は誤検知しない
    for cmd in [
        "echo hello",
        "ls /tmp/python/bin",
        "echo pypy-fixtures pythonic",
        "cat exif-python.json",
    ] {
        let src = format!("Command::new(\"/bin/sh\").args([\"-c\", {cmd:?}]);");
        assert_eq!(problems(&src), Vec::<usize>::new(), "{cmd}");
    }
}

#[test]
fn the_detector_counts_every_launch_with_what_it_launches() {
    assert_eq!(
        sites("Command::new(\"mkfifo\").arg(&p).status();"),
        [(1, "文字列 \"mkfifo\"".to_string())]
    );
    assert_eq!(
        sites(
            "a;\nstd::process::Command::new(std::env::current_exe().unwrap())\n    .args([\"x\", \"y\"]);"
        ),
        [(2, "式 std::env::current_exe().unwrap()".to_string())]
    );
    assert_eq!(
        sites("let a = Command::new(BIN);\nlet b = Command::new(&bin);"),
        [(1, "式 BIN".to_string()), (2, "式 &bin".to_string())]
    );
    assert_eq!(
        sites("use std::process::Command as Process;\nProcess::new(\"kill\");"),
        [(2, "文字列 \"kill\"".to_string())]
    );
    // 文字リテラルの '"' やライフタイムで字句がずれない
    assert_eq!(
        sites("fn f<'a>(x: &'a str) {\n    let q = '\"';\n    Command::new(\"mkfifo\");\n}"),
        [(3, "文字列 \"mkfifo\"".to_string())]
    );
}

#[test]
fn the_detector_ignores_json_keys_fixture_names_comments_and_text() {
    // JSON のキー・基準のファイル名
    assert_eq!(problems("let w = &case[\"python\"];"), Vec::<usize>::new());
    assert_eq!(
        problems("include_str!(\"fixtures/exif-python.json\")"),
        Vec::<usize>::new()
    );
    assert_eq!(
        problems("let p = dir.join(\"local-index-python.json\");"),
        Vec::<usize>::new()
    );
    // 起動する関数の中でも、添字は対象外
    assert_eq!(
        problems("fn f() {\n    let w = &case[\"python\"];\n    Command::new(\"mkfifo\");\n}"),
        Vec::<usize>::new()
    );
    // コメント・文字列の中の説明
    assert_eq!(
        problems("// Command::new(\"python\") は使わない\n/* Command::new(\"python3\") */"),
        Vec::<usize>::new()
    );
    assert_eq!(
        sites("// Command::new(\"mkfifo\")\n/* nested /* Command::new(\"x\") */ */"),
        []
    );
    assert_eq!(
        problems("let doc = \"Command::new(\\\"python\\\") を呼ばない\";"),
        Vec::<usize>::new()
    );
    assert_eq!(sites("let doc = r#\"Command::new(\"python\")\"#;"), []);
    // python と関係ない shell の使い方は誤検知しない
    assert_eq!(
        problems("Command::new(\"sh\").args([\"-c\", \"sleep 30\"]);"),
        Vec::<usize>::new()
    );
    assert_eq!(
        problems("let n = \"python-fixtures\"; Command::new(\"mkfifo\");"),
        Vec::<usize>::new()
    );
    // 起動の道具でない名前
    assert_eq!(
        problems("std::process::exit(1); let id = std::process::id(); std::process::abort();"),
        Vec::<usize>::new()
    );
    // 起動の道具と同じ名前の変数・フィールド・引数・メソッド
    for src in [
        "let subprocess = 42;",
        "let duct = 1; let xshell = duct + 1;",
        "struct S { subprocess: u8, cmd_lib: u8 }",
        "fn f(async_process: u8) -> u8 { async_process }",
        "let n = s.subprocess + s.duct;",
        "let c = S { subprocess: 1, execv: 2 };",
        "env.exec(sql.to_string());",
        "let posix_spawn = true;",
    ] {
        assert_eq!(problems(src), Vec::<usize>::new(), "{src}");
    }
    // Command という名前の別の型（clap の列挙など）
    assert_eq!(
        sites("match c { Command::Serve => 1, Command::Ingest => 2 }"),
        []
    );
}

#[test]
fn python_exe_names_are_recognised() {
    for s in [
        "python",
        "python3",
        "python3.12",
        "/usr/bin/python3",
        "./venv/bin/python",
        "Python3",
        "pypy3",
        "pypy",
        "pypy3.10",
        "/opt/pypy/bin/pypy3",
    ] {
        assert!(is_python_exe(s), "{s}");
        assert!(runs_python(s), "{s}");
    }
    for s in [
        "mkfifo",
        "node",
        "pythonic-tool",
        "python-fixtures",
        "exif-python.json",
        "py",
        "sh",
    ] {
        assert!(!is_python_exe(s), "{s}");
        assert!(!runs_python(s), "{s}");
    }
}
