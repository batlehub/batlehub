#![no_main]
//! Signed download URLs (RFC 0012): the one credential this proxy mints itself.
//!
//! `services/signed_url.rs` documents a forgery that shipped — a `\n` in a
//! package name re-split the canonical string, so one MAC covered two
//! coordinates and a `role: "admin"` payload verified. The fix was a
//! length-prefixed encoding; its unit tests pin the exact attack. This asserts
//! the property the fix was meant to buy, over every coordinate and identity
//! libFuzzer can spell, control characters included:
//!
//! 1. **Round trip.** A token minted for `(coord, identity)` verifies at that
//!    coordinate and hands back the same identity, at the minting instant, any
//!    earlier instant, and at `exp` itself. A day past `exp` it is `Expired`.
//! 2. **Bound to its coordinate.** The same token presented at any coordinate
//!    that differs in *any* of the five fields — including a `/` moved from
//!    `package` into `version`, which leaves the `reg/pkg/ver/art` display
//!    string identical — is refused.
//! 3. **Bound to its secret.** A service built on a different secret refuses
//!    it; one that holds the minting secret as a *previous* secret accepts it
//!    (rotation without a flag day).
//! 4. **No malleability that changes meaning.** Any edit to the token bytes
//!    either fails, or verifies to exactly the identity that was minted. An
//!    edit that verifies is allowed only while the payload it carries still
//!    parses to the same fields the MAC covers.

use chrono::{DateTime, Utc};
use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::{Identity, Role};
use batlehub_core::services::signed_url::{Coordinate, SignedUrlError, SignedUrlService};

/// Five owned strings a `Coordinate<'_>` can borrow from.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Fields {
    method: String,
    registry: String,
    package: String,
    version: String,
    artifact: String,
}

impl Fields {
    fn coord(&self) -> Coordinate<'_> {
        Coordinate {
            method: &self.method,
            registry: &self.registry,
            package: &self.package,
            version: &self.version,
            artifact: &self.artifact,
        }
    }

    fn get_mut(&mut self, i: u8) -> &mut String {
        match i % 5 {
            0 => &mut self.method,
            1 => &mut self.registry,
            2 => &mut self.package,
            3 => &mut self.version,
            _ => &mut self.artifact,
        }
    }
}

