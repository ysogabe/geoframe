//! Drive への公開（design.md 4.2「公開の手順」「復旧」）。
//!
//! 入口は 3 つ。
//!
//! - [`publish`]: 宛先の状態を調べ（4.1 の手順 8）、`incoming` → 検証 → 公開（4.2 の手順 1〜6）まで進める。
//! - [`Recovery`]: 記録した段階と ID から、復旧の表（[`crate::recovery`]）を実行する。公開の途中の失敗では [`publish`] が
//!   その場で使い、起動時（T5）は記録から作って使う。`recover` コマンドの表示は [`Recovery::explain`]（Drive を変えない）。
//! - [`dry_run`]: 宛先の状態と、管理しているフォルダか。呼ぶ rclone のコマンドは `lsjson` と `cat` だけ。
//!
//! ## 呼び出し側（T5）との約束
//!
//! - 候補・書き出す写真が 0 枚のときは、[`publish`] を呼ばない（AC5: rclone を呼ばない）。[`publish`] は、書き出す一式が
//!   1 枚以上（`files` が写真 1 枚以上と末尾の `manifest.json`）であることを前提にし、そうでなければ何も呼ばずに `internal`。
//! - 各段階に進む**前**に、[`ProgressSink::record`] が呼ばれる。ここでジョブの記録（`jobs/<job_id>.json`）へ段階名と ID を書いて
//!   から `Ok` を返すこと。`Err` を返すと、その段階の Drive の操作はしない。`uploading` は 2 回（フォルダを作る前に段階だけ、
//!   作って ID が分かってから ID つきで）呼ばれる。**復旧（[`Recovery::run`]・[`Recovery::execute`]）にも同じ口を渡す**:
//!   復旧が Drive を変える操作（`incoming` の移動＝`publishing`、旧フォルダのゴミ箱への移動＝`trashing`）の前にも、
//!   段階を書く（復旧の途中でまた止まっても、記録と Drive が食い違わないため）。書けなければ、その操作はしない。
//! - ジョブ全体の期限は、渡す [`Rclone`] に `with_deadline` で掛ける。過ぎたら実行中の rclone を kill して `timeout`。後始末と復旧は
//!   期限なし（コマンドごとの期限だけ）で動く。
//! - [`Published::recorded`] が偽（公開の記録 `published.json` に書けなかった）のときは、公開は済んでいる。**正常な完了として
//!   吸収しない**: [`Published::record`] が書くはずだった行なので、`upsert` をやり直し、それでも書けなければ、結果に
//!   「公開済み（フォルダの ID）・記録に書けていない」を残す（次の `replace` はこのフォルダを管理していないものとして断る）。
//! - [`Published::old_left_in_replaced`] が `Some` のときは、新しい一式は公開済みで、旧フォルダが `replaced/` に残っている
//!   （旧 ID が退避先にあると再確認できた場合に限る。別の ID や照会できないときは、成功にせず `blocked`）。
//! - [`PublishFailure::leftover_incoming`]・[`Resolution::Interrupted`] の残りもの（`incoming/<job_id>`）は、ID を記録していない
//!   フォルダを名前だけでは消さないので、残ることがある。結果と `recover` の表示に出す（[`leftover_incoming_text`]）。
//!
//! ## ID の照合をしている段（本物の rclone で配置のときに確かめる項目の根拠）
//!
//! 移動は**パス**で指す（`rclone moveto`）。移したあとは終了コードを信じず、`lsjson` で ID を照合する。
//!
//! | 段 | 照合 |
//! |---|---|
//! | `uploading` の最初 | `incoming/<job_id>` を作ったあと、親の一覧で ID を取り（ちょうど 1 つのフォルダ）、記録する |
//! | `verifying` | 送った中身が一式と完全に一致 |
//! | 公開の前 | 宛先の状態をもう一度調べる（新規なら「無い」、置き換えなら「記録した旧 ID・管理している・中身が変わっていない」） |
//! | `retiring` の直後 | 宛先が無く、`replaced/<…>` が**記録した旧フォルダの ID 1 つだけ**。違えば `blocked`（`retire_mismatch`。パスで解決する移動が、確かめてから移すまでの間にできた別のフォルダを移した場合） |
//! | `publishing` の前 | 宛先が無い（新規は無ければ中止、置き換えは `blocked`） |
//! | `publishing` の直後 | 宛先が**記録した `incoming` の ID 1 つだけ**・`incoming/<job_id>` が残っていない・中身が一式と一致。違えば `blocked`（`publish_mismatch`。ファイル単位の移動への切り替え、宛先に既にあった別のフォルダへの merge など） |
//! | `trashing` の直前 | `replaced/<…>` が**記録した旧フォルダの ID 1 つだけ**、かつ**中身が旧 manifest の一式と完全に一致**。違えば `blocked`（`trash_mismatch`・`retired_content_changed`） |
//! | 取り除き（`incoming` のゴミ箱） | 見えた項目が、**記録した ID** のフォルダちょうど 1 つ（ID が無ければ消さない） |
//!
//! ## ゴミ箱へ移すものと、`moveto` の fallback の影響（残る前提）
//!
//! - rclone で ID を固定した `purge` はできない（上流の `purgeCheck` は、`root_folder_id` で根を指しても何もしない）。`purge` を
//!   呼ぶのは、誰とも共有しない作業用のフォルダの中の `incoming/<job_id>`・`replaced/<種類>-<フォルダ名>-<job_id>`
//!   （`job_id` は 128 ビットの乱数）だけ。競合の余地は、オーナーがその名前のフォルダを数秒のうちに作ることだけ。
//! - rclone の `moveto`（フォルダ）は、移し先にフォルダができていると、Drive の中での移動をやめて**ファイル単位の移動**に切り替わる
//!   （上流の `MoveDir` が `ErrorDirExists` を受けた場合）。止める手段は無く、`ErrorCantDirMove`（remote 名が違うとき）は
//!   remote 名をそろえた設定（起動時に検査済み）では起きないので、残る条件は「直前に移し先が無いと確かめてから移すまでの数秒に、
//!   同じ名前のフォルダが作られる」競合だけ（設計 4.2 の「書き出しの実行中に、オーナーが同じ宛先のフォルダを画面から作ったり
//!   動かしたりしない」前提の範囲）。起きたときの影響は、移し先のフォルダに**写真が足される**こと（既にあるものは消さず、
//!   `--immutable` で書き換えない）。移したあとの ID の照合（移し先の ID が `incoming` の ID と違う）で見つけて `blocked` で止まる。
//!   `recover` の表示（[`BlockedInfo::describe`]）の ID を見て、オーナーが Drive の画面で足された写真を手で片付ける。`retiring`
//!   （旧フォルダの退避）の移動も同じ。
//!
//! rclone の drive の `backend moveid`（ID を指して移す）は、フォルダに使えるかをネットワークなしでは確かめられなかったので
//! 使っていない（パスでの移動と、上の照合）。使えると分かれば、`retiring`・`publishing` の移動だけを差し替えられる
//! （[`crate::drive::Drive::retire_old`]・[`crate::drive::Drive::publish_move`]）。

