//! `DATA_DIR` の外へ出ないパスの検査（inventory.md §6.2）。
//!
//! DB の `display`・`path` 列は**信用しない**。現行 Python は画像を読む箇所ごとに
//! `path.resolve().relative_to(DATA_DIR)` で検査している（`quality.py`・`ai_pipeline.py:_safe_path`・
//! `loc_suggest.py:_image_bytes`・`travel_blog.py:_photo_source`）。Rust の取り込み・読み出しは、
//! 文字列のパスを**必ずここを通して**ファイルにする。
//!
//! 検査は 2 段:
//! 1. 文字列として: 空・NUL・絶対パス・`..` の要素を拒否する（`.` は読み飛ばす）。
//! 2. ファイルシステムとして: 実在する最も深い祖先を `canonicalize` し、シンボリックリンクを辿った先が
//!    `DATA_DIR`（これも `canonicalize` 済み）の中にあることを確かめる。
//!
//! 書き込み先は存在しないので、親ディレクトリを作ってから親を検査する。ファイル名の規則
//! （[`check_file_name`]）は `derivatives/` に置く名前に使う。
//!
//! 検査と使用のあいだの差し替え（TOCTOU）は防がない。置き場は取り込み自身だけが書くので、
//! 悪意のある並行書き込みは想定しない。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// 空、または NUL を含む。
    Invalid,
    /// 絶対パス、または `..` を含む。
    Traversal,
    /// 解決した先（シンボリックリンクを辿った後）が `DATA_DIR` の外。
    Escapes,
    /// ファイル名として使えない文字・形。
    BadName,
    /// ファイルシステムの失敗（`ErrorKind` のみ。パスは持たない）。
    Io(io::ErrorKind),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("空か不正な文字を含む"),
            Self::Traversal => f.write_str("絶対パスか親ディレクトリを含む"),
            Self::Escapes => f.write_str("データ置き場の外を指す"),
            Self::BadName => f.write_str("ファイル名として使えない"),
            Self::Io(kind) => write!(f, "入出力に失敗（{kind}）"),
        }
    }
}

impl std::error::Error for PathError {}

impl From<io::Error> for PathError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.kind())
    }
}

/// 検査済みの `DATA_DIR`（`canonicalize` した絶対パス）。
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

/// `derivatives/` に置く 1 つのファイルの名前（拡張子 `.jpg`）に使える文字だけか。
/// 英数字・`_`・`-`・`.`、先頭は `.` でない、`..` を含まない、1〜200 文字。
pub fn check_file_name(name: &str) -> Result<(), PathError> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok { Ok(()) } else { Err(PathError::BadName) }
}

/// Drive のファイル ID に使える文字か（英数字・`_`・`-`、1〜128 文字）。ID はファイル名に使うので、
/// DB の行や Drive の応答がパスの区切りを含んでいても、`derivatives/` の外へは出られない。
pub fn check_drive_id(id: &str) -> Result<(), PathError> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if ok { Ok(()) } else { Err(PathError::BadName) }
}

/// DB の相対パス文字列の**字面の検査**（`DATA_DIR` の中を指す相対パスの要素にする）。空・NUL は `Invalid`、
/// 絶対パス・`..` は `Traversal`、`.` は読み飛ばす。要素が残らない（`.` だけ）のも `Invalid`。
/// ファイルシステムには触れない。[`DataDir`] の解決と、`/thumb/` が `display` を受ける前の検査が共有する。
pub(crate) fn relative_parts(rel: &str) -> Result<Vec<&str>, PathError> {
    if rel.is_empty() || rel.contains('\0') {
        return Err(PathError::Invalid);
    }
    let mut parts = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(part) => parts.push(part.to_str().ok_or(PathError::Invalid)?),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PathError::Traversal);
            }
        }
    }
    if parts.is_empty() {
        return Err(PathError::Invalid); // `DATA_DIR` そのもの（`.` だけ）は使わない
    }
    Ok(parts)
}

