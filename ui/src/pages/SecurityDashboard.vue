<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { useAuth } from "@/composables/useAuth";
import { PageHeader } from "@/components/ui/page-header";
import HoldsPanel from "@/components/admin/HoldsPanel.vue";
import FindingsPanel from "@/components/admin/FindingsPanel.vue";
import FlagsPanel from "@/components/admin/FlagsPanel.vue";
import ExposurePanel from "@/components/admin/ExposurePanel.vue";

/**
 * The security console (RFC 0042): beside the admin, for whoever holds a
 * security verb at the instance tier. Each section is drawn only when its own
 * verb is held, so the page shows nothing the viewer cannot use (PRODUCT.md
 * principle 4) and a narrower grant is a shorter page rather than locked boxes.
 */
const { t } = useI18n();
const { can } = useAuth();

/** The verdict listing checks both verbs at the instance tier (decision 8). */
const canFindings = computed(() => can("quarantine:read") && can("findings:read"));
/** The registry whose findings are open, from a row of the holds summary. */
const findingsFor = ref<string | null>(null);
</script>

<template>
  <div class="space-y-6">
    <PageHeader variant="display">
      <template #title>{{ t("securityDashboard.title") }}</template>
      <template #description>{{ t("securityDashboard.description") }}</template>
    </PageHeader>
    <!-- RFC 0044: the admin pages that follow scans and findings across
         registries, on the same verbs as their router guard. -->
    <p
      v-if="can('quarantine:read') || can('findings:read')"
      class="flex flex-wrap gap-x-4 gap-y-1 text-sm"
      data-testid="security-admin-links"
    >
      <RouterLink
        v-if="can('quarantine:read')"
        to="/admin/security/scans"
        class="text-primary underline underline-offset-[3px]"
        >{{ t("adminNav.scans") }}</RouterLink
      >
      <RouterLink
        v-if="can('findings:read')"
        to="/admin/security/findings"
        class="text-primary underline underline-offset-[3px]"
        >{{ t("adminNav.findings") }}</RouterLink
      >
    </p>
    <HoldsPanel
      v-if="can('quarantine:read')"
      :can-open-findings="canFindings"
      :findings-for="findingsFor"
      @open-findings="findingsFor = $event"
    />
    <FindingsPanel
      v-if="canFindings && findingsFor"
      :registry="findingsFor"
      @close="findingsFor = null"
    />
    <FlagsPanel v-if="can('flags:read')" />
    <ExposurePanel v-if="can('audit:read')" />
  </div>
</template>
