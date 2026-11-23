//! コマンドの入口（引数の解釈 → 起動時の検査 → サブコマンド）。`--feed-base` は常に使われていないポートへ向け、
//! 本物の写真サービス（127.0.0.1:8080）には何があっても触れない。Drive は、試験のプロセスの中の偽の Drive の HTTP サーバー
//! （`--drive-endpoint`）で、本物の Google には触れない。
//!
//! 起動時の検査（docs/note-photo-export/drive-api.md 2・3 章）: トークンを 1 回取り、3 つのフォルダを `get` で確かめる。
//! 設定の誤り・再認可が要る → 終了コード 2、通信の失敗・5xx → 1。

mod common;

use common::drive::*;
use common::fake_drive as fk;
use common::{TempDir, closed_port_base, set_mode};
use geoframe_export::app::{EXIT_USAGE, main_with};

struct Out {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Out {
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    run_owned(&args)
}

fn run_owned(args: &[String]) -> Out {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = main_with(args, &mut out, &mut err);
    Out {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

/// 全サブコマンドに共通の引数（偽の Drive・使われていないポートの写真サービス）。`replace` の `(名前, 値)` は、同じ名前の引数の値を
/// 差し替える。
fn common(env: &Env, replace: &[(&str, &str)]) -> Vec<String> {
    let mut v = vec![
        "--feed-base".to_owned(),
        closed_port_base().as_str().to_owned(),
    ];
    v.extend(env.cli_settings_args());
    for (name, value) in replace {
        let i = v
            .iter()
            .position(|a| a == name)
            .unwrap_or_else(|| panic!("{name}"));
        v[i + 1] = (*value).to_owned();
    }
    v
}

/// 4 つの起動のしかたの引数の並び（`run`・`run --dry-run`・`serve`・`recover`）。
fn all_commands(common: &[String]) -> Vec<Vec<String>> {
    let with = |head: &[&str]| -> Vec<String> {
        head.iter()
            .map(|s| (*s).to_owned())
            .chain(common.iter().cloned())
            .collect()
    };
    vec![
        with(&["run", "--from", "2030-03-10", "--destination", "yamareco"]),
        with(&[
            "run",
            "--from",
            "2030-03-10",
            "--destination",
            "yamareco",
            "--dry-run",
        ]),
        with(&[
            "serve",
            "--listen",
            "127.0.0.1:9100",
            "--token-file",
            "/nonexistent/token",
            "--allow",
            "192.0.2.1",
        ]),
        with(&["recover"]),
    ]
}

fn run_all(cmds: &[Vec<String>]) -> Vec<Out> {
    cmds.iter().map(|c| run_owned(c)).collect()
}

#[test]
fn every_subcommand_starts_with_a_working_drive_and_reaches_its_own_command() {
    // `--feed-base` は使われていないポート。`run` は一覧 API に届かず失敗（1）、`recover` は記録が 0 件なので 0、`serve` は
    // トークンのファイルが無いので、待ち受けずに 2（起動時の検査には通ったあと）
    let env = Env::new("cli-ok");
    let outs = run_all(&all_commands(&common(&env, &[])));
    let codes: Vec<u8> = outs.iter().map(|o| o.code).collect();
    assert_eq!(
        codes,
        [1, 1, EXIT_USAGE, 0],
        "{:?}",
        outs.iter().map(|o| &o.stderr).collect::<Vec<_>>()
    );
    assert!(
        outs[0].stdout.contains("feed_unavailable"),
        "{}",
        outs[0].stdout
    );
    assert!(
        outs[1].stdout.contains("feed_unavailable"),
        "{}",
        outs[1].stdout
    );
    assert!(
        outs[2].stderr.contains("--token-file") && !outs[2].stderr.contains("/nonexistent"),
        "{}",
        outs[2].stderr
    );
    // 起動時の検査は、トークンを 1 回と、3 つのフォルダ（とその祖先）の get だけ。Drive の中身には触れない
    env.assert_startup_only();
    let tokens = env.op_count("token");
    assert!(tokens >= 4, "起動のたびにトークンを取る: {tokens}");
    env.assert_api_calls_safe();
    for out in &outs {
        assert_no_drive_secrets(&env, &out.stdout);
        assert_no_drive_secrets(&env, &out.stderr);
    }
}

fn assert_no_drive_secrets(env: &Env, text: &str) {
    for secret in [
        fk::FAKE_CLIENT_ID,
        fk::FAKE_CLIENT_SECRET,
        fk::FAKE_REFRESH_TOKEN,
        env.yamareco_id.as_str(),
        env.travel_journal_id.as_str(),
        env.work_id.as_str(),
    ] {
        assert!(!text.contains(secret), "値が出ています: {text}");
    }
    assert!(!text.contains("access_token"), "{text}");
}

#[test]
fn a_revoked_authorization_exits_2_and_says_a_new_consent_is_needed() {
    // サーバー側がトークンの取得を `invalid_grant` で断る（同意が取り消された・更新トークンが失効した）
    let env = Env::new("cli-invalid-grant");
    env.add_rule("token", 0, u64::MAX, fk::invalid_grant());
    for out in run_all(&all_commands(&common(&env, &[]))) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("再認可"), "{}", out.stderr);
        assert!(out.stderr.contains("deploy/README.md"), "{}", out.stderr);
        assert!(out.stdout.is_empty(), "{}", out.stdout);
        assert_no_drive_secrets(&env, &out.stderr);
    }
    // 起動のたびにトークンの取得 1 回で終わる（Drive の中身には触れない。再試行もしない）
    assert_eq!(env.ops_used(), ["token"]);
    assert_eq!(env.op_count("token"), 4);
    assert!(!env.state.join("jobs").exists(), "検査に通らなければ何も作らない");

    // 更新トークンが合わない資格情報のファイルでも同じ（偽のサーバーが invalid_grant を返す）
    let env = Env::new("cli-wrong-token");
    let wrong = fk::write_credentials_with(&env.root.path().join("wrong"), "WRONG-REFRESH-TOKEN");
    let args = common(&env, &[("--drive-credentials", wrong.to_str().unwrap())]);
    for out in run_all(&all_commands(&args)) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("再認可"), "{}", out.stderr);
        assert!(!out.stderr.contains("WRONG-REFRESH-TOKEN"));
    }
}

