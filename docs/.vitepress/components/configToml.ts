// The pure half of ConfigGenerator.vue (types, defaults, helpers and the
// TOML renderer), extracted so `docs/build/config-generator.test.ts` can run
// it under `node --test` and write the fixtures `crates/config` loads with
// the real parser. The component keeps the refs and the form; nothing here
// imports Vue.

export type RegistryMode = "proxy" | "local" | "hybrid";
export type RegistryType =
  | "npm"
  | "cargo"
  | "openvsx"
  | "vscode-marketplace"
  | "goproxy"
  | "github"
  | "forgejo"
  | "gitlab"
  | "maven"
  | "terraform"
  | "rubygems"
  | "composer"
  | "pypi"
  | "conda"
  | "nuget"
  | "deb"
  | "rpm"
  | "pacman"
  | "jetbrains"
  | "jetbrains-marketplace"
  | "generic";
export type AuthRole = "admin" | "user" | "anonymous";
export type StorageBackendType = "filesystem" | "s3";
export type StorageMode = "single" | "multi";
export type AuthType = "token" | "oidc" | "kubernetes" | "actions-oidc";
export type UpstreamAuthType = "" | "bearer" | "basic" | "header";
export type Enforcement = "block" | "warn";
export type MatchMode = "all" | "any";
export type ConditionMatchType = "auto" | "glob" | "regex";

/// RFC 0015 §4.1 — how wide the read audience is at one tier of the hierarchy.
///
/// `private` is deliberately absent: §4.9 rejects it above the package tier,
/// where it "either says nothing or says what `grants = {}` already says
/// properly", and the generator writes registry and namespace tiers only.
export type Visibility = "" | "public" | "internal" | "team";

/// RFC 0015 §4.5 — whether published bytes may be replaced.
export type Immutable = "never" | "released" | "always";

/// One `subject -> verbs` row of a `grants` table (RFC 0015 §4.2).
///
/// The subject is the TOML key: `"*"`, `"role:admin"`, `"group:oidc:team-a"`,
/// `"user:alice"`. Verbs are the closed vocabulary, comma-separated here and
/// expanded once at startup — an unknown one is a startup error, not a
/// permission granted to nobody.
export interface GrantEntry {
  id: number;
  subject: string;
  verbs: string;
}

/// RFC 0015 §4.1 — one `[[registries.namespaces]]` node.
///
/// A namespace is a *prefix* of the package coordinate, not a separate object:
/// `match = "@acme"` covers `@acme/ui` on npm, `match = "com.acme"` covers
/// `com.acme:lib` on Maven. The separator is the ecosystem's own, and matching
/// appends it — which is why a `match` ending in the separator can never match
/// and is refused at startup.
export interface Namespace {
  id: number;
  match_prefix: string;
  /// `sealed` writes `grants = {}`: the one construct that *stops* inheritance
  /// from the registry above, as opposed to an absent block (inherit) or a
  /// populated one (inherit, then widen). The three states are distinct and
  /// collapsing them is the modelling error §4.2 calls out by name.
  sealed: boolean;
  grants: GrantEntry[];
  visibility: Visibility;
  prerelease_visibility: Visibility;
  versioning_enabled: boolean;
  versioning_enforce_semver: boolean;
  versioning_allow_prerelease: boolean;
  versioning_pattern: string;
  versioning_immutable: Immutable;
  versioning_monotonic: boolean;
  versioning_dry_run: boolean;
  quota_enabled: boolean;
  quota_max_bytes: string;
  quota_max_packages: string;
  quota_warn_threshold_pct: number;
  quota_enforcement: Enforcement;
  shadow_enabled: boolean;
  shadow_until: string;
  /// Gate rules for this subtree. Absent (the checkbox off) inherits the
  /// registry's; present replaces them for everything under `match`.
  rules_enabled: boolean;
  rules: RulePolicy;
}

export interface StorageBackend {
  id: number;
  name: string;
  type: StorageBackendType;
  path: string;
  bucket: string;
  region: string;
  endpoint_url: string;
  force_path_style: boolean;
  prefix: string;
}

export interface Token {
  id: number;
  value: string;
  role: AuthRole;
  user_id: string;
}

export interface Condition {
  id: number;
  claim: string;
  pattern: string;
  match_type: ConditionMatchType;
}

export interface ActionsRule {
  id: number;
  group: string;
  group_template: string;
  role: string; // "" = omitted (group-only rule)
  match_mode: MatchMode;
  conditions: Condition[];
}

/// One `claim value -> proxy role` entry of an OIDC/Kubernetes `role_mappings`
/// table. Without at least one of these every federated identity lands on
/// `anonymous`, so a generated SSO config would have no administrator at all.
export interface RoleMapping {
  id: number;
  claim: string;
  role: string;
}

/// One entry of `[registries.rbac.groups]` — a group name (`"oidc:team-a"`, or
/// `"*:team-a"` to match the group across every provider) and its permissions.
export interface RbacGroup {
  id: number;
  name: string;
  perms: string;
}

/// One `[[registries.rate_limit.groups]]` override: a group whose members get a
/// different budget from the registry-wide one.
export interface RateLimitGroup {
  id: number;
  name: string;
  requests_per_window: number;
  window_secs: number;
  enforcement: "" | Enforcement;
}

export type NotifChannelType = "webhook" | "slack" | "teams" | "email";

export interface NotifChannel {
  id: number;
  type: NotifChannelType;
  name: string;
  url: string;
  secret: string;
  timeout_secs: number;
  // email only
  smtp_host: string;
  smtp_port: number;
  smtp_user: string;
  smtp_password: string;
  from: string;
  to: string;
  tls: boolean;
}

export interface InboundHook {
  id: number;
  name: string;
  secret: string;
}

export interface AuthProvider {
  id: number;
  type: AuthType;
  // token
  tokens: Token[];
  // oidc
  oidc_name: string;
  oidc_issuer: string;
  oidc_client_id: string;
  oidc_client_secret: string;
  oidc_redirect_uri: string;
  oidc_frontend_url: string;
  oidc_user_id_claim: string;
  oidc_role_claim: string;
  oidc_scopes: string;
  oidc_role_mappings: RoleMapping[];
  // kubernetes
  k8s_name: string;
  k8s_api_server: string;
  k8s_ca_cert_path: string;
  k8s_token_path: string;
  k8s_audiences: string;
  k8s_role_mappings: RoleMapping[];
  // actions-oidc
  actions_name: string;
  actions_issuer: string;
  /// Value the token's `aud` claim must equal. **Required, and there is no
  /// default.** The issuer is shared by every repository on the forge, so
  /// without it `iss` proves only that the caller is *a* CI job.
  actions_audience: string;
  actions_user_id_claim: string;
  actions_rules: ActionsRule[];
}

/// The seven gate rules, in the flat shape the form binds to.
///
/// Extracted so a namespace can carry its own set: `rules` is one of the
/// policies RFC 0015 §4.1 attaches to every tier, and the emit layer is
/// identical at both — only the table name differs.
export interface RulePolicy {
  rule_age_gate_enabled: boolean;
  rule_age_gate_min_age: number;
  rule_age_gate_bypass_roles: string;
  rule_age_gate_deny_missing_timestamp: boolean;
  rule_deny_latest_enabled: boolean;
  rule_deny_latest_bypass_roles: string;
  rule_signed_release_enabled: boolean;
  rule_signed_release_bypass_roles: string;
  rule_signed_release_deny_missing: boolean;
  rule_license_gate_enabled: boolean;
  rule_license_gate_allow: string;
  rule_license_gate_deny: string;
  rule_license_gate_allow_unknown: boolean;
  rule_license_gate_block: boolean;
  rule_license_gate_bypass_roles: string;
  rule_version_gate_enabled: boolean;
  rule_version_gate_allow: string;
  rule_version_gate_block: string;
  rule_version_gate_bypass_roles: string;
  rule_cve_gate_enabled: boolean;
  rule_cve_gate_min_severity: string;
  rule_cve_gate_block: boolean;
  rule_cve_gate_bypass_roles: string;
  rule_trusted_publisher_enabled: boolean;
  rule_trusted_publisher_allow: string;
  rule_trusted_publisher_bypass_roles: string;
}

