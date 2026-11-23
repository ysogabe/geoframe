//! T5: ジョブの完了ログ（design.md 7 章）。`tracing` の呼び出し箇所は、受け口の無いスレッドが先に通ると「関心なし」を覚えられて、
//! あとから付けた受け口に届かないことがある。ほかの試験とスレッドを共有しないよう、この試験だけで 1 つのファイルにしている。
//! 偽の写真サービス（127.0.0.1）と偽の Drive（HTTP サーバー）だけを使う。

mod common;

use common::drive::*;
use common::site::*;
use geoframe_export::jobs::Status;

const DAY: &str = "2030-03-10";

/// 1 件のイベントのフィールド（名前と、文字列にした値）。
type Event = Vec<(String, String)>;

/// イベントのフィールドを集める `tracing` の受け口（試験用）。
#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<Event>>>);

struct Fields<'a>(&'a mut Vec<(String, String)>);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_owned(), value.to_owned()));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.push((field.name().to_owned(), value.to_string()));
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Vec::new();
        event.record(&mut Fields(&mut fields));
        self.0.lock().unwrap().push(fields);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn field<'a>(event: &'a [(String, String)], name: &str) -> &'a str {
    event
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("{name} が無い: {event:?}"))
}

/// ジョブの完了ログ: ジョブ ID・期間・宛先・枚数・経過時間・status・error の code。ほかの値（トークン・Drive の応答）は載せない。
/// `run`（前面）でも、`submit`（`serve` が使う別のスレッド）でも出る。
#[test]
fn a_finished_job_logs_one_structured_line_with_the_documented_fields() {
    let mut env = Env::new("flow-log");
    let site = photo_site(
        DAY,
        "2030-03-11",
        vec![row(1, "2030-03-10 09:00"), row(2, "2030-03-11 08:00")],
    );
    let svc = service(&mut env, &site);
    let capture = Capture::default();
    let req = request(DAY, Some("2030-03-11"), None, None);
    let rec = tracing::subscriber::with_default(capture.clone(), || {
        run_foreground_retry(&svc, &req).unwrap()
    });
    let events = capture.0.lock().unwrap().clone();
    let done: Vec<_> = events
        .iter()
        .filter(|e| field(e, "message").contains("終わりました"))
        .collect();
    assert_eq!(done.len(), 1, "{events:?}");
    let e = done[0];
    assert_eq!(field(e, "job"), rec.job_id.as_str());
    assert_eq!(field(e, "from"), "2030-03-10");
    assert_eq!(field(e, "to"), "2030-03-11");
    assert_eq!(field(e, "destination"), "yamareco");
    assert_eq!(field(e, "count"), "2");
    assert_eq!(field(e, "status"), "succeeded");
    assert_eq!(field(e, "error"), "none");
    assert!(field(e, "elapsed_secs").parse::<u64>().is_ok());
    let mut names: Vec<&str> = e.iter().map(|(k, _)| k.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "count",
            "destination",
            "elapsed_secs",
            "error",
            "from",
            "job",
            "message",
            "status",
            "to"
        ],
        "載せるのはこれだけ"
    );

    // 失敗では error の code が出る（枚数は 0）
    let mut env2 = Env::new("flow-log-fail");
    let site2 = photo_site(DAY, DAY, vec![pending_row(1, "2030-03-10 09:00")]);
    let svc2 = service(&mut env2, &site2);
    let capture2 = Capture::default();
    let rec2 = tracing::subscriber::with_default(capture2.clone(), || {
        run_foreground_retry(&svc2, &day()).unwrap()
    });
    assert_eq!(rec2.status, Status::Failed);
    let events = capture2.0.lock().unwrap().clone();
    let e = events
        .iter()
        .find(|e| field(e, "message").contains("終わりました"))
        .expect("完了ログ");
    assert_eq!(field(e, "status"), "failed");
    assert_eq!(field(e, "error"), "derivative_pending");
    assert_eq!(field(e, "count"), "0");
}
