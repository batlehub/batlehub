//! `batlehub proxy serve` — the local gallery proxy of RFC 0011 §4.4.
//!
//! A loopback HTTP server the user's own CLI runs in front of one BatleHub
//! VSX registry. The editor's `extensionsGallery` points at it; it attaches
//! the credential the contract file (`contract.rs`) holds, so the editor —
//! a process that runs arbitrary extension code — never sees a token.
//!
//! Three things it does, and each is a section of the RFC:
//!
//! - **The path is the secret (§4.4.1).** In a workspace pod loopback is
//!   shared by every container, so the port protects nothing. Everything is
//!   served under a per-session random segment; a request outside it is a
//!   `404`. The segment is regenerated per invocation and never logged.
//! - **Absolute URLs are rewritten (§4.4.3).** The gallery document names
//!   `assetUri`/`fallbackAssetUri` on the BatleHub origin; left alone, the
//!   editor would fetch the `.vsix` there directly, credential-less, and
//!   get a `401` after the user clicked install. Every string that starts
//!   with the registry base is repointed at the capability base.
//! - **Signing in is data, not a status (§4.4.2).** With no credential a
//!   search answers `200` with exactly one synthetic entry,
//!   `batlehub.sign-in`, whose details asset is the sign-in page and whose
//!   package installs. A lookup by name answers `200` with nothing, because
//!   the editor asks about every installed extension at startup and an
//!   error there marks them all unavailable. RFC 0011 §4.4.4 measured each
//!   rule of that table against VS Code 1.96.4; the tests below keep them.
//!
//! What §6.3 wanted that is not here: the login surface on the same server.
//! No device-code or PKCE-loopback flow exists (§13, *the CLI login flow*),
//! so the sign-in page names the two commands that do — `auth login`, then
//! `auth write-token-file` — and the proxy re-reads the contract file on
//! every request, so a login lands without restarting anything.

use std::io::Write as _;
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};

use crate::contract::{self, ContractFile, State};

/// The synthetic entry's coordinates. Fixed: the editor keys its
/// installed-extension state on them.
pub const SIGN_IN_PUBLISHER: &str = "batlehub";
pub const SIGN_IN_NAME: &str = "sign-in";
pub const SIGN_IN_VERSION: &str = "1.0.0";
/// The one date the entry carries, everywhere the editor reads one.
pub const SIGN_IN_DATE: &str = "2026-01-01T00:00:00Z";
/// The property RFC 0011 §4.4.4 found mandatory: without it the editor
/// fetches the manifest to learn `engines.vscode`, a missing one throws,
/// and the entry is dropped from results — the empty view the entry exists
/// to prevent.
pub const CODE_ENGINE_PROPERTY: &str = "Microsoft.VisualStudio.Code.Engine";
pub const CODE_ENGINE: &str = "^1.0.0";

/// The Marketplace asset types the sign-in entry answers.
pub mod asset_type {
    pub const DETAILS: &str = "Microsoft.VisualStudio.Services.Content.Details";
    pub const MANIFEST: &str = "Microsoft.VisualStudio.Code.Manifest";
    pub const VSIX_PACKAGE: &str = "Microsoft.VisualStudio.Services.VSIXPackage";
}

/// The `filterType` of a lookup by extension name in the Marketplace
/// `extensionquery` (RFC 0011 §4.4.4, measured); a search is `10`, and a
/// browse carries no criterion at all.
pub const FILTER_EXTENSION_NAME: u64 = 7;

/// What one `extensionquery` is asking (§4.4.2's classifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryKind {
    /// A lookup by extension name: startup asking about installed
    /// extensions, or an install by id. Answered with the sign-in entry
    /// only when the name asked for **is** the sign-in entry — an install
    /// by id resolves it through this very lookup (measured against VS Code
    /// 1.96.4: `--install-extension batlehub.sign-in` is a `filterType: 7`
    /// query, and an empty answer is *not found*). Any other name is
    /// answered empty, so the editor never marks an installed extension
    /// unavailable.
    ByName(Vec<String>),
    /// A search or a browse: what the Extensions view shows. Answered with
    /// the sign-in entry while unauthenticated.
    Search,
}

impl QueryKind {
    /// Whether an unauthenticated proxy answers this query with the sign-in
    /// entry: a search, or a lookup naming the entry itself.
    pub fn wants_sign_in(&self) -> bool {
        match self {
            Self::Search => true,
            Self::ByName(names) => names
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&format!("{SIGN_IN_PUBLISHER}.{SIGN_IN_NAME}"))),
        }
    }
}

