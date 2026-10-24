//! 本番への誤指定を、書き込みを始める前に拒否するガード（P5 の切替まで）。
//!
//! # なぜ要るか
//!
//! `GEOFRAME_INGEST=1` を立てると Rust は DB の移行・`.cache.json` の更新・`.dl_*` の掃除をする。
//! 本番機では書き手は Python（`geoframe-server`）で、Python は Rust の `.ingest.lock` を見ない。
//! そこへ Rust が書くと、Python のダウンロード中の `.dl_*` を消し、`.cache.json` の更新が競合する。
//! 配布スクリプト（`deploy/rust-deploy.sh`）の拒否は、直接の起動や設定ファイルの書き換えには効かない。
//! だから**バイナリ自身が**、書き込みの前に次の 2 つを確かめる。どちらかが満たせなければ、何も書かずに
//! 終了コード 2 で断る（unit の `RestartPreventExitStatus=2` により再起動ループにならない）。
//!
//! 1. 検証環境の印 [`STAGING_MARKER`] が通常ファイルとしてある（リンクは不可。配布スクリプトと同じ）。
//!    本番機には印が無いので、ここで必ず止まる。
//! 2. Python 版（[`PYTHON_UNIT`]）が非稼働と**確認できる**。`systemctl show` の結果の `ActiveState` が
//!    `inactive`／`failed` のときだけ通す（`LoadState` は見ない。unit ファイルが無い `not-found` でも、
//!    動いているサービスは動き続けうるので、`not-found/active` も断る。Python が入っていない機体は
//!    `not-found/inactive` で通る）。稼働中・状態不明・`systemctl` が無い／使えない／時間内に答えない／
//!    出力が読めない・`LoadState`／`ActiveState` の行が欠けている・空・重複している、は全て断る
//!    （判定できないから素通し、にはしない）。
//!
//! 検査は起動時の 1 回だけ。その後に Python が起動された場合は検知しない。
//!
//! # 置き場所
//!
//! [`crate::ingest::start`] の先頭（データ置き場の作成より前）。`serve` も `sync`（実行）もここを通る。
//! `sync --dry-run` は書き込み接続を作らず行・写真・キャッシュ・ロックを変えないので、ガードを通さない
//! （SQLite が読み取りでも作る WAL 関連ファイルだけは、できうる）。
//!
//! # 試験用の差し替え
//!
//! デバッグビルド（`cargo test` が作るバイナリ）に限り、環境変数 `GEOFRAME_TEST_STAGING_MARKER`
//! （印のパス）と `GEOFRAME_TEST_PYTHON_STATE`（`<LoadState>/<ActiveState>`。`unavailable` は取得失敗）で
//! 差し替えられる。リリースビルド（配布するもの）にはこのコードが無く、環境変数は無視される。
//!
//! # P5 で外す
//!
//! 本番の切替のときに、このモジュールと `ingest::start` の呼び出しを外す（または本番の印に置き換える）。
//! 手順は docs/rust-migration/plan.md の P5。

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 検証環境の印。`deploy/rust-deploy.sh` が使うものと同じ。検証環境で一度だけ
/// `sudo install -d /etc/geoframe && sudo touch /etc/geoframe/staging` で作る。
pub const STAGING_MARKER: &str = "/etc/geoframe/staging";

/// 本番の書き手（Python 版）の unit。
pub const PYTHON_UNIT: &str = "geoframe-server.service";

/// `systemctl` の置き場所の候補（unit の `PATH` に頼らない）。
const SYSTEMCTL: [&str; 2] = ["/usr/bin/systemctl", "/bin/systemctl"];

