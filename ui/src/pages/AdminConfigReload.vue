<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, onMounted, onUnmounted } from "vue";
import {
  discardPendingReload,
  applyPendingReload,
  reloadConfig,
  listConfigChanges,
} from "@/client/sdk.gen";
import type { PendingReloadSnapshot, ConfigChangeRow, ConfigWarning } from "@/client/types.gen";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { extractMessage } from "@/composables/useApi";
import { formatDate } from "@/lib/format";
import { API_BASE_URL } from "@/config";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import ConfigReadOnlyView from "@/components/admin/ConfigReadOnlyView.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Alert } from "@/components/ui/alert";
import { AsyncState } from "@/components/ui/async-state";
import { Announcer } from "@/components/ui/announcer";
import { ChevronRight } from "@lucide/vue";

const { t } = useI18n();

const { authFetch } = useAuthFetch();

// ── State ─────────────────────────────────────────────────────────────────────

/**
 * Whether this instance reloads config at runtime: `true`, `false`, or `null`
 * for "not known yet".
 *
 * Read from `is_readonly` on the config content, which the server sets to
 * `!hot_reload_enabled` — never inferred from a status code. It was
 * `pending.status !== 503`, so any 503 at all (an ingress or a database
 * outage) made this page announce `BATLEHUB_DISABLE_HOT_RELOAD=1` and withdraw
 * the reload controls: confident misinformation, at the moment an operator
 * most needs the page to be right.
 */
const hotReloadEnabled = ref<boolean | null>(null);
/** Why the pending reload could not be read, when it could not. */
const pendingError = ref<string | null>(null);
/**
 * The editor is a tool and the pending diff is the decision; while a reload
 * is staged the editor folds behind one control instead of putting an
 * 18-row textarea between the operator and the Apply they came for.
 */
const editorOpen = ref(false);
const pendingReload = ref<PendingReloadSnapshot | null>(null);
const changeHistory = ref<ConfigChangeRow[]>([]);

const loadingPending = ref(false);
const loadingForce = ref(false);

/**
 * Both of these change a running instance that other people depend on, and
 * neither had a confirmation — the page's own copy said so out loud ("applies
 * it immediately — no confirmation step"). Announcing an irreversible action is
 * not the same as confirming it (PRODUCT.md principle 2).
 */
const confirmApply = ref(false);
const confirmForce = ref(false);

/** A pending reload past its TTL cannot be applied; the button 404s. */
const pendingExpired = computed(() =>
  pendingReload.value?.expires_at ? new Date(pendingReload.value.expires_at) <= new Date() : false,
);

/**
 * How many things the pending diff actually changes, for the dialog's count.
 * RFC 0037's three lists name every changed key — a restart-only one
 * included, which the older fields never saw — so they win when present;
 * `restart_required` may also carry a key an earlier reload left owed.
 */
const pendingChangeCount = computed(() => {
  const d = pendingReload.value?.diff;
  if (!d) return 0;
  const keys =
    (d.applied?.length ?? 0) +
    (d.partially_applied?.length ?? 0) +
    (d.restart_required?.length ?? 0);
  if (keys > 0) return keys;
  return (
    (d.added_registries?.length ?? 0) +
    (d.removed_registries?.length ?? 0) +
    (d.changed_registries?.length ?? 0) +
    (d.access_config_changed ? 1 : 0) +
    (d.limits_changed ? 1 : 0)
  );
});
/** RFC 0037 §6.5's three lists, in the order an operator acts on them. */
const reloadLists = computed(() => {
  const d = pendingReload.value?.diff;
  return [
    { key: "adminConfigReload.reloadApplied", paths: d?.applied ?? [], restart: false },
    {
      key: "adminConfigReload.reloadPartiallyApplied",
      paths: d?.partially_applied ?? [],
      restart: false,
    },
    {
      key: "adminConfigReload.reloadRestartRequired",
      paths: d?.restart_required ?? [],
      restart: true,
    },
  ];
});
const loadingApply = ref(false);
const loadingDiscard = ref(false);
const loadingHistory = ref(false);
const errorMsg = ref<string | null>(null);
const successMsg = ref<string | null>(null);
const expandedRow = ref<string | null>(null);

let pollTimer: ReturnType<typeof setInterval> | null = null;

