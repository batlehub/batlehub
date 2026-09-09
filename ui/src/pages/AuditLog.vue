<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, watch, onBeforeUnmount } from "vue";
import { auditLog } from "@/client/sdk.gen";
import type { AuditLogResponse } from "@/client/types.gen";
import { useApi, extractMessage } from "@/composables/useApi";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { API_BASE_URL } from "@/config";
import { useAuth } from "@/composables/useAuth";
import { formatDate } from "@/lib/format";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import ExposurePanel from "@/components/admin/ExposurePanel.vue";
import { PageHeader } from "@/components/ui/page-header";
import { OBSERVABILITY_TABS } from "@/config/adminSections";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Pagination } from "@/components/ui/pagination";
import { Announcer } from "@/components/ui/announcer";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Card, CardHeader, CardContent } from "@/components/ui/card";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

const { token } = useAuth();
const { authFetch } = useAuthFetch();

/**
 * The filters go to the server (RFC 0004-bis §6.1).
 *
 * `GET /api/v1/admin/audit-log` has accepted
 * `registry|user_id|from|to|denied_only|page|per_page` since it was written,
 * and this page sent none of them: it fetched the newest hundred rows and
 * filtered them in the browser.
 *
 * That is a correctness bug rather than a limitation, and `denied_only` is
 * where it bites. It is the single most-used audit filter and had no control
 * at all — but even the two filters that existed answered "no events match"
 * about *the newest hundred rows* while presenting it as an answer about the
 * log. On the surface whose entire purpose is establishing what someone did, a
 * blank that reads as a fact is the worst failure mode there is; it is the same
 * defect as the access-check simulator's confident `allow`.
 */
const registryFilter = ref("");
const userFilter = ref("");
const deniedOnly = ref(false);
const from = ref("");
const to = ref("");
const page = ref(0);
const PER_PAGE = 100;

/** `datetime-local` gives `2026-08-13T09:30`; the API wants RFC 3339. */
const asRfc3339 = (value: string): string | undefined =>
  value ? new Date(value).toISOString() : undefined;

/**
 * The two free-text filters, settled.
 *
 * `query` is a dependency of `useApi`, so every keystroke in either box was a
 * request — typing `svc-ci-runner` fired thirteen of them, twelve of which
 * described a query nobody had finished asking. The boxes stay live; what is
 * debounced is the value the request is built from.
 *
 * 300 ms, the number the catalogue and the version list already use, so the
 * three search boxes in the console feel like one thing.
 */
const registryQuery = ref("");
const userQuery = ref("");
let filterTimer: ReturnType<typeof setTimeout> | null = null;

/** Copy the boxes into the query now, cancelling any pending copy. */
function commitFilters() {
  if (filterTimer) clearTimeout(filterTimer);
  filterTimer = null;
  registryQuery.value = registryFilter.value;
  userQuery.value = userFilter.value;
}

watch([registryFilter, userFilter], () => {
  if (filterTimer) clearTimeout(filterTimer);
  filterTimer = setTimeout(() => {
    filterTimer = null;
    registryQuery.value = registryFilter.value;
    userQuery.value = userFilter.value;
  }, 300);
});

// Leaving the page within 300 ms of a keystroke would otherwise run the copy —
// and the fetch it triggers — against a component that is gone.
onBeforeUnmount(() => {
  if (filterTimer) clearTimeout(filterTimer);
});

const query = computed(() => ({
  page: page.value,
  per_page: PER_PAGE,
  ...(registryQuery.value.trim() ? { registry: registryQuery.value.trim() } : {}),
  ...(userQuery.value.trim() ? { user_id: userQuery.value.trim() } : {}),
  ...(deniedOnly.value ? { denied_only: true } : {}),
  ...(asRfc3339(from.value) ? { from: asRfc3339(from.value) } : {}),
  ...(asRfc3339(to.value) ? { to: asRfc3339(to.value) } : {}),
}));

/**
 * `GET /api/v1/admin/audit-log` answers with the paginated envelope
 * `{ items, total, page, per_page }` — not a bare array.
 *
 * This page declared `useApi<AccessEvent[]>` against a hand-written local
 * interface, so `data.value` held the envelope, `data.value.length` was
 * `undefined`, and the page rendered "No events recorded yet." over a full
 * page of events. On an *audit* surface that is the worst failure mode there
 * is: it does not look broken, it looks like nothing happened.
 */
