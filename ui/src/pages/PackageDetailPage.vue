<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, watch } from "vue";
import { useRoute } from "vue-router";
import { listRegistries, packageDetail } from "@/client/sdk.gen";
import type { PackageDetailResponse, RegistryInfo } from "@/client/types.gen";
import { useAuth } from "@/composables/useAuth";
import PackageVersionsTable from "@/components/admin/PackageVersionsTable.vue";
import PackageBetaChannel from "@/components/admin/PackageBetaChannel.vue";
import PackageVisibility from "@/components/admin/PackageVisibility.vue";
import PackageGrants from "@/components/admin/PackageGrants.vue";
import PackageEventsTable from "@/components/admin/PackageEventsTable.vue";
import ReadmePanel from "@/components/package/ReadmePanel.vue";
import VerdictPanel from "@/components/package/VerdictPanel.vue";
import MovingRefsPanel from "@/components/package/MovingRefsPanel.vue";
import VersionsTable from "@/components/package/VersionsTable.vue";
import PackageBreadcrumb from "@/components/package/PackageBreadcrumb.vue";
import PackageMasthead from "@/components/package/PackageMasthead.vue";
import VersionSubject, {
  type UpstreamPackageStatus,
} from "@/components/package/VersionSubject.vue";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { usePackageVersions } from "@/composables/usePackageVersions";
import { useApi, extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";

const { t } = useI18n();

const { token, can } = useAuth();
// RFC 0042 §4.3: each surface on the verb its endpoint asks for, not the role.
const canAdminister = computed(() => can("packages:read"));
const canReadUpstream = computed(() => can("system:read", registry.value));
const canRescan = computed(() => can("gates:exempt", registry.value));
const { authFetch } = useAuthFetch();
const route = useRoute();

const registry = computed(() => String(route.params.registry ?? ""));
const name = computed(() => String(route.params.name ?? ""));

const { data: registriesList } = useApi<RegistryInfo[]>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const registryType = computed(
  () => registriesList.value?.find((r) => r.name === registry.value)?.type ?? null,
);

const {
  data,
  loading,
  error,
  selectedVersion,
  selectedRow,
  showPrereleases,
  versionFilter,
  versionPage,
  pagedVersions,
  filteredTotal,
  unfilteredTotal,
  prereleaseCount,
  hiddenPrereleaseCount,
  versionTotalPages,
  upstreamVersionCount,
  filterPending,
  filterVersions,
  togglePrereleases,
  turnToPage,
  fetching,
  fetchResult,
  onFetchVersion,
  fetchDetail,
} = usePackageVersions(registry, name);

// ── Per-artifact SBOM download ─────────────────────────────────────────────

const sbomLoading = ref<string | null>(null); // "registry/name/version:format"
/**
 * Formats that answered `404` since the page loaded, keyed
 * `registry/name/version:format`.
 *
 * A second line of defence, not the answer: the server now says up front which
 * formats it holds (`version.sbom`), so a button is only ever drawn for one that
 * exists. This still exists because an SBOM can be re-recorded — or a version
 * yanked — between the response and the click, and a format that has just gone
 * should stop being offered rather than keep failing.
 *
 * Keyed per **format**: it used to be keyed per artifact, so one missing SPDX
 * removed the CycloneDX button too.
 */
const sbomMissing = ref<Set<string>>(new Set());
/** The SBOM error a reader can act on, keyed by artifact. */
const sbomError = ref<Record<string, string>>({});

const artifactKey = (version: string) => `${registry.value}/${name.value}/${version}`;
const formatKey = (version: string, fmt: string) => `${artifactKey(version)}:${fmt}`;
const withoutKey = (set: Set<string>, key: string) => {
  if (!set.has(key)) return set;
  const next = new Set(set);
  next.delete(key);
  return next;
};

async function downloadSbom(version: string, fmt: "spdx" | "cyclonedx") {
  const key = `${registry.value}/${name.value}/${version}:${fmt}`;
  sbomLoading.value = key;
  delete sbomError.value[artifactKey(version)];
  try {
    const ext = fmt === "cyclonedx" ? "cyclonedx.json" : "spdx.json";
    const url = `/api/v1/sbom/${encodeURIComponent(registry.value)}/${encodeURIComponent(name.value)}/${encodeURIComponent(version)}?format=${fmt}`;
    const resp = await authFetch(`${API_BASE_URL}${url}`);
    if (resp.status === 404) {
      sbomMissing.value = new Set([...sbomMissing.value, formatKey(version, fmt)]);
      return;
    }
    // A format that *is* there clears its own mark, which was set once and never
    // lifted: one 404 on SPDX hid the CycloneDX button too, for the rest of the
    // session, including after a fetch had populated the row.
    if (resp.ok) sbomMissing.value = withoutKey(sbomMissing.value, formatKey(version, fmt));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const disposition = resp.headers.get("Content-Disposition") ?? "";
    const match = disposition.match(/filename="([^"]+)"/);
    const filename = match?.[1] ?? `${name.value}-${version}.${ext}`;
    const blob = await resp.blob();
    const a = Object.assign(document.createElement("a"), {
      href: URL.createObjectURL(blob),
      download: filename,
    });
    a.click();
    URL.revokeObjectURL(a.href);
  } catch (e) {
    // Said, not swallowed. This was a bare `catch {}`: every failure that was
    // not a 404 — a 502, an expired session, a network drop — produced a click
    // that did nothing, forever, with no way for the reader to tell a missing
    // SBOM from a broken one.
    sbomError.value[artifactKey(version)] = extractMessage(e);
  } finally {
    sbomLoading.value = null;
  }
}

