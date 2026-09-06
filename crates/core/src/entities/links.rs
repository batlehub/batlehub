//! Where a package's code and its site live, as its own metadata names them.
//!
//! Travels on [`crate::entities::PackageMetadata::extra`] under the `links` key,
//! the same channel [`crate::entities::MetadataReadme`] uses and for the same
//! reason: a registry client has already parsed the document that carries this,
//! and routing it through `extra` gets it to the console without widening
//! `RegistryClient`'s signature.
//!
//! **Every URL here becomes an `href` in an authenticated operator's console**,
//! and it was written by whoever published the package. That is the whole reason
//! [`normalize_url`] exists and why it is an allow-list rather than a tidy-up:
//! the README path has a sanitiser between the package's text and the page, and
//! this path has nothing but this function.

use serde::{Deserialize, Serialize};

/// The links a metadata document named for a package.
///
/// Both fields optional and independently so: plenty of packages declare a
/// repository and no homepage, and the reverse is common for anything whose
/// docs site is the product.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataLinks {
    /// Where the source code lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The package's own site, when it has one that is not the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

impl MetadataLinks {
    /// Build from whatever the upstream document said, normalising both and
    /// dropping whatever does not survive.
    ///
    /// Returns `None` when nothing survived, so a client can write
    /// `"links": MetadataLinks::new(a, b)` and get `null` rather than an empty
    /// object for the overwhelmingly common case of a package that declares
    /// neither.
    pub fn new(repository: Option<&str>, homepage: Option<&str>) -> Option<Self> {
        let links = Self {
            repository: repository.and_then(normalize_url),
            homepage: homepage.and_then(normalize_url),
        };
        (links.repository.is_some() || links.homepage.is_some()).then_some(links)
    }

    /// Read the `links` key out of a `PackageMetadata::extra`.
    ///
    /// A malformed value is `None` rather than an error, for the reason
    /// [`crate::entities::MetadataReadme::from_extra`] gives: `extra` is
    /// best-effort, and a client that wrote something unexpected should cost a
    /// missing link, not a failed resolve on a package manager's request path.
    ///
    /// **Re-normalised on the way out.** The value may have been written by an
    /// older build, or come from a metadata cache entry that predates a fix
    /// here, and the guarantee this module exists to make is about what reaches
    /// the page — not about what was true when the row was written.
    pub fn from_extra(extra: &serde_json::Value) -> Option<Self> {
        let raw: Self = serde_json::from_value(extra.get("links")?.clone()).ok()?;
        Self::new(raw.repository.as_deref(), raw.homepage.as_deref())
    }
}

/// A URL safe to put in an `href`, or `None`.
///
/// Package ecosystems spell a repository half a dozen ways and most of them are
/// not something a browser can open: npm writes `git+https://…​.git` and the
/// `github:user/repo` shorthand, Cargo and OpenVSX write a plain URL that often
/// keeps its `.git`, and anything git-native may arrive as `git://` or
/// `git@host:path`. All of those name a page that exists; none of them is a link.
///
/// The rewrites are deliberately narrow and the final check is an **allow-list of
/// two schemes**, not a blocklist. `javascript:`, `data:` and `vbscript:` are the
/// ones that matter, but enumerating them is the wrong shape — a scheme nobody
/// thought of is exactly the one that gets through a blocklist, and this string
/// reaches an authenticated console page with no sanitiser behind it.
/// `ssh://git@host/path` and the scp-like `git@host:path` → `https://host/path`.
///
/// Both name a browsable page on every forge anyone actually uses. Split out of
/// [`normalize_url`] because telling the scp-like form apart from
/// `https://user@host/x` — which must be left exactly as it is — takes three
/// guards, and reading them next to the one-line rewrites obscured both.
///
/// `None` means "not one of these forms", not "not a link": the caller keeps
/// what it had and the allow-list at the end of [`normalize_url`] still decides.
fn rewrite_ssh_form(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("ssh://") {
        let host_and_path = rest.split_once('@').map_or(rest, |(_, r)| r);
        return Some(format!("https://{host_and_path}"));
    }
    let (prefix, rest) = url.split_once('@')?;
    // Only when there is no scheme at all, or `https://user@host/x` would be
    // mangled. A scp-like address has no `//` before the `@`.
    if prefix.contains("://") || prefix.contains('/') || !rest.contains(':') {
        return None;
    }
    Some(format!("https://{}", rest.replacen(':', "/", 1)))
}

/// `github:user/repo` → `https://github.com/user/repo`, npm's shorthand.
///
/// Deliberately only the forges whose shorthand is unambiguous — `user/repo` on
/// its own is not a URL, and guessing which forge it means would be inventing a
/// destination.
fn rewrite_forge_shorthand(url: &str) -> Option<String> {
    for (shorthand, host) in [
        ("github:", "github.com"),
        ("gitlab:", "gitlab.com"),
        ("bitbucket:", "bitbucket.org"),
    ] {
        if let Some(rest) = url.strip_prefix(shorthand) {
            return Some(format!("https://{host}/{rest}"));
        }
    }
    None
}

