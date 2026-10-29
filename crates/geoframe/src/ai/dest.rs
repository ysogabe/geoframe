//! AI の接続先の固定（LAN は private/loopback、クラウドは内部の Public policy）:（P4 / T4.2。設計は p4-design.md §5「接続先は管理者設定のみ」）。
//!
//! AI 専用の HTTP クライアント（[`super::http`]）の名前解決をここに置き換える。`ureq` が URL の host を
//! 解決しようとした時点で、**設定した入口の host:port だけ**を、**解決された全アドレスが private／loopback のとき
//! だけ**許し、その検査済みのアドレス列をそのまま `ureq` の接続へ渡す（`ureq` は検査後に別の DNS 問い合わせをしない）。
//! URL を IP 文字列に書き換えないので、`Host` ヘッダーと TLS の SNI・証明書の検証は元の host のまま。
//!
//! - 許可: RFC 1918（10/8・172.16/12・192.168/16）、IPv4 loopback（127/8）、IPv6 loopback（::1）、
//!   IPv6 ULA（fc00::/7）。IPv4-mapped IPv6（`::ffff:a.b.c.d`）は IPv4 に正規化してから判定する。
//! - 拒否（全体を拒否する）: 公開アドレス、link-local・CGNAT・unspecified・multicast などの特殊アドレス、
//!   空の解決結果、解決結果が 16 個を超えるもの。**1 つでも許可外が混ざれば全体を拒否**する（許可外へ
//!   フォールバックされうる混在を許さない）。
//! - 名前解決は専用の補助スレッドで行い、要求の残り時間（`timeout`）を越えて待たない。同時に走る補助スレッドは
//!   [`MAX_INFLIGHT_LOOKUPS`] 本まで（遅い DNS でスレッドが増え続けない。上限なら時間切れとして扱う）。
//!
//! クラウドは `PinResolver::public`（crate内だけ）で同じ期限・全件検査・pinを使い、通常の公開IPだけに制限する。
//! 本番の任意host/policy切替入口は無い（`ai::cloud`の固定宛先からのみ構築する）。
//!
//! # `ureq` の unversioned API
//!
//! [`ureq::unversioned::resolver::Resolver`] と [`ureq::unversioned::transport::DefaultConnector`] は `ureq` の
//! semver の対象外（minor で変わりうる）。使う箇所はこのファイルの [`PinResolver`] と、[`super::http`] の
//! `Agent::with_parts` の 1 か所だけ。`ureq` を更新するときは、この 2 か所のコンパイルと、
//! `tests_http.rs` の接続先固定の試験（公開アドレス混在・過多・rebinding・Host・SNI）を必ず確認する。

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ArrayVec, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;

/// 解決結果の最大個数（`ureq` の `ResolvedSocketAddrs` の上限と同じ）。超えたら全体を拒否する。
pub const MAX_ADDRS: usize = 16;
/// 同時に走る名前解決の補助スレッドの上限。
pub const MAX_INFLIGHT_LOOKUPS: usize = 4;
/// 名前解決を待つ長さの上限（`ureq` の残り時間がこれより長くても）。
const MAX_LOOKUP_WAIT: Duration = Duration::from_secs(30);

/// 許可する宛先か（private・loopback。IPv4-mapped は IPv4 に正規化して判定）。
pub fn is_allowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_allowed_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_allowed_v4(v4);
            }
            // ::1、fc00::/7（ULA）。link-local（fe80::/10）・IPv4-compatible・NAT64 などは許さない
            v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

fn is_allowed_v4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback()
}

