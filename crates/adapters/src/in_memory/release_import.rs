//! In-memory [`ImportHistory`] for tests and for a deployment with no database.
//!
//! What it loses is the record surviving a restart — which is exactly the thing
//! the table exists to provide, so this is the test double and not a shipping
//! configuration. It is here because every other port has one and a web test
//! that had to stand up Postgres to render a page would be a worse test.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use batlehub_core::{entities::ImportRun, error::CoreError, ports::ImportHistory};

#[derive(Default)]
pub struct InMemoryImportHistory {
    runs: Arc<RwLock<Vec<ImportRun>>>,
}

impl InMemoryImportHistory {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ImportHistory for InMemoryImportHistory {
    async fn record(&self, run: &ImportRun) -> Result<(), CoreError> {
        self.runs.write().await.push(run.clone());
        Ok(())
    }

    async fn latest_for(&self, registry: &str) -> Result<Vec<ImportRun>, CoreError> {
        let runs = self.runs.read().await;
        // The newest run per repo, as the SQL does: a registry with two imports
        // configured into it has two answers, and collapsing them would hide a
        // repo that has been failing behind one that has not.
        let mut newest: std::collections::BTreeMap<&str, &ImportRun> =
            std::collections::BTreeMap::new();
        for run in runs.iter().filter(|r| r.registry == registry) {
            newest
                .entry(&run.repo)
                .and_modify(|held| {
                    if run.started_at > held.started_at {
                        *held = run;
                    }
                })
                .or_insert(run);
        }
        let mut out: Vec<ImportRun> = newest.into_values().cloned().collect();
        // Newest first, as the SQL's outer `ORDER BY started_at DESC` gives.
        out.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        Ok(out)
    }
}
