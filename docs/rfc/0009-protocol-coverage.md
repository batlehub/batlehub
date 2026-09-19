# RFC 0009 — Every endpoint the client actually calls

| Field       | Value                                                        |
| ----------- | ------------------------------------------------------------ |
| Status      | **Implemented** — all nine phases landed, residue closed (§13.23); every ecosystem is verified against its real client by a script in CI rather than a transcript (§12.16), which found six further shipped bugs; the compact index is incremental and measured (§13.24) |
| Short       | Every endpoint the client actually calls |
| Settles     | Serving the paths each package manager really requests, and two mechanisms so the next invented endpoint fails the build |
| Author      | Max Batleforc <maxleriche.60@gmail.com>                       |
| Co-author   | Claude Opus 5 (1M context) <noreply@anthropic.com>            |
| Created     | 2026-08-16                                                    |
| Supersedes  | —                                                             |
| Touches     | `crates/core`, `crates/adapters`, `crates/web`, `server`, `docs` |

---

## 1. Summary

RFC 0006 closed a gap it found on its way past: `openvsx` and
`vscode-marketplace` cached VSIX bytes but served none of the gallery routes an
editor calls, so BatleHub could not be an editor's marketplace at all (§13.1-bis).
The survey that followed — `docs/internal/registry-api-coverage.md` — asked the
same question of the other nineteen registry kinds and found four more of the
same class, plus a long tail.

The headline is not the list. It is that in every case the *tests passed*, the
*docs described the feature as working*, and nobody had written down what the
client actually asks for. `npm audit` is served at
`/-/npm/v1/audit/bulk`; npm calls `/-/npm/v1/security/advisories/bulk`. Four
tests assert the first path. They exercise our route, and npm's route has never
been typed anywhere in this repository.

This RFC fixes all of it, and adds two mechanisms so that the next endpoint we
invent instead of implement fails the build:

1. a **protocol conformance fixture** per ecosystem — the client's literal paths,
   asserted to route — with a ratchet list of the ones not yet served;
2. **generated endpoint reference tables** in `docs/registries/*.md`, from
   `ui/openapi.json`, with a drift check, the way `docs:roadmap` and
   `docs:listing-coverage` already work.

And it states the two obligations every new endpoint inherits:

- **An endpoint that names a version is a listing, and RFC 0006 says listings are
  filtered.** Adding the RubyGems compact index without filtering it would
  re-open, for the default Ruby client, the exact hole RFC 0006 spent eight
  phases closing.
- **An endpoint that calls upstream is a cache, and must survive that upstream's
  loss.** No passthrough in the tree does today: `npm audit`, `govulncheck` and
  `dotnet list package --vulnerable` all fail outright when their advisory
  database is unreachable, having cached nothing from the identical request a
  minute earlier. A cache that only works while the upstream is up is a router.

### Before / after

```text
# today
$ npm audit                       → 404 from BatleHub, 404 from the forward
$ bundle install                  → falls back to specs.4.8.gz, which is NOT filtered
                                    → a blocked version resolves, then 403s on download
$ terraform init                  → network_mirror per our docs: 404 on every provider
$ go mod download                 → still dials sum.golang.org directly
$ conda install                   → repodata.json.zst 404s, full uncompressed transfer
$ npm audit  (upstream down)      → fails; nothing was cached, ever
$ govulncheck (upstream down)     → same
$ dotnet package search           → 200 {"totalHits": 0} — a stub, always

# after
$ npm audit                       → works, on npm's own paths
$ bundle install                  → compact index, filtered, blocked versions never offered
$ terraform init                  → network mirror (path-routed) or registry protocol (host-routed)
$ go mod download                 → checksum database proxied, no egress needed
$ conda install                   → .zst served, filtered before compression
$ npm audit  (upstream down)      → answered from cache; the pipeline keeps going
$ govulncheck (upstream down)     → same, and the sumdb too
$ dotnet package search           → real results, or what we hold when upstream is gone
```

---

## 2. Motivation

### 2.1 The failure is always the same shape

Five findings, one mechanism:

| Finding | What we built | What the client calls |
| --- | --- | --- |
| npm audit | `/-/npm/v1/audit/{quick,bulk}` | `/-/npm/v1/security/audits/quick`, `/-/npm/v1/security/advisories/bulk` |
| Terraform | the registry protocol at `/v1/…` | a network mirror, per our own docs |
| RubyGems | `specs.4.8.gz` + the JSON APIs | the compact index — `/versions`, `/info/{gem}` |
| Go | modules only | modules **and** `/sumdb/…` |
| conda | `repodata.json` | `repodata.json.zst` first |

In each row, somebody read the protocol, built something adjacent to it, and
wrote a test for what they built. The test is the problem: a test written from
the implementation cannot discover that the implementation answers the wrong
question. `crates/web/tests/proxy_npm_edge_cases.rs:76` and
`vuln_proxy_endpoints.rs:38` both POST to `/-/npm/v1/audit/…` and assert a
sensible response. Both pass. Both are wrong in the only way that matters.

**Measured, not argued** (§12.1): npm 11.17.0 sends
`POST /-/npm/v1/security/advisories/bulk`, receives the `404` this server used to
give it, and **exits 1** — one request, no fallback to the quick endpoint. So
`npm audit` against a pre-fix BatleHub failed the build rather than quietly
reporting nothing, which is worse than this RFC first described.

There is a sixth row, found while resolving this RFC's own open questions and
quieter than the five above: **NuGet search is a stub that returns an empty
result while the service index advertises it as supported**
(`nuget/search_publish.rs:103`, advertised at `service_index.rs:57-63`). Nothing
404s, nothing errors, and `dotnet package search` reports zero results against a
registry holding thousands of packages. `vsx` free-text search has the same shape
(`vsx/source.rs:151`), documented in a comment rather than shipped silently.
It is the same failure — an endpoint that is not what the client needs — wearing
a 200 instead of a 404, and it is the reason §5 needs a second assertion class
and search needs the design in §7.7.

### 2.2 RubyGems is not a coverage gap, it is a live block leak

The other four degrade a feature. This one breaks a guarantee.

`RegistryKind::listing_filter()` marks the RubyGems Marshal indexes
`Unsupported`, with the reason: hiding a version from a Marshal index would need
a Marshal encoder in Rust, "to hide what the JSON APIs already hide for every
client released this decade".

The reasoning is correct about the JSON APIs and wrong about which API the
default client reaches: **Bundler resolves from the compact index**, which we
did not serve.

> **Measured, and the consequence is not what this section first claimed**
> (§12.2). Bundler **4.0.17** requests `/versions`, and on a `404` **stops** —
> it does not fall back to the dependency API or to `specs.4.8.gz`. So against a
> pre-phase-2 BatleHub, `bundle install` did not quietly resolve a blocked
> version from an unfiltered index; it **failed outright**, with
> `Could not find gem … in rubygems repository`.
>
> The fallback chain described below is Bundler 2.x behaviour, documented but
> not measured here. On that line the leak is real; on Bundler 4 the ecosystem
> is simply broken. Serving the compact index fixes both, which is why the phase
> stands — but "closes a silent policy hole" was the wrong headline for the
> client most people now run, and the right one is "makes Ruby work at all".

The fix is not a Marshal encoder. The compact index is three plain-text
documents, and `DocumentBody::Text` already exists for exactly this
(`crates/core/src/ports/registry/client.rs:44`). The `Unsupported` reason string
becomes true once the compact index is served, instead of merely defensible.

### 2.3 The docs are the third witness, and they also drifted

Every `docs/registries/*.md` carries a hand-maintained "Endpoint reference"
table. `rubygems.md:116-126` lists six routes and no compact index.
`terraform.md:19-30` configures a protocol the code does not implement.
`npm.md:3` promises `npm audit` works. `use/vulnerability-proxy.md:80-85`
publishes the wrong paths as a reference.

Three independent records of the API surface — routes, tests, docs — and all
three agreed with each other and disagreed with the client. That is what a
hand-maintained table buys: consistency with itself.

---

## 3. Goals / non-goals

**Goals.** Every finding in `docs/internal/registry-api-coverage.md` fixed —
§3.1 through §3.5, the §2 Terraform download gate, and the whole §4 long tail.
Two mechanisms that make the class of failure detectable. Docs regenerated from
the code rather than maintained beside it.

**Non-goals.**

- **GitHub Packages** (`npm.pkg.github.com`, `maven.pkg.github.com`, `ghcr.io`).
  The survey named its absence; the resolution is that it is not a `github`-kind
  gap. `npm.pkg.github.com` speaks npm, and the way to proxy it is an `npm`
  registry with that upstream — which works today. The `github` kind is a
  *release* mirror, which is why `supports_local_mode()` is false for it. Stated
  as a non-goal so the survey row is closed rather than carried.
- **The legacy NuGet OData `/v2/` API**, Composer 1's `providers-url`, and the
  cargo git index. Three protocols no current client selects; adding them is
  surface without a caller, which is how we got here.
- **Correctness of documents we already serve.** RFC 0006 owns that. This RFC
  adds endpoints and the obligation that new ones are filtered; it does not
  re-audit existing filters.
- **Rate limiting, pagination and conditional requests** on the new endpoints
  beyond what the existing helpers already provide.

---

## 4. The two obligations every new endpoint inherits

Before the per-ecosystem design, the two rules that constrain all of it: an
endpoint that names a version must be **filtered** (§4.1), and an endpoint that
calls upstream must be **cached and survive that upstream's loss** (§4.2).

### 4.1 An endpoint that names a version is a listing

RFC 0006's contract is enforced by three checks (§13.7): `listing_filter()` is
an exhaustive match over `RegistryKind`, `blocking::strip` is exhaustive for the
same reason, and `every_advertised_filter_is_reachable_from_dispatch` checks the
two against each other. Those checks are exhaustive over **kinds**, not over
**documents**. A kind that already answers `Filtered` for one document can grow a
second, unfiltered one and nothing complains.

This RFC adds fourteen endpoints that name versions. Each one is a listing:

| New endpoint | Kind | Shape | Filter |
| --- | --- | --- | --- |
| `/versions` (compact index) | rubygems | whole-registry | `dispatch_multi` + snapshot |
| `/info/{gem}` | rubygems | per-package | `dispatch`, new `DocumentKind` |
| `/api/v1/dependencies` | rubygems | multi-package, query-selected | `dispatch_multi` |
| network-mirror `index.json` | terraform | per-provider | `dispatch`, new `DocumentKind` |
| network-mirror `{version}.json` | terraform | single-version | repair by composition |
| `channeldata.json` | conda | whole-registry | `dispatch_multi` |
| `/pypi/{name}/json` | pypi | per-package | `dispatch`, new `DocumentKind` |
| `/-/package/{pkg}/dist-tags` | npm | per-package | must not name a blocked version |
| `/-/v1/search` | npm | multi-package | `dispatch_multi` |
| `/api/v1/crates?q=` | cargo | multi-package | `dispatch_multi` |
| `autocomplete` | nuget | multi-package | `dispatch_multi` |
| `search.json`, `list.json` | composer | multi-package | `dispatch_multi` |
| `api/{namespace}` | openvsx | multi-package | filtered at the `vsx/source.rs` chokepoint |
| `/v1/modules/{…}/{version}` | terraform | single-version | 404 when blocked |

Two of these need machinery that does not exist yet:

- **`dispatch_multi` handles exactly one kind today** — conda
  (`blocking/mod.rs:210`), with `other =>` falling through to a
  `tracing::warn!`. Six kinds above need it. The warn arm becomes a real
  dispatch table, and the same 30-second `blocks:{registry}` snapshot (RFC 0006
  §13.5) serves all of them.
- **Search results are ranked, not just filtered.** Removing a package from a
  page of 20 leaves 19, and the client paginates by offset, so removal silently
  shortens result pages. Search filters remove *versions* from each hit and drop
  a hit only when every version is blocked; the total count is adjusted to match.

**Enforcement.** `every_advertised_filter_is_reachable_from_dispatch` is
extended from kinds to `(kind, DocumentKind)` pairs, and `listing_filter()`'s
`ListingDocument` gains the `DocumentKind` it describes. A kind that serves a
document not named in its `listing_filter()` slice fails the test. That is the
check that would have caught RubyGems: `specs.4.8.gz` was named and marked
`Unsupported`, but nothing recorded that the *document Bundler actually reads*
was neither served nor named.

### 4.2 An endpoint that calls upstream is a cache

A cache that only works while the upstream is reachable is a router. The whole
proposition of this product is that the upstream can go away — be slow, be
rate-limiting, be down, be on the other side of an air gap — and the estate keeps
building. Every document served through `ProxyService` already honours that:
cache-first, then upstream, then `get_stale` when the registry's `serve_stale`
allows (`proxy/handle.rs:689-700`, `config/schema/registry.rs:436`).

**The endpoints that bypass `ProxyService` do not.** Every passthrough in the
tree makes a bare `reqwest` call with no cache read and no cache write:

| Handler | Call | Cache |
| --- | --- | --- |
| `forward_npm_audit` | `npm/read.rs:286` | none |
| `forward_get` (Go vuln DB) | `goproxy/vuln.rs:164-188` | none |
| NuGet vulnerability index/pages | `nuget/vuln.rs` | none |

