//! Drive への照会と、1 回ごとの操作（design.md 4.2、docs/note-photo-export/drive-api.md 4 章）。
//!
//! 子モジュール [`api`]・[`auth`]・[`http`] は、Drive API を**ID で指して**直接呼ぶ操作の型。この階層の [`Drive`] は、中立の入口
//! （[`crate::remote::Remote`]）の上で、1 回の公開に関係する場所（宛先・`incoming/<job_id>`・`replaced/<…>`）を扱う。何をどの順で
//! 呼ぶかの判断は [`crate::publish`]。
//!
//! ## 場所と ID
//!
//! 宛先の親フォルダ（`yamareco`・`travel-journal`）と作業用のフォルダは、起動引数の **Drive の ID** で指す（[`Layout`]）。その下の
//! フォルダ（宛先のフォルダ・`incoming/`・`incoming/<job_id>`・`replaced/`・`replaced/<…>`）は、**親の直下を一覧して、名前の完全一致で
//! 照合して**ID を得る（名前で `q` を絞らない）。以後の操作（一覧・読み出し・移動・ゴミ箱へ移す）は、照合した ID で行う。
//!
//! ## 照会の約束（どれも「Drive が答えなかった」を「無い」と取り違えない）
//!
//! - Drive の 404 は、「無い」とも「見えない」とも言い切れない。記録した ID のフォルダの木をたどる途中の 404（どのページ・どの子
//!   フォルダでも）は、照会の失敗（`drive_unavailable`）。「その ID のフォルダが無い」（一式と一致しない）と言えるのは、**404 のあとに
//!   親の一覧が成功し、その ID がその場所に無いことを確かめられたとき**だけ（[`Drive::folder_matches_expected`]・[`Under`]）。
//! - 「無い」と言えるのは、**親の一覧が成功したうえで、名前に一致するものが無いとき**だけ。`incoming/`・`replaced/` は最初は無いので、
//!   作業用のフォルダ（親）を一覧して、あれば中を一覧する（[`Drive::work_children`]）。一覧の失敗は `drive_unavailable`。
//! - 一覧の形が違えば（API の応答が決められた形でなければ）失敗。空の ID は形の検査で断るので、空どうしが「一致」することはない。
//! - ファイルの集合は、並べ替えた**リスト**で比べる（同じ名前のファイルを Drive は複数持てる。マップにすると 1 つに潰れて通る）。
//! - 「manifest が無い」と決めるのは、読み出しの失敗ではなく、**一覧**（直下に `manifest.json` がちょうど 1 つのファイルで 4MB 以下）
//!   から。読み出しの失敗は `drive_unavailable`。読んだものが一覧の大きさを超えた・違えば、読んだものを信用しない。
//! - 「一覧が manifest と一致する」の比べ方は 1 つ（[`Expected::matches`]）。送信後の検証・管理しているフォルダの変更の確認・
//!   公開後の確認・復旧が、同じものを使う。
//! - 一致の条件は、相対パス・サイズに加えて、**MD5**（`md5Checksum`）。期待する MD5 が分かっているとき（これから送る一式の検証、
//!   公開の記録 `published/` の MD5 と照らす `replace` の前の確認・ゴミ箱へ移す前の確認）は、一覧の MD5 が一致しなければ一致しない
//!   （同じ名前・同じ大きさのまま中身を書き換えられたものを見落とさない）。MD5 を持たないファイル（Google のネイティブ文書など）は、
//!   期待した MD5 を持てないので「一式に無いファイル」として断る。期待する MD5 が無いのは、手元にも記録にも一式の MD5 が無い、
//!   起動時の復旧の照合（宛先の manifest だけで照合する）だけ。
//!
//! ## ショートカット
//!
//! API の一覧では、ショートカットは解決されずに、MIME タイプ `application/vnd.google-apps.shortcut` の 1 件として出る
//! （[`crate::remote::Entry::is_shortcut`]）。同名のショートカットは「同名のものがある」として数える（宛先の判定は
//! `ambiguous_destination`、一式の照合は不一致）。
//!
//! ## サイズ不明のファイル
//!
//! Google のネイティブ文書やフォームには `size` が無く、`Size: -1` として扱う。受け付けて「一式に無いファイル」として扱う
//! （旧フォルダなら `modified`、公開後の照合なら `blocked`）。親フォルダの一覧に無関係な `Size: -1` の項目があっても、名前が一致する
//! ものだけを判定に使うので、一覧全体を `drive_unavailable` にはしない。
//!
//! ## 作ったフォルダの ID は、書き込みまで引き継ぐ
//!
//! `incoming/<job_id>` は、作った [`Drive::mkdir_incoming`] が、`create_folder` が返した ID を、直後の一覧（ちょうど 1 つのフォルダ）と
//! **照合して**から返す（食い違えば何も記録せずに失敗）。以後の送信（[`Drive::upload`] の親）・検証（[`Drive::folder_matches_expected`]）・
//! 移動（[`Drive::publish_move`]）・復旧は、**名前で引き直さず、記録した ID を受け取る**。旧フォルダの退避（[`Drive::retire_old`]）も、
//! 公開の記録・ジョブの記録の ID と一致した旧フォルダの ID を受け取る。移す直前に一覧して、その ID が見えなければ（動かされて同名の別の
//! フォルダだけが残っているなど）、何も変えずに止める（照合の失敗。復旧の表に渡る）。
//!
//! ## ゴミ箱へ移すもの
//!
//! ゴミ箱へ移すのは、**事前に照合した ID** のフォルダだけ（完全な削除はしない。ゴミ箱から戻せる）。`incoming/<job_id>` は、記録した ID と
//! 一致するフォルダ 1 つだけのときに消す（ID が記録されていなければ、名前だけでは消さない）。`replaced/…` は、親・名前・旧 ID の照合に加えて、
//! **中身が旧 manifest の一式と完全に一致する**ことを確かめ、もう一度見てから移す（停止中に足されたファイルごとゴミ箱へ移さない）。
//! パスで指していた rclone 版と違い、移す先が「その瞬間に同じ名前の別のフォルダ」に化ける競合は無い（ID を指すので）。
//!
//! ## 移動・ゴミ箱は、処理されなかったあと、照合をやり直してから再送する
//!
//! API の層は、移動・ゴミ箱へ移すを、どの失敗のあとも再送しない（429・流量制限の 403・401 のあとも。`ApiError::NotProcessed`）。待つ間に
//! 旧フォルダへファイルが足されたりフォルダが動かされたりしていても、同じ ID をもう一度更新すると、それを知らずに動かすため。この階層の
//! `Drive::with_resends` が、待ってから（`Retry-After` を含む。上限で切る）、操作ごとの照合（`incoming/` の一覧と記録した ID・
//! `replaced/` の場所・ID・旧 manifest の一式と記録した MD5・先に `get` で目的の状態でないか・移す直前の確認）をやり直して再送する。
//! 回数の上限と期限（照会の期限・ジョブ全体の期限）を掛ける。
//!
//! 公開のあとの照合（[`Drive::check_published`]）は、中身を ID で照合したあと、もう一度宛先を観察する（[`Drive::still_placed`]）。照合の間に
//! 同じ ID のフォルダが宛先から動かされても、ID では中身が読めるため。

