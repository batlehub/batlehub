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
import { API_BASE_URL } from "@/config";
import ConfigWarningList from "@/components/admin/ConfigWarningList.vue";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import PendingReloadCard from "@/components/admin/PendingReloadCard.vue";
import ConfigEditorCard from "@/components/admin/ConfigEditorCard.vue";
import ConfigChangeHistory from "@/components/admin/ConfigChangeHistory.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Alert } from "@/components/ui/alert";
import { AsyncState } from "@/components/ui/async-state";
import { Announcer } from "@/components/ui/announcer";

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
const loadingApply = ref(false);
const loadingDiscard = ref(false);
const loadingHistory = ref(false);
const errorMsg = ref<string | null>(null);
const successMsg = ref<string | null>(null);

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
        <ConfigWarningList :warnings="visibleWarnings" dismissible @dismiss="dismissWarning" />
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
    <PendingReloadCard
      v-if="hotReloadEnabled !== false"
      :pending-reload="pendingReload"
      :loading="loadingPending"
      :error="pendingError"
      :loading-apply="loadingApply"
      :loading-discard="loadingDiscard"
      @retry="fetchPending"
      @apply="confirmApply = true"
      @discard="discardPending"
    />

    <!-- Feedback -->
    <Alert v-if="successMsg" variant="success">{{ successMsg }}</Alert>
    <Alert v-if="errorMsg" variant="destructive">{{ errorMsg }}</Alert>

    <ConfigEditorCard
      v-model:content="configContent"
      v-model:candidate-warnings="candidateWarnings"
      :config-loading="configLoading"
      :config-load-error="configLoadError"
      :config-readonly="configReadonly"
      :hot-reload-enabled="hotReloadEnabled"
      :has-pending="!!pendingReload"
      :refresh-pending="fetchPending"
      @reload-from-disk="loadConfigContent"
    />

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
    <ConfigChangeHistory :change-history="changeHistory" :loading="loadingHistory" />
  </div>
</template>
