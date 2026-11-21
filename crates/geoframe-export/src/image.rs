//! 表示用 JPEG の取得、区画の検査、寸法・明るさ・シャープネス、作業領域への書き出し、manifest（design.md 4.1 の手順 5〜7）。
//!
//! 入口は [`prepare`]。ジョブの流れ（T5）は、これを次の順で呼ぶ。
//!
//! 1. 一覧 API で [`Selection`] を作る（[`crate::feed::FeedClient::select`]）。
//! 2. **候補が 0 枚なら [`prepare`] を呼ばずに**、`succeeded`・`count: 0`・`uploaded: false` で終える（Drive にも作業領域にも触れない）。
//! 3. [`Workspace::create`] で作業領域を作り、[`prepare`] を呼ぶ。[`prepare`] は、まず**作成待ちの写真が 1 枚でもあれば
//!    `derivative_pending`**（画像を 1 枚も取らずに失敗）、全部が除外されたら `nothing_to_export`（Drive の照会はこの後）。
//! 4. 成功したら、返った [`Prepared`] の一覧（相対パスとサイズ）で宛先の状態を調べ、公開する（T4）。
//! 5. 作業領域は [`Workspace`] を手放すと消える（成功・失敗にかかわらず）。
//!
//! 画像は 1 枚ずつ順に取る（写真サービスへの負荷を最小にする）。1 枚でも取得・デコードに失敗したら、そこで全体が失敗
//! （`fetch_failed`・`decode_failed`。一部が欠けた書き出しを作らない）。取った JPEG に許す区画以外があれば、その写真だけを
//! `has_metadata` として外し、書き出しは続ける（[`crate::jpeg`]）。
//!
//! 上限（[`Limits`]）: 1 枚 30MB（**読んだバイト数**で数える）、画素数 6,000 万（SOF のヘッダで、デコードの前に確かめる）、
//! 1 枚 60 秒（接続から本文を読み終えるまで）、1 ジョブの画像の合計 2GB、リダイレクトはたどらない、proxy は使わない。
//! 合計の上限は**読む前**に効く: 1 枚ごとに「1 枚の上限」と「合計の残り」の小さい方に超過の検出用の 1 バイトを足したところで
//! 読むのを止める（超えると分かった時点で止まり、上限を大きく超えて読まない）。区画の検査で外した写真の分も、読んだバイトとして
//! 合計に数える。
//! ジョブ全体の期限は呼び出し側が [`prepare`] の `deadline` で渡す（過ぎれば `timeout`。1 枚の期限もそこまでに縮める。
//! 期限は、取得の前、デコードと計測の後、最後に manifest を書いた後に確かめる）。
//! 1 ジョブの枚数の上限（2,000）は一覧 API の検査（[`crate::listing`]）が持つ。
//!
//! エラーには URL・本文・`ureq` のメッセージを入れない（固定の説明だけ）。

use std::io::Read;
use std::time::{Duration, Instant};

use crate::cancel::Cancel;
use crate::error::{ErrorCode, ExportError};
use crate::folder::FolderName;
use crate::jpeg::{self, InspectError, Inspection};
use crate::listing::{ExcludeReason, Exclusion, FileState, Selection};
use crate::manifest::{self, ManifestInput, PhotoEntry};
use crate::measure;
use crate::request::Destination;
use crate::settings::FeedBase;
use crate::staging::Workspace;

/// 1 枚の上限（30MB）。
pub const MAX_IMAGE_BYTES: u64 = 30 * 1024 * 1024;
/// 1 枚の画素数の上限（6,000 万）。
pub const MAX_PIXELS: u64 = 60_000_000;
/// 1 枚の取得の期限（60 秒）。
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(60);
/// 1 ジョブの画像の合計の上限（2GB）。
pub const MAX_JOB_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_image_bytes: u64,
    pub max_pixels: u64,
    pub image_timeout: Duration,
    pub max_job_bytes: u64,
}

