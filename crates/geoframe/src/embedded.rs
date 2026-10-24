//! バイナリへ埋め込んだ管理画面の静的ファイル（`app/static`・`app/vendor`、`build.rs` が表を作る）。
//!
//! パスは `static/admin.html` のような `app/` からの相対（先頭の `/` なし）。

/// 埋め込んだ1ファイル。
pub struct File {
    pub path: &'static str,
    pub data: &'static [u8],
    /// 内容から作った強い ETag（引用符つき）。
    pub etag: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/embedded_files.rs"));

/// `static/admin.html` のようなパスのファイル。
pub fn get(path: &str) -> Option<&'static File> {
    FILES
        .binary_search_by(|f| f.path.cmp(path))
        .ok()
        .map(|i| &FILES[i])
}

/// そのパスを親にするファイルがあるか（＝ディレクトリとして存在するか）。
pub fn is_dir(path: &str) -> bool {
    let prefix = format!("{path}/");
    FILES.iter().any(|f| f.path.starts_with(&prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_has_no_dotfiles() {
        assert!(FILES.windows(2).all(|w| w[0].path < w[1].path));
        assert!(
            FILES
                .iter()
                .all(|f| f.path.split('/').all(|p| !p.starts_with('.')))
        );
        assert!(FILES.len() >= 10, "{}", FILES.len());
    }

    #[test]
    fn admin_pages_and_vendor_are_embedded() {
        for path in [
            "static/admin.html",
            "static/blog-admin.html",
            "static/ai-search.html",
            "vendor/fonts.css",
            "vendor/maplibre-gl-4.7.1.min.js",
        ] {
            let file = get(path).unwrap_or_else(|| panic!("{path} が埋め込まれていない"));
            assert!(!file.data.is_empty(), "{path}");
            assert!(file.etag.starts_with('"') && file.etag.ends_with('"'));
        }
        assert!(get("static/nope.js").is_none());
        assert!(get("derivatives/a.jpg").is_none());
    }

    #[test]
    fn directories_are_recognised() {
        assert!(is_dir("static"));
        assert!(is_dir("static/js"));
        assert!(is_dir("vendor"));
        assert!(!is_dir("static/admin.html"));
        assert!(!is_dir("static/nope"));
        // 接頭辞が同じだけの名前はディレクトリではない
        assert!(!is_dir("stat"));
    }
}
