//! 一覧 API の取得（design.md 4.1 の手順 2、8 章「上限」）。偽の写真サービス（127.0.0.1）だけに接続する。

mod common;

use std::time::{Duration, Instant};

use common::{Body, FakeFeed, Reply, closed_port_base, fixture_json};
use geoframe_export::date::Period;
use geoframe_export::error::ErrorCode;
use geoframe_export::feed::{FeedClient, MAX_BODY_BYTES, TIMEOUT};

fn period() -> Period {
    Period::parse(Some("2030-03-10"), Some("2030-03-11")).unwrap()
}

fn code(r: Result<impl std::fmt::Debug, geoframe_export::error::ExportError>) -> ErrorCode {
    r.expect_err("失敗するはず").code
}

#[test]
fn production_limits_are_30_seconds_and_8_megabytes() {
    assert_eq!(TIMEOUT, Duration::from_secs(30));
    assert_eq!(MAX_BODY_BYTES, 8 * 1024 * 1024);
}

#[test]
fn a_200_response_is_parsed_and_the_request_line_is_exact() {
    let feed = FakeFeed::start(Reply::json(serde_json::to_vec(&fixture_json()).unwrap()));
    let v = FeedClient::new(&feed.base).fetch(&period()).unwrap();
    assert_eq!(v["photos"].as_array().unwrap().len(), 8);
    assert_eq!(
        feed.request_lines(),
        ["GET /api/admin/export/photos?from=2030-03-10&to=2030-03-11 HTTP/1.1"]
    );
}

#[test]
fn anything_but_200_is_feed_unavailable() {
    for status in [201, 204, 400, 403, 404, 500, 503] {
        let feed = FakeFeed::start(Reply::status(status));
        assert_eq!(
            code(FeedClient::new(&feed.base).fetch(&period())),
            ErrorCode::FeedUnavailable,
            "{status}"
        );
    }
}

#[test]
fn a_refused_connection_is_feed_unavailable() {
    assert_eq!(
        code(FeedClient::new(&closed_port_base()).fetch(&period())),
        ErrorCode::FeedUnavailable
    );
}

#[test]
fn redirects_are_not_followed() {
    for status in [301, 302, 303, 307, 308] {
        let target = FakeFeed::start(Reply::json(serde_json::to_vec(&fixture_json()).unwrap()));
        let location = format!(
            "http://{}/api/admin/export/photos?from=2030-03-10&to=2030-03-11",
            target.addr
        );
        let feed = FakeFeed::start(Reply::redirect(status, &location));
        assert_eq!(
            code(FeedClient::new(&feed.base).fetch(&period())),
            ErrorCode::FeedUnavailable,
            "{status}"
        );
        // 失敗が返ったあとも、少し待って、行き先には 1 度も接続していないことを確かめる
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(feed.connections(), 1, "{status}");
        assert_eq!(
            target.connections(),
            0,
            "{status}: リダイレクトの行き先に接続した"
        );
    }
}

#[test]
fn a_silent_server_hits_the_timeout() {
    let feed = FakeFeed::start(Reply::Stall(Duration::from_secs(3)));
    let client = FeedClient::with_limits(&feed.base, Duration::from_millis(300), MAX_BODY_BYTES);
    let started = Instant::now();
    assert_eq!(code(client.fetch(&period())), ErrorCode::FeedUnavailable);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_body_that_stalls_midway_hits_the_timeout() {
    // 先頭だけ送って止まる（本文の読み取りにも期限が掛かる）
    let feed = FakeFeed::start(Reply::Raw {
        head: "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n{\""
            .to_owned(),
        body: Body::Bytes(Vec::new()),
    });
    let client = FeedClient::with_limits(&feed.base, Duration::from_millis(500), MAX_BODY_BYTES);
    let started = Instant::now();
    let e = code(client.fetch(&period()));
    assert_eq!(e, ErrorCode::FeedUnavailable);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_body_that_is_not_json_is_feed_invalid() {
    for body in [&b"not json"[..], b"", b"<html></html>", b"{\"photos\":"] {
        let feed = FakeFeed::start(Reply::json(body.to_vec()));
        assert_eq!(
            code(FeedClient::new(&feed.base).fetch(&period())),
            ErrorCode::FeedInvalid,
            "{:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn select_applies_the_value_checks_to_the_response() {
    let mut v = fixture_json();
    v["photos"][0]["inferred"] = serde_json::json!("yes");
    let feed = FakeFeed::start(Reply::json(serde_json::to_vec(&v).unwrap()));
    assert_eq!(
        code(FeedClient::new(&feed.base).select(&period())),
        ErrorCode::FeedInvalid
    );

    let mut v = fixture_json();
    v["truncated"] = serde_json::json!(true);
    let feed = FakeFeed::start(Reply::json(serde_json::to_vec(&v).unwrap()));
    assert_eq!(
        code(FeedClient::new(&feed.base).select(&period())),
        ErrorCode::TooManyPhotos
    );
}

/// 先頭に有効な JSON を置き、残りを空白で埋めた、ちょうど `len` バイトの応答の本文。
fn padded_json(len: usize) -> Vec<u8> {
    let mut body = serde_json::to_vec(&fixture_json()).unwrap();
    assert!(body.len() < len);
    body.resize(len, b' ');
    body
}

#[test]
fn a_body_of_exactly_8_megabytes_is_accepted_and_one_more_byte_is_refused() {
    let max = MAX_BODY_BYTES as usize;
    let ok = FakeFeed::start(Reply::json(padded_json(max)));
    let v = FeedClient::new(&ok.base)
        .fetch(&period())
        .expect("ちょうど 8MB は通る");
    assert_eq!(v["photos"].as_array().unwrap().len(), 8);

    let over = FakeFeed::start(Reply::json(padded_json(max + 1)));
    assert_eq!(
        code(FeedClient::new(&over.base).fetch(&period())),
        ErrorCode::FeedInvalid
    );
}

#[test]
fn the_limit_counts_bytes_read_not_the_content_length() {
    // Content-Length が小さくても、無いときでも、大きく宣言しても、読んだバイト数で止まる。
    // 上限は試験用に小さくする（本番の 8MB の境界は上の試験）
    let limit = 4096u64;
    let heads = [
        // 宣言なし（接続を閉じるまでが本文）
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned(),
        // 実際より遥かに大きな宣言
        "HTTP/1.1 200 OK\r\nContent-Length: 4294967296\r\nConnection: close\r\n\r\n".to_owned(),
        // 上限より少し大きな宣言（宣言は正しい）
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            limit + 1
        ),
    ];
    for head in heads {
        let feed = FakeFeed::start(Reply::Raw {
            head: head.clone(),
            body: Body::Filler {
                len: 16 * 1024 * 1024,
                byte: b' ',
            },
        });
        let client = FeedClient::with_limits(&feed.base, Duration::from_secs(10), limit);
        let started = Instant::now();
        assert_eq!(
            code(client.fetch(&period())),
            ErrorCode::FeedInvalid,
            "{head}"
        );
        // 全部を読み切らずに止まる
        assert!(started.elapsed() < Duration::from_secs(5), "{head}");
    }
}

#[test]
fn a_gzip_labelled_body_is_not_trusted_either() {
    // gzip の機能は入れていない。Content-Encoding を付けて返されても、読んだバイト列を JSON として読めなければ invalid
    let feed = FakeFeed::start(Reply::Raw {
        head: "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_owned(),
        body: Body::Bytes(vec![0x1f, 0x8b, 0x08, 0x00]),
    });
    assert_eq!(
        code(FeedClient::new(&feed.base).fetch(&period())),
        ErrorCode::FeedInvalid
    );
}