pub mod api;
pub mod auth;
pub mod http;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::error::{ErrorCode, ExportError};
use crate::folder::FolderName;
use crate::image::PublishFile;
use crate::jobid::JobId;
use crate::published::{PublishedStore, RecordedFile};
use crate::recovery::{Kind, Observed, Seen};
use crate::remote::{Entry, Remote, RemoteError};
use crate::request::Destination;
use crate::settings::Settings;

/// 管理しているフォルダの確認で読む manifest の上限（design.md 4.2）。
pub const MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;
const MANIFEST_NAME: &str = "manifest.json";
/// 写真を送る並列の数（設計 4 章）。
pub const UPLOAD_PARALLEL: usize = 4;
const INCOMING: &str = "incoming";
const REPLACED: &str = "replaced";

fn unavailable(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::DriveUnavailable, detail)
}

// ---- Drive 上の場所 ----

/// 1 回の公開に関係する Drive 上の場所。宛先の親フォルダ・作業用のフォルダは検査済みの設定の **ID** から、残りは検査済みの部品
/// （フォルダ名・ジョブ ID・固定の名前）だけで決める。呼び出し側の文字列は入らない。親の下の ID は、一覧の名前の照合で得る（[`Drive`]）。
#[derive(Debug, Clone)]
pub struct Layout {
    destination: Destination,
    folder: FolderName,
    job: JobId,
    retired_name: String,
    prefix_id: String,
    work_id: String,
}

impl Layout {
    pub fn new(
        settings: &Settings,
        destination: Destination,
        folder: FolderName,
        job: JobId,
    ) -> Result<Self, ExportError> {
        let retired_name = format!("{}-{}-{}", destination.as_str(), folder, job);
        Ok(Self {
            destination,
            folder,
            job,
            retired_name,
            prefix_id: settings.prefix_id(destination).to_owned(),
            work_id: settings.work_folder.clone(),
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
    /// 宛先の場所の説明（`<種類>/<フォルダ名>`。種類の親フォルダの下のフォルダ。ID は含めない）。記録・表示用。
    pub fn dest_path(&self) -> String {
        format!("{}/{}", self.destination.as_str(), self.folder)
    }
    /// `incoming/<job_id>` の説明（作業用のフォルダの下）。
    pub fn incoming_path(&self) -> String {
        format!("{INCOMING}/{}", self.job)
    }
    /// `replaced/<…>` の説明（作業用のフォルダの下）。
    pub fn retired_path(&self) -> String {
        format!("{REPLACED}/{}", self.retired_name)
    }
}

// ---- 一覧と manifest の突き合わせ ----

/// 公開するフォルダの中身（相対パスとサイズ）。**並べ替えたリスト**で持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// （相対パス、サイズ、MD5）。MD5 が `None` のファイルは、MD5 を比べない。
    files: Vec<(String, u64, Option<String>)>,
    dirs: Vec<String>,
}

fn rel_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.chars().any(char::is_control))
}

