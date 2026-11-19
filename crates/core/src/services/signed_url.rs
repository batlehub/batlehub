//! Signed, expiring, single-coordinate download URLs (RFC 0012).
//!
//! Terraform authenticates the two JSON documents of a provider install and
//! then fetches the provider archive — and its `SHA256SUMS` and `.sig` — **with
//! no `Authorization` header**. That is not a misconfiguration: the client has
//! no mechanism to send one on those requests. Measured against Terraform 1.8.5
//! (RFC 0012 §11): every protocol document is authenticated, all nine artifact
//! fetches in the probe were not, including on the host authenticated one
//! request earlier.
//!
//! The consequence today is that a Terraform mirror needs
//! `anonymous = ["releases:read", "source:read"]`, which is per *registry* — so
//! opening the last step of one provider install opens every read on it.
//!
//! This module mints a signature **inside the document that was already
//! authenticated**, and verifies it on the credential-less follow-up. It
//! authenticates a request; it authorises nothing. The identity recovered from
//! a valid signature is handed to the same rule chain, quota and audit as an
//! identity recovered from a header — see RFC 0012 §6.6. In particular a
//! blocked version stays blocked for a URL minted before the block, because the
//! block is evaluated at redemption rather than at minting.
//!
//! ## Why HMAC and not Ed25519
//!
//! Asymmetric signing buys a verifier that does not hold the secret, and here
//! the verifier *is* the minter. It would cost a larger token and a slower
//! verify for a property nothing needs. HMAC-SHA256 over the `sha2` already in
//! the tree also stays clear of the `rsa` family that `deny.toml` bans
//! (RUSTSEC-2023-0071).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::entities::{Identity, Role};

type HmacSha256 = Hmac<Sha256>;

/// The query parameter a minted signature travels in.
pub const QUERY_PARAM: &str = "bh_sig";

/// Token format version, and the algorithm selector with it: a future move to a
/// different primitive is a new prefix rather than a negotiation.
const TOKEN_VERSION: u8 = 1;

/// Domain separator, so a MAC from this scheme can never be mistaken for one
/// produced by another part of the system over similar-looking bytes.
const DOMAIN: &str = "bh-signed-url:v1";

/// Backward clock skew tolerated at verification. A runner whose clock is a
/// minute behind the minter must not fail an install; forward skew is not
/// tolerated, because that direction only ever extends a credential's life.
const CLOCK_SKEW_SECS: i64 = 60;

/// Hard ceiling on `ttl_seconds`, so a misconfigured instance cannot mint a
/// month-long bearer credential (RFC 0012 §6.4).
pub const MAX_TTL_SECONDS: u64 = 3600;

/// Default TTL. Terraform follows the URL within milliseconds; the margin is
/// for a slow runner, not for a human.
pub const DEFAULT_TTL_SECONDS: u64 = 300;

/// Minimum accepted signing-secret length, in bytes.
pub const MIN_SECRET_BYTES: usize = 32;

/// The five path components a signature is bound to.
///
/// Built from the **request being verified**, never from the token's own copy
/// of them — that is what stops a signature for `random/5.40.0` being replayed
/// against `aws/6.0.0` by editing the path and leaving the query alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coordinate<'a> {
    /// HTTP method. In the MAC because
    /// `providers/{ns}/{type}/{ver}/artifact/{os}/{arch}` is both a `GET`
    /// download route and a `PUT` publish route, and a `GET` signature must not
    /// be presentable to the latter.
    pub method: &'a str,
    pub registry: &'a str,
    pub package: &'a str,
    pub version: &'a str,
    pub artifact: &'a str,
}

/// The token payload, as carried base64url-encoded in the middle segment.
///
/// The coordinate fields are duplicated here even though the MAC is computed
/// from the request's path. They are what lets verification tell "this token
/// was minted for a different coordinate" from "this token is forged" — two
/// failures that are cryptographically identical but mean very different things
/// to an operator reading a `403` (RFC 0012 §4.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Payload {
    v: u8,
    reg: String,
    pkg: String,
    ver: String,
    art: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sub: Option<String>,
    role: String,
    /// Present because `[registries.rbac.groups]` grants exist: a token that
    /// dropped them would silently downgrade a group-authorised caller to their
    /// role's permissions, and the install would fail at its last step having
    /// worked yesterday.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grp: Vec<String>,
    exp: i64,
}

