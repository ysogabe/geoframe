//! 復旧の表（design.md 4.2「復旧」）を、**純粋な関数**として持つ。
//!
//! 入力は、ジョブの記録に書いてあった段階と ID（[`Progress`]）と、Drive の実際の状態（[`Observed`]。宛先・`incoming`・
//! `replaced` の中の、このジョブに関係する名前の項目と、その ID）。出力は、次の動き（[`Action`]）と、`recover` の表示に使う
//! 説明（[`Decision::describe`]）。Drive には触れない。動きの実行は [`crate::publish`]、起動時の呼び出しは T5。
//!
//! ## 表の各行と、この関数の対応
//!
//! 設計の表（4.2）は段階ごとの行だが、Drive の状態から見ると 3 つのまとまりになる。どれにも当てはまらない状態は、
//! 必ず [`Action::Blocked`]（表の最後の行）。
//!
//! 1. **前のまま**（[`Row::BeforeChange`]）: 宛先が前のまま（新規なら無い、置き換えなら記録した旧フォルダの ID だけ）で、
//!    `replaced/` にこのジョブの項目が無い。→ `incoming` を消して `interrupted`。設計の行「`uploading`・`verifying`」と
//!    「`retiring` で、旧フォルダが宛先にあり `replaced/…` が無い」を含む。`replaced/` に何かあれば `blocked`。
//! 2. **前へ進める**（[`Row::RollForward`]）: 置き換えで、旧フォルダが `replaced/` にあり、宛先が無く、`incoming` がある。
//!    → `incoming` を宛先へ移し、旧フォルダをゴミ箱へ（設計の行「`retiring`・`publishing`」）。
//! 3. **公開済み**（[`Row::Published`]）: 宛先が `incoming` の ID のフォルダになっていて、`incoming` は無い。→ 公開の記録に足し、
//!    置き換えなら旧フォルダのゴミ箱への移動を続ける（設計の行「`publishing`」「`trashing`」）。
//!
//! ### 設計の表に文字どおりには無く、この関数が決めている行（報告で確かめてもらう）
//!
//! - `trashing` で、宛先が新しい一式で、`replaced/` が空 → ゴミ箱への移動は済んでいる（応答が失われた）。公開済みとして終える。
//! - 新規（旧フォルダ無し）の `publishing` で、宛先がまだ無く `incoming` が残っている → 前のまま（`incoming` を消して
//!   `interrupted`）。移動が実行されなかった（rclone の失敗で止まった）通常の失敗が、`blocked` にならないようにするため。
//! - `uploading` で、`incoming/<job_id>` はあるが ID を記録する前に止まった → ジョブ ID は乱数で自分しか使わない名前なので、
//!   名前で見つけたものを `incoming` として扱い、消す（記録した ID が無いときだけ）。
//! - 置き換えの `publishing` で、宛先が旧フォルダのまま・`replaced/` が空 → 前のまま（退避は実行されていない）。
//!
//! 起動時（T5）にも、各ジョブの途中の失敗（[`crate::publish`]）にも、同じこの関数を使う（一方が `blocked` にする状態を、
//! もう一方が進めることがないように）。

use std::fmt;

use crate::rclone::{LsEntry, SHORTCUT_MIME};

/// 公開の手順の段階（ジョブの記録に書く名前）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Uploading,
    Verifying,
    Retiring,
    Publishing,
    Trashing,
}

impl Stage {
    pub const ALL: [Stage; 5] = [
        Self::Uploading,
        Self::Verifying,
        Self::Retiring,
        Self::Publishing,
        Self::Trashing,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uploading => "uploading",
            Self::Verifying => "verifying",
            Self::Retiring => "retiring",
            Self::Publishing => "publishing",
            Self::Trashing => "trashing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// ジョブの記録に書く、段階と関係する ID。各段階に進む前に書く（[`crate::publish::ProgressSink`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    /// 置き換えのとき、記録した旧フォルダの Drive の ID（新規なら `None`）。
    pub old_id: Option<String>,
    /// `incoming/<job_id>` の Drive の ID。`uploading` の最初（フォルダを作る前）だけ `None`。
    pub incoming_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Dir,
    File,
    Shortcut,
}

/// Drive で見えた項目 1 つ（ID と種類）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seen {
    pub id: String,
    pub kind: Kind,
}

