//! T6: トークンのファイルの検査（design.md 3.4。AC7 の「弱いトークンのファイルでは起動しない」）。ファイルの作り方ごとに、
//! 受け付けない理由を確かめる。トークンの値は試験の中で作る架空の値。

mod common;

use std::os::unix::fs::symlink;
use std::path::PathBuf;

use common::{TempDir, set_mode};
use geoframe_export::serve::{self, TokenError, load_token, load_token_for_uid};

const GOOD: &str = "TOK-q8Zr4Wm2Vx7Jc5Hn3Bd9Ks6Lt1Pf0YaE";

fn write(dir: &TempDir, name: &str, content: &[u8], mode: u32) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("write");
    set_mode(&path, mode);
    path
}

#[test]
fn a_private_regular_file_with_a_long_enough_value_is_accepted() {
    let dir = TempDir::new("token-ok");
    for (name, content, mode) in [
        ("plain", GOOD.to_owned(), 0o600),
        ("newline", format!("{GOOD}\n"), 0o600),
        ("crlf", format!("\r\n{GOOD}\r\n"), 0o400),
    ] {
        let path = write(&dir, name, content.as_bytes(), mode);
        assert!(load_token(&path).is_ok(), "{name}");
    }
}

#[test]
fn files_that_others_can_read_or_that_are_not_ours_are_refused() {
    let dir = TempDir::new("token-perm");
    for mode in [0o644, 0o640, 0o604, 0o660, 0o666, 0o601] {
        let path = write(&dir, &format!("m{mode:o}"), GOOD.as_bytes(), mode);
        assert_eq!(
            load_token(&path).map(|_| ()),
            Err(TokenError::TooPermissive),
            "{mode:o}"
        );
    }
    // 別のユーザーの持ち物（持ち主を、実行ユーザーでない値として検査する）
    let path = write(&dir, "owned", GOOD.as_bytes(), 0o600);
    let other = geoframe_export::sys::euid().wrapping_add(1);
    assert_eq!(
        load_token_for_uid(&path, other).map(|_| ()),
        Err(TokenError::WrongOwner)
    );
    assert!(load_token_for_uid(&path, geoframe_export::sys::euid()).is_ok());
}

#[test]
fn links_directories_fifos_and_missing_files_are_refused_without_following() {
    let dir = TempDir::new("token-kind");
    let real = write(&dir, "real", GOOD.as_bytes(), 0o600);
    // シンボリックリンク（指す先が良い値でも、たどらない）
    let link = dir.path().join("link");
    symlink(&real, &link).unwrap();
    assert_eq!(load_token(&link).map(|_| ()), Err(TokenError::NotRegular));
    // ぶら下がったリンク
    let dangling = dir.path().join("dangling");
    symlink(dir.path().join("nowhere"), &dangling).unwrap();
    assert_eq!(
        load_token(&dangling).map(|_| ()),
        Err(TokenError::NotRegular)
    );
    // ディレクトリ
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(load_token(&sub).map(|_| ()), Err(TokenError::NotRegular));
    // FIFO（開くのを待たない）
    let fifo = dir.path().join("fifo");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: NUL で終わるパスと、権限だけを渡す。作るのはこの試験のディレクトリの中の FIFO だけ。
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(load_token(&fifo).map(|_| ()), Err(TokenError::NotRegular));
    // 無い
    assert!(matches!(
        load_token(&dir.path().join("missing")),
        Err(TokenError::Unreadable(std::io::ErrorKind::NotFound))
    ));
}

#[test]
fn weak_or_odd_values_are_refused() {
    let dir = TempDir::new("token-weak");
    let cases: Vec<(&str, Vec<u8>, TokenError)> = vec![
        ("empty", Vec::new(), TokenError::TooShort),
        ("short", b"short-token\n".to_vec(), TokenError::TooShort),
        (
            "thirty-one",
            GOOD.as_bytes()[..31].to_vec(),
            TokenError::TooShort,
        ),
        (
            "newlines-padded",
            format!("{}\n\n\n\n\n\n", &GOOD[..20]).into_bytes(),
            TokenError::TooShort,
        ),
        (
            "same-char",
            "a".repeat(64).into_bytes(),
            TokenError::LowVariety,
        ),
        (
            "space",
            format!("{} {}", &GOOD[..16], &GOOD[16..]).into_bytes(),
            TokenError::BadCharacters,
        ),
        (
            "inner-newline",
            format!("{}\n{}", &GOOD[..16], &GOOD[16..]).into_bytes(),
            TokenError::BadCharacters,
        ),
        (
            "japanese",
            format!("{GOOD}あ").into_bytes(),
            TokenError::BadCharacters,
        ),
        (
            "nul",
            [GOOD.as_bytes(), b"\0"].concat(),
            TokenError::BadCharacters,
        ),
        ("huge", vec![b'x'; 100_000], TokenError::TooLong),
    ];
    for (name, content, expect) in cases {
        let path = write(&dir, name, &content, 0o600);
        assert_eq!(load_token(&path).map(|_| ()), Err(expect), "{name}");
    }
}

#[test]
fn error_messages_never_carry_the_path_or_the_value() {
    let dir = TempDir::new("token-msg");
    let path = write(
        &dir,
        "secret-name-xyz",
        format!("{GOOD} bad").as_bytes(),
        0o600,
    );
    let e = load_token(&path).unwrap_err();
    let text = format!("{e} {e:?}");
    assert!(
        !text.contains("secret-name-xyz") && !text.contains(&GOOD[..12]),
        "{text}"
    );
    let t = load_token(&write(&dir, "ok", GOOD.as_bytes(), 0o600)).unwrap();
    assert!(!format!("{t:?}").contains(&GOOD[..12]));
    let _ = serve::TOKEN_MIN_LEN;
}
