<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute } from "vue-router";
import { securityCoverageRegistry } from "@/client/sdk.gen";
import type { CoverageRow } from "@/client/types.gen";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import CoverageProtection from "@/components/admin/CoverageProtection.vue";
import { docsUrl } from "@/config";
import { formatEvery, formatSpan } from "@/lib/format";

/**
 * One registry's protection (RFC 0050 §4.5): what it refuses and holds, its
 * scanners, and how far the scans have got. A registry unknown or sealed for
 * the viewer answers 404, read as "no such registry" — never as a seal.
 */
const { t, locale } = useI18n();
const route = useRoute();
const name = computed(() => String(route.params.registry ?? ""));

const row = ref<CoverageRow | null>(null);
const missing = ref(false);
const loading = ref(false);
const error = ref<string | null>(null);

async function load() {
  loading.value = true;
  error.value = null;
  missing.value = false;
  try {
    const { data, response } = await securityCoverageRegistry({
      path: { registry: name.value },
    });
    if (response?.status === 404) {
      missing.value = true;
      row.value = null;
      return;
    }
    if (!data) throw new Error(t("securityCoverage.loadFailed"));
    row.value = data;
  } catch (e) {
    error.value = (e as Error).message;
  } finally {
    loading.value = false;
  }
}
watch(name, load, { immediate: true });
</script>

<template>
  <div class="space-y-6">
    <RouterLink
      to="/security/coverage"
      class="text-sm text-muted-foreground hover:underline underline-offset-[3px]"
      >← {{ t("securityNav.coverage") }}</RouterLink
    >
    <PageHeader :title="name" :description="t('securityCoverage.registryDescription')" />
    <AsyncState :loading="loading" :error="error" :on-retry="load">
      <p v-if="missing" class="text-sm" data-testid="missing">
        {{ t("securityCoverage.missing", { name }) }}
      </p>
      <template v-else-if="row">
        <p class="text-sm"><CoverageProtection :row="row" /></p>
        <template v-if="row.protected">
          <dl class="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[12rem_1fr]">
            <template v-if="row.min_age_secs != null">
              <dt class="text-muted-foreground">{{ t("securityCoverage.minAge") }}</dt>
              <dd>{{ formatSpan(row.min_age_secs) }}</dd>
            </template>
            <template v-if="row.serve_unscanned_after_secs != null">
              <dt class="text-muted-foreground">{{ t("securityCoverage.unscannedAfter") }}</dt>
              <dd data-testid="unscanned-after">
                {{
                  row.serve_unscanned_after_secs === 0
                    ? t("securityCoverage.unscannedNever")
                    : formatSpan(row.serve_unscanned_after_secs)
                }}
              </dd>
            </template>
            <template v-if="row.unrated_as">
              <dt class="text-muted-foreground">{{ t("securityCoverage.unratedAs") }}</dt>
              <dd class="font-mono">{{ row.unrated_as }}</dd>
            </template>
            <dt class="text-muted-foreground">{{ t("securityCoverage.rescan") }}</dt>
            <dd>
              <template v-if="row.rescan_interval_secs">{{
                formatEvery(row.rescan_interval_secs)
              }}</template>
              <span v-else class="text-copper">▲ {{ t("securityCoverage.noRescan") }}</span>
            </dd>
            <template v-if="row.scanners">
              <dt class="text-muted-foreground">{{ t("securityCoverage.scanners") }}</dt>
              <dd class="font-mono text-xs">
                {{
                  row.scanners
                    .map((s) =>
                      s.required ? `${s.name} (${t("securityCoverage.required")})` : s.name,
                    )
                    .join(", ") || "—"
                }}
              </dd>
            </template>
            <dt class="text-muted-foreground">{{ t("securityCoverage.scans") }}</dt>
            <dd class="font-mono tabular-nums" data-testid="scan-counts">
              {{
                row.scanned == null || row.waiting == null || row.failed == null
                  ? t("securityStatus.couldNotRead")
                  : t("securityCoverage.scanCounts", {
                      scanned: row.scanned,
                      waiting: row.waiting,
                      failed: row.failed,
                    })
              }}
            </dd>
          </dl>
        </template>
        <p
          v-else-if="!row.cve_gate"
          class="text-sm text-muted-foreground"
          data-testid="not-protected"
        >
          {{ t("securityCoverage.notProtectedHint") }}
          <a
            :href="docsUrl('guide/security-gates', locale)"
            class="text-primary underline underline-offset-[3px]"
            >{{ t("securityOverview.howToProtect") }}</a
          >
        </p>
      </template>
    </AsyncState>
  </div>
</template>
