//! 資格情報の読み込みと、アクセストークンの取得・保持（docs/note-photo-export/drive-api.md 2 章）。
//!
//! - 資格情報のファイルは `{"client_id": "...", "client_secret": "...", "refresh_token": "..."}` の 3 つのキーだけの JSON。ほかのキー・
//!   重複したキー・空の値は断る。大きさは 16KiB まで。読み方は `O_NOFOLLOW`・通常のファイル・グループと他人の権限ビットが無いこと・
//!   持ち主が実効ユーザーか root（systemd の `LoadCredential` の写しは 0400 でサービスのユーザーの持ち物。root の持ち物の元のファイルを
//!   直接渡す使い方も許す）。**権限ビットの検査**は [`check_mode`]: その他（other）のビットはすべて 0、グループの書き込み・実行は 0。
//!   グループの読み取り（0040）は、**ファイルのグループが root（gid 0）のときだけ**許す。systemd の `LoadCredential=` の写しは、
//!   mode 0440・所有者 root・グループ root で、サービスのユーザーには ACL（`user:<名前>:r--`・`mask::r--`・`group::---`）で読み取りが
//!   付く。ACL があると stat の mode のグループのビットは ACL の mask（r）になるので、グループのビットを一律に断ると、この写しを
//!   断ってしまい、サービスが起動しない（終了コード 2）。root グループは root しか入らないので、読み取りを許してよい（root は
//!   元から読める）。それ以外のグループの読み取り（0640 など）は、実際にグループの誰かが読めるので断る。
//! - 値は、ログ・エラー・`Debug` に出さない（`Debug` は手で書いて伏せる）。
//! - **資格情報のファイルは決して書かない**（アクセストークンはメモリだけに持つ）。
//! - トークンの取得: `POST <token_url>` に `grant_type=refresh_token&client_id=…&client_secret=…&refresh_token=…`。応答の
//!   `access_token`・`expires_in`（上限 3600 秒。期限の 60 秒前に取り直す）。
//!   - `400`・`401` で本文の `error` が `invalid_grant`・`invalid_client`・`unauthorized_client` → [`ApiError::AuthInvalid`]
//!     （`drive_auth_invalid`。**再試行しない**。資格情報を取り込み直す必要がある）。
//!   - ほかの 4xx（`invalid_request` など。こちらの組み立ての誤りか Google 側の不調）は、認可の失効とは言えないので
//!     [`ApiError::Status`]（`drive_unavailable`）。再試行もしない。
//!   - 429・5xx・通信の失敗は、ほかの要求と同じ再試行。

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::http::{ApiError, Ctx, Method, Outcome, Req, SendOpts, Transport, encode};

/// 資格情報のファイルの大きさの上限。
pub const MAX_CREDENTIALS_BYTES: u64 = 16 * 1024;
/// 値 1 つの長さの上限。
const MAX_VALUE_BYTES: usize = 4096;
const KEYS: [&str; 3] = ["client_id", "client_secret", "refresh_token"];
/// トークンの期限のこれだけ前に取り直す。
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// `expires_in` が無い・読めないときの寿命（Google は 3599）。
const DEFAULT_LIFETIME: Duration = Duration::from_secs(3000);

/// 資格情報（OAuth のクライアントと更新トークン）。`Debug` は値を出さない。
#[derive(Clone)]
pub struct Credentials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(..)")
    }
}

/// 資格情報を読めなかった理由。値（パス・中身）は含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialsError {
    /// 開けない・読めない（種類だけ）。リンクをたどらないので、シンボリックリンクもここ。
    Unreadable(io::ErrorKind),
    /// 通常のファイルでない。
    NotRegularFile,
    /// 持ち主が実効ユーザーでも root でもない。
    WrongOwner,
    /// その他に権限がある・グループに書き込み・実行の権限がある・root 以外のグループに読み取りの権限がある（0600 か 0400 にする。
    /// root グループの読み取りだけは許す）。
    TooPermissive,
    TooLarge,
    /// JSON のオブジェクトとして読めない（UTF-8 でない・文法の誤り・オブジェクトでない・末尾に余り）。
    NotJson,
    /// 3 つ以外のキー（`unknown_field`）か、重複したキー（`duplicate_field`）。
    BadKeys(&'static str),
    MissingKey(&'static str),
    /// 文字列でない・空・長すぎる・制御文字を含む。
    BadValue(&'static str),
}

impl fmt::Display for CredentialsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(kind) => write!(f, "開けません（{kind}）"),
            Self::NotRegularFile => f.write_str("通常のファイルではありません"),
            Self::WrongOwner => f.write_str("持ち主が実行ユーザーでも root でもありません"),
            Self::TooPermissive => {
                f.write_str(
                "その他・グループに権限があります（0600 か 0400 にしてください。root グループの読み取りだけは許します）",
            )
            }
            Self::TooLarge => write!(f, "{MAX_CREDENTIALS_BYTES} バイトを超えています"),
            Self::NotJson => f.write_str("JSON のオブジェクトとして読めません"),
            Self::BadKeys(why) => write!(
                f,
                "キーが client_id・client_secret・refresh_token の 3 つだけではありません（{why}）"
            ),
            Self::MissingKey(k) => write!(f, "{k} がありません"),
            Self::BadValue(k) => write!(
                f,
                "{k} の値が空・長すぎる・文字列でない・制御文字を含むのどれかです"
            ),
        }
    }
}