fn role(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<Role> {
    Ok(match u.int_in_range(0..=2u8)? {
        0 => Role::Anonymous,
        1 => Role::User,
        _ => Role::Admin,
    })
}

fn same_identity(a: &Identity, b: &Identity) -> bool {
    a.user_id == b.user_id && a.role == b.role && a.groups == b.groups
}

/// Whether two secrets are the same HMAC key. HMAC zero-pads a key shorter
/// than the block size, so `b""` and `b"\0"` sign identically — a fact about
/// the primitive, not a finding, and `MIN_SECRET_BYTES` keeps it out of any
/// configuration. The oracle has to know it or it reports HMAC itself.
fn same_hmac_key(a: &[u8], b: &[u8]) -> bool {
    let trim = |k: &[u8]| k.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
    a[..trim(a)] == b[..trim(b)]
}

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("timestamp kept inside chrono's range")
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(secret_len) = u.int_in_range(0..=64usize) else {
        return;
    };
    let Ok(secret) = u.bytes(secret_len).map(<[u8]>::to_vec) else {
        return;
    };
    let Ok(previous): arbitrary::Result<Vec<Vec<u8>>> = u.arbitrary() else {
        return;
    };
    let previous: Vec<Vec<u8>> = previous.into_iter().take(2).collect();
    let Ok(ttl): arbitrary::Result<u64> = u.arbitrary() else {
        return;
    };

    let Ok(fields): arbitrary::Result<(String, String, String, String, String)> = u.arbitrary()
    else {
        return;
    };
    let fields = Fields {
        method: fields.0,
        registry: fields.1,
        package: fields.2,
        version: fields.3,
        artifact: fields.4,
    };
    let Ok(user_id): arbitrary::Result<Option<String>> = u.arbitrary() else {
        return;
    };
    let Ok(role) = role(&mut u) else { return };
    let Ok(groups): arbitrary::Result<Vec<String>> = u.arbitrary() else {
        return;
    };
    let identity = Identity {
        user_id,
        role,
        auth_provider: None,
        groups: groups.into_iter().take(4).collect(),
    };
    // Any instant a real clock could show, far enough from chrono's limits
    // that `exp` and the day past it stay representable.
    let Ok(now_secs) = u.int_in_range(-1_000_000_000_000i64..=1_000_000_000_000) else {
        return;
    };
    let now = at(now_secs);

    let svc = SignedUrlService::new(secret.clone(), previous.clone(), ttl);
    let token = svc.mint_at(&fields.coord(), &identity, now);
    let exp = now_secs + svc.ttl_seconds() as i64;

    // 1. Round trip: at the minting instant, before it, and at expiry.
    for instant in [now, at(now_secs - 1_000_000), at(exp)] {
        match svc.verify_at(&token, &fields.coord(), instant) {
            Ok(got) => {
                assert!(
                    same_identity(&got, &identity),
                    "verified to a different identity: minted {identity:?}, got {got:?}"
                );
                assert_eq!(got.auth_provider.as_deref(), Some("signed-url"));
            }
            Err(e) => panic!("a freshly minted token failed at {instant}: {e:?}"),
        }
    }
    assert!(
        matches!(
            svc.verify_at(&token, &fields.coord(), at(exp + 86_400)),
            Err(SignedUrlError::Expired { .. })
        ),
        "a day past `exp` must be Expired"
    );

    // 2. Bound to its coordinate.
    let mut other = fields.clone();
    let Ok(which) = u.int_in_range(0..=4u8) else {
        return;
    };
    match u.int_in_range(0..=3u8).unwrap_or(0) {
        0 => {
            let Ok(c): arbitrary::Result<char> = u.arbitrary() else {
                return;
            };
            other.get_mut(which).push(c);
        }
        1 => other.get_mut(which).clear(),
        2 => {
            let Ok(j) = u.int_in_range(0..=4u8) else {
                return;
            };
            let a = other.get_mut(which).clone();
            let b = other.get_mut(j).clone();
            *other.get_mut(which) = b;
            *other.get_mut(j) = a;
        }
        _ => {
            // Move the boundary between `package` and `version` across a `/`,
            // so `reg/pkg/ver/art` reads the same and only the fields differ —
            // the shape the pre-mismatch display check cannot see.
            if let Some((head, tail)) = other.package.clone().split_once('/') {
                other.package = head.to_owned();
                other.version = format!("{tail}/{}", other.version);
            } else {
                other.package.push('/');
            }
        }
    }
    if other != fields {
        assert!(
            svc.verify_at(&token, &other.coord(), now).is_err(),
            "token minted for {fields:?} verified at {other:?}"
        );
    }

    // 3. Bound to its secret; accepted through rotation.
    let Ok(other_len) = u.int_in_range(0..=64usize) else {
        return;
    };
    let Ok(other_secret) = u.bytes(other_len).map(<[u8]>::to_vec) else {
        return;
    };
    if !same_hmac_key(&other_secret, &secret)
        && !previous.iter().any(|p| same_hmac_key(p, &other_secret))
    {
        let stranger = SignedUrlService::new(other_secret.clone(), vec![], ttl);
        assert!(
            matches!(
                stranger.verify_at(&token, &fields.coord(), now),
                Err(SignedUrlError::BadSignature)
            ),
            "a service with a different secret accepted the token"
        );
        let rotated = SignedUrlService::new(other_secret, vec![secret.clone()], ttl);
        let got = rotated
            .verify_at(&token, &fields.coord(), now)
            .expect("the minting secret is a previous secret, the token must verify");
        assert!(same_identity(&got, &identity));
    }

    // 4. Malleability: an edited token is refused, or means exactly the same.
    let mut edited = token.clone().into_bytes();
    let Ok(edits) = u.int_in_range(1..=3u8) else {
        return;
    };
    for _ in 0..edits {
        if edited.is_empty() {
            break;
        }
        let Ok(at_byte) = u.int_in_range(0..=edited.len() - 1) else {
            return;
        };
        match u.int_in_range(0..=2u8).unwrap_or(0) {
            0 => {
                let Ok(b): arbitrary::Result<u8> = u.arbitrary() else {
                    return;
                };
                edited[at_byte] = b;
            }
            1 => {
                edited.remove(at_byte);
            }
            _ => {
                let Ok(b): arbitrary::Result<u8> = u.arbitrary() else {
                    return;
                };
                edited.insert(at_byte, b);
            }
        }
    }
    let Ok(edited) = String::from_utf8(edited) else {
        return;
    };
    if edited == token {
        return;
    }
    if let Ok(got) = svc.verify_at(&edited, &fields.coord(), now) {
        assert!(
            same_identity(&got, &identity),
            "an edited token verified to a different identity: {got:?} from {edited:?}"
        );
    }
});
