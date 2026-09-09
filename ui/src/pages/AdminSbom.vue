<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { API_BASE_URL } from "@/config";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import FlagsPanel from "@/components/admin/FlagsPanel.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { useRegistryOptions } from "@/composables/useRegistryOptions";

const { t } = useI18n();

/** RFC 0004-bis §6.2: the registry set is closed, small and already fetched. */
const { options: registryOptions } = useRegistryOptions();

/**
 * The registry set plus an explicit "all registries" entry.
 *
 * The field is optional by contract, so blank has to be a *choosable* value —
 * a `Select` that cannot express it would silently narrow every use that
 * relied on leaving it empty (RFC 0004-bis §6.2).
 */
const registryOptionsWithAll = computed(() => [
  { value: "", label: t("adminPackages.allRegistries") },
  ...registryOptions.value,
]);

const { authFetch } = useAuthFetch();

// ── Filter state ──────────────────────────────────────────────────────────────

const registry = ref("");
const fromDate = ref("");
const toDate = ref("");
const format = ref<"spdx" | "cyclonedx">("spdx");

const loading = ref(false);
const errorMsg = ref<string | null>(null);

// ── Helpers ───────────────────────────────────────────────────────────────────

async function downloadBlob(url: string, defaultFilename: string) {
  const resp = await authFetch(`${API_BASE_URL}${url}`);
  if (!resp.ok) throw new Error(await resp.text().catch(() => `HTTP ${resp.status}`));
  const disposition = resp.headers.get("Content-Disposition") ?? "";
  const match = disposition.match(/filename="([^"]+)"/);
  const filename = match?.[1] ?? defaultFilename;
  const blob = await resp.blob();
  const a = Object.assign(document.createElement("a"), {
    href: URL.createObjectURL(blob),
    download: filename,
  });
  a.click();
  URL.revokeObjectURL(a.href);
}

// ── Export action ─────────────────────────────────────────────────────────────

/**
 * A backwards range is refused here rather than sent (RFC 0004-bis §4.3).
 *
 * `from > to` selects nothing by definition, and the endpoint answers it with a
 * perfectly valid empty SBOM — a file that says this instance has no packages.
 * On an export an operator may hand to an auditor, an empty answer that reads
 * as a fact is the failure mode this whole RFC is about.
 */
const rangeInverted = computed(
  () => !!fromDate.value && !!toDate.value && fromDate.value > toDate.value,
);

