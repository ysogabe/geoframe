//! `geoframe-export import-credentials`（docs/note-photo-export/drive-api.md 2 章）: 今の rclone の設定から、Drive の資格情報の
//! ファイル（3 つのキーだけ・0600）を 1 回だけ作る。値は標準出力・標準エラー・ログに出さない。root が sudo で動かす前提なので、
//! root（`run_command` に渡す実効ユーザー ID が 0）でも断らない（起動時の検査も通らない）。本物の Google・rclone には触れない。

mod common;

use std::os::unix::fs::{MetadataExt, symlink};

use common::{MARK_ID, MARK_SECRET, TempDir, set_mode, write_config};
use geoframe_export::app::{EXIT_USAGE, main_with, run_command};
use geoframe_export::drive::auth::Credentials;
use geoframe_export::rclone_conf::{self, ConfigError, Target};

const REFRESH: &str = "1//REFRESH-TOKEN-MARKER-33aa";
const ACCESS: &str = "ya29.ACCESS-TOKEN-MARKER-77bb";

fn conf_text(remote: &str) -> String {
    format!(
        "[{remote}]\ntype = drive\nclient_id = {MARK_ID}\nclient_secret = {MARK_SECRET}\nscope = drive\n\
         token = {{\"access_token\":\"{ACCESS}\",\"token_type\":\"Bearer\",\"refresh_token\":\"{REFRESH}\",\"expiry\":\"2030-01-01T00:00:00+09:00\"}}\n"
    )
}

struct Out {
    code: u8,
    stdout: String,
    stderr: String,
}

fn import(conf: &std::path::Path, remote: &str, out: &std::path::Path) -> Out {
    import_as(None, conf, remote, out)
}

fn import_as(
    euid: Option<u32>,
    conf: &std::path::Path,
    remote: &str,
    out: &std::path::Path,
) -> Out {
    let args: Vec<String> = [
        "import-credentials",
        "--rclone-config",
        conf.to_str().unwrap(),
        "--remote",
        remote,
        "--out",
        out.to_str().unwrap(),
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let code = run_command(euid, &args, &mut o, &mut e);
    Out {
        code,
        stdout: String::from_utf8(o).unwrap(),
        stderr: String::from_utf8(e).unwrap(),
    }
}

fn assert_no_values(text: &str) {
    for v in [MARK_ID, MARK_SECRET, REFRESH, ACCESS] {
        assert!(!text.contains(v), "値が出ています: {text}");
    }
}

#[test]
fn it_writes_exactly_three_keys_with_mode_0600_and_prints_no_value() {
    let dir = TempDir::new("import-ok");
    let conf = write_config(dir.path(), conf_text("gdrive").as_bytes());
    let out_path = dir.path().join("drive-credentials.json");
    let out = import(&conf, "gdrive", &out_path);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_no_values(&out.stdout);
    assert_no_values(&out.stderr);
    assert!(
        out.stdout.contains("drive-credentials.json"),
        "{}",
        out.stdout
    );

    // 3 つのキーだけ（順序も固定）。値は rclone の設定のもの
    let text = std::fs::read_to_string(&out_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["client_id", "client_secret", "refresh_token"]);
    assert_eq!(v["client_id"], MARK_ID);
    assert_eq!(v["client_secret"], MARK_SECRET);
    assert_eq!(v["refresh_token"], REFRESH);
    // アクセストークンは書かない
    assert!(!text.contains(ACCESS));
    // 持ち主だけが読める
    let meta = std::fs::metadata(&out_path).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o600);
    // `--drive-credentials` と同じ検査で読める（往復）
    assert!(Credentials::load(&out_path).is_ok());
    // 元の rclone の設定は変えない
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), conf_text("gdrive"));
}

