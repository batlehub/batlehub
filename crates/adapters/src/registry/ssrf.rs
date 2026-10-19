//! SSRF guard for upstream artifact fetches.
//!
//! Several registry adapters (PyPI, GitLab, Forgejo/Gitea) fetch a download URL
//! that is supplied by the *upstream index/release JSON*, i.e. controlled by the
//! project owner rather than the operator. Without a guard, a malicious entry can
//! point that URL — or a `30x` redirect from it — at an internal address (e.g. the
//! cloud metadata endpoint `169.254.169.254`) and stream the response back, and,
//! because reqwest does not strip *custom* auth headers (`PRIVATE-TOKEN`, …) on
//! cross-host redirects, exfiltrate the operator's configured credentials.
//!
//! This module provides:
//! - [`ensure_public_url`] — reject a URL whose host resolves to a private,
//!   reserved, loopback or link-local address (blocks SSRF to internal hosts),
//! - [`fetch_following_redirects`] — follow redirects manually, re-checking every
//!   hop with [`ensure_public_url`] and attaching credentials **only** while the
//!   URL stays on the configured instance origin (blocks credential exfiltration
//!   across redirects).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use batlehub_core::error::CoreError;
use reqwest::header::LOCATION;

use super::http_client::same_origin;

/// Maximum number of redirects we will follow for an upstream artifact fetch.
const MAX_REDIRECTS: usize = 5;

/// Is `v4` in a range an upstream-supplied URL must never be allowed to target?
///
/// Everything IANA's special-purpose registry marks as not globally reachable,
/// plus multicast. `fuzz/fuzz_targets/fuzz_ssrf_ip.rs` holds an independent
/// CIDR table and checks this function against it in both directions.
fn is_blocked_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()            // 127.0.0.0/8
        || v4.is_private()      // 10/8, 172.16/12, 192.168/16
        || v4.is_link_local()   // 169.254.0.0/16 (incl. cloud metadata)
        || v4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || o[0] == 0                                   // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64)        // CGNAT 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // IETF protocol assignments 192.0.0.0/24
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)   // 6to4 relay anycast 192.88.99.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18)        // benchmarking 198.18.0.0/15
        || o[0] >= 224 // multicast 224/4, reserved 240/4, broadcast
}

/// The IPv4 address embedded in `v6`, when `v6` is a form that some stack on
/// the path would turn into a packet to that IPv4 host.
///
/// - `::ffff:a.b.c.d` (mapped) and `::a.b.c.d` (compatible, which also covers
///   `::` and `::1`) — the host's own stack;
/// - `64:ff9b::a.b.c.d` — the well-known NAT64 prefix (RFC 6052): any network
///   with a NAT64 gateway delivers it to the v4 host;
/// - `2002:aabb:ccdd::/48` — 6to4 (RFC 3056), the v4 address in bits 16..48.
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let seg = v6.segments();
    let low32 = || {
        Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            seg[6] as u8,
            (seg[7] >> 8) as u8,
            seg[7] as u8,
        )
    };
    if let Some(v4) = v6.to_ipv4() {
        return Some(v4);
    }
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return Some(low32());
    }
    if seg[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            seg[1] as u8,
            (seg[2] >> 8) as u8,
            seg[2] as u8,
        ));
    }
    None
}

