//! The endpoints behind RFC 0040 §4.9's journeys: moderation, policy and
//! exemptions, the staged configuration, flags, `me`, the upstream audit and
//! namespace signing keys.
//!
//! Each call answers the server's JSON as a [`Value`]: the OpenAPI spec is the
//! contract (§6.5), so nothing here re-declares a DTO it already has. The
//! command prints the value, or picks the columns it shows.

use anyhow::Result;
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};

use super::auth::percent_encode as enc;
use super::{expect_ok, BatleHubClient};

/// `registry/name@version`, as three fields.
pub type Item = (String, String, String);

fn items(items: &[Item], reason: Option<&str>) -> Value {
    let rows: Vec<Value> = items
        .iter()
        .map(|(registry, name, version)| {
            let mut row = json!({ "registry": registry, "name": name, "version": version });
            if let Some(r) = reason {
                row["reason"] = json!(r);
            }
            row
        })
        .collect();
    json!({ "items": rows })
}

fn policy_path(registry: &str, package: &str, version: Option<&str>) -> String {
    match version {
        Some(v) => format!(
            "/api/v1/admin/registries/{}/policy/version/{}/{}",
            enc(registry),
            enc(package),
            enc(v)
        ),
        None => format!(
            "/api/v1/admin/registries/{}/policy/package/{}",
            enc(registry),
            enc(package)
        ),
    }
}

#[derive(Serialize)]
pub struct DownloadsQuery {
    pub limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
}

#[derive(Serialize)]
pub struct DisappearedQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub page: u64,
    pub per_page: u64,
}

/// `sha256=<hex>` of `data` under `secret`: `X-Hub-Signature-256`, as the
/// server's `verify_inbound_hmac` reads it.
pub fn hub_signature(secret: &str, data: &[u8]) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(data);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

impl BatleHubClient {
    // ── moderation ───────────────────────────────────────────────────────────

    pub async fn bulk_block(&self, versions: &[Item], reason: &str) -> Result<Value> {
        self.post(
            "/api/v1/admin/packages/bulk-block",
            &items(versions, Some(reason)),
        )
        .await
    }

    pub async fn bulk_unblock(&self, versions: &[Item]) -> Result<Value> {
        self.post(
            "/api/v1/admin/packages/bulk-unblock",
            &items(versions, None),
        )
        .await
    }

    // ── policy and exemptions ────────────────────────────────────────────────

    pub async fn policy_show(&self, r: &str, p: &str, v: Option<&str>) -> Result<Value> {
        self.get(&policy_path(r, p, v)).await
    }

    pub async fn policy_set(&self, r: &str, p: &str, v: Option<&str>, body: &Value) -> Result<()> {
        self.put(&policy_path(r, p, v), body).await
    }

    pub async fn policy_delete(&self, r: &str, p: &str, v: Option<&str>) -> Result<()> {
        self.delete(&policy_path(r, p, v)).await
    }

    pub async fn exemption_set(
        &self,
        (r, p, v): &Item,
        gate: &str,
        until: &str,
        reason: &str,
    ) -> Result<Value> {
        let path = format!("{}/rules/{}", policy_path(r, p, Some(v)), enc(gate));
        self.put_json(&path, &json!({ "exempt_until": until, "reason": reason }))
            .await
    }

    pub async fn exemption_delete(&self, (r, p, v): &Item, gate: &str) -> Result<()> {
        self.delete(&format!(
            "{}/rules/{}",
            policy_path(r, p, Some(v)),
            enc(gate)
        ))
        .await
    }

    pub async fn exemptions(&self, registry: &str) -> Result<Value> {
        self.get(&format!(
            "/api/v1/admin/registries/{}/exemptions",
            enc(registry)
        ))
        .await
    }

    // ── the staged configuration ─────────────────────────────────────────────

    pub async fn config_pending(&self) -> Result<Value> {
        self.get("/api/v1/admin/config/pending").await
    }

    pub async fn config_apply(&self) -> Result<Value> {
        self.post_no_body_json("/api/v1/admin/config/pending/apply")
            .await
    }

