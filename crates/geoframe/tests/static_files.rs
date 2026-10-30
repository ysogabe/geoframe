//! 静的配信・Host 検査・管理画面ページ・ヘッダの結合テスト（T1.3）。合成データだけを使う。
//!
//! 期待値は現行 Python（`app/geoframe/handlers.py`）に同じリクエストを流して確かめた結果。
//! Python と意図して変えた点は、テストのコメントに「差」と書いてある（docs/rust-migration/server.md §9）。

mod common;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use common::{TempDir, build_empty_catalog, request};
use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::server::router;

/// 2025-06-01 00:00:00 UTC（日曜日）
const MTIME: u64 = 1_748_736_000;
const MTIME_HTTP: &str = "sun, 01 jun 2025 00:00:00 gmt";

fn repo_file(path: &str) -> Vec<u8> {
    let full = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../web")
        .join(path);
    std::fs::read(&full).unwrap_or_else(|e| panic!("{full:?}: {e}"))
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(MTIME))
        .unwrap();
}

/// 一時ディレクトリの中の合成データ置き場。
///
/// ```text
/// <root>/data/                  データ置き場（catalog.db もここ）
///   derivatives/a.jpg  sub/b c.png  noext  p.html  あ.jpg  .dl_x
///   photos/p1.jpg   blog_exports/t/index.html  blog_exports/.hid/f
///   derivatives/inner.jpg -> sub/b c.png          （領域の内側を指すリンク）
///   derivatives/slink.jpg -> <root>/outside/secret.txt   （外を指すファイルのリンク）
///   derivatives/link      -> <root>/outside              （外を指すディレクトリのリンク）
///   derivatives/tocat.jpg -> ../catalog.db        （データ置き場の中でも領域の外）
/// <root>/outside/secret.txt     "TOP-SECRET"（決して返ってはいけない）
/// <root>/data-link -> data      （データ置き場自体がリンク）
/// ```
struct Site {
    _root: TempDir,
    root: PathBuf,
    data: PathBuf,
    _writer: rusqlite::Connection,
}

impl Site {
    fn new() -> Self {
        let tmp = TempDir::new("static");
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let (_, writer) = build_empty_catalog(&data, "");

        write(&data.join("derivatives/a.jpg"), "jpegdata\n");
        write(&data.join("derivatives/sub/b c.png"), "pngdata\n");
        write(&data.join("derivatives/noext"), "noext\n");
        write(&data.join("derivatives/p.html"), "<p>user html</p>\n");
        write(&data.join("derivatives/あ.jpg"), "nihongo\n");
        write(&data.join("derivatives/.dl_x"), "hidden original\n");
        write(&data.join("photos/p1.jpg"), "photo\n");
        write(&data.join("blog_exports/t/index.html"), "<p>export</p>\n");
        write(&data.join("blog_exports/.hid/f"), "temp export\n");
        write(&root.join("outside/secret.txt"), "TOP-SECRET\n");

        symlink("sub/b c.png", data.join("derivatives/inner.jpg")).unwrap();
        symlink(
            root.join("outside/secret.txt"),
            data.join("derivatives/slink.jpg"),
        )
        .unwrap();
        symlink(root.join("outside"), data.join("derivatives/link")).unwrap();
        symlink("../catalog.db", data.join("derivatives/tocat.jpg")).unwrap();
        symlink(&data, root.join("data-link")).unwrap();
        Site {
            _root: tmp,
            root,
            data,
            _writer: writer,
        }
    }

    /// `data_dir` を置き場にしてサーバーを起動する。`allowed` は `GEOFRAME_ALLOWED_HOSTS`。
    async fn serve(&self, data_dir: &Path, allowed: &str) -> std::net::SocketAddr {
        self.serve_blog(data_dir, allowed, Some("1")).await
    }

