//! コマンドの入口（引数の解釈 → 起動時の検査 → サブコマンド）。`--feed-base` は常に使われていないポートへ向け、
//! 本物の写真サービス（127.0.0.1:8080）には何があっても触れない。

mod common;

use common::{
    MARK_ID, MARK_SECRET, TempDir, assert_no_secrets, good_config, set_mode, write_config,
};
use geoframe_export::app::{EXIT_NOT_IMPLEMENTED, EXIT_USAGE, main_with};

struct Out {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Out {
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = main_with(&args, &mut out, &mut err);
    Out {
        code,
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
    }
}

/// 全サブコマンドに共通の引数（設定ファイルは `conf`、状態ディレクトリは同じ場所）。
fn common(conf: &std::path::Path, dir: &TempDir) -> Vec<String> {
    vec![
        "--feed-base".into(),
        "http://127.0.0.1:9".into(),
        "--rclone".into(),
        "/nonexistent/rclone".into(),
        "--rclone-config".into(),
        conf.to_str().unwrap().into(),
        "--state-dir".into(),
        dir.path().to_str().unwrap().into(),
    ]
}

/// 3 つのサブコマンドの引数の並び（検査に通る値。リダイレクトなどの副作用は無い）。
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
            "10.0.0.1",
        ]),
        with(&["recover"]),
    ]
}

fn run_all(cmds: &[Vec<String>]) -> Vec<Out> {
    cmds.iter()
        .map(|c| run(&c.iter().map(String::as_str).collect::<Vec<_>>()))
        .collect()
}

#[test]
fn every_subcommand_starts_with_a_dedicated_config_and_is_not_implemented_yet() {
    let dir = TempDir::new("cli-ok");
    let conf = write_config(dir.path(), good_config("gdrive").as_bytes());
    for out in run_all(&all_commands(&common(&conf, &dir))) {
        assert_eq!(out.code, EXIT_NOT_IMPLEMENTED, "{}", out.stderr);
        assert!(out.stderr.contains("未実装"), "{}", out.stderr);
        assert_no_secrets(&out.stdout);
        assert_no_secrets(&out.stderr);
    }
}

#[test]
fn a_config_without_a_dedicated_client_id_refuses_to_start_everywhere_even_for_dry_run() {
    let dir = TempDir::new("cli-noid");
    let content = format!("[gdrive]\ntype = drive\nclient_secret = {MARK_SECRET}\ntoken = {{}}\n");
    let conf = write_config(dir.path(), content.as_bytes());
    for out in run_all(&all_commands(&common(&conf, &dir))) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("client_id"), "{}", out.stderr);
        assert!(!out.stderr.contains("未実装"));
        assert_no_secrets(&out.stdout);
        assert_no_secrets(&out.stderr);
    }
}

#[test]
fn an_empty_client_secret_or_a_missing_section_refuses_to_start() {
    let dir = TempDir::new("cli-empty");
    for (content, needle) in [
        (
            format!("[gdrive]\nclient_id = {MARK_ID}\nclient_secret =\n"),
            "client_secret",
        ),
        (good_config("another"), "セクション"),
    ] {
        let conf = write_config(dir.path(), content.as_bytes());
        for out in run_all(&all_commands(&common(&conf, &dir))) {
            assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
            assert!(out.stderr.contains(needle), "{}", out.stderr);
            assert_no_secrets(&out.stderr);
        }
    }
}

#[test]
fn a_remote_name_that_differs_from_the_section_refuses_to_start() {
    // 接頭辞の remote 名（既定は gdrive）と、設定のセクション名が違う
    let dir = TempDir::new("cli-remote");
    let conf = write_config(dir.path(), good_config("gdrive").as_bytes());
    let mut args = vec![
        "run".to_owned(),
        "--from".into(),
        "2030-03-10".into(),
        "--destination".into(),
        "yamareco".into(),
        "--remote-yamareco".into(),
        "mine:a".into(),
        "--remote-travel-journal".into(),
        "mine:b".into(),
        "--work-root".into(),
        "mine:w".into(),
    ];
    args.extend(common(&conf, &dir));
    let out = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
    assert_no_secrets(&out.stderr);
    // 同じ設定でも、セクション名に合わせれば通る
    let conf = write_config(dir.path(), good_config("mine").as_bytes());
    let mut args2 = args[..11].to_vec();
    args2.extend(common(&conf, &dir));
    let out = run(&args2.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(out.code, EXIT_NOT_IMPLEMENTED, "{}", out.stderr);
}

#[test]
fn wide_permissions_refuse_to_start() {
    let dir = TempDir::new("cli-mode");
    let conf = write_config(dir.path(), good_config("gdrive").as_bytes());
    let cmds = all_commands(&common(&conf, &dir));

    set_mode(&conf, 0o644);
    for out in run_all(&cmds) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("0600"), "{}", out.stderr);
        assert_no_secrets(&out.stderr);
    }
    set_mode(&conf, 0o600);
    set_mode(conf.parent().unwrap(), 0o755);
    for out in run_all(&cmds) {
        assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
        assert!(out.stderr.contains("0700"), "{}", out.stderr);
        assert_no_secrets(&out.stderr);
    }
}

#[test]
fn bad_prefixes_refuse_to_start_before_anything_else() {
    let dir = TempDir::new("cli-prefix");
    let conf = write_config(dir.path(), good_config("gdrive").as_bytes());
    let base = common(&conf, &dir);
    let bad: [&[&str]; 12] = [
        &["--remote-yamareco", "gdrive:"],
        &["--remote-yamareco", "gdrive:yamareco/"],
        &["--remote-yamareco", "-gdrive:yamareco"],
        &["--remote-travel-journal", "gdrive:../x"],
        &["--remote-travel-journal", "gdrive:a b"],
        &["--work-root", "gdrive:yamareco"],
        &["--work-root", "gdrive:yamareco/work"],
        &["--work-root", "gdrive:"],
        &["--work-root", "other:work"],
        &[
            "--remote-yamareco",
            "gdrive:a",
            "--remote-travel-journal",
            "gdrive:a/b",
        ],
        &["--remote-yamareco", "other:yamareco"],
        &["--feed-base", "https://127.0.0.1:9"],
    ];
    for extra in bad {
        for head in [
            &["run", "--from", "2030-03-10", "--destination", "yamareco"][..],
            &[
                "run",
                "--from",
                "2030-03-10",
                "--destination",
                "yamareco",
                "--dry-run",
            ],
            &["recover"],
            &[
                "serve",
                "--listen",
                "127.0.0.1:9100",
                "--token-file",
                "/nonexistent/token",
                "--allow",
                "10.0.0.1",
            ],
        ] {
            let mut owned: Vec<String> =
                head.iter().chain(extra).map(|s| (*s).to_owned()).collect();
            owned.extend(base[2..].iter().cloned());
            // 試験が `--feed-base` を指定しないときも、使われていないポートへ向ける（既定は本物の 8080）
            if extra[0] != "--feed-base" {
                owned.extend(base[..2].iter().cloned());
            }
            let out = run(&owned.iter().map(String::as_str).collect::<Vec<_>>());
            assert_eq!(out.code, EXIT_USAGE, "{extra:?} {head:?}: {}", out.stderr);
            assert!(!out.stderr.contains("未実装"), "{extra:?}: {}", out.stderr);
            assert!(out.stdout.is_empty());
        }
    }
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
    assert!(out.stdout.contains("--rclone-config"));
    assert_eq!(run(&["recover", "--help"]).code, 0);
}