const { data, error, loading, reload } = useApi<AuditLogResponse>(
  () => auditLog({ query: query.value }) as Promise<{ data?: unknown; error?: unknown }>,
  [token, query],
);

/** The events themselves, out of the envelope. */
const events = computed(() => data.value?.items ?? []);
const total = computed(() => data.value?.total ?? 0);
const totalPages = computed(() => Math.max(1, Math.ceil(total.value / PER_PAGE)));

// Any filter change re-queries from the first page. Staying on page 4 of a
// result set that just shrank to one page is how a filter looks like it
// returned nothing.
//
// Watched on the *settled* text rather than on the boxes, so the page reset and
// the new filter reach `query` in the same tick — watching the boxes would fire
// one request for the page reset and a second, 300 ms later, for the filter.
watch([registryQuery, userQuery, deniedOnly, from, to], () => {
  page.value = 0;
});

const hasFilters = computed(
  () =>
    !!registryFilter.value.trim() ||
    !!userFilter.value.trim() ||
    deniedOnly.value ||
    !!from.value ||
    !!to.value,
);

function clearFilters() {
  registryFilter.value = "";
  userFilter.value = "";
  deniedOnly.value = false;
  from.value = "";
  to.value = "";
  // A button press is not typing: it should not wait 300 ms to take effect.
  commitFilters();
}

// ── Export ────────────────────────────────────────────────────────────────────

const exportFormat = ref<"json" | "csv">("csv");
const exporting = ref(false);
/** Said, not swallowed — see the `catch` in `exportAuditLog`. */
const exportError = ref<string | null>(null);

async function exportAuditLog() {
  exporting.value = true;
  exportError.value = null;
  try {
    const headers: Record<string, string> = {};
    if (token.value) headers["Authorization"] = `Bearer ${token.value}`;
    // `API_BASE_URL`, not a relative path: there is no `/api` proxy in front of
    // the SPA (CLAUDE.md), so a relative URL downloads the SPA's own index.html
    // on every deployment where the API is not same-origin.
    const params = new URLSearchParams({ format: exportFormat.value });
    // Export what is on screen. Handing someone a file that disagrees with the
    // table they were reading is worse than offering no export.
    if (userFilter.value.trim()) params.set("user_id", userFilter.value.trim());
    if (registryFilter.value.trim()) params.set("registry", registryFilter.value.trim());
    // Including "Denied only". It was the one filter the export dropped, so a
    // file downloaded from a table of denials silently contained every allowed
    // event as well.
    if (deniedOnly.value) params.set("denied_only", "true");
    const fromIso = asRfc3339(from.value);
    const toIso = asRfc3339(to.value);
    if (fromIso) params.set("from", fromIso);
    if (toIso) params.set("to", toIso);
    const url = `${API_BASE_URL}/api/v1/admin/audit-log/export?${params}`;
    const resp = await fetch(url, { headers });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const blob = await resp.blob();
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    const cd = resp.headers.get("Content-Disposition") ?? "";
    const match = cd.match(/filename="([^"]+)"/);
    a.download = match ? match[1] : `audit-log.${exportFormat.value}`;
    a.click();
    URL.revokeObjectURL(a.href);
  } catch (e: unknown) {
    // There was a `throw` and a `finally` but no `catch`, so a 500 on export
    // produced an unhandled rejection, no message, and a button that simply
    // re-enabled — the operator could not tell a failed export from one the
    // browser had quietly saved.
    exportError.value = extractMessage(e);
  } finally {
    exporting.value = false;
  }
}

// ── Retention purge (RFC 0004-bis A7) ─────────────────────────────────────────
//
// `DELETE /api/v1/admin/audit-log?before=` has existed since the retention work
// landed and had no UI at all, so the only way to apply a retention policy to
// an audit trail was curl. It is irreversible and it deletes evidence, which is
// why it goes through `DestructiveConfirm` with a typed confirmation rather
// than a plain dialog.

const purgeBefore = ref("");
const purgeOpen = ref(false);
const purging = ref(false);
const purgeError = ref<string | null>(null);
const announcement = ref("");