impl Seen {
    pub fn of(e: &LsEntry) -> Self {
        Self {
            id: e.id.clone(),
            kind: if e.mime == SHORTCUT_MIME {
                Kind::Shortcut
            } else if e.is_dir {
                Kind::Dir
            } else {
                Kind::File
            },
        }
    }

    pub fn dir(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            kind: Kind::Dir,
        }
    }
}

/// Drive の今の状態のうち、このジョブに関係する 3 か所。各リストは ID の順に並べる（2 回の観察を比べられるように）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// 接頭辞の直下で、フォルダ名に完全に一致する項目（ショートカットも含む）。
    pub dest: Vec<Seen>,
    /// `incoming/` の直下で、ジョブ ID に一致する項目。
    pub incoming: Vec<Seen>,
    /// `replaced/` の直下で、`<種類>-<フォルダ名>-<job_id>` に一致する項目。
    pub replaced: Vec<Seen>,
}

impl Observed {
    pub fn sorted(mut self) -> Self {
        self.dest.sort();
        self.incoming.sort();
        self.replaced.sort();
        self
    }
}

/// 表のどの行に当てはまったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    BeforeChange,
    RollForward,
    Published,
}

/// `blocked` にした理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedReason {
    /// 記録した段階と、Drive の状態が食い違う。
    StageInconsistent,
    /// 宛先に、記録にない別のもの（ID が違う・複数・ファイル・ショートカット）がある。
    DestinationUnexpected,
    /// `replaced/` に、このジョブの名前の項目がある（あってはならない状態、または記録した旧 ID 以外）。
    ReplacedUnexpected,
    /// `incoming` の ID・個数・種類が、記録と合わない。
    IncomingUnexpected,
    /// 一覧が決まらない（2 回の観察が一致しない・取れない）。
    Unobservable,
    /// 復旧の動きを実行したが、rclone が失敗した。
    ExecutionFailed,
    /// 退避（旧フォルダの移動）のあとの照合が合わない（ファイル単位の移動に切り替わった、同じ名前の別のフォルダを移した、など）。
    RetireMismatch,
    /// 公開（`incoming` の移動）のあとの照合が合わない。
    PublishMismatch,
    /// 前へ進める・公開済みと見る前の、宛先の中身の照合が合わない（manifest と一致しない）。
    ContentMismatch,
    /// ゴミ箱へ移す前の照合が合わない（`replaced/` の項目が記録した旧フォルダの ID と違う）。
    TrashMismatch,
}

impl BlockedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StageInconsistent => "stage_inconsistent",
            Self::DestinationUnexpected => "destination_unexpected",
            Self::ReplacedUnexpected => "replaced_unexpected",
            Self::IncomingUnexpected => "incoming_unexpected",
            Self::Unobservable => "unobservable",
            Self::ExecutionFailed => "execution_failed",
            Self::RetireMismatch => "retire_mismatch",
            Self::PublishMismatch => "publish_mismatch",
            Self::ContentMismatch => "content_mismatch",
            Self::TrashMismatch => "trash_mismatch",
        }
    }

    pub const fn text(self) -> &'static str {
        match self {
            Self::StageInconsistent => "記録した段階と Drive の状態が食い違っています",
            Self::DestinationUnexpected => "宛先に、記録にない別のものがあります",
            Self::ReplacedUnexpected => {
                "replaced/ に、このジョブの名前の項目が想定外の形であります"
            }
            Self::IncomingUnexpected => "incoming の ID・個数・種類が記録と合いません",
            Self::Unobservable => {
                "Drive の状態が決まりません（一覧が安定しない、または取れません）"
            }
            Self::ExecutionFailed => "復旧の動きを実行できませんでした",
            Self::RetireMismatch => "旧フォルダの退避のあとの照合が合いません",
            Self::PublishMismatch => "公開（incoming の移動）のあとの照合が合いません",
            Self::ContentMismatch => "宛先の中身が manifest と一致しません",
            Self::TrashMismatch => {
                "ゴミ箱へ移す前の照合が合いません（replaced/ の項目が記録した旧フォルダと違います）"
            }
        }
    }
}