    /// [`Site::serve`] に `GEOFRAME_BLOG` を与える（`None` は未設定）。
    async fn serve_blog(
        &self,
        data_dir: &Path,
        allowed: &str,
        blog: Option<&str>,
    ) -> std::net::SocketAddr {
        let catalog = Catalog::open(&data_dir.join("catalog.db")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let allowed = allowed.to_string();
        let config = Config::load(
            |key| match key {
                "GEOFRAME_ALLOWED_HOSTS" => Some(allowed.clone()),
                "GEOFRAME_BLOG" => blog.map(str::to_string),
                _ => None,
            },
            &Overrides {
                listen: Some(addr),
                data_dir: Some(data_dir.to_path_buf()),
            },
        )
        .unwrap();
        let app = router(common::test_state(Arc::new(config), catalog));
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    async fn start(&self) -> std::net::SocketAddr {
        self.serve(&self.data, "frame.lan").await
    }
}

const LOCALHOST: &[(&str, &str)] = &[("Host", "localhost")];

async fn get(addr: std::net::SocketAddr, target: &str) -> common::Response {
    request(addr, "GET", target, LOCALHOST).await
}

async fn head(addr: std::net::SocketAddr, target: &str) -> common::Response {
    request(addr, "HEAD", target, LOCALHOST).await
}

// ---- 配信と Content-Type ---------------------------------------------------------------

#[tokio::test]
async fn data_files_are_served_with_type_length_and_last_modified() {
    let site = Site::new();
    let addr = site.start().await;
    for (target, ct, body) in [
        ("/derivatives/a.jpg", "image/jpeg", "jpegdata\n"),
        ("/derivatives/sub/b%20c.png", "image/png", "pngdata\n"),
        ("/derivatives/noext", "application/octet-stream", "noext\n"),
        ("/derivatives/%E3%81%82.jpg", "image/jpeg", "nihongo\n"),
        ("/photos/p1.jpg", "image/jpeg", "photo\n"),
        ("/blog_exports/t/index.html", "text/html", "<p>export</p>\n"),
        // クエリとフラグメントは無視する
        ("/derivatives/a.jpg?x=1", "image/jpeg", "jpegdata\n"),
        ("/derivatives/a.jpg#frag", "image/jpeg", "jpegdata\n"),
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 200, "{target}");
        assert_eq!(r.header("content-type"), Some(ct), "{target}");
        assert_eq!(r.text(), body, "{target}");
        assert_eq!(
            r.header("content-length"),
            Some(body.len().to_string().as_str()),
            "{target}"
        );
        assert_eq!(r.header("last-modified"), Some(MTIME_HTTP), "{target}");
        // 画像などは Cache-Control を付けない（HTML だけ後述）
        if ct != "text/html" {
            assert_eq!(r.header("cache-control"), None, "{target}");
        }
    }
}

#[tokio::test]
async fn head_is_judged_like_get_but_has_no_body() {
    let site = Site::new();
    let addr = site.start().await;
    for target in [
        "/derivatives/a.jpg",
        "/photos/p1.jpg",
        "/static/admin.css",
        "/vendor/fonts.css",
        "/",
        "/admin",
        "/blog-admin",
        "/ai-search",
    ] {
        let g = get(addr, target).await;
        let h = head(addr, target).await;
        assert_eq!(h.status, 200, "{target}");
        assert!(h.body.is_empty(), "{target}");
        assert_eq!(
            h.header("content-length"),
            g.header("content-length"),
            "{target}"
        );
        assert_eq!(
            h.header("content-type"),
            g.header("content-type"),
            "{target}"
        );
        assert_eq!(
            h.header("cache-control"),
            g.header("cache-control"),
            "{target}"
        );
    }
    // GET が 403/404 になるものは HEAD も同じ（存在の有無が HEAD だけで漏れない）
    for target in [
        "/derivatives/.dl_x",
        "/derivatives/.nope",
        "/derivatives",
        "/derivatives/sub/",
        "/derivatives/nope.jpg",
        "/derivatives/slink.jpg",
        "/.env",
        "/server.py",
        "/catalog.db",
        "/nope",
        "/api/nope",
        "/api/v1/photos", // 現行も HEAD /api/... は静的のホワイトリストに当たらず 403
    ] {
        let g = get(addr, target).await;
        let h = head(addr, target).await;
        if target != "/api/v1/photos" {
            assert_eq!(h.status, g.status, "{target}");
        }
        assert!(h.body.is_empty(), "{target}");
    }
    assert_eq!(head(addr, "/api/v1/photos").await.status, 403);
    assert_eq!(head(addr, "/derivatives/.dl_x").await.status, 403);
    assert_eq!(head(addr, "/derivatives/nope.jpg").await.status, 404);
}

// ---- 条件付き要求（304） ---------------------------------------------------------------

#[tokio::test]
async fn if_modified_since_gives_304_for_data_files() {
    let site = Site::new();
    let addr = site.start().await;
    for method in ["GET", "HEAD"] {
        // 同じ時刻・新しい時刻 → 304（本文なし。現行と同じく Last-Modified も付かない）
        for date in [
            "Sun, 01 Jun 2025 00:00:00 GMT",
            "Mon, 02 Jun 2025 00:00:00 GMT",
        ] {
            let r = request(addr, method, "/derivatives/a.jpg", &ims_owned(date)).await;
            assert_eq!(r.status, 304, "{method} {date}");
            assert!(r.body.is_empty());
        }
        // 古い時刻・読めない値 → 200
        for date in ["Sat, 31 May 2025 23:59:59 GMT", "garbage"] {
            let r = request(addr, method, "/derivatives/a.jpg", &ims_owned(date)).await;
            assert_eq!(r.status, 200, "{method} {date}");
        }
    }
    // If-None-Match が付いていれば日付は見ない（現行と同じ。ETag は出さないので毎回 200）
    let r = request(
        addr,
        "GET",
        "/derivatives/a.jpg",
        &[
            ("Host", "localhost"),
            ("If-Modified-Since", "Mon, 02 Jun 2025 00:00:00 GMT"),
            ("If-None-Match", "\"x\""),
        ],
    )
    .await;
    assert_eq!(r.status, 200);
    // 304 でも no-cache などのヘッダ（HTML・/static/）は付く
    let r = request(
        addr,
        "GET",
        "/derivatives/p.html",
        &ims_owned("Mon, 02 Jun 2025 00:00:00 GMT"),
    )
    .await;
    assert_eq!(r.status, 304);
    assert_eq!(r.header("x-frame-options"), Some("deny"));
}

fn ims_owned(date: &str) -> Vec<(&str, &str)> {
    vec![("Host", "localhost"), ("If-Modified-Since", date)]
}

#[tokio::test]
async fn embedded_files_use_etag_not_last_modified() {
    let site = Site::new();
    let addr = site.start().await;
    let first = get(addr, "/static/admin.css").await;
    assert_eq!(first.status, 200);
    // 差: Python は Last-Modified を付けるが、埋め込みはビルド時刻に依存する日付を出さず ETag で再検証する
    assert_eq!(first.header("last-modified"), None);
    let etag = first.header("etag").expect("ETag").to_string();
    assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
    assert_eq!(first.header("cache-control"), Some("no-cache"));

    for match_header in [
        etag.clone(),
        format!("W/{etag}"),
        format!("\"other\", {etag}"),
        "*".into(),
    ] {
        let r = request(
            addr,
            "GET",
            "/static/admin.css",
            &[("Host", "localhost"), ("If-None-Match", &match_header)],
        )
        .await;
        assert_eq!(r.status, 304, "{match_header}");
        assert!(r.body.is_empty());
        assert_eq!(r.header("etag"), Some(etag.as_str()));
        assert_eq!(r.header("cache-control"), Some("no-cache"));
    }
    let r = request(
        addr,
        "GET",
        "/static/admin.css",
        &[("Host", "localhost"), ("If-None-Match", "\"stale\"")],
    )
    .await;
    assert_eq!(r.status, 200);
    // 日付だけの再検証は常に全文（古い JS が 304 で居座らない）
    let r = request(
        addr,
        "GET",
        "/static/admin.css",
        &ims_owned("Mon, 02 Jun 2035 00:00:00 GMT"),
    )
    .await;
    assert_eq!(r.status, 200);
}

// ---- 埋め込みファイルと管理画面のページ ---------------------------------------------------

#[tokio::test]
async fn every_embedded_file_is_served_byte_for_byte() {
    let site = Site::new();
    let addr = site.start().await;
    let mut checked = 0;
    for dir in ["static", "vendor"] {
        let mut stack = vec![
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../web")
                .join(dir),
        ];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                    continue;
                }
                let rel = entry
                    .strip_prefix(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web"))
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                let r = get(addr, &format!("/{rel}")).await;
                assert_eq!(r.status, 200, "{rel}");
                assert_eq!(r.body, repo_file(&rel), "{rel}");
                assert_eq!(r.header("cache-control"), Some("no-cache"), "{rel}");
                checked += 1;
            }
        }
    }
    assert!(checked >= 10, "{checked}");
    // 種類ごとの Content-Type
    for (target, ct) in [
        ("/static/admin.html", "text/html"),
        ("/static/admin.css", "text/css"),
        ("/static/js/admin.js", "text/javascript"),
        ("/vendor/fonts.css", "text/css"),
        ("/vendor/maplibre-gl-4.7.1.min.js", "text/javascript"),
        ("/vendor/fonts/share-tech-mono-latin.woff2", "font/woff2"),
    ] {
        assert_eq!(
            get(addr, target).await.header("content-type"),
            Some(ct),
            "{target}"
        );
    }
}