/// Classify a query body. A query with any `filterType: 7` criterion is a
/// lookup by the names those criteria carry; everything else — search
/// text, a browse with no criteria at all — is a search.
pub fn classify(body: &Value) -> QueryKind {
    let names: Vec<String> = body
        .get("filters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("criteria").and_then(Value::as_array))
        .flatten()
        .filter(|c| c.get("filterType").and_then(Value::as_u64) == Some(FILTER_EXTENSION_NAME))
        .filter_map(|c| c.get("value").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    if names.is_empty() {
        QueryKind::Search
    } else {
        QueryKind::ByName(names)
    }
}

/// Rewrite every string that starts with `from` so it starts with `to`
/// instead — `assetUri`, `fallbackAssetUri`, `files[].source` and whatever
/// else the document carries on the registry's origin (§4.4.3). A string on
/// any other origin is left alone: foreign URLs are never redirected
/// through the proxy, which would hand the credential to a third party.
pub fn rewrite_urls(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix(from) {
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') {
                    *s = format!("{to}{rest}");
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| rewrite_urls(v, from, to)),
        Value::Object(map) => map.values_mut().for_each(|v| rewrite_urls(v, from, to)),
        _ => {}
    }
}

/// The empty-but-well-formed answer to a lookup by name while
/// unauthenticated (§4.4.2): the editor reads a count of zero, not an error.
pub fn empty_result() -> Value {
    json!({
        "results": [{
            "extensions": [],
            "pagingToken": Value::Null,
            "resultMetadata": [{
                "metadataType": "ResultCount",
                "metadataItems": [{ "name": "TotalCount", "count": 0 }],
            }],
        }]
    })
}

/// The gallery document holding the one sign-in entry (§4.4.2's table):
/// pinned first and alone, with the `Code.Engine` property, and with
/// manifest and package assets at the proxy's own capability base.
pub fn sign_in_document(capability_base: &str) -> Value {
    let asset_base = format!("{}/signin/asset", capability_base.trim_end_matches('/'));
    let file = |t: &str| json!({ "assetType": t, "source": format!("{asset_base}/{t}") });
    json!({
        "results": [{
            "extensions": [{
                "publisher": {
                    "publisherName": SIGN_IN_PUBLISHER,
                    "displayName": "BatleHub",
                },
                "extensionId": "0b0a7e1e-5b1a-4d7e-9c3e-0011b0a7e1e0",
                "extensionName": SIGN_IN_NAME,
                "displayName": "Sign in to BatleHub",
                "shortDescription": "Sign in to see this registry's extensions.",
                "flags": "validated, public",
                "categories": ["Other"],
                "tags": [],
                // Read off the extension, not the version, by the editor's
                // details page (`publishedDate`, `releaseDate`,
                // `lastUpdated`): without them it prints "Invalid Date"
                // (measured in the Extensions view, VS Code 1.96.4 and 1.136.1).
                "publishedDate": SIGN_IN_DATE,
                "lastUpdated": SIGN_IN_DATE,
                "releaseDate": SIGN_IN_DATE,
                "versions": [{
                    "version": SIGN_IN_VERSION,
                    "lastUpdated": SIGN_IN_DATE,
                    "assetUri": asset_base,
                    "fallbackAssetUri": asset_base,
                    "files": [
                        file(asset_type::DETAILS),
                        file(asset_type::MANIFEST),
                        file(asset_type::VSIX_PACKAGE),
                    ],
                    "properties": [
                        { "key": CODE_ENGINE_PROPERTY, "value": CODE_ENGINE },
                    ],
                }],
                "statistics": [],
            }],
            "pagingToken": Value::Null,
            "resultMetadata": [{
                "metadataType": "ResultCount",
                "metadataItems": [{ "name": "TotalCount", "count": 1 }],
            }],
        }]
    })
}

/// The sign-in page: the details asset the editor renders for the entry
/// (§4.4.2). A static document of this build with one value interpolated —
/// the registry URL, HTML-escaped — because it is rendered in a webview.
pub fn sign_in_readme(registry_base: &str) -> String {
    let registry = html_escape(registry_base);
    format!(
        "# Sign in to BatleHub\n\n\
         This editor's extension gallery is `{registry}`, and it needs a credential \
         this editor does not hold. The local gallery proxy you are talking to holds \
         it for you, once you sign in:\n\n\
         1. In a terminal: `batlehub-cli --server {registry} auth login`\n\
         2. Then: `batlehub-cli --server {registry} auth write-token-file`\n\n\
         The proxy reads the credential file on every request, so the next search \
         in this view (press its Refresh) shows the registry's extensions. Nothing \
         needs restarting.\n\n\
         The Install button next to this entry is greyed out: the editor only \
         installs signed packages from this view, and this entry is a page, not a \
         package worth signing. `batlehub-cli auth status` says what the proxy \
         currently holds.\n"
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The extension manifest the `Manifest` asset serves and the package
/// carries. `engines.vscode` matches [`CODE_ENGINE`]; no `main`, no
/// contributions — installing it changes nothing about the editor. And no
/// `activationEvents`: the editor's manifest validator refuses that key on
/// an extension with no `main` or `browser` (measured, VS Code 1.96.4:
/// *Cannot read the extension from …*), which made the package
/// uninstallable in the suite's first run.
pub fn sign_in_manifest() -> Value {
    json!({
        "name": SIGN_IN_NAME,
        "displayName": "Sign in to BatleHub",
        "description": "Sign in to see this registry's extensions.",
        "version": SIGN_IN_VERSION,
        "publisher": SIGN_IN_PUBLISHER,
        "engines": { "vscode": CODE_ENGINE },
        "categories": ["Other"],
    })
}

/// The installable `.vsix` (§4.4.4: an unsigned package from a custom
/// gallery installs, and an entry without one leaves the Install button a
/// dead end). Built once at startup; a zip with the four files a VSIX needs.
pub fn sign_in_vsix(registry_base: &str) -> anyhow::Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("extension.vsixmanifest", opts)?;
        zip.write_all(vsix_manifest_xml().as_bytes())?;
        zip.start_file("[Content_Types].xml", opts)?;
        zip.write_all(CONTENT_TYPES_XML.as_bytes())?;
        zip.start_file("extension/package.json", opts)?;
        zip.write_all(serde_json::to_string_pretty(&sign_in_manifest())?.as_bytes())?;
        zip.start_file("extension/README.md", opts)?;
        zip.write_all(sign_in_readme(registry_base).as_bytes())?;
        zip.finish()?;
    }
    Ok(buf.into_inner())
}

fn vsix_manifest_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011" xmlns:d="http://schemas.microsoft.com/developer/vsx-schema-design/2011">
  <Metadata>
    <Identity Language="en-US" Id="{SIGN_IN_NAME}" Version="{SIGN_IN_VERSION}" Publisher="{SIGN_IN_PUBLISHER}"/>
    <DisplayName>Sign in to BatleHub</DisplayName>
    <Description xml:space="preserve">Sign in to see this registry's extensions.</Description>
    <Tags></Tags>
    <Categories>Other</Categories>
    <GalleryFlags>Public</GalleryFlags>
    <Properties>
      <Property Id="{CODE_ENGINE_PROPERTY}" Value="{CODE_ENGINE}"/>
      <Property Id="Microsoft.VisualStudio.Code.ExtensionDependencies" Value=""/>
      <Property Id="Microsoft.VisualStudio.Code.ExtensionPack" Value=""/>
      <Property Id="Microsoft.VisualStudio.Code.ExtensionKind" Value="ui,workspace"/>
    </Properties>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code"/>
  </Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/>
    <Asset Type="Microsoft.VisualStudio.Services.Content.Details" Path="extension/README.md" Addressable="true"/>
  </Assets>
</PackageManifest>
"#
    )
}

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="json" ContentType="application/json"/>
  <Default Extension="md" ContentType="text/markdown"/>
  <Default Extension="vsixmanifest" ContentType="text/xml"/>