/// 次の動き。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `incoming`（あれば。ID は観察した値）を消して、ジョブを `interrupted`（実行中の失敗なら、その失敗）で終える。
    /// 宛先は前のまま。
    Abort { remove_incoming: Option<String> },
    /// 置き換えを前へ進める: `incoming` を宛先へ移し、移動を照合し、旧フォルダをゴミ箱へ移す。
    RollForward { incoming_id: String, old_id: String },
    /// 公開済み。公開の記録に足し、`trash_old` があれば旧フォルダのゴミ箱への移動を続ける。
    FinishPublished {
        folder_id: String,
        trash_old: Option<String>,
    },
    /// 照合できない。記録を残し、オーナーが `recover --resolve` で解除するまで次の書き出しを断る。
    Blocked(BlockedReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub row: Option<Row>,
    pub action: Action,
}

fn one_dir(v: &[Seen]) -> Option<&str> {
    match v {
        [s] if s.kind == Kind::Dir => Some(&s.id),
        _ => None,
    }
}

fn blocked(reason: BlockedReason) -> Decision {
    Decision {
        row: None,
        action: Action::Blocked(reason),
    }
}

/// 復旧の表を当てはめる。
pub fn decide(p: &Progress, o: &Observed) -> Decision {
    let old = p.old_id.as_deref();
    let inc = p.incoming_id.as_deref();
    let dest_one = one_dir(&o.dest);
    let replaced_one = one_dir(&o.replaced);

    // 3. 公開済み: 宛先が incoming の ID のフォルダで、incoming は無い
    if let (Some(d), Some(i)) = (dest_one, inc)
        && d == i
        && o.incoming.is_empty()
    {
        if !matches!(p.stage, Stage::Publishing | Stage::Trashing) {
            return blocked(BlockedReason::StageInconsistent);
        }
        let trash_old = match old {
            None if o.replaced.is_empty() => None,
            Some(old) if replaced_one == Some(old) => Some(old.to_owned()),
            // ゴミ箱への移動は済んでいる（応答が失われた）
            Some(_) if o.replaced.is_empty() && p.stage == Stage::Trashing => None,
            _ => return blocked(BlockedReason::ReplacedUnexpected),
        };
        return Decision {
            row: Some(Row::Published),
            action: Action::FinishPublished {
                folder_id: d.to_owned(),
                trash_old,
            },
        };
    }

    // `incoming` の見え方
    let incoming_ok = if o.incoming.is_empty() {
        Some(None)
    } else {
        one_dir(&o.incoming)
            .filter(|id| inc.is_none_or(|i| i == *id))
            .map(Some)
    };

    // 1. 前のまま
    let unchanged = match old {
        None => o.dest.is_empty(),
        Some(old) => dest_one == Some(old),
    };
    if unchanged && p.stage != Stage::Trashing {
        if !o.replaced.is_empty() {
            return blocked(BlockedReason::ReplacedUnexpected);
        }
        return match incoming_ok {
            Some(id) => Decision {
                row: Some(Row::BeforeChange),
                action: Action::Abort {
                    remove_incoming: id.map(str::to_owned),
                },
            },
            None => blocked(BlockedReason::IncomingUnexpected),
        };
    }

    // 2. 前へ進める
    if let (Some(old), Some(inc_id)) = (old, inc)
        && matches!(p.stage, Stage::Retiring | Stage::Publishing)
        && o.dest.is_empty()
        && replaced_one == Some(old)
    {
        return match incoming_ok {
            Some(Some(id)) if id == inc_id => Decision {
                row: Some(Row::RollForward),
                action: Action::RollForward {
                    incoming_id: inc_id.to_owned(),
                    old_id: old.to_owned(),
                },
            },
            _ => blocked(BlockedReason::IncomingUnexpected),
        };
    }

    blocked(if o.dest.is_empty() || unchanged {
        BlockedReason::StageInconsistent
    } else {
        BlockedReason::DestinationUnexpected
    })
}