#[tokio::test]
async fn admin_pages_are_rewritten_to_the_embedded_html() {
    let site = Site::new();
    let addr = site.start().await;
    let admin = repo_file("static/admin.html");
    for target in [
        "/",
        "/admin",
        "/admin/",
        "/admin?x=1",
        "//admin",
        "/%61dmin",
        "/admin/../admin",
        "/static/admin.html",
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 200, "{target}");
        assert_eq!(r.body, admin, "{target}");
        assert_eq!(r.header("content-type"), Some("text/html"), "{target}");
    }
    assert_eq!(
        get(addr, "/blog-admin").await.body,
        repo_file("static/blog-admin.html")
    );
    assert_eq!(
        get(addr, "/ai-search").await.body,
        repo_file("static/ai-search.html")
    );
    // 読み替えは完全一致だけ。現行に /blog は無い（403）
    for target in [
        "/blog",
        "/admin/x",
        "/admin/../.env",
        "/index.html",
        "/adminx",
        "/ai-search/x",
    ] {
        assert_eq!(get(addr, target).await.status, 403, "{target}");
    }
}

#[tokio::test]
async fn blog_admin_serves_the_admin_page_unless_the_blog_is_enabled() {
    let site = Site::new();
    let admin = repo_file("static/admin.html");
    let blog_admin = repo_file("static/blog-admin.html");
    assert_ne!(admin, blog_admin);
    // 未設定・`0`: `/blog-admin` は管理画面の HTML（200）。ヘッダは HTML のまま
    for blog in [None, Some("0")] {
        let addr = site.serve_blog(&site.data, "frame.lan", blog).await;
        for target in [
            "/blog-admin",
            "/blog-admin/",
            "/blog-admin?x=1",
            "/%62log-admin",
        ] {
            let r = get(addr, target).await;
            assert_eq!(r.status, 200, "{blog:?} {target}");
            assert_eq!(r.body, admin, "{blog:?} {target}");
            assert_eq!(
                r.header("content-type"),
                Some("text/html"),
                "{blog:?} {target}"
            );
            assert_eq!(
                r.header("x-frame-options"),
                Some("deny"),
                "{blog:?} {target}"
            );
            assert_eq!(
                r.header("content-security-policy"),
                Some("frame-ancestors 'none'"),
                "{blog:?} {target}"
            );
            assert_eq!(
                r.header("cache-control"),
                Some("no-cache"),
                "{blog:?} {target}"
            );
        }
        let h = head(addr, "/blog-admin").await;
        assert_eq!(h.status, 200);
        assert!(h.body.is_empty());
        // `/static/` 配下の旅行ブログの部品と書き出し済みの `/blog_exports/` は、今までどおり配る
        let r = get(addr, "/static/blog-admin.html").await;
        assert_eq!(r.status, 200, "{blog:?}");
        assert_eq!(r.body, blog_admin, "{blog:?}");
        assert_eq!(get(addr, "/blog_exports/t/index.html").await.status, 200);
        // ほかのページは変わらない
        assert_eq!(get(addr, "/admin").await.body, admin);
        assert_eq!(
            get(addr, "/ai-search").await.body,
            repo_file("static/ai-search.html")
        );
    }
    // `1`: 旅行ブログの HTML
    let addr = site.serve_blog(&site.data, "frame.lan", Some("1")).await;
    let r = get(addr, "/blog-admin").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, blog_admin);
    assert_eq!(r.header("x-frame-options"), Some("deny"));
}