impl std::error::Error for CredentialsError {}

impl From<CredentialsError> for ApiError {
    fn from(e: CredentialsError) -> Self {
        Self::Credentials(e)
    }
}

impl Credentials {
    /// 値から作る（試験用。空の値は作れない）。
    #[doc(hidden)]
    pub fn from_values(
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<Self, CredentialsError> {
        let value = |key: &'static str, v: &str| -> Result<String, CredentialsError> {
            if v.is_empty() || v.len() > MAX_VALUE_BYTES || v.bytes().any(|b| b < 0x20 || b == 0x7f)
            {
                Err(CredentialsError::BadValue(key))
            } else {
                Ok(v.to_owned())
            }
        };
        Ok(Self {
            client_id: value("client_id", client_id)?,
            client_secret: value("client_secret", client_secret)?,
            refresh_token: value("refresh_token", refresh_token)?,
        })
    }

    /// ファイルから読む。ファイルは書かない。
    pub fn load(path: &Path) -> Result<Self, CredentialsError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| CredentialsError::Unreadable(e.kind()))?;
        let meta = file
            .metadata()
            .map_err(|e| CredentialsError::Unreadable(e.kind()))?;
        if !meta.is_file() {
            return Err(CredentialsError::NotRegularFile);
        }
        if meta.uid() != crate::sys::euid() && meta.uid() != 0 {
            return Err(CredentialsError::WrongOwner);
        }
        check_mode(meta.mode(), meta.gid())?;
        if meta.len() > MAX_CREDENTIALS_BYTES {
            return Err(CredentialsError::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(MAX_CREDENTIALS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| CredentialsError::Unreadable(e.kind()))?;
        if bytes.len() as u64 > MAX_CREDENTIALS_BYTES {
            return Err(CredentialsError::TooLarge);
        }
        Self::parse(&bytes)
    }

    /// JSON の本文から読む（3 つのキーだけ）。
    fn parse(bytes: &[u8]) -> Result<Self, CredentialsError> {
        let object = crate::jsonfile::parse_strict_object(bytes, &KEYS)
            .ok_or(CredentialsError::NotJson)?
            .map_err(CredentialsError::BadKeys)?;
        let get = |key: &'static str| -> Result<&str, CredentialsError> {
            match object.get(key) {
                None => Err(CredentialsError::MissingKey(key)),
                Some(serde_json::Value::String(s)) => Ok(s),
                Some(_) => Err(CredentialsError::BadValue(key)),
            }
        };
        Self::from_values(
            get("client_id")?,
            get("client_secret")?,
            get("refresh_token")?,
        )
    }
}

/// 資格情報のファイルの権限ビット（`mode`。ファイルの種類のビットを含んでよい）と、ファイルのグループ（`gid`）の検査。
/// その他（other）のビットはすべて 0、グループの書き込み・実行は 0 を求める。グループの読み取り（0040）は、`gid` が root（0）の
/// ときだけ許す（systemd の `LoadCredential=` の写し。理由はモジュールの説明）。持ち主の検査は呼び出し側。
#[doc(hidden)]
pub fn check_mode(mode: u32, gid: u32) -> Result<(), CredentialsError> {
    if mode & 0o007 != 0 || mode & 0o030 != 0 || (mode & 0o040 != 0 && gid != 0) {
        return Err(CredentialsError::TooPermissive);
    }
    Ok(())
}

// ---- アクセストークン ----

struct Cached {
    value: String,
    expires_at: Instant,
}

/// アクセストークンの保持と取得。メモリだけに持つ（ファイルへは書かない）。
pub(super) struct Tokens {
    credentials: Credentials,
    state: Mutex<Option<Cached>>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tokens(..)")
    }
}