export interface Registry extends RulePolicy {
  id: number;
  name: string;
  type: RegistryType;
  mode: RegistryMode;
  upstreams: string;
  storage_backend: string;
  rbac_anonymous: string;
  rbac_user: string;
  rbac_admin: string;
  rbac_groups: RbacGroup[];
  rbac_explore_anonymous: boolean;
  rbac_explore_user: boolean;
  rbac_explore_admin: boolean;
  showAdvanced: boolean;
  // routing / addressing
  hosts: string;
  path_routing: boolean;
  path_allow: string;
  index_url: string;
  search_url: string;
  search_url_disabled: boolean;
  vuln_db_url: string;
  vuln_db_url_disabled: boolean;
  // upstream auth
  upstream_auth_type: UpstreamAuthType;
  upstream_auth_token: string;
  upstream_auth_username: string;
  upstream_auth_password: string;
  upstream_auth_header_name: string;
  upstream_auth_header_value: string;
  // tls
  tls_ca_cert_path: string;
  // per-registry egress proxy
  proxy_enabled: boolean;
  proxy_url: string;
  proxy_username: string;
  proxy_password: string;
  proxy_no_proxy: string;
  // firewall
  firewall_only: boolean;
  // cache policy
  cache_metadata_ttl: number;
  cache_artifact_ttl: string;
  cache_idle_days: string;
  cache_max_size_bytes: string;
  cache_keep_latest_n: string;
  cache_serve_stale: boolean;
  cache_warm_packages: string;
  cache_warm_paths: string;
  cache_warm_latest_n: number;
  cache_warm_concurrency: number;
  // rate limit
  rate_limit_enabled: boolean;
  rate_limit_rps: number;
  rate_limit_window: number;
  rate_limit_enforcement: Enforcement;
  rate_limit_groups: RateLimitGroup[];
  // quota (local/hybrid)
  quota_enabled: boolean;
  quota_max_bytes: string;
  quota_max_packages: string;
  quota_warn_threshold_pct: number;
  quota_enforcement: Enforcement;
  // beta channel (local/hybrid)
  beta_channel_enabled: boolean;
  // versioning (local/hybrid)
  versioning_enabled: boolean;
  versioning_enforce_semver: boolean;
  versioning_allow_prerelease: boolean;
  versioning_pattern: string;
  /// RFC 0015 §4.5. Not a verb: immutability is a property of the resource,
  /// which is what lets a node be append-only for everyone, admins included.
  versioning_immutable: Immutable;
  versioning_monotonic: boolean;
  versioning_dry_run: boolean;
  // RFC 0015 — grants and the tiered policy hierarchy
  grants: GrantEntry[];
  namespaces: Namespace[];
  grants_shadow_enabled: boolean;
  grants_shadow_until: string;
  visibility: Visibility;
  prerelease_visibility: Visibility;
  // RFC 0016 — retention of locally published versions (local/hybrid)
  retention_enabled: boolean;
  retention_keep_versions: string;
  retention_keep_for_days: string;
  retention_keep_if_pulled_days: string;
  retention_keep_yanked: boolean;
  retention_download_signal_floor_days: string;
  retention_reclaim_delay_ms: number;
  retention_tombstone_detail_for_days: string;
  retention_dry_run: boolean;
  // RFC 0012 — signed, expiring download URLs in this registry's documents
  signed_downloads: boolean;
  // README capture (RFC 0007-bis)
  readme_customised: boolean;
  readme_enabled: boolean;
  readme_from_archive: boolean;
  readme_max_bytes: number;
  readme_remote_images: "strip" | "proxy";
  readme_remote_image_hosts: string;
  readme_image_max_bytes: number;
  // The console's upstream discovery read
  upstream_detail_customised: boolean;
  upstream_detail_enabled: boolean;
  upstream_detail_max_versions: number;
  upstream_detail_negative_ttl_secs: number;
  /// Whether the console may pull a version this instance does not hold yet.
  console_fetch: boolean;
  /// goproxy only: the checksum database to proxy. Blank string disables
  /// `/sumdb/{path}`, which is what a private-module registry wants.
  sumdb_url: string;
  sumdb_disabled: boolean;
  // signing (local/hybrid)
  signing_enabled: boolean;
  signing_required: boolean;
  signing_allowed_types: string;
  signing_verify_on_download: boolean;
  signing_trusted_keys: string;
  // repo metadata signing (deb/rpm)
  repo_signing_enabled: boolean;
  repo_signing_seed_hex: string;
  repo_signing_user_id: string;
  repo_signing_created: string;
  // [registries.vsx_signing] — RFC 0020
  vsx_signing_enabled: boolean;
  vsx_signing_seed_hex: string;
  vsx_signing_key_id: string;
  // sbom
  sbom_enabled: boolean;
  sbom_formats: string;
  sbom_required: boolean;
  sbom_fetch_upstream: boolean;
  // integrity
  integrity_customised: boolean;
  integrity_enabled: boolean;
  integrity_block_on_mismatch: boolean;
  integrity_require_metadata: boolean;
  integrity_bypass_roles: string;
  integrity_verify_on_serve: boolean;
  // rules
  // feature flags
  feature_flags_socket_badge: boolean;
}

// ── State ───────────────────────────────────────────────────────────────────

// Mirrors `CURRENT_CONFIG_VERSION` in crates/config/src/schema/mod.rs. Bump both
// together: a config declaring a version the binary does not know is rejected.

// Mirrors `CURRENT_CONFIG_VERSION` in crates/config/src/schema/mod.rs. Bump both
// together: a config declaring a version the binary does not know is rejected.
export const CONFIG_VERSION = 1;

/** Id counters shared by the factories below and the form that adds rows. */
export const seq = { channel: 0, inbound: 0, backend: 0, auth: 0, token: 0, rule: 0, cond: 0, mapping: 0, registry: 0 };

export function blankAuthProvider(): AuthProvider {
  return {
    id: seq.auth++,
    type: "token",
    tokens: [],
    oidc_name: "",
    oidc_issuer: "",
    oidc_client_id: "",
    oidc_client_secret: "",
    oidc_redirect_uri: "",
    oidc_frontend_url: "",
    oidc_user_id_claim: "sub",
    oidc_role_claim: "role",
    oidc_scopes: "",
    oidc_role_mappings: [{ id: seq.mapping++, claim: "batlehub-admins", role: "admin" }],
    k8s_name: "",
    k8s_api_server: "",
    k8s_ca_cert_path: "",
    k8s_token_path: "",
    k8s_audiences: "batlehub",
    k8s_role_mappings: [
      { id: seq.mapping++, claim: "system:serviceaccount:ci:builder", role: "user" },
    ],
    actions_name: "",
    // The one issuer GitHub Actions ever signs with; pre-filled because a blank
    // `issuer_url` is a `missing field` parse error, not a warning.
    actions_issuer: "https://token.actions.githubusercontent.com",
    actions_audience: "",
    actions_user_id_claim: "sub",
    actions_rules: [],
  };
}


export const defaultUpstream: Record<RegistryType, string> = {
  npm: "https://registry.npmjs.org",
  cargo: "https://index.crates.io",
  openvsx: "https://open-vsx.org",
  "vscode-marketplace": "https://marketplace.visualstudio.com",
  goproxy: "https://proxy.golang.org",
  github: "https://api.github.com",
  forgejo: "https://codeberg.org",
  gitlab: "https://gitlab.com",
  maven: "https://repo1.maven.org/maven2",
  terraform: "https://registry.terraform.io",
  rubygems: "https://rubygems.org",
  composer: "https://repo.packagist.org",
  pypi: "https://pypi.org",
  conda: "https://conda.anaconda.org",
  nuget: "https://api.nuget.org",
  // Deb has a canonical Debian mirror; RPM and generic have no universal default
  // upstream, so they are left blank for the user to fill in (e.g. a
  // Fedora/openSUSE mirror). The backend rejects those two at startup when the
  // list is empty rather than falling back to an unreachable placeholder.
  deb: "https://deb.debian.org",
  rpm: "",
  pacman: "https://geo.mirror.pkgbuild.com",
  jetbrains: "https://download.jetbrains.com",
  "jetbrains-marketplace": "https://plugins.jetbrains.com",
  generic: "",
};

// Kinds addressed purely by upstream file path — the only ones for which
// `path_allow` and `cache.warm_paths` mean anything (the backend rejects
// `path_allow` on any other kind). Mirrors `RegistryKind::is_path_addressed`.
export const PATH_ADDRESSED_TYPES = new Set<RegistryType>([
  "deb",
  "rpm",
  "pacman",
  "jetbrains",
  "generic",
]);
export const isPathAddressed = (reg: Registry) => PATH_ADDRESSED_TYPES.has(reg.type);

// `deb`/`rpm` registries are the ones that publish signed repository metadata.
export const REPO_SIGNING_TYPES = new Set<RegistryType>(["deb", "rpm"]);
// RFC 0020: the registry's own VSIX signature, for the two gallery kinds.
export const VSX_SIGNING_TYPES = new Set<RegistryType>(["openvsx", "vscode-marketplace"]);

