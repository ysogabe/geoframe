//! T1 の fixture（写真サービスの一覧 API の応答の例）を、書き出し側の検査と選び方に通す。
//! 写真サービス側の試験（`crates/geoframe/tests/export_api.rs`）が、同じファイルと実際の応答の一致を確かめている。

mod common;

use common::{FIXTURE, FakeFeed, Reply, fixture_json};
use geoframe_export::date::{Date, Period};
use geoframe_export::error::ErrorCode;
use geoframe_export::feed::FeedClient;
use geoframe_export::listing::{FileState, select};
use serde_json::json;

fn period() -> Period {
    Period::parse(Some("2030-03-10"), Some("2030-03-11")).unwrap()
}

fn date(s: &str) -> Date {
    Date::parse(s).unwrap()
}

#[test]
fn the_fixture_passes_the_checks_and_gives_the_expected_candidates() {
    assert!(std::path::Path::new(FIXTURE).exists());
    let s = select(&fixture_json(), &period()).expect("fixture は検査に通る");

    // 位置なし（9・4）、重複・類似で非推奨（7・11）、表示用 JPEG なし（4）も候補に入る。時刻の昇順、同じ時刻は id の昇順
    let ids: Vec<i64> = s.candidates.iter().map(|c| c.id).collect();
    assert_eq!(ids, [1, 6, 7, 8, 9, 10, 11, 4]);
    assert_eq!(s.candidate_count(), 8);
    // カタログ全体の件数を引き継ぐ
    assert_eq!((s.undated, s.invalid_time), (2, 3));
    // 作成待ちは id 4 の 1 枚。dry_run は件数で返し、本番の書き出しは derivative_pending で失敗する
    assert_eq!(s.pending_count(), 1);
    assert_eq!(s.candidates.last().unwrap().file, FileState::Pending);
    assert!(s.exclusions().is_empty());
    assert_eq!(s.ready().count(), 7);
    assert_eq!(
        s.clone().require_no_pending().unwrap_err().code,
        ErrorCode::DerivativePending
    );

    // 日ごとの振り分け: 3/10 に 7 枚、3/11 に 1 枚（23:59 の位置なし・作成待ちの写真）
    let days = s.by_day();
    assert_eq!(days.len(), 2);
    let first: Vec<i64> = days[&date("2030-03-10")].iter().map(|c| c.id).collect();
    assert_eq!(first, [1, 6, 7, 8, 9, 10, 11]);
    assert_eq!(days[&date("2030-03-11")].len(), 1);
    assert_eq!(
        days[&date("2030-03-11")][0].time.to_string(),
        "2030-03-11 23:59"
    );
    // 00:00 の境界
    assert_eq!(
        days[&date("2030-03-10")][0].time.to_string(),
        "2030-03-10 00:00"
    );

    // 値は一覧 API どおり（丸めない・null も保つ）
    let by_id = |id: i64| s.candidates.iter().find(|c| c.id == id).unwrap();
    assert_eq!(by_id(1).elev, Some(100.5));
    assert_eq!((by_id(1).lat, by_id(1).lon), (Some(12.5), Some(34.5)));
    assert_eq!(by_id(1).title, "架空の朝");
    assert!(by_id(8).inferred);
    assert_eq!(
        (by_id(9).lat, by_id(9).lon, by_id(9).inferred),
        (None, None, false)
    );
    assert_eq!(by_id(6).elev, None);
}

#[test]
fn once_the_pending_photo_is_ready_the_export_can_go_on() {
    let mut v = fixture_json();
    let photos = v["photos"].as_array_mut().unwrap();
    let pending = photos.iter_mut().find(|p| p["id"] == 4).unwrap();
    pending["file"] = json!("derivatives/p4.jpg");
    let s = select(&v, &period()).unwrap().require_no_pending().unwrap();
    assert_eq!(s.pending_count(), 0);
    assert_eq!(s.ready().count(), 8);
}

#[test]
fn the_fixture_is_refused_for_another_period() {
    // 応答の from・to が要求と違う（別の期間の応答を取り違えない）
    let other = Period::parse(Some("2030-03-10"), None).unwrap();
    assert_eq!(
        select(&fixture_json(), &other).unwrap_err().code,
        ErrorCode::FeedInvalid
    );
}

#[test]
fn the_fixture_round_trips_through_the_http_client() {
    let body = std::fs::read(FIXTURE).unwrap();
    let feed = FakeFeed::start(Reply::json(body));
    let s = FeedClient::new(&feed.base).select(&period()).unwrap();
    assert_eq!(s.candidate_count(), 8);
    assert_eq!((s.undated, s.invalid_time, s.pending_count()), (2, 3, 1));
    assert_eq!(
        feed.request_lines(),
        ["GET /api/admin/export/photos?from=2030-03-10&to=2030-03-11 HTTP/1.1"]
    );
}