impl Tokens {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            state: Mutex::new(None),
        }
    }

    /// 有効なアクセストークン（無い・期限が近いときは取り直す）。取り直しは 1 つずつ（ロックを持ったまま）。
    /// **呼び出し側は値をログに出さないこと。**
    pub fn get(&self, transport: &Transport, ctx: &Ctx<'_>) -> Result<String, ApiError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = state.as_ref()
            && t.expires_at > Instant::now() + REFRESH_MARGIN
        {
            return Ok(t.value.clone());
        }
        let fresh = self.fetch(transport, ctx)?;
        let value = fresh.value.clone();
        *state = Some(fresh);
        Ok(value)
    }

    /// 401 を受けたトークンを捨てる（そのあいだに別のスレッドが取り直していたら、その新しいトークンは捨てない）。
    pub fn invalidate(&self, rejected: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.as_ref().is_some_and(|t| t.value == rejected) {
            *state = None;
        }
    }

    fn fetch(&self, transport: &Transport, ctx: &Ctx<'_>) -> Result<Cached, ApiError> {
        let body = format!(
            "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
            encode(&self.credentials.client_id),
            encode(&self.credentials.client_secret),
            encode(&self.credentials.refresh_token)
        );
        let req = Req::with_body(
            Method::Post,
            transport.endpoints().token().to_owned(),
            "application/x-www-form-urlencoded",
            body.as_bytes(),
        );
        let mut opts = SendOpts::new("token");
        opts.passthrough = &[400];
        let started = Instant::now();
        let Outcome::Reply(reply) = transport.send_with_retry(ctx, &req, None, &opts)? else {
            return Err(ApiError::BadResponse);
        };
        if !reply.ok() {
            // 400・401: OAuth のエラー（本文の `error`）
            let error = serde_json::from_slice::<serde_json::Value>(&reply.body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_default();
            return Err(
                if matches!(
                    error.as_str(),
                    "invalid_grant" | "invalid_client" | "unauthorized_client"
                ) {
                    tracing::warn!(op = "token", status = reply.status, "認可が失効しています");
                    ApiError::AuthInvalid
                } else {
                    tracing::warn!(
                        op = "token",
                        status = reply.status,
                        "トークンの取得が断られました"
                    );
                    ApiError::Status(reply.status)
                },
            );
        }
        let json = reply.json()?;
        let value = json["access_token"]
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= MAX_VALUE_BYTES
                    && !s.bytes().any(|b| b < 0x20 || b == 0x7f)
            })
            .ok_or(ApiError::BadResponse)?
            .to_owned();
        let lifetime = json["expires_in"]
            .as_u64()
            .map_or(DEFAULT_LIFETIME, |s| Duration::from_secs(s.min(3600)));
        tracing::debug!(op = "token", "アクセストークンを取得しました");
        Ok(Cached {
            value,
            expires_at: started + lifetime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_parse_only_the_three_keys() {
        let ok = br#"{"client_id":"i","client_secret":"s","refresh_token":"r"}"#;
        assert!(Credentials::parse(ok).is_ok());
        let cases: [(&[u8], CredentialsError); 9] = [
            (b"", CredentialsError::NotJson),
            (b"[]", CredentialsError::NotJson),
            (b"{", CredentialsError::NotJson),
            (
                br#"{"client_id":"i","client_secret":"s","refresh_token":"r"} x"#,
                CredentialsError::NotJson,
            ),
            (
                br#"{"client_id":"i","client_secret":"s","refresh_token":"r","x":1}"#,
                CredentialsError::BadKeys("unknown_field"),
            ),
            (
                br#"{"client_id":"i","client_id":"j","client_secret":"s","refresh_token":"r"}"#,
                CredentialsError::BadKeys("duplicate_field"),
            ),
            (
                br#"{"client_id":"i","client_secret":"s"}"#,
                CredentialsError::MissingKey("refresh_token"),
            ),
            (
                br#"{"client_id":"","client_secret":"s","refresh_token":"r"}"#,
                CredentialsError::BadValue("client_id"),
            ),
            (
                br#"{"client_id":"i","client_secret":5,"refresh_token":"r"}"#,
                CredentialsError::BadValue("client_secret"),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(
                Credentials::parse(input).unwrap_err(),
                want,
                "{}",
                String::from_utf8_lossy(input)
            );
        }
        // エスケープを戻したあとのキーで重複を見る
        assert_eq!(
            Credentials::parse(
                br#"{"client_id":"i","client_id":"j","client_secret":"s","refresh_token":"r"}"#
            )
            .unwrap_err(),
            CredentialsError::BadKeys("duplicate_field")
        );
        // 値に制御文字
        assert_eq!(
            Credentials::parse(br#"{"client_id":"i\n","client_secret":"s","refresh_token":"r"}"#)
                .unwrap_err(),
            CredentialsError::BadValue("client_id")
        );
    }

    #[test]
    fn debug_never_shows_values() {
        let c = Credentials::from_values("ID-VALUE", "SECRET-VALUE", "REFRESH-VALUE").unwrap();
        let text = format!("{c:?} {:?}", Tokens::new(c.clone()));
        for v in ["ID-VALUE", "SECRET-VALUE", "REFRESH-VALUE"] {
            assert!(!text.contains(v), "{text}");
        }
    }
}
