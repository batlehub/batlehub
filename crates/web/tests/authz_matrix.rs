//! The route-by-route authorization matrix.
//!
//! # Why this file exists
//!
//! The 2026-08-26 security survey (`docs/internal/security-survey-2026-08-26.md`)
//! found the *same* defect in eight places: a local-registry read path that
//! serves package data without evaluating the registry rule chain, or without
//! checking per-package visibility, while the equivalent proxy-mode read
//! enforces both. It was found one ecosystem at a time — maven, nuget,
//! terraform twice, conda, goproxy, jetbrains, pypi — because the check is
//! applied **by convention rather than by construction**. It had already been
//! found and fixed once before that, on the OpenVSX route
//! (`handlers/proxy/openvsx.rs`), and came back.
//!
//! A per-handler code review cannot close this class, and neither can static
//! analysis: a handler with a guarded proxy branch and an unguarded local branch
//! *mentions* `authorize_read` either way, so "does this function call the
//! chain?" answers yes for exactly the routes that are broken. The question is
//! per-path, not per-function, and the only reliable way to ask it is to make
//! the request and look at the status code.
//!
//! # What it asserts
//!
//! For each row: a registry in local mode whose `[registries.rbac]` grants
//! **anonymous nothing**, holding one published package at the default
//! `Visibility::Public`. An anonymous request to the route must not be answered
//! with `200`. The package is public, so only the rule chain can refuse — which
//! is the point.
//!
//! Every row is paired with a positive control: the identical request as
//! `USER_TOKEN`, which the policy *does* grant, must return `200`. Without it a
//! row could pass because the route 404s for an unrelated reason — a seeding
//! mistake, a path typo — and assert nothing at all. A row whose control fails
//! is a broken test, not a passing gate, and is reported as such.
//!
//! # Known gaps
//!
//! Rows for defects the survey found and that are not yet fixed are marked
//! [`Expect::KnownGap`]. They are held to the *current* behaviour, so the suite
//! is green on a tree with open findings — but the ratchet works in both
//! directions: fixing a handler without flipping its row fails this file, and so
//! does a row marked `Denied` that regresses. Deleting the row is the only way
//! to make a gap disappear quietly, and that shows up in review.
//!
//! # Adding a registry type
//!
//! Two steps, and the second is enforced.
//!
//! 1. Add rows to [`matrix`] — one per read route, with both axes. A row is
//!    `Row::new(kind, uri)`, which expects both gates to refuse and seeds
//!    `pkg` / `9.8.7`; chain `.pkg()`, `.coord()`, `.meta()`, `.vis()` or
//!    `.no_control()` for whatever is *not* true of your route, and nothing
//!    else. What is written beside a row is what is unusual about it.
//! 2. Add every one of the new routes to [`ROUTE_INVENTORY`], classified.
//!
//! Step 2 is not optional and not a convention: `the_route_inventory_matches_the_router`
//! asserts the inventory against the live route table in both directions, so a
//! route this server registers and nobody has classified fails the build, and the
//! failure prints the line to paste. That check exists because this file's own
//! guidance used to end at step 1 — "not covered until someone does" — which is
//! the same *by convention rather than by construction* failure the matrix was
//! written to end, one level up. The ninth ecosystem would have arrived with no
//! rows and a green suite.
//!
//! Coverage is not all-or-nothing: [`Coverage::NoRow`] records a package read
//! nobody has exercised yet, so the list is honest while it shrinks.
//! `report_read_surface_coverage` prints the ratio on every run, because a green
//! suite covering 43 of 97 routes otherwise looks exactly like one covering all
//! of them.
//!
//! `task authz-matrix` prints the table.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};
use bytes::Bytes;
use serde_json::json;
use sha2::{Digest, Sha256};

use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{Identity, Role};
use batlehub_core::ports::TeamNamespacePort;
use batlehub_core::services::PublishRequest;

/// A request body and its `Content-Type`, built per row.
type Body = fn() -> (Vec<u8>, &'static str);

/// What the matrix expects a request from a caller the axis denies.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Expect {
    /// The gate refuses. `403`, or `404` where the protocol hides existence
    /// rather than admitting it.
    Denied,
    /// A known, unfixed defect: the route answers `200` to a caller the gate
    /// denies. Pinned so the suite is green with the finding open, and so fixing
    /// it forces this row to be updated.
    ///
    /// **No row uses it today** — every gap the 2026-08-26 survey found is
    /// closed — which is why it needs the `allow`. Kept rather than deleted
    /// because it is the ratchet's mechanism, and the next finding wants to be
    /// pinnable on the day it is found rather than after a debate about how to
    /// keep the suite green.
    #[allow(dead_code)]
    KnownGap(&'static str),
    /// This axis is not applicable to this row, or the fixture cannot express
    /// it. Stated rather than silently omitted.
    NotChecked(&'static str),
}

/// One route under test.
struct Row {
    /// Registry type, as `RegistryMap` knows it.
    kind: &'static str,
    /// Route to request, with `{registry}` already substituted.
    uri: &'static str,
    /// **Axis A — the registry rule chain.** Registry RBAC grants anonymous
    /// nothing; the package is `Visibility::Public`. Only the chain can refuse.
    expect: Expect,
    /// **Axis B — per-package visibility.** Registry RBAC grants anonymous
    /// `releases:read` *and* `source:read`, so the chain allows; the package is
    /// `Visibility::Internal`, which `check_visibility` refuses below `User`.
    /// Only visibility can refuse.
    expect_vis: Expect,
    /// Extra index metadata the ecosystem's read path needs to find the package.
    meta: fn() -> serde_json::Value,
    /// Package coordinate to seed.
    name: &'static str,
    version: &'static str,
    /// Skip the positive control: a few routes legitimately 404 even for a
    /// permitted caller in this minimal fixture (they need multi-file artifacts
    /// or an upstream). Stated per row rather than globally.
    control: bool,
    /// The string that betrays this package in *this* document, when the
    /// coordinate itself does not appear in it.
    ///
    /// `disclosed` looks for the package name, the version or the artifact
    /// bytes, which covers almost every route. It does not cover a document that
    /// names the package by a *part* of its coordinate: the OpenVSX namespace
    /// listing keys extensions by their short name, so `acme.zqfixture` appears
    /// as `zqfixture` and none of the three default signals fires. Naming the
    /// token per row is honest about what leakage looks like there; widening the
    /// default signals would make every row's assertion vaguer to fix one.
    token: Option<&'static str>,
    /// A read reached by `POST` rather than `GET`, with the body it needs.
    ///
    /// Three routes in this server take a request document and answer with
    /// package data — the VS Code gallery's `extensionquery`, JetBrains'
    /// compatible-updates lookup. They are *reads* by every measure this file
    /// cares about, and the HTTP method is the only thing that kept them out of
    /// [`ROUTE_INVENTORY`], which filters on `item.get`. A search that discloses
    /// a private package discloses it whichever verb carried the request.
    post: Option<Body>,
}

/// Axis B is only meaningful on routes that serve *this package's* data.
/// Whole-registry documents (`/versions`, `/names`, a search index) are scoped
/// to the channel, and per-package visibility is not the gate that governs them.
const WHOLE_REGISTRY: Expect =
    Expect::NotChecked("whole-registry document; per-package visibility is not its gate");

fn no_meta() -> serde_json::Value {
    json!({})
}

/// The bytes the fixture publishes, so an artifact leak is recognisable.
const FIXTURE_BYTES: &[u8] = b"matrix-fixture-bytes";

/// Did this response disclose the package to a caller the gate denies?
///
/// Status alone is the wrong test. A search or whole-registry index answering
/// `200` with an **empty** result set has disclosed nothing and is behaving
/// correctly; an artifact route answering `200` with the bytes has disclosed
/// everything. So the question is not "was it a 200" but "did the package
/// appear in it" — which is also the property the operator actually cares
/// about, and the one a status-only assertion gets wrong in both directions.
async fn disclosed<S>(app: &S, row: &Row) -> (bool, u16)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    disclosed_as(app, row, None).await
}

/// [`disclosed`], as a specific caller. `None` is anonymous.
async fn disclosed_as<S>(app: &S, row: &Row, token: Option<&str>) -> (bool, u16)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let (pkg, version) = (row.name, row.version);
    let mut req = match row.post {
        None => TestRequest::get().uri(row.uri),
        Some(body) => {
            let (bytes, content_type) = body();
            TestRequest::post()
                .uri(row.uri)
                .insert_header(("Content-Type", content_type))
                .set_payload(bytes)
        }
    };
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    // Both the request and the body read are bounded.
    //
    // Several rows are routes that fall through to the upstream client, and the
    // fixture's upstream can leave a request outstanding indefinitely — an
    // unbounded `call_service` or `read_body` hangs the entire suite rather than
    // failing one row, which is a far worse failure mode than a wrong answer.
    //
    // A request that never answers has disclosed nothing, so a timeout resolves
    // to "not disclosed" and reports status `0`. That cannot silently pass a
    // row: the row's positive control makes the same request, times out the
    // same way, discloses nothing, and is reported as a broken control.
    let Ok(resp) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call_service(app, req.to_request()),
    )
    .await
    else {
        return (false, 0);
    };
    let status = resp.status().as_u16();
    if status != 200 {
        return (false, status);
    }
    let body = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        actix_web::test::read_body(resp),
    )
    .await
    {
        Ok(b) => b,
        Err(_) => return (false, status),
    };
    let text = String::from_utf8_lossy(&body);
    // Three signals, because a document can disclose the package without naming
    // it: `/info/{gem}` and `@v/list` are keyed by the URL and their bodies are
    // version lists, so the version string is the only thing that betrays them.
    // `9.8.7` is deliberately unusual so `contains` cannot collide with an
    // unrelated version elsewhere in a document.
    let leaked = body
        .windows(FIXTURE_BYTES.len())
        .any(|w| w == FIXTURE_BYTES)
        || text.contains(row.token.unwrap_or(pkg))
        || text.contains(version);
    (leaked, status)
}

impl Row {
    /// A row with the defaults every route shares: both axes expect the gate to
    /// refuse, no extra index metadata, the coordinate `pkg` / `9.8.7`, and the
    /// positive control on. Only a row's *exceptions* are spelled out at the
    /// call site, so what is unusual about a route is the only thing written
    /// next to it.
    fn new(kind: &'static str, uri: &'static str) -> Self {
        Row {
            kind,
            uri,
            expect: Expect::Denied,
            expect_vis: Expect::Denied,
            meta: no_meta,
            name: "pkg",
            version: "9.8.7",
            control: true,
            token: None,
            post: None,
        }
    }

    /// The string that gives this package away in this route's document, when
    /// the coordinate itself does not appear in it.
    fn token(mut self, token: &'static str) -> Self {
        self.token = Some(token);
        self
    }

    /// This read is reached by `POST` with the given body, not by `GET`.
    fn post(mut self, body: Body) -> Self {
        self.post = Some(body);
        self
    }

    /// Override **axis A**, the registry rule chain.
    ///
    /// No row uses it today — every route is expected to refuse — and it is kept
    /// for the same reason as [`Expect::KnownGap`], which is the only thing it
    /// would carry: the next finding wants to be pinnable on the day it is
    /// found, not after a debate about how to keep the suite green.
    #[allow(dead_code)]
    fn chain(mut self, expect: Expect) -> Self {
        self.expect = expect;
        self
    }

    /// Override **axis B**, per-package visibility.
    fn vis(mut self, expect_vis: Expect) -> Self {
        self.expect_vis = expect_vis;
        self
    }

    /// Extra index metadata this ecosystem's read path needs to find the package.
    fn meta(mut self, meta: fn() -> serde_json::Value) -> Self {
        self.meta = meta;
        self
    }

