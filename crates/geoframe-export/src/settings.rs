//! 設定（起動引数）の検査（design.md 6.2、docs/note-photo-export/drive-api.md 1 章）。
//!
//! 宛先の親フォルダ（`--folder-yamareco`・`--folder-travel-journal`）と作業用のフォルダ（`--work-folder`）は、Drive の ID で
//! 指す（パスは使わない）。3 つとも必須で、形（[`crate::remote::is_valid_drive_id`]）を検査し、同じものを指さないことを確かめる
//! （祖先・子孫の関係は、起動時に Drive へ問い合わせて確かめる。[`crate::startup`]）。
//! 実パス（資格情報・状態ディレクトリ）と Drive の ID は文書・コードに書けないので、既定値を持たず必須にする。
//! [`RemotePath`]・[`RemoteReason`]（`remote:パス` の形）は、3 段目で消す rclone 版（[`crate::rclone`]）の試験が使うだけ。

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::drive::http::{EndpointError, Endpoints};
use crate::remote::is_valid_drive_id;
use crate::request::Destination;

pub const OPT_FEED_BASE: &str = "--feed-base";
pub const OPT_STATE_DIR: &str = "--state-dir";
pub const OPT_DRIVE_CREDENTIALS: &str = "--drive-credentials";
pub const OPT_FOLDER_YAMARECO: &str = "--folder-yamareco";
pub const OPT_FOLDER_TRAVEL_JOURNAL: &str = "--folder-travel-journal";
pub const OPT_WORK_FOLDER: &str = "--work-folder";
/// 試験専用（help に出さない。unit・包みには出ない）。偽の Drive の HTTP サーバーへ向ける。
pub const OPT_DRIVE_ENDPOINT: &str = "--drive-endpoint";
pub const OPT_DRIVE_UPLOAD_TIMEOUT: &str = "--drive-upload-timeout";
pub const OPT_DRIVE_TIMEOUT: &str = "--drive-timeout";

pub const DEFAULT_FEED_BASE: &str = "http://127.0.0.1:8080";
/// 写真を送る操作全体の既定の期限（45 分。14 日の最多を細い帯域で送る場合。design.md 2.9）。
pub const DEFAULT_UPLOAD_TIMEOUT_SECS: u64 = 45 * 60;
/// 一覧・取得・フォルダの作成・移動・ゴミ箱へ移す・読み出しの既定の期限（5 分）。
pub const DEFAULT_DRIVE_TIMEOUT_SECS: u64 = 5 * 60;
/// 期限の引数の上限（秒。1 日）。
pub const TIMEOUT_SECS_MAX: u64 = 24 * 60 * 60;

/// 接頭辞を拒んだ理由。値そのものは含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteReason {
    /// `-` で始まる（rclone のオプションに読まれる）。
    LeadingDash,
    /// `..` を含む。
    ParentRef,
    /// 末尾が `/`。
    TrailingSlash,
    /// `remote:` だけ（remote の最上位）。
    TopLevel,
    /// 先頭の `/` や `a//b` のような空の成分。
    EmptyComponent,
    /// `^[A-Za-z0-9_-]+:[A-Za-z0-9_/-]+$` に合わない。
    BadChars,
}

/// `remote:パス` の形の Drive 上の場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePath {
    remote: String,
    path: String,
}

impl RemotePath {
    pub fn parse(s: &str) -> Result<Self, RemoteReason> {
        if s.starts_with('-') {
            return Err(RemoteReason::LeadingDash);
        }
        if s.contains("..") {
            return Err(RemoteReason::ParentRef);
        }
        if s.ends_with('/') {
            return Err(RemoteReason::TrailingSlash);
        }
        let Some((remote, path)) = s.split_once(':') else {
            return Err(RemoteReason::BadChars);
        };
        if path.is_empty() && !remote.is_empty() {
            return Err(RemoteReason::TopLevel);
        }
        let name_ok = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
        if remote.is_empty()
            || !remote.bytes().all(name_ok)
            || !path.bytes().all(|c| name_ok(c) || c == b'/')
        {
            return Err(RemoteReason::BadChars);
        }
        if path.starts_with('/') || path.contains("//") {
            return Err(RemoteReason::EmptyComponent);
        }
        Ok(Self {
            remote: remote.to_owned(),
            path: path.to_owned(),
        })
    }

