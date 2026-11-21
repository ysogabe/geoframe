//! 起動時の検査の入口。`serve`・`run`・`recover` は、何かを始める前に必ずここを通る（`dry_run` でも）。
//!
//! 引数そのものの検査（接頭辞・work-root・feed-base。[`crate::settings`]）は引数を解釈するときに済んでいる。
//! ここでは、ファイルシステムを見る検査を行う。
//!
//! - rclone の設定ファイルが、専用の client_id・client_secret を持ち、持ち主・権限が正しいこと
//!   （[`crate::rclone_conf`]。rclone の共用の client_id を使わせないため）
//!
//! 状態ディレクトリの用意（`jobs/`・`staging/`。0700。リンクでない実ディレクトリで、自分の持ち物）は、ジョブを扱う側
//! （[`crate::jobs::JobStore::open`]。`run`・`recover`・`serve` が通る）が行う。トークンのファイルと `--listen` の検査は、`serve` だけのものなので、[`crate::serve`] が起動の最初に行う（満たさなければ終了コード 2）。

use crate::rclone_conf::{self, ConfigError};
use crate::settings::Settings;

pub fn startup_check(settings: &Settings) -> Result<(), ConfigError> {
    rclone_conf::check_rclone_config(&settings.rclone_config, settings.remote_name())
}