So `npm audit`, `govulncheck` and `dotnet list package --vulnerable` all fail
outright the moment their upstream is unreachable — including when BatleHub
answered the identical request thirty seconds earlier.
`docs/use/vulnerability-proxy.md` presents all three as proxied features. A
vulnerability check that fails closed on upstream loss is not merely degraded:
it is the check most likely to be running in the pipeline that must not stop.

This is the same defect as §5.1's stub, from the other side. There, a route
answered without asking upstream when it should have asked. Here, a route asks
upstream and can do nothing else.

**The rule.** Every endpoint in this RFC that calls upstream goes through the
cache-first / stale-on-error path, with the same three rungs §7.7 spells out for
search:

```text
1. cached response          → serve it
2. upstream, then cache it  → serve it
3. upstream unreachable     → stale cached response, when serve_stale allows
                              (and for search only, the held-package set)
```

Rung 3 is bounded by the registry's existing `serve_stale` flag rather than a new
one, so an operator who has turned stale serving off — because for their estate a
stale answer is worse than none — gets that decision honoured on the new
endpoints too, without having to discover a second switch.

Two documents are deliberately exempt, and the exemption is the reason rather
than an oversight:

- **The Go checksum database** (§7.4) is a signed transparency log whose whole
  purpose is that the client verifies it against a signature. Caching it is
  *sound* — the signature travels with the bytes — and it is added, but it is a
  byte cache, not a document cache: nothing parses or filters it, for the same
  reason `DocumentBody` has no binary variant
  (`ports/registry/client.rs:36-39`).
- **npm publish, and every other write.** A write is not a read and has nothing
  to serve stale. Upstream loss on a publish is an error, correctly.

**Enforcement.** A test asserting that no handler under `handlers/proxy/`
constructs a bare outbound request — the passthroughs route through one helper
that owns the three rungs, and the helper is the only caller of `reqwest` in that
tree. That makes the next passthrough inherit the behaviour by having nowhere
else to go, which is the only enforcement that survives a contributor who has not
read this document.

---

## 5. Mechanism 1 — protocol conformance fixtures

A test whose paths are copied from the client, not from us.

`crates/web/tests/protocol_conformance.rs`: one table per ecosystem of literal
request lines, each with the source it was taken from, run against a full
in-process app (`common::make_app`) and asserted to **route** — any status but
404-from-no-route. It asserts nothing about the body. It is a routing conformance
test, and routing is exactly what we got wrong five times.

```rust
const NPM: &[Conformance] = &[
    // npm/lib/commands/audit.js — the two audit endpoints
    Conformance::post("/-/npm/v1/security/audits/quick"),
    Conformance::post("/-/npm/v1/security/advisories/bulk"),
    Conformance::get("/-/v1/search?text=express"),
    Conformance::get("/-/package/express/dist-tags"),
    Conformance::get("/-/whoami"),
    Conformance::get("/-/ping"),
    …
];
```

Route registration order is a live hazard in this codebase — `lib.rs:775-781`
carries a paragraph about `api/{namespace}/{extension}` swallowing
`api/plugins/{id}`, and `:739-743` about `vscode/item` being taken for
`{name}/{version}`. A conformance table is the only thing that catches a
regression there, because a misordered route returns a *wrong handler's* answer,
not an error.

**The ratchet.** Landing the table red would leave the tree red, which this
repository does not do. So each entry carries an optional `not_yet: &'static str`
naming the phase that will serve it:

```rust
Conformance::get("/versions").not_yet("RFC 0009 §6.3 — phase 2"),
```

`not_yet` entries assert the *opposite* — that the path 404s. When a phase lands
its endpoint, the test fails until `not_yet` is deleted. The list only shrinks,
and it is a published inventory of what we do not serve, which is the artifact
the survey had to be written by hand to produce.

### 5.1 Routing is not behaviour — the second assertion class

The above is not sufficient, and the way we found out is worth recording.

Resolving §11's search question turned up `nuget_search`
(`nuget/search_publish.rs:103`): in proxy and hybrid mode it returns a hardcoded
`{"totalHits": 0, "data": []}`, with the comment *"Return minimal empty response
so dotnet CLI functions without error."* Meanwhile `service_index.rs:57-63`
advertises `SearchQueryService` and `SearchQueryService/3.5.0` pointing at it. So
the service index tells every `dotnet` client "I support search" and the route
always answers "nothing found".

**A conformance fixture as designed above would pass on that route.** The path
routes. It returns 200. It is valid JSON of the right shape. Every signal the
fixture reads is green, and the feature is dead.

That is a distinct failure from the five in §2.1 — not an endpoint at the wrong
address, but an endpoint at the right address that means nothing — and it is
harder to see, because 404 is loud and an empty collection is not. `vsx` has the
same stub for free-text search (`vsx/source.rs:151`), though that one is
honestly documented in a comment ending "Stated here rather than silently
returning empty", which is exactly the right instinct and is why it does not
appear in the survey as a surprise.

So the table carries a second assertion class:

```rust
// Not "does it route" but "does it answer": seed a package, then require
// that the response names it. The only assertion that separates an
// implemented collection endpoint from one stubbed to 200.
Conformance::get("/nuget/v3/query?q=seeded-pkg").must_find("seeded-pkg"),
```

**The rule: any endpoint whose success response is a collection needs a
`must_find` case against seeded data.** An endpoint that can only be observed
returning an empty list is indistinguishable from a stub, by us and by the
client both.

This also settles what to do about the two existing stubs — they are not
"pre-existing behaviour we leave alone", they are §2.1's failure in its quieter
form, and §7.7 fixes them.

### 5.2 What neither assertion class catches

Added after §12 measured five clients against this file. Three defects survived
both classes because both reason about **paths**, and all three were correct
about the path:

- **A resource the client cannot select.** NuGet's search endpoint answered its
  own route perfectly while `dotnet package search` refused to issue a query,
  because the service index did not advertise the `@type` the client's resolver
  looks for.
- **A method the route does not accept.** conda probes an index with `HEAD`, and
  a `GET`-only route rejects it at the method guard *before the handler runs* —
  so the client concludes the document does not exist. `curl -X GET` served it
  perfectly throughout.
- **An auth boundary the client does not cross.** Terraform fetches a provider
  archive without the credentials it sent to the document naming it, so a gated
  URL is unreachable however correct it is.

A fixture table cannot find these, because a fixture is a path. What found them
was running the client. §12 is therefore not a one-off discharge but the only
check that covers this class, and the honest conclusion is that it belongs in CI
against real clients rather than in a table of strings.

**It now is.** Nine `tests/heavy/*.sh` suites drive the real client for every
ecosystem this RFC touches, in CI and nightly (§12.16). Writing them found six
more shipped bugs of exactly this class — including two the *fixtures added by
this RFC* had passed over, and one introduced by a fix in §12.4.

---

## 6. Mechanism 2 — generated endpoint references

`utoipa` already produces `ui/openapi.json`, every proxy handler is already
tagged (`tag = "proxy/npm"`), and `crates/web/tests/openapi_contract.rs` already
fails on a `200` without a `body`. The endpoint tables in
`docs/registries/*.md` are the one place that information is retyped by hand.

`task docs:endpoints` renders, per registry page, the method/path/summary table
for that page's tag, between markers:

```markdown
<!-- generated:endpoints:proxy/npm -->
| Method | Path | Description |
…
<!-- /generated:endpoints -->
```

`task docs:endpoints:check` fails on drift, wired into `task docs:design`
alongside the existing `docs:listing-coverage:check` and `docs:roadmap` checks.
Prose around the block stays hand-written — the generated part is the inventory,
not the explanation.

This is the mechanism that makes `rubygems.md` unable to advertise six routes
while nine exist, and it costs one Taskfile target because the spec is already
built.

---

## 7. Detailed design, per ecosystem

### 7.1 npm — the audit paths, and the rest of the CLI

**The fix.** Register npm's real paths and forward to the matching upstream path:

| Route | Upstream |
| --- | --- |
| `POST …/-/npm/v1/security/audits/quick` | `{upstream}/-/npm/v1/security/audits/quick` |
| `POST …/-/npm/v1/security/advisories/bulk` | `{upstream}/-/npm/v1/security/advisories/bulk` |

`forward_npm_audit` (`npm/read.rs:270`) currently interpolates one template
(`:284`); it takes the full upstream path instead. The two existing routes stay
as aliases — they cost nothing, some deployment may have scripted them, and
removing them is a separate decision from fixing the bug.

It also stops making its own `reqwest` call and goes through §4.2's helper, so an
audit is answered from cache when the advisory database is unreachable rather
than failing the pipeline it runs in. The audit request body is part of the cache
key: `npm audit` POSTs the dependency set, so two different projects asking on
the same registry are two different questions.

The four tests that assert the old paths (`proxy_npm_edge_cases.rs:76,88,101,183`,
`vuln_proxy_endpoints.rs:38,49,61,138,714`) keep working through the aliases, and
gain siblings on the real paths. `docs/registries/npm.md:123` and
`docs/use/vulnerability-proxy.md:80-85` are corrected.

**The rest.** `GET /-/v1/search` (filtered per §4), `GET`/`PUT`/`DELETE
/-/package/{pkg}/dist-tags[/{tag}]`, `GET /-/whoami`, `GET /-/ping`, `DELETE
/{pkg}/-rev/{rev}` (unpublish, gated by the same ownership rules as publish),
and the abbreviated packument (`Accept: application/vnd.npm.install-v1+json`) as
a `DocumentKind` alongside the full one, for the same cache-collision reason PyPI
needed two (RFC 0006 §13.4).

`dist-tags` is the sharp one: it is a map of tag → version, and a tag naming a
blocked version is a listing that hands the client a version it cannot have.
`best_latest` (`blocking/mod.rs:474`) already computes the repair.

npm login (`PUT /-/user/org.couchdb.user:{u}`, `POST /-/v1/login`) is **not**
added — BatleHub issues its own tokens and has an OIDC path; a second credential
issuer on the npm protocol is a security surface, not a coverage gap.

### 7.2 Terraform — both protocols, and the download gate

The decision taken for this RFC: **both**. They are not alternatives.

**Network mirror** (works under path routing, providers only):

```
GET /proxy/{reg}/{hostname}/{namespace}/{type}/index.json
    → { "versions": { "1.0.0": {}, "1.1.0": {} } }
GET /proxy/{reg}/{hostname}/{namespace}/{type}/{version}.json
    → { "archives": { "linux_amd64": { "url": …, "hashes": ["h1:…"] } } }
```

`url` is relative to the `{version}.json` document, so it points back at our own
artifact route by construction — the `X-Terraform-Get` problem does not arise on
this protocol at all. `index.json` is a per-provider listing and is filtered;
`{version}.json` names one version and is a 404 when that version is blocked.

`{hostname}` is validated against the registry's configured upstream and 404s on
mismatch (§11.1) — the segment exists so one mirror can serve several registries,
which we are not, and echoing it back unchecked would attach an
`example.com` provenance to a `registry.terraform.io` provider.

**Registry protocol** (requires host routing, modules and providers):

```
GET https://{host}/.well-known/terraform.json
    → { "modules.v1": "/v1/modules/", "providers.v1": "/v1/providers/" }
```

Discovery is host-rooted by the protocol, so it is served only when the request
arrives on a host bound to exactly one registry — `host_routed_registry(req)`
(`middleware/host_routing.rs:50`) returns `Some`. On a path-routed request the
route returns 404 with a body naming the constraint, because a
`.well-known` document that cannot say *which* registry it describes is worse
than none. RFC 0001 shipped, so this is configuration, not new machinery.

That also makes the source addresses legal: `tf.example.com/myorg/mycloud` is
the three segments Terraform requires, where
`batlehub.example.com/proxy/internal-tf/myorg/mycloud` (`terraform.md:135`) is
five and is rejected before a request is made.

Completing the registry protocol needs, beyond discovery:

- `GET /v1/modules/{ns}/{name}/{provider}/{version}` — module metadata, missing
  today; 404 when the version is blocked.
- `signing_keys` in the provider download response. Terraform verifies the
  provider zip's GPG signature against the keys the registry declares and
  refuses the provider when they are absent — so a private provider is currently
  unusable even with everything else correct. `crates/adapters/src/repo/openpgp.rs`
  already signs deb/rpm/pacman metadata with Ed25519; the provider upload flow
  gains an optional detached `.sig` plus the registry's public key, surfaced here.
- Module and provider list/search endpoints (`/v1/modules`, `/v1/providers`),
  filtered as multi-package listings.

**The download gate** (survey §2, RFC 0006 §13.6). `…/{version}/download/{os}/{arch}`
in proxy mode resolves the upstream's `X-Terraform-Get` and hands the client that
URL, so the bytes never pass through the proxy and no rule runs on them — a
blocked provider version is still downloadable by anyone who can read the
listing. The header is rewritten to our own artifact route, which already exists
(`terraform/providers/read.rs:168`) and already goes through the gate in local
mode. This is the one item in this RFC that closes a *policy* hole rather than a
coverage one.

**Docs.** `docs/registries/terraform.md` is rewritten around the two protocols,
with the host-routing prerequisite stated where an operator reads it, and both
broken snippets (`:19-30` network mirror against registry-protocol routes,
`:82`/`:135` five-segment sources) replaced.

### 7.3 RubyGems — the compact index

Three documents, all `text/plain`, all `DocumentBody::Text`:

