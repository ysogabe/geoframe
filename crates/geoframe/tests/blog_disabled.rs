//! 旅行ブログの無効化（`GEOFRAME_BLOG`。既定は無効）の結合テスト。合成データだけを使う。
//!
//! 無効のとき、`GET /api/blog/*` と `POST /api/blog/save`・`export` は、理由を言わずに 404 を返す
//! （優先順位は 404 ＞ CSRF の 403 ＞ `GEOFRAME_INGEST` が無いときの 503）。有効のときの動きは
//! `blog_api.rs`・`blog_write_api.rs`。`/blog-admin` のページは `static_files.rs`。

mod common;

use common::{TempDir, build_admin_catalog, get, post_raw, spawn_with_env};
use geoframe::db::Catalog;

const GET_TARGETS: [&str; 6] = [
    "/api/blog/trips",
    "/api/blog/trip?trip_id=x",
    "/api/blog/candidates?trip_id=x&day=1",
    "/api/blog/draft?trip_id=x",
    "/api/blog/other",
    "/api/blog/",
];

async fn start(env: &[(&str, &str)]) -> (TempDir, rusqlite::Connection, std::net::SocketAddr) {
    let dir = TempDir::new("blog-disabled");
    let db = build_admin_catalog(dir.path());
    let catalog = Catalog::open(&dir.path().join("catalog.db")).unwrap();
    let addr = spawn_with_env(dir.path(), env, catalog).await;
    (dir, db, addr)
}

async fn post(
    addr: std::net::SocketAddr,
    target: &str,
    content_type: &str,
    body: &str,
) -> common::Response {
    post_raw(
        addr,
        target,
        &[
            ("Host".into(), "localhost".into()),
            ("Origin".into(), "http://localhost".into()),
            ("Content-Type".into(), content_type.into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ],
        body.as_bytes(),
    )
    .await
}

/// 404 の本文に、無効にしている理由（フラグの名前・「無効」）を書かない。
fn assert_no_reason(response: &common::Response, what: &str) {
    let body = String::from_utf8_lossy(&response.body).to_lowercase();
    for word in ["geoframe_blog", "blog=", "無効", "disabled", "disable"] {
        assert!(!body.contains(word), "{what}: {body}");
    }
}

#[tokio::test]
async fn reads_are_404_when_the_blog_is_not_enabled() {
    for env in [&[][..], &[("GEOFRAME_BLOG", "0")][..]] {
        let (_dir, _db, addr) = start(env).await;
        for target in GET_TARGETS {
            let r = get(addr, target).await;
            assert_eq!(r.status, 404, "{env:?} {target}");
            assert_eq!(r.header("content-type"), Some("text/html;charset=utf-8"));
            assert_no_reason(&r, target);
        }
    }
}

#[tokio::test]
async fn writes_are_404_when_the_blog_is_not_enabled_even_with_a_valid_request() {
    let (_dir, _db, addr) = start(&[]).await;
    for target in ["/api/blog/save", "/api/blog/export"] {
        // 正しい JSON・正しい Origin
        let r = post(addr, target, "application/json", "{}").await;
        assert_eq!(r.status, 404, "{target}");
        assert_eq!(
            r.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(r.json()["ok"], false, "{target}");
        assert_no_reason(&r, target);
        // CSRF の違反（Content-Type 違い）でも 403 ではなく 404
        let r = post(addr, target, "text/plain", "{}").await;
        assert_eq!(r.status, 404, "{target} text/plain");
        assert_no_reason(&r, target);
        // 異なる Origin でも 404（理由を出さない）
        let body = "{}";
        let r = post_raw(
            addr,
            target,
            &[
                ("Host".into(), "localhost".into()),
                ("Origin".into(), "http://attacker.example".into()),
                ("Content-Type".into(), "application/json".into()),
                ("Content-Length".into(), body.len().to_string()),
                ("Connection".into(), "close".into()),
            ],
            body.as_bytes(),
        )
        .await;
        assert_eq!(r.status, 404, "{target} cross-origin");
    }
    // ルートの無い `/api/blog/` への POST は今までどおり 403
    let r = post(addr, "/api/blog/nope", "application/json", "{}").await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn the_not_enabled_404_comes_before_the_ingest_503_and_the_enabled_server_still_answers() {
    // 有効にして取り込みが無い（writer が無い）とき、CSRF の違反は 403、正しい要求は 404 ではない
    let (_dir, _db, addr) = start(&[("GEOFRAME_BLOG", "1")]).await;
    for target in ["/api/blog/save", "/api/blog/export"] {
        let r = post(addr, target, "text/plain", "{}").await;
        assert_eq!(r.status, 403, "{target}");
        let r = post(addr, target, "application/json", "{}").await;
        assert_ne!(r.status, 404, "{target}");
    }
    for target in ["/api/blog/trips", "/api/blog/trip?trip_id=Secret_Bad"] {
        assert_ne!(get(addr, target).await.status, 404, "{target}");
    }
    // 取り込みが無い無効の側は、同じ要求が 404（503 にならない）
    let (_dir, _db, addr) = start(&[]).await;
    let r = post(addr, "/api/blog/save", "application/json", "{}").await;
    assert_eq!(r.status, 404);
}