impl Expected {
    fn build(mut files: Vec<(String, u64, Option<String>)>) -> Self {
        files.sort();
        let mut dirs: Vec<String> = Vec::new();
        for (path, _, _) in &files {
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
        Self::build(
            files
                .iter()
                .map(|f| (f.path.clone(), f.size, Some(f.md5.clone())))
                .collect(),
        )
    }

    /// Drive の manifest（旧 manifest と同じ形）から: `photos` の `filename`・`file_size` と、`manifest.json` そのもの
    /// （大きさは一覧の値）。形が違えば `None`。MD5 は持たない（[`Self::with_md5`] で足す）。
    pub fn from_manifest(manifest: &Value, manifest_size: u64) -> Option<Self> {
        let mut files = Vec::new();
        for p in manifest.get("photos")?.as_array()? {
            let name = p.get("filename")?.as_str()?;
            if !rel_ok(name) || name == MANIFEST_NAME {
                return None;
            }
            files.push((name.to_owned(), p.get("file_size")?.as_u64()?, None));
        }
        files.push((MANIFEST_NAME.to_owned(), manifest_size, None));
        Some(Self::build(files))
    }

    /// 公開の記録の MD5 を、ファイルごとに足す。記録のパスの集合が、期待したファイルのパスの集合と**完全に同じ**でなければ
    /// `None`（記録と manifest が食い違っている）。
    pub fn with_md5(mut self, recorded: &[RecordedFile]) -> Option<Self> {
        if recorded.len() != self.files.len() {
            return None;
        }
        for (path, _, md5) in &mut self.files {
            let mut hits = recorded.iter().filter(|r| &r.path == path);
            let (Some(r), None) = (hits.next(), hits.next()) else {
                return None;
            };
            *md5 = Some(r.md5.clone());
        }
        Some(self)
    }

    /// 期待した一式のファイルごとの MD5（公開の記録に書く）。MD5 が分からないファイルが 1 つでもあれば `None`。
    pub fn recorded_files(&self) -> Option<Vec<RecordedFile>> {
        self.files
            .iter()
            .map(|(path, _, md5)| {
                md5.as_ref().map(|m| RecordedFile {
                    path: path.clone(),
                    md5: m.clone(),
                })
            })
            .collect()
    }

    /// 一覧（`lsjson -R --hash`）が、期待した中身と完全に一致するか。ファイルは（パス・サイズ・MD5）の並べ替えたリスト
    /// （期待した MD5 があるファイルは、一覧の MD5 も同じでなければならない。一覧に MD5 が無ければ一致しない）、
    /// フォルダは期待したファイルの親だけ。ショートカットが 1 つでもあれば一致しない。
    pub fn matches(&self, entries: &[Entry]) -> bool {
        if entries.iter().any(Entry::is_shortcut) {
            return false;
        }
        let mut files: Vec<(String, u64, Option<String>)> = entries
            .iter()
            .filter(|e| !e.is_dir)
            .filter_map(|e| {
                u64::try_from(e.size)
                    .ok()
                    .map(|s| (e.path.clone(), s, e.md5.clone()))
            })
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
        dirs == self.dirs
            && files.len() == self.files.len()
            && files.iter().zip(&self.files).all(|(got, want)| {
                got.0 == want.0
                    && got.1 == want.1
                    && want.2.as_ref().is_none_or(|m| got.2.as_ref() == Some(m))
            })
    }
}

/// 再帰の一覧の結果。
#[derive(Debug, Clone)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// ショートカットが 1 つでもある。
    pub has_shortcut: bool,
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
    /// 公開の記録の ID と manifest の `generator`・`destination`・`folder` が合い、中身が manifest と完全に一致し、
    /// ファイルごとの MD5 が公開の記録の MD5 と一致する。
    Managed,
    /// 公開の記録に ID が無い・記録が読めない・記録に MD5 が無い（MD5 を記録する前の古い形の行）、または manifest が無い・
    /// `generator` や `destination`・`folder` が違う。
    NotManaged,
    /// このサービスが書いたが、manifest に無いファイルやフォルダが足されている・サイズが違う・足りない・MD5 が公開の記録と違う
    /// （同じ名前・同じ大きさのまま中身を書き換えられた）。
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

/// 照合の結果（移動のあと、など）。
pub type Check = Result<(), Observed>;

/// 宛先が `folder_id` のフォルダ 1 つだけで、`incoming/<job_id>` が残っていない。
fn placed_only(o: &Observed, folder_id: &str) -> bool {
    o.dest == [Seen::dir(folder_id)] && o.incoming.is_empty()
}

/// 移動・ゴミ箱へ移すの 1 回分の結果（[`Drive::with_resends`]）。
enum Resend<T> {
    Done(T),
    /// 処理されていなかった。待ってから、照合をやり直して再送する。
    Again {
        throttled: bool,
        retry_after: Option<Duration>,
    },
}

/// 変更系の呼び出しの結果を、成功なら `done`、処理されていない応答なら再送、それ以外の失敗なら `ExportError` にする。
fn resend_or<T>(r: Result<(), RemoteError>, done: T) -> Result<Resend<T>, ExportError> {
    match r {
        Ok(()) => Ok(Resend::Done(done)),
        Err(RemoteError::NotProcessed {
            throttled,
            retry_after,
        }) => Ok(Resend::Again {
            throttled,
            retry_after,
        }),
        Err(e) => Err(e.into_op_error()),
    }
}

pub struct Drive<'a> {
    remote: &'a Remote,
    layout: &'a Layout,
}

/// 記録した ID のフォルダが（記録の上で）あるはずの場所。そのフォルダを Drive が 404 と答えたとき、親の一覧で「その場所に無い」ことを
/// 確かめるために使う（[`Drive::folder_matches_expected`] など）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Under {
    /// `incoming/` の直下（送信・検証・移動の前）。
    Incoming,
    /// 宛先の親（種類のフォルダ）の直下（公開のあと）。
    Dest,
    /// `replaced/` の直下（旧フォルダの退避のあと）。
    Replaced,
}

/// フォルダ 1 つの Drive 上の場所（親の ID と名前から、一覧で照合して得た ID）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    id: String,
}

impl<'a> Drive<'a> {
    pub fn new(remote: &'a Remote, layout: &'a Layout) -> Self {
        Self { remote, layout }
    }

    pub fn layout(&self) -> &Layout {
        self.layout
    }

    // ---- 一覧 ----

    /// 親の直下（ゴミ箱の中を除く。ショートカットも 1 件として出る）。失敗は `drive_unavailable`（認可の失効は
    /// `drive_auth_invalid`、ジョブ全体の期限は `timeout`）。名前では絞らない（全ページを取り、手元で名前を照合する）。
    fn children(&self, parent: &str) -> Result<Vec<Entry>, ExportError> {
        self.remote
            .list_children(parent)
            .map_err(RemoteError::into_query_error)
    }

    /// 親の直下で、名前に完全に一致する項目（ショートカットを含む）。
    fn named(&self, parent: &str, name: &str) -> Result<Vec<Entry>, ExportError> {
        let mut v = self.children(parent)?;
        v.retain(|e| e.name == name);
        Ok(v)
    }

    /// 再帰の一覧（`root` の下の全部）。
    fn tree(&self, root: &str) -> Result<Listing, ExportError> {
        let entries = self
            .remote
            .list_tree(root)
            .map_err(RemoteError::into_query_error)?;
        let has_shortcut = entries.iter().any(Entry::is_shortcut);
        Ok(Listing {
            entries,
            has_shortcut,
        })
    }

    /// 記録した ID のフォルダの下の全部。`under` は、そのフォルダが（記録の上で）あるはずの場所（親）。
    ///
    /// Drive の 404 は、「無い」とも「見えない」とも言い切れない（木をたどる途中の、どのページ・どの子フォルダの 404 も同じ）。
    /// そこで 404 のときは、**親の一覧が成功し、その ID がその場所に無いことを確かめられたときだけ**「無い」（`None`）にする。
    /// 親がまだその ID を持っているとき・親を一覧できないときは、照会の失敗（`drive_unavailable`）。`None` の呼び出し側は、
    /// 一式と一致しない〔中身が違う〕ものとして扱う。
    fn tree_in(&self, folder_id: &str, under: Under) -> Result<Option<Listing>, ExportError> {
        match self.remote.list_tree(folder_id) {
            Ok(entries) => {
                let has_shortcut = entries.iter().any(Entry::is_shortcut);
                Ok(Some(Listing {
                    entries,
                    has_shortcut,
                }))
            }
            Err(RemoteError::NotFound) => {
                if self.parent_lists(folder_id, under)? {
                    Err(unavailable("フォルダの中身を一覧できません（404）"))
                } else {
                    Ok(None)
                }
            }
            Err(e) => Err(e.into_query_error()),
        }
    }

