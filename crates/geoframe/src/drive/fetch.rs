//! 同期サイクルの段 `fetch_derivatives`（現行 `drive.py:fetch_derivatives`）。`display` が未設定の Drive 写真の
//! 原本をダウンロードし、derivative を作って `display`・`fetched_at` を書く。
//!
//! # 規則（現行と同じ）
//!
//! - 対象は `source='drive' AND display IS NULL`。**位置の有無では絞らない**（管理画面の「位置なし」タブで
//!   写真を見て位置を指定するため。inventory.md §6.4）。`ORDER BY taken_at, id` で、1 サイクル
//!   `GEOFRAME_FETCH_PER_CYCLE` 枚まで。
//! - 原本は `DATA_DIR/tmp/.dl_<drive_id>` に落とし（`derivatives/` の外。静的配信の対象外）、
//!   [`imaging::store_derivative`] で `derivatives/<drive_id>[.r<回転>].jpg` を作る。
//! - **成功・失敗・パニックにかかわらず原本を消す**（EXIF・GPS 付きの原本を残さない。[`Original`] の `Drop`）。
//!
//! # 現行との違い（server.md §0）
//!
//! - **一時ファイルは `create_new`＋`O_NOFOLLOW`（権限 0600）で作る**。既にある名前（リンクを含む）は
//!   辿らず・消さず、その写真の失敗にする（起動時の掃除が消す）。現行は `open(..., "wb")` で上書きする。
//! - **ダウンロードは 512 MiB まで**（[`imaging::MAX_INPUT_BYTES`]。まるごとメモリに読む derivative 生成の上限と
//!   同じ）、1 枚 15 分まで。
//! - **失敗が続く写真は飛ばす**（受入条件②。[`crate::backoff`]）。現行の `LIMIT n` の SQL だと、恒久的に
//!   失敗する写真が先頭の枠を埋め続け、後続が取得されない。失敗した写真は、30 分・1 時間・2 時間…（上限 12 時間）
//!   のあいだ飛ばして、後続の写真を枠に入れる（プロセスの中だけの記録。スキーマは変えない）。Drive で原本が
//!   更新されれば（`md5` が変わる）すぐ再び試す。
//! - 通信や Drive 側の不調（接続できない・名前解決・TLS・応答の待ちの時間切れ・流量制限・429・5xx・認証の
//!   失敗）は、写真のせいではないので、段を中断して失敗として返す（残りの写真を試さず、失敗の記録も付けない）。
//!   **本文の受信が 1 枚 15 分を超えた**ことだけは、その写真の失敗（`fail_slow_download`）として数える。
//! - ログ・件数にはファイル名を出さない（写真の `id` と、失敗の種類だけ。現行は名前を出す）。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::params;

use super::index::{AUTH_FAILED, NO_KEY, stage_error};
use super::{DriveError, DriveShared, Session};
use crate::backoff::FailureMemo;
use crate::datadir::DataDir;
use crate::imaging::{self, Format, IccOutcome, ImageError};
use crate::sync::{Counts, Stage, StageCtx, StageError, StageOutcome};

/// 失敗の記録のキー: `(drive_id, md5)`。Drive で原本が更新されたら（`md5` が変わる）別のキーになり、
/// すぐ試される。
type MemoKey = (String, Option<String>);

/// 同期サイクルの段 `fetch_derivatives`。
pub struct FetchDerivatives {
    shared: Arc<DriveShared>,
    failures: Mutex<FailureMemo<MemoKey>>,
}

impl FetchDerivatives {
    pub fn new(shared: Arc<DriveShared>) -> Self {
        Self {
            shared,
            failures: Mutex::new(FailureMemo::default()),
        }
    }

    #[cfg(test)]
    pub(super) fn with_memo(shared: Arc<DriveShared>, memo: FailureMemo<MemoKey>) -> Self {
        Self {
            shared,
            failures: Mutex::new(memo),
        }
    }
}

/// 取得待ちの 1 行。
struct Candidate {
    id: i64,
    /// NULL の行（通常は無い）は取得できない。
    drive_id: Option<String>,
    rotation: Option<i64>,
    md5: Option<String>,
}

impl Candidate {
    fn memo_key(&self) -> MemoKey {
        (
            self.drive_id
                .clone()
                .unwrap_or_else(|| format!("\0row{}", self.id)),
            self.md5.clone(),
        )
    }
}

/// 原本の一時ファイル。**落とされたら（成功・失敗・パニックのどれでも）消す**。自分が新規作成に成功した
/// ものだけを持つ（既にあったファイル・リンクは消さない）。
pub(super) struct Original {
    path: PathBuf,
}

