//! 廃止した旅行ブログのパス（`/blog-admin`・`/api/blog/*`）の結合テスト。合成データだけを使う。
//!
//! 旅行ブログの機能は削除した（書き出しは `geoframe-export` が引き継いだ）。削除の前は本番で `GEOFRAME_BLOG=0`（無効）
//! にしていたので、そのときの外から見える応答を変えないように、次の応答だけを残している:
//!
//! - `GET /api/blog/*`: 何を指しても 404（`Nothing matches the given URI`。HTML）
//! - `POST /api/blog/save`・`/api/blog/export`: 404（JSON `Not Found`）。CSRF の 403・`GEOFRAME_INGEST` が無いときの
//!   503 より先（優先順位は 404 ＞ 403 ＞ 503）。ほかの `POST /api/blog/*` はルートが無いので 403
//! - `/blog-admin`: 管理画面の HTML（200）。`static_files.rs` と、ログのラベルは `access_log.rs`
//!
//! 環境に `GEOFRAME_BLOG`・`GEOFRAME_BLOG_PLANS_DIR` が残っていても、読まずに無視するので応答は変わらない
//! （設定側の確認は `config.rs` の試験。起動は `ingest_cli.rs`）。

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
    let dir = TempDir::new("retired-blog");
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

/// 削除の前から環境に残っているかもしれない値（無効・有効・不正な値・旅程の置き場）。どれでも応答は同じ。
const LEFTOVER_ENVS: [&[(&str, &str)]; 5] = [
    &[],
    &[("GEOFRAME_BLOG", "0")],
    &[("GEOFRAME_BLOG", "1")],
    &[("GEOFRAME_BLOG", "yes")],
    &[
        ("GEOFRAME_BLOG", "0"),
        ("GEOFRAME_BLOG_PLANS_DIR", "/nonexistent/plans"),
    ],
];

#[tokio::test]
async fn reads_of_api_blog_are_404_whatever_the_path_and_whatever_is_left_in_the_environment() {
    for env in LEFTOVER_ENVS {
        let (_dir, _db, addr) = start(env).await;
        for target in GET_TARGETS {
            let r = get(addr, target).await;
            assert_eq!(r.status, 404, "{env:?} {target}");
            assert_eq!(r.header("content-type"), Some("text/html;charset=utf-8"));
            let body = String::from_utf8_lossy(&r.body);
            assert!(
                body.contains("Nothing matches the given URI"),
                "{env:?} {target}: {body}"
            );
            assert_no_reason(&r, target);
        }
    }
}

#[tokio::test]
async fn writes_to_save_and_export_are_404_even_with_a_valid_request() {
    for env in LEFTOVER_ENVS {
        let (_dir, _db, addr) = start(env).await;
        for target in ["/api/blog/save", "/api/blog/export"] {
            // 正しい JSON・正しい Origin
            let r = post(addr, target, "application/json", "{}").await;
            assert_eq!(r.status, 404, "{env:?} {target}");
            assert_eq!(
                r.header("content-type"),
                Some("application/json; charset=utf-8")
            );
            assert_eq!(
                r.json(),
                serde_json::json!({"ok": false, "error": "Not Found"}),
                "{env:?} {target}"
            );
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
            // 本文が空・JSON でなくても 404（本文は読まれない）
            let r = post(addr, target, "application/json", "not json").await;
            assert_eq!(r.status, 404, "{target} not json");
        }
        // ルートの無い `/api/blog/` への POST は今までどおり 403
        for target in ["/api/blog/nope", "/api/blog/trips", "/api/blog/"] {
            let r = post(addr, target, "application/json", "{}").await;
            assert_eq!(r.status, 403, "{env:?} {target}");
        }
    }
}

#[tokio::test]
async fn blog_admin_is_the_admin_page() {
    // ページの中身とヘッダの比較は `static_files.rs`。ここでは環境に値が残っていても管理画面（200）を返すことだけ
    let (_dir, _db, addr) = start(&[]).await;
    let admin = get(addr, "/admin").await;
    assert_eq!(admin.status, 200);
    for env in LEFTOVER_ENVS {
        let (_dir, _db, addr) = start(env).await;
        let r = get(addr, "/blog-admin").await;
        assert_eq!(r.status, 200, "{env:?}");
        assert_eq!(r.body, admin.body, "{env:?}");
    }
}
