use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use batlehub_core::{
    entities::Identity,
    error::CoreError,
    ports::{OwnerEntry, OwnershipPort},
};

type OwnerMap = HashMap<(String, String), Vec<OwnerEntry>>;

/// In-memory [`OwnershipPort`].
///
/// Stores owner entries per `(registry, package)` pair. Insertion order is
/// preserved (no `granted_at` timestamp in the struct; ordering matches the
/// PostgreSQL implementation's `ORDER BY granted_at ASC`).
#[derive(Debug, Default)]
pub struct InMemoryOwnershipStore {
    data: Arc<RwLock<OwnerMap>>,
}

impl InMemoryOwnershipStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl OwnershipPort for InMemoryOwnershipStore {
    async fn initialize_owner(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<(), CoreError> {
        let mut map = self.data.write().await;
        let owners = map
            .entry((registry.to_owned(), package.to_owned()))
            .or_default();
        let already_present = owners
            .iter()
            .any(|e| e.principal_type == "user" && e.principal_id == user_id);
        if !already_present {
            owners.push(OwnerEntry {
                principal_type: "user".to_owned(),
                principal_id: user_id.to_owned(),
                role: "admin".to_owned(),
                granted_by: None,
            });
        }
        Ok(())
    }

    async fn claim_if_unowned(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<bool, CoreError> {
        let mut map = self.data.write().await;
        let owners = map
            .entry((registry.to_owned(), package.to_owned()))
            .or_default();
        if !owners.is_empty() {
            return Ok(false);
        }
        owners.push(OwnerEntry {
            principal_type: "user".to_owned(),
            principal_id: user_id.to_owned(),
            role: "admin".to_owned(),
            granted_by: None,
        });
        Ok(true)
    }

    async fn can_publish(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        let map = self.data.read().await;
        let key = (registry.to_owned(), package.to_owned());
        let owners = match map.get(&key) {
            None => return Ok(true),
            Some(owners) if owners.is_empty() => return Ok(true),
            Some(owners) => owners,
        };
        if let Some(uid) = &identity.user_id {
            if owners
                .iter()
                .any(|e| e.principal_type == "user" && e.principal_id == *uid)
            {
                return Ok(true);
            }
        }
        Ok(owners
            .iter()
            .any(|e| e.principal_type == "group" && identity.groups.contains(&e.principal_id)))
    }

    async fn add_owner(
        &self,
        registry: &str,
        package: &str,
        entry: OwnerEntry,
    ) -> Result<(), CoreError> {
        let mut map = self.data.write().await;
        let owners = map
            .entry((registry.to_owned(), package.to_owned()))
            .or_default();
        let exists = owners.iter().any(|e| {
            e.principal_type == entry.principal_type && e.principal_id == entry.principal_id
        });
        if exists {
            return Err(CoreError::Conflict(format!(
                "{} '{}' is already an owner of '{}/{}'",
                entry.principal_type, entry.principal_id, registry, package
            )));
        }
        owners.push(entry);
        Ok(())
    }

    async fn remove_owner(
        &self,
        registry: &str,
        package: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError> {
        let mut map = self.data.write().await;
        if let Some(owners) = map.get_mut(&(registry.to_owned(), package.to_owned())) {
            owners.retain(|e| {
                !(e.principal_type == principal_type && e.principal_id == principal_id)
            });
        }
        Ok(())
    }

    async fn list_owners(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<OwnerEntry>, CoreError> {
        let map = self.data.read().await;
        Ok(map
            .get(&(registry.to_owned(), package.to_owned()))
            .cloned()
            .unwrap_or_default())
    }

    async fn list_owned_by(&self, identity: &Identity) -> Result<Vec<(String, String)>, CoreError> {
        let map = self.data.read().await;
        let mut owned: Vec<(String, String)> = map
            .iter()
            .filter(|(_, owners)| {
                owners.iter().any(|e| match e.principal_type.as_str() {
                    "user" => identity.user_id.as_deref() == Some(e.principal_id.as_str()),
                    "group" => identity.groups.contains(&e.principal_id),
                    _ => false,
                })
            })
            .map(|(key, _)| key.clone())
            .collect();
        // A HashMap has no order; sort so the same identity gets the same list
        // twice running, which the handler's "5 most recent" cap depends on.
        owned.sort();
        Ok(owned)
    }
}

#[cfg(test)]
mod tests {
    use batlehub_core::{entities::Identity, ports::OwnershipPort};

    use super::InMemoryOwnershipStore;

    fn user_identity(uid: &str) -> Identity {
        Identity {
            user_id: Some(uid.to_owned()),
            role: batlehub_core::entities::Role::User,
            auth_provider: None,
            groups: vec![],
        }
    }

    fn group_identity(uid: &str, group: &str) -> Identity {
        Identity {
            user_id: Some(uid.to_owned()),
            role: batlehub_core::entities::Role::User,
            auth_provider: None,
            groups: vec![group.to_owned()],
        }
    }

    #[tokio::test]
    async fn can_publish_when_no_owners() {
        let store = InMemoryOwnershipStore::new();
        assert!(store
            .can_publish("reg", "pkg", &user_identity("alice"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn initialize_owner_makes_user_admin() {
        let store = InMemoryOwnershipStore::new();
        store.initialize_owner("reg", "pkg", "alice").await.unwrap();
        let owners = store.list_owners("reg", "pkg").await.unwrap();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].principal_id, "alice");
        assert_eq!(owners[0].role, "admin");
    }

    #[tokio::test]
    async fn only_the_first_claimant_owns_an_unowned_package() {
        let store = InMemoryOwnershipStore::new();
        assert!(store.claim_if_unowned("reg", "pkg", "alice").await.unwrap());
        assert!(!store
            .claim_if_unowned("reg", "pkg", "mallory")
            .await
            .unwrap());
        let owners = store.list_owners("reg", "pkg").await.unwrap();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].principal_id, "alice");
    }

    #[tokio::test]
    async fn initialize_owner_is_idempotent() {
        let store = InMemoryOwnershipStore::new();
        store.initialize_owner("reg", "pkg", "alice").await.unwrap();
        store.initialize_owner("reg", "pkg", "alice").await.unwrap();
        assert_eq!(store.list_owners("reg", "pkg").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn can_publish_true_for_owner() {
        let store = InMemoryOwnershipStore::new();
        store.initialize_owner("reg", "pkg", "alice").await.unwrap();
        assert!(store
            .can_publish("reg", "pkg", &user_identity("alice"))
            .await
            .unwrap());
        assert!(!store
            .can_publish("reg", "pkg", &user_identity("bob"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn can_publish_true_for_group_owner() {
        let store = InMemoryOwnershipStore::new();
        store.initialize_owner("reg", "pkg", "alice").await.unwrap();
        use batlehub_core::ports::OwnerEntry;
        store
            .add_owner(
                "reg",
                "pkg",
                OwnerEntry {
                    principal_type: "group".to_owned(),
                    principal_id: "frontend".to_owned(),
                    role: "maintainer".to_owned(),
                    granted_by: None,
                },
            )
            .await
            .unwrap();
        assert!(store
            .can_publish("reg", "pkg", &group_identity("bob", "frontend"))
            .await
            .unwrap());
        assert!(!store
            .can_publish("reg", "pkg", &group_identity("bob", "backend"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn add_owner_conflict() {
        let store = InMemoryOwnershipStore::new();
        store.initialize_owner("reg", "pkg", "alice").await.unwrap();
        use batlehub_core::ports::OwnerEntry;
        let result = store
            .add_owner(
                "reg",
                "pkg",
                OwnerEntry {
                    principal_type: "user".to_owned(),
                    principal_id: "alice".to_owned(),
                    role: "maintainer".to_owned(),
                    granted_by: None,
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(batlehub_core::error::CoreError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn remove_owner_silent_on_missing() {
        let store = InMemoryOwnershipStore::new();
        store
            .remove_owner("reg", "pkg", "user", "nobody")
            .await
            .unwrap();
    }
}