/// Is `ip` an address an upstream-supplied URL must never be allowed to target?
///
/// The one rule: IP literals in a URL and every address DNS returns for a host
/// both go through it (see [`ensure_public_url`]).
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            // A form that reaches an IPv4 host is that host: neither
            // `::ffff:169.254.169.254` nor `64:ff9b::a9fe:a9fe` may slip through.
            if let Some(v4) = embedded_v4(v6) {
                return is_blocked_v4(v4);
            }
            let seg = v6.segments();
            (seg[0] & 0xfe00) == 0xfc00                 // unique-local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80          // link-local fe80::/10
                || (seg[0] & 0xffc0) == 0xfec0          // site-local fec0::/10 (deprecated)
                || (seg[0] & 0xff00) == 0xff00          // multicast ff00::/8
                || seg[..4] == [0x100, 0, 0, 0]         // discard-only 100::/64
                || (seg[0] == 0x64 && seg[1] == 0xff9b && seg[2] == 1) // local-use NAT64 64:ff9b:1::/48
                || (seg[0] == 0x2001 && seg[1] < 0x200) // IETF protocol assignments 2001::/23 (Teredo, ORCHID, benchmarking…)
                || (seg[0] == 0x2001 && seg[1] == 0xdb8) // documentation 2001:db8::/32
                || (seg[0] == 0x3fff && seg[1] < 0x1000) // documentation 3fff::/20
                || seg[0] == 0x5f00                     // SRv6 SIDs 5f00::/16
                || seg[0] < 0x100 // reserved ::/8 beyond the v4 forms above
        }
    }
}

fn blocked_error(url: &reqwest::Url) -> CoreError {
    let url = super::http_client::redacted(url);
    CoreError::Registry(format!(
        "refusing to fetch upstream URL '{url}': host resolves to a private, reserved, or \
         link-local address (SSRF guard)"
    ))
}

/// Reject `url` if its host is — or resolves to — a private/reserved/loopback/
/// link-local address. Hostnames are resolved (best-effort DNS-rebinding guard):
/// every resolved address must be public.
pub async fn ensure_public_url(url: &reqwest::Url) -> Result<(), CoreError> {
    let shown = super::http_client::redacted(url);
    let host = url.host_str().ok_or_else(|| {
        CoreError::Registry(format!("refusing to fetch upstream URL '{shown}': no host"))
    })?;

    // IP literal: check directly, no DNS. `host_str()` spells an IPv6 literal
    // with its brackets, which `IpAddr` does not parse — without the strip,
    // `[::1]` fell through to a DNS lookup instead of this check.
    let literal = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = literal.parse::<IpAddr>() {
        return if is_blocked_ip(ip) {
            Err(blocked_error(url))
        } else {
            Ok(())
        };
    }

    // Hostname: resolve and require every address to be public.
    let port = url.port_or_known_default().unwrap_or(0);
    let addrs = tokio::net::lookup_host((host, port)).await.map_err(|e| {
        CoreError::Registry(format!(
            "refusing to fetch upstream URL '{shown}': DNS resolution failed: {e}"
        ))
    })?;
    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if is_blocked_ip(addr.ip()) {
            return Err(blocked_error(url));
        }
    }
    if !saw_any {
        return Err(CoreError::Registry(format!(
            "refusing to fetch upstream URL '{shown}': host did not resolve to any address"
        )));
    }
    Ok(())
}

/// A resolver that only ever hands the connector public addresses.
///
/// [`ensure_public_url`] resolves the host to decide, and reqwest resolves it
/// *again* to connect: a TTL-0 record can answer public to the first lookup and
/// `169.254.169.254` to the second (DNS rebinding). With this resolver on the
/// client, the second lookup is filtered by the same [`is_blocked_ip`], so what
/// is dialled is what was checked. IP-literal hosts never reach a resolver and
/// are decided by `ensure_public_url` alone, which cannot be rebound.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| !is_blocked_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("'{host}' resolves to no public address (SSRF guard)").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Mount [`PublicOnlyResolver`] on a client that only ever dials
/// upstream-supplied, off-origin URLs — the `plain` half of a redirect-walking
/// pair. Never on a client that also talks to the configured upstream: a
/// private upstream is a legitimate deployment.
///
/// Skipped when an egress proxy is in play (configured, or from the
/// environment, which reqwest reads by default): the connector then resolves
/// the proxy, not the target, and an internal proxy is the normal case.
pub fn public_only_dns(
    builder: reqwest::ClientBuilder,
    opts: &super::http_client::UpstreamHttpOptions,
) -> reqwest::ClientBuilder {
    let env_proxy = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .iter()
    .any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    if opts.proxy_url.is_some() || env_proxy {
        return builder;
    }
    builder.dns_resolver(std::sync::Arc::new(PublicOnlyResolver))
}