// ---- ホワイトリスト・ドット・ディレクトリ ---------------------------------------------------

#[tokio::test]
async fn paths_outside_the_whitelist_are_403() {
    let site = Site::new();
    let addr = site.start().await;
    for target in [
        "/.env",
        "/server.py",
        "/catalog.db",
        "/drive_key.json",
        "/nope",
        "/api/nope",
        "/api/v1/photos/",
        "/healthz/",
        "/app/server.py",
        "/Derivatives/a.jpg",
        "/derivativesx/a.jpg",
        "/static", // ディレクトリ自体
        "/vendor/",
        "/derivatives",
        "/derivatives/",
        "/photos",
        "/blog_exports",
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 403, "{target}");
        assert!(!r.text().contains("TOP-SECRET"));
    }
    // /favicon.ico は許可されているが、現行もファイルが無く 404
    assert_eq!(get(addr, "/favicon.ico").await.status, 404);
    assert_eq!(get(addr, "/favicon.ico/").await.status, 404);
    assert_eq!(get(addr, "/favicon.icox").await.status, 403);
}

#[tokio::test]
async fn dot_elements_are_403_even_when_they_do_not_exist() {
    let site = Site::new();
    let addr = site.start().await;
    for target in [
        "/derivatives/.dl_x",   // 実在する作業用ファイル
        "/derivatives/.nope",   // 実在しない（存在の有無で応答を変えない）
        "/derivatives/%2edl_x", // エンコードしても同じ
        "/derivatives/%2Edl_x",
        "/derivatives/sub/.x",
        "/derivatives/.sub/b.png",
        "/blog_exports/.hid/f", // 書き出しの作業領域
        "/blog_exports/.hid/",
        "/blog_exports/t/.tmp/x.jpg",
        "/static/.env",
        "/vendor/.git/config",
        "/photos/.hidden.jpg",
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 403, "{target}");
        assert!(!r.text().contains("hidden original") && !r.text().contains("temp export"));
    }
    // 途中にドットを含むだけの名前は普通に扱う
    write(&site.data.join("derivatives/a.b.c.jpg"), "dots\n");
    assert_eq!(get(addr, "/derivatives/a.b.c.jpg").await.text(), "dots\n");
}

#[tokio::test]
async fn directories_are_403_never_listed_and_missing_files_are_404() {
    let site = Site::new();
    let addr = site.start().await;
    for target in [
        "/derivatives/sub",
        "/derivatives/sub/",
        "/static/js",
        "/static/js/",
        "/vendor/fonts",
        "/blog_exports/t",
        "/derivatives/link", // 外を指すディレクトリのリンクも一覧は出ない
        "/derivatives/link/",
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 403, "{target}");
        assert!(
            !r.text().contains("a.jpg") && !r.text().contains("secret"),
            "{target}"
        );
    }
    for target in [
        "/derivatives/nope.jpg",
        "/derivatives/A.jpg", // 大文字小文字は区別する
        "/derivatives/sub/nope.png",
        "/photos/nope.jpg",
        "/static/nope.js",
        "/vendor/fonts/nope.woff2",
        "/derivatives/a.jpg/", // ファイルに / を付けた要求（現行も 404）
        "/derivatives/a.jpg%2F",
        "/derivatives/a.jpg/x",
        "/static/admin.css/",
    ] {
        assert_eq!(get(addr, target).await.status, 404, "{target}");
    }
}

// ---- パストラバーサル -----------------------------------------------------------------

