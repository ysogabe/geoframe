//! T5: `recover` コマンド（design.md 4.2 の復旧、6.2）。
//!
//! T2 では「未実装」を返すだけ。

use crate::app::NotImplemented;
use crate::settings::Settings;

pub fn recover(_settings: &Settings, _resolve: Option<&str>) -> Result<u8, NotImplemented> {
    Err(NotImplemented {
        task: "T5（recover）",
    })
}
