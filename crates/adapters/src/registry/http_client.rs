use batlehub_core::error::CoreError;
use batlehub_core::services::readme::image::{image_content_type, FetchedImage};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};

/// Options that control how a registry client's upstream HTTP client is built.
#[derive(Debug, Default, Clone)]
pub struct UpstreamHttpOptions {
    /// `Authorization: Bearer <token>` injected as a default header.
    pub bearer_token: Option<String>,
    /// HTTP Basic auth credentials stored for per-request injection (reqwest
    /// does not support per-client basic auth).
    pub basic_auth: Option<(String, String)>,
    /// Arbitrary header name/value injected as a default header (e.g. `X-API-Key`).
    pub custom_header: Option<(String, String)>,
    /// Path to a PEM-encoded CA certificate to add as a trusted root.
    pub ca_cert_path: Option<String>,
    /// Override URL for the upstream search API used by the Package Explorer.
    ///
    /// - `None` — use the registry type's built-in default (e.g. `search.maven.org`
    ///   for Maven, `packagist.org` for Composer).
    /// - `Some(url)` — use this URL as the search base.
    /// - `Some("")` — disable upstream search for this registry entirely.
    pub search_url: Option<String>,
    /// HTTP/SOCKS proxy URL for all upstream requests (e.g. `http://proxy:3128`).
    pub proxy_url: Option<String>,
    /// Proxy Basic-auth username (used with `proxy_url`).
    pub proxy_username: Option<String>,
    /// Proxy Basic-auth password (used with `proxy_url`).
    pub proxy_password: Option<String>,
    /// Comma-separated hosts/domains to bypass the proxy for.
    pub no_proxy: Option<String>,
}

/// Applies TLS and proxy options from `opts` to `builder`, returning the
/// modified builder so callers can add their own default headers before `.build()`.
pub fn apply_upstream_tls(
    mut builder: reqwest::ClientBuilder,
    opts: &UpstreamHttpOptions,
) -> anyhow::Result<reqwest::ClientBuilder> {
    // reqwest has NO default timeouts, so a slow or unresponsive upstream would
    // otherwise pin an actix worker and a pooled connection indefinitely — a
    // resource-exhaustion vector on the proxy's hot path. We deliberately avoid
    // a total `.timeout()` here: this client also streams large artifacts, and a
    // whole-request cap would abort legitimate long transfers. Instead we bound
    // connection establishment and per-read inactivity, which kills a stalled
    // upstream without capping how long a genuinely large (but progressing)
    // download may take.
    builder = builder
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60));
    if let Some(ref path) = opts.ca_cert_path {
        let pem =
            std::fs::read(path).map_err(|e| anyhow::anyhow!("reading CA cert '{}': {e}", path))?;
        let cert = reqwest::Certificate::from_pem(&pem)
            .map_err(|e| anyhow::anyhow!("parsing CA cert '{}': {e}", path))?;
        builder = builder.add_root_certificate(cert);
    }
    if let Some(ref proxy_url) = opts.proxy_url {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|e| anyhow::anyhow!("invalid proxy URL '{}': {e}", proxy_url))?;
        let proxy = match (&opts.proxy_username, &opts.proxy_password) {
            (Some(u), Some(p)) => proxy.basic_auth(u, p),
            _ => proxy,
        };
        let proxy = proxy.no_proxy(
            opts.no_proxy
                .as_deref()
                .and_then(reqwest::NoProxy::from_string),
        );
        builder = builder.proxy(proxy);
    }
    Ok(builder)
}

/// Returns a `HeaderMap` containing any Bearer / custom-header auth entries
/// from `opts`.  Callers can merge this into their own header map before
/// passing it to `.default_headers()`.
pub fn upstream_auth_headers(opts: &UpstreamHttpOptions) -> anyhow::Result<HeaderMap> {
    let mut headers = HeaderMap::new();

    if let Some(ref tok) = opts.bearer_token {
        let value: HeaderValue = format!("Bearer {tok}")
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid bearer token header value"))?;
        headers.insert(AUTHORIZATION, value);
    }

    if let Some((ref name, ref value)) = opts.custom_header {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid custom header name '{name}'"))?;
        let header_value: HeaderValue = value
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid custom header value for '{name}'"))?;
        headers.insert(header_name, header_value);
    }

    Ok(headers)
}

/// Convenience wrapper: applies TLS options, injects auth headers as default
/// headers, and calls `.build()`.  Use this for clients with no registry-specific
/// default headers.  For clients that set their own headers (e.g. GitHub), use
/// `apply_upstream_tls` + `upstream_auth_headers` directly.
pub fn apply_upstream_options(
    builder: reqwest::ClientBuilder,
    opts: &UpstreamHttpOptions,
) -> anyhow::Result<reqwest::Client> {
    let mut builder = apply_upstream_tls(builder, opts)?;
    let auth_headers = upstream_auth_headers(opts)?;
    if !auth_headers.is_empty() {
        builder = builder.default_headers(auth_headers);
    }
    Ok(builder.build()?)
}

/// Builds a `reqwest::Client` with the shared `batlehub/0.1` user agent, an
/// optional redirect-following limit, and `opts`'s TLS/proxy/auth settings
/// applied — the common preamble every registry adapter's constructor repeats.
/// Adapters with their own extra default headers (e.g. GitHub) should build
/// their own `reqwest::ClientBuilder` and call `apply_upstream_options` directly
/// instead of this helper.
pub fn new_http_client(
    redirect_limit: Option<usize>,
    opts: &UpstreamHttpOptions,
) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().user_agent("batlehub/0.1");
    if let Some(limit) = redirect_limit {
        builder = builder.redirect(reqwest::redirect::Policy::limited(limit));
    }
    apply_upstream_options(builder, opts)
}

