<script setup lang="ts">
import { ref, computed, watch, onMounted } from "vue";
import {
  seq,
  blankAuthProvider,
  defaultUpstream,
  isPathAddressed,
  defaultRegistry,
  defaultState,
  namespaceSeparator,
  wrongKindVerbs,
  prereleaseWiderThanRelease,
  shadowDateInvalid,
  blankRulePolicy,
  retentionReclaims,
  signedDownloadsUsedFor,
  signingSecretTooShortFor,
  hostRoutingEnabledFor,
  renderConfigToml,
} from "./configToml";
import type {
  RegistryType,
  Enforcement,
  Visibility,
  GrantEntry,
  Namespace,
  Token,
  Condition,
  ActionsRule,
  RoleMapping,
  AuthProvider,
  Registry,
  GeneratorState,
} from "./configToml";

// ── State ───────────────────────────────────────────────────────────────────
// The pure half — types, defaults, helpers, the TOML renderer — lives in
// ./configToml.ts so a test can run it without Vue. The refs below are the
// form; `state()` is the snapshot the renderer reads.

const initial = defaultState();
const server = ref(initial.server);
const database = ref(initial.database);
const metaCache = ref(initial.metaCache);
const limits = ref(initial.limits);
const ipBlocking = ref(initial.ipBlocking);
const vulnerabilityScan = ref(initial.vulnerabilityScan);
const stats = ref(initial.stats);
const subdomainRouting = ref(initial.subdomainRouting);
const instanceGrants = ref(initial.instanceGrants);
const cacheCoherence = ref(initial.cacheCoherence);
const search = ref(initial.search);
const signedUrls = ref(initial.signedUrls);
const upstreamProxy = ref(initial.upstreamProxy);
const notifications = ref(initial.notifications);
const storageMode = ref(initial.storageMode);
const singleStorage = ref(initial.singleStorage);
const storageDefault = ref(initial.storageDefault);
const storageBackends = ref(initial.storageBackends);
const otel = ref(initial.otel);

const authProviders = ref(initial.authProviders);

type ArgFn = (params: {
  password: string;
  salt: Uint8Array;
  parallelism: number;
  iterations: number;
  memorySize: number;
  hashLength: number;
  outputType: "encoded";
}) => Promise<string>;

let _argon2id: ArgFn | null = null;
const tokenHashes = ref(initial.tokenHashes);
let _hashTimer: ReturnType<typeof setTimeout> | null = null;

/**
 * Generation counter for `runHashComputation`.
 *
 * Argon2id at `memorySize: 65536, iterations: 3` takes well over a second in a
 * browser, which is far longer than the 350 ms debounce — so runs overlap, and
 * `scheduleHashing` can only cancel a run that has not *started*. Without this
 * guard the slower, older run wrote last: type `secret-a`, pause, type `-b`,
 * pause, and run B finishes first with `hash("secret-a-b")` before run A lands
 * and overwrites it with `hash("secret-a")`.
 *
 * The panel then shows a green `$argon2id$…` and emits it into
 * `[[auth.tokens]] value = …` — a correct-looking config holding the hash of a
 * token the operator never had, discovered only as a failed login. A stale
 * result must be dropped, not written.
 */
let _hashRun = 0;

async function runHashComputation() {
  const mine = ++_hashRun;

  const next: Record<number, string | null> = {};
  for (const auth of authProviders.value) {
    if (auth.type !== "token") continue;
    for (const tok of auth.tokens) {
      next[tok.id] = tok.value.trim() ? null : "";
    }
  }
  tokenHashes.value = next;

  for (const auth of authProviders.value) {
    if (auth.type !== "token") continue;
    for (const tok of auth.tokens) {
      const raw = tok.value.trim();
      if (!raw) continue;
      try {
        let result: string;
        if (_argon2id) {
          const salt = new Uint8Array(16);
          crypto.getRandomValues(salt);
          result = await _argon2id({
            password: raw,
            salt,
            parallelism: 4,
            iterations: 3,
            memorySize: 65536,
            hashLength: 32,
            outputType: "encoded",
          });
        } else {
          result = raw;
        }
        if (mine !== _hashRun) return; // superseded — this hash is of stale input
        tokenHashes.value = { ...tokenHashes.value, [tok.id]: result };
      } catch {
        if (mine !== _hashRun) return;
        tokenHashes.value = { ...tokenHashes.value, [tok.id]: raw };
      }
    }
  }
}

function scheduleHashing() {
  if (_hashTimer) clearTimeout(_hashTimer);
  // Clearing the timer only cancels a run that has not started; one already
  // in flight is stranded by the generation counter above.
  _hashTimer = setTimeout(() => void runHashComputation(), 350);
}

onMounted(async () => {
  try {
    const mod = await import("hash-wasm");
    _argon2id = mod.argon2id as unknown as ArgFn;
  } catch {
    // hash-wasm failed to load — plain-text fallback
  }
  await runHashComputation();
});

watch(
  () =>
    authProviders.value.flatMap((a) =>
      a.type === "token" ? a.tokens.map((t) => `${t.id}:${t.value}`) : [],
    ),
  scheduleHashing,
);


const registries = ref(initial.registries);

const state = (): GeneratorState => ({
  server: server.value,
  database: database.value,
  metaCache: metaCache.value,
  limits: limits.value,
  ipBlocking: ipBlocking.value,
  vulnerabilityScan: vulnerabilityScan.value,
  stats: stats.value,
  subdomainRouting: subdomainRouting.value,
  instanceGrants: instanceGrants.value,
  cacheCoherence: cacheCoherence.value,
  search: search.value,
  signedUrls: signedUrls.value,
  upstreamProxy: upstreamProxy.value,
  notifications: notifications.value,
  storageMode: storageMode.value,
  singleStorage: singleStorage.value,
  storageDefault: storageDefault.value,
  storageBackends: storageBackends.value,
  otel: otel.value,
  authProviders: authProviders.value,
  tokenHashes: tokenHashes.value,
  registries: registries.value,
});

const signedDownloadsUsed = computed(() => signedDownloadsUsedFor(state()));
const signingSecretTooShort = computed(() => signingSecretTooShortFor(state()));
const hostRoutingEnabled = computed(() => hostRoutingEnabledFor(state()));

const toml = computed(() => renderConfigToml(state()));


// ── Syntax highlighting ─────────────────────────────────────────────────────

function escHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function hlVal(val: string): string {
  if (!val) return "";
  if (val.startsWith('"') && val.endsWith('"') && val.length >= 2) {
    return `<span class="cg-hl-string">${escHtml(val)}</span>`;
  }
  if (val === "true" || val === "false") {
    return `<span class="cg-hl-bool">${val}</span>`;
  }
  if (/^-?\d+(\.\d+)?$/.test(val)) {
    return `<span class="cg-hl-number">${val}</span>`;
  }
  if (val.startsWith("[") && val.endsWith("]")) {
    const inner = val.slice(1, -1);
    let result = "";
    let last = 0;
    const strRe = /"([^"]*)"/g;
    let m: RegExpExecArray | null;
    while ((m = strRe.exec(inner)) !== null) {
      result += escHtml(inner.slice(last, m.index));
      result += `<span class="cg-hl-string">${escHtml(m[0])}</span>`;
      last = m.index + m[0].length;
    }
    result += escHtml(inner.slice(last));
    return `[${result}]`;
  }
  return escHtml(val);
}

const highlightedToml = computed(() =>
  toml.value
    .split("\n")
    .map((line) => {
      const trimmed = line.trimStart();
      const indent = escHtml(line.slice(0, line.length - trimmed.length));
      if (!trimmed) return "";
      if (trimmed.startsWith("#")) {
        return `<span class="cg-hl-comment">${escHtml(line)}</span>`;
      }
      const arrM = trimmed.match(/^(\[\[)([\w.]+)(\]\])$/);
      if (arrM) {
        return `${indent}<span class="cg-hl-bracket">[[</span><span class="cg-hl-table">${escHtml(arrM[2])}</span><span class="cg-hl-bracket">]]</span>`;
      }
      const tblM = trimmed.match(/^(\[)([\w.]+)(\])$/);
      if (tblM) {
        return `${indent}<span class="cg-hl-bracket">[</span><span class="cg-hl-table">${escHtml(tblM[2])}</span><span class="cg-hl-bracket">]</span>`;
      }
      const kvM = trimmed.match(/^([\w-]+)\s*=\s*(.+)$/);
      if (kvM) {
        return `${indent}<span class="cg-hl-key">${escHtml(kvM[1])}</span> <span class="cg-hl-eq">=</span> ${hlVal(kvM[2])}`;
      }
      return escHtml(line);
    })
    .join("\n"),
);

// ── Actions ─────────────────────────────────────────────────────────────────

const copied = ref(false);
async function copyToml() {
  await navigator.clipboard.writeText(toml.value);
  copied.value = true;
  setTimeout(() => {
    copied.value = false;
  }, 1500);
}

