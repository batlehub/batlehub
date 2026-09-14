//! Proxy trust — *which* forwarded headers the server believes, and *from whom*.
//!
//! Three signals arrive from a reverse proxy and all three are attacker-settable
//! when the server is exposed directly:
//!
//! | Header                              | Decides                          |
//! |-------------------------------------|----------------------------------|
//! | `Forwarded` / `X-Forwarded-Host`    | the host in every generated URL, and (with host-based routing) which registry serves the request |
//! | `X-Forwarded-Proto`                 | `http` vs `https` in generated URLs |
//! | `X-Forwarded-For`                   | the client IP the fail2ban middleware counts violations against |
//!
//! [`ProxyTrust`] is the configured policy (`[server].trusted_proxies`, falling
//! back to the deprecated `[ip_blocking].trusted_proxies`). It is resolved
//! against the TCP peer **once per request** into a [`PeerTrust`] verdict, which
//! the host-routing middleware stores in the request extensions so the outbound
//! URL helper and the IP-based middlewares all read the same answer instead of
//! each re-deriving it.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use actix_web::{dev::ServiceRequest, http::header, web, HttpMessage, HttpRequest};
use ipnet::IpNet;

use batlehub_config::schema::normalise_host;

/// The configured reverse-proxy trust policy, registered as `app_data` and
/// cloned into the host-routing middleware.
///
/// [`Default`] is the legacy-permissive policy, so an app that never registers
/// one (every existing test app, and any deployment with no `trusted_proxies`
/// key) behaves exactly as it did before this policy existed.
///
/// The list lives behind a lock, like every other hot-reloadable table, because
/// the host-routing table it guards *is* hot-reloadable: a reload that turns host
/// routing on while this policy stayed at its startup value would leave routing
/// driven by a header from any peer — the state `validate_host_routing` exists to
/// forbid. All clones share the lock, so `replace_from` reaches the copy held by
/// the middleware and the one in `app_data` alike.
#[derive(Clone, Debug, Default)]
pub struct ProxyTrust {
    /// `None` — no list configured anywhere.
    trusted: Arc<std::sync::RwLock<Option<Arc<Vec<IpNet>>>>>,
}

impl ProxyTrust {
    /// No policy configured: forwarded host/scheme are believed unconditionally
    /// and `X-Forwarded-For` is ignored. This is what the server did before
    /// `[server].trusted_proxies` existed.
    pub fn legacy_permissive() -> Self {
        Self::from_policy(None)
    }

    /// An explicit policy. An empty `networks` list means "trust nobody": every
    /// peer is [`PeerTrust::Untrusted`], so forwarded headers are ignored
    /// entirely.
    pub fn from_networks(networks: Vec<IpNet>) -> Self {
        Self::from_policy(Some(Arc::new(networks)))
    }

    fn from_policy(trusted: Option<Arc<Vec<IpNet>>>) -> Self {
        Self {
            trusted: Arc::new(std::sync::RwLock::new(trusted)),
        }
    }

    /// A snapshot of the policy in force, taken under a short read lock.
    fn policy(&self) -> Option<Arc<Vec<IpNet>>> {
        self.trusted
            .read()
            .expect("proxy trust lock poisoned")
            .clone()
    }

    /// Adopt `other`'s policy, in place, for every clone of this handle — called
    /// by the hot-reload applier so the trust policy and the host-routing table it
    /// guards change together. In-flight requests keep the verdict they already
    /// resolved.
    pub fn replace_from(&self, other: &Self) {
        let snapshot = other.policy();
        *self.trusted.write().expect("proxy trust lock poisoned") = snapshot;
    }

    /// Build from raw config entries, widening bare addresses to `/32` / `/128`.
    /// `None` yields the legacy-permissive policy.
    ///
    /// `[server].trusted_proxies` is validated by `AppConfig::validate`, so a
    /// malformed entry can only come from the deprecated
    /// `[ip_blocking].trusted_proxies`, which predates that validator. Such an
    /// entry is dropped individually — exactly what the old `extract_client_ip`
    /// did — so the valid entries around it keep working; the config layer
    /// surfaces it as `PROXY_TRUST_INVALID_DEPRECATED_ENTRY`. Discarding the whole
    /// list instead would turn one stale hostname into "trust no peer", which
    /// silently rewrites every URL this server advertises.
    pub fn from_config(entries: Option<&[String]>) -> Self {
        let Some(entries) = entries else {
            return Self::legacy_permissive();
        };
        let mut networks = Vec::with_capacity(entries.len());
        for entry in entries {
            match batlehub_config::schema::parse_trusted_proxies(std::slice::from_ref(entry)) {
                Ok(parsed) => networks.extend(parsed),
                Err(e) => tracing::warn!(error = %e, "ignoring malformed trusted_proxies entry"),
            }
        }
        Self::from_networks(networks)
    }

