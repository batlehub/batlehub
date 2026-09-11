use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::ConfigFile;

use super::BatleHubClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeResponse {
    pub user_id: Option<String>,
    pub role: String,
    pub auth_provider: Option<String>,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenListItem {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// Absent on a server older than the PAT group snapshot (RFC 0011-bis
    /// §4.4), which is a server where every token carries none.
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTokenResponse {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub role: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateTokenRequest {
    pub name: String,
    pub expires_in_days: u64,
    pub role: String,
    /// Groups to snapshot onto the token, each one the caller already holds.
    /// Empty is the default and is what a token carried before this existed.
    pub groups: Vec<String>,
}

impl BatleHubClient {
    pub async fn whoami(&self) -> Result<MeResponse> {
        self.get("/api/v1/me").await
    }

    pub async fn list_tokens(&self) -> Result<Vec<TokenListItem>> {
        self.get("/api/v1/auth/tokens").await
    }

    pub async fn create_token(&self, req: CreateTokenRequest) -> Result<CreateTokenResponse> {
        self.post("/api/v1/auth/tokens", &req).await
    }

    pub async fn revoke_token(&self, id: Uuid) -> Result<()> {
        self.delete(&format!("/api/v1/auth/tokens/{id}")).await
    }

    /// List OIDC provider names that have browser SSO enabled.
    pub async fn list_oidc_providers(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Provider {
            name: String,
        }
        let providers: Vec<Provider> = self.get("/api/v1/auth/oidc/providers").await?;
        Ok(providers.into_iter().map(|p| p.name).collect())
    }

    /// Fetch the IdP authorization URL for browser-based OIDC login. Unlike
    /// every other method on this client, this deliberately does not attach the
    /// profile's bearer token — there is no session yet, this call *starts* one.
    pub async fn oidc_login_url(&self, csrf: &str, provider: Option<&str>) -> Result<String> {
        get_oidc_login_url(&self.base_url, csrf, provider).await
    }

    /// Exchange a refresh token for a new access token via the server's OIDC
    /// refresh proxy. Returns `(access_token, refresh_token, expires_in_seconds)`.
    /// Also deliberately unauthenticated — see `oidc_login_url`.
    pub async fn oidc_refresh(
        &self,
        refresh_token: &str,
        provider: Option<&str>,
    ) -> Result<(String, Option<String>, Option<u64>)> {
        oidc_refresh(&self.base_url, refresh_token, provider).await
    }
}

// ── OIDC helpers ───────────────────────────────────────────────────────────────

/// Fetch the IdP authorization URL by issuing a non-redirecting request to the
/// server's OIDC login endpoint and extracting the `Location` header.
pub async fn get_oidc_login_url(
    base_url: &str,
    csrf: &str,
    provider: Option<&str>,
) -> Result<String> {
    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let mut url = format!(
        "{base_url}/api/v1/auth/oidc/login?state={}",
        percent_encode(csrf)
    );
    if let Some(p) = provider {
        url.push_str(&format!("&provider={}", percent_encode(p)));
    }

    let resp = client.get(&url).send().await?;

    let status = resp.status();
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        anyhow::bail!("OIDC is not configured on this server");
    }
    if !status.is_redirection() {
        anyhow::bail!("OIDC login endpoint returned unexpected status {status}");
    }

    let location = resp
        .headers()
        .get("Location")
        .ok_or_else(|| {
            anyhow::anyhow!("Server did not return a redirect — OIDC may not be configured")
        })?
        .to_str()?
        .to_string();

    Ok(location)
}

/// Exchange a refresh token for a new access token via the server's OIDC refresh proxy.
/// Returns `(access_token, refresh_token, expires_in_seconds)`.
pub async fn oidc_refresh(
    base_url: &str,
    refresh_token: &str,
    provider: Option<&str>,
) -> Result<(String, Option<String>, Option<u64>)> {
    #[derive(Serialize)]
    struct Req<'a> {
        refresh_token: &'a str,
        provider: Option<&'a str>,
    }
    #[derive(Deserialize)]
    struct Resp {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    }

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let resp = client
        .post(format!("{base_url}/api/v1/auth/oidc/refresh"))
        .json(&Req {
            refresh_token,
            provider,
        })
        .send()
        .await?;

    let r: Resp = super::expect_ok(resp).await?;
    Ok((r.access_token, r.refresh_token, r.expires_in))
}

/// Resolve the effective bearer token for the active profile:
/// - If a K8s token path is configured, reads it fresh from disk.
/// - If the OIDC access token is about to expire and a refresh token is stored,
///   performs the refresh, updates the config, and returns the new token.
/// - Otherwise returns the stored token as-is.
pub async fn resolve_token(
    base_url: &str,
    profile_name: Option<&str>,
    cfg: &mut ConfigFile,
) -> Result<Option<String>> {
    // Extract values without holding a borrow on cfg
    let (k8s_path, should_refresh, stored_token, stored_refresh, stored_provider) = {
        let p = profile_name
            .and_then(|n| cfg.profiles.get(n))
            .unwrap_or(&cfg.default);
        (
            p.kubernetes_token_path.clone(),
            p.is_token_expiring_soon() && p.oidc_refresh_token.is_some(),
            p.token.clone(),
            p.oidc_refresh_token.clone(),
            p.oidc_provider.clone(),
        )
    };

    if let Some(path) = k8s_path {
        return Ok(match tokio::fs::read_to_string(&path).await {
            Ok(s) => Some(s.trim().to_string()),
            Err(e) => {
                eprintln!("Warning: failed to read kubernetes token file: {e}");
                None
            }
        });
    }

    if should_refresh {
        if let Some(refreshed) =
            try_refresh_token(base_url, stored_refresh, stored_provider, cfg, profile_name).await
        {
            return Ok(Some(refreshed));
        }
    }

    Ok(stored_token)
}

async fn try_refresh_token(
    base_url: &str,
    stored_refresh: Option<String>,
    stored_provider: Option<String>,
    cfg: &mut ConfigFile,
    profile_name: Option<&str>,
) -> Option<String> {
    let rt = stored_refresh?;
    match oidc_refresh(base_url, &rt, stored_provider.as_deref()).await {
        Ok((access_token, new_refresh, expires_in)) => {
            let profile = match profile_name {
                Some(n) => cfg.profiles.entry(n.to_string()).or_default(),
                None => &mut cfg.default,
            };
            profile.token = Some(access_token.clone());
            if let Some(nrt) = new_refresh {
                profile.oidc_refresh_token = Some(nrt);
            }
            if let Some(exp) = expires_in {
                profile.oidc_expires_at = Some(Utc::now().timestamp() + exp as i64);
            }
            cfg.save().ok();
            Some(access_token)
        }
        Err(e) => {
            eprintln!("Warning: OIDC token refresh failed: {e}");
            None
        }
    }
}

/// What the user pasted back after completing an OIDC login in their browser.
pub struct OidcPaste {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    /// The `oidc_state` echoed back by the server, when a full URL was pasted.
    ///
    /// `None` for a bare token, which carries no state to check — see
    /// `handle_auth_login`, which decides what to do about that.
    pub state: Option<String>,
    /// The name of the provider that actually signed the user in, echoed back
    /// as `oidc_provider`.
    ///
    /// Stored in the profile because `resolve_token` sends it with every refresh:
    /// without it the server falls back to `flows.first()`, so on a deployment
    /// with more than one provider the refresh token is presented to the wrong
    /// token endpoint and every session silently stops renewing.
    pub provider: Option<String>,
}

/// Parse a token value or full SPA redirect URL pasted by the user after OIDC login.
///
/// Reads the parameters wherever they appear in the pasted string. The server
/// puts them in the URL fragment rather than the query; both parse the same way
/// here, since a pasted URL is just a string and `#` only ever terminates a
/// value, never starts one that matters.
pub fn parse_oidc_paste(input: &str) -> OidcPaste {
    if input.contains("oidc_access_token=") {
        OidcPaste {
            access_token: extract_param(input, "oidc_access_token")
                .unwrap_or_else(|| input.to_string()),
            refresh_token: extract_param(input, "oidc_refresh_token"),
            expires_at: extract_param(input, "oidc_expires_in")
                .and_then(|s| s.parse::<i64>().ok())
                .map(|secs| Utc::now().timestamp() + secs),
            state: extract_param(input, "oidc_state"),
            provider: extract_param(input, "oidc_provider"),
        }
    } else {
        OidcPaste {
            access_token: input.to_string(),
            refresh_token: None,
            expires_at: None,
            state: None,
            provider: None,
        }
    }
}

fn extract_param(url: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let start = url.find(&needle)? + needle.len();
    let rest = &url[start..];
    let end = rest.find(['&', '#']).unwrap_or(rest.len());
    Some(url_decode(&rest[..end]))
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            decoded.push(b' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) else {
                decoded.push(bytes[i]);
                i += 1;
                continue;
            };
            let Ok(byte) = u8::from_str_radix(hex, 16) else {
                decoded.push(bytes[i]);
                i += 1;
                continue;
            };
            decoded.push(byte);
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Percent-encode one path or query segment.
///
/// `pub(crate)` rather than private: `package.rs` needs the same rule for a
/// scoped npm name (`@scope/pkg`), whose slash would otherwise split a path
/// segment, and a second copy of an encoder is how two of them come to disagree.
pub(crate) fn percent_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                c.to_string()
                    .bytes()
                    .flat_map(|b| format!("%{b:02X}").chars().collect::<Vec<_>>())
                    .collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_oidc_paste_bare_token() {
        let p = parse_oidc_paste("mytoken123");
        assert_eq!(p.access_token, "mytoken123");
        assert!(p.refresh_token.is_none());
        assert!(p.expires_at.is_none());
        assert!(p.state.is_none(), "a bare token carries no state to check");
    }

    #[test]
    fn parse_oidc_paste_full_url() {
        // The server delivers these in the fragment now; the parser reads the
        // pasted string wherever they sit, so both shapes are covered.
        let url = "http://app.example.com/callback#\
                   oidc_access_token=ACCESS&oidc_refresh_token=REFRESH&oidc_expires_in=3600\
                   &oidc_state=STATE";
        let p = parse_oidc_paste(url);
        assert_eq!(p.access_token, "ACCESS");
        assert_eq!(p.refresh_token.as_deref(), Some("REFRESH"));
        assert_eq!(p.state.as_deref(), Some("STATE"));
        let now = Utc::now().timestamp();
        let exp = p.expires_at.unwrap();
        assert!(
            exp > now + 3500 && exp <= now + 3601,
            "expires_at={exp} should be near now+3600"
        );
    }

    #[test]
    fn parse_oidc_paste_url_encoded_token() {
        let url = "http://app.example.com/#oidc_access_token=tok%2Fbar%3Dbaz";
        assert_eq!(parse_oidc_paste(url).access_token, "tok/bar=baz");
    }

    #[test]
    fn parse_oidc_paste_no_refresh_no_expiry() {
        let url = "http://app.example.com/#oidc_access_token=ONLYACCESS";
        let p = parse_oidc_paste(url);
        let (token, refresh, expires) = (p.access_token, p.refresh_token, p.expires_at);
        assert_eq!(token, "ONLYACCESS");
        assert!(refresh.is_none());
        assert!(expires.is_none());
    }

    #[test]
    fn url_decode_handles_plus_and_percent_escapes() {
        assert_eq!(url_decode("a+b%20c"), "a b c");
    }

    #[test]
    fn url_decode_passes_through_trailing_percent() {
        assert_eq!(url_decode("100%"), "100%");
    }

    #[test]
    fn percent_encode_escapes_reserved_chars() {
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
        assert_eq!(percent_encode("safe-_.~chars"), "safe-_.~chars");
    }

    // ── get_oidc_login_url ─────────────────────────────────────────────────

    #[tokio::test]
    async fn get_oidc_login_url_returns_location_header() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v1/auth/oidc/login")
            .match_query(mockito::Matcher::Any)
            .with_status(302)
            .with_header("Location", "https://idp.example.com/authorize?state=abc")
            .create_async()
            .await;

        let url = get_oidc_login_url(&server.url(), "abc", Some("github"))
            .await
            .unwrap();
        assert_eq!(url, "https://idp.example.com/authorize?state=abc");
    }

    #[tokio::test]
    async fn get_oidc_login_url_service_unavailable_errors() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v1/auth/oidc/login")
            .match_query(mockito::Matcher::Any)
            .with_status(503)
            .create_async()
            .await;

        let err = get_oidc_login_url(&server.url(), "abc", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not configured"));
    }

    #[tokio::test]
    async fn get_oidc_login_url_non_redirect_status_errors() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v1/auth/oidc/login")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .create_async()
            .await;

        let err = get_oidc_login_url(&server.url(), "abc", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unexpected status"));
    }

    #[tokio::test]
    async fn get_oidc_login_url_missing_location_header_errors() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v1/auth/oidc/login")
            .match_query(mockito::Matcher::Any)
            .with_status(302)
            .create_async()
            .await;

        let err = get_oidc_login_url(&server.url(), "abc", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not return a redirect"));
    }

    // ── oidc_refresh ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn oidc_refresh_success_returns_tokens() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/v1/auth/oidc/refresh")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "expires_in": 3600
                })
                .to_string(),
            )
            .create_async()
            .await;

        let (access, refresh, expires) = oidc_refresh(&server.url(), "old-refresh", Some("github"))
            .await
            .unwrap();
        assert_eq!(access, "new-access");
        assert_eq!(refresh.as_deref(), Some("new-refresh"));
        assert_eq!(expires, Some(3600));
    }

    #[tokio::test]
    async fn oidc_refresh_failure_includes_body() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/v1/auth/oidc/refresh")
            .with_status(400)
            .with_body("invalid_grant")
            .create_async()
            .await;

        let err = oidc_refresh(&server.url(), "bad-refresh", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid_grant"));
    }

    // ── list_oidc_providers ────────────────────────────────────────────────

    #[tokio::test]
    async fn list_oidc_providers_returns_names() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v1/auth/oidc/providers")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::json!([{"name": "google"}, {"name": "github"}]).to_string())
            .create_async()
            .await;

        let client = BatleHubClient::new(&server.url(), None).unwrap();
        let providers = client.list_oidc_providers().await.unwrap();
        assert_eq!(providers, vec!["google".to_string(), "github".to_string()]);
    }

    // ── resolve_token ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_token_returns_stored_token_when_not_expiring() {
        let mut cfg = ConfigFile::default();
        cfg.default.token = Some("stored-token".into());
        cfg.default.oidc_expires_at = Some(Utc::now().timestamp() + 3600);
        cfg.default.oidc_refresh_token = Some("refresh".into());

        let token = resolve_token("http://localhost", None, &mut cfg)
            .await
            .unwrap();
        assert_eq!(token.as_deref(), Some("stored-token"));
    }

    #[tokio::test]
    async fn resolve_token_reads_kubernetes_token_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("sa-token");
        std::fs::write(&token_file, "  k8s-token-value\n").unwrap();

        let mut cfg = ConfigFile::default();
        cfg.default.kubernetes_token_path = Some(token_file.to_str().unwrap().to_string());

        let token = resolve_token("http://localhost", None, &mut cfg)
            .await
            .unwrap();
        assert_eq!(token.as_deref(), Some("k8s-token-value"));
    }

    #[tokio::test]
    async fn resolve_token_returns_none_when_kubernetes_token_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("does-not-exist");

        let mut cfg = ConfigFile::default();
        cfg.default.kubernetes_token_path = Some(token_file.to_str().unwrap().to_string());

        // Must surface as "no token" (not an error, not a stale success value)
        // rather than silently returning whatever the previous `.ok()` swallow
        // happened to produce.
        let token = resolve_token("http://localhost", None, &mut cfg)
            .await
            .unwrap();
        assert_eq!(token, None);
    }

    #[tokio::test]
    async fn resolve_token_refresh_failure_falls_back_to_stored_token() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/v1/auth/oidc/refresh")
            .with_status(500)
            .with_body("server error")
            .create_async()
            .await;

        let mut cfg = ConfigFile::default();
        cfg.default.token = Some("stale-token".into());
        cfg.default.oidc_expires_at = Some(Utc::now().timestamp() - 10); // already expired
        cfg.default.oidc_refresh_token = Some("refresh-token".into());

        let token = resolve_token(&server.url(), None, &mut cfg).await.unwrap();
        assert_eq!(token.as_deref(), Some("stale-token"));
    }
}