/**
 * The SBOM formats the selected version can actually be downloaded in.
 *
 * The server answers this per version (`sbom.formats`), because it is the only
 * side that knows: which formats exist is decided per registry by
 * `[registries.sbom].formats`, and whether *this* version has any depends on
 * whether this instance has ever held its bytes. The page used to draw both
 * buttons unconditionally and find out on the click — so an SPDX-only registry
 * offered a CycloneDX download that could only 404, and every upstream-only row
 * offered two.
 */
const sbomFormats = computed<string[]>(() => {
  const row = selectedRow.value;
  if (!row || row.sbom?.state !== "available") return [];
  return (row.sbom.formats ?? []).filter(
    (fmt) => !sbomMissing.value.has(formatKey(row.version, fmt)),
  );
});

/**
 * What to say about the selected version's SBOM: offer it, say there is none, or
 * say it does not exist *yet*.
 *
 * `unknown` is not a hedge — it is the honest answer for a version this instance
 * holds no bytes of, where the SBOM is generated on the way in. Rendering that
 * as "no SBOM" would state as a fact about the package something that is only a
 * fact about our cache.
 */
const sbomState = computed<"available" | "none" | "unknown">(() => {
  const row = selectedRow.value;
  if (!row) return "unknown";
  if (sbomFormats.value.length > 0) return "available";
  // The server offered formats and every one of them has since 404'd. That is a
  // definite "none" now, whatever the response said when it was built.
  if (row.sbom?.state === "available") return "none";
  return row.sbom?.state === "none" ? "none" : "unknown";
});

/**
 * Administration is a *section of this page* now, not a parallel page at another
 * URL with its own layout and its own back button. One package, one address
 * (RFC 0003 §4.2).
 *
 * Fetched only when the viewer is an admin: the endpoint is admin-only
 * server-side, so firing it for everyone would mean a 403 on every package view.
 * Hiding the section is a rendering decision — the server still refuses.
 */
const upstreamStatus = ref<UpstreamPackageStatus | null>(null);
async function loadUpstreamStatus() {
  upstreamStatus.value = null;
  if (!canReadUpstream.value || !registry.value || !name.value) return;
  try {
    const resp = await authFetch(
      `${API_BASE_URL}/api/v1/admin/upstream/status/${encodeURIComponent(registry.value)}/${encodeURIComponent(name.value)}`,
    );
    if (!resp?.ok) return;
    upstreamStatus.value = (await resp.json()) as UpstreamPackageStatus;
  } catch {
    upstreamStatus.value = null;
  }
}
watch([registry, name, canReadUpstream], loadUpstreamStatus, { immediate: true });

