//! 起動時の検査の入口。`serve`・`run`・`recover` は、何かを始める前に必ずここを通る（`dry_run` でも）。
//!
//! 引数そのものの検査（接頭辞・work-root・feed-base。[`crate::settings`]）は引数を解釈するときに済んでいる。
//! ここでは、ファイルシステムを見る検査を行う。
//!
//! - rclone の設定ファイルが、専用の client_id・client_secret を持ち、持ち主・権限が正しいこと
//!   （[`crate::rclone_conf`]。rclone の共用の client_id を使わせないため）
//!
//! 後のタスクで、状態ディレクトリ・トークンのファイル（T5・T6）の検査をここに足す。

use crate::rclone_conf::{self, ConfigError};
use crate::settings::Settings;

pub fn startup_check(settings: &Settings) -> Result<(), ConfigError> {
    rclone_conf::check_rclone_config(&settings.rclone_config, settings.remote_name())
}
