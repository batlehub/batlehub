<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { RouterLink } from "vue-router";
import { Package } from "@lucide/vue";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { PageHeader } from "@/components/ui/page-header";
import { PACKAGES_TABS } from "@/config/adminSections";
import {
  listPackages,
  listRegistries,
  blockPackage,
  unblockPackage,
  bulkBlockPackages,
  bulkUnblockPackages,
} from "@/client/sdk.gen";
import type { RegistryInfo } from "@/client/types.gen";
import { useApi, extractMessage } from "@/composables/useApi";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { useAuth } from "@/composables/useAuth";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { formatDate as fmtDate } from "@/lib/format";
import { API_BASE_URL } from "@/config";
import { AsyncState } from "@/components/ui/async-state";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardHeader, CardContent } from "@/components/ui/card";
import { Announcer } from "@/components/ui/announcer";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

interface AdminPackageSummary {
  id: string;
  package_id: {
    registry: string;
    name: string;
    version: string;
    artifact?: string | null;
  };
  status:
    | { status: "available" }
    | {
        status: "blocked";
        reason: string;
        blocked_by: string;
        blocked_at: string;
      };
  last_accessed: string | null;
  last_accessed_by: string | null;
  access_count: number;
}

const { token } = useAuth();
const { authFetch } = useAuthFetch();

interface AdminPackageListResponse {
  items: AdminPackageSummary[];
  total: number;
  page: number;
  per_page: number;
}

/**
 * Server-side filters, not a 1000-row fetch filtered in the browser.
 *
 * `GET /api/v1/admin/packages` declares `registry`, `name`, `blocked_only`,
 * `page` and `per_page`; the page sent only `per_page: 1000`. So "what is
 * blocked right now" — half of this page's question — was answerable by the
 * API and unreachable in the UI, and the header count silently capped at a
 * thousand.
 */
const registryFilter = ref("");
const blockedOnly = ref(false);

const {
  data: packagesResponse,
  error,
  loading,
  reload,
} = useApi<AdminPackageListResponse>(
  () =>
    listPackages({
      query: {
        per_page: 1000,
        ...(registryFilter.value ? { registry: registryFilter.value } : {}),
        ...(blockedOnly.value ? { blocked_only: true } : {}),
      },
    }) as Promise<{
      data?: unknown;
      error?: unknown;
    }>,
  [token, registryFilter, blockedOnly],
);

const packages = computed(() => packagesResponse.value?.items ?? null);

const actionError = ref<string | null>(null);

const { data: registries } = useApi<RegistryInfo[]>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const search = ref("");

const filteredPackages = computed(() => {
  if (!packages.value) return [];
  const q = search.value.toLowerCase().trim();
  if (!q) return packages.value;
  return packages.value.filter(
    (p) =>
      p.package_id.name.toLowerCase().includes(q) ||
      p.package_id.registry.toLowerCase().includes(q) ||
      p.package_id.version.toLowerCase().includes(q),
  );
});

// ── Block existing package ────────────────────────────────────────────────────

async function block(pkg: AdminPackageSummary) {
  const reason = blockReason.value.trim();
  // Said, not swallowed. `runPending` clears `pending` — closing the dialog —
  // before dispatching here, so a bare `return` left the operator looking at a
  // dismissed dialog, an unblocked package and no message at all.
  if (!reason) {
    actionError.value = t("adminPackages.blockNeedsReason");
    return;
  }
  actionError.value = null;
  try {
    // `res.error`, not `catch` alone. The generated client is built without
    // `throwOnError`, so an HTTP failure *resolves* rather than throws: a 403
    // or 409 fell straight through to `reload()` and the operator saw the
    // dialog close on a package that is still Available, with no message
    // anywhere — indistinguishable from a block the server accepted and
    // ignored. Same guard `AdminBulk` already applies.
    const res = await blockPackage({
      body: {
        registry: pkg.package_id.registry,
        name: pkg.package_id.name,
        version: pkg.package_id.version,
        artifact: pkg.package_id.artifact,
        reason,
      },
    });
    if (res.error) {
      actionError.value = extractMessage(res.error);
      return;
    }
    reload();
  } catch (e: unknown) {
    actionError.value = extractMessage(e);
  }
}