impl Default for Limits {
    /// 本番の上限。
    fn default() -> Self {
        Self {
            max_image_bytes: MAX_IMAGE_BYTES,
            max_pixels: MAX_PIXELS,
            image_timeout: IMAGE_TIMEOUT,
            max_job_bytes: MAX_JOB_BYTES,
        }
    }
}

fn fetch_failed(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::FetchFailed, detail)
}

fn decode_failed(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::DecodeFailed, detail)
}

/// 表示用 JPEG の取得（`<feed-base>/<file>`。同期。リダイレクトも proxy も使わない）。
pub struct ImageClient {
    agent: ureq::Agent,
    base: String,
    limits: Limits,
}

impl ImageClient {
    /// 本番の上限。
    pub fn new(base: &FeedBase) -> Self {
        Self::with_limits(base, Limits::default())
    }

    /// 上限を替える（試験が 30MB や 60 秒を使わずに済むように）。本番は [`ImageClient::new`]。
    pub fn with_limits(base: &FeedBase, limits: Limits) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(limits.image_timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT.min(limits.image_timeout)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .user_agent(concat!("geoframe-export/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.as_str().to_owned(),
            limits,
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// 1 枚取る。`file` は検査済みの `derivatives/…jpg`。`timeout` はこの 1 枚の期限（既定の期限以下）。
    /// `job_remaining` は、このジョブの画像の合計の上限（2GB）までの残りのバイト数。1 枚の上限と残りの小さい方に、超過の
    /// 検出用の 1 バイトを足したところで読むのを止める（失敗までに余計に読むのは、その 1 バイトだけ）。
    /// 返すのは配信されたバイト列そのまま。圧縮は頼まない（`Accept-Encoding: identity`）。それでも `Content-Encoding: gzip` で
    /// 配られたときは、一覧 API の取得（[`crate::feed`]）と同じく展開したバイト列を受け取り、上限は**展開後**のバイト数に掛ける
    /// （手元のバイト列を書くので、`file_size` はそのバイト列の大きさと一致する）。
    pub fn fetch(
        &self,
        file: &str,
        timeout: Duration,
        job_remaining: u64,
    ) -> Result<Vec<u8>, ExportError> {
        let url = format!("{}/{}", self.base, file);
        let mut response = self
            .agent
            .get(&url)
            .config()
            .timeout_global(Some(timeout.min(self.limits.image_timeout)))
            .build()
            .header("Accept", "image/jpeg")
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|_| fetch_failed("写真サービスから画像を取れません"))?;
        if response.status().as_u16() != 200 {
            // 3xx もここに来る（たどらない）。本文は読まない
            return Err(fetch_failed("写真サービスが 200 以外を返しました"));
        }
        let limit = self.limits.max_image_bytes.min(job_remaining);
        let cap = limit.saturating_add(1);
        // ureq 自身の上限は（gzip なら）展開の前のバイト数に効くので、ジョブの残りとは切り離し、1 枚の上限から
        // ゆとりを持たせた値にする（圧縮で少し大きくなった正常な応答を断らない）。展開の後のバイト数は take で数える
        let wire_cap = self
            .limits
            .max_image_bytes
            .saturating_mul(2)
            .saturating_add(64 * 1024);
        let reader = response.body_mut().with_config().limit(wire_cap).reader();
        let mut out = Vec::new();
        reader
            .take(cap)
            .read_to_end(&mut out)
            .map_err(|_| fetch_failed("画像の本文を読み取れませんでした"))?;
        if out.len() as u64 > limit {
            return Err(if self.limits.max_image_bytes <= job_remaining {
                fetch_failed("画像が 1 枚の上限（30MB）を超えています")
            } else {
                fetch_failed("画像の合計が 1 ジョブの上限（2GB）を超えました")
            });
        }
        Ok(out)
    }
}

/// 公開する 1 つのファイル（作業領域の中の相対パスと、バイト数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishFile {
    /// `/` 区切りの相対パス。単日は `HHMM00_<id>.jpg`、期間は `YYYY-MM-DD/HHMM00_<id>.jpg`、最後に `manifest.json`。
    pub path: String,
    pub size: u64,
}

/// [`prepare`] の結果。作業領域には、`files` の各ファイルが書いてある。
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub folder: FolderName,
    /// 写真（`time`・`id` の昇順、manifest の `photos` と同じ順）に続けて `manifest.json`。
    pub files: Vec<PublishFile>,
    /// 候補の数（結果の `catalog_count`）。
    pub catalog_count: usize,
    /// 書き出した写真の数（結果の `count` = `catalog_count` − `excluded` の件数）。
    pub count: usize,
    /// 仕様上の除外（`not_derivative`・`has_metadata`）。候補の順。
    pub excluded: Vec<Exclusion>,
    /// 写真のバイト数の合計（結果の `bytes` に使う）。
    pub photo_bytes: u64,
    pub manifest_bytes: u64,
    pub undated_in_catalog: u64,
    pub invalid_time_in_catalog: u64,
}

