//! T4: 復旧の表（design.md 4.2「復旧」）の実行。記録した段階と ID と、偽の Drive の状態から起動して、表のとおりに進むこと。
//! 起動時の復旧（T5）と同じく、手元に一式が無い（Drive の manifest で照合する）形で動かす。表そのもの（純粋な関数）の試験は
//! `src/recovery.rs`。

mod common;

use common::drive::model;
use common::drive::*;
use geoframe_export::drive::{Expected, Layout};
use geoframe_export::error::ErrorCode;
use geoframe_export::folder::FolderName;
use geoframe_export::publish::{Recovery, Resolution};
use geoframe_export::recovery::{Action, BlockedReason, Progress, Stage};
use geoframe_export::request::Destination;

const Y: Destination = Destination::Yamareco;
const PHOTOS: &[(&str, u64)] = &[("090000_1.jpg", 100), ("090100_2.jpg", 200)];

struct World {
    env: Env,
    set: Set,
    layout: Layout,
}

fn world(name: &str) -> World {
    let env = Env::new(name);
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let layout = env.layout(Y, FOLDER, JOB_A);
    World { env, set, layout }
}

impl World {
    fn recovery<'a>(
        &'a self,
        r: &'a geoframe_export::rclone::Rclone,
        expected: Option<&'a Expected>,
    ) -> Recovery<'a> {
        Recovery {
            rclone: r,
            layout: &self.layout,
            published: &self.env.published,
            expected,
            timing: fast_timing(),
        }
    }

    /// `incoming/<job_id>` に、検証済みの一式がある。
    fn put_incoming(&self) -> String {
        self.env
            .seed_set_at(&["geoframe-export-work", "incoming", JOB_A], &self.set)
    }

    /// 旧フォルダ（管理している）を `replaced/<…>` へ移した状態にする。
    fn retire(&self, old_id: &str) {
        let mut d = self.env.drive();
        let dst: Vec<String> = [
            "geoframe-export-work",
            "replaced",
            self.layout.retired_name(),
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert!(d.move_dir(old_id, &dst));
        self.env.save(&d);
    }

    /// `incoming` を宛先へ移した状態にする。
    fn publish_incoming(&self, incoming_id: &str) {
        let mut d = self.env.drive();
        assert!(d.move_dir(incoming_id, &["yamareco".to_owned(), FOLDER.to_owned()]));
        self.env.save(&d);
    }

    fn dest(&self) -> Option<String> {
        self.env.drive().resolve(&["yamareco", FOLDER])
    }

    fn incoming_exists(&self) -> bool {
        self.env
            .drive()
            .resolve(&["geoframe-export-work", "incoming", JOB_A])
            .is_some()
    }

    fn replaced_exists(&self) -> bool {
        self.env
            .drive()
            .resolve(&[
                "geoframe-export-work",
                "replaced",
                self.layout.retired_name(),
            ])
            .is_some()
    }

    fn recorded_id(&self) -> Option<String> {
        self.env
            .published
            .find_id(Y, &FolderName::parse(FOLDER).unwrap())
    }
}

/// 片付けて終えた（残りものは無い）。
fn interrupted() -> Resolution {
    Resolution::Interrupted {
        leftover_incoming: false,
        leftover_incoming_id: None,
    }
}

fn progress(stage: Stage, old: Option<&str>, incoming: Option<&str>) -> Progress {
    Progress {
        stage,
        old_id: old.map(str::to_owned),
        incoming_id: incoming.map(str::to_owned),
    }
}

#[test]
fn uploading_and_verifying_with_an_untouched_destination_remove_incoming_and_interrupt() {
    for stage in [Stage::Uploading, Stage::Verifying] {
        for replace in [false, true] {
            let w = world(&format!("row1-{stage}-{replace}"));
            let old = replace.then(|| w.env.seed_managed(Y, FOLDER, &w.set));
            let inc = w.put_incoming();
            let r = w.env.rclone();
            let res = w
                .recovery(&r, None)
                .run(
                    &progress(stage, old.as_deref(), Some(&inc)),
                    false,
                    &mut Hook::new(&w.env),
                )
                .unwrap();
            assert_eq!(res, interrupted(), "{stage} {replace}");
            assert!(!w.incoming_exists());
            assert_eq!(w.dest(), old, "宛先は前のまま");
            assert_eq!(w.env.drive().trashed, [inc], "incoming はゴミ箱へ");
            assert!(w.env.rclone_calls("moveto").is_empty());
        }
    }
}