#[test]
fn an_existing_output_is_refused_and_left_untouched() {
    let dir = TempDir::new("import-exists");
    let conf = write_config(dir.path(), conf_text("gdrive").as_bytes());
    let out_path = dir.path().join("existing.json");
    std::fs::write(&out_path, b"keep me").unwrap();
    let out = import(&conf, "gdrive", &out_path);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert_eq!(std::fs::read(&out_path).unwrap(), b"keep me");
    assert_no_values(&out.stdout);
    assert_no_values(&out.stderr);
    // 2 回目も同じ（1 回目で作ったものを上書きしない）
    let second = dir.path().join("second.json");
    assert_eq!(import(&conf, "gdrive", &second).code, 0);
    let before = std::fs::read(&second).unwrap();
    assert_eq!(import(&conf, "gdrive", &second).code, 1);
    assert_eq!(std::fs::read(&second).unwrap(), before);
}

#[test]
fn a_symlink_as_the_output_is_not_followed() {
    let dir = TempDir::new("import-symlink");
    let conf = write_config(dir.path(), conf_text("gdrive").as_bytes());
    let target = dir.path().join("target.json");
    let link = dir.path().join("link.json");
    symlink(&target, &link).unwrap();
    let out = import(&conf, "gdrive", &link);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(!target.exists(), "リンクの先に書いてはいけない");
    // 親が無い出力先も、何も作らずに断る
    let out = import(&conf, "gdrive", &dir.path().join("no/such/dir/out.json"));
    assert_eq!(out.code, 1, "{}", out.stderr);
}

#[test]
fn a_config_that_cannot_be_imported_exits_2_without_creating_the_output() {
    let dir = TempDir::new("import-bad");
    let out_path = dir.path().join("out.json");
    // refresh_token が無い・空
    for token in [
        "{}",
        "{\"refresh_token\":\"\"}",
        "not json",
        "{\"refresh_token\":1}",
    ] {
        let text = format!(
            "[gdrive]\nclient_id = {MARK_ID}\nclient_secret = {MARK_SECRET}\ntoken = {token}\n"
        );
        let conf = write_config(dir.path(), text.as_bytes());
        let out = import(&conf, "gdrive", &out_path);
        assert_eq!(out.code, EXIT_USAGE, "{token}: {}", out.stderr);
        assert_no_values(&out.stderr);
        assert!(!out_path.exists(), "{token}");
    }
    // token の行が無い・client_id が無い・セクションが違う
    let cases = [
        (
            format!("[gdrive]\nclient_id = {MARK_ID}\nclient_secret = {MARK_SECRET}\n"),
            "gdrive",
        ),
        (
            format!(
                "[gdrive]\nclient_secret = {MARK_SECRET}\ntoken = {{\"refresh_token\":\"{REFRESH}\"}}\n"
            ),
            "gdrive",
        ),
        (conf_text("another"), "gdrive"),
    ];
    for (text, remote) in cases {
        let conf = write_config(dir.path(), text.as_bytes());
        let out = import(&conf, remote, &out_path);
        assert_eq!(out.code, EXIT_USAGE, "{text}: {}", out.stderr);
        assert_no_values(&out.stderr);
        assert!(!out_path.exists());
    }
    // 権限が広い（rclone の設定の検査と同じ）
    let conf = write_config(dir.path(), conf_text("gdrive").as_bytes());
    set_mode(&conf, 0o644);
    let out = import(&conf, "gdrive", &out_path);
    assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
    assert!(out.stderr.contains("0600"), "{}", out.stderr);
    assert!(!out_path.exists());
    // 無い
    let out = import(&dir.path().join("nothing/rclone.conf"), "gdrive", &out_path);
    assert_eq!(out.code, EXIT_USAGE, "{}", out.stderr);
    assert!(!out_path.exists());
}

