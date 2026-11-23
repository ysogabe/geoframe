//! T4: 宛先の状態、管理しているフォルダの見分け方、incoming → 検証 → 公開、置き換えの退避とゴミ箱、失敗のあとの復旧、dry_run
//! （design.md 4.2、10 章の AC5・AC6・AC10）。試験のプロセスの中の偽の Drive の HTTP サーバー（`tests/common/fake_drive.rs`）だけを使う。
//! 本物の Google・ネットワークには触れない。
//!
//! Drive の操作の記録は、中立の操作名（`list`・`read`・`create_folder`・`upload`・`move`・`trash`。読むだけの `get`・`generate_ids`・
//! `token` も混ざる）で見る（`Env::ops`・`mutating_ops`・`ops_used`・`op_count`）。API の呼び出しの形（Authorization・名前で絞らない・
//! 完全な削除と中身の更新が無い）は `Env::assert_api_calls_safe`。送信は 1 ファイルが 1 回の `upload`（ファイルごと）で、manifest は最後。

mod common;

use std::time::{Duration, Instant};

use common::drive::model::{self, Kind};
use common::drive::*;
use common::fake_drive::{self as fk, do_then_add_folder, late_then_fail};
use geoframe_export::drive::{Managed, Target};
use geoframe_export::error::ErrorCode;
use geoframe_export::publish::{
    ConflictReason, PublishFailure, PublishOutcome, PublishParams, Published, Recovery, Resolution,
    dry_run, publish,
};
use geoframe_export::recovery::{BlockedReason, Progress, Stage};
use geoframe_export::remote::Remote;
use geoframe_export::request::{Destination, OnExists};
use serde_json::{Value, json};

const PHOTOS: &[(&str, u64)] = &[("090000_1.jpg", 100), ("090100_2.jpg", 200)];
const Y: Destination = Destination::Yamareco;

/// 宛先（Y・FOLDER）の公開の記録のファイル。
fn record_path(env: &Env) -> std::path::PathBuf {
    env.published.path_of(
        Y,
        &geoframe_export::folder::FolderName::parse(FOLDER).unwrap(),
    )
}

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
    publish(&env.remote(), &layout, &params(env, set, on_exists), hook)
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
        let f = failure(publish(&env.remote(), &layout, &p, &mut hook));
        assert_eq!(f.error.code, ErrorCode::Internal);
    }
    // 宛先に既にフォルダがあっても同じ。rclone は 1 度も呼ばれず、記録も書かれない
    env.seed_managed(Y, FOLDER, &set);
    let p = PublishParams {
        files: &manifest_only,
        ..params(&env, &set, OnExists::Replace)
    };
    assert_eq!(
        failure(publish(&env.remote(), &layout, &p, &mut hook))
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

    // 各段階の記録が、その段階の Drive の操作より先（1 本の順番で確かめる）。uploading は 2 回。フォルダは `incoming/` と
    // `incoming/<job_id>` の 2 つを作り、写真 2 枚と manifest は 1 ファイルずつ送る
    assert_eq!(
        env.mutating_ops(),
        [
            "record:uploading",
            "create_folder",
            "create_folder",
            "record:uploading",
            "upload",
            "upload",
            "upload",
            "record:verifying",
            "record:publishing",
            "move"
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

    // 呼び方（API）: 写真はファイルごとに送り（並びは問わない）、manifest は最後。送り先は incoming/<job_id> の下の葉の名前。
    // 移動は ID を保った 1 回の更新（親と名前を変える）で、宛先のフォルダの ID は incoming の ID のまま（上で確かめた）
    let incoming = format!("geoframe-export-work/incoming/{JOB_A}");
    let sent = env.uploaded_subjects();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2], format!("{incoming}/manifest.json"));
    let mut photos = sent[..2].to_vec();
    photos.sort();
    assert_eq!(
        photos,
        [
            format!("{incoming}/090000_1.jpg"),
            format!("{incoming}/090100_2.jpg")
        ]
    );
    let moves: Vec<Value> = env
        .http_calls()
        .into_iter()
        .filter(|c| c["op"] == "move")
        .collect();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0]["subject"], incoming);
    // 共有・リンクの操作は呼ばない。使ったのは決まった操作だけ
    assert_eq!(
        env.ops_used(),
        [
            "create_folder",
            "generate_ids",
            "list",
            "move",
            "token",
            "upload"
        ]
    );
    // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
    env.assert_api_calls_safe();
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
        &env.remote(),
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
    // 中身のあるフォルダ・空のフォルダ。どちらも、フォルダの作成・送信・移動・ゴミ箱へ移すは呼ばれず、記録も書かれない
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
        assert_eq!(env.mutating_ops(), Vec::<String>::new(), "{name}");
        assert_eq!(env.ops_used(), ["list", "token"], "{name}");
        assert_eq!(dest_id(&env).as_deref(), Some(id.as_str()));
        // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
        env.assert_api_calls_safe();
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
    env.assert_no_mutations();
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
        env.mutating_ops(),
        [
            "record:uploading",
            "create_folder",
            "create_folder",
            "record:uploading",
            "upload",
            "upload",
            "upload",
            "record:verifying",
            "record:retiring",
            // `replaced/` が無いので、退避の前に作る
            "create_folder",
            "move",
            "record:publishing",
            "move",
            "record:trashing",
            "trash"
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
    // ゴミ箱の中のものは、Drive の一覧には出ない（模型には、ゴミ箱の印つきで残る）
    let replaced = d.resolve(&["geoframe-export-work", "replaced"]).unwrap();
    assert!(d.children(&replaced).iter().all(|n| d.is_trashed(&n.id)));
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

    // 呼び方（API）: 退避は旧フォルダの ID を、宛先の親から replaced/ へ、名前を <種類>-<フォルダ名>-<job_id> にして移す。
    // ゴミ箱へは、同じ旧フォルダの ID を `trashed: true` の更新で移す（完全な削除ではない）
    let moves: Vec<Value> = env
        .http_calls()
        .into_iter()
        .filter(|c| c["op"] == "move")
        .collect();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[0]["subject"], format!("yamareco/{FOLDER}"));
    assert_eq!(
        moves[1]["subject"],
        format!("geoframe-export-work/incoming/{JOB_A}")
    );
    let retired = format!("geoframe-export-work/replaced/yamareco-{FOLDER}-{JOB_A}");
    let trashes: Vec<Value> = env
        .http_calls()
        .into_iter()
        .filter(|c| c["op"] == "trash")
        .collect();
    assert_eq!(trashes.len(), 1);
    assert_eq!(trashes[0]["subject"], retired);
    assert_eq!(
        env.drive().path_of(&old_id).as_deref(),
        Some(retired.as_str()),
        "ID が保たれたまま、退避先にある（ゴミ箱の中）"
    );
    env.assert_api_calls_safe();
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
    assert_eq!(env.mutating_ops(), ["record:uploading"]);

    // uploading の 2 回目（ID の記録）: 写真は送らない。ID を記録できていないので、作ったフォルダは名前だけでは消さず、
    // 残りものとして知らせる
    let env = Env::new("rec-up2");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Uploading, 2);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert_eq!(
        env.mutating_ops(),
        [
            "record:uploading",
            "create_folder",
            "create_folder",
            "record:uploading"
        ]
    );
    assert!(f.leftover_incoming);
    let left = env
        .drive()
        .resolve(&["geoframe-export-work", "incoming", JOB_A])
        .expect("消していない");
    assert_eq!(f.leftover_incoming_id.as_deref(), Some(left.as_str()));
    assert!(env.drive().trashed.is_empty() && env.drive().deleted.is_empty());

    // verifying: 検証の一覧も、公開もしない
    let env = Env::new("rec-ver");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Verifying, 1);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert!(!env.mutating_ops().contains(&"move".to_owned()));
    assert!(dest_id(&env).is_none());

    // 新規の publishing: 移動しない。incoming を片付ける
    let env = Env::new("rec-pub");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env).fail_at(Stage::Publishing, 1);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert!(!env.mutating_ops().contains(&"move".to_owned()));
    assert!(dest_id(&env).is_none());
    assert_eq!(env.drive().trashed.len(), 1);

    // 置き換えの retiring: 旧フォルダを動かさない
    let env = Env::new("rec-ret");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env).fail_at(Stage::Retiring, 1);
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Internal);
    assert!(!env.mutating_ops().contains(&"move".to_owned()));
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
    env.add_rule("move", 0, 1, do_then_fail());
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    assert_eq!(p.folder_id, incoming_id);
    assert_eq!(dest_id(&env).as_deref(), Some(incoming_id.as_str()));
    assert_eq!(env.op_count("move"), 1, "もう一度移動しない");
    assert!(p.recorded);
    assert_eq!(
        files_under(&env, &["yamareco", FOLDER]),
        expected_files(&set)
    );

    // 置き換え: 退避の応答が失われた → 前へ進める（incoming を宛先へ移し、旧フォルダをゴミ箱へ）
    let env = Env::new("lost-retire");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("move", 0, 1, do_then_fail());
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
    env.add_rule("move", 1, 1, do_then_fail());
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(env.drive().trashed, [old_id]);
    assert_eq!(env.op_count("move"), 2);
    assert_eq!(dest_id(&env).as_deref(), Some(p.folder_id.as_str()));

    // 置き換え: ゴミ箱への移動の応答が失われた → 一覧でもう無いと分かり、成功（旧フォルダは残っていない）
    let env = Env::new("lost-purge");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    env.add_rule("trash", 0, 1, do_then_fail());
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
    env.add_rule("trash", 0, u64::MAX, fail(1));
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
    env.add_rule("move", 0, u64::MAX, late_then_fail(3));
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    assert_eq!(p.folder_id, incoming_id);
    assert_eq!(dest_id(&env).as_deref(), Some(incoming_id.as_str()));
    // 2 つの観察が食い違ったので、やり直した（失敗のあとの lsjson が、1 回分の観察の数より多い）
    let seq = env.ops();
    let after_move = seq.iter().rposition(|c| c == "move").unwrap();
    let lists_after = seq[after_move..].iter().filter(|c| *c == "list").count();
    assert!(lists_after >= 18, "{lists_after}");
    assert!(
        !seq[after_move..].contains(&"trash".to_owned()),
        "公開済みの incoming を消さない"
    );
}