fn ids(v: &[Seen]) -> String {
    if v.is_empty() {
        return "なし".to_owned();
    }
    v.iter()
        .map(|s| {
            let kind = match s.kind {
                Kind::Dir => "フォルダ",
                Kind::File => "ファイル",
                Kind::Shortcut => "ショートカット",
            };
            format!("{kind} {}", s.id)
        })
        .collect::<Vec<_>>()
        .join("、")
}

impl Decision {
    /// `recover` の表示に使う説明（Drive を変えない）。ID は Drive の ID で、設定・資格情報は含まない。
    pub fn describe(&self, p: &Progress, o: &Observed) -> String {
        let head = format!(
            "記録した段階: {}（{}）。Drive の状態: 宛先={}、incoming={}、replaced={}。",
            p.stage,
            if p.old_id.is_some() {
                "置き換え"
            } else {
                "新規"
            },
            ids(&o.dest),
            ids(&o.incoming),
            ids(&o.replaced),
        );
        let tail = match &self.action {
            Action::Abort { remove_incoming } => match remove_incoming {
                Some(id) => format!(
                    "宛先は前のままです。incoming（ID {id}）を消して interrupted で終えます。"
                ),
                None => {
                    "宛先は前のままで、消すものはありません。interrupted で終えます。".to_owned()
                }
            },
            Action::RollForward {
                incoming_id,
                old_id,
            } => format!(
                "旧フォルダ（ID {old_id}）は replaced/ にあり、宛先は空です。incoming（ID {incoming_id}）を宛先へ移し、旧フォルダをゴミ箱へ移して続けます。"
            ),
            Action::FinishPublished {
                folder_id,
                trash_old,
            } => match trash_old {
                Some(old) => format!(
                    "宛先は新しい一式（ID {folder_id}）です。公開の記録に足し、旧フォルダ（ID {old}）をゴミ箱へ移して続けます。"
                ),
                None => format!(
                    "宛先は新しい一式（ID {folder_id}）です。公開済みとして、公開の記録に足します。"
                ),
            },
            Action::Blocked(reason) => format!(
                "照合できません（{}）: {}。オーナーが Drive を確かめて `recover --resolve` で解除するまで、次の書き出しを断ります。",
                reason.as_str(),
                reason.text()
            ),
        };
        format!("{head}{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "fidOLD";
    const INC: &str = "fidINC";

    fn dir(id: &str) -> Seen {
        Seen::dir(id)
    }

    /// 宛先・incoming・replaced の見え方の候補。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum D {
        Empty,
        Old,
        Inc,
        Other,
        Dup,
        File,
        Shortcut,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum I {
        Gone,
        Intact,
        Other,
        Dup,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum R {
        Empty,
        Old,
        Other,
        Dup,
    }

    fn dest_of(d: D) -> Vec<Seen> {
        match d {
            D::Empty => vec![],
            D::Old => vec![dir(OLD)],
            D::Inc => vec![dir(INC)],
            D::Other => vec![dir("fidX")],
            D::Dup => vec![dir(OLD), dir("fidX")],
            D::File => vec![Seen {
                id: OLD.into(),
                kind: Kind::File,
            }],
            D::Shortcut => vec![Seen {
                id: INC.into(),
                kind: Kind::Shortcut,
            }],
        }
    }

    fn observed(d: D, i: I, r: R) -> Observed {
        Observed {
            dest: dest_of(d),
            incoming: match i {
                I::Gone => vec![],
                I::Intact => vec![dir(INC)],
                I::Other => vec![dir("fidY")],
                I::Dup => vec![dir(INC), dir("fidY")],
            },
            replaced: match r {
                R::Empty => vec![],
                R::Old => vec![dir(OLD)],
                R::Other => vec![dir("fidZ")],
                R::Dup => vec![dir(OLD), dir("fidZ")],
            },
        }
        .sorted()
    }

    fn progress(stage: Stage, replace: bool) -> Progress {
        Progress {
            stage,
            old_id: replace.then(|| OLD.to_owned()),
            incoming_id: Some(INC.to_owned()),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Expect {
        Abort(bool), // incoming を消すか
        Forward,
        Finish { trash: bool },
    }

    /// 表で「進める・片付ける」と決めているセル。ここに無いセルは、すべて blocked でなければならない。
    fn expected(stage: Stage, replace: bool, d: D, i: I, r: R) -> Option<Expect> {
        use Stage::*;
        // 前のまま: 新規は宛先が無い、置き換えは旧フォルダだけ。replaced は空。incoming は無い・記録どおり
        let unchanged = if replace { d == D::Old } else { d == D::Empty };
        if unchanged && stage != Trashing && r == R::Empty {
            return match i {
                I::Gone => Some(Expect::Abort(false)),
                I::Intact => Some(Expect::Abort(true)),
                _ => None,
            };
        }
        // 前へ進める
        if replace
            && matches!(stage, Retiring | Publishing)
            && d == D::Empty
            && r == R::Old
            && i == I::Intact
        {
            return Some(Expect::Forward);
        }
        // 公開済み
        if d == D::Inc && i == I::Gone && matches!(stage, Publishing | Trashing) {
            return match (replace, r) {
                (false, R::Empty) => Some(Expect::Finish { trash: false }),
                (true, R::Old) => Some(Expect::Finish { trash: true }),
                (true, R::Empty) if stage == Trashing => Some(Expect::Finish { trash: false }),
                _ => None,
            };
        }
        None
    }

    #[test]
    fn every_cell_of_the_state_space_has_an_explicit_decision() {
        let mut non_blocked = 0;
        for stage in Stage::ALL {
            for replace in [false, true] {
                for d in [
                    D::Empty,
                    D::Old,
                    D::Inc,
                    D::Other,
                    D::Dup,
                    D::File,
                    D::Shortcut,
                ] {
                    for i in [I::Gone, I::Intact, I::Other, I::Dup] {
                        for r in [R::Empty, R::Old, R::Other, R::Dup] {
                            let got = decide(&progress(stage, replace), &observed(d, i, r));
                            let want = expected(stage, replace, d, i, r);
                            let cell = format!(
                                "{stage} replace={replace} dest={d:?} incoming={i:?} replaced={r:?}"
                            );
                            match (&got.action, want) {
                                (
                                    Action::Abort { remove_incoming },
                                    Some(Expect::Abort(removes)),
                                ) => {
                                    assert_eq!(remove_incoming.is_some(), removes, "{cell}");
                                    if let Some(id) = remove_incoming {
                                        assert_eq!(id, INC, "{cell}");
                                    }
                                    assert_eq!(got.row, Some(Row::BeforeChange), "{cell}");
                                    non_blocked += 1;
                                }
                                (
                                    Action::RollForward {
                                        incoming_id,
                                        old_id,
                                    },
                                    Some(Expect::Forward),
                                ) => {
                                    assert_eq!(
                                        (incoming_id.as_str(), old_id.as_str()),
                                        (INC, OLD),
                                        "{cell}"
                                    );
                                    assert_eq!(got.row, Some(Row::RollForward), "{cell}");
                                    non_blocked += 1;
                                }
                                (
                                    Action::FinishPublished {
                                        folder_id,
                                        trash_old,
                                    },
                                    Some(Expect::Finish { trash }),
                                ) => {
                                    assert_eq!(folder_id, INC, "{cell}");
                                    assert_eq!(
                                        trash_old.as_deref(),
                                        trash.then_some(OLD),
                                        "{cell}"
                                    );
                                    assert_eq!(got.row, Some(Row::Published), "{cell}");
                                    non_blocked += 1;
                                }
                                (Action::Blocked(_), None) => assert_eq!(got.row, None, "{cell}"),
                                (action, want) => panic!("{cell}: {action:?} / 期待 {want:?}"),
                            }
                        }
                    }
                }
            }
        }
        // 新規 4 段階×2（incoming 無し・あり）＋置き換え 4×2、進める 2、公開済み 2+3
        // （16 + 2 + 5 = 23。上の `expected` と数が合うこと）
        assert_eq!(non_blocked, 23);
    }

    // ---- 設計の表の各行（名前を付けて確かめる） ----

    #[test]
    fn row_uploading_and_verifying_with_an_untouched_destination_abort() {
        for stage in [Stage::Uploading, Stage::Verifying] {
            for replace in [false, true] {
                let d = if replace { D::Old } else { D::Empty };
                let got = decide(&progress(stage, replace), &observed(d, I::Intact, R::Empty));
                assert_eq!(
                    got.action,
                    Action::Abort {
                        remove_incoming: Some(INC.into())
                    }
                );
            }
        }
    }

    #[test]
    fn row_retiring_not_yet_moved_aborts_and_anything_in_replaced_blocks() {
        let p = progress(Stage::Retiring, true);
        assert_eq!(
            decide(&p, &observed(D::Old, I::Intact, R::Empty)).action,
            Action::Abort {
                remove_incoming: Some(INC.into())
            }
        );
        for r in [R::Old, R::Other, R::Dup] {
            assert_eq!(
                decide(&p, &observed(D::Old, I::Intact, r)).action,
                Action::Blocked(BlockedReason::ReplacedUnexpected),
                "{r:?}"
            );
        }
    }

    #[test]
    fn row_retiring_and_publishing_with_the_old_folder_in_replaced_roll_forward() {
        for stage in [Stage::Retiring, Stage::Publishing] {
            let got = decide(
                &progress(stage, true),
                &observed(D::Empty, I::Intact, R::Old),
            );
            assert_eq!(
                got.action,
                Action::RollForward {
                    incoming_id: INC.into(),
                    old_id: OLD.into()
                }
            );
        }
        // incoming が無い・別物なら、進めない
        for i in [I::Gone, I::Other, I::Dup] {
            assert!(matches!(
                decide(
                    &progress(Stage::Publishing, true),
                    &observed(D::Empty, i, R::Old)
                )
                .action,
                Action::Blocked(BlockedReason::IncomingUnexpected)
            ));
        }
    }

    #[test]
    fn row_publishing_with_the_new_set_at_the_destination_is_published() {
        let got = decide(
            &progress(Stage::Publishing, false),
            &observed(D::Inc, I::Gone, R::Empty),
        );
        assert_eq!(
            got.action,
            Action::FinishPublished {
                folder_id: INC.into(),
                trash_old: None
            }
        );
        let got = decide(
            &progress(Stage::Publishing, true),
            &observed(D::Inc, I::Gone, R::Old),
        );
        assert_eq!(
            got.action,
            Action::FinishPublished {
                folder_id: INC.into(),
                trash_old: Some(OLD.into())
            }
        );
    }

    #[test]
    fn row_trashing_retries_while_the_old_folder_is_still_in_replaced() {
        let got = decide(
            &progress(Stage::Trashing, true),
            &observed(D::Inc, I::Gone, R::Old),
        );
        assert_eq!(
            got.action,
            Action::FinishPublished {
                folder_id: INC.into(),
                trash_old: Some(OLD.into())
            }
        );
    }

    #[test]
    fn gaps_in_the_design_table_are_decided_explicitly() {
        // trashing: 旧フォルダは既に無い（purge の応答が失われた）→ 公開済み、ゴミ箱への移動は不要
        let got = decide(
            &progress(Stage::Trashing, true),
            &observed(D::Inc, I::Gone, R::Empty),
        );
        assert_eq!(
            got.action,
            Action::FinishPublished {
                folder_id: INC.into(),
                trash_old: None
            }
        );
        // 同じ状態でも publishing 段階なら、旧フォルダが消えているのは想定外
        assert!(matches!(
            decide(
                &progress(Stage::Publishing, true),
                &observed(D::Inc, I::Gone, R::Empty)
            )
            .action,
            Action::Blocked(BlockedReason::ReplacedUnexpected)
        ));
        // 新規の publishing: 宛先はまだ無く、incoming が残っている → 前のまま
        let got = decide(
            &progress(Stage::Publishing, false),
            &observed(D::Empty, I::Intact, R::Empty),
        );
        assert_eq!(
            got.action,
            Action::Abort {
                remove_incoming: Some(INC.into())
            }
        );
        // uploading: ID を記録する前に止まった（記録に incoming の ID が無い）。名前で見つけたものを消す
        let p = Progress {
            stage: Stage::Uploading,
            old_id: None,
            incoming_id: None,
        };
        let o = Observed {
            incoming: vec![dir("fidNEW")],
            ..Observed::default()
        };
        assert_eq!(
            decide(&p, &o).action,
            Action::Abort {
                remove_incoming: Some("fidNEW".into())
            }
        );
        // 何も作っていない
        assert_eq!(
            decide(&p, &Observed::default()).action,
            Action::Abort {
                remove_incoming: None
            }
        );
        // ID が無くても、incoming が 2 つ・ファイルなら進めない
        let o = Observed {
            incoming: vec![dir("fidA"), dir("fidB")],
            ..Observed::default()
        };
        assert!(matches!(decide(&p, &o).action, Action::Blocked(_)));
        // ID が無い記録では、宛先に別のフォルダがあれば公開済みとは見ない（incoming の ID が分からないので）
        let o = Observed {
            dest: vec![dir("fidA")],
            ..Observed::default()
        };
        assert!(matches!(
            decide(&p, &o).action,
            Action::Blocked(BlockedReason::DestinationUnexpected)
        ));
    }

    #[test]
    fn the_untouched_destination_of_a_replace_must_be_the_recorded_old_id() {
        // 旧フォルダの ID が記録と違う（別のフォルダに入れ替わった）→ 前のままとは言えない
        let p = progress(Stage::Uploading, true);
        let o = Observed {
            dest: vec![dir("fidOTHER")],
            ..Observed::default()
        };
        assert!(matches!(
            decide(&p, &o).action,
            Action::Blocked(BlockedReason::DestinationUnexpected)
        ));
    }

    #[test]
    fn decisions_are_describable_without_touching_drive() {
        let p = progress(Stage::Retiring, true);
        let o = observed(D::Empty, I::Intact, R::Old);
        let text = decide(&p, &o).describe(&p, &o);
        assert!(text.contains("retiring"), "{text}");
        assert!(text.contains(OLD) && text.contains(INC), "{text}");
        let o = observed(D::Other, I::Gone, R::Empty);
        let text = decide(&p, &o).describe(&p, &o);
        assert!(text.contains("recover --resolve"), "{text}");
        assert!(text.contains("destination_unexpected"), "{text}");
    }

    #[test]
    fn stage_names_round_trip() {
        for s in Stage::ALL {
            assert_eq!(Stage::parse(s.as_str()), Some(s));
        }
        assert_eq!(Stage::parse("done"), None);
    }
}