    /// `under` の親の直下を一覧して、`id` の項目が見えるか。親（`incoming/`・`replaced/`）が無ければ見えない。
    /// 親の一覧の失敗は、そのまま照会の失敗。
    fn parent_lists(&self, id: &str, under: Under) -> Result<bool, ExportError> {
        let parent = match under {
            Under::Dest => Some(self.layout.prefix_id.clone()),
            Under::Incoming => self.work_dir(INCOMING)?.map(|d| d.id),
            Under::Replaced => self.work_dir(REPLACED)?.map(|d| d.id),
        };
        let Some(parent) = parent else {
            return Ok(false);
        };
        Ok(self.children(&parent)?.iter().any(|e| e.id == id))
    }

    /// 作業用のフォルダの直下の `incoming`・`replaced`（フォルダ）の ID。親（作業用のフォルダ）の一覧が成功したうえで、
    /// そのフォルダが無ければ `None`、ちょうど 1 つのフォルダならその ID。それ以外（同名が複数・ファイル・ショートカット）は失敗。
    ///
    /// ショートカットは省かずに数える（同名のショートカットを「無い」と読んで、同名のフォルダを作らないため）。
    fn work_dir(&self, which: &str) -> Result<Option<Found>, ExportError> {
        let found = self.named(&self.layout.work_id, which)?;
        match found.as_slice() {
            [] => Ok(None),
            [e] if e.is_dir && !e.is_shortcut() => Ok(Some(Found { id: e.id.clone() })),
            _ => Err(unavailable("作業用のフォルダの構成が想定外です")),
        }
    }

    /// `incoming`・`replaced` の中。無ければ空。
    fn work_children(&self, which: &str) -> Result<Vec<Entry>, ExportError> {
        match self.work_dir(which)? {
            None => Ok(Vec::new()),
            Some(dir) => self.children(&dir.id),
        }
    }

    /// `incoming`・`replaced` を、無ければ作業用のフォルダの直下に作る（今の `mkdir` が親ごと作っていたのと同じ結果）。
    fn ensure_work_dir(&self, which: &str) -> Result<String, ExportError> {
        if let Some(dir) = self.work_dir(which)? {
            return Ok(dir.id);
        }
        self.remote
            .create_folder(&self.layout.work_id, which)
            .map_err(RemoteError::into_op_error)
    }

    fn incoming_entries(&self) -> Result<Vec<Entry>, ExportError> {
        let mut v = self.work_children(INCOMING)?;
        v.retain(|e| e.name == self.layout.job.as_str());
        Ok(v)
    }

    fn replaced_entries(&self) -> Result<Vec<Entry>, ExportError> {
        let mut v = self.work_children(REPLACED)?;
        v.retain(|e| e.name == self.layout.retired_name);
        Ok(v)
    }

    // ---- 宛先の状態 ----

