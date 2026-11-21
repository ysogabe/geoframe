//! 公開の記録 `published.json`（design.md 4.2「管理しているフォルダの見分け方」、7 章）。
//!
//! 公開に成功するたびに、宛先の種類・フォルダ名・Drive のフォルダの ID・ジョブ ID を 1 行足す（同じ種類・同じフォルダ名の行は
//! 置き換える）。`replace` の対象にしてよいのは、宛先のフォルダの ID がここの行と一致し、かつ manifest の確認にも通った
//! フォルダだけ。行は間引かない。
//!
//! ```json
//! { "version": 1, "entries": [ { "destination": "yamareco", "folder": "2030-03-10", "folder_id": "…", "job_id": "…" } ] }
//! ```
//!
//! 書き方はジョブの記録と同じ（[`crate::jsonfile::write_json_atomic`]）。**ファイルが無い**ときは「行が 0 件」として扱うが、
//! **読めない・形が崩れている**ときは [`Loaded::Unreadable`]: `replace` は、どのフォルダも管理していないものとして
//! （`not_managed`）断る。書くときは、読めない記録を**上書きしない**（失敗を返す。オーナーが調べられるよう残す）。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{ErrorCode, ExportError};
use crate::folder::FolderName;
use crate::jobid::JobId;
use crate::jsonfile::{self, ReadError};
use crate::rclone::is_valid_drive_id;
use crate::request::Destination;