async function exportSbom() {
  if (rangeInverted.value) {
    errorMsg.value = t("adminSbom.rangeInverted");
    return;
  }
  loading.value = true;
  errorMsg.value = null;
  try {
    const params = new URLSearchParams({ format: format.value });
    if (registry.value.trim()) params.set("registry", registry.value.trim());
    if (fromDate.value) params.set("from", `${fromDate.value}T00:00:00Z`);
    if (toDate.value) params.set("to", `${toDate.value}T23:59:59Z`);

    const ext = format.value === "cyclonedx" ? "cyclonedx.json" : "spdx.json";
    const ts = new Date().toISOString().slice(0, 10).replaceAll("-", "");
    const label = registry.value.trim() || "all";
    await downloadBlob(
      `/api/v1/sbom/export?${params.toString()}`,
      `sbom-export-${label}-${ts}.${ext}`,
    );
  } catch (e: unknown) {
    errorMsg.value = e instanceof Error ? e.message : t("adminSbom.exportFailed");
  } finally {
    loading.value = false;
  }
}
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <PageHeader :title="t('adminSbom.sbomExport')" variant="display" />

    <!-- Feedback -->
    <div
      v-if="errorMsg"
      class="rounded-sm border border-destructive/40 px-4 py-2 text-destructive text-sm"
    >
      {{ errorMsg }}
    </div>

    <!-- Export card -->
    <Card>
      <CardHeader>
        <CardTitle>{{ t("adminSbom.exportOrgLevelSbom") }}</CardTitle>
      </CardHeader>
      <CardContent class="space-y-4">
        <p class="text-sm text-muted-foreground">{{ t("adminSbom.generatesAMergedSbom") }}</p>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4 max-w-xl">
          <!-- Registry filter -->
          <div class="space-y-1.5">
            <Label for="sbom-registry"
              >{{ t("common.registry") }}
              <span class="text-muted-foreground font-normal">{{
                t("adminSbom.optional")
              }}</span></Label
            >
            <!-- A closed set, already fetched: `Select`, not a box whose placeholder
                 guesses a naming convention (RFC 0004-bis §6.2). Optional, so it
                 carries an explicit "all registries" option — a `Select` that
                 cannot express blank would silently narrow every export. -->
            <Select
              id="sbom-registry"
              v-model="registry"
              :options="registryOptionsWithAll"
              :placeholder="t('adminPackages.allRegistries')"
            />
          </div>

          <!-- Format -->
          <div class="space-y-1.5">
            <Label for="sbom-format">{{ t("common.format") }}</Label>
            <select
              id="sbom-format"
              v-model="format"
              class="w-full border border-input rounded-sm px-2 py-2 font-mono text-sm bg-background focus:outline-none focus:ring-2 focus:ring-ring"
            >
              <option value="spdx">{{ t("adminSbom.spdx23") }}</option>
              <option value="cyclonedx">{{ t("adminSbom.cyclonedx14") }}</option>
            </select>
          </div>

          <!-- From date -->
          <div class="space-y-1.5">
            <Label for="sbom-from"
              >{{ t("common.from") }}
              <span class="text-muted-foreground font-normal">{{
                t("adminSbom.optional")
              }}</span></Label
            >
            <Input id="sbom-from" v-model="fromDate" type="date" />
          </div>

          <!-- To date -->
          <div class="space-y-1.5">
            <Label for="sbom-to"
              >To
              <span class="text-muted-foreground font-normal">{{
                t("adminSbom.optional")
              }}</span></Label
            >
            <Input id="sbom-to" v-model="toDate" type="date" />
          </div>
        </div>

        <!-- Refused at the edge, and visibly: an operator should see why the
             button is dark rather than press it and read an error. -->
        <p v-if="rangeInverted" class="text-sm text-destructive">
          {{ t("adminSbom.rangeInverted") }}
        </p>
        <Button :disabled="loading || rangeInverted" @click="exportSbom">
          {{ loading ? t("adminSbom.exporting") : t("adminSbom.downloadSbom") }}
        </Button>
      </CardContent>
    </Card>

    <!-- RFC 0002: what the configured flag sources have said. Beside the
         SBOM export because it is the other half of the same question —
         what this instance knows about a version's safety. -->
    <FlagsPanel />

    <!-- About card -->
    <Card>
      <CardHeader>
        <CardTitle class="text-base">{{ t("adminSbom.aboutSbomFormats") }}</CardTitle>
      </CardHeader>
      <CardContent class="text-sm text-muted-foreground space-y-2">
        <p>
          <i18n-t keypath="adminSbom.spdxBlurb" tag="span">
            <template #name
              ><strong class="text-foreground">{{ t("adminSbom.spdx23") }}</strong></template
            >
          </i18n-t>
        </p>
        <p>
          <i18n-t keypath="adminSbom.cyclonedxBlurb" tag="span">
            <template #name
              ><strong class="text-foreground">{{ t("adminSbom.cyclonedx14") }}</strong></template
            >
          </i18n-t>
        </p>
        <p>
          <i18n-t keypath="adminSbom.perArtifactSboms" tag="span">
            <template #catalog
              ><RouterLink to="/packages" class="underline hover:text-foreground">{{
                t("adminSbom.catalog")
              }}</RouterLink></template
            >
          </i18n-t>
        </p>
      </CardContent>
    </Card>
  </div>
</template>
