//! Making the other replicas follow a configuration applied on one.
//!
//! The file stays the source of truth. An apply that persisted announces the
//! hash of the primary file it put in force under `reload:applied:{hash}` on
//! the cluster port every replica already polls (`drift.rs`). A replica that
//! reads a new announcement waits until its own primary file has that hash —
//! a Kubernetes Secret or a shared volume reaches each pod with some lag — and
//! then reloads it through the same path an admin's reload takes, validation
//! and rejection record included. Polled rather than pushed, for the reason
//! the port gives: a replica that was restarting catches up on its next read.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::ConfigReloadService;

/// The cluster topics an apply is announced under.
pub const APPLIED_PREFIX: &str = "reload:applied:";

/// How long a replica waits for an announced file to reach its own disk.
const FOLLOW_WINDOW: Duration = Duration::from_secs(120);

/// Who a followed reload is attributed to in `config_changes`.
const FOLLOWER: &str = "config-sync";

/// The hashes this replica still waits to see on disk, and until when.
#[derive(Clone)]
pub struct Follow {
    targets: HashSet<String>,
    deadline: Instant,
}

/// SHA-256 of a config file's bytes, hex: what an apply announces and what the
/// editor sends back as the version it started from.
pub fn content_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

impl ConfigReloadService {
    /// Tell the other replicas that the primary file `text` is now in force
    /// here. Best-effort: a replica that misses it is reported by the drift
    /// warning, as before.
    pub(super) async fn announce_applied(&self, text: &str) {
        let Some((cluster, _)) = self.cluster.get() else {
            return;
        };
        let topic = format!("{APPLIED_PREFIX}{}", content_hash(text));
        if let Err(e) = cluster.bump(&topic).await {
            tracing::warn!(error = %e, "config: could not announce the applied configuration to the other replicas");
        }
    }

    /// One step of following the other replicas, run on every heartbeat with
    /// the counters it returned.
    pub async fn follow_applied(&self, counters: &[(String, i64)]) {
        let now: HashMap<String, i64> = counters
            .iter()
            .filter(|(k, _)| k.starts_with(APPLIED_PREFIX))
            .cloned()
            .collect();
        let fresh: Vec<String> = {
            let mut seen = self
                .applied_seen
                .lock()
                .expect("applied_seen lock poisoned");
            // The first read only records: what was announced before this
            // process started is what it booted from.
            let fresh = seen.as_ref().map_or_else(Vec::new, |previous| {
                now.iter()
                    .filter(|(k, g)| previous.get(*k) != Some(g))
                    .map(|(k, _)| k[APPLIED_PREFIX.len()..].to_owned())
                    .collect()
            });
            *seen = Some(now);
            fresh
        };
        let follow = {
            let mut follow = self.follow.lock().expect("follow lock poisoned");
            if !fresh.is_empty() {
                let mut targets = follow.take().map(|f| f.targets).unwrap_or_default();
                targets.extend(fresh);
                *follow = Some(Follow {
                    targets,
                    deadline: Instant::now() + FOLLOW_WINDOW,
                });
            }
            follow.clone()
        };
        let Some(follow) = follow else {
            return;
        };

        let on_disk = match self.config_file.read().await {
            Ok(text) => content_hash(&text),
            Err(e) => {
                tracing::warn!(error = %e, "config sync: config file unreadable");
                return;
            }
        };
        if !follow.targets.contains(&on_disk) {
            if Instant::now() > follow.deadline {
                *self.follow.lock().expect("follow lock poisoned") = None;
                tracing::warn!(
                    path = %self.config_file,
                    "config sync: another replica applied a configuration this replica's file \
                     never received; it keeps serving its own (see config.replica-drift)"
                );
            }
            return;
        }
        *self.follow.lock().expect("follow lock poisoned") = None;
        let in_force = self
            .applied_layers
            .lock()
            .expect("applied_layers lock poisoned")
            .first()
            .map(|t| content_hash(t));
        if in_force.as_deref() == Some(on_disk.as_str()) {
            return; // This replica applied it, or already followed.
        }
        let reloaded = match self
            .load_pending_by(super::ReloadSource::AdminRequest, FOLLOWER)
            .await
        {
            Ok(_) => self.apply_inner(FOLLOWER, None, false).await,
            Err(e) => Err(e),
        };
        match reloaded {
            Ok(_) => {
                tracing::info!("config sync: applied the configuration another replica applied")
            }
            Err(e) => {
                tracing::warn!(error = %e, "config sync: the configuration another replica applied was not applied here")
            }
        }
    }
}
