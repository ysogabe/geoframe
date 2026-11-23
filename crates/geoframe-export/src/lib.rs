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
//! | [`rclone`]・[`drive`]・[`publish`]・[`recovery`]・[`published`]・[`jsonfile`]・[`jobid`] | rclone の起動、Drive への照会と操作・管理しているフォルダの判定、incoming → 検証 → 公開・退避・復旧の実行、復旧の表（純粋な関数）、公開の記録、アトミックな JSON の書き方、ジョブ ID | T4 |
//! | [`sys`]・[`cancel`]・[`lock`]・[`jobs`]・[`flow`]・[`service`]・[`run`]・[`recover`] | OS への問い合わせ（flock・現地時刻・実効ユーザー ID）、キャンセルの印（SIGINT・SIGTERM）、実行枠、ジョブの記録と状態ディレクトリの配置、ジョブの流れ・復旧・`dry_run`、受付と照会の層（T6 が使う）、`run`・`recover` | T5 |
//! | [`serve`] | HTTP の入口（トークン・送信元の制限・本文の検査・接続の上限・ジョブ API・シグナルでの終了） | T6 |

pub mod app;
pub mod cancel;
pub mod cli;
pub mod date;
pub mod drive;
pub mod error;
pub mod feed;
pub mod folder;
pub mod image;
pub mod jobid;
pub mod jpeg;
pub mod jsonfile;
pub mod listing;
pub mod logging;
pub mod manifest;
pub mod measure;
pub mod publish;
pub mod published;
pub mod rclone_conf;
pub mod request;
pub mod settings;
pub mod staging;
pub mod startup;

pub mod flow;
pub mod jobs;
pub mod lock;
pub mod rclone;
pub mod recover;
pub mod recovery;
pub mod remote;
pub mod run;
pub mod serve;
pub mod service;
pub mod sys;