    /// `:` の前の remote 名。
    pub fn remote(&self) -> &str {
        &self.remote
    }

    /// `:` の後ろのパス（先頭・末尾に `/` は無い）。
    pub fn path(&self) -> &str {
        &self.path
    }

    /// 検査済みの名前を 1 つ足した場所（`remote:パス/名前`）。名前は呼び出し側で検査済みであること。
    pub fn join(&self, name: &str) -> String {
        format!("{}:{}/{name}", self.remote, self.path)
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.remote, self.path)
    }
}

/// 写真サービスの基底 URL。`http://ホスト[:ポート]` だけ（パス・クエリ・ユーザー情報なし）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedBase(String);

impl FeedBase {
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix("http://")?;
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        let host_ok = !host.is_empty()
            && !host.starts_with(['.', '-'])
            && !host.ends_with(['.', '-'])
            && host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-');
        let port_ok = port.is_none_or(|p| {
            !p.is_empty()
                && p.len() <= 5
                && p.bytes().all(|c| c.is_ascii_digit())
                && p.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n))
        });
        (host_ok && port_ok).then(|| Self(s.to_owned()))
    }

    /// 末尾に `/` を付けない基底 URL。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 設定の誤り。値は含めない（どの引数が、なぜ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsError {
    MissingOption(&'static str),
    NotAbsolute(&'static str),
    /// Drive の ID の形（`[A-Za-z0-9_-]`、1〜128 文字）でない。空も断る（`${VAR}` が空のとき）。
    InvalidDriveId(&'static str),
    /// 2 つのフォルダの ID が同じ。
    SameFolder(&'static str, &'static str),
    /// `--drive-endpoint` が loopback の http（`http://127.0.0.1:<port>`・`http://[::1]:<port>`）の形でない。
    InvalidEndpoint,
    InvalidFeedBase,
    InvalidTimeout(&'static str),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOption(o) => write!(f, "{o} が必要です"),
            Self::NotAbsolute(o) => write!(f, "{o} は正規化済みの絶対パス（`.`・`..`・`//`・末尾の `/` を含まない）で指定してください"),
            Self::InvalidDriveId(o) => write!(
                f,
                "{o} は Drive のフォルダの ID（英数字・_・- の 1〜128 文字）で指定してください"
            ),
            Self::SameFolder(a, b) => write!(f, "{a} と {b} は別のフォルダにしてください"),
            Self::InvalidEndpoint => EndpointError.fmt(f),
            Self::InvalidFeedBase => f.write_str(
                "--feed-base は http://ホスト[:ポート] の形にしてください（パス・クエリは付けられません）",
            ),
            Self::InvalidTimeout(o) => {
                write!(f, "{o} は 1〜{TIMEOUT_SECS_MAX} の秒数で指定してください")
            }
        }
    }
}

impl std::error::Error for SettingsError {}

/// 検査前の設定（引数の文字列のまま。無ければ `None`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawSettings {
    pub feed_base: Option<String>,
    pub state_dir: Option<String>,
    pub drive_credentials: Option<String>,
    pub folder_yamareco: Option<String>,
    pub folder_travel_journal: Option<String>,
    pub work_folder: Option<String>,
    pub drive_endpoint: Option<String>,
    pub drive_upload_timeout: Option<String>,
    pub drive_timeout: Option<String>,
}

