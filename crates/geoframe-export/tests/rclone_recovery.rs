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
                .run(&progress(stage, old.as_deref(), Some(&inc)), false)
                .unwrap();
            assert_eq!(
                res,
                Resolution::Interrupted {
                    leftover_incoming: false
                },
                "{stage} {replace}"
            );
            assert!(!w.incoming_exists());
            assert_eq!(w.dest(), old, "宛先は前のまま");
            assert_eq!(w.env.drive().trashed, [inc], "incoming はゴミ箱へ");
            assert!(w.env.rclone_calls("moveto").is_empty());
        }
    }
}

#[test]
fn an_interrupted_upload_that_never_recorded_the_incoming_id_is_cleaned_by_name() {
    let w = world("row1-noid");
    let inc = w.put_incoming();
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(&progress(Stage::Uploading, None, None), false)
        .unwrap();
    assert_eq!(
        res,
        Resolution::Interrupted {
            leftover_incoming: false
        }
    );
    assert_eq!(w.env.drive().trashed, [inc]);
    // 何も作っていなかった
    let w = world("row1-nothing");
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(&progress(Stage::Uploading, None, None), false)
        .unwrap();
    assert_eq!(
        res,
        Resolution::Interrupted {
            leftover_incoming: false
        }
    );
    assert!(w.env.mutations().is_empty());
}

#[test]
fn retiring_before_the_move_is_interrupted_and_anything_in_replaced_blocks() {
    let w = world("row2-a");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    let r = w.env.rclone();
    let res = w
        .recovery(&r, None)
        .run(&progress(Stage::Retiring, Some(&old), Some(&inc)), false)
        .unwrap();
    assert_eq!(
        res,
        Resolution::Interrupted {
            leftover_incoming: false
        }
    );
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
        .run(&progress(Stage::Retiring, Some(&old), Some(&inc)), false)
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
            .run(&progress(stage, Some(&old), Some(&inc)), false)
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
        .run(&progress(Stage::Publishing, Some(&old), Some(&inc)), false)
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
        .run(&progress(Stage::Publishing, None, Some(&inc)), false)
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
        .run(&progress(Stage::Publishing, Some(&old), Some(&inc)), false)
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
        .run(&progress(Stage::Trashing, Some(&old), Some(&inc)), false)
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
        .run(&progress(Stage::Trashing, Some(&old), Some(&inc)), false)
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
        let info = w.recovery(&r, None).run(&p, false).unwrap_err();
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
        .run(&progress(Stage::Verifying, None, Some(&inc)), false)
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
        .run(&progress(Stage::Publishing, Some(&old), Some(&inc)), false)
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
        .run(&progress(Stage::Retiring, Some(&old), Some(&inc)), false)
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert!(!w.env.commands_used().contains(&"cat".to_owned()));
    let _ = (ErrorCode::Blocked, model::ROOT);
}