/// サムネイルの世代（12 桁の小文字 hex）か。
pub fn is_thumb_gen(generation: &str) -> bool {
    generation.len() == 12
        && generation
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl DataDir {
    /// `DATA_DIR` を開く（存在し、ディレクトリであること）。
    pub fn open(path: &Path) -> Result<Self, PathError> {
        let root = fs::canonicalize(path)?;
        if !root.is_dir() {
            return Err(PathError::Io(io::ErrorKind::NotADirectory));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// DB の相対パス文字列を、`DATA_DIR` の中の絶対パスにする（文字列の検査だけ。存在は問わない）。
    fn lexical(&self, rel: &str) -> Result<PathBuf, PathError> {
        let mut out = self.root.clone();
        out.extend(relative_parts(rel)?);
        Ok(out)
    }

    /// 実在するファイルを読むための絶対パス。シンボリックリンクを辿った先も `DATA_DIR` の中でなければ拒否する。
    /// 戻り値は `canonicalize` 済み（リンクを解決した実体）。
    pub fn resolve_existing(&self, rel: &str) -> Result<PathBuf, PathError> {
        let path = self.lexical(rel)?;
        let real = fs::canonicalize(&path)?;
        if real.starts_with(&self.root) {
            Ok(real)
        } else {
            Err(PathError::Escapes)
        }
    }

    /// `derivatives/<name>` の書き込み先。`derivatives/` を作り、その実体が `DATA_DIR` の中であることを確かめる。
    /// 戻り値は `(絶対パス, DB の display に入れる相対パス)`。`name` は [`check_file_name`] を通す。
    pub fn derivative_dest(&self, name: &str) -> Result<(PathBuf, String), PathError> {
        check_file_name(name)?;
        let dir = self.root.join("derivatives");
        fs::create_dir_all(&dir)?;
        let real = fs::canonicalize(&dir)?;
        if real != dir {
            // `derivatives` がシンボリックリンクなら、その先が `DATA_DIR` の中でも `display`
            // （`derivatives/...`）と実体が食い違うので使わない
            return Err(PathError::Escapes);
        }
        Ok((dir.join(name), format!("derivatives/{name}")))
    }

    /// サムネイルの書き込み先 `thumbs/<id>-<gen>.jpg`。`thumbs/` を作り、その実体が `DATA_DIR` の直下のその
    /// ディレクトリ自身であること（リンクでないこと）を確かめる（[`derivative_dest`](Self::derivative_dest) と同じ）。
    /// `generation` は 12 桁の小文字 hex。
    pub fn thumb_dest(&self, id: i64, generation: &str) -> Result<PathBuf, PathError> {
        if id < 1 || !is_thumb_gen(generation) {
            return Err(PathError::BadName);
        }
        let dir = self.root.join("thumbs");
        fs::create_dir_all(&dir)?;
        let real = fs::canonicalize(&dir)?;
        if real != dir {
            return Err(PathError::Escapes);
        }
        Ok(dir.join(format!("{id}-{generation}.jpg")))
    }

    /// 既にある `thumbs/` の絶対パス（古い世代の掃除用。**作らない**）。リンク・ディレクトリでないものは
    /// `Escapes`、無ければ入出力の失敗（`NotFound`）。
    pub fn thumbs_dir(&self) -> Result<PathBuf, PathError> {
        let dir = self.root.join("thumbs");
        let meta = fs::symlink_metadata(&dir)?;
        if !meta.is_dir() {
            return Err(PathError::Escapes);
        }
        let real = fs::canonicalize(&dir)?;
        if real != dir {
            return Err(PathError::Escapes);
        }
        Ok(dir)
    }

    /// 行の `display`（`derivatives/<名前>`）が指す derivative の絶対パス。**消す前の検査用**。
    /// `derivatives/` の直下の 1 ファイルで、名前が [`check_file_name`] を通り、`derivatives/` が
    /// シンボリックリンクでないときだけ返す（DB の値を信用しない。`..` や別の階層は断る）。
    /// ファイルが無くてもよい（存在は見ない）。最後の要素がリンクでも、消すのはそのリンク自体。
    pub fn derivative_file(&self, display: &str) -> Result<PathBuf, PathError> {
        let name = display
            .strip_prefix("derivatives/")
            .ok_or(PathError::BadName)?;
        check_file_name(name)?;
        let dir = self.root.join("derivatives");
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => Ok(dir.join(name)),
            Ok(_) => Err(PathError::Escapes),
            Err(e) => Err(e.into()),
        }
    }

    /// DB の相対パス文字列が指す**エントリそのもの**の絶対パス（最後の要素のリンクは辿らない）。
    /// 親ディレクトリは `canonicalize` して `DATA_DIR` の中であることを確かめる。ファイルを**移す・消す**
    /// ときに使う（[`resolve_existing`](Self::resolve_existing) はリンクを辿って実体を返すので、
    /// 移すと実体を動かしてしまう）。親が無い（`NotFound`）ときは入出力の失敗。
    pub fn entry_path(&self, rel: &str) -> Result<PathBuf, PathError> {
        let lexical = self.lexical(rel)?;
        let (Some(parent), Some(name)) = (lexical.parent(), lexical.file_name()) else {
            return Err(PathError::Invalid);
        };
        let real_parent = fs::canonicalize(parent)?;
        if !real_parent.starts_with(&self.root) {
            return Err(PathError::Escapes);
        }
        Ok(real_parent.join(name))
    }

    /// 取り込みの一時ファイル `tmp/.dl_<drive_id>` の絶対パス。`tmp/` を作る。
    pub fn download_tmp(&self, drive_id: &str) -> Result<PathBuf, PathError> {
        check_drive_id(drive_id)?;
        let dir = self.root.join("tmp");
        fs::create_dir_all(&dir)?;
        let real = fs::canonicalize(&dir)?;
        if real != dir {
            return Err(PathError::Escapes);
        }
        Ok(dir.join(format!(".dl_{drive_id}")))
    }

    /// 絶対パスを `DATA_DIR` からの相対パス（`/` 区切り）にする。外なら `None`。
    pub fn relative(&self, abs: &Path) -> Option<String> {
        let rel = abs.strip_prefix(&self.root).ok()?;
        let parts: Vec<_> = rel
            .components()
            .map(|c| match c {
                Component::Normal(p) => p.to_str().map(str::to_owned),
                _ => None,
            })
            .collect::<Option<_>>()?;
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("/"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::symlink;

    fn setup() -> (TempDir, DataDir) {
        let tmp = TempDir::new("datadir");
        fs::create_dir_all(tmp.path().join("photos/sub")).unwrap();
        fs::write(tmp.path().join("photos/a.jpg"), b"x").unwrap();
        fs::write(tmp.path().join("photos/sub/b.jpg"), b"y").unwrap();
        let dir = DataDir::open(tmp.path()).unwrap();
        (tmp, dir)
    }

    #[test]
    fn 置き場の中のファイルは解決できる() {
        let (_t, d) = setup();
        let p = d.resolve_existing("photos/a.jpg").unwrap();
        assert!(p.starts_with(d.root()) && p.ends_with("photos/a.jpg"));
        // `.` は読み飛ばす
        assert!(d.resolve_existing("./photos/./sub/b.jpg").is_ok());
        assert_eq!(d.relative(&p).as_deref(), Some("photos/a.jpg"));
    }

    #[test]
    fn 絶対パスと親ディレクトリと不正な文字は拒否する() {
        let (_t, d) = setup();
        for bad in ["../x", "photos/../../x", "/etc/passwd", "photos/..", ".."] {
            assert_eq!(
                d.resolve_existing(bad).unwrap_err(),
                PathError::Traversal,
                "{bad}"
            );
        }
        for bad in ["", "a\0b", "."] {
            assert_eq!(
                d.resolve_existing(bad).unwrap_err(),
                PathError::Invalid,
                "{bad:?}"
            );
        }
        // 存在しないものは入出力の失敗（外かどうかの判定には進まない）
        assert!(matches!(
            d.resolve_existing("photos/none.jpg").unwrap_err(),
            PathError::Io(_)
        ));
    }

    #[test]
    fn サムネイルの書き込み先は_thumbs直下で世代の形を検査する() {
        let (t, d) = setup();
        let dest = d.thumb_dest(42, "0123456789ab").unwrap();
        assert_eq!(dest, d.root().join("thumbs/42-0123456789ab.jpg"));
        assert!(t.path().join("thumbs").is_dir());
        for (id, generation) in [
            (0, "0123456789ab"),
            (-1, "0123456789ab"),
            (1, "0123456789AB"),
            (1, "0123456789a"),
            (1, "../1234567890"),
            (1, ""),
        ] {
            assert_eq!(
                d.thumb_dest(id, generation).unwrap_err(),
                PathError::BadName,
                "{id} {generation}"
            );
        }
        assert_eq!(d.thumbs_dir().unwrap(), d.root().join("thumbs"));
    }

    #[test]
    fn サムネイルの置き場がリンクなら書かない() {
        let (t, d) = setup();
        let elsewhere = TempDir::new("datadir-thumbs-elsewhere");
        symlink(elsewhere.path(), t.path().join("thumbs")).unwrap();
        assert_eq!(
            d.thumb_dest(1, "0123456789ab").unwrap_err(),
            PathError::Escapes
        );
        assert_eq!(d.thumbs_dir().unwrap_err(), PathError::Escapes);
        assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn 掃除用の置き場は無ければ作らない() {
        let (t, d) = setup();
        assert!(matches!(d.thumbs_dir().unwrap_err(), PathError::Io(_)));
        assert!(!t.path().join("thumbs").exists());
    }

    #[test]
    fn 字面の検査は要素を返し_を読み飛ばす() {
        assert_eq!(
            relative_parts("./photos/./sub//b.jpg").unwrap(),
            ["photos", "sub", "b.jpg"]
        );
        assert_eq!(relative_parts(".").unwrap_err(), PathError::Invalid);
        assert_eq!(relative_parts("a/../b").unwrap_err(), PathError::Traversal);
    }

    #[test]
    fn 外を指すシンボリックリンクは拒否する() {
        let (t, d) = setup();
        let outside = TempDir::new("datadir-outside");
        fs::write(outside.path().join("secret.jpg"), b"s").unwrap();
        symlink(
            outside.path().join("secret.jpg"),
            t.path().join("photos/link.jpg"),
        )
        .unwrap();
        symlink(outside.path(), t.path().join("photos/dirlink")).unwrap();
        assert_eq!(
            d.resolve_existing("photos/link.jpg").unwrap_err(),
            PathError::Escapes
        );
        assert_eq!(
            d.resolve_existing("photos/dirlink/secret.jpg").unwrap_err(),
            PathError::Escapes
        );
        // 置き場の中を指すリンクは通る
        symlink(
            t.path().join("photos/a.jpg"),
            t.path().join("photos/inner.jpg"),
        )
        .unwrap();
        assert!(d.resolve_existing("photos/inner.jpg").is_ok());
    }

    #[test]
    fn derivativeの書き込み先は名前の規則を通す() {
        let (t, d) = setup();
        let (abs, rel) = d.derivative_dest("abc_DEF-1.r90.jpg").unwrap();
        assert_eq!(rel, "derivatives/abc_DEF-1.r90.jpg");
        assert!(abs.starts_with(d.root()) && t.path().join("derivatives").is_dir());
        for bad in [
            "",
            ".hidden.jpg",
            "a/b.jpg",
            "../a.jpg",
            "a..jpg",
            "a b.jpg",
            "a\0.jpg",
            "日本.jpg",
        ] {
            assert_eq!(
                d.derivative_dest(bad).unwrap_err(),
                PathError::BadName,
                "{bad:?}"
            );
        }
        assert!(d.derivative_dest(&"a".repeat(201)).is_err());
    }

    #[test]
    fn derivativesがリンクなら使わない() {
        let (t, d) = setup();
        let outside = TempDir::new("datadir-outside2");
        symlink(outside.path(), t.path().join("derivatives")).unwrap();
        assert_eq!(d.derivative_dest("a.jpg").unwrap_err(), PathError::Escapes);
    }

    #[test]
    fn driveのidは区切りを含められない() {
        let (_t, d) = setup();
        assert!(d.download_tmp("1AbC_d-e").is_ok());
        for bad in ["", "a/b", "../x", "a.b", "a b", &"x".repeat(129)] {
            assert_eq!(
                d.download_tmp(bad).unwrap_err(),
                PathError::BadName,
                "{bad:?}"
            );
        }
        let p = d.download_tmp("abc").unwrap();
        assert!(p.ends_with("tmp/.dl_abc"));
    }

    #[test]
    fn 消してよいderivativeはderivativesの直下の安全な名前だけ() {
        let (t, d) = setup();
        fs::create_dir_all(t.path().join("derivatives")).unwrap();
        let p = d.derivative_file("derivatives/local_0123.jpg").unwrap();
        assert!(p.ends_with("derivatives/local_0123.jpg"));
        for bad in [
            "photos/a.jpg",
            "derivatives/",
            "derivatives/../catalog.db",
            "derivatives/sub/a.jpg",
            "derivatives/.hidden.jpg",
            "/etc/passwd",
            "derivatives",
            "",
        ] {
            assert!(d.derivative_file(bad).is_err(), "{bad:?}");
        }
        // `derivatives` がリンクなら使わない
        let outside = TempDir::new("datadir-outside3");
        fs::remove_dir_all(t.path().join("derivatives")).unwrap();
        symlink(outside.path(), t.path().join("derivatives")).unwrap();
        assert_eq!(
            d.derivative_file("derivatives/a.jpg").unwrap_err(),
            PathError::Escapes
        );
    }

    #[test]
    fn エントリのパスはリンクを辿らず親だけを検査する() {
        let (t, d) = setup();
        let outside = TempDir::new("datadir-outside4");
        fs::write(outside.path().join("secret.jpg"), b"s").unwrap();
        symlink(
            outside.path().join("secret.jpg"),
            t.path().join("photos/link.jpg"),
        )
        .unwrap();
        // 最後の要素がリンクでも、リンクそのもののパスを返す（外を指していても、ここでは断らない）
        let p = d.entry_path("photos/link.jpg").unwrap();
        assert!(p.ends_with("photos/link.jpg"));
        assert!(fs::symlink_metadata(&p).unwrap().file_type().is_symlink());
        // 親が外へ出るリンクは断る。存在しない親は入出力の失敗
        symlink(outside.path(), t.path().join("photos/dirlink")).unwrap();
        assert_eq!(
            d.entry_path("photos/dirlink/secret.jpg").unwrap_err(),
            PathError::Escapes
        );
        assert!(matches!(
            d.entry_path("photos/none/x.jpg").unwrap_err(),
            PathError::Io(_)
        ));
        assert_eq!(d.entry_path("../x.jpg").unwrap_err(), PathError::Traversal);
    }

    #[test]
    fn 相対パスへ戻せるのは置き場の中だけ() {
        let (_t, d) = setup();
        assert_eq!(d.relative(Path::new("/etc/passwd")), None);
        assert_eq!(d.relative(d.root()), None);
    }
}