#[test]
fn an_interrupted_upload_that_never_recorded_the_incoming_id_is_not_deleted_by_name() {
    // ID を記録する前に止まった: 名前だけでは消さず、残りものとして知らせる（宛先は変わっていないので blocked にはしない）
    let w = world("row1-noid");
    let inc = w.put_incoming();
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Uploading, None, None),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert_eq!(
        res,
        Resolution::Interrupted {
            leftover_incoming: true,
            leftover_incoming_id: Some(inc.clone()),
        }
    );
    assert!(w.incoming_exists(), "消していない");
    assert!(w.env.drive().trashed.is_empty() && w.env.drive().deleted.is_empty());
    assert!(w.env.mutations().is_empty());
    // 結果と recover の表示に出す
    let text = geoframe_export::publish::leftover_incoming_text(&w.layout, Some(&inc));
    assert!(
        text.contains(&inc) && text.contains(w.layout.incoming_path()),
        "{text}"
    );
    let (decision, _, shown) = w
        .recovery(&r, None)
        .explain(&progress(Stage::Uploading, None, None))
        .unwrap();
    assert!(matches!(
        decision.action,
        Action::KeepUnrecordedIncoming { .. }
    ));
    assert!(
        shown.contains(&inc) && shown.contains("消しません"),
        "{shown}"
    );

    // 何も作っていなかった
    let w = world("row1-nothing");
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Uploading, None, None),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert_eq!(res, interrupted());
    assert!(w.env.mutations().is_empty());
}

#[test]
fn the_incoming_is_deleted_only_when_it_matches_the_recorded_id() {
    // 記録した ID と、見えているフォルダの ID が違う → 消さずに blocked
    let w = world("row1-id-differs");
    let _inc = w.put_incoming();
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Verifying, None, Some("fidSOMEONEELSE")),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::IncomingUnexpected);
    assert!(w.env.mutations().is_empty());
    assert!(w.incoming_exists());
}

#[test]
fn retiring_before_the_move_is_interrupted_and_anything_in_replaced_blocks() {
    let w = world("row2-a");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert_eq!(res, interrupted());
    assert_eq!(w.dest().as_deref(), Some(old.as_str()));

    // replaced/ に何かある（旧フォルダはまだ宛先にあるのに）→ blocked。何も消さない・動かさない
    let w = world("row2-b");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.env.seed_set_at(
        &["geoframe-export-work", "replaced", w.layout.retired_name()],
        &w.set,
    );
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ReplacedUnexpected);
    assert!(w.env.mutations().is_empty());
    assert!(w.env.drive().trashed.is_empty());
}

#[test]
fn a_retired_old_folder_with_a_verified_incoming_is_rolled_forward() {
    for stage in [Stage::Retiring, Stage::Publishing] {
        let w = world(&format!("row3-{stage}"));
        let old = w.env.seed_managed(Y, FOLDER, &w.set);
        let inc = w.put_incoming();
        w.retire(&old);
        let r = w.env.rclone();
        let res = w
            .recovery(&r, None)
            .run(
                &progress(stage, Some(&old), Some(&inc)),
                false,
                &mut Hook::new(&w.env),
            )
            .unwrap();
        let Resolution::Published(p) = res else {
            panic!("{res:?}")
        };
        assert_eq!(p.folder_id, inc);
        assert!(p.replaced && p.recorded && p.old_left_in_replaced.is_none());
        assert_eq!(w.dest().as_deref(), Some(inc.as_str()));
        assert!(!w.incoming_exists() && !w.replaced_exists());
        assert_eq!(w.env.drive().trashed, [old]);
        assert_eq!(w.recorded_id().as_deref(), Some(inc.as_str()));
        // 前へ進める前に incoming の中身を manifest と照合し（cat）、移動は 1 回だけ
        assert!(w.env.commands_used().contains(&"cat".to_owned()));
        assert_eq!(w.env.rclone_calls("moveto").len(), 1);
        assert_safe_calls(&w.env);
    }
}

#[test]
fn rolling_forward_refuses_an_incoming_that_no_longer_matches_its_manifest() {
    let w = world("row3-mismatch");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    // incoming の写真が 1 枚消えている
    let mut d = w.env.drive();
    d.nodes
        .retain(|n| n.name != "090000_1.jpg" || n.parent != inc);
    w.env.save(&d);
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ContentMismatch);
    assert!(w.env.rclone_calls("moveto").is_empty());
    assert!(w.env.rclone_calls("purge").is_empty());
    assert!(
        w.dest().is_none(),
        "宛先は空のまま。旧フォルダは replaced/ にある"
    );
    assert!(w.replaced_exists());
}

