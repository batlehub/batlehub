/**
 * The translated label of a code the server sends raw (RFC 0050 §4.2): reason
 * codes, flag effects, audit actions. Spelled out key by key so the catalogue
 * gate sees every one. The raw code is still drawn beside its label in mono —
 * it is what an operator greps the logs and the docs for.
 */

/** A verdict's reason code → its label. */
export const REASON_LABEL_KEYS: Record<string, string> = {
  ADMIN_OVERRIDE: "reasonCode.ADMIN_OVERRIDE",
  ASSET_REPLACED: "reasonCode.ASSET_REPLACED",
  BLOCK_LIST: "reasonCode.BLOCK_LIST",
  DORMANT_RELEASE: "reasonCode.DORMANT_RELEASE",
  INSTALL_HOOK: "reasonCode.INSTALL_HOOK",
  INSTALL_HOOK_ADDED: "reasonCode.INSTALL_HOOK_ADDED",
  KNOWN_MALICIOUS: "reasonCode.KNOWN_MALICIOUS",
  LICENSE_DENIED: "reasonCode.LICENSE_DENIED",
  MALWARE_SIGNAL: "reasonCode.MALWARE_SIGNAL",
  MIN_AGE_NOT_MET: "reasonCode.MIN_AGE_NOT_MET",
  MUTABLE_REF: "reasonCode.MUTABLE_REF",
  PINNED_REF_REQUIRED: "reasonCode.PINNED_REF_REQUIRED",
  PROVENANCE_INVALID: "reasonCode.PROVENANCE_INVALID",
  PROVENANCE_MISSING: "reasonCode.PROVENANCE_MISSING",
  PROVENANCE_REMOVED: "reasonCode.PROVENANCE_REMOVED",
  PROVENANCE_UNVERIFIABLE: "reasonCode.PROVENANCE_UNVERIFIABLE",
  PUBLISHER_CHANGED: "reasonCode.PUBLISHER_CHANGED",
  RAW_SCRIPT: "reasonCode.RAW_SCRIPT",
  REPOSITORY_MOVED: "reasonCode.REPOSITORY_MOVED",
  SCANNER_ERROR: "reasonCode.SCANNER_ERROR",
  SCANNER_UNSUPPORTED: "reasonCode.SCANNER_UNSUPPORTED",
  SCAN_PENDING: "reasonCode.SCAN_PENDING",
  SIGNATURE_MISSING: "reasonCode.SIGNATURE_MISSING",
  SOC_VERDICT: "reasonCode.SOC_VERDICT",
  TAG_MOVED: "reasonCode.TAG_MOVED",
  TIMESTAMP_MISSING: "reasonCode.TIMESTAMP_MISSING",
  TYPOSQUAT_SUSPECT: "reasonCode.TYPOSQUAT_SUSPECT",
  UNPUBLISHED_UPSTREAM: "reasonCode.UNPUBLISHED_UPSTREAM",
  UNRECOGNISED: "reasonCode.UNRECOGNISED",
  UNTRUSTED_PUBLISHER: "reasonCode.UNTRUSTED_PUBLISHER",
  VULNERABILITY: "reasonCode.VULNERABILITY",
};

/** A flag's effect → its consequence, as a reader needs it. */
export const FLAG_EFFECT_LABEL_KEYS: Record<string, string> = {
  gate: "flagEffect.gate",
  hard_block: "flagEffect.hard_block",
  inform: "flagEffect.inform",
  warn: "flagEffect.warn",
};

/**
 * An audit row's action → its label, under both spellings the API emits: the
 * snake_case one (`sign_in_failed`, the database and the timeline) and the
 * squashed one serde puts in the audit-log JSON (`signinfailed`,
 * `AccessAction`'s `rename_all = "lowercase"`).
 */
const AUDIT_ACTION_SNAKE: Record<string, string> = {
  add_beta_member: "auditAction.add_beta_member",
  add_owner: "auditAction.add_owner",
  audit_lifecycle_run: "auditAction.audit_lifecycle_run",
  audit_purge: "auditAction.audit_purge",
  block: "auditAction.block",
  block_ip: "auditAction.block_ip",
  block_user: "auditAction.block_user",
  cache_clear: "auditAction.cache_clear",
  cache_coherence_dry_run: "auditAction.cache_coherence_dry_run",
  cache_coherence_run: "auditAction.cache_coherence_run",
  cache_evict: "auditAction.cache_evict",
  cache_evict_dry_run: "auditAction.cache_evict_dry_run",
  cache_evict_run: "auditAction.cache_evict_run",
  claim_namespace: "auditAction.claim_namespace",
  credential_rejected: "auditAction.credential_rejected",
  delete: "auditAction.delete",
  deprecate: "auditAction.deprecate",
  download: "auditAction.download",
  gdpr_erase: "auditAction.gdpr_erase",
  gdpr_export: "auditAction.gdpr_export",
  grant_revoke: "auditAction.grant_revoke",
  grant_write: "auditAction.grant_write",
  release_namespace: "auditAction.release_namespace",
  relist: "auditAction.relist",
  remove_beta_member: "auditAction.remove_beta_member",
  remove_owner: "auditAction.remove_owner",
  reset_quota: "auditAction.reset_quota",
  retention_dry_run: "auditAction.retention_dry_run",
  retention_reclaim: "auditAction.retention_reclaim",
  retention_run: "auditAction.retention_run",
  set_retention_pin: "auditAction.set_retention_pin",
  set_visibility: "auditAction.set_visibility",
  sign_in: "auditAction.sign_in",
  sign_in_failed: "auditAction.sign_in_failed",
  token_create: "auditAction.token_create",
  token_new_source: "auditAction.token_new_source",
  token_revoke: "auditAction.token_revoke",
  tombstone_compact: "auditAction.tombstone_compact",
  unblock: "auditAction.unblock",
  unblock_ip: "auditAction.unblock_ip",
  unblock_user: "auditAction.unblock_user",
  undeprecate: "auditAction.undeprecate",
  unlist: "auditAction.unlist",
  unyank: "auditAction.unyank",
  view_metadata: "auditAction.view_metadata",
  yank: "auditAction.yank",
};
export const AUDIT_ACTION_LABEL_KEYS: Record<string, string> = Object.fromEntries(
  Object.entries(AUDIT_ACTION_SNAKE).flatMap(([code, key]) => [
    [code, key],
    [code.replaceAll("_", ""), key],
  ]),
);
