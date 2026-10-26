//! 取り込みの起動処理（現行の起動時の準備。棚卸し B0）と、`geoframe sync` の本体。
//!
//! # 有効にする条件
//!
//! **`GEOFRAME_INGEST=1` のときだけ**動く（既定は無効）。P5 の切替まで、本番の書き手は Python
//! （`geoframe-server`）なので、`/etc/default/geoframe` をそのまま使って Rust を起動しても DB に
//! 書かない。無効のときの `serve` は P1 と同じ読み取り専用で、DB が無ければ起動を断る。
//!
//! # 起動の順序（有効のとき）
//!
//! 現行は `load_cache` → `purge_cache_legacy` → `db()`（スキーマ・移行）→ `tile_cache_prune`
//! → `prepare_tmp_dir`。Rust は次の順にする（失敗したら起動しない）。
//!
//! 0. **ガード**（[`crate::guard`]）。検証環境の印が無い・Python 版が非稼働と確認できないなら、何も
//!    書かずに断る（終了コード 2）。データ置き場の作成より前。P5 の切替で外す。
//! 1. データ置き場を用意し、`DATA_DIR/.ingest.lock` を排他で取る。別の `geoframe`（`serve` か `sync`）が
//!    すでに取り込み中なら断る。**Python との二重書き込みは防げない**（Python はこのロックを見ない）
//!    ので、Python が動いている間は取り込まない（0 のガードが、起動時に 1 回だけ確かめる）。
//! 2. writer を開く。DB が無ければ**新しく作る**（`schema::migrate` が空から作る）。作ったときは warn
//!    で知らせる（`GEOFRAME_DATA_DIR` の誤りで空の DB を静かに作ってしまうのを見つけるため）。
//! 3. スキーマの作成・移行（[`crate::schema::migrate`]）と、P4 の AI 表（ジョブ・送信予算・Provider 停止）の追加（[`crate::ai::migrate`]）。
//! 4. `.cache.json` を読み、旧キーを捨て、捨てたものがあれば即保存する。
//! 5. `DATA_DIR/tmp/` を用意し、`tmp/` と `derivatives/` の `.dl_*` を消す（[`prepare_tmp_dir`]）。
//! 6. `derivatives/` の書きかけの `*.part`（プロセスが `SIGKILL`・電源断で落ちて残ったもの）を消す
//!    （[`sweep_part_files`]）。
//!
//! 地図タイルの掃除（`tile_cache_prune`）は取り込みの起動時の掃除に含めない。`catalog.db` に書かないので、取り込みの有無に
//! 関わらず `serve` が独立のスレッドで行う（[`crate::tiles::spawn_pruner`]。T3.4）。

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use crate::ai;
use crate::cache::Cache;
use crate::config::Config;
use crate::guard::{Guard, GuardError};
use crate::schema::{self, Migration, Pending};
use crate::writer::{OpenMode, WriteError, Writer};

const LOCK_FILE: &str = ".ingest.lock";

#[derive(Debug)]
pub enum IngestError {
    /// 本番への誤指定の防止（[`crate::guard`]）で断った。何も書いていない。
    Refused(GuardError),
    /// データ置き場を用意できない・ロックのファイルを開けない。
    Io(io::ErrorKind),
    /// 別の `geoframe` がすでに取り込み中。
    Locked,
    Db(WriteError),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "{e}"),
            Self::Io(kind) => write!(f, "取り込みの準備に失敗しました: {kind}"),
            Self::Locked => f.write_str(
                "別の geoframe が取り込み中です（serve か sync が動いています）。二重に書かないよう起動を断ります",
            ),
            Self::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<WriteError> for IngestError {
    fn from(e: WriteError) -> Self {
        Self::Db(e)
    }
}

