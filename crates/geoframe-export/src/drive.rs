//! Drive への照会と、1 回ごとの操作（design.md 4.2）。`rclone` を呼ぶ側で、判断（何をどの順で呼ぶか）は [`crate::publish`]。
//!
//! ## 照会の約束（どれも「Drive が答えなかった」を「無い」と取り違えない）
//!
//! - 「無い」と言えるのは、**親の一覧が成功したうえで、名前に一致するものが無いとき**だけ。場所そのものへの `lsjson` が失敗
//!   （終了コード 3 を含む）したら `drive_unavailable`。`incoming/`・`replaced/` は最初は無いので、作業用のフォルダ（親）を
//!   一覧して、あれば中を一覧する（[`Drive::work_children`]）。
//! - 一覧の形が違えば（[`crate::rclone::parse_lsjson`]）失敗。空の ID は形の検査で断るので、空どうしが「一致」することはない。
//! - ファイルの集合は、並べ替えた**リスト**で比べる（同じ名前のファイルを Drive は複数持てる。マップにすると 1 つに潰れて通る）。
//! - 「manifest が無い」と決めるのは、`cat` の終了コードではなく、**一覧**（直下に `manifest.json` がちょうど 1 つのファイルで 4MB 以下）
//!   から。`cat` の失敗は `drive_unavailable`。
//! - 「一覧が manifest と一致する」の比べ方は 1 つ（[`Expected::matches`]）。送信後の検証・管理しているフォルダの変更の確認・
//!   公開後の確認・復旧が、同じものを使う。
//!
//! ## ショートカット
//!
//! rclone の drive は、既定ではショートカットを指す先のものとして解決して一覧に出す（解決しないで一覧に出すこともある）。
//! 解決された項目の ID は、リンク先の ID とショートカット自身の ID をタブで結んだ複合 ID なので、
//! [`crate::rclone::parse_lsjson`] が複合 ID をショートカットとして分類する（一覧全体を断らない）。リンク切れは MIME タイプが
//! `…shortcut.dangling` で出る。念のため**同じ場所を 2 回**（`--drive-skip-shortcuts` ありと、`=false` を明示した無し）一覧し、
//! 無しのほうにだけある項目も、ショートカットとして数える（[`Drive::list_checked`]・[`classify`]）。配置のときに本物の rclone で
//! 挙動を確かめる項目。
//!
//! ## サイズ不明のファイル
//!
//! 一覧には `--drive-show-all-gdocs` を付ける（エクスポートできない Google のファイル、例えばフォームも出る）。ネイティブの
//! Google 文書やそうしたファイルは `Size: -1`。受け付けて「一式に無いファイル」として扱う（旧フォルダなら `modified`、公開後の
//! 照合なら `blocked`）。親フォルダの一覧に無関係な `Size: -1` の項目があっても、名前が一致するものだけを判定に使うので、
//! 一覧全体を `drive_unavailable` にはしない。
//!
//! ## ゴミ箱へ移すもの（`purge`）
//!
//! rclone で Drive のゴミ箱へ移せるのは**パスで指したフォルダだけ**。`root_folder_id=<ID>` の接続文字列で根を `purge` しても、
//! rclone の drive は何もしない（上流の `purgeCheck` は、根が空で `check=false` の分岐では消さない）ので、ID を固定した
//! ゴミ箱への移動はできない。そこで `purge` を呼ぶ先は、**誰とも共有しない作業用のフォルダの中の、乱数（128 ビットの
//! `job_id`）で決まる名前だけ**にする: `incoming/<job_id>` と `replaced/<種類>-<フォルダ名>-<job_id>`。残る競合は、オーナーが
//! その名前のフォルダを、照合から `purge` までの数秒のうちに作ることだけ。さらに `replaced/…` は `purge` の直前に、
//! 親・名前・旧 ID の照合に加えて、**中身が旧 manifest の一式と完全に一致する**ことを確かめ直す（停止中に足されたファイルごと
//! ゴミ箱へ移さない）。`incoming/<job_id>` は、記録した ID と一致するフォルダ 1 つだけのときに消す
//! （ID が記録されていなければ、名前だけでは消さない）。

use std::path::Path;

use serde_json::Value;

use crate::error::{ErrorCode, ExportError};
use crate::folder::FolderName;
use crate::image::PublishFile;
use crate::jobid::JobId;
use crate::published::PublishedStore;
use crate::rclone::{CopyScope, LsEntry, Rclone, RemoteLoc, RunError};
use crate::recovery::{Kind, Observed, Seen};
use crate::request::Destination;
use crate::settings::Settings;

/// 管理しているフォルダの確認で読む manifest の上限（design.md 4.2）。
pub const MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;
const MANIFEST_NAME: &str = "manifest.json";

fn unavailable(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::DriveUnavailable, detail)
}

// ---- Drive 上の場所 ----

/// 1 回の公開に関係する Drive 上の場所。接頭辞・作業用のフォルダは検査済みの設定から、残りは検査済みの部品（フォルダ名・
/// ジョブ ID・固定の名前）だけで作る。呼び出し側の文字列は入らない。
#[derive(Debug, Clone)]
pub struct Layout {
    destination: Destination,
    folder: FolderName,
    job: JobId,
    retired_name: String,
    prefix_loc: RemoteLoc,
    dest_loc: RemoteLoc,
    work_loc: RemoteLoc,
    incoming_root: RemoteLoc,
    incoming_loc: RemoteLoc,
    replaced_root: RemoteLoc,
    retired_loc: RemoteLoc,
    manifest_loc: RemoteLoc,
}