#[tokio::test]
async fn traversal_never_reaches_files_outside_the_allowed_areas() {
    let site = Site::new();
    let addr = site.start().await;
    // データ置き場の隣に「外」がある。どの形でも TOP-SECRET・catalog.db・環境ファイルは返らない
    write(&site.data.join(".env"), "SECRET_ENV=1\n");
    for target in [
        "/derivatives/../catalog.db",
        "/derivatives/../.env",
        "/derivatives/%2e%2e/catalog.db",
        "/derivatives/%2E%2E/%2E%2E/outside/secret.txt",
        "/derivatives/%2e%2e%2fcatalog.db",
        "/derivatives/..%2fcatalog.db",
        "/derivatives/..%2F..%2Foutside%2Fsecret.txt",
        "/derivatives/sub/../../catalog.db",
        "/derivatives/sub/..%2f..%2fcatalog.db",
        "/derivatives/%2e%2e/%2e%2e/%2e%2e/etc/passwd",
        "/derivatives/../../../../../../etc/passwd",
        "/static/../server.py",
        "/static/%2e%2e/%2e%2e/catalog.db",
        "/photos/../../etc/passwd",
        "/derivatives/..\\catalog.db",
        "/derivatives/..%5ccatalog.db",
        "/derivatives\\..\\catalog.db",
        "/derivatives/a.jpg%00",
        "/derivatives/a%00.jpg",
        "/derivatives/%00/../catalog.db",
        "/derivatives/%252e%252e/catalog.db",
        "//etc/passwd",
        "///etc/passwd",
        "/%2fetc/passwd",
        "/%2f%2fetc%2fpasswd",
        "/derivatives//..//catalog.db",
        "/derivatives/./../catalog.db",
        "/etc/passwd",
        "/derivatives/%c0%ae%c0%ae/catalog.db",
        "/derivatives/%ff%ff.jpg",
    ] {
        let r = get(addr, target).await;
        assert!(matches!(r.status, 403 | 404), "{target} → {}", r.status);
        let text = String::from_utf8_lossy(&r.body);
        for leaked in ["TOP-SECRET", "SECRET_ENV", "SQLite format", "root:"] {
            assert!(!text.contains(leaked), "{target} が {leaked} を返した");
        }
    }
    // 領域の内側に残る形は現行と同じく普通に配る（`..` は解決して、`%2f` は区切りとして扱う）
    for target in [
        "/derivatives/sub/../a.jpg",
        "/derivatives/./a.jpg",
        "/derivatives//a.jpg",
        "//derivatives/a.jpg",
        "/derivatives/%2e%2e/derivatives/a.jpg",
        "/derivatives/sub%2fb%20c.png",
    ] {
        assert_eq!(get(addr, target).await.status, 200, "{target}");
    }
    // 差: バックスラッシュは、Python は区切りに読み替えたうえで 404、Rust は区切りとして解釈せず 403
    assert_eq!(get(addr, "/derivatives/sub%5Cb%20c.png").await.status, 403);
}

#[tokio::test]
async fn symlinks_may_not_lead_out_of_the_area() {
    let site = Site::new();
    let addr = site.start().await;
    // 差: Python は外を指すリンクも辿って配信する（slink.jpg は 200 で TOP-SECRET を返す）。
    //     Rust は解決後のパスが領域のディレクトリの内側でなければ 403
    for target in [
        "/derivatives/slink.jpg",       // 外のファイル
        "/derivatives/link/secret.txt", // 外のディレクトリ越し
        "/derivatives/tocat.jpg",       // データ置き場の中だが領域（derivatives）の外
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 403, "{target}");
        assert!(
            !r.text().contains("TOP-SECRET") && !r.body.starts_with(b"SQLite"),
            "{target}"
        );
        assert_eq!(head(addr, target).await.status, 403, "HEAD {target}");
    }
    // 領域の内側を指すリンクは配る
    let r = get(addr, "/derivatives/inner.jpg").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.text(), "pngdata\n");
    // 壊れたリンク・自分自身を指すリンクは 404
    symlink("nowhere", site.data.join("derivatives/dangling.jpg")).unwrap();
    symlink("loop.jpg", site.data.join("derivatives/loop.jpg")).unwrap();
    assert_eq!(get(addr, "/derivatives/dangling.jpg").await.status, 404);
    assert_eq!(get(addr, "/derivatives/loop.jpg").await.status, 404);
}

#[tokio::test]
async fn the_data_dir_itself_may_be_a_symlink() {
    let site = Site::new();
    // T1.1 レビューの申し送り: Python は resolve() でデータ置き場のリンクを辿る。Rust も起動時に解決する
    let addr = site.serve(&site.root.join("data-link"), "").await;
    let r = get(addr, "/derivatives/a.jpg").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.text(), "jpegdata\n");
    assert_eq!(get(addr, "/derivatives/inner.jpg").await.status, 200);
    // リンクで入ってきても、外へ出る要求は拒否のまま
    assert_eq!(get(addr, "/derivatives/slink.jpg").await.status, 403);
    assert_eq!(get(addr, "/derivatives/tocat.jpg").await.status, 403);
    assert_eq!(get(addr, "/derivatives/../catalog.db").await.status, 403);
    // 起動後にデータ置き場のリンクの向きが変わっても、起動時に解決した先を使い続ける（入れ替えで外へ出ない）
    let elsewhere = site.root.join("outside");
    std::fs::remove_file(site.root.join("data-link")).unwrap();
    symlink(&elsewhere, site.root.join("data-link")).unwrap();
    let r = get(addr, "/derivatives/a.jpg").await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn an_area_directory_that_is_a_symlink_is_followed() {
    // 運用者が derivatives/ 自体を別のディスクへ向けるのは設定の範囲（Python も辿る）。
    // その中のリンクは、解決後の領域の内側かどうかで判定する
    let site = Site::new();
    let moved = site.root.join("moved-derivatives");
    std::fs::rename(site.data.join("derivatives"), &moved).unwrap();
    symlink(&moved, site.data.join("derivatives")).unwrap();
    let addr = site.start().await;
    assert_eq!(get(addr, "/derivatives/a.jpg").await.status, 200);
    assert_eq!(get(addr, "/derivatives/inner.jpg").await.status, 200);
    assert_eq!(get(addr, "/derivatives/slink.jpg").await.status, 403);
    assert_eq!(get(addr, "/derivatives/tocat.jpg").await.status, 404); // ../catalog.db は moved の隣に無い
}