#[test]
fn root_may_import_a_config_owned_by_the_service_user_but_others_may_not() {
    let dir = TempDir::new("import-root");
    let conf = write_config(dir.path(), conf_text("gdrive").as_bytes());
    // 試験を動かすユーザーの持ち物だが、「実行ユーザーが root」のとき（uid 0）は持ち主を問わない（権限は問う）
    let me = rclone_conf::effective_uid();
    let as_root = rclone_conf::read_credentials_for_uid(&conf, "gdrive", 0).expect("root");
    assert_eq!(as_root.refresh_token, REFRESH);
    // root 以外が、別の持ち主の設定を読むのは断る
    let other = me.wrapping_add(1).max(1);
    if other != 0 {
        assert_eq!(
            rclone_conf::read_credentials_for_uid(&conf, "gdrive", other).unwrap_err(),
            ConfigError::WrongOwner(Target::Directory)
        );
    }
    // 権限が広ければ、root でも断る
    set_mode(&conf, 0o640);
    assert_eq!(
        rclone_conf::read_credentials_for_uid(&conf, "gdrive", 0).unwrap_err(),
        ConfigError::TooPermissive(Target::File)
    );
    // Debug は値を伏せる
    set_mode(&conf, 0o600);
    let c = rclone_conf::read_credentials_for_uid(&conf, "gdrive", 0).unwrap();
    assert_no_values(&format!("{c:?}"));
}

#[test]
fn the_subcommand_is_not_refused_for_root_and_skips_the_startup_check() {
    let dir = TempDir::new("import-as-root");
    let conf = write_config(dir.path(), conf_text("gdrive").as_bytes());
    let out_path = dir.path().join("out.json");
    // `run_command` に渡す実効ユーザー ID が 0 でも、`import-credentials` は断らない（`run`・`serve`・`recover` は断る）。
    // Drive にも、起動時の検査にも触れない（通信先の引数が無くても動く）
    let out = import_as(Some(0), &conf, "gdrive", &out_path);
    assert!(
        !out.stderr.contains("root では動かしません"),
        "{}",
        out.stderr
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out_path.exists());
    let (mut o, mut e) = (Vec::new(), Vec::new());
    let args: Vec<String> = ["recover"].iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(run_command(Some(0), &args, &mut o, &mut e), EXIT_USAGE);
    // 引数の誤りは 2
    let args: Vec<String> = ["import-credentials", "--remote", "gdrive"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let (mut o, mut e) = (Vec::new(), Vec::new());
    assert_eq!(main_with(&args, &mut o, &mut e), EXIT_USAGE);
}

/// root が取り込むときは、パスの途中のディレクトリ（親より上）も確かめる。試験は root でないので、検査の関数を直接呼ぶ
/// （一番近い上のディレクトリで決まるものだけを見る。それより上の環境の状態には依らない）。
#[test]
fn ancestors_above_the_parent_must_be_owned_by_root_or_the_file_owner_and_not_writable_by_others() {
    use std::os::unix::fs::PermissionsExt;
    let me = rclone_conf::effective_uid();
    // 何も問題のない上のディレクトリだけの道（`/` は root の持ち物で、書き込めない）
    assert_eq!(
        rclone_conf::check_ancestors(std::path::Path::new("/usr"), me),
        Ok(())
    );
    assert_eq!(
        rclone_conf::check_ancestors(std::path::Path::new("/"), me),
        Ok(())
    );

    let dir = TempDir::new("import-ancestors");
    let a = dir.path().join("a");
    let parent = a.join("b");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    // 一番近い上のディレクトリ（a）を、グループ・その他が書き込める → 断る
    for mode in [0o775, 0o757, 0o777, 0o722] {
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            rclone_conf::check_ancestors(&parent, me),
            Err(ConfigError::TooPermissive(Target::Directory)),
            "{mode:o}"
        );
    }
    // 持ち主が、root でも、設定ファイルの持ち主でもない → 断る（この試験のユーザーが root でなければ）
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o755)).unwrap();
    if me != 0 {
        assert_eq!(
            rclone_conf::check_ancestors(&parent, me + 1),
            Err(ConfigError::WrongOwner(Target::Directory))
        );
    }
    // シンボリックリンクを通って来ても、実際のディレクトリを見る
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o777)).unwrap();
    let link = dir.path().join("link");
    symlink(&a, &link).unwrap();
    assert_eq!(
        rclone_conf::check_ancestors(&link.join("b"), me),
        Err(ConfigError::TooPermissive(Target::Directory))
    );
    std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o755)).unwrap();
}
