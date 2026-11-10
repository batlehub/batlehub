<script setup lang="ts">
/**
 * "Is anything I just pulled known-vulnerable" (RFC 0004 §4.2), answered on `/`.
 *
 * Two relationships, labelled rather than merged, because they ask different
 * things of the reader (RFC 0004 R7): you are *exposed to* what you pulled, and
 * you can *fix* what you own. A version you pulled and a different version you
 * own are two rows (R15) — the advisory is a fact about a version, and a row
 * that named only the package would leave you guessing which one is affected.
 *
 * The empty state is a real answer, not a blank: "nothing you pulled recently,
 * and nothing you own, has a known advisory". It says the window it covers,
 * because "clear" is only meaningful against a stated period.
 *
 * The one thing it must never do is imply safety it cannot vouch for. When no
 * SBOM re-scan is configured this instance records no findings at all, so an
 * empty list means "we do not know" — `scanning_available` is what separates
 * the two, and conflating them would be the most harmful thing on this page.
 */
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { myAdvisories } from "@/client/sdk.gen";
import type { MyAdvisoriesResponse, MyAdvisoryDto } from "@/client/types.gen";
import { SEVERITY_KEYS } from "@/config/severity";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { severityVariant } from "@/lib/badge-variants";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { packagePath } from "@/config/navigation";
import HomeSection from "./HomeSection.vue";

const { t } = useI18n();
const { token } = useAuth();

const { data, error, loading } = useApi<MyAdvisoriesResponse>(
  () => myAdvisories() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const advisories = computed<MyAdvisoryDto[]>(() => data.value?.advisories ?? []);
const windowDays = computed(() => data.value?.window_days ?? 7);
const scanningAvailable = computed(() => data.value?.scanning_available ?? false);

const relationLabel = (relation: MyAdvisoryDto["relation"]) =>
  relation === "owned" ? t("advisoriesWidget.owned") : t("advisoriesWidget.pulled");

const severityLabel = (severity: MyAdvisoryDto["highest_severity"]) =>
  t(SEVERITY_KEYS[severity] ?? SEVERITY_KEYS.unknown);
</script>

<template>
  <HomeSection
    heading-id="advisories-widget-heading"
    :title="t('advisoriesWidget.title')"
    :loading="loading"
    :error="error"
  >
    <!-- Nothing recorded, because nothing scans. Not the same as "you're clear",
         and the copy must not let a reader hear the second. -->
    <EmptyState
      v-if="!scanningAvailable"
      :title="t('advisoriesWidget.noScanningTitle')"
      :description="t('advisoriesWidget.noScanningBody')"
      data-testid="advisories-unknown"
    />

    <EmptyState
      v-else-if="!advisories.length"
      :title="t('advisoriesWidget.clearTitle')"
      :description="t('advisoriesWidget.clearBody', { days: windowDays })"
      data-testid="advisories-clear"
    />

    <ul v-else class="divide-y divide-border border border-border">
      <li v-for="row in advisories" :key="`${row.registry}/${row.name}/${row.version}`" class="p-4">
        <div class="flex flex-wrap items-baseline justify-between gap-2">
          <RouterLink
            :to="packagePath(row.registry, row.name)"
            class="font-mono text-sm text-primary hover:underline"
          >
            {{ row.name }}<span class="text-muted-foreground">@{{ row.version }}</span>
          </RouterLink>
          <div class="flex items-center gap-2">
            <Badge variant="outline">{{ relationLabel(row.relation) }}</Badge>
            <Badge :variant="severityVariant(row.highest_severity)">
              {{ severityLabel(row.highest_severity) }}
            </Badge>
          </div>
        </div>

        <p class="mt-1 font-mono text-xs text-muted-foreground">{{ row.registry }}</p>

        <ul class="mt-2 space-y-1">
          <li v-for="f in row.findings" :key="f.osv_id" class="text-xs text-muted-foreground">
            <span class="font-mono text-foreground">{{ f.osv_id }}</span>
            — {{ f.summary }}
            <template v-if="f.fixed_version">
              <span class="text-copper">{{
                t("advisoriesWidget.fixedIn", { version: f.fixed_version })
              }}</span>
            </template>
          </li>
        </ul>
      </li>
    </ul>
  </HomeSection>
</template>
