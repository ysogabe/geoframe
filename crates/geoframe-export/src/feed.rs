//! 写真サービスの一覧 API の取得（design.md 4.1 の手順 2、6.4、8 章）。
//!
//! `GET <feed-base>/api/admin/export/photos?from=YYYY-MM-DD&to=YYYY-MM-DD`
//!
//! - タイムアウトは 30 秒（接続から本文を読み終えるまで）。
//! - 応答の本文は 8MB まで。**読んだバイト数**で数える（`Content-Length` は信じない）。超えたら `feed_invalid`
//!   （200 を受け取ったうえで、中身が使えない応答として扱う）。
//! - リダイレクトはたどらない（`max_redirects(0)`。3xx は 200 以外として `feed_unavailable`）。環境変数の proxy も使わない。
//! - 接続できない・タイムアウト・200 以外・本文の読み取りの失敗は `feed_unavailable`。JSON として読めない・
//!   値の検査に通らないものは `feed_invalid`（[`crate::listing`]）。
//! - 圧縮は頼まない（`Accept-Encoding: identity`）。ただし `Content-Encoding: gzip` の応答は展開して読む（ureq の `gzip` feature を
//!   この crate の Cargo.toml で明示している。ワークスペース全体のビルドでも、この crate だけのビルドでも同じ動き）。
//!   上限は**展開後**のバイト数に掛け、壊れた圧縮は `feed_invalid`、通信の失敗（期限・切断）は `feed_unavailable`。
//! - エラーには URL・本文・`ureq` のメッセージを入れない（固定の説明だけ）。

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::date::Period;
use crate::error::{ErrorCode, ExportError};
use crate::listing::{self, Selection};
use crate::settings::FeedBase;

/// 一覧 API の呼び出し全体のタイムアウト。
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// 一覧 API の応答の上限（8MB）。
pub const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
/// 接続だけのタイムアウト（ループバックの接続は一瞬で済む）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct FeedClient {
    agent: ureq::Agent,
    base: String,
    max_body: u64,
}

fn unavailable(detail: &'static str) -> ExportError {
    ExportError::new(ErrorCode::FeedUnavailable, detail)
}

impl FeedClient {
    /// 本番の上限（30 秒・8MB）。
    pub fn new(base: &FeedBase) -> Self {
        Self::with_limits(base, TIMEOUT, MAX_BODY_BYTES)
    }

    /// 期限と上限を替える（試験が 30 秒待たずに済むように）。本番は [`FeedClient::new`]。
    pub fn with_limits(base: &FeedBase, timeout: Duration, max_body: u64) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
            .max_redirects(0)
            .proxy(None)
            .http_status_as_error(false)
            .user_agent(concat!("geoframe-export/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.as_str().to_owned(),
            max_body,
        }
    }

    /// 一覧 API を呼び、応答の JSON を返す（値の検査はしない）。
    pub fn fetch(&self, period: &Period) -> Result<Value, ExportError> {
        // from・to は検査済みの日付なので、そのまま URL に入れてよい
        let url = format!(
            "{}/api/admin/export/photos?from={}&to={}",
            self.base,
            period.from(),
            period.to()
        );
        let mut response = self
            .agent
            .get(&url)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|_| unavailable("一覧 API に接続できません"))?;
        if response.status().as_u16() != 200 {
            // 3xx もここに来る（たどらない）。本文は読まない
            return Err(unavailable("一覧 API が 200 以外を返しました"));
        }
        let bytes = self.read_body(response.body_mut())?;
        serde_json::from_slice(&bytes).map_err(|_| {
            ExportError::new(
                ErrorCode::FeedInvalid,
                "一覧 API の応答が JSON ではありません",
            )
        })
    }

    /// 本文を読んだバイト数で数えて、`max_body` を超えたところで止める。ちょうど `max_body` は通す。
    fn read_body(&self, body: &mut ureq::Body) -> Result<Vec<u8>, ExportError> {
        let too_large = || {
            ExportError::new(
                ErrorCode::FeedInvalid,
                "一覧 API の応答が上限（8MB）を超えています",
            )
        };
        let cap = self.max_body.saturating_add(1);
        // ureq 自身の上限は take より後ろに置く（take が先に止める。ureq の上限は最後の保険）
        let reader = body.with_config().limit(cap.saturating_add(1)).reader();
        let mut out = Vec::new();
        reader
            .take(cap)
            .read_to_end(&mut out)
            .map_err(|e| match ureq::Error::from(e) {
                ureq::Error::BodyExceedsLimit(_) => too_large(),
                // 通信の失敗（期限・切断・途中で止まった）だけを「使えない」とする。それ以外（壊れた圧縮など）は、
                // 応答の中身が使えないものとして feed_invalid にそろえる
                ureq::Error::Timeout(_)
                | ureq::Error::Io(_)
                | ureq::Error::Protocol(_)
                | ureq::Error::BodyStalled
                | ureq::Error::ConnectionFailed => {
                    unavailable("一覧 API の本文を読み取れませんでした")
                }
                _ => ExportError::new(ErrorCode::FeedInvalid, "一覧 API の本文を展開できません"),
            })?;
        if out.len() as u64 > self.max_body {
            return Err(too_large());
        }
        Ok(out)
    }

    /// 取得して、検査して、候補を選ぶ。
    pub fn select(&self, period: &Period) -> Result<Selection, ExportError> {
        listing::select(&self.fetch(period)?, period)
    }
}
