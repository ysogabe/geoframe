//! `GET /api/admin/geocode`（E4。T3.3）の結合テスト。地理院・Nominatim はループバックのスタブ
//! （`common::GeocodeStub`）で、外へは出ない。座標・地名・検索語は合成の値。
//!
//! 結合テストの既定の接続先（`common::app_state` など）は、何も受け付けないスタブ。2 文字以上の検索語で
//! 通信が要る試験は、ここのように自分のスタブを作って `with_geocoder` で向ける。

mod common;

use common::{GeocodeStub, TempDir, Upstream, build_empty_catalog, get, test_state};
use geoframe::config::{Config, Overrides};
use geoframe::db::Catalog;
use geoframe::server::router;
use serde_json::json;
use std::sync::Arc;

async fn start(stub: &GeocodeStub, tmp: &TempDir) -> (std::net::SocketAddr, rusqlite::Connection) {
    let (path, writer) = build_empty_catalog(tmp.path(), "");
    let config = Config::load(|_| None, &Overrides::default()).unwrap();
    let state =
        test_state(Arc::new(config), Catalog::open(&path).unwrap()).with_geocoder(stub.geocoder());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(state)).await });
    // 書き込み用の接続は WAL を保つために、試験の終わりまで呼び出し側が持つ
    (addr, writer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_search_of_two_or_more_characters_reaches_the_stub_only() {
    let tmp = TempDir::new("geocode-api");
    let stub = GeocodeStub::start();
    stub.set(
        Upstream::Gsi,
        r#"[{"geometry":{"coordinates":[139.5,35.5]},"properties":{"title":"合成の地名"}}]"#,
    );
    let (addr, _writer) = start(&stub, &tmp).await;

    let response = get(addr, "/api/admin/geocode?q=ab").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({"ok": true, "items": [
            {"title": "合成の地名", "lat": 35.5, "lon": 139.5, "source": "gsi"}
        ]})
    );
    // 要求はスタブに届いた（地理院に 1 回。結果があったので Nominatim へは回らない）
    assert_eq!(stub.seen(), vec![(Upstream::Gsi, "q=ab".to_string())]);
    stub.assert_no_unexpected();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nominatim_is_asked_only_when_the_gsi_has_nothing_and_the_answer_is_cached() {
    let tmp = TempDir::new("geocode-api-osm");
    let stub = GeocodeStub::start();
    stub.set(Upstream::Gsi, "[]");
    stub.set(
        Upstream::Osm,
        r#"[{"lat":"35.5","lon":"139.5","display_name":"合成の地名"}]"#,
    );
    let (addr, _writer) = start(&stub, &tmp).await;

    for _ in 0..2 {
        let response = get(addr, "/api/admin/geocode?q=cd").await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.json()["items"],
            json!([{"title": "合成の地名", "lat": 35.5, "lon": 139.5, "source": "osm"}])
        );
    }
    // 2 回目は Nominatim のキャッシュに当たる（地理院は毎回引く）
    let osm = stub
        .seen()
        .iter()
        .filter(|(upstream, _)| *upstream == Upstream::Osm)
        .count();
    assert_eq!(osm, 1);
    stub.assert_no_unexpected();
}