export function defaultRegistry(type: RegistryType = "npm"): Registry {
  return {
    id: seq.registry++,
    name: type,
    type,
    mode: "proxy",
    upstreams: defaultUpstream[type],
    storage_backend: "",
    rbac_anonymous: "releases:read, source:read",
    rbac_user: "releases:read, source:read",
    rbac_admin: "*",
    rbac_groups: [],
    rbac_explore_anonymous: true,
    rbac_explore_user: true,
    rbac_explore_admin: true,
    showAdvanced: false,
    hosts: "",
    path_routing: true,
    // `generic` is the one kind the backend refuses to start without an
    // allowlist, so it is seeded with the explicit mirror-everything opt-out
    // rather than a blank field that fails validation.
    path_allow: type === "generic" ? "**" : "",
    index_url: "",
    search_url: "",
    search_url_disabled: false,
    vuln_db_url: "",
    vuln_db_url_disabled: false,
    upstream_auth_type: "",
    upstream_auth_token: "",
    upstream_auth_username: "",
    upstream_auth_password: "",
    upstream_auth_header_name: "",
    upstream_auth_header_value: "",
    tls_ca_cert_path: "",
    proxy_enabled: false,
    proxy_url: "",
    proxy_username: "",
    proxy_password: "",
    proxy_no_proxy: "",
    firewall_only: false,
    cache_metadata_ttl: 300,
    cache_artifact_ttl: "",
    cache_idle_days: "",
    cache_max_size_bytes: "",
    cache_keep_latest_n: "",
    cache_serve_stale: true,
    cache_warm_packages: "",
    cache_warm_paths: "",
    cache_warm_latest_n: 1,
    cache_warm_concurrency: 2,
    rate_limit_enabled: false,
    rate_limit_rps: 100,
    rate_limit_window: 60,
    rate_limit_enforcement: "block",
    rate_limit_groups: [],
    quota_enabled: false,
    quota_max_bytes: "",
    quota_max_packages: "",
    quota_warn_threshold_pct: 80,
    quota_enforcement: "block",
    beta_channel_enabled: false,
    versioning_enabled: false,
    versioning_enforce_semver: false,
    versioning_allow_prerelease: true,
    versioning_pattern: "",
    versioning_immutable: "never",
    versioning_monotonic: false,
    versioning_dry_run: false,
    grants: [],
    namespaces: [],
    grants_shadow_enabled: false,
    grants_shadow_until: "",
    visibility: "",
    prerelease_visibility: "",
    retention_enabled: false,
    retention_keep_versions: "",
    retention_keep_for_days: "",
    retention_keep_if_pulled_days: "",
    retention_keep_yanked: true,
    retention_download_signal_floor_days: "",
    retention_reclaim_delay_ms: 0,
    retention_tombstone_detail_for_days: "",
    // RFC 0016's one dry-run that defaults to on: reclaiming is the only
    // irreversible half of the three, so the safe direction is unambiguous.
    retention_dry_run: true,
    signed_downloads: false,
    readme_customised: false,
    readme_enabled: true,
    readme_from_archive: true,
    readme_max_bytes: 262144,
    readme_remote_images: "strip",
    readme_remote_image_hosts: "",
    readme_image_max_bytes: 2097152,
    upstream_detail_customised: false,
    upstream_detail_enabled: true,
    upstream_detail_max_versions: 300,
    upstream_detail_negative_ttl_secs: 300,
    console_fetch: true,
    sumdb_url: "",
    sumdb_disabled: false,
    signing_enabled: false,
    signing_required: false,
    signing_allowed_types: "",
    signing_verify_on_download: false,
    signing_trusted_keys: "",
    repo_signing_enabled: false,
    repo_signing_seed_hex: "",
    repo_signing_user_id: "",
    repo_signing_created: "",
    vsx_signing_enabled: false,
    vsx_signing_seed_hex: "",
    vsx_signing_key_id: "",
    sbom_enabled: false,
    sbom_formats: "spdx, cyclonedx",
    sbom_required: false,
    sbom_fetch_upstream: true,
    integrity_customised: false,
    integrity_enabled: true,
    integrity_block_on_mismatch: true,
    integrity_require_metadata: false,
    integrity_bypass_roles: "admin",
    integrity_verify_on_serve: false,
    ...blankRulePolicy(),
    feature_flags_socket_badge: true,
  };
}


export function defaultState() {
  return {
    server: {
  host: "0.0.0.0",
  port: 8080,
  static_dir: "",
  cli_binary_path: "",
  cors_allowed_origins: "",
  // `[server].trusted_proxies` supersedes the deprecated `[ip_blocking]` key of
  // the same name; setting it here is what stops the server logging the
  // deprecation warning at startup. Unlike the old key an empty *list* is a
  // policy ("trust nobody"), so the checkbox decides whether the key is written
  // at all and the text decides its contents.
  trusted_proxies_set: false,
  trusted_proxies: "",
},
    database: {
  url: "",
  max_connections: 10,
  min_connections: 1,
  acquire_timeout_secs: 30,
},
    metaCache: { type: "memory", url: "" },
    limits: { max_artifact_size_bytes: "" },
    ipBlocking: {
  enabled: false,
  violation_threshold: 10,
  violation_window_secs: 300,
  ban_duration_secs: 3600,
  trigger_on_status: "429, 401",
},
    vulnerabilityScan: {
  enabled: false,
  interval_secs: 86400,
  osv_api_url: "",
  batch_size: 100,
},
    stats: {
  history_enabled: true,
  // 30, matching `default_history_retention_days` — showing any other number
  // here would promise a retention the server does not apply, since the key is
  // only written when it differs from the default.
  history_retention_days: 30,
  metrics_enabled: true,
},
    subdomainRouting: {
  enabled: false,
  base_domain: "",
  scheme: "https",
},
    instanceGrants: [] as GrantEntry[],
    cacheCoherence: {
  enabled: false,
  interval_secs: 86400,
},
    search: {
  readmes: false,
  text_config: "english",
},
    signedUrls: {
  enabled: false,
  secret: "",
  ttl_seconds: 300,
  previous_secrets: "",
},
    upstreamProxy: {
  enabled: false,
  url: "",
  username: "",
  password: "",
  no_proxy: "",
},
    notifications: {
  enabled: false,
  channels: [] as NotifChannel[],
  inbound: [] as InboundHook[],
},
    storageMode: "single" as StorageMode,
    singleStorage: {
  type: "filesystem",
  path: "./cache",
  bucket: "",
  region: "us-east-1",
  endpoint_url: "",
  force_path_style: false,
  prefix: "",
} as {
  type: StorageBackendType;
  path: string;
  bucket: string;
  region: string;
  endpoint_url: string;
  force_path_style: boolean;
  prefix: string;
},
    storageDefault: "primary",
    storageBackends: [
  {
    id: seq.backend++,
    name: "primary",
    type: "filesystem",
    path: "./cache",
    bucket: "",
    region: "us-east-1",
    endpoint_url: "",
    force_path_style: false,
    prefix: "",
  },
] as StorageBackend[],
    otel: {
  enabled: false,
  endpoint: "http://localhost:4317",
  service_name: "batlehub",
},
    authProviders: [
  {
    ...blankAuthProvider(),
    tokens: [{ id: seq.token++, value: "", role: "admin", user_id: "admin" }],
  },
] as AuthProvider[],
    tokenHashes: {} as Record<number, string | null>,
    registries: [defaultRegistry("npm")] as Registry[],
  };
}

export type GeneratorState = ReturnType<typeof defaultState>;

// ── Helpers ─────────────────────────────────────────────────────────────────

export function q(s: string) {
  return `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

export function permsToToml(csv: string): string {
  const perms = csv
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean);
  if (!perms.length) return "[]";
  return `[${perms.map(q).join(", ")}]`;
}

export function csvToList(csv: string): string[] {
  return csv
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/// The character that separates a namespace prefix from what lies under it, per
/// ecosystem. Mirrors `namespace_separator` in
/// `crates/core/src/entities/grant.rs` — matching appends it, so a `match` that
/// already ends with it can never match and is refused at startup.
export function namespaceSeparator(type: RegistryType): string {
  if (type === "openvsx" || type === "vscode-marketplace" || type === "nuget")
    return ".";
  if (type === "maven") return ":";
  return "/";
}

/// The ecosystem-scoped verbs, and the registry types that define them.
///
/// RFC 0015 §4.2 rule 2: granting one on a registry that does not define it is
/// rejected at config load, because "I granted it and nothing happened" is the
/// failure the closed vocabulary exists to remove.
export const ECOSYSTEM_VERBS: Record<string, RegistryType[]> = {
  "npm:dist-tags:write": ["npm"],
  "openvsx:namespace:claim": ["openvsx", "vscode-marketplace"],
  "terraform:signing-keys:write": ["terraform"],
  "jetbrains:channel:assign": ["jetbrains", "jetbrains-marketplace"],
};

/// Verbs in `csv` that this registry type does not define — the §4.2 rule 2
/// offenders, named so the form can say so before the server refuses to boot.
export function wrongKindVerbs(csv: string, type: RegistryType): string[] {
  return csvToList(csv).filter((v) => {
    const kinds = ECOSYSTEM_VERBS[v];
    return kinds !== undefined && !kinds.includes(type);
  });
}

/// Visibility ordered widest to narrowest, matching the `Visibility` enum's own
/// declaration order — the ordering is load-bearing there and pinned by a test,
/// so it is the same order here.
export const VISIBILITY_ORDER: Visibility[] = ["public", "internal", "team"];

/// Whether pre-releases would reach a *wider* audience than releases.
///
/// Legal, and warned about at startup rather than refused, because it is almost
/// always a typo: the setting exists to do the opposite. An unset registry
/// visibility means `public`, which is the behaviour on every existing instance.
export function prereleaseWiderThanRelease(
  visibility: Visibility,
  prerelease: Visibility,
): boolean {
  if (!prerelease) return false;
  const release = visibility || "public";
  return VISIBILITY_ORDER.indexOf(prerelease) < VISIBILITY_ORDER.indexOf(release);
}

/// A shadow expiry that is blank or already past. Both are startup errors: a
/// shadow serves every request that would have been refused, so one with no
/// future end is a permanent bypass.
export function shadowDateInvalid(until: string): boolean {
  if (!until) return true;
  const d = new Date(`${until}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return true;
  return d.getTime() <= Date.now();
}

