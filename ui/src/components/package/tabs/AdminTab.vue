<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { toRef } from "vue";

import { packageDetail } from "@/client/sdk.gen";
import type { PackageDetailResponse } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import PackageVersionsTable from "@/components/admin/PackageVersionsTable.vue";
import PackageBetaChannel from "@/components/admin/PackageBetaChannel.vue";
import PackageVisibility from "@/components/admin/PackageVisibility.vue";
import PackageGrants from "@/components/admin/PackageGrants.vue";
import PackageEventsTable from "@/components/admin/PackageEventsTable.vue";

/**
 * The package page's Administration tab (RFC 0044 §6.4): everything
 * AdminPackageDetail used to be, moved unchanged. The page renders it only for
 * an administrator, so it loads only for one.
 */
const props = defineProps<{ registry: string; name: string }>();

const { t } = useI18n();
const { token } = useAuth();
const registry = toRef(props, "registry");
const name = toRef(props, "name");

const {
  data: adminData,
  error: adminError,
  reload: reloadAdmin,
} = useApi<PackageDetailResponse>(
  () =>
    packageDetail({
      query: { registry: registry.value, name: name.value },
    }) as Promise<{ data?: unknown; error?: unknown }>,
  [token, registry, name],
);
</script>

<template>
  <section class="space-y-4" aria-labelledby="admin-heading">
    <h2
      id="admin-heading"
      class="font-mono text-sm font-semibold uppercase tracking-wider text-muted-foreground"
    >
      {{ t("common.administration") }}
    </h2>

    <p v-if="adminError" class="text-sm text-destructive">{{ adminError }}</p>

    <template v-else-if="adminData">
      <PackageVersionsTable
        :registry="registry"
        :name="name"
        :versions="adminData.versions"
        @reload="reloadAdmin"
      />
      <PackageBetaChannel :registry="registry" />
      <PackageVisibility :registry="registry" :name="name" />
      <PackageGrants :registry="registry" :name="name" />
      <PackageEventsTable :events="adminData.recent_events" />
    </template>
  </section>
</template>
