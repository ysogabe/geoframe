//! T6: `serve`（bind の検査、トークン、送信元の許可、本文の検査、接続の上限、ジョブの API。design.md 3.4・6.1）。
//!
//! T2 では「未実装」を返すだけ。

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use crate::app::NotImplemented;
use crate::settings::Settings;

pub fn serve(
    _settings: &Settings,
    _listen: SocketAddr,
    _token_file: &Path,
    _allow: &[IpAddr],
) -> Result<u8, NotImplemented> {
    Err(NotImplemented {
        task: "T6（serve）",
    })
}
