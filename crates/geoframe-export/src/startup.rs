//! 起動時の検査の入口。`serve`・`run`（`--dry-run` を含む）・`recover` は、何かを始める前に必ずここを通る
//! （docs/note-photo-export/drive-api.md 2 章・3 章）。
//!
//! 引数そのものの検査（ID の形・3 つが同じでないこと・feed-base。[`crate::settings`]）は引数を解釈するときに済んでいる。
//! ここでは、資格情報のファイルを読み、Drive へ問い合わせて確かめる。
//!
//! 1. 資格情報のファイルを読む（検査に通らなければ終了コード 2。ファイルは書かない）。
//! 2. アクセストークンを 1 回取る。`invalid_grant` など認可の失効は終了コード 2（標準エラーに「再認可が要る」）。通信の失敗・5xx は 1。
//! 3. 3 つのフォルダ（宛先 2 つ・作業用）を `get` で読み、フォルダであること（ショートカットは断る）・ゴミ箱の中でないこと・
//!    3 つが別物で、どれも他のどれかの祖先でないことを確かめる。違えば終了コード 2（設定の誤り）。祖先をたどる途中の通信の失敗・5xx は 1。
//!
//! 終了コード 2 は、systemd の `RestartPreventExitStatus=2` で再起動ループにならない。1 は再起動で直りうる（serve は再試行される）。
//! 表示には資格情報・ID の値を出さない。
//!
//! 状態ディレクトリの用意（`jobs/`・`staging/`。0700。リンクでない実ディレクトリで、自分の持ち物）は、ジョブを扱う側
//! （[`crate::jobs::JobStore::open`]。`run`・`recover`・`serve` が通る）が行う。トークンのファイルと `--listen` の検査は、`serve` だけの
//! ものなので、[`crate::serve`] が起動の最初に行う（満たさなければ終了コード 2）。

use std::fmt;

use crate::drive::auth::Credentials;
use crate::drive::http::{ApiError, Retry};
use crate::remote::Remote;
use crate::settings::{OPT_FOLDER_TRAVEL_JOURNAL, OPT_FOLDER_YAMARECO, OPT_WORK_FOLDER, Settings};

/// 祖先をたどる深さの上限。
pub const MAX_ANCESTOR_DEPTH: usize = 64;

/// 起動時の検査の再試行の方針（最初の 1 回のあとに 2 回。0.5・1 秒の待ち）。ジョブの操作（1・2・4・8・16 秒の 5 回）より短くする:
/// 直らなければ終了コード 1 で終わり、serve は systemd が 15 秒おきに再起動する（`recover`・`run` は、すぐ結果が分かる）。
pub const STARTUP_RETRY: Retry = Retry {
    max_retries: 2,
    base: std::time::Duration::from_millis(500),
    cap: std::time::Duration::from_secs(2),
};

/// 起動できない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupError {
    /// 資格情報のファイルを読めない・検査に通らない（引数 `--drive-credentials`）。
    Credentials,
    /// 認可が失効している（再認可が要る）。
    AuthInvalid,
    /// トークンの入口が、`invalid_grant` 以外の 4xx で断った（資格情報の内容の誤り。再試行しても直らない）。
    TokenRejected,
    /// そのフォルダが無い・読めない（引数の名前）。
    FolderMissing(&'static str),
    /// フォルダでない（ファイル・ショートカット）。
    NotFolder(&'static str),
    /// ゴミ箱の中にある。
    Trashed(&'static str),
    /// 2 つのフォルダが、片方がもう片方の祖先（または同じ）になっている。
    Nested(&'static str, &'static str),
    /// 祖先をたどる深さの上限を超えた。
    TooDeep(&'static str),
    /// Drive に接続できない（通信の失敗・5xx・期限）。再試行で直りうる。
    Unavailable,
}

impl StartupError {
    /// 終了コード（2: 引数・設定の誤り・再認可が要る。1: 通信の失敗）。
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Unavailable => 1,
            _ => 2,
        }
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credentials => f.write_str(
                "Drive の資格情報のファイル（--drive-credentials）を読めません（形・持ち主・権限を確かめてください）",
            ),
            Self::AuthInvalid => f.write_str(
                "Drive の認可が失効しています。再認可が要ります（deploy/README.md の手順）",
            ),
            Self::TokenRejected => f.write_str(
                "Drive のアクセストークンを取得できません（--drive-credentials の内容を確かめてください）",
            ),
            Self::FolderMissing(o) => write!(
                f,
                "{o} のフォルダを Drive から読めません（ID と、資格情報の持ち主の権限を確かめてください）"
            ),
            Self::NotFolder(o) => write!(f, "{o} がフォルダではありません（ショートカットも使えません）"),
            Self::Trashed(o) => write!(f, "{o} のフォルダが Drive のゴミ箱の中にあります"),
            Self::Nested(a, b) => write!(
                f,
                "{a} と {b} は別のフォルダで、一方がもう一方の中にあってはいけません"
            ),
            Self::TooDeep(o) => write!(f, "{o} のフォルダの階層が深すぎて確かめられません"),
            Self::Unavailable => f.write_str("Drive に接続できません（通信の失敗か、Drive 側の一時的な失敗です）"),
        }
    }
}

