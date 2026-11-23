//! Drive の操作の入口と、その周りの中立の型（design.md 4.2「Drive の操作」、docs/note-photo-export/drive-api.md）。
//!
//! 呼び出し側（[`crate::drive`]・[`crate::publish`]・[`crate::flow`]・[`crate::recover`]）は、このモジュールの [`Remote`] だけを
//! 持ち回る。中身は [`crate::drive::api::DriveApi`]（Drive API v3 を HTTPS で直接呼ぶ。フォルダ・ファイルは **Drive の ID** で指す）。
//! 実装を差し替える仕組み（trait・実行時の切り替え）は持たない。
//!
//! ## 操作（どれも ID で指す。パスは使わない）
//!
//! | 操作 | 意味 | Drive API |
//! |---|---|---|
//! | `list_children` | 親の直下の一覧（ゴミ箱の中は出ない） | `files.list` |
//! | `list_tree` | 親の下を再帰で | `files.list` を幅優先で |
//! | `get` | ID で 1 件 | `files.get` |
//! | `read` | ファイルの中身 | `files.get?alt=media` |
//! | `create_folder` | 親の下にフォルダを作る | `files.generateIds` ＋ `files.create` |
//! | `upload` | 手元のファイルを送る | `files.create`（multipart／resumable） |
//! | `move_to` | 親と名前を変える（ID は保たれる） | `files.update`（`addParents`・`removeParents`） |
//! | `trash` | Drive のゴミ箱へ移す | `files.update`（`trashed: true`） |
//!
//! ここに置くのは、操作の入口（[`Remote`]）のほかに、一覧の 1 項目（[`Entry`]）、Drive の ID・MD5・相対パスの形の検査、MIME タイプの
//! 定数、操作の失敗（[`RemoteError`]）と、それをジョブのエラーにする対応。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::cancel::Cancel;
use crate::drive::api::{DriveApi, FileMeta, Limit};
use crate::drive::auth::Credentials;
use crate::drive::http::{ApiError, Deadline, Retry};
use crate::error::{ErrorCode, ExportError};
use crate::settings::Settings;

/// Drive のショートカットの MIME タイプ。
pub const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
/// `dry_run` の照会の期限（design.md 6.1）。
pub const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Drive の一覧の 1 項目（`files.list` の 1 件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 一覧の起点からの相対パス（`/` 区切り）。
    pub path: String,
    pub name: String,
    /// バイト数（フォルダは -1）。
    pub size: i64,
    pub mime: String,
    pub is_dir: bool,
    /// Drive の ID。ショートカットでは、ショートカット自身の ID（リンク先はたどらない）。
    pub id: String,
    /// `md5Checksum`（小文字の 16 進 32 文字）。フォルダ・MD5 を持たないファイル（Google のネイティブ文書など）・形が違う値では
    /// `None`。
    pub md5: Option<String>,
}

impl Entry {
    /// ショートカットか（MIME タイプがショートカット。リンク先はたどらない）。
    pub fn is_shortcut(&self) -> bool {
        self.mime == SHORTCUT_MIME
    }
}

/// Drive の ID の形（`[A-Za-z0-9_-]`、1〜128 文字）。空の ID は決して受け付けない（空どうしが「一致」しないように）。
pub fn is_valid_drive_id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// MD5 の文字列（16 進 32 文字。大文字でも受け、小文字にそろえる）。違えば `None`。
pub fn normalize_md5(s: &str) -> Option<String> {
    (s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// 一覧・記録の相対パス（`/` 区切り。空の部品・`.`・`..`・制御文字を含まない。1024 バイト以下）。
pub fn rel_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.chars().any(char::is_control))
}

