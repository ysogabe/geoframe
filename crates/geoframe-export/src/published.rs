//! 公開の記録 `<state-dir>/published/<種類>-<フォルダ名>.json`（design.md 4.2「管理しているフォルダの見分け方」、7 章）。
//!
//! 公開に成功するたびに、宛先の種類・フォルダ名・Drive のフォルダの ID・ジョブ ID・**ファイルごとの MD5** を、その宛先の
//! ファイルに書く（宛先ごとに 1 ファイル。同じ種類・同じフォルダ名なら同じファイルで、置き換える）。`replace` の対象にしてよいのは、
//! 宛先のフォルダの ID がここの記録と一致し、manifest の確認に通り、**中身の MD5 がここの MD5 と一致する**（公開した後に、
//! 同じ名前・同じ大きさのまま書き換えられていない）フォルダだけ。記録は間引かない。
//!
//! 宛先ごとのファイルに分けるのは、1 つのファイルに全部を入れると、写真の多い書き出し（1 件で 2,000 枚、1 枚ごとに MD5 を持つ）を
//! 何件か記録しただけで上限を超えて全体が読めなくなり、以後すべての `replace` を断る（公開の記録も更新できない）ことになるため。
//! ファイル名は、検査済みの種類（[`Destination`]）と検査済みのフォルダ名（[`FolderName`]）だけから作る（種類は数字で始まらず、
//! フォルダ名は数字で始まるので、`<種類>-<フォルダ名>` の区切りはどの組み合わせでも 1 通りに決まる）。1 つのファイルが読めなくても、
//! ほかの宛先には影響しない。
//!
//! ```json
//! { "version": 1, "entry": { "destination": "yamareco", "folder": "2030-03-10", "folder_id": "…", "job_id": "…",
//!     "files": [ { "path": "090000_1.jpg", "md5": "<小文字の 16 進 32 文字>" }, { "path": "manifest.json", "md5": "…" } ] } }
//! ```
//!
//! 置き換えで旧フォルダを退避したあと、ゴミ箱へ移せないまま新しい記録を書くとき（記録に失敗した・移動に失敗した）は、新しい記録に
//! 旧フォルダの ID と MD5 を `previous` として残す（`{ "folder_id": "…", "files": [ … ] }`）。後の復旧が旧フォルダを
//! ゴミ箱へ移す前に、中身がこの MD5 のままかを確かめるため。ゴミ箱へ移せたら、次に書く記録には残さない。
//!
//! `files` は、公開した一式の全部（写真と `manifest.json`）。**無い記録（MD5 を記録する前の古い形）は読めるが、`replace` は断る**
//! （[`PublishedRecord::files`] が `None`。中身が変わっていないと確かめられないので、管理していないものとして扱う安全な側）。
//! `files` がある記録で形が崩れていれば（空・パスや MD5 の形が違う・同じパスが 2 つ・`manifest.json` が無い）、そのファイルは
//! 読めないもの（[`Loaded::Unreadable`]）になる。ファイルの中の種類・フォルダ名がファイル名と違うときも読めないものにする。
//!
//! 書き方はジョブの記録と同じ（[`crate::jsonfile::write_json_atomic_max`]）。1 ファイルの上限は [`MAX_BYTES`]（1 件の最大
//! ──写真 [`crate::listing::PHOTOS_MAX`] 枚と manifest、`previous` を含む──に十分な値）で、**書く前に**大きさを見て、超えるなら
//! 何も書かずに失敗する（書いたあとで読めなくならないように）。**ファイルが無い**ときは「記録が無い」として扱うが、
//! **読めない・形が崩れている**ときは [`Loaded::Unreadable`]: その宛先の `replace` は、管理していないものとして（`not_managed`）
//! 断る。書くときは、読めない記録を**上書きしない**（失敗を返す。オーナーが調べられるよう残す）。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{ErrorCode, ExportError};
use crate::folder::FolderName;
use crate::jobid::JobId;
use crate::jsonfile::{self, ReadError};
use crate::rclone::{is_valid_drive_id, normalize_md5, rel_path_ok};
use crate::request::Destination;

