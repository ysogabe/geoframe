//! T4: 復旧の表（design.md 4.2「復旧」）の実行。記録した段階と ID と、偽の Drive の状態から起動して、表のとおりに進むこと。
//! 起動時の復旧（T5）と同じく、手元に一式が無い（Drive の manifest で照合する）形で動かす。表そのもの（純粋な関数）の試験は
//! `src/recovery.rs`。
//!
//! Drive の操作の記録は、中立の操作名（`list`・`read`・`create_folder`・`upload`・`move`・`trash`）で見る
//! （`Env::ops`・`mutating_ops`・`ops_used`・`op_count`）。`// rclone 固有` と書いた確認（`rclone_calls`・
//! `assert_rclone_safe_calls`）と、そう書いた試験だけが、rclone の引数・環境変数を見る。それらは rclone 版を消すときに消す。

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
        r: &'a geoframe_export::remote::Remote,
        expected: Option<&'a Expected>,
    ) -> Recovery<'a> {
        Recovery {
            remote: r,
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
        digests: None,
    }
}

#[test]
fn uploading_and_verifying_with_an_untouched_destination_remove_incoming_and_interrupt() {
    for stage in [Stage::Uploading, Stage::Verifying] {
        for replace in [false, true] {
            let w = world(&format!("row1-{stage}-{replace}"));
            let old = replace.then(|| w.env.seed_managed(Y, FOLDER, &w.set));
            let inc = w.put_incoming();
            let r = w.env.remote();
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
            assert!(w.env.op_count("move") == 0);
        }
    }
}

#[test]
fn an_interrupted_upload_that_never_recorded_the_incoming_id_is_not_deleted_by_name() {
    // ID を記録する前に止まった: 名前だけでは消さず、残りものとして知らせる（宛先は変わっていないので blocked にはしない）
    let w = world("row1-noid");
    let inc = w.put_incoming();
    let r = w.env.remote();
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
    w.env.assert_no_mutations();
    // 結果と recover の表示に出す
    let text = geoframe_export::publish::leftover_incoming_text(&w.layout, Some(&inc));
    assert!(
        text.contains(&inc) && text.contains(&w.layout.incoming_path()),
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
    let r = w.env.remote();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Uploading, None, None),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert_eq!(res, interrupted());
    w.env.assert_no_mutations();
}

#[test]
fn the_incoming_is_deleted_only_when_it_matches_the_recorded_id() {
    // 記録した ID と、見えているフォルダの ID が違う → 消さずに blocked
    let w = world("row1-id-differs");
    let _inc = w.put_incoming();
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Verifying, None, Some("fidSOMEONEELSE")),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::IncomingUnexpected);
    w.env.assert_no_mutations();
    assert!(w.incoming_exists());
}

#[test]
fn retiring_before_the_move_is_interrupted_and_anything_in_replaced_blocks() {
    let w = world("row2-a");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    let r = w.env.remote();
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
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ReplacedUnexpected);
    w.env.assert_no_mutations();
    assert!(w.env.drive().trashed.is_empty());
}

#[test]
fn a_retired_old_folder_with_a_verified_incoming_is_rolled_forward() {
    for stage in [Stage::Retiring, Stage::Publishing] {
        let w = world(&format!("row3-{stage}"));
        let old = w.env.seed_managed(Y, FOLDER, &w.set);
        let inc = w.put_incoming();
        w.retire(&old);
        let r = w.env.remote();
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
        assert!(w.env.ops_used().contains(&"read".to_owned()));
        assert_eq!(w.env.op_count("move"), 1);
        // rclone 固有: rclone の引数・環境変数を確かめる（rclone 版を消すときに、この確認も消す）。
        assert_rclone_safe_calls(&w.env);
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
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ContentMismatch);
    assert!(w.env.op_count("move") == 0);
    assert!(w.env.op_count("trash") == 0);
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
    let r = w.env.remote();
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
        w.env.mutating_ops().is_empty(),
        "公開済みなので Drive は変えない"
    );

    // 置き換え: 旧フォルダのゴミ箱への移動を続ける
    let w = world("row4-replace");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.remote();
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
    let r = w.env.remote();
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
    let r = w.env.remote();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Trashing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert!(w.env.op_count("trash") == 0);
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
        let r = w.env.remote();
        let info = w
            .recovery(&r, None)
            .run(&p, false, &mut Hook::new(&w.env))
            .unwrap_err();
        assert_eq!(info.reason, reason, "{name}");
        assert_eq!(info.progress, p, "{name}");
        assert!(info.observed.is_some(), "{name}");
        assert!(
            w.env.mutating_ops().is_empty(),
            "{name}: {:?}",
            w.env.mutating_ops()
        );
        assert_eq!(w.env.drive().nodes.len(), nodes, "{name}");
        assert!(w.env.drive().trashed.is_empty(), "{name}");
    }
}

