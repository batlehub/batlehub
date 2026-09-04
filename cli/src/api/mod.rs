pub mod admin;
pub mod auth;
pub mod authz;
pub mod ide;
pub mod mise_plan;
pub mod owner;
pub mod package;
pub mod publish;
pub mod registry;
pub mod security;
pub mod setup;
pub mod suggest;
pub mod version;

use anyhow::{bail, Result};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};

/// What one seed fetch found.
#[derive(Debug, Clone)]
pub struct SeedFetch {
    pub status: u16,
    pub size: u64,
    /// Bare hex of the bytes served; empty when the fetch failed.
    pub sha256: String,
    /// `X-BatleHub-Verdict`, when the registry has a security profile.
    pub verdict: Option<String>,
    /// `X-BatleHub-Reason`, comma-separated; empty when there is none.
    pub reasons: String,
    /// The body of a failed response, for the line the operator reads.
    pub error: Option<String>,
}

/// One artifact, as the bundle export needs it: the bytes, the key the
/// server keeps them under, and what the security layer said.
#[derive(Debug, Clone)]
pub struct FetchedForBundle {
    pub bytes: Vec<u8>,
    /// `X-BatleHub-Storage-Key`. `None` from a server too old to send it,
    /// in which case the export falls back to the plan's derived key and
    /// says so.
    pub storage_key: Option<String>,
    /// The coordinate the server files those bytes under. Carried rather
    /// than parsed out of the key, which cannot be split unambiguously when
    /// the name contains a slash — and a coordinate guessed wrong files the
    /// verdict against a package nobody asks about.
    ///
    /// It is also what the export asks the verdict endpoint about: RFC 0018's
    /// headers are silent on an `allowed` artifact, and that is the case a
    /// bundle has to carry.
    pub package_name: Option<String>,
    pub version: Option<String>,
    /// `tag`, `branch` or `commit` on a forge coordinate; `None` elsewhere.
    pub ref_kind: Option<String>,
    pub resolved_commit: Option<String>,
    /// The ref as the client spelled it — `main`, `v2.60.0`. Not derivable
    /// from the coordinate on a commit-keyed archive, where the version has
    /// already become the SHA.
    pub requested_ref: Option<String>,
}

#[derive(Clone)]
pub struct BatleHubClient {
    inner: reqwest::Client,
    pub base_url: String,
    pub token: Option<String>,
}