async function purge() {
  const before = asRfc3339(purgeBefore.value);
  if (!before) return;
  purging.value = true;
  purgeError.value = null;
  try {
    const res = await authFetch(
      `${API_BASE_URL}/api/v1/admin/audit-log?before=${encodeURIComponent(before)}`,
      { method: "DELETE" },
    );
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    const body = (await res.json()) as { deleted: number };
    // Announced, not only rendered: a destructive count that lands silently for
    // a screen-reader user is §2.6's finding, on the surface where the operator
    // is the only person able to perform the action.
    announcement.value = t("auditLog.purged", { count: body.deleted }, body.deleted);
    purgeOpen.value = false;
    purgeBefore.value = "";
    page.value = 0;
    reload();
  } catch (e) {
    purgeError.value = extractMessage(e);
  } finally {
    purging.value = false;
  }
}
</script>

<template>
  <div class="space-y-4">
    <SectionTabs :tabs="OBSERVABILITY_TABS" />
    <Announcer :message="announcement" />
    <PageHeader variant="display">
      <template #title>
        {{ t("adminNav.auditLog") }}
        <!-- `total`, not the loaded page: the endpoint returns 100 rows by
             default and the count must not silently mean "the first 100". -->
        <span v-if="total" class="font-mono text-base font-normal text-muted-foreground"
          >({{ total }})</span
        >
      </template>
    </PageHeader>
    <Card>
      <CardHeader class="space-y-3 pb-3">
        <div class="flex flex-row items-center justify-end space-y-0">
          <div class="flex gap-2 items-center">
            <select
              v-model="exportFormat"
              :aria-label="t('auditLog.exportFormat')"
              class="h-8 rounded-sm border border-input bg-transparent px-2 text-sm text-foreground"
            >
              <option value="csv">CSV</option>
              <option value="json">JSON</option>
            </select>
            <Button variant="outline" size="sm" :disabled="exporting" @click="exportAuditLog">
              {{ exporting ? t("adminSbom.exporting") : t("auditLog.export") }}
            </Button>
            <Button variant="outline" size="sm" @click="reload">{{ t("common.refresh") }}</Button>
          </div>
        </div>

        <!-- A failed export has to say so. Without this the button re-enabled
             and nothing else changed, which reads as "saved". -->
        <p v-if="exportError" class="text-sm text-destructive">{{ exportError }}</p>

        <!-- Every one of these is a server-side parameter the endpoint has
             always accepted. -->
        <div class="flex gap-2 flex-wrap items-end">
          <div class="space-y-1">
            <Label for="al-user" class="text-xs">{{ t("common.user") }}</Label>
            <Input
              id="al-user"
              v-model="userFilter"
              :placeholder="t('auditLog.filterByUser')"
              class="h-8 text-sm max-w-[200px]"
            />
          </div>
          <div class="space-y-1">
            <Label for="al-registry" class="text-xs">{{ t("common.registry") }}</Label>
            <Input
              id="al-registry"
              v-model="registryFilter"
              :placeholder="t('auditLog.filterByRegistry')"
              class="h-8 text-sm max-w-[160px]"
            />
          </div>
          <div class="space-y-1">
            <Label for="al-from" class="text-xs">{{ t("common.from") }}</Label>
            <Input id="al-from" v-model="from" type="datetime-local" class="h-8 text-sm" />
          </div>
          <div class="space-y-1">
            <Label for="al-to" class="text-xs">{{ t("common.to") }}</Label>
            <Input id="al-to" v-model="to" type="datetime-local" class="h-8 text-sm" />
          </div>
          <!-- The most-used audit filter, and it had no control at all. -->
          <div class="flex items-center gap-2 h-8">
            <Switch id="al-denied" v-model="deniedOnly" />
            <Label for="al-denied" class="text-xs">{{ t("auditLog.deniedOnly") }}</Label>
          </div>
          <Button v-if="hasFilters" variant="ghost" size="sm" @click="clearFilters">
            {{ t("common.clearAction") }}
          </Button>
        </div>
      </CardHeader>
      <CardContent class="p-0">
        <p v-if="loading" class="p-6 text-sm text-muted-foreground">{{ t("auditLog.loading") }}</p>
        <p v-else-if="error" class="p-6 text-sm text-destructive">
          {{ error }}
        </p>

        <Table v-else-if="events.length">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.time") }}</TableHead>
              <TableHead>{{ t("common.user") }}</TableHead>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead>{{ t("common.package") }}</TableHead>
              <TableHead>{{ t("common.action") }}</TableHead>
              <TableHead>{{ t("common.result") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <!-- No row tint: 1.03:1 is not a channel. The outcome cell carries
                 it in words and in hue. -->
            <TableRow v-for="ev in events" :key="ev.id">
              <TableCell class="whitespace-nowrap text-xs tabular-nums">
                {{ formatDate(ev.timestamp) }}
              </TableCell>
              <TableCell class="text-sm font-mono">
                <span v-if="ev.user_id">{{ ev.user_id }}</span>
                <span v-else class="text-muted-foreground font-sans">{{
                  t("auditLog.anonymousSubject")
                }}</span>
              </TableCell>
              <!--
                `package_id` is null for account- and network-wide actions —
                blocking a user, blocking an IP, purging the trail itself.
                The hand-written interface this page used to carry declared it
                required, so the template dereferenced it unguarded; the
                generated type is honest about it, and those rows would have
                thrown on render once the envelope fix let them through.
              -->
              <TableCell class="font-mono text-xs">
                {{ ev.package_id?.registry ?? "—" }}
              </TableCell>
              <TableCell class="font-mono text-xs">
                <template v-if="ev.package_id">
                  {{ ev.package_id.name }}@{{ ev.package_id.version }}
                  <span v-if="ev.package_id.artifact" class="text-muted-foreground">
                    ({{ ev.package_id.artifact }})
                  </span>
                </template>
                <span v-else class="text-muted-foreground">{{ t("auditLog.accountWide") }}</span>
              </TableCell>
              <TableCell class="text-xs font-mono">
                {{ ev.action }}
              </TableCell>
              <TableCell class="max-w-[220px]">
                <Badge :variant="ev.result.outcome === 'denied' ? 'destructive' : 'secondary'">
                  {{
                    ev.result.outcome === "denied"
                      ? t("accessCheck.denied")
                      : t("accessCheck.allowed")
                  }}
                </Badge>
                <p
                  v-if="ev.result.outcome === 'denied'"
                  class="mt-0.5 text-xs text-muted-foreground truncate"
                  :title="ev.result.reason"
                >
                  {{ ev.result.reason }}
                </p>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>

        <!--
          Which question was answered. "No events match these filters" and "this
          instance has recorded nothing" are different facts, and an audit
          surface that conflates them tells an operator the wrong one.
        -->
        <div v-else-if="!loading" class="p-6 text-sm text-muted-foreground text-center">
          {{ hasFilters ? t("auditLog.noEventsMatchTheCurrent") : t("auditLog.noEventsRecorded") }}
        </div>
      </CardContent>
    </Card>

    <Pagination
      v-if="totalPages > 1"
      v-model:page="page"
      :total-pages="totalPages"
      :disabled="loading"
    />

    <!-- Retention purge (A7). The endpoint existed; the UI did not. -->
    <Card>
      <CardHeader class="pb-3">
        <p class="text-sm font-medium">{{ t("auditLog.retention") }}</p>
        <p class="text-xs text-muted-foreground">{{ t("auditLog.retentionHelp") }}</p>
      </CardHeader>
      <CardContent class="flex flex-wrap items-end gap-2">
        <div class="space-y-1">
          <Label for="al-purge-before" class="text-xs">{{ t("auditLog.purgeBefore") }}</Label>
          <Input
            id="al-purge-before"
            v-model="purgeBefore"
            type="datetime-local"
            class="h-8 text-sm"
          />
        </div>
        <Button variant="destructive" size="sm" :disabled="!purgeBefore" @click="purgeOpen = true">
          {{ t("auditLog.purge") }}
        </Button>
      </CardContent>
    </Card>

    <DestructiveConfirm
      :open="purgeOpen"
      :action="t('auditLog.purge')"
      :count="1"
      :item-noun="t('auditLog.retentionWindow')"
      :scope="t('auditLog.purgeScope', { before: purgeBefore })"
      :consequence="t('auditLog.purgeConsequence')"
      :confirm-name="t('auditLog.purgeConfirmWord')"
      confirm-case-insensitive
      :loading="purging"
      :error="purgeError"
      @update:open="
        (v) => {
          purgeOpen = v;
          if (!v) purgeError = null;
        }
      "
      @confirm="purge"
    />

    <!-- RFC 0002: the same trail, asked a different question — who pulled a
         version a source has since flagged. Under `audit:read`, as the log. -->
    <ExposurePanel />
  </div>
</template>