use std::path::Path;
use std::time::Duration;

use crate::drive::{Drive, Expected, Inspection, Layout, Managed, Place, Removed, Target};
use crate::error::{ErrorCode, ExportError};
use crate::image::PublishFile;
use crate::published::{PublishedRecord, PublishedStore};
use crate::rclone::Rclone;
use crate::recovery::{
    Action, BlockedReason, Decision, Kind, Observed, Progress, Seen, Stage, decide,
};
use crate::request::OnExists;

/// 各段階の前に、ジョブの記録へ段階名と ID を書くための口（T5 が実装する）。
pub trait ProgressSink {
    /// `progress` をジョブの記録に書いて（`fsync` まで）から `Ok` を返す。`Err` なら、その段階の Drive の操作はしない。
    fn record(&mut self, progress: &Progress) -> Result<(), ExportError>;
}

impl<F: FnMut(&Progress) -> Result<(), ExportError>> ProgressSink for F {
    fn record(&mut self, progress: &Progress) -> Result<(), ExportError> {
        self(progress)
    }
}

/// 失敗のあとの「待って 2 回一覧を取る」の設定（試験では短くする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// 一覧を取る前の待ち（既定 30 秒。rclone を止めたあとも Drive 側で移動が遅れて反映されることがある）。
    pub settle: Duration,
    /// 「待つ → 2 回取る」を繰り返す回数の上限。決まらなければ `blocked`。
    pub rounds: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(30),
            rounds: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictReason {
    /// 宛先が既にあり、`on_exists` が `error`。
    Exists,
    /// `replace` だが、このサービスが書いたと確かめられない。
    NotManaged,
    /// このサービスが書いたが、中身が変わっている。
    Modified,
}

impl ConflictReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exists => "exists",
            Self::NotManaged => "not_managed",
            Self::Modified => "modified",
        }
    }
}

/// 公開できたときの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// 公開したフォルダの Drive の ID。
    pub folder_id: String,
    pub replaced: bool,
    /// 置き換えのとき、置き換える前にあったファイル数（新規は 0。復旧で完了したときも 0）。
    pub existing_files: u64,
    /// 旧フォルダのゴミ箱への移動ができなかったとき、`replaced/` に残っている旧フォルダの ID（新しい一式は公開済み）。
    /// **旧 ID が退避先にあると、一覧で再確認できたときだけ**入る（別の ID や、照会できないときは `blocked`）。
    pub old_left_in_replaced: Option<String>,
    /// 公開の記録（`published.json`）に書けたか。偽なら、次の `replace` はこのフォルダを管理していないものとして断る。
    /// 偽のまま正常な完了にしない（[`Published::record`] を `upsert` し直し、書けなければ結果に残す）。
    pub recorded: bool,
    /// 公開の記録に書く（書いた）行。`recorded` が偽のとき、記録の再試行に使う。
    pub record: PublishedRecord,
}

impl Published {
    /// 公開の記録（`published.json`）への書き込みをやり直す（`recorded` が偽のとき）。
    pub fn retry_record(&self, store: &PublishedStore) -> Result<(), ExportError> {
        store.upsert(self.record.clone())
    }