function downloadToml() {
  const blob = new Blob([toml.value], { type: "text/plain" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "config.toml";
  a.click();
  URL.revokeObjectURL(a.href);
}

// Auth providers
function addAuthProvider() {
  authProviders.value.push(blankAuthProvider());
}
function removeAuthProvider(id: number) {
  authProviders.value = authProviders.value.filter((a) => a.id !== id);
}
function addToken(auth: AuthProvider) {
  auth.tokens.push({ id: seq.token++, value: "", role: "user", user_id: "" });
}
function removeToken(auth: AuthProvider, id: number) {
  auth.tokens = auth.tokens.filter((t) => t.id !== id);
}
function addActionsRule(auth: AuthProvider) {
  auth.actions_rules.push({
    id: seq.rule++,
    group: "",
    group_template: "",
    role: "",
    match_mode: "all",
    conditions: [],
  });
}
function removeActionsRule(auth: AuthProvider, id: number) {
  auth.actions_rules = auth.actions_rules.filter((r) => r.id !== id);
}
function addCondition(rule: ActionsRule) {
  rule.conditions.push({ id: seq.cond++, claim: "", pattern: "", match_type: "auto" });
}
function removeCondition(rule: ActionsRule, id: number) {
  rule.conditions = rule.conditions.filter((c) => c.id !== id);
}

function addRoleMapping(list: RoleMapping[]) {
  list.push({ id: seq.mapping++, claim: "", role: "user" });
}
function removeRoleMapping(auth: AuthProvider, key: "oidc" | "k8s", id: number) {
  if (key === "oidc") {
    auth.oidc_role_mappings = auth.oidc_role_mappings.filter((m) => m.id !== id);
  } else {
    auth.k8s_role_mappings = auth.k8s_role_mappings.filter((m) => m.id !== id);
  }
}

let groupSeq = 0;
function addRbacGroup(reg: Registry) {
  reg.rbac_groups.push({ id: groupSeq++, name: "", perms: "releases:read" });
}
function removeRbacGroup(reg: Registry, id: number) {
  reg.rbac_groups = reg.rbac_groups.filter((g) => g.id !== id);
}

// ── RFC 0015 grants and namespaces ──────────────────────────────────────────

let grantSeq = 0;
function addGrant(list: GrantEntry[]) {
  list.push({ id: grantSeq++, subject: "", verbs: "releases:read" });
}
function removeGrant(list: GrantEntry[], id: number) {
  const i = list.findIndex((g) => g.id === id);
  if (i !== -1) list.splice(i, 1);
}

let nsSeq = 0;
function addNamespace(reg: Registry) {
  reg.namespaces.push({
    id: nsSeq++,
    match_prefix: "",
    sealed: false,
    grants: [],
    visibility: "",
    prerelease_visibility: "",
    versioning_enabled: false,
    versioning_enforce_semver: false,
    versioning_allow_prerelease: true,
    versioning_pattern: "",
    versioning_immutable: "never",
    versioning_monotonic: false,
    versioning_dry_run: false,
    quota_enabled: false,
    quota_max_bytes: "",
    quota_max_packages: "",
    quota_warn_threshold_pct: 80,
    quota_enforcement: "block",
    shadow_enabled: false,
    shadow_until: "",
    rules_enabled: false,
    rules: blankRulePolicy(),
  });
}
function removeNamespace(reg: Registry, id: number) {
  reg.namespaces = reg.namespaces.filter((n) => n.id !== id);
}

/// The startup errors a namespace `match` can carry, named while the operator is
/// still typing rather than at boot: a blank prefix matches nothing, a duplicate
/// is two answers to one question, and one ending in the ecosystem's separator
/// can never match because matching appends it.
function namespaceMatchError(reg: Registry, ns: Namespace): string {
  const m = ns.match_prefix.trim();
  if (!m) return "A blank match covers no package at all.";
  const sep = namespaceSeparator(reg.type);
  if (m.endsWith(sep))
    return `Ends with '${sep}', the separator this ecosystem uses — matching appends it, so this can never match. Write "${m.replace(new RegExp(`\\${sep}+$`), "")}".`;
  if (reg.namespaces.filter((o) => o.match_prefix.trim() === m).length > 1)
    return "Two namespace blocks both match this prefix; merge them.";
  return "";
}

let rlGroupSeq = 0;
function addRateLimitGroup(reg: Registry) {
  reg.rate_limit_groups.push({
    id: rlGroupSeq++,
    name: "",
    requests_per_window: reg.rate_limit_rps,
    window_secs: reg.rate_limit_window,
    enforcement: "",
  });
}
function removeRateLimitGroup(reg: Registry, id: number) {
  reg.rate_limit_groups = reg.rate_limit_groups.filter((g) => g.id !== id);
}

function addChannel() {
  notifications.value.channels.push({
    id: seq.channel++,
    type: "slack",
    name: "",
    url: "",
    secret: "",
    timeout_secs: 10,
    smtp_host: "",
    smtp_port: 587,
    smtp_user: "",
    smtp_password: "",
    from: "",
    to: "",
    tls: true,
  });
}
function removeChannel(id: number) {
  notifications.value.channels = notifications.value.channels.filter(
    (c) => c.id !== id,
  );
}
function addInboundHook() {
  notifications.value.inbound.push({ id: seq.inbound++, name: "", secret: "" });
}
function removeInboundHook(id: number) {
  notifications.value.inbound = notifications.value.inbound.filter(
    (h) => h.id !== id,
  );
}

function addRegistry() {
  registries.value.push(defaultRegistry("npm"));
}
function removeRegistry(id: number) {
  registries.value = registries.value.filter((r) => r.id !== id);
}
// Registry types that only support proxy mode (no private/local hosting) — they
// mirror the backend's local/hybrid allowlist: anything NOT in it is proxy-only.
const PROXY_ONLY_TYPES = new Set<RegistryType>([
  "github",
  "forgejo",
  "gitlab",
  "jetbrains",
  "generic",
]);
const isProxyOnly = (reg: Registry) => PROXY_ONLY_TYPES.has(reg.type);

function onTypeChange(reg: Registry) {
  reg.upstreams = defaultUpstream[reg.type];
  // Proxy-only types can't run in local/hybrid mode; force proxy.
  if (isProxyOnly(reg)) reg.mode = "proxy";
  // `path_allow` is rejected outright on kinds that aren't path-addressed, and
  // is mandatory on `generic` — so it follows the type rather than persisting
  // across a switch.
  if (!isPathAddressed(reg)) {
    reg.path_allow = "";
    reg.cache_warm_paths = "";
  } else if (reg.type === "generic" && !reg.path_allow.trim()) {
    reg.path_allow = "**";
  }
}

function addBackend() {
  storageBackends.value.push({
    id: seq.backend++,
    name: "",
    type: "filesystem",
    path: "./cache",
    bucket: "",
    region: "us-east-1",
    endpoint_url: "",
    force_path_style: false,
    prefix: "",
  });
}
function removeBackend(id: number) {
  storageBackends.value = storageBackends.value.filter((b) => b.id !== id);
}

const backendNames = computed(() =>
  storageBackends.value.map((b) => b.name).filter(Boolean),
);

// Registry types whose manifests the SBOM extractor can read a licence out of.
// Everywhere else the licence is permanently unknown, so a blocking gate that
// also refuses unknowns denies every download — the backend emits a
// `license-gate.denies-everything` warning for exactly this shape.
const LICENSE_AWARE_TYPES = new Set<RegistryType>([
  "cargo",
  "maven",
  "npm",
  "nuget",
  "pypi",
]);
const licenseGateDeniesEverything = (reg: Registry) =>
  reg.rule_license_gate_enabled &&
  reg.rule_license_gate_block &&
  !reg.rule_license_gate_allow_unknown &&
  !LICENSE_AWARE_TYPES.has(reg.type);

const isLocalOrHybrid = (reg: Registry) =>
  reg.mode === "local" || reg.mode === "hybrid";

function composerRepoSnippet(registryName: string): string {
  return `{
  "repositories": [
    {
      "type": "composer",
      "url": "https://your-batlehub-host/proxy/${registryName}/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer <token>"]
        }
      }
    }
  ]
}`;
}

const composerAuthSnippet = `{
  "http-basic": {
    "your-batlehub-host": {
      "username": "user",
      "password": "<your-token>"
    }
  }
}`;
</script>

<template>
  <div class="cg-root">
    <!-- ── LEFT: form ──────────────────────────────────────────────────── -->
    <div class="cg-form">
      <!-- Server -->
      <section class="cg-section">
        <h3>Server</h3>
        <div class="cg-two-col">
          <label
            >Host<input v-model="server.host" placeholder="0.0.0.0"
          /></label>
          <label
            >Port<input
              v-model.number="server.port"
              type="number"
              min="1"
              max="65535"
          /></label>
        </div>
        <label
          >Static directory (optional)<input
            v-model="server.static_dir"
            placeholder="./ui/dist"
        /></label>
        <label
          >CLI binary directory (optional)<input
            v-model="server.cli_binary_path"
            placeholder="./dist/cli"
          /><span class="cg-field-hint"
            >Directory of pre-built <code>batlehub-cli</code> binaries served by
            the in-app CLI download page. Leave blank to disable it.</span
          ></label
        >
        <label
          >CORS allowed origins (optional, comma-separated)<input
            v-model="server.cors_allowed_origins"
            placeholder="https://batlehub.example.com"
          /><span class="cg-field-hint"
            >Leave blank to allow all origins (fine for development). Restrict in
            production.</span
          ></label
        >
        <label class="cg-check cg-mb">
          <input
            type="checkbox"
            v-model="server.trusted_proxies_set"
            :disabled="hostRoutingEnabled"
            :checked="server.trusted_proxies_set || hostRoutingEnabled"
          />
          Declare a trusted-proxy policy
        </label>
        <p v-if="hostRoutingEnabled" class="cg-field-hint cg-hint-required">
          Required, because host-based routing is on (a base domain under
          <em>Subdomain routing</em>, or a registry with vanity hosts). Routing
          reads the <code>Forwarded</code> / <code>X-Forwarded-Host</code>
          header, so the server refuses to start until you say which peers may
          set it.
        </p>
        <label v-if="server.trusted_proxies_set || hostRoutingEnabled"
          >Trusted proxy IPs (comma-separated)<input
            v-model="server.trusted_proxies"
            placeholder="10.0.0.1, 10.0.0.2"
          /><span class="cg-field-hint"
            >IPs of reverse proxies trusted to forward
            <code>X-Forwarded-For</code>. An empty list is a policy in itself —
            it means trust nobody and always use the TCP peer address, which is
            the right answer when BatleHub is exposed directly. This supersedes
            the deprecated <code>[ip_blocking].trusted_proxies</code>, which logs
            a warning at startup.</span
          ></label
        >

        <p class="cg-subsection-label">Signed download URLs</p>
        <label class="cg-check cg-mb">
          <input
            type="checkbox"
            v-model="signedUrls.enabled"
            :disabled="signedDownloadsUsed"
            :checked="signedUrls.enabled || signedDownloadsUsed"
          />
          Configure a URL signing secret
        </label>
        <p v-if="signedDownloadsUsed" class="cg-field-hint cg-hint-required">
          Required, because a registry above has <em>signed downloads</em> on.
          Setting one without a secret is a startup error: a registry that
          believes it is closed and is not is what the feature exists to prevent.
        </p>
        <template v-if="signedUrls.enabled || signedDownloadsUsed">
          <label
            >Signing secret<input
              v-model="signedUrls.secret"
              placeholder="${BATLEHUB_URL_SIGNING_SECRET}"
            /><span
              class="cg-field-hint"
              :class="{ 'cg-hint-required': signingSecretTooShort }"
              >{{
                signingSecretTooShort
                  ? `${signedUrls.secret.length} of the 32 bytes an HMAC-SHA256 key needs. Measured in bytes, not characters.`
                  : "HMAC-SHA256 key material. Prefer a ${VAR} placeholder — it is expanded before the file is parsed, so the secret never lands in the config."
              }}</span
            ></label
          >
          <div class="cg-two-col">
            <label
              >TTL (seconds)<input
                type="number"
                v-model.number="signedUrls.ttl_seconds"
                min="1"
                max="3600"
              /><span class="cg-field-hint"
                >The ceiling is 3600, so a misconfiguration cannot mint a
                month-long bearer credential.</span
              ></label
            >
            <label
              >Previous secrets (comma-separated)<input
                v-model="signedUrls.previous_secrets"
                placeholder="${BATLEHUB_URL_SIGNING_SECRET_OLD}"
              /><span class="cg-field-hint"
                >Verified against but never minted with, so a secret rotates
                without a flag day.</span
              ></label
            >
          </div>
        </template>
      </section>

      <!-- Instance-tier grants -->
      <section class="cg-section">
        <h3>Instance permissions</h3>
        <p class="cg-field-hint" style="margin-bottom: 0.5rem">
          <code>[grants]</code> sits above every registry, which is why it cannot
          carry an ecosystem verb like <code>npm:dist-tags:write</code> — no one
          registry defines it. An administrator already holds every
          control-surface verb, so this block is only ever needed to give one of
          them to somebody else.
        </p>
        <div v-for="g in instanceGrants" :key="g.id" class="cg-condition-item">
          <div class="cg-two-col">
            <label
              >Subject<input v-model="g.subject" placeholder="group:oidc:sre"
            /></label>
            <label
              >Verbs<input
                v-model="g.verbs"
                placeholder="config:read, system:read"
            /></label>
          </div>
          <button class="cg-btn-remove" @click="removeGrant(instanceGrants, g.id)">
            Remove grant
          </button>
        </div>
        <button class="cg-btn-add" @click="addGrant(instanceGrants)">
          + Add instance grant
        </button>
      </section>

      <!-- Search -->
      <section class="cg-section">
        <h3>Search</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="search.readmes" />
          Match README prose as well as package names
        </label>
        <!-- No "needs Postgres" warning here: this generator always emits a
             `[database] type = "postgresql"` block, so the pairing the server
             refuses is unreachable from the form. A hint that can never be true
             is one an operator learns to skip. -->
        <span
          class="cg-field-hint"
          style="display: block; margin-bottom: 0.5rem"
          >With this off, <code>?in=readme</code> is accepted and answers exactly
          as <code>?in=name</code> does, plus a field saying so. A parameter that
          silently means something else is worse than one that says prose search
          is not enabled here.</span
        >
        <label
          >Postgres text search configuration<input
            v-model="search.text_config"
            placeholder="english"
          /><span class="cg-field-hint"
            >Changing this <strong>rebuilds the generated column</strong> at
            startup, which makes it an install-time decision rather than
            something to tune later. <code>english</code>, <code>french</code>,
            <code>simple</code>, …</span
          ></label
        >
      </section>

      <!-- Cache coherence -->
      <section class="cg-section">
        <h3>Cache coherence</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="cacheCoherence.enabled" />
          Periodically sweep for storage blobs nothing references
        </label>
        <label v-if="cacheCoherence.enabled"
          >Interval (seconds)<input
            type="number"
            v-model.number="cacheCoherence.interval_secs"
            min="1"
          /><span class="cg-field-hint"
            >Default 86400 (daily). With the sweep off, orphaned blobs are
            collected only when an operator asks for it.</span
          ></label
        >
      </section>

      <!-- Database -->
      <section class="cg-section">
        <h3>Database</h3>
        <label>
          PostgreSQL URL
          <input
            v-model="database.url"
            placeholder="postgresql://batlehub:changeme@localhost:5432/batlehub"
          />
        </label>
        <div class="cg-two-col">
          <label>
            Max connections
            <input
              v-model.number="database.max_connections"
              type="number"
              min="1"
            />
            <span class="cg-field-hint">Connection pool size (default: 10)</span>
          </label>
          <label>
            Min connections
            <input
              v-model.number="database.min_connections"
              type="number"
              min="0"
            />
            <span class="cg-field-hint">Idle connections kept warm (default: 1)</span>
          </label>
        </div>
        <label>
          Acquire timeout (s)
          <input
            v-model.number="database.acquire_timeout_secs"
            type="number"
            min="1"
          />
          <span class="cg-field-hint"
            >How long a request waits for a free connection before failing
            (default: 30)</span
          >
        </label>
      </section>

      <!-- Metadata Cache -->
      <section class="cg-section">
        <h3>Metadata Cache</h3>
        <div class="cg-radio-row cg-mb">
          <label class="cg-radio"
            ><input type="radio" v-model="metaCache.type" value="memory" />
            Memory</label
          >
          <label class="cg-radio"
            ><input type="radio" v-model="metaCache.type" value="postgres" />
            PostgreSQL</label
          >
          <label class="cg-radio"
            ><input type="radio" v-model="metaCache.type" value="redis" />
            Redis</label
          >
        </div>
        <span class="cg-field-hint">
          <template v-if="metaCache.type === 'memory'"
            >In-process cache — fast but lost on restart. Good for single-node
            dev deployments.</template
          >
          <template v-else-if="metaCache.type === 'postgres'"
            >Persisted in the <code>metadata_cache</code> table — survives
            restarts, shared across replicas.</template
          >
          <template v-else
            >Persisted in Redis — survives restarts, shared across
            replicas.</template
          >
        </span>
        <template v-if="metaCache.type === 'redis'">
          <label style="margin-top: 0.5rem"
            >Redis URL<input
              v-model="metaCache.url"
              placeholder="redis://localhost:6379"
          /></label>
        </template>
      </section>

      <!-- Limits -->
      <section class="cg-section">
        <h3>Limits</h3>
        <label>
          Max artifact size (bytes)
          <input
            v-model="limits.max_artifact_size_bytes"
            placeholder="524288000  (500 MiB default)"
          />
          <span class="cg-field-hint"
            >Applies to both proxy downloads and local publishes. Leave blank to
            use the 500 MiB default.</span
          >
        </label>
      </section>

      <!-- Storage -->
      <section class="cg-section">
        <h3>Storage</h3>
        <div class="cg-radio-row cg-mb">
          <label class="cg-radio"
            ><input type="radio" v-model="storageMode" value="single" /> Single
            backend</label
          >
          <label class="cg-radio"
            ><input type="radio" v-model="storageMode" value="multi" />
            Multi-backend</label
          >
        </div>

        <!-- Single backend -->
        <template v-if="storageMode === 'single'">
          <div class="cg-radio-row cg-mb">
            <label class="cg-radio"
              ><input
                type="radio"
                v-model="singleStorage.type"
                value="filesystem"
              />
              Filesystem</label
            >
            <label class="cg-radio"
              ><input type="radio" v-model="singleStorage.type" value="s3" /> S3
              / RustFS</label
            >
          </div>
          <template v-if="singleStorage.type === 'filesystem'">
            <label
              >Cache path<input
                v-model="singleStorage.path"
                placeholder="./cache"
            /></label>
          </template>
          <template v-else>
            <div class="cg-two-col">
              <label
                >Bucket<input
                  v-model="singleStorage.bucket"
                  placeholder="my-artifacts"
              /></label>
              <label
                >Region<input
                  v-model="singleStorage.region"
                  placeholder="us-east-1"
              /></label>
            </div>
            <label
              >Endpoint URL (optional)<input
                v-model="singleStorage.endpoint_url"
                placeholder="http://minio:9000"
            /></label>
            <label
              >Key prefix (optional)<input
                v-model="singleStorage.prefix"
                placeholder="batlehub/"
              /><span class="cg-field-hint"
                >Prepended to every object key — acts as a folder inside the
                bucket.</span
              ></label
            >
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="singleStorage.force_path_style" />
              Force path-style URLs (required for MinIO, RustFS)
            </label>
          </template>
        </template>

        <!-- Multi-backend -->
        <template v-else>
          <label
            >Default backend name<input
              v-model="storageDefault"
              placeholder="primary"
          /></label>
          <div v-for="b in storageBackends" :key="b.id" class="cg-list-item">
            <div class="cg-two-col">
              <label
                >Backend name<input v-model="b.name" placeholder="primary"
              /></label>
              <label>
                Type
                <select v-model="b.type">
                  <option value="filesystem">Filesystem</option>
                  <option value="s3">S3 / RustFS</option>
                </select>
              </label>
            </div>
            <template v-if="b.type === 'filesystem'">
              <label
                >Cache path<input v-model="b.path" placeholder="./cache"
              /></label>
            </template>
            <template v-else>
              <div class="cg-two-col">
                <label
                  >Bucket<input v-model="b.bucket" placeholder="my-artifacts"
                /></label>
                <label
                  >Region<input v-model="b.region" placeholder="us-east-1"
                /></label>
              </div>
              <label
                >Endpoint URL (optional)<input
                  v-model="b.endpoint_url"
                  placeholder="http://minio:9000"
              /></label>
              <label
                >Key prefix (optional)<input
                  v-model="b.prefix"
                  placeholder="batlehub/"
                /><span class="cg-field-hint"
                  >Prepended to every object key.</span
                ></label
              >
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="b.force_path_style" />
                Force path-style URLs (required for MinIO, RustFS)
              </label>
            </template>
            <button class="cg-btn-remove" @click="removeBackend(b.id)">
              Remove
            </button>
          </div>
          <button class="cg-btn-add" @click="addBackend">+ Add backend</button>
        </template>
      </section>

      <!-- Auth providers -->
      <section class="cg-section">
        <h3>Authentication</h3>
        <div v-for="auth in authProviders" :key="auth.id" class="cg-list-item">
          <label>
            Provider type
            <select v-model="auth.type">
              <option value="token">Static tokens</option>
              <option value="oidc">OIDC / OAuth2</option>
              <option value="kubernetes">Kubernetes service accounts</option>
              <option value="actions-oidc">GitHub Actions OIDC</option>
            </select>
          </label>

          <!-- Token auth -->
          <template v-if="auth.type === 'token'">
            <div v-for="tok in auth.tokens" :key="tok.id" class="cg-subitem">
              <label>
                Token value
                <span class="cg-label-note"
                  >(raw — an Argon2id hash is written to the config)</span
                >
                <input
                  v-model="tok.value"
                  placeholder="my-secret-token"
                  autocomplete="off"
                />
              </label>
              <div v-if="tok.value.trim()" class="cg-hash-status">
                <template v-if="tokenHashes[tok.id] === null">
                  <span class="cg-hash-computing">⏳ Computing Argon2id hash…</span>
                </template>
                <template
                  v-else-if="tokenHashes[tok.id]?.startsWith('$argon2')"
                >
                  <span class="cg-hash-ready"
                    >🔐 Argon2id hash ready &mdash; use the raw value above with
                    <code>Authorization: Bearer &lt;raw token&gt;</code></span
                  >
                </template>
                <template v-else>
                  <span class="cg-hash-warn"
                    >⚠ Browser hashing unavailable &mdash; after download run
                    <code>batlehub hash-token {{ tok.value }}</code> and replace
                    the value in the config</span
                  >
                </template>
              </div>
              <div class="cg-two-col">
                <label>
                  Role
                  <select v-model="tok.role">
                    <option value="admin">admin</option>
                    <option value="user">user</option>
                    <option value="anonymous">anonymous</option>
                  </select>
                </label>
                <label
                  >User ID (optional)<input
                    v-model="tok.user_id"
                    placeholder="alice"
                /></label>
              </div>
              <button class="cg-btn-remove" @click="removeToken(auth, tok.id)">
                Remove token
              </button>
            </div>
          </template>

          <!-- OIDC auth -->
          <template v-else-if="auth.type === 'oidc'">
            <label
              >Provider name (optional)<input
                v-model="auth.oidc_name"
                placeholder="oidc"
              /><span class="cg-field-hint"
                >Used as the group prefix (e.g. <code>oidc:team-a</code>). Only
                needed when running multiple OIDC providers.</span
              ></label
            >
            <label
              >Issuer URL<input
                v-model="auth.oidc_issuer"
                placeholder="https://accounts.example.com"
            /></label>
            <div class="cg-two-col">
              <label
                >Client ID<input
                  v-model="auth.oidc_client_id"
                  placeholder="batlehub"
              /></label>
              <label
                >Client secret<input
                  v-model="auth.oidc_client_secret"
                  type="password"
                  placeholder="(optional for PKCE)"
              /></label>
            </div>
            <label
              >Redirect URI<input
                v-model="auth.oidc_redirect_uri"
                placeholder="https://batlehub.example.com/api/v1/auth/oidc/callback"
            /></label>
            <label
              >Frontend URL (dev only)<input
                v-model="auth.oidc_frontend_url"
                placeholder="http://localhost:5173"
              /><span class="cg-field-hint"
                >Leave blank in production — the callback redirects to the same
                origin.</span
              ></label
            >
            <div class="cg-two-col">
              <label
                >User ID claim<input
                  v-model="auth.oidc_user_id_claim"
                  placeholder="sub"
              /></label>
              <label
                >Role claim<input
                  v-model="auth.oidc_role_claim"
                  placeholder="role"
              /></label>
            </div>
            <label
              >Scopes (optional, comma-separated)<input
                v-model="auth.oidc_scopes"
                placeholder="openid, profile, email"
              /><span class="cg-field-hint"
                >Defaults to <code>openid, profile, email</code> when
                blank.</span
              ></label
            >

            <p class="cg-subsection-label" style="margin-top: 0.75rem">
              Role mappings
            </p>
            <span
              class="cg-field-hint"
              style="margin-bottom: 0.5rem; display: block"
              >Maps values of the <code>{{ auth.oidc_role_claim || "role" }}</code>
              claim to proxy roles. <strong>A claim value with no entry here
              falls back to <code>anonymous</code></strong> — without at least
              one <code>admin</code> mapping nobody who logs in through this
              provider can administer the server.</span
            >
            <div
              v-for="m in auth.oidc_role_mappings"
              :key="m.id"
              class="cg-condition-item"
            >
              <div class="cg-two-col">
                <label
                  >Claim value<input
                    v-model="m.claim"
                    placeholder="batlehub-admins"
                /></label>
                <label>
                  Role
                  <select v-model="m.role">
                    <option value="admin">admin</option>
                    <option value="user">user</option>
                    <option value="anonymous">anonymous</option>
                  </select>
                </label>
              </div>
              <button
                class="cg-btn-remove"
                @click="removeRoleMapping(auth, 'oidc', m.id)"
              >
                Remove mapping
              </button>
            </div>
            <button
              class="cg-btn-add"
              @click="addRoleMapping(auth.oidc_role_mappings)"
            >
              + Add role mapping
            </button>
          </template>

          <!-- Kubernetes auth -->
          <template v-else-if="auth.type === 'kubernetes'">
            <label
              >Provider name (optional)<input
                v-model="auth.k8s_name"
                placeholder="kubernetes"
              /><span class="cg-field-hint"
                >Used as the group prefix (e.g.
                <code>kubernetes:ops</code>).</span
              ></label
            >
            <label
              >API server URL (optional)<input
                v-model="auth.k8s_api_server"
                placeholder="https://kubernetes.default.svc"
              /><span class="cg-field-hint"
                >Leave blank to use the in-cluster environment variables.</span
              ></label
            >
            <label
              >CA cert path (optional)<input
                v-model="auth.k8s_ca_cert_path"
                placeholder="/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
              /><span class="cg-field-hint"
                >Defaults to the standard in-cluster CA mount.</span
              ></label
            >
            <label
              >Service account token path (optional)<input
                v-model="auth.k8s_token_path"
                placeholder="/var/run/secrets/kubernetes.io/serviceaccount/token"
              /><span class="cg-field-hint"
                >Defaults to the standard in-cluster token mount.</span
              ></label
            >
            <label
              >Audiences (comma-separated)<input
                v-model="auth.k8s_audiences"
                placeholder="batlehub"
            /></label>

            <p class="cg-subsection-label" style="margin-top: 0.75rem">
              Role mappings
            </p>
            <span
              class="cg-field-hint"
              style="margin-bottom: 0.5rem; display: block"
              >Keys are Kubernetes usernames
              (<code>system:serviceaccount:&lt;ns&gt;:&lt;name&gt;</code>) or
              group names. <strong>An identity with no entry here falls back to
              <code>anonymous</code>.</strong></span
            >
            <div
              v-for="m in auth.k8s_role_mappings"
              :key="m.id"
              class="cg-condition-item"
            >
              <div class="cg-two-col">
                <label
                  >Username or group<input
                    v-model="m.claim"
                    placeholder="system:serviceaccount:ci:builder"
                /></label>
                <label>
                  Role
                  <select v-model="m.role">
                    <option value="admin">admin</option>
                    <option value="user">user</option>
                    <option value="anonymous">anonymous</option>
                  </select>
                </label>
              </div>
              <button
                class="cg-btn-remove"
                @click="removeRoleMapping(auth, 'k8s', m.id)"
              >
                Remove mapping
              </button>
            </div>
            <button
              class="cg-btn-add"
              @click="addRoleMapping(auth.k8s_role_mappings)"
            >
              + Add role mapping
            </button>
          </template>

          <!-- GitHub Actions OIDC auth -->
          <template v-else-if="auth.type === 'actions-oidc'">
            <label
              >Provider name (optional)<input
                v-model="auth.actions_name"
                placeholder="actions-oidc"
              /><span class="cg-field-hint"
                >Used as the group prefix in RBAC group rules.</span
              ></label
            >
            <label
              >Issuer URL<input
                v-model="auth.actions_issuer"
                placeholder="https://token.actions.githubusercontent.com"
              /><span class="cg-field-hint"
                >For GitHub.com use
                <code>https://token.actions.githubusercontent.com</code>.</span
              ></label
            >
            <label
              >Audience (required)<input
                v-model="auth.actions_audience"
                placeholder="https://batlehub.example.com"
              /><span
                class="cg-field-hint"
                :class="{ 'cg-hint-required': !auth.actions_audience.trim() }"
                >The value the token's <code>aud</code> claim must equal. The
                issuer above signs for <em>every</em> repository on the forge, so
                without this <code>iss</code> only proves the caller is some CI
                job and the rules below are the whole barrier. Use this
                deployment's URL and have workflows ask for it:
                <code>core.getIDToken('https://batlehub.example.com')</code>.</span
              ></label
            >
            <label
              >User ID claim (optional)<input
                v-model="auth.actions_user_id_claim"
                placeholder="sub"
              /><span class="cg-field-hint"
                >JWT claim used as the user identifier. Defaults to
                <code>sub</code>.</span
              ></label
            >

            <!-- Rules -->
            <p class="cg-subsection-label" style="margin-top: 0.75rem">
              Rules
            </p>
            <span class="cg-field-hint" style="margin-bottom: 0.5rem; display: block"
              >Each rule assigns a role when a workflow token matches the given
              conditions.</span
            >
            <div
              v-for="rule in auth.actions_rules"
              :key="rule.id"
              class="cg-subitem"
            >
              <div class="cg-two-col">
                <label
                  >Group name (optional)<input
                    v-model="rule.group"
                    placeholder="ci-bots"
                  /><span class="cg-field-hint"
                    >Static group name assigned to matching tokens.</span
                  ></label
                >
                <label>
                  Role (optional)
                  <select v-model="rule.role">
                    <option value="">— none (group only) —</option>
                    <option value="admin">admin</option>
                    <option value="user">user</option>
                    <option value="anonymous">anonymous</option>
                  </select>
                  <span class="cg-field-hint">When blank the rule assigns groups without affecting role elevation.</span>
                </label>
              </div>
              <label
                >Group template (optional)<input
                  v-model="rule.group_template"
                  placeholder="{name}/{repository}/{ref_name}"
                /><span class="cg-field-hint"
                  >Template rendered from JWT claims.
                  <code>{repository}</code>, <code>{ref_name}</code> and any
                  other claim key are supported. Slashes are replaced with
                  dashes.</span
                ></label
              >
              <label>
                Condition match mode
                <select v-model="rule.match_mode">
                  <option value="all">all (every condition must pass)</option>
                  <option value="any">any (at least one must pass)</option>
                </select>
              </label>

              <!-- Conditions -->
              <p class="cg-subsection-label">Conditions</p>
              <div
                v-for="cond in rule.conditions"
                :key="cond.id"
                class="cg-condition-item"
              >
                <div class="cg-two-col">
                  <label
                    >JWT claim<input
                      v-model="cond.claim"
                      placeholder="repository"
                  /></label>
                  <label
                    >Pattern<input
                      v-model="cond.pattern"
                      placeholder="myorg/*"
                  /></label>
                </div>
                <label>
                  Match type
                  <select v-model="cond.match_type">
                    <option value="auto">auto (glob if * present, else exact)</option>
                    <option value="glob">glob</option>
                    <option value="regex">regex</option>
                  </select>
                </label>
                <button
                  class="cg-btn-remove"
                  @click="removeCondition(rule, cond.id)"
                >
                  Remove condition
                </button>
              </div>
              <button class="cg-btn-add" @click="addCondition(rule)">
                + Add condition
              </button>
              <div style="margin-top: 0.5rem">
                <button
                  class="cg-btn-remove"
                  @click="removeActionsRule(auth, rule.id)"
                >
                  Remove rule
                </button>
              </div>
            </div>
            <button class="cg-btn-add" @click="addActionsRule(auth)">
              + Add rule
            </button>
          </template>

          <div class="cg-provider-actions">
            <button
              v-if="auth.type === 'token'"
              class="cg-btn-add"
              @click="addToken(auth)"
            >
              + Add token
            </button>
            <span v-else />
            <button class="cg-btn-remove" @click="removeAuthProvider(auth.id)">
              Remove provider
            </button>
          </div>
        </div>
        <button class="cg-btn-add" @click="addAuthProvider">
          + Add auth provider
        </button>
      </section>

      <!-- Registries -->
      <section class="cg-section">
        <h3>Registries</h3>
        <div v-for="reg in registries" :key="reg.id" class="cg-list-item">
          <div class="cg-two-col">
            <label>Name<input v-model="reg.name" placeholder="npm" /></label>
            <label>
              Type
              <select v-model="reg.type" @change="onTypeChange(reg)">
                <option value="npm">npm</option>
                <option value="cargo">Cargo</option>
                <option value="maven">Maven</option>
                <option value="rubygems">RubyGems</option>
                <option value="composer">Composer (PHP)</option>
                <option value="pypi">PyPI (Python)</option>
                <option value="conda">Conda</option>
                <option value="nuget">NuGet (.NET)</option>
                <option value="openvsx">OpenVSX</option>
                <option value="vscode-marketplace">VS Code Marketplace</option>
                <option value="goproxy">Go Modules</option>
                <option value="terraform">Terraform</option>
                <option value="github">GitHub</option>
                <option value="forgejo">Forgejo / Gitea</option>
                <option value="gitlab">GitLab</option>
                <option value="deb">Deb (APT)</option>
                <option value="rpm">RPM (YUM/DNF)</option>
                <option value="pacman">Pacman (Arch)</option>
                <option value="jetbrains">JetBrains IDE</option>
                <option value="jetbrains-marketplace">JetBrains Marketplace</option>
                <option value="generic">Generic (raw file mirror)</option>
              </select>
            </label>
          </div>
          <div class="cg-radio-row cg-mb">
            <label class="cg-radio"
              ><input type="radio" v-model="reg.mode" value="proxy" />
              proxy</label
            >
            <label class="cg-radio" :class="{ 'cg-disabled': isProxyOnly(reg) }"
              ><input
                type="radio"
                v-model="reg.mode"
                value="local"
                :disabled="isProxyOnly(reg)"
              />
              local</label
            >
            <label class="cg-radio" :class="{ 'cg-disabled': isProxyOnly(reg) }"
              ><input
                type="radio"
                v-model="reg.mode"
                value="hybrid"
                :disabled="isProxyOnly(reg)"
              />
              hybrid</label
            >
          </div>
          <p v-if="isProxyOnly(reg)" class="cg-hint">
            {{ reg.type }} is a proxy-only registry (no private hosting), so mode
            is locked to <code>proxy</code>.
          </p>
          <label v-if="reg.mode !== 'local'">
            Upstreams (one per line)
            <textarea v-model="reg.upstreams" rows="2" />
          </label>

          <!-- Composer client config hint -->
          <div v-if="reg.type === 'composer'" class="cg-registry-hint">
            <p class="cg-hint-title">Composer client setup</p>
            <p class="cg-hint-text">
              Add a repository entry to your project's
              <code>composer.json</code>:
            </p>
            <pre class="cg-hint-code">{{ composerRepoSnippet(reg.name) }}</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem">
              Store credentials in <code>auth.json</code> (never commit this
              file):
            </p>
            <pre class="cg-hint-code">{{ composerAuthSnippet }}</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem">
              Publish via ZIP upload (must contain
              <code>composer.json</code> with <code>"name"</code> and
              <code>"version"</code>):
            </p>
            <pre class="cg-hint-code">
curl -X POST \
  -H "Authorization: Bearer &lt;token&gt;" \
  -H "Content-Type: application/zip" \
  --data-binary @vendor-pkg-1.0.0.zip \
  "/proxy/{{ reg.name }}/api/upload"</pre
            >
          </div>

          <!-- PyPI client config hint -->
          <div v-if="reg.type === 'pypi'" class="cg-registry-hint">
            <p class="cg-hint-title">PyPI client setup</p>
            <p class="cg-hint-text">
              Point pip at the proxy via <code>~/.pip/pip.conf</code>:
            </p>
            <pre class="cg-hint-code">[global]
index-url = https://your-batlehub-host/proxy/{{ reg.name }}/simple/</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem">
              Or with uv in <code>pyproject.toml</code>:
            </p>
            <pre class="cg-hint-code">[[tool.uv.index]]
name = "batlehub"
url = "https://your-batlehub-host/proxy/{{ reg.name }}/simple/"
default = true</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem" v-if="isLocalOrHybrid(reg)">
              Publish with twine (local/hybrid mode):
            </p>
            <pre class="cg-hint-code" v-if="isLocalOrHybrid(reg)">
twine upload \
  --repository-url https://your-batlehub-host/proxy/{{ reg.name }}/legacy/ \
  --username __token__ --password &lt;your-token&gt; \
  dist/*</pre>
          </div>

          <!-- Conda client config hint -->
          <div v-if="reg.type === 'conda'" class="cg-registry-hint">
            <p class="cg-hint-title">Conda client setup</p>
            <p class="cg-hint-text">
              Add the proxy as a channel in <code>~/.condarc</code>:
            </p>
            <pre class="cg-hint-code">channels:
  - https://your-batlehub-host/proxy/{{ reg.name }}
  - nodefaults</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem">
              Credentials are read from <code>~/.netrc</code> automatically.
            </p>
            <p class="cg-hint-text" style="margin-top: 0.5rem" v-if="isLocalOrHybrid(reg)">
              Publish a package (local/hybrid mode):
            </p>
            <pre class="cg-hint-code" v-if="isLocalOrHybrid(reg)">
curl -X POST \
  -H "Authorization: Bearer &lt;token&gt;" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @pkg-1.0.0-py311h0_0.tar.bz2 \
  "https://your-batlehub-host/proxy/{{ reg.name }}/linux-64/"</pre>
          </div>

          <!-- NuGet client config hint -->
          <div v-if="reg.type === 'nuget'" class="cg-registry-hint">
            <p class="cg-hint-title">NuGet client setup</p>
            <p class="cg-hint-text">
              Add the proxy as a NuGet source:
            </p>
            <pre class="cg-hint-code">dotnet nuget add source \
  https://your-batlehub-host/proxy/{{ reg.name }}/nuget/v3/index.json \
  --name {{ reg.name }}</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem">
              Or add to <code>nuget.config</code>:
            </p>
            <pre class="cg-hint-code">&lt;configuration&gt;
  &lt;packageSources&gt;
    &lt;add key="{{ reg.name }}" value="https://your-batlehub-host/proxy/{{ reg.name }}/nuget/v3/index.json" /&gt;
  &lt;/packageSources&gt;
&lt;/configuration&gt;</pre>
            <p class="cg-hint-text" style="margin-top: 0.5rem" v-if="isLocalOrHybrid(reg)">
              Publish a package (local/hybrid mode):
            </p>
            <pre class="cg-hint-code" v-if="isLocalOrHybrid(reg)">dotnet nuget push MyLib.1.0.0.nupkg \
  --api-key &lt;your-token&gt; \
  --source https://your-batlehub-host/proxy/{{ reg.name }}/nuget/v3/index.json</pre>
          </div>

          <label v-if="storageMode === 'multi'">
            Storage backend (blank = use default)
            <select v-model="reg.storage_backend">
              <option value="">— default ({{ storageDefault }}) —</option>
              <option v-for="n in backendNames" :key="n" :value="n">
                {{ n }}
              </option>
            </select>
          </label>
          <p class="cg-perm-label">
            Permissions
            <span class="cg-perm-hint"
              >(comma-separated; use <code>*</code> for all)</span
            >
          </p>
          <p class="cg-field-hint" style="margin-bottom: 0.35rem">
            The verb set is closed — one that is not on the
            <a href="/guide/access-control#verbs">list</a> is a startup error,
            not a permission granted to nobody. Common ones:
            <code>releases:read</code>, <code>releases:list</code>,
            <code>releases:publish</code>, <code>source:read</code>,
            <code>catalogue:browse</code>.
          </p>
          <div class="cg-three-col">
            <label
              >anonymous<input v-model="reg.rbac_anonymous" placeholder=""
            /></label>
            <label
              >user<input
                v-model="reg.rbac_user"
                placeholder="releases:read, source:read"
            /></label>
            <label
              >admin<input v-model="reg.rbac_admin" placeholder="*"
            /></label>
          </div>

          <!-- RBAC groups -->
          <div
            v-for="g in reg.rbac_groups"
            :key="g.id"
            class="cg-condition-item"
          >
            <div class="cg-two-col">
              <label
                >Group name<input
                  v-model="g.name"
                  placeholder="oidc:team-a"
                /><span class="cg-field-hint"
                  >Prefixed with the provider name; use
                  <code>*:team-a</code> to match the group from any
                  provider.</span
                ></label
              >
              <label
                >Permissions<input
                  v-model="g.perms"
                  placeholder="releases:read, releases:publish"
              /></label>
            </div>
            <button class="cg-btn-remove" @click="removeRbacGroup(reg, g.id)">
              Remove group
            </button>
          </div>
          <button class="cg-btn-add" @click="addRbacGroup(reg)">
            + Add group permission
          </button>

          <!-- Advanced toggle + remove row -->
          <div class="cg-registry-actions">
            <button
              class="cg-btn-advanced"
              @click="reg.showAdvanced = !reg.showAdvanced"
            >
              {{ reg.showAdvanced ? "▲ Hide advanced" : "▼ Advanced options" }}
            </button>
            <button class="cg-btn-remove" @click="removeRegistry(reg.id)">
              Remove registry
            </button>
          </div>

          <div v-if="reg.showAdvanced" class="cg-advanced">
            <!-- Access policy — RFC 0015's registry and namespace tiers -->
            <p class="cg-subsection-label">Access policy</p>
            <p class="cg-field-hint" style="margin-bottom: 0.5rem">
              The permissions above are the role translation. Grants sit
              <em>on top</em> of it and only ever widen — they cannot narrow what
              a package is already readable by. Visibility is the knob that
              narrows.
            </p>
            <div class="cg-two-col">
              <label
                >Visibility<select v-model="reg.visibility">
                  <option value="">— default (public) —</option>
                  <option value="public">public — anyone, including anonymous</option>
                  <option value="internal">internal — any authenticated user</option>
                  <option value="team">team — the owning team group only</option>
                </select></label
              >
              <label
                >Pre-release visibility<select v-model="reg.prerelease_visibility">
                  <option value="">— same as visibility —</option>
                  <option value="public">public</option>
                  <option value="internal">internal</option>
                  <option value="team">team</option>
                </select></label
              >
            </div>
            <p
              v-if="prereleaseWiderThanRelease(reg.visibility, reg.prerelease_visibility)"
              class="cg-field-hint cg-hint-required"
            >
              Pre-releases would be visible to a <em>wider</em> audience than
              releases. Legal, and almost always a typo — the setting exists to
              do the opposite.
            </p>

            <!-- [registries.grants] -->
            <div
              v-for="g in reg.grants"
              :key="g.id"
              class="cg-condition-item"
            >
              <div class="cg-two-col">
                <label
                  >Subject<input
                    v-model="g.subject"
                    placeholder="group:oidc:team-a"
                  /><span class="cg-field-hint"
                    ><code>*</code>, <code>role:admin</code>,
                    <code>group:oidc:team-a</code> (or
                    <code>group:*:team-a</code> across providers),
                    <code>user:alice</code>.</span
                  ></label
                >
                <label
                  >Verbs<input
                    v-model="g.verbs"
                    placeholder="releases:read, releases:publish"
                  /><span
                    class="cg-field-hint"
                    :class="{ 'cg-hint-required': wrongKindVerbs(g.verbs, reg.type).length > 0 }"
                    >{{
                      wrongKindVerbs(g.verbs, reg.type).length
                        ? `A ${reg.type} registry does not define ${wrongKindVerbs(g.verbs, reg.type).join(", ")} — an ecosystem verb is only grantable where it is implemented.`
                        : "Comma-separated, from the closed vocabulary."
                    }}</span
                  ></label
                >
              </div>
              <button class="cg-btn-remove" @click="removeGrant(reg.grants, g.id)">
                Remove grant
              </button>
            </div>
            <button class="cg-btn-add" @click="addGrant(reg.grants)">
              + Add registry grant
            </button>

            <label class="cg-check cg-mb" style="margin-top: 0.5rem">
              <input type="checkbox" v-model="reg.grants_shadow_enabled" />
              Shadow mode — evaluate grants, log refusals, refuse nothing
            </label>
            <label v-if="reg.grants_shadow_enabled"
              >Shadow until<input
                type="date"
                v-model="reg.grants_shadow_until"
              /><span
                class="cg-field-hint"
                :class="{ 'cg-hint-required': shadowDateInvalid(reg.grants_shadow_until) }"
                >An expiry is mandatory and must be in the future: a shadow
                serves every request that would have been refused, so one nobody
                revisits is a permanent bypass wearing a migration's
                clothes.</span
              ></label
            >

            <!-- [[registries.namespaces]] -->
            <p class="cg-subsection-label" style="margin-top: 0.75rem">
              Namespaces
            </p>
            <p class="cg-field-hint" style="margin-bottom: 0.5rem">
              A prefix of the package coordinate, not a separate object — on this
              registry the separator is
              <code>{{ namespaceSeparator(reg.type) }}</code
              >, so <code>acme</code> covers
              <code>acme{{ namespaceSeparator(reg.type) }}thing</code>. Policy set
              here overrides the registry above it for everything underneath.
            </p>
            <div
              v-for="ns in reg.namespaces"
              :key="ns.id"
              class="cg-condition-item"
            >
              <label
                >Match<input
                  v-model="ns.match_prefix"
                  :placeholder="reg.type === 'maven' ? 'com.acme' : '@acme'"
                /><span
                  class="cg-field-hint"
                  :class="{ 'cg-hint-required': !!namespaceMatchError(reg, ns) }"
                  >{{
                    namespaceMatchError(reg, ns) ||
                    "The prefix this node governs."
                  }}</span
                ></label
              >
              <div class="cg-two-col">
                <label
                  >Visibility<select v-model="ns.visibility">
                    <option value="">— inherit —</option>
                    <option value="public">public</option>
                    <option value="internal">internal</option>
                    <option value="team">team</option>
                  </select></label
                >
                <label
                  >Pre-release visibility<select v-model="ns.prerelease_visibility">
                    <option value="">— inherit —</option>
                    <option value="public">public</option>
                    <option value="internal">internal</option>
                    <option value="team">team</option>
                  </select></label
                >
              </div>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="ns.sealed" />
                Seal — stop inheriting grants from the registry above
              </label>
              <p v-if="ns.sealed" class="cg-field-hint" style="margin: -0.25rem 0 0.4rem 1.4rem">
                Writes <code>grants = {}</code>. This is the one construct that
                <em>narrows</em>: an absent block inherits, a filled one inherits
                and widens, and a seal stops inheritance. Grants written on this
                namespace are dropped while it is sealed.
              </p>
              <template v-if="!ns.sealed">
                <div
                  v-for="g in ns.grants"
                  :key="g.id"
                  class="cg-two-col"
                >
                  <label
                    >Subject<input v-model="g.subject" placeholder="group:oidc:acme"
                  /></label>
                  <label
                    >Verbs<input
                      v-model="g.verbs"
                      placeholder="releases:read, releases:publish"
                    /><button
                      class="cg-btn-remove"
                      @click="removeGrant(ns.grants, g.id)"
                    >
                      Remove
                    </button></label
                  >
                </div>
                <button class="cg-btn-add" @click="addGrant(ns.grants)">
                  + Add namespace grant
                </button>
              </template>

              <label class="cg-check cg-mb" style="margin-top: 0.5rem">
                <input type="checkbox" v-model="ns.quota_enabled" />
                Override the publish quota here
              </label>
              <div v-if="ns.quota_enabled" class="cg-two-col">
                <label
                  >Max bytes per user<input
                    v-model="ns.quota_max_bytes"
                    placeholder="1073741824"
                /></label>
                <label
                  >Max packages per user<input
                    v-model="ns.quota_max_packages"
                    placeholder="100"
                /></label>
              </div>

              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="ns.versioning_enabled" />
                Override the versioning policy here
              </label>
              <template v-if="ns.versioning_enabled">
                <label class="cg-check">
                  <input type="checkbox" v-model="ns.versioning_enforce_semver" />
                  Require valid semver
                </label>
                <div class="cg-two-col">
                  <label
                    >Immutability<select v-model="ns.versioning_immutable">
                      <option value="never">never</option>
                      <option value="released">released</option>
                      <option value="always">always</option>
                    </select></label
                  >
                  <label
                    >Version regex<input
                      v-model="ns.versioning_pattern"
                      placeholder="^\d+\.\d+\.\d+$"
                  /></label>
                </div>
                <label class="cg-check">
                  <input type="checkbox" v-model="ns.versioning_monotonic" />
                  Versions must sort strictly upward
                </label>
                <label class="cg-check cg-mb">
                  <input type="checkbox" v-model="ns.versioning_dry_run" />
                  Dry run
                </label>
              </template>

              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="ns.rules_enabled" />
                Override the gate rules here
              </label>
              <template v-if="ns.rules_enabled">
                <p class="cg-field-hint" style="margin-bottom: 0.4rem">
                  Replaces the registry's rules for everything under
                  <code>{{ ns.match_prefix || "this prefix" }}</code
                  >, rather than adding to them.
                </p>
                <label class="cg-check">
                  <input type="checkbox" v-model="ns.rules.rule_age_gate_enabled" />
                  Release age gate
                </label>
                <label v-if="ns.rules.rule_age_gate_enabled"
                  >Minimum age (seconds)<input
                    type="number"
                    v-model.number="ns.rules.rule_age_gate_min_age"
                    min="0"
                /></label>
                <label class="cg-check">
                  <input type="checkbox" v-model="ns.rules.rule_deny_latest_enabled" />
                  Deny <code>latest</code> tag resolution
                </label>
                <label class="cg-check">
                  <input
                    type="checkbox"
                    v-model="ns.rules.rule_signed_release_enabled"
                  />
                  Require a signed release
                </label>
                <label class="cg-check">
                  <input type="checkbox" v-model="ns.rules.rule_cve_gate_enabled" />
                  CVE gate
                </label>
                <label v-if="ns.rules.rule_cve_gate_enabled"
                  >Minimum severity<select v-model="ns.rules.rule_cve_gate_min_severity">
                    <option value="low">low</option>
                    <option value="moderate">moderate</option>
                    <option value="high">high</option>
                    <option value="critical">critical</option>
                  </select></label
                >
                <label class="cg-check">
                  <input
                    type="checkbox"
                    v-model="ns.rules.rule_license_gate_enabled"
                  />
                  License gate
                </label>
                <div v-if="ns.rules.rule_license_gate_enabled" class="cg-two-col">
                  <label
                    >Allowed licences<input
                      v-model="ns.rules.rule_license_gate_allow"
                      placeholder="MIT, Apache-2.0"
                  /></label>
                  <label
                    >Denied licences<input
                      v-model="ns.rules.rule_license_gate_deny"
                      placeholder="GPL-3.0"
                  /></label>
                </div>
                <label class="cg-check">
                  <input
                    type="checkbox"
                    v-model="ns.rules.rule_trusted_publisher_enabled"
                  />
                  Trusted publishers only
                </label>
                <label v-if="ns.rules.rule_trusted_publisher_enabled"
                  >Allowed publishers<input
                    v-model="ns.rules.rule_trusted_publisher_allow"
                    placeholder="npmjs"
                /></label>
                <label class="cg-check cg-mb">
                  <input
                    type="checkbox"
                    v-model="ns.rules.rule_version_gate_enabled"
                  />
                  Version gate
                </label>
                <template v-if="ns.rules.rule_version_gate_enabled">
                  <label
                    >Allowed versions (one range per line)
                    <textarea v-model="ns.rules.rule_version_gate_allow" rows="2" />
                  </label>
                  <label
                    >Blocked versions (one range per line)
                    <textarea v-model="ns.rules.rule_version_gate_block" rows="2" />
                  </label>
                </template>
              </template>

              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="ns.shadow_enabled" />
                Shadow this namespace's grants
              </label>
              <label v-if="ns.shadow_enabled"
                >Shadow until<input type="date" v-model="ns.shadow_until" /><span
                  class="cg-field-hint"
                  :class="{ 'cg-hint-required': shadowDateInvalid(ns.shadow_until) }"
                  >Must be a future date.</span
                ></label
              >

              <button class="cg-btn-remove" @click="removeNamespace(reg, ns.id)">
                Remove namespace
              </button>
            </div>
            <button class="cg-btn-add" @click="addNamespace(reg)">
              + Add namespace
            </button>

            <!-- Routing & addressing -->
            <p class="cg-subsection-label" style="margin-top: 0.75rem">
              Routing &amp; addressing
            </p>
            <label
              >Vanity hosts (optional, comma-separated)<input
                v-model="reg.hosts"
                placeholder="npm.acme.io"
              /><span class="cg-field-hint"
                >Hostnames whose <em>root</em> serves this registry, in addition
                to <code>/proxy/{{ reg.name }}/…</code>. DNS and TLS for them are
                yours to arrange.</span
              ></label
            >
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.path_routing" />
              Also serve under <code>/proxy/{{ reg.name }}/…</code>
            </label>
            <span
              v-if="!reg.path_routing"
              class="cg-field-hint"
              style="display: block; margin-bottom: 0.5rem"
              >The subpath returns <code>404</code> — a disabled ingress should
              look absent, not forbidden. Give the registry at least one host
              above (or enable subdomain routing) or nothing can reach it.</span
            >
            <template v-if="isPathAddressed(reg)">
              <label
                >Allowed upstream paths (one glob per line)
                <textarea v-model="reg.path_allow" rows="3" />
                <span class="cg-field-hint"
                  >Matched against the upstream-relative path, where
                  <code>*</code> also crosses <code>/</code>. Use
                  <code>**</code> to mirror everything.
                  <template v-if="reg.type === 'generic'"
                    ><strong>Required for generic registries</strong> — the
                    server refuses to start without it.</template
                  ></span
                >
              </label>
            </template>
            <label v-if="reg.type === 'cargo'"
              >Sparse index URL (optional)<input
                v-model="reg.index_url"
                placeholder="https://index.crates.io"
              /><span class="cg-field-hint"
                >Set for self-hosted Cargo registries (e.g. a Forgejo package
                feed).</span
              ></label
            >
            <label class="cg-check">
              <input type="checkbox" v-model="reg.search_url_disabled" />
              Disable upstream search for this registry
            </label>
            <label v-if="!reg.search_url_disabled"
              >Search URL (optional)<input
                v-model="reg.search_url"
                placeholder="https://search.maven.org"
              /><span class="cg-field-hint"
                >Overrides the built-in default (Maven and Composer have one;
                other types ignore this).</span
              ></label
            >
            <template v-if="reg.type === 'goproxy'">
              <label class="cg-check">
                <input type="checkbox" v-model="reg.vuln_db_url_disabled" />
                Disable the Go vulnerability database passthrough
              </label>
              <label v-if="!reg.vuln_db_url_disabled"
                >govulndb URL (optional)<input
                  v-model="reg.vuln_db_url"
                  placeholder="https://vuln.go.dev"
              /></label>
              <label class="cg-check">
                <input type="checkbox" v-model="reg.sumdb_disabled" />
                Disable the checksum database passthrough
              </label>
              <span
                v-if="reg.sumdb_disabled"
                class="cg-field-hint"
                style="display: block; margin-bottom: 0.5rem"
                >What a registry serving only private modules wants — a lookup
                on <code>sum.golang.org</code> would leak private module paths
                upstream.</span
              >
              <label v-else
                >Checksum database URL (optional)<input
                  v-model="reg.sumdb_url"
                  placeholder="https://sum.golang.org"
                /><span class="cg-field-hint"
                  >The other half of <code>GOPROXY</code>. Without it
                  <code>go mod download</code> still opens a direct connection to
                  <code>sum.golang.org</code> for every module it has not seen,
                  so the proxy has moved the egress rather than removed it.</span
                ></label
              >
            </template>

            <label class="cg-check">
              <input type="checkbox" v-model="reg.signed_downloads" />
              Mint signed, expiring download URLs
            </label>
            <span
              v-if="reg.signed_downloads"
              class="cg-field-hint"
              style="display: block; margin-bottom: 0.5rem"
              >For clients that authenticate a protocol document and then fetch
              the artifact with no credential — Terraform's provider install is
              the case this exists for. Without it such a registry needs
              <code>anonymous = ["releases:read", "source:read"]</code>, which
              opens <em>every</em> read on it rather than the one step that needs
              opening. Requires a secret under
              <em>Signed download URLs</em> in the Server section.</span
            >

            <!-- Package explorer visibility -->
            <p class="cg-subsection-label">Package explorer</p>
            <span class="cg-field-hint" style="display: block; margin-bottom: 0.5rem"
              >Who may browse this registry's cached package list in the UI.
              Independent of download permissions — all three are on by
              default.</span
            >
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rbac_explore_anonymous" />
              anonymous
            </label>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rbac_explore_user" /> user
            </label>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.rbac_explore_admin" /> admin
            </label>

            <label class="cg-check">
              <input type="checkbox" v-model="reg.console_fetch" />
              Let the console pull a version this instance does not hold yet
            </label>
            <span
              v-if="!reg.console_fetch"
              class="cg-field-hint"
              style="display: block; margin-bottom: 0.5rem"
              >The console stays strictly read-only. On by default: the fetch is
              a download the caller could already run with <code>curl</code>,
              through every gate that download would pass, attributed to them in
              the audit log. Inert on a local-mode registry.</span
            >

            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.upstream_detail_customised" />
              Tune the console's upstream discovery read
            </label>
            <template v-if="reg.upstream_detail_customised">
              <label class="cg-check">
                <input type="checkbox" v-model="reg.upstream_detail_enabled" />
                Show upstream-only versions on a package page
              </label>
              <div class="cg-two-col">
                <label
                  >Max upstream versions<input
                    type="number"
                    v-model.number="reg.upstream_detail_max_versions"
                    min="1"
                  /><span class="cg-field-hint"
                    >Newest-first, and the response says the cap was applied — a
                    silently shortened list is a lie about the registry. Ceiling
                    5000.</span
                  ></label
                >
                <label
                  >Negative TTL (seconds)<input
                    type="number"
                    v-model.number="reg.upstream_detail_negative_ttl_secs"
                    min="0"
                  /><span class="cg-field-hint"
                    >How long an upstream <code>404</code> is remembered, so a
                    typo or a crawler cannot turn every reload into an upstream
                    request. A connection failure is not a fact about the package
                    and is never remembered.</span
                  ></label
                >
              </div>
            </template>

            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.readme_customised" />
              Tune README capture
            </label>
            <template v-if="reg.readme_customised">
              <label class="cg-check">
                <input type="checkbox" v-model="reg.readme_enabled" />
                Store and serve READMEs
              </label>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.readme_from_archive" />
                Extract from the artifact when the metadata carries none
              </label>
              <div class="cg-two-col">
                <label
                  >Max README bytes<input
                    type="number"
                    v-model.number="reg.readme_max_bytes"
                    min="1"
                  /><span class="cg-field-hint"
                    >After decompression. Truncation is recorded and surfaced,
                    never silent. Ceiling 4 MiB — a README is a database row read
                    on every page load.</span
                  ></label
                >
                <label
                  >Remote images<select v-model="reg.readme_remote_images">
                    <option value="strip">strip — chart them, load nothing</option>
                    <option value="proxy">proxy — fetch and re-serve</option>
                  </select>
                  <span class="cg-field-hint"
                    >There is no "allow": the console's CSP is baked in at build
                    time, so it could only ever show broken images.</span
                  ></label
                >
              </div>
              <template v-if="reg.readme_remote_images === 'proxy'">
                <label
                  >Image host allowlist (comma-separated)<input
                    v-model="reg.readme_remote_image_hosts"
                    placeholder="img.shields.io, badgen.net"
                  /><span class="cg-field-hint"
                    >An entry matches the host exactly or any subdomain of it.
                    <strong>Leave blank and every host is allowed</strong>, which
                    is what <code>proxy</code> did before this key existed. An
                    image from anywhere else is chipped exactly as
                    <code>strip</code> chips it.</span
                  ></label
                >
                <label
                  >Max image bytes<input
                    type="number"
                    v-model.number="reg.readme_image_max_bytes"
                    min="1"
                  /><span class="cg-field-hint"
                    >Buffered in memory to check the type and the cap before
                    anything is stored.</span
                  ></label
                >
              </template>
            </template>

            <!-- Firewall mode -->
            <p class="cg-subsection-label">Firewall</p>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.firewall_only" />
              Firewall-only mode (enforce rules without caching)
            </label>
            <span v-if="reg.firewall_only" class="cg-field-hint" style="display: block; margin-bottom: 0.5rem"
              >Rules are evaluated but nothing is written to storage. Requests
              stream directly from upstream.</span
            >

            <!-- Cache policy -->
            <p class="cg-subsection-label">Cache policy</p>
            <div class="cg-two-col">
              <label
                >Metadata TTL (s)<input
                  v-model.number="reg.cache_metadata_ttl"
                  type="number"
                  min="0"
                /><span class="cg-field-hint">Default: 300 s</span></label
              >
              <label
                >Artifact TTL (s)<input
                  v-model="reg.cache_artifact_ttl"
                  placeholder="never"
                /><span class="cg-field-hint"
                  >Leave blank to keep forever</span
                ></label
              >
            </div>
            <div class="cg-two-col">
              <label
                >Idle eviction (days)<input
                  v-model="reg.cache_idle_days"
                  placeholder="never"
              /></label>
              <label
                >Size cap (bytes)<input
                  v-model="reg.cache_max_size_bytes"
                  placeholder="no cap"
              /></label>
            </div>
            <label
              >Keep latest N versions<input
                v-model="reg.cache_keep_latest_n"
                placeholder="keep all"
            /></label>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.cache_serve_stale" />
              Serve stale metadata when the upstream is unreachable
            </label>
            <span
              v-if="!reg.cache_serve_stale"
              class="cg-field-hint"
              style="display: block; margin-bottom: 0.5rem"
              >An upstream outage will surface as an error instead of an
              expired-but-usable answer.</span
            >

            <!-- Cache warming -->
            <p class="cg-subsection-label">Cache warming</p>
            <label
              >Packages to keep warm (comma-separated)<input
                v-model="reg.cache_warm_packages"
                placeholder="express, lodash"
              /><span class="cg-field-hint"
                >Fetched ahead of the first request so a cold cache never costs a
                user a round trip upstream.</span
              ></label
            >
            <label v-if="isPathAddressed(reg)"
              >Paths to keep warm (one per line)
              <textarea v-model="reg.cache_warm_paths" rows="2" />
              <span class="cg-field-hint"
                >Path-addressed registries warm by upstream path rather than
                package name.</span
              >
            </label>
            <div class="cg-two-col">
              <label
                >Warm latest N versions<input
                  v-model.number="reg.cache_warm_latest_n"
                  type="number"
                  min="1"
                /><span class="cg-field-hint">Default: 1</span></label
              >
              <label
                >Warm concurrency<input
                  v-model.number="reg.cache_warm_concurrency"
                  type="number"
                  min="1"
                /><span class="cg-field-hint">Default: 2</span></label
              >
            </div>

            <!-- Rate limit -->
            <p class="cg-subsection-label">Rate limiting</p>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.rate_limit_enabled" /> Enable
              per-user rate limit
            </label>
            <template v-if="reg.rate_limit_enabled">
              <div class="cg-two-col">
                <label
                  >Requests per window<input
                    v-model.number="reg.rate_limit_rps"
                    type="number"
                    min="1"
                /></label>
                <label
                  >Window (s)<input
                    v-model.number="reg.rate_limit_window"
                    type="number"
                    min="1"
                /></label>
              </div>
              <label>
                Enforcement
                <select v-model="reg.rate_limit_enforcement">
                  <option value="block">block (429)</option>
                  <option value="warn">warn (header only)</option>
                </select>
              </label>
              <p class="cg-subsection-label">Per-group overrides</p>
              <span
                class="cg-field-hint"
                style="display: block; margin-bottom: 0.5rem"
                >Members of these groups get their own budget instead of the
                registry-wide one above.</span
              >
              <div
                v-for="g in reg.rate_limit_groups"
                :key="g.id"
                class="cg-condition-item"
              >
                <label
                  >Group name<input v-model="g.name" placeholder="oidc:ci"
                /></label>
                <div class="cg-two-col">
                  <label
                    >Requests per window<input
                      v-model.number="g.requests_per_window"
                      type="number"
                      min="1"
                  /></label>
                  <label
                    >Window (s)<input
                      v-model.number="g.window_secs"
                      type="number"
                      min="1"
                  /></label>
                </div>
                <label>
                  Enforcement
                  <select v-model="g.enforcement">
                    <option value="">— inherit —</option>
                    <option value="block">block (429)</option>
                    <option value="warn">warn (header only)</option>
                  </select>
                </label>
                <button
                  class="cg-btn-remove"
                  @click="removeRateLimitGroup(reg, g.id)"
                >
                  Remove group
                </button>
              </div>
              <button class="cg-btn-add" @click="addRateLimitGroup(reg)">
                + Add group override
              </button>
            </template>

            <!-- Quota (local/hybrid only) -->
            <template v-if="isLocalOrHybrid(reg)">
              <p class="cg-subsection-label">Publish quota</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.quota_enabled" /> Enable
                publish quota
              </label>
              <template v-if="reg.quota_enabled">
                <div class="cg-two-col">
                  <label
                    >Max bytes per user<input
                      v-model="reg.quota_max_bytes"
                      placeholder="e.g. 1073741824"
                  /></label>
                  <label
                    >Max packages per user<input
                      v-model="reg.quota_max_packages"
                      placeholder="e.g. 100"
                  /></label>
                </div>
                <label
                  >Warn threshold (%)<input
                    v-model.number="reg.quota_warn_threshold_pct"
                    type="number"
                    min="1"
                    max="100"
                  /><span class="cg-field-hint"
                    >Percentage of the quota at which a warning header is
                    returned (default: 80).</span
                  ></label
                >
                <label>
                  Enforcement
                  <select v-model="reg.quota_enforcement">
                    <option value="block">block (429)</option>
                    <option value="warn">warn (header only)</option>
                  </select>
                </label>
              </template>
            </template>

            <!-- Beta channel (local/hybrid only) -->
            <template v-if="isLocalOrHybrid(reg)">
              <p class="cg-subsection-label">Beta channel</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.beta_channel_enabled" />
                Gate pre-release versions to beta members
              </label>
            </template>

            <!-- Versioning (local/hybrid only) -->
            <template v-if="isLocalOrHybrid(reg)">
              <p class="cg-subsection-label">Versioning policy</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.versioning_enabled" />
                Enforce versioning rules at publish time
              </label>
              <template v-if="reg.versioning_enabled">
                <label class="cg-check">
                  <input type="checkbox" v-model="reg.versioning_enforce_semver" />
                  Require valid semver (reject non-semver versions)
                </label>
                <label class="cg-check cg-mb">
                  <input type="checkbox" v-model="reg.versioning_allow_prerelease" />
                  Allow pre-release versions (e.g. <code>1.0.0-beta.1</code>)
                </label>
                <label
                  >Version regex (optional)<input
                    v-model="reg.versioning_pattern"
                    placeholder="^\d+\.\d+\.\d+$"
                  /><span class="cg-field-hint"
                    >Reject publishes where the version string doesn't match this
                    pattern. A pattern matching none of the ordinary version
                    shapes is refused at startup, because it would refuse every
                    publish.</span
                  ></label
                >
                <label
                  >Immutability<select v-model="reg.versioning_immutable">
                    <option value="never">never — any version may be replaced</option>
                    <option value="released">released — releases frozen, pre-releases may churn</option>
                    <option value="always">always — no version may ever be replaced</option>
                  </select>
                  <span class="cg-field-hint"
                    >A property of the bytes, not of the caller — which is what
                    lets a registry be append-only for <em>everyone</em>,
                    administrators included.
                    <code>releases:overwrite</code> grants nothing under
                    <code>always</code>.</span
                  ></label
                >
                <label class="cg-check">
                  <input type="checkbox" v-model="reg.versioning_monotonic" />
                  Refuse a version that does not sort above the newest existing one
                </label>
                <p class="cg-field-hint" style="margin: 0 0 0.4rem 1.4rem">
                  Catches what immutability cannot: republishing an
                  <em>older</em> number after a bad release. A yanked or deleted
                  version still counts as the newest. Incompatible with bulk
                  import by construction — a package's history publishes
                  oldest-first, so import with this off and turn it on after.
                </p>
                <label class="cg-check cg-mb">
                  <input type="checkbox" v-model="reg.versioning_dry_run" />
                  Dry run — record what would have been refused, refuse nothing
                </label>
              </template>
            </template>

            <!-- Signing (local/hybrid only) -->
            <template v-if="isLocalOrHybrid(reg)">
              <p class="cg-subsection-label">Artifact signing</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.signing_enabled" />
                Accept artifact signatures at publish time
              </label>
              <template v-if="reg.signing_enabled">
                <label class="cg-check cg-mb">
                  <input type="checkbox" v-model="reg.signing_required" />
                  Require signature (reject publishes without
                  <code>X-Artifact-Signature</code>)
                </label>
                <label
                  >Allowed signature types (comma-separated, optional)<input
                    v-model="reg.signing_allowed_types"
                    placeholder="pgp, ed25519"
                  /><span class="cg-field-hint"
                    >Leave blank to accept any type.</span
                  ></label
                >
                <label class="cg-check cg-mb">
                  <input
                    type="checkbox"
                    v-model="reg.signing_verify_on_download"
                  />
                  Verify stored signatures on every download
                </label>
                <label v-if="reg.signing_verify_on_download"
                  >Trusted Ed25519 public keys (comma-separated hex)<input
                    v-model="reg.signing_trusted_keys"
                    placeholder="3b6a27bcceb6a42d62a3a8d02a6f0d73…"
                  /><span class="cg-field-hint"
                    >Verification fails closed: a stored signature that matches
                    no key here — or whose type cannot be verified — fails the
                    download with <code>502</code>. Only unsigned artifacts are
                    exempt.</span
                  ></label
                >
              </template>
            </template>

            <!-- Repository metadata signing (deb/rpm) -->
            <template v-if="reg.type === 'deb' || reg.type === 'rpm'">
              <p class="cg-subsection-label">Repository metadata signing</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.repo_signing_enabled" />
                Sign the generated repository metadata
              </label>
              <template v-if="reg.repo_signing_enabled">
                <label
                  >Ed25519 seed (64 hex chars)<input
                    v-model="reg.repo_signing_seed_hex"
                    placeholder="0123456789abcdef…"
                  /><span class="cg-field-hint"
                    >Keep this stable — it is the identity APT/DNF clients pin
                    to. Treat it as a secret.</span
                  ></label
                >
                <div class="cg-two-col">
                  <label
                    >User ID (optional)<input
                      v-model="reg.repo_signing_user_id"
                      placeholder="BatleHub Repo &lt;repo@example.com&gt;"
                  /></label>
                  <label
                    >Created (unix seconds, optional)<input
                      v-model="reg.repo_signing_created"
                      placeholder="1700000000"
                    /><span class="cg-field-hint"
                      >Part of the key fingerprint, so it must not change.</span
                    ></label
                  >
                </div>
              </template>
            </template>

            <!-- VSIX signing (openvsx / vscode-marketplace) — RFC 0020 -->
            <template
              v-if="
                (reg.type === 'openvsx' || reg.type === 'vscode-marketplace') &&
                isLocalOrHybrid(reg)
              "
            >
              <p class="cg-subsection-label">VSIX signing</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.vsx_signing_enabled" />
                Sign every VSIX this registry publishes
              </label>
              <template v-if="reg.vsx_signing_enabled">
                <label
                  >Ed25519 seed (64 hex chars)<input
                    v-model="reg.vsx_signing_seed_hex"
                    placeholder="0123456789abcdef…"
                  /><span class="cg-field-hint"
                    >`batlehub-cli vsx keygen` prints one. A secret: keep it
                    out of the file with ${VAR}. A current editor's Extensions
                    view installs only signed entries; what the registry
                    proxies is relayed with the upstream's own signature
                    whether or not this is set.</span
                  ></label
                >
                <label
                  >Key id (optional)<input
                    v-model="reg.vsx_signing_key_id"
                    placeholder="2026-09"
                  /><span class="cg-field-hint"
                    >The id the public key is served under. Defaults to a
                    digest of the key; must change when the key does.</span
                  ></label
                >
              </template>
            </template>

            <!-- SBOM -->
            <!-- Retention (local/hybrid only) — RFC 0016 -->
            <template v-if="isLocalOrHybrid(reg)">
              <p class="cg-subsection-label">Retention</p>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.retention_enabled" />
                Reclaim old locally published versions
              </label>
              <template v-if="reg.retention_enabled">
                <p
                  class="cg-field-hint"
                  :class="{ 'cg-hint-required': !retentionReclaims(reg) }"
                  style="margin-bottom: 0.5rem"
                >
                  {{
                    retentionReclaims(reg)
                      ? "Keep conditions are a union: a version survives if any one of them vetoes its removal."
                      : "Set at least one keep condition. A retention block with none is the single most destructive line in the file — the union of vetoes is empty, so the first live run takes every version. The block is left out of the config until one is set."
                  }}
                </p>
                <div class="cg-two-col">
                  <label
                    >Keep newest N versions<input
                      v-model="reg.retention_keep_versions"
                      placeholder="10"
                  /></label>
                  <label
                    >Keep for N days<input
                      v-model="reg.retention_keep_for_days"
                      placeholder="90"
                  /></label>
                </div>
                <div class="cg-two-col">
                  <label
                    >Keep if pulled within N days<input
                      v-model="reg.retention_keep_if_pulled_days"
                      placeholder="30"
                  /><span class="cg-field-hint"
                      >Needs download data to mean anything — see the floor
                      below.</span
                    ></label
                  >
                  <label
                    >Download-signal floor (days)<input
                      v-model="reg.retention_download_signal_floor_days"
                      placeholder="14"
                    /><span class="cg-field-hint"
                      >Ignore the pull signal until the registry has been
                      recording it this long, so a freshly enabled instance does
                      not read "never pulled" as "unused".</span
                    ></label
                  >
                </div>
                <label class="cg-check cg-mb">
                  <input type="checkbox" v-model="reg.retention_keep_yanked" />
                  Keep yanked versions
                </label>
                <div class="cg-two-col">
                  <label
                    >Tombstone detail for N days<input
                      v-model="reg.retention_tombstone_detail_for_days"
                      placeholder="180"
                    /><span class="cg-field-hint"
                      >Compaction discards a deleted version's checksum,
                      publisher and metadata. The floor is 30 days — below that
                      an auditor asking what was removed last month gets no
                      answer.</span
                    ></label
                  >
                  <label
                    >Reclaim delay (ms)<input
                      type="number"
                      v-model.number="reg.retention_reclaim_delay_ms"
                      min="0"
                    /><span class="cg-field-hint"
                      >Pause between deletions, to keep a sweep from saturating
                      the storage backend.</span
                    ></label
                  >
                </div>
                <label class="cg-check cg-mb">
                  <input type="checkbox" v-model="reg.retention_dry_run" />
                  Dry run — report what would be reclaimed, delete nothing
                </label>
                <p
                  v-if="!reg.retention_dry_run"
                  class="cg-field-hint cg-hint-required"
                  style="margin: -0.25rem 0 0.5rem 1.4rem"
                >
                  Live. This is the one irreversible half of the three dry-run
                  switches, which is why it is the only one that defaults to on.
                </p>
              </template>
            </template>

            <p class="cg-subsection-label">SBOM</p>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.sbom_enabled" />
              Generate an SBOM for cached and published artifacts
            </label>
            <template v-if="reg.sbom_enabled">
              <label
                >Formats (comma-separated)<input
                  v-model="reg.sbom_formats"
                  placeholder="spdx, cyclonedx"
              /></label>
              <label class="cg-check">
                <input type="checkbox" v-model="reg.sbom_required" />
                Reject publishes with no discoverable dependency manifest
              </label>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.sbom_fetch_upstream" />
                Prefer a pre-built SBOM from the upstream when one exists
              </label>
            </template>

            <!-- Integrity -->
            <p class="cg-subsection-label">Artifact integrity</p>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.integrity_customised" />
              Customise checksum verification
            </label>
            <span
              v-if="!reg.integrity_customised"
              class="cg-field-hint"
              style="display: block; margin-bottom: 0.5rem"
              >Defaults apply: verify against any advertised checksum and block
              on a mismatch; warn (never block) when the upstream advertises
              none.</span
            >
            <template v-if="reg.integrity_customised">
              <label class="cg-check">
                <input type="checkbox" v-model="reg.integrity_enabled" />
                Verify advertised checksums
              </label>
              <label class="cg-check">
                <input
                  type="checkbox"
                  v-model="reg.integrity_block_on_mismatch"
                />
                Fail the download on a mismatch
              </label>
              <label class="cg-check">
                <input
                  type="checkbox"
                  v-model="reg.integrity_require_metadata"
                />
                Block downloads with no advertised checksum
              </label>
              <label class="cg-check cg-mb">
                <input
                  type="checkbox"
                  v-model="reg.integrity_verify_on_serve"
                />
                Re-hash cached bytes on every serve
              </label>
              <span
                v-if="reg.integrity_verify_on_serve"
                class="cg-field-hint"
                style="display: block; margin-bottom: 0.5rem"
                >Catches storage corruption or tampering after caching, at the
                cost of hashing the bytes on each serve.</span
              >
              <label v-if="reg.integrity_require_metadata"
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.integrity_bypass_roles"
                  placeholder="admin"
                /><span class="cg-field-hint"
                  >Roles exempt from the missing-checksum gate. A mismatch is
                  never bypassable.</span
                ></label
              >
            </template>

            <!-- Rules -->
            <p class="cg-subsection-label">Rules</p>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rule_age_gate_enabled" />
              Release age gate
            </label>
            <template v-if="reg.rule_age_gate_enabled">
              <label
                >Min age (s)<input
                  v-model.number="reg.rule_age_gate_min_age"
                  type="number"
                  min="0"
                /><span class="cg-field-hint"
                  >Reject downloads of packages younger than this many
                  seconds.</span
                ></label
              >
              <label class="cg-check cg-mb">
                <input
                  type="checkbox"
                  v-model="reg.rule_age_gate_deny_missing_timestamp"
                />
                Deny packages whose upstream publishes no timestamp
              </label>
              <span
                v-if="reg.rule_age_gate_deny_missing_timestamp"
                class="cg-field-hint"
                style="display: block; margin-bottom: 0.5rem"
                >Otherwise the check is skipped for those packages and the
                download is allowed. Worth enabling on registries where the
                field is optional (e.g. conda).</span
              >
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_age_gate_bypass_roles"
                  type="text"
                  placeholder="admin"
                /><span class="cg-field-hint"
                  >Roles that can bypass the gate. Leave blank for
                  none.</span
                ></label
              >
            </template>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rule_deny_latest_enabled" />
              Deny <code>@latest</code> / unpinned version requests
            </label>
            <template v-if="reg.rule_deny_latest_enabled">
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_deny_latest_bypass_roles"
                  type="text"
                  placeholder="admin"
                /><span class="cg-field-hint"
                  >Roles that can bypass the gate. Leave blank for
                  none.</span
                ></label
              >
            </template>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rule_signed_release_enabled" />
              Require a signed release
            </label>
            <template v-if="reg.rule_signed_release_enabled">
              <span class="cg-field-hint" style="display: block; margin-bottom: 0.5rem"
                >Gates on the upstream's best-effort signature signal (a
                <code>.asc</code>/<code>.sig</code> asset, an extension
                signature blob) — <strong>not</strong> cryptographic
                verification. Use <em>Artifact signing</em> above for that.</span
              >
              <label class="cg-check cg-mb">
                <input
                  type="checkbox"
                  v-model="reg.rule_signed_release_deny_missing"
                />
                Deny registries that report no signature signal at all
              </label>
              <span
                v-if="reg.rule_signed_release_deny_missing"
                class="cg-field-hint"
                style="display: block; margin-bottom: 0.5rem"
                >npm, PyPI, crates.io and Maven report no signal, so this denies
                them outright rather than skipping the check.</span
              >
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_signed_release_bypass_roles"
                  type="text"
                  placeholder="admin"
              /></label>
            </template>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rule_license_gate_enabled" />
              Licence gate
            </label>
            <template v-if="reg.rule_license_gate_enabled">
              <span class="cg-field-hint" style="display: block; margin-bottom: 0.5rem"
                >The licence is read from the archive, so it is unknown until
                the artifact has been fetched once — the first request for an
                uncached package is governed by
                <em>Allow unknown licences</em>, not by the lists. Matching is
                case-insensitive but literal:
                <code>MIT</code> does not match <code>MIT OR Apache-2.0</code>.</span
              >
              <label
                >Allowed licences (comma-separated, optional)<input
                  v-model="reg.rule_license_gate_allow"
                  placeholder="MIT, Apache-2.0, BSD-3-Clause"
                /><span class="cg-field-hint"
                  >When set, a declared licence matching none of these is
                  refused.</span
                ></label
              >
              <label
                >Denied licences (comma-separated, optional)<input
                  v-model="reg.rule_license_gate_deny"
                  placeholder="AGPL-3.0, SSPL-1.0"
                /><span class="cg-field-hint"
                  >Checked first, so a deny entry always wins.</span
                ></label
              >
              <label class="cg-check">
                <input
                  type="checkbox"
                  v-model="reg.rule_license_gate_allow_unknown"
                />
                Allow unknown licences
              </label>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.rule_license_gate_block" />
                Block downloads (otherwise warn-only, surfaced in the UI)
              </label>
              <p v-if="licenseGateDeniesEverything(reg)" class="cg-hint">
                <strong>This combination denies every download.</strong>
                <code>{{ reg.type }}</code> has no manifest parser, so the
                licence is always unknown — and unknown licences are being
                blocked. Licence extraction covers cargo, maven, npm, nuget and
                pypi. Allow unknown licences, or drop the rule here.
              </p>
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_license_gate_bypass_roles"
                  type="text"
                  placeholder="admin"
              /></label>
            </template>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rule_version_gate_enabled" />
              Version gate
            </label>
            <template v-if="reg.rule_version_gate_enabled">
              <label
                >Allowed versions (one per line, optional)
                <textarea v-model="reg.rule_version_gate_allow" rows="2" />
                <span class="cg-field-hint"
                  >Exact versions or semver ranges
                  (<code>&gt;=1.2.0, &lt;2.0.0</code>). When set, anything
                  matching none of them is rejected. One per line because a
                  range contains commas.</span
                >
              </label>
              <label
                >Blocked versions (one per line, optional)
                <textarea v-model="reg.rule_version_gate_block" rows="2" />
                <span class="cg-field-hint"
                  >Specific versions or ranges with known issues.</span
                >
              </label>
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_version_gate_bypass_roles"
                  type="text"
                  placeholder="admin"
              /></label>
              <span
                v-if="
                  !reg.rule_version_gate_allow.trim() &&
                  !reg.rule_version_gate_block.trim()
                "
                class="cg-field-hint"
                style="display: block; margin-bottom: 0.5rem"
                >Both lists are empty, so the rule is left out of the config —
                it would gate nothing.</span
              >
            </template>
            <label class="cg-check">
              <input type="checkbox" v-model="reg.rule_cve_gate_enabled" />
              CVE gate (uses <code>[vulnerability_scan]</code> findings)
            </label>
            <template v-if="reg.rule_cve_gate_enabled">
              <label
                >Minimum severity<select v-model="reg.rule_cve_gate_min_severity">
                  <option value="unknown">Unknown</option>
                  <option value="low">Low</option>
                  <option value="medium">Medium</option>
                  <option value="high">High</option>
                  <option value="critical">Critical</option>
                </select></label
              >
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="reg.rule_cve_gate_block" />
                Block downloads (otherwise warn-only, surfaced in the UI)
              </label>
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_cve_gate_bypass_roles"
                  type="text"
                  placeholder="admin"
                /><span class="cg-field-hint"
                  >Roles that can bypass the gate even when blocking.</span
                ></label
              >
            </template>
            <label class="cg-check">
              <input
                type="checkbox"
                v-model="reg.rule_trusted_publisher_enabled"
              />
              Trusted publisher allowlist
            </label>
            <template v-if="reg.rule_trusted_publisher_enabled">
              <label
                >Allowed publishers<input
                  v-model="reg.rule_trusted_publisher_allow"
                  type="text"
                  placeholder="my-org, trusted-user"
                /><span class="cg-field-hint"
                  >Comma-separated org/user/scope names. Supported for
                  GitHub, GitLab, Forgejo, npm, OpenVSX, and VS Code
                  Marketplace; not yet for Cargo — an unsupported registry
                  denies every request.</span
                ></label
              >
              <label
                >Bypass roles (comma-separated, optional)<input
                  v-model="reg.rule_trusted_publisher_bypass_roles"
                  type="text"
                  placeholder="admin"
                /><span class="cg-field-hint"
                  >Roles that can bypass the gate.</span
                ></label
              >
            </template>

            <!-- Feature flags -->
            <p class="cg-subsection-label">Feature flags</p>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.feature_flags_socket_badge" />
              Show socket.dev supply-chain badge per version
            </label>

            <!-- Upstream auth -->
            <p class="cg-subsection-label">Upstream authentication</p>
            <label>
              Auth type
              <select v-model="reg.upstream_auth_type">
                <option value="">None</option>
                <option value="bearer">Bearer token</option>
                <option value="basic">Basic (username + password)</option>
                <option value="header">Custom header</option>
              </select>
            </label>
            <template v-if="reg.upstream_auth_type === 'bearer'">
              <label
                >Token<input
                  v-model="reg.upstream_auth_token"
                  placeholder="ghp_..."
              /></label>
            </template>
            <template v-else-if="reg.upstream_auth_type === 'basic'">
              <div class="cg-two-col">
                <label
                  >Username<input v-model="reg.upstream_auth_username"
                /></label>
                <label
                  >Password<input
                    v-model="reg.upstream_auth_password"
                    type="password"
                /></label>
              </div>
            </template>
            <template v-else-if="reg.upstream_auth_type === 'header'">
              <div class="cg-two-col">
                <label
                  >Header name<input
                    v-model="reg.upstream_auth_header_name"
                    placeholder="X-API-Key"
                /></label>
                <label
                  >Header value<input v-model="reg.upstream_auth_header_value"
                /></label>
              </div>
            </template>

            <!-- TLS -->
            <p class="cg-subsection-label">Upstream TLS</p>
            <label
              >Custom CA certificate path (optional)<input
                v-model="reg.tls_ca_cert_path"
                placeholder="/etc/ssl/corp-ca.pem"
              /><span class="cg-field-hint"
                >PEM-encoded CA to trust for this registry's upstream. Only
                needed for self-signed certificates.</span
              ></label
            >

            <!-- Per-registry egress proxy -->
            <p class="cg-subsection-label">Egress proxy</p>
            <label class="cg-check cg-mb">
              <input type="checkbox" v-model="reg.proxy_enabled" />
              Use a different proxy from the global one
            </label>
            <template v-if="reg.proxy_enabled">
              <label
                >Proxy URL<input
                  v-model="reg.proxy_url"
                  placeholder="http://proxy.corp:3128"
              /></label>
              <div class="cg-two-col">
                <label
                  >Username (optional)<input v-model="reg.proxy_username"
                /></label>
                <label
                  >Password (optional)<input
                    v-model="reg.proxy_password"
                    type="password"
                /></label>
              </div>
              <label
                >No-proxy list (optional)<input
                  v-model="reg.proxy_no_proxy"
                  placeholder="localhost,127.0.0.1,.internal"
                /><span class="cg-field-hint"
                  >Comma-separated hosts or suffixes that bypass the
                  proxy.</span
                ></label
              >
            </template>
          </div>

        </div>
        <button class="cg-btn-add" @click="addRegistry">+ Add registry</button>
      </section>

      <!-- IP Blocking -->
      <section class="cg-section">
        <h3>IP Blocking</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="ipBlocking.enabled" /> Enable
          fail2ban-style IP blocking
        </label>
        <template v-if="ipBlocking.enabled">
          <div class="cg-two-col">
            <label
              >Violation threshold<input
                v-model.number="ipBlocking.violation_threshold"
                type="number"
                min="1"
              /><span class="cg-field-hint"
                >Violations before auto-block</span
              ></label
            >
            <label
              >Window (s)<input
                v-model.number="ipBlocking.violation_window_secs"
                type="number"
                min="1"
            /></label>
          </div>
          <label
            >Ban duration (s)<input
              v-model.number="ipBlocking.ban_duration_secs"
              type="number"
              min="1"
          /></label>
          <label
            >Trigger on status codes<input
              v-model="ipBlocking.trigger_on_status"
              placeholder="429, 401"
            /><span class="cg-field-hint"
              >Comma-separated HTTP status codes that count as violations.</span
            ></label
          >
          <p class="cg-hint">
            Trusted proxies are configured once for the whole server, under
            <strong>Server</strong> above — this section's own
            <code>trusted_proxies</code> key is deprecated.
          </p>
        </template>
      </section>

      <!-- Vulnerability scan -->
      <section class="cg-section">
        <h3>Vulnerability scan</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="vulnerabilityScan.enabled" /> Periodically
          re-check cached SBOMs against the OSV database
        </label>
        <template v-if="vulnerabilityScan.enabled">
          <label
            >Interval (s)<input
              v-model.number="vulnerabilityScan.interval_secs"
              type="number"
              min="60"
            /><span class="cg-field-hint"
              >How often to re-scan. Default 86400 (daily).</span
            ></label
          >
          <label
            >OSV API URL (optional)<input
              v-model="vulnerabilityScan.osv_api_url"
              placeholder="https://api.osv.dev"
            /><span class="cg-field-hint"
              >Leave blank to use the public OSV API.</span
            ></label
          >
          <label
            >Batch size<input
              v-model.number="vulnerabilityScan.batch_size"
              type="number"
              min="1"
            /><span class="cg-field-hint"
              >SBOMs processed per page. Default 100.</span
            ></label
          >
        </template>
      </section>

      <!-- Statistics -->
      <section class="cg-section">
        <h3>Statistics</h3>
        <label class="cg-check">
          <input type="checkbox" v-model="stats.metrics_enabled" />
          Expose Prometheus metrics on <code>/metrics</code>
        </label>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="stats.history_enabled" />
          Record request history for the dashboard trends
        </label>
        <label v-if="stats.history_enabled"
          >History retention (days)<input
            v-model.number="stats.history_retention_days"
            type="number"
            min="0"
          /><span class="cg-field-hint"
            >Rows older than this are pruned. Default 30.
            <code>0</code> disables pruning rather than disabling history —
            untick the box above for that.</span
          ></label
        >
        <span
          v-else
          class="cg-field-hint"
          style="display: block"
          >The dashboard's trend charts stay empty; live counters still
          work.</span
        >
      </section>

      <!-- Subdomain routing -->
      <section class="cg-section">
        <h3>Subdomain routing</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="subdomainRouting.enabled" />
          Derive a host for every registry from its name
        </label>
        <template v-if="subdomainRouting.enabled">
          <label
            >Base domain<input
              v-model="subdomainRouting.base_domain"
              placeholder="hub.example.com"
            /><span class="cg-field-hint"
              >A registry named <code>npm1</code> is then served at
              <code>npm1.hub.example.com</code>. Required — the server refuses to
              start with wildcard routing on and no base domain. Registry names
              must be valid DNS labels.</span
            ></label
          >
          <label>
            Advertised scheme
            <select v-model="subdomainRouting.scheme">
              <option value="https">https</option>
              <option value="http">http</option>
            </select>
            <span class="cg-field-hint"
              >Only used when rendering public URLs in the API and UI; a
              request's own scheme decides routing.</span
            >
          </label>
        </template>
      </section>

      <!-- Egress proxy -->
      <section class="cg-section">
        <h3>Egress proxy</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="upstreamProxy.enabled" />
          Route upstream fetches through an HTTP proxy
        </label>
        <template v-if="upstreamProxy.enabled">
          <label
            >Proxy URL<input
              v-model="upstreamProxy.url"
              placeholder="http://proxy.corp:3128"
            /><span class="cg-field-hint"
              >Applies to every registry that does not override it in its
              advanced options.</span
            ></label
          >
          <div class="cg-two-col">
            <label
              >Username (optional)<input v-model="upstreamProxy.username"
            /></label>
            <label
              >Password (optional)<input
                v-model="upstreamProxy.password"
                type="password"
            /></label>
          </div>
          <label
            >No-proxy list (optional)<input
              v-model="upstreamProxy.no_proxy"
              placeholder="localhost,127.0.0.1,.internal"
            /><span class="cg-field-hint"
              >Comma-separated hosts or suffixes that bypass the proxy.</span
            ></label
          >
        </template>
      </section>

      <!-- Notifications -->
      <section class="cg-section">
        <h3>Notifications</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="notifications.enabled" />
          Send notifications on registry events
        </label>
        <template v-if="notifications.enabled">
          <p class="cg-subsection-label">Outbound channels</p>
          <div
            v-for="ch in notifications.channels"
            :key="ch.id"
            class="cg-list-item"
          >
            <div class="cg-two-col">
              <label
                >Name<input v-model="ch.name" placeholder="ops-slack"
              /></label>
              <label>
                Type
                <select v-model="ch.type">
                  <option value="slack">Slack</option>
                  <option value="teams">Microsoft Teams</option>
                  <option value="webhook">Generic webhook</option>
                  <option value="email">Email (SMTP)</option>
                </select>
              </label>
            </div>
            <template v-if="ch.type === 'email'">
              <div class="cg-two-col">
                <label
                  >SMTP host<input v-model="ch.smtp_host" placeholder="smtp.example.com"
                /></label>
                <label
                  >SMTP port<input
                    v-model.number="ch.smtp_port"
                    type="number"
                    min="1"
                    max="65535"
                /></label>
              </div>
              <div class="cg-two-col">
                <label
                  >SMTP user (optional)<input v-model="ch.smtp_user"
                /></label>
                <label
                  >SMTP password (optional)<input
                    v-model="ch.smtp_password"
                    type="password"
                /></label>
              </div>
              <label
                >From<input v-model="ch.from" placeholder="batlehub@example.com"
              /></label>
              <label
                >To (comma-separated)<input
                  v-model="ch.to"
                  placeholder="ops@example.com"
              /></label>
              <label class="cg-check cg-mb">
                <input type="checkbox" v-model="ch.tls" /> Use STARTTLS
              </label>
            </template>
            <template v-else>
              <label
                >Webhook URL<input
                  v-model="ch.url"
                  placeholder="https://hooks.slack.com/services/…"
              /></label>
              <label v-if="ch.type === 'webhook'"
                >HMAC secret (optional)<input
                  v-model="ch.secret"
                  type="password"
                /><span class="cg-field-hint"
                  >When set, each POST carries an
                  <code>X-BatleHub-Signature-256</code> header so the receiver
                  can verify it.</span
                ></label
              >
            </template>
            <label
              >Timeout (s)<input
                v-model.number="ch.timeout_secs"
                type="number"
                min="1"
              /><span class="cg-field-hint">Default: 10</span></label
            >
            <button class="cg-btn-remove" @click="removeChannel(ch.id)">
              Remove channel
            </button>
          </div>
          <button class="cg-btn-add" @click="addChannel">+ Add channel</button>

          <p class="cg-subsection-label" style="margin-top: 1rem">
            Inbound webhooks
          </p>
          <span class="cg-field-hint" style="display: block; margin-bottom: 0.5rem"
            >Endpoints external systems can POST events to, at
            <code>/api/v1/webhooks/inbound/&lt;name&gt;</code>.</span
          >
          <div
            v-for="hook in notifications.inbound"
            :key="hook.id"
            class="cg-condition-item"
          >
            <div class="cg-two-col">
              <label
                >Name<input v-model="hook.name" placeholder="ci-scanner"
              /></label>
              <label
                >HMAC secret (optional)<input
                  v-model="hook.secret"
                  type="password"
                /><span class="cg-field-hint"
                  >Verifies <code>X-Hub-Signature-256</code>. Without it any
                  payload is accepted.</span
                ></label
              >
            </div>
            <button class="cg-btn-remove" @click="removeInboundHook(hook.id)">
              Remove webhook
            </button>
          </div>
          <button class="cg-btn-add" @click="addInboundHook">
            + Add inbound webhook
          </button>
        </template>
      </section>

      <!-- OpenTelemetry -->
      <section class="cg-section">
        <h3>OpenTelemetry</h3>
        <label class="cg-check cg-mb">
          <input type="checkbox" v-model="otel.enabled" /> Enable tracing
        </label>
        <template v-if="otel.enabled">
          <label
            >OTLP gRPC endpoint<input
              v-model="otel.endpoint"
              placeholder="http://localhost:4317"
          /></label>
          <label
            >Service name<input
              v-model="otel.service_name"
              placeholder="batlehub"
          /></label>
        </template>
      </section>
    </div>

    <!-- ── RIGHT: live preview ─────────────────────────────────────────── -->
    <div class="cg-preview">
      <div class="cg-preview-header">
        <span class="cg-filename">config.toml</span>
        <div class="cg-actions">
          <button class="cg-btn-action" @click="copyToml">
            {{ copied ? "Copied!" : "Copy" }}
          </button>
          <button class="cg-btn-action" @click="downloadToml">Download</button>
        </div>
      </div>
      <pre class="cg-code"><code v-html="highlightedToml" /></pre>
    </div>
  </div>
</template>

<style scoped>
/* One column is the floor, not the small-screen special case. The side-by-side
   arrangement is what gets asked for, in the `@container` block at the bottom,
   and only once there is room for it — the previous rules asked the *window*
   whether there was room, which is a different question on a page carrying a
   272px sidebar, and was the reason the form had become unusable at 1366 and
   1440.

   Flex-wrap rather than a grid, and the container declared here rather than on
   a wrapper, so that the two are the same element: an element cannot size its
   own grid columns from a query against itself, but its children can read it,
   and `flex-basis` on the children is the same layout expressed where the
   query is legal. `inline-size` and not `size` — the height here is the
   content's, and a container that must resolve its own height first cannot
   have one. */
.cg-root {
  container: cg / inline-size;
  display: flex;
  flex-wrap: wrap;
  gap: 2rem;
  align-items: flex-start;
  margin-top: 1.5rem;
  width: 100%;
}

/* ── Form column ────────────────────────────────────────────────────── */
.cg-form {
  flex: 1 1 100%;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.cg-section {
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  padding: 1rem 1.2rem;
}

/* Pixel Small — DESIGN.md spends it on exactly this: "a panel heading". The
   tracking is the Tracking Ladder's 0.04em step for a Silkscreen label rather
   than the 0.05em this was carrying, which was not a step at all. */
.cg-section h3 {
  margin: 0 0 0.75rem;
  font-family: var(--face-display);
  font-size: var(--t-px-sm);
  font-weight: 700;
  color: var(--ink);
  text-transform: uppercase;
  letter-spacing: 0.04em;
  border-bottom: 1px solid var(--vp-c-divider);
  padding-bottom: 0.4rem;
}

label {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  font-size: var(--t-body);
  color: var(--vp-c-text-2);
  margin-bottom: 0.45rem;
}

input[type="text"],
input[type="number"],
input[type="password"],
input:not([type]),
select,
textarea {
  padding: 0.35rem 0.6rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  font-size: var(--t-body);
  font-family: var(--vp-font-family-mono);
  width: 100%;
  box-sizing: border-box;
  transition: border-color 0.15s;
}

input:focus,
select:focus,
textarea:focus {
  outline: none;
  border-color: var(--vp-c-brand-1);
}

textarea {
  resize: vertical;
}

/* ── Radios + checkboxes ──────────────────────────────────────────── */
.cg-radio-row {
  display: flex;
  gap: 1.2rem;
  flex-wrap: wrap;
}

.cg-radio,
.cg-check {
  display: flex;
  flex-direction: row;
  align-items: center;
  gap: 0.35rem;
  font-size: var(--t-body);
  color: var(--vp-c-text-1);
  cursor: pointer;
  margin-bottom: 0;
}

.cg-mb {
  margin-bottom: 0.6rem;
}

.cg-radio.cg-disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.cg-hint {
  font-size: var(--t-meta);
  color: var(--vp-c-text-2);
  margin: 0 0 0.6rem;
}

.cg-hint code {
  font-size: var(--t-meta);
}

/* ── Grid layouts ──────────────────────────────────────────────────
   `auto-fit` + a floor, rather than a fixed count unpicked by a breakpoint.
   These sit at four different nesting depths (a section, a list item, a
   condition row inside a list item) so the width available to them is never
   the width of anything a media query can name; what they can promise is that
   a field is 13rem or it is on its own line. `min(100%, …)` is what keeps that
   floor from becoming an overflow when the column really is narrower. */
.cg-two-col {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 13rem), 1fr));
  gap: 0.75rem;
}

