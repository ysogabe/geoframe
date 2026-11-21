//! 公開の記録 `published.json`（design.md 4.2「管理しているフォルダの見分け方」、7 章）。
//!
//! 公開に成功するたびに、宛先の種類・フォルダ名・Drive のフォルダの ID・ジョブ ID・**ファイルごとの MD5** を 1 行足す
//! （同じ種類・同じフォルダ名の行は置き換える）。`replace` の対象にしてよいのは、宛先のフォルダの ID がここの行と一致し、
//! manifest の確認に通り、**中身の MD5 がここの MD5 と一致する**（公開した後に、同じ名前・同じ大きさのまま書き換えられていない）
//! フォルダだけ。行は間引かない。
//!
//! ```json
//! { "version": 1, "entries": [ { "destination": "yamareco", "folder": "2030-03-10", "folder_id": "…", "job_id": "…",
//!     "files": [ { "path": "090000_1.jpg", "md5": "<小文字の 16 進 32 文字>" }, { "path": "manifest.json", "md5": "…" } ] } ] }
//! ```
//!
//! 置き換えで旧フォルダを退避したあと、ゴミ箱へ移せないまま新しい行を書くとき（記録に失敗した・移動に失敗した）は、新しい行に
//! 旧フォルダの ID と MD5 を `previous` として残す（`{ "folder_id": "…", "files": [ … ] }`）。後の復旧が旧フォルダを
//! ゴミ箱へ移す前に、中身がこの MD5 のままかを確かめるため。ゴミ箱へ移せたら、次に書く行には残さない。
//!
//! `files` は、公開した一式の全部（写真と `manifest.json`）。**無い行（MD5 を記録する前の古い形）は読めるが、`replace` は断る**
//! （[`PublishedRecord::files`] が `None`。中身が変わっていないと確かめられないので、管理していないものとして扱う安全な側）。
//! `files` がある行で形が崩れていれば（空・パスや MD5 の形が違う・同じパスが 2 つ・`manifest.json` が無い）、記録全体が
//! 読めないもの（[`Loaded::Unreadable`]）になる。
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
use crate::rclone::{is_valid_drive_id, normalize_md5, rel_path_ok};
use crate::request::Destination;

/// 読む上限（行は 1 件が 300 バイト前後。1MiB で数千件）。
pub const MAX_BYTES: u64 = 1024 * 1024;
const VERSION: u64 = 1;

/// 公開したファイル 1 つの、一式の中の相対パスと MD5。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedFile {
    pub path: String,
    /// 小文字の 16 進 32 文字。
    pub md5: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedRecord {
    pub destination: Destination,
    pub folder: FolderName,
    pub folder_id: String,
    pub job_id: JobId,
    /// 公開した一式（写真と `manifest.json`）のファイルごとの MD5。`None` は MD5 を記録する前の古い形の行
    /// （`replace` は断る）か、公開の直後に Drive の一覧から MD5 を取れなかった行。
    pub files: Option<Vec<RecordedFile>>,
    /// 退避した旧フォルダをゴミ箱へ移せていないときの、旧フォルダの ID と MD5（[`PreviousFolder`]）。
    /// （大きな値なので `Box`。`Published` を含む列挙の大きさを抑える）
    pub previous: Option<Box<PreviousFolder>>,
}

/// 置き換えで退避した旧フォルダ（まだ `replaced/` にある）の ID と、公開したときのファイルごとの MD5。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousFolder {
    pub folder_id: String,
    pub files: Vec<RecordedFile>,
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