#[test]
fn an_unlistable_drive_is_unobservable_and_blocks() {
    let w = world("unobservable");
    let inc = w.put_incoming();
    w.env.add_rule("list", 0, u64::MAX, fail(1));
    let r = w.env.remote();
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
    w.env.assert_no_mutations();
}

#[test]
fn explain_shows_the_state_and_the_next_move_without_changing_drive() {
    let w = world("explain");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let r = w.env.remote();
    let p = progress(Stage::Publishing, Some(&old), Some(&inc));
    let (decision, observed, text) = w.recovery(&r, None).explain(&p).unwrap();
    assert!(matches!(decision.action, Action::RollForward { .. }));
    assert!(observed.dest.is_empty());
    assert!(text.contains(&old) && text.contains(&inc), "{text}");
    // 呼んだのは lsjson だけ
    assert_eq!(w.env.ops_used(), ["list"]);
    w.env.assert_no_mutations();
    assert_eq!(w.dest(), None);
    // ブロックされる状態の説明
    let w2 = world("explain-blocked");
    let r2 = w2.env.remote();
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
    w.env.add_rule("move", 0, u64::MAX, fail(1));
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ExecutionFailed);
    assert!(w.env.op_count("trash") == 0);
}

#[test]
fn the_pending_lookup_works_with_the_local_set_too() {
    // 公開の途中の失敗では、手元の一式（Expected）で照合する（cat を使わない）
    let w = world("local-expected");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    let expected = Expected::from_publish_files(&w.set.files);
    let r = w.env.remote();
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
    // rclone 固有: rclone の引数・環境変数を確かめる（rclone 版を消すときに、この確認も消す）。
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
    let r = w.env.remote();
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
    let r = w.env.remote();
    let mut hook = Hook::new(&w.env).fail_at(Stage::Trashing, 1);
    let res = w
        .recovery(&r, None)
        .run(
            // ジョブの記録には、退避の前に検証済みの MD5 が書いてある
            &Progress {
                digests: Some(w.set.recorded()),
                ..progress(Stage::Retiring, Some(&old), Some(&inc))
            },
            false,
            &mut hook,
        )
        .unwrap();
    // 順番: 移す前に publishing を記録。trashing は書けなかったので、旧フォルダはゴミ箱へ移していない
    assert_eq!(
        w.env.mutating_ops(),
        ["record:publishing", "move", "record:trashing"]
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
    // 旧フォルダが退避先に残るので、新しい行に旧フォルダの ID と MD5 を `previous` として残す（次の復旧がゴミ箱へ移す前に
    // 中身を照らすため。新しい行に置き換わっても旧 MD5 は失われない）
    let folder = FolderName::parse(FOLDER).unwrap();
    let row = w.env.published.find(Y, &folder).expect("row");
    assert_eq!(row.folder_id, inc);
    let previous = row.previous.expect("previous");
    assert_eq!(previous.folder_id, old);
    assert_eq!(previous.files, w.set.recorded());
    // 起動時の復旧（手元の一式なし）が書く行の MD5 は、ジョブの記録に残した検証済みの MD5（宛先の今の MD5 から作り直さない）
    assert_eq!(row.files, Some(w.set.recorded()));
    // 記録は publishing まで進んでいる（retiring のままではない）
    let last = hook.records.last().cloned().unwrap();
    assert_eq!(last.stage, Stage::Publishing);
    // 記録する段階には、検証済みの MD5 が引き継がれる
    assert_eq!(last.digests, Some(w.set.recorded()));
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
    // ゴミ箱へ移せたので、`previous` は残らない
    assert!(w.env.published.find(Y, &folder).unwrap().previous.is_none());
    // 記録の前に Drive を変えていない（trashing を記録してから purge）
    let seq = w.env.mutating_ops();
    let rec = seq.iter().rposition(|c| c == "record:trashing").unwrap();
    let purge = seq.iter().position(|c| c == "trash").unwrap();
    assert!(rec < purge, "{seq:?}");
    // rclone 固有: rclone の引数・環境変数を確かめる（rclone 版を消すときに、この確認も消す）。
    assert_rclone_safe_calls(&w.env);
}

#[test]
fn recovery_records_trashing_before_the_purge_so_a_stop_after_the_purge_resumes() {
    // publishing のまま（記録は publishing）、公開済み・旧フォルダは退避先にある。復旧でゴミ箱へ移して止まる
    let w = world("restop-trash");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.remote();
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
    assert_eq!(w.env.mutating_ops(), ["record:trashing", "trash"]);
    let last = hook.records.last().cloned().unwrap();
    assert_eq!(last.stage, Stage::Trashing);
    assert_eq!(w.env.drive().trashed, std::slice::from_ref(&old));

    // purge の直後に止まった、として、記録した trashing から再開する: 旧フォルダはもう無い → 公開済み。purge は呼ばない
    let res = resume(&w, &last).unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.op_count("trash"), 1);
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
    let r = w.env.remote();
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
    assert!(w.env.op_count("move") == 0);
    assert!(w.dest().is_none() && w.replaced_exists());
    // 書けるようになれば、同じ記録から進められる
    assert!(matches!(resume(&w, &p), Ok(Resolution::Published(_))));

    // ゴミ箱へ移す前の trashing が書けない → purge しない。公開済みとして、旧フォルダが残っていると知らせる
    let w = world("record-fails-trash");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    let r = w.env.remote();
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
    assert!(w.env.op_count("trash") == 0);
}