/// Fetch `start` with `credentialed` client, following up to [`MAX_REDIRECTS`]
/// redirects manually. Every hop (initial URL and each `Location`) is validated
/// with [`ensure_public_url`]. Credentials (the `credentialed` client's default
/// auth headers, plus `basic_auth`) are attached **only** while the URL stays on
/// `instance_origin`; once a redirect leaves that origin the request is re-issued
/// with the credential-free `plain` client so the operator's token can never be
/// sent off-instance.
///
/// Both `credentialed` and `plain` MUST be built with redirects disabled
/// (`reqwest::redirect::Policy::none()`), otherwise redirects would be followed
/// twice (once by reqwest, unchecked, and once here).
pub async fn fetch_following_redirects(
    credentialed: &reqwest::Client,
    plain: &reqwest::Client,
    basic_auth: &Option<(String, String)>,
    instance_origin: &str,
    start: reqwest::Url,
) -> Result<reqwest::Response, CoreError> {
    fetch_following_redirects_trusting(
        credentialed,
        plain,
        basic_auth,
        &[instance_origin.to_owned()],
        start,
    )
    .await
}

/// [`fetch_following_redirects`] for a forge whose bytes live on more than one
/// operator-trusted origin (RFC 0019 §6.3).
///
/// GitHub is three hosts: the API on `api.github.com`, archives on
/// `github.com` and raw files on `raw.githubusercontent.com`, and a private
/// repository needs the token on all three. Each is as operator-trusted as
/// the single instance origin the Forgejo client passes — they are derived
/// from the configured base URL, never from a response — so credentials are
/// attached while the URL is on any of them and the SSRF check applies to
/// every hop that is not.
pub async fn fetch_following_redirects_trusting(
    credentialed: &reqwest::Client,
    plain: &reqwest::Client,
    basic_auth: &Option<(String, String)>,
    trusted_origins: &[String],
    start: reqwest::Url,
) -> Result<reqwest::Response, CoreError> {
    let bases: Vec<reqwest::Url> = trusted_origins
        .iter()
        .filter_map(|o| reqwest::Url::parse(o).ok())
        .collect();
    let mut url = start;

    for _ in 0..=MAX_REDIRECTS {
        let same_origin_as_instance = bases.iter().any(|b| same_origin(&url, b));

        // The configured instance origin is operator-trusted (a private/internal
        // upstream is a legitimate deployment), so the SSRF check applies only to
        // hosts OFF that origin — i.e. cross-origin download URLs and redirect
        // targets, which are the attacker-controllable vectors.
        if !same_origin_as_instance {
            ensure_public_url(&url).await?;
        }

        let mut req = if same_origin_as_instance {
            let mut r = credentialed.get(url.clone());
            if let Some((u, p)) = basic_auth {
                r = r.basic_auth(u, Some(p));
            }
            r
        } else {
            // Off-instance: never send the operator's credentials.
            plain.get(url.clone())
        };
        // Defensive: some callers may pass a shared client; make intent explicit.
        req = req.header(reqwest::header::ACCEPT, "*/*");

        let resp = req
            .send()
            .await
            .map_err(super::http_client::to_registry_error)?;

        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    CoreError::Registry(format!(
                        "upstream returned a redirect with no valid Location header (from '{}')",
                        super::http_client::redacted(&url)
                    ))
                })?;
            // Resolve relative redirects against the current URL.
            url = url.join(location).map_err(|e| {
                CoreError::Registry(format!("invalid redirect Location '{location}': {e}"))
            })?;
            continue;
        }

        return Ok(resp);
    }

    Err(CoreError::Registry(format!(
        "too many redirects (>{MAX_REDIRECTS}) fetching upstream artifact"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The connect-time lookup is filtered too, so a name that resolves
    /// internally is refused however it answered the check before it.
    #[tokio::test]
    async fn the_connect_time_resolver_hands_out_no_internal_address() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;
        let name = reqwest::dns::Name::from_str("localhost").unwrap();
        assert!(PublicOnlyResolver.resolve(name).await.is_err());
    }

    #[test]
    fn blocks_ipv4_private_reserved_and_metadata() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "0.0.0.0",
            "100.64.1.1", // CGNAT
            "198.18.0.1", // benchmarking
            "240.0.0.1",  // reserved
            "255.255.255.255",
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
    }

    #[test]
    fn allows_public_ipv4() {
        for ip in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[test]
    fn blocks_ipv6_local_and_v4_embedded_metadata() {
        for ip in [
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            // IPv4-mapped (`::ffff:a.b.c.d`)
            "::ffff:169.254.169.254",
            "::ffff:127.0.0.1",
            // IPv4-compatible (`::a.b.c.d`) — must be blocked too
            "::169.254.169.254",
            "::127.0.0.1",
            "::10.0.0.1",
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
        assert!(!is_blocked_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn blocks_the_v4_ranges_the_first_table_missed() {
        for ip in [
            "224.0.0.1",       // multicast
            "239.255.255.250", // SSDP multicast
            "192.0.0.170",     // IETF protocol assignments 192.0.0.0/24
            "192.88.99.1",     // 6to4 relay anycast
            "198.51.100.7",    // TEST-NET-2
            "203.0.113.9",     // TEST-NET-3
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in ["223.255.255.255", "192.0.1.1", "192.88.98.1", "198.20.0.1"] {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[test]
    fn nat64_and_6to4_are_classified_by_the_v4_host_they_reach() {
        for ip in [
            "64:ff9b::a9fe:a9fe", // NAT64 → 169.254.169.254
            "64:ff9b::7f00:1",    // NAT64 → 127.0.0.1
            "64:ff9b:1::808:808", // local-use NAT64 prefix, blocked whole
            "2002:a9fe:a9fe::1",  // 6to4 → 169.254.169.254
            "2002:0a00:0001::",   // 6to4 → 10.0.0.1
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in ["64:ff9b::808:808", "2002:808:808::1"] {
            assert!(
                !is_blocked_ip(ip.parse().unwrap()),
                "{ip} reaches 8.8.8.8, allowed"
            );
        }
    }

    #[test]
    fn blocks_the_non_global_v6_ranges() {
        for ip in [
            "fec0::1", // site-local
            "ff02::1", // multicast
            "ff0e::1",
            "100::1",             // discard-only
            "2001::1",            // Teredo
            "2001:0:4136:e378::", // Teredo
            "2001:2::1",          // benchmarking
            "2001:db8::1",        // documentation
            "3fff::1",            // documentation (RFC 9637)
            "5f00::1",            // SRv6
            "::1:0:0:1",          // reserved ::/8
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in [
            "2001:200::1",
            "2a00:1450::1",
            "100:0:0:1::1",
            "3ff0::102a",
            "3fff:1000::1",
        ] {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[tokio::test]
    async fn ensure_public_url_checks_a_bracketed_ipv6_literal_without_dns() {
        for url in [
            "http://[::1]/",
            "http://[fd00::1]:8080/x",
            "http://[::ffff:a9fe:a9fe]/",
        ] {
            let err = ensure_public_url(&reqwest::Url::parse(url).unwrap())
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("SSRF guard"), "{url}: {err}");
        }
        let public = reqwest::Url::parse("http://[2606:4700:4700::1111]/").unwrap();
        assert!(ensure_public_url(&public).await.is_ok());
    }

    #[tokio::test]
    async fn ensure_public_url_rejects_metadata_ip_literal() {
        let url = reqwest::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        assert!(ensure_public_url(&url).await.is_err());
    }
}
