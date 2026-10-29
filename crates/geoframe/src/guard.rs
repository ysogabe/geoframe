//! 書き込みを始める前の守り（Python 版が書き手のあいだの二重書き込みと、root での実行の拒否）。
//!
//! # なぜ要るか
//!
//! `GEOFRAME_INGEST=1` を立てると Rust は DB の移行・`.cache.json` の更新・`.dl_*` の掃除をする。
//! Python 版（`geoframe-server`）が動いているあいだ、書き手は Python で、Python は Rust の
//! `.ingest.lock` を見ない。そこへ Rust が書くと、Python のダウンロード中の `.dl_*` を消し、
//! `.cache.json` の更新が競合する。`geoframe-rs.service` の `Conflicts=`／`After=` は systemd 経由の起動だけ
//! に効くので、手で打つ書き込み系の CLI・直接の起動・設定の書き換えのような経路は、バイナリ自身が守る。
//! 書き込みの前に次の 2 つを確かめ、満たせなければ何も書かずに断る。
//!
//! 1. **root（実効 UID 0）で実行していない**。root で書くと作られるファイル（`catalog.db-wal`・`.cache.json`・
//!    `derivatives/` など）が root の所有になり、サービス（`User=geoframe`）が書けなくなる。終了コード 2。
//!    サービスと同じユーザーで CLI を動かすには `deploy/geoframe-admin.sh` を使う。
//! 2. Python 版（[`PYTHON_UNIT`]）が非稼働と**確認できる**。`systemctl show` の結果の `ActiveState` が
//!    `inactive`／`failed` のときだけ通す（`LoadState` は見ない。unit ファイルが無い `not-found` でも、
//!    動いているサービスは動き続けうるので、`not-found/active` も断る。Python が入っていない機体は
//!    `not-found/inactive` で通る）。稼働中・未知の状態は終了コード 2（再起動しても直らない）。
//!    状態を**取得できない**（`systemctl` が無い／使えない／時間内に答えない／出力が読めない・
//!    `LoadState`／`ActiveState` の行が欠けている・空・重複している）は終了コード 1: 判定できないものは
//!    通さないが、systemd がまだ使えない起動直後のような一時的な失敗かもしれないので、unit の
//!    `Restart=on-failure` で再試行させる（`StartLimitIntervalSec=`／`StartLimitBurst=` が上限を持つ）。
//!
//! 検査は起動時の 1 回だけ。その後に Python が起動された場合は検知しない。
//!
//! 検証環境の印（`/etc/geoframe/staging`）の検査は P5 で外した。切替のあとは本番が Rust の唯一の書き手で、
//! 印は「本番では書かない」ための暫定だったため。Python 版の検査は、Python へ戻したあとの運用中も要るので残す。
//!
//! # 置き場所
//!
//! [`crate::ingest::start`] の先頭（データ置き場の作成より前）。`serve`（取り込みつき）も `sync`（実行）も
//! `organize` なども、`ingest::start` を通るものは全てここを通る。`serve` は取り込みが無効（読み取り専用）でも
//! root では起動しない（[`refuse_root`]。`-wal`／`-shm`・`tile_cache/` が root の所有になるため。Python の検査は
//! 書かないので通さない）。`sync --dry-run` は書き込み接続を作らず行・写真・キャッシュ・ロックを変えないので、
//! ガードを通さない（SQLite が読み取りでも作る WAL 関連ファイルだけは、できうる）。ガードを通さないのは
//! 書かないからで、本番機で実行してよいという意味ではない。向けるのは検証環境の本番コピー（のバックアップの
//! コピー）だけで、本番の DB には向けない（docs/rust-migration/plan.md §2。鍵があれば Drive にも接続する）。
//!
//! # 試験用の差し替え
//!
//! デバッグビルド（`cargo test` が作るバイナリ）に限り、環境変数 `GEOFRAME_TEST_PYTHON_STATE`
//! （`<LoadState>/<ActiveState>`。形が違う値は取得失敗）と `GEOFRAME_TEST_EUID`（実効 UID の代わりの数値。
//! 数値でなければ無視して本物を使う）で差し替えられる。リリースビルド（配布するもの）にはこのコードが無く、
//! 環境変数は無視される。