/// 1 回の操作の失敗。Drive の応答の中身は含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteError {
    /// 送る前に断った（形の違う ID・名前・URL、手元のファイルの大きさ・MD5 の違い）・手元のファイルを読めない・資格情報を読めない。
    /// Drive へは要求していない。
    Spawn,
    /// このコマンドの期限を過ぎて kill した。
    Timeout,
    /// ジョブ全体の期限を過ぎた（起動前、または kill した）。
    JobDeadline,
    /// その ID が無い（404）。
    NotFound,
    /// API の応答が決められた形でない・一覧が不完全・ページ送りの上限を超えた。
    BadOutput,
    /// 読み出したファイルが、決めた上限を超えた（[`Remote::read`]。呼び出し側は「一覧の大きさと違う」と同じに扱う）。
    TooLarge,
    /// キャンセルの印（SIGINT・SIGTERM）が立った（要求の前・再試行の待ち・チャンクの間）。結果は分からない
    /// （送信・移動は、Drive 側で済んでいるかもしれない）ので、呼び出し側は失敗として確定せず、復旧の表に渡す。
    Cancelled,
    /// Drive の認可が失効している（OAuth の更新トークンが `invalid_grant` など）。再認可が要る。照会でも書き込みでも
    /// `drive_auth_invalid`。
    AuthInvalid,
    /// Drive API の要求が失敗した（通信の失敗・状態コード・書き込みのあとの照合の不一致）。再試行は使い切ったあと。
    /// 移動・ゴミ箱へ移すが「処理されていない」と分かる応答（429・流量制限の 403・401。[`crate::drive::http::ApiError::NotProcessed`]）
    /// を受けた場合も、ここにまとめる（API の層は再送せず、呼び出し側も再送しない。5xx・通信断の結果不明と同じに扱い、
    /// 復旧の表に任せる）。
    Failed,
}

impl RemoteError {
    /// 照会（一覧・読み出し）の失敗を、ジョブのエラーにする。「無い」と取り違えないよう、`drive_unavailable`。
    /// 認可の失効は `drive_auth_invalid`、ジョブ全体の期限は `timeout`。
    pub fn into_query_error(self) -> ExportError {
        match self {
            Self::JobDeadline => ExportError::from(ErrorCode::Timeout),
            Self::AuthInvalid => ExportError::from(ErrorCode::DriveAuthInvalid),
            _ => ExportError::new(
                ErrorCode::DriveUnavailable,
                "Drive に照会できません（API の失敗）",
            ),
        }
    }

    /// 書き込み（フォルダの作成・送信・移動・ゴミ箱へ移す）の失敗を、ジョブのエラーにする。操作の期限は `rclone_timeout`
    /// （綴りは呼び出し側との契約で変えない。意味は「Drive の操作が期限内に終わらない」）、ジョブ全体の期限は `timeout`、
    /// 認可の失効は `drive_auth_invalid`、それ以外は `rclone_failed`（同じく、意味は「Drive の変更系の操作の失敗」）。
    pub fn into_op_error(self) -> ExportError {
        match self {
            Self::Timeout => ExportError::from(ErrorCode::RcloneTimeout),
            Self::JobDeadline => ExportError::from(ErrorCode::Timeout),
            Self::AuthInvalid => ExportError::from(ErrorCode::DriveAuthInvalid),
            _ => ExportError::from(ErrorCode::RcloneFailed),
        }
    }
}

/// Drive の操作の入口。`Clone` は安い（接続・トークンは共有。キャンセルの印・期限は複製ごと）。中身は Drive API（ID で指す）。
#[derive(Debug, Clone)]
pub struct Remote {
    api: DriveApi,
}

impl Remote {
    /// 設定から作る。資格情報のファイルを読む（検査に通らなければ [`RemoteError::Spawn`]。ファイルは書かない）。
    /// 期限は `--drive-upload-timeout`・`--drive-timeout`。
    pub fn new(settings: &Settings) -> Result<Self, RemoteError> {
        let credentials = Credentials::load(&settings.drive_credentials).map_err(ApiError::from)?;
        let api = DriveApi::new(credentials, settings.endpoints())
            .with_timeouts(settings.drive_upload_timeout, settings.drive_timeout);
        Ok(Self { api })
    }

    /// 中身の [`DriveApi`]（起動時の検査が、失敗の種類を細かく見る）。
    pub fn api(&self) -> &DriveApi {
        &self.api
    }

    /// 作った [`DriveApi`] を包む（試験用）。
    #[doc(hidden)]
    pub fn from_api(api: DriveApi) -> Self {
        Self { api }
    }