async function unblock(pkg: AdminPackageSummary) {
  actionError.value = null;
  try {
    // See `block()` above: the client resolves on HTTP failure, so the error
    // has to be read off the response rather than waited for as an exception.
    const res = await unblockPackage({
      body: {
        registry: pkg.package_id.registry,
        name: pkg.package_id.name,
        version: pkg.package_id.version,
        artifact: pkg.package_id.artifact,
      },
    });
    if (res.error) {
      actionError.value = extractMessage(res.error);
      return;
    }
    reload();
  } catch (e: unknown) {
    actionError.value = extractMessage(e);
  }
}

async function deletePkg(pkg: AdminPackageSummary) {
  actionError.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/packages/delete`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        registry: pkg.package_id.registry,
        name: pkg.package_id.name,
        version: pkg.package_id.version,
        artifact: pkg.package_id.artifact ?? null,
      }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    reload();
  } catch (e: unknown) {
    actionError.value = extractMessage(e);
  }
}

// ── Multi-select + bulk actions ───────────────────────────────────────────────

const selected = ref<Set<string>>(new Set());
const bulkLoading = ref(false);
const bulkResultMsg = ref<string | null>(null);

/**
 * One sentence for all three bulk verbs.
 *
 * These were three template literals with a hand-built `, N failed` suffix,
 * which is a sentence assembled from fragments — the shape that cannot be
 * translated, because the clause order is a property of the language and not of
 * the code. The catalogue owns the whole sentence and pluralises the noun; the
 * verb is a key so French can inflect it.
 */
const BULK_KEYS = {
  blocked: "adminPackages.bulkBlocked",
  unblocked: "adminPackages.bulkUnblocked",
  deleted: "adminPackages.bulkDeleted",
} as const;

function bulkOutcome(verb: keyof typeof BULK_KEYS, ok: number, failed: number): string {
  const done = t(BULK_KEYS[verb], { count: ok }, ok);
  return failed ? t("adminPackages.bulkWithFailures", { done, failed }, failed) : done;
}

function pkgKey(pkg: AdminPackageSummary) {
  return `${pkg.package_id.registry}:${pkg.package_id.name}:${pkg.package_id.version}:${pkg.package_id.artifact ?? ""}`;
}

const allSelected = computed(
  () =>
    filteredPackages.value.length > 0 &&
    filteredPackages.value.every((p) => selected.value.has(pkgKey(p))),
);

function toggleAll() {
  if (allSelected.value) {
    filteredPackages.value.forEach((p) => selected.value.delete(pkgKey(p)));
  } else {
    filteredPackages.value.forEach((p) => selected.value.add(pkgKey(p)));
  }
  selected.value = new Set(selected.value);
}

function toggleOne(pkg: AdminPackageSummary) {
  const k = pkgKey(pkg);
  if (selected.value.has(k)) selected.value.delete(k);
  else selected.value.add(k);
  selected.value = new Set(selected.value);
}

const selectedPackages = computed(() =>
  (packages.value ?? []).filter((p) => selected.value.has(pkgKey(p))),
);

async function bulkBlock() {
  const reason = blockReason.value.trim();
  if (!reason) {
    actionError.value = t("adminPackages.blockNeedsReason");
    return;
  }
  bulkLoading.value = true;
  bulkResultMsg.value = null;
  try {
    const res = await bulkBlockPackages({
      body: {
        items: selectedPackages.value.map((p) => ({
          registry: p.package_id.registry,
          name: p.package_id.name,
          version: p.package_id.version,
          artifact: p.package_id.artifact ?? null,
          reason,
        })),
      },
    });
    if (res.error) {
      // Without this the request failed, the selection cleared, the table
      // reloaded, and the page said nothing at all — a failed bulk action
      // looked exactly like a successful one.
      actionError.value = extractMessage(res.error);
      return;
    }
    const r = res.data;
    if (r) {
      bulkResultMsg.value = bulkOutcome("blocked", r.succeeded_count, r.failed_count);
    }
  } finally {
    bulkLoading.value = false;
    selected.value = new Set();
    reload();
  }
}

async function bulkUnblock() {
  bulkLoading.value = true;
  bulkResultMsg.value = null;
  try {
    const res = await bulkUnblockPackages({
      body: {
        items: selectedPackages.value.map((p) => ({
          registry: p.package_id.registry,
          name: p.package_id.name,
          version: p.package_id.version,
          artifact: p.package_id.artifact ?? null,
        })),
      },
    });
    if (res.error) {
      // Without this the request failed, the selection cleared, the table
      // reloaded, and the page said nothing at all — a failed bulk action
      // looked exactly like a successful one.
      actionError.value = extractMessage(res.error);
      return;
    }
    const r = res.data;
    if (r) {
      bulkResultMsg.value = bulkOutcome("unblocked", r.succeeded_count, r.failed_count);
    }
  } finally {
    bulkLoading.value = false;
    selected.value = new Set();
    reload();
  }
}

async function bulkDelete() {
  bulkLoading.value = true;
  bulkResultMsg.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/packages/bulk-delete`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        items: selectedPackages.value.map((p) => ({
          registry: p.package_id.registry,
          name: p.package_id.name,
          version: p.package_id.version,
          artifact: p.package_id.artifact ?? null,
        })),
      }),
    });
    const json = (await res.json().catch(() => ({}))) as {
      succeeded_count?: number;
      failed_count?: number;
      message?: string;
    };
    if (!res.ok) {
      // The same defect the comment in `bulkBlock` describes, on the one sibling
      // that went through raw `authFetch` instead of the generated client: with
      // no status check a 500 with an empty body parsed to `{}` and announced
      // "Deleted 0 packages." — a failed bulk delete reported as a success.
      actionError.value = json.message ?? `HTTP ${res.status}`;
      return;
    }
    bulkResultMsg.value = bulkOutcome("deleted", json.succeeded_count ?? 0, json.failed_count ?? 0);
  } catch (e) {
    bulkResultMsg.value = extractMessage(e);
  } finally {
    bulkLoading.value = false;
    selected.value = new Set();
    reload();
  }
}

