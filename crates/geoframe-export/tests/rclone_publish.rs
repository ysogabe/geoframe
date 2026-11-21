//! T4: 宛先の状態、管理しているフォルダの見分け方、incoming → 検証 → 公開、置き換えの退避とゴミ箱、失敗のあとの復旧、dry_run
//! （design.md 4.2、10 章の AC5・AC6・AC10）。偽の rclone（`fake-rclone/`）と偽の Drive だけを使う。
//! 本物の rclone・Drive・ネットワークには触れない。

mod common;

use std::time::{Duration, Instant};

use common::drive::model::{self, Kind};
use common::drive::*;
use geoframe_export::drive::{Managed, Target};
use geoframe_export::error::ErrorCode;
use geoframe_export::publish::{
    ConflictReason, PublishFailure, PublishOutcome, PublishParams, Published, dry_run, publish,
};
use geoframe_export::rclone::Rclone;
use geoframe_export::recovery::{BlockedReason, Stage};
use geoframe_export::request::{Destination, OnExists};
use serde_json::{Value, json};

const PHOTOS: &[(&str, u64)] = &[("090000_1.jpg", 100), ("090100_2.jpg", 200)];
const Y: Destination = Destination::Yamareco;

fn params<'a>(env: &'a Env, set: &'a Set, on_exists: OnExists) -> PublishParams<'a> {
    PublishParams {
        on_exists,
        workspace: &set.dir,
        files: &set.files,
        published: &env.published,
        timing: fast_timing(),
    }
}

fn run(
    env: &Env,
    set: &Set,
    on_exists: OnExists,
    hook: &mut Hook,
) -> Result<PublishOutcome, PublishFailure> {
    let layout = env.layout(Y, FOLDER, JOB_A);
    publish(&env.rclone(), &layout, &params(env, set, on_exists), hook)
}

fn published(out: Result<PublishOutcome, PublishFailure>) -> Published {
    match out {
        Ok(PublishOutcome::Published(p)) => p,
        other => panic!("公開されるはずです: {other:?}"),
    }
}

fn failure(out: Result<PublishOutcome, PublishFailure>) -> PublishFailure {
    match out {
        Err(f) => f,
        other => panic!("失敗するはずです: {other:?}"),
    }
}

fn conflict(out: Result<PublishOutcome, PublishFailure>) -> (ConflictReason, u64) {
    match out {
        Ok(PublishOutcome::Conflict {
            reason,
            existing_files,
        }) => (reason, existing_files),
        other => panic!("conflict のはずです: {other:?}"),
    }
}

fn dest_id(env: &Env) -> Option<String> {
    env.drive().resolve(&["yamareco", FOLDER])
}

/// 宛先のフォルダの直下の、ファイルの（相対パス, サイズ）。
fn files_under(env: &Env, path: &[&str]) -> Vec<(String, u64)> {
    let d = env.drive();
    let root = d.resolve(path).expect("folder");
    let mut out: Vec<(String, u64)> = d
        .ls(&root, true, true, "mime")
        .into_iter()
        .filter(|r| !r.is_dir)
        .map(|r| (r.path, r.size as u64))
        .collect();
    out.sort();
    out
}

fn expected_files(set: &Set) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = set.files.iter().map(|f| (f.path.clone(), f.size)).collect();
    v.sort();
    v
}

fn remove_child(env: &Env, parent: &[&str], name: &str) {
    let mut d = env.drive();
    let p = d.resolve(parent).expect("parent");
    let ids: Vec<String> = d
        .children(&p)
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.id.clone())
        .collect();
    for id in ids {
        d.nodes.retain(|n| n.id != id);
    }
    env.save(&d);
}

fn rewrite_manifest(env: &Env, folder: &[&str], content: &Value) {
    let mut d = env.drive();
    let p = d.resolve(folder).expect("folder");
    let text = format!("{}\n", serde_json::to_string_pretty(content).unwrap());
    for n in &mut d.nodes {
        if n.parent == p && n.name == "manifest.json" {
            n.size = text.len() as u64;
            n.content = Some(text.clone());
        }
    }
    env.save(&d);
}

// ---- AC5: 0 枚では rclone を呼ばない ----

#[test]
fn a_set_without_photos_is_refused_before_any_rclone_call() {
    let env = Env::new("zero");
    let set = env.make_set("zero", Y, FOLDER, PHOTOS);
    let manifest_only = vec![set.files.last().unwrap().clone()];
    let mut hook = Hook::new(&env);
    let layout = env.layout(Y, FOLDER, JOB_A);
    for files in [&[][..], &manifest_only[..], &set.files[..1]] {
        let p = PublishParams {
            files,
            ..params(&env, &set, OnExists::Replace)
        };
        let f = failure(publish(&env.rclone(), &layout, &p, &mut hook));
        assert_eq!(f.error.code, ErrorCode::Internal);
    }
    // 宛先に既にフォルダがあっても同じ。rclone は 1 度も呼ばれず、記録も書かれない
    env.seed_managed(Y, FOLDER, &set);
    let p = PublishParams {
        files: &manifest_only,
        ..params(&env, &set, OnExists::Replace)
    };
    assert_eq!(
        failure(publish(&env.rclone(), &layout, &p, &mut hook))
            .error
            .code,
        ErrorCode::Internal
    );
    assert!(env.calls().is_empty());
}

// ---- 新しく公開する ----