    /// `dry_run` 用: 照会の期限を 30 秒にする（design.md 6.1）。送信（`upload`）は使わない。
    pub fn for_dry_run(&self) -> Self {
        let upload = self.api.upload_timeout().min(DRY_RUN_TIMEOUT);
        let query = self.api.query_timeout().min(DRY_RUN_TIMEOUT);
        Self {
            api: self.api.clone().with_timeouts(upload, query),
        }
    }

    /// 期限（操作ごとの期限）を指定して作る。
    #[doc(hidden)]
    pub fn with_timeouts(self, upload: Duration, query: Duration) -> Self {
        Self {
            api: self.api.with_timeouts(upload, query),
        }
    }

    /// 再試行の方針を替える（試験が待たずに済むように）。
    #[doc(hidden)]
    pub fn with_retry(self, retry: Retry) -> Self {
        Self {
            api: self.api.with_retry(retry),
        }
    }

    /// 一覧のページの大きさ・multipart の上限・チャンクの大きさを替える（試験用）。
    #[doc(hidden)]
    pub fn with_sizes(self, page_size: u32, multipart_max: u64, chunk_size: u64) -> Self {
        Self {
            api: self.api.with_sizes(page_size, multipart_max, chunk_size),
        }
    }

    /// キャンセルの印（SIGINT・SIGTERM）を共有する。立つと、要求の前・再試行の待ち・チャンクの間で止まって
    /// [`RemoteError::Cancelled`] を返す（要求の最中には見ない。1 回の要求の上限は [`crate::drive::api::CHUNK_REQUEST_CAP`] など）。
    pub fn with_cancel(self, cancel: Cancel) -> Self {
        Self {
            api: self.api.with_cancel(cancel),
        }
    }

    /// キャンセルの印が立っているか（復旧の待ちが、眠らずに止まるために見る）。
    pub fn is_cancelled(&self) -> bool {
        self.api.is_cancelled()
    }

    /// ジョブ全体の期限を掛けた複製。過ぎたあとの操作は `JobDeadline`。
    pub fn with_deadline(&self, deadline: Option<Instant>) -> Self {
        Self {
            api: self.api.with_job_deadline(deadline),
        }
    }

    /// アクセストークンを 1 回取る（起動時の確認）。認可の失効は [`RemoteError::AuthInvalid`]。
    pub fn check_credentials(&self) -> Result<(), RemoteError> {
        Ok(self
            .api
            .check_credentials(self.api.deadline(Limit::Query))?)
    }

    /// `parent` の直下の一覧（ゴミ箱の中は出ない。ショートカットも 1 件として出る）。
    pub fn list_children(&self, parent: &str) -> Result<Vec<Entry>, RemoteError> {
        Ok(self
            .api
            .list_children(parent, self.api.deadline(Limit::Query))?)
    }

    /// `root` の下の全部（再帰。`Entry.path` は `root` からの相対パス）。
    pub fn list_tree(&self, root: &str) -> Result<Vec<Entry>, RemoteError> {
        Ok(self.api.list_tree(root, self.api.deadline(Limit::Query))?)
    }

    /// ID で 1 件（ゴミ箱の中でも返る。`trashed`・`parents` つき）。
    pub fn get(&self, id: &str) -> Result<FileMeta, RemoteError> {
        Ok(self.api.get(id, self.api.deadline(Limit::Query))?)
    }

    /// ファイルの中身。`max_bytes` を超えれば [`RemoteError::TooLarge`]。
    pub fn read(&self, id: &str, max_bytes: u64) -> Result<Vec<u8>, RemoteError> {
        Ok(self
            .api
            .read(id, max_bytes, self.api.deadline(Limit::Query))?)
    }

    /// `parent` の下にフォルダを作り、その ID を返す（応答を失った再送でも重複しない）。
    pub fn create_folder(&self, parent: &str, name: &str) -> Result<String, RemoteError> {
        Ok(self
            .api
            .create_folder(parent, name, self.api.deadline(Limit::Query))?)
    }

    /// 写真の送信全体の締め切り（`--drive-upload-timeout` とジョブの期限の早いほう）。**送信全体で 1 つ作って**、写真ごとの
    /// [`Remote::upload`] に同じものを渡す（ファイルごとに作ると期限が延びる）。
    pub fn upload_deadline(&self) -> Deadline {
        self.api.deadline(Limit::Upload)
    }