/// `.ingest.lock` の所有権。複製（[`Ingest::lock_hold`]）を持つ間はロックが外れない。
///
/// 書き込みうるスレッド（同期サイクル・キャッシュ保存）が `Ingest` の drop のあとも残りうる（停止の期限を
/// 超えても待ちきらない）ので、ロックの寿命を `Ingest` だけに結びつけない。そうしないと、旧スレッドが
/// まだ書ける間に別の `sync`／`serve` が起動して、DB やキャッシュの一時ファイルで競合する。
/// 各スレッドは自分の複製をスレッドの終了（パニックを含む）まで持ち、プロセスが終わればOS が外す。
#[derive(Clone)]
pub struct LockHold(#[allow(dead_code)] Arc<File>);

impl LockHold {
    /// 試験用: ロックは取らない（スレッドに渡す引数を満たすだけ）。
    #[cfg(test)]
    pub(crate) fn for_test(dir: &Path) -> Self {
        Self(Arc::new(File::create(dir.join("hold-for-test")).unwrap()))
    }
}

/// 取り込みの書き手一式。drop してもスレッドが [`LockHold`] を持つ間はロックが残る。
pub struct Ingest {
    pub writer: Writer,
    pub cache: Arc<Cache>,
    pub migration: Migration,
    lock: LockHold,
}

impl Ingest {
    /// ロックの所有権の複製。書き込みうるスレッドに渡し、そのスレッドが終わるまで持たせる。
    pub fn lock_hold(&self) -> LockHold {
        self.lock.clone()
    }
}

/// 取り込みを始める準備（上の「起動の順序」）。最初に[ガード](crate::guard)を通す。
pub fn start(config: &Config) -> Result<Ingest, IngestError> {
    start_with(config, &Guard::production())
}

/// [`start`] のガードを差し替えられる形（単体テスト用。配布するバイナリの経路は [`start`] だけ）。
pub(crate) fn start_with(config: &Config, guard: &Guard) -> Result<Ingest, IngestError> {
    // 最初の書き込み（データ置き場の作成・ロック・移行・掃除・キャッシュ保存）より前に断る
    guard.check().map_err(IngestError::Refused)?;
    fs::create_dir_all(&config.data_dir).map_err(|e| IngestError::Io(e.kind()))?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(config.data_dir.join(LOCK_FILE))
        .map_err(|e| IngestError::Io(e.kind()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(IngestError::Locked),
        Err(TryLockError::Error(e)) => return Err(IngestError::Io(e.kind())),
    }

    let db_path = config.db_path();
    let existed = db_path.exists();
    let writer = Writer::open(&db_path, OpenMode::CreateIfMissing)?;
    if !existed {
        tracing::warn!(
            "DB が無かったので新しく作ります（GEOFRAME_DATA_DIR が正しいか確認してください）"
        );
    }
    let mut migration = writer.call(schema::migrate)?;
    // P4: AI の表（ジョブ・送信予算・Provider 停止）。Python と同じスキーマの再現（schema::migrate）とは別に足す
    migration.created.extend(writer.call(ai::migrate)?);
    log_migration(&migration);

    let cache = Arc::new(Cache::load(&config.data_dir));
    let purged = cache.purge_legacy();
    if purged > 0 {
        tracing::info!(
            "[cache] レガシーキー削除: {purged}件 / 現役{}件を維持",
            cache.len()
        );
        cache.save_now();
    }

    match prepare_tmp_dir(&config.data_dir) {
        Ok(0) => {}
        Ok(n) => tracing::info!("[fetch] 一時ダウンロードの残骸を{n}件削除しました"),
        Err(e) => return Err(IngestError::Io(e.kind())),
    }
    match sweepable_dirs(&config.data_dir).skipped {
        0 => {}
        n => tracing::warn!(
            "[fetch] tmp/・derivatives/ のうち{n}つがリンクなどで通常のディレクトリではないので、掃除を飛ばしました"
        ),
    }

    // 書きかけの `.part`。`.dl_*` と同じく起動時に 1 回、ガードとロックのあと（二重に動く書き手がいない）
    match sweep_part_files(&config.data_dir) {
        0 => {}
        n => tracing::info!("[derivative] 書きかけの .part を{n}件削除しました"),
    }

    // 旅行ブログの書き出しの作業領域・置き換え途中の書き出し・下書きの書きかけ（強制終了・電源断で残ったもの）
    crate::blog_write::sweep_leftovers(&config.data_dir);

    Ok(Ingest {
        writer,
        cache,
        migration,
        lock: LockHold(Arc::new(lock)),
    })
}

fn log_migration(m: &Migration) {
    if m.is_noop() {
        tracing::info!("[db] スキーマは最新です");
        return;
    }
    tracing::info!(
        "[db] スキーマを更新しました: 作成 {}件 / 列の追加 {}件 / embedding の写し {}件",
        m.created.len(),
        m.added_columns.len(),
        m.embeddings_copied
    );
    if !m.added_columns.is_empty() {
        tracing::info!("[db] 追加した列: {}", m.added_columns.join(", "));
    }
    if m.places_reset {
        // 全行の再 enrich（地理院へ大量に問い合わせる）が起きる。本番相当の DB では起きないはず
        tracing::warn!("[db] 自治体情報の列を足したので、全行の place を NULL に戻しました");
    }
}

/// `DATA_DIR/tmp/` を用意し、`tmp/` と `derivatives/` の `.dl_*`（前回の残骸）を消す。消した数を返す。
///
/// 残骸を消すのは、フル解像度・EXIF 完全保持の Drive 原本（GPS・機体シリアル・原撮影時刻を含む）を
/// ディスクに置いたままにしないため。`derivatives/` は静的配信の許可 prefix の内側なので特に放置
/// しない。**ファイルだけ**を消す（ディレクトリや消せないものは無視。現行の `unlink` の `OSError: pass`
/// と同じ）。名前はログに出さない。`tmp/`・`derivatives/` 自体がリンク（など通常のディレクトリでない
/// もの）なら、その中は掃除しない（リンク先の別の場所のファイルを消さないため。[`sweepable_dirs`]）。
pub fn prepare_tmp_dir(data_dir: &Path) -> io::Result<usize> {
    fs::create_dir_all(data_dir.join("tmp"))?;
    let mut removed = 0;
    for dir in sweepable_dirs(data_dir).dirs {
        for path in leftovers(&dir) {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// 掃除してよい `tmp/`・`derivatives/`（[`sweepable_dirs`] の結果）。
struct SweepDirs {
    /// 通常のディレクトリであるもの。
    dirs: Vec<std::path::PathBuf>,
    /// 存在するのに通常のディレクトリではない（リンク・ファイルなど）ので飛ばしたものの数。
    skipped: usize,
}

/// `.dl_*` の掃除の対象にする `DATA_DIR/tmp/`・`DATA_DIR/derivatives/` を選ぶ。
///
/// `read_dir` はリンクを辿るので、`tmp/`・`derivatives/` が外へのリンクだと、列挙の前に検査しないと
/// リンク先の `.dl_*` を消してしまう。`symlink_metadata` でリンクを辿らずに見て、通常のディレクトリだけを
/// 対象にする。存在しないものは黙って外す（`derivatives/` は写真を置く前は無い）。それ以外（リンク・
/// ファイル・見られないもの）は数えて、呼び出し側が件数だけ警告する（パスは出さない）。`.part` の
/// 掃除（[`part_leftovers`]）の `derivatives/` の検査も同じ。
fn sweepable_dirs(data_dir: &Path) -> SweepDirs {
    let mut out = SweepDirs {
        dirs: Vec::new(),
        skipped: 0,
    };
    for dir in [data_dir.join("tmp"), data_dir.join("derivatives")] {
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => out.dirs.push(dir),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => out.skipped += 1,
        }
    }
    out
}

/// `.dl_*` のファイルの一覧（消さずに数えるとき・消すときの共通）。ディレクトリ・リンクは含めない。
/// `dir` が通常のディレクトリであることは呼び出し側が [`sweepable_dirs`] で確かめる。
fn leftovers(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".dl_"))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect()
}

/// `derivatives/` の中の書きかけの `*.part` のファイル一覧。
///
/// derivative は `<出力名>.<pid>.<乱数>.part`（Rust）または `<名前>.part`（Python）に書いてから `rename`
/// する。書き込み中に落ちると残り、Rust は名前に乱数を含むので次に同じ derivative を作っても上書き
/// されない。中身は EXIF を持たない部分的な JPEG（個人情報の問題は無く、容量だけ使う）。
///
/// 対象は `derivatives/` の**直下の通常ファイルだけ**（名前は [`crate::datadir::check_file_name`] を通り、
/// `.part` で終わるもの）。`derivatives` がシンボリックリンクなら何もしない（リンク先の別の場所を
/// 掃除しない）。ディレクトリ・シンボリックリンクは対象にしない（`DirEntry::file_type` はリンクを
/// 辿らない）。取り込みのロックを持つ間は、他に書きかけを作るプロセスがいない（Python は P5 までガードが
/// 止まっていることを確かめる）ので、残っている `.part` はすべて書きかけの残骸。
fn part_leftovers(data_dir: &Path) -> Vec<std::path::PathBuf> {
    let dir = data_dir.join("derivatives");
    if !sweepable_dirs(data_dir).dirs.contains(&dir) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".part") && crate::datadir::check_file_name(&name).is_ok()
        })
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect()
}