#[test]
fn absent_destination_is_published_in_the_documented_order() {
    let env = Env::new("absent");
    let set = env.make_set("a", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));

    // 各段階の記録が、その段階の Drive の操作より先（1 本の順番で確かめる）。uploading は 2 回
    assert_eq!(
        env.mutations(),
        [
            "record:uploading",
            "mkdir",
            "record:uploading",
            "copy",
            "copy",
            "record:verifying",
            "record:publishing",
            "moveto"
        ]
    );
    // 記録した ID: 最初は incoming の ID が無く、2 回目から ID つき。新規なので旧 ID は無い
    assert_eq!(hook.records[0].incoming_id, None);
    let incoming_id = hook.records[1].incoming_id.clone().expect("incoming の ID");
    assert!(hook.records.iter().all(|r| r.old_id.is_none()));
    assert!(
        hook.records[1..]
            .iter()
            .all(|r| r.incoming_id.as_deref() == Some(&incoming_id))
    );
    // 宛先は incoming のフォルダ（ID が同じ）。incoming は残らない
    assert_eq!(p.folder_id, incoming_id);
    assert_eq!(dest_id(&env).as_deref(), Some(incoming_id.as_str()));
    assert!(
        env.drive()
            .resolve(&["geoframe-export-work", "incoming", JOB_A])
            .is_none()
    );
    assert_eq!(
        files_under(&env, &["yamareco", FOLDER]),
        expected_files(&set)
    );
    assert!(!p.replaced && p.recorded && p.old_left_in_replaced.is_none());
    assert_eq!(
        env.published
            .find_id(
                Y,
                &geoframe_export::folder::FolderName::parse(FOLDER).unwrap()
            )
            .as_deref(),
        Some(incoming_id.as_str())
    );

    // 呼び方: copy・moveto に --immutable、manifest は最後に別の呼び出し、位置の引数は固定の接頭辞＋検査済みの名前だけ
    let ws = set.dir.to_string_lossy().into_owned();
    let incoming = format!("gdrive:geoframe-export-work/incoming/{JOB_A}");
    let copies = env.rclone_calls("copy");
    assert_eq!(copies.len(), 2);
    for c in &copies {
        assert!(c.contains(&"--immutable".to_owned()));
        let d = c.iter().position(|a| a == "--").unwrap();
        assert_eq!(&c[d + 1..], [ws.clone(), incoming.clone()]);
    }
    assert!(
        copies[0]
            .windows(2)
            .any(|w| w == ["--exclude", "/manifest.json"])
    );
    assert!(
        copies[1]
            .windows(2)
            .any(|w| w == ["--include", "/manifest.json"])
    );
    let mv = &env.rclone_calls("moveto")[0];
    assert!(mv.contains(&"--immutable".to_owned()));
    let d = mv.iter().position(|a| a == "--").unwrap();
    assert_eq!(
        &mv[d + 1..],
        [incoming, format!("gdrive:yamareco/{FOLDER}")]
    );
    // 共有・リンクの操作は呼ばない。使ったのは決まったコマンドだけ
    assert_eq!(env.commands_used(), ["copy", "lsjson", "mkdir", "moveto"]);
    assert_safe_calls(&env);
}

#[test]
fn a_period_with_date_subfolders_is_published_and_verified() {
    let env = Env::new("period");
    let folder = "2030-03-10-11";
    let files = [
        ("2030-03-10/090000_1.jpg", 100),
        ("2030-03-11/090100_2.jpg", 200),
    ];
    let set = env.make_set("p", Y, folder, &files);
    let mut hook = Hook::new(&env);
    let layout = env.layout(Y, folder, JOB_A);
    let out = publish(
        &env.rclone(),
        &layout,
        &params(&env, &set, OnExists::Error),
        &mut hook,
    );
    let p = published(out);
    assert_eq!(
        files_under(&env, &["yamareco", folder]),
        expected_files(&set)
    );
    assert_eq!(
        env.drive().resolve(&["yamareco", folder]).as_deref(),
        Some(p.folder_id.as_str())
    );
}

// ---- 宛先が既にある ----

#[test]
fn an_existing_destination_is_a_conflict_by_default_and_nothing_is_written() {
    // 中身のあるフォルダ・空のフォルダ。どちらも、copy・moveto・purge・mkdir は呼ばれず、記録も書かれない
    for (name, with_files) in [("files", true), ("empty", false)] {
        let env = Env::new(&format!("exists-{name}"));
        let set = env.make_set("e", Y, FOLDER, PHOTOS);
        let id = if with_files {
            env.seed_managed(Y, FOLDER, &set)
        } else {
            let mut d = env.drive();
            let id = d.mkdir_p(&["yamareco", FOLDER]);
            env.save(&d);
            id
        };
        let mut hook = Hook::new(&env);
        let (reason, files) = conflict(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(reason, ConflictReason::Exists, "{name}");
        assert_eq!(files, if with_files { 3 } else { 0 }, "{name}");
        assert_eq!(env.mutations(), Vec::<String>::new(), "{name}");
        assert_eq!(env.commands_used(), ["lsjson"], "{name}");
        assert_eq!(dest_id(&env).as_deref(), Some(id.as_str()));
        assert_safe_calls(&env);
    }
}

#[test]
fn an_empty_folder_is_not_managed_and_replace_is_refused() {
    let env = Env::new("empty-replace");
    let set = env.make_set("e", Y, FOLDER, PHOTOS);
    let mut d = env.drive();
    d.mkdir_p(&["yamareco", FOLDER]);
    env.save(&d);
    let mut hook = Hook::new(&env);
    let (reason, files) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!((reason, files), (ConflictReason::NotManaged, 0));
    assert!(env.mutations().is_empty());
}

// ---- 置き換え ----

#[test]
fn replace_calls_in_the_documented_order_and_trashes_the_old_folder() {
    let env = Env::new("replace");
    let old_set = env.make_set(
        "old",
        Y,
        FOLDER,
        &[("090000_1.jpg", 50), ("090100_9.jpg", 60)],
    );
    let old_id = env.seed_managed(Y, FOLDER, &old_set);
    let set = env.make_set("new", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));

    // incoming → 検証 → 退避 → 移動 → ゴミ箱。各段階の記録は、その操作より先
    assert_eq!(
        env.mutations(),
        [
            "record:uploading",
            "mkdir",
            "record:uploading",
            "copy",
            "copy",
            "record:verifying",
            "record:retiring",
            "moveto",
            "record:publishing",
            "moveto",
            "record:trashing",
            "purge"
        ]
    );
    // 記録した ID: 旧フォルダの ID は最初の記録から、incoming の ID は 2 回目の uploading から
    assert!(
        hook.records
            .iter()
            .all(|r| r.old_id.as_deref() == Some(old_id.as_str()))
    );
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    assert_ne!(incoming_id, old_id);
    assert!(p.replaced);
    assert_eq!(p.existing_files, 3);
    assert_eq!(dest_id(&env).as_deref(), Some(incoming_id.as_str()));
    assert_eq!(
        files_under(&env, &["yamareco", FOLDER]),
        expected_files(&set)
    );
    // 旧フォルダはゴミ箱（--drive-use-trash=true の purge）。replaced/ と incoming/ には何も残らない
    let d = env.drive();
    assert_eq!(d.trashed, std::slice::from_ref(&old_id));
    assert!(d.deleted.is_empty());
    let replaced = d.resolve(&["geoframe-export-work", "replaced"]).unwrap();
    assert!(d.children(&replaced).is_empty());
    assert!(
        d.resolve(&["geoframe-export-work", "incoming", JOB_A])
            .is_none()
    );
    // 公開の記録は新しい ID に置き換わる
    let f = geoframe_export::folder::FolderName::parse(FOLDER).unwrap();
    assert_eq!(
        env.published.find_id(Y, &f).as_deref(),
        Some(incoming_id.as_str())
    );

    // 呼び方: 退避は宛先 → replaced/<種類>-<フォルダ名>-<job_id>、purge は --drive-use-trash=true
    let moves = env.rclone_calls("moveto");
    let tail = |c: &Vec<String>| c[c.iter().position(|a| a == "--").unwrap() + 1..].to_vec();
    assert_eq!(
        tail(&moves[0]),
        [
            format!("gdrive:yamareco/{FOLDER}"),
            format!("gdrive:geoframe-export-work/replaced/yamareco-{FOLDER}-{JOB_A}")
        ]
    );
    assert_eq!(
        tail(&moves[1]),
        [
            format!("gdrive:geoframe-export-work/incoming/{JOB_A}"),
            format!("gdrive:yamareco/{FOLDER}")
        ]
    );
    let purge = &env.rclone_calls("purge")[0];
    assert!(purge.contains(&"--drive-use-trash=true".to_owned()));
    assert_eq!(
        tail(purge),
        [format!(
            "gdrive:geoframe-export-work/replaced/yamareco-{FOLDER}-{JOB_A}"
        )]
    );
    assert_safe_calls(&env);
}