    /// True when an explicit list is configured (as opposed to legacy permissive).
    ///
    /// **Public for a reason that is not this module's.** [`Self::policy`] is
    /// private, so this is the only way to ask the question from outside — and
    /// `services::reload` asks it, to assert that a reload hands the app's
    /// *live* handle over rather than a detached copy. That test is the whole
    /// argument for the struct-literal shape `ConfigReloadParams` documents; a
    /// never-wired policy and a legitimately unconfigured one look identical
    /// here, which is exactly why the wiring has to be a compile error rather
    /// than a runtime check.
    ///
    /// Audited 2026-09-01: no production caller, and that is not a defect.
    pub fn is_configured(&self) -> bool {
        self.policy().is_some()
    }

    /// Resolve this policy against the request's TCP peer.
    pub fn verdict_for(&self, peer: Option<IpAddr>) -> PeerTrust {
        match self.policy() {
            None => PeerTrust::LegacyPermissive,
            Some(networks) => {
                if peer.is_some_and(|ip| networks.iter().any(|net| net.contains(&ip))) {
                    PeerTrust::Trusted
                } else {
                    PeerTrust::Untrusted
                }
            }
        }
    }
}

/// Per-request proxy-trust verdict, stored in the request extensions.
///
/// RFC 0001 sketches this as a `PeerTrusted(bool)`, but two booleans' worth of
/// state are actually needed: "absent" and "trusted" agree about the forwarded
/// host and scheme yet disagree about `X-Forwarded-For` (absent must keep
/// ignoring it, or adopting this type would silently make the client IP
/// spoofable for every deployment that has no list). Three variants say that
/// without a second field that can contradict the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerTrust {
    /// No `trusted_proxies` list configured anywhere. Forwarded host/scheme are
    /// believed (today's behaviour, and what keeps generated URLs stable for
    /// existing deployments); `X-Forwarded-For` is not.
    LegacyPermissive,
    /// A list is configured and the TCP peer falls inside it.
    Trusted,
    /// A list is configured and the TCP peer does not fall inside it — including
    /// the `trusted_proxies = []` case, where no peer ever does.
    Untrusted,
}

impl PeerTrust {
    /// Whether `Forwarded` / `X-Forwarded-Host` / `X-Forwarded-Proto` may decide
    /// the client-facing origin.
    pub fn honours_forwarded_origin(self) -> bool {
        !matches!(self, Self::Untrusted)
    }

    /// Whether `X-Forwarded-For` may decide the client IP.
    pub fn honours_forwarded_for(self) -> bool {
        matches!(self, Self::Trusted)
    }
}

/// The verdict for this request.
///
/// Normally the host-routing middleware has already computed and inserted it.
/// When it has not — an app that does not wrap that middleware, i.e. most unit
/// and integration tests — the policy is resolved from `app_data` instead, and
/// failing that the request is treated as legacy permissive. Both fallbacks
/// reproduce the pre-RFC behaviour rather than inventing a stricter one.
pub fn peer_trust(req: &HttpRequest) -> PeerTrust {
    if let Some(verdict) = req.extensions().get::<PeerTrust>().copied() {
        return verdict;
    }
    req.app_data::<web::Data<ProxyTrust>>()
        .map(|trust| trust.verdict_for(req.peer_addr().map(|a| a.ip())))
        .unwrap_or(PeerTrust::LegacyPermissive)
}

/// [`peer_trust`] for middleware, which holds a [`ServiceRequest`].
pub fn peer_trust_of_service(req: &ServiceRequest) -> PeerTrust {
    peer_trust(req.request())
}