#[test]
fn an_unreadable_or_too_open_credentials_file_exits_2_without_echoing_values() {
    let env = Env::new("cli-cred");
    // 権限が広い
    set_mode(&env.credentials, 0o644);
    for out in run_all(&all_commands(&common(&env, &[]))) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("--drive-credentials"), "{}", out.stderr);
        assert_no_drive_secrets(&env, &out.stderr);
    }
    // 無い
    let missing = env.root.path().join("nothing/credentials.json");
    let args = common(&env, &[("--drive-credentials", missing.to_str().unwrap())]);
    for out in run_all(&all_commands(&args)) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("--drive-credentials"), "{}", out.stderr);
    }
    // どちらも、Drive へは 1 つも要求していない
    assert!(env.http_calls().is_empty(), "{:?}", env.ops());
}

#[test]
fn folders_that_are_missing_trashed_not_folders_or_nested_exit_2() {
    // どれも設定の誤り（終了コード 2）。標準エラーには引数の名前だけを出し、ID の値は出さない
    let env = Env::new("cli-folders");
    let mut d = env.drive();
    let file = d.add_file(&["geoframe-export-work"], "x.txt", 5, None);
    let shortcut = d.add_shortcut(&["travel-journal"], "link", &env.work_id);
    let nested_work = d.mkdir_p(&["yamareco", "work2"]);
    let trashed = d.mkdir_p(&["elsewhere", "trashed"]);
    d.node_mut_pub(&trashed).unwrap().trashed = true;
    d.trashed.push(trashed.clone());
    env.save(&d);

    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, &str, String, &str)> = vec![
        ("missing", "--work-folder", "NoSuchFolderId".into(), "--work-folder"),
        ("a-file", "--work-folder", file, "--work-folder"),
        ("a-shortcut", "--folder-travel-journal", shortcut, "--folder-travel-journal"),
        ("trashed", "--folder-yamareco", trashed, "--folder-yamareco"),
        // 作業用のフォルダが、宛先の親の中にある・宛先の親が作業用のフォルダの中にある
        ("work-inside-prefix", "--work-folder", nested_work.clone(), "--folder-yamareco"),
        (
            "prefix-inside-work",
            "--folder-travel-journal",
            env.drive().mkdir_p(&["geoframe-export-work", "j"]),
            "--folder-travel-journal",
        ),
    ];
    for (name, opt, id, expect_in_message) in cases {
        let args = common(&env, &[(opt, id.as_str())]);
        for out in run_all(&all_commands(&args)) {
            assert_eq!(out.code, EXIT_USAGE, "{name}: {}", out.stderr);
            assert!(out.stderr.contains(expect_in_message), "{name}: {}", out.stderr);
            assert!(!out.stderr.contains(&id), "{name}: ID を写している: {}", out.stderr);
            assert!(out.stdout.is_empty(), "{name}: {}", out.stdout);
        }
    }
    assert!(!env.state.join("jobs").exists());
    env.assert_startup_only();
}