/// The client pair a manually-followed redirect chain needs.
///
/// [`super::ssrf::fetch_following_redirects`] decides *per hop* which of the two
/// to send a request with — the credentialed one while the URL is still on the
/// configured upstream's own origin, the credential-free one once a `Location`
/// takes it off — so both have to exist, and **both must have reqwest's own
/// redirect following disabled**: a policy that follows a `302` itself does so
/// before the per-hop [`super::ssrf::ensure_public_url`] check can run, which is
/// the whole thing the guard exists to prevent.
///
/// `plain` keeps the TLS and proxy settings (a private CA and an egress proxy
/// are properties of this network, not credentials) and drops only the auth
/// headers `upstream_auth_headers` would have injected.
pub fn no_redirect_client_pair(
    opts: &UpstreamHttpOptions,
) -> Result<(reqwest::Client, reqwest::Client), CoreError> {
    let base = || {
        reqwest::Client::builder()
            .user_agent("batlehub/0.1")
            .redirect(reqwest::redirect::Policy::none())
    };
    let credentialed = apply_upstream_options(base(), opts).map_err(CoreError::Other)?;
    let plain = apply_upstream_tls(base(), opts)
        .map_err(CoreError::Other)?
        .build()
        .map_err(|e| CoreError::Other(e.into()))?;
    Ok((credentialed, plain))
}

/// Builds a GET request, applying HTTP Basic auth credentials if configured.
pub fn basic_auth_get(
    http: &reqwest::Client,
    basic_auth: &Option<(String, String)>,
    url: &str,
) -> reqwest::RequestBuilder {
    let rb = http.get(url);
    match basic_auth {
        Some((u, p)) => rb.basic_auth(u, Some(p)),
        None => rb,
    }
}

/// Extracts the `Cache-Control` response header as an owned string, if present.
pub fn cache_control(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Parse an HTTP `Link` header (RFC 8288) and return the URL for `rel="next"`,
/// if present. Used to paginate Gitea/Forgejo and GitLab list endpoints.
pub fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    for part in link.split(',') {
        let mut url = None;
        let mut is_next = false;
        for seg in part.split(';') {
            let s = seg.trim();
            if s.starts_with('<') && s.ends_with('>') {
                url = Some(s[1..s.len() - 1].to_owned());
            } else if s == r#"rel="next""# {
                is_next = true;
            }
        }
        if is_next {
            return url;
        }
    }
    None
}

/// Parse an HTTP `Last-Modified` header value (RFC 7231 / RFC 2822) into a `DateTime<Utc>`.
pub fn parse_http_date(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc2822(s.trim())
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Validates that `url` shares its scheme and host with `base_url`, rejecting
/// any cross-origin URL.
///
/// Some upstream registries (npm, OpenVSX) return an absolute download URL in
/// their own metadata response rather than a path relative to the configured
/// upstream. A malicious or compromised upstream could point that URL at an
/// internal address (SSRF), so callers that fetch such a URL must check it
/// against the configured host first instead of trusting it verbatim.
/// Map a `reqwest::Error` to `CoreError::Registry`. Shared by every registry
/// adapter's `.map_err(...)` call site (both on `Result` and on
/// `Stream::map_err`, e.g. `bytes_stream()`), which otherwise repeat this
/// closure verbatim at each fallible HTTP step.
pub fn to_registry_error(e: reqwest::Error) -> CoreError {
    CoreError::Registry(e.to_string())
}

/// GET an upstream *version-listing document* and return it as JSON.
///
/// Shared by every adapter's `fetch_version_document`, because they all need
/// the same three things and get them wrong in different ways otherwise: a
/// `404` must become [`CoreError::NotFound`] (the handler turns that into a
/// hybrid-mode fall-through, where a generic `Registry` error becomes a `502`),
/// any other non-success must carry the upstream status, and a body that does
/// not parse must fail rather than reach a filter as `null`.
///
/// `what` names the document in errors — "NuGet flat index for 'newtonsoft'".
pub async fn fetch_json_document(
    req: reqwest::RequestBuilder,
    what: &str,
) -> Result<batlehub_core::ports::VersionDocument, CoreError> {
    let body = fetch_document_body(req, what).await?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| CoreError::Registry(format!("parsing {what}: {e}")))?;
    Ok(batlehub_core::ports::VersionDocument::json(value))
}

/// The releases listing of a forge repository, as the client asked for it.
///
/// GitHub, Forgejo and GitLab answer `Versions` and nothing else, with the same
/// guard and the same two error strings; only the URL differs between them.
/// `kind_name` is the registry type as `RegistryClient::registry_type` spells
/// it, `display` the forge's own capitalisation for the fetch error.
///
/// Fetched fresh rather than paged through a typed `fetch_all_releases`: the
/// listing filter rewrites the document the client asked for, so it has to be
/// *that* document rather than a re-serialisation of a typed subset.
pub async fn fetch_release_listing(
    req: reqwest::RequestBuilder,
    kind: batlehub_core::ports::DocumentKind,
    kind_name: &str,
    display: &str,
    package: &str,
) -> Result<batlehub_core::ports::VersionDocument, CoreError> {
    if kind != batlehub_core::ports::DocumentKind::Versions {
        return Err(CoreError::NotSupported(format!(
            "{kind_name} has no '{kind}' listing document"
        )));
    }
    fetch_json_document(req, &format!("{display} releases for '{package}'")).await
}

/// [`fetch_json_document`] for the protocols whose listing is not JSON —
/// `maven-metadata.xml`, a PyPI simple page, Go's `@v/list`, cargo's NDJSON
/// sparse index.
///
/// `content_type` is what this proxy will send back, and is stated by the
/// caller rather than echoed from upstream: an upstream that mislabels its own
/// `maven-metadata.xml` as `text/plain` must not make this proxy mislabel it
/// too, and the client that asked knows what it wants.
pub async fn fetch_text_document(
    req: reqwest::RequestBuilder,
    what: &str,
    content_type: &str,
) -> Result<batlehub_core::ports::VersionDocument, CoreError> {
    let body = fetch_document_body(req, what).await?;
    let text = String::from_utf8(body.to_vec())
        .map_err(|e| CoreError::Registry(format!("{what} is not valid UTF-8: {e}")))?;
    Ok(batlehub_core::ports::VersionDocument::text(
        content_type,
        text,
    ))
}