/// The client-facing `(scheme, host)` of this request, honouring forwarded
/// headers only when the peer is trusted.
///
/// The host keeps whatever case and port the client sent — this is the origin to
/// render into URLs, not the routing key. Use [`normalise_host`] for lookups.
pub fn trusted_origin(req: &HttpRequest) -> (String, String) {
    let (scheme, host) = if peer_trust(req).honours_forwarded_origin() {
        // The one sanctioned use of `ConnectionInfo`'s forwarded-header readers
        // in the workspace — `clippy.toml` disallows them everywhere else so the
        // trust decision cannot be bypassed by reaching for them directly. Here
        // it is guarded: we already established the peer may set those headers.
        #[allow(clippy::disallowed_methods)]
        let conn = req.connection_info();
        #[allow(clippy::disallowed_methods)]
        let origin = (conn.scheme().to_owned(), conn.host().to_owned());
        origin
    } else {
        (connection_scheme(req).to_owned(), connection_host(req))
    };

    // Both halves are client-supplied on the branch above, and both are *echoed*
    // — see `MAX_HOST_LEN` for why that makes their length the security-relevant
    // property rather than a matter of taste.
    (bounded_scheme(req, scheme), bounded_host(req, host))
}

/// The longest string this server will render into a generated URL as the host.
///
/// A presentation-form domain name is at most 253 bytes (RFC 1035 §2.3.4), a
/// bracketed IPv6 literal at most 47, and `:65535` adds 6 — so 259 bytes is well
/// past anything a real ingress can put in `Host` or `X-Forwarded-Host` while
/// still being a hostname.
///
/// The bound matters because this host is *repeated*. `registry_public_base`
/// puts it in front of every self-referencing URL in a generated document, and
/// those documents carry one URL per version: a packument, a NuGet registration
/// index, a PyPI simple page. So a client's whole HTTP/1 head budget — 128 KiB,
/// `actix_http::h1::decoder::MAX_BUFFER_SIZE` — comes back multiplied by the
/// version count, after being built in full as a `serde_json::Value` first. A
/// 100 KiB `X-Forwarded-Host` against a 20-version package already returns 2 MB,
/// and real packuments run to thousands of versions.
///
/// This is the same amplification [`MAX_FORWARDED_HOPS`] exists to stop on
/// `X-Forwarded-For`, on the header that is actually echoed back. It applies to
/// the fallback path too, not just the forwarded one: `Host` is no less
/// client-supplied than `X-Forwarded-Host`, so the bound is applied to the
/// resolved origin rather than to one branch of it.
const MAX_HOST_LEN: usize = 259;

/// `host` when it is short enough to be a hostname, otherwise the server's own
/// configured host.
///
/// Length is the only thing rejected here. The host deliberately keeps whatever
/// case, port and syntax the client sent — that is [`trusted_origin`]'s contract,
/// and [`normalise_host`] is what turns it into a routing key — so this is a
/// bound, not a validator. Falling back to the configured host is what
/// [`connection_host`] and actix's own `ConnectionInfo` already do for a request
/// that carries no host at all, so it introduces no new state.
fn bounded_host(req: &HttpRequest, host: String) -> String {
    if host.len() <= MAX_HOST_LEN {
        host
    } else {
        req.app_config().host().to_owned()
    }
}

/// `scheme` when it is one this server can actually be reached over, otherwise
/// the scheme of the underlying connection.
///
/// `X-Forwarded-Proto` lands in every generated URL exactly as the host does and
/// needs the same bound. `http` and `https` are the only two values that mean
/// anything here, so accepting just those is both tighter than a length cap and
/// simpler to state. Case is preserved rather than normalised: a proxy that
/// sends `HTTPS` keeps producing the URLs it produces today.
fn bounded_scheme(req: &HttpRequest, scheme: String) -> String {
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        scheme
    } else {
        connection_scheme(req).to_owned()
    }
}

/// The scheme of the underlying connection, ignoring `X-Forwarded-Proto`.
fn connection_scheme(req: &HttpRequest) -> &str {
    req.uri().scheme_str().unwrap_or("http")
}

/// The `Host` header (or HTTP/2 `:authority`), ignoring `X-Forwarded-Host`.
///
/// Falls back to the server's own configured host name, which is also what
/// actix's `ConnectionInfo` does when a request carries no host at all.
fn connection_host(req: &HttpRequest) -> String {
    req.headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.as_str().to_owned()))
        .unwrap_or_else(|| req.app_config().host().to_owned())
}

