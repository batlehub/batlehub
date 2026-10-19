//! Telling an admin that the replicas no longer serve the same configuration.
//!
//! A reload — the config editor, an approved file change — is applied by the
//! replica that handles it and by no other: the pending state is in memory and
//! the file is on each pod's own mount. Nothing makes the others follow, and
//! that is deliberate; what was missing is that nobody could see it. So every
//! replica reports a fingerprint of the configuration in force, and the config
//! warnings carry a `config.replica-drift` while the recent reports disagree.

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use batlehub_config::schema::{warnings, ConfigWarning};
use batlehub_core::{error::CoreError, ports::ClusterStatePort};

use super::ConfigReloadService;

/// How recently a replica must have reported to count. Several report periods,
/// so a replica busy for one tick is not counted out; short enough that one
/// scaled away stops counting within a minute.
const REPORT_WINDOW: chrono::Duration = chrono::Duration::seconds(30);

impl ConfigReloadService {
    /// Report this replica, as `replica`, to `cluster` and compare it with the
    /// others. Called once at startup; a second call is ignored.
    pub fn share_with(&self, cluster: Arc<dyn ClusterStatePort>, replica: String) {
        let _ = self.cluster.set((cluster, replica));
    }

    /// A digest of the configuration in force: the layers as merged, so a
    /// comment or a reformatting does not read as a difference.
    pub fn config_fingerprint(&self) -> String {
        let layers = self
            .applied_layers
            .lock()
            .expect("applied_layers lock poisoned")
            .clone();
        let document = batlehub_config::merge_layers(&layers).unwrap_or_else(|_| layers.join("\0"));
        hex::encode(Sha256::digest(document.as_bytes()))
    }

    /// Record this replica's fingerprint. Run on a timer.
    pub async fn report_to_cluster(&self) -> Result<(), CoreError> {
        let Some((cluster, replica)) = self.cluster.get() else {
            return Ok(());
        };
        cluster
            .report_config(replica, &self.config_fingerprint())
            .await
    }

    /// `config.replica-drift` when the replicas that reported recently do not
    /// all serve the same configuration.
    pub async fn replica_drift(&self) -> Option<ConfigWarning> {
        let (cluster, replica) = self.cluster.get()?;
        let since = chrono::Utc::now() - REPORT_WINDOW;
        let reports = match cluster.config_reports(since).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "config: replica reports unreadable; drift not checked");
                return None;
            }
        };
        let mine = self.config_fingerprint();
        let mut by_config: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for report in reports {
            // This replica's own row may be up to one tick old; what it serves
            // now is what counts for it.
            let fingerprint = if &report.replica == replica {
                mine.clone()
            } else {
                report.fingerprint
            };
            by_config
                .entry(fingerprint)
                .or_default()
                .push(report.replica);
        }
        if by_config.len() < 2 {
            return None;
        }
        let groups: Vec<String> = by_config
            .iter()
            .map(|(fingerprint, replicas)| {
                let here = if *fingerprint == mine {
                    " (this one)"
                } else {
                    ""
                };
                format!("{}{here} [{}]", replicas.join(", "), &fingerprint[..12])
            })
            .collect();
        Some(ConfigWarning::new(
            warnings::CONFIG_REPLICA_DRIFT,
            "replicas",
            format!(
                "the replicas serve {} different configurations: {}. A configuration applied \
                 from the editor or approved here reaches only the replica that handled the \
                 request — apply it on each replica, or restart them from the same files.",
                by_config.len(),
                groups.join("; ")
            ),
        ))
    }
}