/// 書きかけの `*.part` を消す。消した数を返す（名前はログに出さない）。消せないものは無視する。
pub fn sweep_part_files(data_dir: &Path) -> usize {
    part_leftovers(data_dir)
        .iter()
        .filter(|p| fs::remove_file(p).is_ok())
        .count()
}

/// `--dry-run` の掃除の計画（消さずに数える）。
pub fn count_part_files(data_dir: &Path) -> usize {
    part_leftovers(data_dir).len()
}

/// `--dry-run` の掃除の計画（消さずに数える）。
pub fn count_leftovers(data_dir: &Path) -> usize {
    sweepable_dirs(data_dir)
        .dirs
        .iter()
        .map(|d| leftovers(d).len())
        .sum()
}

/// `--dry-run` の掃除の計画: リンクなどで掃除を飛ばす `tmp/`・`derivatives/` の数。
pub fn count_skipped_dirs(data_dir: &Path) -> usize {
    sweepable_dirs(data_dir).skipped
}

/// `--dry-run` が出す「やるとしたら何か」。
#[derive(Debug)]
pub struct Plan {
    pub schema: Pending,
    pub leftover_files: usize,
    /// リンクなどで掃除を飛ばす `tmp/`・`derivatives/` の数（[`count_skipped_dirs`]）。
    pub skipped_dirs: usize,
    /// 消す書きかけの `*.part`（[`sweep_part_files`]）。
    pub part_files: usize,
    pub legacy_cache_keys: usize,
}

