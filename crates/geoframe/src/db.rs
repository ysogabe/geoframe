//! SQLite（`catalog.db`）の読み取り。P0 の `check-db` 用で、書き込みはしない。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// `check-db` の結果。
#[derive(Debug, PartialEq, Eq)]
pub struct DbReport {
    pub photos: i64,
    pub sqlite_version: String,
}

/// DB を読み取り専用で開く。`SQLITE_OPEN_READ_ONLY` なので書き込みは SQLite 側が拒否する。
///
/// 注意: WAL モードの DB は、読み取り専用で開いても同じディレクトリに空の `-wal`・`-shm`
/// （共有メモリ用）ファイルが作られる。DB 本体の内容は変わらない（docs/rust-migration/build.md）。
pub fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    // READ_ONLY には CREATE を付けないので、無いパスを新規作成することもない
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

/// `photos` テーブルの行数と、リンクされた SQLite のバージョンを返す。
pub fn check_db(path: &Path) -> rusqlite::Result<DbReport> {
    let conn = open_read_only(path)?;
    let photos = conn.query_row("SELECT COUNT(*) FROM photos", [], |row| row.get(0))?;
    Ok(DbReport {
        photos,
        sqlite_version: rusqlite::version().to_string(),
    })
}

/// SQLite は同期 API なので、tokio の worker を塞がないよう `spawn_blocking` で実行する。
/// P1 以降の DB アクセスはこの形に揃える。
pub async fn check_db_blocking(path: PathBuf) -> Result<DbReport, String> {
    tokio::task::spawn_blocking(move || check_db(&path).map_err(|e| e.to_string()))
        .await
        .map_err(|e| format!("DB タスクが異常終了しました: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成した一時 DB を作る（実データは使わない）。
    fn synthetic_db(dir: &Path, rows: i64) -> PathBuf {
        let path = dir.join("catalog.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE photos (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        for i in 0..rows {
            conn.execute("INSERT INTO photos (name) VALUES (?1)", [format!("p{i}")])
                .unwrap();
        }
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("geoframe-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn counts_photos_and_reports_version() {
        let dir = temp_dir("count");
        let path = synthetic_db(&dir, 3);
        let report = check_db(&path).unwrap();
        assert_eq!(report.photos, 3);
        assert_eq!(report.sqlite_version, rusqlite::version());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn connection_is_read_only_and_file_is_untouched() {
        let dir = temp_dir("ro");
        let path = synthetic_db(&dir, 1);
        let before = std::fs::read(&path).unwrap();

        let conn = open_read_only(&path).unwrap();
        let err = conn
            .execute("INSERT INTO photos (name) VALUES ('x')", [])
            .unwrap_err();
        assert!(err.to_string().contains("readonly"), "{err}");
        drop(conn);
        check_db(&path).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_is_an_error_and_not_created() {
        let dir = temp_dir("missing");
        let path = dir.join("none.db");
        assert!(check_db(&path).is_err());
        assert!(!path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_photos_table_is_an_error() {
        let dir = temp_dir("notable");
        let path = dir.join("empty.db");
        Connection::open(&path).unwrap();
        assert!(check_db(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn blocking_wrapper_returns_same_result() {
        let dir = temp_dir("blocking");
        let path = synthetic_db(&dir, 2);
        let report = check_db_blocking(path).await.unwrap();
        assert_eq!(report.photos, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
