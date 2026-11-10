<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, onBeforeUnmount, watch } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { Check, Copy, FileJson, FileCode, Download } from "@lucide/vue";
import { listRegistries, packageDetail } from "@/client/sdk.gen";
import type { FirewallDto, PackageDetailResponse, RegistryInfo } from "@/client/types.gen";
import { installCommandFor } from "@/config/registryTypes";
import { useAuth } from "@/composables/useAuth";
import PackageVersionsTable from "@/components/admin/PackageVersionsTable.vue";
import PackageBetaChannel from "@/components/admin/PackageBetaChannel.vue";
import PackageVisibility from "@/components/admin/PackageVisibility.vue";
import PackageGrants from "@/components/admin/PackageGrants.vue";
import PackageEventsTable from "@/components/admin/PackageEventsTable.vue";
import ReadmePanel from "@/components/package/ReadmePanel.vue";
import VerdictPanel from "@/components/package/VerdictPanel.vue";
import MovingRefsPanel from "@/components/package/MovingRefsPanel.vue";
import UpstreamNotice from "@/components/package/UpstreamNotice.vue";
import PackageBreadcrumb from "@/components/package/PackageBreadcrumb.vue";
import PackageMasthead from "@/components/package/PackageMasthead.vue";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { rowState, STATE_LABEL_KEYS, usePackageVersions } from "@/composables/usePackageVersions";
import { useApi, extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import { formatCount, formatDay } from "@/lib/format";
import { severityVariant } from "@/lib/badge-variants";
import { Badge } from "@/components/ui/badge";
import { Resolution } from "@/components/ui/resolution";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { SearchInput } from "@/components/ui/search-input";
import { RuleNote } from "@/components/ui/rule-note";
import { Pagination } from "@/components/ui/pagination";
import {
  Table,
  TableHeader,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

const { token, can, isAuthenticated } = useAuth();
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
  versionLink,
  fetching,
  fetchResult,
  onFetchVersion,
  fetchDetail,
} = usePackageVersions(registry, name);

/**
 * Whether a row describes a version this instance holds no bytes for.
 *
 * Every cell that would be a fact about what we hold reads *unknown* on such a
 * row rather than `0` or `—`: nobody has downloaded it *through here*, which is
 * not the same as nobody having downloaded it (RFC 0007 §4.2).
 */
function isUpstreamOnly(source: string) {
  return source === "upstream";
}

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

onBeforeUnmount(() => {
  if (copyTimer) clearTimeout(copyTimer);
});

const stateLabel = (row: { state?: string; firewall: FirewallDto }) =>
  t(STATE_LABEL_KEYS[rowState(row)]);

/**
 * The blocked variant of a firewall status, or `null`.
 *
 * In the script rather than in the template because this is where TypeScript can
 * narrow the union: `FirewallDto` is a discriminated union that *does* declare
 * `reason`, `blocked_by` and `blocked_at`, and the template read them through
 * three `as any` casts because a `v-if` on one element does not narrow a type
 * for a sibling. The casts meant the one part of this page that renders a
 * policy decision was the one part unchecked against the OpenAPI contract.
 */
function blockedInfo(fw: FirewallDto) {
  return fw.status === "blocked" ? fw : null;
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

/** The one line that installs the selected version, when its type has an honest one. */
const installLine = computed(() =>
  selectedVersion.value
    ? installCommandFor(registryType.value, name.value, selectedVersion.value)
    : null,
);

/**
 * Copy confirmed in place, and reset.
 *
 * A copy that says nothing leaves the reader wondering whether it took; a copy
 * that says so forever stops being feedback. Two seconds, and no motion — the
 * system animates exactly one thing and this is not it.
 */
const copied = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | null = null;

async function copyInstall() {
  if (!installLine.value) return;
  try {
    await navigator.clipboard.writeText(installLine.value.command);
    copied.value = true;
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => {
      copied.value = false;
      copyTimer = null;
    }, 2000);
  } catch {
    // A clipboard the browser refuses (no permission, no secure context) is not
    // an error worth a banner: the command is on screen and selectable, which is
    // the fallback every terminal user already uses.
  }
}

// ── Download URL construction ──────────────────────────────────────────────────

/**
 * Build the proxy download URL for a given version based on the registry type.
 * Returns null for registries whose URL can't be derived purely from name/version
 * (Maven, PyPI simple page, etc.).
 */
function downloadUrl(version: string): string | null {
  const n = name.value;
  const r = registry.value;
  const base = `${API_BASE_URL}/proxy/${encodeURIComponent(r)}`;
  switch (registryType.value) {
    case "cargo":
      return `${base}/${encodeURIComponent(n)}/${encodeURIComponent(version)}/download`;
    case "npm":
      // encodeURIComponent handles scoped packages: @scope/pkg → %40scope%2Fpkg (one path segment)
      return `${base}/${encodeURIComponent(n)}/${encodeURIComponent(version)}/tarball`;
    case "nuget":
      return `${base}/nuget/v3/flat/${encodeURIComponent(n.toLowerCase())}/${encodeURIComponent(version.toLowerCase())}/${encodeURIComponent(n.toLowerCase())}.${encodeURIComponent(version.toLowerCase())}.nupkg`;
    case "rubygems":
      return `${base}/gems/${encodeURIComponent(n)}-${encodeURIComponent(version)}.gem`;
    case "pypi":
      // PyPI has hashed filenames — link to the simple page instead
      return `${base}/simple/${encodeURIComponent(n)}/`;
    case "conda":
      return `${base}/noarch/${encodeURIComponent(n)}-${encodeURIComponent(version)}-py_0.conda`;
    case "vsix":
    case "openvsx": {
      const parts = n.split(".");
      if (parts.length === 2) {
        return `${base}/${encodeURIComponent(parts[0])}.${encodeURIComponent(parts[1])}/${encodeURIComponent(version)}/vsix`;
      }
      return null;
    }
    default:
      return null;
  }
}

/**
 * Administration is a *section of this page* now, not a parallel page at another
 * URL with its own layout and its own back button. One package, one address
 * (RFC 0003 §4.2).
 *
 * Fetched only when the viewer is an admin: the endpoint is admin-only
 * server-side, so firing it for everyone would mean a 403 on every package view.
 * Hiding the section is a rendering decision — the server still refuses.
 */
/**
 * RFC 0014 §4.6: what the upstream audit knows about this package, for the
 * badge on the selected version. Admin-only and best-effort — a `503` is
 * "the audit does not run in this process", a `404` "not an audited
 * registry", and both mean no badge, not an error.
 */
interface UpstreamRow {
  version?: string | null;
  state: "missing" | "disappeared";
  consecutive_misses: number;
}
interface UpstreamPackageStatus {
  policy: "audit" | "block";
  rows: UpstreamRow[];
}
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
/** git's own abbreviation. */
const shortSha = (version: string) => refShas.value[version]?.slice(0, 7) ?? null;

/** The audit's row for `version`: its own, or the whole package's. */
function upstreamRowFor(version: string): UpstreamRow | null {
  const rows = upstreamStatus.value?.rows ?? [];
  return rows.find((r) => r.version === version) ?? rows.find((r) => !r.version) ?? null;
}
const UPSTREAM_BADGE_KEYS: Record<string, string> = {
  missing: "packageDetailPage.upstreamMissing",
  disappeared: "packageDetailPage.upstreamDisappeared",
  blocked: "packageDetailPage.upstreamBlocked",
};
function upstreamBadgeKey(row: UpstreamRow): string {
  if (row.state === "disappeared" && upstreamStatus.value?.policy === "block") {
    return UPSTREAM_BADGE_KEYS.blocked;
  }
  return UPSTREAM_BADGE_KEYS[row.state];
}

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

      <!-- ── The subject: one version ──────────────────────────────────────
           The page's centre of gravity, and the reason it is not a table with a
           README under it any more. Every question PRODUCT names — *why was this
           blocked*, *what do I paste* — is answered by exactly one version, and
           the page used to answer "here are twenty-five, click one to change a
           README". So the selected version is drawn as the subject: its state at
           full resolution, its rule in its own words, its vulnerabilities as
           text, and the one line that installs it.

           A region separated by a full-width hairline, not a card. The world has
           no card, and three of them stacked here were the page's largest
           disagreement with its own design system. -->
      <section
        v-if="selectedRow"
        class="border-t border-rule-soft pt-6"
        aria-labelledby="subject-heading"
      >
        <div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4">
          <div class="min-w-0">
            <!-- The version, in the reading face, at the section step. No label
                 above it: the package name is the h1 immediately above, so
                 "react / 18.3.1" reads as a coordinate without a word spent
                 announcing that a version is a version. -->
            <h2
              id="subject-heading"
              class="font-mono text-xl text-foreground [overflow-wrap:anywhere]"
            >
              {{ selectedRow.version }}
            </h2>
            <p
              class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground"
            >
              <span>
                {{
                  selectedRow.source === "local"
                    ? t("packageDetailPage.local")
                    : isUpstreamOnly(selectedRow.source)
                      ? t("packageDetailPage.notHeldHere")
                      : t("common.proxied")
                }}
              </span>
              <span v-if="selectedRow.published_at">
                {{ t("common.published") }}
                <span class="text-foreground tabular-nums">{{
                  formatDay(selectedRow.published_at)
                }}</span>
              </span>
              <span>
                {{ t("common.downloads") }}
                <span class="text-foreground tabular-nums">{{
                  selectedRow.download_count === null || selectedRow.download_count === undefined
                    ? t("common.unknown")
                    : formatCount(selectedRow.download_count)
                }}</span>
              </span>
              <span>{{ selectedRow.license ?? t("packageDetailPage.licenseUnknown") }}</span>
            </p>
            <!-- Why it is unknown, as a sentence rather than as a `title=`.
                 "no manifest parser for this registry type" and "declares no
                 licence" are different facts and the page must not render them
                 identically; the explanation was written for exactly that and
                 was reachable only by holding a mouse still over a `<span>`. -->
            <p v-if="!selectedRow.license" class="mt-2 max-w-[64ch] text-xs text-muted-foreground">
              {{ t("packageDetailPage.licenseUnknownHelp") }}
            </p>
          </div>

          <!-- The supply-chain badge, which is a picture *about this version*
               and was drawn once per row — twenty-five requests to a third-party
               host on one page view, at 71×16px each. -->
          <a
            v-if="selectedRow.socket_badge_url"
            :href="selectedRow.socket_badge_url"
            target="_blank"
            rel="noopener noreferrer"
            class="order-last shrink-0 py-1"
            :title="t('packageDetailPage.supplyChainReportOn')"
          >
            <img :src="selectedRow.socket_badge_url" alt="socket.dev" class="h-5" />
          </a>

          <!-- RFC 0014: the upstream audit's word on this version, for an
               admin — *missing* is a miss not yet believed, *disappeared* a
               confirmed one, and under "block" the block it wrote. -->
          <Badge
            v-if="upstreamRowFor(selectedRow.version)"
            :variant="
              upstreamRowFor(selectedRow.version)!.state === 'disappeared'
                ? 'destructive'
                : 'copper'
            "
            class="shrink-0"
            :title="t('packageDetailPage.upstreamBadgeHelp')"
            data-testid="upstream-badge"
          >
            {{ t(upstreamBadgeKey(upstreamRowFor(selectedRow.version)!)) }}
          </Badge>

          <!-- The signature device, on the row it was written for.
               `blocked` used to be the one state of six that never reached it:
               the table's `v-else` handed that row a crimson chip instead, so
               the world's organising idea was switched off on precisely the row
               a reader opens the page for. -->
          <Resolution
            :state="rowState(selectedRow)"
            :label="stateLabel(selectedRow)"
            class="shrink-0"
          />
        </div>

        <!-- The rule, beside the thing it refused, in text.
             It was a `hidden group-hover:block` span: no focus trigger, no
             `aria-describedby`, no dismiss — WCAG 2.2 1.4.13 failed on all three
             counts, unreadable to a screen reader and unreachable on a phone.
             It is the answer the page exists to give, so it is simply on the
             page. The tie bar is the denial note's own, ported from the catalog
             rather than invented here. -->
        <RuleNote v-if="blockedInfo(selectedRow.firewall)" class="mt-4">
          <span class="text-primary">{{ t("common.reasonLabel") }}</span>
          {{ blockedInfo(selectedRow.firewall)!.reason }}
          <span class="px-2" aria-hidden="true">·</span>
          {{
            t("packageDetailPage.blockedByOn", {
              who: blockedInfo(selectedRow.firewall)!.blocked_by,
              when: formatDay(blockedInfo(selectedRow.firewall)!.blocked_at),
            })
          }}
          <RouterLink
            class="text-primary underline underline-offset-[3px]"
            :to="{
              path: '/tools/access-check',
              query: { registry, name, version: selectedRow.version },
            }"
            >{{ t("packageDetailPage.why") }}</RouterLink
          >
        </RuleNote>

        <!-- The deprecation message, which was a `title=` on a chip in the
             table — the upstream's own sentence about why not to use this
             version, available only to a mouse held still. -->
        <RuleNote v-if="selectedRow.deprecated" class="mt-4">
          <span class="text-copper">{{ t("packageDetailPage.deprecated") }}</span>
          {{ selectedRow.deprecation_message ?? t("packageDetailPage.deprecatedNoReason") }}
        </RuleNote>

        <!-- Vulnerabilities as text, for the same reason. A severity chip whose
             identifier and summary appear only under a stationary mouse is a
             supply-chain fact this page is not really reporting. -->
        <ul v-if="selectedRow.vulnerabilities.length" class="mt-4 space-y-2">
          <RuleNote v-for="vuln in selectedRow.vulnerabilities" :key="vuln.osv_id" as="li">
            <Badge :variant="severityVariant(vuln.severity)" class="mr-2 text-xs">{{
              vuln.severity
            }}</Badge>
            <span class="font-mono text-foreground">{{ vuln.osv_id }}</span>
            <span class="px-2" aria-hidden="true">·</span>
            {{ vuln.summary }}
            <template v-if="vuln.fixed_version">
              <span class="px-2" aria-hidden="true">·</span>
              {{ t("packageDetailPage.fixedIn") }}
              <span class="font-mono text-foreground">{{ vuln.fixed_version }}</span>
            </template>
          </RuleNote>
        </ul>
        <p
          v-else-if="!selectedRow.vulnerabilities_scanned"
          class="mt-4 text-sm text-muted-foreground"
        >
          {{ t("packageDetailPage.notScannedHelp") }}
        </p>

        <!-- The end of the journey, which the page did not have: a reader who
             found their version had nowhere to go and the page stopped on an em
             dash. The command is data (`installCommandFor`), not a switch in
             this component, and a type with no honest one-liner says so rather
             than printing a plausible wrong one. -->
        <div v-if="installLine" class="mt-6">
          <div class="flex items-stretch border border-border">
            <pre
              class="min-w-0 flex-1 overflow-x-auto bg-ground-sunk px-3 py-3 font-mono text-sm text-foreground"
            ><code>{{ installLine.command }}</code></pre>
            <button
              type="button"
              class="flex shrink-0 items-center gap-2 border-l border-border px-3 text-xs uppercase tracking-[0.1em] text-muted-foreground hover:text-foreground"
              @click="copyInstall"
            >
              <component :is="copied ? Check : Copy" class="h-4 w-4" aria-hidden="true" />
              {{ copied ? t("common.copied") : t("common.copy") }}
            </button>
          </div>
          <p class="mt-2 text-xs text-muted-foreground">
            {{ t("packageDetailPage.installAssumesSetup") }}
            <RouterLink class="text-primary underline underline-offset-[3px]" to="/setup">{{
              t("packageDetailPage.setupGuide")
            }}</RouterLink>
          </p>
        </div>

        <!-- The artifact itself and its bill of materials, for the version in
             hand. These were three chips repeated on every row of a nine-column
             table — where the Download one rendered a bare em dash for thirteen
             of the twenty-one registry types, under a column headed "Download",
             stating nothing about why. Here they are absent when there is
             nothing to offer, which is the same answer without the furniture. -->
        <div class="mt-4 flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
          <a
            v-if="downloadUrl(selectedRow.version)"
            :href="downloadUrl(selectedRow.version)!"
            target="_blank"
            rel="noopener noreferrer"
            class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px]"
          >
            <Download class="h-4 w-4" aria-hidden="true" />
            {{ t("packageDetailPage.downloadViaProxy", { version: selectedRow.version }) }}
          </a>
          <template v-if="token && sbomState === 'available'">
            <button
              v-if="sbomFormats.includes('spdx')"
              type="button"
              class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px] disabled:no-underline disabled:opacity-50"
              :disabled="sbomLoading === `${registry}/${name}/${selectedRow.version}:spdx`"
              @click="downloadSbom(selectedRow!.version, 'spdx')"
            >
              <FileJson class="h-4 w-4" aria-hidden="true" />
              {{ t("packageDetailPage.downloadSpdx23") }}
            </button>
            <button
              v-if="sbomFormats.includes('cyclonedx')"
              type="button"
              class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px] disabled:no-underline disabled:opacity-50"
              :disabled="sbomLoading === `${registry}/${name}/${selectedRow.version}:cyclonedx`"
              @click="downloadSbom(selectedRow!.version, 'cyclonedx')"
            >
              <FileCode class="h-4 w-4" aria-hidden="true" />
              {{ t("packageDetailPage.downloadCyclonedx14") }}
            </button>
          </template>
          <span v-else-if="token && sbomState === 'none'" class="text-muted-foreground">{{
            t("packageDetailPage.noSbom")
          }}</span>
          <span v-else-if="token" class="text-muted-foreground">{{
            t("packageDetailPage.sbomPending")
          }}</span>
        </div>
        <output
          v-if="sbomError[artifactKey(selectedRow.version)]"
          class="mt-2 block text-sm text-destructive"
        >
          {{
            t("packageDetailPage.sbomFailed", {
              reason: sbomError[artifactKey(selectedRow.version)],
            })
          }}
        </output>

        <!-- The one filled action on the reader's half of the page, and the only
             act here that spends this instance's bandwidth and writes an audit
             row. It was a 24px outline chip in the second column of a table. -->
        <div v-if="isUpstreamOnly(selectedRow.source)" class="mt-6">
          <p class="mb-3 max-w-[64ch] text-sm text-muted-foreground">
            {{ t("packageDetailPage.notHeldHereHelp") }}
          </p>
          <Button
            v-if="data.fetch.offered"
            :disabled="fetching !== null"
            @click="onFetchVersion(selectedRow!.version)"
          >
            {{
              fetching === selectedRow.version
                ? t("packageDetailPage.fetching")
                : t("packageDetailPage.fetchVersion")
            }}
          </Button>
          <p v-else-if="data.fetch.reason" class="text-sm text-muted-foreground">
            {{ t("packageDetailPage.fetchUnavailable", { reason: data.fetch.reason }) }}
          </p>
          <RouterLink
            v-else-if="!isAuthenticated"
            :to="{ path: '/login', query: { redirect: route.fullPath } }"
            class="text-sm text-primary underline underline-offset-[3px]"
            >{{ t("packageDetailPage.fetchNeedsSession") }}</RouterLink
          >
          <p v-if="fetchResult[selectedRow.version]" class="mt-2 text-sm text-muted-foreground">
            {{ fetchResult[selectedRow.version] }}
          </p>
          <p class="mt-2 max-w-[64ch] text-xs text-muted-foreground">
            {{ t("packageDetailPage.fetchWhatItDoes") }}
          </p>
        </div>
      </section>

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

      <!-- ── The index ─────────────────────────────────────────────────────
           What the table is now: the list you pick the subject from, not the
           page. Everything a reader compares *between* versions stays here;
           everything they read *about* one version moved up to the subject —
           which is what let this go from nine columns to six, three of which
           survive at 390px where five of the nine used to be behind a
           horizontal scroll with no affordance. -->
      <section class="border-t border-rule-soft pt-6" aria-labelledby="versions-heading">
        <div class="space-y-3">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <h2 id="versions-heading" class="font-mono text-base text-foreground">
              {{ t("common.versions") }}
            </h2>
            <!-- The filter says what it is hiding. A control reading "Show
                 pre-releases" over a list that has none is a promise of
                 something to reveal, so it renders only when there is; and the
                 count is in the label rather than in a tooltip, because "8
                 hidden" is the fact that decides whether a reader clicks. -->
            <!-- No `aria-pressed`. The label already inverts — "Hide 8
                 pre-releases" — so a pressed state on top of it announces as
                 "Hide 8 pre-releases, pressed", which is a double negative for
                 the one reader who cannot see the list change. One channel. -->
            <Button
              v-if="prereleaseCount > 0"
              variant="outline"
              size="sm"
              class="px-2 text-xs"
              @click="togglePrereleases()"
            >
              {{
                showPrereleases
                  ? t("packageDetailPage.hidePrereleases", prereleaseCount)
                  : t("packageDetailPage.showPrereleases", hiddenPrereleaseCount)
              }}
            </Button>
          </div>
          <div v-if="data.upstream.attempted">
            <UpstreamNotice
              :upstream="data.upstream"
              :upstream-version-count="upstreamVersionCount"
            />
          </div>
          <!-- The filter sits with the list it filters rather than in the card
               header with the pre-release control: that one changes *what the
               list is*, this one searches inside it, and putting them together
               would read as two halves of one filter. -->
          <div v-if="unfilteredTotal > 1" class="flex flex-wrap items-center gap-3">
            <SearchInput
              :placeholder="t('packageDetailPage.filterVersions')"
              :aria-label="t('packageDetailPage.filterVersions')"
              :value="versionFilter"
              @input="filterVersions(($event.target as HTMLInputElement).value)"
            />
            <!-- The live region that answers "did my filter do anything".
                 It is not the only one on the page — `Pagination` and
                 `UpstreamNotice` are shared components and carry their own, and
                 taking those away here would change every page that uses them —
                 so this comment states the position rather than a fix that was
                 never made: three announcements can still land on one page turn,
                 and consolidating them is a change to those components. -->
            <output class="text-xs text-muted-foreground" aria-live="polite">
              {{
                filterPending
                  ? t("packageDetailPage.filtering")
                  : t("packageDetailPage.versionsShown", {
                      shown: filteredTotal,
                      total: unfilteredTotal,
                    })
              }}
            </output>
          </div>
          <Table :label="t('packageDetailPage.tableLabel', { name })">
            <!-- A visible caption stating the count and the order.
                 The list had neither, so a reader on page 4 of 160 could not
                 know whether they were walking forward or backward through
                 time — and `Table` was called three times on an admin's view
                 with no `label`, which announced all three scroll regions with
                 the same generic name. -->
            <caption class="caption-top pb-3 text-left text-xs text-muted-foreground">
              {{
                t("packageDetailPage.tableCaption", {
                  shown: filteredTotal,
                  total: unfilteredTotal,
                })
              }}
            </caption>
            <TableHeader>
              <!-- One solid rule under the head, dashed between the rows: the
                   proof's grammar, ported from the catalog rather than restated.
                   Cells are flush left and padded right, so the first column
                   lines up with the caption and the specimen above it. -->
              <TableRow class="border-b border-solid border-border hover:bg-transparent">
                <TableHead class="pl-0 pr-3 lg:w-[9.5rem]">{{ t("common.state") }}</TableHead>
                <TableHead class="pl-0 pr-3">{{ t("common.version") }}</TableHead>
                <TableHead class="pl-0 pr-3 lg:w-[8rem]">{{ t("common.source") }}</TableHead>
                <!-- Dropped below `lg`, whole, rather than squeezed: at 390px
                     the nine columns this table used to draw claimed 895px of a
                     356px box, so five of them sat behind a scroll with no
                     affordance. Every fact they carry is on the subject above
                     for the version a reader has selected — which is what makes
                     dropping them honest rather than hiding them. -->
                <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-28">{{
                  t("common.downloads")
                }}</TableHead>
                <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-32">{{
                  t("common.published")
                }}</TableHead>
                <TableHead class="hidden px-0 lg:table-cell lg:w-32">{{
                  t("common.security")
                }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <!-- Selection rides on ink and a lit edge, never on fill.

                   It was `bg-muted/40`, and DESIGN.md's Undependable Fill Rule
                   is about exactly this: on this ground the raised fill computes
                   to about 1.06:1 against the sheet, so the marked row was
                   indistinguishable from every other one — the page chose a
                   default version and then had no way to say which. `Facet`
                   already solved this for the registry bay ("selection rides on
                   ink and a lit edge, not a fill"), and this is the same
                   statement in a table: a crimson left edge, the ink lifted to
                   `--ink` and the weight raised.

                   The transparent edge on unselected rows reserves the same 2px,
                   so marking a row moves nothing sideways.

                   `aria-current="true"` carries it to a screen reader, which
                   cannot see either the rule or the weight. -->
              <TableRow
                v-for="ver in pagedVersions"
                :key="`${ver.version}-${ver.source}`"
                :class="[
                  'border-l border-rule-soft align-baseline hover:border-solid hover:border-border',
                  /* The dashed/solid pair is per-row, not per-side: `border-l-solid`
                     is not a Tailwind utility and is defined nowhere in this repo,
                     so writing it left the whole row dashed — including the crimson
                     selection edge, which is the one edge the world says must read
                     as engaged. A selected row is solid on every side; the rest stay
                     dashed at rest and go solid under the pointer. */
                  selectedVersion === ver.version
                    ? 'border-solid border-l-primary font-semibold text-foreground'
                    : 'border-dashed border-l-transparent',
                  ver.is_prerelease && selectedVersion !== ver.version
                    ? 'text-muted-foreground italic'
                    : '',
                  ver.is_prerelease && selectedVersion === ver.version ? 'italic' : '',
                ]"
                :aria-current="selectedVersion === ver.version ? 'true' : undefined"
              >
                <TableCell class="pl-0 pr-3 py-3 align-baseline">
                  <Resolution :state="rowState(ver)" :label="stateLabel(ver)" />
                </TableCell>
                <TableCell class="pl-0 pr-3 py-3 align-baseline font-mono text-sm">
                  <!-- A link, not a click handler on the `<tr>`.
                       The row was `@click` with `cursor-pointer`, no `tabindex`,
                       no `role` and no key handler, so selecting a version — the
                       page's central interaction, which drives both the subject
                       above and the URL — was unavailable to a keyboard while
                       `aria-current` announced a selection that reader could not
                       change. A link brings the keyboard, the focus ring,
                       middle-click-to-a-new-tab and a copyable target with it,
                       and it takes the six nested buttons out of a clickable
                       row.

                       Selection is carried by the crimson 1px edge on the row,
                       the ink, the weight and `aria-current` — four channels,
                       because the fill this used to use computes to 1.06:1 and
                       the 2px coloured stripe it used to draw is a thing the
                       world refuses by name. -->
                  <!-- `replace`, never `push` — the rule stated at the top of
                       this file and not applied here when the click handler
                       became a link. Reading down a version list is not ten
                       destinations: without it, five clicks were five Back
                       presses, and worse, `history.state.back` stopped being
                       `/packages`, so `goBack()` fell through to its
                       reconstruct-by-hand path and dropped the reader on a
                       catalog with its search, scope, sort, page and scroll
                       offset gone — the exact loss the comment at `goBack` says
                       it prevents. One attribute restores both. -->
                  <RouterLink
                    replace
                    :to="versionLink(ver.version)"
                    class="inline-block py-1 [overflow-wrap:anywhere] hover:underline underline-offset-[3px]"
                    :class="selectedVersion === ver.version ? 'text-primary' : ''"
                  >
                    {{ ver.version }}
                  </RouterLink>
                  <!-- RFC 0019 §6.5: the commit this ref resolved to. -->
                  <span
                    v-if="shortSha(ver.version)"
                    class="ml-2 text-xs text-muted-foreground"
                    :title="refShas[ver.version]"
                    data-testid="ref-sha"
                    >{{ shortSha(ver.version) }}</span
                  >
                  <Badge v-if="ver.is_prerelease" variant="outline" class="ml-2 text-xs">
                    {{ t("packageDetailPage.prerelease") }}
                  </Badge>
                  <!-- Copper, not crimson: the subject region already sets this
                       fact in copper, and crimson is spoken for. One page, one
                       fact, one hue. -->
                  <Badge v-if="ver.deprecated" variant="outline" class="ml-2 text-xs text-copper">
                    {{ t("packageDetailPage.deprecated") }}
                  </Badge>
                  <Badge v-if="ver.unlisted" variant="secondary" class="ml-2 text-xs">
                    {{ t("packageDetailPage.unlisted") }}
                  </Badge>
                  <!-- Under the version rather than in a column of its own: the
                       licence is an attribute of this version, and it is the
                       kind of fact a reader compares down a column at a glance.

                       A stated "unknown" rather than a blank when null —
                       rendering nothing would make "no manifest parser for this
                       registry type" indistinguishable from "declares no
                       licence" (RFC 0004-bis §13.1). -->
                  <p class="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
                    {{ ver.license ?? t("packageDetailPage.licenseUnknown") }}
                  </p>
                </TableCell>
                <TableCell class="pl-0 pr-3 py-3 align-baseline">
                  <!-- Three values, not two. `upstream` means this instance
                       holds no bytes for the version and knows about it only
                       because it asked — the badge says so rather than letting
                       it read as something we have.

                       The fetch button that used to live beside it moved to the
                       subject above: it is the one act on this page that spends
                       bandwidth and writes an audit row, and it was a 24px
                       outline chip repeated twenty-five times down a column. -->
                  <Badge
                    :variant="ver.source === 'local' ? 'secondary' : 'outline'"
                    class="text-xs"
                    :class="isUpstreamOnly(ver.source) ? 'border-dashed' : ''"
                  >
                    {{
                      ver.source === "local"
                        ? t("packageDetailPage.local")
                        : isUpstreamOnly(ver.source)
                          ? t("packageDetailPage.notHeldHere")
                          : t("common.proxied")
                    }}
                  </Badge>
                </TableCell>
                <!-- `unknown`, never `0`: a definite-looking number for a
                     version this instance has never held would be a claim we
                     cannot support (RFC 0007 §4.2). -->
                <TableCell
                  class="hidden pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  {{
                    ver.download_count === null || ver.download_count === undefined
                      ? t("common.unknown")
                      : formatCount(ver.download_count)
                  }}
                </TableCell>
                <TableCell
                  class="hidden whitespace-nowrap pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  {{ formatDay(ver.published_at ?? null) }}
                </TableCell>
                <TableCell
                  class="hidden px-0 py-3 align-baseline text-muted-foreground lg:table-cell"
                >
                  <!-- A count, not five hoverable chips. What each advisory
                       *says* is on the subject above, in text, for the version
                       the reader selected — which is the only place it was ever
                       readable without a mouse. -->
                  <span v-if="!ver.vulnerabilities_scanned" class="text-xs">
                    {{ t("packageDetailPage.notScanned") }}
                  </span>
                  <span v-else-if="ver.vulnerabilities.length" class="text-xs text-destructive">
                    {{ t("packageDetailPage.vulnerabilityCount", ver.vulnerabilities.length) }}
                  </span>
                  <span v-else class="text-xs">{{ t("packageDetailPage.noKnownVulns") }}</span>
                </TableCell>
              </TableRow>
              <!-- A filter that matched nothing is not a package with no
                   versions, and answering the first with the second's words
                   ("nothing has been pulled through") would describe the
                   instance to a reader who asked about their own typing. The
                   counter above already says `0 of 60 shown`; this says which
                   of the two absences the empty table is. -->
              <TableRow v-if="pagedVersions.length === 0 && unfilteredTotal > 0">
                <TableCell colspan="6" class="px-0 py-6 text-center text-muted-foreground">
                  <!-- `filtered`, which its own doc calls required rather than
                       inferred, and which this call site omitted — so
                       `data-filtered="false"` was being written on precisely the
                       state the flag exists to mark. -->
                  <EmptyState
                    filtered
                    :title="t('packageDetailPage.noVersionsMatch')"
                    :description="t('packageDetailPage.noVersionsMatchHint')"
                  />
                </TableCell>
              </TableRow>
              <TableRow v-else-if="pagedVersions.length === 0">
                <TableCell colspan="6" class="px-0 py-6 text-center text-muted-foreground">
                  <!-- Two different absences, and the reader needs to know
                       which: "nothing has been pulled through, and the upstream
                       does not have it either" is an answer; "the upstream
                       could not be reached" is a gap. -->
                  <EmptyState
                    :title="t('packageDetailPage.noVersionsYet')"
                    :description="
                      data.upstream.error
                        ? t('packageDetailPage.upstreamUnreachable')
                        : data.upstream.attempted
                          ? t('packageDetailPage.upstreamHasNoneEither')
                          : t('packageDetailPage.nothingHasBeenPulledThrough')
                    "
                  />
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
          <div v-if="versionTotalPages > 1" class="border-t border-rule-soft pt-3">
            <Pagination
              :page="versionPage"
              :total-pages="versionTotalPages"
              @update:page="turnToPage($event)"
            />
          </div>
        </div>
      </section>
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