#[test]
fn a_lost_move_response_during_recovery_waits_and_lists_twice_before_it_checks() {
    // 移動は失敗で終わるが、Drive 側では数回の一覧のあとに反映される。待って 2 回取り、同じになってから照合する
    let w = world("recovery-late");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("move", 0, 1, late_then_fail(3));
    let r = w.env.remote();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.op_count("move"), 1, "もう一度移さない");
    assert_eq!(w.dest().as_deref(), Some(inc.as_str()));
    // 応答が失われただけ（済んでいる）の場合
    let w = world("recovery-lost");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("move", 0, 1, do_then_fail());
    let r = w.env.remote();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    assert_eq!(w.env.op_count("move"), 1);
    // 一覧が決まらなければ blocked（unobservable）。移す前までの lsjson の回数は、同じ状況をもう 1 回動かして数える
    let probe = world("recovery-unstable-probe");
    let old = probe.env.seed_managed(Y, FOLDER, &probe.set);
    let inc = probe.put_incoming();
    probe.retire(&old);
    probe.env.add_rule("move", 0, 1, fail(1));
    let r = probe.env.remote();
    let _ = probe.recovery(&r, None).run(
        &progress(Stage::Publishing, Some(&old), Some(&inc)),
        false,
        &mut Hook::new(&probe.env),
    );
    let before_move = probe
        .env
        .calls()
        .iter()
        .take_while(|c| c["op"] != "move")
        .filter(|c| c["op"] == "list")
        .count() as u64;
    assert!(before_move > 0);

    let w = world("recovery-unstable");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("move", 0, 1, fail(1));
    let mut c = w.env.ctl();
    c.rules.push(model::Rule {
        op: "list".into(),
        contains: None,
        // 移す前の一覧は通し、失敗のあとの一覧をすべて失敗させる
        skip: before_move,
        times: u64::MAX,
        seen: 0,
        effect: fail(1),
    });
    c.save(&w.env.fake_dir);
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::Unobservable);
}

#[test]
fn startup_recovery_checks_the_destination_after_the_move_before_it_goes_to_the_trash() {
    // 起動時の復旧（手元に一式が無い）でも、移した直後に、宛先の ID・incoming が残っていない・中身を照合する
    // ファイル単位の移動に切り替わった → 宛先の ID が incoming の ID と違う
    let w = world("startup-filewise");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.env.add_rule("move", 0, 1, filewise());
    let r = w.env.remote();
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
    assert!(w.env.op_count("trash") == 0);
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
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Retiring, Some(&old), Some(&inc)),
            false,
            &mut hook,
        )
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::PublishMismatch);
    assert!(w.env.op_count("trash") == 0);
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
        let r = w.env.remote();
        let info = w
            .recovery(&r, expected.then_some(&set_expected))
            .run(
                &progress(Stage::Publishing, Some(&old), Some(&inc)),
                false,
                &mut Hook::new(&w.env),
            )
            .unwrap_err();
        assert_eq!(info.reason, BlockedReason::RetiredContentChanged, "{name}");
        assert!(w.env.op_count("trash") == 0, "{name}");
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

