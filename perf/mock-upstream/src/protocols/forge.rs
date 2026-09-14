//! The three forges: GitHub, Forgejo and GitLab release APIs.
//!
//! One module, because the proxy's three clients read the same idea through
//! three spellings — a list of releases, each with a tag and a set of assets
//! addressed by URL. What differs is the path (`/repos/{owner}/{repo}/releases`
//! for GitHub and Forgejo, `/projects/{id}/releases` for GitLab), the asset
//! container (`assets` vs `assets.links`), and, for Forgejo, that an asset is
//! served from `{forge}/attachments/{uuid}` rather than from the API host.
//!
//! The asset URLs point back at this mock, so a proxied *download* is exercised
//! and not only the document parse.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::{artifact_bytes, delay, host, sha256_hex};
use crate::Args;

const TAGS: [&str; 3] = ["v1.0.0", "v1.1.0", "v1.2.0"];

/// `GET /github/repos/{owner}/{repo}/releases`
#[route(
    "/github/repos/{owner}/{repo}/releases",
    method = "GET",
    method = "HEAD"
)]
async fn github_releases(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (owner, repo) = path.into_inner();
    let host = host(&req);
    let releases: Vec<_> = TAGS
        .iter()
        .enumerate()
        .map(|(i, tag)| {
            let name = format!("{repo}-{tag}.tar.gz");
            let bytes = artifact_bytes(&name, tag, args.artifact_size_kb * 1024);
            serde_json::json!({
                "id": 1000 + i,
                "tag_name": tag,
                "published_at": "2024-01-01T00:00:00Z",
                "draft": false,
                "prerelease": false,
                "assets": [{
                    "id": 2000 + i,
                    "name": name,
                    "browser_download_url":
                        format!("http://{host}/github/download/{owner}/{repo}/{tag}/{name}"),
                    "size": bytes.len(),
                    "digest": format!("sha256:{}", sha256_hex(&bytes)),
                }],
            })
        })
        .rev()
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(releases).to_string())
}

/// `GET /github/download/{owner}/{repo}/{tag}/{name}` — the asset itself.
#[route(
    "/github/download/{owner}/{repo}/{tag}/{name}",
    method = "GET",
    method = "HEAD"
)]
async fn github_asset(
    path: web::Path<(String, String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_owner, _repo, tag, name) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&name, &tag, args.artifact_size_kb * 1024))
}

/// `GET /forgejo/api/v1/repos/{owner}/{repo}/releases`
///
/// The asset URL is `{forge}/attachments/{uuid}` — *always*, and this is the
/// one place the three forges genuinely differ rather than merely spell things
/// differently. Rewriting `browser_download_url` to anything else routes the
/// checksum and leaves the file pointing at the upstream.
#[route(
    "/forgejo/api/v1/repos/{owner}/{repo}/releases",
    method = "GET",
    method = "HEAD"
)]
async fn forgejo_releases(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_owner, repo) = path.into_inner();
    let host = host(&req);
    let releases: Vec<_> = TAGS
        .iter()
        .enumerate()
        .map(|(i, tag)| {
            let name = format!("{repo}-{tag}.tar.gz");
            serde_json::json!({
                "id": 1000 + i,
                "tag_name": tag,
                "published_at": "2024-01-01T00:00:00Z",
                "draft": false,
                "prerelease": false,
                "assets": [{
                    "id": 2000 + i,
                    "name": name,
                    "size": 0,
                    "browser_download_url": format!(
                        "http://{host}/forgejo/attachments/0000000{i}-0000-0000-0000-00000000000{i}"
                    ),
                }],
            })
        })
        .rev()
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(releases).to_string())
}

/// `GET /forgejo/attachments/{uuid}` — where a Forgejo asset actually lives.
#[route("/forgejo/attachments/{uuid}", method = "GET", method = "HEAD")]
async fn forgejo_attachment(uuid: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let uuid = uuid.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&uuid, "", args.artifact_size_kb * 1024))
}

/// `GET /gitlab/api/v4/projects/{id}/releases`
#[route(
    "/gitlab/api/v4/projects/{id}/releases",
    method = "GET",
    method = "HEAD"
)]
async fn gitlab_releases(
    req: HttpRequest,
    id: web::Path<String>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let id = id.into_inner();
    let host = host(&req);
    let releases: Vec<_> = TAGS
        .iter()
        .map(|tag| {
            let name = format!("asset-{tag}.tar.gz");
            serde_json::json!({
                "tag_name": tag,
                "released_at": "2024-01-01T00:00:00Z",
                "upcoming_release": false,
                "evidences": [],
                "assets": {
                    "links": [{
                        "name": name,
                        "url": format!("http://{host}/gitlab/download/{tag}/{name}"),
                        "direct_asset_url":
                            format!("http://{host}/gitlab/download/{tag}/{name}"),
                    }],
                    "sources": [{
                        "format": "tar.gz",
                        "url": format!("http://{host}/gitlab/download/{tag}/source.tar.gz"),
                    }],
                },
                "_project": id,
            })
        })
        .rev()
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(releases).to_string())
}

/// `GET /gitlab/download/{tag}/{name}` — a release link's target.
#[route("/gitlab/download/{tag}/{name}", method = "GET", method = "HEAD")]
async fn gitlab_asset(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (tag, name) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&name, &tag, args.artifact_size_kb * 1024))
}