pub fn normalize_url(raw: &str) -> Option<String> {
    let mut url = raw.trim().to_owned();
    if url.is_empty() {
        return None;
    }

    // `git+https://…` → `https://…`. npm's canonical spelling.
    if let Some(rest) = url.strip_prefix("git+") {
        url = rest.to_owned();
    }
    // `git://host/path` → `https://host/path`. The git protocol is unauthenticated
    // and unencrypted, and no browser opens it, but the host and path are right.
    if let Some(rest) = url.strip_prefix("git://") {
        url = format!("https://{rest}");
    }
    // `ssh://git@host/path` and the scp-like `git@host:path`.
    if let Some(rewritten) = rewrite_ssh_form(&url) {
        url = rewritten;
    }
    // `github:user/repo`, npm's shorthand.
    if let Some(rewritten) = rewrite_forge_shorthand(&url) {
        url = rewritten;
    }
    // A trailing `.git` is how the clone URL is spelled, not the page.
    if let Some(rest) = url.strip_suffix(".git") {
        url = rest.to_owned();
    }

    // The allow-list, applied to the *result*: every rewrite above had its
    // chance, and whatever did not become http(s) by now is not a link.
    // Case-insensitive, because `JavaScript:` is the same scheme as
    // `javascript:` and a browser knows it even if a string comparison does not.
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    // A scheme and nothing else is not a destination.
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or_default();
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spellings_ecosystems_actually_use_all_become_pages() {
        for (raw, want) in [
            (
                "https://github.com/gamunu/vscode-yarn.git",
                "https://github.com/gamunu/vscode-yarn",
            ),
            ("git+https://github.com/o/r.git", "https://github.com/o/r"),
            ("git://github.com/o/r.git", "https://github.com/o/r"),
            ("ssh://git@github.com/o/r.git", "https://github.com/o/r"),
            ("git@github.com:o/r.git", "https://github.com/o/r"),
            ("github:o/r", "https://github.com/o/r"),
            ("gitlab:o/r", "https://gitlab.com/o/r"),
            ("bitbucket:o/r", "https://bitbucket.org/o/r"),
            ("  https://example.com/x  ", "https://example.com/x"),
        ] {
            assert_eq!(normalize_url(raw).as_deref(), Some(want), "{raw}");
        }
    }

    /// The half that matters: this string becomes an `href` on an authenticated
    /// console page, and nothing sanitises it afterwards.
    #[test]
    fn nothing_that_is_not_http_becomes_a_link() {
        for raw in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "  javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "file:///etc/passwd",
            "mailto:x@example.com",
            "ftp://example.com/x",
            "git+javascript:alert(1)",
            "user/repo",
            "",
            "   ",
            "https://",
            "http:///nohost",
        ] {
            assert_eq!(normalize_url(raw), None, "{raw} must not become a link");
        }
    }

    /// A `@` inside a perfectly ordinary URL must not trigger the scp-like
    /// rewrite and mangle the host.
    #[test]
    fn a_userinfo_url_is_not_mistaken_for_an_scp_address() {
        assert_eq!(
            normalize_url("https://user@github.com/o/r").as_deref(),
            Some("https://user@github.com/o/r")
        );
    }

    #[test]
    fn new_is_none_when_the_package_declared_nothing_usable() {
        assert_eq!(MetadataLinks::new(None, None), None);
        assert_eq!(MetadataLinks::new(Some("javascript:x"), Some("")), None);
    }

    #[test]
    fn new_keeps_whichever_half_survived() {
        let links = MetadataLinks::new(Some("git+https://github.com/o/r.git"), Some("nonsense"))
            .expect("the repository survived");
        assert_eq!(links.repository.as_deref(), Some("https://github.com/o/r"));
        assert_eq!(links.homepage, None);
    }

    /// A cache entry written before a normalisation fix must not serve what that
    /// fix exists to refuse.
    #[test]
    fn from_extra_re_normalises_rather_than_trusting_the_stored_value() {
        let extra = serde_json::json!({
            "links": { "repository": "javascript:alert(1)", "homepage": "https://ok.example" }
        });
        let links = MetadataLinks::from_extra(&extra).expect("the homepage survived");
        assert_eq!(links.repository, None);
        assert_eq!(links.homepage.as_deref(), Some("https://ok.example"));
    }

    #[test]
    fn from_extra_is_none_for_absent_or_malformed_values() {
        assert_eq!(MetadataLinks::from_extra(&serde_json::json!({})), None);
        assert_eq!(
            MetadataLinks::from_extra(&serde_json::json!({ "links": "not an object" })),
            None
        );
    }
}