/// [`prepare`] の失敗。失敗の理由に加えて、**そこまでに分かっていた数**を持つ（ジョブの結果に載せる。`nothing_to_export`
/// のときは、候補の数と除外の理由が全部入っている）。
#[derive(Debug, Clone, PartialEq)]
pub struct PrepareError {
    pub error: ExportError,
    /// 候補の数（結果の `catalog_count`）。
    pub catalog_count: usize,
    /// 失敗までに外した仕様上の除外（`not_derivative`・`has_metadata`）。候補の順。`nothing_to_export` では全部。
    /// ほかの失敗では、失敗した写真より前の分だけ。
    pub excluded: Vec<Exclusion>,
    pub undated_in_catalog: u64,
    pub invalid_time_in_catalog: u64,
}

impl PrepareError {
    pub fn code(&self) -> ErrorCode {
        self.error.code
    }
}

impl From<PrepareError> for ExportError {
    fn from(e: PrepareError) -> Self {
        e.error
    }
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for PrepareError {}

/// 全体の期限と、キャンセルの印（1 枚ごとに確かめる）。
struct Gate<'a> {
    deadline: Option<Instant>,
    now: &'a dyn Fn() -> Instant,
    cancel: Option<&'a Cancel>,
}

impl Gate<'_> {
    /// キャンセルされていれば `internal`、期限を過ぎていれば `timeout`。残りの時間を返す。
    fn check(&self) -> Result<Duration, ExportError> {
        if self.cancel.is_some_and(Cancel::is_cancelled) {
            return Err(ExportError::new(
                ErrorCode::Internal,
                "キャンセルされました",
            ));
        }
        match self.deadline {
            None => Ok(Duration::MAX),
            Some(d) => {
                let left = d.saturating_duration_since((self.now)());
                if left.is_zero() {
                    Err(ExportError::from(ErrorCode::Timeout))
                } else {
                    Ok(left)
                }
            }
        }
    }

    fn past_deadline(&self) -> bool {
        self.deadline.is_some_and(|d| (self.now)() >= d)
    }
}

/// 選んだ候補の表示用 JPEG を取って検査し、作業領域に書き、manifest を作る（上の流れの 3）。
///
/// - 候補が 0 枚なら `internal`（呼ばない。上の流れの 2）。
/// - 作成待ち（`file: null`）の写真が 1 枚でもあれば、何も取らずに `derivative_pending`。
/// - 1 枚の取得・デコードの失敗、上限（1 枚・画素数・合計）の超過で全体が失敗（`fetch_failed`・`decode_failed`）。
///   区画の検査に通らない写真（`has_metadata`）は外して続ける。
/// - 書き出す写真が 0 枚になったら `nothing_to_export`（[`PrepareError`] に、候補の数と除外の理由を持たせる）。
/// - `deadline` を過ぎたら `timeout`（取得の前、デコードと計測の後、manifest を書いた後に確かめる）。
///
/// 失敗しても作業領域の中身は残る（呼び出し側が [`Workspace`] を手放して消す）。
pub fn prepare(
    client: &ImageClient,
    selection: &Selection,
    destination: Destination,
    workspace: &Workspace,
    deadline: Option<Instant>,
) -> Result<Prepared, PrepareError> {
    prepare_with_clock(
        client,
        selection,
        destination,
        workspace,
        deadline,
        &Instant::now,
    )
}