// ---- 各段階の記録が書けなければ、その段階の操作はしない ----

#[test]
fn a_failed_record_stops_the_stage_before_its_drive_operation() {
    // uploading の最初: フォルダも作らない
    let env = Env::new("rec-up1");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Uploading, 1);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert_eq!(env.mutations(), ["record:uploading"]);

    // uploading の 2 回目（ID の記録）: 作ったフォルダを片付ける。写真は送らない
    let env = Env::new("rec-up2");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Uploading, 2);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert_eq!(
        env.mutations(),
        ["record:uploading", "mkdir", "record:uploading", "purge"]
    );
    assert!(!f.leftover_incoming);

    // verifying: 検証の一覧も、公開もしない
    let env = Env::new("rec-ver");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Verifying, 1);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert!(!env.mutations().contains(&"moveto".to_owned()));
    assert!(dest_id(&env).is_none());

    // 新規の publishing: 移動しない。incoming を片付ける
    let env = Env::new("rec-pub");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Publishing, 1);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert!(!env.mutations().contains(&"moveto".to_owned()));
    assert!(dest_id(&env).is_none());
    assert_eq!(env.drive().trashed.len(), 1);

    // 置き換えの retiring: 旧フォルダを動かさない
    let env = Env::new("rec-ret");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env).fail_at(Stage::Retiring, 1);
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert!(!env.mutations().contains(&"moveto".to_owned()));
    assert_eq!(dest_id(&env).as_deref(), Some(old_id.as_str()));
    assert_eq!(env.drive().trashed.len(), 1); // incoming だけ

    // 退避のあとで publishing の記録が書けなくても、宛先を空のままにしない（記録済みの段階から復旧の表で進める）
    let env = Env::new("rec-pub-replace");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env).fail_at(Stage::Publishing, 1);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert_ne!(dest_id(&env).as_deref(), Some(old_id.as_str()));
    assert_eq!(env.drive().trashed, [old_id]);
    assert!(p.recorded);

    // trashing の記録が書けなくても、公開は済んでいる。旧フォルダはゴミ箱へ
    let env = Env::new("rec-trash");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env).fail_at(Stage::Trashing, 1);
    let _ = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(env.drive().trashed, [old_id]);
}

// ---- 移動の応答が失われた・遅れた ----

#[test]
fn a_move_whose_response_was_lost_is_found_done_by_listing() {
    // 新規: 移動は済んでいるのに rclone は失敗を返す → lsjson で済んでいると判定し、成功
    let env = Env::new("lost-absent");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("moveto", 0, 1, do_then_fail());
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    assert_eq!(p.folder_id, incoming_id);
    assert_eq!(dest_id(&env).as_deref(), Some(incoming_id.as_str()));
    assert_eq!(env.rclone_calls("moveto").len(), 1, "もう一度移動しない");
    assert!(p.recorded);
    assert_eq!(
        files_under(&env, &["yamareco", FOLDER]),
        expected_files(&set)
    );

    // 置き換え: 退避の応答が失われた → 前へ進める（incoming を宛先へ移し、旧フォルダをゴミ箱へ）
    let env = Env::new("lost-retire");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("moveto", 0, 1, do_then_fail());
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert!(p.replaced && p.recorded);
    assert_eq!(env.drive().trashed, [old_id]);
    assert_eq!(dest_id(&env).as_deref(), Some(p.folder_id.as_str()));
    assert_eq!(
        files_under(&env, &["yamareco", FOLDER]),
        expected_files(&set)
    );

    // 置き換え: 公開の移動の応答が失われた → 公開済み。旧フォルダのゴミ箱への移動を続ける
    let env = Env::new("lost-publish");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("moveto", 1, 1, do_then_fail());
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(env.drive().trashed, [old_id]);
    assert_eq!(env.rclone_calls("moveto").len(), 2);
    assert_eq!(dest_id(&env).as_deref(), Some(p.folder_id.as_str()));

    // 置き換え: ゴミ箱への移動の応答が失われた → 一覧でもう無いと分かり、成功（旧フォルダは残っていない）
    let env = Env::new("lost-purge");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("purge", 0, 1, do_then_fail());
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(p.old_left_in_replaced, None);
    assert_eq!(env.drive().trashed, [old_id]);
}