fn parse_files(v: &Value) -> Option<Vec<RecordedFile>> {
    let mut out: Vec<RecordedFile> = Vec::new();
    for f in v.as_array()? {
        let path = f.get("path")?.as_str()?;
        if !rel_path_ok(path) || out.iter().any(|x| x.path == path) {
            return None;
        }
        out.push(RecordedFile {
            path: path.to_owned(),
            md5: normalize_md5(f.get("md5")?.as_str()?)?,
        });
    }
    // 公開した一式には、必ず manifest.json がある
    out.iter().any(|f| f.path == "manifest.json").then_some(out)
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
            // 無い（古い形）は `None`。あって形が崩れていれば、記録全体を読めないものにする
            files: match e.get("files") {
                None => None,
                Some(f) => Some(parse_files(f)?),
            },
            previous: match e.get("previous") {
                None => None,
                Some(p) => Some(Box::new(PreviousFolder {
                    folder_id: {
                        let id = p.get("folder_id")?.as_str()?;
                        is_valid_drive_id(id).then(|| id.to_owned())?
                    },
                    files: parse_files(p.get("files")?)?,
                })),
            },
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

fn files_json(files: &[RecordedFile]) -> Value {
    files
        .iter()
        .map(|f| json!({ "path": f.path, "md5": f.md5 }))
        .collect::<Vec<_>>()
        .into()
}

fn to_json(records: &[PublishedRecord]) -> Value {
    json!({
        "version": VERSION,
        "entries": records.iter().map(|r| {
            let mut row = json!({
                "destination": r.destination.as_str(),
                "folder": r.folder.as_str(),
                "folder_id": r.folder_id,
                "job_id": r.job_id.as_str(),
            });
            if let Some(files) = &r.files {
                row["files"] = files_json(files);
            }
            if let Some(p) = &r.previous {
                row["previous"] = json!({ "folder_id": p.folder_id, "files": files_json(&p.files) });
            }
            row
        }).collect::<Vec<_>>(),
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

    /// 同じ種類・同じフォルダ名の行。行が無い・記録が読めないときは `None`（管理していない）。
    pub fn find(&self, destination: Destination, folder: &FolderName) -> Option<PublishedRecord> {
        match self.load() {
            Loaded::Records(records) => records
                .into_iter()
                .find(|r| r.destination == destination && &r.folder == folder),
            Loaded::Unreadable => None,
        }
    }

    /// フォルダ ID が `folder_id` のフォルダの、公開したときのファイルごとの MD5。同じ種類・同じフォルダ名の行が、その ID
    /// （`files` がある）か、その ID の `previous` のときだけ。行が無い・記録が読めない・ID が合わない・MD5 の記録が無いときは
    /// `None`（中身が変わっていないと確かめられない）。
    pub fn recorded_files_of(
        &self,
        destination: Destination,
        folder: &FolderName,
        folder_id: &str,
    ) -> Option<PreviousFolder> {
        let row = self.find(destination, folder)?;
        match (row.folder_id == folder_id, row.files, row.previous) {
            (true, Some(files), _) => Some(PreviousFolder {
                folder_id: row.folder_id,
                files,
            }),
            (false, _, Some(p)) if p.folder_id == folder_id => Some(*p),
            _ => None,
        }
    }

    /// 同じ種類・同じフォルダ名の行の、フォルダの ID。行が無い・記録が読めないときは `None`（管理していない）。
    pub fn find_id(&self, destination: Destination, folder: &FolderName) -> Option<String> {
        self.find(destination, folder).map(|r| r.folder_id)
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
            files: Some(files(&[("a.jpg", M1), ("manifest.json", M2)])),
            previous: None,
        }
    }

    // 架空の MD5
    const M1: &str = "11111111111111111111111111111111";
    const M2: &str = "22222222222222222222222222222222";

    fn files(v: &[(&str, &str)]) -> Vec<RecordedFile> {
        v.iter()
            .map(|(p, m)| RecordedFile {
                path: (*p).to_owned(),
                md5: (*m).to_owned(),
            })
            .collect()
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

    fn row_with_files(files: &str) -> String {
        format!(
            r#"{{"version":1,"entries":[{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"0123456789abcdef0123456789abcdef","files":{files}}}]}}"#
        )
    }

    #[test]
    fn files_are_written_and_read_back_and_an_old_row_without_files_is_kept_as_such() {
        let d = Dir::new("files");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = FolderName::parse("2030-03-10").unwrap();
        // 書いて読み戻す（MD5 は小文字）
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idA"))
            .unwrap();
        let got = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(
            got.files,
            Some(files(&[("a.jpg", M1), ("manifest.json", M2)]))
        );
        // 大文字の MD5 の記録は、小文字にそろえて読む
        std::fs::write(
            store.path(),
            row_with_files(&format!(
                r#"[{{"path":"manifest.json","md5":"{}"}}]"#,
                M1.replace('1', "A")
            )),
        )
        .unwrap();
        let got = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(
            got.files,
            Some(files(&[("manifest.json", &M1.replace('1', "a"))]))
        );

        // MD5 を記録する前の古い形の行（files が無い）は読める。files は None（replace は断る）で、ほかの行を足しても残る
        let job = "0123456789abcdef0123456789abcdef";
        std::fs::write(
            store.path(),
            format!(
                r#"{{"version":1,"entries":[{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{job}"}}]}}"#
            ),
        )
        .unwrap();
        let old = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(old.folder_id, "idA");
        assert_eq!(old.files, None);
        store
            .upsert(rec(Destination::TravelJournal, "2030-03-10", "idB"))
            .unwrap();
        let again = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(again.files, None);
        let text = std::fs::read_to_string(store.path()).unwrap();
        // 古い行は files なしのまま書き戻す（MD5 を作り出さない）。新しい行は files つき
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["entries"][0].get("files").is_none());
        assert!(v["entries"][1]["files"].is_array());
        // 同じ宛先を公開し直すと、files つきの行に置き換わる
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idC"))
            .unwrap();
        let new = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(new.folder_id, "idC");
        assert!(new.files.is_some());
    }

    #[test]
    fn the_previous_folder_is_kept_in_the_row_and_found_by_its_id() {
        let d = Dir::new("previous");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = FolderName::parse("2030-03-10").unwrap();
        let mut row = rec(Destination::Yamareco, "2030-03-10", "idNew");
        row.previous = Some(Box::new(PreviousFolder {
            folder_id: "idOld".into(),
            files: files(&[("manifest.json", M1)]),
        }));
        store.upsert(row.clone()).unwrap();
        assert_eq!(store.find(Destination::Yamareco, &f), Some(row));
        // 新しい ID でも、旧 ID（previous）でも、MD5 が引ける。別の ID は引けない
        let of = |id: &str| store.recorded_files_of(Destination::Yamareco, &f, id);
        assert_eq!(
            of("idNew").unwrap().files,
            files(&[("a.jpg", M1), ("manifest.json", M2)])
        );
        assert_eq!(of("idOld").unwrap().files, files(&[("manifest.json", M1)]));
        assert!(of("idOther").is_none());
        // 置き換え直すと（previous なし）、旧 ID は引けなくなる
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idNew"))
            .unwrap();
        assert!(of("idOld").is_none());
        // 形の崩れ（previous の ID・files）は、記録全体を読めないものにする
        let job = "0123456789abcdef0123456789abcdef";
        for previous in [
            r#"{"folder_id":"","files":[{"path":"manifest.json","md5":"11111111111111111111111111111111"}]}"#,
            r#"{"folder_id":"idOld"}"#,
            r#"{"folder_id":"idOld","files":[]}"#,
            r#"[]"#,
        ] {
            std::fs::write(
                store.path(),
                format!(
                    r#"{{"version":1,"entries":[{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{job}","previous":{previous}}}]}}"#
                ),
            )
            .unwrap();
            assert_eq!(store.load(), Loaded::Unreadable, "{previous}");
        }
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
            // files がある行の形の崩れ: 空・配列でない・MD5 やパスの形・同じパス・manifest.json が無い・キーが足りない
            row_with_files(r#"[]"#),
            row_with_files(r#"{}"#),
            row_with_files(r#"[{"path":"manifest.json","md5":"short"}]"#),
            row_with_files(&format!(r#"[{{"path":"manifest.json","md5":"{}z"}}]"#, &M1[1..])),
            row_with_files(r#"[{"path":"manifest.json","md5":5}]"#),
            row_with_files(r#"[{"path":"manifest.json"}]"#),
            row_with_files(&format!(r#"[{{"md5":"{M1}"}}]"#)),
            row_with_files(&format!(r#"[{{"path":"../a.jpg","md5":"{M1}"}},{{"path":"manifest.json","md5":"{M2}"}}]"#)),
            row_with_files(&format!(r#"[{{"path":"a.jpg","md5":"{M1}"}}]"#)),
            row_with_files(&format!(
                r#"[{{"path":"a.jpg","md5":"{M1}"}},{{"path":"a.jpg","md5":"{M2}"}},{{"path":"manifest.json","md5":"{M2}"}}]"#
            )),
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
