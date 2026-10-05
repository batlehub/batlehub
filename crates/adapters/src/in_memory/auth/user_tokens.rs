use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use batlehub_core::{
    entities::Role,
    error::CoreError,
    ports::{TokenOwner, UserToken, UserTokenRepository},
};

/// A [`UserTokenRepository`] that rejects token creation and returns empty
/// results for all lookups.
///
/// Appropriate for tests that authenticate via [`StaticTokenAuthProvider`]
/// (static tokens in config) and do not exercise the user-generated-token flow.
///
/// [`StaticTokenAuthProvider`]: crate::auth::StaticTokenAuthProvider
#[derive(Debug, Default)]
pub struct NullUserTokenRepository;

impl NullUserTokenRepository {
    pub fn arc() -> Arc<dyn UserTokenRepository> {
        Arc::new(Self)
    }
}

#[async_trait]
impl UserTokenRepository for NullUserTokenRepository {
    async fn create_token(
        &self,
        _id: Uuid,
        _owner: &TokenOwner,
        _name: &str,
        _token_hash: &str,
        _role: Role,
        _expires_at: DateTime<Utc>,
        _groups: &[String],
    ) -> Result<UserToken, CoreError> {
        Err(CoreError::Database(
            "NullUserTokenRepository does not support token creation".into(),
        ))
    }

    async fn find_by_hash(&self, _token_hash: &str) -> Result<Option<UserToken>, CoreError> {
        Ok(None)
    }

    async fn list_for_user(&self, _owner: &TokenOwner) -> Result<Vec<UserToken>, CoreError> {
        Ok(vec![])
    }

    async fn touch_last_used(&self, _id: Uuid, _source_ip: Option<&str>) -> Result<(), CoreError> {
        Ok(())
    }

    async fn revoke(&self, _id: Uuid, _owner: &TokenOwner) -> Result<bool, CoreError> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn create_token_returns_err() {
        let repo = NullUserTokenRepository::arc();
        let result = repo
            .create_token(
                Uuid::new_v4(),
                &TokenOwner::new("oidc", "alice"),
                "my-token",
                "hash",
                Role::User,
                Utc::now(),
                &[],
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn find_by_hash_returns_none() {
        let repo = NullUserTokenRepository::arc();
        assert!(repo.find_by_hash("any-hash").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_for_user_returns_empty() {
        let repo = NullUserTokenRepository::arc();
        assert!(repo
            .list_for_user(&TokenOwner::new("oidc", "alice"))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn revoke_returns_false() {
        let repo = NullUserTokenRepository::arc();
        assert!(!repo
            .revoke(Uuid::new_v4(), &TokenOwner::new("oidc", "alice"))
            .await
            .unwrap());
    }
}
