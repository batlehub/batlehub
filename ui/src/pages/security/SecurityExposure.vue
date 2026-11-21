<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { PageHeader } from "@/components/ui/page-header";
import ExposurePanel from "@/components/admin/ExposurePanel.vue";

/** RFC 0050 §6.6: the exposure panel of the former one-page console, as its own page. */
const { t } = useI18n();
</script>

<template>
  <div class="space-y-6">
    <PageHeader :title="t('securityNav.exposure')" :description="t('exposurePanel.intro')" />
    <ExposurePanel />
  </div>
</template>
