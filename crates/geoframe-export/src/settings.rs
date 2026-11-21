//! 設定（起動引数）の検査（design.md 4.2「rclone の呼び方」、6.2）。
//!
//! 接頭辞（`--remote-*`）と作業用のフォルダ（`--work-root`）は、Drive 上のパスを組み立てる材料なので、起動時に厳しく
//! 検査する。呼び出し側の文字列は Drive のパスに入れない（宛先は固定の接頭辞＋検査済みのフォルダ名だけ）。
//! 実パス（rclone・設定ファイル・状態ディレクトリ）は文書・コードに書けないので、既定値を持たず必須にする。

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::request::Destination;

pub const OPT_FEED_BASE: &str = "--feed-base";
pub const OPT_RCLONE: &str = "--rclone";
pub const OPT_RCLONE_CONFIG: &str = "--rclone-config";
pub const OPT_REMOTE_YAMARECO: &str = "--remote-yamareco";
pub const OPT_REMOTE_TRAVEL_JOURNAL: &str = "--remote-travel-journal";
pub const OPT_WORK_ROOT: &str = "--work-root";
pub const OPT_STATE_DIR: &str = "--state-dir";
pub const OPT_RCLONE_COPY_TIMEOUT: &str = "--rclone-copy-timeout";
pub const OPT_RCLONE_TIMEOUT: &str = "--rclone-timeout";

pub const DEFAULT_FEED_BASE: &str = "http://127.0.0.1:8080";
pub const DEFAULT_REMOTE_YAMARECO: &str = "gdrive:yamareco";
pub const DEFAULT_REMOTE_TRAVEL_JOURNAL: &str = "gdrive:travel-journal";
pub const DEFAULT_WORK_ROOT: &str = "gdrive:geoframe-export-work";
/// 写真を送る `rclone copy` の既定の期限（45 分。14 日の最多を細い帯域で送る場合。design.md 2.9）。
pub const DEFAULT_COPY_TIMEOUT_SECS: u64 = 45 * 60;
/// `lsjson`・`moveto`・`purge`・`cat` の既定の期限（5 分）。
pub const DEFAULT_RCLONE_TIMEOUT_SECS: u64 = 5 * 60;
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

