//! In-memory forge ports (RFC 0019 §5.2), for tests and for a deployment
//! that wired no database.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;

use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    budget_allows, BudgetRole, RateLimitBudget, RateLimitObservation, RefResolutionRepository,
    StoredRefResolution,
};

#[derive(Default)]
pub struct InMemoryRefResolutionRepository {
    rows: RwLock<HashMap<(String, String, String), StoredRefResolution>>,
}

impl InMemoryRefResolutionRepository {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl RefResolutionRepository for InMemoryRefResolutionRepository {
    async fn get(
        &self,
        registry: &str,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<Option<StoredRefResolution>, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .get(&(
                registry.to_owned(),
                owner_repo.to_owned(),
                git_ref.to_owned(),
            ))
            .cloned())
    }

    async fn upsert(
        &self,
        registry: &str,
        owner_repo: &str,
        git_ref: &str,
        resolution: &StoredRefResolution,
    ) -> Result<(), CoreError> {
        self.rows.write().await.insert(
            (
                registry.to_owned(),
                owner_repo.to_owned(),
                git_ref.to_owned(),
            ),
            resolution.clone(),
        );
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryRateLimitBudget {
    rows: RwLock<HashMap<(String, String), RateLimitObservation>>,
}

impl InMemoryRateLimitBudget {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// What was last observed for a budget, for assertions.
    pub async fn observed(
        &self,
        registry: &str,
        token_fingerprint: &str,
    ) -> Option<RateLimitObservation> {
        self.rows
            .read()
            .await
            .get(&(registry.to_owned(), token_fingerprint.to_owned()))
            .copied()
    }
}

#[async_trait]
impl RateLimitBudget for InMemoryRateLimitBudget {
    async fn acquire(
        &self,
        registry: &str,
        token_fingerprint: &str,
        role: BudgetRole,
    ) -> Result<bool, CoreError> {
        let observation = self
            .rows
            .read()
            .await
            .get(&(registry.to_owned(), token_fingerprint.to_owned()))
            .copied();
        Ok(budget_allows(observation, role, Utc::now()))
    }

    async fn observe(
        &self,
        registry: &str,
        token_fingerprint: &str,
        observation: RateLimitObservation,
    ) -> Result<(), CoreError> {
        self.rows.write().await.insert(
            (registry.to_owned(), token_fingerprint.to_owned()),
            observation,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two clients on the same token see one number: what one observes, the
    /// other is refused by.
    #[tokio::test]
    async fn the_budget_is_shared_across_roles_and_the_proxy_keeps_priority() {
        let budget = InMemoryRateLimitBudget::new();
        assert!(budget
            .acquire("gh", "tok", BudgetRole::Worker)
            .await
            .unwrap());
        budget
            .observe(
                "gh",
                "tok",
                RateLimitObservation {
                    remaining: 12,
                    limit: 60,
                    reset_at: Utc::now() + chrono::Duration::minutes(10),
                },
            )
            .await
            .unwrap();
        assert!(!budget
            .acquire("gh", "tok", BudgetRole::Worker)
            .await
            .unwrap());
        assert!(budget
            .acquire("gh", "tok", BudgetRole::Proxy)
            .await
            .unwrap());
        assert!(
            budget
                .acquire("gh", "other", BudgetRole::Worker)
                .await
                .unwrap(),
            "another token is another budget"
        );
    }

    #[tokio::test]
    async fn resolutions_round_trip() {
        let repo = InMemoryRefResolutionRepository::new();
        assert!(repo.get("gh", "o/r", "main").await.unwrap().is_none());
        let row = StoredRefResolution {
            kind: batlehub_core::entities::RefKind::Branch,
            sha: "a".repeat(40),
            resolved_at: Utc::now(),
            previous: None,
        };
        repo.upsert("gh", "o/r", "main", &row).await.unwrap();
        assert_eq!(repo.get("gh", "o/r", "main").await.unwrap(), Some(row));
    }
}
