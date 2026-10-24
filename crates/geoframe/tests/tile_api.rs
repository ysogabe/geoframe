//! 地図タイル（E11 `GET/HEAD /tiles/…`）の結合テスト。上流（OSM・地理院）はループバックのスタブ
//! （`common::TileStub`）で、外へは出ない。Python との細かな一致は `src/tiles/tests.rs` のフィクスチャが見る。
//! ここは、ルーター全体（Host 検査・メソッドの扱い・CORS・取り込み無効）を通した振る舞い。

mod common;

use std::sync::Arc;

use common::{
    SAMPLE_PNG, TempDir, TileStub, build_empty_catalog, get, request, with_tile_endpoints,
};
use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::server::router;

struct Site {
    dir: TempDir,
    stub: TileStub,
    addr: std::net::SocketAddr,
    _conn: rusqlite::Connection,
}

impl Site {
    async fn start() -> Self {
        let dir = TempDir::new("tile-api");
        let (path, conn) = build_empty_catalog(dir.path(), "");
        let config = Config::load(
            |_| None,
            &Overrides {
                data_dir: Some(dir.path().to_path_buf()),
                ..Overrides::default()
            },
        )
        .unwrap();
        let stub = TileStub::start();
        let state = common::test_state(Arc::new(config), Catalog::open(&path).unwrap());
        let state = with_tile_endpoints(state, stub.endpoints());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        Self {
            dir,
            stub,
            addr,
            _conn: conn,
        }
    }
}

#[tokio::test]
async fn a_tile_is_fetched_once_cached_and_served_with_the_documented_headers() {
    let site = Site::start().await;
    let r = get(site.addr, "/tiles/osm/3/1/2.png").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, SAMPLE_PNG);
    assert_eq!(r.header("content-type"), Some("image/png"));
    assert_eq!(
        r.header("content-length"),
        Some(SAMPLE_PNG.len().to_string().as_str())
    );
    assert_eq!(r.header("cache-control"), Some("public, max-age=86400"));
    assert_eq!(r.header("x-tile-cache"), Some("miss"));
    assert_eq!(site.stub.seen(), ["/osm/3/1/2.png"]);
    assert_eq!(
        std::fs::read(site.dir.path().join("tile_cache/osm/3/1/2.png")).unwrap(),
        SAMPLE_PNG
    );

    let r = get(site.addr, "/tiles/osm/3/1/2.png").await;
    assert_eq!(r.header("x-tile-cache"), Some("hit"));
    assert_eq!(r.body, SAMPLE_PNG);
    assert_eq!(site.stub.seen().len(), 1, "ヒットは上流へ行かない");

    // HEAD は同じヘッダで本文なし
    let r = request(
        site.addr,
        "HEAD",
        "/tiles/osm/3/1/2.png",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(r.status, 200);
    assert!(r.body.is_empty());
    assert_eq!(r.header("x-tile-cache"), Some("hit"));
    assert_eq!(
        r.header("content-length"),
        Some(SAMPLE_PNG.len().to_string().as_str())
    );
    // CORS のヘッダは付かない
    assert!(!r.headers.contains("access-control-"), "{}", r.headers);
}

#[tokio::test]
async fn upstream_failures_are_a_200_transparent_tile_that_is_not_cached() {
    let site = Site::start().await;
    site.stub.set_status(Some(503));
    let r = get(site.addr, "/tiles/gsi/5/3/4.png").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("x-tile-cache"), Some("error"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.body, geoframe::tiles::TRANSPARENT_TILE);
    assert!(!site.dir.path().join("tile_cache/gsi/5/3/4.png").exists());
    // 回復すれば取れる
    site.stub.set_status(None);
    let r = get(site.addr, "/tiles/gsi/5/3/4.png").await;
    assert_eq!(r.header("x-tile-cache"), Some("miss"));
}

#[tokio::test]
async fn invalid_or_out_of_range_tiles_are_404_without_touching_the_upstream() {
    let site = Site::start().await;
    for target in [
        "/tiles/osm/20/0/0.png",
        "/tiles/gsi/19/0/0.png",
        "/tiles/osm/3/8/0.png",
        "/tiles/osm/3/0/0",
        "/tiles/xyz/3/0/0.png",
        "/tiles/",
    ] {
        let r = get(site.addr, target).await;
        assert_eq!(r.status, 404, "{target}");
        assert!(r.text().contains("Message: Not Found."), "{target}");
        assert!(r.header("x-tile-cache").is_none(), "{target}");
    }
    assert!(site.stub.seen().is_empty());
}

#[tokio::test]
async fn the_usual_checks_run_before_the_tile_route() {
    let site = Site::start().await;
    // Host 検査
    let r = request(
        site.addr,
        "GET",
        "/tiles/osm/3/1/2.png",
        &[("Host", "evil.example")],
    )
    .await;
    assert_eq!(r.status, 403);
    // 絶対形式の request-target
    let r = request(
        site.addr,
        "GET",
        "http://evil.example/tiles/osm/3/1/2.png",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(r.status, 403);
    // POST はルートが無いので 403、OPTIONS は 501（CORS のヘッダは付かない）
    let r = request(
        site.addr,
        "POST",
        "/tiles/osm/3/1/2.png",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(r.status, 403);
    let r = request(
        site.addr,
        "OPTIONS",
        "/tiles/osm/3/1/2.png",
        &[("Host", "localhost")],
    )
    .await;
    assert_eq!(r.status, 501);
    assert!(!r.headers.contains("access-control-"));
    assert!(site.stub.seen().is_empty());
}