impl RemoteReason {
    const fn text(self) -> &'static str {
        match self {
            Self::LeadingDash => "- で始まっています",
            Self::ParentRef => ".. を含んでいます",
            Self::TrailingSlash => "末尾が / です",
            Self::TopLevel => "remote の最上位です（remote:パス の形にしてください）",
            Self::EmptyComponent => "先頭の / や // のような空の成分があります",
            Self::BadChars => "形が ^[A-Za-z0-9_-]+:[A-Za-z0-9_/-]+$ に合いません",
        }
    }
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

    fn components(&self) -> impl Iterator<Item = &str> {
        self.path.split('/')
    }

    /// `self` が `other` と同じか、その上の（祖先の）場所か。パスの成分で比べる（`a` と `ab` は入れ子にならない）。
    fn contains(&self, other: &Self) -> bool {
        let mut mine = self.components();
        let mut theirs = other.components();
        loop {
            match (mine.next(), theirs.next()) {
                (None, _) => return true,
                (Some(_), None) => return false,
                (Some(a), Some(b)) if a == b => {}
                _ => return false,
            }
        }
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
    Remote(&'static str, RemoteReason),
    /// 3 つの remote 名（`:` の前）が同じでない。
    RemoteNameMismatch,
    /// 2 つの場所が同じ、または一方がもう一方の中にある。
    Overlap(&'static str, &'static str),
    InvalidFeedBase,
    InvalidTimeout(&'static str),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOption(o) => write!(f, "{o} が必要です"),
            Self::NotAbsolute(o) => write!(f, "{o} は正規化済みの絶対パス（`.`・`..`・`//`・末尾の `/` を含まない）で指定してください"),
            Self::Remote(o, r) => write!(f, "{o}: {}", r.text()),
            Self::RemoteNameMismatch => f.write_str(
                "--remote-yamareco・--remote-travel-journal・--work-root の remote 名（: の前）は同じにしてください",
            ),
            Self::Overlap(a, b) => write!(f, "{a} と {b} は別の場所で、入れ子にしないでください"),
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
    pub rclone: Option<String>,
    pub rclone_config: Option<String>,
    pub remote_yamareco: Option<String>,
    pub remote_travel_journal: Option<String>,
    pub work_root: Option<String>,
    pub state_dir: Option<String>,
    pub rclone_copy_timeout: Option<String>,
    pub rclone_timeout: Option<String>,
}

/// 検査済みの設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub feed_base: FeedBase,
    pub rclone: PathBuf,
    pub rclone_config: PathBuf,
    pub remote_yamareco: RemotePath,
    pub remote_travel_journal: RemotePath,
    pub work_root: RemotePath,
    pub state_dir: PathBuf,
    pub rclone_copy_timeout: Duration,
    pub rclone_timeout: Duration,
}

fn absolute(option: &'static str, value: Option<&str>) -> Result<PathBuf, SettingsError> {
    let value = value.ok_or(SettingsError::MissingOption(option))?;
    let path = PathBuf::from(value);
    if value.is_empty() || value.contains('\0') || !path.is_absolute() {
        return Err(SettingsError::NotAbsolute(option));
    }
    // 正規化済みの形だけを受け付ける（`.`・`..`・`//`・末尾の `/` を断る）。検査（リンクをたどって解決する）と
    // 子プロセス（rclone は `..` を文字列の上で畳む）とで、別のファイルを指すことがないようにする
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

fn remote(
    option: &'static str,
    value: Option<&str>,
    default: &str,
) -> Result<RemotePath, SettingsError> {
    RemotePath::parse(value.unwrap_or(default)).map_err(|r| SettingsError::Remote(option, r))
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
        let rclone = absolute(OPT_RCLONE, raw.rclone.as_deref())?;
        let rclone_config = absolute(OPT_RCLONE_CONFIG, raw.rclone_config.as_deref())?;
        let state_dir = absolute(OPT_STATE_DIR, raw.state_dir.as_deref())?;
        let remote_yamareco = remote(
            OPT_REMOTE_YAMARECO,
            raw.remote_yamareco.as_deref(),
            DEFAULT_REMOTE_YAMARECO,
        )?;
        let remote_travel_journal = remote(
            OPT_REMOTE_TRAVEL_JOURNAL,
            raw.remote_travel_journal.as_deref(),
            DEFAULT_REMOTE_TRAVEL_JOURNAL,
        )?;
        let work_root = remote(OPT_WORK_ROOT, raw.work_root.as_deref(), DEFAULT_WORK_ROOT)?;
        let rclone_copy_timeout = timeout(
            OPT_RCLONE_COPY_TIMEOUT,
            raw.rclone_copy_timeout.as_deref(),
            DEFAULT_COPY_TIMEOUT_SECS,
        )?;
        let rclone_timeout = timeout(
            OPT_RCLONE_TIMEOUT,
            raw.rclone_timeout.as_deref(),
            DEFAULT_RCLONE_TIMEOUT_SECS,
        )?;

        // 別の remote や共有ドライブだと、移動がファイル単位のコピーと削除になる
        if remote_yamareco.remote() != remote_travel_journal.remote()
            || remote_yamareco.remote() != work_root.remote()
        {
            return Err(SettingsError::RemoteNameMismatch);
        }
        let named = [
            (OPT_REMOTE_YAMARECO, &remote_yamareco),
            (OPT_REMOTE_TRAVEL_JOURNAL, &remote_travel_journal),
            (OPT_WORK_ROOT, &work_root),
        ];
        for (i, (name_a, a)) in named.iter().enumerate() {
            for (name_b, b) in &named[i + 1..] {
                if a.contains(b) || b.contains(a) {
                    return Err(SettingsError::Overlap(name_a, name_b));
                }
            }
        }
        Ok(Self {
            feed_base,
            rclone,
            rclone_config,
            remote_yamareco,
            remote_travel_journal,
            work_root,
            state_dir,
            rclone_copy_timeout,
            rclone_timeout,
        })
    }

    /// 書き出し先の種類に対応する固定の接頭辞。
    pub fn prefix(&self, destination: Destination) -> &RemotePath {
        match destination {
            Destination::Yamareco => &self.remote_yamareco,
            Destination::TravelJournal => &self.remote_travel_journal,
        }
    }

    /// rclone 設定ファイルの中で、専用の client_id を確かめるセクションの名前（3 つの remote 名は同じ）。
    pub fn remote_name(&self) -> &str {
        self.remote_yamareco.remote()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(f: impl FnOnce(&mut RawSettings)) -> RawSettings {
        let mut r = RawSettings {
            rclone: Some("/x/rclone".into()),
            rclone_config: Some("/x/state/rclone/rclone.conf".into()),
            state_dir: Some("/x/state".into()),
            ..RawSettings::default()
        };
        f(&mut r);
        r
    }

    fn remote_err(s: &str) -> RemoteReason {
        RemotePath::parse(s).expect_err(s)
    }

    #[test]
    fn defaults_are_the_current_destinations() {
        let s = Settings::from_raw(&raw(|_| {})).unwrap();
        assert_eq!(s.remote_yamareco.to_string(), "gdrive:yamareco");
        assert_eq!(s.remote_travel_journal.to_string(), "gdrive:travel-journal");
        assert_eq!(s.work_root.to_string(), "gdrive:geoframe-export-work");
        assert_eq!(s.feed_base.as_str(), "http://127.0.0.1:8080");
        assert_eq!(s.rclone_copy_timeout, Duration::from_secs(45 * 60));
        assert_eq!(s.rclone_timeout, Duration::from_secs(5 * 60));
        assert_eq!(s.remote_name(), "gdrive");
        assert_eq!(
            s.prefix(Destination::Yamareco).to_string(),
            "gdrive:yamareco"
        );
        assert_eq!(
            s.prefix(Destination::TravelJournal).join("2030-03-10"),
            "gdrive:travel-journal/2030-03-10"
        );
    }

    #[test]
    fn paths_without_defaults_are_required_and_absolute() {
        let missing = |f: fn(&mut RawSettings)| Settings::from_raw(&raw(f)).unwrap_err();
        assert_eq!(
            missing(|r| r.rclone = None),
            SettingsError::MissingOption(OPT_RCLONE)
        );
        assert_eq!(
            missing(|r| r.rclone_config = None),
            SettingsError::MissingOption(OPT_RCLONE_CONFIG)
        );
        assert_eq!(
            missing(|r| r.state_dir = None),
            SettingsError::MissingOption(OPT_STATE_DIR)
        );
        assert_eq!(
            missing(|r| r.state_dir = Some("state".into())),
            SettingsError::NotAbsolute(OPT_STATE_DIR)
        );
        assert_eq!(
            missing(|r| r.rclone = Some("rclone".into())),
            SettingsError::NotAbsolute(OPT_RCLONE)
        );
        assert_eq!(
            missing(|r| r.rclone_config = Some(String::new())),
            SettingsError::NotAbsolute(OPT_RCLONE_CONFIG)
        );
        // 正規化されていない絶対パス（検査と rclone とで別のファイルを指しうる）も断る
        for bad in [
            "/srv/a/../rclone.conf",
            "/srv/./rclone.conf",
            "/srv//rclone.conf",
            "/srv/rclone/",
            "/srv/rclone/.",
        ] {
            assert_eq!(
                Settings::from_raw(&raw(|r| r.rclone_config = Some(bad.into()))).unwrap_err(),
                SettingsError::NotAbsolute(OPT_RCLONE_CONFIG),
                "{bad}"
            );
        }
    }

    #[test]
    fn remote_prefixes_follow_the_documented_conditions() {
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
        // 設定の誤りは引数名付きで出る
        let e =
            Settings::from_raw(&raw(|r| r.remote_yamareco = Some("gdrive:a/".into()))).unwrap_err();
        assert_eq!(
            e,
            SettingsError::Remote(OPT_REMOTE_YAMARECO, RemoteReason::TrailingSlash)
        );
    }

    #[test]
    fn the_three_locations_must_be_distinct_and_not_nested() {
        let err = |y: &str, t: &str, w: &str| {
            Settings::from_raw(&raw(|r| {
                r.remote_yamareco = Some(y.into());
                r.remote_travel_journal = Some(t.into());
                r.work_root = Some(w.into());
            }))
        };
        assert!(err("gdrive:a", "gdrive:b", "gdrive:c").is_ok());
        // 名前の前方一致は入れ子ではない
        assert!(err("gdrive:a", "gdrive:ab", "gdrive:a-work").is_ok());
        assert!(err("gdrive:a/x", "gdrive:a/y", "gdrive:b").is_ok());
        assert_eq!(
            err("gdrive:a", "gdrive:a", "gdrive:c"),
            Err(SettingsError::Overlap(
                OPT_REMOTE_YAMARECO,
                OPT_REMOTE_TRAVEL_JOURNAL
            ))
        );
        assert_eq!(
            err("gdrive:a", "gdrive:b", "gdrive:a"),
            Err(SettingsError::Overlap(OPT_REMOTE_YAMARECO, OPT_WORK_ROOT))
        );
        // 入れ子（どちら向きでも）
        assert!(matches!(
            err("gdrive:a/b", "gdrive:a", "gdrive:c"),
            Err(SettingsError::Overlap(..))
        ));
        assert!(matches!(
            err("gdrive:a", "gdrive:b", "gdrive:b/work"),
            Err(SettingsError::Overlap(
                OPT_REMOTE_TRAVEL_JOURNAL,
                OPT_WORK_ROOT
            ))
        ));
        assert!(matches!(
            err("gdrive:a", "gdrive:b", "gdrive:a/work"),
            Err(SettingsError::Overlap(OPT_REMOTE_YAMARECO, OPT_WORK_ROOT))
        ));
    }

    #[test]
    fn the_three_remote_names_must_match() {
        let e = |y: &str, t: &str, w: &str| {
            Settings::from_raw(&raw(|r| {
                r.remote_yamareco = Some(y.into());
                r.remote_travel_journal = Some(t.into());
                r.work_root = Some(w.into());
            }))
            .unwrap_err()
        };
        assert_eq!(
            e("gdrive:a", "gdrive:b", "other:c"),
            SettingsError::RemoteNameMismatch
        );
        assert_eq!(
            e("other:a", "gdrive:b", "gdrive:c"),
            SettingsError::RemoteNameMismatch
        );
        assert_eq!(
            e("gdrive:a", "gdrive2:b", "gdrive:c"),
            SettingsError::RemoteNameMismatch
        );
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
            r.rclone_copy_timeout = Some("90".into());
            r.rclone_timeout = Some("1".into());
        }))
        .unwrap();
        assert_eq!(ok.rclone_copy_timeout, Duration::from_secs(90));
        assert_eq!(ok.rclone_timeout, Duration::from_secs(1));
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
                Settings::from_raw(&raw(|r| r.rclone_timeout = Some(bad.into()))),
                Err(SettingsError::InvalidTimeout(OPT_RCLONE_TIMEOUT)),
                "{bad:?}"
            );
        }
        assert_eq!(
            Settings::from_raw(&raw(|r| r.rclone_copy_timeout = Some("0".into()))),
            Err(SettingsError::InvalidTimeout(OPT_RCLONE_COPY_TIMEOUT))
        );
    }
}