#[test]
fn the_old_folder_is_not_trashed_when_a_file_was_rewritten_to_the_same_size_while_the_service_was_down()
 {
    // 停止中に、退避先の旧フォルダの写真が、同じ名前・同じ大きさのまま書き換えられた（旧フォルダの MD5 は公開の記録のまま）
    // 旧フォルダの MD5 が確かめられないとき（記録に旧 ID の行も `previous` も無い・MD5 が無い）も、移さない
    for (name, expected, case) in [
        ("startup-rewritten", false, "rewritten"),
        ("local-rewritten", true, "rewritten"),
        ("no-md5", false, "no-md5"),
        ("other-id", false, "other-id"),
    ] {
        let w = world(&format!("trash-md5-{name}"));
        let old = w.env.seed_managed(Y, FOLDER, &w.set);
        let inc = w.put_incoming();
        w.retire(&old);
        w.publish_incoming(&inc);
        let folder = FolderName::parse(FOLDER).unwrap();
        match case {
            "rewritten" => {
                let mut d = w.env.drive();
                let dir = d
                    .resolve(&["geoframe-export-work", "replaced", w.layout.retired_name()])
                    .unwrap();
                let id = d
                    .children(&dir)
                    .iter()
                    .find(|n| n.name == "090000_1.jpg")
                    .unwrap()
                    .id
                    .clone();
                d.set_md5(&id, &"f".repeat(32));
                w.env.save(&d);
            }
            // 記録の旧 ID の行に MD5 が無い（MD5 を記録する前の古い形）
            "no-md5" => {
                let mut row = w.env.published.find(Y, &folder).unwrap();
                row.files = None;
                w.env.published.upsert(row).unwrap();
            }
            // 記録の行が別のフォルダの ID で、`previous` も無い
            _ => {
                let mut row = w.env.published.find(Y, &folder).unwrap();
                row.folder_id = "fidOTHER".into();
                w.env.published.upsert(row).unwrap();
            }
        }
        let set_expected = Expected::from_publish_files(&w.set.files);
        let r = w.env.remote();
        let info = w
            .recovery(&r, expected.then_some(&set_expected))
            .run(
                &progress(Stage::Publishing, Some(&old), Some(&inc)),
                false,
                &mut Hook::new(&w.env),
            )
            .unwrap_err();
        assert_eq!(info.reason, BlockedReason::RetiredContentChanged, "{name}");
        assert!(w.env.op_count("trash") == 0, "{name}");
        assert!(w.env.drive().trashed.is_empty(), "{name}");
        assert!(w.replaced_exists(), "{name}");
        assert_eq!(w.dest().as_deref(), Some(inc.as_str()), "{name}");
    }
}

// ---- 中止の片付け（aborting）と、記録の失敗時の再確認の順序 ----

#[test]
fn recovery_records_aborting_before_it_trashes_the_incoming_of_a_new_publish() {
    // 新規の publishing で、宛先はまだ無く、incoming が残っている → 前のまま。ゴミ箱へ移す前に aborting を記録する
    let w = world("abort-from-publishing");
    let inc = w.put_incoming();
    let r = w.env.remote();
    let mut hook = Hook::new(&w.env);
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, None, Some(&inc)),
            false,
            &mut hook,
        )
        .unwrap();
    assert_eq!(res, interrupted());
    assert_eq!(w.env.mutating_ops(), ["record:aborting", "trash"]);
    let last = hook.records.last().cloned().unwrap();
    assert_eq!(last, progress(Stage::Aborting, None, Some(&inc)));
    assert!(!w.incoming_exists());

    // purge の直後にまた止まった、として、記録した aborting から再開する: incoming は無い → 片付け済みで interrupted
    let res = resume(&w, &last).unwrap();
    assert_eq!(res, interrupted());
    assert_eq!(w.env.op_count("trash"), 1, "もう消さない");
    // 記録が publishing のままなら、あるはずのものが無い状態として blocked だった（aborting を先に記録する理由）
    let stale = progress(Stage::Publishing, None, Some(&inc));
    let info = resume(&w, &stale).unwrap_err();
    assert_eq!(info.reason, BlockedReason::StageInconsistent);
}

#[test]
fn recovery_does_not_trash_the_incoming_when_aborting_cannot_be_recorded() {
    let w = world("abort-record-fails");
    let inc = w.put_incoming();
    let r = w.env.remote();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, None, Some(&inc)),
            false,
            &mut Hook::new(&w.env).fail_at(Stage::Aborting, 1),
        )
        .unwrap();
    assert_eq!(
        res,
        Resolution::Interrupted {
            leftover_incoming: true,
            leftover_incoming_id: Some(inc.clone()),
        }
    );
    assert!(w.env.op_count("trash") == 0);
    assert!(w.incoming_exists());
}

