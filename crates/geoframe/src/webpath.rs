//! URL パスの正規化と、静的配信の対象の判定。現行 `handlers.py` の `_norm_path`・`_static_allowed`・
//! `_admin_page_rewrite` などの移植で、純関数だけ（ファイルには触れない。実体の解決は `statics`）。
//!
//! 判定は必ず**デコードしたあとの正規化済みパス**で行う（`%2e%2e` でホワイトリストを迂回されない
//! ように）。仕様と Python との差は docs/rust-migration/server.md §9。

/// 正規化したパス。
#[derive(Debug, PartialEq, Eq)]
pub struct Normalized {
    /// `/` で始まり、`.` と `..` を解決して連続・末尾のスラッシュを落としたパス。
    pub path: String,
    /// デコード後のパスの末尾が `/` だった（ファイルへの要求にこれが付くと 404 にする）。
    pub trailing_slash: bool,
}

/// `%XX` を戻す。壊れた `%`（16進でない・足りない）はそのまま残す（Python の `unquote` と同じ）。
pub(crate) fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// リクエストのパス（クエリ付きでもよい）を正規化する。`None` は**拒否**:
/// NUL を含む（Python と同じ）、またはバックスラッシュを含む（Python は `/` に読み替えてから
/// 実ファイルを探し 404 になる。Rust は区切りの解釈を持たず、最初から 403 にする）。
///
/// Python との対応: クエリ・フラグメントを落とす → 先頭の `//` を1つに（`http.server` の
/// `parse_request` がやる）→ `%` を戻す（不正な UTF-8 は U+FFFD）→ `posixpath.normpath`。
/// `normpath` はデコード後の先頭がちょうど `//` のときだけ `//` を残す（`/%2fderivatives/…` は
/// ホワイトリストに当たらず 403 になる、という Python と同じ結果にするため同じ癖を移した）。
pub fn normalize(raw: &str) -> Option<Normalized> {
    let raw = raw.split('?').next().unwrap_or("");
    let raw = raw.split('#').next().unwrap_or("");
    let collapsed;
    let raw = if raw.starts_with("//") {
        collapsed = format!("/{}", raw.trim_start_matches('/'));
        collapsed.as_str()
    } else {
        raw
    };
    let decoded = String::from_utf8_lossy(&percent_decode(raw)).into_owned();
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let trailing_slash = decoded.trim_end().ends_with('/');
    let decoded = if decoded.starts_with('/') {
        decoded
    } else {
        format!("/{decoded}")
    };
    let initial = if decoded.starts_with("//") && !decoded.starts_with("///") {
        "//"
    } else {
        "/"
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Some(Normalized {
        path: format!("{initial}{}", parts.join("/")),
        trailing_slash,
    })
}

/// 管理画面のページ。正規化後の**完全一致**だけを内部で HTML へ読み替える（接頭辞一致にしない。
/// `/admin/../.env` のようなパスを通さないため）。戻り値は埋め込みファイルのパス。
pub fn page_target(normalized: &str) -> Option<&'static str> {
    match normalized {
        "/" | "/admin" => Some("static/admin.html"),
        "/blog-admin" => Some("static/blog-admin.html"),
        "/ai-search" => Some("static/admin.html"),
        _ => None,
    }
}

/// 静的に配る領域。許可は `/favicon.ico` と、この5つの接頭辞だけ（ホワイトリスト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// `/static/` — 埋め込み（`web/static`）
    Static,
    /// `/vendor/` — 埋め込み（`web/vendor`）
    Vendor,
    /// `/derivatives/` — `GEOFRAME_DATA_DIR/derivatives`
    Derivatives,
    /// `/photos/` — `GEOFRAME_DATA_DIR/photos`
    Photos,
    /// `/blog_exports/` — `GEOFRAME_DATA_DIR/blog_exports`
    BlogExports,
    /// `/favicon.ico` — 現行も実ファイルが無く 404
    Favicon,
}

impl Area {
    const PREFIXED: [Area; 5] = [
        Area::Static,
        Area::Vendor,
        Area::Derivatives,
        Area::Photos,
        Area::BlogExports,
    ];

    /// 先頭の `/` から末尾の `/` までの接頭辞（`Favicon` は完全一致のパス）。
    pub fn prefix(self) -> &'static str {
        match self {
            Area::Static => "/static/",
            Area::Vendor => "/vendor/",
            Area::Derivatives => "/derivatives/",
            Area::Photos => "/photos/",
            Area::BlogExports => "/blog_exports/",
            Area::Favicon => "/favicon.ico",
        }
    }

    /// アクセスログに出すルートのテンプレート。実際のパスは出さない。
    pub fn route(self) -> &'static str {
        match self {
            Area::Static => "/static/{*path}",
            Area::Vendor => "/vendor/{*path}",
            Area::Derivatives => "/derivatives/{*path}",
            Area::Photos => "/photos/{*path}",
            Area::BlogExports => "/blog_exports/{*path}",
            Area::Favicon => "/favicon.ico",
        }
    }

    /// データ置き場の直下のディレクトリ名。埋め込みの領域は `None`。
    pub fn data_dir_name(self) -> Option<&'static str> {
        match self {
            Area::Derivatives => Some("derivatives"),
            Area::Photos => Some("photos"),
            Area::BlogExports => Some("blog_exports"),
            _ => None,
        }
    }
}