impl Drop for Original {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// 原本の置き場に新規作成して開く（`O_CREAT|O_EXCL`＋`O_NOFOLLOW`、権限 0600）。同じ名前のファイル・
/// リンク（切れたものも）が既にあれば `AlreadyExists` で、リンクの先を書き換えない。
pub(super) fn create_original(path: &Path) -> io::Result<(fs::File, Original)> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    Ok((
        file,
        Original {
            path: path.to_path_buf(),
        },
    ))
}

/// 1 枚の取得の結果。
struct Fetched {
    format: Format,
    icc_dropped: bool,
}

/// 1 枚の失敗。
enum Fail {
    /// その写真の失敗（種類は固定の語。文面はログに出してよいもの）。
    Photo { kind: &'static str, detail: String },
    /// 通信・Drive 側の不調、DB の失敗など。段を中断する。
    Abort(StageError),
    /// 停止の合図。
    Stopped,
}

fn photo_fail(kind: &'static str, detail: impl ToString) -> Fail {
    Fail::Photo {
        kind,
        detail: detail.to_string(),
    }
}

fn drive_fail(e: DriveError) -> Fail {
    if e == DriveError::Stopped {
        return Fail::Stopped;
    }
    if e.is_outage() {
        return Fail::Abort(stage_error(e));
    }
    let kind = match e {
        DriveError::TooLarge => "fail_too_large",
        DriveError::BodyTimeout => "fail_slow_download",
        DriveError::Io(_) => "fail_io",
        _ => "fail_download",
    };
    photo_fail(kind, e)
}

/// 画像処理の失敗の種類（固定の語）。件数の名前に使う。
fn image_fail_kind(e: &ImageError) -> &'static str {
    match e {
        ImageError::NotAnImage => "fail_not_image",
        ImageError::BigTiff => "fail_bigtiff",
        ImageError::FileTooLarge => "fail_too_large",
        ImageError::TooManyPixels { .. } => "fail_too_many_pixels",
        ImageError::Decode { format, .. } => match *format {
            "jpeg" => "fail_decode_jpeg",
            "png" => "fail_decode_png",
            "tiff" => "fail_decode_tiff",
            _ => "fail_decode_other",
        },
        ImageError::Heic(_) => "fail_decode_heic",
        ImageError::Io(_) => "fail_io",
        ImageError::Encode => "fail_encode",
        ImageError::BadRotation | ImageError::Path(_) => "fail_other",
    }
}

fn ok_kind(format: Format) -> &'static str {
    match format {
        Format::Jpeg => "ok_jpeg",
        Format::Png => "ok_png",
        Format::Tiff => "ok_tiff",
        Format::Heic => "ok_heic",
    }
}

const SET_DISPLAY: &str = "UPDATE photos SET display=?1, fetched_at=?2 WHERE id=?3";

