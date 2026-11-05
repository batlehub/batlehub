use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use batlehub_core::{
    error::CoreError,
    ports::{LoginState, LoginStateStore},
};

/// Process-local [`LoginStateStore`] for tests and single-replica runs.
///
/// The `Mutex` is what makes `take` atomic here, matching the
/// `DELETE … RETURNING` in the Postgres implementation: an entry is removed by
/// the first caller to reach it, so a second callback with the same `state`
/// finds nothing.
#[derive(Default)]
pub struct InMemoryLoginStateStore {
    entries: Mutex<HashMap<String, (LoginState, DateTime<Utc>)>>,
}

impl InMemoryLoginStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn arc() -> Arc<dyn LoginStateStore> {
        Arc::new(Self::new())
    }
}

#[async_trait]
impl LoginStateStore for InMemoryLoginStateStore {
    async fn put(&self, state: &str, value: LoginState, ttl_secs: u32) -> Result<(), CoreError> {
        let expires_at = Utc::now() + Duration::seconds(i64::from(ttl_secs));
        self.entries
            .lock()
            .expect("login state mutex")
            .insert(state.to_owned(), (value, expires_at));
        Ok(())
    }

    async fn take(&self, state: &str) -> Result<Option<LoginState>, CoreError> {
        let taken = self
            .entries
            .lock()
            .expect("login state mutex")
            .remove(state);
        // Expiry is checked on read, not only by the prune, so a store that has
        // not been swept cannot hand back a stale login.
        Ok(taken.and_then(|(value, expires_at)| (Utc::now() < expires_at).then_some(value)))
    }

    async fn prune_expired(&self) -> Result<u64, CoreError> {
        let mut entries = self.entries.lock().expect("login state mutex");
        let before = entries.len();
        let now = Utc::now();
        entries.retain(|_, (_, expires_at)| now < *expires_at);
        Ok((before - entries.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(provider: &str) -> LoginState {
        LoginState {
            provider: provider.to_owned(),
            code_verifier: "verifier".to_owned(),
            nonce: "nonce".to_owned(),
            spa_state: "spa".to_owned(),
            return_to: None,
        }
    }

    #[tokio::test]
    async fn put_then_take_returns_the_entry() {
        let store = InMemoryLoginStateStore::new();
        store.put("s1", state("authentik"), 600).await.unwrap();
        let got = store.take("s1").await.unwrap().unwrap();
        assert_eq!(got.provider, "authentik");
        assert_eq!(got.code_verifier, "verifier");
    }

    #[tokio::test]
    async fn take_consumes_the_entry() {
        let store = InMemoryLoginStateStore::new();
        store.put("s1", state("oidc"), 600).await.unwrap();
        assert!(store.take("s1").await.unwrap().is_some());
        assert!(
            store.take("s1").await.unwrap().is_none(),
            "a callback must not be redeemable twice"
        );
    }

    #[tokio::test]
    async fn take_of_an_unknown_state_returns_none() {
        let store = InMemoryLoginStateStore::new();
        assert!(store.take("never-issued").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_expired_entry_is_not_returned_even_before_a_prune() {
        let store = InMemoryLoginStateStore::new();
        store.put("s1", state("oidc"), 0).await.unwrap();
        assert!(store.take("s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn prune_drops_only_expired_entries() {
        let store = InMemoryLoginStateStore::new();
        store.put("fresh", state("oidc"), 600).await.unwrap();
        store.put("stale", state("oidc"), 0).await.unwrap();
        assert_eq!(store.prune_expired().await.unwrap(), 1);
        assert!(store.take("fresh").await.unwrap().is_some());
    }
}