// ── Pre-block form ────────────────────────────────────────────────────────────

/**
 * The "Block a package" form is gone (RFC 0004 Phase 5, *distill*).
 *
 * It asked an operator to type a registry, name, version and artifact by hand
 * — while a list of packages sat on the same screen — and it was a one-row
 * copy of `/admin/packages/bulk`, one tab away, which does the same job with
 * validation, a preview table and a per-item failure report this page threw
 * away. Blocking something that is *not here yet* is that page's question; the
 * toolbar links to it rather than answering it twice.
 */

type PendingAction =
  | { kind: "delete-one"; pkg: AdminPackageSummary }
  | { kind: "block-one"; pkg: AdminPackageSummary }
  | { kind: "bulk-block" }
  | { kind: "bulk-delete" }
  | { kind: "bulk-unblock" };

/** The reason a block carries, collected by the dialog rather than a prompt. */
const blockReason = ref("");

const pending = ref<PendingAction | null>(null);

const confirmProps = computed(() => {
  const action = pending.value;
  if (!action) return null;
  if (action.kind === "delete-one") {
    const id = action.pkg.package_id;
    return {
      action: t("common.delete"),
      count: 1,
      itemNoun: t("adminPackages.itemPackageRecord"),
      scope: t("adminPackages.scopeDeleteOne", {
        name: id.name,
        version: id.version,
        registry: id.registry,
      }),
      reversible: false,
      // Stated here rather than inherited. This is the one action the stock
      // sentence was ever true of, and leaving it implicit is what let three
      // other irreversible verbs borrow a consequence that does not happen.
      consequence: t("adminPackages.deleteConsequence"),
      confirmName: id.name,
    };
  }
  if (action.kind === "block-one") {
    const id = action.pkg.package_id;
    return {
      action: t("common.block"),
      count: 1,
      itemNoun: t("adminPackages.itemPackage"),
      scope: t("adminPackages.scopeBlockOne", {
        name: id.name,
        version: id.version,
        registry: id.registry,
      }),
      reversible: true,
    };
  }
  if (action.kind === "bulk-block") {
    return {
      action: t("common.block"),
      count: selected.value.size,
      itemNoun: t("adminPackages.itemPackage"),
      scope: t("adminPackages.scopeBlockSelection"),
      reversible: true,
    };
  }
  if (action.kind === "bulk-delete") {
    return {
      action: t("common.delete"),
      count: selected.value.size,
      itemNoun: t("adminPackages.itemPackageRecord"),
      scope: t("adminPackages.scopeDeleteSelection"),
      reversible: false,
      consequence: t("adminPackages.deleteConsequence"),
      // A keyword, not an identifier: unlike `delete-one` above — where the
      // typed name is the package's own and `Foo` is a different package from
      // `foo` — there is nothing here for the case to disambiguate.
      confirmName: "delete",
      confirmCaseInsensitive: true,
    };
  }
  return {
    action: t("common.unblock"),
    count: selected.value.size,
    itemNoun: t("adminPackages.itemPackage"),
    scope: t("adminPackages.scopeSelection"),
    reversible: true,
  };
});