    /// `old_left_in_replaced` があるときの、結果と `recover` に出す説明。
    pub fn old_left_text(&self, layout: &Layout) -> Option<String> {
        self.old_left_in_replaced.as_ref().map(|id| {
            format!(
                "新しい一式は公開済みですが、旧フォルダ（ID {id}）をゴミ箱へ移せず、{} に残っています。\
                 Drive の画面で確かめてゴミ箱へ移してください。",
                layout.retired_path()
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    Published(Published),
    /// 書かずに終えた（Drive は変えていない）。
    Conflict {
        reason: ConflictReason,
        existing_files: u64,
    },
}

/// `blocked` にしたときの記録（T5 がジョブの記録に残し、`recover` が表示する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedInfo {
    pub reason: BlockedReason,
    /// 最後に記録した段階と ID。
    pub progress: Progress,
    /// 照合できなかったときに見えていた Drive の状態（取れなかったときは `None`）。
    pub observed: Option<Observed>,
}

impl BlockedInfo {
    /// `recover` の表示（ID は Drive の ID。設定・資格情報は含まない）。
    pub fn describe(&self, layout: &Layout) -> String {
        let mut text = format!(
            "止めた理由: {}（{}）。最後に記録した段階: {}。",
            self.reason.as_str(),
            self.reason.text(),
            self.progress.stage
        );
        if let Some(o) = &self.observed {
            let list = |v: &[Seen]| {
                if v.is_empty() {
                    "なし".to_owned()
                } else {
                    v.iter()
                        .map(|s| s.id.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                }
            };
            text.push_str(&format!(
                " 見えていた状態: 宛先（{}）の ID={}、incoming（{}）の ID={}、replaced（{}）の ID={}。",
                layout.dest_path(),
                list(&o.dest),
                layout.incoming_path(),
                list(&o.incoming),
                layout.retired_path(),
                list(&o.replaced),
            ));
        }
        match self.reason {
            BlockedReason::RetireMismatch => text.push_str(&format!(
                " 戻す手順: replaced の項目（上の ID）が、記録した旧フォルダ（ID {}）と違うフォルダなら、移してしまったものです。\
                 Drive の画面で、そのフォルダを元の場所（{}）へ戻し、本物の旧フォルダが宛先に残っていることを確かめてください。",
                self.progress.old_id.as_deref().unwrap_or("記録なし"),
                layout.dest_path(),
            )),
            BlockedReason::PublishMismatch => text.push_str(&format!(
                " 宛先（{}）の ID が incoming の ID（{}）と違うときは、宛先に既にあったフォルダへファイル単位で移された（写真が足された）\
                 可能性があります。消してはいません。Drive の画面で、足された写真を確かめて片付けてください。",
                layout.dest_path(),
                self.progress.incoming_id.as_deref().unwrap_or("記録なし"),
            )),
            BlockedReason::RetiredContentChanged => text.push_str(&format!(
                " 退避した旧フォルダ（{}）に、旧 manifest の一式にないファイルがある・足りない・大きさが違います。ゴミ箱へは移していません。\
                 Drive の画面で中身を確かめ、足されたものがあれば取り出してから、旧フォルダをゴミ箱へ移してください。",
                layout.retired_path(),
            )),
            _ => {}
        }
        text
    }
}

/// 片付けられずに作業用のフォルダに残っている `incoming/<job_id>` の知らせ（結果と `recover` の表示に出す）。
/// 宛先は変わっていない。ID が分かっていれば入れる。
pub fn leftover_incoming_text(layout: &Layout, id: Option<&str>) -> String {
    format!(
        "作業用のフォルダに incoming（{}{}）が残っています。宛先は変わっていません。\
         ID を記録していないフォルダは名前だけでは消さないので、オーナーが Drive の画面で確かめて消してください。",
        layout.incoming_path(),
        id.map(|i| format!("、ID {i}")).unwrap_or_default()
    )
}

/// 公開できなかったときの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishFailure {
    /// ジョブの `error`（`blocked` のときは `Blocked`）。
    pub error: ExportError,
    /// `blocked` のときの記録。
    pub blocked: Option<Box<BlockedInfo>>,
    /// `incoming` を片付けられず、作業用のフォルダに残っている（宛先は変わっていない。オーナーが消してよい）。
    /// ID を記録していないフォルダは、名前だけでは消さずにここへ出す。
    pub leftover_incoming: bool,
    /// 残っている `incoming/<job_id>` の ID（分かっているとき）。表示は [`leftover_incoming_text`]。
    pub leftover_incoming_id: Option<String>,
}

impl PublishFailure {
    fn plain(error: ExportError) -> Self {
        Self {
            error,
            blocked: None,
            leftover_incoming: false,
            leftover_incoming_id: None,
        }
    }

    fn blocked(info: Box<BlockedInfo>) -> Self {
        Self {
            error: ExportError::from(ErrorCode::Blocked),
            blocked: Some(info),
            leftover_incoming: false,
            leftover_incoming_id: None,
        }
    }
}

// ---- 残りものの片付け ----

/// `incoming/<job_id>` を片付けた結果。
struct Leftover {
    present: bool,
    id: Option<String>,
}

impl Leftover {
    const NONE: Self = Self {
        present: false,
        id: None,
    };
}

/// `incoming/<job_id>` を片付ける。**記録した（または自分で確かめた）ID と一致するフォルダ 1 つだけ**をゴミ箱へ移す。
/// ID が無ければ名前だけでは消さず、あるかどうかだけを見て、あれば残りものとして返す。
fn clean_incoming(drive: &Drive<'_>, id: Option<&str>) -> Leftover {
    match id {
        Some(id) => match drive.remove_incoming(id) {
            Ok(Removed::Done | Removed::Nothing) => Leftover::NONE,
            _ => Leftover {
                present: true,
                id: Some(id.to_owned()),
            },
        },
        None => match drive.incoming_seen() {
            Ok(seen) if seen.is_empty() => Leftover::NONE,
            Ok(seen) => Leftover {
                present: true,
                id: match seen.as_slice() {
                    [s] if s.kind == Kind::Dir => Some(s.id.clone()),
                    _ => None,
                },
            },
            Err(_) => Leftover {
                present: true,
                id: None,
            },
        },
    }
}

// ---- 復旧の実行 ----

/// 復旧の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// 宛先は前のまま。`incoming` を片付けた（`leftover_incoming` は、片付けられなかった・名前だけでは消さなかった）。ジョブは
    /// `interrupted`（公開の途中の失敗では、元の失敗）で終える。
    Interrupted {
        leftover_incoming: bool,
        /// 残っている `incoming/<job_id>` の ID（分かっているとき。表示は [`leftover_incoming_text`]）。
        leftover_incoming_id: Option<String>,
    },
    /// 新しい一式が公開されている。
    Published(Published),
}

/// ゴミ箱へ移す前の照合で止めた理由（何も消していない）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum TrashStop {
    /// `replaced/` の項目が、記録した旧フォルダの ID のフォルダ 1 つだけではない。
    Mismatch(Vec<Seen>),
    /// 旧フォルダだが、中身が旧 manifest の一式と違う。
    Content(Vec<Seen>),
    /// `replaced/` を一覧できない（確かめられない）。
    Unconfirmed,
}

impl TrashStop {
    fn into_blocked(self, progress: &Progress, base: Option<&Observed>) -> Box<BlockedInfo> {
        let with_replaced = |seen: Vec<Seen>| {
            let mut o = base.cloned().unwrap_or_default();
            o.replaced = seen;
            o
        };
        let (reason, observed) = match self {
            Self::Mismatch(seen) => (BlockedReason::TrashMismatch, Some(with_replaced(seen))),
            Self::Content(seen) => (
                BlockedReason::RetiredContentChanged,
                Some(with_replaced(seen)),
            ),
            Self::Unconfirmed => (BlockedReason::Unobservable, base.cloned()),
        };
        Box::new(BlockedInfo {
            reason,
            progress: progress.clone(),
            observed,
        })
    }
}

/// 復旧の表を実行するための部品。
pub struct Recovery<'a> {
    /// 期限なし（コマンドごとの期限だけ）の rclone。
    pub rclone: &'a Rclone,
    pub layout: &'a Layout,
    pub published: &'a PublishedStore,
    /// 手元にある期待した一式（公開の途中の失敗のとき）。起動時の復旧では `None`（Drive の manifest で照合する）。
    pub expected: Option<&'a Expected>,
    pub timing: Timing,
}