impl Layout {
    pub fn new(
        settings: &Settings,
        destination: Destination,
        folder: FolderName,
        job: JobId,
    ) -> Result<Self, ExportError> {
        let internal = || ExportError::new(ErrorCode::Internal, "Drive のパスを作れません");
        let prefix_loc = RemoteLoc::of(settings.prefix(destination));
        let dest_loc = prefix_loc.child(folder.as_str()).ok_or_else(internal)?;
        let work_loc = RemoteLoc::of(&settings.work_root);
        let incoming_root = work_loc.child("incoming").ok_or_else(internal)?;
        let incoming_loc = incoming_root.child(job.as_str()).ok_or_else(internal)?;
        let replaced_root = work_loc.child("replaced").ok_or_else(internal)?;
        let retired_name = format!("{}-{}-{}", destination.as_str(), folder, job);
        let retired_loc = replaced_root.child(&retired_name).ok_or_else(internal)?;
        let manifest_loc = dest_loc.child(MANIFEST_NAME).ok_or_else(internal)?;
        Ok(Self {
            destination,
            folder,
            job,
            retired_name,
            prefix_loc,
            dest_loc,
            work_loc,
            incoming_root,
            incoming_loc,
            replaced_root,
            retired_loc,
            manifest_loc,
        })
    }

    pub fn destination(&self) -> Destination {
        self.destination
    }
    pub fn folder(&self) -> &FolderName {
        &self.folder
    }
    pub fn job(&self) -> &JobId {
        &self.job
    }
    /// `replaced/` に置く名前（`<種類>-<フォルダ名>-<job_id>`）。
    pub fn retired_name(&self) -> &str {
        &self.retired_name
    }
    pub fn dest_path(&self) -> &str {
        self.dest_loc.as_str()
    }
    pub fn incoming_path(&self) -> &str {
        self.incoming_loc.as_str()
    }
    pub fn retired_path(&self) -> &str {
        self.retired_loc.as_str()
    }
}

// ---- 一覧と manifest の突き合わせ ----

/// 公開するフォルダの中身（相対パスとサイズ）。**並べ替えたリスト**で持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    files: Vec<(String, u64)>,
    dirs: Vec<String>,
}

fn rel_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.chars().any(char::is_control))
}

impl Expected {
    fn build(mut files: Vec<(String, u64)>) -> Self {
        files.sort();
        let mut dirs: Vec<String> = Vec::new();
        for (path, _) in &files {
            let mut parts: Vec<&str> = path.split('/').collect();
            parts.pop();
            for i in 1..=parts.len() {
                dirs.push(parts[..i].join("/"));
            }
        }
        dirs.sort();
        dirs.dedup();
        Self { files, dirs }
    }

    /// これから送る一式（[`crate::image::Prepared::files`]。最後が `manifest.json`）から。
    pub fn from_publish_files(files: &[PublishFile]) -> Self {
        Self::build(files.iter().map(|f| (f.path.clone(), f.size)).collect())
    }

    /// Drive の manifest（旧 manifest と同じ形）から: `photos` の `filename`・`file_size` と、`manifest.json` そのもの
    /// （大きさは一覧の値）。形が違えば `None`。
    pub fn from_manifest(manifest: &Value, manifest_size: u64) -> Option<Self> {
        let mut files = Vec::new();
        for p in manifest.get("photos")?.as_array()? {
            let name = p.get("filename")?.as_str()?;
            if !rel_ok(name) || name == MANIFEST_NAME {
                return None;
            }
            files.push((name.to_owned(), p.get("file_size")?.as_u64()?));
        }
        files.push((MANIFEST_NAME.to_owned(), manifest_size));
        Some(Self::build(files))
    }

    /// 一覧（`lsjson -R`）が、期待した中身と完全に一致するか。ファイルは（パス・サイズ）の並べ替えたリスト、フォルダは
    /// 期待したファイルの親だけ。ショートカットが 1 つでもあれば一致しない。
    pub fn matches(&self, entries: &[LsEntry]) -> bool {
        if entries.iter().any(LsEntry::is_shortcut) {
            return false;
        }
        let mut files: Vec<(String, u64)> = entries
            .iter()
            .filter(|e| !e.is_dir)
            .filter_map(|e| u64::try_from(e.size).ok().map(|s| (e.path.clone(), s)))
            .collect();
        if files.len() != entries.iter().filter(|e| !e.is_dir).count() {
            return false;
        }
        files.sort();
        let mut dirs: Vec<String> = entries
            .iter()
            .filter(|e| e.is_dir)
            .map(|e| e.path.clone())
            .collect();
        dirs.sort();
        files == self.files && dirs == self.dirs
    }
}

/// ショートカットまで含めた一覧の結果。
#[derive(Debug, Clone)]
pub struct Listing {
    /// `--drive-skip-shortcuts` ありの一覧。
    pub entries: Vec<LsEntry>,
    /// ショートカットがある、または 2 つの一覧が食い違う。
    pub has_shortcut: bool,
}

