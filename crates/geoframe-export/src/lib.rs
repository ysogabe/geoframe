//! note 記事用の写真書き出し（`geoframe-export`。docs/note-photo-export/design.md）。
//!
//! 写真サービス（`geoframe serve`）とは別のプロセスで、`geoframe` crate には依存しない。つながりは HTTP
//! （一覧 API `GET /api/admin/export/photos` と `/derivatives/*`）だけ。Drive へは rclone で送る。
//!
//! モジュールと、後のタスクが乗る場所:
//!
//! | モジュール | 中身 | タスク |
//! |---|---|---|
//! | [`error`] | 固定のエラーコード | T2 |
//! | [`date`] | 日付・時刻・期間（14 日以下の検査） | T2 |
//! | [`request`] | 要求の検査（HTTP と CLI で共通） | T2 |
//! | [`folder`] | 書き出し先のフォルダ名 4 通り | T2 |
//! | [`settings`] | 起動引数の検査（接頭辞・work-root・feed-base） | T2 |
//! | [`rclone_conf`] | rclone 設定の専用 client_id・持ち主・権限の検査 | T2 |
//! | [`startup`] | 起動時の検査の入口（serve・run・recover が必ず通る） | T2 |
//! | [`feed`] | 一覧 API の取得（30 秒・8MB・リダイレクトなし） | T2 |
//! | [`listing`] | 一覧 API の値の検査（4.4）と候補の選び方 | T2 |
//! | [`cli`]・[`app`] | 引数の解釈と、コマンドの振り分け | T2 |
//! | [`image`]・[`jpeg`]・[`measure`]・[`staging`]・[`manifest`] | 画像の取得と書き出しの入口、JPEG の区画の検査とデコード、明るさ・シャープネス、作業領域、manifest | T3 |
//! | [`rclone`] | rclone の呼び出し、宛先の状態、公開・退避・復旧 | T4 |
//! | [`lock`]・[`jobs`]・[`run`]・[`recover`] | 実行枠、ジョブと公開の記録、`run`・`recover` | T5 |
//! | [`serve`] | HTTP の入口（トークン・送信元・本文の検査・ジョブ API） | T6 |

pub mod app;
pub mod cli;
pub mod date;
pub mod error;
pub mod feed;
pub mod folder;
pub mod image;
pub mod jpeg;
pub mod listing;
pub mod manifest;
pub mod measure;
pub mod rclone_conf;
pub mod request;
pub mod settings;
pub mod staging;
pub mod startup;

// 後のタスクが中身を足す場所（T2 では空）
pub mod jobs;
pub mod lock;
pub mod rclone;
pub mod recover;
pub mod run;
pub mod serve;