</Types>
"#;

// ── the server ───────────────────────────────────────────────────────────────

/// What one `proxy serve` holds for its lifetime.
pub struct ProxyState {
    /// The BatleHub registry base, `http://host/proxy/{name}`, no trailing slash.
    pub registry_base: String,
    /// The origin the contract file keys its entries by.
    pub registry_origin: String,
    /// `http://127.0.0.1:{port}/{session}/vsx`, no trailing slash — what the
    /// editor's `extensionsGallery` points at, and what every rewritten
    /// URL starts with.
    pub capability_base: String,
    /// The per-session secret segment (§4.4.1).
    pub session: String,
    pub contract_path: std::path::PathBuf,
    pub http: reqwest::Client,
    pub vsix: Vec<u8>,
}

impl ProxyState {
    /// The credential the contract file holds for this registry right now,
    /// re-read on every call so a login lands without a restart.
    pub fn credential(&self) -> Option<String> {
        let doc = ContractFile::load(&self.contract_path);
        let entry = doc.entry(&self.registry_origin)?;
        let resolved = entry.resolve();
        match resolved.state {
            State::Ok => resolved.token,
            _ => None,
        }
    }
}

/// The routes, all under `/{session}/vsx`. Mounted by `serve` and by the
/// tests; a request whose first segment is not the session is a `404`
/// from the catch-all, so the session check is not a middleware anyone
/// could forget.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/{session}/vsx/vscode/gallery/extensionquery",
        web::post().to(extension_query),
    )
    .route(
        "/{session}/vsx/signin/asset/{asset_type}",
        web::get().to(sign_in_asset),
    )
    .route("/{session}/vsx/{tail:.*}", web::to(forward))
    .default_service(web::to(not_found));
}