#[tokio::test]
async fn special_files_are_not_opened() {
    let site = Site::new();
    // FIFO を開くと読み手が来るまで固まる。通常のファイル以外は 404
    let fifo = site.data.join("derivatives/pipe.jpg");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status();
    if status.is_ok_and(|s| s.success()) {
        let addr = site.start().await;
        let r = tokio::time::timeout(Duration::from_secs(5), get(addr, "/derivatives/pipe.jpg"))
            .await
            .expect("FIFO で固まった");
        assert_eq!(r.status, 404);
    }
}

// ---- ヘッダ ------------------------------------------------------------------------------

#[tokio::test]
async fn cache_and_framing_headers_follow_the_current_server() {
    let site = Site::new();
    let addr = site.start().await;
    let deny = ("x-frame-options", "deny");
    let csp = "frame-ancestors 'none'";

    // HTML: no-cache + 枠への埋め込み禁止
    for target in [
        "/",
        "/admin",
        "/static/admin.html",
        "/blog-admin",
        "/ai-search",
        "/derivatives/p.html",
        "/blog_exports/t/index.html",
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.header(deny.0), Some(deny.1), "{target}");
        assert_eq!(r.header("content-security-policy"), Some(csp), "{target}");
        assert_eq!(r.header("cache-control"), Some("no-cache"), "{target}");
    }
    // /static/・/vendor/ の JS・CSS は no-cache だけ（HTML ではない）
    for target in [
        "/static/admin.css",
        "/static/js/admin.js",
        "/vendor/fonts.css",
    ] {
        let r = get(addr, target).await;
        assert_eq!(r.header("cache-control"), Some("no-cache"), "{target}");
        assert_eq!(r.header(deny.0), None, "{target}");
    }
    // 画像（/derivatives/・/photos/）は付けない
    for target in ["/derivatives/a.jpg", "/photos/p1.jpg"] {
        let r = get(addr, target).await;
        assert_eq!(r.header("cache-control"), None, "{target}");
        assert_eq!(r.header(deny.0), None, "{target}");
    }
    // 403・404 にも、パスに応じて付く（現行と同じ）
    assert_eq!(
        get(addr, "/static/nope.js").await.header("cache-control"),
        Some("no-cache")
    );
    assert_eq!(
        get(addr, "/static/").await.header("cache-control"),
        Some("no-cache")
    );
    assert_eq!(get(addr, "/index.html").await.header(deny.0), Some(deny.1));
    assert_eq!(
        get(addr, "/derivatives/nope.jpg")
            .await
            .header("cache-control"),
        None
    );
    // 差: Python は生のパスで判定するため、`%2E` で書くと同じ HTML・JS に付かなかった。Rust はデコード後で判定する
    let r = get(addr, "/static/admin%2Ehtml").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header(deny.0), Some(deny.1));
    assert_eq!(
        get(addr, "/%73tatic/admin.css")
            .await
            .header("cache-control"),
        Some("no-cache")
    );
    // API は no-store で、HTML 用のヘッダは付かない
    let r = get(addr, "/api/v1/photos").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.header(deny.0), None);
    assert_eq!(
        get(addr, "/healthz").await.header("cache-control"),
        Some("no-store")
    );
}

#[tokio::test]
async fn error_pages_have_the_python_body() {
    let site = Site::new();
    let addr = site.start().await;
    let r = get(addr, "/.env").await;
    assert_eq!(r.header("content-type"), Some("text/html;charset=utf-8"));
    assert_eq!(r.header("content-length"), Some("349"));
    assert!(r.text().contains("<p>Error code: 403</p>"));
    assert!(r.text().contains("<p>Message: Forbidden.</p>"));
    let r = get(addr, "/derivatives/nope.jpg").await;
    assert_eq!(r.header("content-length"), Some("335"));
    assert!(r.text().contains("<p>Message: File not found.</p>"));
    // どちらも環境の情報（パス・ファイル名）を本文に含めない
    assert!(!r.text().contains("nope.jpg") && !r.text().contains("derivatives"));
}

// ---- Host 検査 ---------------------------------------------------------------------------