// ── Config warnings ───────────────────────────────────────────────────────────
//
// Non-fatal problems with the config currently in force. `activeWarnings` comes
// from the server; `dismissedCodes` only hides them for this session — the
// underlying config is unchanged, so they come back on the next visit until the
// TOML is fixed.

const activeWarnings = ref<ConfigWarning[]>([]);
const candidateWarnings = ref<ConfigWarning[]>([]);
const dismissedCodes = ref<Set<string>>(new Set());
const visibleWarnings = computed(() =>
  activeWarnings.value.filter((w) => !dismissedCodes.value.has(`${w.code}@${w.path}`)),
);

function dismissWarning(w: ConfigWarning) {
  dismissedCodes.value = new Set(dismissedCodes.value).add(`${w.code}@${w.path}`);
}

async function fetchWarnings() {
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/config/warnings`);
    if (!res.ok) return;
    const body = (await res.json()) as { warnings?: ConfigWarning[] };
    activeWarnings.value = body.warnings ?? [];
    // A warning that has come back is worth showing again.
    dismissedCodes.value = new Set(
      [...dismissedCodes.value].filter((key) =>
        activeWarnings.value.some((w) => `${w.code}@${w.path}` === key),
      ),
    );
  } catch {
    // non-critical — the panel stays as it was
  }
}

// ── Config editor ─────────────────────────────────────────────────────────────

const configContent = ref("");
const configReadonly = ref(false);
const configLoadError = ref<string | null>(null);
const configLoading = ref(true);
const editorValidating = ref(false);
const editorCreating = ref(false);
const editorSuccess = ref<string | null>(null);
/**
 * Neither success nor failure: the call was accepted but staged nothing, so the
 * green box would promise an apply step that is not there.
 */
const editorNotice = ref<string | null>(null);
const editorError = ref<string | null>(null);
const validatedContent = ref<string | null>(null);

async function loadConfigContent() {
  configLoadError.value = null;
  configLoading.value = true;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/config/content`);
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string } | null;
      configLoadError.value = body?.error ?? `HTTP ${res.status}`;
      return;
    }
    const body = (await res.json()) as { content: string; is_readonly: boolean };
    configContent.value = body.content;
    configReadonly.value = body.is_readonly;
    hotReloadEnabled.value = !body.is_readonly;
  } catch (e) {
    configLoadError.value = extractMessage(e);
  } finally {
    configLoading.value = false;
  }
}

function retryLoad() {
  void Promise.all([loadConfigContent(), fetchPending(), fetchWarnings()]);
}

async function validateConfigContent() {
  editorValidating.value = true;
  editorSuccess.value = null;
  editorNotice.value = null;
  editorError.value = null;
  validatedContent.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/config/validate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content: configContent.value }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    const body = (await res.json().catch(() => ({}))) as { warnings?: ConfigWarning[] };
    candidateWarnings.value = body.warnings ?? [];
    validatedContent.value = configContent.value;
    editorSuccess.value = t("adminConfigReload.configValidStageIt", {
      action: t("adminConfigReload.createPendingReload"),
    });
  } catch (e) {
    editorError.value = extractMessage(e);
  } finally {
    editorValidating.value = false;
  }
}

