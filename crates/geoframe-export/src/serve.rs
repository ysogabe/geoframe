//! T6: `serve`（bind の検査、トークン、送信元の許可、本文の検査、接続の上限、ジョブの API。design.md 3.4・6.1）。
//!
//! T2 では「未実装」を返すだけ。
//!
//! T6 が守ること（キャンセルと終わりの待ち。[`crate::service`] の「キャンセル」）: `cancel` は、バイナリの `main` が signal の
//! handler と共有している印。[`crate::service::Service::with_cancel`] に渡す（tokio のシグナルを使うなら、`main` の handler の
//! 代わりに、受けたときに [`Cancel::cancel`] を呼ぶ）。印が立ったら、新しい受付を止め、[`crate::service::Service::wait_idle`] で
//! 実行中のジョブが（rclone を止めて `wait` し終えて）終わるまで待ってから、プロセスを終える。

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use crate::app::NotImplemented;
use crate::cancel::Cancel;
use crate::settings::Settings;

pub fn serve(
    _settings: &Settings,
    _listen: SocketAddr,
    _token_file: &Path,
    _allow: &[IpAddr],
    _cancel: &Cancel,
) -> Result<u8, NotImplemented> {
    Err(NotImplemented {
        task: "T6（serve）",
    })
}