/// Why a signature was not accepted.
///
/// Three distinguishable outcomes, because an operator debugging a
/// clock-skewed runner should not have to guess which one they hit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignedUrlError {
    #[error("malformed signed URL: {0}")]
    Malformed(String),
    #[error("signed URL uses unsupported version {0}")]
    UnsupportedVersion(u8),
    #[error("signed URL was minted for {minted_for}, presented at {presented_at}")]
    CoordinateMismatch {
        minted_for: String,
        presented_at: String,
    },
    #[error("signed URL expired at {expired_at} (now {now})")]
    Expired {
        expired_at: DateTime<Utc>,
        now: DateTime<Utc>,
    },
    #[error("signed URL signature does not verify")]
    BadSignature,
}

impl SignedUrlError {
    /// Stable slug for the API error body. One code for all of them, because a
    /// client cannot act differently on any of them; the distinction is in the
    /// message, for the human.
    pub fn code(&self) -> &'static str {
        "signed-url.invalid"
    }
}

/// Mints and verifies signed download URLs for one instance.
///
/// Stateless: verification needs only the configured secrets, so it holds
/// across replicas with no shared store.
#[derive(Clone)]
pub struct SignedUrlService {
    secret: Vec<u8>,
    /// Verified against but never minted with, so a secret can be rotated
    /// without a flag day.
    previous_secrets: Vec<Vec<u8>>,
    ttl_seconds: u64,
}

impl std::fmt::Debug for SignedUrlService {
    /// Hand-written so a `{:?}` of a config-carrying struct cannot put the
    /// signing secret in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedUrlService")
            .field("secret", &"<redacted>")
            .field("previous_secrets", &self.previous_secrets.len())
            .field("ttl_seconds", &self.ttl_seconds)
            .finish()
    }
}

impl SignedUrlService {
    /// Build a service. `ttl_seconds` is clamped to [`MAX_TTL_SECONDS`]; the
    /// config layer rejects an over-large value at startup, and this clamp is
    /// the second line so a programmatic caller cannot exceed the ceiling
    /// either.
    pub fn new(
        secret: impl Into<Vec<u8>>,
        previous_secrets: Vec<Vec<u8>>,
        ttl_seconds: u64,
    ) -> Self {
        Self {
            secret: secret.into(),
            previous_secrets,
            ttl_seconds: ttl_seconds.clamp(1, MAX_TTL_SECONDS),
        }
    }

    pub fn ttl_seconds(&self) -> u64 {
        self.ttl_seconds
    }

