use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use batlehub_core::{
    entities::Identity,
    error::CoreError,
    ports::{BetaChannelEntry, BetaChannelPort},
};

/// In-memory [`BetaChannelPort`].
///
/// Stores members per registry. Insertion order is preserved.
#[derive(Debug, Default)]
pub struct InMemoryBetaChannelStore {
    data: Arc<RwLock<HashMap<String, Vec<BetaChannelEntry>>>>,
}

impl InMemoryBetaChannelStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl BetaChannelPort for InMemoryBetaChannelStore {
    async fn is_member(&self, registry: &str, identity: &Identity) -> Result<bool, CoreError> {
        let map = self.data.read().await;
        let Some(members) = map.get(registry) else {
            return Ok(false);
        };
        if let Some(uid) = &identity.user_principal() {
            if members
                .iter()
                .any(|e| e.principal_type == "user" && e.principal_id == *uid)
            {
                return Ok(true);
            }
        }
        Ok(members
            .iter()
            .any(|e| e.principal_type == "group" && identity.groups.contains(&e.principal_id)))
    }

    async fn add_member(&self, registry: &str, entry: BetaChannelEntry) -> Result<(), CoreError> {
        let mut map = self.data.write().await;
        let members = map.entry(registry.to_owned()).or_default();
        let exists = members.iter().any(|e| {
            e.principal_type == entry.principal_type && e.principal_id == entry.principal_id
        });
        if exists {
            return Err(CoreError::Conflict(format!(
                "{} '{}' is already a beta-channel member of '{}'",
                entry.principal_type, entry.principal_id, registry
            )));
        }
        members.push(entry);
        Ok(())
    }

    async fn remove_member(
        &self,
        registry: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError> {
        let mut map = self.data.write().await;
        if let Some(members) = map.get_mut(registry) {
            members.retain(|e| {
                !(e.principal_type == principal_type && e.principal_id == principal_id)
            });
        }
        Ok(())
    }

    async fn list_members(&self, registry: &str) -> Result<Vec<BetaChannelEntry>, CoreError> {
        let map = self.data.read().await;
        Ok(map
            .get(registry)
            .map(|v| {
                v.iter()
                    .map(|e| BetaChannelEntry {
                        principal_type: e.principal_type.clone(),
                        principal_id: e.principal_id.clone(),
                        granted_by: e.granted_by.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use batlehub_core::{
        entities::{Identity, Role},
        ports::{BetaChannelEntry, BetaChannelPort},
    };

    use super::InMemoryBetaChannelStore;

    fn user_identity(uid: &str) -> Identity {
        Identity {
            user_id: Some(uid.to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        }
    }

    fn anon() -> Identity {
        Identity::anonymous()
    }

    fn user_entry(uid: &str) -> BetaChannelEntry {
        BetaChannelEntry {
            principal_type: "user".to_owned(),
            principal_id: uid.to_owned(),
            granted_by: None,
        }
    }

    #[tokio::test]
    async fn anonymous_is_never_member() {
        let store = InMemoryBetaChannelStore::new();
        assert!(!store.is_member("reg", &anon()).await.unwrap());
    }

    #[tokio::test]
    async fn not_member_by_default() {
        let store = InMemoryBetaChannelStore::new();
        assert!(!store
            .is_member("reg", &user_identity("alice"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn add_then_is_member() {
        let store = InMemoryBetaChannelStore::new();
        store.add_member("reg", user_entry("alice")).await.unwrap();
        assert!(store
            .is_member("reg", &user_identity("alice"))
            .await
            .unwrap());
        assert!(!store.is_member("reg", &user_identity("bob")).await.unwrap());
    }

    /// A member is `<provider>:<id>`: the same id from another provider is
    /// another person and is not a beta member.
    #[tokio::test]
    async fn a_user_member_is_scoped_to_its_provider() {
        let store = InMemoryBetaChannelStore::new();
        store
            .add_member("reg", user_entry("corp:alice"))
            .await
            .unwrap();
        let from = |p: &str| Identity {
            auth_provider: Some(p.to_owned()),
            ..user_identity("alice")
        };
        assert!(store.is_member("reg", &from("corp")).await.unwrap());
        assert!(!store.is_member("reg", &from("signup")).await.unwrap());
    }

    #[tokio::test]
    async fn add_duplicate_returns_conflict() {
        let store = InMemoryBetaChannelStore::new();
        store.add_member("reg", user_entry("alice")).await.unwrap();
        let result = store.add_member("reg", user_entry("alice")).await;
        assert!(matches!(
            result,
            Err(batlehub_core::error::CoreError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn remove_member_then_not_member() {
        let store = InMemoryBetaChannelStore::new();
        store.add_member("reg", user_entry("alice")).await.unwrap();
        store.remove_member("reg", "user", "alice").await.unwrap();
        assert!(!store
            .is_member("reg", &user_identity("alice"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn remove_missing_member_succeeds() {
        let store = InMemoryBetaChannelStore::new();
        store.remove_member("reg", "user", "nobody").await.unwrap();
    }

    #[tokio::test]
    async fn group_membership() {
        let store = InMemoryBetaChannelStore::new();
        store
            .add_member(
                "reg",
                BetaChannelEntry {
                    principal_type: "group".to_owned(),
                    principal_id: "beta-testers".to_owned(),
                    granted_by: None,
                },
            )
            .await
            .unwrap();
        let member = Identity {
            user_id: Some("bob".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec!["beta-testers".to_owned()],
        };
        assert!(store.is_member("reg", &member).await.unwrap());
    }
}
