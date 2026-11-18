use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use batlehub_core::error::CoreError;
use batlehub_core::ports::{ScannerStatus, ScannerStatusRepository};

/// In-memory `scanner_status` (RFC 0050 §6.2), with the Postgres store's
/// merge: timestamps only move forward, the class follows the newer error.
#[derive(Debug, Default)]
pub struct InMemoryScannerStatus {
    rows: RwLock<BTreeMap<String, ScannerStatus>>,
}

impl InMemoryScannerStatus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl ScannerStatusRepository for InMemoryScannerStatus {
    async fn record(&self, s: &ScannerStatus) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        let row = rows
            .entry(s.scanner.clone())
            .or_insert_with(|| ScannerStatus {
                scanner: s.scanner.clone(),
                last_ok_at: None,
                last_error_at: None,
                last_error_class: None,
                unavailable_until: None,
            });
        // The Postgres rule, read against the row before this write.
        let (ok_before, err_before) = (row.last_ok_at, row.last_error_at);
        if s.unavailable_until.is_some() {
            if s.last_error_at.is_none() || ok_before.is_none() || s.last_error_at >= ok_before {
                row.unavailable_until = row.unavailable_until.max(s.unavailable_until);
            }
        } else if s.last_ok_at.is_some() && (err_before.is_none() || s.last_ok_at > err_before) {
            row.unavailable_until = None;
        }
        row.last_ok_at = row.last_ok_at.max(s.last_ok_at);
        if s.last_error_at.is_some() && s.last_error_at > row.last_error_at {
            row.last_error_at = s.last_error_at;
            row.last_error_class = s.last_error_class.clone();
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ScannerStatus>, CoreError> {
        Ok(self.rows.read().await.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    /// Two workers writing out of order never move a timestamp back.
    #[tokio::test]
    async fn timestamps_only_move_forward() {
        let repo = InMemoryScannerStatus::new();
        let now = Utc::now();
        let at = |ok, err, class: Option<&str>| ScannerStatus {
            scanner: "osv".into(),
            last_ok_at: ok,
            last_error_at: err,
            last_error_class: class.map(str::to_owned),
            unavailable_until: None,
        };
        repo.record(&at(Some(now), Some(now), Some("timeout")))
            .await
            .unwrap();
        repo.record(&at(
            Some(now - Duration::minutes(5)),
            Some(now - Duration::minutes(5)),
            Some("upstream"),
        ))
        .await
        .unwrap();
        let row = &repo.list().await.unwrap()[0];
        assert_eq!(row.last_ok_at, Some(now));
        assert_eq!(row.last_error_class.as_deref(), Some("timeout"));
    }

    /// Several workers share the row, and writes land out of order: an open
    /// breaker only extends, an error without one leaves it, a newer success
    /// closes it, and an error older than that success does not reopen it.
    #[tokio::test]
    async fn the_breaker_only_extends_until_a_success() {
        let repo = InMemoryScannerStatus::new();
        let now = Utc::now();
        let row = |ok: bool, at: chrono::DateTime<Utc>, until: Option<chrono::DateTime<Utc>>| {
            ScannerStatus {
                scanner: "osv".into(),
                last_ok_at: ok.then_some(at),
                last_error_at: (!ok).then_some(at),
                last_error_class: (!ok).then(|| "unavailable".to_owned()),
                unavailable_until: until,
            }
        };
        let later = now + Duration::minutes(5);
        repo.record(&row(false, now, Some(later))).await.unwrap();
        repo.record(&row(false, now, Some(now + Duration::minutes(1))))
            .await
            .unwrap();
        repo.record(&row(false, now, None)).await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].unavailable_until, Some(later));
        let after = now + Duration::seconds(1);
        repo.record(&row(true, after, None)).await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].unavailable_until, None);
        // A stale error, written before that success, lands after it.
        repo.record(&row(false, now, Some(later))).await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0].unavailable_until, None);
    }
}