#[test]
fn the_aborting_row_finishes_the_cleanup_or_blocks() {
    // incoming が記録の ID で残っている（記録済みの aborting から。再度は記録しない）→ 消して interrupted
    let w = world("aborting-intact");
    let inc = w.put_incoming();
    let r = w.env.remote();
    let mut hook = Hook::new(&w.env);
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Aborting, None, Some(&inc)),
            false,
            &mut hook,
        )
        .unwrap();
    assert_eq!(res, interrupted());
    assert!(
        hook.records.is_empty(),
        "すでに aborting なので、書き直さない"
    );
    assert_eq!(w.env.drive().trashed, std::slice::from_ref(&inc));

    // もう無い → 片付け済みとして interrupted（置き換えでも同じ。旧フォルダは宛先のまま）
    let w = world("aborting-gone");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let res = resume(&w, &progress(Stage::Aborting, Some(&old), Some("fidGONE"))).unwrap();
    assert_eq!(res, interrupted());
    w.env.assert_no_mutations();
    assert_eq!(w.dest().as_deref(), Some(old.as_str()));

    // 宛先が前のままでない（別のフォルダがある・新しい一式になっている）→ blocked。何も動かさない
    let w = world("aborting-dest-changed");
    let inc = w.put_incoming();
    w.env.seed_set_at(&["yamareco", FOLDER], &w.set);
    let info = resume(&w, &progress(Stage::Aborting, None, Some(&inc))).unwrap_err();
    assert_eq!(info.reason, BlockedReason::DestinationUnexpected);
    w.env.assert_no_mutations();
    assert!(w.incoming_exists());

    let w = world("aborting-published");
    let inc = w.put_incoming();
    w.publish_incoming(&inc);
    let info = resume(&w, &progress(Stage::Aborting, None, Some(&inc))).unwrap_err();
    assert_eq!(info.reason, BlockedReason::StageInconsistent);
    w.env.assert_no_mutations();

    // incoming が記録の ID と違う → 消さずに blocked
    let w = world("aborting-other-incoming");
    let _inc = w.put_incoming();
    let info = resume(&w, &progress(Stage::Aborting, None, Some("fidSOMEONEELSE"))).unwrap_err();
    assert_eq!(info.reason, BlockedReason::IncomingUnexpected);
    w.env.assert_no_mutations();
    // 表示にも出る（recover）
    let (_, _, text) = w
        .recovery(&w.env.remote(), None)
        .explain(&progress(Stage::Aborting, None, Some("fidSOMEONEELSE")))
        .unwrap();
    assert!(text.contains("aborting"), "{text}");
}

/// ゴミ箱へ移す前の `trashing` が書けず、その書き込みの間に退避先の状態が変わった（記録の口の中で Drive を書き換える）。
/// 先に公開の記録を更新していると、`blocked` なのに公開の記録だけ更新済みになる。
fn trashing_record_fails_while_replaced_changes(
    name: &str,
    change: &dyn Fn(&std::path::Path, &str),
) {
    let w = world(name);
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    assert_eq!(w.recorded_id().as_deref(), Some(old.as_str()));
    let fake = w.env.fake_dir.clone();
    let retired = w.layout.retired_name().to_owned();
    let mut sink = |p: &Progress| -> Result<(), geoframe_export::error::ExportError> {
        if p.stage == Stage::Trashing {
            change(&fake, &retired);
            return Err(geoframe_export::error::ExportError::new(
                ErrorCode::Internal,
                "ジョブの記録を書けません（試験）",
            ));
        }
        Ok(())
    };
    let r = w.env.remote();
    let info = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Publishing, Some(&old), Some(&inc)),
            false,
            &mut sink,
        )
        .unwrap_err();
    // 旧フォルダが退避先にあると確かめられないので blocked にし、公開の記録は更新しない（旧い行のまま）
    assert!(
        matches!(
            info.reason,
            BlockedReason::TrashMismatch | BlockedReason::Unobservable
        ),
        "{:?}",
        info.reason
    );
    assert_eq!(
        w.recorded_id().as_deref(),
        Some(old.as_str()),
        "blocked なのに公開の記録だけ更新済みになっていない"
    );
    assert!(w.env.op_count("trash") == 0);
}