impl std::error::Error for StartupError {}

/// 取得の失敗を、起動できない理由にする。再試行しない 4xx（権限の不足など）は設定の誤り、通信の失敗・5xx・期限は
/// `Unavailable`。
fn classify(option: &'static str, e: ApiError) -> StartupError {
    match e {
        ApiError::AuthInvalid => StartupError::AuthInvalid,
        ApiError::NotFound => StartupError::FolderMissing(option),
        ApiError::Status(code) if (400..500).contains(&code) && code != 408 => {
            StartupError::FolderMissing(option)
        }
        ApiError::Credentials(_) | ApiError::Rejected(_) => StartupError::Credentials,
        _ => StartupError::Unavailable,
    }
}

/// トークンの取得の失敗を、起動できない理由にする（フォルダの誤りとは別。引数名は出さない）。
fn classify_token(e: ApiError) -> StartupError {
    match e {
        ApiError::AuthInvalid => StartupError::AuthInvalid,
        ApiError::Status(code) if (400..500).contains(&code) && code != 408 => {
            StartupError::TokenRejected
        }
        ApiError::Credentials(_) | ApiError::Rejected(_) => StartupError::Credentials,
        _ => StartupError::Unavailable,
    }
}

pub fn startup_check(settings: &Settings) -> Result<(), StartupError> {
    Credentials::load(&settings.drive_credentials).map_err(|_| StartupError::Credentials)?;
    let remote = Remote::new(settings)
        .map_err(|_| StartupError::Credentials)?
        .with_retry(STARTUP_RETRY);
    check_drive(&remote, settings)
}

/// 資格情報（トークンを 1 回取る）と、3 つのフォルダの検査（上の 2・3）。
pub fn check_drive(remote: &Remote, settings: &Settings) -> Result<(), StartupError> {
    let api = remote.api();
    api.check_credentials(api.deadline(crate::drive::api::Limit::Query))
        .map_err(classify_token)?;
    let folders = [
        (OPT_FOLDER_YAMARECO, settings.folder_yamareco.as_str()),
        (
            OPT_FOLDER_TRAVEL_JOURNAL,
            settings.folder_travel_journal.as_str(),
        ),
        (OPT_WORK_FOLDER, settings.work_folder.as_str()),
    ];
    // 祖先（根まで。自分は含めない）
    let mut ancestors: Vec<Vec<String>> = Vec::new();
    for (option, id) in folders {
        let meta = api
            .get(id, api.deadline(crate::drive::api::Limit::Query))
            .map_err(|e| classify(option, e))?;
        if !meta.entry.is_dir || meta.entry.is_shortcut() {
            return Err(StartupError::NotFolder(option));
        }
        if meta.trashed {
            return Err(StartupError::Trashed(option));
        }
        let mut chain = Vec::new();
        let mut parents = meta.parents;
        while let Some(parent) = parents.first().cloned() {
            if chain.len() >= MAX_ANCESTOR_DEPTH {
                return Err(StartupError::TooDeep(option));
            }
            if chain.contains(&parent) {
                break;
            }
            chain.push(parent.clone());
            // 親を見られなければ（共有されたフォルダの上は、見えないことがある）、そこでたどるのをやめる。それ以外の失敗は再試行に回す
            match api.get(&parent, api.deadline(crate::drive::api::Limit::Query)) {
                Ok(m) => parents = m.parents,
                Err(ApiError::NotFound) => break,
                Err(ApiError::AuthInvalid) => return Err(StartupError::AuthInvalid),
                Err(_) => return Err(StartupError::Unavailable),
            }
        }
        ancestors.push(chain);
    }
    for i in 0..folders.len() {
        for j in (i + 1)..folders.len() {
            let (name_i, id_i) = folders[i];
            let (name_j, id_j) = folders[j];
            if id_i == id_j
                || ancestors[i].iter().any(|a| a == id_j)
                || ancestors[j].iter().any(|a| a == id_i)
            {
                return Err(StartupError::Nested(name_i, name_j));
            }
        }
    }
    Ok(())
}