#[test]
fn publishing_with_the_new_set_at_the_destination_is_recorded_as_published() {
    // 新規
    let w = world("row4-absent");
    let inc = w.put_incoming();
    w.publish_incoming(&inc);
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, None, Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    let Resolution::Published(p) = res else {
        panic!("{res:?}")
    };
    assert!(!p.replaced && p.recorded);
    assert_eq!(w.recorded_id().as_deref(), Some(inc.as_str()));
    assert!(
        w.env.mutations().is_empty(),
        "公開済みなので Drive は変えない"
    );

    // 置き換え: 旧フォルダのゴミ箱への移動を続ける
    let w = world("row4-replace");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    let Resolution::Published(p) = res else {
        panic!("{res:?}")
    };
    assert!(p.replaced && p.recorded && p.old_left_in_replaced.is_none());
    assert_eq!(w.env.drive().trashed, [old]);
    assert_eq!(w.recorded_id().as_deref(), Some(inc.as_str()));
}

#[test]
fn trashing_retries_while_the_old_folder_is_in_replaced_and_finishes_when_it_is_gone() {
    let w = world("row5-retry");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Trashing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.drive().trashed, std::slice::from_ref(&old));
    assert!(!w.replaced_exists());

    // 設計の表に文字どおりには無い行: ゴミ箱への移動は済んでいる（応答が失われた）→ もう一度は移さず、公開済み
    let w = world("row5-done");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    // 旧フォルダは宛先から退避され、ゴミ箱へも移されたあと（purge の応答だけが失われた）
    w.retire(&old);
    let mut d = w.env.drive();
    d.purge(&old, true);
    w.env.save(&d);
    w.publish_incoming(&inc);
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Trashing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert!(w.env.rclone_calls("purge").is_empty());
    assert_eq!(w.recorded_id().as_deref(), Some(inc.as_str()));
}

#[test]
fn states_the_table_does_not_cover_block_without_touching_anything() {
    // ID が見つからない（旧フォルダも incoming も無い）、宛先に別のものがある、など
    type Setup = Box<dyn Fn(&World) -> Progress>;
    let cases: Vec<(&str, Setup, BlockedReason)> = vec![
        (
            "nothing-found",
            Box::new(|w| {
                let old = w.env.seed_managed(Y, FOLDER, &w.set);
                let mut d = w.env.drive();
                d.nodes.retain(|n| n.id != old);
                w.env.save(&d);
                progress(Stage::Retiring, Some(&old), Some("fidGONE"))
            }),
            BlockedReason::StageInconsistent,
        ),
        (
            "dest-is-another-folder",
            Box::new(|w| {
                let old = w.env.seed_managed(Y, FOLDER, &w.set);
                let inc = w.put_incoming();
                w.retire(&old);
                // オーナーが同じ名前のフォルダを作った
                let mut d = w.env.drive();
                d.mkdir_p(&["yamareco", FOLDER]);
                w.env.save(&d);
                progress(Stage::Publishing, Some(&old), Some(&inc))
            }),
            BlockedReason::DestinationUnexpected,
        ),
        (
            "incoming-id-differs",
            Box::new(|w| {
                let old = w.env.seed_managed(Y, FOLDER, &w.set);
                let inc = w.put_incoming();
                w.retire(&old);
                progress(Stage::Publishing, Some(&old), Some(&format!("{inc}x")))
            }),
            BlockedReason::IncomingUnexpected,
        ),
        (
            "two-folders-at-dest",
            Box::new(|w| {
                let inc = w.put_incoming();
                let mut d = w.env.drive();
                d.mkdir_dup(&["yamareco"], FOLDER);
                d.mkdir_dup(&["yamareco"], FOLDER);
                w.env.save(&d);
                progress(Stage::Publishing, None, Some(&inc))
            }),
            BlockedReason::DestinationUnexpected,
        ),
        (
            "uploading-but-dest-has-the-new-set",
            Box::new(|w| {
                let inc = w.put_incoming();
                w.publish_incoming(&inc);
                progress(Stage::Uploading, None, Some(&inc))
            }),
            BlockedReason::StageInconsistent,
        ),
    ];
    for (name, setup, reason) in cases {
        let w = world(&format!("blocked-{name}"));
        let p = setup(&w);
        let nodes = w.env.drive().nodes.len();
        let r = w.env.rclone();
        let info = w
            .recovery(&r, None)
            .run(&p, false, &mut Hook::new(&w.env))
            .unwrap_err();
        assert_eq!(info.reason, reason, "{name}");
        assert_eq!(info.progress, p, "{name}");
        assert!(info.observed.is_some(), "{name}");
        assert!(
            w.env.mutations().is_empty(),
            "{name}: {:?}",
            w.env.mutations()
        );
        assert_eq!(w.env.drive().nodes.len(), nodes, "{name}");
        assert!(w.env.drive().trashed.is_empty(), "{name}");
    }
}

