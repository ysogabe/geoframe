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

/// 地理院の標高 API（`config.py` の `GSI_ELEV`）。`{lat}`・`{lon}` を埋めて使う。
pub const GSI_ELEV: &str = "https://cyberjapandata2.gsi.go.jp/general/dem/scripts/getelevation.php?lon={lon}&lat={lat}&outtype=JSON";

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
    /// 接続は張れたが、応答のヘッダーを受け取る前、または本文の途中で切れた（切断・リセット・中断・
    /// 予期しない終端など）。
    Disconnected,
    /// TLS（証明書の検証・ハンドシェイク）。
    Tls,
    /// 時間切れ。
    Timeout,
    /// Drive の 403 の**理由の本文が読めなかった**（壊れたチャンク・途中で終わる gzip など、切断や時間切れ
    /// 以外の読み出しの失敗）。ヘッダーの前の `Protocol`（`Other`＝写真の失敗）とは別で、障害にする。
    /// 数百バイトの理由の本文が読めないのは、権限の不足とも流量制限とも決められない回線・相手の不調で、
    /// 写真の失敗として数えて待ちを付けると、直ってからも取得が遅れる。
    ReasonUnreadable,
    /// 小さな JSON の応答（地理院の逆ジオ・標高）の**本文が読めなかった**（壊れたチャンク・途中で終わる gzip など、
    /// 切断・時間切れ・TLS 以外の読み出しの失敗）。ヘッダーの前の `Protocol`（`Other`）とは別。数百バイトの本文が
    /// 読めないのは、座標の中身ではなく回線・相手の不調と言えるので、障害にする（ただし**どの座標でも起きるとは
    /// 限らない**ので、地理院の段は連続して複数の座標で失敗したときに中断する）。
    BodyUnreadable,
    /// 要求側の誤り・プロトコル違反・大きすぎる本文など、通信の障害とは言えないもの。
    Other,
}

impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Disconnected => "disconnected",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::ReasonUnreadable => "reason_unreadable",
            Self::BodyUnreadable => "body_unreadable",
            Self::Other => "other",
        }
    }

    /// 通信の障害か（写真・検索語といった**要求の中身のせいではなく**、回線・相手・TLS の不調と言える）。
    /// `Other` だけが、要求や応答の内容に由来する失敗（`ReasonUnreadable` は読み出しの種類によらず障害）。
    pub fn is_outage(self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// 回線が切れた・繋がっていない、と言える I/O の種類（本文を読んでいる最中の判定にも使う）。
pub fn is_disconnect(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        kind,
        K::ConnectionReset
            | K::ConnectionAborted
            | K::BrokenPipe
            | K::UnexpectedEof
            | K::NotConnected
            | K::ConnectionRefused
            | K::NetworkDown
            | K::NetworkUnreachable
            | K::HostUnreachable
    )
}

