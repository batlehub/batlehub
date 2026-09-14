//! The two editor marketplaces: Visual Studio Marketplace and JetBrains.
//!
//! Neither is addressed like a package registry, which is why they are here
//! rather than beside `openvsx`:
//!
//!   - **VS Code** has one endpoint, a `POST` of a *query document* to
//!     `_apis/public/gallery/extensionquery`, and every answer carries the
//!     extension's assets as URLs on a different host. This mock answers any
//!     query with one extension, which is enough for the proxy's parse,
//!     rewrite and download path and is not enough to browse.
//!   - **JetBrains** speaks two spellings of the same plugin and the proxy uses
//!     both: `/plugins/list?pluginId={xmlId}` is the classic
//!     plugin-repository **XML**, which is what a published coordinate
//!     (`org.rust.lang`) resolves through, while `/api/plugins/{numericId}`
//!     and `/plugin/download` are the JSON the IDE itself uses. A mock with
//!     only the JSON answers "plugin not found" for every published
//!     coordinate, which is how this one started.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::*;
use crate::Args;

// ── Visual Studio Marketplace ───────────────────────────────────────────────

const VSCODE_VERSION: &str = "1.2.0";

/// `POST /vscode/_apis/public/gallery/extensionquery`
///
/// The request names the extension in a filter; the reply is a result set. The
/// query is not read: every arm of the soak asks for an extension this answers
/// for, and a mock that matched on the filter would be implementing search.
#[route(
    "/vscode/_apis/public/gallery/extensionquery",
    method = "POST",
    method = "GET",
    method = "HEAD"
)]
async fn extension_query(req: HttpRequest, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let host = host(&req);
    let base = format!("http://{host}/vscode/assets/perf/demo/{VSCODE_VERSION}");
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "results": [{
                "extensions": [{
                    "extensionName": "demo",
                    "displayName": "Perf Demo",
                    "shortDescription": "mock extension for perf tests",
                    "publisher": { "publisherName": "perf" },
                    "versions": [{
                        "version": VSCODE_VERSION,
                        "lastUpdated": "2024-01-01T00:00:00Z",
                        "files": [
                            {
                                "assetType": "Microsoft.VisualStudio.Services.VSIXPackage",
                                "source": format!("{base}/Microsoft.VisualStudio.Services.VSIXPackage"),
                            },
                            {
                                "assetType": "Microsoft.VisualStudio.Services.Content.Details",
                                "source": format!("{base}/Microsoft.VisualStudio.Services.Content.Details"),
                            },
                        ],
                    }],
                }],
            }],
        })
        .to_string(),
    )
}

/// `GET /vscode/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage`
///
/// The VSIX, at the fixed URL the client builds — *not* at the `files[].source`
/// the query document advertises. Those two are different addresses for the
/// same bytes and the download uses this one; a mock that serves only the
/// advertised URL answers the document fine and 404s every download.
#[route(
    "/vscode/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage",
    method = "GET",
    method = "HEAD"
)]
async fn vspackage(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (publisher, name, version) = path.into_inner();
    // Served uncompressed. The real gallery answers `Content-Encoding: gzip`
    // unsolicited and the client gunzips it; sending plain bytes exercises the
    // same path without making this mock's answer depend on that quirk.
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &format!("{publisher}.{name}"),
            &version,
            args.artifact_size_kb * 1024,
        ))
}

/// `GET /vscode/assets/{publisher}/{name}/{version}/{asset_type}` — the VSIX
/// and everything else the query document linked to.
#[route(
    "/vscode/assets/{publisher}/{name}/{version}/{asset_type}",
    method = "GET",
    method = "HEAD"
)]
async fn vscode_asset(
    path: web::Path<(String, String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_publisher, name, version, asset_type) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &format!("{name}/{asset_type}"),
            &version,
            args.artifact_size_kb * 1024,
        ))
}

// ── JetBrains Marketplace ───────────────────────────────────────────────────

/// The plugin this mock publishes, by the numeric id the API is keyed on.
const PLUGIN_ID: u64 = 12345;
const UPDATE_ID: u64 = 67890;

/// `GET /jbmarket/plugins/list?pluginId={xmlId}` — the plugin-repository XML.
///
/// One `<idea-plugin>` per version, newest first, inside a `<category>`. This
/// is the document a published coordinate resolves through; the JSON below is
/// the IDE's own spelling of the same plugin.
#[route("/jbmarket/plugins/list", method = "GET", method = "HEAD")]
async fn plugins_list(req: HttpRequest, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let xml_id = req
        .query_string()
        .split('&')
        .find_map(|p| p.strip_prefix("pluginId="))
        .unwrap_or("com.perf.demo")
        .to_owned();

    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plugin-repository>\n  <category name=\"Perf\">\n",
    );
    for (i, version) in ["1.2.0", "1.1.0", "1.0.0"].iter().enumerate() {
        let date_ms = 1_704_067_200_000i64 - (i as i64) * 86_400_000;
        body.push_str(&format!(
            concat!(
                "    <idea-plugin downloads=\"100\" size=\"2048\" date=\"{date}\">\n",
                "      <name>Perf Demo</name>\n",
                "      <id>{id}</id>\n",
                "      <version>{version}</version>\n",
                "      <idea-version since-build=\"233.0\" until-build=\"243.*\"/>\n",
                "      <vendor>Perf</vendor>\n",
                "      <description>mock plugin for perf tests</description>\n",
                "    </idea-plugin>\n",
            ),
            date = date_ms,
            id = xml_id,
            version = version,
        ));
    }
    body.push_str("  </category>\n</plugin-repository>\n");
    HttpResponse::Ok().content_type("text/xml").body(body)
}

/// `GET /jbmarket/api/plugins/{id}` — the plugin's own document.
#[route("/jbmarket/api/plugins/{id}", method = "GET", method = "HEAD")]
async fn plugin(id: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let id = id.into_inner();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "id": PLUGIN_ID,
            "xmlId": "com.perf.demo",
            "name": "Perf Demo",
            "preview": "mock plugin for perf tests",
            "family": "intellij",
            "urlName": id,
        })
        .to_string(),
    )
}

/// `GET /jbmarket/api/plugins/{id}/updates` — the versions of that plugin.
#[route("/jbmarket/api/plugins/{id}/updates", method = "GET", method = "HEAD")]
async fn plugin_updates(id: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let _ = id;
    let updates: Vec<_> = ["1.0.0", "1.1.0", "1.2.0"]
        .iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::json!({
                "id": UPDATE_ID + i as u64,
                "pluginId": PLUGIN_ID,
                "version": v,
                "cdate": "1704067200000",
                "since": "233",
                "until": "243.*",
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(updates).to_string())
}

/// `GET /jbmarket/plugin/download` — the archive, addressed by `updateId`.
#[route("/jbmarket/plugin/download", method = "GET", method = "HEAD")]
async fn plugin_download(req: HttpRequest, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let query = req.query_string().to_owned();
    HttpResponse::Ok()
        .content_type("application/zip")
        .body(artifact_bytes(&query, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(extension_query)
        .service(vspackage)
        .service(plugins_list)
        .service(vscode_asset)
        .service(plugin)
        .service(plugin_updates)
        .service(plugin_download);
}
