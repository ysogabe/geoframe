//! T5: `run` コマンド（design.md 6.2）。結果の JSON と終了コード（0・1・3・4）。
//!
//! T2 では、検査済みの要求と設定を受け取って「未実装」を返すだけ。

use crate::app::NotImplemented;
use crate::request::ExportRequest;
use crate::settings::Settings;

pub fn run(_settings: &Settings, _request: &ExportRequest) -> Result<u8, NotImplemented> {
    Err(NotImplemented {
        task: "T5（run）"
    })
}