// ── one release, by tag ─────────────────────────────────────────────────────
//
// An asset download does not read the release *list*: it asks for the one
// release the tag names, and falls through to a tag/branch lookup when that
// 404s — which is why a mock with only the list answers "no tag or branch named
// 'v1.2.0'" for every download.

/// `GET /github/repos/{owner}/{repo}/releases/tags/{tag}`
#[route(
    "/github/repos/{owner}/{repo}/releases/tags/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn github_release_by_tag(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (owner, repo, tag) = path.into_inner();
    let host = host(&req);
    let name = format!("{repo}-{tag}.tar.gz");
    let bytes = artifact_bytes(&name, &tag, args.artifact_size_kb * 1024);
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "id": 1000,
            "tag_name": tag,
            "published_at": "2024-01-01T00:00:00Z",
            "draft": false,
            "prerelease": false,
            "assets": [{
                "id": 2000,
                "name": name,
                "browser_download_url":
                    format!("http://{host}/github/download/{owner}/{repo}/{tag}/{name}"),
                "size": bytes.len(),
                "digest": format!("sha256:{}", sha256_hex(&bytes)),
            }],
        })
        .to_string(),
    )
}

/// `GET /forgejo/api/v1/repos/{owner}/{repo}/releases/tags/{tag}`
#[route(
    "/forgejo/api/v1/repos/{owner}/{repo}/releases/tags/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn forgejo_release_by_tag(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_owner, repo, tag) = path.into_inner();
    let host = host(&req);
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "id": 1000,
            "tag_name": tag,
            "published_at": "2024-01-01T00:00:00Z",
            "draft": false,
            "prerelease": false,
            "assets": [{
                "id": 2000,
                "name": format!("{repo}-{tag}.tar.gz"),
                "size": 0,
                "browser_download_url":
                    format!("http://{host}/forgejo/attachments/00000000-0000-0000-0000-000000000000"),
            }],
        })
        .to_string(),
    )
}

/// `GET /gitlab/api/v4/projects/{id}/releases/{tag}`
#[route(
    "/gitlab/api/v4/projects/{id}/releases/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn gitlab_release_by_tag(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_id, tag) = path.into_inner();
    let host = host(&req);
    let name = format!("asset-{tag}.tar.gz");
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "tag_name": tag,
            "released_at": "2024-01-01T00:00:00Z",
            "upcoming_release": false,
            "evidences": [],
            "assets": {
                "links": [{
                    "name": name,
                    "url": format!("http://{host}/gitlab/download/{tag}/{name}"),
                    "direct_asset_url": format!("http://{host}/gitlab/download/{tag}/{name}"),
                }],
                "sources": [{
                    "format": "tar.gz",
                    "url": format!("http://{host}/gitlab/download/{tag}/source.tar.gz"),
                }],
            },
        })
        .to_string(),
    )
}

// ── resolving the ref behind a tag ──────────────────────────────────────────
//
// An asset download is not only a release lookup: RFC 0019's provenance wants
// the commit the tag points at, so each client resolves the ref first and
// reports "no tag or branch named '…'" when it cannot. Three more spellings of
// the same question — GitHub's `git/ref/tags/{tag}`, Forgejo's `tags/{tag}`,
// GitLab's `repository/tags/{tag}`.

const COMMIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// `GET /github/repos/{owner}/{repo}/git/ref/tags/{tag}`
#[route(
    "/github/repos/{owner}/{repo}/git/ref/tags/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn github_ref(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_owner, _repo, tag) = path.into_inner();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "ref": format!("refs/tags/{tag}"),
            // A lightweight tag: the ref object *is* the commit, so there is no
            // second `git/tags/{sha}` round trip to answer.
            "object": { "sha": COMMIT_SHA, "type": "commit" },
        })
        .to_string(),
    )
}

/// `GET /forgejo/api/v1/repos/{owner}/{repo}/tags/{tag}`
#[route(
    "/forgejo/api/v1/repos/{owner}/{repo}/tags/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn forgejo_tag(
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_owner, _repo, tag) = path.into_inner();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "name": tag,
            "commit": { "sha": COMMIT_SHA, "created": "2024-01-01T00:00:00Z" },
        })
        .to_string(),
    )
}

/// `GET /gitlab/api/v4/projects/{id}/repository/tags/{tag}`
#[route(
    "/gitlab/api/v4/projects/{id}/repository/tags/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn gitlab_tag(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_id, tag) = path.into_inner();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "name": tag,
            "created_at": "2024-01-01T00:00:00Z",
            "commit": {
                "id": COMMIT_SHA,
                "committed_date": "2024-01-01T00:00:00Z",
                "committer_name": "perf",
                "committer_email": "perf@example.com",
            },
        })
        .to_string(),
    )
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(github_ref)
        .service(forgejo_tag)
        .service(gitlab_tag)
        .service(github_release_by_tag)
        .service(forgejo_release_by_tag)
        .service(gitlab_release_by_tag)
        .service(github_releases)
        .service(github_asset)
        .service(forgejo_releases)
        .service(forgejo_attachment)
        .service(gitlab_releases)
        .service(gitlab_asset);
}