#[tokio::test]
async fn host_check_follows_the_current_rules() {
    let site = Site::new();
    let addr = site.start().await;
    let port = addr.port();
    let allowed_hosts: Vec<String> = [
        "localhost".to_string(),
        "LOCALHOST.".into(),
        format!("localhost:{port}"),
        "localhost:".into(),
        "frame.lan".into(),
        format!("Frame.LAN:{port}"),
        "192.0.2.7".into(),
        format!("192.0.2.7:{port}"),
        "[::1]".into(),
        format!("[::1]:{port}"),
        "::1".into(),
        "fe80::1%en0".into(),
        format!("[fe80::1%25en0]:{port}"),
        "::ffff:192.0.2.7".into(),
    ]
    .into();
    let refused_hosts: Vec<String> = [
        "evil.example".to_string(),
        format!("evil.example:{port}"),
        "localhost:1".into(), // 待ち受けポートでないポート
        format!("localhost:0{port}"),
        format!("192.0.2.7:{}", port.wrapping_add(1)),
        "010.0.0.1".into(),
        "999.1.1.1".into(),
        "1.2.3".into(),
        format!(":{port}"),
        "a:b:c".into(),
        "localhost.evil.example".into(),
        "xframe.lan".into(),
    ]
    .into();
    // ルーティングより前に効くので、どのルートでも結果は同じ
    for route in [
        "/api/v1/photos",
        "/healthz",
        "/derivatives/a.jpg",
        "/static/admin.css",
        "/",
    ] {
        for host in &allowed_hosts {
            let r = request(addr, "GET", route, &[("Host", host)]).await;
            assert_ne!(r.status, 403, "{host} {route} が拒否された");
            assert_eq!(r.status, 200, "{host} {route}");
        }
        for host in &refused_hosts {
            let r = request(addr, "GET", route, &[("Host", host)]).await;
            assert_eq!(r.status, 403, "{host} {route} が通った");
            assert!(
                r.text().contains("<p>Message: Forbidden host.</p>"),
                "{host}"
            );
            assert!(r.text().contains("GEOFRAME_ALLOWED_HOSTS"), "{host}");
            assert_eq!(r.header("content-length"), Some("475"), "{host}");
            // 拒否したことは本文に Host の値を入れない
            assert!(!r.text().contains("evil.example"), "{host}");
        }
    }
    // Host が無い・空は許可（HTTP/1.0 の素の要求・監視スクリプト）
    assert_eq!(request(addr, "GET", "/healthz", &[]).await.status, 200);
    assert_eq!(
        request(addr, "GET", "/healthz", &[("Host", "")])
            .await
            .status,
        200
    );
    assert_eq!(
        request(addr, "GET", "/healthz", &[("Host", "  ")])
            .await
            .status,
        200
    );
    // HEAD・POST・未実装のメソッドの扱い
    assert_eq!(
        request(
            addr,
            "HEAD",
            "/derivatives/a.jpg",
            &[("Host", "evil.example")]
        )
        .await
        .status,
        403
    );
    let r = request(
        addr,
        "POST",
        "/api/admin/setflag",
        &[("Host", "evil.example")],
    )
    .await;
    assert_eq!(r.status, 403);
    assert!(r.text().contains("Forbidden host"));
}

/// `Host` ヘッダが複数ある要求は、順序によらず 400（先頭だけで判定してすり抜けさせない）。
/// 差: Python は先頭の値だけで判定する（`self.headers.get`）ので、`Host: localhost` を先に置けば通った。
#[tokio::test]
async fn duplicate_host_headers_are_rejected_in_either_order() {
    let site = Site::new();
    let addr = site.start().await;
    for route in ["/healthz", "/api/v1/photos", "/derivatives/a.jpg", "/"] {
        for hosts in [
            [("Host", "localhost"), ("Host", "evil.example")],
            [("Host", "evil.example"), ("Host", "localhost")],
            [("Host", "localhost"), ("Host", "localhost")],
        ] {
            let r = request(addr, "GET", route, &hosts).await;
            assert_eq!(r.status, 400, "{hosts:?} {route}");
            assert!(r.text().contains("<p>Error code: 400</p>"), "{}", r.text());
            // 理由の説明にも Host の値を載せない
            assert!(!r.text().contains("evil.example"), "{}", r.text());
        }
    }
    // 1つだけなら従来どおり（許可は200、不許可は403）、無ければ許可
    assert_eq!(get(addr, "/healthz").await.status, 200);
    assert_eq!(
        request(addr, "GET", "/healthz", &[("Host", "evil.example")])
            .await
            .status,
        403
    );
    assert_eq!(request(addr, "GET", "/healthz", &[]).await.status, 200);
}

/// 絶対形式の request-target（`GET http://host/...`）は、`Host` が許可でも全ルートで 403
/// （ホワイトリストに当たらない現行の挙動。`/healthz`・`/api/v1/photos` にも通さない）。
/// Host 検査が先なので、`Host` が不許可なら 403（Host 用の本文）のまま。
#[tokio::test]
async fn absolute_form_targets_are_refused_on_every_route() {
    let site = Site::new();
    let addr = site.start().await;
    for route in [
        "/healthz",
        "/api/v1/photos",
        "/derivatives/a.jpg",
        "/static/admin.css",
        "/photos/p1.jpg",
        "/admin",
        "/",
        "/tiles/1/2/3.png",
        "/api/admin/status",
        "/nope",
    ] {
        for method in ["GET", "HEAD", "POST"] {
            let target = format!("http://evil.example{route}");
            let r = request(addr, method, &target, LOCALHOST).await;
            assert_eq!(r.status, 403, "{method} {target}");
            assert!(r.text().is_empty() || r.text().contains("Message: Forbidden."));
            assert!(!r.text().contains("TOP-SECRET") && !r.text().contains("\"ok\""));
        }
        // 同じ宛先でも、origin-form なら（GET の healthz と写真APIは）200
    }
    assert_eq!(get(addr, "/healthz").await.status, 200);
    assert_eq!(get(addr, "/api/v1/photos").await.status, 200);
    // Host が不許可なら、絶対形式でも Host 検査の本文（現行は _host_ok が先）
    let r = request(
        addr,
        "GET",
        "http://evil.example/healthz",
        &[("Host", "evil.example")],
    )
    .await;
    assert_eq!(r.status, 403);
    assert!(r.text().contains("Forbidden host"));
}

#[tokio::test]
async fn names_are_allowed_only_when_listed_in_the_environment() {
    let site = Site::new();
    // 既定（GEOFRAME_ALLOWED_HOSTS なし）では localhost だけ
    let addr = site.serve(&site.data, "").await;
    assert_eq!(
        request(addr, "GET", "/healthz", &[("Host", "localhost")])
            .await
            .status,
        200
    );
    assert_eq!(
        request(addr, "GET", "/healthz", &[("Host", "frame.lan")])
            .await
            .status,
        403
    );
    // 追加すると通る。ポート付きで書いても名前だけ見る
    let addr = site
        .serve(&site.data, "frame.lan:8080, Other.Example.")
        .await;
    for host in ["frame.lan", "other.example", "OTHER.example."] {
        assert_eq!(
            request(addr, "GET", "/healthz", &[("Host", host)])
                .await
                .status,
            200,
            "{host}"
        );
    }
    assert_eq!(
        request(addr, "GET", "/healthz", &[("Host", "third.example")])
            .await
            .status,
        403
    );
}