#[test]
fn an_unlistable_drive_is_unobservable_and_blocks() {
    let w = world("unobservable");
    let inc = w.put_incoming();
    w.env.add_rule("lsjson", 0, u64::MAX, fail(1));
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Verifying, None, Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::Unobservable);
    assert_eq!(info.observed, None);
    assert!(w.env.mutations().is_empty());
}

#[test]
fn explain_shows_the_state_and_the_next_move_without_changing_drive() {
    let w = world("explain");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let r = w.env.rclone();
    let p = progress(Stage::Publishing, Some(&old), Some(&inc));
    let (decision, observed, text) = w.recovery(&r, None).explain(&p).unwrap();
    assert!(matches!(decision.action, Action::RollForward { .. }));
    assert!(observed.dest.is_empty());
    assert!(text.contains(&old) && text.contains(&inc), "{text}");
    // 呼んだのは lsjson だけ
    assert_eq!(w.env.commands_used(), ["lsjson"]);
    assert!(w.env.mutations().is_empty());
    assert_eq!(w.dest(), None);
    // ブロックされる状態の説明
    let w2 = world("explain-blocked");
    let r2 = w2.env.rclone();
    let mut d = w2.env.drive();
    d.mkdir_p(&["yamareco", FOLDER]);
    w2.env.save(&d);
    let (decision, _, text) = w2
        .recovery(&r2, None)
        .explain(&progress(Stage::Retiring, Some("fidOLD"), Some("fidINC")))
        .unwrap();
    assert!(matches!(decision.action, Action::Blocked(_)));
    assert!(text.contains("recover --resolve"), "{text}");
}

#[test]
fn a_failed_move_during_recovery_blocks_instead_of_guessing() {
    let w = world("exec-fails");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("moveto", 0, u64::MAX, fail(1));
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ExecutionFailed);
    assert!(w.env.rclone_calls("purge").is_empty());
}

#[test]
fn the_pending_lookup_works_with_the_local_set_too() {
    // 公開の途中の失敗では、手元の一式（Expected）で照合する（cat を使わない）
    let w = world("local-expected");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let expected = Expected::from_publish_files(&w.set.files);
    let r = w.env.rclone();
    let res = w
        .recovery(&r, Some(&expected))
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    // 手元の一式で照合するので、incoming・宛先の manifest は読まない。読むのは、ゴミ箱へ移す前の、退避した旧フォルダの
    // manifest（旧 manifest の一式と中身の照合）だけ
    let cats = w.env.rclone_calls("cat");
    assert_eq!(cats.len(), 1, "{cats:?}");
    assert!(
        cats[0].last().unwrap().contains("/replaced/"),
        "{:?}",
        cats[0]
    );
    let _ = (ErrorCode::Blocked, model::ROOT);
}

// ---- 復旧にも段階の記録を掛ける（復旧の途中でまた止まっても、記録と Drive が食い違わない） ----

/// 次に起動したときの復旧: 最後に記録できた段階から、新しい口で動かす。
fn resume(
    w: &World,
    last: &Progress,
) -> Result<Resolution, Box<geoframe_export::publish::BlockedInfo>> {
    let r = w.env.rclone();
    w.recovery(&r, None)
        .run(last, false, &mut Hook::new(&w.env))
}

