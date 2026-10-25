//! 管理画面の静的ファイル（`app/static`・`app/vendor`）と、旅行ブログの旅程 JSON
//! （`app/travel_blog/plans/*.json`）をバイナリへ埋め込む表を作る。
//!
//! 単一バイナリは「バイナリ＋設定＋データ」だけで起動できる必要があり、実行時に `app/` を読まない
//! （docs/rust-migration/plan.md §1）。外部の埋め込み crate を足さず、ここで `include_bytes!` の
//! 表を生成する（依存を増やさない・musl 静的ビルドへ影響しない・ファイルが増えても再生成される）。
//!
//! 生成物は `$OUT_DIR/embedded_files.rs`。パスは `static/admin.html` のような `app/` からの相対で、
//! ソート済み（二分探索）。ドットで始まる名前は含めない（配信側もドット始まりは拒否する）。
//! 旅程 JSON は別の表（`PLAN_FILES`。名前は `<trip_id>.json` のファイル名だけで、`.json` で終わるものだけ。
//! `app/travel_blog/plans` 直下の通常ファイルに限り、そこへ至るパスの各要素（`app`・`travel_blog`・`plans`）と
//! 直下のリンクは辿らずにビルドを止める。エラーにはパス・ファイル名を出さない。検査は`build_plans.rs`）。
//! 配信の表（`FILES`）には入れない（`/static/`・`/vendor/` の配信とは無関係）。
//! 各ファイルに内容のハッシュ（FNV-1a 64bit）から作った ETag を付ける。時刻は埋め込まない:
//! チェックアウト時刻に依存する `Last-Modified` を出すと、ビルドごとに値が揺れるうえ、内容が
//! 変わったのに古い時刻で 304 になる事故（キオスクの古い JS）の元になるため。

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

// 旅程の検査（リンク・名前・読めること）は単体試験できるよう別ファイルにある（`src/embedded.rs` の試験も読む）
mod build_plans;

const ROOTS: [&str; 2] = ["static", "vendor"];

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} を読めません: {e}", dir.display()))
        .map(|e| e.unwrap())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .expect("ファイル名が UTF-8 でない");
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let key = format!("{rel}/{name}");
        // metadata はリンクを辿る。リンク先がファイルでもディレクトリでも扱える
        let meta = fs::metadata(&path).unwrap();
        if meta.is_dir() {
            walk(&path, &key, out);
        } else if meta.is_file() {
            out.push((key, path));
        }
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let app = manifest.join("../../app");
    let mut files = Vec::new();
    for root in ROOTS {
        let dir = app.join(root);
        // ディレクトリ配下の変更・追加・削除で再生成する
        println!("cargo:rerun-if-changed={}", dir.display());
        walk(&dir, root, &mut files);
    }
    files.sort();

    let mut code = String::from("static FILES: &[File] = &[\n");
    for (key, path) in &files {
        let path = path.canonicalize().unwrap();
        let data = fs::read(&path).unwrap();
        let etag = format!("\"{:016x}-{:x}\"", fnv1a(&data), data.len());
        writeln!(
            code,
            "    File {{ path: {key:?}, data: include_bytes!({:?}), etag: {etag:?} }},",
            path.display().to_string()
        )
        .unwrap();
    }
    code.push_str("];\n");

    // 旅行ブログの旅程 JSON（git 管理のソース。実行時は `app/` を読まないので埋め込む）。
    // 直下の通常ファイルだけ。リンクは断る（git 管理外のファイルの検査は配布スクリプト）
    // リポジトリの根から旅程のディレクトリまでの各要素（`app`・`app/travel_blog`・`plans`）にリンクがあれば止める。
    // エラーはパス・ファイル名を出さず定型文だけ（旅程の名前をビルドのログに残さない）
    let plans_dir = app.join("travel_blog/plans");
    println!("cargo:rerun-if-changed={}", plans_dir.display());
    println!("cargo:rerun-if-changed=build_plans.rs");
    let fail = |e: build_plans::PlanError| -> ! { panic!("{}", e.message()) };
    build_plans::reject_links(&manifest.join("../.."), &["app", "travel_blog", "plans"])
        .unwrap_or_else(|e| fail(e));
    let plans = build_plans::plan_files(&plans_dir).unwrap_or_else(|e| fail(e));
    code.push_str("static PLAN_FILES: &[PlanFile] = &[\n");
    for (name, path) in plans {
        let path = path
            .canonicalize()
            .unwrap_or_else(|_| fail(build_plans::PlanError::Unreadable));
        writeln!(
            code,
            "    PlanFile {{ name: {name:?}, data: include_bytes!({:?}) }},",
            path.display().to_string()
        )
        .unwrap();
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded_files.rs");
    fs::write(out, code).unwrap();
}