async fn fetch_document_body(
    req: reqwest::RequestBuilder,
    what: &str,
) -> Result<bytes::Bytes, CoreError> {
    let resp = req
        .send()
        .await
        .map_err(|e| CoreError::Registry(format!("{what} request failed: {e}")))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(CoreError::NotFound(format!("{what} not found upstream")));
    }
    if !resp.status().is_success() {
        return Err(CoreError::Registry(format!(
            "{what} returned {} from upstream",
            resp.status()
        )));
    }

    resp.bytes()
        .await
        .map_err(|e| CoreError::Registry(format!("reading {what}: {e}")))
}

/// Read a linked README, same-origin checked and bounded.
///
/// The guards RFC 0007 §7.4 requires of every linked-README fetch, in one place
/// so the two kinds that have one cannot implement them differently:
///
/// - [`ensure_linked_origin`] against the registry's own base URL, the guard
///   `npm.rs` already applies to tarball URLs, so a compromised or misconfigured
///   upstream cannot point BatleHub at an internal host — checked on the URL
///   asked for **and on the one that answered**. `extra_host_suffixes` widens
///   that origin for a caller whose protocol *specifies* a second host — see
///   [`ensure_linked_origin`];
/// - the redirect chain walked by [`super::ssrf::fetch_following_redirects`]
///   rather than by reqwest, which is what makes the two guarantees above real
///   rather than retrospective. Checked only after the fact — the shape this had
///   while the callers' clients carried `redirect::Policy::limited(10)` — an
///   upstream answering its own README URL with
///   `302 Location: http://169.254.169.254/…` still had that address *dialled*
///   (only the stored body was refused), and reqwest, which strips
///   `Authorization` across hosts but not a configured `custom_header`
///   (`PRIVATE-TOKEN`, `X-API-Key`), had already handed the operator's token to
///   whatever host the `Location` named. Now every hop is validated before it is
///   dialled, and **the credentials stop at the configured origin**: they still
///   travel there, because a linked README is a request to the registry the
///   operator configured them for and an anonymous one is the one that gets
///   rate-limited, and they travel nowhere else;
/// - the body read **incrementally** to `max_bytes` rather than `bytes()`-then-
///   truncate, so an upstream cannot make this buffer a gigabyte to keep 256 KiB
///   of it;
/// - the response `Content-Type` ignored entirely — the caller states the format
///   from the protocol, and this returns text.
///
/// `credentialed`/`plain` are the pair [`no_redirect_client_pair`] builds; both
/// must have reqwest's redirect following disabled.
///
/// `Ok(None)` for a `404`: an extension whose README asset has gone is a fact
/// about that extension, not a failure worth logging as one.
pub async fn fetch_linked_text(
    credentialed: &reqwest::Client,
    plain: &reqwest::Client,
    basic_auth: &Option<(String, String)>,
    url: &str,
    base_url: &str,
    extra_host_suffixes: &[&str],
    max_bytes: usize,
) -> Result<Option<String>, CoreError> {
    use futures::StreamExt;

    ensure_linked_origin(url, base_url, extra_host_suffixes)?;
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CoreError::Registry(format!("invalid readme URL '{url}': {e}")))?;

    let resp =
        super::ssrf::fetch_following_redirects(credentialed, plain, basic_auth, base_url, parsed)
            .await?;

    // The origin that actually answered, which is not necessarily the one asked.
    // Every hop was already SSRF-checked and left the credentials behind the
    // moment it went off-origin; this is the narrower question of which hosts a
    // *stored document* may be read from, and the answer is the same list the
    // URL in the metadata had to be on.
    ensure_linked_origin(resp.url().as_str(), base_url, extra_host_suffixes)?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(CoreError::Registry(format!(
            "readme returned {} from upstream",
            resp.status()
        )));
    }

    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CoreError::Registry(format!("reading readme: {e}")))?;
        let remaining = max_bytes.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() >= max_bytes {
            break;
        }
    }
    // "The cap stopped this read", not "a chunk overran the cap". Tracking the
    // overrun instead missed the case where the chunk that fills the buffer ends
    // exactly on the boundary — `chunk.len() > remaining` is false there, and
    // that is not rare: a power-of-two cap and the uniform chunks a TLS stream
    // delivers line up on it routinely. A document cut mid-character then went
    // untrimmed and was reported as *no README* rather than as a long one, which
    // is the failure the trim below exists to prevent.
    let cut = body.len() >= max_bytes;

    // The cap counts bytes, and a character is not one byte. Cutting at
    // `max_bytes` can land inside a multi-byte sequence — one emoji, one
    // accented letter, any CJK character at the boundary — and the decode below
    // would then answer `None` for a README that is perfectly valid and merely
    // long, while the log blamed the upstream for sending bad bytes.
    //
    // `error_len() == None` is precisely "the input ended mid-sequence", the
    // only breakage truncation can create, so only that is trimmed; a genuinely
    // invalid byte still fails the decode. Same treatment, for the same reason,
    // as `sbom::extractor::readme::read_bounded`.
    if cut {
        if let Err(e) = std::str::from_utf8(&body) {
            if e.error_len().is_none() {
                body.truncate(e.valid_up_to());
            }
        }
    }

    // A README that is not UTF-8 is not a README this can display. Lossy
    // conversion would put replacement characters in a document an operator is
    // reading to decide something, which is worse than saying there is none.
    match String::from_utf8(body) {
        Ok(text) => Ok(Some(text)),
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "linked readme is not valid UTF-8; ignoring");
            Ok(None)
        }
    }
}