/**
 * RFC 0019 §6.5: on a forge a "version" is a ref, and the commit it resolved
 * to is the identity the cache, the audit log and the verdict key on. The
 * page already reads the resolved refs for the moving-refs panel; the same
 * answer puts the short SHA beside each version row that names a ref.
 */
const refShas = ref<Record<string, string>>({});
const FORGE_TYPES = new Set(["github", "gitlab", "forgejo"]);
async function loadRefShas() {
  refShas.value = {};
  if (!registryType.value || !FORGE_TYPES.has(registryType.value)) return;
  try {
    const resp = await authFetch(
      `${API_BASE_URL}/api/v1/explore/${encodeURIComponent(registry.value)}/${encodeURIComponent(name.value)}/refs`,
    );
    if (!resp?.ok) return;
    const data = (await resp.json()) as { refs?: { git_ref: string; sha: string }[] };
    const map: Record<string, string> = {};
    for (const r of data.refs ?? []) map[r.git_ref] = r.sha;
    refShas.value = map;
  } catch {
    refShas.value = {};
  }
}
watch([registry, name, registryType], loadRefShas, { immediate: true });

const {
  data: adminData,
  error: adminError,
  reload: reloadAdmin,
} = useApi<PackageDetailResponse>(
  () =>
    canAdminister.value
      ? (packageDetail({
          query: { registry: registry.value, name: name.value },
        }) as Promise<{
          data?: unknown;
          error?: unknown;
        }>)
      : Promise.resolve({ data: undefined }),
  [token, registry, name],
);
</script>