    /// Per-registry subkey: `HMAC(secret, "registry:" || name)`.
    ///
    /// A key recovered from one registry's blast radius — a log line, a memory
    /// dump of one worker — cannot mint for another, and the derivation costs
    /// one HMAC.
    fn subkey(secret: &[u8], registry: &str) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any length");
        mac.update(b"registry:");
        mac.update(registry.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    /// The canonical string the MAC covers (RFC 0012 §6.2).
    ///
    /// Coordinate fields come from the request; identity fields and `exp` come
    /// from the payload. A payload whose coordinate copy disagrees with the
    /// path therefore fails the MAC even if the mismatch check were removed.
    ///
    /// ## Why every field is length-prefixed
    ///
    /// The encoding has to be **injective**: exactly one input tuple may
    /// produce any given string. The first version of this function joined the
    /// fields with `\n` and the groups with `,`, and that is not injective —
    /// a value containing the delimiter shifts the boundary, so one MAC covers
    /// two different tuples.
    ///
    /// That was exploitable rather than merely untidy. `validate_path_safe`
    /// permits control characters, and actix percent-decodes path segments
    /// before extraction, so `%0A` in a path segment arrives here as a real
    /// newline. An attacker who could mint at a coordinate they controlled
    /// could publish under a name carrying the extra lines, keep the MAC
    /// bytes, and re-split the payload into a *different* coordinate with
    /// `role: "admin"` — the reconstruction was byte-identical, so it verified.
    /// Minting and redemption do not run the same rules (minting authorizes
    /// through `authorize_listing`, which is the RBAC rule alone; redemption
    /// runs the full chain), and every gate rule has a `bypass_roles` list
    /// operators fill with `admin`. The comma had the same shape one field
    /// over: a single group `"a,b"` re-split into two groups.
    ///
    /// So each field is written as `<byte-length>:<value>`, netstring-style,
    /// and the group list is preceded by its own count. Parsing is
    /// deterministic — read digits to the `:`, take exactly that many bytes —
    /// which is the definition of the property needed here, and no value can
    /// pose as a delimiter because no delimiter is being looked for.
    fn canonical(
        coord: &Coordinate<'_>,
        sub: Option<&str>,
        role: &str,
        groups: &[String],
        exp: i64,
    ) -> String {
        /// `<byte-length>:<value>`. Byte length, not character count — the MAC
        /// covers bytes, and a multi-byte value must not be able to claim a
        /// shorter span than it occupies.
        fn push_field(out: &mut String, value: &str) {
            out.push_str(&value.len().to_string());
            out.push(':');
            out.push_str(value);
        }

        let mut out = String::with_capacity(160);
        out.push_str(DOMAIN);
        out.push('\n');
        for field in [
            coord.method,
            coord.registry,
            coord.package,
            coord.version,
            coord.artifact,
        ] {
            push_field(&mut out, field);
        }
        // `sub` is an `Option`, and length-prefixing alone cannot separate
        // `None` from `Some("")` — both render as the empty string. An
        // anonymous token could then be edited to claim a zero-length user id.
        // No read path keys on that today, which is why this is a correction
        // rather than a fix, but "the MAC covers the identity" should be true
        // without a caveat.
        match sub {
            Some(value) => {
                push_field(&mut out, "1");
                push_field(&mut out, value);
            }
            None => {
                push_field(&mut out, "0");
                push_field(&mut out, "");
            }
        }
        push_field(&mut out, role);
        // The count first, so a group holding a separator cannot pose as two
        // groups — and so an empty list is distinct from a list holding one
        // empty string.
        push_field(&mut out, &groups.len().to_string());
        for group in groups {
            push_field(&mut out, group);
        }
        push_field(&mut out, &exp.to_string());
        out
    }

    fn mac(
        secret: &[u8],
        coord: &Coordinate<'_>,
        sub: Option<&str>,
        role: &str,
        groups: &[String],
        exp: i64,
    ) -> Vec<u8> {
        let key = Self::subkey(secret, coord.registry);
        let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts keys of any length");
        mac.update(Self::canonical(coord, sub, role, groups, exp).as_bytes());
        mac.finalize().into_bytes().to_vec()
    }

    /// Mint a signature for `coord` carrying `identity`, valid for `ttl_seconds`.
    ///
    /// The only legitimate caller is a handler that has *just* authenticated and
    /// authorised a request for this same coordinate: the signature records that
    /// verdict, it does not create one.
    pub fn mint(&self, coord: &Coordinate<'_>, identity: &Identity) -> String {
        self.mint_at(coord, identity, Utc::now())
    }

    /// [`Self::mint`] with an explicit clock, for tests and for any caller that
    /// needs determinism.
    pub fn mint_at(
        &self,
        coord: &Coordinate<'_>,
        identity: &Identity,
        now: DateTime<Utc>,
    ) -> String {
        let exp = now.timestamp() + self.ttl_seconds as i64;
        let role = identity.role.to_string();
        let groups = identity.groups.clone();
        // `<provider>:<id>`, so the bearer keeps the `user:` grants of the
        // signer and no other provider's user of the same id.
        let sub = identity
            .user_principal()
            .or_else(|| identity.user_id.clone());

        let payload = Payload {
            v: TOKEN_VERSION,
            reg: coord.registry.to_owned(),
            pkg: coord.package.to_owned(),
            ver: coord.version.to_owned(),
            art: coord.artifact.to_owned(),
            sub: sub.clone(),
            role: role.clone(),
            grp: groups.clone(),
            exp,
        };
        // `Payload` is a plain struct of owned scalars; serialisation cannot fail.
        let json = serde_json::to_vec(&payload).expect("Payload always serialises");
        let mac = Self::mac(&self.secret, coord, sub.as_deref(), &role, &groups, exp);

        format!(
            "{TOKEN_VERSION}.{}.{}",
            URL_SAFE_NO_PAD.encode(json),
            URL_SAFE_NO_PAD.encode(mac)
        )
    }

    /// Verify `token` against the coordinate of the request presenting it, and
    /// recover the identity it was minted for.
    pub fn verify(&self, token: &str, coord: &Coordinate<'_>) -> Result<Identity, SignedUrlError> {
        self.verify_at(token, coord, Utc::now())
    }

    /// [`Self::verify`] with an explicit clock.
    pub fn verify_at(
        &self,
        token: &str,
        coord: &Coordinate<'_>,
        now: DateTime<Utc>,
    ) -> Result<Identity, SignedUrlError> {
        let mut parts = token.splitn(3, '.');
        let (Some(version), Some(payload_b64), Some(mac_b64)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(SignedUrlError::Malformed(
                "expected three dot-separated segments".to_owned(),
            ));
        };

        let version: u8 = version
            .parse()
            .map_err(|_| SignedUrlError::Malformed("version prefix is not a number".to_owned()))?;
        if version != TOKEN_VERSION {
            return Err(SignedUrlError::UnsupportedVersion(version));
        }

        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| SignedUrlError::Malformed("payload is not base64url".to_owned()))?;
        let payload: Payload = serde_json::from_slice(&payload_bytes)
            .map_err(|_| SignedUrlError::Malformed("payload is not valid JSON".to_owned()))?;
        if payload.v != TOKEN_VERSION {
            return Err(SignedUrlError::UnsupportedVersion(payload.v));
        }
        let presented_mac = URL_SAFE_NO_PAD
            .decode(mac_b64)
            .map_err(|_| SignedUrlError::Malformed("signature is not base64url".to_owned()))?;

        // Before the MAC, and only to produce a better error: the payload is
        // public, so comparing its coordinate copy against the path leaks
        // nothing an attacker could not decode themselves.
        let minted_for = format!(
            "{}/{}/{}/{}",
            payload.reg, payload.pkg, payload.ver, payload.art
        );
        let presented_at = format!(
            "{}/{}/{}/{}",
            coord.registry, coord.package, coord.version, coord.artifact
        );
        if minted_for != presented_at {
            return Err(SignedUrlError::CoordinateMismatch {
                minted_for,
                presented_at,
            });
        }

        let sub = payload.sub.as_deref();
        // Current secret first, then each previous one, so a rotation verifies
        // both generations. `Mac::verify_slice` is constant-time.
        let verified = std::iter::once(&self.secret)
            .chain(self.previous_secrets.iter())
            .any(|secret| {
                let key = Self::subkey(secret, coord.registry);
                let Ok(mut mac) = HmacSha256::new_from_slice(&key) else {
                    return false;
                };
                mac.update(
                    Self::canonical(coord, sub, &payload.role, &payload.grp, payload.exp)
                        .as_bytes(),
                );
                mac.verify_slice(&presented_mac).is_ok()
            });
        if !verified {
            return Err(SignedUrlError::BadSignature);
        }

        // Expiry is checked only after the MAC, so the decision is never made
        // on a field an attacker could still be editing.
        if now.timestamp() > payload.exp + CLOCK_SKEW_SECS {
            return Err(SignedUrlError::Expired {
                expired_at: DateTime::from_timestamp(payload.exp, 0).unwrap_or(now),
                now,
            });
        }

        let role: Role = payload.role.parse().map_err(|_| {
            SignedUrlError::Malformed(format!("payload carries unknown role '{}'", payload.role))
        })?;

        // A `sub` minted as `<provider>:<id>` keeps its provider behind the
        // `signed-url:` prefix (what `Identity::principal_provider` strips), so
        // the bearer is never mistaken for an interactive session.
        let (user_id, auth_provider) = match payload.sub {
            Some(sub) => match sub.split_once(':') {
                Some((p, id)) => (
                    Some(id.to_owned()),
                    format!("{}{p}", Identity::SIGNED_URL_PROVIDER_PREFIX),
                ),
                None => (Some(sub), "signed-url".to_owned()),
            },
            None => (None, "signed-url".to_owned()),
        };
        Ok(Identity {
            user_id,
            role,
            auth_provider: Some(auth_provider),
            groups: payload.grp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const OTHER: &[u8] = b"fedcba9876543210fedcba9876543210";

    fn svc() -> SignedUrlService {
        SignedUrlService::new(SECRET, vec![], DEFAULT_TTL_SECONDS)
    }

    fn coord() -> Coordinate<'static> {
        Coordinate {
            method: "GET",
            registry: "tf",
            package: "providers/hashicorp/random",
            version: "5.40.0",
            artifact: "linux/amd64",
        }
    }

    fn alice() -> Identity {
        Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: Some("oidc".to_owned()),
            groups: vec!["platform".to_owned()],
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_767_225_600, 0).unwrap()
    }

    // ── Round trip ───────────────────────────────────────────────────────────

    #[test]
    fn mint_then_verify_recovers_the_identity() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let got = s.verify_at(&token, &coord(), now()).unwrap();

        assert_eq!(got.user_id.as_deref(), Some("alice"));
        assert_eq!(got.role, Role::User);
        assert_eq!(got.groups, vec!["platform".to_owned()]);
    }