/// `systemctl show` が答えるまで待つ上限。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// 検証環境の印が無い（または通常ファイルでない）。
    NoMarker(String),
    /// Python 版が非稼働と確認できない（稼働中・不明な状態）。状態名だけを持つ。
    PythonNotStopped(String),
    /// Python 版の状態を取得できない。理由は定型文。
    PythonUnknown(&'static str),
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMarker(path) => write!(
                f,
                "検証環境の印（{path}）が無いので、取り込み（GEOFRAME_INGEST=1）を始めません。P5 の切替まで、\
                 取り込みは検証環境でしか動きません。本番機（Python 版が書き手の機体）では動かせません"
            ),
            Self::PythonNotStopped(state) => write!(
                f,
                "Python 版（{PYTHON_UNIT}）が非稼働と確認できない（状態: {state}）ので、取り込みを始めません。\
                 二重に書くと Python のダウンロードと .cache.json を壊します"
            ),
            Self::PythonUnknown(why) => write!(
                f,
                "Python 版（{PYTHON_UNIT}）の状態を取得できない（{why}）ので、取り込みを始めません。\
                 非稼働と確認できないものは通しません"
            ),
        }
    }
}

impl std::error::Error for GuardError {}

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
    marker: PathBuf,
    probe: Probe,
    #[cfg(test)]
    skip: bool,
}