/// Fetch one image a README pointed at, for `remote_images = "proxy"`.
///
/// The guards RFC 0007-bis §7.1 requires, in the order they matter:
///
/// - **the caller named no URL.** This one came out of a README this instance
///   stored, resolved by index. That is the property that stops the endpoint
///   being an open proxy, and it is established before this is reached;
/// - [`super::ssrf::fetch_following_redirects`] validates **every hop**, not
///   only the first — an image URL that `302`s to `169.254.169.254` is the
///   attack this exists to stop — and never attaches the operator's
///   credentials, because an image host is by definition not the upstream they
///   were configured for;
/// - `allowed_hosts` (`remote_image_hosts`) is re-checked against the URL that
///   **answered**, so a redirect cannot walk an allow-listed CDN's response off
///   the list;
/// - the body is read **incrementally** to `max_bytes` and refused if it would
///   exceed it, rather than buffered then truncated: half a PNG is a broken
///   image, and an upstream must not be able to make this hold a gigabyte in
///   order to discard most of it;
/// - the `Content-Type` is checked against
///   [`batlehub_core::services::readme::image::IMAGE_TYPES`] and echoed **from
///   the list**, never passed through.
///
/// `Ok(None)` for anything that is simply not there. The caller turns that into
/// the chip the `strip` policy would have shown, which is a better answer than a
/// broken-image icon and the reason none of these is an error.
pub async fn fetch_image(
    plain: &reqwest::Client,
    url: &str,
    allowed_hosts: &[String],
    max_bytes: usize,
) -> Result<Option<FetchedImage>, CoreError> {
    use batlehub_core::services::readme::image::image_host_allowed;
    use futures::StreamExt;

    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CoreError::Registry(format!("invalid image URL '{url}': {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(None);
    }

    // No instance origin and no credentials: an image host is a third party by
    // construction, so there is no case in which the operator's token should
    // travel with this request. Passing `plain` for both clients makes that a
    // property of the call rather than of where the redirect chain happens to
    // end, and the empty origin makes the SSRF check apply to every hop.
    let resp = super::ssrf::fetch_following_redirects(plain, plain, &None, "", parsed).await?;

    // **The host that answered**, not the host that was asked for.
    //
    // `remote_image_hosts` is documented as deciding which hosts this server
    // will dial, and the caller can only check the URL in the README — the
    // redirect chain is followed inside `fetch_following_redirects`. Checked
    // only there, an allow-listed badge CDN that answers
    // `302 Location: https://evil.example/x.svg` had those bytes served back
    // from the console's own origin, under a `Content-Type` this function
    // vouches for. The bytes are what the allow-list is about, so this is where
    // it has to hold.
    //
    // Every hop is still *dialled*: `ensure_public_url` refuses a private or
    // loopback target on each one, so what a redirect can still cost is a
    // request to a public host — the same residue `fetch_linked_text` accepts,
    // which walks its chain through the same guard.
    if !image_host_allowed(resp.url().as_str(), allowed_hosts) {
        tracing::debug!(
            url,
            answered = %resp.url(),
            "readme image: redirected off the allow-listed hosts"
        );
        return Ok(None);
    }

    if !resp.status().is_success() {
        tracing::debug!(url, status = %resp.status(), "readme image: upstream refused");
        return Ok(None);
    }

    let Some(content_type) = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(image_content_type)
    else {
        tracing::debug!(
            url,
            "readme image: content type is not an allow-listed image"
        );
        return Ok(None);
    };

    // Refuse on the declared length before reading a byte, when there is one.
    // The incremental read below is what actually enforces the cap — a
    // `Content-Length` is a claim, not a fact — but believing an honest one
    // saves the transfer.
    if resp
        .content_length()
        .is_some_and(|len| len > max_bytes as u64)
    {
        tracing::debug!(url, "readme image: over the cap by its own Content-Length");
        return Ok(None);
    }

    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CoreError::Registry(format!("reading image: {e}")))?;
        if bytes.len() + chunk.len() > max_bytes {
            // Refused, not truncated. Half an image is a broken image, and
            // serving one would look like a bug in this proxy rather than a
            // limit the operator set.
            tracing::debug!(url, max_bytes, "readme image: over the cap");
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(Some(FetchedImage {
        content_type,
        bytes,
    }))
}

/// True when `a` and `b` share scheme, host, and (explicit-or-default) port.
pub fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

pub fn ensure_same_origin(url: &str, base_url: &str) -> Result<(), CoreError> {
    ensure_linked_origin(url, base_url, &[])
}

/// [`ensure_same_origin`], widened by host suffixes the *protocol* requires.
///
/// Same-origin is the right default and the wrong absolute: the VS Code
/// Marketplace answers `extensionquery` from `marketplace.visualstudio.com` and
/// serves every asset it names — the `Content.Details` README among them — from
/// `{publisher}.gallerycdn.vsassets.io`. Checked strictly against the base URL,
/// *every* linked-README read for that kind failed, so no VS Code Marketplace
/// README was ever stored while the support table advertised `MetadataLinked`.
///
/// The widening is deliberately narrow:
///
/// - **suffix, not substring** — `host == suffix` or `host.ends_with(".{suffix}")`,
///   so `gallerycdn.vsassets.io.evil.example` does not match;
/// - **`https` only**, so a redirect that downgrades the scheme is still
///   refused;
/// - **caller-supplied**, and the only caller that supplies anything passes its
///   list only when its base URL *is* the public marketplace — a self-hosted
///   mirror serves its own assets from its own origin and inherits no exception.
pub fn ensure_linked_origin(
    url: &str,
    base_url: &str,
    extra_host_suffixes: &[&str],
) -> Result<(), CoreError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CoreError::Registry(format!("invalid upstream URL '{url}': {e}")))?;
    let base = reqwest::Url::parse(base_url)
        .map_err(|e| CoreError::Registry(format!("invalid base URL '{base_url}': {e}")))?;

    if same_origin(&parsed, &base) {
        return Ok(());
    }

    if parsed.scheme() == "https" {
        if let Some(host) = parsed.host_str() {
            let host = host.to_ascii_lowercase();
            if extra_host_suffixes.iter().any(|suffix| {
                let suffix = suffix.to_ascii_lowercase();
                host == suffix || host.ends_with(&format!(".{suffix}"))
            }) {
                return Ok(());
            }
        }
    }

    Err(CoreError::Registry(format!(
        "refusing to fetch cross-origin upstream URL '{url}' (expected origin of '{base_url}')"
    )))
}