<template>
  <!-- Full-bleed, like the masthead, the footer and the catalog.

       This was `max-w-4xl`: 896px of page no matter how wide the window, so at
       1920 the content stopped 1009px short of the rails the header and footer
       draw, and the versions table — nine columns, of which SECURITY, SBOM and
       DOWNLOAD are the ones an operator is here for — sat inside 846px and
       scrolled them out of sight. The measure was also only half applied: the
       whole Administration section below already renders *outside* this div and
       ran edge to edge, so an admin was reading one page drawn to two different
       widths.

       No page-level measure replaces it. That is the rule App.vue states when
       it drops the global container: a page sets no width, and the surfaces
       that need one set their own — the specimen caption at `72ch`,
       `PageHeader` and `EmptyState` at `64ch`, and now the README body, which
       is the only long-form prose here (see `ReadmePanel.vue`). A table and a
       paragraph do not want the same width, which is exactly what one cap on
       the page was giving them. -->
  <div class="space-y-6">
    <PackageBreadcrumb :registry="registry" :name="name" :selected-row="selectedRow" />

    <!-- Loading and error are *panels*, which is the only bounded box this
         system has besides the settings popover — 1px rule, 40/24 padding, a
         Pixel Small heading, copy capped at 64ch.

         Both used to be one bare `<p>`. The loading one rendered an empty body
         with no live region at all, so a screen reader was told nothing was
         happening; the error one printed the literal `HTTP error` with no
         heading, no `role`, no status code and no way back — the end of the
         journey, and therefore the thing the visit is remembered by. -->
    <template v-if="loading">
      <EmptyState
        class="mx-auto max-w-[64ch]"
        role="status"
        aria-live="polite"
        :title="t('packageDetailPage.loading')"
        :description="t('packageDetailPage.loadingHint', { name })"
      />
    </template>

    <template v-else-if="error">
      <EmptyState
        class="mx-auto max-w-[64ch]"
        variant="error"
        role="alert"
        :title="t('packageDetailPage.couldNotLoad', { registry, name })"
        :description="error"
      >
        <template #action>
          <Button variant="outline" size="sm" @click="fetchDetail()">
            {{ t("common.retry") }}
          </Button>
        </template>
      </EmptyState>
    </template>

    <template v-else-if="data">
      <PackageMasthead :data="data" :unfiltered-total="unfilteredTotal" @refresh="fetchDetail()" />

      <VersionSubject
        v-if="selectedRow"
        :registry="registry"
        :name="name"
        :registry-type="registryType"
        :selected-version="selectedVersion"
        :selected-row="selectedRow"
        :fetch-offer="data.fetch"
        :upstream-status="upstreamStatus"
        :sbom-formats="sbomFormats"
        :sbom-state="sbomState"
        :sbom-loading="sbomLoading"
        :sbom-error="sbomError[artifactKey(selectedRow.version)]"
        :fetching="fetching"
        :fetch-result="fetchResult"
        @download-sbom="downloadSbom"
        @fetch-version="onFetchVersion"
      />

      <!-- README, below the subject and above the index, bound to the selected
           version. Fetched separately from the detail response so the catalogue
           cache's TTL never holds a stale document and the detail payload does
           not grow by a megabyte per package (RFC 0007 §5.4). -->
      <ReadmePanel :registry="registry" :name="name" :version="selectedVersion" />

      <!-- The selected version's supply-chain verdict (RFC 0018), when the
           registry has one: what it is held or warned for, and the findings
           for a reader who may see them. Silent otherwise. -->
      <!-- RFC 0019 §6.5: on a forge, what a "version" is needs saying. -->
      <MovingRefsPanel :registry="registry" :name="name" />
      <VerdictPanel
        :registry="registry"
        :name="name"
        :version="selectedVersion"
        :can-rescan="canRescan"
      />

      <VersionsTable
        :name="name"
        :upstream="data.upstream"
        :paged-versions="pagedVersions"
        :selected-version="selectedVersion"
        :show-prereleases="showPrereleases"
        :prerelease-count="prereleaseCount"
        :hidden-prerelease-count="hiddenPrereleaseCount"
        :upstream-version-count="upstreamVersionCount"
        :version-filter="versionFilter"
        :filter-pending="filterPending"
        :filtered-total="filteredTotal"
        :unfiltered-total="unfilteredTotal"
        :version-page="versionPage"
        :version-total-pages="versionTotalPages"
        :ref-shas="refShas"
        @toggle-prereleases="togglePrereleases()"
        @filter="filterVersions"
        @turn-page="turnToPage"
      />
    </template>
  </div>

  <!-- ── Administration ──────────────────────────────────────────────────
         Everything AdminPackageDetail used to be, in place. Same gate as the
         public sheet above: it rendered under a loading or failed page too. -->
  <template v-if="canAdminister && !loading && !error && data">
    <!-- A full-width hairline, which is how this world separates two regions —
         `Separator` with a 24px margin either side is the same line drawn by a
         component that also carries a `my-6`; the region's own `pt-6` is the
         rhythm, on the scale. -->
    <section class="mt-6 space-y-4 border-t border-rule-soft pt-6" aria-labelledby="admin-heading">
      <!-- Not copper. Copper means *waiting or held* in this world — a stale
           entry, a quota approaching its limit — and it was painting a section
           label, which is neither. Dim ink is what every other label on the page
           is set in. -->
      <h2
        id="admin-heading"
        class="font-mono font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground"
      >
        {{ t("common.administration") }}
      </h2>

      <p v-if="adminError" class="text-sm text-destructive">{{ adminError }}</p>

      <template v-else-if="adminData">
        <PackageVersionsTable
          :registry="registry"
          :name="name"
          :versions="adminData.versions"
          @reload="reloadAdmin"
        />
        <PackageBetaChannel :registry="registry" />
        <PackageVisibility :registry="registry" :name="name" />
        <PackageGrants :registry="registry" :name="name" />
        <PackageEventsTable :events="adminData.recent_events" />
      </template>
    </section>
  </template>
</template>
