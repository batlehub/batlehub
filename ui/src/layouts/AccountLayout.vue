<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { useAuth } from "@/composables/useAuth";
import { accountTabs } from "@/config/navigation";
import HubLayout from "./HubLayout.vue";

const { t } = useI18n();
const { isOidcSession } = useAuth();

/* Tokens are OIDC-only. A static-token user never sees the tab rather than
   seeing it and bouncing off the guard. */
const tabs = computed(() => accountTabs({ isOidc: isOidcSession.value }));
</script>

<template>
  <HubLayout :title="t('account.title')" :description="t('account.description')" :tabs="tabs" />
</template>
