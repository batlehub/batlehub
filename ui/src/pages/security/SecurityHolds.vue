<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { PageHeader } from "@/components/ui/page-header";
import HoldsPanel from "@/components/admin/HoldsPanel.vue";
import HeldVersions from "@/components/admin/HeldVersions.vue";
import { useAuth } from "@/composables/useAuth";

/** RFC 0050 §6.6: the holds panel of the former one-page console, as its own page. */
const { t } = useI18n();
const { can } = useAuth();
</script>

<template>
  <div class="space-y-6">
    <PageHeader :title="t('securityNav.holds')" :description="t('holdsPanel.intro')" />
    <HoldsPanel :can-open-findings="can('findings:read')" />
    <HeldVersions />
  </div>
</template>
