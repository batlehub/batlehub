use std::collections::HashMap;
use std::sync::Arc;

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use base64::Engine as _;
use tokio::task::spawn_blocking;

use batlehub_core::{
    entities::{Identity, Role},
    error::CoreError,
    ports::{AuthProvider, RawAuthRequest},
};

#[derive(Clone)]
struct TokenRecord {
    user_id: Option<String>,
    role: Role,
    groups: Vec<String>,
}

fn is_argon2_hash(s: &str) -> bool {
    s.starts_with("$argon2id$") || s.starts_with("$argon2i$") || s.starts_with("$argon2d$")
}

/// Constant-time byte comparison for secret values. Unlike `==`, this never
/// short-circuits on the first differing byte, so it doesn't leak how much of
/// a guessed token matches a real one through response timing. Lengths are
/// compared up front (a standard, accepted leak — see Go's
/// `subtle.ConstantTimeCompare` — since it reveals far less than a per-byte
/// timing oracle would).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Hash a plain-text token with Argon2id. Use the output as the `value` in
/// `[[auth.tokens]]` to avoid storing credentials in plain text.
pub fn hash_static_token(plain: &str) -> String {
    // argon2 0.6 draws the salt itself from the system RNG (the `getrandom`
    // feature); 0.5 took a caller-supplied `SaltString::generate(&mut OsRng)`.
    // Same source of randomness, one fewer thing for a caller to get wrong.
    Argon2::default()
        .hash_password(plain.as_bytes())
        .expect("argon2 hash")
        .to_string()
}

/// Authenticates requests via static Bearer tokens configured in `config.toml`.
///
/// Tokens may be stored as plain text **or** as Argon2 PHC strings (the output
/// of `batlehub hash-token`).  Plain-text tokens are checked with a
/// constant-time comparison against every configured entry (O(n) in the
/// number of static tokens, which is config-driven and expected to be small);
/// hashed tokens require a CPU-bound Argon2 verify for each configured hash
/// entry.
pub struct StaticTokenAuthProvider {
    /// Plain-text tokens, scanned in full on every request (see `constant_time_eq`).
    plain: HashMap<String, TokenRecord>,
    /// Argon2-hashed tokens → linear scan, verified off the async thread.
    hashed: Vec<(String, TokenRecord)>,
}

impl StaticTokenAuthProvider {
    pub fn new(entries: impl IntoIterator<Item = (String, Option<String>, Role)>) -> Self {
        let mut plain = HashMap::new();
        let mut hashed = Vec::new();
        for (value, user_id, role) in entries {
            Self::classify(
                &mut plain,
                &mut hashed,
                value,
                TokenRecord {
                    user_id,
                    role,
                    groups: vec![],
                },
            );
        }
        Self { plain, hashed }
    }

    /// Add entries that carry explicit group memberships (e.g. for testing OIDC group flows).
    pub fn with_group_entries(
        mut self,
        entries: impl IntoIterator<Item = (String, Option<String>, Role, Vec<String>)>,
    ) -> Self {
        for (value, user_id, role, groups) in entries {
            Self::classify(
                &mut self.plain,
                &mut self.hashed,
                value,
                TokenRecord {
                    user_id,
                    role,
                    groups,
                },
            );
        }
        self
    }

    /// Route a configured token into the plain or Argon2-hashed bucket, keyed
    /// by its own PHC-string prefix, matching how `authenticate` later scans them.
    fn classify(
        plain: &mut HashMap<String, TokenRecord>,
        hashed: &mut Vec<(String, TokenRecord)>,
        value: String,
        record: TokenRecord,
    ) {
        if is_argon2_hash(&value) {
            hashed.push((value, record));
        } else {
            plain.insert(value, record);
        }
    }
}

#[async_trait]
impl AuthProvider for StaticTokenAuthProvider {
    fn name(&self) -> &str {
        "static-token"
    }

    async fn authenticate(&self, req: &RawAuthRequest) -> Result<Option<Identity>, CoreError> {
        let token = if let Some(t) = req.bearer_token() {
            t.to_owned()
        } else if let Some(encoded) = req.headers.get("authorization").and_then(|value| {
            value
                .strip_prefix("Basic ")
                .or_else(|| value.strip_prefix("basic "))
        }) {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            let Some(decoded) = decoded else {
                return Ok(None);
            };
            // user:token — split at first `:` only, token may contain `:`
            decoded
                .split_once(':')
                .map(|x| x.1)
                .unwrap_or("")
                .to_owned()
        } else {
            return Ok(None);
        };

        // Plain-text tokens: compare against every entry in constant time,
        // never short-circuiting on the first match, so a request's timing
        // can't reveal how close a guessed token is to a real one or which
        // entry (if any) it matched.
        let mut plain_match: Option<&TokenRecord> = None;
        for (candidate, record) in self.plain.iter() {
            if constant_time_eq(token.as_bytes(), candidate.as_bytes()) {
                plain_match = Some(record);
            }
        }
        if let Some(record) = plain_match {
            return Ok(Some(to_identity(record)));
        }

        // Slow path: Argon2 verify against each hashed entry.
        if !self.hashed.is_empty() {
            // Clone the candidate list so it can be moved into spawn_blocking.
            let candidates: Arc<Vec<(String, TokenRecord)>> = Arc::new(self.hashed.clone());
            let result = spawn_blocking(move || {
                let argon2 = Argon2::default();
                for (hash_str, record) in candidates.iter() {
                    let Ok(parsed) = PasswordHash::new(hash_str) else {
                        continue;
                    };
                    if argon2.verify_password(token.as_bytes(), &parsed).is_ok() {
                        return Some(to_identity(record));
                    }
                }
                None
            })
            .await
            .map_err(|e| CoreError::Auth(format!("argon2 verify task: {e}")))?;

            return Ok(result);
        }

        Ok(None)
    }
}