/// クラウドは通常の公開ユニキャストだけ。IPv4-mapped/変換用 IPv6 も拒否する。
pub(super) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let n = u32::from(v);
            let within = |base: u32, bits: u32| n >> (32 - bits) == base >> (32 - bits);
            ![
                (0x00000000, 8),
                (0x0a000000, 8),
                (0x64400000, 10),
                (0x7f000000, 8),
                (0xa9fe0000, 16),
                (0xac100000, 12),
                (0xc0000000, 24),
                (0xc0000200, 24),
                (0xc01fc400, 24),
                (0xc034c100, 24),
                (0xc0af3000, 24),
                (0xc0586300, 24),
                (0xc0a80000, 16),
                (0xc6120000, 15),
                (0xc6336400, 24),
                (0xcb007100, 24),
                (0xe0000000, 3),
            ]
            .into_iter()
            .any(|(base, bits)| within(base, bits))
        }
        IpAddr::V6(v) => {
            let n = u128::from(v);
            // 2000::/3 の通常 global unicast だけ。IETF特殊用途 2001::/23、文書用、6to4を除く。
            n >> 125 == 1
                && n >> 105 != u128::from(0x2001u16) << 7
                && n >> 96 != 0x20010db8
                && n >> 112 != 0x2002
                && n >> 108 != 0x3fff0
                && n >> 80 != 0x2620004f8000
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Policy {
    Lan,
    Public,
}

/// 検査の結果、拒否した理由（ログ・エラーには種類だけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// 設定した入口の host:port ではない（任意の URL を受けない）。
    NotConfiguredTarget,
    /// private／loopback でないアドレスが 1 つでも含まれる。
    Disallowed,
    /// 解決結果が [`MAX_ADDRS`] を超える。
    TooMany,
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConfiguredTarget => "not_configured_target",
            Self::Disallowed => "disallowed_address",
            Self::TooMany => "too_many_addresses",
        })
    }
}

impl std::error::Error for Denied {}

/// 解決結果（0 個でないもの）全体の検査。
pub fn vet(addrs: &[SocketAddr]) -> Result<(), Denied> {
    if addrs.len() > MAX_ADDRS {
        return Err(Denied::TooMany);
    }
    if addrs.iter().all(|a| is_allowed_ip(a.ip())) {
        Ok(())
    } else {
        Err(Denied::Disallowed)
    }
}

/// 名前解決の差し込み口。本番は [`SystemLookup`]。試験だけが偽の DNS を差し込む（`#[cfg(test)]` の構築口のみ。
/// 本番の設定・環境変数からは選べない）。
pub trait Lookup: Send + Sync + 'static {
    /// `host` を解決し、最大 `MAX_ADDRS + 1` 個まで返す（超過を検出するため 1 個多く読む）。
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

/// OS の名前解決（`getaddrinfo`）。
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port)
            .to_socket_addrs()?
            .take(MAX_ADDRS + 1)
            .collect())
    }
}

/// 設定した入口 1 つだけを解決する `ureq` の Resolver。
pub struct PinResolver {
    host: String,
    port: u16,
    lookup: Arc<dyn Lookup>,
    inflight: Arc<AtomicUsize>,
    policy: Policy,
    #[cfg(test)]
    loopback_stub: bool,
}

impl fmt::Debug for PinResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinResolver").finish_non_exhaustive()
    }
}

impl PinResolver {
    /// `host` は URL の host 部分（IPv6 リテラルは `[...]` を外したもの）。
    pub fn new(host: &str, port: u16, lookup: Arc<dyn Lookup>) -> Self {
        Self {
            host: host.to_ascii_lowercase(),
            port,
            lookup,
            inflight: Arc::new(AtomicUsize::new(0)),
            policy: Policy::Lan,
            #[cfg(test)]
            loopback_stub: false,
        }
    }

    pub(super) fn public(host: &str, lookup: Arc<dyn Lookup>) -> Self {
        let mut resolver = Self::new(host, 443, lookup);
        resolver.policy = Policy::Public;
        resolver
    }

    #[cfg(test)]
    pub(super) fn stub(host: &str, port: u16, lookup: Arc<dyn Lookup>) -> Self {
        let mut resolver = Self::new(host, port, lookup);
        resolver.policy = Policy::Public;
        resolver.loopback_stub = true;
        resolver
    }

    fn vet(&self, addrs: &[SocketAddr]) -> Result<(), Denied> {
        if addrs.len() > MAX_ADDRS {
            return Err(Denied::TooMany);
        }
        if addrs.is_empty() {
            return Err(Denied::Disallowed);
        }
        let allowed = |ip| {
            #[cfg(test)]
            if self.loopback_stub {
                return matches!(ip, IpAddr::V4(v) if v.is_loopback());
            }
            match self.policy {
                Policy::Lan => is_allowed_ip(ip),
                Policy::Public => is_public_ip(ip),
            }
        };
        if addrs.iter().all(|a| {
            (matches!(self.policy, Policy::Lan) || a.port() == self.port) && allowed(a.ip())
        }) {
            Ok(())
        } else {
            Err(Denied::Disallowed)
        }
    }