async fn not_found() -> HttpResponse {
    HttpResponse::NotFound().finish()
}

fn session_ok(state: &ProxyState, req: &HttpRequest) -> bool {
    req.match_info().get("session") == Some(state.session.as_str())
}

async fn extension_query(
    req: HttpRequest,
    state: web::Data<Arc<ProxyState>>,
    body: web::Bytes,
) -> HttpResponse {
    if !session_ok(&state, &req) {
        return not_found().await;
    }
    let query: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(token) = state.credential() else {
        let doc = if classify(&query).wants_sign_in() {
            sign_in_document(&state.capability_base)
        } else {
            empty_result()
        };
        return HttpResponse::Ok().json(doc);
    };
    let url = format!("{}/vscode/gallery/extensionquery", state.registry_base);
    let upstream = state
        .http
        .post(&url)
        .bearer_auth(&token)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await;
    match upstream {
        Ok(resp) => {
            let status = actix_status(resp.status());
            match resp.json::<Value>().await {
                Ok(mut doc) => {
                    rewrite_urls(&mut doc, &state.registry_base, &state.capability_base);
                    HttpResponse::build(status).json(doc)
                }
                Err(e) => upstream_failed(&format!("unreadable gallery answer: {e}")),
            }
        }
        Err(e) => upstream_failed(&format!("gallery unreachable: {e}")),
    }
}

async fn sign_in_asset(req: HttpRequest, state: web::Data<Arc<ProxyState>>) -> HttpResponse {
    if !session_ok(&state, &req) {
        return not_found().await;
    }
    let asset = req.match_info().get("asset_type").unwrap_or_default();
    match asset {
        asset_type::DETAILS => HttpResponse::Ok()
            .content_type("text/markdown; charset=utf-8")
            .body(sign_in_readme(&state.registry_base)),
        asset_type::MANIFEST => HttpResponse::Ok().json(sign_in_manifest()),
        asset_type::VSIX_PACKAGE => HttpResponse::Ok()
            .content_type("application/octet-stream")
            .body(state.vsix.clone()),
        _ => not_found().await,
    }
}

/// Every other gallery request — asset fetches, `vspackage`, `item` — goes
/// upstream with the credential and comes back as it was, body streamed.
async fn forward(
    req: HttpRequest,
    state: web::Data<Arc<ProxyState>>,
    body: web::Bytes,
) -> HttpResponse {
    if !session_ok(&state, &req) {
        return not_found().await;
    }
    let tail = req.match_info().get("tail").unwrap_or_default();
    let mut url = format!("{}/{tail}", state.registry_base);
    if let Some(q) = req.uri().query() {
        url.push('?');
        url.push_str(q);
    }
    let method = match reqwest::Method::from_bytes(req.method().as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return HttpResponse::MethodNotAllowed().finish(),
    };
    let mut upstream = state.http.request(method, &url);
    if let Some(token) = state.credential() {
        upstream = upstream.bearer_auth(token);
    }
    for name in ["Accept", "Range", "If-None-Match", "Content-Type"] {
        if let Some(v) = req.headers().get(name) {
            upstream = upstream.header(name, v.as_bytes());
        }
    }
    if !body.is_empty() {
        upstream = upstream.body(body.to_vec());
    }
    match upstream.send().await {
        Ok(resp) => {
            let status = actix_status(resp.status());
            let mut out = HttpResponse::build(status);
            for name in ["Content-Type", "Content-Length", "ETag", "Content-Range"] {
                if let Some(v) = resp.headers().get(name) {
                    out.insert_header((name, v.as_bytes()));
                }
            }
            out.streaming(resp.bytes_stream())
        }
        Err(e) => upstream_failed(&format!("gallery unreachable: {e}")),
    }
}