```
GET /versions          → gem versions md5      (whole registry, append-only)
GET /info/{gem}        → version deps checksum  (per package)
GET /names             → one gem name per line  (whole registry)
```

`/versions` and `/names` are whole-registry documents and filter through
`dispatch_multi` against the `blocks:{registry}` snapshot, exactly as conda's
`repodata.json` does (RFC 0006 §13.5) — including the same up-to-30-second lag,
which is acceptable for the same reason and must be documented for the same
reason.

`/info/{gem}` is per-package: a new `DocumentKind::COMPACT_INFO`, a
line-oriented filter in `blocking/rubygems.rs` next to the existing ones, and a
line-oriented parse — the same shape as the PyPI simple-page filter
(`blocking/pypi.rs:73`, "line-oriented rather than a DOM rewrite").

**One interaction worth stating.** Bundler fetches `/versions` incrementally with
HTTP `Range` and validates the result against the digest the server declares. A
filtered `/versions` changes when a block is added, so a client with a cached
prefix sees a digest mismatch and refetches the whole document. That is a
bandwidth cost on block changes, not a correctness problem, and it is the
behaviour Bundler already has for the upstream's own rewrites. We serve the
digest of what we send, never the upstream's.

`GET /api/v1/dependencies?gems=a,b,c` is added alongside — Bundler's middle
fallback, cheap once the compact index machinery exists, and it keeps older
Bundlers off `specs.4.8.gz`.

`listing_filter()`'s RubyGems entry gains the three compact-index documents as
`Filtered`. The Marshal entry stays `Unsupported`, and its reason string is
rewritten to be true: the JSON *and compact* APIs hide what it cannot, and no
current client reads it.

### 7.4 Go — the checksum database

```
GET /proxy/{registry}/sumdb/{sumdb-name}/{path:.*}
```

A byte passthrough, in the shape of the existing vuln passthrough
(`goproxy/vuln.rs:40-73`) but through §4.2's helper: `require_registry_type`,
resolve the configured sumdb base, forward, cache, serve stale on loss. No
filtering — the sumdb is a signed transparency log, editing it is neither
possible nor wanted, and `DocumentBody` deliberately has no binary variant for
exactly this class of document (`ports/registry/client.rs:36-39`).

Caching it is what makes the air-gapped case work at all: a sumdb lookup that
can only be answered while `sum.golang.org` is reachable has not removed the
egress, it has moved it. Cached, the second build needs no route off the site,
and the signature the client verifies is the upstream's own — so a cached
checksum record is exactly as trustworthy as a live one.

Configuration gains a `sumdb` field per goproxy registry defaulting to
`sum.golang.org`, and `""` to disable — a private-module-only registry has no
sumdb and should 404 rather than proxy a lookup that leaks module paths upstream.

`docs/registries/goproxy.md:31` currently documents `GONOSUMDB` for private
modules; it gains the public case, which is that no `GONOSUMCHECK` is needed at
all once the sumdb is proxied.

### 7.5 conda — compressed repodata and channeldata

`repodata.json.zst` and `repodata.json.bz2` as compressed encodings of the
document we already synthesise and filter. Filtering runs on the JSON, then the
result is compressed — so RFC 0006's guarantee carries over with no new filter.
Both are cached under their own artifact key so the compression is paid once per
TTL, not per request.

The `{filename}` route regex
(`conda.rs:220`, `{filename:.+\.(?:tar\.bz2|conda)}`) currently means a `.zst`
request falls through the whole route table rather than reaching a handler. The
two new routes are literal and registered before it.

`channeldata.json` is a whole-channel document used by `conda search` for
cross-platform discovery: a `dispatch_multi` filter next to
`conda::strip_repodata` (`blocking/mod.rs:210`).

### 7.6 The long tail

| Kind | Added |
| --- | --- |
| **cargo** | `GET /api/v1/crates?q=` (search, filtered); `PUT`/`DELETE /api/v1/crates/{name}/owners` — ownership is readable but not manageable today, and the local-registry ownership model already exists behind the admin API |
| **NuGet** | `SearchAutocompleteService` (+ the `autocomplete` route, filtered); `SymbolPackagePublish/4.9.0` (+ `.snupkg` accept — `nuget push` of symbols currently fails silently); `ReportAbuseUriTemplate` and `PackageDetailsUriTemplate` pointed at our own UI rather than left to fall back to nuget.org links, which for a private registry is a small information leak in CLI output |
| **Composer** | `GET /search.json` — the adapter already calls it upstream (`composer/impl_registry.rs:226`) with no route exposing it; `GET /list.json` |
| **PyPI** | `GET /pypi/{name}/json` and `/pypi/{name}/{version}/json` (filtered; a new `DocumentKind`); **PEP 658** `.metadata` siblings with `data-dist-info-metadata` / `data-core-metadata` on the simple page — uv and pip use them to resolve without downloading wheels, and their absence is a silent slowdown proportional to how much anyone uses the PyPI proxy |
| **openvsx / vscode-marketplace** | `sortBy`/`sortOrder` honoured rather than parsed and ignored; `filterType 12` (exclude-with-flags); the OpenVSX namespace endpoints `api/{namespace}`, `api/{ns}/{ext}/reviews`, `api/-/query`, `api/version`; and the OpenVSX publish API `api/-/publish` / `api/user/publish`, which is what `ovsx publish` calls — we accept only `PUT …/{ext}/{version}/vsix`, which no tool sends |

Everything in this table filters through the same obligation in §4. The openvsx
additions filter at the `vsx/source.rs` chokepoint, where the existing gallery
filter already sits (RFC 0006 §13.1-bis), rather than through `strip` — the
entries render into two protocols and a `(kind, document, package)` signature
cannot address them.

### 7.7 Search, across five ecosystems

Search is the one surface this RFC touches in five places at once — NuGet and
`vsx` have it stubbed today (§5.1), and npm, cargo and Composer gain it here —
so it gets one design rather than five.

**Three rungs, tried in order** — §4.2's rule, with one rung search adds:

```text
1. cached response for this query   → serve it
2. upstream, then cache the result  → serve it
3. upstream unreachable:
     a. stale cached response for this query, if any   → serve it
     b. otherwise: the packages this registry already holds
```

Rung 3b is the part that makes this different from the metadata path, and it is
the answer to "what should a search return when the upstream is gone": not an
error, and not an empty list, but **what we actually have**. A registry that has
cached four hundred packages can answer a search from those four hundred. That is
a true answer about this proxy, degraded but honest, and it is strictly better
than the empty 200 that ships today — which is a false answer about the upstream.

The held set is `PackageRepository` for that registry — the same store
`GET /api/v1/packages` reads — unioned with locally published packages in local
and hybrid mode. So rung 3b is a query against a table we already maintain, not a
new index.

**None of the three rungs is new machinery.** Rung 1 and 2 are the metadata
cache under a `search:{registry}:{query}:{limit}` key. Rung 3a is
`cache.get_stale` (`proxy/handle.rs:695`) gated by the registry's existing
`serve_stale` flag (`config/schema/registry.rs:436`, default true) — the same
policy that already governs stale metadata, so an operator who has turned stale
serving off for correctness reasons gets that decision honoured here too. Rung 3b
is the only genuinely new rung, and it is a `list_package_names`-shaped query.

**Mode behaviour** follows the rule the rest of the codebase already uses:
proxy mode runs all three rungs; local mode is rung 3b only (there is no upstream
to ask); hybrid merges upstream results with the local set, deduped by
coordinate. `RegistryClient::search_packages` already exists for rung 2
(`ports/registry/client.rs:231`) with a default implementation returning empty,
so kinds with no upstream search API opt out for free.

**Egress is a fact operators must be told.** Rung 2 forwards the user's query
string to the upstream registry. That is what makes search useful and it is not
free: search queries are a record of what an organisation is looking for. It is
documented on each registry page and in `docs/guide/caching.md`, not buried —
and `serve_stale = false` plus an unreachable upstream is the configuration for
an operator who wants rung 3b and nothing else.

**Degradation is observable.** Falling to rung 3 marks the registry through the
existing counters (`services/metrics.rs:152-161`, `batlehub_upstream_health_degraded`),
so a search silently answering from the held set shows up on the same gauge and
alert path as stale metadata rather than being invisible. Responses served from
rung 3 carry a header naming the degradation, so the UI can say so instead of
presenting a short result list as complete.

**Filtering** is §4's obligation: blocked versions are removed from each hit, a
hit is dropped when every version is blocked, and the reported total is adjusted
to match — because the client paginates by offset and a silently shortened page
is its own bug. Cached search responses are filtered on read, not on write, so a
block takes effect immediately rather than at TTL expiry — the same rule RFC 0006
§4.2 sets for every other document.

**The service index stops lying.** NuGet advertises `SearchQueryService` only
when search can actually answer — which, with rung 3b, is always, so the
advertisement becomes true rather than being withdrawn.

---

## 8. What this does not change

The seams RFC 0006 built are sufficient for all of the above, and this RFC adds
no new ones:

- `VersionDocument`/`DocumentBody` gain no variants. Every new document is JSON
  or text. The deliberate absence of a binary variant
  (`ports/registry/client.rs:36-39`) is preserved, and the two documents that
  might have tempted one — the Go sumdb and conda's compressed repodata — are a
  passthrough and a transport encoding respectively, neither of which is a
  document the filter sees.
- `ProxyService::handle`'s authorise → resolve → filter → stream order is
  unchanged.
- `validate_coordinate` / `ensure_safe_key` remain the two funnels, and every new
  handler that builds a storage key validates at the edge for a clean `400`, per
  `CLAUDE.md`'s adding-a-registry rule 3. The Terraform network mirror is the one
  new path that takes a `{hostname}` segment from the request, and it is
  validated against the configured registry rather than trusted.

---

## 9. Phasing

Each phase leaves the tree green: builds, clippy clean, tests pass, and the
conformance ratchet strictly shrinks.

| Phase | Content | Closes |
| --- | --- | --- |
| 0 ✅ | Both mechanisms. `protocol_conformance.rs` with the full client-path inventory and `not_yet` entries for everything unserved; `task docs:endpoints` + `:check` wired into `docs:design`; `listing_filter()`'s `ListingDocument` gains its `DocumentKind` and the reachability test extends to pairs. No behaviour change. | — |
| 1 ✅ | The shared upstream-call helper of §4.2 — three rungs, `serve_stale`-gated, the only `reqwest` caller under `handlers/proxy/` — and its first three callers: the npm audit paths (fixed and cached), the Go vuln DB, and the NuGet vulnerability documents. Both docs pages corrected. | §7.1 (audit), §4.2 |
| 2 ✅ | RubyGems compact index, `/names`, `/info/{gem}`, dependency API; `dispatch_multi` generalised from conda-only to a real dispatch table; `listing_filter()` RubyGems entry rewritten. | §7.3 |
| 3 ✅ | conda `.zst`/`.bz2`/`channeldata.json`. | §7.5 |
| 4 ✅ | Go sumdb passthrough + config field, through phase 1's helper as a byte cache. | §7.4 |
| 5 ✅ | Terraform: network mirror, `.well-known` discovery, module metadata, `signing_keys`, `X-Terraform-Get` rewrite, list/search, docs rewritten. | §7.2 |
| 6 ✅ | Search, once, for all five ecosystems: the three rungs, the `search:` cache key, rung-3b against `PackageRepository`, degradation header + gauge, result filtering with adjusted totals. Un-stubs NuGet and `vsx`; lands the npm, cargo and Composer routes on the same path. | §7.7, §5.1 |
| 7 ✅ | The rest of the long tail: cargo owners write, NuGet symbol publish + service-index templates, Composer `list.json`, PyPI JSON API + PEP 658, openvsx namespace/publish endpoints and `sortBy`/`filterType 12`. | §7.6 |
| 8 ✅ | npm's remaining CLI surface — dist-tags, whoami, ping, unpublish, abbreviated packument. | §7.1 (rest) |
| 9 ✅ | Docs: regenerate every endpoint table, refresh `ui/openapi.json` + the TypeScript client, mark the survey closed, add the conda snapshot-lag note for `/versions` and `channeldata.json`, and document search egress on each registry page. | — |

Phase 0 is a prerequisite for the ratchet but not for the fixes; phases 1–4 are
independent of each other and each closes one finding. Phase 5 is the largest and
the only one that touches configuration and host routing. Phase 2 is the one that
closes a live policy hole, and phase 5 closes the other.

**Ordering rationale.** Phase 1 first because it is the smallest diff for a
feature we document as working. Phase 2 second because it is a block leak, not a
coverage gap. Phases 3 and 4 are mechanical. Phase 5 is deferred not because it
matters least but because it is the only one with a design decision already
spent, and it wants the conformance harness from phase 0 to have proven itself on
four easier ecosystems first.

---

## 10. Risks