impl Recovery<'_> {
    fn drive(&self) -> Drive<'_> {
        Drive::new(self.rclone, self.layout)
    }

    fn blocked(
        &self,
        progress: &Progress,
        reason: BlockedReason,
        observed: Option<&Observed>,
    ) -> Box<BlockedInfo> {
        Box::new(BlockedInfo {
            reason,
            progress: progress.clone(),
            observed: observed.cloned(),
        })
    }

    /// 段階を記録する（Drive を変える操作の前）。すでにその段階なら書かない。書けなければ `Err`（その操作はしない）。
    /// 書けたら `cur`（最後に記録できた段階と ID）を進める。
    fn record_stage(
        cur: &mut Progress,
        stage: Stage,
        sink: &mut dyn ProgressSink,
    ) -> Result<(), ExportError> {
        if cur.stage == stage {
            return Ok(());
        }
        let next = Progress {
            stage,
            old_id: cur.old_id.clone(),
            incoming_id: cur.incoming_id.clone(),
        };
        sink.record(&next)?;
        *cur = next;
        Ok(())
    }

    /// 待って、一覧を 2 回取り、同じなら返す。違えば（または取れなければ）待ってやり直し、`rounds` 回で決まらなければ `None`。
    /// `wait_first` が偽なら、最初の 1 回目は待たない（Drive の移動が起きていない段階の失敗）。
    pub fn settle_observe(&self, wait_first: bool) -> Option<Observed> {
        let drive = self.drive();
        for round in 0..self.timing.rounds.max(1) {
            if wait_first || round > 0 {
                std::thread::sleep(self.timing.settle);
            }
            if let (Ok(a), Ok(b)) = (drive.observe(), drive.observe())
                && a == b
            {
                return Some(a);
            }
        }
        None
    }

    /// 復旧の表を当てはめて実行する。`wait_first`: 最初に待つか（移動・ゴミ箱への移動の段階の失敗、起動時は真）。
    /// `sink`: Drive を変える操作の前に、段階（`publishing`・`trashing`）を書く口（公開と同じもの）。
    pub fn run(
        &self,
        progress: &Progress,
        wait_first: bool,
        sink: &mut dyn ProgressSink,
    ) -> Result<Resolution, Box<BlockedInfo>> {
        let Some(observed) = self.settle_observe(wait_first) else {
            return Err(self.blocked(progress, BlockedReason::Unobservable, None));
        };
        let decision = decide(progress, &observed);
        self.execute(progress, &decision, &observed, sink)
    }

    /// `recover` コマンドの表示用: Drive を変えずに、今の状態と、表の動きと、その説明を返す。
    pub fn explain(
        &self,
        progress: &Progress,
    ) -> Result<(Decision, Observed, String), ExportError> {
        let observed = self.drive().observe()?;
        let decision = decide(progress, &observed);
        let text = decision.describe(progress, &observed);
        Ok((decision, observed, text))
    }

    /// 決めた動きを実行する。Drive を変える操作（`incoming` の移動、旧フォルダのゴミ箱への移動）の前には、段階を `sink` に書く。
    /// 書けなければその操作はしない（移す前なら `blocked`〔`record_failed`〕、ゴミ箱へ移す前なら、公開済みとして旧フォルダが
    /// `replaced/` に残っていることを知らせる）。`blocked` の `progress` は、最後に記録できた段階。
    pub fn execute(
        &self,
        progress: &Progress,
        decision: &Decision,
        observed: &Observed,
        sink: &mut dyn ProgressSink,
    ) -> Result<Resolution, Box<BlockedInfo>> {
        let mut cur = progress.clone();
        let drive = self.drive();
        match &decision.action {
            Action::Blocked(reason) => Err(self.blocked(&cur, *reason, Some(observed))),
            Action::Abort { remove_incoming } => {
                let left = match remove_incoming {
                    Some(id) => clean_incoming(&drive, Some(id)),
                    None => Leftover::NONE,
                };
                Ok(Resolution::Interrupted {
                    leftover_incoming: left.present,
                    leftover_incoming_id: left.id,
                })
            }
            // ID を記録していない incoming は、名前だけでは消さない
            Action::KeepUnrecordedIncoming { incoming_id } => Ok(Resolution::Interrupted {
                leftover_incoming: true,
                leftover_incoming_id: Some(incoming_id.clone()),
            }),
            Action::FinishPublished {
                folder_id,
                trash_old,
            } => {
                match self.content_ok(Place::Dest) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(self.blocked(
                            &cur,
                            BlockedReason::ContentMismatch,
                            Some(observed),
                        ));
                    }
                    Err(_) => {
                        return Err(self.blocked(
                            &cur,
                            BlockedReason::ExecutionFailed,
                            Some(observed),
                        ));
                    }
                }
                self.finish(&mut cur, observed, folder_id, trash_old.as_deref(), sink)
            }
            Action::RollForward {
                incoming_id,
                old_id,
            } => {
                match self.content_ok(Place::Incoming) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(self.blocked(
                            &cur,
                            BlockedReason::ContentMismatch,
                            Some(observed),
                        ));
                    }
                    Err(_) => {
                        return Err(self.blocked(
                            &cur,
                            BlockedReason::ExecutionFailed,
                            Some(observed),
                        ));
                    }
                }
                // 移す前に `publishing` を記録する（移した直後にまた止まっても、記録が `retiring` のままにならない）
                if Self::record_stage(&mut cur, Stage::Publishing, sink).is_err() {
                    return Err(self.blocked(&cur, BlockedReason::RecordFailed, Some(observed)));
                }
                // 直前に宛先が無いことをもう一度確かめる（あれば、移さない）
                match drive.target() {
                    Ok(Target::Absent) => {}
                    Ok(Target::Exists { .. }) | Err(_) => {
                        return Err(self.blocked(
                            &cur,
                            BlockedReason::DestinationUnexpected,
                            Some(observed),
                        ));
                    }
                }
                if drive.publish_move().is_err() {
                    // 移動の応答が失われても済んでいることがあり、止めたあとも Drive 側で遅れて反映されることがある。
                    // 公開の途中の失敗と同じく、待って、一覧を 2 回取って、同じなら照合する
                    match self.settle_observe(true) {
                        Some(o) if o.dest == [Seen::dir(incoming_id)] && o.incoming.is_empty() => {}
                        Some(o) => {
                            return Err(self.blocked(
                                &cur,
                                BlockedReason::ExecutionFailed,
                                Some(&o),
                            ));
                        }
                        None => return Err(self.blocked(&cur, BlockedReason::Unobservable, None)),
                    }
                }
                // 公開の直後の照合（宛先の ID・incoming が残っていない・中身）。起動時は宛先の manifest で照合する
                match drive.check_published(incoming_id, self.expected) {
                    Ok(Ok(())) => {}
                    Ok(Err(o)) => {
                        return Err(self.blocked(&cur, BlockedReason::PublishMismatch, Some(&o)));
                    }
                    Err(_) => {
                        return Err(self.blocked(
                            &cur,
                            BlockedReason::ExecutionFailed,
                            Some(observed),
                        ));
                    }
                }
                self.finish(&mut cur, observed, incoming_id, Some(old_id), sink)
            }
        }
    }

    /// 宛先（[`Place::Dest`]）または `incoming` の中身が、期待した一式と一致するか。
    fn content_ok(&self, place: Place) -> Result<bool, ExportError> {
        let drive = self.drive();
        match (self.expected, place) {
            (Some(e), Place::Incoming) => drive.incoming_matches(e),
            (Some(e), _) => drive.dest_matches_expected(e),
            (None, place) => drive.folder_matches_its_manifest(place),
        }
    }

    /// 公開済みの仕上げ（旧フォルダのゴミ箱への移動と、公開の記録）。ゴミ箱へ移す前に `trashing` を記録する。
    fn finish(
        &self,
        cur: &mut Progress,
        observed: &Observed,
        folder_id: &str,
        trash_old: Option<&str>,
        sink: &mut dyn ProgressSink,
    ) -> Result<Resolution, Box<BlockedInfo>> {
        let drive = self.drive();
        // 記録できなければ、ゴミ箱へは移さない（公開は済んでいる。旧フォルダは `replaced/` に残る）
        let mut skipped_trash = None;
        let mut trash = trash_old;
        if let Some(old) = trash_old
            && Self::record_stage(cur, Stage::Trashing, sink).is_err()
        {
            skipped_trash = Some(old);
            trash = None;
        }
        let mut published = finish_tail(
            &drive,
            &drive,
            self.published,
            self.layout,
            Finish {
                folder_id,
                trash_old: trash,
                replaced: cur.old_id.is_some(),
                existing_files: 0,
            },
        )
        .map_err(|stop| stop.into_blocked(cur, Some(observed)))?;
        if let Some(old) = skipped_trash {
            published.old_left_in_replaced = confirm_old_left(&drive, old)
                .map_err(|stop| stop.into_blocked(cur, Some(observed)))?;
        }
        Ok(Resolution::Published(published))
    }
}