/// 静的配信の対象。`rel` は領域の接頭辞より後ろの要素（空でない・`.`/`..` なし・ドット始まりなし）。
#[derive(Debug, PartialEq, Eq)]
pub struct Target<'a> {
    pub area: Area,
    pub rel: Vec<&'a str>,
}

/// 正規化済みパスがどの領域の接頭辞に当たるか（ドット始まりかどうかは見ない）。拒否した要求も
/// アクセスログにはテンプレートで出すために使う（`/derivatives/.dl_x` の 403 を `(unmatched)` にしない）。
pub fn area_of(normalized: &str) -> Option<Area> {
    if normalized == Area::Favicon.prefix() {
        return Some(Area::Favicon);
    }
    Area::PREFIXED
        .into_iter()
        .find(|area| normalized.starts_with(area.prefix()))
}

/// 正規化済みパスが静的に配ってよいものか。`None` は 403。
///
/// - どの要素もドットで始まらない（`/derivatives/.dl_<id>`、`/blog_exports/.<trip>-<temp>/…`。
///   実在するかを判定の前に見ないので、無いファイルも同じ 403 で、存在の有無が漏れない）。
/// - ホワイトリスト（`/favicon.ico` と `Area` の接頭辞）の内側であること。
pub fn classify(normalized: &str) -> Option<Target<'_>> {
    if !normalized.starts_with('/') {
        return None;
    }
    if normalized.split('/').any(|part| part.starts_with('.')) {
        return None;
    }
    let area = area_of(normalized)?;
    if area == Area::Favicon {
        return Some(Target {
            area,
            rel: Vec::new(),
        });
    }
    let rel: Vec<&str> = normalized[area.prefix().len()..].split('/').collect();
    Some(Target { area, rel })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(raw: &str) -> String {
        normalize(raw).unwrap().path
    }

    #[test]
    fn normalizes_like_posixpath_normpath() {
        for (raw, expected) in [
            ("/", "/"),
            ("/admin/", "/admin"),
            ("/derivatives//a.jpg", "/derivatives/a.jpg"),
            ("//derivatives/a.jpg", "/derivatives/a.jpg"),
            ("///derivatives/a.jpg", "/derivatives/a.jpg"),
            ("/derivatives/./a.jpg", "/derivatives/a.jpg"),
            ("/derivatives/sub/../a.jpg", "/derivatives/a.jpg"),
            (
                "/derivatives/%2e%2e/derivatives/a.jpg",
                "/derivatives/a.jpg",
            ),
            ("/derivatives/sub/%2E%2e/a.jpg", "/derivatives/a.jpg"),
            ("/derivatives/sub%2fb.png", "/derivatives/sub/b.png"),
            // ルートより上へは出られない
            ("/../../etc/passwd", "/etc/passwd"),
            ("/derivatives/../../../.env", "/.env"),
            ("/derivatives/%2e%2e/%2e%2e/.env", "/.env"),
            // クエリとフラグメントは落とす（生のパスでは見ない）
            ("/derivatives/a.jpg?x=../..", "/derivatives/a.jpg"),
            ("/derivatives/a.jpg#../..", "/derivatives/a.jpg"),
            ("/admin?x=1", "/admin"),
            // 日本語のファイル名
            ("/derivatives/%E3%81%82.jpg", "/derivatives/あ.jpg"),
            // 壊れた % はそのまま
            ("/derivatives/100%.jpg", "/derivatives/100%.jpg"),
            ("/derivatives/%zz.jpg", "/derivatives/%zz.jpg"),
            ("/derivatives/a%2", "/derivatives/a%2"),
            // 先頭のスラッシュが無い
            ("derivatives/a.jpg", "/derivatives/a.jpg"),
            // デコード後にちょうど `//` で始まるときだけ `//` が残る（normpath の癖）→ ホワイトリスト外
            ("/%2fderivatives/a.jpg", "//derivatives/a.jpg"),
            ("/%2f%2fderivatives/a.jpg", "/derivatives/a.jpg"),
        ] {
            assert_eq!(norm(raw), expected, "{raw}");
        }
    }

    #[test]
    fn invalid_utf8_becomes_replacement_char() {
        assert_eq!(norm("/derivatives/%ff.jpg"), "/derivatives/\u{fffd}.jpg");
    }

    #[test]
    fn nul_and_backslash_are_refused() {
        for raw in [
            "/derivatives/a.jpg%00",
            "/derivatives/a%00.jpg",
            "/derivatives/a\0.jpg",
            "/derivatives\\a.jpg",
            "/derivatives/sub%5Cb.png",
            "/derivatives/..%5c..%5c.env",
            "/derivatives/%5C..%5C..%5Cetc%5Cpasswd",
        ] {
            assert_eq!(normalize(raw), None, "{raw}");
        }
    }

    #[test]
    fn trailing_slash_is_reported_on_the_decoded_path() {
        assert!(normalize("/derivatives/a.jpg/").unwrap().trailing_slash);
        assert!(normalize("/derivatives/a.jpg%2F").unwrap().trailing_slash);
        assert!(normalize("/derivatives/").unwrap().trailing_slash);
        assert!(!normalize("/derivatives/a.jpg").unwrap().trailing_slash);
        assert!(!normalize("/derivatives/a.jpg?x=/").unwrap().trailing_slash);
    }

    #[test]
    fn page_rewrite_is_exact_match_after_normalization() {
        for raw in [
            "/",
            "/admin",
            "/admin/",
            "//admin",
            "/%61dmin",
            "/admin?x=1",
        ] {
            assert_eq!(page_target(&norm(raw)), Some("static/admin.html"), "{raw}");
        }
        assert_eq!(
            page_target(&norm("/blog-admin")),
            Some("static/blog-admin.html")
        );
        assert_eq!(page_target(&norm("/ai-search")), Some("static/admin.html"));
        // 現行に /blog は無い（403）。接頭辞一致でもない
        for raw in [
            "/blog",
            "/admin/x",
            "/admin/../.env",
            "/index.html",
            "/adminx",
        ] {
            assert_eq!(page_target(&norm(raw)), None, "{raw}");
        }
    }

    fn area(path: &str) -> Option<(Area, Vec<&str>)> {
        classify(path).map(|t| (t.area, t.rel))
    }

    #[test]
    fn whitelist_is_the_five_prefixes_and_favicon() {
        assert_eq!(
            area("/derivatives/sub/b.png"),
            Some((Area::Derivatives, vec!["sub", "b.png"]))
        );
        assert_eq!(area("/photos/x.jpg"), Some((Area::Photos, vec!["x.jpg"])));
        assert_eq!(
            area("/blog_exports/t/index.html"),
            Some((Area::BlogExports, vec!["t", "index.html"]))
        );
        assert_eq!(
            area("/static/js/admin.js"),
            Some((Area::Static, vec!["js", "admin.js"]))
        );
        assert_eq!(
            area("/vendor/fonts.css"),
            Some((Area::Vendor, vec!["fonts.css"]))
        );
        assert_eq!(area("/favicon.ico"), Some((Area::Favicon, vec![])));
        for path in [
            "/",
            "/.env",
            "/server.py",
            "/catalog.db",
            "/api/v1/photos",
            "/derivatives", // ディレクトリ自体（末尾の / を落とした形）
            "/static",
            "/staticx/a.js",
            "/Static/a.js",
            "//derivatives/a.jpg", // `normpath` の癖で残った `//`
            "/favicon.ico/x",
            "/favicon.icox",
        ] {
            assert_eq!(classify(path), None, "{path}");
        }
    }

    #[test]
    fn any_dot_element_is_refused_even_when_nothing_exists() {
        for path in [
            "/derivatives/.dl_abc",
            "/derivatives/.nope",
            "/blog_exports/.hid/f",
            "/blog_exports/trip/.tmp/x.jpg",
            "/static/.env",
            "/photos/a/.b/c.jpg",
            "/derivatives/.",
        ] {
            assert_eq!(classify(path), None, "{path}");
        }
        // 途中にドットを含むだけの名前は通る
        assert!(classify("/derivatives/a.b.c.jpg").is_some());
        assert!(classify("/derivatives/a..b.jpg").is_some());
    }

    #[test]
    fn every_traversal_shape_ends_outside_the_whitelist_or_inside_it() {
        // 正規化したあとにホワイトリストの内側へ残る形だけが通り、外へ出る形は必ず None
        for raw in [
            "/derivatives/../.env",
            "/derivatives/%2e%2e/.env",
            "/derivatives/%2e%2e%2f.env",
            "/derivatives/..%2f..%2fcatalog.db",
            "/derivatives/sub/../../catalog.db",
            "/static/../server.py",
            "/photos/../../etc/passwd",
            "/%2e%2e/%2e%2e/etc/passwd",
            "/etc/passwd",
            "//etc/passwd",
            "/derivatives/%2e%2e/%2e%2e/%2e%2e/etc/passwd",
        ] {
            let n = normalize(raw).unwrap();
            assert_eq!(classify(&n.path), None, "{raw} → {}", n.path);
        }
    }

    #[test]
    fn route_templates_never_contain_a_request_value() {
        for area in Area::PREFIXED {
            assert!(
                area.route().ends_with("/{*path}") || area == Area::Favicon,
                "{area:?}"
            );
        }
    }
}