**The conformance table is only as good as its sources.** It encodes what we
believe the client calls. If the belief is wrong, the table is confidently wrong
in the same way the old tests were — with a green build. Mitigation: every entry
carries a comment naming where the path was read from (client source file,
protocol spec section), so a reviewer can check the claim rather than the code.
The paths in this RFC's phase 1 and 2 should be verified against a real `npm
audit` and `bundle install` transcript before those phases land — the survey
verified our side against the route table, not the client's side against a
client.

**Generated docs tables lose their editorial voice.** A generated inventory is
worse prose than a hand-picked table. Mitigation: only the table is generated;
the surrounding explanation, which is the part worth reading, stays hand-written,
and the marker comments make the boundary obvious to the next editor.

**Six kinds newly using `dispatch_multi` all pay the snapshot.** conda's
30-second `blocks:{registry}` lag becomes RubyGems', cargo search's, npm search's
and Composer's too. That is the documented trade (RFC 0006 §13.5) — the listing
lags, the download gate does not — but it now applies to the default install path
of two more ecosystems, and `docs/guide/admin-policies.md` must say so rather
than describing it as a conda quirk.

**Aliased npm audit routes are surface with no caller.** §3 calls that an
anti-pattern and §7.1 then does it. The justification is that they are already
shipped and removing them is a breaking change to any deployment that scripted
them; they are marked deprecated in the generated table and removed in a later
release, not kept indefinitely.

---

## 11. Resolved questions

All three questions this RFC opened are settled. They are kept here with their
resolutions rather than deleted, because two of them were settled by finding that
the codebase had already answered them and one changed the design.

1. **Does the Terraform network mirror need per-registry `{hostname}`
   validation?** — **Yes, validate and 404 on mismatch.** The mirror protocol
   puts the *original* registry hostname in the path so one mirror can serve
   several registries; we have one upstream per registry, so the segment is
   redundant. Redundant is not the same as ignorable: echoing it back would serve
   `registry.terraform.io` providers under an `example.com` path, which is a
   provenance claim we would be making up. It is also the one new path segment in
   this RFC taken from the request and used to address an upstream, so §8's
   validate-at-the-edge rule applies to it directly.

2. **Should search proxy upstream or answer from our own index?** — **Both, in
   three rungs, with a third rung neither option originally had.** See §7.7. The
   original proposal was "proxy in proxy mode, local in local mode" and it was
   incomplete: it said nothing about what happens when the upstream is
   unreachable, which for a proxy is not an edge case but the condition the
   product exists to survive. The resolution is cache-first, stale-on-error, and
   then degrade to the packages this registry already holds — so an air-gapped or
   upstream-down deployment answers search from what it has instead of returning
   an empty list that reads as "no such package". Every rung but the last is
   existing machinery.

   This is also what closes the two stubs in §5.1, which were the same question
   answered by not answering it.

3. **What storage key shape do symbol packages (`.snupkg`) take?** — **Same
   coordinate, `snupkg` sub-coordinate.** `PackageId::with_artifact` is the
   established pattern for a second artifact under one coordinate — `vsix` for
   both extension kinds, `plugin` for JetBrains Marketplace, both recorded in
   `RegistryKind::warm_artifact()`. Symbol *servers* are conventionally separate
   infrastructure, but that is a deployment convention of the ecosystem, not a
   property of the artifact, and splitting the coordinate would put a package's
   symbols outside every policy that addresses the package. Symbols are
   deliberately not warmed: `warm_artifact()` names one artifact per kind, and
   for NuGet that stays the `.nupkg`.

## 12. Verification against real clients

The conformance table encodes what we *believe* each client sends, read from
protocol documentation. Our side was checked against the route table; the
client's side is a different claim, and a fixture that encodes the wrong path is
confidently wrong in exactly the way the bug it replaced was.

This section originally said the check was "the one thing that should happen
before phase 1 lands". Phase 1 landed, and so did phases 2 through 9, without
it — which is worth leaving on the page rather than quietly editing, because
that is the same drift between a document and its project that the rest of this
RFC is about.

The subsections below are the transcripts, kept as written. §12.16 is what
happened when they were turned into scripts that run on every push.

### 12.1 npm — discharged

Captured from **npm 11.17.0** against a logging registry, with a real lockfile:

| Command | Path observed |
| --- | --- |
| `npm audit` | `POST /-/npm/v1/security/advisories/bulk` |
| `npm ping` | `GET /-/ping` |
| `npm whoami` | `GET /-/whoami` |
| `npm search` | `GET /-/v1/search?text=…&size=…&from=…` |
| `npm dist-tag ls` | `GET /-/package/{pkg}/dist-tags` |
| `npm view` | `GET /{pkg}` |

All six match the fixtures. Two things the reading had right and one it had
wrong:

- **Right, and load-bearing:** the audit path really is
  `/-/npm/v1/security/advisories/bulk`, not the `/-/npm/v1/audit/bulk` this
  server shipped. The finding this RFC opens with is confirmed against the
  client rather than argued from documentation.
- **Right by luck:** `npm search` sends `text` and `size`, which is what
  `NpmSearchQuery` guessed. It also sends `from`, `quality`, `popularity` and
  `maintenance`, which we ignore — correct, but not something the reading
  predicted.
- **Wrong:** §2.1 and the survey both claimed npm "falls back to quick" when the
  bulk endpoint 404s. **It does not.** npm sends the bulk request, gets the 404,
  prints `npm error audit endpoint returned an error` and **exits 1**. One
  request, no fallback.

That last one makes the original defect *worse* than reported: `npm audit`
against a pre-fix BatleHub does not degrade to a quiet no-op, it fails the
build. Both documents are corrected.

`/-/npm/v1/security/audits/quick` was **never observed**. It is still served —
older npm and other clients may send it, and the route costs nothing — but no
part of this RFC's evidence covers it, and the fixture says so.

### 12.2 RubyGems — discharged

Captured from **Bundler 4.0.17 / Ruby 3.3.12**, installed through
`examples/rubygems/.mise.toml`, resolving against a logging registry at the
source URL that example already configures.

**With a compact index served**, a complete resolution is exactly two requests:

```text
GET /versions      [range: bytes=80-]
GET /info/rack     [range: bytes=84-]
```

and it produces a valid `Gemfile.lock`. Nothing else is touched — not
`/api/v1/dependencies`, not `/specs.4.8.gz`, not the versions JSON API. §2.2's
premise is confirmed: **the compact index is what Bundler resolves from**, and
phase 2 was aimed at the right documents.

Three things the reading did not have right:

- **There is no fallback chain, on this Bundler.** With the compact index
  `404`ing, Bundler issues **one** request to `/versions` and stops:
  `Could not find gem 'x' in rubygems repository`. It never tries the dependency
  API or `specs.4.8.gz`. §2.2 said it did, and §2.2 is corrected — the fallback
  chain is Bundler 2.x behaviour, not Bundler 4's.
- **So the severity was mischaracterised.** The pre-phase-2 failure was not a
  silent leak of blocked versions through an unfiltered Marshal index; on
  Bundler 4 it was `bundle install` failing outright. The leak reading holds for
  Bundler 2.x, which is not measured here. Either way phase 2 is the fix, but
  "makes Ruby work at all" is the honest headline for the current client.
- **Bundler fetches both documents with `Range: bytes=N-`**, appending to a
  local cache under `~/.bundle/cache/compact_index/`. §7.3 anticipated this for
  `/versions` and not for `/info/{gem}`, which does it too.

That last point was written as a warning to whoever optimised this next, and it
was half right. BatleHub answered `200` with the whole document and ignored
`Range`, so Bundler replaced its cached copy every time — correct, and slightly
wasteful. **Serving `206` with a byte range *as stated here* would have been a
correctness bug**: the offset the client is asking from was computed against a
document filtered under a *different* blocked set, so appending our bytes to its
prefix would splice two different documents together.

What the warning missed is that the client hands us the means to check. Bundler
sends `If-None-Match` alongside the `Range`, and the validator it sends is the
`ETag` we issued with the bytes it holds — an opaque value it read back out of
its own etag file, so the algorithm is ours to choose (a SHA-256 since
2026-09-15). So "is the client's prefix our prefix" is answerable rather than
assumable, and a `206` is served only when it is provably yes. §13.24 has the implementation and
the two things measurement corrected in it.

### 12.3 Terraform — discharged

Captured from **Terraform 1.8.5**, installed through
`examples/terraform/.mise.toml`, running `terraform init` against a logging
mirror.

A complete provider install is three requests:

```text
GET {mirror}/registry.terraform.io/hashicorp/random/index.json     [auth]
GET {mirror}/registry.terraform.io/hashicorp/random/5.40.0.json    [auth]
GET {mirror}/terraform-provider-x.zip                              ← no auth
```

**The path shapes are confirmed** — `{hostname}/{namespace}/{type}/index.json`
and `…/{version}.json`, exactly the routes phase 5 registered.

**The relative-URL reasoning is confirmed, and it was the risky part.**
§7.2 claims `url` in `{version}.json` "is relative to the document, so it points
back at our own artifact route by construction". Measured: a document at
`…/hashicorp/random/5.40.0.json` returning `url: "../../../x.zip"` produced a
fetch of `{mirror}/x.zip`. That is the arithmetic
`terraform_mirror_version` relies on for
`../../../v1/providers/{ns}/{type}/{ver}/artifact/{os}/{arch}`, and it resolves
where intended. Derived by hand in phase 5 and now checked.

Two findings the reading did not have, both operator-facing:

- **A network mirror must be an `https:` URL.** Terraform rejects plain HTTP
  outright: *"the mirror must be at an https: URL"*. `examples/terraform/.terraformrc`
  shipped `http://localhost:8080/…`, so its `terraform init` could never have
  worked. Corrected, with the constraint written into the file.
- **Terraform does not authenticate the archive download.** It sends the
  `credentials` token to `index.json` and `{version}.json` and then fetches the
  provider zip **with no `Authorization` header**. BatleHub points that URL at
  `…/artifact/{os}/{arch}`, which runs the rule chain — so on a registry that
  does not grant anonymous read, provider installation fails at the last step.

