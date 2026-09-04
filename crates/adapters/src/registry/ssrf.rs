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

use std::net::{IpAddr, Ipv4Addr};

use batlehub_core::error::CoreError;
use reqwest::header::LOCATION;

use super::http_client::same_origin;

/// Maximum number of redirects we will follow for an upstream artifact fetch.
const MAX_REDIRECTS: usize = 5;

/// Is `v4` in a range an upstream-supplied URL must never be allowed to target?
fn is_blocked_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()            // 127.0.0.0/8
        || v4.is_private()      // 10/8, 172.16/12, 192.168/16
        || v4.is_link_local()   // 169.254.0.0/16 (incl. cloud metadata)
        || v4.is_broadcast()    // 255.255.255.255
        || v4.is_unspecified()  // 0.0.0.0
        || v4.is_documentation()
        || o[0] == 0                                   // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64)        // CGNAT 100.64.0.0/10
        || (o[0] == 198 && (o[1] & 0xfe) == 18)        // benchmarking 198.18.0.0/15
        || o[0] >= 240 // reserved 240.0.0.0/4
}

/// Is `ip` an address an upstream-supplied URL must never be allowed to target?
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            // An IPv4-mapped (`::ffff:a.b.c.d`) or IPv4-compatible (`::a.b.c.d`)
            // address is really the v4 host — check it as such so neither
            // `::ffff:169.254.169.254` nor `::169.254.169.254` can slip through.
            // `to_ipv4()` covers both forms (it is broader than
            // `to_ipv4_mapped()`); native IPv6 addresses fall through unchanged.
            if let Some(v4) = v6.to_ipv4() {
                return is_blocked_v4(v4);
            }
            let seg = v6.segments();
            v6.is_loopback()                       // ::1
                || v6.is_unspecified()             // ::
                || (seg[0] & 0xfe00) == 0xfc00     // unique-local fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

fn blocked_error(url: &reqwest::Url) -> CoreError {
    CoreError::Registry(format!(
        "refusing to fetch upstream URL '{url}': host resolves to a private, reserved, or \
         link-local address (SSRF guard)"
    ))
}

/// Reject `url` if its host is — or resolves to — a private/reserved/loopback/
/// link-local address. Hostnames are resolved (best-effort DNS-rebinding guard):
/// every resolved address must be public.
pub async fn ensure_public_url(url: &reqwest::Url) -> Result<(), CoreError> {
    let host = url.host_str().ok_or_else(|| {
        CoreError::Registry(format!("refusing to fetch upstream URL '{url}': no host"))
    })?;

    // IP literal: check directly, no DNS.
    if let Ok(ip) = host.parse::<IpAddr>() {
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
            "refusing to fetch upstream URL '{url}': DNS resolution failed: {e}"
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
            "refusing to fetch upstream URL '{url}': host did not resolve to any address"
        )));
    }
    Ok(())
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
            .map_err(|e| CoreError::Registry(e.to_string()))?;

        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    CoreError::Registry(format!(
                        "upstream returned a redirect with no valid Location header (from '{url}')"
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

    #[tokio::test]
    async fn ensure_public_url_rejects_metadata_ip_literal() {
        let url = reqwest::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        assert!(ensure_public_url(&url).await.is_err());
    }
}
