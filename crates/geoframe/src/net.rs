//! 外向きの HTTPS（P2: Drive・地理院・Nominatim）。
//!
//! # 方針
//!
//! - クライアントは `ureq`（同期）。取り込みの同期サイクルは専用の std スレッドで動く（[`crate::sync`]）ので、
//!   tokio のブロッキングプールを使わずに済む。API を受ける tokio の worker からは呼ばない。
//! - TLS は rustls（暗号は ring）。**ルート証明書は `webpki-roots` をバイナリに埋め込む**ので、
//!   OS の証明書（`/etc/ssl/certs`）に依存しない。musl 静的ビルドのまま動く
//!   （docs/rust-migration/build.md §7）。native-tls / openssl は使わない。
//! - 名前解決は std の `getaddrinfo`（musl では musl の実装）。
//! - プロキシは `ureq` の既定どおり環境変数（`HTTPS_PROXY` など）を見る。unit は与えない。
//!
//! # ログ
//!
//! 通信の失敗は**種類**（[`Failure`]）と HTTP ステータスだけを出す。URL（座標・Drive の ID・検索語が
//! 入りうる）と、`ureq` のエラー文字列は出さない。

use std::time::Duration;

use serde_json::Value;

/// 地理院の逆ジオコーダ（`config.py` の `GSI_RGEO`）。`{lat}`・`{lon}` を埋めて使う。
pub const GSI_RGEO: &str =
    "https://mreversegeocoder.gsi.go.jp/reverse-geocoder/LonLatToAddress?lat={lat}&lon={lon}";

/// 確認用の公開の代表点（東京駅付近。個人の位置ではない）。`selfcheck-net` だけが使う。
const SELFCHECK_POINT: (f64, f64) = (35.681, 139.767);

/// 接続を張るまでの待ち。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 1 回の要求（名前解決から本文の読み切りまで）の上限。Drive の大きな原本のダウンロードだけ、
/// 要求ごとに延ばす（[`crate::drive`]）。
const GLOBAL_TIMEOUT: Duration = Duration::from_secs(30);

/// 同期サイクルが使う HTTP クライアント。複製は安く、接続は共有される。
pub fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(GLOBAL_TIMEOUT))
            // 4xx/5xx も応答として受け、呼び出し側が状態コードで判断する
            .http_status_as_error(false)
            .user_agent(concat!("geoframe/", env!("CARGO_PKG_VERSION")))
            .build(),
    )
}

/// 通信の失敗の種類。メッセージ・URL を持たない（ログに出してよい）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// 名前解決できない。
    Dns,
    /// 接続できない（拒否・到達不能）。
    Connect,
    /// TLS（証明書の検証・ハンドシェイク）。
    Tls,
    /// 時間切れ。
    Timeout,
    /// 本文が大きすぎる・プロトコル違反など、その他。
    Other,
}

impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::Other => "other",
        }
    }
}

impl From<&ureq::Error> for Failure {
    fn from(e: &ureq::Error) -> Self {
        match e {
            ureq::Error::HostNotFound => Self::Dns,
            ureq::Error::ConnectionFailed => Self::Connect,
            ureq::Error::Timeout(_) => Self::Timeout,
            ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::Pem(_) => Self::Tls,
            ureq::Error::Io(io) => match io.kind() {
                // rustls は証明書の不正などを InvalidData の I/O エラーとして返すことがある
                std::io::ErrorKind::InvalidData => Self::Tls,
                std::io::ErrorKind::TimedOut => Self::Timeout,
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NetworkUnreachable
                | std::io::ErrorKind::HostUnreachable => Self::Connect,
                _ => Self::Other,
            },
            _ => Self::Other,
        }
    }
}

/// `selfcheck-net` の結果（種類と状態コードだけ）。
#[derive(Debug, PartialEq, Eq)]
pub struct SelfCheck {
    /// 応答を受けた（TLS・名前解決を含めて通った）。
    pub reached: bool,
    pub http_status: Option<u16>,
    /// 本文が JSON として読めた。
    pub json: bool,
    pub failure: Option<Failure>,
}

impl SelfCheck {
    pub fn ok(&self) -> bool {
        self.reached && self.http_status == Some(200) && self.json
    }

    /// 標準出力へ出す 1 行。状態と HTTP ステータスだけで、URL・座標・応答の中身は含めない。
    pub fn summary(&self) -> String {
        let status = if self.ok() { "ok" } else { "ng" };
        let http = self
            .http_status
            .map_or_else(|| "-".to_string(), |s| s.to_string());
        let failure = self.failure.map_or("-", Failure::as_str);
        format!(
            "status={status} http={http} json={} failure={failure}",
            if self.json { "ok" } else { "ng" }
        )
    }
}

/// 地理院の逆ジオコーダへ公開の代表点を 1 件問い合わせ、TLS・名前解決・証明書の検証が通るかを確かめる。
pub fn selfcheck(agent: &ureq::Agent) -> SelfCheck {
    let url = GSI_RGEO
        .replace("{lat}", &SELFCHECK_POINT.0.to_string())
        .replace("{lon}", &SELFCHECK_POINT.1.to_string());
    match agent.get(&url).call() {
        Ok(mut response) => {
            let http_status = Some(response.status().as_u16());
            let json = response
                .body_mut()
                .read_to_string()
                .ok()
                .and_then(|body| serde_json::from_str::<Value>(&body).ok())
                .is_some_and(|v| v.is_object());
            SelfCheck {
                reached: true,
                http_status,
                json,
                failure: None,
            }
        }
        Err(e) => SelfCheck {
            reached: false,
            http_status: None,
            json: false,
            failure: Some(Failure::from(&e)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn summary_has_only_state_and_status() {
        let ok = SelfCheck {
            reached: true,
            http_status: Some(200),
            json: true,
            failure: None,
        };
        assert!(ok.ok());
        assert_eq!(ok.summary(), "status=ok http=200 json=ok failure=-");
        let ng = SelfCheck {
            reached: false,
            http_status: None,
            json: false,
            failure: Some(Failure::Dns),
        };
        assert!(!ng.ok());
        assert_eq!(ng.summary(), "status=ng http=- json=ng failure=dns");
        // 200 でも JSON として読めなければ ng
        let bad = SelfCheck {
            reached: true,
            http_status: Some(200),
            json: false,
            failure: None,
        };
        assert!(!bad.ok());
    }

    /// ループバックの平文 HTTP サーバー（この端末の中だけ。外へ出ない）で、状態コードを受けられること
    /// （`http_status_as_error(false)`）と、接続できないときの失敗の種類を確かめる。
    #[test]
    fn agent_returns_error_statuses_and_classifies_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let response = agent().get(&format!("http://{addr}/")).call().unwrap();
        assert_eq!(response.status().as_u16(), 503);
        server.join().unwrap();

        // 待ち受けの無いポート（閉じたばかりのもの）
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let err = agent().get(&format!("http://{addr}/")).call().unwrap_err();
        assert!(matches!(
            Failure::from(&err),
            Failure::Connect | Failure::Other
        ));
    }
}
