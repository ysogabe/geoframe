//! `crates` の Rust のコードが Python を起動しないことの検査（P5b / T5.6a。受け入れ条件 AC7）。
//!
//! Python 版をリポジトリから外したあとに、試験や build.rs がまた Python を呼ぶようになると、CI の Rust のジョブに
//! Python の導入が戻る。文字列で `python` を探すだけでは、変数に入れた名前（`let exe = ..; Command::new(exe)`）、
//! `Command` の別名、`sh -c "python .."` を見逃す。そこで**プロセスを起動する箇所そのもの**を管理する。
//!
//! - `crates` 以下の `.rs` を字句に分け（コメントは捨て、文字列は 1 つの塊にする）、`Command::new(` の形の呼び出し
//!   （`use .. process::Command as X` や `type X = ..Command;` の別名を含む）を全部数える。最初の引数が文字列なら
//!   その文字列、そうでなければ式のまま（`BIN`、`std::env::current_exe().unwrap()` など）を「起動するもの」とする。
//! - 数えた結果を `ALLOWED`（ファイル・起動するもの・件数）と**完全に**照合する。リストに無い起動が増えても、
//!   件数が増えても減っても失敗にする。増えたときは、何を起動するかを確かめてからリストに足す。
//!   式のままの起動はリストに式そのものを書いた場所だけ許す。
//! - 次は許可リストと無関係に失敗にする: 起動するものが python・python3・python3.x・パス付きの python、
//!   起動する関数の中に python と読める文字列がある（`let exe = "python"`、`env` に渡す引数など）、
//!   shell に渡す `-c` の文字列に python がある、`tokio::process`・`CommandExt`・`exec*`・`posix_spawn` など
//!   `Command::new` 以外の起動の道具を使う。
//!
//! 限界（捕まえられない書き方）: 文字列を実行時に組み立てる（`format!("py{}", ..)`、連結）、別の関数や別のファイル
//! から渡る変数や引数、マクロの中で作る `Command`、`Command` を返す関数の呼び出し側（`make().spawn()`）、
//! 別の名前での再公開を 2 段以上重ねた別名。式のままの起動は許可リストの式が何を指すかを人が確かめる前提。
//! `["python"]` のような JSON のキー、`*-python.json` のような基準のファイル名、コメントや文字列の中の説明は、
//! 起動するコードではないので対象にしない。

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
    (c[start..i.min(c.len())].iter().collect(), i + 1)
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

fn mentions_python(s: &str) -> bool {
    s.to_ascii_lowercase().contains("python")
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
        if OTHER_LAUNCHERS.contains(&name) {
            a.problems.push((
                t[i].line,
                format!("`{name}` は使えない（Command::new 以外の起動の道具）"),
            ));
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

        // Command::new(..)
        if names.contains(name)
            && is_path_sep(&t, i + 1)
            && ident(&t, i + 3) == Some("new")
            && punct(&t, i + 4, '(')
        {
            let first = i + 5;
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
            let line = t[i].line;
            if let Exe::Lit(s) = &exe {
                if is_python_exe(s) {
                    a.problems
                        .push((line, format!("python を起動している: {s:?}")));
                }
                if is_wrapper(s) {
                    let (from, to) = enclosing_fn(&t, i);
                    for k in from..to {
                        if string(&t, k).is_some_and(mentions_python) {
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
                                        format!(
                                            "`{var}` に python の実行ファイルの名前が入っている"
                                        ),
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
    }

    // shell に文字列を実行させる印の次の引数に python がある（どこで shell を起動しても）
    for i in 0..t.len() {
        if string(&t, i).is_some_and(is_shell_command_flag) && punct(&t, i + 1, ',') {
            let from = i + 2;
            let to = arg_end(&t, from);
            if (from..to).any(|k| string(&t, k).is_some_and(mentions_python)) {
                a.problems.push((
                    t[i].line,
                    "shell の `-c` に渡す文字列に python がある".to_string(),
                ));
            }
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

#[test]
fn process_launches_under_crates_match_the_allow_list_and_none_is_python() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    assert!(files.len() > 50, "{} files", files.len());
    let texts: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let rel = f
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, std::fs::read_to_string(f).unwrap())
        })
        .collect();

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
            assert!(!is_python_exe(s) && !mentions_python(s), "{file} {exe}");
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
    ] {
        assert!(is_python_exe(s), "{s}");
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
    }
}
