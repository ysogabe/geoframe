//! ログの初期化。標準エラーへ 1 行 1 イベントで出し、systemd の journal が拾う（時刻は journald が付けるので出さない。色も
//! 付けない）。標準出力は、コマンドの JSON 専用。`crates/geoframe` の `logging.rs` と同じ作り（同じ版・同じ feature）。
//!
//! 出すのは、このクレート自身のログだけ（依存クレートのログは全部捨てる）。出してよいもの: ジョブの完了（ジョブ ID・期間・宛先・
//! 枚数・経過時間・status・error の code）、rclone の失敗（コマンド名・失敗の種類・伏せ字にした標準エラーの末尾）。トークン・
//! 資格情報・Drive の ID・rclone の標準出力は出さない。

use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

/// ログの購読側を作る。`writer` は、出すたびに（ロックを持ち続けずに）書く先を得る形。
pub fn subscriber<W>(writer: W) -> impl tracing::Subscriber + Send + Sync
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
        .with_target("geoframe_export", LevelFilter::INFO);
    tracing_subscriber::registry().with(fmt.with_filter(filter))
}

/// 標準エラーへのログを有効にする。プロセスで 1 回だけ呼ぶ（バイナリの `main`）。すでに入っていれば何もしない。
/// `std::io::stderr` は出すたびにロックを取って放す（コマンドの実行中ずっとは持たない）。
pub fn init() {
    let _ = tracing::subscriber::set_global_default(subscriber(std::io::stderr));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }

    #[test]
    fn this_crates_info_logs_are_written_without_time_and_debug_is_dropped() {
        let buf = Buf::default();
        tracing::subscriber::with_default(subscriber(buf.clone()), || {
            tracing::info!(count = 3, "書き出しのジョブが終わりました");
            tracing::debug!("出さない");
        });
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(text.contains("書き出しのジョブが終わりました"), "{text}");
        assert!(text.contains("count=3"), "{text}");
        assert!(!text.contains("出さない"), "{text}");
        assert_eq!(text.lines().count(), 1, "{text}");
    }
}
