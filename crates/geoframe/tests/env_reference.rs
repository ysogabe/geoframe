//! `docs/reference/configuration.md`（手で保つ設定リファレンス）と、Rust のコードが読む `GEOFRAME_*` の名前の
//! 照合（P5b / T5.6b）。Python 版の `config.py` から文書を作っていた `gen_env_reference.py --check` の代わり。
//!
//! 文書は 2 つの節に分ける。
//! - 「Rust が読む環境変数」: コードが読む名前の全部。
//! - 「旧版のみ（削除済み。Rust は読まない）」: 旧 Python 版だけが読んだ名前。コードには出てこない。
//!
//! 照合するもの（「コード」は試験とコメントを除いた本体: `tests/` の下・`tests.rs`・`tests_*.rs`・`*_tests.rs` の
//! ファイルと、`#[cfg(test)]` の付いた項目、コメントを除く。試験やコメントに名前が残っていても、本体が
//! 読まなくなれば 2 で分かるように）:
//! 1. `crates` 以下の本体の `.rs` にある `"GEOFRAME_…"` の文字列（引用符で囲んだ名前そのもの）は、
//!    試験だけの名前（`TEST_ONLY`）を除いて、すべて「Rust が読む」の節に載っている。
//! 2. 「Rust が読む」の節の名前は、どれもコードに出てくる（綴りの誤り・読まなくなった名前が残らない）。
//! 3. 「旧版のみ」の節の名前は、コードに出てこない（読むようになったのに節を移し忘れない）。
//! 4. 2 つの節は重ならない。`TEST_ONLY` の名前は文書に載せず、使われていなければ失敗にする。
//!
//! 限界: 名前を実行時に組み立てる（`format!("GEOFRAME_{}", ..)`）読み方、`"GEOFRAME_X=1"` のように文字列の
//! 一部としてだけ出てくる名前、shell スクリプト（`deploy/`）が読む名前（`GEOFRAME_RS_*` など。配布の道具の
//! 引数で、サーバーの設定ではない）は見ない。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 本体のコードにあるが、試験のためだけの名前（文書に載せない）。`guard.rs` の、デバッグビルドだけの注入口
/// （実行ユーザーを試験から与える）。試験のファイルの中だけで使う名前（子プロセスの目印など）は、
/// 本体を走査しないのでここに書かなくてよい。
const TEST_ONLY: &[&str] = &["GEOFRAME_TEST_EUID"];

const SECTION_RUST: &str = "## Rust が読む環境変数";
const SECTION_OLD_ONLY: &str = "## 旧版のみ（削除済み。Rust は読まない）";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// 試験だけのファイル（`tests/` の下、`tests.rs`・`tests_*.rs`・`*_tests.rs`）。
fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    path.components().any(|c| c.as_os_str() == "tests")
        || name == "tests.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
}

/// 字句の分類（バイトごと）。`masked` はコメント・文字列・文字リテラルの中身を空白にした写し（区切り記号は残す）。
/// `comment` はコメントのバイト。境界（`{`・`}`・`;`・`[`・`]`）は `masked` の上で探すので、文字列・生文字列・
/// 文字リテラル・コメントの中の記号を数えない。
struct Lexed {
    masked: Vec<u8>,
    comment: Vec<bool>,
}

