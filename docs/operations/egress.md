# What leaves this instance

For the operator who has to answer *what does this box talk to, and when*.

A caching proxy exists partly so that developer machines stop talking to the
public internet. That only holds if you know what the proxy itself talks to, and
what makes it start. This page is the list.

Nothing here is new outbound *infrastructure*: every request below goes to a
registry upstream you configured, through the same HTTP client, with the same
timeouts, TLS settings, upstream authentication and SSRF guards as any other
fetch. What differs between the entries is **what makes the request happen**.

## A build asks for something

The ordinary case, and the reason the software exists. A package manager
resolves a version or downloads an artifact, the proxy has neither cached, and it
fetches from the configured upstream.

Turn it off by not running a proxy-mode registry: `mode = "local"` never
consults an upstream at all.

**One kind fetches from hosts you did not configure.** An `sdkman` registry
asks `broker.sdkman.io` for a download and is answered with a `302` to
wherever the vendor publishes — `github.com` (and
`objects.githubusercontent.com`), `repo.maven.apache.org`,
`services.gradle.org`, `groovy.jfrog.io` at minimum. The chain is followed
server-side through the SSRF guard and the operator's credentials stop at the
two configured origins, but egress to those CDN hosts is a prerequisite of the
kind. The list is *observed, not exhaustive*: the broker can add a host without
telling anyone, which is itself an argument for warming ahead of an air gap
([RFC 0010](/rfc/0010-toolchain-managers) §9).

## A search box is typed into

`GET /api/v1/explore/upstream` fans a query out across every accessible
registry's upstream search API. **The user's free-text query is forwarded
upstream**, which for an operator whose threat model is "this box does not tell
anyone what we are looking for" is a disclosure worth knowing about.

Turn it off per registry with `search_url = ""`.

## The console's discovery read {#the-console-s-discovery-read}

**New behaviour, on by default.** When somebody opens the package page for a
package this instance holds nothing of, BatleHub asks the registry's upstream
what versions exist — one metadata document, the same one the first
`npm install` of that package would have caused.

Before this, the page said *"no versions yet"* about a package the console's own
search had just told the reader exists. That is the defect it fixes, and it is
also the one default in the change that alters an instance's outbound traffic
without the operator asking. It is named here rather than left to be discovered
in a traffic graph.

**What it does not do.** Looking at a package is not downloading it. The read
fetches one metadata document and nothing else:

- no artifact is fetched, and no storage entry is created;
- no `package_statuses` row is written, so the package does not appear in
  `GET /api/v1/explore/packages` because somebody looked at it;
- no access event is recorded, no download count moves, no `last_accessed` is
  touched;
- no quota is consumed, and the eviction service's accounting does not change.

A page view must not be able to change what the catalogue claims this instance
has — otherwise browsing the console silently rewrites the inventory you read to
make decisions.

**What bounds it.**

| Bound | Effect |
| --- | --- |
| Cache-first | The document lands in the metadata cache under the key the proxy path already uses, so it obeys the registry's `metadata_ttl_secs`. N page views within one TTL produce one request. |
| …including the gallery kinds | Open VSX, the VS Code and JetBrains marketplaces and conda have no listing document to fetch and answer by *query* instead. That answer is cached in the same place, under the same TTL, behind the same single flight — which it was not before: every page view of one of those was an upstream query, and for conda one `repodata.json` per channel platform. |
| Single-flight | Ten people opening the same new package at once produce one upstream request, not ten. |
| Negative cache | An upstream `404` is remembered for `negative_ttl_secs` (300 by default), so a bad URL or a crawler cannot make every reload a request. A *connection failure* is not remembered — it is not a fact about the package. |
| Registry access | The read only happens for a registry the caller can already explore. Somebody who cannot see a registry cannot make it emit traffic. |
| Rate limit | The per-registry `rate_limit` still applies on top, for a caller enumerating *different* names. |

**A private name is never sent upstream.** If the local backend hosts the
package, the read is suppressed entirely — on any mode. On a `hybrid` registry a
private package shares a namespace with a public index, and sending its name
there on every page view would disclose the existence of internal software to a
third party. It would also invite a dependency-confusion answer, where the page
shows upstream's versions of a name that means something else here.

**Turning it off.** Per registry:

```toml
[registries.upstream_detail]
enabled = false
```

The page then answers from local rows exactly as it did before, with no attempt
and no banner. That is the right setting for an
[air-gapped estate](/rfc/0008-mise-in-an-air-gapped-estate): with no route off
site the read would fail once per TTL and the page would say the upstream could
not be reached, which is a supported outcome but a noisy one when it is the
permanent state of the world.

## A README's images