/// Refuse a built URL that no longer sits under `base`'s path prefix.
///
/// [`ensure_same_origin`] compares scheme, host and port, which is the wrong
/// question for a URL assembled by interpolating a caller-supplied path: a dot
/// segment walks *within* the origin, so the result is still same-origin while
/// naming something the caller was never authorised to read — another
/// repository, or a path outside a mirror's subtree. `Url::parse` has already
/// applied its own normalisation by the time this runs, so comparing the parsed
/// path against the base's is what catches the escape.
pub fn ensure_url_under_base(url: &str, base_url: &str) -> Result<(), CoreError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CoreError::Registry(format!("invalid upstream URL '{url}': {e}")))?;
    let base = reqwest::Url::parse(base_url)
        .map_err(|e| CoreError::Registry(format!("invalid base URL '{base_url}': {e}")))?;
    if !same_origin(&parsed, &base) {
        return Err(CoreError::Registry(format!(
            "refusing to fetch cross-origin upstream URL '{url}' (expected origin of '{base_url}')"
        )));
    }
    let prefix = base.path().trim_end_matches('/');
    // An empty prefix means the base is the origin root, which every path is
    // under; otherwise the path must be the prefix itself or sit beneath it,
    // matched on a `/` boundary so `/repo-evil` does not pass for `/repo`.
    if prefix.is_empty()
        || parsed.path() == prefix
        || parsed.path().starts_with(&format!("{prefix}/"))
    {
        return Ok(());
    }
    Err(CoreError::Registry(format!(
        "refusing to fetch '{url}': it escapes the base path of '{base_url}'"
    )))
}