    #[test]
    fn recovered_identity_is_labelled_as_signed_url() {
        // The auth provider is labelled rather than carried: what the original
        // credential was is not what authenticated *this* request. The signer's
        // provider rides behind the prefix, so `user:` grants still scope to it.
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let got = s.verify_at(&token, &coord(), now()).unwrap();
        assert_eq!(got.auth_provider.as_deref(), Some("signed-url:oidc"));
        assert_eq!(got.user_principal().as_deref(), Some("oidc:alice"));
    }

    #[test]
    fn groups_survive_the_round_trip() {
        // RFC 0012 §6.1: dropping groups would silently downgrade a
        // group-authorised caller to their role's permissions.
        let s = svc();
        let mut id = alice();
        id.groups = vec!["platform".to_owned(), "sre".to_owned()];
        let token = s.mint_at(&coord(), &id, now());
        let got = s.verify_at(&token, &coord(), now()).unwrap();
        assert_eq!(got.groups, vec!["platform".to_owned(), "sre".to_owned()]);
    }

    #[test]
    fn anonymous_identity_round_trips() {
        let s = svc();
        let token = s.mint_at(&coord(), &Identity::anonymous(), now());
        let got = s.verify_at(&token, &coord(), now()).unwrap();
        assert!(got.user_id.is_none());
        assert_eq!(got.role, Role::Anonymous);
        assert!(got.groups.is_empty());
    }

