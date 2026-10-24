//! ログの方針と初期化。
//!
//! ログは標準出力へ1行1イベントで出し、systemd の journal が拾う（時刻は journald が付けるので
//! 出さない。色も付けない）。詳しい方針は docs/rust-migration/server.md の「ログ」。
//!
//! # ログに出してよいもの
//!
//! - リクエストログ（[`crate::server`] の `access_log`）: **メソッド・ルートのテンプレート
//!   （`/derivatives/{*path}` のようにパラメータを伏せた形）・状態コード・所要時間（ミリ秒）**
//!   だけ。
//! - 起動・停止、待ち受けアドレス、データ置き場、スキーマ不足の**列名**、件数、状態名。
//! - エラー: SQLite などが返すメッセージ。ただし HTTP 応答には載せない（応答は定型文）。
//!
//! # ログに出してはいけないもの
//!
//! - 写真の内容、EXIF・GPS、緯度経度、地名
//! - ファイル名・パス（Drive ID を含む）。実際のリクエストのパスとクエリ（検索語・ファイル名が
//!   入りうる）。テンプレートに当てはまらないリクエストは `(unmatched)` とだけ出す
//! - 認証情報・トークン・鍵ファイルの中身、`Authorization` などのヘッダ値
//! - 接続元アドレス、`Host` ヘッダの値
//!
//! 新しいログを足すときは、上の「出してよいもの」に当てはまるかを先に確かめる。

use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

use crate::config::LogLevel;

/// ログの購読側を作る。**GEOFRAME 自身のログだけ**を `level` 以上で出し、依存クレート（axum・
/// hyper など）のログは全部捨てる。依存のログには接続元アドレスなどが入りうる
/// （例: axum は TRACE で `connection 127.0.0.1:… accepted` を出す）ため。
pub fn subscriber<W>(level: LogLevel, writer: W) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .without_time()
        .with_target(false);
    let filter = Targets::new()
        .with_default(LevelFilter::OFF)
        .with_target("geoframe", level.as_tracing());
    tracing_subscriber::registry().with(fmt.with_filter(filter))
}

/// 標準出力へのログを有効にする。プロセスで1回だけ呼ぶ。
pub fn init(level: LogLevel) {
    tracing::subscriber::set_global_default(subscriber(level, std::io::stdout))
        .expect("ログの初期化は1回だけです");
}