/// 接頭辞の一覧 2 つ（ショートカットを除いた `a` と、除かない `b`）から、`name` に一致する項目を ID・種類で返す。
/// `b` にだけある項目（解決されたショートカット）と、ショートカットの項目（MIME タイプがショートカット・リンク切れ・
/// 複合 ID）は、種類を [`Kind::Shortcut`] にする。
pub fn classify(a: &[LsEntry], b: &[LsEntry], name: &str) -> Vec<Seen> {
    let mut pool: Vec<&LsEntry> = a.iter().filter(|e| e.name == name).collect();
    let mut out = Vec::new();
    for e in b.iter().filter(|e| e.name == name) {
        if e.is_shortcut() {
            out.push(Seen::of(e));
            continue;
        }
        match pool
            .iter()
            .position(|x| x.id == e.id && x.is_dir == e.is_dir && !x.is_shortcut())
        {
            Some(pos) => {
                pool.remove(pos);
                out.push(Seen::of(e));
            }
            None => out.push(Seen {
                id: e.id.clone(),
                kind: Kind::Shortcut,
            }),
        }
    }
    // `a` にだけある項目（2 つの一覧の食い違い）も、見えたものとして数える
    out.extend(pool.into_iter().map(Seen::of));
    out
}

// ---- 宛先の状態 ----

/// 宛先の状態（`ambiguous` と照会の失敗は [`ExportError`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Absent,
    Exists { id: String },
}

/// このサービスが書いたフォルダか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Managed {
    /// 公開の記録の ID と manifest の `generator`・`destination`・`folder` が合い、中身が manifest と完全に一致する。
    Managed,
    /// 公開の記録に ID が無い・記録が読めない、または manifest が無い・`generator` や `destination`・`folder` が違う。
    NotManaged,
    /// このサービスが書いたが、manifest に無いファイルやフォルダが足されている・サイズが違う・足りない。
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub target: Target,
    /// `exists` のとき、直下と下のフォルダを含むファイル数（それ以外は 0）。
    pub existing_files: u64,
    /// `exists` で確認を頼んだときだけ。
    pub managed: Option<Managed>,
}

/// 取り除きの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removed {
    Done,
    /// 消すものが無かった。
    Nothing,
    /// 見えた項目が、記録した ID と違う・複数・フォルダでない（何も消していない）。
    Mismatch(Vec<Seen>),
    /// 項目は記録した旧フォルダの ID のフォルダ 1 つだが、中身が旧 manifest の一式と一致しない（停止中に足された・変えられた。
    /// 何も消していない）。見えていた項目を返す。
    ContentMismatch(Vec<Seen>),
}

/// [`Drive::folder_matches_its_manifest`] の対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// `incoming/<job_id>`。
    Incoming,
    /// 宛先。
    Dest,
    /// 退避先 `replaced/<種類>-<フォルダ名>-<job_id>`。
    Retired,
}

/// 照合の結果（移動のあと、など）。
pub type Check = Result<(), Observed>;

pub struct Drive<'a> {
    rclone: &'a Rclone,
    layout: &'a Layout,
}

impl<'a> Drive<'a> {
    pub fn new(rclone: &'a Rclone, layout: &'a Layout) -> Self {
        Self { rclone, layout }
    }

    pub fn layout(&self) -> &Layout {
        self.layout
    }

    // ---- 一覧 ----

    /// `--drive-skip-shortcuts` ありの一覧。場所が無い（終了コード 3）ことも含め、失敗は `drive_unavailable`。
    fn list(&self, loc: &RemoteLoc, recursive: bool) -> Result<Vec<LsEntry>, ExportError> {
        self.rclone
            .lsjson(loc, recursive, true)
            .map_err(RunError::into_query_error)
    }

    fn list_with_shortcuts(
        &self,
        loc: &RemoteLoc,
        recursive: bool,
    ) -> Result<Vec<LsEntry>, ExportError> {
        self.rclone
            .lsjson(loc, recursive, false)
            .map_err(RunError::into_query_error)
    }

    fn list_checked(&self, loc: &RemoteLoc, recursive: bool) -> Result<Listing, ExportError> {
        let a = self.list(loc, recursive)?;
        let b = self.list_with_shortcuts(loc, recursive)?;
        let mut pool: Vec<&LsEntry> = a.iter().collect();
        let mut has_shortcut = a.iter().any(LsEntry::is_shortcut);
        for e in &b {
            if e.is_shortcut() {
                has_shortcut = true;
                continue;
            }
            match pool
                .iter()
                .position(|x| x.path == e.path && x.id == e.id && x.is_dir == e.is_dir)
            {
                Some(pos) => {
                    pool.remove(pos);
                }
                None => has_shortcut = true,
            }
        }
        has_shortcut |= !pool.is_empty();
        Ok(Listing {
            entries: a,
            has_shortcut,
        })
    }

