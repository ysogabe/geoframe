//! `crates` の Rust のコードが Python を起動しないことの検査（P5b / T5.6a）。
//!
//! Python 版をリポジトリから外したあとに、試験や build.rs がまた Python を呼ぶようになると、CI の Rust のジョブに
//! Python の導入が戻る。プロセスを起動する呼び出しの引数に python が出る箇所と、`python3` の引用符つきの文字列を、
//! `crates` 以下の `.rs` から探す。`["python"]` のような JSON のキーや、`*-python.json` のような基準のファイル名は対象外
//! （起動するコードではない）。この検査の針は、この検査自身に当たらないように組み立てている。

use std::path::{Path, PathBuf};

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

/// 起動しているように見える箇所（1 始まりの行番号）を返す。
fn launches_python(text: &str) -> Vec<usize> {
    let call = ["Command", "::new("].concat();
    let literal = ["\"py", "thon3\""].concat();
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.contains(&literal) {
            found.push(i + 1);
        }
    }
    let mut from = 0;
    while let Some(at) = text[from..].find(&call) {
        let start = from + at + call.len();
        let end = text[start..].find(')').map_or(text.len(), |n| start + n);
        if text[start..end].to_ascii_lowercase().contains("python") {
            found.push(text[..start].matches('\n').count() + 1);
        }
        from = start;
    }
    found.sort_unstable();
    found.dedup();
    found
}

#[test]
fn no_rust_code_under_crates_launches_python() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(files.len() > 50, "{} files", files.len());
    let mut bad = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for line in launches_python(&text) {
            bad.push(format!(
                "{}:{line}",
                file.strip_prefix(&root).unwrap().display()
            ));
        }
    }
    assert!(bad.is_empty(), "Python を起動するコードがある: {bad:?}");
}

#[test]
fn the_detector_finds_launches_and_ignores_json_keys_and_fixture_names() {
    let call = ["Command", "::new("].concat();
    let py3 = ["\"py", "thon3\""].concat();
    let py = ["\"py", "thon\""].concat();
    assert_eq!(launches_python(&format!("let c = {call}{py3});")), [1]);
    assert_eq!(
        launches_python(&format!("x\nlet c = {call}{py}).arg(1);")),
        [2]
    );
    assert_eq!(launches_python(&format!("{call}\n    python())")), [1]);
    assert_eq!(
        launches_python(&format!("{call}std::env::current_exe().unwrap());")),
        []
    );
    assert_eq!(launches_python(&format!("{call}\"mkfifo\")")), []);
    assert_eq!(launches_python(&format!("let w = &case[{py}];")), []);
    assert_eq!(
        launches_python("include_str!(\"fixtures/exif-python.json\")"),
        []
    );
}
