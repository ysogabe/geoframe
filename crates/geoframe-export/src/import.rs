//! `geoframe-export import-credentials --rclone-config <path> --remote <name> --out <path>`
//! （docs/note-photo-export/drive-api.md 2 章）。
//!
//! 今の rclone の設定（[`crate::rclone_conf`] で読む）から、専用の `client_id`・`client_secret` と、`token` の `refresh_token` を
//! 取り出し、3 つのキーだけの JSON（[`crate::drive::auth::Credentials`] が読む形）として `--out` に書く。
//!
//! - 書き方は 0600・`O_CREAT | O_EXCL | O_NOFOLLOW`（既にあれば断る。リンクはたどらない）。書いたあと、同じ検査
//!   （[`Credentials::load`]）で読み直し、読めなければ消す。
//! - **値は標準出力・標準エラー・ログに出さない。** 出すのは、書いたパスと「書いた」ことだけ。
//! - root が sudo で 1 回だけ動かす前提（同意のやり直しはしない）。root のときは、設定ファイルの持ち主を問わない
//!   （サービスのユーザーの持ち物なので。[`crate::rclone_conf::read_credentials`]）。起動時の検査は通らない（Drive に触れない）。
//!   別の機体で `rclone authorize` して得た設定を取り込むときにも使う（再認可）。

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde_json::json;

use crate::app::EXIT_USAGE;
use crate::drive::auth::Credentials;
use crate::rclone_conf;

pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILED: u8 = 1;

pub fn run(
    rclone_config: &Path,
    remote: &str,
    out_path: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let creds = match rclone_conf::read_credentials(rclone_config, remote) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "geoframe-export: rclone の設定を読めません: {e}");
            return EXIT_USAGE;
        }
    };
    let body = serde_json::to_vec_pretty(&json!({
        "client_id": creds.client_id,
        "client_secret": creds.client_secret,
        "refresh_token": creds.refresh_token,
    }))
    .unwrap_or_default();
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(out_path);
    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            // 既にある（AlreadyExists）・親が無い・権限が無い、など。OS のメッセージにパスは入るが、値は入らない
            let _ = writeln!(
                err,
                "geoframe-export: 出力先を作れません（既にあるファイルは上書きしません）: {}",
                e.kind()
            );
            return EXIT_FAILED;
        }
    };
    let written = file
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(&body))
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = std::fs::remove_file(out_path);
        let _ = writeln!(
            err,
            "geoframe-export: 出力先に書けませんでした（消しました）: {}",
            e.kind()
        );
        return EXIT_FAILED;
    }
    // 読み直す（`--drive-credentials` と同じ検査）。読めなければ、使えないファイルを残さない
    if Credentials::load(out_path).is_err() {
        let _ = std::fs::remove_file(out_path);
        let _ = writeln!(
            err,
            "geoframe-export: 書いたファイルを資格情報として読み直せませんでした（消しました）"
        );
        return EXIT_FAILED;
    }
    let _ = writeln!(out, "資格情報のファイルを書きました: {}", out_path.display());
    EXIT_OK
}