impl Guard {
    /// 実際に使うガード。印は [`STAGING_MARKER`]、Python の判定は `systemctl show`。
    /// デバッグビルドだけは、上の「試験用の差し替え」を読む。
    pub fn production() -> Self {
        let guard = Self {
            marker: PathBuf::from(STAGING_MARKER),
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
        if let Some(path) = get("GEOFRAME_TEST_STAGING_MARKER").filter(|v| !v.is_empty()) {
            self.marker = PathBuf::from(path);
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

    /// 印のパスだけを差し替えた、本番と同じ判定のガード（`ingest` の単体テスト用）。
    #[cfg(test)]
    pub fn production_with_marker_for_test(marker: PathBuf) -> Self {
        Self {
            marker,
            probe: Probe::Systemctl,
            skip: false,
        }
    }

    /// 検査しない（`ingest` の単体テストが合成の置き場で `start` を呼ぶため）。
    #[cfg(test)]
    pub fn skip() -> Self {
        Self {
            marker: PathBuf::new(),
            probe: Probe::Systemctl,
            skip: true,
        }
    }

    /// 検証環境の印と Python 版の非稼働を確かめる。何も書かない。
    pub fn check(&self) -> Result<(), GuardError> {
        #[cfg(test)]
        if self.skip {
            return Ok(());
        }
        check_marker(&self.marker)?;
        let (load, active) = self.probe.run()?;
        let verdict = decide(&load, &active);
        if verdict.is_ok() {
            tracing::info!(
                "[guard] 検証環境の印を確認しました。{PYTHON_UNIT} は {}",
                if load == "not-found" {
                    "未導入です"
                } else {
                    &active
                }
            );
        }
        verdict
    }
}

/// 配布スクリプトと同じ: 通常ファイルで、シンボリックリンクではない。
fn check_marker(marker: &std::path::Path) -> Result<(), GuardError> {
    match fs::symlink_metadata(marker) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        _ => Err(GuardError::NoMarker(marker.display().to_string())),
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
    use crate::testutil::TempDir;

    fn guard(dir: &TempDir, probe: Probe) -> Guard {
        Guard {
            marker: dir.path().join("staging"),
            probe,
            skip: false,
        }
    }

    fn fixed(load: &str, active: &str) -> Probe {
        Probe::Fixed(Some((load.into(), active.into())))
    }

    #[test]
    fn without_the_marker_it_refuses_even_if_python_is_stopped() {
        let dir = TempDir::new("guard-nomarker");
        let g = guard(&dir, fixed("loaded", "inactive"));
        assert!(matches!(g.check(), Err(GuardError::NoMarker(_))));
        // ディレクトリや壊れたリンクは印ではない
        fs::create_dir(dir.path().join("staging")).unwrap();
        assert!(matches!(g.check(), Err(GuardError::NoMarker(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_not_a_marker() {
        let dir = TempDir::new("guard-link");
        fs::write(dir.path().join("real"), b"").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("staging")).unwrap();
        let g = guard(&dir, fixed("loaded", "inactive"));
        assert!(matches!(g.check(), Err(GuardError::NoMarker(_))));
    }

    #[test]
    fn the_marker_and_a_stopped_python_pass() {
        let dir = TempDir::new("guard-pass");
        fs::write(dir.path().join("staging"), b"").unwrap();
        for (load, active) in [
            ("loaded", "inactive"),
            ("loaded", "failed"),
            ("not-found", "inactive"),
        ] {
            assert_eq!(
                guard(&dir, fixed(load, active)).check(),
                Ok(()),
                "{load}/{active}"
            );
        }
    }

    #[test]
    fn a_running_or_unknown_python_is_refused() {
        let dir = TempDir::new("guard-running");
        fs::write(dir.path().join("staging"), b"").unwrap();
        // unit ファイルを消して daemon-reload しても、動いているサービスは止まらない
        for load in ["loaded", "not-found"] {
            assert_eq!(
                guard(&dir, fixed(load, "active")).check(),
                Err(GuardError::PythonNotStopped("active".into())),
                "{load}/active"
            );
        }
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
                    guard(&dir, fixed(load, active)).check(),
                    Err(GuardError::PythonNotStopped(active.into())),
                    "{load}/{active}"
                );
            }
        }
    }

    #[test]
    fn an_unavailable_state_is_refused_not_passed() {
        let dir = TempDir::new("guard-unavailable");
        fs::write(dir.path().join("staging"), b"").unwrap();
        assert!(matches!(
            guard(&dir, Probe::Fixed(None)).check(),
            Err(GuardError::PythonUnknown(_))
        ));
    }

    #[test]
    fn the_systemctl_output_is_parsed_strictly() {
        let dir = TempDir::new("guard-shell");
        fs::write(dir.path().join("staging"), b"").unwrap();
        let ok = |script| guard(&dir, Probe::Shell(script)).check();
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
        let dir = TempDir::new("guard-hung");
        fs::write(dir.path().join("staging"), b"").unwrap();
        let started = Instant::now();
        let result = guard(&dir, Probe::Shell("sleep 30")).check();
        assert!(matches!(result, Err(GuardError::PythonUnknown(_))));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn the_refusal_messages_carry_no_values() {
        let text = GuardError::NoMarker("/etc/geoframe/staging".into()).to_string();
        assert!(text.contains("/etc/geoframe/staging") && text.contains("GEOFRAME_INGEST=1"));
        let text = GuardError::PythonNotStopped("active".into()).to_string();
        assert!(text.contains("geoframe-server.service") && text.contains("active"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn test_overrides_change_the_marker_path_but_never_remove_the_requirement() {
        let dir = TempDir::new("guard-override");
        let env = |key: &str| match key {
            "GEOFRAME_TEST_STAGING_MARKER" => Some(dir.path().join("m").display().to_string()),
            "GEOFRAME_TEST_PYTHON_STATE" => Some("loaded/inactive".to_string()),
            _ => None,
        };
        let g = Guard {
            marker: PathBuf::from(STAGING_MARKER),
            probe: Probe::Systemctl,
            skip: false,
        }
        .with_test_overrides(env);
        // 差し替えた先に印が無ければ断る
        assert!(matches!(g.check(), Err(GuardError::NoMarker(_))));
        fs::write(dir.path().join("m"), b"").unwrap();
        assert_eq!(g.check(), Ok(()));
        // 値の形が違えば取得失敗として断る（通さない）
        let bad = Guard {
            marker: dir.path().join("m"),
            probe: Probe::Systemctl,
            skip: false,
        }
        .with_test_overrides(|key| {
            (key == "GEOFRAME_TEST_PYTHON_STATE").then(|| "garbage".to_string())
        });
        assert!(matches!(bad.check(), Err(GuardError::PythonUnknown(_))));
    }
}
