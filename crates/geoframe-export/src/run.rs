//! `run` コマンド（design.md 6.2）: 同じ実行枠で、フォアグラウンドでジョブを流し、`GET /v1/exports/{job_id}` と同じ形の JSON
//! （`job_id`・`status`・`request`・`started_at`・`finished_at`・`result`・`error`）を標準出力に出す。
//!
//! 終わるときの終了コード:
//!
//! | 終了コード | 意味 |
//! |---|---|
//! | 0 | 成功（0 枚を含む）、`--dry-run` の結果を出せた |
//! | 1 | 失敗・`blocked`・`interrupted`、`blocked` の記録があって断った、`dry_run` の失敗 |
//! | 2 | 引数・設定の誤り（`geoframe-export` の入口で。状態ディレクトリを用意できないときも） |
//! | 3 | `conflict`（宛先が既にある・管理していない・中身が変わっている） |
//! | 4 | 実行中（実行枠が取れない） |
//!
//! **ジョブにならなかった断り**（実行中・`blocked` の記録・実行枠の失敗）の標準出力は、`job_id: null`・`status: "failed"`・
//! `result: null`・`error: {code, message}`（コードは `busy`・`blocked`・`internal`）の形にする。`status` は 6 値のままで
//! 増やさない。`blocked` を断る理由の説明（`recover --resolve` の案内）は標準エラーへ出す。
//! `dry_run` は JSON（6.1 の 200 の形）を出して 0。

use std::io::Write;

use serde_json::{Value, json};

use crate::app::EXIT_USAGE;
use crate::error::{ErrorCode, ExportError};
use crate::flow::Tuning;
use crate::jobs::{Status, error_json};
use crate::request::ExportRequest;
use crate::service::{DryRunError, Service, SubmitError};
use crate::settings::Settings;

pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_CONFLICT: u8 = 3;
pub const EXIT_BUSY: u8 = 4;

pub fn run(
    settings: &Settings,
    request: &ExportRequest,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    run_with(settings, request, Tuning::default(), out, err)
}

/// [`run`] の、調整値（待ちと期限）を替えられる形（試験用）。
pub fn run_with(
    settings: &Settings,
    request: &ExportRequest,
    tuning: Tuning,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let service = match Service::with_tuning(settings.clone(), tuning) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(
                err,
                "geoframe-export: 状態ディレクトリを用意できません: {e}"
            );
            return EXIT_USAGE;
        }
    };
    if request.dry_run {
        return match service.dry_run(request) {
            Ok(body) => {
                print_json(out, &body);
                EXIT_OK
            }
            Err(DryRunError::Busy) => refuse(out, ErrorCode::Busy.into(), EXIT_BUSY),
            Err(DryRunError::Blocked(b)) => {
                let _ = writeln!(err, "geoframe-export: {}", b.text());
                refuse(out, ErrorCode::Blocked.into(), EXIT_FAILED)
            }
            Err(DryRunError::Failed(e)) => refuse(out, e, EXIT_FAILED),
        };
    }
    match service.run_foreground(request) {
        Ok(record) => {
            print_json(out, &record.to_api_json());
            match record.status {
                Status::Succeeded => EXIT_OK,
                Status::Conflict => EXIT_CONFLICT,
                _ => EXIT_FAILED,
            }
        }
        Err(SubmitError::Busy) => refuse(out, ErrorCode::Busy.into(), EXIT_BUSY),
        Err(SubmitError::Blocked(b)) => {
            let _ = writeln!(err, "geoframe-export: {}", b.text());
            refuse(out, ErrorCode::Blocked.into(), EXIT_FAILED)
        }
        Err(e) => {
            let code = e.code();
            refuse(out, code.into(), EXIT_FAILED)
        }
    }
}

/// ジョブにならなかった断りの JSON と終了コード。
fn refuse(out: &mut dyn Write, e: ExportError, exit: u8) -> u8 {
    let body: Value = json!({
        "job_id": null,
        "status": "failed",
        "result": null,
        "error": error_json(Some(e.code)),
    });
    print_json(out, &body);
    exit
}

fn print_json(out: &mut dyn Write, v: &Value) {
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_owned())
    );
}