#[test]
fn a_trashing_record_failure_rechecks_the_retired_folder_before_the_published_record_is_updated() {
    // 退避先の項目が、別の ID のフォルダに入れ替わった → trash_mismatch
    trashing_record_fails_while_replaced_changes("trash-record-swap", &|fake, retired| {
        let mut d = model::Drive::load(fake);
        let id = d
            .resolve(&["geoframe-export-work", "replaced", retired])
            .unwrap();
        d.purge(&id, false);
        d.mkdir_p(&["geoframe-export-work", "replaced", retired]);
        d.save(fake);
    });
    // 退避先から旧フォルダが消えた（説明できない消失）→ 公開の記録を更新せず blocked（再起動の復旧と食い違わない）
    trashing_record_fails_while_replaced_changes("trash-record-vanished", &|fake, retired| {
        let mut d = model::Drive::load(fake);
        let id = d
            .resolve(&["geoframe-export-work", "replaced", retired])
            .unwrap();
        d.purge(&id, false);
        d.save(fake);
    });
    // 退避先を一覧できなくなった → unobservable
    trashing_record_fails_while_replaced_changes("trash-record-unlistable", &|fake, _| {
        let mut c = model::Ctl::load(fake);
        c.rules.push(model::Rule {
            op: "list".into(),
            contains: Some("/replaced".into()),
            skip: 0,
            times: u64::MAX,
            seen: 0,
            effect: fail(1),
        });
        c.save(fake);
    });
}

#[test]
fn a_purge_that_fails_during_recovery_keeps_the_old_folder_in_the_result() {
    // trashing は書けたが、purge が失敗する → 公開済みで、旧フォルダが退避先に残っている（結果が None に上書きされない）
    let w = world("recovery-purge-fails");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    w.env.add_rule("trash", 0, u64::MAX, fail(1));
    let r = w.env.remote();
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
    assert_eq!(p.old_left_in_replaced.as_deref(), Some(old.as_str()));
    assert!(p.recorded);
    assert_eq!(w.recorded_id().as_deref(), Some(inc.as_str()));
    assert!(w.replaced_exists());
}

// ---- 起動時の復旧は、ジョブの記録に残した検証済みの MD5 で宛先の中身を照らす ----

/// `parent` の直下の `name` のファイルの MD5 を、別の値にする（同じ名前・同じ大きさのまま中身を書き換えた、を作る）。
fn change_md5(env: &Env, parent: &[&str], name: &str) {
    let mut d = env.drive();
    let p = d.resolve(parent).expect("parent");
    let ids: Vec<String> = d
        .children(&p)
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.id.clone())
        .collect();
    assert_eq!(ids.len(), 1, "{name}");
    d.set_md5(&ids[0], &"f".repeat(32));
    env.save(&d);
}

fn row_file(w: &World) -> std::path::PathBuf {
    w.env
        .published
        .path_of(Y, &FolderName::parse(FOLDER).unwrap())
}

/// 置き換えで、旧フォルダを退避し、新しい一式を宛先へ移した状態（記録は publishing 以降）。
fn replaced_world(name: &str) -> (World, String, String) {
    let w = world(name);
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    w.publish_incoming(&inc);
    (w, old, inc)
}

#[test]
fn a_rewrite_after_the_job_record_stopped_is_blocked_and_the_published_record_is_not_overwritten() {
    // 公開の記録を書く前（ジョブの記録は publishing）に止まり、止まっている間に宛先の写真が、同じ名前・同じ大きさのまま
    // 書き換えられた。宛先の今の MD5 を正規の一式として登録せず、blocked にして、公開の記録（旧フォルダの行）を変えない
    let (w, old, inc) = replaced_world("digest-publishing");
    let before = std::fs::read(row_file(&w)).unwrap();
    change_md5(&w.env, &["yamareco", FOLDER], "090000_1.jpg");
    let r = w.env.remote();
    let kept = Progress {
        digests: Some(w.set.recorded()),
        ..progress(Stage::Publishing, Some(&old), Some(&inc))
    };
    let info = w
        .recovery(&r, None)
        .run(&kept, false, &mut Hook::new(&w.env))
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ContentMismatch);
    assert_eq!(
        std::fs::read(row_file(&w)).unwrap(),
        before,
        "記録を上書きしない"
    );
    assert_eq!(w.recorded_id().as_deref(), Some(old.as_str()));
    // 何も動かしていない（trashing の記録も purge もない）
    w.env.assert_no_mutations();
    assert!(w.env.op_count("trash") == 0);
    assert!(w.replaced_exists() && w.env.drive().trashed.is_empty());

    // 書き換えられていなければ、同じ記録で公開済みとして終わり、行にはジョブの記録の MD5 が書かれる
    let (w, old, inc) = replaced_world("digest-publishing-clean");
    let r = w.env.remote();
    let kept = Progress {
        digests: Some(w.set.recorded()),
        ..progress(Stage::Publishing, Some(&old), Some(&inc))
    };
    let res = w
        .recovery(&r, None)
        .run(&kept, false, &mut Hook::new(&w.env))
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    let row = w
        .env
        .published
        .find(Y, &FolderName::parse(FOLDER).unwrap())
        .unwrap();
    assert_eq!(row.folder_id, inc);
    assert_eq!(row.files, Some(w.set.recorded()));
    assert_eq!(w.env.drive().trashed, [old]);
}

