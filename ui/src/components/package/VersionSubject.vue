<script setup lang="ts">
import { useI18n } from "vue-i18n";
import type { ExploreVersionDto, FetchOfferDto, FirewallDto } from "@/client/types.gen";
import { isUpstreamOnly, rowState, STATE_LABEL_KEYS } from "@/composables/usePackageVersions";
import { formatCount, formatDay } from "@/lib/format";
import { Badge } from "@/components/ui/badge";
import { Resolution } from "@/components/ui/resolution";
import BlockNotes from "@/components/package/BlockNotes.vue";
import InstallLine from "@/components/package/InstallLine.vue";
import ArtifactDownloads from "@/components/package/ArtifactDownloads.vue";
import FetchVersion from "@/components/package/FetchVersion.vue";

/**
 * RFC 0014 §4.6: what the upstream audit knows about this package, for the
 * badge on the selected version. Admin-only and best-effort — a `503` is
 * "the audit does not run in this process", a `404` "not an audited
 * registry", and both mean no badge, not an error.
 */
export interface UpstreamRow {
  version?: string | null;
  state: "missing" | "disappeared";
  consecutive_misses: number;
}
export interface UpstreamPackageStatus {
  policy: "audit" | "block";
  rows: UpstreamRow[];
}

/** The selected version, drawn as the page's subject. */
const props = defineProps<{
  registry: string;
  name: string;
  registryType: string | null;
  selectedVersion: string | null;
  selectedRow: ExploreVersionDto;
  fetchOffer: FetchOfferDto;
  upstreamStatus: UpstreamPackageStatus | null;
  sbomFormats: string[];
  sbomState: "available" | "none" | "unknown";
  sbomLoading: string | null;
  sbomError?: string;
  fetching: string | null;
  fetchResult: Record<string, string>;
}>();

const emit = defineEmits<{
  "download-sbom": [version: string, fmt: "spdx" | "cyclonedx"];
  "fetch-version": [version: string];
}>();

const { t } = useI18n();

const stateLabel = (row: { state?: string; firewall: FirewallDto }) =>
  t(STATE_LABEL_KEYS[rowState(row)]);

/** The audit's row for `version`: its own, or the whole package's. */
function upstreamRowFor(version: string): UpstreamRow | null {
  const rows = props.upstreamStatus?.rows ?? [];
  return rows.find((r) => r.version === version) ?? rows.find((r) => !r.version) ?? null;
}
const UPSTREAM_BADGE_KEYS: Record<string, string> = {
  missing: "packageDetailPage.upstreamMissing",
  disappeared: "packageDetailPage.upstreamDisappeared",
  blocked: "packageDetailPage.upstreamBlocked",
};
function upstreamBadgeKey(row: UpstreamRow): string {
  if (row.state === "disappeared" && props.upstreamStatus?.policy === "block") {
    return UPSTREAM_BADGE_KEYS.blocked;
  }
  return UPSTREAM_BADGE_KEYS[row.state];
}
</script>

<template>
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
  <section class="border-t border-rule-soft pt-6" aria-labelledby="subject-heading">
    <div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4">
      <div class="min-w-0">
        <!-- The version, in the reading face, at the section step. No label
             above it: the package name is the h1 immediately above, so
             "react / 18.3.1" reads as a coordinate without a word spent
             announcing that a version is a version. -->
        <h2 id="subject-heading" class="font-mono text-xl text-foreground [overflow-wrap:anywhere]">
          {{ selectedRow.version }}
        </h2>
        <p class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
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
          upstreamRowFor(selectedRow.version)!.state === 'disappeared' ? 'destructive' : 'copper'
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

    <!-- RFC 0044: vulnerabilities and the SBOM live on the Security tab. -->
    <BlockNotes :registry="registry" :name="name" :selected-row="selectedRow" part="notes" />

    <InstallLine :registry-type="registryType" :name="name" :version="selectedVersion" />

    <ArtifactDownloads
      part="download"
      :registry="registry"
      :name="name"
      :registry-type="registryType"
      :selected-row="selectedRow"
      :sbom-formats="sbomFormats"
      :sbom-state="sbomState"
      :sbom-loading="sbomLoading"
      :sbom-error="sbomError"
      @download-sbom="(version, fmt) => emit('download-sbom', version, fmt)"
    />

    <FetchVersion
      :selected-row="selectedRow"
      :offer="fetchOffer"
      :fetching="fetching"
      :fetch-result="fetchResult"
      @fetch-version="emit('fetch-version', $event)"
    />
  </section>
</template>
