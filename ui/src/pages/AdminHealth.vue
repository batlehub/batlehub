<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { onMounted, ref } from "vue";
import { registryHealth, clearRegistryCache, invalidateExploreCache } from "@/client/sdk.gen";
import type { RegistryHealthDto } from "@/client/types.gen";
import { useApi, extractMessage } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { API_BASE_URL } from "@/config";
import {
  formatBytes as fmtBytes,
  formatDate as fmtDate,
  formatRelative as fmtRelative,
  formatCount,
} from "@/lib/format";
import { REGISTRY_TYPE_VARIANTS, variantFromMap } from "@/lib/badge-variants";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { OBSERVABILITY_TABS } from "@/config/adminSections";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import ConfirmDialog from "@/components/ConfirmDialog.vue";
import DeleteCachedArtifact from "@/components/admin/DeleteCachedArtifact.vue";

const { t } = useI18n();

const { token } = useAuth();

const { data, error, loading, reload } = useApi<RegistryHealthDto[]>(
  () => registryHealth() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const expandedErrors = ref<Set<string>>(new Set());

const clearTarget = ref<string | null>(null);

/**
 * The explore-cache invalidation, relocated from `/admin/operations/explore-cache`
 * (RFC 0004 Phase 5, *remove*).
 *
 * It was a whole route whose one job was pressing a button. Nobody navigates
 * Admin → Operations → Explore Cache; they arrive at a registry noticing its
 * package list looks wrong — which is this page. The control belongs where the
 * symptom is seen, beside the sibling destructive control that was already
 * here.
 */
const explorePending = ref<string | null>(null);
const exploreBusy = ref(false);
const exploreError = ref<string | null>(null);
const exploreDone = ref<string | null>(null);

async function confirmInvalidateExplore() {
  if (!explorePending.value) return;
  exploreBusy.value = true;
  exploreError.value = null;
  try {
    const { error: apiErr } = await invalidateExploreCache({
      body: { registry: explorePending.value },
    });
    if (apiErr) {
      exploreError.value = extractMessage(apiErr);
      return;
    }
    exploreDone.value = explorePending.value;
    explorePending.value = null;
  } finally {
    exploreBusy.value = false;
  }
}
const clearing = ref(false);
const clearError = ref<string | null>(null);

async function confirmClearCache() {
  if (!clearTarget.value) return;
  clearing.value = true;
  clearError.value = null;
  try {
    const { error: apiErr } = await clearRegistryCache({ path: { registry: clearTarget.value } });
    if (apiErr) throw new Error((apiErr as { message?: string })?.message ?? t("common.apiError"));
    clearTarget.value = null;
    reload();
  } catch (e) {
    clearError.value = extractMessage(e);
  } finally {
    clearing.value = false;
  }
}

function toggleErrors(registry: string) {
  if (expandedErrors.value.has(registry)) {
    expandedErrors.value.delete(registry);
  } else {
    expandedErrors.value.add(registry);
  }
  expandedErrors.value = new Set(expandedErrors.value);
}

/**
 * RFC 0014 §4.6: the audit's card — the active policy and, per audited
 * registry, how many packages are missing at their upstream and how many
 * are confirmed gone. Absent (`503`) when the audit is not running here,
 * which is a fact about this process and not an error.
 */
interface UpstreamSummary {
  policy: "audit" | "block";
  counts: { registry: string; missing: number; disappeared: number }[];
}
const { authFetch } = useAuthFetch();
const upstream = ref<UpstreamSummary | null>(null);
const UPSTREAM_POLICY_KEYS: Record<string, string> = {
  audit: "adminHealth.upstreamPolicyAudit",
  block: "adminHealth.upstreamPolicyBlock",
};
async function loadUpstream() {
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/upstream/disappeared?per_page=1`);
    if (!res?.ok) {
      upstream.value = null;
      return;
    }
    upstream.value = (await res.json()) as UpstreamSummary;
  } catch {
    upstream.value = null;
  }
}
onMounted(loadUpstream);

const ROLE_LABEL_KEYS: Record<string, string> = {
  anonymous: "adminHealth.roleAnonymous",
  user: "adminHealth.roleUsers",
  admin: "adminHealth.roleAdmins",
};
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OBSERVABILITY_TABS" />
    <PageHeader
      :title="t('adminHealth.registryHealth')"
      :description="t('adminHealth.liveSnapshotOfEachRegistry')"
      variant="display"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="reload">
          {{ loading ? t("adminHealth.refreshing") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>

    <AsyncState
      :loading="loading && !data"
      :error="error"
      :empty="!!data && data.length === 0"
      :empty-message="t('adminHealth.noRegistriesConfigured')"
    >
      <!--
        The aggregate card is gone (RFC 0004-bis §6.1).

        It restated `adminStats().aggregate` — the same four numbers
        `AdminDashboard` states as a sentence, rendered as four tiles and
        *without* the trend that makes them mean something. Its own Refresh
        button did not refresh it (`useApi` was destructured without `reload`),
        so the page had a control that lied about a card that duplicated
        another page. Removing it also removes this page's second fetch.

        What this page owns is per-registry health, which is everything below.
      -->

      <!-- RFC 0014 §4.6: the upstream audit, when this process runs it. -->
      <Card v-if="upstream" data-testid="upstream-card">
        <CardHeader class="pb-2">
          <CardTitle class="text-base">{{ t("adminHealth.upstreamTitle") }}</CardTitle>
        </CardHeader>
        <CardContent class="flex flex-wrap items-center gap-3 text-sm">
          <Badge :variant="upstream.policy === 'block' ? 'destructive' : 'secondary'">
            {{ t(UPSTREAM_POLICY_KEYS[upstream.policy]) }}
          </Badge>
          <span
            v-for="c in upstream.counts"
            :key="c.registry"
            class="font-mono text-xs"
            data-testid="upstream-card-count"
          >
            {{ c.registry }}:
            {{
              t("adminHealth.upstreamCounts", { missing: c.missing, disappeared: c.disappeared })
            }}
          </span>
          <RouterLink to="/admin/operations/upstream" class="text-xs underline">
            {{ t("adminHealth.upstreamOpen") }}
          </RouterLink>
        </CardContent>
      </Card>

      <!-- Registry cards grid -->
      <div
        v-if="data && data.length > 0"
        class="grid gap-4 sm:grid-cols-1 lg:grid-cols-2 xl:grid-cols-2"
      >
        <!-- `min-w-0`: a grid item defaults to `min-width: auto` and will not
             shrink below its content, so the errors table inside it pushed the
             whole document sideways instead of scrolling in its own box
             (DESIGN.md, The Own-Container Overflow Rule). -->
        <Card v-for="reg in data" :key="reg.registry" class="flex min-w-0 flex-col">
          <CardHeader class="pb-2">
            <div class="flex flex-wrap items-center justify-between gap-2">
              <CardTitle class="min-w-0 text-base font-mono">
                {{ reg.registry }}
              </CardTitle>
              <div class="flex flex-wrap items-center gap-2">
                <Badge
                  :variant="variantFromMap(reg.registry_type, REGISTRY_TYPE_VARIANTS)"
                  class="text-xs uppercase"
                >
                  {{ reg.registry_type }}
                </Badge>
                <!--
                  The mode (RFC 0004-bis A2). "0 cached, last pull: never" reads
                  identically for a broken proxy and for a healthy `local`
                  registry that has nothing to pull by definition — this row is
                  what tells them apart, and the page had to guess.
                -->
                <Badge variant="outline" class="text-xs uppercase">{{ reg.mode }}</Badge>
                <Badge v-if="reg.beta_channel_enabled" variant="copper" class="text-xs">{{
                  t("adminHealth.betaChannelOn")
                }}</Badge>
                <Button
                  variant="outline"
                  size="sm"
                  class="text-xs h-6 px-2"
                  @click="clearTarget = reg.registry"
                  >{{ t("adminHealth.clearCache") }}</Button
                >
                <Button
                  variant="outline"
                  size="sm"
                  class="text-xs h-6 px-2"
                  @click="explorePending = reg.registry"
                  >{{ t("adminHealth.refreshExplore") }}</Button
                >
              </div>
            </div>
          </CardHeader>

          <CardContent class="flex-1 space-y-4">
            <!-- Stats row -->
            <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
              <!-- Packages -->
              <div class="rounded-sm border bg-muted/30 p-3 space-y-0.5">
                <p class="text-xs text-muted-foreground">{{ t("common.packages") }}</p>
                <p class="text-xl font-semibold tabular-nums">
                  {{ formatCount(reg.package_count) }}
                </p>
                <p class="text-xs text-muted-foreground">tracked</p>
              </div>

              <!-- Cache size -->
              <div class="rounded-sm border bg-muted/30 p-3 space-y-0.5">
                <p class="text-xs text-muted-foreground">{{ t("adminHealth.cacheSize") }}</p>
                <p class="text-xl font-semibold">
                  {{ fmtBytes(reg.total_size_bytes ?? null) }}
                </p>
                <p class="text-xs text-muted-foreground">
                  {{ reg.cached_artifact_count }} artifacts
                </p>
              </div>

              <!-- Last pull -->
              <div class="rounded-sm border bg-muted/30 p-3 space-y-0.5">
                <p class="text-xs text-muted-foreground">{{ t("adminHealth.lastPull") }}</p>
                <p class="text-base font-semibold">
                  {{ fmtRelative(reg.last_pull_at ?? null) }}
                </p>
                <p v-if="reg.last_pull_at" class="text-xs text-muted-foreground">
                  {{ fmtDate(reg.last_pull_at ?? "") }}
                </p>
              </div>

              <!-- Pulls / hour -->
              <div class="rounded-sm border bg-muted/30 p-3 space-y-0.5">
                <p class="text-xs text-muted-foreground">{{ t("adminHealth.pullsHour") }}</p>
                <p
                  class="text-xl font-semibold tabular-nums"
                  :class="reg.pulls_last_hour > 0 ? 'text-primary' : 'text-muted-foreground'"
                >
                  {{ formatCount(reg.pulls_last_hour) }}
                </p>
              </div>

              <!-- Pulls / day -->
              <div class="rounded-sm border bg-muted/30 p-3 space-y-0.5">
                <p class="text-xs text-muted-foreground">{{ t("adminHealth.pullsDay") }}</p>
                <p class="text-xl font-semibold tabular-nums">
                  {{ formatCount(reg.pulls_last_day) }}
                </p>
              </div>
            </div>

            <!-- Recent errors -->
            <div>
              <button
                class="flex items-center gap-2 w-full text-left font-mono text-sm font-medium py-1 hover:text-accent-foreground transition-colors"
                @click="toggleErrors(reg.registry)"
              >
                <!-- Healthy mirrors the degraded branch below, in ink rather
                     than in green: this palette has no green, and the pair was
                     failing AA at 15 nodes. Quiet on purpose — the degraded
                     state has to stay the loud one (§6.4). The ping went with
                     it; the only authored motion in this world is the resolve
                     transition. -->
                <span
                  v-if="reg.recent_errors.length === 0"
                  class="flex items-center gap-1.5 text-muted-foreground"
                >
                  <span class="inline-block h-2 w-2 rounded-sm bg-muted-foreground" />
                  {{ t("adminHealth.noErrors24h") }}
                </span>
                <span v-else class="flex items-center gap-1.5 text-destructive">
                  <span class="inline-block h-2 w-2 rounded-sm bg-destructive" />
                  {{ t("adminHealth.errorsIn24h", reg.recent_errors.length) }}
                  <span class="text-muted-foreground text-xs ml-auto">
                    {{
                      expandedErrors.has(reg.registry)
                        ? t("packageBetaChannel.hide")
                        : t("packageBetaChannel.show")
                    }}
                  </span>
                </span>
              </button>

              <div
                v-if="expandedErrors.has(reg.registry) && reg.recent_errors.length > 0"
                class="mt-2 rounded-sm border overflow-x-auto"
              >
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead class="text-xs"> {{ t("common.when") }} </TableHead>
                      <TableHead class="text-xs"> {{ t("common.user") }} </TableHead>
                      <TableHead class="text-xs"> {{ t("common.package") }} </TableHead>
                      <TableHead class="text-xs"> {{ t("common.type") }} </TableHead>
                      <TableHead class="text-xs"> {{ t("common.reason") }} </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    <TableRow
                      v-for="err in reg.recent_errors"
                      :key="err.timestamp + err.package_name"
                    >
                      <TableCell class="text-xs whitespace-nowrap">
                        {{ fmtRelative(err.timestamp) }}
                      </TableCell>
                      <TableCell class="text-xs">
                        <span v-if="err.user_id">{{ err.user_id }}</span>
                        <span v-else class="text-muted-foreground italic">anonymous</span>
                      </TableCell>
                      <TableCell class="font-mono text-xs">
                        {{ err.package_name
                        }}<span class="text-muted-foreground">@{{ err.version }}</span>
                      </TableCell>
                      <TableCell>
                        <Badge
                          :variant="err.error_type === 'error' ? 'destructive' : 'secondary'"
                          class="text-xs"
                        >
                          {{
                            err.error_type === "error"
                              ? t("adminHealth.upstreamError")
                              : t("accessCheck.denied")
                          }}
                        </Badge>
                      </TableCell>
                      <TableCell
                        class="text-xs text-muted-foreground max-w-[200px] truncate"
                        :title="err.reason"
                      >
                        {{ err.reason }}
                      </TableCell>
                    </TableRow>
                  </TableBody>
                </Table>
              </div>
            </div>

            <!-- Who has access -->
            <div class="space-y-1.5">
              <p class="text-xs font-medium text-muted-foreground uppercase tracking-wide">
                {{ t("adminHealth.whoHasAccess") }}
              </p>
              <div class="flex flex-wrap gap-1.5">
                <Badge
                  v-for="role in reg.access.roles"
                  :key="role"
                  variant="secondary"
                  class="text-xs"
                >
                  {{ ROLE_LABEL_KEYS[role] ? t(ROLE_LABEL_KEYS[role]) : role }}
                </Badge>
                <Badge
                  v-for="group in reg.access.groups"
                  :key="group"
                  variant="outline"
                  class="text-xs font-mono"
                >
                  {{ group }}
                </Badge>
                <Badge
                  v-if="reg.access.roles.length === 0 && reg.access.groups.length === 0"
                  variant="destructive"
                  class="text-xs"
                  >{{ t("adminHealth.noAccessConfigured") }}</Badge
                >
                <span
                  v-else-if="
                    !reg.access.roles.includes('anonymous') && !reg.access.roles.includes('user')
                  "
                  class="text-xs text-copper flex items-center gap-1"
                  >{{ t("adminHealth.restrictedNoPublicAccess") }}</span
                >
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
      <!-- Split out of `/admin/operations/warming`: the two cache-eviction
           controls now sit on one route instead of two sections apart. -->
      <DeleteCachedArtifact :registries="(data ?? []).map((r) => r.registry)" />
    </AsyncState>
  </div>

  <!-- Clear cache confirmation dialog -->
  <ConfirmDialog
    :open="clearTarget !== null"
    :confirm-label="t('adminHealth.clearCache')"
    :loading-label="t('adminHealth.clearing')"
    destructive
    :loading="clearing"
    :error="clearError"
    @update:open="
      (v) => {
        if (!v) {
          clearTarget = null;
          clearError = null;
        }
      }
    "
    @confirm="confirmClearCache"
  >
    <template #title>
      <i18n-t keypath="adminHealth.clearCacheFor" tag="span">
        <template #registry
          ><span class="font-mono">{{ clearTarget }}</span></template
        >
      </i18n-t>
    </template>
    <template #description>{{ t("adminHealth.allCachedArtifactsFor") }}</template>
  </ConfirmDialog>

  <!-- Explore-cache invalidation, relocated here from its own route. Confirmed
       like its neighbour: it is cheap and self-healing, but it is still a
       control that acts on what other people are reading. -->
  <ConfirmDialog
    :open="explorePending !== null"
    :confirm-label="t('adminHealth.refreshExplore')"
    :loading-label="t('adminHealth.refreshingExplore')"
    :loading="exploreBusy"
    :error="exploreError"
    @update:open="
      (v) => {
        if (!v) {
          explorePending = null;
          exploreError = null;
        }
      }
    "
    @confirm="confirmInvalidateExplore"
  >
    <template #title>
      <i18n-t keypath="adminHealth.refreshExploreFor" tag="span">
        <template #registry
          ><span class="font-mono">{{ explorePending }}</span></template
        >
      </i18n-t>
    </template>
    <template #description>{{ t("adminHealth.exploreRepopulates") }}</template>
  </ConfirmDialog>
</template>
