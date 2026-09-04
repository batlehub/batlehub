//! Git forges: GitHub, Forgejo/Gitea and GitLab release listings.
//!
//! Three APIs, one document shape — a JSON array of release objects, newest
//! first, each naming its release by `tag_name`. Forgejo is deliberately
//! GitHub-compatible here, and GitLab's `/releases` uses the same field name.
//!
//! A "version" on a forge is a **tag**, and tags are spelled inconsistently:
//! the same release is `1.2.3` in one repository and `v1.2.3` in the next. An
//! operator who blocks `1.2.3` means the release, not the string, so
//! `normalize` strips the prefix on both sides for these kinds.
//!
//! Nothing in the document names a preferred release beyond its position, so
//! there is nothing to repair — dropping the entry is the whole filter.

use serde_json::Value;

use super::BlockedVersions;

/// Remove blocked releases from a forge's release listing.
///
/// Order is preserved: forges serve these newest-first and clients page through
/// them in that order.
pub fn strip_releases(doc: &mut Value, blocked: &BlockedVersions) -> Vec<String> {
    let Some(releases) = doc.as_array_mut() else {
        return Vec::new();
    };

    let mut removed = Vec::new();
    releases.retain(|r| {
        let Some(tag) = release_tag(r) else {
            // No tag to judge it by; keeping it over-lists, which is the safe
            // direction.
            return true;
        };
        if blocked.contains(tag) {
            removed.push(tag.to_owned());
            false
        } else {
            true
        }
    });
    removed
}