// ---- メソッド（CSRF の外側の関門） ----------------------------------------------------------

#[tokio::test]
async fn post_without_a_route_is_403_and_other_methods_are_501_without_cors() {
    let site = Site::new();
    let addr = site.start().await;
    let post = |target: &'static str| async move {
        request(
            addr,
            "POST",
            target,
            &[
                ("Host", "localhost"),
                ("Content-Type", "application/json"),
                ("Content-Length", "0"),
            ],
        )
        .await
    };
    // ルートが無い POST は、Content-Type・本文の検査より前に 403（`/api/blog/` 配下も。現行の send_error(403)）
    for target in [
        "/api/nope",
        "/api/blog/nope",
        "/api/admin/status",
        "/",
        "/admin",
        "/derivatives/a.jpg",
        "/healthz",
        "/api/v1/photos",
    ] {
        let r = post(target).await;
        assert_eq!(r.status, 403, "POST {target}");
        assert!(!r.headers.contains("access-control-"), "{target}");
    }
    // ルートがある POST は CSRF 対策を通り、空の本文は JSON として解釈できず 400（更新 API は update_api.rs）
    for target in [
        "/api/admin/setflag",
        "/api/admin/setloc",
        "/api/admin/rotate",
        "/api/blog/save",
        "/api/blog/export",
    ] {
        let r = post(target).await;
        assert_eq!(r.status, 400, "POST {target}");
        assert!(!r.headers.contains("access-control-"), "{target}");
    }
    // OPTIONS（CORS プリフライト）は 501 で、CORS ヘッダを一切返さない。クロスオリジンの POST は成立しない
    for method in ["OPTIONS", "PUT", "DELETE", "PATCH", "TRACE", "CONNECT"] {
        for target in ["/api/admin/setflag", "/", "/healthz", "/derivatives/a.jpg"] {
            let r = request(
                addr,
                method,
                target,
                &[
                    ("Host", "localhost"),
                    ("Origin", "http://attacker.example"),
                    ("Access-Control-Request-Method", "POST"),
                    ("Access-Control-Request-Headers", "content-type"),
                ],
            )
            .await;
            assert_eq!(r.status, 501, "{method} {target}");
            assert!(
                !r.headers.contains("access-control-"),
                "{method} {target}: {}",
                r.headers
            );
            assert!(!r.headers.contains("allow:"), "{method} {target}");
        }
    }
    // GET にも CORS ヘッダは付かない
    let r = request(
        addr,
        "GET",
        "/api/v1/photos",
        &[("Host", "localhost"), ("Origin", "http://attacker.example")],
    )
    .await;
    assert_eq!(r.status, 200);
    assert!(!r.headers.contains("access-control-"));
    // 未実装のメソッドは Host 検査より前（現行と同じ順序）
    let r = request(addr, "OPTIONS", "/", &[("Host", "evil.example")]).await;
    assert_eq!(r.status, 501);
}

#[tokio::test]
async fn features_not_yet_in_rust_answer_503_with_a_fixed_json() {
    let site = Site::new();
    let addr = site.start().await;
    // 旅行ブログの読み取り（`/api/blog/`）は T3.5 で実装した（tests/blog_api.rs）。残るのは AI（P4）
    for target in ["/api/ai/unknown", "/api/ai/search?q=x"] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 503, "{target}");
        assert_eq!(
            r.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert_eq!(r.json()["ok"], false);
        // 要求の値（検索語・パス）を返さない
        assert!(
            !r.text().contains("geocode") && !r.text().contains("osm"),
            "{target}"
        );
    }
    // HEAD /api/... は現行でも 403（静的のホワイトリストに当たる）
    assert_eq!(head(addr, "/api/admin/status").await.status, 403);
}

#[tokio::test]
async fn photo_feed_route_keeps_working_behind_the_checks() {
    let site = Site::new();
    let addr = site.start().await;
    for target in ["/api/v1/photos", "/api/v1/photos?x=1"] {
        let r = get(addr, target).await;
        assert_eq!(r.status, 200, "{target}");
        assert!(r.json().is_array());
        assert_eq!(
            r.header("content-type"),
            Some("application/json; charset=utf-8")
        );
    }
    // 生のパスの完全一致だけ（現行と同じ。エンコードした形・末尾のスラッシュは API ではなく静的の 403）
    assert_eq!(get(addr, "/api/v1/%70hotos").await.status, 403);
    assert_eq!(get(addr, "/api/v1/photos/").await.status, 403);
}

#[tokio::test]
async fn large_files_stream_completely() {
    let site = Site::new();
    // チャンク（64KiB）をまたぐ大きさ
    let big: String = (0..200_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    write(&site.data.join("photos/big.jpg"), &big);
    let addr = site.start().await;
    let r = get(addr, "/photos/big.jpg").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body.len(), big.len());
    assert_eq!(r.text(), big);
    assert_eq!(
        head(addr, "/photos/big.jpg").await.header("content-length"),
        Some("200000")
    );
}