use std::fmt;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 本番の書き手（Python 版）の unit。
pub const PYTHON_UNIT: &str = "geoframe-server.service";

/// `systemctl` の置き場所の候補（unit の `PATH` に頼らない）。
const SYSTEMCTL: [&str; 2] = ["/usr/bin/systemctl", "/bin/systemctl"];

/// `systemctl show` が答えるまで待つ上限。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// root（実効 UID 0）で実行している。
    Root,
    /// Python 版が非稼働と確認できない（稼働中・不明な状態）。状態名だけを持つ。
    PythonNotStopped(String),
    /// Python 版の状態を取得できない。理由は定型文。
    PythonUnknown(&'static str),
}

impl GuardError {
    /// 再起動しても直らない拒否か。`false`（状態を取得できない）は一時的な失敗でありうるので、
    /// 終了コード 1 にして unit の `Restart=on-failure` で再試行させる。
    pub fn is_permanent(&self) -> bool {
        !matches!(self, Self::PythonUnknown(_))
    }
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str(
                "root で実行しているので、書き込みを始めません。作られるファイルが root の所有になり、\
                 サービス（geoframe ユーザー）が書けなくなります。サービスと同じユーザー・設定で動かすには \
                 deploy/geoframe-admin.sh を使ってください",
            ),
            Self::PythonNotStopped(state) => write!(
                f,
                "Python 版（{PYTHON_UNIT}）が非稼働と確認できない（状態: {state}）ので、書き込みを始めません。\
                 二重に書くと Python のダウンロードと .cache.json を壊します"
            ),
            Self::PythonUnknown(why) => write!(
                f,
                "Python 版（{PYTHON_UNIT}）の状態を取得できない（{why}）ので、書き込みを始めません。\
                 非稼働と確認できないものは通しません"
            ),
        }
    }
}

impl std::error::Error for GuardError {}

/// 実効 UID の取り方。
#[derive(Clone, Copy)]
enum Euid {
    /// 本物（`geteuid`）。
    Real,
    /// 試験用の固定値。
    #[cfg(any(test, debug_assertions))]
    Fixed(u32),
}

impl Euid {
    /// 本物。デバッグビルドだけは `GEOFRAME_TEST_EUID` で差し替えられる。
    fn from_env() -> Self {
        #[cfg(debug_assertions)]
        if let Some(uid) = std::env::var("GEOFRAME_TEST_EUID")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
        {
            return Self::Fixed(uid);
        }
        Self::Real
    }

    fn get(self) -> u32 {
        match self {
            Self::Real => crate::sysinfo::effective_uid(),
            #[cfg(any(test, debug_assertions))]
            Self::Fixed(uid) => uid,
        }
    }

    fn check(self) -> Result<(), GuardError> {
        if self.get() == 0 {
            Err(GuardError::Root)
        } else {
            Ok(())
        }
    }
}

/// root で実行していれば断る。`serve` は取り込みが無効でも、`-wal`／`-shm` と `tile_cache/` を
/// root の所有で作ってしまうので、起動の最初にこれを通す。
pub fn refuse_root() -> Result<(), GuardError> {
    Euid::from_env().check()
}

enum Probe {
    /// `systemctl show`（本番の経路）。
    Systemctl,
    /// 試験用の固定の答え（`None` は取得失敗）。
    #[cfg(any(test, debug_assertions))]
    Fixed(Option<(String, String)>),
    /// 試験用: `sh -c` のスクリプトの標準出力を `systemctl show` の出力として読む。
    #[cfg(test)]
    Shell(&'static str),
}

pub struct Guard {
    euid: Euid,
    probe: Probe,
    #[cfg(test)]
    skip: bool,
}

impl Guard {
    /// 実際に使うガード。root の判定は `geteuid`、Python の判定は `systemctl show`。
    /// デバッグビルドだけは、上の「試験用の差し替え」を読む。
    pub fn production() -> Self {
        let guard = Self {
            euid: Euid::Real,
            probe: Probe::Systemctl,
            #[cfg(test)]
            skip: false,
        };
        #[cfg(debug_assertions)]
        let guard = guard.with_test_overrides(|key| std::env::var(key).ok());
        guard
    }