/// [`finish_tail`] の入力。
struct Finish<'a> {
    folder_id: &'a str,
    /// ゴミ箱へ移す旧フォルダの ID（移動が済んでいるときは `None`）。
    trash_old: Option<&'a str>,
    /// 置き換えだったか（記録した旧 ID があるか）。ゴミ箱への移動が済んでいても真。
    replaced: bool,
    existing_files: u64,
}

/// 旧フォルダが `replaced/` にまだあるかを、一覧で確かめる（ゴミ箱への移動の失敗のあと・移さなかったあと）。
/// 無ければ `None`（済んでいる）、記録した旧 ID のフォルダ 1 つだけなら `Some`（残っている）。それ以外・一覧できないときは
/// 成功にせず、止める理由を返す。
fn confirm_old_left(drive: &Drive<'_>, old: &str) -> Result<Option<String>, TrashStop> {
    match drive.replaced_seen() {
        Ok(seen) if seen.is_empty() => Ok(None),
        Ok(seen) if matches!(seen.as_slice(), [s] if s.kind == Kind::Dir && s.id == old) => {
            Ok(Some(old.to_owned()))
        }
        Ok(seen) => Err(TrashStop::Mismatch(seen)),
        Err(_) => Err(TrashStop::Unconfirmed),
    }
}