    /// 宛先の親の直下で、フォルダ名に完全に一致する項目（ショートカットを含む）。
    pub fn dest_matches(&self) -> Result<Vec<Seen>, ExportError> {
        Ok(self
            .named(&self.layout.prefix_id, self.layout.folder.as_str())?
            .iter()
            .map(Seen::of)
            .collect())
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
    /// 呼ぶ Drive の操作は一覧と読み出しだけ（`dry_run` もこれを使う）。
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
        let listing = self.tree(&id)?;
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

    /// 直下の `manifest.json`: 一覧から、ちょうど 1 つのファイルで 4MB 以下のとき、その ID と大きさ。
    fn top_manifest(listing: &Listing) -> Option<(String, u64)> {
        let top: Vec<&Entry> = listing
            .entries
            .iter()
            .filter(|e| e.path == MANIFEST_NAME)
            .collect();
        match top.as_slice() {
            [m] if !m.is_dir
                && !m.is_shortcut()
                && (0..=MANIFEST_MAX_BYTES as i64).contains(&m.size) =>
            {
                Some((m.id.clone(), m.size as u64))
            }
            _ => None,
        }
    }

    /// manifest を読む。`Ok(None)` は、読めたが一覧の大きさと違う（上限を超えた場合を含む。読んだものを信用しない）。
    fn read_manifest(&self, id: &str, size: u64) -> Result<Option<Vec<u8>>, ExportError> {
        match self.remote.read(id, MANIFEST_MAX_BYTES) {
            Ok(bytes) if bytes.len() as u64 == size => Ok(Some(bytes)),
            Ok(_) | Err(RemoteError::TooLarge) => Ok(None),
            Err(e) => Err(e.into_query_error()),
        }
    }

    /// 管理しているフォルダの見分け方（design.md 4.2）。
    fn check_managed(
        &self,
        id: &str,
        listing: &Listing,
        published: &PublishedStore,
    ) -> Result<Managed, ExportError> {
        // 1. 公開の記録の同じ種類・同じフォルダ名の行の ID と一致する（記録が読めない・行が無ければ管理していない）。
        //    行に MD5 が無ければ（MD5 を記録する前の古い形）、中身が変わっていないと確かめられないので、管理していないものとして断る
        let recorded_files = match published.find(self.layout.destination, &self.layout.folder) {
            Some(rec) if rec.folder_id == id => match rec.files {
                Some(files) => files,
                None => return Ok(Managed::NotManaged),
            },
            _ => return Ok(Managed::NotManaged),
        };
        // 2. 直下の manifest.json: 一覧から、ちょうど 1 つのファイルで 4MB 以下
        let Some((manifest_id, size)) = Self::top_manifest(listing) else {
            return Ok(Managed::NotManaged);
        };
        // 3. 読んで、generator・destination・folder を見る（上限を掛けて読む。読み出しの失敗は drive_unavailable）。
        //    一覧の大きさと違えば、読んだものを信用しない
        let Some(bytes) = self.read_manifest(&manifest_id, size)? else {
            return Ok(Managed::Modified);
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(Managed::NotManaged);
        };
        if !self.is_ours(&manifest) {
            return Ok(Managed::NotManaged);
        }
        // 4. 中身が manifest（と manifest.json 自身）に完全に一致し、ファイルごとの MD5 が公開の記録と一致する。足されたもの・
        //    サイズの違うもの・MD5 の違うもの（同じ名前・同じ大きさのまま中身を書き換えられたもの）があれば modified。
        //    公開の記録のファイルの集合が manifest と食い違うときも、中身が変わっているものとして modified
        let Some(expected) = Expected::from_manifest(&manifest, size) else {
            return Ok(Managed::NotManaged);
        };
        let Some(expected) = expected.with_md5(&recorded_files) else {
            return Ok(Managed::Modified);
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

    /// 宛先の親フォルダと作業用のフォルダが、フォルダで（ショートカットでなく）、ゴミ箱の中でないことを `get` で確かめる
    /// （ジョブの始め。起動後に誰かが動かした場合に備える）。失敗は `drive_unavailable`。
    pub fn check_roots(&self) -> Result<(), ExportError> {
        for id in [&self.layout.prefix_id, &self.layout.work_id] {
            let meta = self.remote.get(id).map_err(RemoteError::into_query_error)?;
            if !meta.entry.is_dir || meta.entry.is_shortcut() || meta.trashed {
                return Err(unavailable(
                    "宛先の親フォルダか作業用のフォルダが、フォルダでないか、ゴミ箱の中にあります",
                ));
            }
        }
        Ok(())
    }

    // ---- 公開の操作 ----

    /// `incoming/<job_id>` を作り、その ID を返す（`incoming/` が無ければ、それも作業用のフォルダの直下に作る）。`create_folder` が
    /// 返した ID を、**直後の一覧**（`incoming/` の直下で、名前が `<job_id>` のもの）と照合し、ちょうど 1 つのフォルダで ID が同じ
    /// ときだけ返す。食い違えば（作ったフォルダが動かされて同名の別のフォルダだけが残っているなど）何も記録せずに失敗する。
    pub fn mkdir_incoming(&self) -> Result<String, ExportError> {
        let root = self.ensure_work_dir(INCOMING)?;
        let created = self
            .remote
            .create_folder(&root, self.layout.job.as_str())
            .map_err(RemoteError::into_op_error)?;
        let mut listed = self.children(&root)?;
        listed.retain(|e| e.name == self.layout.job.as_str());
        match listed.as_slice() {
            [e] if e.is_dir && !e.is_shortcut() && e.id == created => Ok(created),
            _ => Err(unavailable("incoming のフォルダを確認できません")),
        }
    }

    /// 写真を並列（[`UPLOAD_PARALLEL`]）で送り、**全部が成功してから最後に** manifest を送る。送る先は、記録した `incoming/<job_id>` の
    /// ID（`incoming_id`。名前で引き直さない）。`files` は一式（相対パス・大きさ・MD5。
    /// `manifest.json` を含む）、`workspace` は作業領域（絶対パス）。サブフォルダ（日付のフォルダ）は先に作り、葉の名前で送る。
    ///
    /// 1 つでも失敗したら、残りは始めずに、走っているものを待ってから失敗を返す。**全スレッドを join してから戻る**（キャンセル
    /// されたときも同じ。呼び出し側はそのあとで実行枠を手放す）。キャンセルの印が立っていれば、ほかの失敗より
    /// [`RemoteError::Cancelled`] を優先して返す（結果不明として復旧の表へ渡す）。期限は、写真の送信全体で 1 つ
    /// （[`Remote::upload_deadline`]）。
    pub fn upload(
        &self,
        incoming_id: &str,
        workspace: &Path,
        files: &[PublishFile],
    ) -> Result<(), ExportError> {
        let root = incoming_id.to_owned();
        let (photos, manifest): (Vec<&PublishFile>, Vec<&PublishFile>) =
            files.iter().partition(|f| f.path != MANIFEST_NAME);
        let [manifest] = manifest.as_slice() else {
            return Err(ExportError::new(
                ErrorCode::Internal,
                "送る一式に manifest.json がちょうど 1 つではありません",
            ));
        };
        // サブフォルダ（浅い順）。同じフォルダを 2 度は作らない
        let mut dirs: BTreeMap<String, String> = BTreeMap::new();
        dirs.insert(String::new(), root);
        for f in &photos {
            let mut prefix = String::new();
            let parts: Vec<&str> = f.path.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                let parent = dirs[&prefix].clone();
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if !dirs.contains_key(&prefix) {
                    let id = self
                        .remote
                        .create_folder(&parent, part)
                        .map_err(RemoteError::into_op_error)?;
                    dirs.insert(prefix.clone(), id);
                }
            }
        }
        let deadline = self.remote.upload_deadline();
        let send = |f: &PublishFile| -> Result<(), RemoteError> {
            let (dir, name) = f.path.rsplit_once('/').unwrap_or(("", f.path.as_str()));
            let parent = &dirs[dir];
            self.remote
                .upload(
                    parent,
                    name,
                    &workspace.join(&f.path),
                    &f.md5,
                    f.size,
                    deadline,
                )
                .map(|_| ())
        };
        // 写真（並列）
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let first: Mutex<Option<RemoteError>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..UPLOAD_PARALLEL.min(photos.len()) {
                scope.spawn(|| {
                    while !failed.load(Ordering::SeqCst) && !self.remote.is_cancelled() {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(f) = photos.get(i) else { break };
                        if let Err(e) = send(f) {
                            failed.store(true, Ordering::SeqCst);
                            let mut slot = first.lock().unwrap_or_else(|p| p.into_inner());
                            if slot.is_none() {
                                *slot = Some(e);
                            }
                        }
                    }
                });
            }
        });
        // 全スレッドが終わった（scope が join 済み）
        if self.remote.is_cancelled() {
            return Err(RemoteError::Cancelled.into_op_error());
        }
        if let Some(e) = first.into_inner().unwrap_or_else(|p| p.into_inner()) {
            return Err(e.into_op_error());
        }
        // 最後に manifest
        send(manifest).map_err(RemoteError::into_op_error)
    }

    /// ID のフォルダ（`incoming/<job_id>`・宛先）の中身が、期待した一式と完全に一致するか。`under` は、そのフォルダがあるはずの場所
    /// （Drive が 404 と答えたとき、親の一覧で「無い」ことを確かめるため。確かめられなければ照会の失敗）。
    pub fn folder_matches_expected(
        &self,
        folder_id: &str,
        under: Under,
        expected: &Expected,
    ) -> Result<bool, ExportError> {
        let Some(listing) = self.tree_in(folder_id, under)? else {
            return Ok(false);
        };
        Ok(!listing.has_shortcut && expected.matches(&listing.entries))
    }