/// [`prepare`] の、現在時刻の取り方を差し替えられる形（期限の確かめを、時間に頼らずに試験するため。本番は [`prepare`]）。
#[doc(hidden)]
pub fn prepare_with_clock(
    client: &ImageClient,
    selection: &Selection,
    destination: Destination,
    workspace: &Workspace,
    deadline: Option<Instant>,
    now: &dyn Fn() -> Instant,
) -> Result<Prepared, PrepareError> {
    prepare_full(
        client,
        selection,
        destination,
        workspace,
        deadline,
        now,
        None,
    )
}

/// [`prepare`] に、キャンセルの印（[`Cancel`]。SIGINT・SIGTERM）を掛けた形。印が立つと、次の確かめ（1 枚ごと）で `internal` で
/// 止まる（呼び出し側は、印を見て、この失敗を記録に確定しない。Drive には触れていない）。
pub fn prepare_cancellable(
    client: &ImageClient,
    selection: &Selection,
    destination: Destination,
    workspace: &Workspace,
    deadline: Option<Instant>,
    cancel: &Cancel,
) -> Result<Prepared, PrepareError> {
    prepare_full(
        client,
        selection,
        destination,
        workspace,
        deadline,
        &Instant::now,
        Some(cancel),
    )
}

fn prepare_full(
    client: &ImageClient,
    selection: &Selection,
    destination: Destination,
    workspace: &Workspace,
    deadline: Option<Instant>,
    now: &dyn Fn() -> Instant,
    cancel: Option<&Cancel>,
) -> Result<Prepared, PrepareError> {
    let mut excluded = Vec::new();
    let gate = Gate {
        deadline,
        now,
        cancel,
    };
    prepare_inner(
        client,
        selection,
        destination,
        workspace,
        &gate,
        &mut excluded,
    )
    .map_err(|error| PrepareError {
        error,
        catalog_count: selection.candidate_count(),
        excluded,
        undated_in_catalog: selection.undated,
        invalid_time_in_catalog: selection.invalid_time,
    })
}