#[test]
fn a_purge_that_really_failed_leaves_the_new_set_published_and_says_the_old_folder_remains() {
    let env = Env::new("purge-fail");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("purge", 0, u64::MAX, fail(1));
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(p.old_left_in_replaced.as_deref(), Some(old_id.as_str()));
    assert!(p.recorded);
    assert_eq!(dest_id(&env).as_deref(), Some(p.folder_id.as_str()));
    assert!(env.drive().trashed.is_empty());
}

#[test]
fn a_late_move_makes_two_listings_differ_so_the_service_waits_and_looks_again() {
    let env = Env::new("late");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    // 移動は失敗で終わり、Drive 側では数回の一覧のあとに反映される。1 回目の観察の途中で反映される
    env.add_rule("moveto", 0, 1, late_then_fail(3));
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    assert_eq!(p.folder_id, incoming_id);
    assert_eq!(dest_id(&env).as_deref(), Some(incoming_id.as_str()));
    // 2 つの観察が食い違ったので、やり直した（失敗のあとの lsjson が、1 回分の観察の数より多い）
    let seq = env.sequence();
    let after_move = seq.iter().rposition(|c| c == "moveto").unwrap();
    let lists_after = seq[after_move..].iter().filter(|c| *c == "lsjson").count();
    assert!(lists_after >= 18, "{lists_after}");
    assert!(
        !seq[after_move..].contains(&"purge".to_owned()),
        "公開済みの incoming を消さない"
    );
}

#[test]
fn a_move_that_never_shows_up_is_treated_as_not_done_and_incoming_is_cleaned() {
    // 2 回の観察が同じなら、その状態に表を当てはめる（移動は実行されなかったと見る）。失敗は元のまま返る
    let env = Env::new("stale");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("moveto", 0, 1, late_then_fail(100_000));
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::RcloneFailed);
    assert!(f.blocked.is_none() && !f.leftover_incoming);
    assert!(dest_id(&env).is_none());
    assert_eq!(env.drive().trashed.len(), 1);
}

// ---- 照合できない → blocked ----

#[test]
fn a_file_by_file_publish_move_is_found_by_the_id_check_and_blocks() {
    let env = Env::new("filewise-publish");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("moveto", 0, 1, filewise());
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    let info = f.blocked.expect("blocked の記録");
    assert_eq!(info.reason, BlockedReason::PublishMismatch);
    assert_eq!(info.progress.stage, Stage::Publishing);
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    let o = info.observed.clone().expect("見えていた状態");
    assert_eq!(o.dest.len(), 1);
    assert_ne!(o.dest[0].id, incoming_id, "宛先の ID が incoming と違う");
    // それ以上は何もしない（purge なし、移動も 1 回だけ）
    assert_eq!(env.rclone_calls("moveto").len(), 1);
    assert!(env.rclone_calls("purge").is_empty());
    assert!(
        info.describe(&env.layout(Y, FOLDER, JOB_A))
            .contains("publish_mismatch")
    );
}

#[test]
fn a_file_by_file_retire_move_blocks_before_anything_reaches_the_trash() {
    let env = Env::new("filewise-retire");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("moveto", 0, 1, filewise());
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    let info = f.blocked.unwrap();
    assert_eq!(info.reason, BlockedReason::RetireMismatch);
    assert_eq!(info.progress.stage, Stage::Retiring);
    assert_eq!(info.progress.old_id.as_deref(), Some(old_id.as_str()));
    let o = info.observed.clone().unwrap();
    assert!(o.dest.is_empty());
    assert_eq!(o.replaced.len(), 1);
    assert_ne!(
        o.replaced[0].id, old_id,
        "replaced の ID が記録した旧 ID と違う"
    );
    assert_eq!(env.rclone_calls("moveto").len(), 1);
    assert!(env.rclone_calls("purge").is_empty());
    assert!(env.drive().trashed.is_empty());
    // recover の表示: 移してしまったフォルダの ID と、戻す手順
    let text = info.describe(&env.layout(Y, FOLDER, JOB_A));
    assert!(text.contains(&o.replaced[0].id), "{text}");
    assert!(text.contains("戻す手順"), "{text}");
}