/// 検査済みの設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub feed_base: FeedBase,
    pub state_dir: PathBuf,
    /// 資格情報の JSON のパス（絶対パス。読むだけで、書かない）。
    pub drive_credentials: PathBuf,
    /// 宛先 `yamareco` の親フォルダの Drive ID。
    pub folder_yamareco: String,
    /// 宛先 `travel-journal` の親フォルダの Drive ID。
    pub folder_travel_journal: String,
    /// 作業用のフォルダ（`incoming/`・`replaced/` を中に作る）の Drive ID。
    pub work_folder: String,
    /// 試験専用の通信先。無ければ本物の固定値。
    pub drive_endpoint: Option<Endpoints>,
    /// 写真を送る操作全体の期限（`--drive-upload-timeout`）。
    pub drive_upload_timeout: Duration,
    /// それ以外の Drive の操作の期限（`--drive-timeout`）。
    pub drive_timeout: Duration,
}

fn absolute(option: &'static str, value: Option<&str>) -> Result<PathBuf, SettingsError> {
    let value = value.ok_or(SettingsError::MissingOption(option))?;
    let path = PathBuf::from(value);
    if value.is_empty() || value.contains('\0') || !path.is_absolute() {
        return Err(SettingsError::NotAbsolute(option));
    }
    // 正規化済みの形だけを受け付ける（`.`・`..`・`//`・末尾の `/` を断る）。検査（リンクをたどって解決する）と
    // 実際に開くときとで、別のファイルを指すことがないようにする
    let normalized: PathBuf = path.components().collect();
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) || normalized.as_os_str() != path.as_os_str()
    {
        return Err(SettingsError::NotAbsolute(option));
    }
    Ok(path)
}

fn drive_id(option: &'static str, value: Option<&str>) -> Result<String, SettingsError> {
    let value = value.ok_or(SettingsError::MissingOption(option))?;
    if is_valid_drive_id(value) {
        Ok(value.to_owned())
    } else {
        Err(SettingsError::InvalidDriveId(option))
    }
}

fn timeout(
    option: &'static str,
    value: Option<&str>,
    default: u64,
) -> Result<Duration, SettingsError> {
    let secs = match value {
        None => default,
        Some(s) => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(SettingsError::InvalidTimeout(option));
            }
            s.parse::<u64>()
                .map_err(|_| SettingsError::InvalidTimeout(option))?
        }
    };
    if !(1..=TIMEOUT_SECS_MAX).contains(&secs) {
        return Err(SettingsError::InvalidTimeout(option));
    }
    Ok(Duration::from_secs(secs))
}

impl Settings {
    pub fn from_raw(raw: &RawSettings) -> Result<Self, SettingsError> {
        let feed_base = FeedBase::parse(raw.feed_base.as_deref().unwrap_or(DEFAULT_FEED_BASE))
            .ok_or(SettingsError::InvalidFeedBase)?;
        let state_dir = absolute(OPT_STATE_DIR, raw.state_dir.as_deref())?;
        let drive_credentials = absolute(OPT_DRIVE_CREDENTIALS, raw.drive_credentials.as_deref())?;
        let folder_yamareco = drive_id(OPT_FOLDER_YAMARECO, raw.folder_yamareco.as_deref())?;
        let folder_travel_journal = drive_id(
            OPT_FOLDER_TRAVEL_JOURNAL,
            raw.folder_travel_journal.as_deref(),
        )?;
        let work_folder = drive_id(OPT_WORK_FOLDER, raw.work_folder.as_deref())?;
        let drive_endpoint = raw
            .drive_endpoint
            .as_deref()
            .map(|e| Endpoints::loopback(e).map_err(|_| SettingsError::InvalidEndpoint))
            .transpose()?;
        let drive_upload_timeout = timeout(
            OPT_DRIVE_UPLOAD_TIMEOUT,
            raw.drive_upload_timeout.as_deref(),
            DEFAULT_UPLOAD_TIMEOUT_SECS,
        )?;
        let drive_timeout = timeout(
            OPT_DRIVE_TIMEOUT,
            raw.drive_timeout.as_deref(),
            DEFAULT_DRIVE_TIMEOUT_SECS,
        )?;
        let named = [
            (OPT_FOLDER_YAMARECO, &folder_yamareco),
            (OPT_FOLDER_TRAVEL_JOURNAL, &folder_travel_journal),
            (OPT_WORK_FOLDER, &work_folder),
        ];
        for (i, (name_a, a)) in named.iter().enumerate() {
            for (name_b, b) in &named[i + 1..] {
                if a == b {
                    return Err(SettingsError::SameFolder(name_a, name_b));
                }
            }
        }
        Ok(Self {
            feed_base,
            state_dir,
            drive_credentials,
            folder_yamareco,
            folder_travel_journal,
            work_folder,
            drive_endpoint,
            drive_upload_timeout,
            drive_timeout,
        })
    }