/// `ureq` のエラー（`ureq` 3.5 の全種類）を、**要求を送って応答のヘッダーを受け取るまで**の失敗として
/// 分類する。`Agent::call` / `send` が返す `Err` はすべてここに入る（ヘッダーを受け取ったあとの本文の
/// 読み出しの失敗は、呼び出し側が別に扱う。Drive の原本は `crate::drive` の本文の扱い）。
///
/// | `ureq::Error` | `Failure` | 通信の障害か |
/// | --- | --- | --- |
/// | `HostNotFound` | `Dns` | 障害 |
/// | `ConnectionFailed`・`ConnectProxyFailed` | `Connect` | 障害 |
/// | `Io`（`ConnectionRefused`・`NetworkUnreachable`・`HostUnreachable`・`NetworkDown`） | `Connect` | 障害 |
/// | `Io`（`TimedOut`・`WouldBlock`）・`Timeout(_)` | `Timeout` | 障害 |
/// | `Io`（`InvalidData`。rustls が証明書の不正などで返す）・`Tls`・`Rustls`・`Pem`・`TlsRequired` | `Tls` | 障害 |
/// | `Io`（上記以外のすべて: `UnexpectedEof`・`ConnectionReset`・`ConnectionAborted`・`BrokenPipe`・`NotConnected`・`PermissionDenied` など） | `Disconnected` | 障害 |
/// | `Protocol`・`LargeResponseHeader`（応答の頭が壊れている・大きすぎる） | `Other` | 障害としない（※） |
/// | `Http`・`BadUri`・`InvalidProxyUrl`・`RequireHttpsOnly`・`RedirectFailed`・`TooManyRedirects`・`StatusCode`・`BodyExceedsLimit`・`Other` など | `Other` | 障害としない |
///
/// 上の表は**ヘッダーまで**。Drive の 403 の理由の本文を読む失敗は別で、`Protocol`・`Decompress` など
/// 表で `Other` になる種類も [`Failure::ReasonUnreadable`] にして障害にする（`crate::drive` の表）。
/// 地理院の逆ジオ・標高の本文を読む失敗も同じ考え方で、`Other` になる種類は [`Failure::BodyUnreadable`]
/// にして障害にする（`crate::geo` の表。上限超過の `BodyExceedsLimit` だけは応答の中身の問題なので障害にしない）。
///
/// ※ 応答は届いているが中身が壊れている。要求ごとに決まった結果になりうる（特定の写真だけが毎回壊れた
/// 応答になる）ので、障害として段を止めると、その 1 枚が残りの写真を永久に塞ぐ。写真の失敗として数えれば
/// 待ちが付き、ほかの写真は進む。切断は逆に、どの写真でも起きる回線・相手の不調なので障害にする。
impl From<&ureq::Error> for Failure {
    fn from(e: &ureq::Error) -> Self {
        use std::io::ErrorKind as K;
        match e {
            ureq::Error::HostNotFound => Self::Dns,
            ureq::Error::ConnectionFailed | ureq::Error::ConnectProxyFailed(_) => Self::Connect,
            ureq::Error::Timeout(_) => Self::Timeout,
            ureq::Error::Tls(_)
            | ureq::Error::Rustls(_)
            | ureq::Error::Pem(_)
            | ureq::Error::TlsRequired => Self::Tls,
            ureq::Error::Io(io) => match io.kind() {
                K::InvalidData => Self::Tls,
                K::TimedOut | K::WouldBlock => Self::Timeout,
                K::ConnectionRefused
                | K::NetworkUnreachable
                | K::HostUnreachable
                | K::NetworkDown => Self::Connect,
                // 相手が応答の前に切った（`ureq` は `UnexpectedEof`）・リセット・中断など。
                // 応答のヘッダーを受け取る前の I/O の失敗は、種類によらず通信の障害にする
                _ => Self::Disconnected,
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

    /// `Failure::from` の表（ヘッダーを受け取るまで）どおりに分類され、`Other` 以外が障害になる。
    #[test]
    fn head_phase_errors_follow_the_table() {
        use std::io::ErrorKind as K;
        let io = |kind| ureq::Error::Io(std::io::Error::from(kind));
        for (kind, expected) in [
            (K::UnexpectedEof, Failure::Disconnected),
            (K::ConnectionReset, Failure::Disconnected),
            (K::ConnectionAborted, Failure::Disconnected),
            (K::BrokenPipe, Failure::Disconnected),
            (K::NotConnected, Failure::Disconnected),
            (K::PermissionDenied, Failure::Disconnected),
            (K::Other, Failure::Disconnected),
            (K::ConnectionRefused, Failure::Connect),
            (K::NetworkUnreachable, Failure::Connect),
            (K::HostUnreachable, Failure::Connect),
            (K::TimedOut, Failure::Timeout),
            (K::WouldBlock, Failure::Timeout),
            (K::InvalidData, Failure::Tls),
        ] {
            let failure = Failure::from(&io(kind));
            assert_eq!(failure, expected, "{kind:?}");
            assert!(failure.is_outage(), "{kind:?}");
        }
        for (error, expected) in [
            (ureq::Error::HostNotFound, Failure::Dns),
            (ureq::Error::ConnectionFailed, Failure::Connect),
            (
                ureq::Error::Timeout(ureq::Timeout::Global),
                Failure::Timeout,
            ),
            (
                ureq::Error::Timeout(ureq::Timeout::RecvResponse),
                Failure::Timeout,
            ),
            (ureq::Error::Tls("x"), Failure::Tls),
            (ureq::Error::BadUri("x".into()), Failure::Other),
            (ureq::Error::TooManyRedirects, Failure::Other),
            (ureq::Error::BodyExceedsLimit(1), Failure::Other),
            (ureq::Error::LargeResponseHeader(1, 1), Failure::Other),
        ] {
            let failure = Failure::from(&error);
            assert_eq!(failure, expected);
            assert_eq!(failure.is_outage(), expected != Failure::Other);
        }
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
        assert_eq!(Failure::from(&err), Failure::Connect);
    }
}
