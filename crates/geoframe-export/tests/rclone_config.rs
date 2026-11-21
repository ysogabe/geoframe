//! rclone 設定ファイルの起動時の検査（専用の client_id、持ち主・権限）。値をエラーに出さないことも確かめる。

mod common;

use std::os::unix::fs::symlink;

use common::{
    MARK_ID, MARK_SECRET, TempDir, assert_no_secrets, good_config, set_mode, write_config,
};
use geoframe_export::rclone_conf::{
    CONFIG_MAX_BYTES, ConfigError, Target, check_rclone_config, check_rclone_config_for_uid,
    effective_uid,
};

fn check(path: &std::path::Path) -> Result<(), ConfigError> {
    check_rclone_config(path, "gdrive")
}

/// 失敗の表示（Display・Debug）に設定の値とパスが出ていないこと。
fn assert_clean(e: &ConfigError, dir: &TempDir) {
    for text in [e.to_string(), format!("{e:?}")] {
        assert_no_secrets(&text);
        assert!(!text.contains(dir.path().to_str().unwrap()), "{text}");
    }
}

#[test]
fn a_dedicated_config_with_safe_permissions_passes() {
    let dir = TempDir::new("conf-ok");
    let path = write_config(dir.path(), good_config("gdrive").as_bytes());
    assert_eq!(check(&path), Ok(()));
}

#[test]
fn the_client_id_and_secret_must_be_present_and_non_empty() {
    let dir = TempDir::new("conf-keys");
    let cases: [(&str, ConfigError); 7] = [
        // 共用の client_id のまま（キーが無い）
        (
            &format!("[gdrive]\ntype = drive\nclient_secret = {MARK_SECRET}\n"),
            ConfigError::MissingKey("client_id"),
        ),
        (
            &format!("[gdrive]\ntype = drive\nclient_id = {MARK_ID}\n"),
            ConfigError::MissingKey("client_secret"),
        ),
        (
            &format!("[gdrive]\nclient_id =\nclient_secret = {MARK_SECRET}\n"),
            ConfigError::EmptyValue("client_id"),
        ),
        (
            &format!("[gdrive]\nclient_id = {MARK_ID}\nclient_secret =   \n"),
            ConfigError::EmptyValue("client_secret"),
        ),
        // セクションが無い（別の remote の設定だけ）
        (&good_config("other"), ConfigError::MissingSection),
        // セクションの名前が部分一致
        (&good_config("gdrive2"), ConfigError::MissingSection),
        ("", ConfigError::MissingSection),
    ];
    for (content, want) in cases {
        let path = write_config(dir.path(), content.as_bytes());
        let got = check(&path).unwrap_err();
        assert_eq!(got, want, "{content:?}");
        assert_clean(&got, &dir);
    }
}

#[test]
fn another_remote_having_the_values_does_not_count() {
    let dir = TempDir::new("conf-other-remote");
    let content = format!(
        "[other]\nclient_id = {MARK_ID}\nclient_secret = {MARK_SECRET}\n\n[gdrive]\ntype = drive\ntoken = {{}}\n"
    );
    let path = write_config(dir.path(), content.as_bytes());
    let e = check(&path).unwrap_err();
    assert_eq!(e, ConfigError::MissingKey("client_id"));
    assert_clean(&e, &dir);
}

#[test]
fn a_file_readable_by_group_or_others_is_refused() {
    let dir = TempDir::new("conf-file-mode");
    let path = write_config(dir.path(), good_config("gdrive").as_bytes());
    for mode in [
        0o640, 0o604, 0o644, 0o660, 0o606, 0o666, 0o602, 0o620, 0o601,
    ] {
        set_mode(&path, mode);
        let e = check(&path).unwrap_err();
        assert_eq!(e, ConfigError::TooPermissive(Target::File), "{mode:o}");
        assert_clean(&e, &dir);
    }
    // 0400・0600 は通る
    for mode in [0o400, 0o600] {
        set_mode(&path, mode);
        assert_eq!(check(&path), Ok(()), "{mode:o}");
    }
}

