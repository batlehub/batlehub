//! Protocol conformance — the paths the *client* sends, not the ones we built.
//!
//! RFC 0009 §5. Every other test in this tree is written from our own
//! implementation, which is why five endpoints shipped at addresses no package
//! manager calls and every test still passed: a test written from the
//! implementation cannot discover that the implementation answers the wrong
//! question. `proxy_npm_edge_cases.rs:76` POSTs to `/-/npm/v1/audit/quick` and
//! asserts a sensible response. It passes. npm has never sent that path.
//!
//! So this file contains no knowledge of our routes beyond one field, and each
//! entry is a literal request line copied from a client or a protocol
//! specification with its source recorded beside it.
//!
//! ## Why "does it route" is not the question
//!
//! The obvious assertion — the path reaches *a* handler — is nearly meaningless
//! here, because `/proxy/{registry}/{package}` is a two-segment catch-all and
//! `/proxy/{registry}/{package}/{version}` a three-segment one. Almost any path
//! reaches something. `GET /proxy/npm/-/whoami` reaches the npm *version*
//! handler and returns **200 OK with a package document** — a wrong answer with
//! a success status, which is worse than the 404 it deserves.
//!
//! So each entry names the route pattern that must match, and the test asserts
//! `HttpRequest::match_pattern()` equals it. A catch-all swallowing a client
//! path is then a failure, which is the route-ordering hazard `lib.rs:775-781`
//! warns about in prose made executable.
//!
//! ## The two assertion classes
//!
//! ## Which of these are evidence, and which are still belief
//!
//! A `source` beginning **"observed"** was captured from the real client
//! against a logging registry, not read from documentation — the npm suite is
//! verified that way (RFC 0009 §12). Everything else is protocol reading, and
//! is the part of this file most worth re-checking before relying on it: a
//! fixture that encodes the wrong path is confidently wrong in exactly the way
//! the bug it replaced was.
//!
//! - **pattern** (always) — the request matched the route that implements its
//!   protocol.
//! - **`must_find`** — the response also names a known package. Required for any
//!   endpoint whose success response is a collection, because a route that
//!   returns an empty `200` is indistinguishable from a stub by every other
//!   signal (RFC 0009 §5.1). `nuget_search` is exactly that stub today: it
//!   matches its own pattern, returns 200, and always says nothing was found.
//!
//! ## The ratchet
//!
//! `not_yet(phase)` **inverts** the expectation: the entry asserts we do *not*
//! satisfy the requirement yet, so the tree stays green while the inventory of
//! what we do not serve lives here rather than in a hand-written survey.
//! Implementing the endpoint makes this file fail until the marker is deleted.
//! The list only shrinks. `swallowed_by`, where present, pins *which* catch-all
//! currently eats the path, so the wrong-handler defect is recorded rather than
//! merely absent.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::http::Method;
use actix_web::test::{call_service, TestRequest};

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;

/// One request line a real client sends.
struct Conformance {
    method: Method,
    path: &'static str,
    /// Where the path was read from. The reviewable half of the claim: this
    /// file asserts what we *believe* a client sends, and a belief with a
    /// citation can be checked without reading our code.
    source: &'static str,
    /// The route pattern that must match, exactly as `match_pattern()` renders
    /// it. For a `not_yet` entry this is the pattern the phase will register —
    /// a design commitment recorded where it will be checked.
    expect: &'static str,
    /// Present ⇒ expectation inverted; names the RFC 0009 phase that serves it.
    not_yet: Option<&'static str>,
    /// The catch-all that currently eats this path, when one does. Pins the
    /// wrong-handler defect so a route-ordering change surfaces here.
    swallowed_by: Option<&'static str>,
    /// Present ⇒ the response body must also contain this token.
    must_find: Option<&'static str>,
    /// JSON request body, for POST/PUT entries.
    body: Option<&'static str>,
}