fn to_identity(record: &TokenRecord) -> Identity {
    Identity {
        user_id: record.user_id.clone(),
        role: record.role.clone(),
        auth_provider: Some("static-token".to_owned()),
        groups: record.groups.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::{entities::Role, ports::RawAuthRequest};
    use std::collections::HashMap;

    fn req(auth: &str) -> RawAuthRequest {
        RawAuthRequest {
            headers: HashMap::from([("authorization".to_owned(), auth.to_owned())]),
            query_params: HashMap::new(),
            source_ip: None,
        }
    }

    fn provider() -> StaticTokenAuthProvider {
        StaticTokenAuthProvider::new([("secret".to_owned(), Some("alice".to_owned()), Role::Admin)])
    }

    #[tokio::test]
    async fn no_auth_header_returns_none() {
        let p = provider();
        let r = RawAuthRequest {
            headers: HashMap::new(),
            query_params: HashMap::new(),
            source_ip: None,
        };
        assert!(p.authenticate(&r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn lowercase_bearer_prefix_works() {
        let p = provider();
        let id = p
            .authenticate(&req("bearer secret"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id.role, Role::Admin);
    }

    #[tokio::test]
    async fn unknown_token_returns_none() {
        let p = provider();
        assert!(p
            .authenticate(&req("Bearer wrong"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn basic_auth_extracts_token_from_password_field() {
        // base64("user:secret") = "dXNlcjpzZWNyZXQ="
        let p = provider();
        let id = p
            .authenticate(&req("Basic dXNlcjpzZWNyZXQ="))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id.user_id.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn basic_auth_invalid_base64_returns_none() {
        let p = provider();
        assert!(p
            .authenticate(&req("Basic !!!not-base64!!!"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn basic_auth_no_colon_in_decoded_returns_none() {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode("secretonly");
        let p = provider();
        assert!(p
            .authenticate(&req(&format!("Basic {encoded}")))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn with_group_entries_populates_groups() {
        let p = StaticTokenAuthProvider::new([]).with_group_entries([(
            "tok".to_owned(),
            Some("bob".to_owned()),
            Role::User,
            vec!["team-a".to_owned()],
        )]);
        let id = p.authenticate(&req("Bearer tok")).await.unwrap().unwrap();
        assert_eq!(id.groups, vec!["team-a"]);
        assert_eq!(id.role, Role::User);
    }

    #[tokio::test]
    async fn unrecognised_scheme_returns_none() {
        let p = provider();
        assert!(p
            .authenticate(&req("Digest something"))
            .await
            .unwrap()
            .is_none());
    }

    // ── Argon2 hashed token tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn argon2_hashed_token_authenticates() {
        let hash = hash_static_token("my-secret");
        let p = StaticTokenAuthProvider::new([(hash, Some("carol".to_owned()), Role::User)]);
        let id = p
            .authenticate(&req("Bearer my-secret"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id.user_id.as_deref(), Some("carol"));
        assert_eq!(id.role, Role::User);
    }

    #[tokio::test]
    async fn argon2_hashed_token_wrong_value_returns_none() {
        let hash = hash_static_token("my-secret");
        let p = StaticTokenAuthProvider::new([(hash, Some("carol".to_owned()), Role::User)]);
        assert!(p
            .authenticate(&req("Bearer wrong"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn plain_and_hashed_coexist() {
        let hash = hash_static_token("hashed-tok");
        let p = StaticTokenAuthProvider::new([
            (
                "plain-tok".to_owned(),
                Some("alice".to_owned()),
                Role::Admin,
            ),
            (hash, Some("bob".to_owned()), Role::User),
        ]);
        let id_plain = p
            .authenticate(&req("Bearer plain-tok"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id_plain.user_id.as_deref(), Some("alice"));

        let id_hashed = p
            .authenticate(&req("Bearer hashed-tok"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id_hashed.user_id.as_deref(), Some("bob"));
    }

    #[test]
    fn hash_token_produces_argon2_phc_string() {
        let h = hash_static_token("sometoken");
        assert!(is_argon2_hash(&h), "expected argon2 PHC string, got: {h}");
    }

    // ── constant_time_eq ──────────────────────────────────────────────────────

    #[test]
    fn constant_time_eq_equal_values() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
    }

    #[test]
    fn constant_time_eq_different_values_same_length() {
        assert!(!constant_time_eq(b"secret-token", b"secret-tokeX"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"short", b"much-longer-value"));
    }

    #[test]
    fn constant_time_eq_empty_values() {
        assert!(constant_time_eq(b"", b""));
    }
}