fn prepare_inner(
    client: &ImageClient,
    selection: &Selection,
    destination: Destination,
    workspace: &Workspace,
    gate: &Gate<'_>,
    excluded: &mut Vec<Exclusion>,
) -> Result<Prepared, ExportError> {
    if selection.candidates.is_empty() {
        return Err(ExportError::new(
            ErrorCode::Internal,
            "候補が 0 枚のときは書き出しを始めません",
        ));
    }
    if selection.pending_count() > 0 {
        return Err(ExportError::from(ErrorCode::DerivativePending));
    }
    let period = &selection.period;
    let folder = FolderName::for_period(period)?;
    let limits = client.limits();

    let mut photos: Vec<PhotoEntry> = Vec::new();
    let mut files: Vec<PublishFile> = Vec::new();
    let mut photo_bytes = 0u64;
    // 取った画像の合計（外した写真も数える。2GB の上限は、取ったバイト数に掛ける）
    let mut fetched_bytes = 0u64;

    for candidate in &selection.candidates {
        let path = match &candidate.file {
            FileState::Ready(path) => path,
            FileState::NotDerivative => {
                excluded.push(Exclusion {
                    id: candidate.id,
                    reason: ExcludeReason::NotDerivative,
                });
                continue;
            }
            // 上で止めている。ここに来たら食い違い
            FileState::Pending => return Err(ExportError::from(ErrorCode::DerivativePending)),
        };
        let left = gate.check()?;
        let remaining = limits.max_job_bytes.saturating_sub(fetched_bytes);
        let bytes = client.fetch(path, left, remaining).map_err(|e| {
            // 全体の期限で 1 枚の期限が縮んでいたときの失敗は、期限切れとして返す
            if gate.past_deadline() {
                ExportError::from(ErrorCode::Timeout)
            } else {
                e
            }
        })?;
        // 読む前に残りで止めている（超えていれば fetch が失敗している）。ここは取りこぼしの保険
        fetched_bytes = fetched_bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= limits.max_job_bytes)
            .ok_or(fetch_failed(
                "画像の合計が 1 ジョブの上限（2GB）を超えました",
            ))?;

        let header = match jpeg::inspect(&bytes, limits.max_pixels) {
            Ok(Inspection::Clean(header)) => header,
            Ok(Inspection::HasMetadata) => {
                // 外す写真は、書かず・デコードもしない
                excluded.push(Exclusion {
                    id: candidate.id,
                    reason: ExcludeReason::HasMetadata,
                });
                continue;
            }
            Err(InspectError::Malformed(detail)) => return Err(decode_failed(detail)),
            Err(InspectError::TooManyPixels) => {
                return Err(fetch_failed("画像の画素数が上限（6,000 万）を超えています"));
            }
        };
        let gray = jpeg::decode_gray(&bytes, header)
            .map_err(|_| decode_failed("画像をデコードできません"))?;
        let measures = measure::measure(&gray.data, gray.width, gray.height);
        drop(gray);
        // デコードと計測は長くかかりうる。そのあいだに期限が過ぎていれば、この写真を書かずに止める
        gate.check()?;

        let filename = manifest::relative_path(period, candidate.time, candidate.id);
        workspace.write_new(&filename, &bytes)?;
        photo_bytes += bytes.len() as u64;
        files.push(PublishFile {
            path: filename.clone(),
            size: bytes.len() as u64,
        });
        photos.push(PhotoEntry {
            id: candidate.id,
            filename,
            title: candidate.title.clone(),
            time: candidate.time,
            lat: candidate.lat,
            lon: candidate.lon,
            width: header.width,
            height: header.height,
            file_size: bytes.len() as u64,
            mean_brightness: measures.mean_brightness,
            sharpness_laplacian_var: measures.sharpness_laplacian_var,
            inferred: candidate.inferred,
            elev: candidate.elev,
        });
    }

    // 早く終わる結果（全部除外）も含めて、結果を返す前に全体の期限を確かめる
    gate.check()?;
    // 仕様上の除外のあとで 0 枚になったら失敗（宛先の状態に関係なく同じ結果。Drive の照会はこの後）
    if photos.is_empty() {
        return Err(ExportError::from(ErrorCode::NothingToExport));
    }

    let manifest = manifest::build(&ManifestInput {
        destination,
        folder: &folder,
        period,
        catalog_count: selection.candidate_count(),
        excluded,
        undated_in_catalog: selection.undated,
        invalid_time_in_catalog: selection.invalid_time,
        photos: &photos,
    });
    let manifest_bytes = manifest::to_bytes(&manifest);
    workspace.write_new("manifest.json", &manifest_bytes)?;
    files.push(PublishFile {
        path: "manifest.json".to_owned(),
        size: manifest_bytes.len() as u64,
    });
    // 期限を過ぎてから「成功」を返さない（呼び出し側は、この後に宛先を照会して送る）
    gate.check()?;

    Ok(Prepared {
        folder,
        count: photos.len(),
        catalog_count: selection.candidate_count(),
        files,
        excluded: std::mem::take(excluded),
        photo_bytes,
        manifest_bytes: manifest_bytes.len() as u64,
        undated_in_catalog: selection.undated,
        invalid_time_in_catalog: selection.invalid_time,
    })
}
