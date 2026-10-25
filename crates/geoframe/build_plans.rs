//! `build.rs` が旅程 JSON（`app/travel_blog/plans/*.json`）を埋め込む前の検査（リンクと名前）。
//!
//! `build.rs` と、`src/embedded.rs` の単体試験（`#[path]` で同じファイルを読む）が共有する。
//! エラーにパス・ファイル名・I/O エラーの中身は入れない（旅程の名前は個人の旅の手がかりなので、
//! ビルドのログに出さない）。メッセージは定型文だけ。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum PlanError {
    /// 旅程のディレクトリか、そこへ至るパスの途中にシンボリックリンク、または直下にリンクがある。
    Link,
    /// 読めないディレクトリ・ファイルがある。
    Unreadable,
    /// UTF-8 でないファイル名がある。
    BadName,
}

impl PlanError {
    pub fn message(&self) -> &'static str {
        match self {
            PlanError::Link => {
                "app/travel_blog/plans かその親にシンボリックリンクがある（埋め込まない。実体にすること）"
            }
            PlanError::Unreadable => {
                "app/travel_blog/plans に読めないディレクトリかファイルがある（埋め込めない）"
            }
            PlanError::BadName => {
                "app/travel_blog/plans に埋め込めない名前（UTF-8 でない）のファイルがある"
            }
        }
    }
}

/// `root` から `components` を順にたどり、途中（最後の要素を含む）にシンボリックリンクがあれば `Link`。
/// 存在しない要素に着いたらそこで止めて通す（ディレクトリが無ければ空の表になるだけ）。
/// `root` 自体は見ない（リポジトリの置き場所は利用者の自由）。
pub fn reject_links(root: &Path, components: &[&str]) -> Result<(), PlanError> {
    let mut path = root.to_path_buf();
    for component in components {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(PlanError::Link),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(PlanError::Unreadable),
        }
    }
    Ok(())
}