/// 仕上げ: 旧フォルダ（`trash_old`）をゴミ箱へ移し、公開の記録（`published.json`）に足す。
///
/// - ゴミ箱へ移す前に、`replaced/<…>` が記録した旧フォルダの ID 1 つだけで、中身が旧 manifest の一式と完全に一致することを
///   確かめる（[`Drive::trash_replaced`]）。違えば何も消さずに `Err`（`blocked`）。
/// - 移せなかったときは、`free`（ジョブの期限を掛けない）で一覧し直して確かめる。もう無ければ（応答が失われても、移動は済んで
///   いることがある）済み。旧 ID のフォルダが 1 つだけ残っていれば、新しい一式は公開済みなので `old_left_in_replaced` で知らせて
///   終える。**別の ID だった・一覧できなかったときは成功にせず `Err`**（`blocked`）。
/// - 公開の記録に書けなかったときは `recorded: false`（[`Published::record`] を残す。次の `replace` は、このフォルダを管理して
///   いないものとして断る）。
fn finish_tail(
    drive: &Drive<'_>,
    free: &Drive<'_>,
    published: &PublishedStore,
    layout: &Layout,
    finish: Finish<'_>,
) -> Result<Published, TrashStop> {
    let Finish {
        folder_id,
        trash_old,
        replaced,
        existing_files,
    } = finish;
    let mut left = None;
    if let Some(old) = trash_old {
        match drive.trash_replaced(old) {
            Ok(Removed::Done | Removed::Nothing) => {}
            Ok(Removed::Mismatch(seen)) => return Err(TrashStop::Mismatch(seen)),
            Ok(Removed::ContentMismatch(seen)) => return Err(TrashStop::Content(seen)),
            Err(_) => left = confirm_old_left(free, old)?,
        }
    }
    let record = PublishedRecord {
        destination: layout.destination(),
        folder: layout.folder().clone(),
        folder_id: folder_id.to_owned(),
        job_id: layout.job().clone(),
    };
    let recorded = published.upsert(record.clone()).is_ok();
    Ok(Published {
        folder_id: folder_id.to_owned(),
        replaced,
        existing_files,
        old_left_in_replaced: left,
        recorded,
        record,
    })
}

// ---- 公開 ----

/// [`publish`] への入力。
pub struct PublishParams<'a> {
    pub on_exists: OnExists,
    /// 作業領域（ローカルのディレクトリ。絶対パス）。
    pub workspace: &'a Path,
    /// 公開する一式（写真と、最後に `manifest.json`。[`crate::image::Prepared::files`]）。
    pub files: &'a [PublishFile],
    pub published: &'a PublishedStore,
    pub timing: Timing,
}

enum StepErr {
    /// rclone の失敗（応答が失われた移動を含む）。その場で復旧の表を実行する。
    Op(ExportError),
    /// こちらで決めた中止（検証の不一致、宛先が変わっていた、記録を書けない）。`incoming` を片付けて失敗で終える。
    Abort(ExportError),
    /// 照合できない。
    Blocked(Box<BlockedInfo>),
}

struct Run<'a> {
    layout: &'a Layout,
    params: &'a PublishParams<'a>,
    sink: &'a mut dyn ProgressSink,
    /// ジョブの期限を掛けた rclone の `Drive`。
    drive: Drive<'a>,
    /// 期限を掛けない `Drive`（仕上げの確認と後始末）。
    free_drive: Drive<'a>,
    expected: Expected,
    /// 最後に**記録できた**段階と ID。
    progress: Progress,
    old_id: Option<String>,
    existing_files: u64,
    incoming_created: bool,
    /// `incoming/<job_id>` を作ったあとに、この処理が一覧から確かめた ID（ジョブの記録に書けなかったときも、この ID
    /// なら片付けられる。作ったばかりのフォルダを指している）。
    known_incoming_id: Option<String>,
    retire_issued: bool,
}

/// 照会の失敗を、ステップの失敗にする。`ambiguous_destination` は Drive を変える前の判定なので中止、それ以外は rclone の失敗。
fn query_failure(e: ExportError) -> StepErr {
    if e.code == ErrorCode::AmbiguousDestination {
        StepErr::Abort(e)
    } else {
        StepErr::Op(e)
    }
}

fn changed_during_run() -> ExportError {
    ExportError::new(
        ErrorCode::AmbiguousDestination,
        "実行中に書き出し先が変わりました",
    )
}