    /// 作業用のフォルダの直下の `incoming`・`replaced` の中。親（作業用のフォルダ）の一覧が成功したうえで、そのフォルダが
    /// 無ければ空、ちょうど 1 つのフォルダならその中の一覧。それ以外（同名が複数・ファイル）は失敗。
    ///
    /// どちらの一覧も、ショートカットを省かずに取る（同名のショートカットを「無い」と読んで、`mkdir`・`moveto` で同名の
    /// フォルダを作らないため。ショートカットは名前が一致すれば「同名がある」として数える）。
    fn work_children(&self, which: &str, loc: &RemoteLoc) -> Result<Vec<LsEntry>, ExportError> {
        let work = self.list_with_shortcuts(&self.layout.work_loc, false)?;
        let found: Vec<&LsEntry> = work.iter().filter(|e| e.name == which).collect();
        match found.as_slice() {
            [] => Ok(Vec::new()),
            [e] if e.is_dir && !e.is_shortcut() => self.list_with_shortcuts(loc, false),
            _ => Err(unavailable("作業用のフォルダの構成が想定外です")),
        }
    }

    fn incoming_entries(&self) -> Result<Vec<LsEntry>, ExportError> {
        let mut v = self.work_children("incoming", &self.layout.incoming_root)?;
        v.retain(|e| e.name == self.layout.job.as_str());
        Ok(v)
    }

    fn replaced_entries(&self) -> Result<Vec<LsEntry>, ExportError> {
        let mut v = self.work_children("replaced", &self.layout.replaced_root)?;
        v.retain(|e| e.name == self.layout.retired_name);
        Ok(v)
    }

    // ---- 宛先の状態 ----

    /// 接頭辞の直下で、フォルダ名に完全に一致する項目（ショートカットを含む）。
    pub fn dest_matches(&self) -> Result<Vec<Seen>, ExportError> {
        let a = self.list(&self.layout.prefix_loc, false)?;
        let b = self.list_with_shortcuts(&self.layout.prefix_loc, false)?;
        Ok(classify(&a, &b, self.layout.folder.as_str()))
    }

    /// 宛先の状態。2 つ以上・ファイル・ショートカットは `ambiguous_destination`、照会の失敗は `drive_unavailable`。
    pub fn target(&self) -> Result<Target, ExportError> {
        let seen = self.dest_matches()?;
        match seen.as_slice() {
            [] => Ok(Target::Absent),
            [s] if s.kind == Kind::Dir => Ok(Target::Exists { id: s.id.clone() }),
            _ => Err(ExportError::from(ErrorCode::AmbiguousDestination)),
        }
    }

    /// 宛先の状態と、`exists` のときの `existing_files`・管理しているフォルダか（`want_managed` のとき）。
    /// 呼ぶ `rclone` のコマンドは `lsjson` と `cat` だけ（`dry_run` もこれを使う）。
    pub fn inspect(
        &self,
        published: &PublishedStore,
        want_managed: bool,
    ) -> Result<Inspection, ExportError> {
        let Target::Exists { id } = self.target()? else {
            return Ok(Inspection {
                target: Target::Absent,
                existing_files: 0,
                managed: None,
            });
        };
        let listing = self.list_checked(&self.layout.dest_loc, true)?;
        let existing_files = listing.entries.iter().filter(|e| !e.is_dir).count() as u64;
        let managed = if want_managed {
            Some(self.check_managed(&id, &listing, published)?)
        } else {
            None
        };
        Ok(Inspection {
            target: Target::Exists { id },
            existing_files,
            managed,
        })
    }

    /// manifest の `generator`・`destination`・`folder` が、このサービスが書いたもので、この宛先のものか。
    fn is_ours(&self, manifest: &Value) -> bool {
        let text = |k: &str| manifest.get(k).and_then(Value::as_str);
        text("generator").is_some_and(|g| g.starts_with("geoframe-export "))
            && text("destination") == Some(self.layout.destination.as_str())
            && text("folder") == Some(self.layout.folder.as_str())
    }