/// 読む上限（行は 1 件が 300 バイト前後。1MiB で数千件）。
pub const MAX_BYTES: u64 = 1024 * 1024;
const VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedRecord {
    pub destination: Destination,
    pub folder: FolderName,
    pub folder_id: String,
    pub job_id: JobId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    Records(Vec<PublishedRecord>),
    /// 読めない・形が崩れている（上限超え、JSON でない、版が違う、行の値が検査に通らない、同じ宛先の行が複数）。
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct PublishedStore {
    path: PathBuf,
}

fn parse_entries(v: &Value) -> Option<Vec<PublishedRecord>> {
    if v.get("version")?.as_u64()? != VERSION {
        return None;
    }
    let mut out: Vec<PublishedRecord> = Vec::new();
    for e in v.get("entries")?.as_array()? {
        let rec = PublishedRecord {
            destination: Destination::parse(e.get("destination")?.as_str()?)?,
            folder: FolderName::parse(e.get("folder")?.as_str()?)?,
            folder_id: {
                let id = e.get("folder_id")?.as_str()?;
                is_valid_drive_id(id).then(|| id.to_owned())?
            },
            job_id: JobId::parse(e.get("job_id")?.as_str()?)?,
        };
        // 同じ宛先の行が 2 つあると、どちらが正しいか決められない
        if out
            .iter()
            .any(|r| r.destination == rec.destination && r.folder == rec.folder)
        {
            return None;
        }
        out.push(rec);
    }
    Some(out)
}

fn to_json(records: &[PublishedRecord]) -> Value {
    json!({
        "version": VERSION,
        "entries": records.iter().map(|r| json!({
            "destination": r.destination.as_str(),
            "folder": r.folder.as_str(),
            "folder_id": r.folder_id,
            "job_id": r.job_id.as_str(),
        })).collect::<Vec<_>>(),
    })
}

impl PublishedStore {
    /// `<state-dir>/published.json`。
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("published.json"),
        }
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Loaded {
        match jsonfile::read_json(&self.path, MAX_BYTES) {
            Ok(v) => parse_entries(&v).map_or(Loaded::Unreadable, Loaded::Records),
            Err(ReadError::NotFound) => Loaded::Records(Vec::new()),
            Err(_) => Loaded::Unreadable,
        }
    }

    /// 同じ種類・同じフォルダ名の行の、フォルダの ID。行が無い・記録が読めないときは `None`（管理していない）。
    pub fn find_id(&self, destination: Destination, folder: &FolderName) -> Option<String> {
        match self.load() {
            Loaded::Records(records) => records
                .into_iter()
                .find(|r| r.destination == destination && &r.folder == folder)
                .map(|r| r.folder_id),
            Loaded::Unreadable => None,
        }
    }

    /// 行を足す（同じ種類・フォルダ名の行は置き換える）。記録が読めないときは書かずに失敗する。
    pub fn upsert(&self, record: PublishedRecord) -> Result<(), ExportError> {
        let Loaded::Records(mut records) = self.load() else {
            return Err(ExportError::new(
                ErrorCode::Internal,
                "公開の記録が読めないので、書きません",
            ));
        };
        match records
            .iter_mut()
            .find(|r| r.destination == record.destination && r.folder == record.folder)
        {
            Some(row) => *row = record,
            None => records.push(record),
        }
        jsonfile::write_json_atomic(&self.path, &to_json(&records))
            .map_err(|_| ExportError::new(ErrorCode::Internal, "公開の記録を書けません"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 試験の作業用ディレクトリの親（ビルドの出力の下。`/tmp` は使わない）。
    fn unit_tmp() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/unit-tmp")
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = unit_tmp().join(format!("published-{name}-{}", std::process::id()));
            // 自分で作る名前（プロセス ID つき）。前のものは消さない（残っていれば作成に失敗して気づく）
            std::fs::create_dir_all(&p).unwrap();
            assert_eq!(
                std::fs::read_dir(&p).unwrap().count(),
                0,
                "前の試験の残りがあります"
            );
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rec(dest: Destination, folder: &str, id: &str) -> PublishedRecord {
        PublishedRecord {
            destination: dest,
            folder: FolderName::parse(folder).unwrap(),
            folder_id: id.to_owned(),
            job_id: JobId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        }
    }

    #[test]
    fn missing_file_means_no_records_and_upsert_replaces_by_destination_and_folder() {
        let d = Dir::new("upsert");
        let store = PublishedStore::in_state_dir(&d.0);
        assert_eq!(store.load(), Loaded::Records(vec![]));
        let f = FolderName::parse("2030-03-10").unwrap();
        assert_eq!(store.find_id(Destination::Yamareco, &f), None);

        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idA"))
            .unwrap();
        store
            .upsert(rec(Destination::TravelJournal, "2030-03-10", "idB"))
            .unwrap();
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idA")
        );
        assert_eq!(
            store.find_id(Destination::TravelJournal, &f).as_deref(),
            Some("idB")
        );
        // 置き換え（行は増えない）
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idC"))
            .unwrap();
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idC")
        );
        let Loaded::Records(all) = store.load() else {
            panic!()
        };
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn unreadable_records_mean_nothing_is_managed_and_are_never_overwritten() {
        let d = Dir::new("bad");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = FolderName::parse("2030-03-10").unwrap();
        let job = "0123456789abcdef0123456789abcdef";
        let good_entry = format!(
            r#"{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{job}"}}"#
        );
        let bad: Vec<String> = vec![
            "{broken".into(),
            "[]".into(),
            r#"{"version":2,"entries":[]}"#.into(),
            r#"{"version":1}"#.into(),
            r#"{"version":1,"entries":{}}"#.into(),
            format!(r#"{{"version":1,"entries":[{good_entry},{good_entry}]}}"#),
            format!(r#"{{"version":1,"entries":[{{"destination":"other","folder":"2030-03-10","folder_id":"idA","job_id":"{job}"}}]}}"#),
            format!(r#"{{"version":1,"entries":[{{"destination":"yamareco","folder":"../x","folder_id":"idA","job_id":"{job}"}}]}}"#),
            format!(r#"{{"version":1,"entries":[{{"destination":"yamareco","folder":"2030-03-10","folder_id":"","job_id":"{job}"}}]}}"#),
            r#"{"version":1,"entries":[{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"short"}]}"#.into(),
            "x".repeat(MAX_BYTES as usize + 1),
        ];
        for content in bad {
            std::fs::write(store.path(), &content).unwrap();
            assert_eq!(
                store.load(),
                Loaded::Unreadable,
                "{}",
                &content[..content.len().min(60)]
            );
            assert_eq!(store.find_id(Destination::Yamareco, &f), None);
            assert!(
                store
                    .upsert(rec(Destination::Yamareco, "2030-03-10", "idZ"))
                    .is_err()
            );
            // 壊れた記録は残っている
            assert_eq!(std::fs::read(store.path()).unwrap(), content.as_bytes());
        }
        // 良い記録は読める
        std::fs::write(
            store.path(),
            format!(r#"{{"version":1,"entries":[{good_entry}]}}"#),
        )
        .unwrap();
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idA")
        );
    }
}
