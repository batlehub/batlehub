<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { PageHeader } from "@/components/ui/page-header";
import FlagsPanel from "@/components/admin/FlagsPanel.vue";

/** RFC 0050 §6.6: the flags panel of the former one-page console, as its own page. */
const { t } = useI18n();
</script>

<template>
  <div class="space-y-6">
    <PageHeader :title="t('securityNav.flags')" :description="t('flagsPanel.intro')" />
    <FlagsPanel />
  </div>
</template>