.cg-three-col {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 10rem), 1fr));
  gap: 0.6rem;
}

/* ── List items ──────────────────────────────────────────────────── */
.cg-list-item {
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  padding: 0.75rem;
  margin-bottom: 0.75rem;
  background: var(--vp-c-bg-soft);
}

.cg-subitem {
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  padding: 0.6rem;
  margin-bottom: 0.5rem;
  background: var(--vp-c-bg);
}

.cg-condition-item {
  border: 1px dashed var(--vp-c-divider);
  border-radius: var(--radius);
  padding: 0.5rem;
  margin-bottom: 0.4rem;
  background: var(--vp-c-bg-soft);
}

/* ── Advanced panel ──────────────────────────────────────────────── */
.cg-advanced {
  border-top: 1px solid var(--vp-c-divider);
  margin-top: 0.75rem;
  padding-top: 0.75rem;
}

.cg-subsection-label {
  margin: 0.6rem 0 0.3rem;
  font-size: var(--t-meta);
  font-weight: 600;
  color: var(--vp-c-text-2);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

/* ── Labels / hints ──────────────────────────────────────────────── */
.cg-perm-label {
  margin: 0.4rem 0 0.25rem;
  font-size: var(--t-body);
  color: var(--vp-c-text-2);
  font-weight: 500;
}

.cg-field-hint {
  font-size: var(--t-meta);
  color: var(--vp-c-text-3);
  margin-top: 0.15rem;
}

.cg-field-hint code {
  font-size: var(--t-meta);
}

/* A required field left blank. The generated config is still emitted (so the
   preview stays live), but it will not load until this is filled. */
.cg-hint-required {
  color: var(--vp-c-danger-1);
}

.cg-perm-hint {
  font-size: var(--t-meta);
  font-weight: 400;
  color: var(--vp-c-text-3);
}

/* ── Buttons ─────────────────────────────────────────────────────── */
.cg-btn-add {
  display: inline-block;
  padding: 0.3rem 0.8rem;
  font-size: var(--t-body);
  border: 1px dashed var(--vp-c-brand-2);
  border-radius: var(--radius);
  color: var(--vp-c-brand-1);
  background: transparent;
  cursor: pointer;
  transition: background 0.15s;
}
.cg-btn-add:hover {
  background: var(--vp-c-brand-soft);
}

.cg-btn-remove {
  font-size: var(--t-meta);
  padding: 0.2rem 0.6rem;
  margin-top: 0.3rem;
  border: 1px solid var(--vp-c-danger-1);
  border-radius: var(--radius);
  color: var(--vp-c-danger-1);
  background: transparent;
  cursor: pointer;
  transition: background 0.15s;
}
.cg-btn-remove:hover {
  background: color-mix(
    in srgb,
    var(--vp-c-danger-1) 10%,
    transparent
  );
}

.cg-btn-advanced {
  display: inline-block;
  margin: 0;
  padding: 0.2rem 0.6rem;
  font-size: var(--t-meta);
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  background: transparent;
  color: var(--vp-c-text-2);
  cursor: pointer;
  transition:
    background 0.15s,
    border-color 0.15s;
}
.cg-btn-advanced:hover {
  border-color: var(--vp-c-brand-1);
  color: var(--vp-c-brand-1);
  background: var(--vp-c-brand-soft);
}

/* ── Action rows (add/remove on same line) ───────────────────────── */
.cg-provider-actions,
.cg-registry-actions {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 0.75rem;
}

/* ── Preview column ────────────────────────────────────────────────
   Stacked below the form until the container says otherwise, so it is sized
   here as a panel and re-sized as a sticky rail in the `@container` block. */
.cg-preview {
  flex: 1 1 100%;
  min-width: 0;
  height: 520px;
  display: flex;
  flex-direction: column;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  overflow: hidden;
}

.cg-preview-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0.5rem 0.9rem;
  background: var(--vp-c-bg-soft);
  border-bottom: 1px solid var(--vp-c-divider);
  flex-shrink: 0;
}

