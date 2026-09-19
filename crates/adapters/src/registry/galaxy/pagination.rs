//! Walking upstream's pages so the client never has to (RFC 0031 §6.4).
//!
//! Every listing this instance serves is **one page**, because no pagination
//! link it could emit survives `ansible-galaxy`'s own URL joining: collections
//! do `urljoin(self.api_server, next_link)`, and an absolute-path link replaces
//! the whole path — a link of `/api/v3/…` sends the next request to the root of
//! the host rather than to `/proxy/{registry}/galaxy/…`; roles do
//! `urljoin("{scheme}://{netloc}/", next_link)`, with the path stripped *on
//! purpose* (the fix for ansible issue 64355). So the adapter walks the pages
//! and hands back one assembled document.
//!
//! Upstream caps a page at 100 whatever `?limit` asks for — `?limit=500` and
//! `?limit=1000` both answer 100 items and a `next` whose `limit` has been
//! rewritten to 100 — so `community.general`'s 241 versions are three requests
//! per cache fill, for the largest collection in existence.
//!
//! Two bounds make that walk safe rather than merely correct: a link is
//! followed only when it stays on the **same origin** as the page that carried
//! it, and at most [`MAX_PAGES`] pages are read. Without them one client
//! request could be amplified into an unbounded upstream walk by a document
//! that links to itself.

use batlehub_core::error::CoreError;
use serde_json::Value;

use crate::registry::http_client::{same_origin, to_registry_error};

/// The hard cap on one assembled listing.
///
/// 50 pages × 100 entries is 5 000 versions: two decades of weekly releases for
/// the busiest collection that exists, and still a bounded amount of work for
/// one client request.
pub const MAX_PAGES: usize = 50;

/// How many entries one page asks for. Upstream rewrites anything larger down
/// to 100 and says so in the `next` it returns, so asking for more is a request
/// that describes itself wrongly.
pub const PAGE_LIMIT: usize = 100;

/// The entries of every page reachable from `start`, concatenated in order.
///
/// `data` is galaxy_ng's key and `results` is a standalone pulp_ansible's; both
/// are read, on each page independently, because a proxy in front of either
/// should behave the same.
pub async fn collect_pages(
    fetch: impl Fn(String) -> futures::future::BoxFuture<'static, Result<reqwest::Response, CoreError>>,
    start: &str,
) -> Result<(Vec<Value>, Option<u64>), CoreError> {
    let origin = reqwest::Url::parse(start)
        .map_err(|e| CoreError::Registry(format!("invalid listing URL '{start}': {e}")))?;
    let mut url = start.to_owned();
    let mut entries: Vec<Value> = Vec::new();
    let mut upstream_count: Option<u64> = None;

    for page in 0..MAX_PAGES {
        let resp = fetch(url.clone()).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{url} not found upstream")));
        }
        let body: Value = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json()
            .await
            .map_err(|e| CoreError::Registry(format!("parsing listing page '{url}': {e}")))?;

        if upstream_count.is_none() {
            upstream_count = body
                .get("meta")
                .and_then(|m| m.get("count"))
                .or_else(|| body.get("count"))
                .and_then(Value::as_u64);
        }
        if let Some(page_entries) = body
            .get("data")
            .or_else(|| body.get("results"))
            .and_then(Value::as_array)
        {
            entries.extend(page_entries.iter().cloned());
        }

        let Some(next) = next_link(&body) else {
            return Ok((entries, upstream_count));
        };
        let resolved = origin.join(&next).map_err(|e| {
            CoreError::Registry(format!(
                "upstream pagination link '{next}' is not a URL: {e}"
            ))
        })?;
        // Same-origin only: a `next` that points somewhere else is an upstream
        // document redirecting this instance's own walk at a host the operator
        // never configured, which is the shape of an SSRF rather than of a page
        // two.
        if !same_origin(&resolved, &origin) {
            return Err(CoreError::Registry(format!(
                "refusing to follow cross-origin pagination link '{next}' \
                 (expected the origin of '{start}')"
            )));
        }
        if resolved.as_str() == url {
            // A page that links to itself would otherwise be read MAX_PAGES
            // times before the cap noticed.
            return Ok((entries, upstream_count));
        }
        url = resolved.into();
        if page + 1 == MAX_PAGES {
            tracing::warn!(
                start = %start,
                pages = MAX_PAGES,
                "stopped walking an upstream listing at the page cap; the served \
                 document is truncated"
            );
        }
    }
    Ok((entries, upstream_count))
}

/// The continuation link of a page, in either of the two spellings the two
/// upstream implementations use.
fn next_link(body: &Value) -> Option<String> {
    let v3 = body
        .get("links")
        .and_then(|l| l.get("next"))
        .and_then(Value::as_str);
    let v1 = body
        .get("next_link")
        .or_else(|| body.get("next"))
        .and_then(Value::as_str);
    v3.or(v1).filter(|s| !s.is_empty()).map(str::to_owned)
}
