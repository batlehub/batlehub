<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { securityCoverage } from "@/client/sdk.gen";
import type { CoverageList } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import CoverageProtection from "@/components/admin/CoverageProtection.vue";

/**
 * Every registry the viewer may see, protected or not, and how (RFC 0050
 * §4.2). A registry sealed for the viewer is absent, as an unknown one.
 * Scanner names ride only with `config:read`; the column goes without them.
 */
const { t } = useI18n();
const { token } = useAuth();
const { data, loading, error, reload } = useApi<CoverageList>(
  () => securityCoverage() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const rows = computed(() => data.value?.registries ?? []);
// `cve_gate` alone covers a registry, as `CoverageProtection` draws it.
const protectedCount = computed(() => rows.value.filter((r) => r.protected || r.cve_gate).length);
// Left unprotected on purpose (`[console].accepted`): not counted as unprotected.
const unprotectedCount = computed(
  () => rows.value.filter((r) => !r.protected && !r.cve_gate && !r.accepted).length,
);
const withScanners = computed(() => rows.value.some((r) => r.scanners != null));
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      :title="t('securityNav.coverage')"
      :description="t('securityCoverage.description')"
    />
    <AsyncState
      :loading="loading"
      :error="error"
      :empty="!!data && rows.length === 0"
      :empty-message="t('securityCoverage.empty')"
      :on-retry="reload"
    >
      <p class="font-mono text-sm tabular-nums" data-testid="coverage-summary">
        {{
          t("securityCoverage.summary", {
            total: rows.length,
            protected: protectedCount,
            unprotected: unprotectedCount,
          })
        }}
      </p>
      <Table stacked :label="t('securityNav.coverage')" data-testid="coverage-rows">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("common.registry") }}</TableHead>
            <TableHead>{{ t("securityCoverage.mode") }}</TableHead>
            <TableHead>{{ t("securityCoverage.protection") }}</TableHead>
            <TableHead v-if="withScanners">{{ t("securityCoverage.scanners") }}</TableHead>
            <TableHead>{{ t("securityCoverage.gates") }}</TableHead>
            <TableHead class="text-right">{{ t("securityCoverage.scanned") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="r in rows" :key="r.registry">
            <TableCell class="font-mono text-sm">
              <RouterLink
                :to="`/security/coverage/${encodeURIComponent(r.registry)}`"
                class="underline underline-offset-[3px]"
                >{{ r.registry }}</RouterLink
              >
            </TableCell>
            <TableCell class="font-mono text-xs" :label="t('securityCoverage.mode')">{{
              r.mode
            }}</TableCell>
            <TableCell class="text-sm" :label="t('securityCoverage.protection')"
              ><CoverageProtection :row="r"
            /></TableCell>
            <TableCell
              v-if="withScanners"
              class="font-mono text-xs"
              :label="t('securityCoverage.scanners')"
            >
              <template v-if="r.scanners?.length">
                {{
                  r.scanners
                    .map((s) =>
                      s.required ? `${s.name} (${t("securityCoverage.required")})` : s.name,
                    )
                    .join(", ")
                }}
              </template>
              <template v-else>—</template>
            </TableCell>
            <TableCell class="text-xs" :label="t('securityCoverage.gates')">
              {{
                [
                  r.cve_gate ? t("securityCoverage.cveGate") : "",
                  r.license_gate ? t("securityCoverage.licenseGate") : "",
                ]
                  .filter(Boolean)
                  .join(", ") || "—"
              }}
            </TableCell>
            <TableCell
              class="font-mono text-xs tabular-nums md:text-right"
              :label="t('securityCoverage.scanned')"
            >
              <template v-if="r.protected && r.scanned != null && r.waiting != null"
                >{{ r.scanned }} / {{ r.scanned + r.waiting }}</template
              >
              <template v-else-if="r.protected">{{ t("securityStatus.couldNotRead") }}</template>
              <template v-else>—</template>
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </AsyncState>
  </div>
</template>