async function createPendingFromContent() {
  editorCreating.value = true;
  editorSuccess.value = null;
  editorNotice.value = null;
  editorError.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/config/from-content`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content: validatedContent.value ?? configContent.value }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    const body = (await res.json().catch(() => ({}))) as {
      warnings?: ConfigWarning[];
      pending_created?: boolean;
    };
    candidateWarnings.value = body.warnings ?? [];
    validatedContent.value = null;
    if (body.pending_created) {
      editorSuccess.value = t("adminConfigReload.pendingCreatedReviewIt");
    } else {
      // The server deduped: these exact bytes were the last config it loaded, so
      // there is nothing to rebuild and nothing staged. Saying "created" here is
      // what sends the admin to an Apply button that answers "no pending reload".
      editorNotice.value = t("adminConfigReload.nothingToStage");
    }
    await fetchPending();
  } catch (e) {
    editorError.value = extractMessage(e);
  } finally {
    editorCreating.value = false;
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async function fetchPending() {
  loadingPending.value = true;
  try {
    // Raw fetch, because 404 is an answer here: nothing is staged.
    const resp = await authFetch(`${API_BASE_URL}/api/v1/admin/config/pending`);
    if (resp.status === 404) {
      pendingReload.value = null;
      pendingError.value = null;
    } else if (resp.ok) {
      pendingReload.value = await resp.json();
      pendingError.value = null;
    } else if (!(resp.status === 503 && hotReloadEnabled.value === false)) {
      // A 503 is the expected answer only where hot reload is known to be
      // off; anywhere else it is a failure, and said as one.
      const body = (await resp.json().catch(() => ({}))) as { error?: string } | null;
      pendingError.value = body?.error ?? `HTTP ${resp.status}`;
    }
  } catch (e) {
    pendingError.value = extractMessage(e);
  } finally {
    loadingPending.value = false;
  }
}

async function fetchHistory() {
  loadingHistory.value = true;
  try {
    const { data } = await listConfigChanges({ query: { per_page: 20 } });
    changeHistory.value = (data as { items?: ConfigChangeRow[] })?.items ?? [];
  } catch {
    // non-critical — history list will remain empty
  } finally {
    loadingHistory.value = false;
  }
}

async function forceReload() {
  loadingForce.value = true;
  errorMsg.value = null;
  successMsg.value = null;
  try {
    const { data, error: apiErr } = await reloadConfig();
    if (apiErr) throw new Error(extractMessage(apiErr));
    const diff = (data as { diff?: { added_registries: string[]; removed_registries: string[] } })
      ?.diff;
    successMsg.value = t("adminConfigReload.reloadedDiff", {
      added: diff?.added_registries.length ?? 0,
      removed: diff?.removed_registries.length ?? 0,
    });
    await Promise.all([fetchPending(), fetchHistory(), fetchWarnings()]);
  } catch (e: unknown) {
    errorMsg.value = extractMessage(e);
  } finally {
    loadingForce.value = false;
  }
}

async function applyPending() {
  loadingApply.value = true;
  errorMsg.value = null;
  successMsg.value = null;
  try {
    const { data, error: apiErr } = await applyPendingReload();
    if (apiErr) throw new Error(extractMessage(apiErr));
    const diff = (data as { diff?: { added_registries: string[]; removed_registries: string[] } })
      ?.diff;
    successMsg.value = t("adminConfigReload.appliedDiff", {
      added: diff?.added_registries.length ?? 0,
      removed: diff?.removed_registries.length ?? 0,
    });
    pendingReload.value = null;
    candidateWarnings.value = [];
    await Promise.all([fetchHistory(), fetchWarnings()]);
  } catch (e: unknown) {
    errorMsg.value = extractMessage(e);
  } finally {
    loadingApply.value = false;
  }
}

async function discardPending() {
  loadingDiscard.value = true;
  errorMsg.value = null;
  try {
    const { error: apiErr } = await discardPendingReload();
    if (apiErr) throw new Error(extractMessage(apiErr));
    pendingReload.value = null;
  } catch (e: unknown) {
    errorMsg.value = extractMessage(e);
  } finally {
    loadingDiscard.value = false;
  }
}

const expiresIn = computed(() => {
  if (!pendingReload.value) return "";
  const secs = Math.max(
    0,
    Math.round((new Date(pendingReload.value.expires_at).getTime() - Date.now()) / 1000),
  );
  if (secs > 60) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${secs}s`;
});

onMounted(() => {
  // The interval is installed *before* the initial loads are awaited. Behind
  // the await, `onUnmounted` could already have run — it would find
  // `pollTimer` still null, clear nothing, and the interval would then be
  // installed on a dead component and poll `/admin/config/pending` every 5 s
  // for the rest of the session, one leaked timer per visit. Navigating away
  // from this page before four requests settle is not an edge case on a slow
  // instance.
  pollTimer = setInterval(() => void fetchPending(), 5_000);
  void Promise.all([fetchPending(), fetchHistory(), loadConfigContent(), fetchWarnings()]);
});
onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = null;
});
</script>