    /// ID のフォルダの中身が、そのフォルダの manifest が言う一式と一致するか（復旧用。手元に一式が無いとき）。manifest は、
    /// このサービスが書いたもの（`generator`・`destination`・`folder`）でなければならない。`md5` があれば（公開の記録の
    /// ファイルごとの MD5）、一覧の MD5 もそれと一致しなければならない（ファイルの集合が manifest と違えば不一致）。
    pub fn folder_matches_its_manifest(
        &self,
        folder_id: &str,
        under: Under,
        md5: Option<&[RecordedFile]>,
    ) -> Result<bool, ExportError> {
        let Some(listing) = self.tree_in(folder_id, under)? else {
            return Ok(false);
        };
        let Some((manifest_id, size)) = Self::top_manifest(&listing) else {
            return Ok(false);
        };
        let Some(bytes) = self.read_manifest(&manifest_id, size)? else {
            return Ok(false);
        };
        let Some(manifest) = serde_json::from_slice::<Value>(&bytes).ok() else {
            return Ok(false);
        };
        let Some(mut expected) = self
            .is_ours(&manifest)
            .then(|| Expected::from_manifest(&manifest, size))
            .flatten()
        else {
            return Ok(false);
        };
        if let Some(recorded) = md5 {
            let Some(with) = expected.with_md5(recorded) else {
                return Ok(false);
            };
            expected = with;
        }
        Ok(!listing.has_shortcut && expected.matches(&listing.entries))
    }

    /// `incoming/<job_id>` をゴミ箱へ移す。先に一覧で、見えた項目が**記録した ID** のフォルダちょうど 1 つであることを確かめる
    /// （違えば何も消さずに [`Removed::Mismatch`]。無ければ [`Removed::Nothing`]）。ID は必ず渡す（名前だけでは消せない。ID を
    /// 記録できていなければ、呼び出し側は消さずに残りものとして知らせる）。移すのは、照合した ID。
    ///
    /// 流量制限・401 で処理されなかったときは、待ってから**照合をやり直して**再送する（[`Self::with_resends`]。ゴミ箱の中に
    /// なっていれば、一覧に出ないので `Nothing`）。
    pub fn remove_incoming(&self, expect_id: &str) -> Result<Removed, ExportError> {
        self.with_resends(|resent| {
            // 再送の前に、先に `get` で、もうゴミ箱の中でないか（処理されていなかったか）を見る
            if resent && self.is_trashed(expect_id)? {
                return Ok(Resend::Done(Removed::Done));
            }
            let entries = self.incoming_entries()?;
            let seen: Vec<Seen> = entries.iter().map(Seen::of).collect();
            match seen.as_slice() {
                [] => Ok(Resend::Done(Removed::Nothing)),
                [s] if s.kind == Kind::Dir && s.id == expect_id => {
                    resend_or(self.remote.trash(&s.id), Removed::Done)
                }
                _ => Ok(Resend::Done(Removed::Mismatch(seen))),
            }
        })
    }

    /// 旧フォルダ（宛先のフォルダ。ID は `old_id`。公開の記録・ジョブの記録と一致したもの）を `replaced/<種類>-<フォルダ名>-<job_id>` へ
    /// 移す（ID で移す。`replaced/` が無ければ作る。移したあとの照合は呼び出し側）。移す直前に宛先を一覧して、見えたフォルダが
    /// `old_id` でなければ（動かされた・同名の別のフォルダに替わった）、何も変えずに失敗する。
    ///
    /// 流量制限・401 で処理されなかったときは、待ってから、まず `get` で目的の状態（`replaced/<…>` に移っている）かを見て、
    /// そうでなければ**上の確認を最初からやり直して**再送する（[`Self::with_resends`]）。
    pub fn retire_old(&self, old_id: &str) -> Result<(), ExportError> {
        let check = || match self.target()? {
            Target::Exists { id } if id == old_id => Ok(()),
            _ => Err(unavailable("宛先のフォルダが記録した旧フォルダと違います")),
        };
        self.with_resends(|resent| {
            if resent
                && let Some(dir) = self.work_dir(REPLACED)?
                && self.moved_to(old_id, &dir.id, &self.layout.retired_name)?
            {
                return Ok(Resend::Done(()));
            }
            // 何も作らずに確かめ、`replaced/` を用意したあと、移す直前にもう一度確かめる
            check()?;
            let replaced = self.ensure_work_dir(REPLACED)?;
            check()?;
            resend_or(
                self.remote.move_to(
                    old_id,
                    &self.layout.prefix_id,
                    &replaced,
                    &self.layout.retired_name,
                ),
                (),
            )
        })
    }

    /// `incoming/<job_id>`（ID は `incoming_id`。記録したもの）を宛先へ移す（ID で移す。親を宛先の親へ、名前をフォルダ名にして、
    /// 1 回の更新で変える。移したあとの照合は呼び出し側）。移す直前に `incoming/` の直下を一覧して、`<job_id>` の項目が
    /// `incoming_id` のフォルダ 1 つだけでなければ、何も変えずに失敗する。移す直前に宛先に同名が無いことを確かめるのは、呼び出し側
    /// （[`crate::publish`]）。API の移動はフォルダの移動のまま（rclone のように、同名のフォルダがあるとファイル単位の移動に化ける
    /// ことはない）。
    ///
    /// 流量制限・401 で処理されなかったときは、待ってから、まず `get` で目的の状態（宛先に移っている）かを見て、そうでなければ
    /// 上の確認を最初からやり直して再送する（[`Self::with_resends`]）。
    pub fn publish_move(&self, incoming_id: &str) -> Result<(), ExportError> {
        self.with_resends(|resent| {
            if resent
                && self.moved_to(
                    incoming_id,
                    &self.layout.prefix_id,
                    self.layout.folder.as_str(),
                )?
            {
                return Ok(Resend::Done(()));
            }
            let Some(root) = self.work_dir(INCOMING)? else {
                return Err(unavailable("incoming のフォルダを確認できません"));
            };
            let mut listed = self.children(&root.id)?;
            listed.retain(|e| e.name == self.layout.job.as_str());
            match listed.as_slice() {
                [e] if e.is_dir && !e.is_shortcut() && e.id == incoming_id => {}
                _ => {
                    return Err(unavailable("incoming のフォルダが記録した ID と違います"));
                }
            }
            resend_or(
                self.remote.move_to(
                    incoming_id,
                    &root.id,
                    &self.layout.prefix_id,
                    self.layout.folder.as_str(),
                ),
                (),
            )
        })
    }