    /// Seed under a different package name. The version stays `9.8.7`.
    fn pkg(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Seed under a different name *and* version.
    fn coord(mut self, name: &'static str, version: &'static str) -> Self {
        self.name = name;
        self.version = version;
        self
    }

    /// Skip the positive control: this route legitimately 404s even for a
    /// permitted caller in the minimal fixture. Every use says why.
    fn no_control(mut self) -> Self {
        self.control = false;
        self
    }
}

/// The cargo sparse-index line, which both cargo rows read.
fn cargo_index_meta() -> serde_json::Value {
    json!({"name": "pkg", "vers": "9.8.7", "deps": [], "cksum": "", "features": {}, "yanked": false})
}

/// Terraform provider coordinates, plus the one platform the download row asks for.
fn terraform_provider_meta() -> serde_json::Value {
    json!({"kind": "provider", "namespace": "acme", "type": "vault", "platforms": [{"os": "linux", "arch": "amd64"}]})
}

/// Terraform module coordinates.
fn terraform_module_meta() -> serde_json::Value {
    json!({"kind": "module", "namespace": "acme", "name": "vpc", "provider": "aws"})
}

/// [`terraform_module_meta`] with the fields a module *detail* read resolves by.
///
/// `terraform_module_upload` writes `version` and `sha256` into the index line,
/// and the detail route answers `404 — not available` without them. The
/// versions-listing row above does not need them, which is why the two metas are
/// separate rather than one widened for both.
fn terraform_module_version_meta() -> serde_json::Value {
    json!({
        "kind": "module", "namespace": "acme", "name": "vpc", "provider": "aws",
        "version": "9.8.7", "sha256": "", "yanked": false,
    })
}

/// An OpenVSX extension in the `acme` namespace, under a name no other string
/// in a namespace document collides with.
fn namespaced_extension_meta() -> serde_json::Value {
    json!({"id": "acme.zqfixture", "version": "9.8.7"})
}

/// The sdist filename the PyPI read paths key on.
fn pypi_meta() -> serde_json::Value {
    json!({"filename": "pkg-9.8.7.tar.gz"})
}

/// The conda package filename and the subdir channel holding it.
fn conda_meta() -> serde_json::Value {
    json!({"filename": "pkg-9.8.7-py311_0.conda", "subdir": "linux-64"})
}

/// A marketplace extension's identity, for the openvsx and VS Code rows.
fn extension_meta() -> serde_json::Value {
    json!({"id": "acme.ext", "version": "9.8.7"})
}

/// A JetBrains plugin's identity.
fn plugin_meta() -> serde_json::Value {
    json!({"id": "org.acme.plugin", "version": "9.8.7"})
}

/// `get_go_version_list` reads each version from `index_metadata["Version"]`, so
/// with `no_meta` the list came back *empty for everyone* — including the
/// permitted caller, which is what the broken positive control was reporting. A
/// row whose control cannot see the package asserts nothing about the caller who
/// should not.
fn go_list_meta() -> serde_json::Value {
    json!({"Version": "v9.8.7"})
}

fn matrix() -> Vec<Row> {
    vec![
        // ── cargo ────────────────────────────────────────────────────────────
        Row::new("cargo", "/proxy/reg/pkg/9.8.7/download").meta(cargo_index_meta),
        // ── npm ──────────────────────────────────────────────────────────────
        Row::new("npm", "/proxy/reg/pkg/9.8.7/tarball"),
        Row::new("npm", "/proxy/reg/pkg"),
        // ── nuget ────────────────────────────────────────────────────────────
        Row::new("nuget", "/proxy/reg/nuget/v3/flat/pkg/index.json"),
        // Axis B here was survey finding 6, and the row that calibrated the axis:
        // the download read `local_svc.storage` directly while the sibling flat
        // *index* checked visibility. It goes through `get_artifact_at_key` now,
        // which gates before it reads.
        Row::new(
            "nuget",
            "/proxy/reg/nuget/v3/flat/pkg/9.8.7/pkg.9.8.7.nupkg",
        ),
        // ── pypi ─────────────────────────────────────────────────────────────
        Row::new("pypi", "/proxy/reg/simple/pkg/").meta(pypi_meta), // was survey finding 9
        // ── conda ────────────────────────────────────────────────────────────
        Row::new("conda", "/proxy/reg/linux-64/pkg-9.8.7-py311_0.conda").meta(conda_meta), // was survey finding 4
        // ── goproxy ──────────────────────────────────────────────────────────
        // Was survey finding 10. `source:read` here, not `releases:read`: a
        // module zip is the source, and the proxy fall-through says so too.
        Row::new("goproxy", "/proxy/reg/example.com/m@v/v9.8.7.zip")
            .coord("example.com/m", "v9.8.7"),
        // Was survey finding 10.
        Row::new("goproxy", "/proxy/reg/example.com/m@v/list")
            .coord("example.com/m", "v9.8.7")
            .meta(go_list_meta),
        // ── rubygems — no finding names it; that is why it is here ───────────
        Row::new("rubygems", "/proxy/reg/gems/pkg-9.8.7.gem"),
        Row::new("rubygems", "/proxy/reg/api/v1/versions/pkg.json"),
        // Was survey finding 16, found by this matrix.
        Row::new("rubygems", "/proxy/reg/info/pkg"),
        // Was survey finding 16. Answers `200` with an *empty* document rather
        // than `403`: it is built by asking each package in turn, and a caller
        // the chain denies is denied every one of them. An empty index discloses
        // nothing, which is what this axis measures.
        Row::new("rubygems", "/proxy/reg/versions").vis(WHOLE_REGISTRY),
        // Same `serve_compact` branch, same empty-document answer. `/names` stays
        // deliberately unfiltered for *blocking* — a gem with one blocked version
        // still exists — which was always a separate question from whether an
        // RBAC-denied caller may read it at all.
        Row::new("rubygems", "/proxy/reg/names").vis(WHOLE_REGISTRY),
        Row::new(
            "rubygems",
            "/proxy/reg/quick/Marshal.4.8/pkg-9.8.7.gemspec.rz",
        )
        .vis(Expect::NotChecked(
            "gem_gemspec has no local branch — it always goes through proxy_stream, so it \
                 never reads the local package whose visibility this axis sets",
        ))
        .no_control(), // needs a stored gemspec, not the flat fixture
        Row::new("rubygems", "/proxy/reg/api/v1/gems/pkg.json"),
        // ── composer ─────────────────────────────────────────────────────────
        Row::new("composer", "/proxy/reg/p2/vendor/pkg.json").pkg("vendor/pkg"),
        Row::new("composer", "/proxy/reg/dist/vendor/pkg/9.8.7").pkg("vendor/pkg"),
        // ── maven ────────────────────────────────────────────────────────────
        Row::new(
            "maven",
            "/proxy/reg/maven2/com/acme/pkg/9.8.7/pkg-9.8.7.jar",
        )
        .pkg("com.acme:pkg")
        .no_control(), // needs a maven multi-file artifact key, not the flat one
        // ── terraform ────────────────────────────────────────────────────────
        // Was survey finding 8.
        Row::new("terraform", "/proxy/reg/v1/providers/acme/vault/versions")
            .pkg("providers/acme/vault")
            .meta(terraform_provider_meta),
        // ── jetbrains marketplace ────────────────────────────────────────────
        // Was survey finding 5.
        Row::new(
            "jetbrains-marketplace",
            "/proxy/reg/pluginManager?action=download&id=org.acme.plugin",
        )
        .pkg("org.acme.plugin")
        .meta(plugin_meta),
        // ── openvsx — the route this class was first found and fixed on ──────
        Row::new("openvsx", "/proxy/reg/acme.ext/9.8.7/vsix")
            .pkg("acme.ext")
            .meta(extension_meta),
        // ── further rows, added after the first pass found rubygems ──────────
        Row::new("npm", "/proxy/reg/-/package/pkg/dist-tags"),
        Row::new("npm", "/proxy/reg/pkg/9.8.7"),
        Row::new("pypi", "/proxy/reg/pypi/pkg/json")
            .meta(pypi_meta)
            .vis(Expect::NotChecked(
                "pypi_json renders from ProxyService::version_document with no local branch, so \
                 it never reads the local package whose visibility this axis sets",
            )),
        Row::new("pypi", "/proxy/reg/packages/pkg-9.8.7.tar.gz").meta(pypi_meta),
        Row::new("nuget", "/proxy/reg/nuget/v3/registration5/pkg/index.json"),
        // Was survey finding 15, found by this matrix: the cargo *sparse index*,
        // the read path of every `cargo build`. `serve_local_index` called
        // `get_index` with no chain while `proxy_upstream_index` directly below
        // it carried a comment recording that this exact gap — "a private cargo
        // registry's crate names and versions were readable by anyone who could
        // reach the port" — was why the proxy path moved onto `ProxyService`.
        // Closed there, left open here.
        Row::new("cargo", "/proxy/reg/registry/pk/g/pkg").meta(cargo_index_meta),
        Row::new("goproxy", "/proxy/reg/example.com/m@latest").coord("example.com/m", "v9.8.7"),
        // Was survey finding 8.
        Row::new(
            "terraform",
            "/proxy/reg/v1/providers/acme/vault/9.8.7/download/linux/amd64",
        )
        .pkg("providers/acme/vault")
        .meta(terraform_provider_meta),
        // ── ecosystems no survey finding names: deb/rpm/pacman, generic, vsx ──
        //
        // The completeness critic's judgement was that these are "far more
        // likely to be *unexamined* than *correct*", which is the whole reason
        // the matrix exists rather than a list of the handlers already known bad.
        Row::new("deb", "/proxy/reg/deb/dists/stable/Release")
            .vis(WHOLE_REGISTRY)
            .no_control(), // repo metadata is generated, not the flat fixture
        Row::new("rpm", "/proxy/reg/rpm/repodata/repomd.xml")
            .vis(WHOLE_REGISTRY)
            .no_control(),
        Row::new("pacman", "/proxy/reg/pacman/reg.db")
            .vis(WHOLE_REGISTRY)
            .no_control(),
        // Axis B is not a finding, and worth stating so nobody re-raises it:
        // `generic` is a path mirror with no local branch at all. Its coordinate
        // is the synthetic `repo/_` with the whole request path as the artifact,
        // so it never reads the local package this axis marks Internal — the
        // `200` is the upstream's file, and the fixture's URL merely contains the
        // string `pkg`.
        Row::new("generic", "/proxy/reg/generic/pkg/9.8.7/file.bin")
            .vis(Expect::NotChecked(
                "path mirror: the coordinate is repo/_ and no local package is read",
            ))
            .no_control(),
        // ── nodedist (RFC 0010) ──────────────────────────────────────────────
        // Proxy-only like `generic`, so axis B has no local package to read —
        // but unlike `generic` the coordinate is real (`node` / `v9.8.7`), which
        // is exactly what makes a Node release blockable. The two listings are
        // whole-registry documents: one package, every release.
        Row::new("nodedist", "/proxy/reg/nodedist/v9.8.7/SHASUMS256.txt")
            .coord("node", "v9.8.7")
            .vis(Expect::NotChecked(
                "proxy-only: the file is streamed from upstream and no local package is read",
            )),
        Row::new("nodedist", "/proxy/reg/nodedist/index.tab")
            .coord("node", "v9.8.7")
            .token("v1.1.0")
            .vis(WHOLE_REGISTRY),
        Row::new("nodedist", "/proxy/reg/nodedist/index.json")
            .coord("node", "v9.8.7")
            .token("v1.1.0")
            .vis(WHOLE_REGISTRY),
        // ── sdkman (RFC 0010 phase 6) ────────────────────────────────────────
        // Proxy-only like `nodedist`, with a real coordinate: the candidate is
        // the package and the platform the artifact. The per-candidate listings
        // carry no local package to check; the three relayed API documents that
        // name no package at all (`healthcheck`, `broker/version`, `selfupdate`)
        // are `NoPackage` in the inventory rather than rows.
        Row::new(
            "sdkman",
            "/proxy/reg/sdkman/broker/download/pkg/9.8.7/linuxx64",
        )
        .vis(Expect::NotChecked(
            "proxy-only: the archive is streamed through the broker and no local package is read",
        )),
        Row::new(
            "sdkman",
            "/proxy/reg/sdkman/candidates/pkg/linuxx64/versions/all",
        )
        .token("1.1.0")
        .vis(Expect::NotChecked(
            "proxy-only listing: no local package to gate on",
        )),
        Row::new(
            "sdkman",
            "/proxy/reg/sdkman/candidates/pkg/linuxx64/versions/list?current=&installed=",
        )
        .vis(Expect::NotChecked(
            "proxy-only listing: no local package to gate on",
        )),
        Row::new("sdkman", "/proxy/reg/sdkman/candidates/default/pkg")
            .token("1.1.0")
            .vis(Expect::NotChecked(
                "proxy-only listing: no local package to gate on",
            )),
        Row::new(
            "sdkman",
            "/proxy/reg/sdkman/candidates/validate/pkg/9.8.7/linuxx64",
        )
        .token("valid")
        .vis(Expect::NotChecked(
            "proxy-only: the answer is one word about an upstream version",
        )),
        Row::new("sdkman", "/proxy/reg/sdkman/hooks/post/pkg/9.8.7/linuxx64").vis(
            Expect::NotChecked(
                "proxy-only: a hook script relayed byte-exact, no local package is read",
            ),
        ),
        Row::new("sdkman", "/proxy/reg/sdkman/candidates/all")
            .token("java")
            .vis(WHOLE_REGISTRY),
        Row::new("sdkman", "/proxy/reg/sdkman/candidates/list")
            .token("fixture")
            .vis(WHOLE_REGISTRY),
        Row::new(
            "vscode-marketplace",
            "/proxy/reg/vscode/asset/acme/ext/9.8.7/Microsoft.VisualStudio.Services.VSIXPackage",
        )
        .pkg("acme.ext")
        .meta(extension_meta)
        .no_control(),
        Row::new(
            "vscode-marketplace",
            "/proxy/reg/vscode/gallery/publishers/acme/vsextensions/ext/9.8.7/vspackage",
        )
        .pkg("acme.ext")
        .meta(extension_meta)
        .no_control(),
        // ── terraform modules (the provider half is covered above) ───────────
        Row::new("terraform", "/proxy/reg/v1/modules/acme/vpc/aws/versions")
            .pkg("modules/acme/vpc/aws")
            .meta(terraform_module_meta)
            .no_control(),
        // ── search / whole-registry indexes ──────────────────────────────────
        // Was survey finding 11. `resolve_and_search` took the identity and
        // dropped it (`let _ = identity`); it now authorises the listing *and*
        // filters the local hits, so both halves of that finding are covered by
        // this row and by `search.rs`'s own tests.
        Row::new("npm", "/proxy/reg/-/v1/search?text=pkg")
            .vis(WHOLE_REGISTRY)
            .no_control(),
        // Was survey finding 11 — the same `resolve_and_search` middle.
        Row::new("nuget", "/proxy/reg/nuget/v3/query?q=pkg")
            .vis(WHOLE_REGISTRY)
            .no_control(),
        Row::new("composer", "/proxy/reg/packages.json")
            .pkg("vendor/pkg")
            .vis(WHOLE_REGISTRY)
            .no_control(),
        // ── jetbrains marketplace: the XML and files routes ──────────────────
        Row::new(
            "jetbrains-marketplace",
            "/proxy/reg/plugins/list?build=IU-241.1",
        )
        .pkg("org.acme.plugin")
        .meta(plugin_meta)
        .vis(WHOLE_REGISTRY)
        .no_control(),
        Row::new("jetbrains-marketplace", "/proxy/reg/updatePlugins.xml")
            .pkg("org.acme.plugin")
            .meta(plugin_meta)
            .vis(WHOLE_REGISTRY)
            .no_control(),
        Row::new(
            "jetbrains-marketplace",
            "/proxy/reg/plugin/download?pluginId=org.acme.plugin&version=9.8.7",
        )
        .pkg("org.acme.plugin")
        .meta(plugin_meta),
        // ── routes the inventory claimed and no row reached ──────────────────
        //
        // Five entries were marked `Coverage::Row` with nothing behind them,
        // while five others were marked `NoRow` and were being exercised all
        // along. The counts matched, so `every_row_is_accounted_for_in_the_inventory`
        // saw nothing; `coverage_claims_match_the_routes_rows_actually_reach`
        // is what found it, and these rows are what make the five claims true
        // rather than retracted.
        // The namespace listing keys extensions by their short name, so the
        // coordinate `acme.zqfixture` shows up as `zqfixture` and nothing else
        // in the document resembles it — `ext` would have matched the literal
        // `"extensions"` key and reported a leak from an empty document.
        Row::new("openvsx", "/proxy/reg/api/acme")
            .pkg("acme.zqfixture")
            .token("zqfixture")
            .meta(namespaced_extension_meta)
            .vis(WHOLE_REGISTRY),
        Row::new("openvsx", "/proxy/reg/api/acme/ext")
            .pkg("acme.ext")
            .meta(extension_meta),
        Row::new("openvsx", "/proxy/reg/api/acme/ext/9.8.7")
            .pkg("acme.ext")
            .meta(extension_meta),
        Row::new("terraform", "/proxy/reg/v1/modules/acme/vpc/aws/9.8.7")
            .pkg("modules/acme/vpc/aws")
            .meta(terraform_module_version_meta)
            // Like the `/versions` row above it: the handler answers from
            // `ProxyService::version_document` rather than from the local
            // registry, so the fixture's published module never reaches it and
            // a permitted caller gets the same `404`. The negative assertion is
            // what this row is for — the chain must run before the fall-through.
            .no_control(),
        // ── reads that arrive as POST ────────────────────────────────────────
        //
        // Neither of these was in any inventory: the read gate filters on
        // `item.get`, and the write gate below would have classified them as
        // writes they are not. Both are searches, and a search that answers an
        // anonymous caller with a package name is survey finding 11 with a
        // different verb on the front of it.
        Row::new("openvsx", "/proxy/reg/vscode/gallery/extensionquery")
            .pkg("acme.ext")
            .meta(extension_meta)
            .post(extension_query_body)
            .vis(WHOLE_REGISTRY),
        Row::new(
            "jetbrains-marketplace",
            "/proxy/reg/api/search/updates/compatible",
        )
        .pkg("org.acme.plugin")
        .meta(plugin_meta)
        .post(jbm_compatible_body)
        .vis(WHOLE_REGISTRY)
        // The fixture publishes no plugin *update* rows, which is what this
        // route answers from — a permitted caller gets `[]` too.
        .no_control(),
    ]
}

/// The VS Code gallery query an editor sends to resolve one extension by name.
fn extension_query_body() -> (Vec<u8>, &'static str) {
    let body = json!({
        "filters": [{
            "criteria": [
                { "filterType": 8, "value": "Microsoft.VisualStudio.Code" },
                { "filterType": 7, "value": "acme.ext" }
            ],
            "pageNumber": 1, "pageSize": 50, "sortBy": 0, "sortOrder": 0
        }],
        "assetTypes": [],
        // IncludeVersions | IncludeFiles | IncludeVersionProperties
        "flags": 0x1 | 0x2 | 0x10
    });
    (
        serde_json::to_vec(&body).expect("query body"),
        "application/json",
    )
}

/// The JetBrains compatible-updates lookup, keyed by IDE build.
fn jbm_compatible_body() -> (Vec<u8>, &'static str) {
    let body = json!({ "build": "IU-241.1", "pluginXMLIds": ["org.acme.plugin"] });
    (
        serde_json::to_vec(&body).expect("query body"),
        "application/json",
    )
}

/// Build a local-mode registry whose RBAC *allows* anonymous, holding one
/// published package set to `Visibility::Internal`.
///
/// Axis B in isolation: the chain permits the read, so anything that refuses is
/// `check_visibility` and nothing else. `check_visibility` returns `Ok(())`
/// outright when no `team_namespace` port is wired, so the port is the fixture —
/// without it every row would pass vacuously.
async fn vis_app_for(row: &Row) -> impl TestService {
    let ns_store = batlehub_adapters::in_memory::InMemoryTeamNamespaceStore::new();
    let mut parts = local_only_app_parts_with_policy(
        "reg",
        row.kind,
        RegistryMode::Local,
        true,
        rbac_policy_anon_source,
    )
    .await;

    let cur = parts.local_svc.clone();
    parts.local_svc = Arc::new(batlehub_core::services::LocalRegistryService {
        backend: cur.backend.clone(),
        storage: cur.storage.clone(),
        hot: cur.hot.clone(),
        quota: cur.quota.clone(),
        ownership: cur.ownership.clone(),
        team_namespace: Some(ns_store.clone() as Arc<dyn batlehub_core::ports::TeamNamespacePort>),
        sbom: cur.sbom.clone(),
        explore_cache: cur.explore_cache.clone(),
        package_repo: cur.package_repo.clone(),
        readme: cur.readme.clone(),
    });

    seed(&parts, row).await;

    // Internal rather than Team: it refuses anything below `User` with no group
    // setup, so the row tests the gate rather than the fixture's group wiring.
    ns_store
        .set_visibility(
            "reg",
            row.name,
            batlehub_core::entities::Visibility::Internal,
        )
        .await
        .expect("set_visibility");

    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

/// Publish the row's package into `parts`.
async fn seed(parts: &LocalRegistryAppParts, row: &Row) {
    publish_fixture(parts, row.name, row.version, (row.meta)()).await;
}

/// Publish one coordinate into `parts`, as `user-1`.
///
/// Shared by the read rows and the write rows: a yank has to have something to
/// yank, and a fixture that seeds differently for the two halves is a fixture
/// whose two halves can disagree.
async fn publish_fixture(
    parts: &LocalRegistryAppParts,
    name: &str,
    version: &str,
    index_metadata: serde_json::Value,
) {
    let artifact = Bytes::from_static(FIXTURE_BYTES);
    let checksum = hex::encode(Sha256::digest(&artifact));
    parts
        .local_svc
        .publish(PublishRequest {
            registry: "reg".to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            artifact,
            checksum,
            index_metadata,
            unlisted: false,
            publisher: Identity {
                user_id: Some("user-1".to_owned()),
                role: Role::User,
                auth_provider: None,
                groups: vec![],
            },
            signature_bytes: None,
            signature_type: None,
        })
        .await
        .expect("matrix fixture must publish");
}

/// Build a local-mode registry denying anonymous everything, holding one
/// published package, and return the app.
async fn app_for(row: &Row) -> impl TestService {
    // `upstream: true` even though every row is a **local**-mode registry.
    // Several local read paths render their document through
    // `ProxyService::version_document`, whose `request_prelude` resolves the
    // registry *client* before authorizing — so with no client the route 400s
    // before the gate is reached and the row asserts nothing. The upstream
    // cannot weaken the negative assertion: the rule chain runs ahead of any
    // fall-through, so an anonymous caller is refused whether or not one exists.
    let parts = local_only_app_parts_with_policy(
        "reg",
        row.kind,
        RegistryMode::Local,
        true,
        rbac_policy_deny_anonymous,
    )
    .await;

    seed(&parts, row).await;
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

// ── Completeness ─────────────────────────────────────────────────────────────
//
// The rows above are only as good as their coverage, and until this section
// existed nothing failed when a route shipped without one. The file's own
// guidance said as much — "a new ecosystem's read routes are not covered until
// someone does" — which is the same *by convention rather than by construction*
// failure the matrix exists to end, moved up one level. A ninth ecosystem would
// have arrived with no rows, a green suite and no signal.
//
// So the inventory below names **every** `GET` this server registers under
// `/proxy/**`, and the test asserts it against the live route table in both
// directions. Adding a route fails until it is classified here; deleting one
// fails until its entry goes. Neither can happen by accident, and neither can
// happen without a reviewer seeing the diff.
//
// # How to extend
//
// 1. Add the route to [`ROUTE_INVENTORY`] — the failure message prints the line.
// 2. Choose its [`Coverage`]:
//    - [`Coverage::Row`] once a row in [`matrix`] makes a real request to it;
//    - [`Coverage::NoPackage`] if the response contains no package, with the
//      reason;
//    - [`Coverage::NoRow`] otherwise — a package read nobody has covered yet.
// 3. If you chose `NoRow`, adding the row is the actual work. `NoRow` is a
//    placeholder that keeps the suite honest, not a resting place.
//
// # Why an exact list rather than pattern-matching rows onto routes
//
// It was written the other way first: derive coverage by testing whether a
// row's concrete URI is routed by a template. That needs a router, and a
// hand-written approximation of one got it wrong in both directions — a path
// parameter is not always a single segment (`{module}` holds
// `example.com/team/lib`), and a greedy matcher then claims coverage that does
// not exist. A wrong "covered" is exactly the outcome this file exists to
// prevent, so the mapping is stated instead of inferred.

/// Whether the matrix exercises a route, and if not, why not.
#[derive(Debug, Clone, Copy)]
enum Coverage {
    /// A row in [`matrix`] makes a real request to this route, so both axes are
    /// asserted against real behaviour.
    Row,
    /// The response contains no package, so neither axis applies: a service
    /// index, a liveness probe, a CVE feed. A claim about the route, so it is
    /// recorded per route rather than inferred from a path prefix.
    NoPackage(&'static str),
    /// A package read with no row yet — the route-level twin of
    /// [`Expect::KnownGap`]. Finite, visible, and meant to shrink.
    NoRow(&'static str),
}

const ROUTE_INVENTORY: &[(&str, Coverage)] = &[
    ("/proxy/{registry}/-/package/{package}/dist-tags", Coverage::Row),
    ("/proxy/{registry}/-/ping", Coverage::NoPackage("npm liveness handshake; answers the same to everyone")),
    ("/proxy/{registry}/-/v1/search", Coverage::Row),
    ("/proxy/{registry}/-/whoami", Coverage::NoPackage("echoes the caller's own identity, never a package")),
    ("/proxy/{registry}/.well-known/terraform.json", Coverage::NoPackage("Terraform service discovery; static endpoint map")),
    ("/proxy/{registry}/api/-/search", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/packages/{path}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/plugins/{id}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/plugins/{id}/updates", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/products/intellij/plugins/{id}/comments", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/search/aggregation/{field}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/search/plugins", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/searchPlugins", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/security-advisories/", Coverage::NoPackage("Composer advisory feed; CVE data, not package contents")),
    ("/proxy/{registry}/api/v1/crates", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/v1/crates/{name}/owners", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/v1/gems/{name}.json", Coverage::Row),
    ("/proxy/{registry}/api/v1/versions/{name}.json", Coverage::Row),
    ("/proxy/{registry}/api/v4/{path}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/api/version", Coverage::NoPackage("JetBrains marketplace API version banner")),
    ("/proxy/{registry}/api/{namespace}", Coverage::Row),
    ("/proxy/{registry}/api/{namespace}/{extension}", Coverage::Row),
    ("/proxy/{registry}/api/{namespace}/{extension}/{version}", Coverage::Row),
    ("/proxy/{registry}/api/{namespace}/{extension}/{version}/file/{filename}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/channeldata.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/deb/{path}", Coverage::Row),
    ("/proxy/{registry}/dist/{vendor}/{package}/{version}", Coverage::Row),
    ("/proxy/{registry}/feature/getImplementations", Coverage::NoPackage("JetBrains feature lookup; no package coordinate in the answer")),
    ("/proxy/{registry}/files/IDE/extensions.json", Coverage::NoPackage("JetBrains IDE-wide bundled-extension manifest")),
    ("/proxy/{registry}/files/brokenPlugins.json", Coverage::NoPackage("JetBrains compatibility blocklist, published upstream for every IDE")),
    ("/proxy/{registry}/files/jbPluginsXMLIds.json", Coverage::NoPackage("JetBrains ID index, upstream-wide")),
    ("/proxy/{registry}/files/pluginsXMLIds.json", Coverage::NoPackage("JetBrains ID index, upstream-wide")),
    ("/proxy/{registry}/files/{plugin}/meta.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/files/{plugin}/{update}/meta.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/files/{plugin}/{update}/{file_name}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/gems/{filename}", Coverage::Row),
    ("/proxy/{registry}/generic/{path}", Coverage::Row),
    ("/proxy/{registry}/info/{gem}", Coverage::Row),
    ("/proxy/{registry}/jetbrains/{path}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/latest_specs.4.8.gz", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/list.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/maven2/{path}", Coverage::Row),
    ("/proxy/{registry}/names", Coverage::Row),
    ("/proxy/{registry}/nodedist/index.json", Coverage::Row),
    ("/proxy/{registry}/nodedist/index.tab", Coverage::Row),
    ("/proxy/{registry}/nodedist/{version}/{file}", Coverage::Row),
    ("/proxy/{registry}/sdkman/broker/download/{candidate}/{version}/{platform}", Coverage::Row),
    ("/proxy/{registry}/sdkman/broker/version/sdkman/{component}/{channel}", Coverage::NoPackage("the SDKMAN script/native version on a channel; names no package")),
    ("/proxy/{registry}/sdkman/candidates/all", Coverage::Row),
    ("/proxy/{registry}/sdkman/candidates/default/{candidate}", Coverage::Row),
    ("/proxy/{registry}/sdkman/candidates/list", Coverage::Row),
    ("/proxy/{registry}/sdkman/candidates/validate/{candidate}/{version}/{platform}", Coverage::Row),
    ("/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/all", Coverage::Row),
    ("/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/list", Coverage::Row),
    ("/proxy/{registry}/sdkman/healthcheck", Coverage::NoPackage("the upstream health token, relayed as-is; names no package")),
    ("/proxy/{registry}/sdkman/hooks/{phase}/{candidate}/{version}/{platform}", Coverage::Row),
    ("/proxy/{registry}/sdkman/selfupdate/{channel}/{platform}", Coverage::NoPackage("the SDKMAN self-update script for a channel; names no package")),
    ("/proxy/{registry}/nuget/v3/autocomplete", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/nuget/v3/flat/{id}/index.json", Coverage::Row),
    ("/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}", Coverage::Row),
    ("/proxy/{registry}/nuget/v3/index.json", Coverage::NoPackage("NuGet service index; the list of this registry's own endpoints")),
    ("/proxy/{registry}/nuget/v3/query", Coverage::Row),
    ("/proxy/{registry}/nuget/v3/registration5/{id}/index.json", Coverage::Row),
    ("/proxy/{registry}/nuget/v3/vulnerabilities/index.json", Coverage::NoPackage("NuGet vulnerability feed; CVE data, not package contents")),
    ("/proxy/{registry}/nuget/v3/vulnerabilities/page/{page}", Coverage::NoPackage("NuGet vulnerability feed page; CVE data, not package contents")),
    ("/proxy/{registry}/p2/{path}", Coverage::Row),
    ("/proxy/{registry}/packages.json", Coverage::Row),
    ("/proxy/{registry}/packages/{filename}", Coverage::Row),
    ("/proxy/{registry}/pacman/{path}", Coverage::Row),
    ("/proxy/{registry}/plugin/download", Coverage::Row),
    ("/proxy/{registry}/pluginManager", Coverage::Row),
    ("/proxy/{registry}/plugins/list", Coverage::Row),
    ("/proxy/{registry}/prerelease_specs.4.8.gz", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/pypi/{package}/json", Coverage::Row),
    ("/proxy/{registry}/quick/Marshal.4.8/{filename}", Coverage::Row),
    ("/proxy/{registry}/registry/config.json", Coverage::NoPackage("cargo sparse-index config; names the download endpoint")),
    ("/proxy/{registry}/registry/{path}", Coverage::Row),
    ("/proxy/{registry}/rpm/{path}", Coverage::Row),
    ("/proxy/{registry}/search.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/simple/", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/simple/{package}/", Coverage::Row),
    ("/proxy/{registry}/specs.4.8.gz", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/sumdb/{path}", Coverage::NoPackage("Go checksum-database mirror; upstream notary data, not this registry's packages")),
    ("/proxy/{registry}/updatePlugins.xml", Coverage::Row),
    ("/proxy/{registry}/v1/ID/{id}.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/v1/index.json", Coverage::NoPackage("Terraform service discovery, subpath form")),
    ("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions", Coverage::Row),
    ("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}", Coverage::Row),
    ("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/artifact", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/download", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions", Coverage::Row),
    ("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/download/{os}/{arch}", Coverage::Row),
    ("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/versions", Coverage::Row),
    ("/proxy/{registry}/vscode/asset/{publisher}/{name}/{version}/{asset_type}", Coverage::Row),
    ("/proxy/{registry}/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage", Coverage::Row),
    ("/proxy/{registry}/vscode/item", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/vscode/unpkg/{publisher}/{name}/{version}/{path}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{extension_id}/{version}/vsix", Coverage::Row),
    ("/proxy/{registry}/{hostname}/{namespace}/{ptype}/index.json", Coverage::NoRow("package read, not yet exercised: the Terraform provider *mirror*, which refuses any hostname that is not the registry's own upstream and answers `no upstream configured` against this fixture's FixedRegistry. Needs a registry with a real upstream URL, not a local-mode one")),
    ("/proxy/{registry}/{hostname}/{namespace}/{ptype}/{version}.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{module}/@latest", Coverage::Row),
    ("/proxy/{registry}/{module}/@v/list", Coverage::Row),
    ("/proxy/{registry}/{module}/@v/{filename}", Coverage::Row),
    ("/proxy/{registry}/{name}/{version}/download", Coverage::Row),
    ("/proxy/{registry}/{owner}/{repo}/branches/{branch}", Coverage::NoRow("RFC 0019 [api_reads]: opt-in, off by default, exercised in forge_api_reads.rs")),
    ("/proxy/{registry}/{owner}/{repo}/commits/{sha}", Coverage::NoRow("RFC 0019 [api_reads]: opt-in, off by default, exercised in forge_api_reads.rs")),
    ("/proxy/{registry}/{owner}/{repo}/raw/{git_ref}/{path}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{owner}/{repo}/releases", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{owner}/{repo}/releases/assets/{asset_id}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{owner}/{repo}/releases/download/{tag}/{filename}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{owner}/{repo}/releases/tags/{tag}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{owner}/{repo}/tags", Coverage::NoRow("RFC 0019 [api_reads]: opt-in, off by default, exercised in forge_api_reads.rs")),
    ("/proxy/{registry}/{owner}/{repo}/tarball/{tag}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{owner}/{repo}/zipball/{tag}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{package}", Coverage::Row),
    ("/proxy/{registry}/{package}/{version}", Coverage::Row),
    ("/proxy/{registry}/{package}/{version}/tarball", Coverage::Row),
    ("/proxy/{registry}/{platform}/current_repodata.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{platform}/repodata.json", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{platform}/repodata.json.bz2", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{platform}/repodata.json.zst", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{platform}/{filename}", Coverage::Row),
    ("/proxy/{registry}/{project}/-/archive/{tag}/{filename}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{project}/-/raw/{git_ref}/{path}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{project}/-/releases", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{project}/-/releases/{tag}", Coverage::NoRow("package read, not yet exercised")),
    ("/proxy/{registry}/{project}/-/releases/{tag}/downloads/{name}", Coverage::NoRow("package read, not yet exercised")),
];

/// The inventory and the router agree, exactly.
///
/// Two failure directions, both of which have to be loud. A route the server
/// registers and the inventory does not name is a read nobody has considered —
/// the whole finding class in one sentence. An entry naming a route the server
/// no longer registers is a claim of coverage over nothing, which hides the day
/// that path comes back under a different handler.
#[test]
fn the_route_inventory_matches_the_router() {
    let spec = batlehub_web::openapi_spec();
    let registered: Vec<String> = spec
        .paths
        .paths
        .iter()
        .filter(|(path, item)| path.starts_with("/proxy/") && item.get.is_some())
        .map(|(path, _)| path.clone())
        .collect();

    let listed: Vec<&str> = ROUTE_INVENTORY.iter().map(|(p, _)| *p).collect();

    let unlisted: Vec<&String> = registered
        .iter()
        .filter(|p| !listed.contains(&p.as_str()))
        .collect();
    let stale: Vec<&str> = listed
        .iter()
        .filter(|p| !registered.iter().any(|r| r == *p))
        .copied()
        .collect();

    let mut report = String::new();
    if !unlisted.is_empty() {
        report.push_str(&format!(
            "\n{} proxy GET route(s) are registered but absent from ROUTE_INVENTORY.\n\
             Every read route has to be classified, or the next ecosystem repeats the\n\
             2026-08-26 survey's finding class in silence. Add:\n\n{}\n\n\
             …then either write a row in `matrix()` and mark it Coverage::Row, or record\n\
             why it needs none.\n",
            unlisted.len(),
            unlisted
                .iter()
                .map(|p| format!(
                    "    (\"{p}\", Coverage::NoRow(\"package read, not yet exercised\")),"
                ))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    if !stale.is_empty() {
        report.push_str(&format!(
            "\n{} ROUTE_INVENTORY entr(ies) name a route this server no longer registers.\n\
             Delete them — a stale entry claims coverage of nothing:\n  {}\n",
            stale.len(),
            stale.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");
}

/// Every row points at a route that exists, and every `Coverage::Row` has one.
///
/// The two halves can drift apart silently: a row whose URI stops routing
/// anywhere still "passes" its negative assertion, because a `404` reads as
/// *denied*. Its positive control is what normally catches that — this is the
/// cheaper check that says so directly.
#[test]
fn every_row_is_accounted_for_in_the_inventory() {
    let claimed = ROUTE_INVENTORY
        .iter()
        .filter(|(_, c)| matches!(c, Coverage::Row))
        .count();
    let rows = matrix().len();
    assert!(
        claimed > 0 && rows > 0,
        "the matrix and the inventory must both be non-empty"
    );
    assert!(
        rows >= claimed,
        "ROUTE_INVENTORY claims {claimed} routes are exercised by a row, but `matrix()` \
         only has {rows} rows — at least one claim has no test behind it"
    );
}

/// How much of the read surface is actually exercised, printed on every run.
///
/// Not an assertion — a number that goes up. It is here because the honest
/// answer to "is this suite good enough" is a ratio, and the ratio is otherwise
/// invisible: a green suite with 43 of 113 routes covered looks exactly like a
/// green suite with all of them.
#[test]
fn report_read_surface_coverage() {
    let total = ROUTE_INVENTORY.len();
    let rows = ROUTE_INVENTORY
        .iter()
        .filter(|(_, c)| matches!(c, Coverage::Row))
        .count();
    let no_package = ROUTE_INVENTORY
        .iter()
        .filter(|(_, c)| matches!(c, Coverage::NoPackage(_)))
        .count();
    let no_row: Vec<(&str, &str)> = ROUTE_INVENTORY
        .iter()
        .filter_map(|(p, c)| match c {
            Coverage::NoRow(why) => Some((*p, *why)),
            _ => None,
        })
        .collect();
    let denominator = total - no_package;
    println!(
        "authz matrix: {rows}/{denominator} package-read routes exercised \
         ({} with no row, {no_package} disclose no package, {total} total)",
        no_row.len()
    );
    for (path, why) in &no_row {
        println!("  no row: {path} — {why}");
    }
}

/// Every classification states a reason, and every reason says something.
///
/// `NoPackage` and `NoRow` are both assertions about a route that nobody has
/// tested — the only thing standing behind them is the sentence next to them, so
/// an empty one is a classification with no argument at all.
#[test]
fn every_unexercised_route_gives_a_reason() {
    let empty: Vec<&str> = ROUTE_INVENTORY
        .iter()
        .filter_map(|(path, c)| match c {
            Coverage::NoPackage(why) | Coverage::NoRow(why) if why.trim().is_empty() => Some(*path),
            _ => None,
        })
        .collect();
    assert!(
        empty.is_empty(),
        "these routes are excused with no reason given:\n  {}",
        empty.join("\n  ")
    );
}

/// What one axis of one row asserts, and how it words itself when it fails.
struct Axis {
    /// The `[chain]` / `[visibility]` prefix every message from this axis carries.
    label: &'static str,
    expect: Expect,
    /// Appended to the broken-control message, which differs per axis.
    control_note: &'static str,
    /// The middle of the "disclosed …" message, which also differs per axis.
    served_note: &'static str,
}

/// The three lists the final report is built from.
struct AxisReport<'a> {
    failures: &'a mut Vec<String>,
    broken_controls: &'a mut Vec<String>,
    gaps_now_fixed: &'a mut Vec<String>,
}

/// Run one axis: the positive control first, then the anonymous assertion.
///
/// The two axes differ only in which app they drive, which `Expect` they read
/// and how they word themselves — so they share this, and a fix to the control
/// logic cannot land on one axis and miss the other.
async fn check_axis<S>(app: &S, row: &Row, axis: Axis, out: &mut AxisReport<'_>)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let label = axis.label;
    // Positive control: a caller the policy grants must be served, or the
    // negative assertion below proves nothing.
    if row.control {
        let (shown, status) = disclosed_as(app, row, Some(USER_TOKEN)).await;
        if !shown {
            out.broken_controls.push(format!(
                "[{label}] {} {} — a permitted caller did NOT see the package (status {status}). {}",
                row.kind, row.uri, axis.control_note,
            ));
            return;
        }
    }

    let (served, status) = disclosed(app, row).await;
    match axis.expect {
        Expect::Denied if served => out.failures.push(format!(
            "[{label}] {} {} — {} (status {status})",
            row.kind, row.uri, axis.served_note,
        )),
        Expect::KnownGap(_) if !served => out.gaps_now_fixed.push(format!(
            "[{label}] {} {} — now correctly refuses ({status}). Flip this row to Expect::Denied.",
            row.kind, row.uri,
        )),
        _ => {}
    }
}

/// The matrix.
///
/// One test rather than one per row, because the value is in the table being
/// exhaustive and read as a whole; a failure names the row precisely.
#[actix_web::test]
async fn every_local_read_route_enforces_the_registry_rule_chain() {
    let mut failures: Vec<String> = Vec::new();
    let mut broken_controls: Vec<String> = Vec::new();
    let mut gaps_now_fixed: Vec<String> = Vec::new();

    let mut report = AxisReport {
        failures: &mut failures,
        broken_controls: &mut broken_controls,
        gaps_now_fixed: &mut gaps_now_fixed,
    };

    for row in matrix() {
        // ── Axis A: the registry rule chain ──────────────────────────────────
        let app = app_for(&row).await;
        check_axis(
            &app,
            &row,
            Axis {
                label: "chain",
                expect: row.expect,
                control_note: "The fixture or the route is wrong, and the anonymous assertion \
                               below proves nothing until it is fixed.",
                served_note: "disclosed the package to a caller the registry's RBAC denies",
            },
            &mut report,
        )
        .await;

        // ── Axis B: per-package visibility ───────────────────────────────────
        if matches!(row.expect_vis, Expect::NotChecked(_)) {
            continue;
        }
        // A `User` is above the bar `Visibility::Internal` sets, so the same
        // request must still be served — the control for this axis. Anonymous:
        // the chain allows, and only `check_visibility` can refuse.
        let vapp = vis_app_for(&row).await;
        check_axis(
            &vapp,
            &row,
            Axis {
                label: "visibility",
                expect: row.expect_vis,
                control_note: "fixture or route is wrong",
                served_note: "disclosed an Internal-visibility package to an anonymous caller",
            },
            &mut report,
        )
        .await;
    }

    let mut report = String::new();
    if !broken_controls.is_empty() {
        report.push_str(&format!(
            "\n{} row(s) whose positive control failed:\n  {}\n",
            broken_controls.len(),
            broken_controls.join("\n  ")
        ));
    }
    if !failures.is_empty() {
        report.push_str(&format!(
            "\n{} NEW authorization gap(s):\n  {}\n",
            failures.len(),
            failures.join("\n  ")
        ));
    }
    if !gaps_now_fixed.is_empty() {
        report.push_str(&format!(
            "\n{} known gap(s) that appear fixed:\n  {}\n",
            gaps_now_fixed.len(),
            gaps_now_fixed.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");
}

// ═══ The write surface ═══════════════════════════════════════════════════════
//
// Everything above this line is about reads, and until now so was the whole
// file: [`ROUTE_INVENTORY`] filters the router on `item.get`, so a `PUT` that
// publishes a package, a `DELETE` that yanks one and the owners API were not
// merely uncovered — they were not *counted*. RFC 0015 §11.1 names that as the
// first thing to fix, and the reason is the same one the read half opens with:
// today's write authority is one `has_role_at_least(&Role::User)` repeated at
// seven call sites (`local_registry/publish.rs`, six more in `lifecycle.rs`),
// applied by convention rather than by construction. A handler that forgets it
// looks exactly like one that does not.
//
// # What a write row asserts
//
// Two things, because the status code alone is not enough:
//
// 1. An anonymous request is **not** answered `2xx`.
// 2. The registry's published state for the coordinate is **byte-identical**
//    afterwards.
//
// The second is what makes the row a test of the write rather than of the
// response. A handler that validates its payload, mutates, and *then* refuses
// passes (1) and fails (2); so does one that answers `400` for an unrelated
// reason while a side effect has already landed. The fingerprint is
// `get_versions`, serialised — it carries the yank, deprecation, unlist and
// retention flags as well as the version set, so a yank that succeeded shows up
// even though the version count did not change.
//
// # The positive control is not optional here
//
// It matters more than on the read side. A write row sends a body, and a body
// the handler rejects produces a `400` that reads as *denied* — a row that
// asserts nothing while looking green. So the control makes the identical
// request as `USER_TOKEN` and requires a `2xx` *and* a changed fingerprint.
// Refusal is only attributable to authorization once the same payload is known
// to work for someone.

use batlehub_core::ports::{LocalRegistryBackend, OwnershipPort};

/// The HTTP verb a write row uses.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verb {
    Put,
    Post,
    Delete,
}

/// One non-GET route under test.
struct WriteRow {
    kind: &'static str,
    verb: Verb,
    /// Route to request, with `{registry}` already substituted.
    uri: &'static str,
    /// The request body and its `Content-Type`.
    body: Body,
    /// Publish the coordinate before the request. Yank, unyank and the owners
    /// API all need a version to act on; a publish row must not have one, or it
    /// answers `409` for both callers.
    seed: bool,
    /// Yank the seeded version too, so an `unyank` row has something to undo.
    ///
    /// Without it an unyank is a no-op that answers `200` and changes nothing,
    /// and the row's control cannot distinguish that from a route that works.
    /// Whether the no-op is even *visible* differs by ecosystem — cargo seeds
    /// `"yanked": false` in its index line, so unyanking it rewrites the same
    /// value, while rubygems has no such field and gains one. A row whose
    /// meaning depends on that is not a row.
    seed_yanked: bool,
    /// Add this principal as a second owner before the request.
    ///
    /// `cargo owner --remove` refuses to remove the last owner — that would
    /// leave a crate anyone may publish to — so a remove row whose target is not
    /// already an owner answers `200` and changes nothing, and its control
    /// cannot tell that from a working route.
    seed_owner: Option<&'static str>,
    /// Wire an `OwnershipPort` into the service.
    ///
    /// Not a detail: with the port absent the cargo owners routes answer `404`
    /// *before* any authorization, so the row would assert nothing and its
    /// control would fail. `make_local_cargo_ownership_app` records the same
    /// hazard for the same reason.
    ownership: bool,
    /// The coordinate to seed, and the one the row's URI names.
    name: &'static str,
    version: &'static str,
    meta: fn() -> serde_json::Value,
    expect: Expect,
    control: bool,
}

impl WriteRow {
    /// A row with the defaults every write shares: the gate refuses, no seeded
    /// coordinate, no ownership port, `pkg` / `9.8.7`, control on.
    fn new(kind: &'static str, verb: Verb, uri: &'static str, body: Body) -> Self {
        WriteRow {
            kind,
            verb,
            uri,
            body,
            seed: false,
            seed_yanked: false,
            seed_owner: None,
            ownership: false,
            name: "pkg",
            version: "9.8.7",
            meta: no_meta,
            expect: Expect::Denied,
            control: true,
        }
    }

    /// Publish the coordinate first. Every yank, unyank and owners row needs it.
    fn seeded(mut self) -> Self {
        self.seed = true;
        self
    }

    /// Publish the coordinate and yank it, so an `unyank` row has work to do.
    fn seeded_yanked(mut self) -> Self {
        self.seed = true;
        self.seed_yanked = true;
        self
    }

    /// Wire an `OwnershipPort`, without which the route 404s before authorizing.
    fn with_ownership(mut self) -> Self {
        self.ownership = true;
        self
    }

    /// Seed a second owner, so a `--remove` row has one it may actually remove.
    fn with_second_owner(mut self, principal: &'static str) -> Self {
        self.seed_owner = Some(principal);
        self.ownership = true;
        self
    }

    /// Seed and address a different coordinate.
    fn coord(mut self, name: &'static str, version: &'static str) -> Self {
        self.name = name;
        self.version = version;
        self
    }

    /// Seed under a different name. The version stays `9.8.7`.
    fn pkg(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Extra index metadata this ecosystem's read path needs.
    fn meta(mut self, meta: fn() -> serde_json::Value) -> Self {
        self.meta = meta;
        self
    }

    /// Skip the positive control. Every use says why.
    ///
    /// No row uses it today, and that is the state to keep: on the write side a
    /// row without a control asserts almost nothing, because a rejected payload
    /// and a rejected caller both answer non-`2xx`. It is kept for the same
    /// reason as [`Expect::KnownGap`] — the first write route that legitimately
    /// cannot be driven from this fixture wants to be recordable on the day it
    /// arrives, not after a debate about how to keep the suite green.
    #[allow(dead_code)]
    fn no_control(mut self) -> Self {
        self.control = false;
        self
    }
}

// ── Request bodies ───────────────────────────────────────────────────────────
//
// Each is the wire format the ecosystem's own client sends, harvested from the
// suite that already publishes through that route — `local_npm_registry.rs`,
// `local_nuget_registry.rs`, `publish_traversal_guards.rs` and friends. A
// hand-rolled approximation would be rejected on its merits and the row would
// assert nothing, which is exactly what the positive control is there to catch.

fn empty_body() -> (Vec<u8>, &'static str) {
    (Vec::new(), "application/octet-stream")
}

/// The `npm publish` wire format: a packument with the tarball attached.
fn npm_publish_body() -> (Vec<u8>, &'static str) {
    use base64::Engine as _;
    let tarball = base64::engine::general_purpose::STANDARD.encode(FIXTURE_BYTES);
    let doc = json!({
        "name": "pkg",
        "versions": { "9.8.7": {
            "name": "pkg", "version": "9.8.7",
            "description": "matrix fixture",
            "dist": { "shasum": "abc123" }
        }},
        "_attachments": { "pkg-9.8.7.tgz": {
            "content_type": "application/octet-stream",
            "data": tarball,
            "length": FIXTURE_BYTES.len(),
        }}
    });
    (
        serde_json::to_vec(&doc).expect("packument"),
        "application/json",
    )
}

/// `cargo publish`'s length-prefixed metadata + crate pair.
fn cargo_publish_body() -> (Vec<u8>, &'static str) {
    let meta = json!({
        "name": "pkg", "vers": "9.8.7",
        "deps": [], "features": {}, "authors": [],
        "description": null, "documentation": null, "homepage": null,
        "readme": null, "readme_file": null, "keywords": [],
        "categories": [], "license": null, "license_file": null,
        "repository": null, "badges": {}, "links": null
    });
    let meta_bytes = serde_json::to_vec(&meta).expect("crate metadata");
    let mut buf = Vec::new();
    buf.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&meta_bytes);
    buf.extend_from_slice(&(FIXTURE_BYTES.len() as u32).to_le_bytes());
    buf.extend_from_slice(FIXTURE_BYTES);
    (buf, "application/octet-stream")
}

/// `cargo owner --add`.
fn cargo_owners_body() -> (Vec<u8>, &'static str) {
    let doc = json!({ "users": ["user-2"] });
    (
        serde_json::to_vec(&doc).expect("owners"),
        "application/json",
    )
}

/// A minimal `.nupkg` (a ZIP holding one `.nuspec`), in the `multipart/form-data`
/// envelope `dotnet nuget push` wraps it in.
fn nuget_publish_body() -> (Vec<u8>, &'static str) {
    use std::io::Write as _;
    let nuspec = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>pkg</id>
    <version>9.8.7</version>
    <description>matrix fixture</description>
    <authors>TestAuthor</authors>
  </metadata>
</package>"#;
    let mut nupkg = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut nupkg));
        zip.start_file("pkg.nuspec", zip::write::SimpleFileOptions::default())
            .expect("nuspec entry");
        zip.write_all(nuspec.as_bytes()).expect("nuspec bytes");
        zip.finish().expect("nupkg");
    }

    let boundary = "matrixboundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"package\"; \
         filename=\"package.nupkg\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&nupkg);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, "multipart/form-data; boundary=matrixboundary")
}

/// A minimal `.gem`: a tar holding a gzipped `metadata.gz`.
fn gem_publish_body() -> (Vec<u8>, &'static str) {
    (make_gem("pkg", "9.8.7"), "application/octet-stream")
}

/// The `twine upload` `multipart/form-data` envelope.
fn pypi_publish_body() -> (Vec<u8>, &'static str) {
    let boundary = "matrixboundary";
    let mut body = Vec::new();
    for (field, value) in [
        (":action", "file_upload"),
        ("name", "pkg"),
        ("version", "9.8.7"),
    ] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; \
                 name=\"{field}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"content\"; \
             filename=\"pkg-9.8.7.tar.gz\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(FIXTURE_BYTES);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, "multipart/form-data; boundary=matrixboundary")
}

/// A ZIP holding a `composer.json`.
fn composer_publish_body() -> (Vec<u8>, &'static str) {
    (
        make_composer_zip("vendor/pkg", "9.8.7"),
        "application/octet-stream",
    )
}

/// A Go module ZIP, whose entries must be prefixed `module@version/`.
fn go_publish_body() -> (Vec<u8>, &'static str) {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        writer
            .start_file("example.com/m@v9.8.7/go.mod", opts)
            .expect("go.mod entry");
        writer
            .write_all(b"module example.com/m\n\ngo 1.21\n")
            .expect("go.mod bytes");
        writer.finish().expect("module zip");
    }
    (buf.into_inner(), "application/octet-stream")
}

/// A conda `.tar.bz2`: a bzip2ed tar holding `info/index.json`.
fn conda_publish_body() -> (Vec<u8>, &'static str) {
    use std::io::Write as _;
    let index = json!({
        "name": "pkg", "version": "9.8.7", "build": "0",
        "build_number": 0, "depends": [], "subdir": "linux-64",
    });
    let index_bytes = serde_json::to_vec(&index).expect("index.json");

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(index_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "info/index.json", index_bytes.as_slice())
            .expect("tar entry");
        builder.finish().expect("tar");
    }

    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("bzip2");
    (encoder.finish().expect("bzip2"), "application/octet-stream")
}

/// A minimal VSIX: a ZIP with the `[Content_Types].xml` the handler looks for.
fn vsix_publish_body() -> (Vec<u8>, &'static str) {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        zw.start_file(
            "[Content_Types].xml",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("content types entry");
        zw.write_all(
            b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/\
              package/2006/content-types\"></Types>",
        )
        .expect("content types bytes");
        zw.finish().expect("vsix");
    }
    (buf.into_inner(), "application/octet-stream")
}

/// The write matrix.
fn write_matrix() -> Vec<WriteRow> {
    vec![
        // ── npm ──────────────────────────────────────────────────────────────
        WriteRow::new("npm", Verb::Put, "/proxy/reg/pkg", npm_publish_body),
        // ── cargo ────────────────────────────────────────────────────────────
        WriteRow::new(
            "cargo",
            Verb::Put,
            "/proxy/reg/api/v1/crates/new",
            cargo_publish_body,
        ),
        WriteRow::new(
            "cargo",
            Verb::Delete,
            "/proxy/reg/api/v1/crates/pkg/9.8.7/yank",
            empty_body,
        )
        .seeded()
        .meta(cargo_index_meta),
        WriteRow::new(
            "cargo",
            Verb::Put,
            "/proxy/reg/api/v1/crates/pkg/9.8.7/unyank",
            empty_body,
        )
        .seeded_yanked()
        .meta(cargo_index_meta),
        WriteRow::new(
            "cargo",
            Verb::Put,
            "/proxy/reg/api/v1/crates/pkg/owners",
            cargo_owners_body,
        )
        .seeded()
        .with_ownership()
        .meta(cargo_index_meta),
        WriteRow::new(
            "cargo",
            Verb::Delete,
            "/proxy/reg/api/v1/crates/pkg/owners",
            cargo_owners_body,
        )
        .seeded()
        .with_second_owner("user-2")
        .meta(cargo_index_meta),
        // ── nuget ────────────────────────────────────────────────────────────
        WriteRow::new(
            "nuget",
            Verb::Put,
            "/proxy/reg/nuget/api/v2/package",
            nuget_publish_body,
        ),
        WriteRow::new(
            "nuget",
            Verb::Delete,
            "/proxy/reg/nuget/v2/package/pkg/9.8.7",
            empty_body,
        )
        .seeded(),
        // ── rubygems ─────────────────────────────────────────────────────────
        WriteRow::new(
            "rubygems",
            Verb::Post,
            "/proxy/reg/api/v1/gems",
            gem_publish_body,
        ),
        WriteRow::new(
            "rubygems",
            Verb::Delete,
            "/proxy/reg/api/v1/gems/yank?gem_name=pkg&version=9.8.7",
            empty_body,
        )
        .seeded(),
        WriteRow::new(
            "rubygems",
            Verb::Put,
            "/proxy/reg/api/v1/gems/unyank?gem_name=pkg&version=9.8.7",
            empty_body,
        )
        .seeded_yanked(),
        // ── pypi ─────────────────────────────────────────────────────────────
        WriteRow::new("pypi", Verb::Post, "/proxy/reg/legacy/", pypi_publish_body),
        // ── composer ─────────────────────────────────────────────────────────
        WriteRow::new(
            "composer",
            Verb::Post,
            "/proxy/reg/api/upload",
            composer_publish_body,
        )
        .pkg("vendor/pkg"),
        WriteRow::new(
            "composer",
            Verb::Delete,
            "/proxy/reg/api/packages/vendor/pkg/versions/9.8.7",
            empty_body,
        )
        .seeded()
        .pkg("vendor/pkg"),
        // ── goproxy ──────────────────────────────────────────────────────────
        WriteRow::new(
            "goproxy",
            Verb::Put,
            "/proxy/reg/example.com/m/@v/v9.8.7.zip",
            go_publish_body,
        )
        .coord("example.com/m", "v9.8.7"),
        // ── conda ────────────────────────────────────────────────────────────
        WriteRow::new(
            "conda",
            Verb::Post,
            "/proxy/reg/linux-64/",
            conda_publish_body,
        )
        .meta(conda_meta),
        // ── openvsx ──────────────────────────────────────────────────────────
        WriteRow::new(
            "openvsx",
            Verb::Put,
            "/proxy/reg/acme.ext/9.8.7/vsix",
            vsix_publish_body,
        )
        .pkg("acme.ext"),
    ]
}

/// Build a local-mode registry denying anonymous everything, seeded per the row,
/// and hand back the backend so the caller can fingerprint it.
async fn write_app_for(row: &WriteRow) -> (impl TestService, StateProbe) {
    let mut parts = local_only_app_parts_with_policy(
        "reg",
        row.kind,
        RegistryMode::Local,
        true,
        rbac_policy_deny_anonymous,
    )
    .await;

    let mut ownership: Option<Arc<dyn OwnershipPort>> = None;
    if row.ownership {
        let store =
            batlehub_adapters::in_memory::InMemoryOwnershipStore::new() as Arc<dyn OwnershipPort>;
        ownership = Some(store.clone());
        let cur = parts.local_svc.clone();
        parts.local_svc = Arc::new(batlehub_core::services::LocalRegistryService {
            backend: cur.backend.clone(),
            storage: cur.storage.clone(),
            hot: cur.hot.clone(),
            quota: cur.quota.clone(),
            ownership: Some(store),
            team_namespace: cur.team_namespace.clone(),
            sbom: cur.sbom.clone(),
            explore_cache: cur.explore_cache.clone(),
            package_repo: cur.package_repo.clone(),
            readme: cur.readme.clone(),
        });
    }

    if row.seed {
        publish_fixture(&parts, row.name, row.version, (row.meta)()).await;
    }
    if let Some(principal) = row.seed_owner {
        ownership
            .as_ref()
            .expect("seed_owner implies with_ownership")
            .add_owner(
                "reg",
                row.name,
                batlehub_core::ports::OwnerEntry {
                    principal_type: "user".to_owned(),
                    principal_id: principal.to_owned(),
                    role: "maintainer".to_owned(),
                    granted_by: Some("user-1".to_owned()),
                },
            )
            .await
            .expect("seed owner");
    }
    if row.seed_yanked {
        parts
            .local_svc
            .backend
            .yank("reg", row.name, row.version)
            .await
            .expect("seed yank");
    }

    let probe = StateProbe {
        backend: parts.local_svc.backend.clone(),
        ownership,
    };
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, probe)
}

/// Everything the registry records about a coordinate, for before/after comparison.
///
/// Two sources, because one is not enough. `get_versions` carries `yanked`,
/// `deprecated`, `unlisted` and the retention pin as well as the version set, so
/// a yank that landed shows up even though nothing was added or removed. But it
/// is blind to ownership — `package_owners` is a separate store — and the cargo
/// owners routes write only there. A probe that could not see them would report
/// "changed nothing" for a working route and, worse, for a broken one.
struct StateProbe {
    backend: Arc<dyn LocalRegistryBackend>,
    ownership: Option<Arc<dyn OwnershipPort>>,
}

impl StateProbe {
    async fn fingerprint(&self, name: &str) -> String {
        let versions = self
            .backend
            .get_versions("reg", name)
            .await
            .unwrap_or_default();
        let mut out = serde_json::to_string(&versions).unwrap_or_default();
        if let Some(ownership) = &self.ownership {
            let owners = ownership.list_owners("reg", name).await.unwrap_or_default();
            out.push_str(&format!("{owners:?}"));
        }
        out
    }
}

/// Make the row's request as `token` (`None` is anonymous) and report the status
/// alongside whether the registry's state for the coordinate changed.
async fn attempt_write<S: TestService>(
    app: &S,
    probe: &StateProbe,
    row: &WriteRow,
    token: Option<&str>,
) -> (u16, bool) {
    let before = probe.fingerprint(row.name).await;

    let (bytes, content_type) = (row.body)();
    let mut req = match row.verb {
        Verb::Put => TestRequest::put(),
        Verb::Post => TestRequest::post(),
        Verb::Delete => TestRequest::delete(),
    }
    .uri(row.uri)
    .insert_header(("Content-Type", content_type));
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }

    // Bounded for the same reason the read half is: a row that falls through to
    // the fixture's upstream can otherwise hang the suite instead of failing one
    // row. A request that never answers changed nothing, and its control times
    // out identically and is reported as broken.
    let Ok(resp) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        call_service(app, req.set_payload(bytes).to_request()),
    )
    .await
    else {
        return (0, false);
    };
    let status = resp.status().as_u16();

    let after = probe.fingerprint(row.name).await;
    (status, before != after)
}

/// The write matrix.
///
/// One test rather than one per row, for the same reason as the read half: the
/// value is in the table being exhaustive, and a failure names its row.
#[actix_web::test]
async fn no_write_route_accepts_an_unauthenticated_caller() {
    let mut failures: Vec<String> = Vec::new();
    let mut broken_controls: Vec<String> = Vec::new();
    let mut gaps_now_fixed: Vec<String> = Vec::new();

    for row in write_matrix() {
        // Positive control: a caller today's `has_role_at_least(&Role::User)`
        // admits must be able to make this exact request, with this exact body.
        // Until that holds, a refusal below is not attributable to authorization
        // — it could be a rejected payload, and a `400` reads as denied.
        let mut control_ok = true;
        if row.control {
            let (app, probe) = write_app_for(&row).await;
            let (status, changed) = attempt_write(&app, &probe, &row, Some(USER_TOKEN)).await;
            if !(200..300).contains(&status) || !changed {
                broken_controls.push(format!(
                    "[write] {} {:?} {} — a permitted caller was refused or changed nothing \
                     (status {status}, state changed: {changed}). The fixture or the route is \
                     wrong, and the anonymous assertion below proves nothing until it is fixed.",
                    row.kind, row.verb, row.uri,
                ));
                control_ok = false;
            }
        }

        if !control_ok {
            continue;
        }

        // A fresh app, so the control's own write is not what the anonymous
        // caller is measured against.
        let (app, probe) = write_app_for(&row).await;
        let (status, changed) = attempt_write(&app, &probe, &row, None).await;
        let accepted = (200..300).contains(&status) || changed;

        match row.expect {
            Expect::Denied if accepted => failures.push(format!(
                "[write] {} {:?} {} — accepted a write from an unauthenticated caller \
                 (status {status}, state changed: {changed})",
                row.kind, row.verb, row.uri
            )),
            Expect::KnownGap(_) if !accepted => gaps_now_fixed.push(format!(
                "[write] {} {:?} {} — now correctly refuses ({status}). Flip this row to \
                 Expect::Denied.",
                row.kind, row.verb, row.uri,
            )),
            _ => {}
        }
    }

    let mut report = String::new();
    if !broken_controls.is_empty() {
        report.push_str(&format!(
            "\n{} row(s) whose positive control failed:\n  {}\n",
            broken_controls.len(),
            broken_controls.join("\n  ")
        ));
    }
    if !failures.is_empty() {
        report.push_str(&format!(
            "\n{} NEW write authorization gap(s):\n  {}\n",
            failures.len(),
            failures.join("\n  ")
        ));
    }
    if !gaps_now_fixed.is_empty() {
        report.push_str(&format!(
            "\n{} known gap(s) that appear fixed:\n  {}\n",
            gaps_now_fixed.len(),
            gaps_now_fixed.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");
}

// ── Write completeness ───────────────────────────────────────────────────────
//
// The read inventory's argument, applied to the half it could not see. It
// filters the router on `item.get`, so every `PUT`, `POST` and `DELETE` this
// server registers was invisible to it — not classified as uncovered, simply
// absent. RFC 0015 §11.1: *"the same pattern over `put`/`post`/`delete` yields
// the write surface, which currently has no coverage of any kind."*
//
// # How to extend
//
// 1. Add the `(method, path)` pair — the failure message prints the line.
// 2. Choose its [`WriteCoverage`]:
//    - [`WriteCoverage::Row`] once a row in [`write_matrix`] makes the request;
//    - [`WriteCoverage::ReadRow`] for a POST-shaped *read*, covered by a row in
//      [`matrix`] carrying a `.post(…)` body;
//    - [`WriteCoverage::NoWrite`] if nothing is mutated, with the reason;
//    - [`WriteCoverage::NoRow`] otherwise — a write nobody has covered yet.
// 3. If you chose `NoRow`, adding the row is the actual work.

/// Whether the matrix exercises a non-GET route, and if not, why not.
#[derive(Debug, Clone, Copy)]
enum WriteCoverage {
    /// A row in [`write_matrix`] makes the request and asserts both halves —
    /// refused, and nothing written.
    Row,
    /// A POST-shaped *read*, exercised by a `.post(…)` row in [`matrix`] under
    /// the disclosure assertion, which is the one that fits it.
    ReadRow,
    /// The route mutates nothing: a vulnerability feed that takes a manifest, or
    /// an endpoint that declines unconditionally. A claim about the route, so it
    /// is recorded per route rather than inferred from the path.
    NoWrite(&'static str),
    /// A write with no row yet — the write-side twin of [`Coverage::NoRow`].
    /// Finite, visible, and meant to shrink.
    NoRow(&'static str),
}

const WRITE_ROUTE_INVENTORY: &[(&str, &str, WriteCoverage)] = &[
    // ── npm ──────────────────────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/{name}", WriteCoverage::Row),
    ("PUT", "/proxy/{registry}/-/package/{package}/dist-tags/{tag}", WriteCoverage::NoWrite("declined unconditionally with 501: dist-tags are derived from the published version set here, so nothing is mutated for any caller. RFC 0015 §4.2 files the action itself under a future `npm:dist-tags:write`")),
    ("DELETE", "/proxy/{registry}/-/package/{package}/dist-tags/{tag}", WriteCoverage::NoWrite("declined unconditionally with 501, as the `add` half above")),
    ("POST", "/proxy/{registry}/-/npm/v1/audit/quick", WriteCoverage::NoWrite("npm audit: takes a dependency manifest and answers with advisories. CVE data, not package contents, and nothing is written")),
    ("POST", "/proxy/{registry}/-/npm/v1/audit/bulk", WriteCoverage::NoWrite("npm audit, bulk form; same shape")),
    ("POST", "/proxy/{registry}/-/npm/v1/security/audits/quick", WriteCoverage::NoWrite("npm audit under its current path; same shape")),
    ("POST", "/proxy/{registry}/-/npm/v1/security/advisories/bulk", WriteCoverage::NoWrite("npm advisories, bulk form; same shape")),
    // ── cargo ────────────────────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/api/v1/crates/new", WriteCoverage::Row),
    ("DELETE", "/proxy/{registry}/api/v1/crates/{name}/{version}/yank", WriteCoverage::Row),
    ("PUT", "/proxy/{registry}/api/v1/crates/{name}/{version}/unyank", WriteCoverage::Row),
    ("PUT", "/proxy/{registry}/api/v1/crates/{name}/owners", WriteCoverage::Row),
    ("DELETE", "/proxy/{registry}/api/v1/crates/{name}/owners", WriteCoverage::Row),
    // ── nuget ────────────────────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/nuget/api/v2/package", WriteCoverage::Row),
    ("PUT", "/proxy/{registry}/nuget/api/v2/symbolpackage", WriteCoverage::NoRow("write, not yet exercised: needs a symbol package whose .nuspec the symbol path accepts")),
    ("DELETE", "/proxy/{registry}/nuget/v2/package/{id}/{version}", WriteCoverage::Row),
    // ── rubygems ─────────────────────────────────────────────────────────────
    ("POST", "/proxy/{registry}/api/v1/gems", WriteCoverage::Row),
    ("DELETE", "/proxy/{registry}/api/v1/gems/yank", WriteCoverage::Row),
    ("PUT", "/proxy/{registry}/api/v1/gems/unyank", WriteCoverage::Row),
    // ── pypi ─────────────────────────────────────────────────────────────────
    ("POST", "/proxy/{registry}/legacy/", WriteCoverage::Row),
    // ── composer ─────────────────────────────────────────────────────────────
    ("POST", "/proxy/{registry}/api/upload", WriteCoverage::Row),
    ("DELETE", "/proxy/{registry}/api/packages/{vendor}/{package}/versions/{version}", WriteCoverage::Row),
    // ── goproxy ──────────────────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/{module}/@v/{filename}", WriteCoverage::Row),
    ("POST", "/proxy/{registry}/v1/query", WriteCoverage::NoWrite("Go vulnerability database query: takes a module list, answers with OSV records. Nothing is written")),
    // ── conda ────────────────────────────────────────────────────────────────
    ("POST", "/proxy/{registry}/{platform}/", WriteCoverage::Row),
    // ── openvsx / vscode ─────────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/{extension_id}/{version}/vsix", WriteCoverage::Row),
    ("POST", "/proxy/{registry}/api/-/publish", WriteCoverage::NoRow("write, not yet exercised: the OpenVSX REST publish, which takes its coordinate from the VSIX manifest rather than the URL")),
    ("POST", "/proxy/{registry}/api/-/namespace/create", WriteCoverage::NoRow("write, covered outside this matrix: `local_vsx_registry.rs`'s `openvsx_namespace_claim_*` rows assert the refusal, the empty store afterwards, and a working positive control. It cannot be a `Row` here because a write row's fingerprint is `get_versions` for a coordinate and a namespace claim publishes nothing — both the denial and its control would fingerprint identically, so the control could not pass")),
    ("POST", "/proxy/{registry}/vscode/gallery/extensionquery", WriteCoverage::ReadRow),
    // ── maven ────────────────────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/maven2/{path}", WriteCoverage::NoRow("write, not yet exercised: a Maven deploy is several files under one coordinate, so the row needs the multi-file storage key rather than the flat fixture")),
    // ── terraform ────────────────────────────────────────────────────────────
    ("POST", "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}", WriteCoverage::NoRow("write, not yet exercised")),
    ("DELETE", "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}", WriteCoverage::NoRow("write, not yet exercised")),
    ("POST", "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}/unyank", WriteCoverage::NoRow("write, not yet exercised")),
    ("POST", "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions", WriteCoverage::NoRow("write, not yet exercised")),
    ("PUT", "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}", WriteCoverage::NoRow("write, not yet exercised: a provider binary is uploaded per platform against a version created by the route above")),
    ("DELETE", "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}", WriteCoverage::NoRow("write, not yet exercised")),
    ("POST", "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}/unyank", WriteCoverage::NoRow("write, not yet exercised")),
    // ── jetbrains marketplace ────────────────────────────────────────────────
    ("POST", "/proxy/{registry}/api/updates/upload", WriteCoverage::NoRow("write, not yet exercised: a plugin upload is a multipart form carrying a JAR whose plugin.xml supplies the coordinate")),
    ("POST", "/proxy/{registry}/api/search/updates/compatible", WriteCoverage::ReadRow),
    // ── deb / rpm / pacman ───────────────────────────────────────────────────
    ("PUT", "/proxy/{registry}/deb/pool/{distribution}/{component}/upload", WriteCoverage::NoRow("write, not yet exercised: needs a real .deb, whose control archive supplies the coordinate")),
    ("PUT", "/proxy/{registry}/rpm/upload", WriteCoverage::NoRow("write, not yet exercised: needs a real .rpm header")),
    ("PUT", "/proxy/{registry}/pacman/upload", WriteCoverage::NoRow("write, not yet exercised: needs a real .pkg.tar.zst with a .PKGINFO")),
];

/// Every non-GET `/proxy/**` route this server registers is classified, exactly.
///
/// The read gate's two failure directions, on the surface it could not see. A
/// write route the server registers and this list does not name is a mutation
/// nobody has considered; an entry naming a route that no longer exists is a
/// claim of coverage over nothing.
#[test]
fn the_write_route_inventory_matches_the_router() {
    let spec = batlehub_web::openapi_spec();
    let mut registered: Vec<(&'static str, String)> = Vec::new();
    for (path, item) in spec.paths.paths.iter() {
        if !path.starts_with("/proxy/") {
            continue;
        }
        for (verb, present) in [
            ("PUT", item.put.is_some()),
            ("POST", item.post.is_some()),
            ("DELETE", item.delete.is_some()),
            ("PATCH", item.patch.is_some()),
        ] {
            if present {
                registered.push((verb, path.clone()));
            }
        }
    }

    let unlisted: Vec<&(&str, String)> = registered
        .iter()
        .filter(|(m, p)| {
            !WRITE_ROUTE_INVENTORY
                .iter()
                .any(|(im, ip, _)| im == m && ip == p)
        })
        .collect();
    let stale: Vec<String> = WRITE_ROUTE_INVENTORY
        .iter()
        .filter(|(im, ip, _)| !registered.iter().any(|(m, p)| m == im && p == ip))
        .map(|(m, p, _)| format!("{m} {p}"))
        .collect();

    let mut report = String::new();
    if !unlisted.is_empty() {
        report.push_str(&format!(
            "\n{} proxy write route(s) are registered but absent from WRITE_ROUTE_INVENTORY.\n\
             A route that mutates a package and nobody has classified is the write-side\n\
             version of the 2026-08-26 survey's finding class. Add:\n\n{}\n\n\
             …then either write a row in `write_matrix()` and mark it WriteCoverage::Row,\n\
             or record why it needs none.\n",
            unlisted.len(),
            unlisted
                .iter()
                .map(|(m, p)| format!(
                    "    (\"{m}\", \"{p}\", WriteCoverage::NoRow(\"write, not yet exercised\")),"
                ))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    if !stale.is_empty() {
        report.push_str(&format!(
            "\n{} WRITE_ROUTE_INVENTORY entr(ies) name a route this server no longer\n\
             registers. Delete them — a stale entry claims coverage of nothing:\n  {}\n",
            stale.len(),
            stale.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");
}

/// Every `WriteCoverage::Row` has a row behind it, and every excuse says something.
#[test]
fn every_write_claim_has_a_row_and_every_excuse_has_a_reason() {
    let claimed = WRITE_ROUTE_INVENTORY
        .iter()
        .filter(|(_, _, c)| matches!(c, WriteCoverage::Row))
        .count();
    let rows = write_matrix().len();
    assert!(
        rows >= claimed,
        "WRITE_ROUTE_INVENTORY claims {claimed} routes are exercised by a row, but \
         `write_matrix()` only has {rows} rows — at least one claim has no test behind it"
    );

    let claimed_reads = WRITE_ROUTE_INVENTORY
        .iter()
        .filter(|(_, _, c)| matches!(c, WriteCoverage::ReadRow))
        .count();
    let post_rows = matrix().iter().filter(|r| r.post.is_some()).count();
    assert!(
        post_rows >= claimed_reads,
        "WRITE_ROUTE_INVENTORY claims {claimed_reads} POST-shaped reads are covered by a \
         `.post(…)` row, but `matrix()` only has {post_rows}"
    );

    let empty: Vec<String> = WRITE_ROUTE_INVENTORY
        .iter()
        .filter_map(|(m, p, c)| match c {
            WriteCoverage::NoWrite(why) | WriteCoverage::NoRow(why) if why.trim().is_empty() => {
                Some(format!("{m} {p}"))
            }
            _ => None,
        })
        .collect();
    assert!(
        empty.is_empty(),
        "these write routes are excused with no reason given:\n  {}",
        empty.join("\n  ")
    );
}

/// How much of the write surface is actually exercised, printed on every run.
///
/// The read half's ratio, for the half that had none. Before this existed the
/// honest number was zero and nothing said so.
#[test]
fn report_write_surface_coverage() {
    let total = WRITE_ROUTE_INVENTORY.len();
    let rows = WRITE_ROUTE_INVENTORY
        .iter()
        .filter(|(_, _, c)| matches!(c, WriteCoverage::Row | WriteCoverage::ReadRow))
        .count();
    let no_write = WRITE_ROUTE_INVENTORY
        .iter()
        .filter(|(_, _, c)| matches!(c, WriteCoverage::NoWrite(_)))
        .count();
    let no_row: Vec<String> = WRITE_ROUTE_INVENTORY
        .iter()
        .filter_map(|(m, p, c)| match c {
            WriteCoverage::NoRow(why) => Some(format!("{m} {p} — {why}")),
            _ => None,
        })
        .collect();
    let denominator = total - no_write;
    println!(
        "authz matrix (writes): {rows}/{denominator} mutating routes exercised \
         ({} with no row, {no_write} mutate nothing, {total} total)",
        no_row.len()
    );
    for line in &no_row {
        println!("  no row: {line}");
    }
}

/// Every `Coverage::Row` claim names a route a row actually reaches, and every
/// route a row reaches is claimed.
///
/// # Why this is measured rather than reviewed
///
/// [`ROUTE_INVENTORY`]'s `Coverage::Row` entries were hand-maintained, and the
/// only thing checking them was `every_row_is_accounted_for_in_the_inventory` —
/// which compares two *counts*. Counts cannot see a swap: one route over-claimed
/// and one under-claimed net to zero, and the report prints the same ratio
/// either way. An over-claim is the worse half, because it is a route this file
/// says is covered and is not, which is precisely the "wrong *covered*" the
/// inventory's own header calls "exactly the outcome this file exists to
/// prevent".
///
/// That header goes on to say the mapping is stated rather than inferred
/// because a hand-written matcher got it wrong in both directions — a path
/// parameter is not always one segment, so a greedy matcher claims coverage
/// that does not exist. True, and the conclusion does not follow: actix already
/// holds the answer. `HttpRequest::match_pattern` reports the pattern the real
/// router selected for a request that was really made, so there is no
/// approximation to get wrong. The inventory stays stated; this asserts the
/// statement against the router instead of against a count.
///
/// One app routes every row: the route table is registered in full regardless of
/// registry type, and which handler *answers* is a question about guards, not
/// about matching.
///
/// # The two spellings
///
/// actix reports the pattern as registered, regexes and all —
/// `/proxy/{registry}/deb/{path:.*}`, `/proxy/{registry}/{module:[^@]+}@v/list`
/// — while the OpenAPI spec [`ROUTE_INVENTORY`] is written against strips the
/// constraint and splices a `/` before the `@`. Same route, two spellings, so
/// both sides are canonicalised before they are compared. Getting that wrong
/// would report thirteen phantom mismatches, which is a gate nobody would keep.
/// Consume one `{param:regex}` body, having already read the opening brace, and
/// return just the parameter name.
///
/// The scan is brace-balanced rather than a search for `}`: a constraint can
/// contain braces of its own (`{filename:.+\.(?:tar\.bz2|conda)}` does not, but
/// `{n:\d{1,3}}` would), and a naive split would truncate mid-pattern and
/// silently invent a route.
fn param_name(chars: &mut std::str::Chars<'_>) -> String {
    let mut name = String::new();
    let mut depth = 1usize;
    let mut in_constraint = false;
    for c in chars.by_ref() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            ':' if depth == 1 && !in_constraint => in_constraint = true,
            _ if !in_constraint => name.push(c),
            _ => {}
        }
    }
    name
}

/// Drop `:regex` from every `{param:regex}`, then join `}/@` back to `}@`.
///
/// Both sides of the coverage comparison go through this: actix reports the
/// pattern as registered, the inventory is written without the constraints, and
/// comparing the two spellings directly would report thirteen phantom
/// mismatches.
fn canonical(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        out.push('{');
        out.push_str(&param_name(&mut chars));
        out.push('}');
    }
    out.replace("}/@", "}@")
}

#[actix_web::test]
async fn coverage_claims_match_the_routes_rows_actually_reach() {
    use std::collections::BTreeSet;

    let parts = local_only_app_parts_with_policy(
        "reg",
        "npm",
        RegistryMode::Local,
        true,
        rbac_policy_deny_anonymous,
    )
    .await;
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut unrouted: Vec<&str> = Vec::new();

    for row in matrix() {
        // GET-shaped rows only. A `.post(…)` row is claimed in
        // WRITE_ROUTE_INVENTORY as a `ReadRow`, because ROUTE_INVENTORY filters
        // the router on `item.get` and cannot name a POST-only path.
        if row.post.is_some() {
            continue;
        }
        let resp = call_service(&app, TestRequest::get().uri(row.uri).to_request()).await;
        match resp.request().match_pattern() {
            Some(pattern) => {
                reached.insert(canonical(&pattern));
            }
            None => unrouted.push(row.uri),
        }
    }

    let claimed: BTreeSet<String> = ROUTE_INVENTORY
        .iter()
        .filter(|(_, c)| matches!(c, Coverage::Row))
        .map(|(p, _)| canonical(p))
        .collect();

    let over: Vec<&String> = claimed.difference(&reached).collect();
    let under: Vec<&String> = reached.difference(&claimed).collect();

    let mut report = String::new();
    if !unrouted.is_empty() {
        report.push_str(&format!(
            "\n{} row URI(s) match no route at all. A row that routes nowhere still \"passes\"\n\
             its negative assertion, because a 404 reads as denied:\n  {}\n",
            unrouted.len(),
            unrouted.join("\n  ")
        ));
    }
    if !over.is_empty() {
        report.push_str(&format!(
            "\n{} route(s) are marked Coverage::Row and no row reaches them. This is a\n\
             claim of coverage over nothing — the failure direction the inventory's header\n\
             calls out. Either add a row, or reclassify:\n  {}\n",
            over.len(),
            over.iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }
    if !under.is_empty() {
        report.push_str(&format!(
            "\n{} route(s) are reached by a row but not marked Coverage::Row. The suite is\n\
             under-reporting its own coverage; mark them:\n  {}\n",
            under.len(),
            under
                .iter()
                .map(|p| format!("    (\"{p}\", Coverage::Row),"))
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");
}