<template>
  <div class="space-y-6">
    <!-- An outcome announced, not only rendered (RFC 0004-bis §2.6). Every bulk
         result, block, warm and cache invalidation in this console reached
         sighted users only — on the surface whose audience includes the one
         person able to perform destructive actions. -->
    <Announcer :message="successMsg ?? ''" />
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <PageHeader variant="display" :title="t('adminConfigReload.configReload')" />

    <!--
      The config could not be read: say so, say what it costs, offer a retry.
      Until it loads the page does not know whether hot reload is on, so the
      controls below stay visible and disabled rather than vanishing or
      guessing — and nothing claims the instance is configured one way or the
      other.
    -->
    <AsyncState
      v-if="configLoadError"
      :loading="false"
      :error="configLoadError"
      :title="t('adminConfigReload.configCouldNotLoad')"
      :on-retry="retryLoad"
    >
      <template #error="{ error }">
        <Alert variant="destructive" class="space-y-2">
          <p class="font-medium">{{ t("adminConfigReload.configCouldNotLoad") }}</p>
          <p class="text-xs break-words">{{ error }}</p>
          <p class="text-xs">{{ t("adminConfigReload.controlsWaitForConfig") }}</p>
          <Button variant="outline" size="sm" @click="retryLoad">{{ t("common.retry") }}</Button>
        </Alert>
      </template>
    </AsyncState>

    <!-- Status: hot reload disabled — only when the server said so. -->
    <Alert v-if="hotReloadEnabled === false">
      <i18n-t keypath="adminConfigReload.hotReloadDisabled" tag="span">
        <template #flag><code>BATLEHUB_DISABLE_HOT_RELOAD=1</code></template>
      </i18n-t>
    </Alert>

    <!-- Config warnings: valid config, but something an operator should know -->
    <Card v-if="visibleWarnings.length">
      <CardHeader>
        <CardTitle>{{
          t("adminConfigReload.configurationWarnings", { count: visibleWarnings.length })
        }}</CardTitle>
      </CardHeader>
      <CardContent class="space-y-2">
        <!-- Opaque copper edge: alpha on the line that carries the contrast
             was the one thing the other components had stopped doing. -->
        <div
          v-for="w in visibleWarnings"
          :key="`${w.code}@${w.path}`"
          class="flex items-start gap-3 border border-copper px-4 py-2 text-sm text-copper"
        >
          <!-- `min-w-0` so a long config path can shrink, and the code/badge
               pair wraps: a warning names a TOML key, and TOML keys are long
               enough to push the page sideways on a phone. -->
          <div class="min-w-0 flex-1 space-y-1">
            <div class="flex flex-wrap items-center gap-2">
              <code class="font-mono text-xs break-all">{{ w.path }}</code>
              <Badge variant="outline">{{ w.code }}</Badge>
            </div>
            <p>{{ w.message }}</p>
          </div>
          <Button variant="ghost" size="sm" @click="dismissWarning(w)">{{
            t("common.dismiss")
          }}</Button>
        </div>
      </CardContent>
    </Card>

    <!--
      The decision before the tool.

      Measured before this move: the Config Editor heading sat at y=440 with a
      450px textarea under it, and "Pending Reload" — the thing an operator came
      to accept or refuse — began at y=930, below the fold on a 900px viewport.
      The editor is a tool; the diff is the decision surface. Its empty state is
      one line, so it costs ~90px when nothing is staged and buys the whole
      decision when something is.
    -->
    <Card v-if="hotReloadEnabled !== false">
      <CardHeader>
        <CardTitle>{{ t("adminConfigReload.pendingReload") }}</CardTitle>
      </CardHeader>
      <CardContent>
        <AsyncState
          v-if="pendingError"
          :loading="false"
          :error="pendingError"
          :title="t('adminConfigReload.pendingCouldNotLoad')"
          :on-retry="fetchPending"
        />
        <div v-else-if="loadingPending && !pendingReload" class="text-sm text-muted-foreground">
          {{ t("adminConfigReload.loading") }}
        </div>
        <div v-else-if="!pendingReload" class="text-sm text-muted-foreground">
          {{ t("adminConfigReload.noPendingReloadThe") }}
        </div>
        <div v-else class="space-y-3">
          <div class="flex flex-wrap gap-x-4 gap-y-1 text-sm">
            <span
              ><strong>{{ t("adminConfigReload.sourceLabel") }}</strong>
              {{ pendingReload.source }}</span
            >
            <span
              ><strong>{{ t("adminConfigReload.createdLabel") }}</strong>
              {{ formatDate(pendingReload.created_at) }}</span
            >
            <span
              ><strong>{{ t("adminConfigReload.expiresIn") }}</strong> {{ expiresIn }}</span
            >
          </div>

          <!--
            What the *candidate* would warn about (RFC 0004-bis A4).

            `PendingReload` has held these since it was written — `apply()`
            promotes them to the live store — and `PendingReloadSnapshot`
            dropped them, so the one surface that exists to review a change
            before applying it could not show what the change would warn about.
            The page worked around it by remembering the warnings from the
            `validate` call that staged the reload, which is lost the moment
            anyone reloads the page or arrives from a file-watcher reload they
            did not stage themselves.
          -->
          <div v-if="pendingReload.warnings?.length" class="space-y-1">
            <p class="text-xs font-medium text-copper">
              {{
                t("adminConfigReload.candidateWarnsAbout", {
                  count: pendingReload.warnings.length,
                })
              }}
            </p>
            <div
              v-for="w in pendingReload.warnings"
              :key="`pending-${w.code}@${w.path}`"
              class="border border-copper px-3 py-1.5 text-xs text-copper"
            >
              <code class="font-mono break-all">{{ w.path }}</code>
              — {{ w.message }}
            </div>
          </div>
          <div class="flex gap-2 flex-wrap">
            <Badge v-for="r in pendingReload.diff.added_registries" :key="r" variant="copper"
              >+{{ r }}</Badge
            >
            <Badge v-for="r in pendingReload.diff.removed_registries" :key="r" variant="copper"
              >-{{ r }}</Badge
            >
            <!--
              `fields` and `access_config_changed` are on the wire and were both
              discarded: a changed registry rendered as a bare `~npm`, and a
              change to RBAC alone rendered *nothing at all* — an empty badge
              row that invites a blind apply. This is the operator's decision
              surface; it has to say what it is asking them to accept.
            -->
            <Badge v-for="r in pendingReload.diff.changed_registries" :key="r.name" variant="copper"
              >~{{ r.name }}<span v-if="r.fields?.length"> ({{ r.fields.join(", ") }})</span></Badge
            >
            <Badge v-if="pendingReload.diff.access_config_changed" variant="copper">{{
              t("adminConfigReload.accessConfigChanged")
            }}</Badge>
            <Badge v-if="pendingReload.diff.limits_changed" variant="copper">{{
              t("adminConfigReload.limitsChanged")
            }}</Badge>
          </div>
          <!--
            Which keys this reload actually applies (RFC 0037 §6.5). Before
            these lists, a change to `database.url` staged, applied, and showed
            an empty diff: the one change that does nothing until a restart was
            the one the page could not show.
          -->
          <dl class="space-y-1 text-xs">
            <template v-for="row in reloadLists" :key="row.key">
              <div v-if="row.paths.length">
                <dt :class="row.restart ? 'font-medium text-copper' : 'font-medium'">
                  {{ t(row.key) }}
                </dt>
                <dd class="flex flex-wrap gap-1">
                  <code v-for="p in row.paths" :key="p" class="font-mono break-all">{{ p }}</code>
                  <span v-if="row.restart && pendingReload.diff.restart_reason">
                    — {{ pendingReload.diff.restart_reason }}</span
                  >
                </dd>
              </div>
            </template>
          </dl>
          <!-- The view's one filled action: Apply. -->
          <div class="flex gap-2">
            <Button
              size="sm"
              :disabled="loadingApply || pendingExpired"
              @click="confirmApply = true"
            >
              {{ loadingApply ? t("adminConfigReload.applying") : t("adminConfigReload.apply") }}
            </Button>
            <Button size="sm" variant="outline" :disabled="loadingDiscard" @click="discardPending">
              {{
                loadingDiscard ? t("adminConfigReload.discarding") : t("adminConfigReload.discard")
              }}
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>

    <!-- Feedback -->
    <Alert v-if="successMsg" variant="success">{{ successMsg }}</Alert>
    <Alert v-if="errorMsg" variant="destructive">{{ errorMsg }}</Alert>

    <!--
      Config Editor — only once the config has loaded and is writable. A
      failed load used to leave an empty 18-row textarea under the error, as
      if the config were blank; the read-only case is its own screen rather
      than this one with the controls greyed out.
    -->
    <template v-if="!configLoading && !configLoadError">
      <Card v-if="!configReadonly">
        <CardHeader class="flex-row flex-wrap items-center justify-between gap-2">
          <CardTitle>{{ t("adminConfigReload.configEditor") }}</CardTitle>
          <Button
            v-if="pendingReload && !editorOpen"
            variant="outline"
            size="sm"
            @click="editorOpen = true"
          >
            {{ t("adminConfigReload.editConfig") }}
          </Button>
        </CardHeader>
        <CardContent v-if="!pendingReload || editorOpen" class="space-y-3">
          <textarea
            v-model="configContent"
            rows="18"
            class="w-full border border-input bg-background font-mono text-xs p-3 resize-y"
            spellcheck="false"
            :aria-label="t('adminConfigReload.configTomlContent')"
            @input="validatedContent = null"
          />
          <Alert v-if="editorSuccess" variant="success">{{ editorSuccess }}</Alert>
          <div v-if="editorNotice" class="border border-copper px-4 py-2 text-sm text-copper">
            {{ editorNotice }}
          </div>
          <div v-if="candidateWarnings.length" class="space-y-2">
            <p class="text-sm text-muted-foreground">
              <i18n-t keypath="adminConfigReload.validWithWarnings" tag="span">
                <template #count>{{ candidateWarnings.length }}</template>
              </i18n-t>
            </p>
            <div
              v-for="w in candidateWarnings"
              :key="`${w.code}@${w.path}`"
              class="border border-copper px-4 py-2 text-sm text-copper"
            >
              <code class="font-mono text-xs">{{ w.path }}</code> — {{ w.message }}
            </div>
          </div>
          <Alert v-if="editorError" variant="destructive">{{ editorError }}</Alert>
          <!-- One filled action per view: staging is it here — unless a reload
               is already staged, where Apply above is, and staging another
               becomes the secondary control. -->
          <div class="flex gap-2 flex-wrap">
            <Button
              variant="outline"
              :disabled="editorValidating || editorCreating || hotReloadEnabled !== true"
              @click="validateConfigContent"
            >
              {{
                editorValidating
                  ? t("adminConfigReload.validating")
                  : t("adminConfigReload.validate")
              }}
            </Button>
            <Button
              :variant="pendingReload ? 'outline' : 'default'"
              :disabled="
                editorCreating || editorValidating || !validatedContent || hotReloadEnabled !== true
              "
              @click="createPendingFromContent"
            >
              {{
                editorCreating
                  ? t("tokensPage.creating")
                  : t("adminConfigReload.createPendingReload")
              }}
            </Button>
            <Button variant="ghost" @click="loadConfigContent">{{
              t("adminConfigReload.reloadFromDisk")
            }}</Button>
          </div>
        </CardContent>
      </Card>

      <ConfigReadOnlyView v-else :content="configContent" :error="configLoadError" />
    </template>

    <!-- Force reload: an outlined control — the confirmation is where its
         weight lives, not a second crimson fill on the page. Shown while
         hot reload is unknown, but disabled until the server has said. -->
    <Card v-if="hotReloadEnabled !== false">
      <CardHeader>
        <CardTitle>{{ t("adminConfigReload.forceReloadNow") }}</CardTitle>
      </CardHeader>
      <CardContent class="space-y-2">
        <p class="text-sm text-muted-foreground">{{ t("adminConfigReload.reReadsTheConfig") }}</p>
        <p
          v-if="hotReloadEnabled === null && configLoadError"
          class="text-xs text-muted-foreground"
        >
          {{ t("adminConfigReload.controlsWaitForConfig") }}
        </p>
        <Button
          variant="outline"
          :disabled="loadingForce || hotReloadEnabled !== true"
          @click="confirmForce = true"
        >
          {{ loadingForce ? t("adminConfigReload.reloading") : t("adminConfigReload.reloadNow") }}
        </Button>
      </CardContent>
    </Card>

    <DestructiveConfirm
      :open="confirmApply"
      :action="t('adminConfigReload.apply')"
      :count="pendingChangeCount"
      :item-noun="t('adminConfigReload.changeNoun')"
      :item-noun-plural="t('adminConfigReload.changeNounPlural')"
      :scope="t('adminConfigReload.thisInstance')"
      :loading="loadingApply"
      reversible
      allow-empty
      @confirm="
        () => {
          confirmApply = false;
          applyPending();
        }
      "
      @update:open="confirmApply = $event"
    />

    <!--
      Not `reversible`: force-reload re-reads whatever is on disk and applies it
      unseen, so the operator is accepting a change they have not been shown.
      The typed name is the point.
    -->
    <DestructiveConfirm
      :open="confirmForce"
      :action="t('adminConfigReload.forceReloadNow')"
      :count="1"
      :item-noun="t('adminConfigReload.configNoun')"
      :scope="t('adminConfigReload.thisInstance')"
      :consequence="t('adminConfigReload.forceConsequence')"
      :confirm-name="t('adminConfigReload.reloadConfirmWord')"
      confirm-case-insensitive
      :loading="loadingForce"
      @confirm="
        () => {
          confirmForce = false;
          forceReload();
        }
      "
      @update:open="confirmForce = $event"
    />

    <!-- Change History -->
    <Card>
      <CardHeader>
        <CardTitle>{{ t("adminConfigReload.changeHistory") }}</CardTitle>
      </CardHeader>
      <CardContent>
        <div v-if="loadingHistory" class="text-sm text-muted-foreground">
          {{ t("adminConfigReload.loading") }}
        </div>
        <div v-else-if="changeHistory.length === 0" class="text-sm text-muted-foreground">
          {{ t("adminConfigReload.noChangesRecordedYet") }}
        </div>
        <div v-else class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead>
              <tr class="text-left border-b border-rule-strong">
                <th class="pb-2 pr-4">{{ t("common.date") }}</th>
                <th class="pb-2 pr-4">{{ t("adminConfigReload.byColumn") }}</th>
                <th class="pb-2 pr-4">{{ t("common.status") }}</th>
                <th class="pb-2">{{ t("common.summary") }}</th>
              </tr>
            </thead>
            <tbody>
              <template v-for="row in changeHistory" :key="row.id">
                <!-- The disclosure is a button in the first cell, not a `@click`
                     on the `<tr>`. A clickable row with `cursor-pointer`, no
                     `tabindex`, no `role` and no key handler is unreachable by
                     keyboard and announces nothing, which is the same finding
                     `PackageDetailPage.vue` records against its version list.
                     A button brings the focus ring, Enter and Space with it, and
                     `aria-expanded` is what tells a reader whether the diff below
                     is open — the chevron alone says it only to someone who can
                     see it. -->
                <tr class="border-b border-dashed border-rule-soft">
                  <td class="py-2 pr-4">
                    <button
                      type="button"
                      class="inline-flex items-center gap-1.5 text-left hover:underline underline-offset-[3px]"
                      :aria-expanded="expandedRow === row.id"
                      :aria-controls="
                        expandedRow === row.id ? `config-change-${row.id}` : undefined
                      "
                      @click="expandedRow = expandedRow === row.id ? null : row.id"
                    >
                      <ChevronRight
                        class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
                        :class="expandedRow === row.id ? 'rotate-90' : ''"
                        aria-hidden="true"
                      />
                      {{ formatDate(row.triggered_at) }}
                      <span class="sr-only">{{
                        expandedRow === row.id
                          ? t("adminConfigReload.hideDiff")
                          : t("adminConfigReload.showDiff")
                      }}</span>
                    </button>
                  </td>
                  <td class="py-2 pr-4">{{ row.triggered_by }}</td>
                  <td class="py-2 pr-4">
                    <Badge
                      :class="row.status === 'applied' ? 'text-foreground' : 'text-destructive'"
                    >
                      {{ row.status }}
                    </Badge>
                  </td>
                  <td class="py-2">{{ row.summary }}</td>
                </tr>
                <tr v-if="expandedRow === row.id" :id="`config-change-${row.id}`">
                  <td colspan="4" class="pb-3">
                    <pre class="bg-muted text-xs p-2 overflow-x-auto">{{
                      JSON.stringify(row.diff, null, 2)
                    }}</pre>
                  </td>
                </tr>
              </template>
            </tbody>
          </table>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