#[test]
fn recovery_records_publishing_before_it_moves_the_incoming_so_a_second_stop_resumes() {
    // retiring から復旧して、新フォルダを移した直後に止まる（trashing の記録が書けないことで、その場で止めたことにする）
    let w = world("restop-forward");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let r = w.env.rclone();
    let mut hook = Hook::new(&w.env).fail_at(Stage::Trashing, 1);
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut hook,
        )
        .unwrap();
    // 順番: 移す前に publishing を記録。trashing は書けなかったので、旧フォルダはゴミ箱へ移していない
    assert_eq!(
        w.env.mutations(),
        ["record:publishing", "moveto", "record:trashing"]
    );
    let Resolution::Published(p) = res else {
        panic!("{res:?}")
    };
    assert_eq!(p.folder_id, inc);
    assert_eq!(
        p.old_left_in_replaced.as_deref(),
        Some(old.as_str()),
        "旧フォルダが退避先にあると再確認できたので、残っていると知らせる"
    );
    assert!(w.replaced_exists() && w.env.drive().trashed.is_empty());
    // 記録は publishing まで進んでいる（retiring のままではない）
    let last = hook.records.last().cloned().unwrap();
    assert_eq!(last.stage, Stage::Publishing);
    assert_eq!(last.old_id.as_deref(), Some(old.as_str()));
    assert_eq!(last.incoming_id.as_deref(), Some(inc.as_str()));

    // 次の起動: 記録した publishing から、公開済みとして旧フォルダのゴミ箱への移動を続けて終わる
    let res = resume(&w, &last).unwrap();
    let Resolution::Published(p) = res else {
        panic!("{res:?}")
    };
    assert!(p.old_left_in_replaced.is_none() && p.recorded);
    assert_eq!(w.env.drive().trashed, [old]);
    assert!(!w.replaced_exists());
    assert_eq!(w.recorded_id().as_deref(), Some(inc.as_str()));
    // 記録の前に Drive を変えていない（trashing を記録してから purge）
    let seq = w.env.mutations();
    let rec = seq.iter().rposition(|c| c == "record:trashing").unwrap();
    let purge = seq.iter().position(|c| c == "purge").unwrap();
    assert!(rec < purge, "{seq:?}");
    assert_safe_calls(&w.env);
}

#[test]
fn recovery_records_trashing_before_the_purge_so_a_stop_after_the_purge_resumes() {
    // publishing のまま（記録は publishing）、公開済み・旧フォルダは退避先にある。復旧でゴミ箱へ移して止まる
    let w = world("restop-trash");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.rclone();
    let mut hook = Hook::new(&w.env);
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut hook,
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.mutations(), ["record:trashing", "purge"]);
    let last = hook.records.last().cloned().unwrap();
    assert_eq!(last.stage, Stage::Trashing);
    assert_eq!(w.env.drive().trashed, std::slice::from_ref(&old));

    // purge の直後に止まった、として、記録した trashing から再開する: 旧フォルダはもう無い → 公開済み。purge は呼ばない
    let res = resume(&w, &last).unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.rclone_calls("purge").len(), 1);
    // 記録が publishing のままだったら、旧フォルダが無いのは想定外として止まっていた（trashing を先に記録する理由）
    let stale = progress(Stage::Publishing, Some(&old), Some(&inc));
    let info = resume(&w, &stale).unwrap_err();
    assert_eq!(info.reason, BlockedReason::ReplacedUnexpected);
}

#[test]
fn a_record_that_cannot_be_written_stops_the_recovery_before_the_drive_operation() {
    // 移す前の publishing が書けない → 移さない。blocked（record_failed）、記録は retiring のまま
    let w = world("record-fails-forward");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let r = w.env.rclone();
    let p = progress(Stage::Retiring, Some(&old), Some(&inc));
    let info = w
        .recovery(&r, None)
        .run(
            &p,
            false,
            &mut Hook::new(&w.env).fail_at(Stage::Publishing, 1),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::RecordFailed);
    assert_eq!(info.progress, p, "最後に記録できた段階");
    assert!(w.env.rclone_calls("moveto").is_empty());
    assert!(w.dest().is_none() && w.replaced_exists());
    // 書けるようになれば、同じ記録から進められる
    assert!(matches!(resume(&w, &p), Ok(Resolution::Published(_))));

    // ゴミ箱へ移す前の trashing が書けない → purge しない。公開済みとして、旧フォルダが残っていると知らせる
    let w = world("record-fails-trash");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env).fail_at(Stage::Trashing, 1),
        )
        .unwrap();
    let Resolution::Published(p) = res else {
        panic!("{res:?}")
    };
    assert_eq!(p.old_left_in_replaced.as_deref(), Some(old.as_str()));
    assert!(w.env.rclone_calls("purge").is_empty());
}