**None by default.** A README's images are not loaded: they normally live on
third-party hosts, and rendering them would mean every console page view sending
a request — with a `Referer` — to a host the *package author* chose, announcing
that someone inside your network is reading about this package at this moment.

Each image is replaced with an inline chip carrying its alt text and its host, so
a reader can see that an image was there and where it pointed.

With `remote_images = "proxy"`, **this server** fetches them instead, once, and
serves them from its own origin. The reader's browser still never talks to a host
the package author chose; what changes is that this instance does, on the first
render of each image. The requests are this server's, coalesced and cached under
the registry's `metadata_ttl_secs`, and a URL that fails is remembered so a dead
badge is not re-dialled on every page view.

The residual is worth naming rather than hiding: a package author still chooses
*which host this server talks to*, and can therefore learn that **somebody** on
this instance rendered their README, plus this instance's egress IP. What they
cannot learn is who, when repeatedly, or from which internal address. That is a
real reduction and not an elimination; an operator for whom even that is
unacceptable keeps `"strip"`, which remains the default. See
[`remote_images`](/guide/admin-config#readme-capture).

## Someone presses Fetch {#someone-presses-fetch}

The package page lists versions this instance holds nothing of and marks each one
**not held here**. On those rows there is a **Fetch this version** button, and
pressing it downloads the artifact from upstream.

This is the only thing on the list that is **a decision rather than a side
effect**. Everything else here happens because a page was opened or a build ran;
this happens because a person pressed a button, and the audit log names them.

It is the ordinary download path — the same request a package manager would make,
under the caller's own identity, through the rules, the integrity check, quota
and the audit. It is not a warming task and does not use the warming service,
which bypasses all of those because its only caller is an administrator.

On by default, per registry, and inert on a `local`-mode registry. Turn it off
with `console_fetch = false` if you want the console strictly read-only. See
[fetching a version from the console](/guide/admin-config#console-fetch).

## A linked README

OpenVSX and the VS Code Marketplace give a *URL* for an extension's README rather
than the text. Following it is one outbound request, made in a background task
rather than on the request path a package manager is waiting on, and only for a
version this instance is caching. The URL is checked to be on the same origin as
the configured registry — widened, for the public VS Code Marketplace only, to
its `*.gallerycdn.vsassets.io` asset CDN — so a compromised or misconfigured
upstream cannot use it to point BatleHub at an internal host.

Redirects are followed by BatleHub rather than by its HTTP client, one hop at a
time, and each hop is re-checked before it is dialled: an upstream that answers
its own README URL with `302 Location: http://169.254.169.254/…` gets no request
at all. The registry's configured credentials travel with the read while it stays
on that registry's own origin — it is a request to the upstream they were
configured for, and the anonymous read is the one that gets rate-limited — and
are dropped the moment a redirect leaves it.

## Vulnerability scanning

The periodic OSV re-check, when `[vulnerability_scan] enabled = true`. Off by
default. See [SBOM](/guide/sbom).

## A scan job runs {#a-scan-job-runs}

A registry with a [`[registries.security]`](/guide/configuration#security)
profile hands each new version to the worker, and the scanners the profile names
make their own requests. Each is off unless you configured it in `[[scanners]]`.

Most of them dial a host you named: OSV, a Trivy server, the Socket and mlab
APIs. The binary scanners under the sandbox reach only what the sandbox lets
them. Two entries are worth stating separately.

**Rekor**, when `sigstore` is enabled: one lookup per transparency-log entry an
attestation cites, at `rekor_url` (`https://rekor.sigstore.dev` by default).
A host you configured, like the rest.

**A host the upstream chose**, in that same scanner. npm announces a version's
attestations in the packument as `dist.attestations.url`, and the bundle is
fetched from that URL — which means the *upstream index*, not your config,
names the host. It is treated the way [a linked README](#a-linked-readme) is:
the scheme must be `http` or `https`, redirects are followed by BatleHub one hop
at a time rather than by its HTTP client, every hop is re-checked against the
private, reserved, loopback and link-local ranges before it is dialled, and no
credential travels with the request at all. An upstream that answers with
`302 Location: http://169.254.169.254/…` gets no request; the scan reports the
refusal as a scanner error rather than a finding, so the response body of
something internal can never reach a finding an operator reads.

Turn the whole class off by leaving `[registries.security]` unwritten, or the
one entry off by removing `sigstore` from `[[scanners]]`.

## See also

- [Production hardening](/operations/production-hardening) — the other settings
  that differ between a working instance and one you would put in front of a
  company.
- [Configuration → README capture](/guide/admin-config#readme-capture) and
  [→ the console's discovery read](/guide/admin-config#the-console-s-discovery-read).
- [Package Explorer search](/use/package-explorer-search#upstream-search).