/// 書かずに計画を作る。DB は読み取り専用の接続で見る。
pub fn plan(config: &Config, conn: &Connection) -> rusqlite::Result<Plan> {
    let mut schema = schema::pending(conn)?;
    schema.missing_objects.extend(ai::pending(conn)?);
    Ok(Plan {
        schema,
        leftover_files: count_leftovers(&config.data_dir),
        skipped_dirs: count_skipped_dirs(&config.data_dir),
        part_files: count_part_files(&config.data_dir),
        legacy_cache_keys: Cache::load_frozen(&config.data_dir).legacy_count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Overrides;
    use crate::testutil::TempDir;

    /// 合成の置き場で試すので、ガード（検証環境の印・Python の状態）は通す。
    fn start(config: &Config) -> Result<Ingest, IngestError> {
        super::start_with(config, &Guard::skip())
    }

    fn config(dir: &TempDir) -> Config {
        let overrides = Overrides {
            listen: None,
            data_dir: Some(dir.path().to_path_buf()),
        };
        Config::load(|_| None, &overrides).unwrap()
    }

    #[test]
    fn prepare_tmp_dir_removes_only_dl_files_in_tmp_and_derivatives() {
        let dir = TempDir::new("ingest-tmp");
        let root = dir.path();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::create_dir_all(root.join("derivatives")).unwrap();
        fs::create_dir_all(root.join("photos")).unwrap();
        for f in [
            "tmp/.dl_a",
            "tmp/.dl_b.part",
            "tmp/keep.txt",
            "derivatives/.dl_c",
            "derivatives/x.jpg",
            "derivatives/.hidden",
            "photos/.dl_d", // 対象の外
        ] {
            fs::write(root.join(f), b"x").unwrap();
        }
        // ディレクトリは消さない（消せなくて無視する、現行と同じ）
        fs::create_dir_all(root.join("tmp/.dl_dir")).unwrap();
        assert_eq!(count_leftovers(root), 3);
        assert_eq!(prepare_tmp_dir(root).unwrap(), 3);
        assert_eq!(count_leftovers(root), 0);
        for kept in [
            "tmp/keep.txt",
            "derivatives/x.jpg",
            "derivatives/.hidden",
            "photos/.dl_d",
            "tmp/.dl_dir",
        ] {
            assert!(root.join(kept).exists(), "{kept}");
        }
        // 2 回目は 0
        assert_eq!(prepare_tmp_dir(root).unwrap(), 0);
    }

    #[test]
    fn sweep_removes_only_part_files_directly_in_derivatives() {
        let dir = TempDir::new("ingest-part");
        let root = dir.path();
        fs::create_dir_all(root.join("derivatives/sub")).unwrap();
        fs::create_dir_all(root.join("photos")).unwrap();
        for f in [
            // 消す: Rust の名前・Python の名前
            "derivatives/abc.jpg.123.00ff00ff00ff00ff.part",
            "derivatives/x.r90.jpg.9.0123456789abcdef.part",
            "derivatives/old.part",
            // 残す
            "derivatives/abc.jpg",
            "derivatives/abc.part.keep",
            "derivatives/.hidden.part", // ドット始まりは派生の名前ではない
            "derivatives/sub/inner.part",
            "derivatives/日本.part",
            "photos/outside.part",
        ] {
            fs::write(root.join(f), b"x").unwrap();
        }
        // ディレクトリ名が `.part` で終わっても消さない
        fs::create_dir_all(root.join("derivatives/dir.part")).unwrap();
        assert_eq!(count_part_files(root), 3);
        assert_eq!(sweep_part_files(root), 3);
        assert_eq!(count_part_files(root), 0);
        for kept in [
            "derivatives/abc.jpg",
            "derivatives/abc.part.keep",
            "derivatives/.hidden.part",
            "derivatives/sub/inner.part",
            "derivatives/日本.part",
            "derivatives/dir.part",
            "photos/outside.part",
        ] {
            assert!(root.join(kept).exists(), "{kept}");
        }
        assert_eq!(sweep_part_files(root), 0);
    }

    #[cfg(unix)]
    #[test]
    fn sweep_does_not_follow_or_remove_links() {
        let dir = TempDir::new("ingest-part-link");
        let root = dir.path();
        fs::create_dir_all(root.join("derivatives")).unwrap();
        fs::write(root.join("target.txt"), b"x").unwrap();
        // `.part` という名前のリンクは消さず、先にも触れない
        std::os::unix::fs::symlink(
            root.join("target.txt"),
            root.join("derivatives/link.jpg.1.aa.part"),
        )
        .unwrap();
        assert_eq!(sweep_part_files(root), 0);
        assert!(root.join("target.txt").exists());
        assert!(fs::symlink_metadata(root.join("derivatives/link.jpg.1.aa.part")).is_ok());
        // `derivatives` がリンクなら、先の `.part` を掃除しない
        let elsewhere = TempDir::new("ingest-part-elsewhere");
        fs::write(elsewhere.path().join("a.jpg.1.aa.part"), b"x").unwrap();
        let root2 = TempDir::new("ingest-part-root2");
        std::os::unix::fs::symlink(elsewhere.path(), root2.path().join("derivatives")).unwrap();
        assert_eq!(count_part_files(root2.path()), 0);
        assert_eq!(sweep_part_files(root2.path()), 0);
        assert!(elsewhere.path().join("a.jpg.1.aa.part").exists());
    }

    #[test]
    fn start_sweeps_part_files_after_the_guard() {
        let dir = TempDir::new("ingest-part-start");
        let root = dir.path().join("data");
        let overrides = Overrides {
            listen: None,
            data_dir: Some(root.clone()),
        };
        let config = Config::load(|_| None, &overrides).unwrap();
        fs::create_dir_all(root.join("derivatives")).unwrap();
        fs::write(root.join("derivatives/a.jpg.1.aa.part"), b"x").unwrap();
        // ガードが断れば消さない
        let guard = Guard::production_with_marker_for_test(dir.path().join("no-such-marker"));
        assert!(super::start_with(&config, &guard).is_err());
        assert!(root.join("derivatives/a.jpg.1.aa.part").exists());
        // 通れば消える
        drop(start(&config).unwrap());
        assert!(!root.join("derivatives/a.jpg.1.aa.part").exists());
    }

    #[test]
    fn prepare_tmp_dir_creates_tmp_and_tolerates_a_missing_derivatives() {
        let dir = TempDir::new("ingest-tmp-new");
        assert_eq!(prepare_tmp_dir(dir.path()).unwrap(), 0);
        assert!(dir.path().join("tmp").is_dir());
        assert!(!dir.path().join("derivatives").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_named_dl_are_not_followed() {
        let dir = TempDir::new("ingest-tmp-link");
        let root = dir.path();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::write(root.join("target.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("target.txt"), root.join("tmp/.dl_link")).unwrap();
        // リンクは消さず、リンク先にも触れない
        assert_eq!(prepare_tmp_dir(root).unwrap(), 0);
        assert!(root.join("target.txt").exists());
    }

    /// `tmp/`・`derivatives/` が外へのリンクのとき、リンク先の `.dl_*` を消さず、`.part` も消さない
    /// （実行・dry-run の件数の両方）。通常のディレクトリのほうは、同じ実行でふつうに掃除する。
    #[cfg(unix)]
    #[test]
    fn linked_tmp_and_derivatives_are_not_swept() {
        for linked in ["tmp", "derivatives"] {
            let dir = TempDir::new("ingest-linked-dir");
            let root = dir.path().join("data");
            let outside = dir.path().join("outside");
            fs::create_dir_all(&root).unwrap();
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join(".dl_x"), b"x").unwrap();
            fs::write(outside.join("y.1.aa.part"), b"x").unwrap();
            // リンクでないほうの側には、ふつうの残骸を置く
            let plain = if linked == "tmp" {
                "derivatives"
            } else {
                "tmp"
            };
            fs::create_dir_all(root.join(plain)).unwrap();
            fs::write(root.join(plain).join(".dl_z"), b"x").unwrap();
            std::os::unix::fs::symlink(&outside, root.join(linked)).unwrap();

            // dry-run の件数も、リンク先を数えず、飛ばす数を出す
            assert_eq!(count_leftovers(&root), 1, "{linked}");
            assert_eq!(count_part_files(&root), 0, "{linked}");
            assert_eq!(count_skipped_dirs(&root), 1, "{linked}");

            assert_eq!(prepare_tmp_dir(&root).unwrap(), 1, "{linked}");
            assert_eq!(sweep_part_files(&root), 0, "{linked}");
            assert!(outside.join(".dl_x").exists(), "{linked}: リンク先を消した");
            assert!(outside.join("y.1.aa.part").exists(), "{linked}");
            assert!(!root.join(plain).join(".dl_z").exists(), "{linked}");
            assert!(
                fs::symlink_metadata(root.join(linked))
                    .unwrap()
                    .is_symlink(),
                "{linked}"
            );
        }
    }

    /// `derivatives/` が無い（写真を置く前）のは飛ばした数に入れない。ファイルなどの通常でないものは入れる。
    #[test]
    fn missing_derivatives_is_not_counted_as_skipped() {
        let dir = TempDir::new("ingest-no-derivatives");
        fs::create_dir_all(dir.path().join("tmp")).unwrap();
        assert_eq!(count_skipped_dirs(dir.path()), 0);
        fs::write(dir.path().join("derivatives"), b"file").unwrap();
        assert_eq!(count_skipped_dirs(dir.path()), 1);
    }

    #[test]
    fn start_creates_a_missing_database_and_cleans_up() {
        let dir = TempDir::new("ingest-start");
        let root = dir.path().join("data");
        let overrides = Overrides {
            listen: None,
            data_dir: Some(root.clone()),
        };
        let config = Config::load(|_| None, &overrides).unwrap();
        // 旧キーのある .cache.json と、残骸
        fs::create_dir_all(root.join("derivatives")).unwrap();
        fs::write(root.join(".cache.json"), r#"{"exif:a": 1, "geo:1": 2}"#).unwrap();
        fs::write(root.join("derivatives/.dl_x"), b"x").unwrap();

        let ingest = start(&config).unwrap();
        assert!(config.db_path().exists());
        // Python と同じスキーマの 16（表 11・索引 5）と、P4 の AI 表（`ai_jobs`・その索引・送信予算・Provider 停止）
        assert_eq!(ingest.migration.created.len(), 16 + 4);
        for name in ["ai_jobs", "ai_request_usage", "ai_provider_pauses"] {
            assert!(
                ingest.migration.created.contains(&name.to_string()),
                "{name}"
            );
        }
        assert!(!ingest.migration.places_reset);
        crate::db::Catalog::open(&config.db_path()).unwrap();
        // 旧キーは即保存された
        let saved = fs::read_to_string(root.join(".cache.json")).unwrap();
        assert!(
            !saved.contains("exif:a") && saved.contains("geo:1"),
            "{saved}"
        );
        assert!(!root.join("derivatives/.dl_x").exists());
        assert!(root.join("tmp").is_dir());
    }

    #[test]
    fn a_second_ingest_on_the_same_data_dir_is_refused() {
        let dir = TempDir::new("ingest-lock");
        let config = config(&dir);
        let first = start(&config).unwrap();
        assert!(matches!(start(&config), Err(IngestError::Locked)));
        drop(first);
        // 外れたら取れる
        assert!(start(&config).is_ok());
    }

    /// `Ingest` を drop しても、スレッドが [`LockHold`] を持つ間は次の取り込みを断り、手放したら取れる。
    #[test]
    fn the_lock_outlives_ingest_while_a_thread_holds_it() {
        let dir = TempDir::new("ingest-lock-hold");
        let config = config(&dir);
        let first = start(&config).unwrap();
        let hold = first.lock_hold();
        drop(first);
        assert!(matches!(start(&config), Err(IngestError::Locked)));
        drop(hold);
        assert!(start(&config).is_ok());
    }

    #[test]
    fn restarting_on_an_existing_database_changes_nothing() {
        let dir = TempDir::new("ingest-restart");
        let config = config(&dir);
        drop(start(&config).unwrap());
        let again = start(&config).unwrap();
        assert!(again.migration.is_noop(), "{:?}", again.migration);
    }

    /// P4 より前の DB（`ai_jobs` が無い）を `--dry-run` で見ると、ジョブ表を作ると数える（書かない）。
    /// 実際の起動はそれだけを足し、次の起動からは何もしない。
    #[test]
    fn plan_counts_the_ai_jobs_table_that_a_start_would_add() {
        let dir = TempDir::new("ingest-plan-ai");
        let config = config(&dir);
        drop(start(&config).unwrap());
        {
            let conn = Connection::open(config.db_path()).unwrap();
            conn.execute_batch("DROP TABLE ai_jobs").unwrap();
        }
        let conn = crate::db::open_read_only(&config.db_path()).unwrap();
        let plan = plan(&config, &conn).unwrap();
        assert_eq!(
            plan.schema.missing_objects,
            ["ai_jobs", "idx_ai_jobs_runnable"]
        );
        assert!(plan.schema.missing_columns.is_empty());
        drop(conn);
        let again = start(&config).unwrap();
        assert_eq!(again.migration.created, ["ai_jobs", "idx_ai_jobs_runnable"]);
        drop(again);
        assert!(start(&config).unwrap().migration.is_noop());
    }

    /// T4.1a の状態（`ai_jobs` だけある）の DB を `--dry-run` で見ると、送信予算・Provider 停止の表だけを作ると
    /// 数える。実際の起動はそれだけを足し、`ai_jobs` の行は変えない。
    #[test]
    fn plan_counts_the_budget_and_pause_tables_added_after_the_jobs_table() {
        let dir = TempDir::new("ingest-plan-ai-store");
        let config = config(&dir);
        drop(start(&config).unwrap());
        {
            let conn = Connection::open(config.db_path()).unwrap();
            conn.execute_batch(
                "DROP TABLE ai_request_usage; DROP TABLE ai_provider_pauses;
                 INSERT INTO ai_jobs(kind,target_key,input_hash,processor_version,created_at,updated_at)
                   VALUES ('image_embedding','t','h','p',1,1)",
            )
            .unwrap();
        }
        let conn = crate::db::open_read_only(&config.db_path()).unwrap();
        let plan = plan(&config, &conn).unwrap();
        assert_eq!(
            plan.schema.missing_objects,
            ["ai_request_usage", "ai_provider_pauses"]
        );
        drop(conn);
        let again = start(&config).unwrap();
        assert_eq!(
            again.migration.created,
            ["ai_request_usage", "ai_provider_pauses"]
        );
        drop(again);
        let conn = Connection::open(config.db_path()).unwrap();
        let jobs: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(jobs, 1);
        drop(conn);
        assert!(start(&config).unwrap().migration.is_noop());
    }

    #[test]
    fn plan_counts_without_writing() {
        let dir = TempDir::new("ingest-plan");
        let config = config(&dir);
        drop(start(&config).unwrap());
        fs::write(config.data_dir.join("tmp/.dl_a"), b"x").unwrap();
        fs::write(
            config.data_dir.join(".cache.json"),
            r#"{"exif:a": 1, "geo:1": 2}"#,
        )
        .unwrap();
        let conn = crate::db::open_read_only(&config.db_path()).unwrap();
        let plan = plan(&config, &conn).unwrap();
        assert!(plan.schema.is_noop());
        assert_eq!(plan.leftover_files, 1);
        assert_eq!(plan.part_files, 0);
        assert_eq!(plan.legacy_cache_keys, 1);
        // 何も消えていない・書かれていない
        assert!(config.data_dir.join("tmp/.dl_a").exists());
        assert!(
            fs::read_to_string(config.data_dir.join(".cache.json"))
                .unwrap()
                .contains("exif:a")
        );
    }

    /// ガードが断ったら、データ置き場も DB もロックも作られない（最初の書き込みより前に止まる）。
    #[test]
    fn a_refused_guard_writes_nothing() {
        let dir = TempDir::new("ingest-refused");
        let root = dir.path().join("data");
        let overrides = Overrides {
            listen: None,
            data_dir: Some(root.clone()),
        };
        let config = Config::load(|_| None, &overrides).unwrap();
        // 印の無い（通常はここで止まる）ガード。置き場そのものが無い状態から
        let guard = Guard::production_with_marker_for_test(dir.path().join("no-such-marker"));
        assert!(matches!(
            super::start_with(&config, &guard),
            Err(IngestError::Refused(GuardError::NoMarker(_)))
        ));
        assert!(!root.exists());
        // 既存の置き場でも、DB・キャッシュ・tmp・derivatives・ロックは作られず、中身も変わらない
        fs::create_dir_all(root.join("derivatives")).unwrap();
        fs::write(root.join(".cache.json"), r#"{"exif:a": 1}"#).unwrap();
        fs::write(root.join("derivatives/.dl_x"), b"x").unwrap();
        assert!(super::start_with(&config, &guard).is_err());
        let mut names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, [".cache.json", "derivatives"]);
        assert_eq!(
            fs::read_to_string(root.join(".cache.json")).unwrap(),
            r#"{"exif:a": 1}"#
        );
        assert!(root.join("derivatives/.dl_x").exists());
    }
}
