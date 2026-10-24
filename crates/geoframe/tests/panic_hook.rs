//! パニックのログに、場所（ファイルと行）だけが出て、メッセージの値が出ないこと（AC7）。
//!
//! 単独のテストファイルにしてある。パニックフックはプロセスに1つで、購読者もスレッドをまたいで
//! 効かせるために `set_global_default`（1プロセス1回）を使うので、他のテストと同じプロセスに
//! 置くと互いに干渉する。

use std::io;
use std::sync::{Arc, Mutex};

use geoframe::config::LogLevel;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Captured {
        self.clone()
    }
}

#[test]
fn panic_log_has_the_location_but_not_the_message() {
    let captured = Captured::default();
    tracing::subscriber::set_global_default(geoframe::logging::subscriber(
        LogLevel::Info,
        captured.clone(),
    ))
    .unwrap();
    geoframe::logging::install_panic_hook();

    let secret = "IMG_secret-name.jpg";
    let handle = std::thread::spawn(move || {
        panic!("切り出しに失敗: {secret}");
    });
    assert!(handle.join().is_err());

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("ERROR"), "{log}");
    assert!(log.contains("パニックしました"), "{log}");
    assert!(log.contains("panic_hook.rs:"), "場所が出ていない:\n{log}");
    assert!(!log.contains("secret-name"), "メッセージが出ている:\n{log}");
    assert!(!log.contains("切り出し"), "メッセージが出ている:\n{log}");
}