    /// 管理しているフォルダの見分け方（design.md 4.2）。
    fn check_managed(
        &self,
        id: &str,
        listing: &Listing,
        published: &PublishedStore,
    ) -> Result<Managed, ExportError> {
        // 1. 公開の記録の同じ種類・同じフォルダ名の行の ID と一致する（記録が読めない・行が無ければ管理していない）
        match published.find_id(self.layout.destination, &self.layout.folder) {
            Some(recorded) if recorded == id => {}
            _ => return Ok(Managed::NotManaged),
        }
        // 2. 直下の manifest.json: 一覧から、ちょうど 1 つのファイルで 4MB 以下
        let top: Vec<&LsEntry> = listing
            .entries
            .iter()
            .filter(|e| e.path == MANIFEST_NAME)
            .collect();
        let size = match top.as_slice() {
            [m] if !m.is_dir
                && !m.is_shortcut()
                && (0..=MANIFEST_MAX_BYTES as i64).contains(&m.size) =>
            {
                m.size as u64
            }
            _ => return Ok(Managed::NotManaged),
        };
        // 3. 読んで、generator・destination・folder を見る（上限を掛けて読む。`cat` の失敗は drive_unavailable）
        let bytes = self
            .rclone
            .cat(&self.layout.manifest_loc, MANIFEST_MAX_BYTES)
            .map_err(RunError::into_query_error)?;
        // 一覧の大きさと違えば、読んだものを信用しない
        if bytes.len() as u64 != size {
            return Ok(Managed::Modified);
        }
        let Ok(manifest) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(Managed::NotManaged);
        };
        if !self.is_ours(&manifest) {
            return Ok(Managed::NotManaged);
        }
        // 4. 中身が manifest（と manifest.json 自身）に完全に一致する。足されたもの・サイズの違うものがあれば modified
        let Some(expected) = Expected::from_manifest(&manifest, size) else {
            return Ok(Managed::NotManaged);
        };
        if listing.has_shortcut || !expected.matches(&listing.entries) {
            return Ok(Managed::Modified);
        }
        Ok(Managed::Managed)
    }

    /// 宛先・`incoming`・`replaced` の今の状態（このジョブに関係する名前の項目だけ）。
    pub fn observe(&self) -> Result<Observed, ExportError> {
        let dest = self.dest_matches()?;
        let incoming = self.incoming_entries()?.iter().map(Seen::of).collect();
        let replaced = self.replaced_entries()?.iter().map(Seen::of).collect();
        Ok(Observed {
            dest,
            incoming,
            replaced,
        }
        .sorted())
    }

    // ---- 公開の操作 ----

    /// `incoming/<job_id>` を作る。
    pub fn mkdir_incoming(&self) -> Result<(), ExportError> {
        self.rclone
            .mkdir(&self.layout.incoming_loc)
            .map_err(RunError::into_op_error)
    }

    /// `incoming/<job_id>` の Drive の ID（作ったあと、親の一覧から。ちょうど 1 つのフォルダでなければ失敗）。
    pub fn incoming_id(&self) -> Result<String, ExportError> {
        match self.incoming_entries()?.as_slice() {
            [e] if e.is_dir && !e.is_shortcut() => Ok(e.id.clone()),
            _ => Err(unavailable("incoming のフォルダを確認できません")),
        }
    }

    /// 写真を送り、最後に manifest を送る。
    pub fn upload(&self, workspace: &Path) -> Result<(), ExportError> {
        self.rclone
            .copy(workspace, &self.layout.incoming_loc, CopyScope::PhotosOnly)
            .map_err(RunError::into_op_error)?;
        self.rclone
            .copy(
                workspace,
                &self.layout.incoming_loc,
                CopyScope::ManifestOnly,
            )
            .map_err(RunError::into_op_error)
    }

    /// `incoming/<job_id>` の中身が、期待した一式と完全に一致するか（`lsjson -R`）。
    pub fn incoming_matches(&self, expected: &Expected) -> Result<bool, ExportError> {
        let listing = self.list_checked(&self.layout.incoming_loc, true)?;
        Ok(!listing.has_shortcut && expected.matches(&listing.entries))
    }

    /// 宛先の中身が、期待した一式と完全に一致するか。
    pub fn dest_matches_expected(&self, expected: &Expected) -> Result<bool, ExportError> {
        let listing = self.list_checked(&self.layout.dest_loc, true)?;
        Ok(!listing.has_shortcut && expected.matches(&listing.entries))
    }

    /// フォルダの中身が、そのフォルダの manifest が言う一式と一致するか（復旧用。手元に一式が無いとき）。manifest は、
    /// このサービスが書いたもの（`generator`・`destination`・`folder`）でなければならない。
    pub fn folder_matches_its_manifest(&self, place: Place) -> Result<bool, ExportError> {
        let (dir, manifest) = match place {
            Place::Incoming => (
                &self.layout.incoming_loc,
                self.layout.incoming_loc.child(MANIFEST_NAME),
            ),
            Place::Dest => (
                &self.layout.dest_loc,
                Some(self.layout.manifest_loc.clone()),
            ),
            Place::Retired => (
                &self.layout.retired_loc,
                self.layout.retired_loc.child(MANIFEST_NAME),
            ),
        };
        let Some(manifest_loc) = manifest else {
            return Ok(false);
        };
        let listing = self.list_checked(dir, true)?;
        let top: Vec<&LsEntry> = listing
            .entries
            .iter()
            .filter(|e| e.path == MANIFEST_NAME)
            .collect();
        let size = match top.as_slice() {
            [m] if !m.is_dir
                && !m.is_shortcut()
                && (0..=MANIFEST_MAX_BYTES as i64).contains(&m.size) =>
            {
                m.size as u64
            }
            _ => return Ok(false),
        };
        let bytes = self
            .rclone
            .cat(&manifest_loc, MANIFEST_MAX_BYTES)
            .map_err(RunError::into_query_error)?;
        if bytes.len() as u64 != size {
            return Ok(false);
        }
        let Some(manifest) = serde_json::from_slice::<Value>(&bytes).ok() else {
            return Ok(false);
        };
        let Some(expected) = self
            .is_ours(&manifest)
            .then(|| Expected::from_manifest(&manifest, size))
            .flatten()
        else {
            return Ok(false);
        };
        Ok(!listing.has_shortcut && expected.matches(&listing.entries))
    }

    /// `incoming/<job_id>` をゴミ箱へ移す（`--drive-use-trash=true`）。先に一覧で、見えた項目が**記録した ID** のフォルダ
    /// ちょうど 1 つであることを確かめる（違えば何も消さずに [`Removed::Mismatch`]。無ければ [`Removed::Nothing`]）。
    /// ID は必ず渡す（名前だけでは消せない。ID を記録できていなければ、呼び出し側は消さずに残りものとして知らせる）。
    pub fn remove_incoming(&self, expect_id: &str) -> Result<Removed, ExportError> {
        let entries = self.incoming_entries()?;
        let seen: Vec<Seen> = entries.iter().map(Seen::of).collect();
        match seen.as_slice() {
            [] => Ok(Removed::Nothing),
            [s] if s.kind == Kind::Dir && s.id == expect_id => {
                self.rclone
                    .purge(&self.layout.incoming_loc)
                    .map_err(RunError::into_op_error)?;
                Ok(Removed::Done)
            }
            _ => Ok(Removed::Mismatch(seen)),
        }
    }

    /// 旧フォルダを `replaced/<種類>-<フォルダ名>-<job_id>` へ移す（パスでの移動。移したあとの照合は呼び出し側）。
    pub fn retire_old(&self) -> Result<(), ExportError> {
        self.rclone
            .moveto(&self.layout.dest_loc, &self.layout.retired_loc)
            .map_err(RunError::into_op_error)
    }

    /// `incoming/<job_id>` を宛先へ移す（パスでの移動。移したあとの照合は呼び出し側）。
    ///
    /// rclone の `moveto` は、宛先にフォルダができていると Drive の中でのフォルダの移動をやめて、ファイル単位の移動に切り替わる
    /// （上流の `MoveDir` が `ErrorDirExists` を受けた場合の fallback）。止める手段は無いので、残る条件は「直前に宛先が無いと
    /// 確かめてから移すまでの数秒に、オーナーが同じ名前のフォルダを作る」競合だけ。起きると、宛先のフォルダに写真が足され
    /// （消しはしない）、宛先の ID が `incoming` の ID と違うので、移したあとの照合で見つけて `blocked` にする。
    pub fn publish_move(&self) -> Result<(), ExportError> {
        self.rclone
            .moveto(&self.layout.incoming_loc, &self.layout.dest_loc)
            .map_err(RunError::into_op_error)
    }

    /// `incoming/<job_id>` の今の項目（ID・種類。無ければ空）。
    pub fn incoming_seen(&self) -> Result<Vec<Seen>, ExportError> {
        Ok(self.incoming_entries()?.iter().map(Seen::of).collect())
    }

    /// `replaced/<…>` の今の項目（ID・種類。無ければ空）。
    pub fn replaced_seen(&self) -> Result<Vec<Seen>, ExportError> {
        Ok(self.replaced_entries()?.iter().map(Seen::of).collect())
    }

    /// 退避した旧フォルダをゴミ箱へ移す。**移す前に**次をすべて確かめる（どれかが合わなければ、何も消さずに返す）。
    ///
    /// 1. `replaced/` の項目が、記録した旧フォルダの ID のフォルダ 1 つだけ。
    /// 2. そのフォルダの中身（`lsjson -R` の相対パスとサイズ）が、中の旧 manifest の一式と完全に一致する
    ///    （停止している間に足されたファイルごとゴミ箱へ移さない。旧 manifest は `cat` で読み直す）。
    /// 3. 2 の間に差し替わっていないことを、1 をもう一度確かめて見る。
    ///
    /// `purge` はパスで指す（ID を固定した `purge` は rclone ではできない。モジュールの説明）。指す名前は、`job_id`（乱数）を
    /// 含む、作業用のフォルダの中の名前だけ。
    pub fn trash_replaced(&self, old_id: &str) -> Result<Removed, ExportError> {
        let is_old = |seen: &[Seen]| matches!(seen, [s] if s.kind == Kind::Dir && s.id == old_id);
        let seen = self.replaced_seen()?;
        if seen.is_empty() {
            return Ok(Removed::Nothing);
        }
        if !is_old(&seen) {
            return Ok(Removed::Mismatch(seen));
        }
        if !self.folder_matches_its_manifest(Place::Retired)? {
            return Ok(Removed::ContentMismatch(seen));
        }
        let again = self.replaced_seen()?;
        if !is_old(&again) {
            return Ok(Removed::Mismatch(again));
        }
        self.rclone
            .purge(&self.layout.retired_loc)
            .map_err(RunError::into_op_error)?;
        Ok(Removed::Done)
    }

    // ---- 移動のあとの照合（終了コードを信じず、一覧で確かめる） ----

    /// 退避のあと: 宛先が無く、`replaced/<…>` が記録した旧フォルダの ID のフォルダ 1 つだけ。
    pub fn check_retired(&self, old_id: &str) -> Result<Check, ExportError> {
        let o = self.observe()?;
        let ok = o.dest.is_empty() && o.replaced == [Seen::dir(old_id)];
        Ok(if ok { Ok(()) } else { Err(o) })
    }

    /// 公開のあと: 宛先が記録した `incoming` の ID のフォルダ 1 つだけ、`incoming/<job_id>` が残っていない、
    /// 中身が一式と一致する。`expected` が無ければ（起動時の復旧）、宛先の manifest が言う一式と照合する。
    pub fn check_published(
        &self,
        incoming_id: &str,
        expected: Option<&Expected>,
    ) -> Result<Check, ExportError> {
        let o = self.observe()?;
        let placed = o.dest == [Seen::dir(incoming_id)] && o.incoming.is_empty();
        if !placed {
            return Ok(Err(o));
        }
        let content_ok = match expected {
            Some(e) => self.dest_matches_expected(e)?,
            None => self.folder_matches_its_manifest(Place::Dest)?,
        };
        if !content_ok {
            return Ok(Err(o));
        }
        Ok(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rclone::{DIRECTORY_MIME, SHORTCUT_MIME};

    fn file(path: &str, size: i64) -> LsEntry {
        LsEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            size,
            mime: "image/jpeg".into(),
            is_dir: false,
            id: format!("f{}", path.replace(['/', '.'], "_")),
            shortcut_id: None,
        }
    }
    fn dir(path: &str) -> LsEntry {
        LsEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            size: -1,
            mime: DIRECTORY_MIME.into(),
            is_dir: true,
            id: format!("d{}", path.replace('/', "_")),
            shortcut_id: None,
        }
    }

    fn pf(path: &str, size: u64) -> PublishFile {
        PublishFile {
            path: path.into(),
            size,
        }
    }

    #[test]
    fn expected_matches_exactly_the_published_set() {
        let e = Expected::from_publish_files(&[
            pf("2030-03-10/090000_1.jpg", 10),
            pf("2030-03-11/090000_2.jpg", 20),
            pf("manifest.json", 99),
        ]);
        let good = vec![
            dir("2030-03-10"),
            file("2030-03-10/090000_1.jpg", 10),
            dir("2030-03-11"),
            file("2030-03-11/090000_2.jpg", 20),
            file("manifest.json", 99),
        ];
        assert!(e.matches(&good));
        // 順序は関係ない
        let mut shuffled = good.clone();
        shuffled.reverse();
        assert!(e.matches(&shuffled));
        // 足りない・余分・サイズ違い・置き場所違い
        assert!(!e.matches(&good[..4]));
        let mut extra = good.clone();
        extra.push(file("memo.txt", 1));
        assert!(!e.matches(&extra));
        let mut size = good.clone();
        size[1] = file("2030-03-10/090000_1.jpg", 11);
        assert!(!e.matches(&size));
        let mut moved = good.clone();
        moved[1] = file("090000_1.jpg", 10);
        assert!(!e.matches(&moved));
        // 余分な空のフォルダ
        let mut empty_dir = good.clone();
        empty_dir.push(dir("2030-03-12"));
        assert!(!e.matches(&empty_dir));
        // ショートカット
        let mut sc = good.clone();
        sc.push(LsEntry {
            mime: SHORTCUT_MIME.into(),
            ..file("link", 0)
        });
        assert!(!e.matches(&sc));
    }

    #[test]
    fn unknown_size_files_are_not_part_of_any_set() {
        // ネイティブの Google 文書・エクスポートできないファイル（Size -1）が混ざった一覧は、一式と一致しない
        let e = Expected::from_publish_files(&[pf("a.jpg", 5), pf("manifest.json", 9)]);
        let good = vec![file("a.jpg", 5), file("manifest.json", 9)];
        assert!(e.matches(&good));
        let mut with_doc = good.clone();
        with_doc.push(LsEntry {
            mime: "application/vnd.google-apps.document".into(),
            ..file("memo", -1)
        });
        assert!(!e.matches(&with_doc));
        // 期待したファイルそのものの Size が -1 でも一致しない（サイズが分からないものは認めない）
        let unknown = vec![file("a.jpg", -1), file("manifest.json", 9)];
        assert!(!e.matches(&unknown));
    }

    #[test]
    fn a_composite_id_marks_a_resolved_shortcut_even_when_the_mime_type_looks_ordinary() {
        let name = "2030-03-10";
        let real = entry(name, "idR", true, "");
        // 解決されたフォルダのショートカット: MIME タイプはフォルダのまま、ID は複合 ID（前半がリンク先）
        let resolved = LsEntry {
            shortcut_id: Some("idS".into()),
            ..entry(name, "idT", true, "")
        };
        assert!(resolved.is_shortcut());
        let got = classify(
            std::slice::from_ref(&real),
            &[real.clone(), resolved.clone()],
            name,
        );
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|s| s.kind == Kind::Dir && s.id == "idR"));
        assert!(
            got.iter()
                .any(|s| s.kind == Kind::Shortcut && s.id == "idS"),
            "ショートカット自身の ID（recover の表示で見つけられるほう）"
        );
        // 解決されたショートカットだけがある（本物のフォルダは無い）→ ショートカット 1 つ（フォルダとしては数えない）
        let got = classify(&[], std::slice::from_ref(&resolved), name);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, Kind::Shortcut);
        // 一式の照合では、1 つでもあれば一致しない
        let e = Expected::from_publish_files(&[pf("a.jpg", 5), pf("manifest.json", 9)]);
        let mut set = vec![file("a.jpg", 5), file("manifest.json", 9)];
        assert!(e.matches(&set));
        set.push(LsEntry {
            shortcut_id: Some("idS".into()),
            ..file("pic.jpg", 5)
        });
        assert!(!e.matches(&set));
        // リンク切れ
        let dangling = LsEntry {
            mime: crate::rclone::SHORTCUT_DANGLING_MIME.into(),
            ..file("gone", 0)
        };
        let mut set = vec![file("a.jpg", 5), file("manifest.json", 9)];
        set.push(dangling);
        assert!(!e.matches(&set));
    }

    #[test]
    fn duplicates_are_not_collapsed() {
        // 同じ名前・同じサイズのファイルが 2 つある（Drive は許す）。期待は 1 つだけなので一致しない
        let e = Expected::from_publish_files(&[pf("a.jpg", 5), pf("manifest.json", 9)]);
        let mut dup = vec![file("a.jpg", 5), file("manifest.json", 9)];
        assert!(e.matches(&dup));
        let mut second = file("a.jpg", 5);
        second.id = "other".into();
        dup.push(second);
        assert!(!e.matches(&dup));
        // 同じ名前のフォルダが 2 つ
        let e = Expected::from_publish_files(&[pf("2030-03-10/a.jpg", 5), pf("manifest.json", 9)]);
        let mut dd = vec![
            dir("2030-03-10"),
            dir("2030-03-10"),
            file("2030-03-10/a.jpg", 5),
            file("manifest.json", 9),
        ];
        assert!(!e.matches(&dd));
        dd.remove(0);
        assert!(e.matches(&dd));
        // 期待のほうに同じ行が 2 つあっても、1 つだけの一覧とは一致しない
        let e2 =
            Expected::from_publish_files(&[pf("a.jpg", 5), pf("a.jpg", 5), pf("manifest.json", 9)]);
        assert!(!e2.matches(&[file("a.jpg", 5), file("manifest.json", 9)]));
    }

    #[test]
    fn expected_from_a_manifest_reads_filename_and_file_size() {
        let m = serde_json::json!({
            "photos": [
                {"filename": "2030-03-10/090000_1.jpg", "file_size": 10},
                {"filename": "2030-03-11/090000_2.jpg", "file_size": 20},
            ]
        });
        let e = Expected::from_manifest(&m, 99).unwrap();
        assert_eq!(
            e,
            Expected::from_publish_files(&[
                pf("2030-03-10/090000_1.jpg", 10),
                pf("2030-03-11/090000_2.jpg", 20),
                pf("manifest.json", 99),
            ])
        );
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"photos": 1}),
            serde_json::json!({"photos": [{"filename": "a.jpg"}]}),
            serde_json::json!({"photos": [{"file_size": 1}]}),
            serde_json::json!({"photos": [{"filename": "../a.jpg", "file_size": 1}]}),
            serde_json::json!({"photos": [{"filename": "/a.jpg", "file_size": 1}]}),
            serde_json::json!({"photos": [{"filename": "a.jpg", "file_size": -1}]}),
            serde_json::json!({"photos": [{"filename": "manifest.json", "file_size": 1}]}),
        ] {
            assert!(Expected::from_manifest(&bad, 1).is_none(), "{bad}");
        }
    }

    fn entry(name: &str, id: &str, is_dir: bool, mime: &str) -> LsEntry {
        LsEntry {
            path: name.into(),
            name: name.into(),
            size: if is_dir { -1 } else { 1 },
            mime: if is_dir {
                DIRECTORY_MIME.into()
            } else {
                mime.into()
            },
            is_dir,
            id: id.into(),
            shortcut_id: None,
        }
    }

    #[test]
    fn classify_finds_shortcuts_in_either_listing_mode() {
        let name = "2030-03-10";
        let real = entry(name, "idR", true, "");
        // 無し
        assert!(classify(&[], &[], name).is_empty());
        assert!(
            classify(
                &[entry("other", "i", true, "")],
                &[entry("other", "i", true, "")],
                name
            )
            .is_empty()
        );
        // 本物 1 つ
        assert_eq!(
            classify(
                std::slice::from_ref(&real),
                std::slice::from_ref(&real),
                name
            ),
            vec![Seen::dir("idR")]
        );
        // ショートカットが MIME タイプで見える（b にだけ）
        let sc = entry(name, "idS", false, SHORTCUT_MIME);
        let got = classify(&[], std::slice::from_ref(&sc), name);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, Kind::Shortcut);
        // ショートカットが指す先として見える（b にだけ、フォルダとして）
        let resolved = entry(name, "idT", true, "");
        let got = classify(std::slice::from_ref(&real), &[real.clone(), resolved], name);
        assert_eq!(got.len(), 2);
        assert!(
            got.iter()
                .any(|s| s.kind == Kind::Shortcut && s.id == "idT")
        );
        assert!(got.iter().any(|s| s.kind == Kind::Dir && s.id == "idR"));
        // 本物と同じ ID を指すショートカット（解決されて同じに見える）も、数が増えるので 2 つになる
        let twin = real.clone();
        let got = classify(std::slice::from_ref(&real), &[real.clone(), twin], name);
        assert_eq!(got.len(), 2);
        assert_eq!(got.iter().filter(|s| s.kind == Kind::Shortcut).count(), 1);
        // 名前が同じファイル
        let f = entry(name, "idF", false, "image/jpeg");
        assert_eq!(
            classify(std::slice::from_ref(&f), std::slice::from_ref(&f), name),
            vec![Seen {
                id: "idF".into(),
                kind: Kind::File
            }]
        );
    }
}
