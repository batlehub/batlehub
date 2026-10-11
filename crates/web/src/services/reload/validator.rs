use uuid::Uuid;

use super::{ConfigReloadService, ReloadDiff};
use crate::AccessConfig;
use batlehub_core::ports::ConfigChangeRecord;
use batlehub_core::services::{audit_stream, HotConfig};

impl ConfigReloadService {
    pub(super) async fn compute_diff(
        &self,
        new_hot: &HotConfig,
        new_access: &AccessConfig,
    ) -> ReloadDiff {
        let old_hot = self.hot.read().await;
        let old_names: std::collections::HashSet<&str> =
            old_hot.registries.keys().map(String::as_str).collect();
        let new_names: std::collections::HashSet<&str> =
            new_hot.registries.keys().map(String::as_str).collect();

        let added: Vec<String> = new_names
            .difference(&old_names)
            .map(|s| s.to_string())
            .collect();
        let removed: Vec<String> = old_names
            .difference(&new_names)
            .map(|s| s.to_string())
            .collect();
        let limits_changed = old_hot.max_artifact_size_bytes != new_hot.max_artifact_size_bytes;

        // AccessConfig doesn't implement PartialEq (HashSet comparison is cheap but
        // we don't want to derive it on a potentially large struct). Mark as changed
        // conservatively whenever the registry set differs or limits change.
        let access_config_changed = !added.is_empty() || !removed.is_empty() || limits_changed || {
            let old_access = self.access.read().await;
            old_access.anonymous != new_access.anonymous
                || old_access.user != new_access.user
                || old_access.admin != new_access.admin
        };

        ReloadDiff {
            added_registries: added,
            removed_registries: removed,
            changed_registries: vec![],
            access_config_changed,
            limits_changed,
        }
    }

    pub(super) async fn persist_audit(
        &self,
        diff: &ReloadDiff,
        triggered_by: &str,
        status: &str,
        error_msg: Option<&str>,
    ) {
        // The stream line goes out whether or not there is a table to write to:
        // `config_rejected.yml` has to see a rejection on a deployment whose
        // database write failed too (RFC 0036 §6.2).
        let action = format!("config_{status}");
        let outcome = if status == "applied" {
            "allowed"
        } else {
            "denied"
        };
        let repo = match self.config_change_repo.as_ref() {
            Some(r) => r,
            None => {
                audit_stream::emit_config_change(&action, triggered_by, outcome, false);
                return;
            }
        };
        let diff_json = match serde_json::to_value(diff) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize diff for audit");
                return;
            }
        };
        let added = diff.added_registries.len();
        let removed = diff.removed_registries.len();
        let summary = format!(
            "{} registr{} added, {} removed",
            added,
            if added == 1 { "y" } else { "ies" },
            removed
        );
        let record = ConfigChangeRecord {
            id: Uuid::new_v4(),
            triggered_by: triggered_by.to_owned(),
            triggered_at: chrono::Utc::now(),
            status: status.to_owned(),
            diff: diff_json,
            summary,
            error_msg: error_msg.map(str::to_owned),
        };
        let persisted = match repo.insert(record).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "failed to persist config change audit row");
                false
            }
        };
        audit_stream::emit_config_change(&action, triggered_by, outcome, persisted);
    }
}