#[test]
fn a_manual_folder_created_between_the_check_and_the_retire_is_caught_by_the_id_check() {
    // 確かめてから移すまでの間に、同じ名前の別のフォルダに入れ替わった（rclone の moveto はパスで解決する）
    let env = Env::new("swap");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let (fake, folder) = (env.fake_dir.clone(), FOLDER);
    let mut hook = Hook::new(&env).mutate_after(Stage::Retiring, move || {
        let mut d = model::Drive::load(&fake);
        let old = d.resolve(&["yamareco", folder]).unwrap();
        d.purge(&old, false);
        d.mkdir_p(&["yamareco", folder]);
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    let info = f.blocked.expect("blocked");
    assert_eq!(info.reason, BlockedReason::RetireMismatch);
    let moved = &info.observed.as_ref().unwrap().replaced[0].id;
    assert_ne!(moved, &old_id);
    assert!(env.rclone_calls("purge").is_empty(), "ゴミ箱へは移さない");
    // 移してしまったフォルダは replaced/ にある（オーナーが戻せる）
    assert!(
        env.drive()
            .resolve(&[
                "geoframe-export-work",
                "replaced",
                &format!("yamareco-{FOLDER}-{JOB_A}")
            ])
            .is_some()
    );
}

#[test]
fn a_manual_folder_appearing_after_the_retire_blocks_instead_of_publishing_over_it() {
    let env = Env::new("after-retire");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let fake = env.fake_dir.clone();
    let mut hook = Hook::new(&env).mutate_after(Stage::Publishing, move || {
        let mut d = model::Drive::load(&fake);
        d.mkdir_p(&["yamareco", FOLDER]);
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    let info = f.blocked.expect("blocked");
    assert_eq!(info.reason, BlockedReason::DestinationUnexpected);
    assert_eq!(env.rclone_calls("moveto").len(), 1, "公開の移動はしない");
    assert!(env.rclone_calls("purge").is_empty());
    // 旧フォルダは replaced/ に残っている（ID も記録のまま）
    let d = env.drive();
    let replaced = d
        .resolve(&[
            "geoframe-export-work",
            "replaced",
            &format!("yamareco-{FOLDER}-{JOB_A}"),
        ])
        .unwrap();
    assert_eq!(replaced, old_id);
}

#[test]
fn an_unstable_listing_that_never_settles_blocks() {
    // 一覧が毎回違う形で返り続ける → 2 回の観察が一致せず、rounds 回で決まらない
    let env = Env::new("unstable");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("moveto", 0, 1, fail(1));
    // 失敗のあとの一覧がすべて失敗する
    let mut c = env.ctl();
    c.rules.push(model::Rule {
        cmd: "lsjson".into(),
        contains: None,
        // 失敗した移動より前の lsjson（宛先の状態 2、incoming の ID 2、検証 2、確かめ直し 2、公開の直前 2）は通し、
        // 失敗のあとの一覧（復旧の観察）をすべて失敗させる
        skip: 10,
        times: u64::MAX,
        seen: 0,
        effect: fail(1),
    });
    c.save(&env.fake_dir);
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    assert_eq!(f.blocked.unwrap().reason, BlockedReason::Unobservable);
}

// ---- 実行中に宛先が変わった・検証が合わない ----

#[test]
fn a_destination_created_during_the_run_stops_the_publish_and_cleans_incoming() {
    for (stage, name) in [
        (Stage::Verifying, "during-verify"),
        (Stage::Publishing, "before-move"),
    ] {
        let env = Env::new(name);
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let fake = env.fake_dir.clone();
        let mut hook = Hook::new(&env).mutate_after(stage, move || {
            let mut d = model::Drive::load(&fake);
            d.mkdir_p(&["yamareco", FOLDER]);
            d.save(&fake);
        });
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(f.error.code, ErrorCode::AmbiguousDestination, "{name}");
        assert!(f.blocked.is_none() && !f.leftover_incoming, "{name}");
        assert!(env.rclone_calls("moveto").is_empty(), "{name}: 移動しない");
        assert_eq!(env.drive().trashed.len(), 1, "{name}: incoming を片付けた");
        // オーナーが作ったフォルダには触れない
        assert!(dest_id(&env).is_some());
    }
}

#[test]
fn a_replace_target_that_changed_during_the_run_is_not_replaced() {
    let env = Env::new("changed-replace");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let fake = env.fake_dir.clone();
    // 検証の間に、オーナーが旧フォルダにメモを足した → 管理しているが変更あり。移動しない
    let mut hook = Hook::new(&env).mutate_after(Stage::Verifying, move || {
        let mut d = model::Drive::load(&fake);
        d.add_file(&["yamareco", FOLDER], "memo.txt", 3, None);
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::AmbiguousDestination);
    assert!(env.rclone_calls("moveto").is_empty());
    assert_eq!(dest_id(&env).as_deref(), Some(old_id.as_str()));
}

#[test]
fn a_verification_mismatch_trashes_incoming_and_fails() {
    // 送ったあとで、incoming のファイルのサイズが変わった（または足りない・余分がある）
    for (name, tamper) in [("size", 0), ("missing", 1), ("extra", 2)] {
        let env = Env::new(&format!("verify-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let fake = env.fake_dir.clone();
        let mut hook = Hook::new(&env).mutate_after(Stage::Verifying, move || {
            let mut d = model::Drive::load(&fake);
            let inc = d
                .resolve(&["geoframe-export-work", "incoming", JOB_A])
                .unwrap();
            match tamper {
                0 => {
                    for n in &mut d.nodes {
                        if n.parent == inc && n.name == "090000_1.jpg" {
                            n.size += 1;
                        }
                    }
                }
                1 => d
                    .nodes
                    .retain(|n| !(n.parent == inc && n.name == "090000_1.jpg")),
                _ => {
                    d.add_file(
                        &["geoframe-export-work", "incoming", JOB_A],
                        "extra.jpg",
                        1,
                        None,
                    );
                }
            }
            d.save(&fake);
        });
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(f.error.code, ErrorCode::VerifyMismatch, "{name}");
        assert!(env.rclone_calls("moveto").is_empty(), "{name}");
        assert_eq!(env.drive().trashed.len(), 1, "{name}: incoming はゴミ箱へ");
        assert!(dest_id(&env).is_none(), "{name}");
    }
}

// ---- 宛先の状態: ambiguous・照会の失敗 ----

#[test]
fn duplicates_files_and_shortcuts_at_the_destination_are_ambiguous() {
    enum Case {
        TwoFolders,
        File,
        ShortcutMime,
        ShortcutResolved,
        FolderPlusShortcut,
    }
    for (name, case) in [
        ("two-folders", Case::TwoFolders),
        ("file", Case::File),
        ("shortcut-mime", Case::ShortcutMime),
        ("shortcut-resolved", Case::ShortcutResolved),
        ("folder-and-shortcut", Case::FolderPlusShortcut),
    ] {
        let env = Env::new(&format!("amb-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let mut d = env.drive();
        let other = d.mkdir_p(&["elsewhere"]);
        match case {
            Case::TwoFolders => {
                d.mkdir_dup(&["yamareco"], FOLDER);
                d.mkdir_dup(&["yamareco"], FOLDER);
            }
            Case::File => {
                d.add_file(&["yamareco"], FOLDER, 5, None);
            }
            Case::ShortcutMime => {
                d.add_shortcut(&["yamareco"], FOLDER, &other);
            }
            Case::ShortcutResolved => {
                d.add_shortcut(&["yamareco"], FOLDER, &other);
            }
            Case::FolderPlusShortcut => {
                d.mkdir_p(&["yamareco", FOLDER]);
                d.add_shortcut(&["yamareco"], FOLDER, &other);
            }
        }
        env.save(&d);
        if matches!(case, Case::ShortcutResolved) {
            env.set_shortcut_mode("resolved");
        }
        for on_exists in [OnExists::Error, OnExists::Replace] {
            let mut hook = Hook::new(&env);
            let f = failure(run(&env, &set, on_exists, &mut hook));
            assert_eq!(f.error.code, ErrorCode::AmbiguousDestination, "{name}");
        }
        assert!(env.mutations().is_empty(), "{name}: {:?}", env.mutations());
    }
}

#[test]
fn a_failed_query_is_drive_unavailable_and_never_read_as_absent() {
    // rclone の失敗（終了コード 1・3）、決められた形でない出力、ID の無い出力、期限切れ
    let cases: Vec<(&str, Value)> = vec![
        ("exit1", fail(1)),
        ("exit3", fail(3)),
        ("garbage", garbage("not json")),
        ("object", garbage("{}")),
        (
            "no-id",
            garbage(r#"[{"Path":"a","Name":"a","Size":1,"MimeType":"x/y","IsDir":false}]"#),
        ),
        (
            "empty-id",
            garbage(r#"[{"Path":"a","Name":"a","Size":1,"MimeType":"x/y","IsDir":false,"ID":""}]"#),
        ),
    ];
    for (name, effect) in cases {
        let env = Env::new(&format!("unavail-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        env.add_rule("lsjson", 0, u64::MAX, effect);
        let mut hook = Hook::new(&env);
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(f.error.code, ErrorCode::DriveUnavailable, "{name}");
        assert!(env.mutations().is_empty(), "{name}: 何も書かない");
    }
    // 接頭辞のフォルダそのものが無い（lsjson が終了コード 3）。「宛先が無い」とは読まない
    let env = Env::new("unavail-no-prefix");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut d = env.drive();
    let id = d.resolve(&["yamareco"]).unwrap();
    d.nodes.retain(|n| n.id != id);
    env.save(&d);
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::DriveUnavailable);
    assert!(env.mutations().is_empty());
}

#[test]
fn a_query_that_times_out_is_drive_unavailable() {
    let env = Env::new("unavail-timeout");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("lsjson", 0, u64::MAX, sleep_ms(20_000));
    let rclone = env
        .rclone()
        .with_timeouts(Duration::from_secs(30), Duration::from_millis(300));
    let layout = env.layout(Y, FOLDER, JOB_A);
    let mut hook = Hook::new(&env);
    let started = Instant::now();
    let f = failure(publish(
        &rclone,
        &layout,
        &params(&env, &set, OnExists::Error),
        &mut hook,
    ));
    assert_eq!(f.error.code, ErrorCode::DriveUnavailable);
    assert!(started.elapsed() < Duration::from_secs(10));
}

// ---- 管理しているフォルダの見分け方 ----

#[test]
fn folders_not_written_by_this_service_are_never_replaced() {
    type Setup = Box<dyn Fn(&Env, &Set)>;
    let f_ = geoframe_export::folder::FolderName::parse(FOLDER).unwrap();
    let cases: Vec<(&str, Setup)> = vec![
        // 公開の記録に ID が無い（手作業のフォルダ。manifest だけは同じ）
        (
            "no-record",
            Box::new(|env, set| {
                env.seed_folder(Y, FOLDER, set);
            }),
        ),
        // 記録の ID が別のフォルダのもの
        (
            "other-id",
            Box::new(move |env, set| {
                env.seed_managed(Y, FOLDER, set);
                env.published
                    .upsert(geoframe_export::published::PublishedRecord {
                        destination: Y,
                        folder: geoframe_export::folder::FolderName::parse(FOLDER).unwrap(),
                        folder_id: "fidOTHER".into(),
                        job_id: geoframe_export::jobid::JobId::parse(JOB_B).unwrap(),
                    })
                    .unwrap();
            }),
        ),
        // 公開の記録が壊れている
        (
            "corrupt-record",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                std::fs::write(env.published.path(), "{not json").unwrap();
            }),
        ),
        // manifest が無い
        (
            "no-manifest",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                remove_child(env, &["yamareco", FOLDER], "manifest.json");
            }),
        ),
        // manifest が 2 つ（直下にちょうど 1 つではない）
        (
            "two-manifests",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                let mut d = env.drive();
                d.add_file(&["yamareco", FOLDER], "manifest.json", 2, Some("{}"));
                env.save(&d);
            }),
        ),
        // manifest がフォルダ
        (
            "manifest-is-a-folder",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                remove_child(env, &["yamareco", FOLDER], "manifest.json");
                let mut d = env.drive();
                d.mkdir_p(&["yamareco", FOLDER, "manifest.json"]);
                env.save(&d);
            }),
        ),
        // generator が違う（このサービスが書いたものではない）
        (
            "other-generator",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                let mut m = manifest_json(Y, FOLDER, PHOTOS);
                m["generator"] = json!("someone-else 1.0");
                rewrite_manifest(env, &["yamareco", FOLDER], &m);
            }),
        ),
        // generator が無い（旧 manifest と同じ形）
        (
            "no-generator",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                let mut m = manifest_json(Y, FOLDER, PHOTOS);
                m.as_object_mut().unwrap().remove("generator");
                rewrite_manifest(env, &["yamareco", FOLDER], &m);
            }),
        ),
        // destination が違う
        (
            "other-destination",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                rewrite_manifest(
                    env,
                    &["yamareco", FOLDER],
                    &manifest_json(Destination::TravelJournal, FOLDER, PHOTOS),
                );
            }),
        ),
        // folder が違う
        (
            "other-folder",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                rewrite_manifest(
                    env,
                    &["yamareco", FOLDER],
                    &manifest_json(Y, "2030-03-11", PHOTOS),
                );
            }),
        ),
        // manifest が JSON でない
        (
            "manifest-not-json",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                let mut d = env.drive();
                let p = d.resolve(&["yamareco", FOLDER]).unwrap();
                for n in &mut d.nodes {
                    if n.parent == p && n.name == "manifest.json" {
                        n.content = Some("hello".into());
                        n.size = 5;
                    }
                }
                env.save(&d);
            }),
        ),
    ];
    for (name, setup) in cases {
        let env = Env::new(&format!("notmanaged-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        setup(&env, &set);
        let before = env.drive().nodes.len();
        let mut hook = Hook::new(&env);
        let (reason, _) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(reason, ConflictReason::NotManaged, "{name}");
        // moveto・purge・copy・mkdir は呼ばれず、記録も書かれない。Drive は変わらない
        assert_eq!(env.mutations(), Vec::<String>::new(), "{name}");
        assert!(
            env.commands_used()
                .iter()
                .all(|c| c == "lsjson" || c == "cat"),
            "{name}"
        );
        assert_eq!(env.drive().nodes.len(), before, "{name}");
        let _ = &f_;
        assert_safe_calls(&env);
    }
}

#[test]
fn an_oversized_manifest_is_not_read() {
    let env = Env::new("big-manifest");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    let mut d = env.drive();
    let p = d.resolve(&["yamareco", FOLDER]).unwrap();
    for n in &mut d.nodes {
        if n.parent == p && n.name == "manifest.json" {
            n.size = 4 * 1024 * 1024 + 1; // 4MB を超える（一覧の大きさ）
        }
    }
    env.save(&d);
    let mut hook = Hook::new(&env);
    let (reason, _) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(reason, ConflictReason::NotManaged);
    assert!(
        !env.commands_used().contains(&"cat".to_owned()),
        "上限を超えるものは読まない"
    );
}

#[test]
fn a_managed_folder_with_added_or_changed_files_is_modified_and_untouched() {
    type Tamper = Box<dyn Fn(&Env)>;
    let cases: Vec<(&str, Tamper)> = vec![
        (
            "extra-file",
            Box::new(|env| {
                let mut d = env.drive();
                d.add_file(&["yamareco", FOLDER], "memo.txt", 3, None);
                env.save(&d);
            }),
        ),
        (
            "extra-file-in-subfolder",
            Box::new(|env| {
                let mut d = env.drive();
                d.add_file(&["yamareco", FOLDER, "notes"], "memo.txt", 3, None);
                env.save(&d);
            }),
        ),
        (
            "extra-empty-folder",
            Box::new(|env| {
                let mut d = env.drive();
                d.mkdir_p(&["yamareco", FOLDER, "notes"]);
                env.save(&d);
            }),
        ),
        (
            "size-differs",
            Box::new(|env| {
                let mut d = env.drive();
                let p = d.resolve(&["yamareco", FOLDER]).unwrap();
                for n in &mut d.nodes {
                    if n.parent == p && n.name == "090000_1.jpg" {
                        n.size += 1;
                    }
                }
                env.save(&d);
            }),
        ),
        (
            "photo-removed",
            Box::new(|env| remove_child(env, &["yamareco", FOLDER], "090000_1.jpg")),
        ),
        (
            "duplicate-photo",
            Box::new(|env| {
                let mut d = env.drive();
                d.add_file(&["yamareco", FOLDER], "090000_1.jpg", 100, None);
                env.save(&d);
            }),
        ),
        (
            "shortcut-inside",
            Box::new(|env| {
                let mut d = env.drive();
                let other = d.mkdir_p(&["elsewhere"]);
                d.add_shortcut(&["yamareco", FOLDER], "link", &other);
                env.save(&d);
            }),
        ),
    ];
    for (name, tamper) in cases {
        let env = Env::new(&format!("modified-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let id = env.seed_managed(Y, FOLDER, &set);
        tamper(&env);
        let before = env.drive().nodes.len();
        let mut hook = Hook::new(&env);
        let (reason, _) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(reason, ConflictReason::Modified, "{name}");
        assert_eq!(env.mutations(), Vec::<String>::new(), "{name}");
        assert_eq!(dest_id(&env).as_deref(), Some(id.as_str()), "{name}");
        assert_eq!(env.drive().nodes.len(), before, "{name}");
        assert!(env.drive().trashed.is_empty(), "{name}");
    }
}

#[test]
fn a_managed_unchanged_folder_is_recognised_in_a_period_with_subfolders_too() {
    let env = Env::new("managed-period");
    let folder = "2030-03-10-11";
    let files = [
        ("2030-03-10/090000_1.jpg", 100),
        ("2030-03-11/090100_2.jpg", 200),
    ];
    let set = env.make_set("p", Y, folder, &files);
    env.seed_managed(Y, folder, &set);
    let layout = env.layout(Y, folder, JOB_A);
    let i = dry_run(&env.rclone(), &layout, &env.published).unwrap();
    assert_eq!(i.managed, Some(Managed::Managed));
    assert_eq!(i.existing_files, 3);
}

// ---- AC10: dry_run ----

#[test]
fn dry_run_calls_only_lsjson_and_cat() {
    let layout = |env: &Env| env.layout(Y, FOLDER, JOB_A);
    // 無い
    let env = Env::new("dry-absent");
    let i = dry_run(&env.rclone(), &layout(&env), &env.published).unwrap();
    assert_eq!(
        (i.target.clone(), i.existing_files, i.managed),
        (Target::Absent, 0, None)
    );
    assert_eq!(env.commands_used(), ["lsjson"]);

    // ある・管理している（cat で manifest を読む）
    let env = Env::new("dry-managed");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let id = env.seed_managed(Y, FOLDER, &set);
    let i = dry_run(&env.rclone(), &layout(&env), &env.published).unwrap();
    assert_eq!(i.target, Target::Exists { id });
    assert_eq!(i.existing_files, 3);
    assert_eq!(i.managed, Some(Managed::Managed));
    assert_eq!(env.commands_used(), ["cat", "lsjson"]);
    assert!(env.mutations().is_empty());

    // ある・管理していない（手作業）／変更あり
    let env = Env::new("dry-manual");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_folder(Y, FOLDER, &set);
    let i = dry_run(&env.rclone(), &layout(&env), &env.published).unwrap();
    assert_eq!(i.managed, Some(Managed::NotManaged));
    let env = Env::new("dry-modified");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    let mut d = env.drive();
    d.add_file(&["yamareco", FOLDER], "memo.txt", 1, None);
    env.save(&d);
    let i = dry_run(&env.rclone(), &layout(&env), &env.published).unwrap();
    assert_eq!(i.managed, Some(Managed::Modified));
    assert!(
        env.commands_used()
            .iter()
            .all(|c| c == "lsjson" || c == "cat")
    );
    assert!(env.mutations().is_empty());
    assert_safe_calls(&env);

    // 照会の失敗は drive_unavailable、同名が複数なら ambiguous
    let env = Env::new("dry-fail");
    env.add_rule("lsjson", 0, u64::MAX, fail(1));
    assert_eq!(
        dry_run(&env.rclone(), &layout(&env), &env.published)
            .unwrap_err()
            .code,
        ErrorCode::DriveUnavailable
    );
    let env = Env::new("dry-ambiguous");
    let mut d = env.drive();
    d.mkdir_dup(&["yamareco"], FOLDER);
    d.mkdir_dup(&["yamareco"], FOLDER);
    env.save(&d);
    assert_eq!(
        dry_run(&env.rclone(), &layout(&env), &env.published)
            .unwrap_err()
            .code,
        ErrorCode::AmbiguousDestination
    );
}

#[test]
fn dry_run_as_a_whole_has_a_deadline_and_a_slow_drive_is_drive_unavailable() {
    // 宛先があると rclone を 5 回ほど呼ぶ。1 回ごとの期限とは別に、全体に期限を掛ける
    let env = Env::new("dry-whole");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    env.add_rule("lsjson", 0, u64::MAX, sleep_ms(400));
    let layout = env.layout(Y, FOLDER, JOB_A);
    let started = Instant::now();
    let err = geoframe_export::publish::dry_run_within(
        &env.rclone(),
        &layout,
        &env.published,
        Duration::from_millis(900),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::DriveUnavailable);
    assert!(started.elapsed() < Duration::from_secs(10));
    // 期限が足りていれば通る
    let ok = geoframe_export::publish::dry_run_within(
        &env.rclone(),
        &layout,
        &env.published,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(ok.managed, Some(Managed::Managed));
}

#[test]
fn the_dry_run_rclone_has_a_thirty_second_limit_per_call() {
    let env = Env::new("dry-limit");
    let settings = &env.settings;
    // 既定の期限は 5 分。dry_run は 30 秒に縮める（実際に待たずに、短い期限での失敗で確かめるのは別の試験）
    let r = Rclone::for_dry_run(settings);
    let text = format!("{r:?}");
    assert!(text.contains("copy_timeout: 30s, timeout: 30s"), "{text}");
    // 設定が 30 秒より短ければ、それを使う
    let mut raw = geoframe_export::settings::RawSettings {
        rclone: Some("/x/rclone".into()),
        rclone_config: Some("/x/state/rclone/rclone.conf".into()),
        state_dir: Some("/x/state".into()),
        ..Default::default()
    };
    raw.rclone_timeout = Some("10".into());
    let short = geoframe_export::settings::Settings::from_raw(&raw).unwrap();
    assert!(format!("{:?}", Rclone::for_dry_run(&short)).contains("timeout: 10s"));
}

// ---- 期限 ----

#[test]
fn the_copy_timeout_kills_rclone_and_cleans_incoming() {
    let env = Env::new("copy-timeout");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("copy", 0, 1, sleep_ms(20_000));
    let rclone = env
        .rclone()
        .with_timeouts(Duration::from_millis(400), Duration::from_secs(30));
    let layout = env.layout(Y, FOLDER, JOB_A);
    let mut hook = Hook::new(&env);
    let started = Instant::now();
    let f = failure(publish(
        &rclone,
        &layout,
        &params(&env, &set, OnExists::Error),
        &mut hook,
    ));
    assert_eq!(f.error.code, ErrorCode::RcloneTimeout);
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(dest_id(&env).is_none());
    assert_eq!(env.drive().trashed.len(), 1, "incoming を片付けた");
}

#[test]
fn the_job_deadline_overrides_the_command_limit_and_cleanup_still_runs() {
    let env = Env::new("job-deadline");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("copy", 0, 1, sleep_ms(20_000));
    let rclone = env
        .rclone()
        .with_deadline(Some(Instant::now() + Duration::from_millis(2500)));
    let layout = env.layout(Y, FOLDER, JOB_A);
    let mut hook = Hook::new(&env);
    let started = Instant::now();
    let f = failure(publish(
        &rclone,
        &layout,
        &params(&env, &set, OnExists::Error),
        &mut hook,
    ));
    assert_eq!(f.error.code, ErrorCode::Timeout);
    assert!(started.elapsed() < Duration::from_secs(15));
    // 期限のあとの後始末は、期限なしで動く
    assert!(dest_id(&env).is_none());
    assert_eq!(env.drive().trashed.len(), 1);
    assert!(!f.leftover_incoming);
}

// ---- 設定: 作業用のフォルダと接頭辞 ----

#[test]
fn settings_with_a_different_remote_or_an_overlapping_work_root_do_not_start() {
    use geoframe_export::settings::{RawSettings, Settings};
    let base = || RawSettings {
        rclone: Some("/x/rclone".into()),
        rclone_config: Some("/x/state/rclone/rclone.conf".into()),
        state_dir: Some("/x/state".into()),
        ..RawSettings::default()
    };
    assert!(Settings::from_raw(&base()).is_ok());
    // remote 名が違う（別の remote や共有ドライブだと、移動がファイル単位のコピーと削除になる）
    for w in ["other:geoframe-export-work", "gdrive2:geoframe-export-work"] {
        let mut r = base();
        r.work_root = Some(w.into());
        assert!(Settings::from_raw(&r).is_err(), "{w}");
    }
    let mut r = base();
    r.remote_yamareco = Some("other:yamareco".into());
    assert!(Settings::from_raw(&r).is_err());
    // 同じ・入れ子（作業用のフォルダが接頭辞の中、接頭辞が作業用のフォルダの中）
    for w in [
        "gdrive:yamareco",
        "gdrive:travel-journal",
        "gdrive:yamareco/work",
        "gdrive:a",
    ] {
        let mut r = base();
        r.work_root = Some(w.into());
        if w == "gdrive:a" {
            r.remote_yamareco = Some("gdrive:a/yamareco".into());
        }
        assert!(Settings::from_raw(&r).is_err(), "{w}");
    }
}

// ---- 呼び出しの安全（環境変数・引数） ----

#[test]
fn rclone_gets_a_minimal_environment_and_no_credential_flags_in_every_call() {
    let env = Env::new("safe");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old = env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env);
    let _ = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert!(env.calls().iter().filter(|c| c["kind"] == "rclone").count() > 10);
    assert_safe_calls(&env);
    // 子プロセスの環境にも引数にも、client_id・client_secret・token の文字は無い
    for c in env.calls().iter().filter(|c| c["kind"] == "rclone") {
        let text = c.to_string().to_lowercase();
        for bad in ["client_id", "client_secret", "access_token", "rclone_"] {
            assert!(!text.contains(bad), "{bad}: {text}");
        }
    }
    let _ = (old, Kind::Dir);
}