#[test]
fn a_rewrite_right_after_the_published_record_was_updated_is_blocked_by_the_recorded_md5() {
    // 公開の記録を更新した直後・ジョブの完了の前に止まった（ジョブの記録は trashing）。行は新しいフォルダの ID と MD5 を
    // 持っている。止まっている間の書き換えは、その行の MD5 で見つけて blocked にする（行を宛先の今の MD5 で上書きしない。
    // 次の `replace` が、手作業で変えたフォルダをゴミ箱へ移すことになる）
    for (name, kept_digests) in [("with-job-digests", true), ("without-job-digests", false)] {
        let (w, old, inc) = replaced_world(&format!("digest-row-{name}"));
        // 公開の記録は、新しいフォルダの行になっている（旧フォルダは previous に残る）
        w.env
            .published
            .upsert(geoframe_export::published::PublishedRecord {
                destination: Y,
                folder: FolderName::parse(FOLDER).unwrap(),
                folder_id: inc.clone(),
                job_id: geoframe_export::jobid::JobId::parse(JOB_A).unwrap(),
                files: Some(w.set.recorded()),
                previous: Some(Box::new(geoframe_export::published::PreviousFolder {
                    folder_id: old.clone(),
                    files: w.set.recorded(),
                })),
            })
            .unwrap();
        let before = std::fs::read(row_file(&w)).unwrap();
        change_md5(&w.env, &["yamareco", FOLDER], "090100_2.jpg");
        let r = w.env.remote();
        let p = Progress {
            // 行があれば、ジョブの記録に MD5 が無くても（古い記録）行の MD5 で照らす
            digests: kept_digests.then(|| w.set.recorded()),
            ..progress(Stage::Trashing, Some(&old), Some(&inc))
        };
        let info = w
            .recovery(&r, None)
            .run(&p, false, &mut Hook::new(&w.env))
            .unwrap_err();
        assert_eq!(info.reason, BlockedReason::ContentMismatch, "{name}");
        assert_eq!(std::fs::read(row_file(&w)).unwrap(), before, "{name}");
        assert!(w.env.op_count("trash") == 0, "{name}");
        assert!(w.env.drive().trashed.is_empty(), "{name}");
    }

    // 書き換えられていなければ、行の MD5 のまま完了する（旧フォルダをゴミ箱へ移し、previous は残さない）
    let (w, old, inc) = replaced_world("digest-row-clean");
    w.env
        .published
        .upsert(geoframe_export::published::PublishedRecord {
            destination: Y,
            folder: FolderName::parse(FOLDER).unwrap(),
            folder_id: inc.clone(),
            job_id: geoframe_export::jobid::JobId::parse(JOB_A).unwrap(),
            files: Some(w.set.recorded()),
            // 旧フォルダがまだ退避先にあるので、その ID と MD5 が previous に残っている
            previous: Some(Box::new(geoframe_export::published::PreviousFolder {
                folder_id: old.clone(),
                files: w.set.recorded(),
            })),
        })
        .unwrap();
    let r = w.env.remote();
    let res = w
        .recovery(&r, None)
        .run(
            &progress(Stage::Trashing, Some(&old), Some(&inc)),
            false,
            &mut Hook::new(&w.env),
        )
        .unwrap();
    assert!(matches!(res, Resolution::Published(_)));
    let row = w
        .env
        .published
        .find(Y, &FolderName::parse(FOLDER).unwrap())
        .unwrap();
    assert_eq!(row.files, Some(w.set.recorded()));
    assert!(row.previous.is_none());
    assert_eq!(w.env.drive().trashed, [old]);
}