#[test]
fn a_parent_directory_open_to_group_or_others_is_refused() {
    let dir = TempDir::new("conf-dir-mode");
    let path = write_config(dir.path(), good_config("gdrive").as_bytes());
    let parent = path.parent().unwrap();
    for mode in [0o750, 0o701, 0o755, 0o770, 0o707, 0o777, 0o710, 0o740] {
        set_mode(parent, mode);
        let e = check(&path).unwrap_err();
        assert_eq!(e, ConfigError::TooPermissive(Target::Directory), "{mode:o}");
        assert_clean(&e, &dir);
    }
    set_mode(parent, 0o700);
    assert_eq!(check(&path), Ok(()));
}

#[test]
fn files_and_directories_owned_by_someone_else_are_refused() {
    let dir = TempDir::new("conf-owner");
    let path = write_config(dir.path(), good_config("gdrive").as_bytes());
    // 実行ユーザーが別の UID だったら、という見立て（別ユーザーの持ち物は root なしでは作れない）
    let other = effective_uid().wrapping_add(1);
    let e = check_rclone_config_for_uid(&path, "gdrive", other).unwrap_err();
    assert_eq!(e, ConfigError::WrongOwner(Target::Directory));
    assert_clean(&e, &dir);
    assert_eq!(
        check_rclone_config_for_uid(&path, "gdrive", effective_uid()),
        Ok(())
    );
}

#[test]
fn a_missing_file_a_symlink_and_a_directory_are_refused() {
    let dir = TempDir::new("conf-kind");
    let path = write_config(dir.path(), good_config("gdrive").as_bytes());

    let missing = path.with_file_name("absent.conf");
    assert!(matches!(
        check(&missing),
        Err(ConfigError::Unreadable(Target::File, _))
    ));

    // シンボリックリンクはたどらない（確かめた後の差し替えを防ぐ）
    let link = path.with_file_name("link.conf");
    symlink(&path, &link).unwrap();
    assert_eq!(check(&link), Err(ConfigError::WrongKind(Target::File)));

    // ディレクトリをファイルとして指す
    let sub = path.with_file_name("subdir");
    std::fs::create_dir(&sub).unwrap();
    set_mode(&sub, 0o700);
    assert_eq!(check(&sub), Err(ConfigError::WrongKind(Target::File)));

    // 親が無い
    let no_parent = dir.path().join("nodir").join("rclone.conf");
    assert!(matches!(
        check(&no_parent),
        Err(ConfigError::Unreadable(Target::Directory, _))
    ));
}

#[test]
fn an_unsafe_parent_is_reported_before_the_file_is_read() {
    // 親が広いときは、ファイルの中身（値の欠け）は見ずに親の権限を先に断る
    let dir = TempDir::new("conf-order");
    let path = write_config(dir.path(), b"[gdrive]\n");
    set_mode(path.parent().unwrap(), 0o755);
    assert_eq!(
        check(&path),
        Err(ConfigError::TooPermissive(Target::Directory))
    );
}

#[test]
fn oversized_and_non_utf8_files_are_refused_without_echoing_content() {
    let dir = TempDir::new("conf-size");
    let mut big = good_config("gdrive").into_bytes();
    big.resize(CONFIG_MAX_BYTES as usize + 1, b'#');
    let path = write_config(dir.path(), &big);
    let e = check(&path).unwrap_err();
    assert_eq!(e, ConfigError::TooLarge);
    assert_clean(&e, &dir);

    // 上限ちょうどは通る（コメントで埋める）
    let mut ok = good_config("gdrive").into_bytes();
    ok.resize(CONFIG_MAX_BYTES as usize, b'\n');
    let path = write_config(dir.path(), &ok);
    assert_eq!(check(&path), Ok(()));

    let mut bad = good_config("gdrive").into_bytes();
    bad.extend_from_slice(&[0xff, 0xfe]);
    let path = write_config(dir.path(), &bad);
    let e = check(&path).unwrap_err();
    assert_eq!(e, ConfigError::NotUtf8);
    assert_clean(&e, &dir);
}

#[test]
fn malformed_lines_never_leak_into_the_error() {
    let dir = TempDir::new("conf-malformed");
    let content = format!(
        "[gdrive\nclient_id {MARK_ID}\n= {MARK_SECRET}\n[gdrive]\nclient_secret\n{MARK_ID} {MARK_SECRET}\n"
    );
    let path = write_config(dir.path(), content.as_bytes());
    let e = check(&path).unwrap_err();
    assert!(matches!(e, ConfigError::MissingKey(_)), "{e:?}");
    assert_clean(&e, &dir);
}