#[test]
fn a_lost_move_response_during_recovery_waits_and_lists_twice_before_it_checks() {
    // 移動は失敗で終わるが、Drive 側では数回の一覧のあとに反映される。待って 2 回取り、同じになってから照合する
    let w = world("recovery-late");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("moveto", 0, 1, late_then_fail(3));
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.rclone_calls("moveto").len(), 1, "もう一度移さない");
    assert_eq!(w.dest().as_deref(), Some(inc.as_str()));
    // 応答が失われただけ（済んでいる）の場合
    let w = world("recovery-lost");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("moveto", 0, 1, do_then_fail());
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.rclone_calls("moveto").len(), 1);
    // 一覧が決まらなければ blocked
    let w = world("recovery-unstable");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("moveto", 0, 1, fail(1));
    let mut c = w.env.ctl();
    c.rules.push(model::Rule {
        cmd: "lsjson".into(),
        contains: None,
        // 移す前の確認（incoming・宛先）の一覧は通し、失敗のあとの一覧をすべて失敗させる
        skip: 12,
        times: u64::MAX,
        seen: 0,
        effect: fail(1),
    });
    c.save(&w.env.fake_dir);
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert!(
        matches!(
            info.reason,
            BlockedReason::Unobservable | BlockedReason::ExecutionFailed
        ),
        "{:?}",
        info.reason
    );
}

#[test]
fn startup_recovery_checks_the_destination_after_the_move_before_it_goes_to_the_trash() {
    // 起動時の復旧（手元に一式が無い）でも、移した直後に、宛先の ID・incoming が残っていない・中身を照合する
    // ファイル単位の移動に切り替わった → 宛先の ID が incoming の ID と違う
    let w = world("startup-filewise");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("moveto", 0, 1, filewise());
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::PublishMismatch);
    assert_eq!(info.progress.stage, Stage::Publishing);
    assert!(w.env.rclone_calls("purge").is_empty());
    assert!(w.replaced_exists());

    // 移したあとの中身が、宛先の manifest と合わない（移す直前に写真が 1 枚消えた）→ ゴミ箱へ進まない
    let w = world("startup-content");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let fake = w.env.fake_dir.clone();
    let inc2 = inc.clone();
    let mut hook = Hook::new(&w.env).mutate_after(Stage::Publishing, move || {
        let mut d = model::Drive::load(&fake);
        d.nodes
            .retain(|n| n.name != "090000_1.jpg" || n.parent != inc2);
        d.save(&fake);
    });
    let r = w.env.rclone();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut hook,
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::PublishMismatch);
    assert!(w.env.rclone_calls("purge").is_empty());
}

#[test]
fn the_old_folder_is_not_trashed_when_its_contents_changed_while_the_service_was_down() {
    // 停止中に、オーナーが退避先の旧フォルダにメモを足した → ファイルごとゴミ箱へ移さない
    for (name, expected) in [("startup", false), ("local", true)] {
        let w = world(&format!("trash-content-{name}"));
        let old = w.env.seed_managed(Y, FOLDER, &w.set);
        let inc = w.put_incoming();
        w.retire(&old);
        w.publish_incoming(&inc);
        let mut d = w.env.drive();
        d.add_file(
            &["geoframe-export-work", "replaced", w.layout.retired_name()],
            "owner-memo.txt",
            7,
            None,
        );
        w.env.save(&d);
        let set_expected = Expected::from_publish_files(&w.set.files);
        let r = w.env.rclone();
        let info = w
            .recovery(&r, expected.then_some(&set_expected))
            .run(
                &progress(Stage::Publishing, Some(&old), Some(&inc)),
                false,
                &mut Hook::new(&w.env),
            )
            .unwrap_err();
        assert_eq!(info.reason, BlockedReason::RetiredContentChanged, "{name}");
        assert!(w.env.rclone_calls("purge").is_empty(), "{name}");
        assert!(w.env.drive().trashed.is_empty(), "{name}");
        assert!(w.replaced_exists(), "{name}");
        let o = info.observed.clone().expect("見えていた状態");
        assert_eq!(o.replaced, [geoframe_export::recovery::Seen::dir(&old)]);
        let text = info.describe(&w.layout);
        assert!(
            text.contains("retired_content_changed") && text.contains("取り出して"),
            "{text}"
        );
        // 公開は済んでいる（宛先は新しい一式のまま）
        assert_eq!(w.dest().as_deref(), Some(inc.as_str()));
    }
}