impl Run<'_> {
    /// 段階を記録する（Drive の操作の前）。記録できなければ、その段階の操作はしない。
    fn record(&mut self, stage: Stage, incoming_id: Option<&str>) -> Result<(), StepErr> {
        let next = Progress {
            stage,
            old_id: self.old_id.clone(),
            incoming_id: incoming_id
                .map(str::to_owned)
                .or_else(|| self.progress.incoming_id.clone()),
        };
        match self.sink.record(&next) {
            Ok(()) => {
                self.progress = next;
                Ok(())
            }
            // 旧フォルダを退避したあとは、中止できない（宛先が空のまま）。記録済みの段階から復旧の表で進める
            Err(e) if self.retire_issued => Err(StepErr::Op(e)),
            Err(e) => Err(StepErr::Abort(e)),
        }
    }

    fn steps(&mut self) -> Result<Published, StepErr> {
        // 1. uploading: 段階を記録 → incoming/<job_id> を作る → ID を記録 → 写真 → 最後に manifest
        self.record(Stage::Uploading, None)?;
        self.incoming_created = true;
        self.drive.mkdir_incoming().map_err(StepErr::Op)?;
        let incoming_id = self.drive.incoming_id().map_err(StepErr::Op)?;
        self.known_incoming_id = Some(incoming_id.clone());
        self.record(Stage::Uploading, Some(&incoming_id))?;
        self.drive
            .upload(self.params.workspace)
            .map_err(StepErr::Op)?;

        // 2. verifying: 送った中身が一式と完全に一致する（受け取り側と同じ検証）
        self.record(Stage::Verifying, Some(&incoming_id))?;
        if !self
            .drive
            .incoming_matches(&self.expected)
            .map_err(StepErr::Op)?
        {
            return Err(StepErr::Abort(ExportError::from(ErrorCode::VerifyMismatch)));
        }

        // 3. 宛先の状態をもう一度調べる（変わっていたら公開しない。置き換えなら、管理しているフォルダかも確かめ直す）
        let again = self
            .drive
            .inspect(self.params.published, self.old_id.is_some())
            .map_err(query_failure)?;
        let unchanged = match (&self.old_id, &again.target, again.managed) {
            (None, Target::Absent, _) => true,
            (Some(old), Target::Exists { id }, Some(Managed::Managed)) => id == old,
            _ => false,
        };
        if !unchanged {
            return Err(StepErr::Abort(changed_during_run()));
        }

        // 4. 公開
        match self.old_id.clone() {
            None => self.publish_absent(&incoming_id),
            Some(old) => self.publish_replace(&incoming_id, &old),
        }
    }

    /// 宛先が無いとき: 直前にもう一度「無い」ことを確かめて、`incoming` を宛先へ移す。
    ///
    /// 確かめてから移すまでの数秒に、オーナーが同じ名前のフォルダを作ると、rclone の `moveto` は Drive の中での移動をやめて
    /// ファイル単位の移動に切り替わる（止める手段が無い。設計 4.2 の「残る前提」の範囲）。宛先のフォルダに `incoming` の写真が
    /// 足される（消しはしない）。移したあとの照合（宛先の ID が `incoming` の ID と違う）で見つけて `blocked` で止まり、
    /// `recover` の表示で、オーナーが足された写真を手で片付ける。
    fn publish_absent(&mut self, incoming_id: &str) -> Result<Published, StepErr> {
        self.record(Stage::Publishing, Some(incoming_id))?;
        match self.drive.target() {
            Ok(Target::Absent) => {}
            // 何も移していない段階。作業用のものを片付けて中止する
            Ok(Target::Exists { .. }) => return Err(StepErr::Abort(changed_during_run())),
            Err(e) => return Err(query_failure(e)),
        }
        self.drive.publish_move().map_err(StepErr::Op)?;
        self.check_published(incoming_id)?;
        self.finish(incoming_id, None)
    }

    /// 置き換え: 退避 → 公開 → ゴミ箱。退避・公開の移動が、確かめてから移すまでの数秒に同じ名前のフォルダができた場合の影響は、
    /// [`Self::publish_absent`] と同じ（`retiring` では、`replaced/<…>` にできていたフォルダへ旧フォルダの中身が足される。
    /// 退避の直後の照合で見つけて `blocked` にし、ゴミ箱へは移さない）。
    fn publish_replace(&mut self, incoming_id: &str, old: &str) -> Result<Published, StepErr> {
        // retiring: 旧フォルダ（ID 記録済み）を replaced/<…> へ移し、結果を照合する
        self.record(Stage::Retiring, Some(incoming_id))?;
        self.retire_issued = true;
        self.drive.retire_old().map_err(StepErr::Op)?;
        match self.drive.check_retired(old).map_err(StepErr::Op)? {
            Ok(()) => {}
            // 合わなければ先へ進まない（ゴミ箱へも移さない）
            Err(o) => {
                return Err(StepErr::Blocked(Box::new(BlockedInfo {
                    reason: BlockedReason::RetireMismatch,
                    progress: self.progress.clone(),
                    observed: Some(o),
                })));
            }
        }
        // publishing: 直前に宛先が無いことを確かめる（あれば、旧フォルダは退避済みなので復旧の表に任せる → blocked）
        self.record(Stage::Publishing, Some(incoming_id))?;
        match self.drive.target() {
            Ok(Target::Absent) => {}
            Ok(Target::Exists { .. }) => return Err(StepErr::Op(changed_during_run())),
            Err(e) => return Err(StepErr::Op(e)),
        }
        self.drive.publish_move().map_err(StepErr::Op)?;
        self.check_published(incoming_id)?;
        // trashing: 直前に replaced/ を一覧して ID を確かめてから、ゴミ箱へ（仕上げの中でする）
        self.record(Stage::Trashing, Some(incoming_id))?;
        self.finish(incoming_id, Some(old))
    }

    /// 移動のあとの照合（終了コードを信じず、一覧で）。合わなければ `blocked`。
    fn check_published(&self, incoming_id: &str) -> Result<(), StepErr> {
        match self
            .drive
            .check_published(incoming_id, Some(&self.expected))
            .map_err(StepErr::Op)?
        {
            Ok(()) => Ok(()),
            Err(o) => Err(StepErr::Blocked(Box::new(BlockedInfo {
                reason: BlockedReason::PublishMismatch,
                progress: self.progress.clone(),
                observed: Some(o),
            }))),
        }
    }

    fn finish(&self, folder_id: &str, trash_old: Option<&str>) -> Result<Published, StepErr> {
        finish_tail(
            &self.drive,
            &self.free_drive,
            self.params.published,
            self.layout,
            Finish {
                folder_id,
                trash_old,
                replaced: self.old_id.is_some(),
                existing_files: self.existing_files,
            },
        )
        .map_err(|stop| StepErr::Blocked(stop.into_blocked(&self.progress, None)))
    }
}