/// `dir` 直下の通常ファイルのうち、`*.json`（ドット始まりを除く）を名前順で返す。ディレクトリが無ければ空。
/// リンクは辿らずに `Link`。読めない・UTF-8 でない名前も止める（黙って外さない）。
pub fn plan_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, PlanError> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(PlanError::Unreadable),
    };
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|_| PlanError::Unreadable)?;
        let file_type = entry.file_type().map_err(|_| PlanError::Unreadable)?;
        if file_type.is_symlink() {
            return Err(PlanError::Link);
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| PlanError::BadName)?;
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        if file_type.is_file() {
            // 読めないファイルは include_bytes! の失敗（パスが出る）になる前に、ここで止める
            fs::File::open(entry.path()).map_err(|_| PlanError::Unreadable)?;
            out.push((name, entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// 一時ディレクトリ（終わりに自分で作ったものだけを消す）。
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let dir =
                std::env::temp_dir().join(format!("gf-build-plans-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            // 読めないディレクトリを残さない
            if let Ok(read) = fs::read_dir(&self.0) {
                for e in read.flatten() {
                    let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(0o755));
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PLANS: [&str; 3] = ["app", "travel_blog", "plans"];

    fn make_repo(t: &Tmp) -> PathBuf {
        let plans = t.0.join("repo/app/travel_blog/plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(plans.join("a.json"), "{}").unwrap();
        t.0.join("repo")
    }

    #[test]
    fn plain_directories_pass_and_missing_ones_are_empty() {
        let t = Tmp::new("plain");
        let root = make_repo(&t);
        assert_eq!(reject_links(&root, &PLANS), Ok(()));
        let files = plan_files(&root.join("app/travel_blog/plans")).unwrap();
        assert_eq!(files.len(), 1);
        // 途中から無い場合は、検査を通して空の表
        assert_eq!(reject_links(&t.0.join("none"), &PLANS), Ok(()));
        assert_eq!(plan_files(&t.0.join("none")).unwrap(), Vec::new());
    }

    #[test]
    fn a_link_at_every_level_of_the_path_is_refused() {
        for level in 0..PLANS.len() {
            let t = Tmp::new(&format!("lv{level}"));
            let root = make_repo(&t);
            // リンク先は別の場所に実体を持つ（リポジトリの外のファイルが混ざる想定）
            let elsewhere = t.0.join("elsewhere");
            let target_rel: PathBuf = PLANS[..=level].iter().collect();
            let link = root.join(&target_rel);
            fs::rename(&link, &elsewhere).unwrap();
            symlink(&elsewhere, &link).unwrap();
            assert_eq!(
                reject_links(&root, &PLANS),
                Err(PlanError::Link),
                "level {level}"
            );
        }
    }

    #[test]
    fn a_dangling_link_in_the_middle_is_refused() {
        let t = Tmp::new("dangling");
        let root = t.0.join("repo");
        fs::create_dir_all(&root).unwrap();
        symlink(t.0.join("gone"), root.join("app")).unwrap();
        assert_eq!(reject_links(&root, &PLANS), Err(PlanError::Link));
    }

    #[test]
    fn links_inside_the_directory_are_refused() {
        let t = Tmp::new("inner");
        let root = make_repo(&t);
        let plans = root.join("app/travel_blog/plans");
        symlink(plans.join("a.json"), plans.join("b.json")).unwrap();
        assert_eq!(plan_files(&plans), Err(PlanError::Link));
    }

    #[test]
    fn only_plain_json_files_are_listed() {
        let t = Tmp::new("list");
        let root = make_repo(&t);
        let plans = root.join("app/travel_blog/plans");
        fs::write(plans.join(".hidden.json"), "{}").unwrap();
        fs::write(plans.join("note.txt"), "x").unwrap();
        fs::create_dir(plans.join("dir.json")).unwrap();
        fs::write(plans.join("b.json"), "{}").unwrap();
        let names: Vec<_> = plan_files(&plans)
            .unwrap()
            .into_iter()
            .map(|p| p.0)
            .collect();
        assert_eq!(names, ["a.json", "b.json"]);
    }

    #[test]
    fn a_name_that_is_not_utf8_stops_without_showing_it() {
        let t = Tmp::new("badname");
        let root = make_repo(&t);
        let plans = root.join("app/travel_blog/plans");
        let bad = std::ffi::OsStr::from_bytes(b"secret-\xff.json");
        fs::write(plans.join(bad), "{}").unwrap();
        let err = plan_files(&plans).unwrap_err();
        assert_eq!(err, PlanError::BadName);
        assert!(!err.message().contains("secret"));
    }

    #[test]
    fn unreadable_entries_stop_without_showing_their_names() {
        // root では権限を無視できるので試験にならない
        if running_as_root() {
            return;
        }
        let t = Tmp::new("unreadable");
        let root = make_repo(&t);
        let plans = root.join("app/travel_blog/plans");
        // 読めないファイル
        let locked = plans.join("secret-file.json");
        fs::write(&locked, "{}").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let err = plan_files(&plans).unwrap_err();
        assert_eq!(err, PlanError::Unreadable);
        assert!(!err.message().contains("secret"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        // 読めないディレクトリ（旅程のディレクトリ自体）
        fs::set_permissions(&plans, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(plan_files(&plans), Err(PlanError::Unreadable));
        // 途中の親が読めない（symlink_metadata が権限で失敗する）
        fs::set_permissions(root.join("app"), fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(reject_links(&root, &PLANS), Err(PlanError::Unreadable));
        fs::set_permissions(root.join("app"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&plans, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn running_as_root() -> bool {
        // /proc/self の所有者が 0 か（libc を足さずに実効 uid を調べる）
        use std::os::unix::fs::MetadataExt;
        fs::metadata("/proc/self")
            .map(|m| m.uid() == 0)
            .unwrap_or(false)
    }
}