/// Emit a rule policy as `[[<table>]]` entries.
///
/// One function for both tiers: `[[registries.rules]]` and
/// `[[registries.namespaces.rules]]` deserialise into the same `Vec<RuleConfig>`
/// and differ only in the table name. Two copies of this would be two places for
/// a rule's keys to drift apart.
export function rulesToToml(r: RulePolicy, table: string): string[] {
  const out: string[] = [];
  const open = () => {
    out.push("");
    out.push(`[[${table}]]`);
  };
  const bypass = (csv: string) => {
    const roles = csvToList(csv);
    if (roles.length) out.push(`bypass_roles = [${roles.map(q).join(", ")}]`);
  };
  if (r.rule_age_gate_enabled) {
    open();
    out.push(`kind = "release_age_gate"`);
    out.push(`min_age_secs = ${r.rule_age_gate_min_age}`);
    if (r.rule_age_gate_deny_missing_timestamp)
      out.push(`deny_missing_timestamp = true`);
    bypass(r.rule_age_gate_bypass_roles);
  }
  if (r.rule_deny_latest_enabled) {
    open();
    out.push(`kind = "deny_latest"`);
    bypass(r.rule_deny_latest_bypass_roles);
  }
  if (r.rule_signed_release_enabled) {
    open();
    out.push(`kind = "require_signed_release"`);
    out.push(`enabled = true`);
    if (r.rule_signed_release_deny_missing)
      out.push(`deny_missing_signature = true`);
    bypass(r.rule_signed_release_bypass_roles);
  }
  if (r.rule_license_gate_enabled) {
    open();
    out.push(`kind = "license_gate"`);
    const licAllow = csvToList(r.rule_license_gate_allow);
    const licDeny = csvToList(r.rule_license_gate_deny);
    if (licAllow.length) out.push(`allow = ${tomlArray(licAllow)}`);
    if (licDeny.length) out.push(`deny = ${tomlArray(licDeny)}`);
    if (!r.rule_license_gate_allow_unknown) out.push(`allow_unknown = false`);
    if (r.rule_license_gate_block) out.push(`block = true`);
    bypass(r.rule_license_gate_bypass_roles);
  }
  if (r.rule_version_gate_enabled) {
    // Each entry may itself contain a comma (">=1.2.0, <2.0.0"), so these two
    // are one-per-line fields rather than comma-separated ones.
    const verAllow = linesToList(r.rule_version_gate_allow);
    const verBlock = linesToList(r.rule_version_gate_block);
    if (verAllow.length || verBlock.length) {
      open();
      out.push(`kind = "version_gate"`);
      if (verAllow.length) out.push(`allow = ${tomlArray(verAllow)}`);
      if (verBlock.length) out.push(`block = ${tomlArray(verBlock)}`);
      bypass(r.rule_version_gate_bypass_roles);
    }
  }
  if (r.rule_cve_gate_enabled) {
    open();
    out.push(`kind = "cve_gate"`);
    out.push(`min_severity = ${q(r.rule_cve_gate_min_severity)}`);
    if (r.rule_cve_gate_block) out.push(`block = true`);
    bypass(r.rule_cve_gate_bypass_roles);
  }
  if (r.rule_trusted_publisher_enabled) {
    const allow = csvToList(r.rule_trusted_publisher_allow);
    if (allow.length) {
      open();
      out.push(`kind = "trusted_publisher"`);
      out.push(`allow = [${allow.map(q).join(", ")}]`);
      bypass(r.rule_trusted_publisher_bypass_roles);
    }
  }
  return out;
}

/// The rule defaults, in one place, so a registry and a namespace start from the
/// same policy rather than from two lists that drift.
export function blankRulePolicy(): RulePolicy {
  return {
    rule_age_gate_enabled: false,
    rule_age_gate_min_age: 3600,
    rule_age_gate_bypass_roles: "admin",
    rule_age_gate_deny_missing_timestamp: false,
    rule_deny_latest_enabled: false,
    rule_deny_latest_bypass_roles: "admin",
    rule_signed_release_enabled: false,
    rule_signed_release_bypass_roles: "admin",
    rule_signed_release_deny_missing: false,
    rule_license_gate_enabled: false,
    rule_license_gate_allow: "",
    rule_license_gate_deny: "",
    rule_license_gate_allow_unknown: true,
    rule_license_gate_block: false,
    rule_license_gate_bypass_roles: "admin",
    rule_version_gate_enabled: false,
    rule_version_gate_allow: "",
    rule_version_gate_block: "",
    rule_version_gate_bypass_roles: "admin",
    rule_cve_gate_enabled: false,
    rule_cve_gate_min_severity: "high",
    rule_cve_gate_block: false,
    rule_cve_gate_bypass_roles: "admin",
    rule_trusted_publisher_enabled: false,
    rule_trusted_publisher_allow: "",
    rule_trusted_publisher_bypass_roles: "admin",
  };
}

/// Whether a retention block would actually veto anything.
///
/// Mirrors `RetentionConfig::reclaims_anything()` plus the
/// `tombstone_detail_for_days` escape hatch. `keep_yanked` deliberately does not
/// count: it defaults to true and would make an otherwise-empty block look
/// configured while still reclaiming every unyanked version on the first run.
export function retentionReclaims(reg: Registry): boolean {
  return !!(
    reg.retention_keep_versions ||
    reg.retention_keep_for_days ||
    reg.retention_keep_if_pulled_days ||
    reg.retention_tombstone_detail_for_days
  );
}

/// Emit a `grants` table as `subject = [verbs]` rows.
///
/// Rows with a blank subject are dropped rather than emitted empty: a grant
/// keyed on `""` names nobody, and the union of nothing grants nothing.
export function grantRows(entries: GrantEntry[]): GrantEntry[] {
  return entries.filter((g) => g.subject.trim());
}

/// Splits a textarea's contents into one entry per line. Used where an entry may
/// legitimately contain a comma (glob patterns, semver ranges like
/// `">=1.2.0, <2.0.0"`), which rules out the comma-separated inputs.
export function linesToList(text: string): string[] {
  return text
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
}

export function tomlArray(items: string[]): string {
  return `[${items.map(q).join(", ")}]`;
}

/// Comma-separated numbers as an *unquoted* TOML array. `trigger_on_status` is
/// a `Vec<u16>` on the Rust side, so quoting the entries makes the whole config
/// fail to parse ("invalid type: string, expected u16"). Non-numeric input is
/// dropped rather than passed through, for the same reason.
export function numListToToml(csv: string): string {
  const nums = csv
    .split(",")
    .map((n) => n.trim())
    .filter((n) => /^\d+$/.test(n));
  return `[${nums.join(", ")}]`;
}

export function listToToml(csv: string): string {
  const items = csv
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean);
  if (!items.length) return "[]";
  return `[${items.map(q).join(", ")}]`;
}

export function backendFields(b: {
  type: StorageBackendType;
  path: string;
  bucket: string;
  region: string;
  endpoint_url: string;
  force_path_style: boolean;
  prefix: string;
}): string[] {
  const lines: string[] = [];
  lines.push(`type = ${q(b.type)}`);
  if (b.type === "filesystem") {
    lines.push(`path = ${q(b.path || "./cache")}`);
  } else {
    lines.push(`bucket = ${q(b.bucket)}`);
    lines.push(`region = ${q(b.region)}`);
    if (b.prefix) lines.push(`prefix = ${q(b.prefix)}`);
    if (b.endpoint_url) lines.push(`endpoint_url = ${q(b.endpoint_url)}`);
    if (b.force_path_style) lines.push(`force_path_style = true`);
  }
  return lines;
}

// ── TOML generation ─────────────────────────────────────────────────────────

/// Whether anything routes on the `Forwarded` / `X-Forwarded-Host` header —
/// wildcard derivation from a base domain, or a registry claiming vanity hosts.
///
/// The server refuses to start in that state without a declared trusted-proxy
/// policy, because routing on a header nothing vouches for lets any client pick
/// the registry it lands on. The two controls used to be independent here, so
/// the form could emit a config that never loads.
/// Whether any registry mints signed download URLs, which makes
/// `[server.signed_urls]` mandatory rather than optional.

export function signedDownloadsUsedFor(s: GeneratorState): boolean {
  return s.registries.some((r) => r.signed_downloads);
}

/// A signing secret below the HMAC-SHA256 minimum. Byte length, not character
/// count: a 32-character string of multi-byte characters is fine while a
/// 20-character ASCII one is not. A `${VAR}` placeholder is exempt — it is
/// expanded before the file is parsed, so what is typed here is not the key.
export function signingSecretTooShortFor(s: GeneratorState): boolean {
  const secret = s.signedUrls.secret;
  if (!secret || /^\$\{[^}]+\}$/.test(secret)) return false;
  return new TextEncoder().encode(secret).length < 32;
}

export function hostRoutingEnabledFor(s: GeneratorState): boolean {
  return (
    (s.subdomainRouting.enabled && !!s.subdomainRouting.base_domain.trim()) ||
    s.registries.some((r) => r.hosts.trim())
  );
}