impl BatleHubClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self> {
        // No total `.timeout()`: this client also streams artifact downloads,
        // which may legitimately be large. Bound connection setup and per-read
        // inactivity instead so an unresponsive server can't hang the CLI forever.
        let inner = reqwest::Client::builder()
            .user_agent("batlehub-cli/0.1")
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            inner,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.map(str::to_string),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {t}"))
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.inner.request(method, self.url(path))
    }

    /// Attach the auth header (if any) and send. Every HTTP-verb method funnels
    /// through here so the auth-attach step exists in exactly one place.
    async fn send(&self, req: RequestBuilder) -> Result<Response> {
        let mut req = req;
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        Ok(req.send().await?)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.send(self.request(Method::GET, path)).await?;
        expect_ok(resp).await
    }

    pub async fn get_with_params<T: DeserializeOwned, P: Serialize>(
        &self,
        path: &str,
        params: &P,
    ) -> Result<T> {
        let req = self.request(Method::GET, path).query(params);
        let resp = self.send(req).await?;
        expect_ok(resp).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let req = self.request(Method::POST, path).json(body);
        let resp = self.send(req).await?;
        expect_ok(resp).await
    }

    pub async fn post_no_body(&self, path: &str) -> Result<()> {
        let resp = self.send(self.request(Method::POST, path)).await?;
        expect_no_content(resp).await
    }

    /// POST with no body, expecting a JSON document back — the shape of the
    /// endpoints that *act* and then report what they did.
    pub async fn post_no_body_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self.send(self.request(Method::POST, path)).await?;
        expect_ok(resp).await
    }

    pub async fn post_void<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let req = self.request(Method::POST, path).json(body);
        let resp = self.send(req).await?;
        expect_no_content(resp).await
    }

    pub async fn put<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let req = self.request(Method::PUT, path).json(body);
        let resp = self.send(req).await?;
        expect_no_content(resp).await
    }

    /// PUT a raw binary body (e.g. a package upload) and expect a 2xx response.
    pub async fn put_bytes(&self, path: &str, body: Vec<u8>) -> Result<()> {
        let req = self
            .request(Method::PUT, path)
            .header("Content-Type", "application/octet-stream")
            .body(body);
        let resp = self.send(req).await?;
        expect_no_content(resp).await
    }

    /// POST a raw binary body (e.g. a package upload) and expect a 2xx response.
    pub async fn post_bytes(&self, path: &str, body: Vec<u8>) -> Result<()> {
        let req = self
            .request(Method::POST, path)
            .header("Content-Type", "application/octet-stream")
            .body(body);
        let resp = self.send(req).await?;
        expect_no_content(resp).await
    }

    /// PUT a JSON body and read a JSON response.
    ///
    /// Distinct from [`Self::put`], which expects `204`: the grants editor
    /// answers `200` with the expanded action set and any warnings, and those
    /// are the two things an operator most needs to see — `releases:*` names one
    /// verb and stores several, and a redundant grant is legal and inert.
    pub async fn put_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let req = self.request(Method::PUT, path).json(body);
        let resp = self.send(req).await?;
        expect_ok(resp).await
    }

    /// DELETE with a JSON body, reading a JSON response.
    ///
    /// A body on a DELETE because the coordinate is three fields — package,
    /// optional version, subject — and a subject spelling contains `:` and `*`,
    /// which are exactly the characters a path segment or query string would
    /// need escaping for.
    pub async fn delete_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let req = self.request(Method::DELETE, path).json(body);
        let resp = self.send(req).await?;
        expect_ok(resp).await
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        let resp = self.send(self.request(Method::DELETE, path)).await?;
        expect_no_content(resp).await
    }

    pub async fn delete_with_params_json<T: DeserializeOwned, P: Serialize>(
        &self,
        path: &str,
        params: &P,
    ) -> Result<T> {
        let req = self.request(Method::DELETE, path).query(params);
        let resp = self.send(req).await?;
        expect_ok(resp).await
    }

    /// GET a proxy path (relative to the server base URL) or an absolute URL and
    /// stream the response body into `dest`, returning the number of bytes written.
    ///
    /// The auth token is attached **only** when the target resolves to the
    /// configured server origin, so RBAC-protected registries on that server stay
    /// reachable. An arbitrary absolute URL (e.g. a redirect or a manifest-sourced
    /// download link to another host) is fetched **without** the token — sending
    /// the BatleHub credential to an unrelated host would be a leak.
    /// Fetch one proxy path, hashing as it goes, and report what the server
    /// said about it (RFC 0008 §4.3).
    ///
    /// One request does both halves of a seed: fetching *is* warming, and
    /// the digest is of exactly the bytes the proxy served — not of a
    /// second read that could differ. The verdict headers come back on the
    /// same response, so `--verify` needs no second call and no coordinate
    /// parsing: RFC 0018 already judged this request.
    pub async fn seed_fetch(&self, path: &str) -> Result<SeedFetch> {
        use futures::StreamExt;
        use sha2::{Digest, Sha256};

        let url = self.url(path);
        let mut req = self.inner.request(Method::GET, url.as_str());
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let verdict = header("X-BatleHub-Verdict");
        let reasons = header("X-BatleHub-Reason").unwrap_or_default();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Ok(SeedFetch {
                status,
                size: 0,
                sha256: String::new(),
                verdict,
                reasons,
                error: Some(body),
            });
        }
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
        }
        Ok(SeedFetch {
            status,
            size,
            sha256: hex::encode(hasher.finalize()),
            verdict,
            reasons,
            error: None,
        })
    }

    /// Fetch one proxy path into memory. For the bundle export, which needs
    /// the bytes to hash *and* to write, so streaming to a file and reading
    /// it back would be two passes over the same artifact.
    ///
    /// The response's own account of itself comes back with the bytes: the
    /// storage key it was served from, and the coordinate it files them
    /// under. Neither can be derived by a client: the key is a function of
    /// the route rather than of the URL, and the key cannot be split back
    /// into a coordinate when the name contains a slash.
    pub async fn fetch_for_bundle(&self, path: &str) -> Result<FetchedForBundle> {
        let url = self.url(path);
        let mut req = self.inner.request(Method::GET, url.as_str());
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {body}");
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let storage_key = header("X-BatleHub-Storage-Key");
        let package_name = header("X-BatleHub-Package");
        let version = header("X-BatleHub-Version");
        // RFC 0019's forge headers, when this was a forge coordinate: what
        // the client asked for and what it resolved to. The bundle carries
        // the pair so the disconnected instance can answer the ref without
        // resolving it, which it cannot do.
        let ref_kind = header("X-BatleHub-Ref-Kind");
        let resolved_commit = header("X-BatleHub-Resolved-Commit");
        let requested_ref = header("X-BatleHub-Ref-Requested");
        Ok(FetchedForBundle {
            bytes: resp.bytes().await?.to_vec(),
            storage_key,
            package_name,
            version,
            ref_kind,
            resolved_commit,
            requested_ref,
        })
    }

    pub async fn download_to<W: std::io::Write>(
        &self,
        path_or_url: &str,
        dest: &mut W,
    ) -> Result<u64> {
        use futures::StreamExt;

        let (url, is_own_origin) =
            if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
                let own = path_or_url == self.base_url
                    || path_or_url.starts_with(&format!("{}/", self.base_url));
                (path_or_url.to_string(), own)
            } else if let Some(rest) = path_or_url.strip_prefix('/') {
                (self.url(&format!("/{rest}")), true)
            } else {
                (self.url(&format!("/{path_or_url}")), true)
            };

        let mut req = self.inner.request(Method::GET, url.as_str());
        if is_own_origin {
            if let Some(auth) = self.auth_header() {
                req = req.header("Authorization", auth);
            }
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {body}");
        }

        let mut total: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            dest.write_all(&chunk)?;
            total += chunk.len() as u64;
        }
        Ok(total)
    }

    pub async fn put_multipart_void(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
    ) -> Result<()> {
        let req = self.request(Method::PUT, path).multipart(form);
        let resp = self.send(req).await?;
        expect_no_content(resp).await
    }

    pub async fn post_multipart_void(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
    ) -> Result<()> {
        let req = self.request(Method::POST, path).multipart(form);
        let resp = self.send(req).await?;
        expect_no_content(resp).await
    }
}

pub(crate) async fn expect_ok<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp.json::<T>().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        bail!("HTTP {status}: {body}")
    }
}

async fn expect_no_content(resp: reqwest::Response) -> Result<()> {
    let status = resp.status();
    if status.is_success() || status == StatusCode::NO_CONTENT {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        bail!("HTTP {status}: {body}")
    }
}