#[test]
fn drive_failures_at_startup_exit_1_because_a_retry_may_fix_them() {
    // Drive に届かない（接続できない）・5xx は、設定の誤りではない。終了コード 1（serve は systemd が再起動する）
    let env = Env::new("cli-unreachable");
    let port = closed_port_base();
    let dead = port.as_str();
    let args = common(&env, &[("--drive-endpoint", dead)]);
    let cmds = all_commands(&args);
    for i in [0usize, 2] {
        let out = run_owned(&cmds[i]);
        assert_eq!(out.code, 1, "{}", out.stderr);
        assert!(out.stderr.contains("接続できません"), "{}", out.stderr);
        assert!(!out.stderr.contains("再認可"), "{}", out.stderr);
    }

    let env = Env::new("cli-5xx");
    env.add_rule("get", 0, u64::MAX, fk::status(503));
    let cmds = all_commands(&common(&env, &[]));
    for i in [1usize, 2] {
        let out = run_owned(&cmds[i]);
        assert_eq!(out.code, 1, "{}", out.stderr);
        assert!(out.stdout.is_empty(), "{}", out.stdout);
    }
    // 起動時の祖先のたどりで、親が見えない（404）のは、そこで打ち切る（設定の誤りにも、通信の失敗にもしない）
    let env = Env::new("cli-hidden-parent");
    let mut d = env.drive();
    let work3 = d.mkdir_p(&["hidden-top", "work3"]);
    let hidden = d.resolve(&["hidden-top"]).unwrap();
    d.nodes.retain(|n| n.id != hidden);
    env.save(&d);
    let args = common(&env, &[("--work-folder", work3.as_str())]);
    let out = run_owned(&all_commands(&args)[3]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn bad_settings_refuse_to_start_before_anything_else() {
    // 引数の検査（ID の形・同じ ID・試験専用の通信先・消えた引数）は、Drive へ要求を出す前に断る
    let env = Env::new("cli-bad-settings");
    let same = env.yamareco_id.clone();
    let bad: Vec<Vec<(&str, String)>> = vec![
        vec![("--folder-yamareco", String::new())],
        vec![("--folder-travel-journal", "a b".into())],
        vec![("--work-folder", "a/b".into())],
        vec![("--work-folder", "日本語".into())],
        vec![("--folder-travel-journal", same.clone())],
        vec![("--work-folder", same)],
        vec![("--drive-endpoint", "https://www.googleapis.com".into())],
        vec![("--drive-endpoint", "http://localhost:8080".into())],
        vec![("--drive-endpoint", "http://192.0.2.1:8080".into())],
        vec![("--drive-credentials", "relative/path".into())],
        vec![("--feed-base", "https://127.0.0.1:9".into())],
    ];
    for replace in bad {
        let pairs: Vec<(&str, &str)> = replace.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let out_all = run_all(&all_commands(&common(&env, &pairs)));
        for out in out_all {
            assert_eq!(out.code, EXIT_USAGE, "{replace:?}: {}", out.stderr);
            assert!(out.stdout.is_empty());
            assert!(!out.stderr.contains("未実装"));
        }
    }
    // 消えた引数（rclone 版のもの）は、知らない引数として断る
    for old in [
        ["--rclone", "/x/rclone"],
        ["--rclone-config", "/x/rclone.conf"],
        ["--remote-yamareco", "gdrive:a"],
        ["--remote-travel-journal", "gdrive:b"],
        ["--work-root", "gdrive:w"],
        ["--rclone-copy-timeout", "60"],
        ["--rclone-timeout", "30"],
    ] {
        let mut args = common(&env, &[]);
        args.extend(old.iter().map(|s| (*s).to_owned()));
        for cmd in all_commands(&args) {
            let out = run_owned(&cmd);
            assert_eq!(out.code, EXIT_USAGE, "{old:?}: {}", out.stderr);
            assert!(out.stderr.contains(old[0]), "{old:?}: {}", out.stderr);
        }
    }
    assert!(env.http_calls().is_empty(), "要求を出す前に断る");
    assert!(!env.state.join("jobs").exists());
}

#[test]
fn usage_errors_exit_with_2_and_never_echo_values() {
    let out = run(&[]);
    assert_eq!(out.code, EXIT_USAGE);
    let out = run(&["run", "--from", "2030-02-30", "--destination", "yamareco"]);
    assert_eq!(out.code, EXIT_USAGE);
    assert!(out.stderr.contains("invalid_date"), "{}", out.stderr);
    assert!(!out.stderr.contains("2030-02-30"));
    let out = run(&["frobnicate"]);
    assert_eq!(out.code, EXIT_USAGE);
    let out = run(&["run", "--token-file=SECRETVALUE"]);
    assert_eq!(out.code, EXIT_USAGE);
    assert!(!out.stderr.contains("SECRETVALUE"));
}

#[test]
fn version_and_help_exit_with_0() {
    let out = run(&["--version"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.starts_with("geoframe-export "), "{}", out.stdout);
    let out = run(&["--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("geoframe-export run"));
    for name in [
        "--drive-credentials",
        "--folder-yamareco",
        "--folder-travel-journal",
        "--work-folder",
        "import-credentials",
    ] {
        assert!(out.stdout.contains(name), "{name}");
    }
    // 試験専用の通信先は、help に出さない
    assert!(!out.stdout.contains("--drive-endpoint"));
    assert_eq!(run(&["recover", "--help"]).code, 0);
    // 別の一時ディレクトリを使わずに済んだ（何も作っていない）
    let _ = TempDir::new("cli-help");
}