/// The tag a release object names, whichever of the two spellings the forge
/// uses for the field.
fn release_tag(release: &Value) -> Option<&str> {
    release
        .get("tag_name")
        .or_else(|| release.get("tag"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;
    use serde_json::json;

    fn blocked(vs: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Github,
            vs.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    fn releases() -> Value {
        json!([
            { "id": 3, "tag_name": "v2.0.0", "published_at": "2024-03-01T00:00:00Z" },
            { "id": 2, "tag_name": "v1.1.0", "published_at": "2024-02-01T00:00:00Z" },
            { "id": 1, "tag_name": "v1.0.0", "published_at": "2024-01-01T00:00:00Z" }
        ])
    }

    fn tags(doc: &Value) -> Vec<String> {
        doc.as_array()
            .expect("a release array")
            .iter()
            .map(|r| r["tag_name"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn a_blocked_release_leaves_the_listing_and_order_survives() {
        let mut doc = releases();
        let removed = strip_releases(&mut doc, &blocked(&["v1.1.0"]));

        assert_eq!(removed, vec!["v1.1.0".to_owned()]);
        assert_eq!(tags(&doc), ["v2.0.0", "v1.0.0"], "still newest-first");
    }

    /// The same release is tagged `1.2.3` in one repository and `v1.2.3` in the
    /// next; a block must not depend on which habit the operator copied.
    #[test]
    fn a_block_matches_a_tag_with_or_without_its_v_prefix() {
        let mut doc = releases();
        strip_releases(&mut doc, &blocked(&["1.1.0"]));
        assert_eq!(tags(&doc), ["v2.0.0", "v1.0.0"]);

        let mut doc = json!([{ "tag_name": "1.1.0" }]);
        strip_releases(&mut doc, &blocked(&["v1.1.0"]));
        assert_eq!(doc, json!([]));
    }

    /// Forgejo mirrors GitHub's `tag_name`; some Gitea versions answer with
    /// `tag`. Reading either is cheaper than being wrong on one of them.
    #[test]
    fn either_tag_field_spelling_is_understood() {
        let mut doc = json!([{ "tag": "v1.1.0" }, { "tag": "v1.0.0" }]);
        strip_releases(&mut doc, &blocked(&["v1.1.0"]));

        assert_eq!(doc.as_array().unwrap().len(), 1);
    }

    #[test]
    fn blocking_an_absent_release_changes_nothing() {
        let mut doc = releases();
        let before = doc.clone();
        assert!(strip_releases(&mut doc, &blocked(&["v9.9.9"])).is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn blocking_every_release_leaves_an_empty_array() {
        let mut doc = releases();
        strip_releases(&mut doc, &blocked(&["v2.0.0", "v1.1.0", "v1.0.0"]));

        assert_eq!(doc, json!([]));
    }

    #[test]
    fn a_release_with_no_tag_is_kept() {
        let mut doc = json!([{ "id": 1, "name": "untagged draft" }]);
        let before = doc.clone();
        assert!(strip_releases(&mut doc, &blocked(&["v1.0.0"])).is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn a_malformed_document_is_returned_unchanged() {
        let mut doc = json!({ "message": "Not Found" });
        let before = doc.clone();
        assert!(strip_releases(&mut doc, &blocked(&["v1.0.0"])).is_empty());
        assert_eq!(doc, before);
    }
}

/// Repoint a release document's download URLs at this proxy (RFC 0019 §4.2
/// *API reads*).
///
/// The forge's own JSON advertises `tarball_url`, `zipball_url` and each
/// asset's `browser_download_url` as absolute upstream URLs. A client that
/// reads them — `mise`, `gh`, anything that follows the release document
/// rather than building a path — goes straight to the forge and past the
/// proxy: no policy, no cache, no audit row, and on a private upstream no
/// credential either. Rewriting them is what makes the release document mean
/// the same thing as the routes beside it.
///
/// Works on one release object or on a list of them, which is the two shapes
/// the two routes return. Fields the forge does not carry are not invented,
/// and any other field is left exactly as it came.
pub fn rewrite_release_urls(doc: &mut Value, public_base: &str, owner_repo: &str) {
    let base = public_base.trim_end_matches('/');
    if base.is_empty() {
        return;
    }
    match doc {
        Value::Array(items) => {
            for item in items {
                rewrite_one(item, base, owner_repo);
            }
        }
        other => rewrite_one(other, base, owner_repo),
    }
}

fn rewrite_one(release: &mut Value, base: &str, owner_repo: &str) {
    let Some(obj) = release.as_object_mut() else {
        return;
    };
    // The tag names the archive coordinates; without one there is nothing to
    // point at and the upstream URL is left alone rather than replaced by a
    // path that 404s.
    let tag = obj
        .get("tag_name")
        .or_else(|| obj.get("name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(tag) = &tag {
        let tag_seg = super::encode_package_segment(tag);
        if obj.contains_key("tarball_url") {
            obj.insert(
                "tarball_url".to_owned(),
                Value::String(format!("{base}/{owner_repo}/tarball/{tag_seg}")),
            );
        }
        if obj.contains_key("zipball_url") {
            obj.insert(
                "zipball_url".to_owned(),
                Value::String(format!("{base}/{owner_repo}/zipball/{tag_seg}")),
            );
        }
    }
    // GitLab's release document is a different shape: `assets` is an object
    // with `sources` (the generated archives, by format) and `links` (what
    // the maintainer attached). Confirmed against gitlab.com on 2026-09-04.
    if let Some(assets) = obj.get_mut("assets").and_then(Value::as_object_mut) {
        if let (Some(tag), Some(sources)) = (
            &tag,
            assets.get_mut("sources").and_then(Value::as_array_mut),
        ) {
            for source in sources {
                let Some(src) = source.as_object_mut() else {
                    continue;
                };
                let format = src
                    .get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("tar.gz")
                    .to_owned();
                let repo = owner_repo.rsplit('/').next().unwrap_or(owner_repo);
                src.insert(
                    "url".to_owned(),
                    Value::String(format!(
                        "{base}/{owner_repo}/-/archive/{}/{repo}-{}.{format}",
                        super::encode_package_segment(tag),
                        super::encode_package_segment(tag)
                    )),
                );
            }
        }
        if let (Some(tag), Some(links)) =
            (&tag, assets.get_mut("links").and_then(Value::as_array_mut))
        {
            for link in links {
                let Some(l) = link.as_object_mut() else {
                    continue;
                };
                let Some(name) = l.get("name").and_then(Value::as_str).map(str::to_owned) else {
                    continue;
                };
                let url = Value::String(format!(
                    "{base}/{owner_repo}/-/releases/{}/downloads/{}",
                    super::encode_package_segment(tag),
                    super::encode_package_segment(&name)
                ));
                if l.contains_key("url") {
                    l.insert("url".to_owned(), url.clone());
                }
                if l.contains_key("direct_asset_url") {
                    l.insert("direct_asset_url".to_owned(), url);
                }
            }
        }
        // The forge's own API links: no equivalent here, and a working way
        // around every rule above.
        obj.remove("_links");
        return;
    }

    let Some(assets) = obj.get_mut("assets").and_then(Value::as_array_mut) else {
        return;
    };
    for asset in assets {
        let Some(a) = asset.as_object_mut() else {
            continue;
        };
        let name = a.get("name").and_then(Value::as_str).map(str::to_owned);
        let by_id = a
            .get("id")
            .and_then(|v| v.as_u64())
            .map(|id| format!("{base}/{owner_repo}/releases/assets/{id}"));
        // By name where the asset has one, by id otherwise: both are routes
        // this proxy serves, and the name is the one a human reads.
        let replacement = match (&tag, &name) {
            (Some(tag), Some(name)) => Some(format!(
                "{base}/{owner_repo}/releases/download/{}/{}",
                super::encode_package_segment(tag),
                super::encode_package_segment(name)
            )),
            _ => by_id.clone(),
        };
        if let (Some(url), true) = (replacement, a.contains_key("browser_download_url")) {
            a.insert("browser_download_url".to_owned(), Value::String(url));
        }
        // The asset's API URL — the forge's own asset endpoint, which needs
        // the forge's token and is a working way around every rule above.
        //
        // **Repointed, not removed.** Removing it looked safe and is not:
        // `url` is a required field of an asset in the GitHub API's own
        // schema, and a client that deserializes the document strictly fails
        // on the whole release list rather than on one asset. mise does, and
        // an install through a BatleHub github registry answered `missing
        // field \`url\`` for every repository until this was measured — a
        // rule that silently broke the clients it was protecting. This proxy
        // *does* have an equivalent for it: `releases/assets/{id}` is a route
        // it serves, under the same rules as every other artifact, so the
        // bypass is closed by pointing the field here rather than by deleting
        // it. An asset with no id has no route to name, and only then is the
        // field dropped.
        match by_id {
            Some(url) if a.contains_key("url") => {
                a.insert("url".to_owned(), Value::String(url));
            }
            _ => {
                a.remove("url");
            }
        }
        a.remove("uploader");
    }
    obj.remove("assets_url");
    obj.remove("upload_url");
}

#[cfg(test)]
mod rewrite_tests {
    use super::*;
    use serde_json::json;

    fn release() -> Value {
        json!({
            "tag_name": "v2.60.0",
            "tarball_url": "https://api.github.com/repos/cli/cli/tarball/v2.60.0",
            "zipball_url": "https://api.github.com/repos/cli/cli/zipball/v2.60.0",
            "assets_url": "https://api.github.com/repos/cli/cli/releases/1/assets",
            "upload_url": "https://uploads.github.com/repos/cli/cli/releases/1/assets{?name,label}",
            "assets": [{
                "id": 42,
                "name": "gh_2.60.0_linux_amd64.tar.gz",
                "browser_download_url": "https://github.com/cli/cli/releases/download/v2.60.0/gh.tar.gz",
                "url": "https://api.github.com/repos/cli/cli/releases/assets/42",
                "uploader": { "login": "someone" },
                "digest": "sha256:abc"
            }],
            "body": "notes with a https://github.com link that is prose, not a download"
        })
    }

    #[test]
    fn every_download_url_points_back_at_the_proxy() {
        let mut doc = release();
        rewrite_release_urls(&mut doc, "https://hub.example/proxy/gh", "cli/cli");
        assert_eq!(
            doc["tarball_url"],
            "https://hub.example/proxy/gh/cli/cli/tarball/v2.60.0"
        );
        assert_eq!(
            doc["zipball_url"],
            "https://hub.example/proxy/gh/cli/cli/zipball/v2.60.0"
        );
        assert_eq!(
            doc["assets"][0]["browser_download_url"],
            "https://hub.example/proxy/gh/cli/cli/releases/download/v2.60.0/gh_2.60.0_linux_amd64.tar.gz"
        );
        // The asset's API URL is *repointed*, not removed: it is a required
        // field of the GitHub schema, and a client that deserializes the
        // document strictly — mise does — fails on the whole release list
        // when it is missing. The route it now names is one this proxy
        // serves, under the same rules, so the way around is still closed.
        assert_eq!(
            doc["assets"][0]["url"],
            "https://hub.example/proxy/gh/cli/cli/releases/assets/42"
        );
        // The ways around the proxy with no equivalent here are gone;
        // everything else is untouched.
        assert!(doc["assets"][0].get("uploader").is_none());
        assert!(doc.get("assets_url").is_none());
        assert!(doc.get("upload_url").is_none());
        assert_eq!(doc["assets"][0]["digest"], "sha256:abc");
        assert!(doc["body"].as_str().unwrap().contains("prose"));
    }

    /// An asset with no id has no route to name, so the field goes rather
    /// than pointing at something that would 404.
    #[test]
    fn an_asset_with_no_id_loses_the_field_rather_than_gaining_a_dead_route() {
        let mut doc = json!({
            "tag_name": "v1",
            "assets": [{
                "name": "thing.tar.gz",
                "url": "https://api.github.com/repos/cli/cli/releases/assets/7",
            }],
        });
        rewrite_release_urls(&mut doc, "https://hub.example/proxy/gh", "cli/cli");
        assert!(doc["assets"][0].get("url").is_none());
    }

    #[test]
    fn a_listing_is_rewritten_release_by_release() {
        let mut doc = json!([release(), release()]);
        rewrite_release_urls(&mut doc, "https://hub.example/proxy/gh", "cli/cli");
        for item in doc.as_array().unwrap() {
            assert!(item["tarball_url"]
                .as_str()
                .unwrap()
                .starts_with("https://hub.example/proxy/gh/"));
        }
    }

    /// GitLab's shape, as gitlab.com answers it (confirmed 2026-09-04):
    /// `assets` is an object, its `sources` are the generated archives and
    /// its `links` are what the maintainer attached.
    #[test]
    fn a_gitlab_release_is_rewritten_through_its_own_shape() {
        let mut doc = json!({
            "tag_name": "v1.40.0",
            "assets": {
                "count": 5,
                "sources": [
                    { "format": "zip", "url": "https://gitlab.com/gitlab-org/cli/-/archive/v1.40.0/cli-v1.40.0.zip" },
                    { "format": "tar.gz", "url": "https://gitlab.com/gitlab-org/cli/-/archive/v1.40.0/cli-v1.40.0.tar.gz" }
                ],
                "links": [
                    { "id": 1, "name": "glab_linux", "url": "https://elsewhere.example/glab", "direct_asset_url": "https://elsewhere.example/glab" }
                ]
            },
            "evidences": [{ "sha": "abc" }],
            "_links": { "self": "https://gitlab.com/gitlab-org/cli/-/releases/v1.40.0" }
        });
        rewrite_release_urls(&mut doc, "https://hub.example/proxy/gl", "gitlab-org/cli");
        assert_eq!(
            doc["assets"]["sources"][0]["url"],
            "https://hub.example/proxy/gl/gitlab-org/cli/-/archive/v1.40.0/cli-v1.40.0.zip"
        );
        assert_eq!(
            doc["assets"]["links"][0]["url"],
            "https://hub.example/proxy/gl/gitlab-org/cli/-/releases/v1.40.0/downloads/glab_linux"
        );
        assert_eq!(
            doc["assets"]["links"][0]["direct_asset_url"],
            doc["assets"]["links"][0]["url"]
        );
        assert!(doc.get("_links").is_none());
        // The evidence block is GitLab's own provenance and is left alone —
        // phase 5 reads it.
        assert_eq!(doc["evidences"][0]["sha"], "abc");
    }

    #[test]
    fn nothing_is_invented_and_no_base_means_no_rewrite() {
        // No `tag_name`: the archive URLs are left as they came rather than
        // replaced by a path that would 404.
        let mut doc = json!({ "tarball_url": "https://api.github.com/x" });
        rewrite_release_urls(&mut doc, "https://hub.example/proxy/gh", "cli/cli");
        assert_eq!(doc["tarball_url"], "https://api.github.com/x");

        // A registry with no public base (a test, an unrouted host) leaves
        // the document alone rather than emitting a relative URL.
        let mut doc = release();
        rewrite_release_urls(&mut doc, "", "cli/cli");
        assert_eq!(
            doc["tarball_url"],
            "https://api.github.com/repos/cli/cli/tarball/v2.60.0"
        );
    }
}