impl FetchDerivatives {
    fn candidates(ctx: &StageCtx<'_>) -> Result<Vec<Candidate>, StageError> {
        let mut stmt = ctx.read.prepare(
            "SELECT id, drive_id, rotation, md5 FROM photos
             WHERE source='drive' AND display IS NULL
             ORDER BY taken_at, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Candidate {
                id: row.get(0)?,
                drive_id: row.get::<_, Option<String>>(1).unwrap_or(None),
                rotation: row.get::<_, Option<i64>>(2).unwrap_or(None),
                md5: row.get::<_, Option<String>>(3).unwrap_or(None),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 1 枚。原本を落とし、derivative を作り、`display`・`fetched_at` を書く。原本は必ず消える。
    fn fetch_one(
        session: &Session<'_>,
        data: &DataDir,
        writer: &crate::writer::Writer,
        candidate: &Candidate,
    ) -> Result<Fetched, Fail> {
        let Some(drive_id) = candidate.drive_id.as_deref() else {
            return Err(photo_fail("fail_other", "drive_id がありません"));
        };
        let rotation = i32::try_from(candidate.rotation.unwrap_or(0))
            .map_err(|_| photo_fail("fail_other", ImageError::BadRotation))?;
        // 名前の検査（ID が区切りを含まないこと）。保存先の検査は `store_derivative` が改めて行う
        let name = imaging::derivative_name(drive_id, rotation)
            .map_err(|e| photo_fail("fail_other", ImageError::Path(e)))?;
        let tmp = data
            .download_tmp(drive_id)
            .map_err(|e| photo_fail("fail_other", ImageError::Path(e)))?;
        let (mut file, _original) =
            create_original(&tmp).map_err(|e| photo_fail("fail_io", ImageError::Io(e.kind())))?;
        session
            .download(drive_id, &mut file, imaging::MAX_INPUT_BYTES)
            .map_err(drive_fail)?;
        drop(file);
        let stored = imaging::store_derivative(data, &tmp, &name, rotation)
            .map_err(|e| photo_fail(image_fail_kind(&e), e))?;
        let now = crate::sysinfo::local_isoformat();
        let display = stored.display.clone();
        let id = candidate.id;
        writer
            .call(move |conn| conn.execute(SET_DISPLAY, params![display, now, id]))
            .map_err(|e| Fail::Abort(StageError::from(e)))?;
        Ok(Fetched {
            format: stored.info.format,
            icc_dropped: stored.info.icc == IccOutcome::Dropped,
        })
        // `_original` がここで落とされ、原本が消える（`?` で抜けたときも、パニックでも同じ）
    }
}

impl Stage for FetchDerivatives {
    fn name(&self) -> &'static str {
        "fetch_derivatives"
    }

    fn run(&self, ctx: &StageCtx<'_>) -> Result<StageOutcome, StageError> {
        // 現行は `svc`（鍵があって認証できたとき）があるときだけ動く
        if !ctx.config.drive_key.exists() {
            return Ok(StageOutcome::Skipped(NO_KEY));
        }
        if self.shared.auth_failed() {
            return Ok(StageOutcome::Skipped(AUTH_FAILED));
        }
        let limit = ctx.config.fetch_per_cycle as usize;
        let now = Instant::now();
        let mut pending = 0u64;
        let mut waiting = 0u64;
        let mut selected: Vec<Candidate> = Vec::new();
        {
            let memo = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            for candidate in Self::candidates(ctx)? {
                pending += 1;
                if selected.len() >= limit {
                    continue;
                }
                if memo.is_waiting(&candidate.memo_key(), now) {
                    waiting += 1;
                    continue;
                }
                selected.push(candidate);
            }
        }
        let mut head = vec![("pending", pending), ("skipped_backoff", waiting)];

        if ctx.is_dry_run() {
            // ダウンロードも derivative の生成もしない。取得するはずの写真を数え、成功したものとして写しの
            // `display` を埋める（後続の段・最後の `写真:` の行に見せる）
            let mut planned = 0u64;
            let mut unusable = 0u64;
            for candidate in &selected {
                let name = candidate.drive_id.as_deref().and_then(|id| {
                    let rotation = i32::try_from(candidate.rotation.unwrap_or(0)).ok()?;
                    imaging::derivative_name(id, rotation).ok()
                });
                let Some(name) = name else {
                    unusable += 1;
                    continue;
                };
                ctx.read
                    .execute(
                        SET_DISPLAY,
                        params![
                            format!("derivatives/{name}"),
                            crate::sysinfo::local_isoformat(),
                            candidate.id
                        ],
                    )
                    .map_err(|e| StageError::text(format!("dry-run の写しに書けません: {e}")))?;
                planned += 1;
            }
            head.push(("planned", planned));
            head.push(("unusable", unusable));
            return Ok(StageOutcome::Done(Counts(head)));
        }

        if selected.is_empty() {
            return Ok(StageOutcome::Done(Counts(head)));
        }
        let data = DataDir::open(&ctx.config.data_dir)?;
        let writer = ctx.writer()?;
        let session = Session::new(&self.shared, ctx.http, ctx.stop, &ctx.config.drive_key);
        let mut tally: BTreeMap<&'static str, u64> = BTreeMap::new();
        let (mut attempted, mut fetched, mut icc_dropped) = (0u64, 0u64, 0u64);
        let mut aborted: Option<StageError> = None;
        for candidate in &selected {
            if ctx.stop.is_stopped() {
                break;
            }
            attempted += 1;
            let outcome = Self::fetch_one(&session, &data, writer, candidate);
            let mut memo = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            match outcome {
                Ok(done) => {
                    memo.record_success(&candidate.memo_key());
                    fetched += 1;
                    icc_dropped += u64::from(done.icc_dropped);
                    *tally.entry(ok_kind(done.format)).or_default() += 1;
                }
                Err(Fail::Photo { kind, detail }) => {
                    memo.record_failure(candidate.memo_key(), Instant::now());
                    *tally.entry(kind).or_default() += 1;
                    // 文面はパス・ファイル名を含まない（`DriveError`・`ImageError` の Display）
                    tracing::warn!(
                        "[fetch] 取得に失敗しました（id={}）: {detail}",
                        candidate.id
                    );
                }
                Err(Fail::Abort(e)) => {
                    aborted = Some(e);
                    break;
                }
                Err(Fail::Stopped) => break,
            }
        }
        head.push(("attempted", attempted));
        head.push(("fetched", fetched));
        head.push(("icc_dropped", icc_dropped));
        head.extend(tally);
        let counts = Counts(head);
        match aborted {
            Some(e) => Err(StageError::text(format!("{e}（ここまでの件数: {counts}）"))),
            None => Ok(StageOutcome::Done(counts)),
        }
    }
}