/// The normalised host this request should be routed on.
///
/// Normalisation lives in `batlehub_config::schema::normalise_host` so the host
/// that validates in the config is exactly the host that routes here.
pub fn routing_host(req: &HttpRequest) -> String {
    normalise_host(&trusted_origin(req).1)
}

/// The client IP: the right-most `X-Forwarded-For` entry that is not itself one
/// of the configured proxies when the peer is trusted, otherwise the TCP peer
/// address.
pub fn client_ip(req: &HttpRequest, trust: PeerTrust) -> String {
    if trust.honours_forwarded_for() {
        if let Some(ip) = forwarded_client_ip(req) {
            return ip.to_string();
        }
    }
    match req.peer_addr() {
        Some(addr) => addr.ip().to_string(),
        None => "unknown".to_owned(),
    }
}

/// How many trailing `X-Forwarded-For` entries [`forwarded_client_ip`] keeps.
///
/// The walk only ever reads from the right, and it stops at the first hop that is
/// not one of our own proxies — so this bounds how many *consecutive configured
/// proxies* may sit at the end of the chain before the answer is given up on, not
/// how long a chain may be. Real deployments put one to three proxies there; 64 is
/// far enough past that to be unreachable in practice while keeping the retained
/// window at about a kilobyte.
///
/// Truncation degrades in the safe direction. Dropping the left-hand entries can
/// only ever discard values the *client* chose — those are exactly what the
/// right-to-left walk exists to ignore — and a chain whose last 64 entries are all
/// configured proxies yields `None`, which falls back to the TCP peer address.
/// That is the same fallback the all-hops-are-ours case already took, and it is
/// never a value an attacker can steer.
const MAX_FORWARDED_HOPS: usize = 64;

/// Walk `X-Forwarded-For` right to left and return the first hop that is not
/// itself a configured proxy.
///
/// Each hop *appends* the peer address it observed, so the right-most entry was
/// written by the proxy we are talking to and the left-most is whatever the
/// original client chose to send. Reading the left side therefore hands every
/// client the IP that fail2ban counts violations against and that the anonymous
/// rate-limit bucket is keyed on — enough to dodge one's own ban, or to get a
/// third party blocked by naming them. Walking from the right and skipping the
/// hops we recognise as our own proxies yields the address the last proxy we
/// trust actually saw, which is the first value the client could not choose.
///
/// The trusted networks come from the [`ProxyTrust`] registered as `app_data`,
/// the same handle [`peer_trust`] falls back to; a reload reaches it through the
/// shared lock. When none is registered no hop can be recognised as a proxy, so
/// the right-most entry — the one our peer wrote — is used.
fn forwarded_client_ip(req: &HttpRequest) -> Option<IpAddr> {
    let networks = req
        .app_data::<web::Data<ProxyTrust>>()
        .and_then(|trust| trust.policy());
    let is_proxy = |ip: &IpAddr| {
        networks
            .as_ref()
            .is_some_and(|nets| nets.iter().any(|net| net.contains(ip)))
    };

    // Multiple header lines are one logical list, joined in the order received.
    //
    // Only the right-hand end is ever read, so only the right-hand end is kept:
    // this retains the last `MAX_FORWARDED_HOPS` entries in a fixed-size ring and
    // discards the rest as it goes. Collecting the whole list into a `Vec` instead
    // let a client turn its 128 KiB header budget into a much larger allocation —
    // the shortest possible entry is two bytes (`1,`), so a full HTTP/1 head buys
    // ~65 K hops, and at 16 bytes per `&str` that is a ~1 MiB `Vec` from a 128 KiB
    // request, once per request. Bounded and freed immediately, so this was an
    // amplification factor rather than a leak, but there is no reason to pay it.
    let mut tail: VecDeque<&str> = VecDeque::with_capacity(MAX_FORWARDED_HOPS);
    for hop in req
        .headers()
        .get_all("x-forwarded-for")
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
    {
        if tail.len() == MAX_FORWARDED_HOPS {
            tail.pop_front();
        }
        tail.push_back(hop);
    }

    for hop in tail.iter().rev() {
        // An entry that does not parse ends the walk rather than being skipped:
        // an unrecognisable hop cannot be shown to be one of our proxies, and
        // stepping over it would resume trusting values from further left —
        // exactly the entries the client controls. Falling back to the peer
        // address instead degrades to "everyone behind this proxy shares a
        // bucket", which is wrong but not attacker-steerable.
        let ip = parse_forwarded_hop(hop.trim())?;
        if !is_proxy(&ip) {
            return Some(ip);
        }
    }
    None
}