#[test]
fn when_the_published_row_and_the_job_record_disagree_on_md5_the_row_wins() {
    // 公開の記録の行（同じフォルダ ID）とジョブの記録に、違う MD5 が入っているとき、照らすのは行の MD5。
    // 行が正しければ（宛先の中身と一致）、ジョブの記録の MD5 が違っていても進み、行が違っていれば（ジョブの記録が宛先と一致していても）
    // blocked にする。行はジョブの記録（退避・公開の前に書く）より後に書かれた、より新しい記録なので
    let different = {
        let mut v = world("digest-precedence-probe").set.recorded();
        v[0].md5 = "e".repeat(32);
        v
    };
    for (name, row_is_right) in [("row-right", true), ("row-wrong", false)] {
        let (w, old, inc) = replaced_world(&format!("digest-precedence-{name}"));
        let truth = w.set.recorded();
        let mut wrong = truth.clone();
        wrong[0].md5 = different[0].md5.clone();
        assert_ne!(truth, wrong);
        let (row_files, job_files) = if row_is_right {
            (truth.clone(), wrong.clone())
        } else {
            (wrong.clone(), truth.clone())
        };
        w.env
            .published
            .upsert(geoframe_export::published::PublishedRecord {
                destination: Y,
                folder: FolderName::parse(FOLDER).unwrap(),
                folder_id: inc.clone(),
                job_id: geoframe_export::jobid::JobId::parse(JOB_A).unwrap(),
                files: Some(row_files),
                previous: Some(Box::new(geoframe_export::published::PreviousFolder {
                    folder_id: old.clone(),
                    files: truth.clone(),
                })),
            })
            .unwrap();
        let before = std::fs::read(row_file(&w)).unwrap();
        let r = w.env.remote();
        let p = Progress {
            digests: Some(job_files),
            ..progress(Stage::Trashing, Some(&old), Some(&inc))
        };
        let res = w.recovery(&r, None).run(&p, false, &mut Hook::new(&w.env));
        if row_is_right {
            assert!(
                matches!(res, Ok(Resolution::Published(_))),
                "{name}: {res:?}"
            );
            assert_eq!(w.env.drive().trashed, [old], "{name}");
        } else {
            assert_eq!(
                res.unwrap_err().reason,
                BlockedReason::ContentMismatch,
                "{name}"
            );
            assert_eq!(std::fs::read(row_file(&w)).unwrap(), before, "{name}");
            assert!(w.env.drive().trashed.is_empty(), "{name}");
            assert!(w.env.op_count("trash") == 0, "{name}");
        }
    }
}

#[test]
fn a_new_destination_rewritten_while_stopped_is_blocked_too() {
    // 新規（置き換えでない）: ジョブの記録は publishing、宛先は incoming の ID のフォルダ。書き換えられていれば blocked
    let w = world("digest-new");
    let inc = w.put_incoming();
    w.publish_incoming(&inc);
    change_md5(&w.env, &["yamareco", FOLDER], "090000_1.jpg");
    let r = w.env.remote();
    let kept = Progress {
        digests: Some(w.set.recorded()),
        ..progress(Stage::Publishing, None, Some(&inc))
    };
    let info = w
        .recovery(&r, None)
        .run(&kept, false, &mut Hook::new(&w.env))
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ContentMismatch);
    assert_eq!(w.recorded_id(), None, "公開の記録を書かない");
    assert!(!row_file(&w).exists());

    // 移す前（retiring のあと・incoming のまま）に書き換えられていても、移さずに blocked
    let w = world("digest-roll-forward");
    let old = w.env.seed_managed(Y, FOLDER, &w.set);
    let inc = w.put_incoming();
    w.retire(&old);
    change_md5(
        &w.env,
        &["geoframe-export-work", "incoming", JOB_A],
        "090000_1.jpg",
    );
    let r = w.env.remote();
    let kept = Progress {
        digests: Some(w.set.recorded()),
        ..progress(Stage::Retiring, Some(&old), Some(&inc))
    };
    let info = w
        .recovery(&r, None)
        .run(&kept, false, &mut Hook::new(&w.env))
        .unwrap_err();
    assert_eq!(info.reason, BlockedReason::ContentMismatch);
    assert!(w.env.op_count("move") == 0);
    assert!(w.dest().is_none());
    assert_eq!(w.recorded_id().as_deref(), Some(old.as_str()));
}

#[test]
fn an_old_job_record_without_md5_writes_a_row_without_md5_so_the_next_replace_is_refused() {
    // ジョブの記録にも、同じフォルダ ID の公開の記録にも MD5 が無い（古い記録）。宛先の今の MD5 から作らず、MD5 の無い行にする
    let w = world("digest-legacy");
    let inc = w.put_incoming();
    w.publish_incoming(&inc);
    let r = w.env.remote();
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
    assert!(p.recorded);
    let row = w
        .env
        .published
        .find(Y, &FolderName::parse(FOLDER).unwrap())
        .unwrap();
    assert_eq!(row.folder_id, inc);
    assert_eq!(row.files, None);
    // 次の replace は、中身を確かめられないので管理していないものとして断る
    let layout = w.env.layout(Y, FOLDER, JOB_B);
    let rclone = w.env.remote();
    assert_eq!(
        geoframe_export::publish::dry_run(&rclone, &layout, &w.env.published)
            .unwrap()
            .managed,
        Some(geoframe_export::drive::Managed::NotManaged)
    );
}