impl Conformance {
    const fn new(
        method: Method,
        path: &'static str,
        expect: &'static str,
        source: &'static str,
    ) -> Self {
        Self {
            method,
            path,
            source,
            expect,
            not_yet: None,
            swallowed_by: None,
            must_find: None,
            body: None,
        }
    }
    const fn get(path: &'static str, expect: &'static str, source: &'static str) -> Self {
        Self::new(Method::GET, path, expect, source)
    }
    const fn post(path: &'static str, expect: &'static str, source: &'static str) -> Self {
        Self::new(Method::POST, path, expect, source)
    }
    const fn put(path: &'static str, expect: &'static str, source: &'static str) -> Self {
        Self::new(Method::PUT, path, expect, source)
    }
    const fn delete(path: &'static str, expect: &'static str, source: &'static str) -> Self {
        Self::new(Method::DELETE, path, expect, source)
    }
    const fn not_yet(mut self, phase: &'static str) -> Self {
        self.not_yet = Some(phase);
        self
    }
    const fn swallowed_by(mut self, pattern: &'static str) -> Self {
        self.swallowed_by = Some(pattern);
        self
    }
    const fn must_find(mut self, token: &'static str) -> Self {
        self.must_find = Some(token);
        self
    }
    const fn body(mut self, json: &'static str) -> Self {
        self.body = Some(json);
        self
    }