#[test]
fn a_move_that_never_shows_up_is_treated_as_not_done_and_incoming_is_cleaned() {
    // 2 回の観察が同じなら、その状態に表を当てはめる（移動は実行されなかったと見る）。失敗は元のまま返る
    let env = Env::new("stale");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("move", 0, u64::MAX, late_then_fail(100_000));
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::RcloneFailed);
    assert!(f.blocked.is_none() && !f.leftover_incoming);
    assert!(dest_id(&env).is_none());
    assert_eq!(env.drive().trashed.len(), 1);
}

// ---- 照合できない → blocked ----

#[test]
fn a_same_named_folder_created_right_after_the_publish_move_is_found_by_the_id_check_and_blocks() {
    // 公開の移動は済んだが、その直後（確かめる前）に、オーナーが宛先に同じ名前のフォルダを作った。移動は ID で行うので、写真が足される
    // ことはなく、宛先に同名のフォルダが 2 つ並ぶ。移したあとの照合（宛先が incoming の ID のフォルダ 1 つだけ）が見つけて、blocked にする
    let env = Env::new("dup-after-publish");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("move", 0, 1, do_then_add_folder(&["yamareco"], FOLDER));
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    let info = f.blocked.expect("blocked の記録");
    assert_eq!(info.reason, BlockedReason::PublishMismatch);
    assert_eq!(info.progress.stage, Stage::Publishing);
    let incoming_id = hook.records[1].incoming_id.clone().unwrap();
    let o = info.observed.clone().expect("見えていた状態");
    assert_eq!(o.dest.len(), 2, "同名のフォルダが並ぶ");
    assert!(o.dest.iter().any(|s| s.id == incoming_id));
    assert!(o.dest.iter().any(|s| s.id != incoming_id));
    // それ以上は何もしない（ゴミ箱なし、移動も 1 回だけ）
    assert_eq!(env.op_count("move"), 1);
    assert!(env.op_count("trash") == 0);
    let text = info.describe(&env.layout(Y, FOLDER, JOB_A));
    assert!(text.contains("publish_mismatch"), "{text}");
    assert!(!text.contains("足された写真"), "{text}");
}

#[test]
fn a_same_named_folder_created_right_after_the_retire_move_blocks_before_anything_reaches_the_trash()
{
    let env = Env::new("dup-after-retire");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    // 退避の移動（最初の move）の直後に、退避先に同じ名前のフォルダができた
    env.add_rule(
        "move",
        0,
        1,
        do_then_add_folder(
            &["geoframe-export-work", "replaced"],
            &format!("yamareco-{FOLDER}-{JOB_A}"),
        ),
    );
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    let info = f.blocked.unwrap();
    assert_eq!(info.reason, BlockedReason::RetireMismatch);
    assert_eq!(info.progress.stage, Stage::Retiring);
    assert_eq!(info.progress.old_id.as_deref(), Some(old_id.as_str()));
    let o = info.observed.clone().unwrap();
    assert!(o.dest.is_empty());
    assert_eq!(o.replaced.len(), 2, "同名のフォルダが並ぶ");
    assert!(o.replaced.iter().any(|s| s.id == old_id), "旧フォルダは ID のまま退避先にある");
    assert_eq!(env.op_count("move"), 1);
    assert!(env.op_count("trash") == 0);
    assert!(env.drive().trashed.is_empty());
    // recover の表示: 見えていた ID と、戻す手順
    let text = info.describe(&env.layout(Y, FOLDER, JOB_A));
    assert!(text.contains(&old_id), "{text}");
    assert!(text.contains("戻す手順"), "{text}");
    assert!(
        text.contains(&env.layout(Y, FOLDER, JOB_A).dest_path()),
        "{text}"
    );
    assert!(!text.contains("ファイル単位"), "{text}");
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
    assert!(env.op_count("trash") == 0, "ゴミ箱へは移さない");
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
    assert_eq!(env.op_count("move"), 1, "公開の移動はしない");
    assert!(env.op_count("trash") == 0);
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
    env.add_rule("move", 0, 1, fail(1));
    // 失敗のあとの一覧がすべて失敗する
    let mut c = env.ctl();
    c.rules.push(model::Rule {
        op: "list".into(),
        contains: None,
        // 失敗した移動より前の lsjson（宛先の状態 2、incoming の事前確認 1〔作業用のフォルダに incoming がまだ無い〕、
        // incoming の ID 2、検証 2、確かめ直し 2、公開の直前 2）は通し、失敗のあとの一覧（復旧の観察）をすべて失敗させる
        skip: 11,
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
        assert!(env.op_count("move") == 0, "{name}: 移動しない");
        assert_eq!(env.drive().trashed.len(), 1, "{name}: incoming を片付けた");
        // 中止の片付けに入ったことを、ゴミ箱へ移す前に記録する（`publishing` から中止したときだけ。それ以外は前のままと見える）
        let seq = env.mutating_ops();
        let aborting = seq.iter().position(|c| c == "record:aborting");
        if stage == Stage::Publishing {
            let rec = hook.records.last().unwrap();
            assert_eq!(rec.stage, Stage::Aborting, "{name}");
            assert_eq!(rec.incoming_id, hook.records[1].incoming_id, "{name}");
            let purge = seq.iter().position(|c| c == "trash").unwrap();
            assert!(aborting.is_some_and(|a| a < purge), "{name}: {seq:?}");
        } else {
            assert!(aborting.is_none(), "{name}: {seq:?}");
        }
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
    assert!(env.op_count("move") == 0);
    assert_eq!(dest_id(&env).as_deref(), Some(old_id.as_str()));
}

#[test]
fn a_verification_mismatch_trashes_incoming_and_fails() {
    // 送ったあとで、incoming のファイルのサイズが変わった（または足りない・余分がある）
    for (name, tamper) in [("size", 0), ("missing", 1), ("extra", 2), ("md5", 3)] {
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
                2 => {
                    d.add_file(
                        &["geoframe-export-work", "incoming", JOB_A],
                        "extra.jpg",
                        1,
                        None,
                    );
                }
                // 同じ名前・同じ大きさのまま中身が違う（MD5 だけが違う）
                _ => {
                    let id = d
                        .children(&inc)
                        .iter()
                        .find(|n| n.name == "090000_1.jpg")
                        .unwrap()
                        .id
                        .clone();
                    d.set_md5(&id, &"f".repeat(32));
                }
            }
            d.save(&fake);
        });
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(f.error.code, ErrorCode::VerifyMismatch, "{name}");
        assert!(env.op_count("move") == 0, "{name}");
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
        }
        for on_exists in [OnExists::Error, OnExists::Replace] {
            let mut hook = Hook::new(&env);
            let f = failure(run(&env, &set, on_exists, &mut hook));
            assert_eq!(f.error.code, ErrorCode::AmbiguousDestination, "{name}");
        }
        assert!(
            env.mutating_ops().is_empty(),
            "{name}: {:?}",
            env.mutating_ops()
        );
    }
}