The second is the same shape as the VS Code gallery, which
`vscode-marketplace.md` already documents ("the editor sends no credentials to
its gallery"), and it takes the same answer: the registry needs `anonymous`
read or an authenticating ingress. Recorded on the registry page rather than
left for an operator to hit.

It is worth being clear that this is a **limitation, not a fix**. Serving the
archive from an ungated route would defeat the download gate phase 5 exists to
close, and minting a signed URL inside `{version}.json` would be a new auth
mechanism invented for one client. Neither is in scope here; the constraint is
stated instead.

### 12.4 NuGet and conda — discharged, two shipped bugs each

**NuGet**, from **dotnet 10.0.400** against a logging v3 registry.

`dotnet add package` follows `RegistrationsBaseUrl/3.6.0` and requests
`{base}/{id-lowercase}/index.json` — the exact resource type and path shape
BatleHub serves. `dotnet package search` sends:

```text
GET /nuget/v3/query?q=Newtonsoft&skip=0&take=20&prerelease=false&semVerLevel=2.0.0
```

Two defects, both shipped:

- **The search endpoint was unreachable.** With exactly the resource types
  BatleHub advertises — bare `SearchQueryService` plus `SearchQueryService/3.5.0`
  — `dotnet package search` answers *"The source does not have a Search
  service!"* and never issues a query. Bisected: it selects
  **`SearchQueryService/3.0.0-beta`**, which we did not advertise. So phase 6
  un-stubbed an endpoint that the client still could not find, and phase 7 added
  autocomplete with the same omission. Both types are now advertised.
- **`skip` was ignored.** The client paginates with `skip=0&take=20`, then
  `skip=20`. `SearchQuery` parsed only `q` and `take`, so every page returned the
  same first results — which reads as "this registry has twenty packages".
  §7.7 worried about exactly this ("clients paginate by offset") and adjusted
  totals without honouring the offset.

Also observed: **NuGet refuses a plain-HTTP source** unless the entry carries
`allowInsecureConnections="true"`. Unlike Terraform's mirror there is an opt-out,
but a local HTTP instance needs it.

**conda**, from **micromamba 2.9.0** against a logging channel. A single
`create` probes:

```text
HEAD /{subdir}/repodata.json.zst              ← .zst first, as §7.5 claims
HEAD /{subdir}/repodata_shards.msgpack.zst
GET  /{subdir}/repodata.json                  ← only after the probes 404
```

§7.5's premise is confirmed: `.zst` is asked for first. Two findings:

- **Phase 3's `.zst` work was unreachable.** conda probes with **`HEAD`**, and
  actix does not route `HEAD` to a `GET` handler — the request matches the
  resource pattern and is then rejected by the method guard *before the handler
  runs*, producing a bodyless `404`. So a real conda client concluded the
  compressed document did not exist and fell back to plain `repodata.json`,
  exactly as before phase 3. `curl -X GET` served it perfectly, which is why
  every test passed. The five conda index routes now answer `GET` and `HEAD`,
  with a regression test.
- **`repodata_shards.msgpack.zst` is not implemented.** Modern conda/mamba probe
  for a sharded repodata format BatleHub has never served. It falls back
  cleanly, so this is a performance gap rather than a break — but it is a real
  coverage gap that no amount of reading the older protocol would have found.

The `HEAD` finding is the sharpest thing this whole verification produced. It is
a *third* failure mode, distinct from both of §2.1's: not an endpoint at the
wrong address, and not a stub returning nothing, but a correctly-implemented
endpoint at the right address that the client's actual request method never
reaches. No fixture asserting paths — including this RFC's own — would have
caught it, because the path was right.

### 12.5 Composer — discharged, one more shipped bug

`mise`'s PHP backends both compile from source and need `autoconf`/`bison`/`re2c`,
which cannot be installed without root here. A **static `php-cli` 8.3.28** from
`dl.static-php.dev` plus `composer.phar` gave a working client without a build.

**Composer 2.10.2**, resolving against a logging repository configured the way
`examples/composer/composer.json` configures one. With exactly the fields
BatleHub's `packages.json` served, a resolution is:

```text
GET /packages.json                       [auth]
GET /p2/monolog/monolog.json             [auth]
GET /p2/monolog/monolog~dev.json         [auth]
```

All three are BatleHub routes, and the `~dev` variant confirms
`DocumentKind::P2_DEV` models something the client really asks for.

**`composer search` never reached `search.json`.** Composer discovers every
endpoint except `packages.json` itself from a **URL template** in that document,
and BatleHub advertised only `metadata-url` and `available-packages`. So
`composer search` answered from the cached `available-packages` list and made no
request at all, and `composer_list` was equally unreachable — phase 6's search
route and phase 7's list route both shipped correct and undiscoverable.
Re-running with a `search` template advertised produced
`GET /search.json?q=monolog&type=`, exactly the route that already existed.
`packages.json` now advertises both.

This is the **second** instance of the same defect: NuGet's search was
unreachable for want of a resource `@type`, Composer's for want of a URL
template. Neither is a path error, and the conformance table asserts paths.

Also observed: **Composer refuses plain HTTP** unless `config.secure-http` is
`false` — the third of five clients to require TLS, after Terraform (no opt-out)
and NuGet (`allowInsecureConnections`).

### 12.6 Open VSX — discharged, and §7.6's claim confirmed

**ovsx 1.1.1**, against a logging Open VSX registry.

`ovsx get` is two requests, both BatleHub routes:

```text
GET /api/{namespace}/{extension}
GET /api/{ns}/{ext}/{version}/file/{filename}     ← followed from `files.download`
```

The second is reached by following the `download` URL in the first, which is why
`vsx/render.rs` rewriting that field to point at this proxy is load-bearing
rather than cosmetic — an unrewritten one sends the client to upstream for the
bytes, the same hole phase 5 closed for Terraform.

**`ovsx publish` sends `POST /api/-/publish?token=…`** and got a `404`. §7.6
said exactly that — "the OpenVSX publish API is what `ovsx publish` calls; we
accept only `PUT …/{ext}/{version}/vsix`, which no tool sends" — and it is the
one claim in this RFC that measurement *confirmed* rather than corrected. It was
also still unimplemented after phase 7, which listed it and did not do it.

Now served, with two properties the URL forced:

- **The URL carries no coordinate.** Extension id and version come from the
  VSIX's own `extension/package.json`. `vsix_publish` prefers the URL when the
  two disagree; here there is nothing to prefer, so an unreadable manifest is a
  `400` rather than a degraded publish with invented metadata.
- **The token arrives as a query parameter**, not an `Authorization` header —
  something no amount of reading the API docs suggested. The auth middleware
  reads the header, so a bare `ovsx publish` currently arrives anonymous and
  needs a registry that permits it, or `--registryUrl` with credentials in the
  usual header. Teaching the middleware about `?token=` is a change to an
  authentication path and is done in §13.23, scoped to this one route.

**The VS Code gallery half is not verified.** `code` on this machine is
`/checode/.../remote-cli/code`, a remote shim whose extension handling does not
match a local editor, so pointing it at a gallery would measure the shim. The
end-to-end proof already exists as `tests/heavy/marketplace.sh`, which drives a
real editor against a real instance — that is where the gallery claim belongs,
and it needs Postgres and CI rather than this environment.

### 12.7 PyPI — measured, and it was a hard failure

Not planned as a verification target; `pip` 26.1.2 was installed, so it was
cheap. It found the worst defect of the set.

BatleHub's simple-page rewrite touches `href` and nothing else, so upstream's
**`data-core-metadata`** attribute survives onto our page. pip trusts it,
requests `{file}.metadata` from us, and gets a `404` — and **does not fall back
to downloading the wheel**:

```text
ERROR: 404 Client Error: Not Found for url: …/packages/probe-1.0.0-py3-none-any.whl.metadata
```

§7.6 called PEP 658 "a silent slowdown rather than an error". It is an error,
and against a real PyPI upstream — where essentially every modern wheel
advertises the attribute — it would break `pip install` outright.

Now served: the sibling resolves to its distribution's coordinate with the full
filename as the artifact sub-coordinate, and the adapter appends `.metadata` to
the CDN URL it already resolves. Same gate, same cache, one extra suffix.

### 12.8 Terraform's checksums, closed

§13.14 left `shasums_url` and `shasums_signature_url` pointing upstream: the
provider *archive* was gated, its checksum manifest was not. Terraform verifies
the archive against those files, so an otherwise air-gapped install reached the
internet at the last step — and failed there.

Both now point at `…/{version}/shasums` and `…/shasums.sig`. The URLs are named
*inside* the download document rather than addressed by a path, so the adapter
resolves that document and follows the field, with the same SSRF treatment the
module tarball gets. An air-gapped provider install is now complete.

### 12.9 Score

Seven ecosystems verified against their real clients, and every one produced at
least one correction:

| Ecosystem | Client | Corrections |
| --- | --- | --- |
| npm | 11.17.0 | audit has no quick fallback; it exits 1 |
| RubyGems | Bundler 4.0.17 | no fallback chain; severity mischaracterised; `Range` on both documents |
| Terraform | 1.8.5 | mirror must be https; archive fetched unauthenticated |
| NuGet | dotnet 10.0.400 | search type missing so the endpoint was unreachable; `skip` ignored |
| conda | micromamba 2.9.0 | `HEAD` probe never reached the handler; sharded repodata unserved |
| Composer | 2.10.2 | search/list unreachable — never advertised in `packages.json`; `available-packages` made proxy and hybrid resolve nothing (§12.10) |
| Open VSX | ovsx 1.1.1 | `ovsx publish` 404'd; token arrives as a query parameter |
| PyPI | pip 26.1.2 | PEP 658 metadata advertised and unserved — a hard failure, not a slowdown |

**Seventeen corrections, twelve of them shipped bugs.** Six were found by the
per-ecosystem probes above; six more only by running the fixes back against a
real server (§12.10–§12.15):

| # | Shipped bug | Found in |
| --- | --- | --- |
| 1 | NuGet's search unreachable — no `SearchQueryService` type | §12.4 |
| 2 | Composer's search and list unreachable — never advertised | §12.5 |
| 3 | conda's `.zst` unreachable — `HEAD` reached no handler | §12.4 |
| 4 | Terraform's provider archive fetched unauthenticated | §12.3 |
| 5 | Open VSX's publish endpoint absent | §12.6 |
| 6 | PyPI advertised PEP 658 metadata and did not serve it | §12.7 |
| 7 | Composer's `available-packages` made proxy and hybrid resolve nothing | §12.10 |
| 8 | Terraform discovery unreachable on the only hosts it serves | §12.11 |
| 9 | Terraform's download document was the versions listing | §12.12 |
| 10 | Terraform's provider archive was that JSON document | §12.12 |
| 11 | conda's compressed channel pinned to its pre-publish contents | §12.13 |
| 12 | RubyGems' compact index proxied upstream from a local registry | §12.15 |

Every one passed every test in this
repository, including the conformance fixtures this RFC added, because every one
is correct about *paths* and wrong about something else: a resource type, a URL
template, a request method, an auth boundary, a document's shape, a cache key.

Several are the **same defect wearing different clothes** — an endpoint the
client is never told about, or told about and cannot use. NuGet needed a
`@type`, Composer a URL template, conda a method it would accept, Terraform a
route at the path its own middleware produces. Each was implemented, tested,
correct, and dead.

The other family is newer and worse: a document that is **the wrong document**.
Terraform's download endpoint answered with a listing, its archive endpoint with
that listing's replacement, conda's `.zst` with a channel from before the
publish, and RubyGems' compact index with a different registry entirely. Each of
those returned `200`, well-formed, to the right URL. No status code, schema
check or route assertion catches any of them — only a client that tried to use
the answer.

### 12.10 The re-verification, and the seventh bug

Everything above was found by measuring a real client against a *mock* shaped
like BatleHub, and then changing BatleHub to match the mock. That is a sound
inference and it is not the same as checking, so each fix was re-run against a
**real server** — the release binary, the live Postgres, seven local registries
and one proxy registry — with a transparent logging proxy in front of it so the
client's request sequence stayed visible.

Two things had to be got right before the transcript meant anything. The tap
originally rewrote the `Host` header, and since the server builds its absolute
URL templates from that header, every request after the first went straight to
the server and the tap saw nothing. And Composer adds `packagist.org` to every
project implicitly, so a search that BatleHub never answered still returned
results. Both are the same hazard this RFC keeps meeting: a green result
produced by something other than the thing under test.

The run confirmed the fixes and found a seventh shipped bug, larger than the six
before it.

**`available-packages` is a claim of completeness.** Composer treats it as
authoritative: a package absent from the list is not requested, whatever
`metadata-url` would have answered. BatleHub sent it in every mode.

- **proxy** sent `[]` — "this repository is empty". Measured against Composer
  2.10.2: a `composer update` requesting `monolog/monolog` fetched
  `packages.json`, stopped, and reported *"could not be found in any version"*.
  It never requested `p2/`. **A proxy-mode Composer registry could not resolve
  anything at all** — the entire purpose of the mode.
- **hybrid** sent the locally published packages only — "upstream's packages do
  not exist here", so it could serve what it hosted and nothing it proxied.

Only `local` mode can honestly make the claim, and now only `local` mode makes
it. Omitted, Composer falls back to per-package `metadata-url` requests, which
is exactly what a proxy can answer. Against the real server afterwards:

```
GET /proxy/proxy-composer/packages.json              -> 200
GET /proxy/proxy-composer/p2/monolog/monolog.json    -> 200
GET /proxy/proxy-composer/p2/psr/log.json            -> 200
GET /proxy/proxy-composer/search.json?q=monolog&type= -> 200
```

`composer update` locked `monolog/monolog 3.10.0` and `psr/log 3.0.2`;
`composer search monolog` returned results with Packagist disabled, so they came
through BatleHub. Line 4 is also §12.5's search fix, confirmed end to end rather
than against a mock.

This one is worth dwelling on, because a test was **pinning** it:

```rust
assert_eq!(body["available-packages"], serde_json::json!([]));
```

That assertion passed for as long as the bug existed, and would have failed the
moment it was fixed. It is the failure mode of §5.2 in its purest form — the
wire shape was pinned without anyone asking what the client *does* with it. The
replacement asserts the field is absent and says why, and a sibling test covers
hybrid.

One is the opposite failure and worth its own note: Open VSX's publish endpoint
was **named in §7.6, listed in phase 7, and never built**. No test failed,
because nothing tested for it — a planned item silently not done is invisible to
every mechanism in this RFC except running the client.

That is the honest verdict on §5's mechanism. It catches endpoints at the wrong
address, which is what it was built for and what it did catch. It does not catch
an endpoint the client cannot select, cannot reach with the method it uses, or
cannot authenticate to. Those need the client.

A `source` beginning `"observed"` in `protocol_conformance.rs` marks a captured
path; everything else is a reading. That distinction is now visible per entry
rather than stated once here.

---

### 12.11 Terraform discovery, unreachable where it exists

`/.well-known/terraform.json` answers only on a host bound to one registry
(§7.2). The host-routing middleware rewrites every request on such a host to
`/proxy/{registry}{path}` **before** routing. So the address the protocol
fetches is never the address that arrives, and what arrived —
`/proxy/tf/.well-known/terraform.json` — was claimed by the npm/cargo catch-all
`/proxy/{registry}/{package}/{version}`, which replied *"registry 'proxy-tf' is
not an npm or cargo registry"*.

Host routing is the one condition under which discovery answers, and the one
condition under which its route cannot match. It was unreachable everywhere it
was meant to work, and reachable nowhere else.

The rewritten path is now registered too, above the catch-all, with the same
`host_routed_registry` guard — so a direct request on a shared host still gets
the 404 that explains what to configure. Five conformance fixtures now pin the
request sequence Terraform actually sends.

### 12.12 …and then two more, in the same install

With discovery reachable, Terraform 1.8.5 got further and stopped twice more.

**The download document was the versions listing.** The handler asked
`version_document` for `DocumentKind::Versions` and patched URLs into whatever
came back, so a request for one platform of one version was answered with the
listing of every version — no `os`, no `arch`, no `filename`, no `shasum`, and
`signing_keys` defaulted to empty. Terraform:

```
Error while installing tf.localhost:8443/hashicorp/null v3.2.2: registry
response to request for linux_amd64 archive has incorrect target _
```

`_` is the empty `os` and the empty `arch` joined by an underscore. A new
`DocumentKind::PROVIDER_DOWNLOAD` fetches the real document; the listing is
still fetched, but now for what it is good for — authorization and RFC 0006's
version filtering, against the real package name, so a blocked version makes
this endpoint 404 rather than answering.

**The archive was the download document.** `artifact_url` returns the download
document's URL for every provider artifact, and `fetch_artifact` followed that
field for `shasums` and `shasums.sig` but not for the archive — so it streamed
8 KB of JSON, labelled `application/zip`, as the provider binary:

```
Error while installing ... v3.2.2: archive has incorrect checksum
zh:29e5447... (expected zh:3248aae...)
```

Note what that message means: Terraform had already fetched our proxied
`shasums` and `shasums.sig`, verified the signature over them against
HashiCorp's key, and was comparing a real expected checksum against our bytes.
§12.8's fix was working. The archive underneath it was a JSON document.

Both fixed, the full sequence runs through the proxy:

```
GET /.well-known/terraform.json                                  -> 200
GET /v1/providers/hashicorp/null/versions                        -> 200
GET /v1/providers/hashicorp/null/3.2.2/download/linux/amd64      -> 200
GET /v1/providers/hashicorp/null/3.2.2/shasums                   -> 200
GET /v1/providers/hashicorp/null/3.2.2/shasums.sig               -> 200
GET /v1/providers/hashicorp/null/3.2.2/artifact/linux/amd64      -> 200
```

```
- Installed tf.localhost:8443/hashicorp/null v3.2.2 (signed by HashiCorp)
Terraform has been successfully initialized!
```

Signature verified, checksum verified, every byte through BatleHub. Registry
protocol, proxy mode, air-gapped: it had never once worked.

One adapter test asserted the old behaviour — that `fetch_artifact` returns
bytes containing `"linux"`, which they did, because they were the document. It
is now a test that a document naming no `download_url` is a 404.

### 12.13 conda's compressed channel could not see a publish

`repodata.json.zst` is cached under a key built from the **blocked-set**
fingerprint — deliberately, so a block change cannot be raced against a TTL
(RFC 0006 §4.2). A publish does not change the blocked set, and the entry was
written with no expiry at all. So in local and hybrid mode the compressed
channel was pinned, for good, to whatever the channel looked like the first time
anyone asked.

`repodata.json` is regenerated per request and was correct. The two encodings
described different channels, and micromamba asks for the compressed one first:
a package published after any client had probed once was simply unresolvable,
while `curl` on the `.json` URL showed it present. That is the shape of a bug
that survives a support conversation.

The cache is now skipped entirely in local and hybrid mode — where the channel
comes from the database and is cheap — and bounded by a TTL in proxy mode, where
it derives from an upstream document that has one of its own. The regression
test warms the cache before publishing, which is the whole test: without that
read there is nothing stale, and it passes against the bug.

Verified with micromamba 2.9.0 against the real server: `HEAD` on
`repodata.json.zst` is still its first request (§12.4), and a package published
between two of them installs.

### 12.14 `ovsx publish`, and what it prints

The publish endpoint and the query-parameter token (§12.6) both work against the
real server: `POST /api/-/publish?token=…` → `201`. But ovsx printed

```
🚀  Published e2eorg.e2eprobe v1.0.0@undefined
```

It appends `@{targetPlatform}` for anything that is not `"universal"`, and our
response omitted the field. A successful publish reported itself as broken. One
line, and it now prints `Published e2eorg.e2eprobe v1.0.1`.

### 12.15 RubyGems' compact index served the wrong registry

`bundle install` against a **local** registry, one command after publishing a
gem to it:

```
Fetching gem metadata from http://127.0.0.1:8090/proxy/my-gems/.
Could not find gem 'e2eprobe' in rubygems repository ... or installed locally.
```

All three compact documents — `/versions`, `/info/{gem}`, `/names` — went
straight to `ProxyService` with no mode check. In local mode they proxied
rubygems.org. So:

- a gem published to a local registry was **invisible to Bundler**, the client
  §7.3 added the compact index for, while the JSON APIs Bundler only falls back
  to showed it perfectly;
- a `local` registry answered `/versions` with the **public index** — 23 MB of
  rubygems.org — which is not something a local registry should be able to do.

§7.3 fixed the leak (Bundler read the one index we did not filter) by adding the
documents. It wired them to the proxy path only, and nothing noticed, because
every test for them is a proxy-mode test.

Local mode is now generated from the database, hybrid appends the local gems to
the upstream document, and proxy is unchanged. Generation goes through
`load_visible_versions`, so blocking and visibility apply before a line is
written rather than being stripped afterwards — the §4.1 obligation is met by
construction.

Two things had to be built for it:

- **`created_at`** is a fixed epoch, not `now()`. Bundler treats it as the point
  its incremental fetches start from; a timestamp that moves every request
  invalidates every cache every request.
- **Dependencies.** The compact index carries them inline, and nothing parsed
  them: `GemMetadata` had no field for them and publish stored none. A resolver
  handed an empty dependency list installs a gem without the gems it needs, so
  shipping the index without them would have replaced an invisible gem with a
  broken one. Runtime dependencies are now parsed from the gemspec YAML —
  development ones are not, since they are not what an installer resolves.

Each line of `/versions` ends with the MD5 of that gem's `/info` document, which
is how Bundler decides whether its cached copy is current, so it is computed
from the same bytes the info route returns. A test asserts exactly that, rather
than asserting the field is 32 characters long.

```
GET /proxy/my-gems/versions [range: bytes=23296246-] -> 200
GET /proxy/my-gems/info/e2eprobe                     -> 200
GET /proxy/my-gems/gems/e2eprobe-1.0.0.gem           -> 200
Bundle complete! 1 Gemfile dependency, 2 gems now installed.
```

The `Range` header is Bundler asking for the tail of the copy it had cached —
of the *upstream* index, from the failed run before. A `200` with the whole
document is a legal answer and Bundler handles it by replacing its cache, which
is how that run completed. Serving `206` makes the compact index incremental,
which is what the format is for; that is a performance property, it was named
here rather than left to be discovered, and it is now implemented — §13.24.

### 12.16 The transcripts became scripts, and found six more

§12 was a set of transcripts: seven clients run by hand, once, against a server
that was then changed to match. §5.2 concluded that this — not the fixture
table — is the check that finds this class of defect, and that it "belongs in
CI against real clients rather than in a table of strings". It did not, for as
long as it took someone to write the scripts.

Now it does. `tests/heavy/{npm,pypi,openvsx,conda,nuget,composer,terraform}.sh`
join `bundler.sh` and `marketplace.sh`: each starts a real server against a real
Postgres behind the logging tap, drives the real client, and asserts on the wire
(`task test:heavy`; in CI, the `heavy-client` matrix). Shared machinery moved to
`tests/heavy/lib.sh`.

**Writing them found six more shipped bugs, in one pass.** Not one is a wrong
path, so not one is reachable by the mechanism §5 built:

| # | Shipped bug | Found by |
| --- | --- | --- |
| 13 | `npm dist-tag ls` answered `404` on a local registry | `npm.sh` |
| 14 | `npm search` crashed the client: no `maintainers` in the hit | `npm.sh` |
| 15 | A dedup hit with a missing blob failed the download **permanently** | `npm.sh` |
| 16 | `dotnet nuget push` `404`d: it appends a trailing slash | `nuget.sh` |
| 17 | Search paged into the page, so page 2 was always empty | `nuget.sh` |
| 18 | Composer's `dist.shasum` was a SHA-256, and Composer hashes SHA-1 | `composer.sh` |

Each is worth a sentence, because each is a *different* way to be correct about
the path:

- **A route that skips the mode ladder.** `npm_dist_tags` read the packument
  straight off `ProxyService`, so on a local registry it asked npmjs.org about a
  package published here. `npm view` described the package; `npm dist-tag ls`,
  one command later, said it did not exist. Exactly §12.15's defect — the
  RubyGems compact index proxying upstream from a local registry — in a route
  nobody thought to re-check.
- **A field the client dereferences without a guard.** npm's search output does
  `data.maintainers.map(…)`, so a hit without the array is not a degraded
  listing, it is `Cannot read properties of undefined (reading 'map')`. And
  `npm_search_renders_the_npm_shape` passed throughout — it asserted the shape
  *we* had decided was npm's.
- **An index that outlives the bytes it indexes.** The storage router keeps a
  dedup index in Postgres and blobs in the backend. On a dedup hit it skipped
  materialising, trusting `ref_count > 1` — a claim about the backend that is
  false once storage has been cleared. `store_streaming` then reported success
  having written nothing, `promote_staged` found the staged artifact "vanished",
  and the 502 was **permanent**: every later fetch hit the same dangling row.
  The same-key path (`reuse_identical_blob`) checked; the new-key path did not.
  Found because the heavy suites use a per-run storage directory and a shared
  database — which is also what an operator does when they recreate a bucket.
- **A trailing slash.** The service index advertises `…/api/v2/package`;
  `dotnet nuget push` appends `/`. Every test in the repository PUT to the
  unslashed path. Publishing had never worked from the client.
- **An offset applied to the wrong set.** §12.4 measured `skip` being ignored,
  and the fix parsed it and applied it *after* the search had been limited to
  `take` hits — so `take=20&skip=20`, the second page `dotnet package search`
  asks for, was empty on every registry however many packages it held. A bug fix
  that reproduces the bug one layer down. npm's `from` had the same shape and
  was never read at all.
- **The right digest, the wrong algorithm.** Composer's `dist.shasum` is a
  SHA-1 and its client verifies against it; we published the SHA-256 we store as
  the artifact's checksum. Not a weaker check — a failed download, every time:
  *"The checksum verification of the file failed"*, after a resolve that worked
  perfectly. Locally published Composer packages had never been installable.

Three of the six (13, 15, 18) make a *published* package unusable, and all three
were introduced by work this RFC did. That is the honest reading of what the
heavy layer is worth: the ratchet in §5 catches endpoints at the wrong address,
and this catches everything else, because it is the only check whose oracle is
the program the user runs.

Two things the scripts had to get right, both of which produced a green run
measuring nothing before they were fixed:

- **The client's own cache.** `npm publish` seeds the tarball it packed into
  cacache, so the install that followed fetched no tarball and the transcript
  assertion had nothing to match; micromamba reuses repodata for the whole TTL,
  so the second resolve — the one §12.13 is about — never asked the server. Each
  phase now gets its own cache, which is the second developer on the second
  machine rather than the same one twice.
- **The tap.** `ovsx publish` streams the VSIX with `Transfer-Encoding:
  chunked`, and the tap read `Content-Length` only — so it forwarded an empty
  body and the server answered `400 could not read extension/package.json`. A
  broken *measurement* that looks exactly like a broken publish endpoint, which
  is the failure mode this whole section is about, arriving in the instrument.

---

## 13. Implementation notes

Phases 0 and 1 have landed. Everything below is a place where the
implementation departed from the design above, or where the design rested on a
wrong assumption about the codebase.

### 13.1 "Does it route" was the wrong question, twice over

§5's fixture was specified as asserting that a client path *reaches a handler*.
That assertion is nearly worthless in this codebase, for a reason the design
missed: `/proxy/{registry}/{package}` is a two-segment catch-all and
`/proxy/{registry}/{package}/{version}` a three-segment one, so almost any path
reaches something and is rejected by a handler that was never written for it.
Six of the ratchet's paths are eaten that way rather than 404ing.

The fixture asserts `HttpRequest::match_pattern()` instead — *which* route
matched, named per entry. That also makes the design commitment for an unbuilt
endpoint explicit: the pattern a phase will register is written down where it
will be checked.

One further subtlety, found only by running it: actix reports a matched pattern
while answering a **bodyless 404** when a path matches a resource registered for
a different method. Terraform's module-metadata `GET` collides with the upload
`POST` that way. So the predicate is the *conjunction* of a pattern match and
evidence a handler ran — a non-empty body, or any 2xx. Neither signal alone is
sufficient, and `an_unrouted_path_is_distinguishable_from_a_handled_404` pins
the mechanism itself, because if `AppError` ever stops rendering a body every
assertion in the file silently inverts.

### 13.2 `npm whoami` and `npm ping` return 200 with a package document

Found by the fixture on its first run, and not in the survey. These do not
404 — they are eaten by the three-segment catch-all and answered **`200 OK`
with an npm version document**. A wrong answer under a success status is worse
than the failure it replaces, because nothing downstream can tell.

Pinned by `the_npm_catch_alls_answer_paths_that_are_not_npm_packages` until
phase 8 serves them properly, so the defect is recorded rather than latent. The
same class may exist on other two- and three-segment paths this RFC has not
enumerated.

### 13.3 There was already a second cached-forward helper

§4.2 says the enforcement is that the passthrough helper becomes "the only
`reqwest` caller under `handlers/proxy/`". That was written without knowing
`jetbrains_marketplace/cached_forward.rs` exists and has done the same job since
RFC 0001's era — cache-first, stale-on-error, base64 in `PackageMetadata.extra`,
a `fwd:` key namespace. Its own module docs say *"Unlike the npm-audit
passthrough (plain forward, nothing persisted)"*, so the gap this RFC opens with
was known and left.

Reading it improved the new helper: it already had a **body size ceiling** and a
**TTL fallback**, both of which the design omitted and both of which matter —
an unbounded buffered body is how a hostile upstream exhausts memory, and a
passthrough cached with no expiry pins yesterday's advisory answer forever. Both
are now in `handlers/proxy/upstream.rs`.

One difference is left deliberately: the JetBrains helper serves stale
**unconditionally**, where the new one honours the registry's
`serve_stale_metadata`. The new behaviour is what §4.2 argues for; changing
JetBrains' is a behaviour change for a shipped path and belongs in its own
commit.

**So the "only caller" claim is not true yet**, and two more bare callers remain
beyond JetBrains: Composer's security-advisories forward
(`composer/metadata.rs:259`) and Terraform's module download
(`terraform/modules/read.rs:110`, which is also the `X-Terraform-Get` hole of
§7.2). Unifying all four onto one helper is phase 7 work, and the enforcement
test §4.2 promises cannot be written until it is done — a test asserting "no
bare `reqwest`" would fail today.

### 13.4 The generated tables immediately found a docs bug of their own

`docs:endpoints` rendered `PUT /proxy/{registry}/{name}` with an empty
description. The cause: someone had inserted `NpmPublishResponse` between the
`npm_publish` handler's doc comment and its `#[utoipa::path]`, so the handler's
documentation was silently absorbed into the struct and the handler had none.
Invisible while the table was hand-written, which is the argument for §6 in one
line.

### 13.5 npm audit's cache key includes the request body

Not stated in §7.1, and load-bearing: `npm audit` POSTs the dependency set, so
two projects asking one registry are two different questions. The key carries a
SHA-256 of the canonicalised body rather than the body itself — a lockfile's
worth of dependencies is far too long for a cache key — and
`a_different_dependency_set_is_a_different_cache_entry` asserts one project's
advisories are never served to another's question.

The same applies to the Go vulnerability database's `/v1/query`, which is
POST-shaped for the same reason and now keyed the same way.

### 13.6 The dependency API is declined, not deferred

§7.3 says `GET /api/v1/dependencies?gems=a,b,c` is "cheap once the compact index
machinery exists". It is not, and the reason is the one §2.2 uses to argue the
opposite case: **the dependency API returns Marshal**, the Ruby serialisation
format whose absence from this codebase is exactly why `specs.4.8.gz` is marked
`Unsupported`. Adding it would need the encoder that §2.2 says the fix does not
need.

It is also unreachable. Bundler's fallback chain is compact index → dependency
API → full index, and the compact index now answers — so nothing gets that far.
Building a Marshal encoder to serve a rung no client descends to would be
surface with no caller, which §3 lists as a non-goal in its own right.

Declined, with the conformance entry left in place carrying
`not_yet("RFC 0009 §13.6 — declined, not deferred")` so the decision is visible
where someone would otherwise re-derive it.

### 13.7 `/names` carries no filtering obligation

§4's table lists `/names` as a whole-registry document filtered through
`dispatch_multi`. That is wrong: it lists gem *names* and no versions, so a
block has nothing in it to hide. Removing a gem whose versions are partly
blocked would tell Bundler the gem does not exist — a different and worse answer
than "some of its versions are restricted", and one that would break resolution
rather than steer it.

It is served unfiltered, and `listing_filter()` records it as covering no
`DocumentKind` rather than claiming a filter it does not need.

### 13.8 The reachability contract needed per-document exemptions

§4.1's extension from kinds to `(kind, DocumentKind)` pairs immediately hit a
case the design did not anticipate. `FILTERED_ELSEWHERE` exempts a *whole kind*
— fine for conda, whose every listing goes through `dispatch_multi`. RubyGems is
mixed: its JSON APIs go through `strip` and must keep being checked, while
`/versions` goes through `dispatch_multi` and `/names` goes nowhere.

So there is now a second, finer list, `DOCUMENTS_FILTERED_ELSEWHERE`, keyed by
`(kind, document)` and carrying a reason per entry for the same anti-parking
reason the first one does.

The new check earned itself on the first run: it rejected `listing_filter()`
naming a `DocumentKind` that did not exist yet, which is precisely the silent
skip it was added to prevent.

### 13.9 The `/versions` checksum has to move when the line does

Not in §7.3, and load-bearing. Each `/versions` line ends with the md5 of that
gem's `/info` document, and Bundler uses it to decide whether to re-fetch
`/info`. Filtering the version list without touching the checksum would let a
client keep serving an `/info` copy fetched before the block — the listing would
be filtered and the resolver would still see the blocked version.

The checksum is therefore recomputed from the upstream value plus the surviving
versions, **and only when the line actually changed**. Bundler treats the field
as opaque, so it only has to be stable and different; keeping untouched lines
byte-identical to upstream is what stops one gem's block from making every
client re-download every other gem's metadata.

### 13.10 Compressed repodata cannot be cached by TTL alone

§7.5 says the compressed variants are "cached under their own artifact key so
the compression is paid once per TTL". A TTL is the wrong key, and the reason is
RFC 0006 §4.2: what gets compressed is the **filtered** document, and caching a
filtered document by time means it keeps serving a version for the rest of that
time after an operator blocked it. The whole point of applying blocks on top of
a cached *upstream* copy on every request is that a block takes effect on the
next request, not eventually.

Compressing per request is not an option either — a busy channel's
`repodata.json` runs to tens of megabytes and is fetched on every solve.

So the compressed entry is keyed by a **fingerprint of the blocked set**
(`MultiPackageBlocks::fingerprint`, exposed as
`ProxyService::blocked_snapshot_fingerprint`). A block change produces a
different key, so the entry filtered under the old list is never *read* rather
than being raced against an expiry. Correctness comes from the key, not from a
clock.

The fingerprint sorts before hashing, because `HashMap` iteration order is not
stable across processes and two replicas must agree on the key or they cache the
same bytes twice.

### 13.11 conda's `channeldata.json` is dropped, not repaired

§4's table lists it as a `dispatch_multi` document, which is right, but says
nothing about *how* it filters — and it cannot filter the way its sibling does.
`repodata.json` lists every file, so a blocked version is removed and the rest
of the package survives. `channeldata.json` lists every package **once**, naming
only its newest release, and carries no version list at all.

RubyGems' single-gem document has the same shape and is repaired onto the newest
allowed version. That is impossible here: the versions live in `repodata.json`,
a different document, fetched per subdir. So a package whose named release is
blocked is dropped from the summary.

That is the narrower of two wrong answers. A dropped entry makes `conda search`
report the package as absent; a repaired entry would name a version this filter
has not verified exists. Search degrades, install does not — the solver reads
`repodata.json`, which is filtered per-version. If it ever needs repairing, the
fix is composition against the filtered repodata, exactly as Go's `@latest` and
RubyGems' gem document are composed (RFC 0006 §13.3) — a handler change, not a
filter change. `listing_filter()` records this as `Qualified` rather than
`Filtered` so an operator reads it before wondering where a package went.

### 13.12 The sumdb needed a config map and its whole reload path

§7.4 describes the route and the config field and stops there. In practice a new
per-registry URL map is not one field: `SumDbMap` had to be threaded through
`BuiltHotState`, `PendingReload`, `ConfigReloadParams`, the applier's swap, the
server factory's `app_data`, and every test fixture that constructs a reload
service — sixteen sites.

Worth stating because it is the cost of the *next* one too, and because skipping
the reload half would have produced a field that silently ignores
`config reload` — the class of half-wired setting RFC 0004-bis spent a phase
finding. `SumDbMap` is swapped alongside `VulnDbMap` in the same apply, so the
two goproxy passthroughs cannot come to disagree about which config generation
they are serving.

Caching it is not an optimisation but the feature: an uncacheable sumdb lookup
has moved the egress rather than removed it. `a_sumdb_lookup_survives_the_log_going_away`
is the air-gapped case end to end — one upstream answer, then the log is gone,
and the second build still resolves.

### 13.13 Terraform: three route-ordering bugs, two of them mine

§7.2 described the routes and said nothing about registering them, which turned
out to be where the work was. Adding the network mirror introduced two
regressions and exposed one latent hazard, and `protocol_conformance.rs` caught
all three — the first time the mechanism paid for itself on code written after it.

- **The mirror's four-segment pattern swallowed RubyGems.**
  `{hostname}/{namespace}/{ptype}/index.json` matched
  `/api/v1/versions/{gem}.json` as host=`api`, ns=`v1`, type=`versions`. Fixed by
  constraining `{hostname}` to contain a dot, which every registry hostname does
  and `api`, `v1` and `v3` never do. Ordering would have fixed the symptom;
  the constraint fixes the class.
- **`index.json` is a valid `{version}` capture**, so the version route claimed
  the index path when registered first. Ordering, and now pinned by a test.
- **`cargo search` at `/api/v1/crates` is swallowed by openvsx's
  `api/{namespace}/{extension}`** — the same greedy route `lib.rs:775-781` already
  warned about in prose. Registered with the cargo routes, above openvsx, rather
  than with the other search routes.

The general lesson is in the second one: a pattern that is *correct* is not
enough, because correctness here is relative to every other registered pattern.
That is not a property any single handler's tests can see.

### 13.14 The module download gate needed an adapter change, not a header rewrite

§7.2 says `X-Terraform-Get` is "rewritten to our own artifact route". Half the
work is missing from that sentence: our artifact route read *local storage*, so
in proxy mode it had nothing to serve.

`X-Terraform-Get` is a pointer, not the bytes — the upstream `/download`
endpoint answers `204` with a header naming where the tarball actually lives. So
`TerraformRegistryClient::fetch_artifact` now follows it, which is what puts the
bytes on the gated path at all. Two constraints came with that:

- **Only `http(s)` targets are followed.** The header is a go-getter source and
  may legitimately be `git::ssh://…`; that is not an archive this proxy can
  fetch, cache and gate, and pretending otherwise would produce a corrupt
  artifact rather than an honest error.
- **The target host is upstream-controlled**, so it goes through
  `ssrf::fetch_following_redirects` with the configured upstream as the trusted
  origin — the same treatment PyPI, GitLab and Forgejo already give their
  cross-origin download URLs.

Providers had the same hole in a different shape: the download *document* named
upstream's CDN in `download_url`. The rule chain ran on the request for that
document but never on the bytes, so there was no cache, no download audit and no
integrity check. It is now fetched as a document and repointed at our own
artifact route, which gained a proxy fall-through to match.

**Since closed** (§12.8): both fields now point at
`…/{version}/shasums` and `…/shasums.sig`, which resolve the URL named inside the
download document and stream it through the gate. A gated archive whose
checksums came from the internet was not an offline install.

### 13.15 Two shipped tests asserted a status, not a property

`terraform_{module,provider}_artifact_proxy_mode_rejects_previously_published_*`
asserted `404` after a hot-reload into proxy mode. The property they exist for is
"local storage must not answer once the registry is in proxy mode", and that
still holds — but the route now has a proxy fall-through, so the status is
whatever the proxy path returns rather than the guard's `404`.

Updated to assert the property directly: not `200`, and not the bytes that were
published. A test pinned to a status code is a test that fails when the
implementation improves, which is how a suite comes to discourage the change it
should be enabling.

### 13.16 Rung 3b needed the repository to answer honestly

The core rung-3b test failed at first with an empty result set, and the bug was
in the test: its `PackageRepository` stub ignored `PackageFilter::blocked_only`.
The default `blocked_in_registry` derives a registry's blocked set from
`list_packages` with that flag set — so a stub that ignores it reports **every
held package as blocked**, and the search filter then correctly removed all of
them.

Worth recording because the failure looked like "rung 3b does not work" and was
actually "the filter works, and the fixture lied". The same shape will catch the
next person who writes a repository stub for a filtering path.

### 13.17 The long tail had one item that was not a route

Most of §7.6 was mechanical. Two were not.

**`cargo owner --remove` can escalate privilege.** `require_owner` uses
`can_publish`, which deliberately returns `true` for a package with *no* owners
— that is how first publish works. So removing the last owner turns an owned
crate into one anybody with `User` may publish to. The handler refuses that with
a `409` rather than allowing it and logging a warning; a warning nobody reads is
not a control.

**`npm dist-tag add` is declined, not implemented.** BatleHub *derives*
`dist-tags` from the published version set — `latest` is the newest allowed
release, recomputed on every read so a block moves it immediately (RFC 0006). A
stored tag would be overwritten by the next request, and `npm dist-tag ls` would
then report something the client never set. Accepting the write would be a `200`
that does not hold, which is the failure mode this whole RFC is about. It answers
`501` with the reason, and `AppError` gained `not_implemented` for it: `404`
would tell a client to look elsewhere, and there is nowhere else to look.

`npm dist-tag ls` reads its map out of the **filtered packument** rather than
filtering separately, so the two documents agree by construction. A second filter
over the same facts is a second thing to keep in step.

### 13.18 `sortBy` was parsed and then discarded

`GalleryQuery` carried `sort_by` from the moment the gallery landed and nothing
read it, so every `extensionquery` came back in qualified-name order whatever the
editor asked for. For "sort by recently updated" that is not a slower answer, it
is a wrong one.

Now honoured for title, publisher and last-updated. Relevance keeps
qualified-name order deliberately: this proxy has no relevance signal of its own,
and a stable predictable order is a better answer than a fabricated ranking —
the same reasoning `GalleryEntry::matches` already gives for not ranking search
results.

### 13.19 On `git::ssh` module sources

§13.14 declines every non-`http(s)` `X-Terraform-Get` target. Worth recording why
this is a decision rather than a gap, because it will be re-proposed.

A `git::` source is a **clone, not an archive**. Caching and gating one means
cloning server-side, archiving the working tree, and serving that tarball — and
nothing upstream states a checksum for those bytes, because there is no canonical
archive. What would land in the cache, acquire an SBOM and get an integrity
record would be our own construction presented as the module. That is a
provenance claim we cannot back, and it is the same reasoning that made
`strip_channeldata` drop an entry rather than repair it onto an unverified
version (§13.11).

Three costs stack on top: `git::ssh://` needs key material at rest and host-key
verification, which is a new credential class rather than a new route; a
`?ref=main` source is *mutable*, so caching it needs staleness semantics the
name/version model does not have; and supporting one go-getter scheme invites
`hg::`, `s3::` and `gcs::`, each with its own auth model.

The useful work is narrower and should be done first:

1. **Rewrite `git::https://{host}/…` when `{host}` matches a configured forge
   registry.** GitHub, GitLab and Forgejo archives are already proxied at
   `tarball/{tag}` and `archive/{tag}/{filename}` — gated, cached, audited. That
   converts a whole class of git-sourced modules onto machinery that exists, with
   no new credentials and a real upstream artifact behind it.
2. **Make the current error actionable** — it correctly says the source cannot be
   cached, and should name the forge-registry alternative.
3. **`git::ssh://` stays declined**, recorded here so the reasoning travels with
   the code the way `listing_filter()`'s `Unsupported` strings do.

### 13.20 Search read the wrong store for a published package

Caught by `local_nuget_registry.rs::nuget_search_local_returns_packages` on the
full-workspace run, and it is a design error rather than a test artifact.

§7.7 says rung 3b answers from "the packages this registry already holds", and
names `PackageRepository` — the store `GET /api/v1/packages` reads. That store
records what has been **accessed through** the proxy. Packages **published to**
a local registry live in `LocalRegistryBackend`, which is a different store.
`CLAUDE.md` already documents the split, and adds that in Postgres the two share
tables so the separation "only matters in tests" — which is true of the *test*
consequence it was describing and not of this one: a local-mode registry has no
proxied traffic at all, so reading only `PackageRepository` returned nothing for
a package it had accepted a moment earlier.

`ProxyService::search` therefore takes the published set as a parameter rather
than reading it: only the web layer holds a `LocalRegistryService`, and core
should not grow a dependency on it to answer one question. Rung 3b is now the
union of both stores — what was proxied through and what was published here —
and local mode is the published set alone.

The old NuGet local branch had the right store and emitted `"version": ""` for
every hit, which is not a well-formed NuGet search result. The shared helper
resolves each name's newest published version instead.

### 13.21 Two drift gates cannot tell prose from drift

`docs:roadmap:check` and `docs:listing-coverage:check` both regenerate their
block and then run `git diff --quiet` on the **whole page**. That fails on any
uncommitted change to that file — including hand-written prose the generator
does not own and never touches.

Both fired during this work, neither for a real reason: `ROADMAP.md` had an
unstaged edit, and `admin-policies.md` had one paragraph of new prose beside a
generated table that was byte-identical before and after regeneration. The
message each prints ("run `task docs:…` and commit the result") is then actively
misleading, because running it changes nothing.

**Fixed in §13.23.** Both gates now compare the generated content before and
after regeneration rather than asking git whether the file is dirty, which is
what `docs:endpoints:check` already did.

### 13.22 What phase 9 actually regenerated

- `ui/openapi.json` — 20 new operations across the eight phases.
- The TypeScript client. It is **gitignored** (`ui/.gitignore:3`), so it is a
  local build step rather than a committed artifact; regenerating it and
  building `ui/` is how one checks the spec did not break a consumer, which it
  did not.
- All ten per-registry endpoint tables.
- The listing-coverage table, which now carries RubyGems' compact index and
  conda's `channeldata.json`.

The admin guide's block-delay note was generalised from "conda's block delay" to
the three whole-registry documents that now share the 30-second snapshot —
conda's two repodata documents plus `channeldata.json`, and RubyGems'
`/versions`. It also now states the half of the trade the old note left implicit:
the window is one where a client may be *offered* a version it will then be
refused, which is the mid-resolve failure RFC 0006 exists to remove, narrowed to
half a minute rather than eliminated.

### 13.23 Finishing the residue

Everything §12 and §13 left open, except two items that are not RFC residue at
all. Each was small; together they are the difference between "the phases
landed" and "the RFC is done".

- **Composer's advisory forward** was the last bare `reqwest` caller among the
  passthroughs (§4.2). Routed through the helper, so `composer audit` survives
  an unreachable upstream like `npm audit` and `govulncheck` already do.
- **JetBrains' `serve_stale_or` ignored `serve_stale_metadata`** and served
  stale unconditionally (§13.3). Now gated by the same flag as every other
  stale path — an operator who turned stale serving off was still getting stale
  answers from that one helper.
- **The §4.2 enforcement test now exists**: `upstream_calls_are_cached.rs`
  scans `handlers/proxy/` and fails on any outbound call outside the two
  allowlisted helpers. §4.2 promised "the only `reqwest` caller"; there are two,
  both implement the three rungs, and the allowlist says why rather than being a
  place to quietly add a third. A second test asserts the allowlisted files
  exist, so a rename cannot turn the check into a scan that permits nothing.
- **PEP 658** — see §12.7; this was not a slowdown but a hard failure.
- **Terraform checksum sidecars** — see §12.8.
- **Open VSX `api/version` and `api/{namespace}`**, the two discoverable
  documents `ovsx` and the web UI read. The namespace document is built from the
  same filtered entry list as the gallery and the extension document, so a
  blocked version cannot show through one and be hidden in the others. It
  reports `verified: false` because BatleHub has no namespace-ownership model
  and claiming verification it never performed would be a provenance claim it
  cannot back.
- **`?token=` for `ovsx publish`**, normalised beside the existing
  `X-NuGet-ApiKey` rewrite and **scoped to that one path**. A token in a query
  string is a token in access logs; `ovsx` offers no header alternative, so the
  narrowest form of yes is to accept it exactly where it is needed. An explicit
  header still wins.
- **Both drift gates now compare content instead of git state** (§13.21), so
  editing prose beside a generated block no longer fails a check that then tells
  you to run a task which changes nothing.
- **`git::https://…` targets are now followed** rather than refused: the
  transport prefix is stripped when what follows is plain http(s), because that
  really is a fetchable archive. The refusal that remains — `git::ssh://`,
  `hg::`, `s3::` — now names the alternative (configure the forge as a
  github/gitlab/forgejo registry) instead of only saying no (§13.19).

**`filterType 12` turned out not to be a gap.** §7.6 listed it as unhandled; it
has had an explicit arm all along whose comment explains that `4096`
(Unpublished) has no counterpart here, because an unlisted version is already
gone by the time the gallery sees it. Adding a field to parse it would have been
the "parsed and then discarded" defect §13.18 criticises. The RFC was wrong, not
the code.

**conda's sharded repodata is not done, and is not residue.**
`repodata_shards.msgpack.zst` (CEP-16) is a different document model — a msgpack
shard index plus per-package shard files — needing a new dependency and a
synthesis step, with no way to verify it end to end here. It degrades cleanly:
micromamba probes, gets a `404`, and uses `repodata.json`, which §12.4 observed
directly. That is a feature request, not an unfinished item, and calling it
either would be wrong.

### 13.24 The compact index is incremental, and two things had to be measured

§12.15 closed the correctness half — a local registry serves its own compact
index — and named the performance half as unimplemented: Bundler asks for the
tail of what it holds, and this server answered `200` with the whole document
every time. That is legal (RFC 9110 §14.2 permits ignoring `Range`) and it
discards the reason the format exists. `/versions` against a public mirror is
tens of megabytes; against a hybrid registry it is that plus our own gems, on
every `bundle install`.

All three documents now answer `If-None-Match` and `Range`. Two of the three
things that makes work could not have been reasoned to.

**The prefix guard, which §12.2 said was impossible.** §12.2 warned that a `206`
would be a correctness bug because the client's offset was computed against a
document filtered under a different blocked set — ours is generated from a
query, so a gem published under a name that sorts early changes the *middle* of
the document, not only its end. True, and it stops being a problem the moment
you notice the client tells you what it holds: Bundler sends `If-None-Match`
with the range, carrying the `ETag` we issued with the bytes now in its cache.
Ours is a SHA-256 of the document. So if the client's validator equals the
SHA-256 of our document's first *N* bytes, its copy **is** our prefix and
appending the tail is provably correct;
otherwise it diverges somewhere inside the part it is not asking for, and it
gets `200` — one round trip instead of a `206` it would have to detect as
corrupt and re-fetch. The guard is what makes ranges safe over a generated
document, and it is a check a generic file server cannot perform.

**`Repr-Digest`, without which a `206` is worse than none.** Bundler refuses to
append a partial response that carries no digest of the whole representation —
*"appending is too error prone to do without digests"* — and falls through to a
plain re-fetch. Measured, the first version of this code produced exactly
`GET /versions [range] -> 206` followed by `GET /versions -> 200`: the document
transferred one and a half times, to save nothing. Every answer, full or
partial, now carries `Repr-Digest: sha-256=:<base64>:` (RFC 9530, byte sequence
per RFC 8941) over the whole document — the same value the client verifies its
reassembled file against.

**Bundler asks from one byte before the end of its copy.** `bytes=(size-1)-`
(`updater.rb`: *"Subtract a byte to ensure the range won't be empty. Avoids 416
(Range Not Satisfiable) responses."*), so the answer is never empty and it never
has to handle a `416`. Its validator
therefore describes `N+1` bytes while its range starts at `N`. A guard that
checked only `N` would have been correct, tested, and never once matched the
client it exists for — §5.2's failure shape again, and the reason the guard
tries both lengths. The overlapping byte is one the client already has; it
engineered the overlap and reconciles it.

The rest is ordinary HTTP: `Accept-Ranges` and an `ETag` on every answer,
because a client never given a validator can never ask a conditional question —
which is how these documents were fetched whole, forever; `304` when the
validator matches; `416` with `Content-Range: bytes */N` when the range starts
past the end, which is what Bundler produces when its copy is already current
and the document has not grown; and `200` for multi-range or any form we do not
serve, rather than a half-implemented `multipart/byteranges` no compact-index
client asks for.

The unit tests cover the resolution and the guard. A separate integration test
asserts the *route* is wired to them and that prefix plus tail reassemble the
document — because "the helper is correct" and "the endpoint uses it" are the
two halves §5.1 exists to keep apart, and this RFC has now been wrong about the
second one four times.

**One more gap of the same kind, found while checking this one.** §12.15's
hybrid merge — upstream's document with this registry's gems appended — had no
test at all: the measurement was in local mode, and hybrid was a claim in this
RFC and a row in the registry page's mode table with nothing under it. It now
has three, covering the merged `/versions` and `/names` (one header, one
separator, both sides' gems) and `/info/{gem}` answering locally for a gem this
registry hosts and from upstream for one it does not.

**Measured, against Bundler 4.0.17 and the real server**, and the measurement
is now a test rather than a transcript: `tests/heavy/bundler.sh`
(`task test:bundler-heavy`, and the `heavy-bundler` CI job), beside the
marketplace probe §12.6 leans on. A
local registry, two gems, the second depending on the first and published
between two `bundle install` runs, through a logging tap that leaves `Host`
alone (§12.10):

```
POST /proxy/e2e-gems/api/v1/gems                     -> 200
GET  /proxy/e2e-gems/versions                        -> 200 (90B)
        ETag: "fcde8e48…"
GET  /proxy/e2e-gems/info/aa…probe                   -> 200
GET  /proxy/e2e-gems/gems/aa…probe-1.0.0.gem         -> 200
POST /proxy/e2e-gems/api/v1/gems                     -> 200
GET  /proxy/e2e-gems/versions                        -> 206 (54B)
        Range: bytes=89-  If-None-Match: "fcde8e48…"
        Content-Range: bytes 89-142/143
GET  /proxy/e2e-gems/info/zz…probe                   -> 200
GET  /proxy/e2e-gems/gems/zz…probe-1.0.0.gem         -> 200
```

54 bytes where 143 were sent before, and — the assertion that matters —
**no second `GET /versions`**. That absence is the whole test: the failed first
attempt showed up as exactly such a re-fetch, and its absence here is Bundler
saying it verified the digest and appended. `Bundle complete!`, with the
`Gemfile.lock` carrying `zz…probe (1.0.0)` → `aa…probe (>= 1.0.0)`, which also
closes §12.15's dependency parsing end to end rather than against a fixture.

A third step re-runs the resolve with nothing published in between, and gets
`304` — the cheap answer, from the client that asks for it, which no test in the
repository could previously observe being *accepted*.

The prefix guard is not hypothetical either: the first two runs of this probe
reused a database that already held gems from an earlier run, one of which
sorted *after* the newly published gem. The append became a middle-insertion,
the guard refused it, and both runs answered `200` and completed. That is the
case §12.2 was worried about, arriving by accident, and being handled. The
committed script gives each run its own registry so the measurement is of the
append path rather than of whatever the database happens to hold.

**Two of this RFC's own lessons showed up while writing the script.** The health
check inherited a `PORT` from the environment and probed an unrelated service
that never became healthy — a green-looking wait on the wrong target. And the
re-fetch assertion, the one thing the whole test exists to check, used `exit 0`
inside an awk rule, which still runs `END{exit 1}` and overrode it: run against
a transcript that plainly showed the re-fetch, it passed. Both are §5.2 in
miniature — the check was written from what it was supposed to prove rather than
from what it would do — and the second was caught by feeding the assertion a
transcript it was required to reject.