    fn requirement(&self) -> String {
        match self.must_find {
            Some(t) => format!("match {:?} and name {t:?}", self.expect),
            None => format!("match {:?}", self.expect),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// npm
//
// `npm audit` is the reason this file exists. npm-registry-fetch sends
// `/-/npm/v1/security/advisories/bulk` (the default since npm 7) and
// `/-/npm/v1/security/audits/quick`. We serve `/-/npm/v1/audit/{bulk,quick}`,
// which is neither — in neither direction, since `npm/read.rs:284` builds the
// same invented path for the upstream forward.
//
// `whoami` and `ping` are the sharper case: they do not 404, they are eaten by
// the three-segment catch-all and answered **200** with a package document.
// ─────────────────────────────────────────────────────────────────────────────
const NPM: &[Conformance] = &[
    Conformance::get(
        "/proxy/npm/express",
        "/proxy/{registry}/{package}",
        "npm 11.17.0, observed: `npm view` / install resolution",
    )
    .must_find("1.1.0"),
    Conformance::get(
        "/proxy/npm/express/4.18.2",
        "/proxy/{registry}/{package}/{version}",
        "npm-registry-fetch — version document",
    ),
    Conformance::post(
        "/proxy/npm/-/npm/v1/security/advisories/bulk",
        "/proxy/{registry}/-/npm/v1/security/advisories/bulk",
        "npm 11.17.0, observed: `npm audit` sends exactly this and nothing else",
    )
    .body("{}"),
    Conformance::post(
        "/proxy/npm/-/npm/v1/security/audits/quick",
        "/proxy/{registry}/-/npm/v1/security/audits/quick",
        "npm/lib/commands/audit.js — quick audit",
    )
    .body("{}"),
    Conformance::get(
        "/proxy/npm/-/v1/search?text=fixed",
        "/proxy/{registry}/-/v1/search",
        "npm 11.17.0, observed: `npm search` — note `text`/`size`, not `q`/`limit`",
    )
    .must_find("fixed-alpha"),
    Conformance::get(
        "/proxy/npm/-/package/express/dist-tags",
        "/proxy/{registry}/-/package/{package}/dist-tags",
        "npm 11.17.0, observed: `npm dist-tag ls`",
    ),
    Conformance::get(
        "/proxy/npm/-/whoami",
        "/proxy/{registry}/-/whoami",
        "npm 11.17.0, observed: `npm whoami`",
    ),
    Conformance::get(
        "/proxy/npm/-/ping",
        "/proxy/{registry}/-/ping",
        "npm 11.17.0, observed: `npm ping`",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// RubyGems
//
// Bundler resolves from the compact index first, the dependency API second and
// `specs.4.8.gz` last. We serve only the last, and `listing_filter()` marks it
// `Unsupported` — so every `bundle install` reads the one index we do not
// filter (RFC 0009 §2.2). Three of the four missing paths are swallowed by the
// npm catch-alls rather than 404ing.
// ─────────────────────────────────────────────────────────────────────────────
const RUBYGEMS: &[Conformance] = &[
    Conformance::get(
        "/proxy/rubygems/gems/rails-7.0.0.gem",
        "/proxy/{registry}/gems/{filename}",
        "gem fetch",
    ),
    Conformance::get(
        "/proxy/rubygems/api/v1/versions/rails.json",
        "/proxy/{registry}/api/v1/versions/{name}.json",
        "rubygems.org API — all versions",
    ),
    Conformance::get(
        "/proxy/rubygems/versions",
        "/proxy/{registry}/versions",
        "Bundler 4.0.17, observed: request 1 of 2 for a full resolution, sent with `Range:`",
    ),
    Conformance::get(
        "/proxy/rubygems/info/rails",
        "/proxy/{registry}/info/{gem}",
        "Bundler 4.0.17, observed: request 2 of 2, also sent with `Range:`",
    ),
    Conformance::get(
        "/proxy/rubygems/names",
        "/proxy/{registry}/names",
        "bundler compact_index_client — gem names",
    ),
    // Deliberately not served — see RFC 0009 §13.6. The dependency API returns
    // Marshal, which is the encoder §2.2 says the fix does not need, and
    // Bundler never reaches it now that the compact index above answers.
    Conformance::get(
        "/proxy/rubygems/api/v1/dependencies?gems=rails",
        "/proxy/{registry}/api/v1/dependencies",
        "Bundler 4.0.17, observed NOT sent: there is no fallback chain (RFC 0009 §12.2)",
    )
    .not_yet("RFC 0009 §13.6 — declined, not deferred")
    .swallowed_by("/proxy/{registry}/api/{namespace}/{extension}"),
];

// ─────────────────────────────────────────────────────────────────────────────
// conda
//
// Modern conda and mamba request `.zst` first and fall back on 404. The
// `{filename}` route regex admits only `.tar.bz2`/`.conda` (`conda.rs:220`), so
// a `.zst` request is eaten by the npm three-segment catch-all instead.
// ─────────────────────────────────────────────────────────────────────────────
const CONDA: &[Conformance] = &[
    Conformance::get(
        "/proxy/conda/linux-64/repodata.json",
        "/proxy/{registry}/{platform}/repodata.json",
        "micromamba 2.9.0, observed: fetched only after the .zst probe 404s",
    ),
    Conformance::get(
        "/proxy/conda/linux-64/current_repodata.json",
        "/proxy/{registry}/{platform}/current_repodata.json",
        "conda — newest-versions subset",
    ),
    Conformance::get(
        "/proxy/conda/linux-64/repodata.json.zst",
        "/proxy/{registry}/{platform}/repodata.json.zst",
        "micromamba 2.9.0, observed: probed with HEAD before any GET (RFC 0009 §12.4)",
    ),
    Conformance::get(
        "/proxy/conda/linux-64/repodata.json.bz2",
        "/proxy/{registry}/{platform}/repodata.json.bz2",
        "conda — legacy compression",
    ),
    Conformance::get(
        "/proxy/conda/channeldata.json",
        "/proxy/{registry}/channeldata.json",
        "conda search — cross-platform discovery",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// Go — the module half is served; the checksum-database half is not, so
// `go mod download` still dials sum.golang.org and fails closed air-gapped.
// ─────────────────────────────────────────────────────────────────────────────
const GOPROXY: &[Conformance] = &[
    Conformance::get(
        "/proxy/go/github.com/pkg/errors/@v/list",
        "/proxy/{registry}/{module:[^@]+}@v/list",
        "GOPROXY — version list",
    ),
    Conformance::get(
        "/proxy/go/github.com/pkg/errors/@latest",
        "/proxy/{registry}/{module:[^@]+}@latest",
        "GOPROXY — latest",
    ),
    Conformance::get(
        "/proxy/go/github.com/pkg/errors/@v/v0.9.1.info",
        "/proxy/{registry}/{module:[^@]+}@v/{filename}",
        "GOPROXY — version metadata",
    ),
    Conformance::get(
        "/proxy/go/sumdb/sum.golang.org/supported",
        "/proxy/{registry}/sumdb/{path:.*}",
        "GOPROXY — the `sumdb/` half of the protocol",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// Terraform — discovery is host-rooted by the protocol and absent entirely, and
// the network mirror our own docs configure is a different protocol from the
// `/v1/` registry routes we implement (RFC 0009 §7.2).
// ─────────────────────────────────────────────────────────────────────────────
const TERRAFORM: &[Conformance] = &[
    Conformance::get(
        "/proxy/terraform/v1/modules/hashicorp/consul/aws/versions",
        "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions",
        "Terraform registry protocol — module versions",
    ),
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/aws/versions",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions",
        "Terraform registry protocol — provider versions",
    ),
    Conformance::get(
        "/.well-known/terraform.json",
        "/.well-known/terraform.json",
        "Terraform — service discovery, host-rooted",
    ),
    Conformance::get(
        "/proxy/terraform/registry.terraform.io/hashicorp/aws/index.json",
        "/proxy/{registry}/{hostname:[^/]+\\.[^/]+}/{namespace}/{ptype}/index.json",
        "Terraform 1.8.5, observed: request 1 of 3 for a provider install, sent with auth",
    ),
    Conformance::get(
        "/proxy/terraform/v1/modules/hashicorp/consul/aws/0.1.0",
        "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}",
        "Terraform registry protocol — module metadata",
    ),
    // Discovery is host-routed only, and the host-routing middleware rewrites
    // every request on a vanity host to `/proxy/{registry}{path}` *before*
    // routing. So the address above is never the address that arrives: this is,
    // and it was claimed by the npm/cargo catch-all
    // `/proxy/{registry}/{package}/{version}`, which answered "registry
    // 'x' is not an npm or cargo registry". Discovery was unreachable on
    // precisely the hosts it exists for (RFC 0009 §12.11).
    Conformance::get(
        "/proxy/terraform/.well-known/terraform.json",
        "/proxy/{registry}/.well-known/terraform.json",
        "Terraform 1.8.5, observed: request 1 of 6 for a provider install, after the host-routing rewrite",
    ),
    // Request 3 of 6. A different document from the versions listing, and the
    // proxy path used to answer with the listing — no `os`, no `arch`, no
    // `filename`, no `shasum` (§12.12).
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/download/linux/amd64",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/download/{os}/{arch}",
        "Terraform 1.8.5, observed: request 3 of 6 for a provider install",
    ),
    // Requests 4 and 5. Terraform verifies the archive against these, so an
    // air-gapped install that fetched them from the internet was not air-gapped.
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/shasums",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums",
        "Terraform 1.8.5, observed: request 4 of 6 for a provider install",
    ),
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/shasums.sig",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig",
        "Terraform 1.8.5, observed: request 5 of 6 for a provider install",
    ),
    // Request 6 of 6, the provider zip itself.
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/artifact/linux/amd64",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}",
        "Terraform 1.8.5, observed: request 6 of 6 for a provider install",
    ),
    // Request 2 of 3 of a *mirror* install, which the table did not carry until
    // the RFC 0012 phase-7 run went looking for it. This is the document that
    // names the archive, and therefore the one a signature is minted into.
    Conformance::get(
        "/proxy/terraform/registry.terraform.io/hashicorp/null/3.2.2.json",
        "/proxy/{registry}/{hostname:[^/]+\\.[^/]+}/{namespace}/{ptype}/{version}.json",
        "Terraform 1.8.5, observed: request 2 of 3 for a mirror install, sent with auth",
    ),
    // ── RFC 0012: the same three paths, signed ───────────────────────────────
    //
    // Observed against a live BatleHub with `anonymous = []` and
    // `signed_downloads = true`: `terraform init` completed, and the archive
    // arrived with **no `Authorization` header** and a `bh_sig` query string.
    //
    // What these pin is narrow and worth pinning: **a query string must not
    // change which route matches.** Nothing about `bh_sig` is special to the
    // router, which is exactly why a `web::Query<T>` extractor added later, or
    // a pattern tightened to reject a query, would break signed downloads
    // without breaking anything else — and would break them for the one client
    // that cannot fall back to a header.
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/artifact/linux/amd64?bh_sig=1.eyJ2IjoxfQ.AAAA",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}",
        "Terraform 1.8.5, observed: the provider zip, signed — fetched with no auth header",
    ),
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/shasums?bh_sig=1.eyJ2IjoxfQ.AAAA",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums",
        "Terraform 1.8.5, observed: the checksum manifest, signed",
    ),
    Conformance::get(
        "/proxy/terraform/v1/providers/hashicorp/null/3.2.2/shasums.sig?bh_sig=1.eyJ2IjoxfQ.AAAA",
        "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig",
        "Terraform 1.8.5, observed: the detached signature over the manifest, signed",
    ),
    // `shasums.sig` and `shasums` differ by a suffix, and `{version}.json` in
    // the mirror routes shows the router does split on dots. With a query
    // appended the two must still not collide.
    Conformance::get(
        "/proxy/terraform/registry.terraform.io/hashicorp/null/3.2.2.json?bh_sig=1.eyJ2IjoxfQ.AAAA",
        "/proxy/{registry}/{hostname:[^/]+\\.[^/]+}/{namespace}/{ptype}/{version}.json",
        "RFC 0012: the mirror document itself, were a client to append a query",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// NuGet — every route below already exists. `/v3/query` is the one that matches
// its own pattern, returns 200, and means nothing: proxy and hybrid mode answer
// `{"totalHits": 0, "data": []}` unconditionally (`search_publish.rs:103`)
// while the service index advertises `SearchQueryService`. It is why
// `must_find` exists, and why its `must_find` carries a `not_yet`.
// ─────────────────────────────────────────────────────────────────────────────
const NUGET: &[Conformance] = &[
    Conformance::get(
        "/proxy/nuget/nuget/v3/index.json",
        "/proxy/{registry}/nuget/v3/index.json",
        "NuGet — service index",
    ),
    Conformance::get(
        "/proxy/nuget/nuget/v3/flat/newtonsoft.json/index.json",
        "/proxy/{registry}/nuget/v3/flat/{id}/index.json",
        "NuGet PackageBaseAddress — version list",
    ),
    // `must_find`, not just a routing assertion. This endpoint routed, returned
    // 200 and valid JSON while answering `{"totalHits": 0}` unconditionally —
    // the stub of RFC 0009 §5.1. Only naming a seeded package tells the two
    // apart.
    Conformance::get(
        "/proxy/nuget/nuget/v3/query?q=fixed",
        "/proxy/{registry}/nuget/v3/query",
        "dotnet 10.0.400, observed: `q`/`skip`/`take`/`prerelease`/`semVerLevel`",
    )
    .must_find("fixed-alpha"),
    Conformance::get(
        "/proxy/nuget/nuget/v3/autocomplete?q=newton",
        "/proxy/{registry}/nuget/v3/autocomplete",
        "dotnet 10.0.400: selected only via the `/3.0.0-beta` resource type",
    ),
    // The trailing slash is the client's. `dotnet nuget push` reads
    // `PackagePublish/2.0.0` out of the service index — which advertises the
    // path *without* it — and appends one; the route registered only for the
    // unslashed spelling answered `404` to every real push while every test in
    // this repository PUT to the other one (RFC 0009 §12.16, found by
    // `tests/heavy/nuget.sh`). Both spellings are registered, and this fixture
    // is what keeps the client's one from being dropped as redundant.
    Conformance::put(
        "/proxy/nuget/nuget/api/v2/package/",
        "/proxy/{registry}/nuget/api/v2/package/",
        "dotnet 10.0.400, observed: PUT with a trailing slash",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// PyPI, Composer, cargo, Maven. The passing entries here are regression guards
// on route *ordering*, which is a live hazard: `lib.rs:775-781` documents
// `api/{namespace}/{extension}` swallowing `api/plugins/{id}`, and the same
// route eats `cargo search` and the RubyGems dependency API today.
// ─────────────────────────────────────────────────────────────────────────────
const OTHERS: &[Conformance] = &[
    Conformance::get(
        "/proxy/pypi/simple/requests/",
        "/proxy/{registry}/simple/{package}/",
        "pip — PEP 503 simple index",
    ),
    Conformance::get(
        "/proxy/pypi/simple/",
        "/proxy/{registry}/simple/",
        "pip — simple index root",
    ),
    Conformance::get(
        "/proxy/pypi/pypi/requests/json",
        "/proxy/{registry}/pypi/{package}/json",
        "Poetry and assorted tooling — the PyPI JSON API",
    ),
    Conformance::get(
        "/proxy/composer/packages.json",
        "/proxy/{registry}/packages.json",
        "Composer 2 — root document",
    ),
    Conformance::get(
        "/proxy/composer/p2/monolog/monolog.json",
        "/proxy/{registry}/p2/{path:.*}",
        "Composer 2 — p2 metadata",
    ),
    Conformance::get(
        "/proxy/composer/search.json?q=monolog",
        "/proxy/{registry}/search.json",
        "composer search",
    ),
    Conformance::get(
        "/proxy/cargo/registry/config.json",
        "/proxy/{registry}/registry/config.json",
        "cargo — sparse index config",
    ),
    Conformance::get(
        "/proxy/cargo/api/v1/crates?q=serde",
        "/proxy/{registry}/api/v1/crates",
        "cargo search",
    ),
    Conformance::put(
        "/proxy/cargo/api/v1/crates/new",
        "/proxy/{registry}/api/v1/crates/new",
        "cargo publish",
    )
    .body("{}"),
    Conformance::delete(
        "/proxy/cargo/api/v1/crates/serde/1.0.0/yank",
        "/proxy/{registry}/api/v1/crates/{name}/{version}/yank",
        "cargo yank",
    ),
    Conformance::get(
        "/proxy/maven/maven2/org/slf4j/slf4j-api/maven-metadata.xml",
        "/proxy/{registry}/maven2/{path:.*}",
        "Maven — path-addressed metadata",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// The long tail (RFC 0009 §7.6) — endpoints a client calls that had no route.
// ─────────────────────────────────────────────────────────────────────────────
const LONG_TAIL: &[Conformance] = &[
    Conformance::post(
        "/proxy/openvsx/api/-/publish?token=t",
        "/proxy/{registry}/api/-/publish",
        "ovsx 1.1.1, observed: `ovsx publish` sends exactly this, token in the query",
    ),
    Conformance::get(
        "/proxy/openvsx/api/rust-lang/rust-analyzer",
        "/proxy/{registry}/api/{namespace}/{extension}",
        "ovsx 1.1.1, observed: request 1 of 2 for `ovsx get`",
    ),
    Conformance::get(
        "/proxy/openvsx/api/rust-lang/rust-analyzer/1.0.0/file/x.vsix",
        "/proxy/{registry}/api/{namespace}/{extension}/{version}/file/{filename:.*}",
        "ovsx 1.1.1, observed: request 2 of 2, followed from `files.download`",
    ),
    Conformance::put(
        "/proxy/cargo/api/v1/crates/serde/owners",
        "/proxy/{registry}/api/v1/crates/{name}/owners",
        "cargo owner --add",
    )
    .body(r#"{"users":["someone"]}"#),
    Conformance::delete(
        "/proxy/cargo/api/v1/crates/serde/owners",
        "/proxy/{registry}/api/v1/crates/{name}/owners",
        "cargo owner --remove",
    )
    .body(r#"{"users":["someone"]}"#),
    Conformance::put(
        "/proxy/nuget/nuget/api/v2/symbolpackage",
        "/proxy/{registry}/nuget/api/v2/symbolpackage",
        "nuget push — .snupkg symbol package",
    ),
    Conformance::get(
        "/proxy/composer/list.json",
        "/proxy/{registry}/list.json",
        "composer — bulk package enumeration",
    ),
];

// ─────────────────────────────────────────────────────────────────────────────
// nodedist — nvm (RFC 0010)
//
// nvm resolves *every* install through `index.tab`, then fetches the checksum
// file and the tarball from the release directory. All three lines are read
// from nvm.sh v0.40.3; the heavy suite (`tests/heavy/nvm.sh`) is what turns
// "read" into "observed". `index.tab` and `{version}/{file}` are the pair that
// proves route ordering: a listing must be a document, never a file.
// ─────────────────────────────────────────────────────────────────────────────
const NODEDIST: &[Conformance] = &[
    Conformance::get(
        "/proxy/nodedist/nodedist/index.tab",
        "/proxy/{registry}/nodedist/index.tab",
        "nvm.sh 0.40.3:1657, `nvm_ls_remote_index_tab` — `nvm ls-remote` and every `nvm install`",
    )
    .must_find("v1.1.0"),
    Conformance::get(
        "/proxy/nodedist/nodedist/index.json",
        "/proxy/{registry}/nodedist/index.json",
        "fnm src/remote_node_index.rs and mise's core node plugin — the same table as JSON",
    )
    .must_find("v1.1.0"),
    Conformance::get(
        "/proxy/nodedist/nodedist/v1.1.0/SHASUMS256.txt",
        "/proxy/{registry}/nodedist/{version}/{file}",
        "nvm.sh 0.40.3:1853, `nvm_get_checksum` — read before every download",
    ),
    Conformance::get(
        "/proxy/nodedist/nodedist/v1.1.0/node-v1.1.0-linux-x64.tar.xz",
        "/proxy/{registry}/nodedist/{version}/{file}",
        "nvm.sh 0.40.3:2466, `nvm_download_artifact` — `${MIRROR}/${VERSION}/${SLUG}.${COMPRESSION}`",
    ),
];

const SUITES: &[(&str, &[Conformance])] = &[
    ("npm", NPM),
    ("rubygems", RUBYGEMS),
    ("conda", CONDA),
    ("goproxy", GOPROXY),
    ("terraform", TERRAFORM),
    ("nuget", NUGET),
    ("nodedist", NODEDIST),
    ("others", OTHERS),
    ("long-tail", LONG_TAIL),
];

#[actix_web::test]
async fn every_path_a_client_sends_reaches_its_own_handler() {
    let app = make_app(InMemoryRepo::new()).await;

    let mut report = Report::default();

    for (suite, entries) in SUITES {
        for c in *entries {
            let mut req = TestRequest::with_uri(c.path).method(c.method.clone());
            if let Some(b) = c.body {
                req = req
                    .insert_header(("content-type", "application/json"))
                    .set_payload(b);
            }
            let resp = call_service(&app, req.to_request()).await;
            let matched = resp.request().match_pattern();
            let status = resp.status();
            let body = actix_web::test::read_body(resp).await;

            let satisfied = is_satisfied(c, matched.as_deref(), status, &body);
            report.record(suite, c, satisfied, matched.as_deref(), status);
        }
    }

    let Report {
        broken,
        landed,
        moved,
    } = report;

    assert!(
        broken.is_empty(),
        "these paths are sent by a real client and do not reach the handler \
         that implements their protocol:\n{}",
        broken.join("\n")
    );
    assert!(
        landed.is_empty(),
        "these endpoints now work — the ratchet only shrinks, so remove their \
         `not_yet` markers:\n{}",
        landed.join("\n")
    );
    assert!(
        moved.is_empty(),
        "route ordering changed for a path we do not serve. Not necessarily \
         wrong, but it means a different handler now answers a client request \
         it was never written for — confirm and update `swallowed_by`:\n{}",
        moved.join("\n")
    );
}

/// Does this entry's requirement hold, given what the request came back with?
///
/// Two signals, and both are needed.
///
/// The pattern alone is not enough: a pattern can be registered for a
/// *different method* — Terraform's module-metadata GET collides with the
/// upload POST on the same path — and actix still reports the matched pattern
/// while answering a **bodyless 404**, not a 405. So a pattern match can mean
/// "this path is spoken for by a route the client cannot use".
///
/// Whether a handler ran is not enough either, since the catch-alls above mean
/// nearly everything reaches one.
///
/// Together they are exact: `AppError` always renders a JSON body
/// (`crates/web/src/error.rs:91-97`), so a non-empty body — or any 2xx, which
/// may legitimately be empty — means our handler for this pattern actually
/// executed.
fn is_satisfied(
    c: &Conformance,
    matched: Option<&str>,
    status: actix_web::http::StatusCode,
    body: &[u8],
) -> bool {
    let handler_ran = !body.is_empty() || status.is_success();
    if matched != Some(c.expect) || !handler_ran {
        return false;
    }
    match c.must_find {
        Some(token) => String::from_utf8_lossy(body).contains(token),
        None => true,
    }
}

/// The three ways an entry can be worth reporting, accumulated across suites.
#[derive(Default)]
struct Report {
    /// A requirement that should hold and does not.
    broken: Vec<String>,
    /// A `not_yet` entry that now passes — the ratchet only shrinks.
    landed: Vec<String>,
    /// A path whose pinned catch-all is no longer the one that eats it.
    moved: Vec<String>,
}

impl Report {
    fn record(
        &mut self,
        suite: &str,
        c: &Conformance,
        satisfied: bool,
        matched: Option<&str>,
        status: actix_web::http::StatusCode,
    ) {
        // Pin the catch-all that currently eats this path, when one does.
        if let (Some(swallow), Some(actual)) = (c.swallowed_by, matched) {
            if !satisfied && actual != swallow {
                self.moved.push(format!(
                    "  [{suite}] {} {}\n      was swallowed by {swallow:?}, now matches {actual:?}",
                    c.method, c.path
                ));
            }
        }

        match (c.not_yet, satisfied) {
            (Some(phase), true) => self.landed.push(format!(
                "  [{suite}] {} {}\n      now satisfied — delete `.not_yet({phase:?})`",
                c.method, c.path
            )),
            (None, false) => self.broken.push(format!(
                "  [{suite}] {} {}\n      source: {}\n      must {} — matched {:?}, status {}",
                c.method,
                c.path,
                c.source,
                c.requirement(),
                matched,
                status
            )),
            (Some(_), false) | (None, true) => {}
        }
    }
}

/// The catch-alls, and the defect that is now closed.
///
/// `/proxy/{registry}/{package}` and `/proxy/{registry}/{package}/{version}`
/// match almost any two- or three-segment path, which is why this file asserts
/// *which* route matched rather than merely that one did.
///
/// `npm whoami` and `npm ping` used to be answered **200 with a package
/// document** by the three-segment route. Phase 8 gave them their own routes;
/// this asserts they no longer reach the catch-all, so a future reordering
/// cannot quietly hand them back to it.
#[actix_web::test]
async fn the_npm_catch_alls_no_longer_answer_whoami_and_ping() {
    let app = make_app(InMemoryRepo::new()).await;

    for path in ["/proxy/npm/-/whoami", "/proxy/npm/-/ping"] {
        let resp = call_service(&app, TestRequest::with_uri(path).to_request()).await;
        assert_ne!(
            resp.request().match_pattern().as_deref(),
            Some("/proxy/{registry}/{package}/{version}"),
            "{path} is being answered by the npm version handler again"
        );
    }
}
