//! バイナリへ埋め込んだ管理画面の静的ファイル（`web/static`・`web/vendor`、`build.rs` が表を作る）。
//!
//! パスは `static/admin.html` のような `web/` からの相対（先頭の `/` なし）。

/// 埋め込んだ1ファイル。
pub struct File {
    pub path: &'static str,
    pub data: &'static [u8],
    /// 内容から作った強い ETag（引用符つき）。
    pub etag: &'static str,
}

/// 埋め込んだ旅行ブログの旅程 JSON 1つ（`web/travel_blog/plans/<name>`）。
pub struct PlanFile {
    /// `<trip_id>.json` のようなファイル名（ディレクトリは含まない）。
    pub name: &'static str,
    pub data: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/embedded_files.rs"));

/// 埋め込んだ旅程 JSON（ファイル名順）。
pub fn plan_files() -> &'static [PlanFile] {
    PLAN_FILES
}

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

/// `build.rs` が旅程の埋め込みの前に使う検査（リンク・名前・読めること）の単体試験。同じファイルを読む。
#[cfg(test)]
#[path = "../build_plans.rs"]
#[allow(dead_code)]
mod build_plans;

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
            "static/js/photos.js",
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

    /// 旅程 JSON の表がソート済みで、`.json` だけ・ドット始まりなしであること（中身は見ない）。
    #[test]
    fn plan_table_is_sorted_json_only() {
        let plans = plan_files();
        assert!(plans.windows(2).all(|w| w[0].name < w[1].name));
        assert!(
            plans
                .iter()
                .all(|p| p.name.ends_with(".json") && !p.name.starts_with('.'))
        );
        assert!(plans.iter().all(|p| !p.data.is_empty()));
        // 配信の表には入らない
        assert!(get("travel_blog/plans").is_none() && !is_dir("travel_blog"));
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
