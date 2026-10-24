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
//! 3. スキーマの作成・移行（[`crate::schema::migrate`]）。
//! 4. `.cache.json` を読み、旧キーを捨て、捨てたものがあれば即保存する。
//! 5. `DATA_DIR/tmp/` を用意し、`tmp/` と `derivatives/` の `.dl_*` を消す（[`prepare_tmp_dir`]）。
//!
//! 地図タイルの掃除（`tile_cache_prune`）は P3。

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

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
    let migration = writer.call(schema::migrate)?;
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
/// と同じ）。名前はログに出さない。
pub fn prepare_tmp_dir(data_dir: &Path) -> io::Result<usize> {
    fs::create_dir_all(data_dir.join("tmp"))?;
    let mut removed = 0;
    for dir in [data_dir.join("tmp"), data_dir.join("derivatives")] {
        for path in leftovers(&dir) {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// `.dl_*` のファイルの一覧（消さずに数えるとき・消すときの共通）。ディレクトリ・リンクは含めない。
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

/// `--dry-run` の掃除の計画（消さずに数える）。
pub fn count_leftovers(data_dir: &Path) -> usize {
    [data_dir.join("tmp"), data_dir.join("derivatives")]
        .iter()
        .map(|d| leftovers(d).len())
        .sum()
}

/// `--dry-run` が出す「やるとしたら何か」。
#[derive(Debug)]
pub struct Plan {
    pub schema: Pending,
    pub leftover_files: usize,
    pub legacy_cache_keys: usize,
}

/// 書かずに計画を作る。DB は読み取り専用の接続で見る。
pub fn plan(config: &Config, conn: &Connection) -> rusqlite::Result<Plan> {
    Ok(Plan {
        schema: schema::pending(conn)?,
        leftover_files: count_leftovers(&config.data_dir),
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
        assert_eq!(ingest.migration.created.len(), 16);
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