.cg-filename {
  font-size: var(--t-body);
  font-family: var(--vp-font-family-mono);
  color: var(--vp-c-text-2);
}

.cg-actions {
  display: flex;
  gap: 0.5rem;
}

.cg-btn-action {
  font-size: var(--t-meta);
  padding: 0.25rem 0.7rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  cursor: pointer;
  transition:
    border-color 0.15s,
    background 0.15s;
}

.cg-btn-action:hover {
  border-color: var(--vp-c-brand-1);
  background: var(--vp-c-brand-soft);
  color: var(--vp-c-brand-1);
}

.cg-code {
  margin: 0;
  padding: 0.9rem;
  font-size: var(--t-meta);
  line-height: 1.55;
  font-family: var(--vp-font-family-mono);
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  overflow-y: auto;
  flex: 1 1 0;
  white-space: pre;
}

/* ── Registry-type hints ─────────────────────────────────────────── */
.cg-registry-hint {
  border: 1px solid var(--vp-c-brand-soft);
  border-left: 1px solid var(--vp-c-brand-1);
  border-radius: var(--radius);
  padding: 0.65rem 0.8rem;
  margin: 0.5rem 0;
  background: var(--vp-c-brand-soft);
}

.cg-hint-title {
  font-size: var(--t-meta);
  font-weight: 600;
  color: var(--vp-c-brand-1);
  margin: 0 0 0.35rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.cg-hint-text {
  font-size: var(--t-meta);
  color: var(--vp-c-text-2);
  margin: 0 0 0.25rem;
}

.cg-hint-text code {
  font-size: var(--t-meta);
}

.cg-hint-code {
  font-size: var(--t-meta);
  font-family: var(--vp-font-family-mono);
  background: var(--vp-c-bg);
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  padding: 0.5rem 0.65rem;
  margin: 0.25rem 0 0;
  white-space: pre;
  overflow-x: auto;
  color: var(--vp-c-text-1);
  line-height: 1.5;
}

/* ── Responsive ────────────────────────────────────────────────────
   Three steps, each asked of the container and each stated as what it adds.
   The numbers are the widths the arrangement needs, not the widths of any
   particular screen: whether a 1440px window clears 60rem here depends on the
   sidebar, and that is precisely what the component should not have to know.
   ────────────────────────────────────────────────────────────────────────── */

/* 60rem = 960px, which is what this page hands the component on a 1366px
   laptop — the width the old 1300px window breakpoint got wrong by exactly
   the sidebar. Below it the preview goes under the form rather than beside
   it; above it there is room for a form column that still fits two 13rem
   fields after the preview has taken its share. The preview is a proportion
   with both ends pinned: never so narrow that a TOML line has nowhere to go,
   never so wide that it is reading room taken from the form it previews. */
@container cg (min-width: 60rem) {
  .cg-form {
    flex: 1 1 0;
  }

  .cg-preview {
    flex: 0 0 clamp(22rem, 34%, 35rem);
    position: sticky;
    top: calc(var(--vp-nav-height) + 1rem);
    height: calc(100vh - var(--vp-nav-height) - 2rem);
    min-height: 480px;
  }
}

/* 90rem = 1440px, reached on a 1920px screen. Only here is the form column
   itself wide enough that one stack of sections leaves half the row empty and
   a text input is 450px for a port number. The old rule asked this of a 1600px
   *window*, which on this page meant splitting a 442px column in two: inputs
   came out 80px wide, and that is the state the page was reported in. */
@container cg (min-width: 90rem) {
  .cg-form {
    display: block;
    columns: 2;
    column-gap: 1rem;
  }

  .cg-section {
    break-inside: avoid;
    margin-bottom: 1rem;
  }
}

/* ── Argon2 hash status ───────────────────────────────────────────── */
.cg-label-note {
  font-size: var(--t-meta);
  font-weight: 400;
  color: var(--vp-c-text-2);
  margin-left: 0.25rem;
}

.cg-hash-status {
  margin-top: 0.3rem;
  margin-bottom: 0.25rem;
  font-size: var(--t-meta);
  line-height: 1.4;
}

.cg-hash-computing {
  color: var(--vp-c-text-2);
}

/* Ready is a fact and takes ink; "not hashed yet" is waiting, which is copper's
   whole job. Neither needs a `.dark` twin — the tokens flip with the rendition,
   which is what the two hard-coded greens and yellows were doing by hand. */
.cg-hash-ready {
  color: var(--ink);
}

.cg-hash-warn {
  color: var(--copper);
}

.cg-hash-status code {
  font-size: var(--t-meta);
  background: var(--vp-code-bg);
  padding: 0.1em 0.3em;
  border-radius: var(--radius);
}
</style>

<style>
/* TOML syntax tokens.
   ────────────────────────────────────────────────────────────────────────────
   This is the site's one hand-rolled highlighter — every other code block goes
   through Shiki. It carried sixteen literal hex values, a GitHub-derived
   palette in two hand-maintained renditions, and one of the eight failed AA
   against this pane's own background (`--vp-c-bg` → `--ground`, paper):
   comment #6e7781 at 4.15:1. The other seven measured 4.60 to 6.93 and passed.
   `#6e7781` is the same value rejected when the Shiki theme was chosen for the
   rest of the site; it survived here because nothing had ever looked, and the
   rendered gate could not look — the default form state emits no comment, so
   that span never rendered on the scanned page (see docs/build/design-routes.mjs).

   Ratios are axe's, measured in the browser against the painted pixels, not
   computed from the token values. Calibrated first: axe returns 5.63:1 for
   --accent and 7.24:1 for --ink-dim, the two figures tokens.css asserts.

   Four colours now, not eight, because the palette has four and The One
   Synthetic Rule caps it. TOML has few enough token classes that the
   distinctions that matter survive: a section header is the accent, a key is
   ink, a value is copper, and the punctuation and comments recede into dim ink.
   Every ratio below is one `tokens.css` already asserts in both renditions, so
   there is nothing left here to measure separately — and no `.dark` twin to
   keep in step, since the tokens flip with the ground. */
.cg-hl-comment  { color: var(--ink-dim); font-style: italic; }
.cg-hl-bracket  { color: var(--ink-dim); }
.cg-hl-table    { color: var(--accent); font-weight: 600; }
.cg-hl-key      { color: var(--ink); }
.cg-hl-eq       { color: var(--ink-dim); }
.cg-hl-string   { color: var(--copper); }
.cg-hl-number   { color: var(--copper); }
.cg-hl-bool     { color: var(--accent); }
</style>