/// Parse one `X-Forwarded-For` entry, tolerating the `ip:port` and `[ipv6]:port`
/// forms some proxies emit.
fn parse_forwarded_hop(hop: &str) -> Option<IpAddr> {
    hop.parse::<IpAddr>()
        .ok()
        .or_else(|| hop.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;

    use super::*;

    fn nets(entries: &[&str]) -> ProxyTrust {
        let owned: Vec<String> = entries.iter().map(|s| (*s).to_owned()).collect();
        ProxyTrust::from_config(Some(&owned))
    }

    // ── verdict ───────────────────────────────────────────────────────────────

    #[test]
    fn absent_list_is_legacy_permissive_for_every_peer() {
        let trust = ProxyTrust::from_config(None);
        assert!(!trust.is_configured());
        assert_eq!(
            trust.verdict_for(Some("203.0.113.7".parse().unwrap())),
            PeerTrust::LegacyPermissive
        );
        assert_eq!(trust.verdict_for(None), PeerTrust::LegacyPermissive);
    }

    #[test]
    fn empty_list_trusts_nobody() {
        let trust = ProxyTrust::from_config(Some(&[]));
        assert!(trust.is_configured());
        assert_eq!(
            trust.verdict_for(Some("10.42.0.1".parse().unwrap())),
            PeerTrust::Untrusted
        );
    }

    #[test]
    fn cidr_contains_peer() {
        let trust = nets(&["10.42.0.0/16"]);
        assert_eq!(
            trust.verdict_for(Some("10.42.7.9".parse().unwrap())),
            PeerTrust::Trusted
        );
    }

    #[test]
    fn peer_just_outside_cidr_is_untrusted() {
        let trust = nets(&["10.42.0.0/16"]);
        assert_eq!(
            trust.verdict_for(Some("10.43.0.1".parse().unwrap())),
            PeerTrust::Untrusted
        );
    }

    #[test]
    fn bare_address_matches_exactly_and_nothing_else() {
        let trust = nets(&["192.168.1.10"]);
        assert_eq!(
            trust.verdict_for(Some("192.168.1.10".parse().unwrap())),
            PeerTrust::Trusted
        );
        assert_eq!(
            trust.verdict_for(Some("192.168.1.11".parse().unwrap())),
            PeerTrust::Untrusted
        );
    }

    #[test]
    fn ipv6_cidr_and_bare_address() {
        let trust = nets(&["2001:db8::/32", "::1"]);
        assert_eq!(
            trust.verdict_for(Some("2001:db8::dead:beef".parse().unwrap())),
            PeerTrust::Trusted
        );
        assert_eq!(
            trust.verdict_for(Some("::1".parse().unwrap())),
            PeerTrust::Trusted
        );
        assert_eq!(
            trust.verdict_for(Some("2001:db9::1".parse().unwrap())),
            PeerTrust::Untrusted
        );
    }

    #[test]
    fn a_malformed_entry_is_dropped_without_discarding_the_valid_ones() {
        // Only the deprecated `[ip_blocking].trusted_proxies` can reach here with
        // a bad entry; dropping the whole list would silently retarget every URL
        // this server advertises.
        let trust = nets(&["10.0.0.5", "ingress.internal"]);
        assert_eq!(
            trust.verdict_for(Some("10.0.0.5".parse().unwrap())),
            PeerTrust::Trusted
        );
        assert_eq!(
            trust.verdict_for(Some("10.0.0.6".parse().unwrap())),
            PeerTrust::Untrusted
        );
    }

    #[test]
    fn replace_from_updates_every_clone() {
        // The middleware and `app_data` each hold a clone; a reload has to reach
        // both, or routing keeps running under the startup policy.
        let live = ProxyTrust::legacy_permissive();
        let held_by_middleware = live.clone();
        assert_eq!(
            held_by_middleware.verdict_for(Some("203.0.113.7".parse().unwrap())),
            PeerTrust::LegacyPermissive
        );

        live.replace_from(&nets(&["10.42.0.0/16"]));

        assert!(held_by_middleware.is_configured());
        assert_eq!(
            held_by_middleware.verdict_for(Some("203.0.113.7".parse().unwrap())),
            PeerTrust::Untrusted
        );
        assert_eq!(
            held_by_middleware.verdict_for(Some("10.42.7.1".parse().unwrap())),
            PeerTrust::Trusted
        );
    }

    #[test]
    fn missing_peer_addr_is_untrusted_when_a_list_exists() {
        assert_eq!(
            nets(&["10.0.0.0/8"]).verdict_for(None),
            PeerTrust::Untrusted
        );
    }

    // ── trusted_origin ────────────────────────────────────────────────────────

    fn origin_with(trust: PeerTrust, build: TestRequest) -> (String, String) {
        let req = build.to_http_request();
        req.extensions_mut().insert(trust);
        trusted_origin(&req)
    }

    #[test]
    fn forwarded_headers_decide_origin_for_a_trusted_peer() {
        let (scheme, host) = origin_with(
            PeerTrust::Trusted,
            TestRequest::default()
                .insert_header(("host", "internal.svc:8080"))
                .insert_header(("x-forwarded-host", "npm.acme.io"))
                .insert_header(("x-forwarded-proto", "https")),
        );
        assert_eq!(scheme, "https");
        assert_eq!(host, "npm.acme.io");
    }

    #[test]
    fn forwarded_headers_are_ignored_for_an_untrusted_peer() {
        let (scheme, host) = origin_with(
            PeerTrust::Untrusted,
            TestRequest::default()
                .insert_header(("host", "internal.svc:8080"))
                .insert_header(("x-forwarded-host", "npm.acme.io"))
                .insert_header(("x-forwarded-proto", "https")),
        );
        assert_eq!(scheme, "http", "X-Forwarded-Proto must not be believed");
        assert_eq!(host, "internal.svc:8080");
    }

    #[test]
    fn legacy_permissive_reproduces_todays_unconditional_trust() {
        let (scheme, host) = origin_with(
            PeerTrust::LegacyPermissive,
            TestRequest::default()
                .insert_header(("host", "internal.svc:8080"))
                .insert_header(("x-forwarded-host", "npm.acme.io"))
                .insert_header(("x-forwarded-proto", "https")),
        );
        assert_eq!(scheme, "https");
        assert_eq!(host, "npm.acme.io");
    }

    #[test]
    fn origin_without_any_extension_defaults_to_legacy_permissive() {
        let req = TestRequest::default()
            .insert_header(("x-forwarded-host", "npm.acme.io"))
            .to_http_request();
        assert_eq!(trusted_origin(&req).1, "npm.acme.io");
    }

    #[test]
    fn origin_falls_back_to_the_app_data_policy_when_no_middleware_ran() {
        let req = TestRequest::default()
            .app_data(web::Data::new(ProxyTrust::from_networks(Vec::new())))
            .insert_header(("host", "internal.svc"))
            .insert_header(("x-forwarded-host", "npm.acme.io"))
            .to_http_request();
        assert_eq!(
            trusted_origin(&req).1,
            "internal.svc",
            "the registered policy must apply even without the middleware"
        );
    }

    // ── origin bounds ─────────────────────────────────────────────────────────

    #[test]
    fn an_over_long_forwarded_host_falls_back_to_the_configured_host() {
        let huge = "a".repeat(100 * 1024);
        let (_, host) = origin_with(
            PeerTrust::Trusted,
            TestRequest::default()
                .insert_header(("host", "internal.svc:8080"))
                .insert_header(("x-forwarded-host", huge.as_str())),
        );
        assert!(
            host.len() <= MAX_HOST_LEN,
            "an echoed host must stay bounded, got {} bytes",
            host.len()
        );
        assert!(
            !host.contains("aaaa"),
            "the client's bytes must not survive"
        );
    }

    #[test]
    fn an_over_long_plain_host_header_is_bounded_too() {
        // The untrusted branch reads `Host`, which the client also writes.
        let huge = "a".repeat(100 * 1024);
        let (_, host) = origin_with(
            PeerTrust::Untrusted,
            TestRequest::default().insert_header(("host", huge.as_str())),
        );
        assert!(host.len() <= MAX_HOST_LEN, "got {} bytes", host.len());
    }

    #[test]
    fn a_host_at_the_limit_is_kept_verbatim() {
        let long_but_legal = format!("{}.acme.io:8443", "a".repeat(MAX_HOST_LEN - 13));
        assert_eq!(long_but_legal.len(), MAX_HOST_LEN);
        let (_, host) = origin_with(
            PeerTrust::Trusted,
            TestRequest::default().insert_header(("x-forwarded-host", long_but_legal.as_str())),
        );
        assert_eq!(host, long_but_legal, "a real hostname must pass untouched");
    }

    #[test]
    fn an_over_long_forwarded_proto_falls_back_to_the_connection_scheme() {
        let huge = "h".repeat(100 * 1024);
        let (scheme, _) = origin_with(
            PeerTrust::Trusted,
            TestRequest::default()
                .insert_header(("host", "npm.acme.io"))
                .insert_header(("x-forwarded-proto", huge.as_str())),
        );
        assert_eq!(scheme, "http");
    }

    #[test]
    fn a_forwarded_proto_keeps_its_case() {
        let (scheme, _) = origin_with(
            PeerTrust::Trusted,
            TestRequest::default()
                .insert_header(("host", "npm.acme.io"))
                .insert_header(("x-forwarded-proto", "HTTPS")),
        );
        assert_eq!(
            scheme, "HTTPS",
            "bounding the scheme must not rewrite what a proxy sends"
        );
    }

    #[test]
    fn the_forwarded_header_is_bounded_on_both_halves() {
        // RFC 7239's `Forwarded` feeds the same two fields as the `X-` headers.
        let huge = "a".repeat(100 * 1024);
        let (scheme, host) = origin_with(
            PeerTrust::Trusted,
            TestRequest::default()
                .insert_header(("host", "internal.svc"))
                .insert_header(("forwarded", format!("host={huge};proto=gopher").as_str())),
        );
        assert!(host.len() <= MAX_HOST_LEN, "got {} bytes", host.len());
        assert_eq!(scheme, "http", "an implausible scheme must not be echoed");
    }

    // ── routing_host ──────────────────────────────────────────────────────────

    #[test]
    fn routing_host_normalises_the_trusted_origin() {
        let req = TestRequest::default()
            .insert_header(("host", "NPM.Acme.io:8443"))
            .to_http_request();
        req.extensions_mut().insert(PeerTrust::Untrusted);
        assert_eq!(routing_host(&req), "npm.acme.io");
    }

    // ── client_ip ─────────────────────────────────────────────────────────────

    #[test]
    fn client_ip_uses_xff_only_for_a_trusted_peer() {
        let build = || {
            TestRequest::default()
                .peer_addr("10.0.0.1:1234".parse().unwrap())
                .insert_header(("x-forwarded-for", "203.0.113.5, 172.16.0.1"))
                .to_http_request()
        };
        assert_eq!(
            client_ip(&build(), PeerTrust::Trusted),
            "172.16.0.1",
            "the right-most hop is the one our own proxy wrote"
        );
        assert_eq!(client_ip(&build(), PeerTrust::Untrusted), "10.0.0.1");
        assert_eq!(
            client_ip(&build(), PeerTrust::LegacyPermissive),
            "10.0.0.1",
            "an unconfigured deployment must keep ignoring X-Forwarded-For"
        );
    }

    /// A request whose `PeerTrust` is `Trusted` and whose `app_data` carries the
    /// list those proxies were configured from — the shape a real deployment has.
    fn trusted_req(proxies: &[&str], xff: &str) -> HttpRequest {
        let req = TestRequest::default()
            .app_data(web::Data::new(nets(proxies)))
            .peer_addr("10.42.0.1:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", xff.to_owned()))
            .to_http_request();
        req.extensions_mut().insert(PeerTrust::Trusted);
        req
    }

    #[test]
    fn client_ip_skips_our_own_proxies_walking_right_to_left() {
        // client → edge (10.42.0.2) → inner (10.42.0.1) → here.
        let req = trusted_req(&["10.42.0.0/16"], "203.0.113.5, 198.51.100.9, 10.42.0.2");
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "198.51.100.9");
    }

    #[test]
    fn client_ip_ignores_a_client_supplied_prefix() {
        // The client prepended a chain of its own before reaching the proxy; only
        // the entry the proxy appended may decide the ban/rate-limit key.
        let req = trusted_req(&["10.42.0.0/16"], "1.1.1.1, 2.2.2.2, 203.0.113.77");
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "203.0.113.77");
    }

    #[test]
    fn client_ip_accepts_a_hop_carrying_a_port() {
        let req = trusted_req(&["10.42.0.0/16"], "[2001:db8::5]:443");
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "2001:db8::5");
        let req = trusted_req(&["10.42.0.0/16"], "203.0.113.5:51234");
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "203.0.113.5");
    }

    #[test]
    fn client_ip_falls_back_to_peer_on_an_unparseable_hop() {
        // Stepping over `unknown` would resume trusting the client-controlled
        // entries to its left.
        let req = trusted_req(&["10.42.0.0/16"], "203.0.113.5, unknown, 10.42.0.2");
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "10.42.0.1");
    }

    #[test]
    fn client_ip_falls_back_to_peer_when_every_hop_is_our_own_proxy() {
        let req = trusted_req(&["10.42.0.0/16"], "10.42.0.3, 10.42.0.2");
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "10.42.0.1");
    }

    #[test]
    fn client_ip_joins_repeated_xff_headers_in_order() {
        let req = TestRequest::default()
            .app_data(web::Data::new(nets(&["10.42.0.0/16"])))
            .peer_addr("10.42.0.1:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "1.1.1.1"))
            .append_header(("x-forwarded-for", "203.0.113.5, 10.42.0.2"))
            .to_http_request();
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "203.0.113.5");
    }

    #[test]
    fn client_ip_reads_the_right_end_of_an_oversized_chain() {
        // The shape a flood actually takes: the client prepends thousands of
        // entries, the proxy appends the one address it observed. The answer is at
        // the right end, so the retained window finds it on the first step and the
        // junk is never examined.
        let mut xff = "1.1.1.1, ".repeat(10_000);
        xff.push_str("203.0.113.77");
        let req = trusted_req(&["10.42.0.0/16"], &xff);
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "203.0.113.77");
    }

    #[test]
    fn client_ip_falls_back_to_peer_when_the_window_is_all_our_own_proxies() {
        // More consecutive configured proxies at the tail than the window holds,
        // so the walk never reaches the client value to their left. Truncation has
        // to degrade to the peer address — the same answer the all-hops-are-ours
        // case gives — and never to the attacker-chosen entry.
        let mut xff = String::from("203.0.113.5, ");
        xff.push_str(&"10.42.0.2, ".repeat(MAX_FORWARDED_HOPS + 8));
        let xff = xff.trim_end_matches(", ").to_owned();
        let req = trusted_req(&["10.42.0.0/16"], &xff);
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "10.42.0.1");
    }

    #[test]
    fn client_ip_is_unaffected_by_the_bound_for_a_realistic_chain() {
        // A chain comfortably inside the window behaves exactly as before the
        // bound existed: skip our own proxies, return the first hop that is not.
        let mut xff = String::from("203.0.113.9, ");
        xff.push_str(&"10.42.0.2, ".repeat(MAX_FORWARDED_HOPS - 8));
        let xff = xff.trim_end_matches(", ").to_owned();
        let req = trusted_req(&["10.42.0.0/16"], &xff);
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "203.0.113.9");
    }

    #[test]
    fn client_ip_falls_back_to_peer_when_xff_is_blank() {
        let req = TestRequest::default()
            .peer_addr("10.0.0.1:1234".parse().unwrap())
            .insert_header(("x-forwarded-for", "  "))
            .to_http_request();
        assert_eq!(client_ip(&req, PeerTrust::Trusted), "10.0.0.1");
    }

    #[test]
    fn client_ip_is_unknown_without_a_peer() {
        let req = TestRequest::default().to_http_request();
        assert_eq!(client_ip(&req, PeerTrust::Untrusted), "unknown");
    }
}