    /// `id` が Drive のゴミ箱の中か（`get` で見る。ゴミ箱へ移すが処理されていたかの確認）。
    fn is_trashed(&self, id: &str) -> Result<bool, ExportError> {
        Ok(self
            .remote
            .get(id)
            .map_err(RemoteError::into_query_error)?
            .trashed)
    }

    /// `id` が、親 `parent` の下で名前 `name` になっていて、ゴミ箱の中でないか（`get` で見る。移動が処理されていたかの確認）。
    fn moved_to(&self, id: &str, parent: &str, name: &str) -> Result<bool, ExportError> {
        let meta = self.remote.get(id).map_err(RemoteError::into_query_error)?;
        Ok(meta.parents == [parent] && meta.entry.name == name && !meta.trashed)
    }

    /// 移動・ゴミ箱へ移すの再送。これらは API の層では、処理されていないと分かる応答（429・流量制限の 403・401）のあとも再送しない
    /// （[`RemoteError::NotProcessed`]。待つ間に旧フォルダへファイルが足されたり、フォルダが動かされたりしていても、同じ ID を
    /// もう一度更新すると、足された分ごと動かしてしまう）。そこで、ここで**待ってから、`step` の中で操作ごとの照合をやり直して**
    /// 再送する。`step` の引数は、再送かどうか（真なら、先に「もう目的の状態か」を `get` などで見る）。
    ///
    /// 再送には回数の上限（[`Remote::resend_limit`]）と、全体の期限（[`Remote::resend_deadline`]。ジョブ全体の期限を含む）を掛ける。
    /// 上限を使い切ったら `rclone_failed`、期限・キャンセルはその待ちのエラー。
    fn with_resends<T>(
        &self,
        mut step: impl FnMut(bool) -> Result<Resend<T>, ExportError>,
    ) -> Result<T, ExportError> {
        let deadline = self.remote.resend_deadline();
        let mut resends: u32 = 0;
        loop {
            match step(resends > 0)? {
                Resend::Done(v) => return Ok(v),
                Resend::Again {
                    throttled,
                    retry_after,
                } => {
                    if resends >= self.remote.resend_limit() {
                        return Err(RemoteError::Failed.into_op_error());
                    }
                    let wait = if throttled {
                        self.remote.resend_wait(resends, retry_after)
                    } else {
                        Duration::ZERO
                    };
                    self.remote
                        .pause(wait, deadline)
                        .map_err(RemoteError::into_op_error)?;
                    resends += 1;
                }
            }
        }
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
    /// 2. そのフォルダの中身（一覧の相対パス・サイズ・MD5）が、中の旧 manifest の一式と完全に一致し、MD5 が**公開の記録の MD5**
    ///    （旧フォルダを公開したときに記録したもの。新しい行を書いたあとの復旧では、新しい行の `previous`）と一致する
    ///    （停止している間に足されたファイルや、同じ名前・同じ大きさのまま書き換えられたファイルごとゴミ箱へ移さない。旧 manifest は
    ///    読み直す）。公開の記録に旧フォルダの行と MD5 が無いとき（行の ID が旧 ID と違う・MD5 が無い）は、確かめられないので
    ///    移さない（[`Removed::ContentMismatch`]）。
    /// 3. 2 の間に差し替わっていないことを、1 をもう一度確かめて見る。
    ///
    /// 移すのは、照合した **ID**（`old_id`）。
    pub fn trash_replaced(
        &self,
        old_id: &str,
        published: &PublishedStore,
    ) -> Result<Removed, ExportError> {
        let is_old = |seen: &[Seen]| matches!(seen, [s] if s.kind == Kind::Dir && s.id == old_id);
        // 流量制限・401 で処理されなかったときは、待ってから、**1 から 3 までを最初からやり直して**再送する
        // （ゴミ箱の中になっていれば、`replaced/` の一覧に出ないので `Nothing`）
        self.with_resends(|resent| {
            // 再送の前に、先に `get` で、もうゴミ箱の中でないか（処理されていなかったか）を見る
            if resent && self.is_trashed(old_id)? {
                return Ok(Resend::Done(Removed::Done));
            }
            let seen = self.replaced_seen()?;
            if seen.is_empty() {
                return Ok(Resend::Done(Removed::Nothing));
            }
            if !is_old(&seen) {
                return Ok(Resend::Done(Removed::Mismatch(seen)));
            }
            let Some(recorded) =
                published.recorded_files_of(self.layout.destination, &self.layout.folder, old_id)
            else {
                return Ok(Resend::Done(Removed::ContentMismatch(seen)));
            };
            if !self.folder_matches_its_manifest(old_id, Under::Replaced, Some(&recorded.files))? {
                return Ok(Resend::Done(Removed::ContentMismatch(seen)));
            }
            let again = self.replaced_seen()?;
            if !is_old(&again) {
                return Ok(Resend::Done(Removed::Mismatch(again)));
            }
            resend_or(self.remote.trash(old_id), Removed::Done)
        })
    }

    // ---- 移動のあとの照合（応答を信じず、一覧で確かめる） ----

    /// 退避のあと: 宛先が無く、`replaced/<…>` が記録した旧フォルダの ID のフォルダ 1 つだけ。
    pub fn check_retired(&self, old_id: &str) -> Result<Check, ExportError> {
        let o = self.observe()?;
        let ok = o.dest.is_empty() && o.replaced == [Seen::dir(old_id)];
        Ok(if ok { Ok(()) } else { Err(o) })
    }

    /// 公開のあと: 宛先が記録した `incoming` の ID のフォルダ 1 つだけ、`incoming/<job_id>` が残っていない、
    /// 中身が一式と一致する。`expected` が無ければ（起動時の復旧）、宛先の manifest が言う一式と照合し、`recorded`（検証済みの
    /// ファイルごとの MD5。ジョブの記録・公開の記録から）があれば、一覧の MD5 もそれと一致しなければならない。
    pub fn check_published(
        &self,
        incoming_id: &str,
        expected: Option<&Expected>,
        recorded: Option<&[RecordedFile]>,
    ) -> Result<Check, ExportError> {
        let o = self.observe()?;
        if !placed_only(&o, incoming_id) {
            return Ok(Err(o));
        }
        // 宛先は、上で ID を確かめた `incoming_id` のフォルダ 1 つだけ。その ID で中身を照合する
        let content_ok = match expected {
            Some(e) => self.folder_matches_expected(incoming_id, Under::Dest, e)?,
            None => self.folder_matches_its_manifest(incoming_id, Under::Dest, recorded)?,
        };
        if !content_ok {
            return Ok(Err(o));
        }
        // 中身は ID で読めるので、照合している間にそのフォルダが宛先から動かされても、読めてしまう。公開の確定（記録）と旧フォルダの
        // 片付けに入る前に、宛先がその ID のフォルダ 1 つだけで `incoming` が空であることを、もう一度確かめる
        self.still_placed(incoming_id)
    }

    /// 宛先が `folder_id` のフォルダ 1 つだけで、`incoming/<job_id>` が残っていないことを、今あらためて確かめる（中身の照合のあと、
    /// 公開の確定と旧フォルダの片付けに入る前に呼ぶ）。食い違えば、今見えた状態を返す（照合の失敗）。
    pub fn still_placed(&self, folder_id: &str) -> Result<Check, ExportError> {
        let o = self.observe()?;
        Ok(if placed_only(&o, folder_id) {
            Ok(())
        } else {
            Err(o)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::api::FOLDER_MIME;
    use crate::remote::SHORTCUT_MIME;

    /// 試験用の MD5（パスから決まる架空の値。同じパスなら同じ）。
    fn fake_md5(path: &str) -> String {
        crate::image::md5_hex(path.as_bytes())
    }

    fn file(path: &str, size: i64) -> Entry {
        Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            size,
            mime: "image/jpeg".into(),
            is_dir: false,
            id: format!("f{}", path.replace(['/', '.'], "_")),
            shortcut_id: None,
            md5: Some(fake_md5(path)),
        }
    }
    fn dir(path: &str) -> Entry {
        Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            size: -1,
            mime: FOLDER_MIME.into(),
            is_dir: true,
            id: format!("d{}", path.replace('/', "_")),
            shortcut_id: None,
            md5: None,
        }
    }

    fn pf(path: &str, size: u64) -> PublishFile {
        PublishFile {
            path: path.into(),
            size,
            md5: fake_md5(path),
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
        sc.push(Entry {
            mime: SHORTCUT_MIME.into(),
            ..file("link", 0)
        });
        assert!(!e.matches(&sc));
    }

    #[test]
    fn md5_must_match_when_the_expected_set_knows_it() {
        let e = Expected::from_publish_files(&[
            pf("a.jpg", 5),
            pf("2030-03-11/b.jpg", 7),
            pf("manifest.json", 9),
        ]);
        let good = vec![
            file("a.jpg", 5),
            dir("2030-03-11"),
            file("2030-03-11/b.jpg", 7),
            file("manifest.json", 9),
        ];
        assert!(e.matches(&good));
        // 同じ名前・同じ大きさで MD5 が違う（写真・下のフォルダの写真・manifest）
        for i in [0, 2, 3] {
            let mut changed = good.clone();
            changed[i].md5 = Some("f".repeat(32));
            assert!(!e.matches(&changed), "{i}");
        }
        // 一覧に MD5 が無い（--hash なし・MD5 を持たないファイル）
        for i in [0, 2, 3] {
            let mut none = good.clone();
            none[i].md5 = None;
            assert!(!e.matches(&none), "{i}");
        }
    }

    #[test]
    fn a_manifest_set_has_no_md5_until_the_record_adds_it() {
        let m = serde_json::json!({
            "photos": [{"filename": "a.jpg", "file_size": 5}]
        });
        let bare = Expected::from_manifest(&m, 9).unwrap();
        assert_eq!(bare.recorded_files(), None);
        // MD5 なし: 一覧の MD5 は比べない（起動時の復旧の照合）
        let mut listing = vec![file("a.jpg", 5), file("manifest.json", 9)];
        listing[0].md5 = Some("9".repeat(32));
        assert!(bare.matches(&listing));
        // 記録の MD5 を足すと、比べる
        let rec = |a: &str, m: &str| {
            vec![
                RecordedFile {
                    path: "a.jpg".into(),
                    md5: a.into(),
                },
                RecordedFile {
                    path: "manifest.json".into(),
                    md5: m.into(),
                },
            ]
        };
        let with = bare
            .clone()
            .with_md5(&rec(&"9".repeat(32), &fake_md5("manifest.json")))
            .unwrap();
        assert!(with.matches(&listing));
        assert_eq!(
            with.recorded_files().unwrap(),
            rec(&"9".repeat(32), &fake_md5("manifest.json"))
        );
        listing[0].md5 = Some(fake_md5("a.jpg"));
        assert!(!with.matches(&listing));
        // 記録のファイルの集合が manifest と違えば足せない（足りない・余分・名前違い・同じパスが 2 つ）
        let all = rec("1", "2");
        assert!(bare.clone().with_md5(&all[..1]).is_none());
        let mut extra = all.clone();
        extra.push(RecordedFile {
            path: "b.jpg".into(),
            md5: "3".into(),
        });
        assert!(bare.clone().with_md5(&extra).is_none());
        let mut renamed = all.clone();
        renamed[0].path = "c.jpg".into();
        assert!(bare.clone().with_md5(&renamed).is_none());
        let mut twice = all.clone();
        twice[1].path = "a.jpg".into();
        assert!(bare.with_md5(&twice).is_none());
    }

    #[test]
    fn unknown_size_files_are_not_part_of_any_set() {
        // ネイティブの Google 文書・エクスポートできないファイル（Size -1）が混ざった一覧は、一式と一致しない
        let e = Expected::from_publish_files(&[pf("a.jpg", 5), pf("manifest.json", 9)]);
        let good = vec![file("a.jpg", 5), file("manifest.json", 9)];
        assert!(e.matches(&good));
        let mut with_doc = good.clone();
        with_doc.push(Entry {
            mime: "application/vnd.google-apps.document".into(),
            ..file("memo", -1)
        });
        assert!(!e.matches(&with_doc));
        // 期待したファイルそのものの Size が -1 でも一致しない（サイズが分からないものは認めない）
        let unknown = vec![file("a.jpg", -1), file("manifest.json", 9)];
        assert!(!e.matches(&unknown));
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
        // MD5 は持たない（記録の MD5 は with_md5 で足す）
        assert_eq!(
            e,
            Expected::build(vec![
                ("2030-03-10/090000_1.jpg".into(), 10, None),
                ("2030-03-11/090000_2.jpg".into(), 20, None),
                ("manifest.json".into(), 99, None),
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
}