    /// 書き出し先の種類に対応する、宛先の親フォルダの Drive ID。
    pub fn prefix_id(&self, destination: Destination) -> &str {
        match destination {
            Destination::Yamareco => &self.folder_yamareco,
            Destination::TravelJournal => &self.folder_travel_journal,
        }
    }

    /// Drive への通信先（試験専用の `--drive-endpoint` があればそれ、無ければ本物の固定値）。
    pub fn endpoints(&self) -> Endpoints {
        self.drive_endpoint
            .clone()
            .unwrap_or_else(Endpoints::production)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(f: impl FnOnce(&mut RawSettings)) -> RawSettings {
        let mut r = RawSettings {
            state_dir: Some("/x/state".into()),
            drive_credentials: Some("/x/cred/drive-credentials".into()),
            folder_yamareco: Some("FolderYamareco1".into()),
            folder_travel_journal: Some("FolderTravel2".into()),
            work_folder: Some("FolderWork3".into()),
            ..RawSettings::default()
        };
        f(&mut r);
        r
    }

    fn remote_err(s: &str) -> RemoteReason {
        RemotePath::parse(s).expect_err(s)
    }

    #[test]
    fn defaults_are_the_documented_values() {
        let s = Settings::from_raw(&raw(|_| {})).unwrap();
        assert_eq!(s.feed_base.as_str(), "http://127.0.0.1:8080");
        assert_eq!(s.drive_upload_timeout, Duration::from_secs(45 * 60));
        assert_eq!(s.drive_timeout, Duration::from_secs(5 * 60));
        assert_eq!(s.drive_endpoint, None);
        assert_eq!(s.endpoints(), Endpoints::production());
        assert_eq!(s.prefix_id(Destination::Yamareco), "FolderYamareco1");
        assert_eq!(s.prefix_id(Destination::TravelJournal), "FolderTravel2");
        assert_eq!(s.work_folder, "FolderWork3");
    }

    #[test]
    fn paths_and_ids_without_defaults_are_required() {
        let missing = |f: fn(&mut RawSettings)| Settings::from_raw(&raw(f)).unwrap_err();
        assert_eq!(
            missing(|r| r.drive_credentials = None),
            SettingsError::MissingOption(OPT_DRIVE_CREDENTIALS)
        );
        assert_eq!(
            missing(|r| r.state_dir = None),
            SettingsError::MissingOption(OPT_STATE_DIR)
        );
        assert_eq!(
            missing(|r| r.folder_yamareco = None),
            SettingsError::MissingOption(OPT_FOLDER_YAMARECO)
        );
        assert_eq!(
            missing(|r| r.folder_travel_journal = None),
            SettingsError::MissingOption(OPT_FOLDER_TRAVEL_JOURNAL)
        );
        assert_eq!(
            missing(|r| r.work_folder = None),
            SettingsError::MissingOption(OPT_WORK_FOLDER)
        );
        assert_eq!(
            missing(|r| r.state_dir = Some("state".into())),
            SettingsError::NotAbsolute(OPT_STATE_DIR)
        );
        assert_eq!(
            missing(|r| r.drive_credentials = Some(String::new())),
            SettingsError::NotAbsolute(OPT_DRIVE_CREDENTIALS)
        );
        // 正規化されていない絶対パス（検査と実際に開くときとで別のファイルを指しうる）も断る
        for bad in [
            "/srv/a/../credentials",
            "/srv/./credentials",
            "/srv//credentials",
            "/srv/credentials/",
            "/srv/credentials/.",
        ] {
            assert_eq!(
                Settings::from_raw(&raw(|r| r.drive_credentials = Some(bad.into()))).unwrap_err(),
                SettingsError::NotAbsolute(OPT_DRIVE_CREDENTIALS),
                "{bad}"
            );
        }
    }

    #[test]
    fn folder_ids_must_look_like_drive_ids_and_never_be_empty() {
        // unit の `${VAR}` が空のとき（変数が未設定・空）は、空の引数が 1 つ残る。形で断る（終了コード 2）
        type Setter = fn(&mut RawSettings, String);
        let setters: [(&str, Setter); 3] = [
            (OPT_FOLDER_YAMARECO, |r, v| r.folder_yamareco = Some(v)),
            (OPT_FOLDER_TRAVEL_JOURNAL, |r, v| {
                r.folder_travel_journal = Some(v)
            }),
            (OPT_WORK_FOLDER, |r, v| r.work_folder = Some(v)),
        ];
        for bad in ["", "a b", "a.b", "a/b", "日本", "a'b", &"a".repeat(129)] {
            for (opt, set) in &setters {
                let e = Settings::from_raw(&raw(|r| set(r, bad.to_owned()))).unwrap_err();
                assert_eq!(e, SettingsError::InvalidDriveId(opt), "{bad:?}");
            }
        }
        let ok = "A".repeat(128);
        assert!(Settings::from_raw(&raw(|r| r.work_folder = Some(ok))).is_ok());
    }

    #[test]
    fn the_three_folders_must_be_distinct() {
        let err = |y: &str, t: &str, w: &str| {
            Settings::from_raw(&raw(|r| {
                r.folder_yamareco = Some(y.into());
                r.folder_travel_journal = Some(t.into());
                r.work_folder = Some(w.into());
            }))
        };
        assert!(err("a", "b", "c").is_ok());
        // 前方一致は同じではない
        assert!(err("a", "ab", "a-work").is_ok());
        assert_eq!(
            err("a", "a", "c"),
            Err(SettingsError::SameFolder(
                OPT_FOLDER_YAMARECO,
                OPT_FOLDER_TRAVEL_JOURNAL
            ))
        );
        assert_eq!(
            err("a", "b", "a"),
            Err(SettingsError::SameFolder(
                OPT_FOLDER_YAMARECO,
                OPT_WORK_FOLDER
            ))
        );
        assert_eq!(
            err("a", "b", "b"),
            Err(SettingsError::SameFolder(
                OPT_FOLDER_TRAVEL_JOURNAL,
                OPT_WORK_FOLDER
            ))
        );
    }

    #[test]
    fn the_drive_endpoint_is_loopback_http_only() {
        for ok in ["http://127.0.0.1:8123", "http://[::1]:9"] {
            let s = Settings::from_raw(&raw(|r| r.drive_endpoint = Some(ok.into()))).unwrap();
            assert_eq!(s.endpoints(), Endpoints::loopback(ok).unwrap());
        }
        for bad in [
            "",
            "https://127.0.0.1:8123",
            "http://localhost:8123",
            "http://192.0.2.1:8123",
            "http://127.0.0.1",
            "http://127.0.0.1:8123/",
            "http://127.0.0.1:8123/x",
            "http://user@127.0.0.1:8123",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://www.googleapis.com",
        ] {
            assert_eq!(
                Settings::from_raw(&raw(|r| r.drive_endpoint = Some(bad.into()))),
                Err(SettingsError::InvalidEndpoint),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn remote_prefixes_follow_the_documented_conditions() {
        // rclone 版（3 段目で消す）の試験が使う `remote:パス` の形
        assert!(RemotePath::parse("gdrive:yamareco").is_ok());
        assert!(RemotePath::parse("gdrive:a/b/c").is_ok());
        assert!(RemotePath::parse("my_remote-1:x_y-z").is_ok());
        assert_eq!(remote_err("-gdrive:x"), RemoteReason::LeadingDash);
        assert_eq!(remote_err("gdrive:../x"), RemoteReason::ParentRef);
        assert_eq!(remote_err("gdrive:a/.."), RemoteReason::ParentRef);
        assert_eq!(remote_err("gdrive:yamareco/"), RemoteReason::TrailingSlash);
        assert_eq!(remote_err("gdrive:"), RemoteReason::TopLevel);
        assert_eq!(remote_err("gdrive:/a"), RemoteReason::EmptyComponent);
        assert_eq!(remote_err("gdrive:a//b"), RemoteReason::EmptyComponent);
        for bad in [
            "",
            "yamareco",
            ":yamareco",
            "gdrive:a b",
            "gdrive:a.b",
            "gdrive:a:b",
            "gd rive:a",
            "gdrive:日本語",
            "gdrive:a\n",
            "gdrive:a\0b",
            "gdrive:a?b",
            "g/drive:a",
        ] {
            assert!(RemotePath::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn feed_base_is_a_plain_http_origin() {
        for ok in [
            "http://127.0.0.1:8080",
            "http://127.0.0.1",
            "http://localhost:1",
            "http://feed.example:65535",
        ] {
            assert_eq!(FeedBase::parse(ok).unwrap().as_str(), ok);
        }
        for bad in [
            "",
            "https://127.0.0.1:8080",
            "ftp://127.0.0.1",
            "127.0.0.1:8080",
            "http://",
            "http://127.0.0.1:8080/",
            "http://127.0.0.1:8080/api",
            "http://127.0.0.1:8080?x=1",
            "http://127.0.0.1:8080#x",
            "http://user@127.0.0.1:8080",
            "http://127.0.0.1:",
            "http://127.0.0.1:0",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:80a",
            "http://[::1]:8080",
            "http://a b",
            "HTTP://127.0.0.1",
            "http://.example",
            "http://127.0.0.1:8080 ",
        ] {
            assert_eq!(FeedBase::parse(bad), None, "{bad:?}");
        }
        assert_eq!(
            Settings::from_raw(&raw(|r| r.feed_base = Some("https://x".into()))),
            Err(SettingsError::InvalidFeedBase)
        );
    }

    #[test]
    fn timeouts_are_whole_seconds_in_range() {
        let ok = Settings::from_raw(&raw(|r| {
            r.drive_upload_timeout = Some("90".into());
            r.drive_timeout = Some("1".into());
        }))
        .unwrap();
        assert_eq!(ok.drive_upload_timeout, Duration::from_secs(90));
        assert_eq!(ok.drive_timeout, Duration::from_secs(1));
        for bad in [
            "",
            "0",
            "-1",
            "1.5",
            "abc",
            "86401",
            "99999999999999999999",
            " 5",
            "5s",
        ] {
            assert_eq!(
                Settings::from_raw(&raw(|r| r.drive_timeout = Some(bad.into()))),
                Err(SettingsError::InvalidTimeout(OPT_DRIVE_TIMEOUT)),
                "{bad:?}"
            );
        }
        assert_eq!(
            Settings::from_raw(&raw(|r| r.drive_upload_timeout = Some("0".into()))),
            Err(SettingsError::InvalidTimeout(OPT_DRIVE_UPLOAD_TIMEOUT))
        );
    }
}