    /// 補助スレッドで名前解決する。残り時間で打ち切り、同時数を上限で止める。
    fn lookup_bounded(&self, wait: Duration) -> Result<Vec<SocketAddr>, ureq::Error> {
        let timeout = || ureq::Error::Timeout(ureq::Timeout::Resolve);
        if self.inflight.fetch_add(1, Ordering::SeqCst) >= MAX_INFLIGHT_LOOKUPS {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return Err(timeout());
        }
        let (tx, rx) = mpsc::channel();
        let (lookup, host, port, inflight) = (
            Arc::clone(&self.lookup),
            self.host.clone(),
            self.port,
            Arc::clone(&self.inflight),
        );
        let spawned = std::thread::Builder::new()
            .name("ai-dns".into())
            .spawn(move || {
                let result = lookup.lookup(&host, port);
                inflight.fetch_sub(1, Ordering::SeqCst);
                let _ = tx.send(result);
            });
        if spawned.is_err() {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return Err(timeout());
        }
        match rx.recv_timeout(wait.min(MAX_LOOKUP_WAIT)) {
            Ok(Ok(addrs)) => Ok(addrs),
            Ok(Err(_)) => Err(ureq::Error::HostNotFound),
            Err(_) => Err(timeout()),
        }
    }
}

impl Resolver for PinResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let deny = |d: Denied| ureq::Error::Other(Box::new(d));
        let host = uri.host().unwrap_or_default();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
            Some("https") => 443,
            _ => 80,
        });
        if !host.eq_ignore_ascii_case(&self.host) || port != self.port {
            return Err(deny(Denied::NotConfiguredTarget));
        }
        // IP リテラルは名前解決しない（そのまま検査する）
        let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => self.lookup_bounded(*timeout.after)?,
        };
        if addrs.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        self.vet(&addrs).map_err(deny)?;
        // 名前解決を待っている間に停止が来ていれば、接続しない（要求の書き込み前の確認は gate.rs）
        if super::gate::stopped() {
            return Err(ureq::Error::Other(Box::new(super::gate::StopRequested)));
        }
        let mut out: ResolvedSocketAddrs =
            ArrayVec::from_fn(|_| SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0));
        for addr in addrs {
            out.push(addr);
        }
        Ok(out)
    }
}

/// `ureq` のエラーが [`Denied`] か。
pub fn denied(error: &ureq::Error) -> Option<Denied> {
    match error {
        ureq::Error::Other(inner) => inner.downcast_ref::<Denied>().copied(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn only_private_and_loopback_are_allowed() {
        for ok in [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "127.0.0.1",
            "::1",
            "fd00::1",
            "fc00::1",
            "::ffff:192.168.1.1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_allowed_ip(ip(ok)), "{ok}");
        }
        for ng in [
            "8.8.8.8",
            "172.32.0.1",
            "172.15.255.255",
            "100.64.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::",
            "fe80::1",
            "2001:db8::1",
            "ff02::1",
            "::ffff:8.8.8.8",
            "::ffff:169.254.0.1",
            // IPv4-compatible（::a.b.c.d）は正規化しない
            "::192.168.1.1",
            "64:ff9b::c0a8:101",
            "fec0::1",
        ] {
            assert!(!is_allowed_ip(ip(ng)), "{ng}");
        }
    }

    #[test]
    fn one_bad_address_denies_everything_and_the_count_is_capped() {
        let sa = |t: &str| SocketAddr::new(ip(t), 80);
        assert_eq!(vet(&[sa("10.0.0.1"), sa("fd00::2")]), Ok(()));
        assert_eq!(
            vet(&[sa("10.0.0.1"), sa("8.8.8.8")]),
            Err(Denied::Disallowed)
        );
        assert_eq!(
            vet(&[sa("8.8.8.8"), sa("10.0.0.1")]),
            Err(Denied::Disallowed)
        );
        let many: Vec<_> = (0..=MAX_ADDRS)
            .map(|i| sa(&format!("10.0.0.{i}")))
            .collect();
        assert_eq!(many.len(), MAX_ADDRS + 1);
        assert_eq!(vet(&many[..MAX_ADDRS]), Ok(()));
        assert_eq!(vet(&many), Err(Denied::TooMany));
    }
}