#[test]
fn a_failed_query_is_drive_unavailable_and_never_read_as_absent() {
    // API の失敗（500 を再試行しきる・404・403）、決められた形でない応答、ID の無い項目
    let cases: Vec<(&str, Value)> = vec![
        ("http500", fail(500)),
        ("http404", fk::status(404)),
        ("http403", fk::status(403)),
        ("garbage", garbage("not json")),
        ("wrong-type", garbage(r#"{"files": 1}"#)),
        (
            "no-id",
            garbage(r#"{"files":[{"name":"a","mimeType":"x/y","size":"1"}]}"#),
        ),
        (
            "empty-id",
            garbage(r#"{"files":[{"id":"","name":"a","mimeType":"x/y","size":"1"}]}"#),
        ),
    ];
    for (name, effect) in cases {
        let env = Env::new(&format!("unavail-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        env.add_rule("list", 0, u64::MAX, effect);
        let mut hook = Hook::new(&env);
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(f.error.code, ErrorCode::DriveUnavailable, "{name}");
        assert!(env.mutating_ops().is_empty(), "{name}: 何も書かない");
    }
    // 宛先の親フォルダそのものが無い（一覧が 404）。「宛先が無い」とは読まない
    let env = Env::new("unavail-no-prefix");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut d = env.drive();
    let id = d.resolve(&["yamareco"]).unwrap();
    d.nodes.retain(|n| n.id != id);
    env.save(&d);
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::DriveUnavailable);
    env.assert_no_mutations();
}

#[test]
fn a_query_that_times_out_is_drive_unavailable() {
    let env = Env::new("unavail-timeout");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("list", 0, u64::MAX, sleep_ms(20_000));
    let rclone = env
        .remote()
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
                        files: Some(set.recorded()),
                        previous: None,
                    })
                    .unwrap();
            }),
        ),
        // 公開の記録が壊れている
        (
            "corrupt-record",
            Box::new(|env, set| {
                env.seed_managed(Y, FOLDER, set);
                std::fs::write(record_path(env), "{not json").unwrap();
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
        assert_eq!(env.mutating_ops(), Vec::<String>::new(), "{name}");
        assert!(
            env.ops_used()
                .iter()
                .all(|c| c == "list" || c == "read" || c == "token"),
            "{name}"
        );
        assert_eq!(env.drive().nodes.len(), before, "{name}");
        let _ = &f_;
        // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
        env.assert_api_calls_safe();
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
        !env.ops_used().contains(&"read".to_owned()),
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
        assert_eq!(env.mutating_ops(), Vec::<String>::new(), "{name}");
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
    let i = dry_run(&env.remote(), &layout, &env.published).unwrap();
    assert_eq!(i.managed, Some(Managed::Managed));
    assert_eq!(i.existing_files, 3);
}

// ---- AC10: dry_run ----

#[test]
fn dry_run_only_lists_and_reads() {
    let layout = |env: &Env| env.layout(Y, FOLDER, JOB_A);
    // 無い
    let env = Env::new("dry-absent");
    let i = dry_run(&env.remote(), &layout(&env), &env.published).unwrap();
    assert_eq!(
        (i.target.clone(), i.existing_files, i.managed),
        (Target::Absent, 0, None)
    );
    assert_eq!(env.ops_used(), ["list", "token"]);

    // ある・管理している（manifest を読む）
    let env = Env::new("dry-managed");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let id = env.seed_managed(Y, FOLDER, &set);
    let i = dry_run(&env.remote(), &layout(&env), &env.published).unwrap();
    assert_eq!(i.target, Target::Exists { id });
    assert_eq!(i.existing_files, 3);
    assert_eq!(i.managed, Some(Managed::Managed));
    assert_eq!(env.ops_used(), ["list", "read", "token"]);
    env.assert_no_mutations();

    // ある・管理していない（手作業）／変更あり
    let env = Env::new("dry-manual");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_folder(Y, FOLDER, &set);
    let i = dry_run(&env.remote(), &layout(&env), &env.published).unwrap();
    assert_eq!(i.managed, Some(Managed::NotManaged));
    let env = Env::new("dry-modified");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    let mut d = env.drive();
    d.add_file(&["yamareco", FOLDER], "memo.txt", 1, None);
    env.save(&d);
    let i = dry_run(&env.remote(), &layout(&env), &env.published).unwrap();
    assert_eq!(i.managed, Some(Managed::Modified));
    assert!(
        env.ops_used()
            .iter()
            .all(|c| c == "list" || c == "read" || c == "token")
    );
    env.assert_no_mutations();
    // API の呼び出しの形（Authorization が付く・名前で絞らない・完全な削除と中身の更新が無い）
    env.assert_api_calls_safe();

    // 照会の失敗は drive_unavailable、同名が複数なら ambiguous
    let env = Env::new("dry-fail");
    env.add_rule("list", 0, u64::MAX, fail(1));
    assert_eq!(
        dry_run(&env.remote(), &layout(&env), &env.published)
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
        dry_run(&env.remote(), &layout(&env), &env.published)
            .unwrap_err()
            .code,
        ErrorCode::AmbiguousDestination
    );
}

#[test]
fn dry_run_as_a_whole_has_a_deadline_and_a_slow_drive_is_drive_unavailable() {
    // 宛先があると一覧を 2 回と読み出しを呼ぶ。1 回ごとの期限とは別に、全体に期限を掛ける
    let env = Env::new("dry-whole");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    env.add_rule("list", 0, u64::MAX, sleep_ms(600));
    let layout = env.layout(Y, FOLDER, JOB_A);
    let started = Instant::now();
    let err = geoframe_export::publish::dry_run_within(
        &env.remote(),
        &layout,
        &env.published,
        Duration::from_millis(900),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::DriveUnavailable);
    assert!(started.elapsed() < Duration::from_secs(10));
    // 期限が足りていれば通る
    let ok = geoframe_export::publish::dry_run_within(
        &env.remote(),
        &layout,
        &env.published,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(ok.managed, Some(Managed::Managed));
}

#[test]
fn the_dry_run_has_a_thirty_second_limit_per_call() {
    let env = Env::new("dry-limit");
    // 既定の期限は 5 分。dry_run は 30 秒に縮める（実際に待たずに、短い期限での失敗で確かめるのは別の試験）
    let base = env
        .remote()
        .with_timeouts(Duration::from_secs(45 * 60), Duration::from_secs(5 * 60));
    let text = format!("{:?}", base.for_dry_run());
    assert!(
        text.contains("upload_timeout: 30s") && text.contains("query_timeout: 30s"),
        "{text}"
    );
    // 設定が 30 秒より短ければ、それを使う
    let short = base.with_timeouts(Duration::from_secs(10), Duration::from_secs(10));
    let text = format!("{:?}", short.for_dry_run());
    assert!(text.contains("query_timeout: 10s"), "{text}");
}

// ---- 期限 ----

#[test]
fn the_copy_timeout_kills_rclone_and_cleans_incoming() {
    let env = Env::new("copy-timeout");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.add_rule("upload", 0, 1, sleep_ms(20_000));
    let rclone = env
        .remote()
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
    env.add_rule("upload", 0, 1, sleep_ms(20_000));
    let rclone = env
        .remote()
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

// ---- 設定: 3 つのフォルダ ----

#[test]
fn settings_with_the_same_folder_twice_do_not_start() {
    use geoframe_export::settings::{RawSettings, Settings};
    let env = Env::new("same-folders");
    let base = || RawSettings {
        state_dir: Some(env.state.to_string_lossy().into_owned()),
        drive_credentials: Some(env.credentials.to_string_lossy().into_owned()),
        folder_yamareco: Some(env.yamareco_id.clone()),
        folder_travel_journal: Some(env.travel_journal_id.clone()),
        work_folder: Some(env.work_id.clone()),
        ..RawSettings::default()
    };
    assert!(Settings::from_raw(&base()).is_ok());
    // 宛先どうし・宛先と作業用のフォルダが同じ ID（祖先・子孫の関係は、起動時に Drive へ問い合わせて断る。startup_cli.rs）
    let mut r = base();
    r.folder_travel_journal = Some(env.yamareco_id.clone());
    assert!(Settings::from_raw(&r).is_err());
    let mut r = base();
    r.work_folder = Some(env.yamareco_id.clone());
    assert!(Settings::from_raw(&r).is_err());
    let mut r = base();
    r.work_folder = Some(env.travel_journal_id.clone());
    assert!(Settings::from_raw(&r).is_err());
}

// ---- 呼び出しの安全（環境変数・引数） ----

#[test]
fn the_api_calls_carry_the_token_only_in_the_header_and_the_log_never_holds_a_credential() {
    let env = Env::new("safe");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old = env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env);
    let _ = published(run(&env, &set, OnExists::Replace, &mut hook));
    assert!(env.http_calls().len() > 10);
    env.assert_api_calls_safe();
    // 記録（偽の Drive が書いた要求の記録）にも、資格情報の値・トークンの値は無い
    let log = std::fs::read_to_string(env.fake_dir.join("calls.log")).unwrap();
    for secret in [
        fk::FAKE_CLIENT_SECRET,
        fk::FAKE_REFRESH_TOKEN,
        fk::FAKE_CLIENT_ID,
        "access_token",
    ] {
        assert!(!log.contains(secret), "{secret}");
    }
    let _ = (old, Kind::Dir);
}

// ---- Google のファイル（サイズ不明）とショートカット（実 rclone の一覧の形） ----

#[test]
fn native_google_files_in_a_managed_folder_make_it_modified_and_nothing_is_touched() {
    // ネイティブの文書（Size -1）・エクスポートできないフォーム（--drive-show-all-gdocs が無いと一覧に出ない）
    for (name, form) in [("gdoc", false), ("form", true)] {
        let env = Env::new(&format!("gdocs-{name}"));
        let old_set = env.make_set("old", Y, FOLDER, PHOTOS);
        let old_id = env.seed_managed(Y, FOLDER, &old_set);
        let mut d = env.drive();
        if form {
            d.add_form(&["yamareco", FOLDER], "survey");
        } else {
            d.add_gdoc(&["yamareco", FOLDER], "notes");
        }
        env.save(&d);
        let set = env.make_set("new", Y, FOLDER, PHOTOS);
        let mut hook = Hook::new(&env);
        let (reason, files) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(reason, ConflictReason::Modified, "{name}");
        assert_eq!(files, 4, "{name}: 写真 2・manifest・足されたファイル");
        assert!(env.mutating_ops().is_empty(), "{name}");
        assert_eq!(dest_id(&env).as_deref(), Some(old_id.as_str()));
        // dry_run も同じ判定
        let layout = env.layout(Y, FOLDER, JOB_A);
        let i = dry_run(&env.remote().for_dry_run(), &layout, &env.published).unwrap();
        assert_eq!(i.managed, Some(Managed::Modified), "{name}");
    }
}

#[test]
fn unrelated_unknown_size_entries_in_a_parent_do_not_fail_the_whole_listing() {
    let env = Env::new("gdocs-parent");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut d = env.drive();
    d.add_gdoc(&["yamareco"], "memo");
    d.add_form(&["yamareco"], "form");
    d.add_gdoc(&["geoframe-export-work"], "scratch");
    d.add_gdoc(&["geoframe-export-work", "incoming"], "stray");
    env.save(&d);
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(dest_id(&env).as_deref(), Some(p.folder_id.as_str()));
    // 宛先と同じ名前のフォームは、一覧に出る（付けないと「無い」ように見える）ので、ambiguous
    let env = Env::new("gdocs-same-name");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut d = env.drive();
    d.add_form(&["yamareco"], FOLDER);
    env.save(&d);
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::AmbiguousDestination);
    env.assert_no_mutations();
}

#[test]
fn a_google_file_that_appears_in_the_incoming_fails_the_verification() {
    let env = Env::new("gdocs-incoming");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let fake = env.fake_dir.clone();
    let mut hook = Hook::new(&env).mutate_after(Stage::Verifying, move || {
        let mut d = model::Drive::load(&fake);
        d.add_gdoc(&["geoframe-export-work", "incoming", JOB_A], "extra");
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::VerifyMismatch);
    assert!(env.op_count("move") == 0);
    assert!(dest_id(&env).is_none());
}

#[test]
fn a_google_file_found_in_the_destination_after_the_move_blocks() {
    // 検証のあと、移す前に incoming へ文書が足された → 公開後の照合で一式と一致せず blocked（ゴミ箱へは進まない）
    let env = Env::new("gdocs-after-move");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let fake = env.fake_dir.clone();
    let mut hook = Hook::new(&env).mutate_after(Stage::Publishing, move || {
        let mut d = model::Drive::load(&fake);
        d.add_gdoc(&["geoframe-export-work", "incoming", JOB_A], "extra");
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    assert_eq!(
        f.blocked.expect("blocked").reason,
        BlockedReason::PublishMismatch
    );
    assert!(env.op_count("trash") == 0);
}

#[test]
fn resolved_and_dangling_shortcuts_are_classified_not_rejected() {
    // 解決されたショートカット（複合 ID）とリンク切れが、宛先と同じ名前なら ambiguous
    for (name, dangling) in [("resolved", false), ("dangling", true)] {
        let env = Env::new(&format!("sc-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let mut d = env.drive();
        let target = if dangling {
            "fidMISSING".to_owned()
        } else {
            d.mkdir_p(&["elsewhere"])
        };
        d.add_shortcut(&["yamareco"], FOLDER, &target);
        env.save(&d);
        let mut hook = Hook::new(&env);
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        assert_eq!(f.error.code, ErrorCode::AmbiguousDestination, "{name}");
        assert!(env.mutating_ops().is_empty(), "{name}");
        // API の一覧では、ショートカットは解決されずに 1 件として出る。同じ場所を 2 回一覧して差を見ることはしない
        let parent_lists = env
            .http_calls()
            .iter()
            .filter(|c| c["op"] == "list" && c["subject"] == "yamareco")
            .count();
        assert_eq!(parent_lists, 1, "{name}");
        env.assert_api_calls_safe();
    }
    // 名前が違うショートカットが親にあっても、一覧全体は断らない（名前が一致するものだけを使う）
    let env = Env::new("sc-unrelated");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut d = env.drive();
    let other = d.mkdir_p(&["elsewhere"]);
    d.add_shortcut(&["yamareco"], "linked", &other);
    d.add_shortcut(&["yamareco"], "broken", "fidMISSING");
    d.add_shortcut(&["geoframe-export-work", "incoming"], "linked2", &other);
    env.save(&d);
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(dest_id(&env).as_deref(), Some(p.folder_id.as_str()));
}

#[test]
fn a_shortcut_inside_a_managed_folder_makes_it_modified() {
    for (name, dangling) in [("resolved", false), ("dangling", true)] {
        let env = Env::new(&format!("sc-inside-{name}"));
        let old_set = env.make_set("old", Y, FOLDER, PHOTOS);
        let old_id = env.seed_managed(Y, FOLDER, &old_set);
        let mut d = env.drive();
        let target = if dangling {
            "fidMISSING".to_owned()
        } else {
            d.mkdir_p(&["elsewhere"])
        };
        d.add_shortcut(&["yamareco", FOLDER], "link", &target);
        env.save(&d);
        let set = env.make_set("new", Y, FOLDER, PHOTOS);
        let mut hook = Hook::new(&env);
        let (reason, _) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(reason, ConflictReason::Modified, "{name}");
        assert!(env.mutating_ops().is_empty(), "{name}");
        assert_eq!(dest_id(&env).as_deref(), Some(old_id.as_str()));
    }
}

// ---- ゴミ箱へ移す前の照合（purge はパスで指すので、中身も確かめ直す） ----

#[test]
fn the_old_folder_is_not_trashed_when_something_was_added_to_it_after_the_check() {
    let env = Env::new("trash-added");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let old_id = env.seed_managed(Y, FOLDER, &set);
    let fake = env.fake_dir.clone();
    // 退避のあと、trashing の記録の直後に、オーナーが旧フォルダ（退避先）にメモを足した
    let mut hook = Hook::new(&env).mutate_after(Stage::Trashing, move || {
        let mut d = model::Drive::load(&fake);
        d.add_file(
            &[
                "geoframe-export-work",
                "replaced",
                &format!("yamareco-{FOLDER}-{JOB_A}"),
            ],
            "owner-memo.txt",
            3,
            None,
        );
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    let info = f.blocked.expect("blocked");
    assert_eq!(info.reason, BlockedReason::RetiredContentChanged);
    assert_eq!(info.progress.stage, Stage::Trashing);
    assert!(env.op_count("trash") == 0);
    assert!(env.drive().trashed.is_empty());
    // 新しい一式は公開済み。旧フォルダ（メモごと）は退避先に残っている
    assert_ne!(dest_id(&env).as_deref(), Some(old_id.as_str()));
    let o = info.observed.clone().unwrap();
    assert_eq!(o.replaced.len(), 1);
    assert_eq!(o.replaced[0].id, old_id);
    let text = info.describe(&env.layout(Y, FOLDER, JOB_A));
    assert!(
        text.contains("retired_content_changed") && text.contains(&old_id),
        "{text}"
    );
    // 旧 manifest は退避先の中の manifest.json を読み直している（ID で読む。対象のパスが退避先の下）
    let reads: Vec<String> = env
        .http_calls()
        .iter()
        .filter(|c| c["op"] == "read")
        .map(|c| c["subject"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(
        reads
            .iter()
            .any(|p| p.contains("/replaced/") && p.ends_with("/manifest.json")),
        "{reads:?}"
    );
}

#[test]
fn the_old_folder_is_not_trashed_when_replaced_shows_another_id_or_a_changed_file() {
    // 同じ名前の別のフォルダに入れ替わった（ID が違う）→ 中身が同じでも、ゴミ箱へ移さない
    let env = Env::new("trash-swapped");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let _old_id = env.seed_managed(Y, FOLDER, &set);
    let fake = env.fake_dir.clone();
    let name = format!("yamareco-{FOLDER}-{JOB_A}");
    let set_files = set.files.clone();
    let manifest = set.manifest.clone();
    let mut hook = Hook::new(&env).mutate_after(Stage::Trashing, move || {
        let mut d = model::Drive::load(&fake);
        let old = d
            .resolve(&["geoframe-export-work", "replaced", &name])
            .unwrap();
        d.purge(&old, false);
        let path = ["geoframe-export-work", "replaced", &name];
        for f in &set_files {
            let content = (f.path == "manifest.json").then_some(manifest.as_str());
            match f.path.rsplit_once('/') {
                Some((dir, leaf)) => {
                    d.add_file(&[path[0], path[1], path[2], dir], leaf, f.size, content);
                }
                None => {
                    d.add_file(&path, &f.path, f.size, content);
                }
            }
        }
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    let info = f.blocked.expect("blocked");
    assert_eq!(info.reason, BlockedReason::TrashMismatch);
    assert!(env.op_count("trash") == 0);

    // 写真のサイズが違う
    let env = Env::new("trash-resized");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    let fake = env.fake_dir.clone();
    let name = format!("yamareco-{FOLDER}-{JOB_A}");
    let mut hook = Hook::new(&env).mutate_after(Stage::Trashing, move || {
        let mut d = model::Drive::load(&fake);
        let dir = d
            .resolve(&["geoframe-export-work", "replaced", &name])
            .unwrap();
        for n in &mut d.nodes {
            if n.parent == dir && n.name == "090000_1.jpg" {
                n.size += 1;
            }
        }
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(
        f.blocked.expect("blocked").reason,
        BlockedReason::RetiredContentChanged
    );
    assert!(env.op_count("trash") == 0);
}

#[test]
fn a_failed_purge_succeeds_with_the_old_folder_left_only_if_it_is_confirmed_in_replaced() {
    // purge が失敗し、そのあとの一覧もできない → 旧フォルダが退避先にあると確かめられないので、成功にせず blocked
    let probe = Env::new("purge-unlistable-probe");
    let set = probe.make_set("s", Y, FOLDER, PHOTOS);
    probe.seed_managed(Y, FOLDER, &set);
    probe.add_rule("trash", 0, u64::MAX, fail(1));
    let mut hook = Hook::new(&probe);
    let _ = published(run(&probe, &set, OnExists::Replace, &mut hook));
    // purge までに、`replaced` を含む場所への lsjson が何回あったか
    let mut before_purge = 0;
    for c in probe.calls() {
        if c["op"] == "trash" {
            break;
        }
        if c["op"] == "list" && c["subject"].as_str().is_some_and(|p| p.ends_with("/replaced")) {
            before_purge += 1;
        }
    }
    assert!(before_purge > 0);

    let env = Env::new("purge-unlistable");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    env.add_rule("trash", 0, u64::MAX, fail(1));
    let mut c = env.ctl();
    c.rules.push(model::Rule {
        op: "list".into(),
        contains: Some("/replaced".into()),
        skip: before_purge,
        times: u64::MAX,
        seen: 0,
        effect: fail(1),
    });
    c.save(&env.fake_dir);
    let mut hook = Hook::new(&env);
    let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(f.error.code, ErrorCode::Blocked);
    assert_eq!(
        f.blocked.expect("blocked").reason,
        BlockedReason::Unobservable
    );
}

// ---- 公開の記録に書けなかった ----

#[test]
fn a_published_result_whose_record_failed_carries_what_is_needed_to_retry_it() {
    let env = Env::new("unrecorded");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    // 公開の記録が読めない（壊れている）→ 書かない
    std::fs::create_dir_all(env.published.dir()).unwrap();
    std::fs::write(record_path(&env), b"not json").unwrap();
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    assert!(!p.recorded);
    assert_eq!(p.record.folder_id, p.folder_id);
    assert_eq!(p.record.folder.as_str(), FOLDER);
    assert_eq!(p.record.destination, Y);
    assert_eq!(p.record.job_id.as_str(), JOB_A);
    assert!(p.retry_record(&env.published).is_err());
    // 記録が直れば、結果に残した行で書き直せる
    std::fs::remove_file(record_path(&env)).unwrap();
    p.retry_record(&env.published).unwrap();
    let f = geoframe_export::folder::FolderName::parse(FOLDER).unwrap();
    assert_eq!(
        env.published.find_id(Y, &f).as_deref(),
        Some(p.folder_id.as_str())
    );
}

// ---- incoming は、記録した ID と一致するときだけ消す ----

#[test]
fn the_incoming_that_no_longer_matches_the_recorded_id_is_left_and_reported() {
    let env = Env::new("incoming-swapped");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let fake = env.fake_dir.clone();
    // 検証の前に、incoming が別のフォルダ（空）に入れ替わった（ID が違う）
    let mut hook = Hook::new(&env).mutate_after(Stage::Verifying, move || {
        let mut d = model::Drive::load(&fake);
        let inc = d
            .resolve(&["geoframe-export-work", "incoming", JOB_A])
            .unwrap();
        d.purge(&inc, false);
        d.mkdir_p(&["geoframe-export-work", "incoming", JOB_A]);
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::VerifyMismatch);
    assert!(f.leftover_incoming, "消さずに、残りものとして知らせる");
    let recorded = hook.records.last().unwrap().incoming_id.clone().unwrap();
    assert_eq!(f.leftover_incoming_id.as_deref(), Some(recorded.as_str()));
    assert!(env.op_count("trash") == 0);
    assert!(env.drive().trashed.is_empty());
    assert!(
        env.drive()
            .resolve(&["geoframe-export-work", "incoming", JOB_A])
            .is_some()
    );
}

// ---- 退避の前に、退避先に同名の項目が無いことを確かめる ----

#[test]
fn a_same_named_item_already_in_replaced_stops_the_retire_before_anything_moves() {
    for as_file in [false, true] {
        let env = Env::new(&format!("retire-occupied-{as_file}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let old_id = env.seed_managed(Y, FOLDER, &set);
        let retired = format!("yamareco-{FOLDER}-{JOB_A}");
        // 実行前から、退避先と同じ名前の項目がある（旧フォルダの中身と同じ名前のファイルを含むフォルダ、またはファイル）
        let pre = if as_file {
            env.drive_add_file(&["geoframe-export-work", "replaced"], &retired)
        } else {
            env.seed_set_at(&["geoframe-export-work", "replaced", &retired], &set)
        };
        let mut hook = Hook::new(&env);
        let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(f.error.code, ErrorCode::Blocked, "file={as_file}");
        let info = f.blocked.expect("blocked");
        assert_eq!(info.reason, BlockedReason::ReplacedUnexpected);
        assert_eq!(info.progress.stage, Stage::Retiring);
        assert_eq!(info.progress.old_id.as_deref(), Some(old_id.as_str()));
        let seen = &info.observed.as_ref().unwrap().replaced;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, pre, "見えていた項目の ID");
        // 何も動かしていない: 移動も purge も無く、旧フォルダは宛先のまま、incoming も残っている
        assert!(env.op_count("move") == 0, "file={as_file}");
        assert!(env.op_count("trash") == 0);
        assert_eq!(
            env.mutating_ops().last().map(String::as_str),
            Some("record:retiring")
        );
        assert_eq!(dest_id(&env).as_deref(), Some(old_id.as_str()));
        assert_eq!(
            files_under(&env, &["yamareco", FOLDER]),
            expected_files(&set),
            "旧一式のファイルは宛先のまま"
        );
        if !as_file {
            assert_eq!(
                files_under(&env, &["geoframe-export-work", "replaced", &retired]),
                expected_files(&set),
                "もともとあった項目に、何も足されていない"
            );
        }
        assert!(
            env.drive()
                .resolve(&["geoframe-export-work", "incoming", JOB_A])
                .is_some()
        );
        assert!(env.drive().trashed.is_empty() && env.drive().deleted.is_empty());
        let layout = env.layout(Y, FOLDER, JOB_A);
        let text = info.describe(&layout);
        assert!(
            text.contains("replaced_unexpected") && text.contains(&pre),
            "{text}"
        );
        assert!(text.contains("何も動かしていません"), "{text}");

        // 再起動の復旧も、記録した retiring と同じ状態を blocked にする（動かさない）
        let r = env.remote();
        let rec = Recovery {
            remote: &r,
            layout: &layout,
            published: &env.published,
            expected: None,
            timing: fast_timing(),
        };
        let again = rec
            .run(&info.progress, false, &mut Hook::new(&env))
            .unwrap_err();
        assert_eq!(again.reason, BlockedReason::ReplacedUnexpected);
        assert!(env.op_count("move") == 0);
    }
}

// ---- incoming を作る前に、同名の項目が無いことを確かめる ----

#[test]
fn a_same_named_incoming_that_already_exists_is_not_adopted_and_is_reported_as_a_leftover() {
    for variant in [
        "dir",
        "file",
        "two-dirs",
        "shortcut-mime",
        "shortcut-resolved",
    ] {
        let env = Env::new(&format!("incoming-occupied-{variant}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let work = ["geoframe-export-work", "incoming"];
        let (expect_id, kept) = match variant {
            "dir" => {
                let id = env.seed_set_at(&["geoframe-export-work", "incoming", JOB_A], &set);
                (Some(id.clone()), vec![id])
            }
            "file" => {
                let id = env.drive_add_file(&work, JOB_A);
                (None, vec![id])
            }
            "shortcut-mime" | "shortcut-resolved" => {
                // 同名のショートカット（ショートカットを省く一覧では見えない）。どちらの見え方でも同名として数える
                let mut d = env.drive();
                let other = d.mkdir_p(&["elsewhere"]);
                let id = d.add_shortcut(&work, JOB_A, &other);
                env.save(&d);
                if variant == "shortcut-resolved" {
                }
                (None, vec![id])
            }
            _ => {
                let mut d = env.drive();
                d.mkdir_p(&work);
                let a = d.mkdir_dup(&work, JOB_A);
                let b = d.mkdir_dup(&work, JOB_A);
                env.save(&d);
                (None, vec![a, b])
            }
        };
        let mut hook = Hook::new(&env);
        let f = failure(run(&env, &set, OnExists::Error, &mut hook));
        // 宛先は動かしておらず、守るものは無いので blocked にはしない。残りものとして知らせる
        assert_eq!(f.error.code, ErrorCode::Internal, "{variant}");
        assert!(f.blocked.is_none(), "{variant}");
        assert!(f.leftover_incoming, "{variant}");
        assert_eq!(f.leftover_incoming_id, expect_id, "{variant}");
        // 既存のフォルダの ID を、このジョブのものとして記録しない。作らず、送らず、消さない
        // 同名の確かめは最初の記録より前に行うので、何も記録しない（再起動の復旧が blocked にならない）
        assert!(hook.records.is_empty(), "{variant}");
        for op in ["create_folder", "upload", "move", "trash"] {
            assert!(env.op_count(op) == 0, "{variant}: {op}");
        }
        assert!(dest_id(&env).is_none(), "{variant}");
        assert!(env.drive().trashed.is_empty() && env.drive().deleted.is_empty());
        let d = env.drive();
        for id in kept {
            assert!(d.node(&id).is_some(), "{variant}: 触っていない");
        }
        if variant == "dir" {
            assert_eq!(
                files_under(&env, &["geoframe-export-work", "incoming", JOB_A]),
                expected_files(&set),
                "既存のフォルダの中身は触っていない"
            );
        }
    }
}

// ---- 中止の片付けの前に、`aborting` を記録する ----

#[test]
fn an_abort_from_publishing_records_aborting_before_the_trash_and_a_second_stop_resumes() {
    let env = Env::new("abort-record");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let fake = env.fake_dir.clone();
    // publishing を記録したあと、直前の確認の前に、オーナーが宛先を作った
    let mut hook = Hook::new(&env).mutate_after(Stage::Publishing, move || {
        let mut d = model::Drive::load(&fake);
        d.mkdir_p(&["yamareco", FOLDER]);
        d.save(&fake);
    });
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::AmbiguousDestination);
    let seq = env.mutating_ops();
    let n = seq.len();
    assert_eq!(
        &seq[n - 3..],
        ["record:publishing", "record:aborting", "trash"]
    );
    let last = hook.records.last().cloned().unwrap();
    assert_eq!(last.stage, Stage::Aborting);
    assert_eq!(last.old_id, None);
    assert_eq!(last.incoming_id, hook.records[1].incoming_id);

    // purge の直後に止まった、として再起動する。この中止は、宛先にオーナーのフォルダができたことが原因なので、宛先は
    // 前のままではない。表のとおり（宛先が変わっていれば blocked）、再起動の復旧は destination_unexpected で止まる
    let layout = env.layout(Y, FOLDER, JOB_A);
    let r = env.remote();
    let rec = Recovery {
        remote: &r,
        layout: &layout,
        published: &env.published,
        expected: None,
        timing: fast_timing(),
    };
    let info = rec.run(&last, false, &mut Hook::new(&env)).unwrap_err();
    assert_eq!(info.reason, BlockedReason::DestinationUnexpected);
    // オーナーのフォルダを片付けた（宛先が前のまま）あとの再起動: aborting なら、片付け済みとして interrupted で終える。
    // 記録が publishing のままなら、あるはずのものが無い状態（stage_inconsistent）で blocked になっていた
    let owner = env.drive().resolve(&["yamareco", FOLDER]).unwrap();
    let mut d = env.drive();
    d.purge(&owner, false);
    env.save(&d);
    let res = rec.run(&last, false, &mut Hook::new(&env)).unwrap();
    assert_eq!(
        res,
        Resolution::Interrupted {
            leftover_incoming: false,
            leftover_incoming_id: None
        }
    );
    let stale = Progress {
        stage: Stage::Publishing,
        ..last.clone()
    };
    let info = rec.run(&stale, false, &mut Hook::new(&env)).unwrap_err();
    assert_eq!(
        info.reason,
        BlockedReason::StageInconsistent,
        "aborting を記録していなければ、これで blocked になっていた"
    );
}

#[test]
fn an_abort_whose_aborting_record_cannot_be_written_does_not_trash_the_incoming() {
    let env = Env::new("abort-record-fails");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let fake = env.fake_dir.clone();
    let mut hook = Hook::new(&env)
        .mutate_after(Stage::Publishing, move || {
            let mut d = model::Drive::load(&fake);
            d.mkdir_p(&["yamareco", FOLDER]);
            d.save(&fake);
        })
        .fail_at(Stage::Aborting, 1);
    let f = failure(run(&env, &set, OnExists::Error, &mut hook));
    assert_eq!(f.error.code, ErrorCode::AmbiguousDestination);
    // 書けなければ、ゴミ箱へは移さない。残りものとして ID つきで知らせる
    assert!(env.op_count("trash") == 0);
    assert!(f.leftover_incoming);
    assert_eq!(f.leftover_incoming_id, hook.records[1].incoming_id);
    assert!(env.drive().trashed.is_empty());
    assert!(
        env.drive()
            .resolve(&["geoframe-export-work", "incoming", JOB_A])
            .is_some()
    );
}

// ---- 中身の MD5（AC6: その後に中身が変わっていないフォルダだけを置き換える） ----

#[test]
fn a_rewrite_with_the_same_name_and_size_is_modified_and_nothing_is_touched() {
    // 写真・manifest.json・下のフォルダの写真の、どれを書き換えても（サイズは同じ）、replace は conflict（modified）。
    // moveto・purge どころか、copy・mkdir も呼ばない
    for (name, folder, files, target) in [
        ("photo", FOLDER, PHOTOS, ("", "090000_1.jpg")),
        ("manifest", FOLDER, PHOTOS, ("", "manifest.json")),
        (
            "sub-photo",
            "2030-03-10-11",
            &[
                ("2030-03-10/090000_1.jpg", 100),
                ("2030-03-11/090100_2.jpg", 200),
            ][..],
            ("2030-03-11", "090100_2.jpg"),
        ),
    ] {
        let env = Env::new(&format!("md5-modified-{name}"));
        let set = env.make_set("s", Y, folder, files);
        let id = env.seed_managed(Y, folder, &set);
        // 書き換える前は、管理しているフォルダ
        let layout = env.layout(Y, folder, JOB_A);
        assert_eq!(
            dry_run(&env.remote(), &layout, &env.published)
                .unwrap()
                .managed,
            Some(Managed::Managed),
            "{name}"
        );
        let mut parent = vec!["yamareco", folder];
        if !target.0.is_empty() {
            parent.push(target.0);
        }
        change_md5(&env, &parent, target.1);
        let before = env.drive().nodes.len();
        assert_eq!(
            dry_run(&env.remote(), &layout, &env.published)
                .unwrap()
                .managed,
            Some(Managed::Modified),
            "{name}: dry_run も同じ判定"
        );
        let layout = env.layout(Y, folder, JOB_A);
        let mut hook = Hook::new(&env);
        let out = publish(
            &env.remote(),
            &layout,
            &params(&env, &set, OnExists::Replace),
            &mut hook,
        );
        let (reason, _) = conflict(out);
        assert_eq!(reason, ConflictReason::Modified, "{name}");
        assert_eq!(env.mutating_ops(), Vec::<String>::new(), "{name}");
        for op in ["move", "trash", "upload", "create_folder"] {
            assert!(env.op_count(op) == 0, "{name}: {op}");
        }
        assert_eq!(
            env.drive().resolve(&["yamareco", folder]).as_deref(),
            Some(id.as_str()),
            "{name}"
        );
        assert_eq!(env.drive().nodes.len(), before, "{name}");
        assert!(env.drive().trashed.is_empty(), "{name}");
    }
}

#[test]
fn a_record_without_md5_refuses_replace_as_not_managed() {
    // MD5 を記録する前の古い形の行（files が無い）: 中身が変わっていないと確かめられないので、管理していないものとして断る
    let env = Env::new("md5-legacy");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let id = env.seed_folder(Y, FOLDER, &set);
    std::fs::create_dir_all(env.published.dir()).unwrap();
    std::fs::write(
        record_path(&env),
        format!(
            r#"{{"version":1,"entry":{{"destination":"yamareco","folder":"{FOLDER}","folder_id":"{id}","job_id":"{JOB_B}"}}}}"#
        ),
    )
    .unwrap();
    let layout = env.layout(Y, FOLDER, JOB_A);
    assert_eq!(
        dry_run(&env.remote(), &layout, &env.published)
            .unwrap()
            .managed,
        Some(Managed::NotManaged)
    );
    let mut hook = Hook::new(&env);
    let (reason, _) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
    assert_eq!(reason, ConflictReason::NotManaged);
    env.assert_no_mutations();
    assert_eq!(dest_id(&env).as_deref(), Some(id.as_str()));
    // 古い行は、そのまま残っている（MD5 を作り出さない）
    let text = std::fs::read_to_string(record_path(&env)).unwrap();
    assert!(!text.contains("md5"), "{text}");
}

#[test]
fn a_record_whose_files_disagree_with_the_manifest_is_modified() {
    // 記録のファイルの集合が、宛先の manifest の一式と違う（足りない・名前が違う）→ 中身が変わっているものとして modified
    for (name, drop_one) in [("missing", true), ("renamed", false)] {
        let env = Env::new(&format!("md5-record-{name}"));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let id = env.seed_managed(Y, FOLDER, &set);
        let mut files = set.recorded();
        if drop_one {
            files.remove(0);
        } else {
            files[0].path = "other.jpg".into();
        }
        env.published
            .upsert(geoframe_export::published::PublishedRecord {
                destination: Y,
                folder: geoframe_export::folder::FolderName::parse(FOLDER).unwrap(),
                folder_id: id,
                job_id: geoframe_export::jobid::JobId::parse(JOB_B).unwrap(),
                files: Some(files),
                previous: None,
            })
            .unwrap();
        let mut hook = Hook::new(&env);
        let (reason, _) = conflict(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(reason, ConflictReason::Modified, "{name}");
        assert_eq!(env.mutating_ops(), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn the_old_folder_is_not_trashed_when_a_file_was_rewritten_to_the_same_size_after_the_check() {
    // 退避のあと、旧フォルダの写真（または manifest.json）が、同じ名前・同じ大きさのまま書き換えられた → ゴミ箱へ移さず blocked
    for target in ["090000_1.jpg", "manifest.json"] {
        let env = Env::new(&format!("md5-trash-{}", target.replace('.', "_")));
        let set = env.make_set("s", Y, FOLDER, PHOTOS);
        let old_id = env.seed_managed(Y, FOLDER, &set);
        let fake = env.fake_dir.clone();
        let name = format!("yamareco-{FOLDER}-{JOB_A}");
        let mut hook = Hook::new(&env).mutate_after(Stage::Trashing, move || {
            let mut d = model::Drive::load(&fake);
            let dir = d
                .resolve(&["geoframe-export-work", "replaced", &name])
                .unwrap();
            let id = d
                .children(&dir)
                .iter()
                .find(|n| n.name == target)
                .unwrap()
                .id
                .clone();
            d.set_md5(&id, &"f".repeat(32));
            d.save(&fake);
        });
        let f = failure(run(&env, &set, OnExists::Replace, &mut hook));
        assert_eq!(f.error.code, ErrorCode::Blocked, "{target}");
        let info = f.blocked.expect("blocked");
        assert_eq!(
            info.reason,
            BlockedReason::RetiredContentChanged,
            "{target}"
        );
        assert_eq!(info.progress.stage, Stage::Trashing, "{target}");
        assert!(env.op_count("trash") == 0, "{target}");
        assert!(env.drive().trashed.is_empty(), "{target}");
        // 新しい一式は公開済み。旧フォルダは退避先にそのまま残っている
        assert_ne!(dest_id(&env).as_deref(), Some(old_id.as_str()), "{target}");
        assert_eq!(info.observed.clone().unwrap().replaced[0].id, old_id);
    }
}

#[test]
fn the_published_row_records_the_md5_of_every_file_and_a_replace_works_again_from_it() {
    let env = Env::new("md5-record");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env);
    let p = published(run(&env, &set, OnExists::Error, &mut hook));
    // 作業領域の中身の MD5（写真ごとに違い、manifest.json も入る）が、公開の記録に入る
    let f = geoframe_export::folder::FolderName::parse(FOLDER).unwrap();
    let row = env.published.find(Y, &f).expect("row");
    assert_eq!(row.files, Some(set.recorded()));
    assert_eq!(p.record.files, Some(set.recorded()));
    assert!(row.previous.is_none());
    let md5s: std::collections::BTreeSet<&str> = set.files.iter().map(|f| f.md5.as_str()).collect();
    assert_eq!(
        md5s.len(),
        set.files.len(),
        "試験の一式は、ファイルごとに MD5 が違う"
    );
    // 宛先の MD5 は、送った中身と同じ
    let layout = env.layout(Y, FOLDER, JOB_B);
    assert_eq!(
        dry_run(&env.remote(), &layout, &env.published)
            .unwrap()
            .managed,
        Some(Managed::Managed)
    );
    // 中身を作り直して replace すると、新しい行の MD5 が入れ替わり、旧フォルダの MD5 は `previous` に残らない
    let set2 = env.make_set(
        "s2",
        Y,
        FOLDER,
        &[("090000_1.jpg", 101), ("090100_2.jpg", 200)],
    );
    let layout = env.layout(Y, FOLDER, JOB_B);
    let mut hook = Hook::new(&env);
    let out = publish(
        &env.remote(),
        &layout,
        &params(&env, &set2, OnExists::Replace),
        &mut hook,
    );
    let p2 = published(out);
    assert!(p2.replaced && p2.old_left_in_replaced.is_none());
    let row = env.published.find(Y, &f).unwrap();
    assert_eq!(row.files, Some(set2.recorded()));
    assert_eq!(row.folder_id, p2.folder_id);
    assert!(row.previous.is_none());
}

#[test]
fn every_listing_asks_for_the_md5_field_and_nothing_else_is_asked_for_in_the_tree() {
    // API の一覧は、いつも `md5Checksum` を含む項目だけを取る（親の一覧も、木の一覧も同じ。中身の照合に使うのは木の一覧）
    let env = Env::new("md5-fields");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env);
    published(run(&env, &set, OnExists::Replace, &mut hook));
    let lists: Vec<Value> = env
        .http_calls()
        .into_iter()
        .filter(|c| c["op"] == "list")
        .collect();
    assert!(!lists.is_empty());
    for c in &lists {
        assert_eq!(
            c["fields"],
            "nextPageToken,files(id,name,mimeType,size,md5Checksum)",
            "{c}"
        );
    }
}

// ---- 検証済みの MD5 は、退避・公開の前にジョブの記録へ残す ----

#[test]
fn the_verified_md5_is_recorded_in_the_job_record_before_the_retire_or_the_publish() {
    // 新規: uploading・verifying の記録には無く、publishing の記録（公開の移動の前）から、作業領域の各ファイルの MD5 が入る
    let env = Env::new("digests-new");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    let mut hook = Hook::new(&env);
    published(run(&env, &set, OnExists::Error, &mut hook));
    let stages: Vec<(Stage, bool)> = hook
        .records
        .iter()
        .map(|r| (r.stage, r.digests.is_some()))
        .collect();
    assert_eq!(
        stages,
        [
            (Stage::Uploading, false),
            (Stage::Uploading, false),
            (Stage::Verifying, false),
            (Stage::Publishing, true),
        ]
    );
    let last = hook.records.last().unwrap();
    assert_eq!(last.digests, Some(set.recorded()));
    // 記録は、公開の移動（moveto）より前に書いている
    let seq = env.mutating_ops();
    let rec = seq.iter().position(|c| c == "record:publishing").unwrap();
    let mv = seq.iter().position(|c| c == "move").unwrap();
    assert!(rec < mv, "{seq:?}");

    // 置き換え: retiring の記録（旧フォルダの退避の移動の前）から入り、publishing・trashing に引き継がれる
    let env = Env::new("digests-replace");
    let set = env.make_set("s", Y, FOLDER, PHOTOS);
    env.seed_managed(Y, FOLDER, &set);
    let mut hook = Hook::new(&env);
    published(run(&env, &set, OnExists::Replace, &mut hook));
    let stages: Vec<(Stage, bool)> = hook
        .records
        .iter()
        .map(|r| (r.stage, r.digests.is_some()))
        .collect();
    assert_eq!(
        stages,
        [
            (Stage::Uploading, false),
            (Stage::Uploading, false),
            (Stage::Verifying, false),
            (Stage::Retiring, true),
            (Stage::Publishing, true),
            (Stage::Trashing, true),
        ]
    );
    assert!(
        hook.records[3..]
            .iter()
            .all(|r| r.digests == Some(set.recorded()))
    );
    let seq = env.mutating_ops();
    let rec = seq.iter().position(|c| c == "record:retiring").unwrap();
    let mv = seq.iter().position(|c| c == "move").unwrap();
    assert!(rec < mv, "{seq:?}");
}