/// The TOML the generator writes, from a plain snapshot of the form. Pure:
/// the same state renders the same file, which is what the harness holds.
export function renderConfigToml(s: GeneratorState): string {
  const lines: string[] = [];

  // Declares which schema generation this file targets. The server refuses a
  // version newer than it understands instead of silently ignoring keys.
  lines.push(`config_version = ${CONFIG_VERSION}`);
  lines.push("");

  // [server]
  lines.push("[server]");
  lines.push(`host = ${q(s.server.host)}`);
  lines.push(`port = ${s.server.port}`);
  if (s.server.static_dir)
    lines.push(`static_dir = ${q(s.server.static_dir)}`);
  if (s.server.cli_binary_path)
    lines.push(`cli_binary_path = ${q(s.server.cli_binary_path)}`);
  if (s.server.cors_allowed_origins) {
    lines.push(`cors_allowed_origins = ${listToToml(s.server.cors_allowed_origins)}`);
  }
  // Host-based routing makes this mandatory, not optional: `validate()` bails
  // without it. An empty list is itself a policy — "trust nobody, always use the
  // TCP peer address" — which is the right default for a server exposed
  // directly, and is what the config's own error message suggests.
  if (s.server.trusted_proxies_set || hostRoutingEnabledFor(s)) {
    lines.push(`trusted_proxies = ${listToToml(s.server.trusted_proxies)}`);
  }

  // [server.signed_urls] — RFC 0012. Emitted whenever a registry asks for signed
  // downloads, because that pairing without a secret is a startup error: a
  // registry that believes it is closed and is not is what the feature exists to
  // prevent.
  if (s.signedUrls.enabled || signedDownloadsUsedFor(s)) {
    lines.push("");
    lines.push("[server.signed_urls]");
    lines.push(`secret = ${q(s.signedUrls.secret)}`);
    if (s.signedUrls.ttl_seconds !== 300)
      lines.push(`ttl_seconds = ${s.signedUrls.ttl_seconds}`);
    const previous = csvToList(s.signedUrls.previous_secrets);
    if (previous.length)
      lines.push(`previous_secrets = ${tomlArray(previous)}`);
  }

  // [database]
  lines.push("");
  lines.push("[database]");
  lines.push(`type = "postgresql"`);
  lines.push(
    `url = ${q(s.database.url || "postgresql://batlehub:changeme@localhost:5432/batlehub")}`,
  );
  if (s.database.max_connections !== 10)
    lines.push(`max_connections = ${s.database.max_connections}`);
  if (s.database.min_connections !== 1)
    lines.push(`min_connections = ${s.database.min_connections}`);
  if (s.database.acquire_timeout_secs !== 30)
    lines.push(`acquire_timeout_secs = ${s.database.acquire_timeout_secs}`);

  // [cache]
  if (s.metaCache.type !== "memory" || s.metaCache.url) {
    lines.push("");
    lines.push("[cache]");
    lines.push(`type = ${q(s.metaCache.type)}`);
    if (s.metaCache.type === "redis" && s.metaCache.url) {
      lines.push(`url = ${q(s.metaCache.url)}`);
    }
  }

  // [limits]
  if (s.limits.max_artifact_size_bytes) {
    lines.push("");
    lines.push("[limits]");
    lines.push(
      `max_artifact_size_bytes = ${s.limits.max_artifact_size_bytes}`,
    );
  }

  // [[auth]]
  for (const auth of s.authProviders) {
    lines.push("");
    lines.push("[[auth]]");
    lines.push(`type = ${q(auth.type)}`);

    if (auth.type === "token") {
      const valid = auth.tokens.filter((t) => t.value.trim());
      for (const tok of valid) {
        lines.push("");
        lines.push("[[auth.tokens]]");
        const hash = s.tokenHashes[tok.id];
        if (hash === null) {
          lines.push(`value = "# computing Argon2id hash…"`);
        } else if (hash && hash.startsWith("$argon2")) {
          lines.push(`value = ${q(hash)}`);
        } else {
          lines.push(`# Argon2id hashing unavailable in this browser.`);
          lines.push(`# Harden this token: batlehub hash-token ${tok.value}`);
          lines.push(`value = ${q(tok.value)}`);
        }
        lines.push(`role = ${q(tok.role)}`);
        if (tok.user_id) lines.push(`user_id = ${q(tok.user_id)}`);
      }
    } else if (auth.type === "oidc") {
      if (auth.oidc_name) lines.push(`name = ${q(auth.oidc_name)}`);
      // Neither key has a serde default, so omitting one is a `missing field`
      // parse error naming the whole `[[auth]]` table. Emitting them blank keeps
      // the diagnostic on the key the operator still has to fill.
      lines.push(`issuer_url = ${q(auth.oidc_issuer)}`);
      lines.push(`client_id = ${q(auth.oidc_client_id)}`);
      if (auth.oidc_client_secret)
        lines.push(`client_secret = ${q(auth.oidc_client_secret)}`);
      if (auth.oidc_redirect_uri)
        lines.push(`redirect_uri = ${q(auth.oidc_redirect_uri)}`);
      if (auth.oidc_frontend_url)
        lines.push(`frontend_url = ${q(auth.oidc_frontend_url)}`);
      if (auth.oidc_user_id_claim && auth.oidc_user_id_claim !== "sub")
        lines.push(`user_id_claim = ${q(auth.oidc_user_id_claim)}`);
      if (auth.oidc_role_claim && auth.oidc_role_claim !== "role")
        lines.push(`role_claim = ${q(auth.oidc_role_claim)}`);
      if (auth.oidc_scopes) {
        const scopes = auth.oidc_scopes.split(",").map((s) => s.trim()).filter(Boolean);
        if (scopes.length) lines.push(`scopes = [${scopes.map(q).join(", ")}]`);
      }
      // Claim values with no mapping fall back to `anonymous`, so a provider
      // with no entries here authenticates people into having no rights at all.
      const oidcMappings = auth.oidc_role_mappings.filter((m) => m.claim.trim());
      if (oidcMappings.length) {
        lines.push("");
        lines.push("[auth.role_mappings]");
        for (const m of oidcMappings) {
          lines.push(`${q(m.claim.trim())} = ${q(m.role)}`);
        }
      }
    } else if (auth.type === "kubernetes") {
      if (auth.k8s_name) lines.push(`name = ${q(auth.k8s_name)}`);
      if (auth.k8s_api_server)
        lines.push(`api_server = ${q(auth.k8s_api_server)}`);
      if (auth.k8s_ca_cert_path)
        lines.push(`ca_cert_path = ${q(auth.k8s_ca_cert_path)}`);
      if (auth.k8s_token_path)
        lines.push(`token_path = ${q(auth.k8s_token_path)}`);
      if (auth.k8s_audiences) {
        const auds = auth.k8s_audiences
          .split(",")
          .map((a) => a.trim())
          .filter(Boolean);
        lines.push(`audiences = [${auds.map(q).join(", ")}]`);
      }
      // Keys are Kubernetes usernames (`system:serviceaccount:<ns>:<name>`) or
      // group names; unmapped identities land on `anonymous`.
      const k8sMappings = auth.k8s_role_mappings.filter((m) => m.claim.trim());
      if (k8sMappings.length) {
        lines.push("");
        lines.push("[auth.role_mappings]");
        for (const m of k8sMappings) {
          lines.push(`${q(m.claim.trim())} = ${q(m.role)}`);
        }
      }
    } else if (auth.type === "actions-oidc") {
      if (auth.actions_name) lines.push(`name = ${q(auth.actions_name)}`);
      // `issuer_url` and `audience` have no serde default: omitting either is a
      // `missing field` parse error rather than something the server can warn
      // about. Emit both unconditionally so a half-filled form produces the
      // config's *own* diagnostic ("`audience` must not be blank"), which names
      // the key to fill, instead of a parse error that names the whole table.
      lines.push(`issuer_url = ${q(auth.actions_issuer)}`);
      lines.push(`audience = ${q(auth.actions_audience)}`);
      if (auth.actions_user_id_claim && auth.actions_user_id_claim !== "sub")
        lines.push(`user_id_claim = ${q(auth.actions_user_id_claim)}`);
      for (const rule of auth.actions_rules) {
        lines.push("");
        lines.push("[[auth.rules]]");
        if (rule.group) lines.push(`group = ${q(rule.group)}`);
        if (rule.group_template) lines.push(`group_template = ${q(rule.group_template)}`);
        if (rule.role) lines.push(`role = ${q(rule.role)}`);
        if (rule.match_mode !== "all") lines.push(`match = ${q(rule.match_mode)}`);
        for (const cond of rule.conditions) {
          lines.push("");
          lines.push("[[auth.rules.conditions]]");
          lines.push(`claim = ${q(cond.claim)}`);
          lines.push(`pattern = ${q(cond.pattern)}`);
          if (cond.match_type !== "auto") lines.push(`match_type = ${q(cond.match_type)}`);
        }
      }
    }
  }

  // [storage]
  lines.push("");
  if (s.storageMode === "single") {
    lines.push("[storage]");
    for (const l of backendFields(s.singleStorage)) lines.push(l);
  } else {
    lines.push("[storage]");
    lines.push(`default = ${q(s.storageDefault)}`);
    for (const b of s.storageBackends) {
      if (!b.name) continue;
      lines.push("");
      lines.push("[[storage.backends]]");
      lines.push(`name = ${q(b.name)}`);
      for (const l of backendFields(b)) lines.push(l);
    }
  }

  // [[registries]]
  for (const reg of s.registries) {
    if (!reg.name) continue;
    lines.push("");
    lines.push("[[registries]]");
    lines.push(`type = ${q(reg.type)}`);
    lines.push(`name = ${q(reg.name)}`);
    if (reg.mode !== "proxy") lines.push(`mode = ${q(reg.mode)}`);
    if (reg.firewall_only) lines.push(`firewall_only = true`);
    if (reg.mode !== "local") {
      const ups = reg.upstreams
        .split("\n")
        .map((u) => u.trim())
        .filter(Boolean);
      if (ups.length) lines.push(`upstreams = [${ups.map(q).join(", ")}]`);
    }
    if (s.storageMode === "multi" && reg.storage_backend) {
      lines.push(`storage = ${q(reg.storage_backend)}`);
    }
    if (reg.type === "cargo" && reg.index_url) {
      lines.push(`index_url = ${q(reg.index_url)}`);
    }
    // `search_url`/`vuln_db_url` are three-state: absent (built-in default), a
    // URL, or `""` — the explicit way to switch the feature off.
    if (reg.search_url_disabled) {
      lines.push(`search_url = ""`);
    } else if (reg.search_url) {
      lines.push(`search_url = ${q(reg.search_url)}`);
    }
    if (reg.type === "goproxy") {
      if (reg.vuln_db_url_disabled) {
        lines.push(`vuln_db_url = ""`);
      } else if (reg.vuln_db_url) {
        lines.push(`vuln_db_url = ${q(reg.vuln_db_url)}`);
      }
    }
    const regHosts = csvToList(reg.hosts);
    if (regHosts.length) lines.push(`hosts = ${tomlArray(regHosts)}`);
    if (!reg.path_routing) lines.push(`path_routing = false`);
    if (isPathAddressed(reg)) {
      const allow = linesToList(reg.path_allow);
      if (allow.length) lines.push(`path_allow = ${tomlArray(allow)}`);
    }

    // RFC 0015 §4.1 — the registry-tier policy scalars. These have to precede
    // every sub-table below: once `[registries.rbac]` is open, a bare
    // `visibility = …` would land inside it.
    if (reg.visibility) lines.push(`visibility = ${q(reg.visibility)}`);
    if (reg.prerelease_visibility)
      lines.push(`prerelease_visibility = ${q(reg.prerelease_visibility)}`);
    if (reg.signed_downloads) lines.push(`signed_downloads = true`);
    // Defaults to true; only the deliberate "keep the console read-only" is
    // written. Inert on a local-mode registry, which has no upstream to fetch
    // from and already holds every version it lists.
    if (!reg.console_fetch) lines.push(`console_fetch = false`);
    // goproxy only. Absent means `https://sum.golang.org`; `""` disables the
    // sumdb route, which is what a registry serving only private modules wants —
    // a lookup there would leak private module paths upstream.
    if (reg.type === "goproxy") {
      if (reg.sumdb_disabled) lines.push(`sumdb_url = ""`);
      else if (reg.sumdb_url) lines.push(`sumdb_url = ${q(reg.sumdb_url)}`);
    }

    // [registries.rbac]
    lines.push("");
    lines.push("[registries.rbac]");
    lines.push(`anonymous = ${permsToToml(reg.rbac_anonymous)}`);
    lines.push(`user = ${permsToToml(reg.rbac_user)}`);
    lines.push(`admin = ${permsToToml(reg.rbac_admin)}`);
    // Both sub-tables must follow the scalar keys above — once a sub-table is
    // opened, any further `anonymous = …` would land inside it.
    const groups = reg.rbac_groups.filter((g) => g.name.trim());
    if (groups.length) {
      lines.push("");
      lines.push("[registries.rbac.groups]");
      for (const g of groups) {
        lines.push(`${q(g.name.trim())} = ${permsToToml(g.perms)}`);
      }
    }
    if (
      !reg.rbac_explore_anonymous ||
      !reg.rbac_explore_user ||
      !reg.rbac_explore_admin
    ) {
      lines.push("");
      lines.push("[registries.rbac.explore]");
      if (!reg.rbac_explore_anonymous) lines.push(`anonymous = false`);
      if (!reg.rbac_explore_user) lines.push(`user = false`);
      if (!reg.rbac_explore_admin) lines.push(`admin = false`);
    }

    // [registries.grants] — RFC 0015 §4.2/§4.3.
    //
    // Unioned on top of the `[registries.rbac]` translation above, never
    // replacing it: a grant only ever adds. An *empty* block is refused at
    // startup rather than treated as a seal, because a registry has no ancestor
    // to stop inheriting from — so rows with no subject are dropped and the
    // table is omitted entirely when nothing is left.
    const regGrants = grantRows(reg.grants);
    if (regGrants.length) {
      lines.push("");
      lines.push("[registries.grants]");
      for (const g of regGrants) {
        lines.push(`${q(g.subject.trim())} = ${permsToToml(g.verbs)}`);
      }
    }

    // [registries.grants_shadow] — evaluate, record, refuse nothing.
    if (reg.grants_shadow_enabled && reg.grants_shadow_until) {
      lines.push("");
      lines.push("[registries.grants_shadow]");
      // A quoted string: `chrono::NaiveDate` deserialises from text, not from
      // TOML's own bare-date type.
      lines.push(`until = ${q(reg.grants_shadow_until)}`);
    }

    // [registries.cache]
    const warmPackages = csvToList(reg.cache_warm_packages);
    const warmPaths = isPathAddressed(reg) ? linesToList(reg.cache_warm_paths) : [];
    const warmingOn = warmPackages.length > 0 || warmPaths.length > 0;
    const nonDefaultCache =
      reg.cache_metadata_ttl !== 300 ||
      reg.cache_artifact_ttl ||
      reg.cache_idle_days ||
      reg.cache_max_size_bytes ||
      reg.cache_keep_latest_n ||
      !reg.cache_serve_stale ||
      warmingOn;
    if (nonDefaultCache) {
      lines.push("");
      lines.push("[registries.cache]");
      if (reg.cache_metadata_ttl !== 300)
        lines.push(`metadata_ttl_secs = ${reg.cache_metadata_ttl}`);
      if (!reg.cache_serve_stale) lines.push(`serve_stale = false`);
      if (reg.cache_artifact_ttl)
        lines.push(`artifact_ttl_secs = ${reg.cache_artifact_ttl}`);
      if (reg.cache_idle_days) lines.push(`idle_days = ${reg.cache_idle_days}`);
      if (reg.cache_max_size_bytes)
        lines.push(`max_size_bytes = ${reg.cache_max_size_bytes}`);
      if (reg.cache_keep_latest_n)
        lines.push(`keep_latest_n = ${reg.cache_keep_latest_n}`);
      if (warmPackages.length)
        lines.push(`warm_packages = ${tomlArray(warmPackages)}`);
      if (warmPaths.length) lines.push(`warm_paths = ${tomlArray(warmPaths)}`);
      // Only meaningful once there is something to warm, so they stay out of the
      // file until then rather than pinning defaults for a disabled feature.
      if (warmingOn && reg.cache_warm_latest_n !== 1)
        lines.push(`warm_latest_n = ${reg.cache_warm_latest_n}`);
      if (warmingOn && reg.cache_warm_concurrency !== 2)
        lines.push(`warm_concurrency = ${reg.cache_warm_concurrency}`);
    }

    // [registries.rate_limit]
    if (reg.rate_limit_enabled) {
      lines.push("");
      lines.push("[registries.rate_limit]");
      lines.push(`requests_per_window = ${reg.rate_limit_rps}`);
      lines.push(`window_secs = ${reg.rate_limit_window}`);
      if (reg.rate_limit_enforcement !== "block")
        lines.push(`enforcement = ${q(reg.rate_limit_enforcement)}`);
      // Per-group overrides of the registry-wide budget above.
      for (const g of reg.rate_limit_groups) {
        if (!g.name.trim()) continue;
        lines.push("");
        lines.push("[[registries.rate_limit.groups]]");
        lines.push(`name = ${q(g.name.trim())}`);
        lines.push(`requests_per_window = ${g.requests_per_window}`);
        lines.push(`window_secs = ${g.window_secs}`);
        if (g.enforcement) lines.push(`enforcement = ${q(g.enforcement)}`);
      }
    }

    // [registries.quota]
    if (reg.quota_enabled && (reg.mode === "local" || reg.mode === "hybrid")) {
      lines.push("");
      lines.push("[registries.quota]");
      if (reg.quota_max_bytes)
        lines.push(`max_storage_bytes_per_user = ${reg.quota_max_bytes}`);
      if (reg.quota_max_packages)
        lines.push(`max_packages_per_user = ${reg.quota_max_packages}`);
      if (reg.quota_warn_threshold_pct !== 80)
        lines.push(`warn_threshold_pct = ${reg.quota_warn_threshold_pct}`);
      if (reg.quota_enforcement !== "block")
        lines.push(`enforcement = ${q(reg.quota_enforcement)}`);
    }

    // [registries.beta_channel]
    if (
      reg.beta_channel_enabled &&
      (reg.mode === "local" || reg.mode === "hybrid")
    ) {
      lines.push("");
      lines.push("[registries.beta_channel]");
      lines.push(`enabled = true`);
    }

    // [registries.versioning]
    if (
      reg.versioning_enabled &&
      (reg.mode === "local" || reg.mode === "hybrid")
    ) {
      lines.push("");
      lines.push("[registries.versioning]");
      if (reg.versioning_enforce_semver) lines.push(`enforce_semver = true`);
      if (!reg.versioning_allow_prerelease) lines.push(`allow_prerelease = false`);
      if (reg.versioning_pattern) lines.push(`version_pattern = ${q(reg.versioning_pattern)}`);
      // RFC 0015 §4.5. `immutable` defaults to `never` — not because it is the
      // best policy, but because nothing enforced immutability before, and any
      // other default would change what an existing config means.
      if (reg.versioning_immutable !== "never")
        lines.push(`immutable = ${q(reg.versioning_immutable)}`);
      if (reg.versioning_monotonic) lines.push(`monotonic = true`);
      if (reg.versioning_dry_run) lines.push(`dry_run = true`);
    }

    // [registries.retention] — RFC 0016.
    //
    // Local/hybrid only: a proxy-mode registry publishes nothing, so every
    // setting here would govern an empty set and the operator meant
    // `[registries.cache]`. The server refuses that pairing outright.
    //
    // The block is emitted only once it reclaims something. An empty one is the
    // single most destructive config in the file — the union of keep conditions
    // is empty, so nothing vetoes and the first live run takes every version —
    // and the server refuses it for that reason.
    if (
      reg.retention_enabled &&
      (reg.mode === "local" || reg.mode === "hybrid") &&
      retentionReclaims(reg)
    ) {
      lines.push("");
      lines.push("[registries.retention]");
      if (reg.retention_keep_versions)
        lines.push(`keep_versions = ${reg.retention_keep_versions}`);
      if (reg.retention_keep_for_days)
        lines.push(`keep_for_days = ${reg.retention_keep_for_days}`);
      if (reg.retention_keep_if_pulled_days)
        lines.push(`keep_if_pulled_days = ${reg.retention_keep_if_pulled_days}`);
      if (!reg.retention_keep_yanked) lines.push(`keep_yanked = false`);
      if (reg.retention_download_signal_floor_days)
        lines.push(
          `download_signal_floor_days = ${reg.retention_download_signal_floor_days}`,
        );
      if (reg.retention_reclaim_delay_ms)
        lines.push(`reclaim_delay_ms = ${reg.retention_reclaim_delay_ms}`);
      if (reg.retention_tombstone_detail_for_days)
        lines.push(
          `tombstone_detail_for_days = ${reg.retention_tombstone_detail_for_days}`,
        );
      // Defaults to true, so only the deliberate switch to enforcing is written.
      if (!reg.retention_dry_run) lines.push(`dry_run = false`);
    }

    // [registries.signing]
    if (
      reg.signing_enabled &&
      (reg.mode === "local" || reg.mode === "hybrid")
    ) {
      lines.push("");
      lines.push("[registries.signing]");
      if (reg.signing_required) lines.push(`required = true`);
      if (reg.signing_allowed_types) {
        const types = reg.signing_allowed_types.split(",").map((t) => t.trim()).filter(Boolean);
        if (types.length) lines.push(`allowed_types = [${types.map(q).join(", ")}]`);
      }
      if (reg.signing_verify_on_download) lines.push(`verify_on_download = true`);
      const trustedKeys = csvToList(reg.signing_trusted_keys);
      if (trustedKeys.length)
        lines.push(`trusted_keys = ${tomlArray(trustedKeys)}`);
    }

    // [registries.repo_signing] — deb/rpm repository metadata signing
    if (reg.repo_signing_enabled && REPO_SIGNING_TYPES.has(reg.type)) {
      lines.push("");
      lines.push("[registries.repo_signing]");
      lines.push(`seed_hex = ${q(reg.repo_signing_seed_hex)}`);
      if (reg.repo_signing_user_id)
        lines.push(`user_id = ${q(reg.repo_signing_user_id)}`);
      if (reg.repo_signing_created)
        lines.push(`created = ${reg.repo_signing_created}`);
    }

    // [registries.vsx_signing] — RFC 0020, the registry's VSIX signature
    if (reg.vsx_signing_enabled && VSX_SIGNING_TYPES.has(reg.type)) {
      lines.push("");
      lines.push("[registries.vsx_signing]");
      lines.push(`seed_hex = ${q(reg.vsx_signing_seed_hex)}`);
      if (reg.vsx_signing_key_id)
        lines.push(`key_id = ${q(reg.vsx_signing_key_id)}`);
    }

    // [registries.sbom]
    if (reg.sbom_enabled) {
      lines.push("");
      lines.push("[registries.sbom]");
      lines.push(`enabled = true`);
      const formats = csvToList(reg.sbom_formats);
      if (formats.length) lines.push(`formats = ${tomlArray(formats)}`);
      if (reg.sbom_required) lines.push(`required = true`);
      if (!reg.sbom_fetch_upstream) lines.push(`fetch_upstream = false`);
    }

    // [registries.readme] — omitted unless the operator changed something, since
    // the absent block already means "capture READMEs, extract from the archive
    // when the metadata carries none, strip remote images".
    if (reg.readme_customised) {
      lines.push("");
      lines.push("[registries.readme]");
      if (!reg.readme_enabled) lines.push(`enabled = false`);
      if (!reg.readme_from_archive) lines.push(`from_archive = false`);
      if (reg.readme_max_bytes !== 262144)
        lines.push(`max_bytes = ${reg.readme_max_bytes}`);
      if (reg.readme_remote_images !== "strip") {
        lines.push(`remote_images = ${q(reg.readme_remote_images)}`);
        // Absent means every host, which is what `proxy` did before the key
        // existed — so an allowlist is only written when there is one.
        const hosts = csvToList(reg.readme_remote_image_hosts);
        if (hosts.length)
          lines.push(`remote_image_hosts = ${tomlArray(hosts)}`);
        if (reg.readme_image_max_bytes !== 2097152)
          lines.push(`image_max_bytes = ${reg.readme_image_max_bytes}`);
      }
    }

    // [registries.upstream_detail] — the console's discovery read. No TTL of its
    // own: the document lands in the metadata cache and obeys this registry's
    // `metadata_ttl_secs`.
    if (reg.upstream_detail_customised) {
      lines.push("");
      lines.push("[registries.upstream_detail]");
      if (!reg.upstream_detail_enabled) lines.push(`enabled = false`);
      if (reg.upstream_detail_max_versions !== 300)
        lines.push(`max_versions = ${reg.upstream_detail_max_versions}`);
      if (reg.upstream_detail_negative_ttl_secs !== 300)
        lines.push(`negative_ttl_secs = ${reg.upstream_detail_negative_ttl_secs}`);
    }

    // [registries.integrity] — omitted entirely unless the operator changed
    // something, since the absent section already means "verify and block on
    // mismatch, warn when no checksum is advertised".
    if (reg.integrity_customised) {
      lines.push("");
      lines.push("[registries.integrity]");
      if (!reg.integrity_enabled) lines.push(`enabled = false`);
      if (!reg.integrity_block_on_mismatch)
        lines.push(`block_on_mismatch = false`);
      if (reg.integrity_require_metadata) lines.push(`require_metadata = true`);
      if (reg.integrity_verify_on_serve) lines.push(`verify_on_serve = true`);
      if (reg.integrity_require_metadata) {
        const bypass = csvToList(reg.integrity_bypass_roles);
        if (bypass.length) lines.push(`bypass_roles = ${tomlArray(bypass)}`);
      }
    }

    // [[registries.rules]] — see rulesToToml, shared with the namespace tier.
    lines.push(...rulesToToml(reg, "registries.rules"));

    // [registries.feature_flags]
    if (!reg.feature_flags_socket_badge) {
      lines.push("");
      lines.push("[registries.feature_flags]");
      lines.push(`socket_badge = false`);
    }

    // [registries.upstream_auth]
    if (reg.upstream_auth_type) {
      lines.push("");
      lines.push("[registries.upstream_auth]");
      lines.push(`type = ${q(reg.upstream_auth_type)}`);
      if (reg.upstream_auth_type === "bearer" && reg.upstream_auth_token) {
        lines.push(`token = ${q(reg.upstream_auth_token)}`);
      } else if (reg.upstream_auth_type === "basic") {
        if (reg.upstream_auth_username)
          lines.push(`username = ${q(reg.upstream_auth_username)}`);
        if (reg.upstream_auth_password)
          lines.push(`password = ${q(reg.upstream_auth_password)}`);
      } else if (reg.upstream_auth_type === "header") {
        if (reg.upstream_auth_header_name)
          lines.push(`name = ${q(reg.upstream_auth_header_name)}`);
        if (reg.upstream_auth_header_value)
          lines.push(`value = ${q(reg.upstream_auth_header_value)}`);
      }
    }

    // [registries.tls]
    if (reg.tls_ca_cert_path) {
      lines.push("");
      lines.push("[registries.tls]");
      lines.push(`ca_cert_path = ${q(reg.tls_ca_cert_path)}`);
    }

    // [registries.proxy] — overrides the global [proxy] for this registry only.
    if (reg.proxy_enabled && reg.proxy_url) {
      lines.push("");
      lines.push("[registries.proxy]");
      lines.push(`url = ${q(reg.proxy_url)}`);
      if (reg.proxy_username) lines.push(`username = ${q(reg.proxy_username)}`);
      if (reg.proxy_password) lines.push(`password = ${q(reg.proxy_password)}`);
      if (reg.proxy_no_proxy) lines.push(`no_proxy = ${q(reg.proxy_no_proxy)}`);
    }

    // [[registries.namespaces]] — RFC 0015 §4.1, emitted last.
    //
    // Position matters. `[[registries.namespaces]]` opens an array-of-tables
    // element, and every `[registries.<x>]` header written after it still
    // resolves against the registry rather than the namespace — but a reader
    // cannot see that from the file. Keeping the namespaces at the end of the
    // registry means every sub-table above them reads in the order it applies.
    for (const ns of reg.namespaces) {
      if (!ns.match_prefix.trim()) continue;
      lines.push("");
      lines.push("[[registries.namespaces]]");
      lines.push(`match = ${q(ns.match_prefix.trim())}`);
      if (ns.visibility) lines.push(`visibility = ${q(ns.visibility)}`);
      if (ns.prerelease_visibility)
        lines.push(`prerelease_visibility = ${q(ns.prerelease_visibility)}`);

      // The seal. `grants = {}` is the one construct that stops a node
      // inheriting from its ancestors — distinct from an absent block (inherit)
      // and from a populated one (inherit, then widen). Written inline because
      // an empty `[registries.namespaces.grants]` header is indistinguishable
      // from a table somebody meant to fill in.
      const nsGrants = grantRows(ns.grants);
      if (ns.sealed) {
        lines.push(`grants = {}`);
      } else if (nsGrants.length) {
        lines.push("");
        lines.push("[registries.namespaces.grants]");
        for (const g of nsGrants) {
          lines.push(`${q(g.subject.trim())} = ${permsToToml(g.verbs)}`);
        }
      }

      if (ns.quota_enabled) {
        lines.push("");
        lines.push("[registries.namespaces.quota]");
        if (ns.quota_max_bytes)
          lines.push(`max_storage_bytes_per_user = ${ns.quota_max_bytes}`);
        if (ns.quota_max_packages)
          lines.push(`max_packages_per_user = ${ns.quota_max_packages}`);
        if (ns.quota_warn_threshold_pct !== 80)
          lines.push(`warn_threshold_pct = ${ns.quota_warn_threshold_pct}`);
        if (ns.quota_enforcement !== "block")
          lines.push(`enforcement = ${q(ns.quota_enforcement)}`);
      }

      if (ns.versioning_enabled) {
        lines.push("");
        lines.push("[registries.namespaces.versioning]");
        if (ns.versioning_enforce_semver) lines.push(`enforce_semver = true`);
        if (!ns.versioning_allow_prerelease)
          lines.push(`allow_prerelease = false`);
        if (ns.versioning_pattern)
          lines.push(`version_pattern = ${q(ns.versioning_pattern)}`);
        if (ns.versioning_immutable !== "never")
          lines.push(`immutable = ${q(ns.versioning_immutable)}`);
        if (ns.versioning_monotonic) lines.push(`monotonic = true`);
        if (ns.versioning_dry_run) lines.push(`dry_run = true`);
      }

      if (ns.shadow_enabled && ns.shadow_until) {
        lines.push("");
        lines.push("[registries.namespaces.grants_shadow]");
        lines.push(`until = ${q(ns.shadow_until)}`);
      }

      // Last within the namespace, because an array-of-tables header ends the
      // run of sibling sub-tables: anything written after it would have to
      // re-open one, which reads as belonging to the rules rather than to the
      // namespace.
      if (ns.rules_enabled) {
        lines.push(...rulesToToml(ns.rules, "registries.namespaces.rules"));
      }
    }
  }

  // [ip_blocking]
  if (s.ipBlocking.enabled) {
    lines.push("");
    lines.push("[ip_blocking]");
    lines.push(`enabled = true`);
    lines.push(`violation_threshold = ${s.ipBlocking.violation_threshold}`);
    lines.push(
      `violation_window_secs = ${s.ipBlocking.violation_window_secs}`,
    );
    lines.push(`ban_duration_secs = ${s.ipBlocking.ban_duration_secs}`);
    lines.push(
      `trigger_on_status = ${numListToToml(s.ipBlocking.trigger_on_status)}`,
    );
  }

  // [vulnerability_scan]
  if (s.vulnerabilityScan.enabled) {
    lines.push("");
    lines.push("[vulnerability_scan]");
    lines.push(`enabled = true`);
    lines.push(`interval_secs = ${s.vulnerabilityScan.interval_secs}`);
    if (s.vulnerabilityScan.osv_api_url) {
      lines.push(`osv_api_url = ${q(s.vulnerabilityScan.osv_api_url)}`);
    }
    if (s.vulnerabilityScan.batch_size !== 100) {
      lines.push(`batch_size = ${s.vulnerabilityScan.batch_size}`);
    }
  }

  // [stats]
  if (
    !s.stats.history_enabled ||
    !s.stats.metrics_enabled ||
    s.stats.history_retention_days !== 30
  ) {
    lines.push("");
    lines.push("[stats]");
    if (!s.stats.history_enabled) lines.push(`history_enabled = false`);
    if (s.stats.history_retention_days !== 30)
      lines.push(`history_retention_days = ${s.stats.history_retention_days}`);
    if (!s.stats.metrics_enabled) lines.push(`metrics_enabled = false`);
  }

  // [grants] — the instance tier (RFC 0015 §4.2).
  //
  // Only ever emitted with rows in it: an empty `[grants]` is a startup error,
  // because a seal stops a node inheriting from its ancestors and the instance
  // tier has none — so it would grant nothing and block nothing.
  const topGrants = grantRows(s.instanceGrants);
  if (topGrants.length) {
    lines.push("");
    lines.push("[grants]");
    for (const g of topGrants) {
      lines.push(`${q(g.subject.trim())} = ${permsToToml(g.verbs)}`);
    }
  }

  // [cache_coherence]
  if (s.cacheCoherence.enabled) {
    lines.push("");
    lines.push("[cache_coherence]");
    lines.push(`enabled = true`);
    if (s.cacheCoherence.interval_secs !== 86400)
      lines.push(`interval_secs = ${s.cacheCoherence.interval_secs}`);
  }

  // [search]
  if (s.search.readmes || s.search.text_config !== "english") {
    lines.push("");
    lines.push("[search]");
    if (s.search.readmes) lines.push(`readmes = true`);
    if (s.search.text_config !== "english")
      lines.push(`text_config = ${q(s.search.text_config)}`);
  }

  // [subdomain_routing]
  if (s.subdomainRouting.enabled) {
    lines.push("");
    lines.push("[subdomain_routing]");
    lines.push(`enabled = true`);
    lines.push(`base_domain = ${q(s.subdomainRouting.base_domain)}`);
    if (s.subdomainRouting.scheme !== "https")
      lines.push(`scheme = ${q(s.subdomainRouting.scheme)}`);
  }

  // [proxy] — global egress proxy for every upstream fetch.
  if (s.upstreamProxy.enabled && s.upstreamProxy.url) {
    lines.push("");
    lines.push("[proxy]");
    lines.push(`url = ${q(s.upstreamProxy.url)}`);
    if (s.upstreamProxy.username)
      lines.push(`username = ${q(s.upstreamProxy.username)}`);
    if (s.upstreamProxy.password)
      lines.push(`password = ${q(s.upstreamProxy.password)}`);
    if (s.upstreamProxy.no_proxy)
      lines.push(`no_proxy = ${q(s.upstreamProxy.no_proxy)}`);
  }

  // [notifications]
  if (s.notifications.enabled) {
    lines.push("");
    lines.push("[notifications]");
    lines.push(`enabled = true`);
    for (const ch of s.notifications.channels) {
      if (!ch.name.trim()) continue;
      lines.push("");
      lines.push("[[notifications.channels]]");
      lines.push(`name = ${q(ch.name.trim())}`);
      lines.push(`type = ${q(ch.type)}`);
      if (ch.type === "email") {
        lines.push(`smtp_host = ${q(ch.smtp_host)}`);
        if (ch.smtp_port !== 587) lines.push(`smtp_port = ${ch.smtp_port}`);
        if (ch.smtp_user) lines.push(`smtp_user = ${q(ch.smtp_user)}`);
        if (ch.smtp_password) lines.push(`smtp_password = ${q(ch.smtp_password)}`);
        lines.push(`from = ${q(ch.from)}`);
        lines.push(`to = ${listToToml(ch.to)}`);
        if (!ch.tls) lines.push(`tls = false`);
      } else {
        lines.push(`url = ${q(ch.url)}`);
        // Only the generic webhook channel signs its payloads; Slack and Teams
        // authenticate by the secrecy of the hook URL itself.
        if (ch.type === "webhook" && ch.secret)
          lines.push(`secret = ${q(ch.secret)}`);
      }
      if (ch.timeout_secs !== 10) lines.push(`timeout_secs = ${ch.timeout_secs}`);
    }
    for (const hook of s.notifications.inbound) {
      if (!hook.name.trim()) continue;
      lines.push("");
      lines.push("[[notifications.inbound]]");
      lines.push(`name = ${q(hook.name.trim())}`);
      if (hook.secret) lines.push(`secret = ${q(hook.secret)}`);
    }
  }

  // [otel]
  if (s.otel.enabled) {
    lines.push("");
    lines.push("[otel]");
    lines.push(`endpoint = ${q(s.otel.endpoint)}`);
    lines.push(`service_name = ${q(s.otel.service_name)}`);
  }

  return lines.join("\n");
}