/// 1 ファイルの上限（読む上限であり、書く上限でもある）。写真 2,000 枚と manifest の MD5・パス（1 件が整形して 150 バイト前後）に、
/// `previous`（同じ大きさ）を足しても 1MiB に届かない。余裕を見て 2MiB。
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
const VERSION: u64 = 1;
/// 状態ディレクトリの下の、公開の記録を置くディレクトリ。
const DIR_NAME: &str = "published";

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
    /// 公開した一式（写真と `manifest.json`）のファイルごとの MD5。`None` は MD5 を記録する前の古い形の記録
    /// （`replace` は断る）か、公開の直後に MD5 を取れなかった記録。
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

/// 1 つの宛先の記録を読んだ結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// ファイルが無い（この宛先を公開したことがない、または記録していない）。
    Absent,
    Record(Box<PublishedRecord>),
    /// 読めない・形が崩れている（上限超え、JSON でない、版が違う、値が検査に通らない、種類・フォルダ名がファイル名と違う）。
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct PublishedStore {
    dir: PathBuf,
}

/// 一式のファイルの並びの検査（パス・MD5 の形、同じパスの重複、`manifest.json` があること）。
pub(crate) fn parse_files(v: &Value) -> Option<Vec<RecordedFile>> {
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

fn parse_entry(e: &Value) -> Option<PublishedRecord> {
    Some(PublishedRecord {
        destination: Destination::parse(e.get("destination")?.as_str()?)?,
        folder: FolderName::parse(e.get("folder")?.as_str()?)?,
        folder_id: {
            let id = e.get("folder_id")?.as_str()?;
            is_valid_drive_id(id).then(|| id.to_owned())?
        },
        job_id: JobId::parse(e.get("job_id")?.as_str()?)?,
        // 無い（古い形）は `None`。あって形が崩れていれば、そのファイルを読めないものにする
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
    })
}

fn parse_file(v: &Value) -> Option<PublishedRecord> {
    if v.get("version")?.as_u64()? != VERSION {
        return None;
    }
    parse_entry(v.get("entry")?)
}

pub(crate) fn files_json(files: &[RecordedFile]) -> Value {
    files
        .iter()
        .map(|f| json!({ "path": f.path, "md5": f.md5 }))
        .collect::<Vec<_>>()
        .into()
}

fn to_json(r: &PublishedRecord) -> Value {
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
    json!({ "version": VERSION, "entry": row })
}

impl PublishedStore {
    /// `<state-dir>/published/`（記録は、その下の宛先ごとのファイル）。
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(DIR_NAME),
        }
    }

    /// 記録を置くディレクトリ。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 宛先（種類・フォルダ名）の記録のファイル。名前は、検査済みの種類とフォルダ名だけから作る。
    pub fn path_of(&self, destination: Destination, folder: &FolderName) -> PathBuf {
        self.dir
            .join(format!("{}-{}.json", destination.as_str(), folder.as_str()))
    }

    /// 宛先の記録を読む。
    pub fn load(&self, destination: Destination, folder: &FolderName) -> Loaded {
        match jsonfile::read_json(&self.path_of(destination, folder), MAX_BYTES) {
            Ok(v) => match parse_file(&v) {
                // 中身の種類・フォルダ名がファイル名と違うものは、別の宛先の記録を取り違えるので読めないものにする
                Some(r) if r.destination == destination && &r.folder == folder => {
                    Loaded::Record(Box::new(r))
                }
                _ => Loaded::Unreadable,
            },
            Err(ReadError::NotFound) => Loaded::Absent,
            Err(_) => Loaded::Unreadable,
        }
    }

    /// 同じ種類・同じフォルダ名の記録。記録が無い・読めないときは `None`（管理していない）。
    pub fn find(&self, destination: Destination, folder: &FolderName) -> Option<PublishedRecord> {
        match self.load(destination, folder) {
            Loaded::Record(r) => Some(*r),
            Loaded::Absent | Loaded::Unreadable => None,
        }
    }

    /// フォルダ ID が `folder_id` のフォルダの、公開したときのファイルごとの MD5。同じ種類・同じフォルダ名の記録が、その ID
    /// （`files` がある）か、その ID の `previous` のときだけ。記録が無い・読めない・ID が合わない・MD5 の記録が無いときは
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

    /// 記録の**今のフォルダ**（`previous` ではなく `folder_id`）が `folder_id` で、`files` があるときの、その MD5。
    /// 起動時の復旧が、公開した中身を照らすのに使う（記録を書いた直後に止まったあとの照合）。
    pub fn current_files_of(
        &self,
        destination: Destination,
        folder: &FolderName,
        folder_id: &str,
    ) -> Option<Vec<RecordedFile>> {
        let row = self.find(destination, folder)?;
        (row.folder_id == folder_id).then_some(row.files).flatten()
    }

    /// 同じ種類・同じフォルダ名の記録の、フォルダの ID。記録が無い・読めないときは `None`（管理していない）。
    pub fn find_id(&self, destination: Destination, folder: &FolderName) -> Option<String> {
        self.find(destination, folder).map(|r| r.folder_id)
    }

    /// 記録を書く（同じ種類・フォルダ名の記録は置き換える）。その宛先の記録が読めないとき、書く大きさが [`MAX_BYTES`] を
    /// 超えるときは、何も書かずに失敗する。
    pub fn upsert(&self, record: PublishedRecord) -> Result<(), ExportError> {
        if self.load(record.destination, &record.folder) == Loaded::Unreadable {
            return Err(ExportError::new(
                ErrorCode::Internal,
                "公開の記録が読めないので、書きません",
            ));
        }
        crate::jobs::ensure_dir(&self.dir, "published/ を用意できません")?;
        jsonfile::write_json_atomic_max(
            &self.path_of(record.destination, &record.folder),
            &to_json(&record),
            MAX_BYTES,
        )
        .map_err(|e| {
            ExportError::new(
                ErrorCode::Internal,
                if e.kind() == std::io::ErrorKind::InvalidData {
                    "公開の記録が上限の大きさを超えるので、書きません"
                } else {
                    "公開の記録を書けません"
                },
            )
        })
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

    const JOB: &str = "0123456789abcdef0123456789abcdef";

    fn rec(dest: Destination, folder: &str, id: &str) -> PublishedRecord {
        PublishedRecord {
            destination: dest,
            folder: FolderName::parse(folder).unwrap(),
            folder_id: id.to_owned(),
            job_id: JobId::parse(JOB).unwrap(),
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

    fn folder(name: &str) -> FolderName {
        FolderName::parse(name).unwrap()
    }

    /// 宛先 yamareco・2030-03-10 のファイルに、`entry` の中身を書く。
    fn write_entry(store: &PublishedStore, entry: &str) {
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(
            store.path_of(Destination::Yamareco, &folder("2030-03-10")),
            format!(r#"{{"version":1,"entry":{entry}}}"#),
        )
        .unwrap();
    }

    fn entry_with_files(files: &str) -> String {
        format!(
            r#"{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{JOB}","files":{files}}}"#
        )
    }

    #[test]
    fn missing_file_means_no_record_and_upsert_replaces_by_destination_and_folder() {
        let d = Dir::new("upsert");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = folder("2030-03-10");
        assert_eq!(store.load(Destination::Yamareco, &f), Loaded::Absent);
        assert_eq!(store.find_id(Destination::Yamareco, &f), None);

        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idA"))
            .unwrap();
        store
            .upsert(rec(Destination::TravelJournal, "2030-03-10", "idB"))
            .unwrap();
        // 宛先ごとのファイル（published/<種類>-<フォルダ名>.json）
        assert!(store.dir().join("yamareco-2030-03-10.json").is_file());
        assert!(store.dir().join("travel-journal-2030-03-10.json").is_file());
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idA")
        );
        assert_eq!(
            store.find_id(Destination::TravelJournal, &f).as_deref(),
            Some("idB")
        );
        // 置き換え（ファイルは増えない）
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idC"))
            .unwrap();
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idC")
        );
        assert_eq!(std::fs::read_dir(store.dir()).unwrap().count(), 2);
        // 別のフォルダ名は別のファイル
        assert_eq!(
            store.find_id(Destination::Yamareco, &folder("2030-03-11")),
            None
        );
    }

    #[test]
    fn files_are_written_and_read_back_and_an_old_row_without_files_is_kept_as_such() {
        let d = Dir::new("files");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = folder("2030-03-10");
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
        write_entry(
            &store,
            &entry_with_files(&format!(
                r#"[{{"path":"manifest.json","md5":"{}"}}]"#,
                M1.replace('1', "A")
            )),
        );
        let got = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(
            got.files,
            Some(files(&[("manifest.json", &M1.replace('1', "a"))]))
        );

        // MD5 を記録する前の古い形（files が無い）は読める。files は None（replace は断る）で、ほかの宛先を足しても残る
        write_entry(
            &store,
            &format!(
                r#"{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{JOB}"}}"#
            ),
        );
        let old = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(old.folder_id, "idA");
        assert_eq!(old.files, None);
        store
            .upsert(rec(Destination::TravelJournal, "2030-03-10", "idB"))
            .unwrap();
        let again = store.find(Destination::Yamareco, &f).unwrap();
        assert_eq!(again.files, None);
        // 古い記録は files なしのまま書き戻す（MD5 を作り出さない）
        store.upsert(again).unwrap();
        let text = std::fs::read_to_string(store.path_of(Destination::Yamareco, &f)).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["entry"].get("files").is_none());
        // 同じ宛先を公開し直すと、files つきの記録に置き換わる
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
        let f = folder("2030-03-10");
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
        // 今のフォルダの MD5（previous は含めない）
        let current = |id: &str| store.current_files_of(Destination::Yamareco, &f, id);
        assert_eq!(
            current("idNew"),
            Some(files(&[("a.jpg", M1), ("manifest.json", M2)]))
        );
        assert_eq!(current("idOld"), None);
        // 置き換え直すと（previous なし）、旧 ID は引けなくなる
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idNew"))
            .unwrap();
        assert!(of("idOld").is_none());
        // 形の崩れ（previous の ID・files）は、そのファイルを読めないものにする
        for previous in [
            r#"{"folder_id":"","files":[{"path":"manifest.json","md5":"11111111111111111111111111111111"}]}"#,
            r#"{"folder_id":"idOld"}"#,
            r#"{"folder_id":"idOld","files":[]}"#,
            r#"[]"#,
        ] {
            write_entry(
                &store,
                &format!(
                    r#"{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{JOB}","previous":{previous}}}"#
                ),
            );
            assert_eq!(
                store.load(Destination::Yamareco, &f),
                Loaded::Unreadable,
                "{previous}"
            );
        }
    }

    #[test]
    fn unreadable_records_mean_nothing_is_managed_and_are_never_overwritten() {
        let d = Dir::new("bad");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = folder("2030-03-10");
        let path = store.path_of(Destination::Yamareco, &f);
        std::fs::create_dir_all(store.dir()).unwrap();
        let good_entry = format!(
            r#"{{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"{JOB}"}}"#
        );
        let bad: Vec<String> = vec![
            "{broken".into(),
            "[]".into(),
            r#"{"version":2,"entry":{}}"#.into(),
            r#"{"version":1}"#.into(),
            r#"{"version":1,"entry":[]}"#.into(),
            // 旧い 1 ファイルの形（entries）は読まない
            format!(r#"{{"version":1,"entries":[{good_entry}]}}"#),
            format!(r#"{{"version":1,"entry":{{"destination":"other","folder":"2030-03-10","folder_id":"idA","job_id":"{JOB}"}}}}"#),
            format!(r#"{{"version":1,"entry":{{"destination":"yamareco","folder":"../x","folder_id":"idA","job_id":"{JOB}"}}}}"#),
            format!(r#"{{"version":1,"entry":{{"destination":"yamareco","folder":"2030-03-10","folder_id":"","job_id":"{JOB}"}}}}"#),
            r#"{"version":1,"entry":{"destination":"yamareco","folder":"2030-03-10","folder_id":"idA","job_id":"short"}}"#.into(),
            // ファイル名と違う宛先・フォルダ名の記録（別の宛先の記録の取り違え）
            format!(r#"{{"version":1,"entry":{{"destination":"travel-journal","folder":"2030-03-10","folder_id":"idA","job_id":"{JOB}"}}}}"#),
            format!(r#"{{"version":1,"entry":{{"destination":"yamareco","folder":"2030-03-11","folder_id":"idA","job_id":"{JOB}"}}}}"#),
            // files がある記録の形の崩れ: 空・配列でない・MD5 やパスの形・同じパス・manifest.json が無い・キーが足りない
            wrap(&entry_with_files(r#"[]"#)),
            wrap(&entry_with_files(r#"{}"#)),
            wrap(&entry_with_files(r#"[{"path":"manifest.json","md5":"short"}]"#)),
            wrap(&entry_with_files(&format!(r#"[{{"path":"manifest.json","md5":"{}z"}}]"#, &M1[1..]))),
            wrap(&entry_with_files(r#"[{"path":"manifest.json","md5":5}]"#)),
            wrap(&entry_with_files(r#"[{"path":"manifest.json"}]"#)),
            wrap(&entry_with_files(&format!(r#"[{{"md5":"{M1}"}}]"#))),
            wrap(&entry_with_files(&format!(r#"[{{"path":"../a.jpg","md5":"{M1}"}},{{"path":"manifest.json","md5":"{M2}"}}]"#))),
            wrap(&entry_with_files(&format!(r#"[{{"path":"a.jpg","md5":"{M1}"}}]"#))),
            wrap(&entry_with_files(&format!(
                r#"[{{"path":"a.jpg","md5":"{M1}"}},{{"path":"a.jpg","md5":"{M2}"}},{{"path":"manifest.json","md5":"{M2}"}}]"#
            ))),
            "x".repeat(MAX_BYTES as usize + 1),
        ];
        for content in bad {
            std::fs::write(&path, &content).unwrap();
            assert_eq!(
                store.load(Destination::Yamareco, &f),
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
            assert_eq!(std::fs::read(&path).unwrap(), content.as_bytes());
        }
        // 良い記録は読める
        write_entry(&store, &good_entry);
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idA")
        );
    }

    fn wrap(entry: &str) -> String {
        format!(r#"{{"version":1,"entry":{entry}}}"#)
    }

    #[test]
    fn a_broken_file_does_not_affect_other_destinations() {
        let d = Dir::new("isolated");
        let store = PublishedStore::in_state_dir(&d.0);
        let y = Destination::Yamareco;
        let t = Destination::TravelJournal;
        store.upsert(rec(y, "2030-03-10", "idA")).unwrap();
        store.upsert(rec(y, "2030-03-11", "idB")).unwrap();
        store.upsert(rec(t, "2030-03-10", "idC")).unwrap();
        // 1 つだけ壊す
        std::fs::write(store.path_of(y, &folder("2030-03-10")), b"{broken").unwrap();
        // 壊れた宛先は管理していないものとして扱い、書き直しも断る（壊れたまま残す）
        assert_eq!(store.load(y, &folder("2030-03-10")), Loaded::Unreadable);
        assert_eq!(store.find_id(y, &folder("2030-03-10")), None);
        assert!(store.upsert(rec(y, "2030-03-10", "idZ")).is_err());
        assert_eq!(
            std::fs::read(store.path_of(y, &folder("2030-03-10"))).unwrap(),
            b"{broken"
        );
        // ほかの宛先は引けるし、書ける
        assert_eq!(
            store.find_id(y, &folder("2030-03-11")).as_deref(),
            Some("idB")
        );
        assert_eq!(
            store.find_id(t, &folder("2030-03-10")).as_deref(),
            Some("idC")
        );
        store.upsert(rec(y, "2030-03-11", "idD")).unwrap();
        store.upsert(rec(t, "2030-03-12", "idE")).unwrap();
        assert_eq!(
            store.find_id(y, &folder("2030-03-11")).as_deref(),
            Some("idD")
        );
        assert_eq!(
            store.find_id(t, &folder("2030-03-12")).as_deref(),
            Some("idE")
        );
    }

    /// 写真 `n` 枚と manifest の MD5 の並び（名前は 1 枚ごとに違う。MD5 も変える）。
    fn big_files(n: usize, name_len: usize) -> Vec<RecordedFile> {
        let mut v: Vec<RecordedFile> = (0..n)
            .map(|i| RecordedFile {
                path: format!("{:0>name_len$}.jpg", i),
                md5: format!("{i:032x}"),
            })
            .collect();
        v.push(RecordedFile {
            path: "manifest.json".into(),
            md5: M2.into(),
        });
        v
    }

    #[test]
    fn many_destinations_and_the_largest_row_can_all_be_found() {
        let d = Dir::new("many");
        let store = PublishedStore::in_state_dir(&d.0);
        // 1 件の最大（写真 2,000 枚と manifest。置き換えで旧フォルダも残している）。全体を 1 ファイルにしていたら 1MiB の上限を
        // 5 件ほどで超えた大きさを、60 件の宛先に記録する
        let n = crate::listing::PHOTOS_MAX;
        let mut names = Vec::new();
        for day in 1..=30 {
            for dest in [Destination::Yamareco, Destination::TravelJournal] {
                let name = format!("2030-04-{day:02}");
                let mut row = rec(dest, &name, &format!("id{day}"));
                row.files = Some(big_files(n, 12));
                row.previous = Some(Box::new(PreviousFolder {
                    folder_id: "idOld".into(),
                    files: big_files(n, 12),
                }));
                store.upsert(row).unwrap();
                names.push((dest, name));
            }
        }
        // 1 ファイルは上限（MAX_BYTES）に収まり、全体の大きさは 1MiB を大きく超える
        let total: u64 = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total > 10 * 1024 * 1024, "{total}");
        let one = std::fs::metadata(store.path_of(Destination::Yamareco, &folder("2030-04-01")))
            .unwrap()
            .len();
        assert!(
            one < MAX_BYTES / 2,
            "最大の 1 件は上限の半分に収まる: {one}"
        );
        for (i, (dest, name)) in names.iter().enumerate() {
            let day = i / 2 + 1;
            let f = folder(name);
            assert_eq!(
                store.find_id(*dest, &f).as_deref(),
                Some(format!("id{day}").as_str())
            );
            assert_eq!(store.find(*dest, &f).unwrap().files.unwrap().len(), n + 1);
        }
        // 置き換えて書き直しても、ほかの宛先は読める
        store
            .upsert(rec(Destination::Yamareco, "2030-04-05", "idNew"))
            .unwrap();
        assert_eq!(
            store
                .find_id(Destination::Yamareco, &folder("2030-04-05"))
                .as_deref(),
            Some("idNew")
        );
        assert!(
            store
                .find(Destination::TravelJournal, &folder("2030-04-30"))
                .is_some()
        );
    }

    #[test]
    fn a_row_over_the_limit_is_not_written_and_the_old_record_stays() {
        let d = Dir::new("toolarge");
        let store = PublishedStore::in_state_dir(&d.0);
        let f = folder("2030-03-10");
        store
            .upsert(rec(Destination::Yamareco, "2030-03-10", "idOld"))
            .unwrap();
        let path = store.path_of(Destination::Yamareco, &f);
        let before = std::fs::read(&path).unwrap();
        // パスを長くして上限を超える（1 つ 1000 文字 × 2,000 枚 ≒ 2MiB 超）
        let mut big = rec(Destination::Yamareco, "2030-03-10", "idBig");
        big.files = Some(big_files(crate::listing::PHOTOS_MAX, 1000));
        let err = store.upsert(big).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        // 書かずに失敗し、前の記録も一時ファイルも残さない
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(store.dir()).unwrap().count(), 1);
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idOld")
        );
        // 上限に収まる大きさなら書ける
        let mut ok = rec(Destination::Yamareco, "2030-03-10", "idOk");
        ok.files = Some(big_files(crate::listing::PHOTOS_MAX, 100));
        store.upsert(ok).unwrap();
        assert_eq!(
            store.find_id(Destination::Yamareco, &f).as_deref(),
            Some("idOk")
        );
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_BYTES);
    }
}
