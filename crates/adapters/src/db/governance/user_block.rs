use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use tokio::sync::Mutex;

use batlehub_core::error::CoreError;
use batlehub_core::ports::{UserBlock, UserBlockRepository};

use crate::db::DbResultExt;

// ── Postgres ──────────────────────────────────────────────────────────────────

/// PostgreSQL-backed user block repository.
///
/// Blocks survive restarts and are shared across all server instances pointing
/// at the same database. Requires the migration in `028_user_blocks.sql`.
pub struct PgUserBlockRepository {
    pool: PgPool,
}

impl PgUserBlockRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserBlockRepository for PgUserBlockRepository {
    async fn list(&self) -> Result<Vec<UserBlock>, CoreError> {
        let rows = sqlx::query(
            "SELECT user_id, blocked_at, blocked_by, reason \
             FROM user_blocks ORDER BY blocked_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows
            .into_iter()
            .map(|r| UserBlock {
                user_id: r.get("user_id"),
                blocked_at: r.get("blocked_at"),
                blocked_by: r.get("blocked_by"),
                reason: r.try_get("reason").ok(),
            })
            .collect())
    }

    async fn block(
        &self,
        user_id: &str,
        blocked_by: &str,
        reason: Option<&str>,
    ) -> Result<(), CoreError> {
        let _clear = crate::db::read_cache::CACHES.user_blocks.clear_after();
        sqlx::query(
            "INSERT INTO user_blocks (user_id, blocked_by, reason) VALUES ($1, $2, $3) \
             ON CONFLICT (user_id) DO UPDATE \
               SET blocked_at = NOW(), \
                   blocked_by = EXCLUDED.blocked_by, \
                   reason     = EXCLUDED.reason",
        )
        .bind(user_id)
        .bind(blocked_by)
        .bind(reason)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn unblock(&self, user_id: &str) -> Result<(), CoreError> {
        let _clear = crate::db::read_cache::CACHES.user_blocks.clear_after();
        sqlx::query("DELETE FROM user_blocks WHERE user_id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    async fn is_blocked(&self, user_id: &str) -> Result<bool, CoreError> {
        crate::db::read_cache::CACHES
            .user_blocks
            .get_or_try(user_id.to_owned(), async {
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_blocks WHERE user_id = $1)")
                    .bind(user_id)
                    .fetch_one(&self.pool)
                    .await
                    .db_err()
            })
            .await
    }
}

// ── In-memory (tests) ─────────────────────────────────────────────────────────

struct BlockEntry {
    blocked_at: DateTime<Utc>,
    blocked_by: String,
    reason: Option<String>,
}

/// In-process user block repository for use in tests.
///
/// State is **not** persisted across restarts. Use `PgUserBlockRepository` in production.
#[derive(Default)]
pub struct InMemoryUserBlockRepository {
    blocks: Mutex<std::collections::HashMap<String, BlockEntry>>,
}

impl InMemoryUserBlockRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl UserBlockRepository for InMemoryUserBlockRepository {
    async fn list(&self) -> Result<Vec<UserBlock>, CoreError> {
        let guard = self.blocks.lock().await;
        Ok(guard
            .iter()
            .map(|(id, e)| UserBlock {
                user_id: id.clone(),
                blocked_at: e.blocked_at,
                blocked_by: e.blocked_by.clone(),
                reason: e.reason.clone(),
            })
            .collect())
    }

    async fn block(
        &self,
        user_id: &str,
        blocked_by: &str,
        reason: Option<&str>,
    ) -> Result<(), CoreError> {
        let mut guard = self.blocks.lock().await;
        guard.insert(
            user_id.to_owned(),
            BlockEntry {
                blocked_at: Utc::now(),
                blocked_by: blocked_by.to_owned(),
                reason: reason.map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn unblock(&self, user_id: &str) -> Result<(), CoreError> {
        self.blocks.lock().await.remove(user_id);
        Ok(())
    }

    async fn is_blocked(&self, user_id: &str) -> Result<bool, CoreError> {
        Ok(self.blocks.lock().await.contains_key(user_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn not_blocked_initially() {
        let repo = InMemoryUserBlockRepository::new();
        assert!(!repo.is_blocked("alice").await.unwrap());
    }

    #[tokio::test]
    async fn block_and_detect() {
        let repo = InMemoryUserBlockRepository::new();
        repo.block("alice", "admin", Some("spammer")).await.unwrap();
        assert!(repo.is_blocked("alice").await.unwrap());
        assert!(!repo.is_blocked("bob").await.unwrap());
    }

    #[tokio::test]
    async fn unblock_removes_block() {
        let repo = InMemoryUserBlockRepository::new();
        repo.block("alice", "admin", None).await.unwrap();
        repo.unblock("alice").await.unwrap();
        assert!(!repo.is_blocked("alice").await.unwrap());
    }

    #[tokio::test]
    async fn list_returns_all_blocked() {
        let repo = InMemoryUserBlockRepository::new();
        repo.block("alice", "admin", Some("test")).await.unwrap();
        repo.block("bob", "admin", None).await.unwrap();
        let list = repo.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn reblock_updates_entry() {
        let repo = InMemoryUserBlockRepository::new();
        repo.block("alice", "admin", Some("reason1")).await.unwrap();
        repo.block("alice", "admin2", Some("reason2"))
            .await
            .unwrap();
        let list = repo.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].reason.as_deref(), Some("reason2"));
    }
}