fn lex(text: &str) -> Lexed {
    let b = text.as_bytes();
    let n = b.len();
    let mut masked = b.to_vec();
    let mut comment = vec![false; n];
    let blank = |m: &mut Vec<u8>, from: usize, to: usize| {
        for x in &mut m[from..to] {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    };
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = b[i..].iter().position(|&x| x == b'\n').map_or(n, |k| i + k);
            blank(&mut masked, i, end);
            comment[i..end].iter_mut().for_each(|x| *x = true);
            i = end;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0usize;
            let mut k = i;
            while k < n {
                if b[k] == b'/' && b.get(k + 1) == Some(&b'*') {
                    depth += 1;
                    k += 2;
                } else if b[k] == b'*' && b.get(k + 1) == Some(&b'/') {
                    depth -= 1;
                    k += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    k += 1;
                }
            }
            blank(&mut masked, i, k);
            comment[i..k].iter_mut().for_each(|x| *x = true);
            i = k;
        } else if c == b'r'
            && (i == 0 || !ident(b[i - 1]) || matches!(b[i - 1], b'b' | b'c'))
            && matches!(b.get(i + 1), Some(b'#') | Some(b'"'))
        {
            // 生文字列 r"..."・r#"..."#（br・cr も、前の b・c はただの識別子の字として残る）
            let hashes = b[i + 1..].iter().take_while(|&&x| x == b'#').count();
            if b.get(i + 1 + hashes) != Some(&b'"') {
                i += 1;
                continue;
            }
            let open = i + 1 + hashes;
            let mut k = open + 1;
            let end = loop {
                if k >= n {
                    break n;
                }
                if b[k] == b'"'
                    && b[k + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&x| x == b'#')
                        .count()
                        == hashes
                    && k + hashes < n
                {
                    break k;
                }
                k += 1;
            };
            blank(&mut masked, open + 1, end);
            i = (end + 1 + hashes).min(n);
        } else if c == b'"' {
            let mut k = i + 1;
            while k < n && b[k] != b'"' {
                k += if b[k] == b'\\' { 2 } else { 1 };
            }
            let end = k.min(n);
            blank(&mut masked, i + 1, end);
            i = end + 1;
        } else if c == b'\'' {
            // 文字リテラル（'x'・'\n'・'\u{..}'・多バイトの 1 文字）。それ以外の ' はライフタイム
            let end = if b.get(i + 1) == Some(&b'\\') {
                b[i + 2..]
                    .iter()
                    .position(|&x| x == b'\'')
                    .map(|k| i + 2 + k)
            } else {
                let len = text[i + 1..].chars().next().map_or(0, char::len_utf8);
                (b.get(i + 1 + len) == Some(&b'\'')).then_some(i + 1 + len)
            };
            match end {
                Some(e) => {
                    blank(&mut masked, i + 1, e);
                    i = e + 1;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    Lexed { masked, comment }
}

/// `open`（`{` か `[`）に対応する閉じの次の位置。
fn matching(m: &[u8], open: usize) -> usize {
    let (o, c) = if m[open] == b'{' {
        (b'{', b'}')
    } else {
        (b'[', b']')
    };
    let mut depth = 0usize;
    for (k, &x) in m.iter().enumerate().skip(open) {
        if x == o {
            depth += 1;
        } else if x == c {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
    }
    m.len()
}

/// 本体だけを残す: コメントと、`#[cfg(test)]` の付いた項目（続く属性を読み飛ばし、次の `;` か対応する `}` まで）を
/// 空白にする。文字列の中身はそのまま残す（名前を読むため）。
fn production_source(text: &str) -> String {
    let Lexed { masked, comment } = lex(text);
    let mut out: Vec<u8> = text
        .bytes()
        .zip(&comment)
        .map(|(x, &c)| if c && x != b'\n' { b' ' } else { x })
        .collect();
    let pat = b"#[cfg(test)]";
    let mut i = 0;
    while let Some(k) = masked[i..].windows(pat.len()).position(|w| w == pat) {
        let start = i + k;
        let mut j = start + pat.len();
        loop {
            while j < masked.len() && masked[j].is_ascii_whitespace() {
                j += 1;
            }
            if masked.get(j) == Some(&b'#') && masked.get(j + 1) == Some(&b'[') {
                j = matching(&masked, j + 1);
            } else {
                break;
            }
        }
        let end = match masked[j..].iter().position(|&x| x == b';' || x == b'{') {
            Some(p) if masked[j + p] == b';' => j + p + 1,
            Some(p) => matching(&masked, j + p),
            None => masked.len(),
        };
        for x in &mut out[start..end] {
            if *x != b'\n' {
                *x = b' ';
            }
        }
        i = end;
    }
    // 空白にしたのは ASCII の範囲の外側も含むので、壊れた UTF-8 は置き換える（名前は ASCII なので影響しない）
    String::from_utf8_lossy(&out).into_owned()
}

/// `"GEOFRAME_…"`（引用符で囲んだ、大文字・数字・`_` だけの名前）を集める。末尾が `_` のもの
/// （`"GEOFRAME_"`、`"GEOFRAME_AI_"` のような接頭辞）は名前ではないので除く。
fn names_in(text: &str, out: &mut BTreeSet<String>) {
    let mut rest = text;
    while let Some(i) = rest.find("\"GEOFRAME_") {
        let body = &rest[i + 1..];
        let end = body
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(body.len());
        if body[end..].starts_with('"') && !body[..end].ends_with('_') {
            out.insert(body[..end].to_string());
        }
        rest = &rest[i + 1 + end..];
    }
}

/// 文書の節ごとに、行頭が ``- `GEOFRAME_…` `` の項目の名前を集める。
fn documented(doc: &str, section: &str) -> BTreeSet<String> {
    let mut in_section = false;
    let mut names = BTreeSet::new();
    for line in doc.lines() {
        if line.starts_with("## ") {
            in_section = line.trim_end() == section;
        } else if in_section && line.starts_with("- `GEOFRAME_") {
            names.insert(line[3..].split('`').next().unwrap().to_string());
        }
    }
    names
}

#[test]
fn every_variable_the_code_reads_is_documented() {
    let root = workspace_root();
    let this_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/env_reference.rs");
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 50,
        "ソースを集められていない: {}",
        files.len()
    );

    let mut in_code = BTreeSet::new();
    for file in files
        .iter()
        .filter(|f| f.canonicalize().ok() != this_file.canonicalize().ok() && !is_test_file(f))
    {
        names_in(
            &production_source(&std::fs::read_to_string(file).unwrap()),
            &mut in_code,
        );
    }
    assert!(in_code.len() >= 25, "名前を集められていない: {in_code:?}");

    let doc = std::fs::read_to_string(root.join("docs/reference/configuration.md")).unwrap();
    let rust = documented(&doc, SECTION_RUST);
    let old_only = documented(&doc, SECTION_OLD_ONLY);
    assert!(rust.len() >= 25, "「Rust が読む」の節が読めない: {rust:?}");
    assert!(
        !old_only.is_empty(),
        "「旧版のみ」の節が読めない（見出しを変えたら試験の定数も直す）"
    );

    let test_only: BTreeSet<String> = TEST_ONLY.iter().map(|s| s.to_string()).collect();
    let mut problems = Vec::new();

    for name in in_code.difference(&test_only) {
        if !rust.contains(name) {
            problems.push(format!(
                "{name}: コードが読むが、設定リファレンスの「Rust が読む」の節に無い（載せる。試験だけの名前なら TEST_ONLY へ）"
            ));
        }
    }
    for name in &rust {
        if !in_code.contains(name) {
            problems.push(format!(
                "{name}: 「Rust が読む」の節にあるが、コードに出てこない（綴りの誤りか、読まなくなった。節を移すか消す）"
            ));
        }
    }
    for name in &old_only {
        if in_code.contains(name) {
            problems.push(format!(
                "{name}: 「旧版のみ」の節にあるが、コードが読んでいる（「Rust が読む」の節へ移す）"
            ));
        }
    }
    for name in rust.intersection(&old_only) {
        problems.push(format!("{name}: 2 つの節の両方にある"));
    }
    for name in &test_only {
        if !in_code.contains(name) {
            problems.push(format!(
                "{name}: TEST_ONLY にあるが、コードに出てこない（外す）"
            ));
        }
        if rust.contains(name) || old_only.contains(name) {
            problems.push(format!("{name}: TEST_ONLY の名前は文書に載せない"));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn name_scanner_reads_only_complete_quoted_names() {
    let mut out = BTreeSet::new();
    names_in(
        r#"get("GEOFRAME_PORT") "GEOFRAME_" "GEOFRAME_AI_" "GEOFRAME_X=1" "GEOFRAME_a" k == "GEOFRAME_AI_CONFIG""#,
        &mut out,
    );
    let got: Vec<&str> = out.iter().map(String::as_str).collect();
    assert_eq!(got, ["GEOFRAME_AI_CONFIG", "GEOFRAME_PORT"]);
}

#[test]
fn names_only_in_tests_or_comments_do_not_count_as_read() {
    let src = r#"fn port() -> String { std::env::var("GEOFRAME_PORT").unwrap() }
// 以前は "GEOFRAME_OLD_COMMENT" を読んだ
let url = "http://example.test/x"; let k = "GEOFRAME_AFTER_URL";
#[cfg(test)]
mod tests {
    fn f() { let s = "}"; std::env::set_var("GEOFRAME_ONLY_IN_TEST", "1"); }
}
#[cfg(test)]
const T: &str = "GEOFRAME_TEST_CONST";
fn after() { let _ = "GEOFRAME_AFTER_TESTS"; }
"#;
    let mut out = BTreeSet::new();
    names_in(&production_source(src), &mut out);
    let got: Vec<&str> = out.iter().map(String::as_str).collect();
    assert_eq!(
        got,
        [
            "GEOFRAME_AFTER_TESTS",
            "GEOFRAME_AFTER_URL",
            "GEOFRAME_PORT"
        ]
    );
    // 文字リテラル・生文字列・続く属性の中の `{`・`;` で境界がずれない
    for src in [
        "#[cfg(test)]\nfn t() { let c = '{'; }\nfn main() { std::env::var(\"GEOFRAME_REAL\"); }",
        "#[cfg(test)]\nfn t() { let c = r#\"a\"{\"#; }\nfn main() { std::env::var(\"GEOFRAME_REAL\"); }",
        "#[cfg(test)]\n#[doc = \"{\"]\nfn t() { }\nfn main() { std::env::var(\"GEOFRAME_REAL\"); }",
        "#[cfg(test)]\n#[doc = \";\"]\nfn t() { std::env::var(\"GEOFRAME_IN_TEST\"); }\nfn main() { std::env::var(\"GEOFRAME_REAL\"); }",
        "/* { \"GEOFRAME_IN_COMMENT\" */\nfn main() { std::env::var(\"GEOFRAME_REAL\"); }",
    ] {
        let mut out = BTreeSet::new();
        names_in(&production_source(src), &mut out);
        let got: Vec<&str> = out.iter().map(String::as_str).collect();
        assert_eq!(got, ["GEOFRAME_REAL"], "{src}");
    }
    assert!(is_test_file(Path::new("crates/geoframe/src/ai/tests.rs")));
    assert!(is_test_file(Path::new(
        "crates/geoframe/src/ai/tests_http.rs"
    )));
    assert!(is_test_file(Path::new("crates/geoframe/tests/blog_api.rs")));
    assert!(!is_test_file(Path::new("crates/geoframe/src/config.rs")));
}