    #[test]
    fn every_role_round_trips() {
        let s = svc();
        for role in [Role::Anonymous, Role::User, Role::Admin] {
            let id = Identity {
                user_id: Some("u".to_owned()),
                role: role.clone(),
                auth_provider: None,
                groups: vec![],
            };
            let token = s.mint_at(&coord(), &id, now());
            assert_eq!(s.verify_at(&token, &coord(), now()).unwrap().role, role);
        }
    }

    // ── Tampering ────────────────────────────────────────────────────────────

    #[test]
    fn tampered_signature_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let (head, mac) = token.rsplit_once('.').unwrap();
        let mut bytes = URL_SAFE_NO_PAD.decode(mac).unwrap();
        bytes[0] ^= 0xff;
        let forged = format!("{head}.{}", URL_SAFE_NO_PAD.encode(bytes));

        assert_eq!(
            s.verify_at(&forged, &coord(), now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    #[test]
    fn escalating_role_in_the_payload_is_rejected() {
        // The MAC covers subject, role and groups, so editing any of them to
        // widen the caller invalidates the token.
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let mut parts = token.split('.');
        let (v, payload_b64, mac) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let mut payload: Payload =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).unwrap()).unwrap();
        payload.role = "admin".to_owned();
        let forged = format!(
            "{v}.{}.{mac}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );

        assert_eq!(
            s.verify_at(&forged, &coord(), now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    #[test]
    fn extending_expiry_in_the_payload_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let mut parts = token.split('.');
        let (v, payload_b64, mac) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let mut payload: Payload =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).unwrap()).unwrap();
        payload.exp += 86_400;
        let forged = format!(
            "{v}.{}.{mac}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );

        assert_eq!(
            s.verify_at(&forged, &coord(), now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    // ── Replay at another coordinate ─────────────────────────────────────────

    #[test]
    fn replay_against_another_package_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let elsewhere = Coordinate {
            package: "providers/hashicorp/aws",
            ..coord()
        };
        assert!(matches!(
            s.verify_at(&token, &elsewhere, now()),
            Err(SignedUrlError::CoordinateMismatch { .. })
        ));
    }

    #[test]
    fn replay_against_another_version_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let elsewhere = Coordinate {
            version: "6.0.0",
            ..coord()
        };
        assert!(matches!(
            s.verify_at(&token, &elsewhere, now()),
            Err(SignedUrlError::CoordinateMismatch { .. })
        ));
    }

    #[test]
    fn replay_against_another_platform_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let elsewhere = Coordinate {
            artifact: "darwin/arm64",
            ..coord()
        };
        assert!(matches!(
            s.verify_at(&token, &elsewhere, now()),
            Err(SignedUrlError::CoordinateMismatch { .. })
        ));
    }

    #[test]
    fn replay_against_another_registry_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let elsewhere = Coordinate {
            registry: "tf2",
            ..coord()
        };
        assert!(matches!(
            s.verify_at(&token, &elsewhere, now()),
            Err(SignedUrlError::CoordinateMismatch { .. })
        ));
    }

    #[test]
    fn a_get_signature_is_not_valid_for_put() {
        // RFC 0012 §6.2: the download and publish routes share a path shape, so
        // the method is in the MAC. It is *not* in the payload, so this fails
        // the signature check rather than the coordinate check — which is the
        // distinction this test exists to pin.
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let as_put = Coordinate {
            method: "PUT",
            ..coord()
        };
        assert_eq!(
            s.verify_at(&token, &as_put, now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    #[test]
    fn coordinate_mismatch_names_both_coordinates() {
        // §4.2: the operator must not have to guess which failure they hit.
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let elsewhere = Coordinate {
            version: "6.0.0",
            ..coord()
        };
        let err = s.verify_at(&token, &elsewhere, now()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("5.40.0"), "{msg}");
        assert!(msg.contains("6.0.0"), "{msg}");
    }

    // ── Expiry ───────────────────────────────────────────────────────────────

    #[test]
    fn expired_token_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let later = now() + chrono::Duration::seconds(DEFAULT_TTL_SECONDS as i64 + 61);
        assert!(matches!(
            s.verify_at(&token, &coord(), later),
            Err(SignedUrlError::Expired { .. })
        ));
    }

    #[test]
    fn token_just_inside_the_clock_skew_allowance_is_accepted() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let later = now() + chrono::Duration::seconds(DEFAULT_TTL_SECONDS as i64 + 59);
        assert!(s.verify_at(&token, &coord(), later).is_ok());
    }

    #[test]
    fn token_just_outside_the_clock_skew_allowance_is_rejected() {
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let later = now() + chrono::Duration::seconds(DEFAULT_TTL_SECONDS as i64 + 61);
        assert!(matches!(
            s.verify_at(&token, &coord(), later),
            Err(SignedUrlError::Expired { .. })
        ));
    }

    #[test]
    fn a_clock_running_early_does_not_accept_a_dead_token() {
        // Forward skew buys an attacker a longer-lived credential, so none is
        // allowed: at exactly exp + skew + 1 the token is dead.
        let s = svc();
        let token = s.mint_at(&coord(), &alice(), now());
        let boundary =
            now() + chrono::Duration::seconds(DEFAULT_TTL_SECONDS as i64 + CLOCK_SKEW_SECS);
        assert!(s.verify_at(&token, &coord(), boundary).is_ok());
        assert!(matches!(
            s.verify_at(&token, &coord(), boundary + chrono::Duration::seconds(1)),
            Err(SignedUrlError::Expired { .. })
        ));
    }

    // ── Keys, subkeys and rotation ───────────────────────────────────────────

    #[test]
    fn a_token_from_an_unrelated_secret_is_rejected() {
        let minted = SignedUrlService::new(OTHER, vec![], DEFAULT_TTL_SECONDS).mint_at(
            &coord(),
            &alice(),
            now(),
        );
        assert_eq!(
            svc().verify_at(&minted, &coord(), now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    #[test]
    fn a_previous_secret_still_verifies_after_rotation() {
        let old = SignedUrlService::new(OTHER, vec![], DEFAULT_TTL_SECONDS);
        let token = old.mint_at(&coord(), &alice(), now());

        let rotated = SignedUrlService::new(SECRET, vec![OTHER.to_vec()], DEFAULT_TTL_SECONDS);
        assert!(rotated.verify_at(&token, &coord(), now()).is_ok());
    }

    #[test]
    fn rotation_mints_only_with_the_current_secret() {
        let rotated = SignedUrlService::new(SECRET, vec![OTHER.to_vec()], DEFAULT_TTL_SECONDS);
        let token = rotated.mint_at(&coord(), &alice(), now());
        // The old-secret-only service must not accept what the new one minted.
        let old_only = SignedUrlService::new(OTHER, vec![], DEFAULT_TTL_SECONDS);
        assert_eq!(
            old_only.verify_at(&token, &coord(), now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    #[test]
    fn subkeys_differ_per_registry() {
        // The blast-radius argument in §6.3: the same secret must not produce
        // the same key for two registries.
        let a = SignedUrlService::subkey(SECRET, "tf");
        let b = SignedUrlService::subkey(SECRET, "tf2");
        assert_ne!(a, b);
        assert_eq!(a, SignedUrlService::subkey(SECRET, "tf"));
    }

    #[test]
    fn subkey_derivation_is_not_a_prefix_collision() {
        // "registry:" || name means ("a", "bc") and ("ab", "c") must not collide.
        assert_ne!(
            SignedUrlService::subkey(SECRET, "a"),
            SignedUrlService::subkey(SECRET, "ab")
        );
    }

    // ── Malformed input ──────────────────────────────────────────────────────

    #[test]
    fn malformed_tokens_are_rejected_without_panicking() {
        let s = svc();
        for bad in [
            "",
            "1",
            "1.",
            "1.abc",
            "not-a-version.abc.def",
            "1.!!!not-base64!!!.abc",
            "1.YWJj.!!!",
            "1.YWJj.YWJj", // valid base64, not valid JSON
        ] {
            let got = s.verify_at(bad, &coord(), now());
            assert!(got.is_err(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn a_future_token_version_is_reported_as_unsupported() {
        let s = svc();
        assert_eq!(
            s.verify_at("2.YWJj.YWJj", &coord(), now()).unwrap_err(),
            SignedUrlError::UnsupportedVersion(2)
        );
    }

    #[test]
    fn a_validly_signed_payload_with_an_unknown_role_is_malformed() {
        // Forged through the real MAC, so this exercises the parse *after*
        // verification rather than the signature path.
        let exp = now().timestamp() + 300;
        let payload = Payload {
            v: TOKEN_VERSION,
            reg: "tf".to_owned(),
            pkg: "providers/hashicorp/random".to_owned(),
            ver: "5.40.0".to_owned(),
            art: "linux/amd64".to_owned(),
            sub: Some("alice".to_owned()),
            role: "wizard".to_owned(),
            grp: vec![],
            exp,
        };
        let mac = SignedUrlService::mac(SECRET, &coord(), Some("alice"), "wizard", &[], exp);
        let token = format!(
            "{TOKEN_VERSION}.{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap()),
            URL_SAFE_NO_PAD.encode(mac)
        );

        assert!(matches!(
            svc().verify_at(&token, &coord(), now()).unwrap_err(),
            SignedUrlError::Malformed(_)
        ));
    }

    // ── TTL handling ─────────────────────────────────────────────────────────

    #[test]
    fn ttl_is_clamped_to_the_ceiling() {
        let s = SignedUrlService::new(SECRET, vec![], 999_999);
        assert_eq!(s.ttl_seconds(), MAX_TTL_SECONDS);
    }

    #[test]
    fn a_zero_ttl_is_clamped_up_rather_than_minting_dead_tokens() {
        let s = SignedUrlService::new(SECRET, vec![], 0);
        assert_eq!(s.ttl_seconds(), 1);
        let token = s.mint_at(&coord(), &alice(), now());
        assert!(s.verify_at(&token, &coord(), now()).is_ok());
    }

    // ── Canonical-string injectivity ─────────────────────────────────────────
    //
    // The security review of this module found the first version of
    // `canonical()` — `\n`-joined fields, `,`-joined groups — was not
    // injective, and that the ambiguity was reachable: `validate_path_safe`
    // permits control characters and actix percent-decodes path segments, so a
    // `%0A` in a published package name arrives here as a newline.

    /// The exact attack, end to end.
    ///
    /// Mint at a coordinate the attacker controls, whose package name carries
    /// the extra lines. Keep the MAC bytes. Re-split the payload into the
    /// *victim's* coordinate with `role: "admin"`. Under the old encoding the
    /// two reconstructions were byte-identical and this verified.
    #[test]
    fn a_payload_reslit_across_field_boundaries_is_rejected() {
        let s = svc();
        let exp_now = now();

        // What the attacker legitimately publishes and mints for.
        let attacker_coord = Coordinate {
            method: "GET",
            registry: "tf",
            package: "providers/hashicorp/aws\n5.0.0\nlinux/amd64\nmallory\nadmin",
            version: "9.9.9",
            artifact: "linux/amd64",
        };
        let mallory = Identity {
            user_id: Some("mallory".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec!["oidc:team".to_owned()],
        };
        let token = s.mint_at(&attacker_coord, &mallory, exp_now);
        let mac = token.rsplit_once('.').unwrap().1;
        let exp = exp_now.timestamp() + DEFAULT_TTL_SECONDS as i64;

        // The victim's coordinate, and the payload re-split to reach it.
        let victim_coord = Coordinate {
            method: "GET",
            registry: "tf",
            package: "providers/hashicorp/aws",
            version: "5.0.0",
            artifact: "linux/amd64",
        };
        let forged_payload = Payload {
            v: TOKEN_VERSION,
            reg: "tf".to_owned(),
            pkg: "providers/hashicorp/aws".to_owned(),
            ver: "5.0.0".to_owned(),
            art: "linux/amd64".to_owned(),
            sub: Some("mallory".to_owned()),
            role: "admin".to_owned(),
            grp: vec!["9.9.9\nlinux/amd64\nmallory\nuser\noidc:team".to_owned()],
            exp,
        };
        let forged = format!(
            "{TOKEN_VERSION}.{}.{mac}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged_payload).unwrap())
        );

        assert_eq!(
            s.verify_at(&forged, &victim_coord, exp_now).unwrap_err(),
            SignedUrlError::BadSignature,
            "a re-split payload must not reuse the MAC of another tuple"
        );
    }

    /// The same shape one field over, needing no newline at all: one group
    /// containing a comma used to re-split into two groups, forging membership.
    #[test]
    fn a_group_containing_a_separator_cannot_pose_as_two_groups() {
        let one = SignedUrlService::canonical(&coord(), Some("u"), "user", &["a,b".to_owned()], 1);
        let two = SignedUrlService::canonical(
            &coord(),
            Some("u"),
            "user",
            &["a".to_owned(), "b".to_owned()],
            1,
        );
        assert_ne!(one, two);
    }

    /// An empty list and a list holding one empty string are different
    /// identities, so they must not share a MAC. This is what the group *count*
    /// buys over length-prefixing the elements alone.
    #[test]
    fn an_empty_group_list_differs_from_one_empty_group() {
        assert_ne!(
            SignedUrlService::canonical(&coord(), Some("u"), "user", &[], 1),
            SignedUrlService::canonical(&coord(), Some("u"), "user", &["".to_owned()], 1),
        );
    }

    /// Injectivity, asserted over a table of adversarial values rather than one
    /// case: every distinct tuple must produce a distinct string. A collision
    /// here is a forgeable token, whichever pair collides.
    #[test]
    fn distinct_inputs_never_share_a_canonical_string() {
        let nasty: [&str; 17] = [
            "",
            "a",
            "a\nb",
            "\n",
            "\na",
            "a\n",
            ",",
            "a,b",
            "1:a",
            "2:ab",
            ":",
            "5",
            "aws",
            "aws\n5.0.0",
            "linux/amd64",
            "admin",
            "user",
        ];
        let roles: [&str; 2] = ["user", "admin"];
        let groupings: [&[&str]; 4] = [&[], &["a"], &["a,b"], &["a", "b"]];

        // The product is built as one iterator rather than five nested `for`s:
        // the nesting is arithmetic, and the only part worth reading is the
        // collision check below.
        let cases = nasty.into_iter().flat_map(move |pkg| {
            nasty.into_iter().flat_map(move |ver| {
                nasty.into_iter().flat_map(move |sub| {
                    roles.into_iter().flat_map(move |role| {
                        groupings
                            .into_iter()
                            .map(move |groups| (pkg, ver, sub, role, groups))
                    })
                })
            })
        });

        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for (pkg, ver, sub, role, groups) in cases {
            let groups: Vec<String> = groups.iter().map(|g| (*g).to_owned()).collect();
            let c = Coordinate {
                method: "GET",
                registry: "tf",
                package: pkg,
                version: ver,
                artifact: "linux/amd64",
            };
            let key = SignedUrlService::canonical(&c, Some(sub), role, &groups, 1_767_225_600);
            let id = format!("{pkg:?}|{ver:?}|{sub:?}|{role}|{groups:?}");
            if let Some(previous) = seen.insert(key.clone(), id.clone()) {
                panic!("canonical collision:\n  {previous}\n  {id}\n  -> {key:?}");
            }
        }
    }

    /// An anonymous token names no user, and must not be editable into one that
    /// names the empty user. Length-prefixing alone does not separate `None`
    /// from `Some("")`; the presence marker does.
    #[test]
    fn an_absent_subject_cannot_be_edited_into_an_empty_one() {
        let s = svc();
        let token = s.mint_at(&coord(), &Identity::anonymous(), now());
        let mac = token.rsplit_once('.').unwrap().1;

        let forged = Payload {
            v: TOKEN_VERSION,
            reg: "tf".to_owned(),
            pkg: "providers/hashicorp/random".to_owned(),
            ver: "5.40.0".to_owned(),
            art: "linux/amd64".to_owned(),
            sub: Some(String::new()),
            role: "anonymous".to_owned(),
            grp: vec![],
            exp: now().timestamp() + DEFAULT_TTL_SECONDS as i64,
        };
        let token = format!(
            "{TOKEN_VERSION}.{}.{mac}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap())
        );

        assert_eq!(
            s.verify_at(&token, &coord(), now()).unwrap_err(),
            SignedUrlError::BadSignature
        );
    }

    #[test]
    fn an_absent_and_an_empty_subject_have_different_canonical_strings() {
        assert_ne!(
            SignedUrlService::canonical(&coord(), None, "user", &[], 1),
            SignedUrlService::canonical(&coord(), Some(""), "user", &[], 1),
        );
    }

    /// A coordinate field carrying a newline is still *signable* — refusing it
    /// belongs in `validate_path_safe`, not here. What must hold is that it
    /// only ever verifies at the coordinate it names.
    #[test]
    fn a_newline_bearing_coordinate_still_only_opens_itself() {
        let s = svc();
        let weird = Coordinate {
            package: "providers/acme/a\nb",
            ..coord()
        };
        let token = s.mint_at(&weird, &alice(), now());

        assert!(s.verify_at(&token, &weird, now()).is_ok());
        let split = Coordinate {
            package: "providers/acme/a",
            ..coord()
        };
        assert!(s.verify_at(&token, &split, now()).is_err());
    }

    // ── Hygiene ──────────────────────────────────────────────────────────────

    #[test]
    fn debug_does_not_leak_the_secret() {
        let s = SignedUrlService::new(SECRET, vec![OTHER.to_vec()], 300);
        let rendered = format!("{s:?}");
        assert!(!rendered.contains("0123456789abcdef"), "{rendered}");
        assert!(!rendered.contains("fedcba"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn a_minted_token_is_url_safe() {
        // It rides in a query string; `+`, `/` and `=` would need escaping and
        // the relative-URL arithmetic in the mirror document does not do any.
        let token = svc().mint_at(&coord(), &alice(), now());
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "{token}"
        );
    }
}
