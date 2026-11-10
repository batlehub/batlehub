<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { useAuth } from "@/composables/useAuth";
import { PageHeader } from "@/components/ui/page-header";
import HoldsPanel from "@/components/admin/HoldsPanel.vue";
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
</script>

<template>
  <div class="space-y-6">
    <PageHeader variant="display">
      <template #title>{{ t("securityDashboard.title") }}</template>
      <template #description>{{ t("securityDashboard.description") }}</template>
    </PageHeader>
    <HoldsPanel v-if="can('quarantine:read')" />
    <FlagsPanel v-if="can('flags:read')" />
    <ExposurePanel v-if="can('audit:read')" />
  </div>
</template>