async function runPending(): Promise<void> {
  const action = pending.value;
  pending.value = null;
  if (!action) return;
  if (action.kind === "delete-one") await deletePkg(action.pkg);
  else if (action.kind === "block-one") await block(action.pkg);
  else if (action.kind === "bulk-block") await bulkBlock();
  else if (action.kind === "bulk-delete") await bulkDelete();
  else await bulkUnblock();
}
</script>

<template>
  <div class="space-y-4">
    <!-- An outcome announced, not only rendered (RFC 0004-bis §2.6). Every bulk
         result, block, warm and cache invalidation in this console reached
         sighted users only — on the surface whose audience includes the one
         person able to perform destructive actions. -->
    <Announcer :message="bulkResultMsg ?? ''" />
    <SectionTabs :tabs="PACKAGES_TABS" />
    <PageHeader variant="display">
      <template #title>
        {{ t("adminNav.allPackages") }}
        <span v-if="packages?.length" class="font-mono text-base font-normal text-muted-foreground"
          >({{ packages.length }})</span
        >
      </template>
    </PageHeader>
    <!-- The form that used to sit here is `/admin/packages/bulk`, one tab
         away and better at the job. A link, not a second implementation. -->
    <p class="text-sm text-muted-foreground">
      <i18n-t keypath="adminPackages.blockNotHereYet" tag="span">
        <template #link>
          <!-- Underlined at rest, not on hover. Crimson on this sentence's dim
               ink measures 1.28:1 against the surrounding text, so colour alone
               cannot mark it as a link: axe's `link-in-text-block` wants 3:1 or
               a non-colour cue. The same fix the catalog's denial notes already
               carry — and the reason it went unnoticed here is that the gate
               measures 1440 and 390, where this sentence does not wrap into a
               text block; it fails at 820–1023, which nothing measured. -->
          <RouterLink
            to="/admin/packages/bulk"
            class="text-primary underline underline-offset-[3px]"
            >{{ t("adminNav.bulkBlock") }}</RouterLink
          >
        </template>
      </i18n-t>
    </p>

    <!-- Bulk action bar.

         Block and Delete were the same crimson fill side by side — one undoes
         with a click, the other erases artifacts for good. Block is now the
         outlined control (its confirmation carries the weight), and Delete is
         the bar's one filled action, set apart at the far end so it is never
         the button beside the one you meant. -->
    <div
      v-if="selected.size > 0"
      class="sticky top-16 z-30 flex flex-wrap items-center gap-3 border border-rule-strong bg-background px-4 py-2.5"
    >
      <span class="text-sm font-medium">{{
        t("adminPackages.selectedCount", { count: selected.size }, selected.size)
      }}</span>
      <Button
        size="sm"
        variant="outline"
        :disabled="bulkLoading"
        @click="
          blockReason = '';
          pending = { kind: 'bulk-block' };
        "
        >{{ t("adminPackages.blockSelected") }}</Button
      >
      <Button
        size="sm"
        variant="outline"
        :disabled="bulkLoading"
        @click="pending = { kind: 'bulk-unblock' }"
        >{{ t("adminPackages.unblockSelected") }}</Button
      >
      <Button size="sm" variant="ghost" @click="selected = new Set()">
        {{ t("common.clearAction") }}
      </Button>
      <Button
        size="sm"
        variant="destructive"
        class="ml-auto"
        :disabled="bulkLoading"
        @click="pending = { kind: 'bulk-delete' }"
        >{{ t("adminPackages.deleteSelected") }}</Button
      >
    </div>

    <!-- Outside the selection-gated bar above: every bulk handler clears the
         selection in its `finally`, so a message rendered inside that bar was
         written into a node destroyed in the same tick and never seen. -->
    <output v-if="bulkResultMsg" class="block text-sm text-muted-foreground">
      {{ bulkResultMsg }}
    </output>

    <!-- Package list -->
    <Card>
      <CardHeader class="space-y-3 pb-3">
        <div class="flex flex-row items-center justify-end space-y-0">
          <Button variant="outline" size="sm" @click="reload"> {{ t("common.refresh") }} </Button>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          <Input
            v-model="search"
            :placeholder="t('adminPackages.filterByNameRegistry')"
            :aria-label="t('adminPackages.filterPackages')"
            class="max-w-sm h-8 text-sm"
          />
          <!-- Both go to the server. `blocked_only` answers half this page's
               question and had no control at all. -->
          <select
            v-model="registryFilter"
            :aria-label="t('common.registry')"
            class="h-8 rounded-sm border border-input bg-transparent px-2 text-sm"
          >
            <option value="">{{ t("adminPackages.allRegistries") }}</option>
            <option v-for="r in registries ?? []" :key="r.name" :value="r.name">
              {{ r.name }}
            </option>
          </select>
          <label class="flex items-center gap-1.5 text-sm text-muted-foreground">
            <input v-model="blockedOnly" type="checkbox" class="h-3.5 w-3.5" />
            {{ t("adminPackages.blockedOnly") }}
          </label>
        </div>
      </CardHeader>
      <CardContent class="p-0">
        <p v-if="actionError" class="px-6 pt-4 text-sm text-destructive">
          {{ actionError }}
        </p>
        <AsyncState :loading="loading" :error="error" :empty="!filteredPackages.length">
          <template #empty>
            <div class="py-12 text-center space-y-2">
              <Package class="h-8 w-8 mx-auto text-muted-foreground/50" />
              <p class="text-sm text-muted-foreground">
                {{
                  search
                    ? t("adminPackages.noPackagesMatchYourFilter")
                    : t("adminPackages.noPackagesYet")
                }}
              </p>
              <p v-if="search" class="text-xs text-muted-foreground">
                {{ t("adminPackages.tryClearingTheFilter") }}
              </p>
            </div>
          </template>

          <Table>
            <TableHeader>
              <TableRow>
                <TableHead class="w-8">
                  <input
                    type="checkbox"
                    :aria-label="t('adminPackages.selectAllPackages')"
                    :checked="allSelected"
                    class="cursor-pointer"
                    @change="toggleAll"
                  />
                </TableHead>
                <!--
                  Six columns, not ten (RFC 0004-bis §6.1).

                  Measured at 1440: the table wanted ~1650px intrinsic in a
                  1134px container, so the row verbs sat off-screen at the
                  console's own standard width. The two links that were an
                  unlabelled nav column are now *on* the cells they are about —
                  the name opens the package, the version opens the artifact
                  query — which is where a reader already points; `artifact` is
                  folded into the name cell, and `last_accessed_by` moved to the
                  detail page, where one row's history belongs.
                -->
                <TableHead>{{ t("common.registry") }}</TableHead>
                <TableHead>{{ t("common.name") }}</TableHead>
                <TableHead>{{ t("common.version") }}</TableHead>
                <TableHead>{{ t("common.status") }}</TableHead>
                <TableHead>{{ t("adminPackages.lastPulled") }}</TableHead>
                <TableHead class="text-right"> {{ t("common.downloads") }} </TableHead>
                <TableHead class="text-right"> {{ t("common.actions") }} </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <!-- No row tint. `bg-destructive/5` measured 1.03:1 in dark and
                   1.09:1 in light — DESIGN.md's Undependable Fill Rule in its
                   plainest form, a fill standing in for a state channel while
                   being, measurably, invisible. The state is carried by the
                   status cell below: the word ("Blocked"), the hue (the
                   `destructive` badge) and the reason under it. -->
              <!--
                Keyed by the package, not by its position.

                `:key="i"` made the *row element* the identity, so filtering
                re-labelled the existing rows in place instead of moving them.
                Nothing bound broke — every visible bit of a row is a bound
                value, and `:checked` is written on every patch — but keyboard
                focus is not a bound value. It sits on a DOM node, and the node
                stayed where it was: focus a checkbox, type in the filter, and
                the focus ring is now on a *different package*, silently. On a
                page whose row actions are block, unblock and delete.

                `pkgKey` was already here for the selection `Set`. It is the
                row's identity in every other respect; it is its identity here
                too.
              -->
              <TableRow v-for="pkg in filteredPackages" :key="pkgKey(pkg)">
                <TableCell class="w-8">
                  <input
                    type="checkbox"
                    :aria-label="t('adminPackages.selectPackage', { name: pkg.package_id.name })"
                    :checked="selected.has(pkgKey(pkg))"
                    class="cursor-pointer"
                    @change="toggleOne(pkg)"
                  />
                </TableCell>
                <TableCell class="font-mono text-xs">
                  {{ pkg.package_id.registry }}
                </TableCell>
                <TableCell class="font-medium">
                  <RouterLink
                    :to="`/packages/${encodeURIComponent(
                      pkg.package_id.registry,
                    )}/${encodeURIComponent(pkg.package_id.name)}`"
                    class="text-primary underline underline-offset-2"
                  >
                    {{ pkg.package_id.name }}
                  </RouterLink>
                  <span
                    v-if="pkg.package_id.artifact"
                    class="ml-1.5 font-mono text-xs text-muted-foreground"
                    >{{ pkg.package_id.artifact }}</span
                  >
                </TableCell>
                <TableCell class="font-mono text-xs">
                  <RouterLink
                    :to="{
                      path: `/packages/${encodeURIComponent(
                        pkg.package_id.registry,
                      )}/${encodeURIComponent(pkg.package_id.name)}`,
                      query: {
                        version: pkg.package_id.version,
                        ...(pkg.package_id.artifact ? { artifact: pkg.package_id.artifact } : {}),
                      },
                    }"
                    class="text-primary underline underline-offset-2"
                  >
                    {{ pkg.package_id.version }}
                  </RouterLink>
                </TableCell>
                <TableCell>
                  <div class="space-y-0.5">
                    <Badge :variant="pkg.status.status === 'blocked' ? 'destructive' : 'secondary'">
                      {{
                        pkg.status.status === "blocked"
                          ? t("common.blocked")
                          : t("packageVersionsTable.available")
                      }}
                    </Badge>
                    <p v-if="pkg.status.status === 'blocked'" class="text-xs text-muted-foreground">
                      {{ pkg.status.reason }}
                    </p>
                  </div>
                </TableCell>
                <TableCell class="text-xs tabular-nums whitespace-nowrap">
                  {{ fmtDate(pkg.last_accessed) }}
                </TableCell>
                <TableCell class="text-right tabular-nums">
                  {{ pkg.access_count }}
                </TableCell>
                <TableCell class="text-right">
                  <div class="flex gap-1 justify-end">
                    <Button
                      v-if="pkg.status.status === 'blocked'"
                      variant="outline"
                      size="sm"
                      @click="unblock(pkg)"
                    >
                      {{ t("common.unblock") }}
                    </Button>
                    <Button
                      v-else
                      variant="outline"
                      size="sm"
                      @click="
                        blockReason = '';
                        pending = { kind: 'block-one', pkg };
                      "
                    >
                      {{ t("common.block") }}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      class="text-destructive hover:text-destructive hover:bg-accent"
                      @click="pending = { kind: 'delete-one', pkg }"
                    >
                      {{ t("common.delete") }}
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </AsyncState>
      </CardContent>
    </Card>
  </div>

  <DestructiveConfirm
    v-if="confirmProps"
    :open="pending !== null"
    v-bind="confirmProps"
    :loading="bulkLoading"
    :error="actionError"
    @update:open="(open: boolean) => !open && (pending = null)"
    @confirm="runPending"
  >
    <template v-if="pending?.kind === 'block-one' || pending?.kind === 'bulk-block'">
      <div class="space-y-1">
        <Label for="block-reason">{{ t("common.reason") }}</Label>
        <Input
          id="block-reason"
          v-model="blockReason"
          placeholder="CVE-2025-XXXX or policy violation"
        />
      </div>
    </template>
  </DestructiveConfirm>
</template>
