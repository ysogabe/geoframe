//! `docs/reference/configuration.md`（手で保つ設定リファレンス）と、Rust のコードが読む `GEOFRAME_*` の名前の
//! 照合（P5b / T5.6b）。Python 版の `config.py` から文書を作っていた `gen_env_reference.py --check` の代わり。
//!
//! 文書は 2 つの節に分ける。
//! - 「Rust が読む環境変数」: コードが読む名前の全部。
//! - 「旧版のみ（削除済み。Rust は読まない）」: 旧 Python 版だけが読んだ名前。コードには出てこない。
//!
//! 照合するもの:
//! 1. `crates` 以下の全 `.rs` にある `"GEOFRAME_…"` の文字列（引用符で囲んだ名前そのもの）は、
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

/// 試験の中だけで使う名前（文書に載せない）。読む場所:
/// - `GEOFRAME_TEST_EUID`・`GEOFRAME_TEST_PYTHON_STATE`: `guard.rs` の、デバッグビルドだけの注入口
///   （実行ユーザーと Python 版の状態を試験から与える）。
/// - `GEOFRAME_TEST_PROXY_CHILD`・`GEOFRAME_TEST_CLOUD_PROXY_CHILD`: 試験の実行ファイルを子プロセスとして
///   動かすときの目印（`ai/tests_http.rs`・`ai/cloud/tests.rs`）。
const TEST_ONLY: &[&str] = &[
    "GEOFRAME_TEST_CLOUD_PROXY_CHILD",
    "GEOFRAME_TEST_EUID",
    "GEOFRAME_TEST_PROXY_CHILD",
    "GEOFRAME_TEST_PYTHON_STATE",
];

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
        .filter(|f| f.canonicalize().ok() != this_file.canonicalize().ok())
    {
        names_in(&std::fs::read_to_string(file).unwrap(), &mut in_code);
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