    pub async fn config_discard(&self) -> Result<()> {
        self.delete("/api/v1/admin/config/pending").await
    }

    pub async fn config_warnings(&self) -> Result<Value> {
        self.get("/api/v1/admin/config/warnings").await
    }

    // ── flags (RFC 0002): signed by the source's secret, not the caller ──────

    pub async fn flag_push(&self, source: &str, secret: &str, body: Vec<u8>) -> Result<Value> {
        let signature = hub_signature(secret, &body);
        let req = self
            .request(Method::POST, &format!("/api/v1/flags/{}", enc(source)))
            .header("Content-Type", "application/json")
            .header("X-Hub-Signature-256", signature)
            .body(body);
        expect_ok(req.send().await?).await
    }

    pub async fn flag_revoke(
        &self,
        source: &str,
        external_id: &str,
        secret: &str,
    ) -> Result<Value> {
        let path = format!("/api/v1/flags/{}/{}", enc(source), enc(external_id));
        // The server signs the canonical `DELETE\n<path>` as it routes it,
        // decoded (`handlers/flags.rs::revoke_canonical`).
        let canonical = format!("DELETE\n/api/v1/flags/{source}/{external_id}");
        let req = self.request(Method::DELETE, &path).header(
            "X-Hub-Signature-256",
            hub_signature(secret, canonical.as_bytes()),
        );
        expect_ok(req.send().await?).await
    }

    // ── me ───────────────────────────────────────────────────────────────────

    pub async fn me_show(&self) -> Result<Value> {
        self.get("/api/v1/me").await
    }

    pub async fn me_quota(&self) -> Result<Value> {
        self.get("/api/v1/me/quota").await
    }

    pub async fn me_downloads(&self, query: &DownloadsQuery) -> Result<Value> {
        self.get_with_params("/api/v1/me/downloads", query).await
    }

    pub async fn me_advisories(&self) -> Result<Value> {
        self.get("/api/v1/me/advisories").await
    }

    pub async fn me_namespaces(&self) -> Result<Value> {
        self.get("/api/v1/me/namespaces").await
    }

    // ── the upstream audit (RFC 0014) ────────────────────────────────────────

    pub async fn upstream_status(&self, registry: &str, name: &str) -> Result<Value> {
        self.get(&format!(
            "/api/v1/admin/upstream/status/{}/{}",
            enc(registry),
            enc(name)
        ))
        .await
    }

    pub async fn upstream_recheck(
        &self,
        registry: &str,
        name: &str,
        version: Option<&str>,
    ) -> Result<Value> {
        let body = json!({ "registry": registry, "package_name": name, "version": version });
        self.post("/api/v1/admin/upstream/recheck", &body).await
    }

    pub async fn upstream_disappeared(&self, query: &DisappearedQuery) -> Result<Value> {
        self.get_with_params("/api/v1/admin/upstream/disappeared", query)
            .await
    }

    // ── namespace signing keys ───────────────────────────────────────────────

    fn signing_path(registry: &str, namespace: &str) -> String {
        format!(
            "/api/v1/admin/registries/{}/signing-keys/{}",
            enc(registry),
            enc(namespace)
        )
    }

    pub async fn signing_keys(&self, registry: &str, namespace: &str) -> Result<Value> {
        self.get(&Self::signing_path(registry, namespace)).await
    }

    pub async fn signing_key_set(
        &self,
        registry: &str,
        namespace: &str,
        key: &Value,
    ) -> Result<()> {
        self.put(&Self::signing_path(registry, namespace), key)
            .await
    }

    pub async fn signing_key_delete(
        &self,
        registry: &str,
        namespace: &str,
        key_id: &str,
    ) -> Result<()> {
        self.delete(&format!(
            "{}/{}",
            Self::signing_path(registry, namespace),
            enc(key_id)
        ))
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_signature_matches_the_servers_known_vector() {
        // The vector `notification/channels.rs` holds its own HMAC to.
        assert_eq!(
            hub_signature("key", b"The quick brown fox jumps over the lazy dog"),
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }
}