    #[cfg(debug_assertions)]
    fn with_test_overrides(mut self, get: impl Fn(&str) -> Option<String>) -> Self {
        if let Some(uid) = get("GEOFRAME_TEST_EUID").and_then(|v| v.parse::<u32>().ok()) {
            self.euid = Euid::Fixed(uid);
        }
        if let Some(state) = get("GEOFRAME_TEST_PYTHON_STATE").filter(|v| !v.is_empty()) {
            self.probe = Probe::Fixed(
                state
                    .split_once('/')
                    .map(|(load, active)| (load.to_string(), active.to_string())),
            );
        }
        self
    }

    /// 検査しない（`ingest` の単体テストが合成の置き場で `start` を呼ぶため）。
    #[cfg(test)]
    pub fn skip() -> Self {
        Self {
            euid: Euid::Fixed(1000),
            probe: Probe::Systemctl,
            skip: true,
        }
    }

    /// Python 版が稼働中と答える、必ず断るガード（`ingest` の単体テスト用）。
    #[cfg(test)]
    pub fn refusing_for_test() -> Self {
        Self {
            euid: Euid::Fixed(1000),
            probe: Probe::Fixed(Some(("loaded".into(), "active".into()))),
            skip: false,
        }
    }

    /// root でないこと・Python 版の非稼働を確かめる。何も書かない。
    pub fn check(&self) -> Result<(), GuardError> {
        #[cfg(test)]
        if self.skip {
            return Ok(());
        }
        self.euid.check()?;
        let (load, active) = self.probe.run()?;
        let verdict = decide(&load, &active);
        if verdict.is_ok() {
            tracing::info!(
                "[guard] root ではないことと、{PYTHON_UNIT} が{}ことを確認しました",
                if load == "not-found" {
                    " 未導入である".to_string()
                } else {
                    format!(" {active} である")
                }
            );
        }
        verdict
    }
}

/// `ActiveState` から、通してよいかを決める（配布スクリプトの `preflight` と同じ）。
/// `LoadState` は見ない: unit ファイルを消して `daemon-reload` しても、動いているサービスは
/// 動き続けうる（`not-found` と `active` は両立する）ので、ロード状態からは停止を判断できない。
/// 未導入の機体は `not-found/inactive` になり、そのまま通る。
fn decide(_load: &str, active: &str) -> Result<(), GuardError> {
    match active {
        "inactive" | "failed" => Ok(()),
        other => Err(GuardError::PythonNotStopped(other.to_string())),
    }
}

impl Probe {
    fn run(&self) -> Result<(String, String), GuardError> {
        match self {
            Self::Systemctl => {
                for path in SYSTEMCTL {
                    let mut command = Command::new(path);
                    command
                        .args(["show", "--property=LoadState,ActiveState", PYTHON_UNIT])
                        .env_clear()
                        .env("LC_ALL", "C");
                    match capture(&mut command) {
                        Err(CaptureError::NotFound) => continue,
                        Err(CaptureError::Failed(why)) => {
                            return Err(GuardError::PythonUnknown(why));
                        }
                        Ok(out) => return parse_show(&out),
                    }
                }
                Err(GuardError::PythonUnknown("systemctl が見つかりません"))
            }
            #[cfg(any(test, debug_assertions))]
            Self::Fixed(state) => state
                .clone()
                .ok_or(GuardError::PythonUnknown("試験用: 取得失敗")),
            #[cfg(test)]
            Self::Shell(script) => {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", script]);
                match capture(&mut command) {
                    Ok(out) => parse_show(&out),
                    Err(CaptureError::NotFound) => {
                        Err(GuardError::PythonUnknown("試験用: sh が無い"))
                    }
                    Err(CaptureError::Failed(why)) => Err(GuardError::PythonUnknown(why)),
                }
            }
        }
    }
}

enum CaptureError {
    NotFound,
    Failed(&'static str),
}

/// コマンドを動かし、標準出力を返す。成功終了でなければ・時間内に終わらなければ失敗。
fn capture(command: &mut Command) -> Result<String, CaptureError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => CaptureError::NotFound,
            _ => CaptureError::Failed("systemctl を起動できません"),
        })?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CaptureError::Failed("systemctl が時間内に答えません"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err(CaptureError::Failed("systemctl の終了を確認できません")),
        }
    };
    if !status.success() {
        return Err(CaptureError::Failed("systemctl が失敗しました"));
    }
    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or(CaptureError::Failed("systemctl の出力を読めません"))?
        .take(4096)
        .read_to_string(&mut out)
        .map_err(|_| CaptureError::Failed("systemctl の出力を読めません"))?;
    Ok(out)
}