/// Percent-encode a slash-separated path, encoding each segment but keeping the
/// `/` separators literal.
///
/// Interpolating a caller-supplied path straight into an upstream URL lets the
/// URL parser reinterpret it: `%2e%2e` is a dot segment to `url::Url::parse`
/// and pops a path component, so a path that cleared the edge validator can
/// still escape the base it was joined to. Encoding here makes the segment mean
/// itself — a literal `%2e%2e` filename — rather than a navigation instruction.
///
/// `.` is unreserved, so `percent_encode` leaves it alone and a segment of
/// nothing but dots would survive as navigation. Those are encoded here
/// explicitly; every other segment keeps its dots, so `main.rs` stays readable.
pub fn percent_encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if !segment.is_empty() && segment.bytes().all(|b| b == b'.') {
                return segment.replace('.', "%2E");
            }
            percent_encode(segment)
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode a query string value, encoding all characters except
/// unreserved ones (letters, digits, `-`, `_`, `.`, `~`).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(
                    char::from_digit((byte >> 4) as u32, 16)
                        .expect("nibble 0-15 always yields a hex digit")
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit((byte & 0xf) as u32, 16)
                        .expect("nibble 0-15 always yields a hex digit")
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_date_valid() {
        let dt = parse_http_date("Fri, 15 Mar 2024 12:34:56 GMT").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2024-03-15 12:34:56"
        );
    }

    #[test]
    fn parse_http_date_invalid_returns_none() {
        assert!(parse_http_date("not-a-date").is_none());
    }

    #[test]
    fn ensure_same_origin_matching_origin_ok() {
        assert!(ensure_same_origin(
            "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
            "https://registry.npmjs.org"
        )
        .is_ok());
    }

    #[test]
    fn ensure_same_origin_different_host_rejected() {
        let err = ensure_same_origin(
            "https://evil.example.com/payload.tgz",
            "https://registry.npmjs.org",
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::Registry(_)));
    }

    /// The widening the VS Code Marketplace needs: its assets live on a
    /// per-publisher sub-domain of a CDN the API host does not share.
    #[test]
    fn an_extra_host_suffix_admits_the_asset_cdn_and_its_subdomains() {
        for url in [
            "https://ms-python.gallerycdn.vsassets.io/extensions/x/Details",
            "https://gallerycdn.vsassets.io/extensions/x/Details",
        ] {
            assert!(
                ensure_linked_origin(
                    url,
                    "https://marketplace.visualstudio.com",
                    &["gallerycdn.vsassets.io"]
                )
                .is_ok(),
                "{url} should be admitted"
            );
        }
    }

    /// Suffix, not substring — or the exception is a wildcard for anyone who can
    /// register a domain ending in the right letters.
    #[test]
    fn an_extra_host_suffix_is_not_a_substring_match() {
        for url in [
            // The suffix as a *prefix* of somebody else's domain.
            "https://gallerycdn.vsassets.io.evil.example/payload",
            // The suffix without the dot separator.
            "https://evilgallerycdn.vsassets.io/payload",
        ] {
            assert!(
                ensure_linked_origin(
                    url,
                    "https://marketplace.visualstudio.com",
                    &["gallerycdn.vsassets.io"]
                )
                .is_err(),
                "{url} must be refused"
            );
        }
    }

    /// A redirect that downgrades the scheme does not get the exception: the
    /// widening is about *which host*, never about reading a stored document off
    /// a plaintext connection.
    #[test]
    fn an_extra_host_suffix_does_not_admit_plaintext() {
        assert!(ensure_linked_origin(
            "http://ms-python.gallerycdn.vsassets.io/extensions/x/Details",
            "https://marketplace.visualstudio.com",
            &["gallerycdn.vsassets.io"]
        )
        .is_err());
    }

    /// With no suffixes, this is exactly `ensure_same_origin` — including for
    /// the host the widened caller trusts.
    #[test]
    fn no_extra_host_suffixes_is_plain_same_origin() {
        assert!(ensure_linked_origin(
            "https://ms-python.gallerycdn.vsassets.io/extensions/x/Details",
            "https://marketplace.visualstudio.com",
            &[]
        )
        .is_err());
    }

    // ── The linked-README redirect chain ─────────────────────────────────────

    /// The credential still travels while the chain stays on the configured
    /// upstream: a linked README is a request to the registry the operator
    /// configured that token for, and the anonymous read is the one that gets
    /// rate-limited.
    #[tokio::test]
    async fn a_same_origin_readme_redirect_still_carries_the_credential() {
        let mut server = mockito::Server::new_async().await;
        let redirect = server
            .mock("GET", "/readme")
            .with_status(302)
            .with_header("location", "/readme-final")
            .create_async()
            .await;
        let served = server
            .mock("GET", "/readme-final")
            .match_header("x-api-key", "secret")
            .with_status(200)
            .with_body("# hi")
            .create_async()
            .await;

        let opts = UpstreamHttpOptions {
            custom_header: Some(("X-Api-Key".to_owned(), "secret".to_owned())),
            ..Default::default()
        };
        let (credentialed, plain) = no_redirect_client_pair(&opts).unwrap();
        let url = format!("{}/readme", server.url());
        let text = fetch_linked_text(&credentialed, &plain, &None, &url, &server.url(), &[], 4096)
            .await
            .unwrap();

        assert_eq!(text.as_deref(), Some("# hi"));
        redirect.assert_async().await;
        served.assert_async().await;
    }

    /// A `302` off the configured origin is refused **before it is dialled**.
    ///
    /// While the chain was reqwest's to follow, the target was requested — and
    /// handed the configured `custom_header`, which reqwest does not strip
    /// cross-host — and only the *stored body* was refused afterwards. `expect(0)`
    /// is the whole point of this test: the redirect target must see no request
    /// at all, so there is nothing for it to log a token from.
    #[tokio::test]
    async fn a_readme_redirect_off_origin_is_refused_before_it_is_dialled() {
        let mut attacker = mockito::Server::new_async().await;
        let stolen = attacker
            .mock("GET", "/steal")
            .expect(0)
            .with_status(200)
            .with_body("owned")
            .create_async()
            .await;

        let mut upstream = mockito::Server::new_async().await;
        let _redirect = upstream
            .mock("GET", "/readme")
            .with_status(302)
            .with_header("location", &format!("{}/steal", attacker.url()))
            .create_async()
            .await;

        let opts = UpstreamHttpOptions {
            custom_header: Some(("X-Api-Key".to_owned(), "secret".to_owned())),
            ..Default::default()
        };
        let (credentialed, plain) = no_redirect_client_pair(&opts).unwrap();
        let url = format!("{}/readme", upstream.url());
        let result = fetch_linked_text(
            &credentialed,
            &plain,
            &None,
            &url,
            &upstream.url(),
            &[],
            4096,
        )
        .await;

        assert!(
            matches!(result, Err(CoreError::Registry(_))),
            "an off-origin redirect target must not become a stored README"
        );
        stolen.assert_async().await;
    }

    #[test]
    fn percent_encode_alphanumeric_and_safe_chars_unchanged() {
        assert_eq!(percent_encode("abc123-_.~"), "abc123-_.~");
    }

    #[test]
    fn percent_encode_path_keeps_separators_and_encodes_the_segments() {
        assert_eq!(percent_encode_path("src/main.rs"), "src/main.rs");
        // The dot segments a URL parser would act on become literal filenames.
        assert_eq!(percent_encode_path("a/../b"), "a/%2E%2E/b");
        assert_eq!(percent_encode_path("a/%2e%2e/b"), "a/%252e%252e/b");
        assert_eq!(percent_encode_path("a b/c"), "a%20b/c");
        // A single dot is navigation too, and a filename keeps its dots.
        assert_eq!(percent_encode_path("a/./b"), "a/%2E/b");
        assert_eq!(percent_encode_path("a/main.rs"), "a/main.rs");
        assert_eq!(percent_encode_path("a/...hidden/b"), "a/...hidden/b");
    }

    /// A path that walks out of the base is refused even though it never
    /// leaves the origin — which is the only thing `ensure_same_origin` asks.
    #[test]
    fn a_dot_segment_that_escapes_the_base_path_is_refused() {
        let base = "https://forge.example/org/repo";
        for url in [
            "https://forge.example/org/repo/../../victim/private",
            "https://forge.example/org/repo/%2e%2e/%2e%2e/victim/private",
            "https://forge.example/other/repo",
            // A prefix that merely starts with the base's is not under it.
            "https://forge.example/org/repo-evil/x",
        ] {
            assert!(
                ensure_url_under_base(url, base).is_err(),
                "{url} must not pass as under {base}"
            );
        }
    }

    #[test]
    fn a_path_under_the_base_passes() {
        let base = "https://forge.example/org/repo";
        for url in [
            "https://forge.example/org/repo",
            "https://forge.example/org/repo/raw/main/src/main.rs",
            // An encoded dot segment that `Url::parse` leaves alone: it is a
            // filename, not navigation, so it stays under the base.
            "https://forge.example/org/repo/raw/main/%252e%252e",
        ] {
            ensure_url_under_base(url, base)
                .unwrap_or_else(|e| panic!("{url} must be under {base}, got {e:?}"));
        }
        // A base at the origin root has no prefix to escape.
        ensure_url_under_base(
            "https://forge.example/anything/at/all",
            "https://forge.example",
        )
        .expect("an origin-root base admits every path");
    }

    #[test]
    fn a_cross_origin_url_is_still_refused_by_the_base_check() {
        assert!(ensure_url_under_base(
            "https://evil.example/org/repo/x",
            "https://forge.example/org/repo"
        )
        .is_err());
    }

    #[test]
    fn percent_encode_encodes_space_and_special_chars() {
        let encoded = percent_encode("hello world+foo=bar");
        assert!(encoded.contains("%20"), "space should be %20");
        assert!(encoded.contains("%2B"), "plus should be %2B");
        assert!(encoded.contains("%3D"), "equals should be %3D");
        assert!(
            !encoded.contains(' '),
            "encoded string should have no raw spaces"
        );
    }

    #[test]
    fn ensure_same_origin_different_scheme_rejected() {
        assert!(ensure_same_origin(
            "http://registry.npmjs.org/lodash.tgz",
            "https://registry.npmjs.org"
        )
        .is_err());
    }

    #[test]
    fn ensure_same_origin_different_port_rejected() {
        assert!(ensure_same_origin(
            "https://registry.npmjs.org:8443/lodash.tgz",
            "https://registry.npmjs.org"
        )
        .is_err());
    }

    #[test]
    fn ensure_same_origin_invalid_url_rejected() {
        assert!(ensure_same_origin("not a url", "https://registry.npmjs.org").is_err());
    }

    fn empty_opts() -> UpstreamHttpOptions {
        UpstreamHttpOptions::default()
    }

    // ── Proxy routing integration tests ──────────────────────────────────────

    /// A captured request as seen by the recording proxy.
    struct ProxyRequest {
        method: String,
        target_url: String,
        proxy_authorization: Option<String>,
    }

    /// Spawn a minimal HTTP/1.1 forwarding proxy on a random port.
    ///
    /// The proxy:
    /// - records each request it handles, sending a `ProxyRequest` on the channel,
    /// - strips the `Proxy-Authorization` header before forwarding,
    /// - converts the absolute-URI request line to relative form,
    /// - forwards to the upstream and pipes the response back.
    ///
    /// Returns the proxy port and a receiver for recorded requests.
    async fn start_recording_proxy() -> (u16, tokio::sync::mpsc::UnboundedReceiver<ProxyRequest>) {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let tx = tx.clone();
                tokio::spawn(proxy_handle(stream, tx));
            }
        });

        (port, rx)
    }

    /// Spawn a proxy that always returns 502 — used for `no_proxy` bypass tests.
    async fn start_rejecting_proxy() -> u16 {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let _ = stream
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        port
    }

    async fn proxy_handle(
        mut client: tokio::net::TcpStream,
        tx: tokio::sync::mpsc::UnboundedSender<ProxyRequest>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut buf = vec![0u8; 8192];
        let n = client.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            return;
        }

        // Parse the raw HTTP/1.x request.
        let raw = String::from_utf8_lossy(&buf[..n]);
        let mut lines = raw.lines();

        // Request line: "GET http://host:port/path HTTP/1.1"
        let request_line = lines.next().unwrap_or("");
        let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
        if parts.len() < 3 {
            return;
        }
        let method = parts[0].to_owned();
        let url = parts[1].to_owned();
        let version = parts[2];

        let mut proxy_auth: Option<String> = None;
        let mut forwarded_headers: Vec<String> = Vec::new();
        for line in lines.by_ref() {
            if line.is_empty() {
                break;
            }
            if line
                .to_ascii_lowercase()
                .starts_with("proxy-authorization:")
            {
                proxy_auth = Some(line.to_owned());
            } else {
                forwarded_headers.push(line.to_owned());
            }
        }

        let _ = tx.send(ProxyRequest {
            method: method.clone(),
            target_url: url.clone(),
            proxy_authorization: proxy_auth,
        });

        // Derive relative path and upstream address from the absolute URI.
        let without_scheme = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .unwrap_or(url.as_str());

        let (host_port, rel_path) = match without_scheme.split_once('/') {
            Some((h, p)) => (h, format!("/{p}")),
            None => (without_scheme, "/".to_owned()),
        };
        let upstream_addr = if host_port.contains(':') {
            host_port.to_owned()
        } else {
            format!("{host_port}:80")
        };

        let mut upstream = match tokio::net::TcpStream::connect(&upstream_addr).await {
            Ok(s) => s,
            Err(_) => {
                let _ = client
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return;
            }
        };

        // Forward as a relative-URI request (strip proxy metadata).
        let forwarded_req = format!(
            "{method} {rel_path} {version}\r\n{}\r\n\r\n",
            forwarded_headers.join("\r\n")
        );
        if upstream.write_all(forwarded_req.as_bytes()).await.is_err() {
            return;
        }

        // Pipe upstream response back to client.
        let mut resp = vec![0u8; 65536];
        let m = upstream.read(&mut resp).await.unwrap_or(0);
        let _ = client.write_all(&resp[..m]).await;
    }

    #[tokio::test]
    async fn proxy_routes_http_request_through_proxy() {
        let mut upstream = mockito::Server::new_async().await;
        let _mock = upstream
            .mock("GET", "/registry-path")
            .with_status(200)
            .with_body("ok")
            .create_async()
            .await;

        let (proxy_port, mut proxy_rx) = start_recording_proxy().await;

        let opts = UpstreamHttpOptions {
            proxy_url: Some(format!("http://127.0.0.1:{proxy_port}")),
            ..Default::default()
        };
        let client = apply_upstream_options(reqwest::Client::builder(), &opts).unwrap();

        let url = format!("{}/registry-path", upstream.url());
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("request should succeed");
        assert_eq!(resp.status(), 200);

        // The proxy must have received exactly one request for our URL.
        let recorded = proxy_rx.recv().await.expect("proxy received a request");
        assert_eq!(recorded.method, "GET");
        assert!(
            recorded.target_url.ends_with("/registry-path"),
            "proxy saw absolute URI: {}",
            recorded.target_url
        );
    }

    #[tokio::test]
    async fn no_proxy_bypasses_proxy_for_excluded_host() {
        // Start an upstream that responds 200.
        let mut upstream = mockito::Server::new_async().await;
        let _mock = upstream
            .mock("GET", "/direct")
            .with_status(200)
            .with_body("direct")
            .create_async()
            .await;

        // Start a proxy that always returns 502; if the request goes through it the test fails.
        let bad_proxy_port = start_rejecting_proxy().await;

        let opts = UpstreamHttpOptions {
            proxy_url: Some(format!("http://127.0.0.1:{bad_proxy_port}")),
            // 127.0.0.1 is the upstream host — exclude it from proxying.
            no_proxy: Some("127.0.0.1".to_owned()),
            ..Default::default()
        };
        let client = apply_upstream_options(reqwest::Client::builder(), &opts).unwrap();

        let url = format!("{}/direct", upstream.url());
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("request should reach upstream directly, bypassing the 502 proxy");
        assert_eq!(
            resp.status(),
            200,
            "response should come from upstream, not from the rejecting proxy"
        );
    }

    #[tokio::test]
    async fn proxy_auth_credentials_sent_as_proxy_authorization_header() {
        let mut upstream = mockito::Server::new_async().await;
        let _mock = upstream
            .mock("GET", "/auth-test")
            .with_status(200)
            .create_async()
            .await;

        let (proxy_port, mut proxy_rx) = start_recording_proxy().await;

        let opts = UpstreamHttpOptions {
            proxy_url: Some(format!("http://127.0.0.1:{proxy_port}")),
            proxy_username: Some("proxyuser".to_owned()),
            proxy_password: Some("proxypass".to_owned()),
            ..Default::default()
        };
        let client = apply_upstream_options(reqwest::Client::builder(), &opts).unwrap();

        let url = format!("{}/auth-test", upstream.url());
        let _ = client.get(&url).send().await;

        let recorded = proxy_rx.recv().await.expect("proxy received a request");
        assert!(
            recorded.proxy_authorization.is_some(),
            "Proxy-Authorization header should be sent with credentials"
        );
        let auth = recorded.proxy_authorization.unwrap();
        // reqwest encodes "proxyuser:proxypass" in Base64 for Basic auth.
        assert!(
            auth.to_ascii_lowercase().contains("basic"),
            "expected Basic auth, got: {auth}"
        );
    }

    #[test]
    fn default_opts_have_no_fields_set() {
        let opts = empty_opts();
        assert!(opts.bearer_token.is_none());
        assert!(opts.basic_auth.is_none());
        assert!(opts.custom_header.is_none());
        assert!(opts.ca_cert_path.is_none());
    }

    #[test]
    fn clone_preserves_all_fields() {
        let opts = UpstreamHttpOptions {
            bearer_token: Some("tok".to_owned()),
            basic_auth: Some(("user".to_owned(), "pass".to_owned())),
            custom_header: Some(("X-Key".to_owned(), "val".to_owned())),
            ca_cert_path: Some("/etc/ca.pem".to_owned()),
            ..Default::default()
        };
        let cloned = opts.clone();
        assert_eq!(cloned.bearer_token.as_deref(), Some("tok"));
        assert_eq!(cloned.ca_cert_path.as_deref(), Some("/etc/ca.pem"));
    }

    #[test]
    fn debug_format_contains_field_names() {
        let opts = UpstreamHttpOptions {
            bearer_token: Some("t".to_owned()),
            ..Default::default()
        };
        let s = format!("{opts:?}");
        assert!(s.contains("bearer_token"));
    }

    #[test]
    fn upstream_auth_headers_empty_opts_returns_empty_map() {
        let headers = upstream_auth_headers(&empty_opts()).unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn upstream_auth_headers_bearer_injects_authorization_header() {
        let opts = UpstreamHttpOptions {
            bearer_token: Some("mytoken".to_owned()),
            ..Default::default()
        };
        let headers = upstream_auth_headers(&opts).unwrap();
        let auth = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(auth, "Bearer mytoken");
    }

    #[test]
    fn upstream_auth_headers_custom_header_is_injected() {
        let opts = UpstreamHttpOptions {
            custom_header: Some(("X-Api-Key".to_owned(), "secret".to_owned())),
            ..Default::default()
        };
        let headers = upstream_auth_headers(&opts).unwrap();
        let val = headers.get("x-api-key").unwrap().to_str().unwrap();
        assert_eq!(val, "secret");
    }

    #[test]
    fn upstream_auth_headers_both_bearer_and_custom() {
        let opts = UpstreamHttpOptions {
            bearer_token: Some("tok".to_owned()),
            custom_header: Some(("X-Tenant".to_owned(), "acme".to_owned())),
            ..Default::default()
        };
        let headers = upstream_auth_headers(&opts).unwrap();
        assert!(headers.contains_key("authorization"));
        assert!(headers.contains_key("x-tenant"));
    }

    #[test]
    fn apply_upstream_tls_no_cert_returns_unmodified_builder() {
        let builder = reqwest::Client::builder();
        let result = apply_upstream_tls(builder, &empty_opts());
        assert!(result.is_ok());
        assert!(result.unwrap().build().is_ok());
    }

    #[test]
    fn apply_upstream_tls_nonexistent_cert_returns_error() {
        let opts = UpstreamHttpOptions {
            ca_cert_path: Some("/nonexistent/ca.pem".to_owned()),
            ..Default::default()
        };
        let err = apply_upstream_tls(reqwest::Client::builder(), &opts).unwrap_err();
        assert!(err.to_string().contains("reading CA cert"));
    }

    #[test]
    fn apply_upstream_options_no_auth_builds_client() {
        let client = apply_upstream_options(reqwest::Client::builder(), &empty_opts());
        assert!(client.is_ok());
    }

    #[test]
    fn apply_upstream_options_with_bearer_builds_client() {
        let opts = UpstreamHttpOptions {
            bearer_token: Some("tok".to_owned()),
            ..Default::default()
        };
        let client = apply_upstream_options(reqwest::Client::builder(), &opts);
        assert!(client.is_ok());
    }

    #[test]
    fn apply_upstream_options_with_custom_header_builds_client() {
        let opts = UpstreamHttpOptions {
            custom_header: Some(("X-Api-Key".to_owned(), "val".to_owned())),
            ..Default::default()
        };
        let client = apply_upstream_options(reqwest::Client::builder(), &opts);
        assert!(client.is_ok());
    }

    #[test]
    fn apply_upstream_tls_with_proxy_url_builds_client() {
        let opts = UpstreamHttpOptions {
            proxy_url: Some("http://proxy.example.com:3128".to_owned()),
            ..Default::default()
        };
        let result = apply_upstream_tls(reqwest::Client::builder(), &opts);
        assert!(result.is_ok());
        assert!(result.unwrap().build().is_ok());
    }

    #[test]
    fn apply_upstream_tls_with_proxy_and_basic_auth_builds_client() {
        let opts = UpstreamHttpOptions {
            proxy_url: Some("http://proxy.example.com:3128".to_owned()),
            proxy_username: Some("proxyuser".to_owned()),
            proxy_password: Some("proxypass".to_owned()),
            ..Default::default()
        };
        let result = apply_upstream_tls(reqwest::Client::builder(), &opts);
        assert!(result.is_ok());
        assert!(result.unwrap().build().is_ok());
    }

    #[test]
    fn apply_upstream_tls_with_proxy_and_no_proxy_builds_client() {
        let opts = UpstreamHttpOptions {
            proxy_url: Some("http://proxy.example.com:3128".to_owned()),
            no_proxy: Some("localhost,127.0.0.1,internal.example.com".to_owned()),
            ..Default::default()
        };
        let result = apply_upstream_tls(reqwest::Client::builder(), &opts);
        assert!(result.is_ok());
        assert!(result.unwrap().build().is_ok());
    }
}