/// 宛先の状態を調べ、書ける状態なら Drive に公開する（4.1 の手順 8・9）。`rclone` にはジョブ全体の期限を掛けておく。
///
/// - 宛先がある（`on_exists: error`）、管理しているフォルダでない（`replace` だが `not_managed`）、中身が変わっている
///   （`modified`）ときは、`copy`・`moveto`・`purge` を呼ばずに [`PublishOutcome::Conflict`]。
/// - 照会の失敗は `drive_unavailable`、同名が複数・ファイル・ショートカットは `ambiguous_destination`。
/// - 公開の途中で rclone が失敗・タイムアウトしたら、待って（[`Timing`]）一覧を 2 回取り、復旧の表（[`crate::recovery`]）を
///   その場で実行する。前のままなら `incoming` を片付けて、元の失敗を返す。公開まで済んでいれば、成功として返す。
///   決まらなければ `blocked`。
pub fn publish(
    rclone: &Rclone,
    layout: &Layout,
    params: &PublishParams<'_>,
    sink: &mut dyn ProgressSink,
) -> Result<PublishOutcome, PublishFailure> {
    // 前提: 写真が 1 枚以上と、最後の manifest.json（0 枚のときは呼ばない）
    let well_formed = params.files.len() >= 2
        && params
            .files
            .last()
            .is_some_and(|f| f.path == "manifest.json")
        && params.files[..params.files.len() - 1]
            .iter()
            .all(|f| f.path != "manifest.json")
        && params.workspace.is_absolute();
    if !well_formed {
        return Err(PublishFailure::plain(ExportError::new(
            ErrorCode::Internal,
            "公開する一式が決められた形ではありません",
        )));
    }
    let free = rclone.with_deadline(None);
    let drive = Drive::new(rclone, layout);
    let expected = Expected::from_publish_files(params.files);

    // 4.1 の手順 8: 宛先の状態
    let want_managed = params.on_exists == OnExists::Replace;
    let inspection = drive
        .inspect(params.published, want_managed)
        .map_err(PublishFailure::plain)?;
    let old_id = match (&inspection.target, params.on_exists) {
        (Target::Absent, _) => None,
        (Target::Exists { .. }, OnExists::Error) => {
            return Ok(PublishOutcome::Conflict {
                reason: ConflictReason::Exists,
                existing_files: inspection.existing_files,
            });
        }
        (Target::Exists { id }, OnExists::Replace) => match inspection.managed {
            Some(Managed::Managed) => Some(id.clone()),
            Some(Managed::Modified) => {
                return Ok(PublishOutcome::Conflict {
                    reason: ConflictReason::Modified,
                    existing_files: inspection.existing_files,
                });
            }
            _ => {
                return Ok(PublishOutcome::Conflict {
                    reason: ConflictReason::NotManaged,
                    existing_files: inspection.existing_files,
                });
            }
        },
    };

    let mut run = Run {
        layout,
        params,
        sink,
        drive,
        free_drive: Drive::new(&free, layout),
        expected: expected.clone(),
        progress: Progress {
            stage: Stage::Uploading,
            old_id: old_id.clone(),
            incoming_id: None,
        },
        old_id,
        existing_files: inspection.existing_files,
        incoming_created: false,
        known_incoming_id: None,
        retire_issued: false,
    };
    let recovery = Recovery {
        rclone: &free,
        layout,
        published: params.published,
        expected: Some(&expected),
        timing: params.timing,
    };

    match run.steps() {
        Ok(published) => Ok(PublishOutcome::Published(published)),
        Err(StepErr::Blocked(info)) => Err(PublishFailure::blocked(info)),
        Err(StepErr::Abort(error)) => {
            // 記録した ID、なければ作った直後にこの処理が確かめた ID で片付ける。どちらも無ければ名前だけでは消さず、
            // 残っていれば残りものとして返す
            let id = run
                .progress
                .incoming_id
                .clone()
                .or_else(|| run.known_incoming_id.clone());
            let left = if run.incoming_created {
                clean_incoming(&run.free_drive, id.as_deref())
            } else {
                Leftover::NONE
            };
            Err(PublishFailure {
                error,
                blocked: None,
                leftover_incoming: left.present,
                leftover_incoming_id: left.id,
            })
        }
        Err(StepErr::Op(error)) => {
            // 移動・ゴミ箱の段階は、rclone を止めたあとに Drive 側で遅れて反映されることがあるので、待ってから取る
            let wait = matches!(
                run.progress.stage,
                Stage::Retiring | Stage::Publishing | Stage::Trashing
            ) || run.retire_issued;
            match recovery.run(&run.progress, wait, &mut *run.sink) {
                Ok(Resolution::Interrupted {
                    leftover_incoming,
                    leftover_incoming_id,
                }) => Err(PublishFailure {
                    error,
                    blocked: None,
                    leftover_incoming,
                    leftover_incoming_id,
                }),
                Ok(Resolution::Published(mut published)) => {
                    published.existing_files = run.existing_files;
                    Ok(PublishOutcome::Published(published))
                }
                Err(info) => Err(PublishFailure::blocked(info)),
            }
        }
    }
}

/// `dry_run`（design.md 6.1）: 宛先の状態と、`exists` のときの `existing_files`・管理しているフォルダか。
/// 呼ぶ rclone のコマンドは `lsjson` と（管理の確認の）`cat` だけ。`rclone` は [`Rclone::for_dry_run`]（期限 30 秒）で作る。
pub fn dry_run(
    rclone: &Rclone,
    layout: &Layout,
    published: &PublishedStore,
) -> Result<Inspection, ExportError> {
    dry_run_within(rclone, layout, published, crate::rclone::DRY_RUN_TIMEOUT)
}

/// [`dry_run`] の、全体の期限を替えられる形（試験用。本番は 30 秒）。宛先がある場合は 5 回ほど rclone を呼ぶので、
/// コマンドごとの期限（[`Rclone::for_dry_run`] の 30 秒）とは別に、`dry_run` 全体に期限を掛ける。過ぎたら `drive_unavailable`。
#[doc(hidden)]
pub fn dry_run_within(
    rclone: &Rclone,
    layout: &Layout,
    published: &PublishedStore,
    limit: std::time::Duration,
) -> Result<Inspection, ExportError> {
    let bounded = rclone.with_deadline(Some(std::time::Instant::now() + limit));
    Drive::new(&bounded, layout)
        .inspect(published, true)
        .map_err(|e| {
            if e.code == ErrorCode::Timeout {
                ExportError::new(
                    ErrorCode::DriveUnavailable,
                    "Drive への照会が期限内に終わりませんでした",
                )
            } else {
                e
            }
        })
}