/// reqwest and actix carry two `http` crate generations; the status crosses
/// by number.
fn actix_status(s: reqwest::StatusCode) -> actix_web::http::StatusCode {
    actix_web::http::StatusCode::from_u16(s.as_u16())
        .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY)
}

/// An upstream that did not answer is a `502` with the reason — the one
/// branch where a status, not data, is the honest answer: there is no
/// document to put the news in.
fn upstream_failed(reason: &str) -> HttpResponse {
    HttpResponse::BadGateway().json(json!({ "error": reason }))
}

/// A fresh 128-bit session segment, hex, from the process's own randomness.
pub fn new_session() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Build the state for one invocation. `registry_base` is what the operator
/// passes; the origin the contract keys by is derived from it the way
/// `auth write-token-file` derives it (`contract::normalize_origin`).
pub fn state_for(
    registry_base: &str,
    capability_base: &str,
    session: String,
    contract_path: std::path::PathBuf,
) -> anyhow::Result<ProxyState> {
    let registry_base = registry_base.trim_end_matches('/').to_owned();
    let vsix = sign_in_vsix(&registry_base)?;
    Ok(ProxyState {
        registry_origin: contract::normalize_origin(&registry_base),
        registry_base,
        capability_base: capability_base.trim_end_matches('/').to_owned(),
        session,
        contract_path,
        http: reqwest::Client::builder()
            .user_agent("batlehub-cli/0.1 (gallery proxy)")
            .build()?,
        vsix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test as atest, App};
    use std::io::Read as _;

    fn state(contract: &std::path::Path, registry_base: &str) -> Arc<ProxyState> {
        Arc::new(
            state_for(
                registry_base,
                "http://127.0.0.1:1/abc123/vsx",
                "abc123".into(),
                contract.to_path_buf(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn a_lookup_by_name_and_a_search_are_told_apart_by_filter_type_7() {
        let by_name = json!({ "filters": [{ "criteria": [
            { "filterType": 8, "value": "Microsoft.VisualStudio.Code" },
            { "filterType": 7, "value": "ms-python.python" },
        ]}], "flags": 950 });
        let search = json!({ "filters": [{ "criteria": [
            { "filterType": 8, "value": "Microsoft.VisualStudio.Code" },
            { "filterType": 10, "value": "python" },
        ]}] });
        let browse = json!({ "filters": [{ "criteria": [] }] });
        assert_eq!(
            classify(&by_name),
            QueryKind::ByName(vec!["ms-python.python".into()])
        );
        assert_eq!(classify(&search), QueryKind::Search);
        assert_eq!(classify(&browse), QueryKind::Search);
        assert_eq!(classify(&Value::Null), QueryKind::Search);
        // The one lookup by name that gets the entry is the entry's own:
        // an install by id resolves through it.
        assert!(!classify(&by_name).wants_sign_in());
        assert!(classify(&search).wants_sign_in());
        let own = json!({ "filters": [{ "criteria": [
            { "filterType": 7, "value": "BatleHub.Sign-In" },
        ]}] });
        assert!(classify(&own).wants_sign_in());
    }

    #[test]
    fn urls_on_the_registry_are_rewritten_and_foreign_ones_left_alone() {
        let mut doc = json!({
            "results": [{ "extensions": [{ "versions": [{
                "assetUri": "http://bh.local/proxy/vsx/vscode/asset/a/b/1.0.0",
                "fallbackAssetUri": "http://bh.local/proxy/vsx/vscode/asset/a/b/1.0.0",
                "files": [{ "source": "http://bh.local/proxy/vsx/vscode/asset/a/b/1.0.0/X?x=1" }],
                "homepage": "https://github.com/a/b",
                "trap": "http://bh.local/proxy/vsx2/vscode/asset",
            }]}]}]
        });
        rewrite_urls(
            &mut doc,
            "http://bh.local/proxy/vsx",
            "http://127.0.0.1:1/s/vsx",
        );
        let v = &doc["results"][0]["extensions"][0]["versions"][0];
        assert_eq!(
            v["assetUri"],
            "http://127.0.0.1:1/s/vsx/vscode/asset/a/b/1.0.0"
        );
        assert_eq!(
            v["fallbackAssetUri"],
            "http://127.0.0.1:1/s/vsx/vscode/asset/a/b/1.0.0"
        );
        assert_eq!(
            v["files"][0]["source"],
            "http://127.0.0.1:1/s/vsx/vscode/asset/a/b/1.0.0/X?x=1"
        );
        assert_eq!(
            v["homepage"], "https://github.com/a/b",
            "a foreign origin is left alone"
        );
        assert_eq!(
            v["trap"], "http://bh.local/proxy/vsx2/vscode/asset",
            "a registry whose name merely starts with ours is not ours"
        );
    }

    /// RFC 0011 §4.4.2's table, each row a regression test for a §4.4.4
    /// finding: the entry carries `Code.Engine`, a manifest and a package,
    /// and its assets live on the capability base.
    #[test]
    fn the_sign_in_entry_carries_the_engine_property_and_installable_assets() {
        let doc = sign_in_document("http://127.0.0.1:1/s/vsx/");
        let ext = &doc["results"][0]["extensions"][0];
        assert_eq!(ext["publisher"]["publisherName"], SIGN_IN_PUBLISHER);
        assert_eq!(ext["extensionName"], SIGN_IN_NAME);
        let v = &ext["versions"][0];
        let props = v["properties"].as_array().unwrap();
        assert!(props
            .iter()
            .any(|p| p["key"] == CODE_ENGINE_PROPERTY && p["value"] == CODE_ENGINE));
        let types: Vec<&str> = v["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["assetType"].as_str().unwrap())
            .collect();
        assert!(types.contains(&asset_type::MANIFEST));
        assert!(types.contains(&asset_type::VSIX_PACKAGE));
        assert!(types.contains(&asset_type::DETAILS));
        assert_eq!(v["assetUri"], "http://127.0.0.1:1/s/vsx/signin/asset");
        assert_eq!(v["fallbackAssetUri"], v["assetUri"]);
        assert_eq!(
            doc["results"][0]["resultMetadata"][0]["metadataItems"][0]["count"],
            1
        );
    }

    #[test]
    fn the_package_is_a_vsix_with_a_manifest_whose_engine_matches() {
        let bytes = sign_in_vsix("http://bh.local/proxy/vsx").unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_owned())
            .collect();
        for needed in [
            "extension.vsixmanifest",
            "[Content_Types].xml",
            "extension/package.json",
            "extension/README.md",
        ] {
            assert!(names.contains(&needed.to_owned()), "{names:?}");
        }
        let mut manifest = String::new();
        zip.by_name("extension/package.json")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        let manifest: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["engines"]["vscode"], CODE_ENGINE);
        assert_eq!(manifest["publisher"], SIGN_IN_PUBLISHER);
        assert_eq!(manifest["name"], SIGN_IN_NAME);
        let mut readme = String::new();
        zip.by_name("extension/README.md")
            .unwrap()
            .read_to_string(&mut readme)
            .unwrap();
        assert!(readme.contains("auth write-token-file"));
        assert!(readme.contains("http://bh.local/proxy/vsx"));
    }

    #[test]
    fn the_readme_escapes_the_one_value_it_interpolates() {
        let page = sign_in_readme("http://x/<script>");
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;script&gt;"));
    }

    #[actix_web::test]
    async fn outside_the_session_segment_everything_is_404() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(
            &dir.path().join("none.json"),
            "http://127.0.0.1:1/proxy/vsx",
        );
        let app = atest::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&st)))
                .configure(configure),
        )
        .await;
        for uri in [
            "/wrong/vsx/vscode/gallery/extensionquery",
            "/wrong/vsx/signin/asset/Microsoft.VisualStudio.Code.Manifest",
            "/vsx/vscode/gallery/extensionquery",
            "/",
        ] {
            let req = atest::TestRequest::post()
                .uri(uri)
                .set_json(json!({}))
                .to_request();
            let resp = atest::call_service(&app, req).await;
            assert_eq!(resp.status(), 404, "{uri}");
        }
    }

    #[actix_web::test]
    async fn unauthenticated_a_search_is_the_sign_in_entry_and_a_lookup_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let st = state(
            &dir.path().join("none.json"),
            "http://127.0.0.1:1/proxy/vsx",
        );
        let app = atest::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&st)))
                .configure(configure),
        )
        .await;
        let ask = |body: Value| {
            atest::TestRequest::post()
                .uri("/abc123/vsx/vscode/gallery/extensionquery")
                .set_json(body)
                .to_request()
        };
        let resp = atest::call_service(
            &app,
            ask(json!({ "filters": [{ "criteria": [{ "filterType": 10, "value": "x" }] }] })),
        )
        .await;
        assert_eq!(resp.status(), 200);
        let doc: Value = atest::read_body_json(resp).await;
        assert_eq!(
            doc["results"][0]["extensions"][0]["extensionName"],
            SIGN_IN_NAME
        );

        let resp = atest::call_service(
            &app,
            ask(json!({ "filters": [{ "criteria": [{ "filterType": 7, "value": "a.b" }] }] })),
        )
        .await;
        assert_eq!(resp.status(), 200, "never an error on a name lookup");
        let doc: Value = atest::read_body_json(resp).await;
        assert_eq!(doc["results"][0]["extensions"].as_array().unwrap().len(), 0);
        // …except the lookup an install by id of the entry itself makes.
        let resp = atest::call_service(
            &app,
            ask(json!({ "filters": [{ "criteria": [{ "filterType": 7, "value": "batlehub.sign-in" }] }] })),
        )
        .await;
        let doc: Value = atest::read_body_json(resp).await;
        assert_eq!(
            doc["results"][0]["extensions"][0]["extensionName"],
            SIGN_IN_NAME
        );

        // The three assets the entry names are served, and the package is
        // a zip.
        for (asset, prefix) in [
            (asset_type::DETAILS, b"# Sign".as_slice()),
            (asset_type::MANIFEST, b"{".as_slice()),
            (asset_type::VSIX_PACKAGE, b"PK".as_slice()),
        ] {
            let req = atest::TestRequest::get()
                .uri(&format!("/abc123/vsx/signin/asset/{asset}"))
                .to_request();
            let resp = atest::call_service(&app, req).await;
            assert_eq!(resp.status(), 200, "{asset}");
            let body = atest::read_body(resp).await;
            assert!(body.starts_with(prefix), "{asset}");
        }
    }

    #[actix_web::test]
    async fn with_a_credential_the_query_goes_upstream_with_it_and_comes_back_rewritten() {
        let mut server = mockito::Server::new_async().await;
        let registry_base = format!("{}/proxy/vsx", server.url());
        let upstream = server
            .mock("POST", "/proxy/vsx/vscode/gallery/extensionquery")
            .match_header("authorization", "Bearer bh_pat_secret")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({ "results": [{ "extensions": [{ "extensionName": "real", "versions": [{
                    "assetUri": format!("{registry_base}/vscode/asset/p/real/1.0.0"),
                    "fallbackAssetUri": format!("{registry_base}/vscode/asset/p/real/1.0.0"),
                }]}]}]})
                .to_string(),
            )
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let contract = dir.path().join("vsx-token.json");
        let mut doc = ContractFile::load(&contract);
        doc.set_entry(
            &contract::normalize_origin(&registry_base),
            contract::Entry::literal("bh_pat_secret", contract::Kind::Pat, None),
        );
        doc.save(&contract).unwrap();

        let st = state(&contract, &registry_base);
        assert_eq!(st.credential().as_deref(), Some("bh_pat_secret"));
        let app = atest::init_service(
            App::new()
                .app_data(web::Data::new(Arc::clone(&st)))
                .configure(configure),
        )
        .await;
        let req = atest::TestRequest::post()
            .uri("/abc123/vsx/vscode/gallery/extensionquery")
            .set_json(
                json!({ "filters": [{ "criteria": [{ "filterType": 10, "value": "real" }] }] }),
            )
            .to_request();
        let resp = atest::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let doc: Value = atest::read_body_json(resp).await;
        let v = &doc["results"][0]["extensions"][0]["versions"][0];
        assert_eq!(
            v["assetUri"],
            "http://127.0.0.1:1/abc123/vsx/vscode/asset/p/real/1.0.0"
        );
        assert!(
            doc.to_string().contains("\"real\"") && !doc.to_string().contains(SIGN_IN_NAME),
            "an authenticated session never sees the sign-in entry"
        );
        upstream.assert_async().await;
    }
}