    /// `parent` の下に `local` を `name` で送り、その ID を返す。`md5`・`size` は送る中身の MD5 と大きさ。
    pub fn upload(
        &self,
        parent: &str,
        name: &str,
        local: &Path,
        md5: &str,
        size: u64,
        deadline: Deadline,
    ) -> Result<String, RemoteError> {
        Ok(self.api.upload(parent, name, local, md5, size, deadline)?)
    }

    /// `id` を `from_parent` から `to_parent` へ、`new_name` にして移す（ID は保たれる）。
    pub fn move_to(
        &self,
        id: &str,
        from_parent: &str,
        to_parent: &str,
        new_name: &str,
    ) -> Result<(), RemoteError> {
        Ok(self.api.move_to(
            id,
            from_parent,
            to_parent,
            new_name,
            self.api.deadline(Limit::Query),
        )?)
    }

    /// `id` を Drive のゴミ箱へ移す（完全な削除ではない）。
    pub fn trash(&self, id: &str) -> Result<(), RemoteError> {
        Ok(self.api.trash(id, self.api.deadline(Limit::Query))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_ids_are_never_empty() {
        for ok in ["a", "0BxYz-_9", &"a".repeat(128)] {
            assert!(is_valid_drive_id(ok));
        }
        for bad in ["", "a b", "a.b", &"a".repeat(129), "日本"] {
            assert!(!is_valid_drive_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn shortcut_entries_are_recognised_by_mime() {
        let e = Entry {
            path: "a".into(),
            name: "a".into(),
            size: 1,
            mime: SHORTCUT_MIME.into(),
            is_dir: false,
            id: "i".into(),
            md5: None,
        };
        assert!(e.is_shortcut());
        let plain = Entry {
            mime: "image/jpeg".into(),
            ..e
        };
        assert!(!plain.is_shortcut());
    }

    #[test]
    fn md5_is_normalised_to_lowercase_hex() {
        assert_eq!(
            normalize_md5(&"A".repeat(32)).as_deref(),
            Some("a".repeat(32).as_str())
        );
        for bad in ["", "abc", &"g".repeat(32), &"a".repeat(33)] {
            assert!(normalize_md5(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn relative_paths_are_checked() {
        for ok in ["a", "a/b.jpg"] {
            assert!(rel_path_ok(ok), "{ok:?}");
        }
        for bad in [
            "",
            "/a",
            "a//b",
            "a/./b",
            "a/../b",
            "a\u{0}b",
            &"a/".repeat(600),
        ] {
            assert!(!rel_path_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn remote_errors_map_to_the_documented_codes() {
        assert_eq!(
            RemoteError::Timeout.into_op_error().code,
            ErrorCode::RcloneTimeout
        );
        assert_eq!(
            RemoteError::Failed.into_op_error().code,
            ErrorCode::RcloneFailed
        );
        assert_eq!(
            RemoteError::JobDeadline.into_op_error().code,
            ErrorCode::Timeout
        );
        for e in [
            RemoteError::Timeout,
            RemoteError::NotFound,
            RemoteError::Failed,
            RemoteError::BadOutput,
            RemoteError::Spawn,
        ] {
            assert_eq!(e.into_query_error().code, ErrorCode::DriveUnavailable);
        }
        assert_eq!(
            RemoteError::JobDeadline.into_query_error().code,
            ErrorCode::Timeout
        );
    }

    #[test]
    fn auth_failures_map_to_drive_auth_invalid_for_queries_and_writes() {
        assert_eq!(
            RemoteError::AuthInvalid.into_query_error().code,
            ErrorCode::DriveAuthInvalid
        );
        assert_eq!(
            RemoteError::AuthInvalid.into_op_error().code,
            ErrorCode::DriveAuthInvalid
        );
        // API の要求の失敗は、照会なら drive_unavailable、書き込みなら rclone_failed
        assert_eq!(
            RemoteError::Failed.into_query_error().code,
            ErrorCode::DriveUnavailable
        );
        assert_eq!(
            RemoteError::Failed.into_op_error().code,
            ErrorCode::RcloneFailed
        );
    }
}