/// `systemctl show --property=LoadState,ActiveState` の出力から 2 つの値を取る。
/// `LoadState=`・`ActiveState=` はそれぞれ**ちょうど 1 行**で、値が空でないときだけ受け入れる。
/// 欠けている・空・重複（同じ値の重複も）は失敗: 先頭だけを使うと、矛盾する行
/// （`ActiveState=inactive` のあとに `ActiveState=active`）を「止まっている」と取り違えるため。
fn parse_show(out: &str) -> Result<(String, String), GuardError> {
    let find = |key: &str| {
        let mut lines = out.lines().filter_map(|line| line.strip_prefix(key));
        match (lines.next(), lines.next()) {
            (Some(value), None) if !value.is_empty() => Some(value.to_string()),
            _ => None,
        }
    };
    match (find("LoadState="), find("ActiveState=")) {
        (Some(load), Some(active)) => Ok((load, active)),
        _ => Err(GuardError::PythonUnknown(
            "systemctl の出力の状態が欠けている・空・重複しています",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(probe: Probe) -> Guard {
        Guard {
            euid: Euid::Fixed(1000),
            probe,
            skip: false,
        }
    }

    fn fixed(load: &str, active: &str) -> Probe {
        Probe::Fixed(Some((load.into(), active.into())))
    }

    #[test]
    fn root_is_refused_even_if_python_is_stopped() {
        let g = Guard {
            euid: Euid::Fixed(0),
            probe: fixed("loaded", "inactive"),
            skip: false,
        };
        assert_eq!(g.check(), Err(GuardError::Root));
        assert_eq!(Euid::Fixed(0).check(), Err(GuardError::Root));
        assert_eq!(Euid::Fixed(1000).check(), Ok(()));
    }

    #[test]
    fn a_stopped_python_passes_without_any_marker() {
        for (load, active) in [
            ("loaded", "inactive"),
            ("loaded", "failed"),
            ("not-found", "inactive"),
        ] {
            assert_eq!(
                guard(fixed(load, active)).check(),
                Ok(()),
                "{load}/{active}"
            );
        }
    }

    #[test]
    fn a_running_or_unknown_python_is_refused() {
        // unit ファイルを消して daemon-reload しても、動いているサービスは止まらない
        for active in [
            "active",
            "activating",
            "reloading",
            "deactivating",
            "maintenance",
            "something-new",
        ] {
            for load in ["loaded", "not-found"] {
                assert_eq!(
                    guard(fixed(load, active)).check(),
                    Err(GuardError::PythonNotStopped(active.into())),
                    "{load}/{active}"
                );
            }
        }
    }

    #[test]
    fn an_unavailable_state_is_refused_not_passed() {
        assert!(matches!(
            guard(Probe::Fixed(None)).check(),
            Err(GuardError::PythonUnknown(_))
        ));
    }

    #[test]
    fn only_an_unreadable_state_is_worth_retrying() {
        // 稼働中・root は再起動しても直らない（終了コード 2）。状態を取れないのは再試行に意味がある（終了コード 1）
        assert!(GuardError::Root.is_permanent());
        assert!(GuardError::PythonNotStopped("active".into()).is_permanent());
        assert!(!GuardError::PythonUnknown("x").is_permanent());
    }

    #[test]
    fn the_systemctl_output_is_parsed_strictly() {
        let ok = |script| guard(Probe::Shell(script)).check();
        assert_eq!(
            ok("printf 'LoadState=loaded\\nActiveState=inactive\\n'"),
            Ok(())
        );
        assert_eq!(
            ok("printf 'ActiveState=failed\\nLoadState=loaded\\n'"),
            Ok(())
        );
        assert_eq!(
            ok("printf 'LoadState=not-found\\nActiveState=inactive\\n'"),
            Ok(())
        );
        assert_eq!(
            ok("printf 'LoadState=loaded\\nActiveState=active\\n'"),
            Err(GuardError::PythonNotStopped("active".into()))
        );
        assert_eq!(
            ok("printf 'LoadState=not-found\\nActiveState=active\\n'"),
            Err(GuardError::PythonNotStopped("active".into()))
        );
        // 空・欠け・値が空・異常終了・出力なしの成功は、全て不明として断る
        for script in [
            "true",
            "printf 'LoadState=loaded\\n'",
            "printf 'LoadState=loaded\\nActiveState=\\n'",
            "printf 'LoadState=\\nActiveState=inactive\\n'",
            "printf 'LoadState=loaded\\nActiveState=inactive\\n'; exit 1",
            "echo oops >&2; exit 3",
            // 重複は、矛盾する行（先頭が止まっていても後ろが稼働中）も、同じ値の重複も断る
            "printf 'LoadState=loaded\\nActiveState=inactive\\nActiveState=active\\n'",
            "printf 'LoadState=loaded\\nActiveState=active\\nActiveState=inactive\\n'",
            "printf 'LoadState=loaded\\nActiveState=inactive\\nActiveState=inactive\\n'",
            "printf 'LoadState=loaded\\nLoadState=not-found\\nActiveState=inactive\\n'",
            "printf 'LoadState=loaded\\nLoadState=loaded\\nActiveState=inactive\\n'",
            // 空の行が混ざっても、1 行だけではないので断る
            "printf 'LoadState=loaded\\nActiveState=\\nActiveState=inactive\\n'",
        ] {
            assert!(
                matches!(ok(script), Err(GuardError::PythonUnknown(_))),
                "{script}"
            );
        }
    }

    #[test]
    fn a_hung_systemctl_is_killed_and_refused() {
        // 上限（5 秒）まで待つので、このテストだけは数秒かかる
        let started = Instant::now();
        let result = guard(Probe::Shell("sleep 30")).check();
        assert!(matches!(result, Err(GuardError::PythonUnknown(_))));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn the_refusal_messages_carry_no_values() {
        let text = GuardError::PythonNotStopped("active".into()).to_string();
        assert!(text.contains("geoframe-server.service") && text.contains("active"));
        let text = GuardError::Root.to_string();
        assert!(text.contains("root") && text.contains("geoframe-admin.sh"));
        let text = GuardError::PythonUnknown("systemctl が見つかりません").to_string();
        assert!(text.contains("geoframe-server.service"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_overrides_replace_the_checks_but_never_remove_them() {
        let base = || Guard {
            euid: Euid::Real,
            probe: Probe::Systemctl,
            skip: false,
        };
        let env = |uid: &'static str, state: &'static str| {
            move |key: &str| match key {
                "GEOFRAME_TEST_EUID" => Some(uid.to_string()),
                "GEOFRAME_TEST_PYTHON_STATE" => Some(state.to_string()),
                _ => None,
            }
        };
        // 差し替えても、root なら断り、稼働中なら断る
        assert_eq!(
            base()
                .with_test_overrides(env("0", "loaded/inactive"))
                .check(),
            Err(GuardError::Root)
        );
        assert_eq!(
            base()
                .with_test_overrides(env("1000", "loaded/inactive"))
                .check(),
            Ok(())
        );
        assert_eq!(
            base()
                .with_test_overrides(env("1000", "loaded/active"))
                .check(),
            Err(GuardError::PythonNotStopped("active".into()))
        );
        // 値の形が違えば取得失敗として断る（通さない）
        assert!(matches!(
            base().with_test_overrides(env("1000", "garbage")).check(),
            Err(GuardError::PythonUnknown(_))
        ));
    }
}
