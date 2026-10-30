//! 書き込みを始める前の守り（root での実行の拒否）。
//!
//! # なぜ要るか
//!
//! `GEOFRAME_INGEST=1` を立てると Rust は DB の移行・`.cache.json` の更新・`.dl_*` の掃除をする。
//! root で書くと、作られるファイル（`catalog.db-wal`・`.cache.json`・`derivatives/` など）が root の
//! 所有になり、サービス（`User=geoframe`）が書けなくなる。書き込みの前に**root（実効 UID 0）で実行して
//! いない**ことを確かめ、満たせなければ何も書かずに断る（終了コード 2）。手で打つ書き込み系の CLI・
//! 直接の起動・設定の書き換えのような経路も、バイナリ自身が守る。
//! サービスと同じユーザーで CLI を動かすには `deploy/geoframe-admin.sh` を使う。
//!
//! 検査は起動時の 1 回だけ。
//!
//! # 置き場所
//!
//! [`crate::ingest::start`] の先頭（データ置き場の作成より前）。`serve`（取り込みつき）も `sync`（実行）も
//! `organize` なども、`ingest::start` を通るものは全てここを通る。`serve` は取り込みが無効（読み取り専用）でも
//! root では起動しない（[`refuse_root`]。`-wal`／`-shm`・`tile_cache/` が root の所有になるため）。
//! `sync --dry-run` は書き込み接続を作らず行・写真・キャッシュ・ロックを変えないので、
//! ガードを通さない（SQLite が読み取りでも作る WAL 関連ファイルだけは、できうる）。ガードを通さないのは
//! 書かないからで、本番機で実行してよいという意味ではない。向けるのは検証環境の本番コピー（のバックアップの
//! コピー）だけで、本番の DB には向けない（docs/rust-migration/plan.md §2。鍵があれば Drive にも接続する）。
//!
//! # 試験用の差し替え
//!
//! デバッグビルド（`cargo test` が作るバイナリ）に限り、環境変数 `GEOFRAME_TEST_EUID`（実効 UID の代わりの
//! 数値。数値でなければ無視して本物を使う）で差し替えられる。リリースビルド（配布するもの）にはこのコードが無く、
//! 環境変数は無視される。

use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// root（実効 UID 0）で実行している。
    Root,
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str(
                "root で実行しているので、書き込みを始めません。作られるファイルが root の所有になり、\
                 サービス（geoframe ユーザー）が書けなくなります。サービスと同じユーザー・設定で動かすには \
                 deploy/geoframe-admin.sh を使ってください",
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

pub struct Guard {
    euid: Euid,
    #[cfg(test)]
    skip: bool,
}

impl Guard {
    /// 実際に使うガード。root の判定は `geteuid`。
    /// デバッグビルドだけは、上の「試験用の差し替え」を読む。
    pub fn production() -> Self {
        Self {
            euid: Euid::from_env(),
            #[cfg(test)]
            skip: false,
        }
    }

    /// 検査しない（`ingest` の単体テストが合成の置き場で `start` を呼ぶため）。
    #[cfg(test)]
    pub fn skip() -> Self {
        Self {
            euid: Euid::Fixed(1000),
            skip: true,
        }
    }

    /// root と答える、必ず断るガード（`ingest` の単体テスト用）。
    #[cfg(test)]
    pub fn refusing_for_test() -> Self {
        Self {
            euid: Euid::Fixed(0),
            skip: false,
        }
    }

    /// root でないことを確かめる。何も書かない。
    pub fn check(&self) -> Result<(), GuardError> {
        #[cfg(test)]
        if self.skip {
            return Ok(());
        }
        self.euid.check()?;
        tracing::info!("[guard] root ではないことを確認しました");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_refused() {
        let g = Guard {
            euid: Euid::Fixed(0),
            skip: false,
        };
        assert_eq!(g.check(), Err(GuardError::Root));
        assert_eq!(Euid::Fixed(0).check(), Err(GuardError::Root));
        assert_eq!(Euid::Fixed(1000).check(), Ok(()));
    }

    #[test]
    fn a_non_root_user_passes() {
        let g = Guard {
            euid: Euid::Fixed(1000),
            skip: false,
        };
        assert_eq!(g.check(), Ok(()));
    }

    #[test]
    fn the_refusal_message_names_the_admin_wrapper() {
        let text = GuardError::Root.to_string();
        assert!(text.contains("root") && text.contains("geoframe-admin.sh"));
    }
}
