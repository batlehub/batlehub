//! The audit trail's seal chain (RFC 0036 §5.3): what one record is, how a
//! window's rows are reduced to a digest, and what the signature covers.
//!
//! Pure data and pure functions — the job that writes records and the verifier
//! that replays them live in `services::audit_trail`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::entities::AccessEvent;

/// The `prev_digest` of the first record: there is nothing before it.
pub const GENESIS_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// What a record says happened to its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SealKind {
    /// The window closed; its rows as they were.
    Seal,
    /// The lifecycle rewrote rows in the window (pseudonymisation, erasure).
    Amend,
    /// Rows of the window were deleted (retention, a purge).
    Expire,
}

impl SealKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Seal => "seal",
            Self::Amend => "amend",
            Self::Expire => "expire",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "seal" => Some(Self::Seal),
            "amend" => Some(Self::Amend),
            "expire" => Some(Self::Expire),
            _ => None,
        }
    }
}

/// One record of the chain. Every record names the window it is about, what
/// the window holds *after* it, and the previous record's digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SealRecord {
    /// Position in the chain, from 1. A SIEM keys on it: the same position
    /// arriving twice with two digests is a rewritten tail.
    pub seq: i64,
    pub kind: SealKind,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// Rows the window holds after this record.
    pub row_count: u64,
    /// Rows this record rewrote (`amend`) or removed (`expire`); `0` for a seal.
    pub affected: u64,
    /// SHA-256 over the window's rows in canonical form, hex.
    pub rows_digest: String,
    /// SHA-256 over [`signed_message`], hex — what the next record chains to.
    pub digest: String,
    pub prev_digest: String,
    /// Ed25519 over `digest`, base64.
    pub signature: String,
    pub key_id: String,
}

/// One row in canonical form: its export JSON with every object's keys
/// sorted, on one line. Stable across serde_json's map ordering.
pub fn canonical_row(event: &AccessEvent) -> String {
    let value = serde_json::to_value(event).expect("an AccessEvent always serialises");
    let mut out = String::new();
    write_sorted(&value, &mut out);
    out
}

/// `serde_json::to_string` with every object's keys sorted, so the result does
/// not depend on how `serde_json::Map` iterates (it keeps insertion order
/// under `preserve_order`).
fn write_sorted(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("a string serialises"));
                out.push(':');
                write_sorted(&map[k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_sorted(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// SHA-256 over a window's rows, canonical and one per line, in the order the
/// store returns them (`created_at`, then `id`).
pub fn rows_digest(rows: &[AccessEvent]) -> String {
    let mut h = Sha256::new();
    for row in rows {
        h.update(canonical_row(row).as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

/// What a record's digest is the hash of: every field but the digest and the
/// signature, so neither the kind, the window nor the counts can be swapped
/// without breaking it.
pub fn signed_message(r: &SealRecord) -> String {
    format!(
        "batlehub-audit-seal-v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        r.seq,
        r.kind.as_str(),
        r.window_start.timestamp_micros(),
        r.window_end.timestamp_micros(),
        r.row_count,
        r.affected,
        r.rows_digest,
        r.prev_digest,
    )
}

/// The digest [`signed_message`] hashes to.
pub fn record_digest(r: &SealRecord) -> String {
    hex::encode(Sha256::digest(signed_message(r).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{AccessAction, AccessResult, CallerNet, Role};

    fn event() -> AccessEvent {
        let mut e = AccessEvent::about_identity(
            AccessAction::SignIn,
            Some("alice".into()),
            Role::User,
            AccessResult::Allowed,
            CallerNet {
                ip: Some("10.0.0.1".into()),
                user_agent: None,
            },
            Some("provider=authentik".into()),
        );
        e.id = uuid::Uuid::nil();
        e.timestamp = DateTime::parse_from_rfc3339("2026-10-05T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        e
    }

    /// The canonical form is the contract every past seal was computed under:
    /// a change here makes every sealed window fail `verify`. This pins it.
    #[test]
    fn the_canonical_form_is_stable() {
        assert_eq!(
            canonical_row(&event()),
            r#"{"action":"signin","detail":"provider=authentik","id":"00000000-0000-0000-0000-000000000000","ip_address":"10.0.0.1","package_id":null,"result":{"outcome":"allowed"},"timestamp":"2026-10-05T09:00:00Z","user_id":"alice","user_role":"user"}"#
        );
    }

    #[test]
    fn any_change_to_a_row_changes_the_window_digest() {
        let rows = vec![event()];
        let mut edited = rows.clone();
        edited[0].ip_address = Some("10.0.0.2".into());
        assert_ne!(rows_digest(&rows), rows_digest(&edited));
        assert_ne!(rows_digest(&rows), rows_digest(&[]));
    }

    #[test]
    fn the_digest_covers_kind_window_and_counts() {
        let base = SealRecord {
            seq: 1,
            kind: SealKind::Seal,
            window_start: event().timestamp,
            window_end: event().timestamp + chrono::Duration::seconds(300),
            row_count: 1,
            affected: 0,
            rows_digest: rows_digest(&[event()]),
            digest: String::new(),
            prev_digest: GENESIS_DIGEST.into(),
            signature: String::new(),
            key_id: String::new(),
        };
        let d = record_digest(&base);
        for changed in [
            SealRecord {
                kind: SealKind::Amend,
                ..base.clone()
            },
            SealRecord {
                row_count: 2,
                ..base.clone()
            },
            SealRecord {
                seq: 2,
                ..base.clone()
            },
            SealRecord {
                window_end: base.window_start,
                ..base.clone()
            },
        ] {
            assert_ne!(record_digest(&changed), d);
        }
    }
}
