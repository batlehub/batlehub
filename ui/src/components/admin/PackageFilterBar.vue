<script setup lang="ts">
import { useI18n } from "vue-i18n";

import type { RegistryInfo } from "@/client/types.gen";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";

/**
 * The package list's filters: a text match applied in the browser, and the
 * registry and "blocked only" filters the page sends to the server.
 */
defineProps<{ registries: RegistryInfo[] | null }>();
const search = defineModel<string>("search", { required: true });
const registryFilter = defineModel<string>("registryFilter", { required: true });
const blockedOnly = defineModel<boolean>("blockedOnly", { required: true });

const { t } = useI18n();
</script>

<template>
  <div class="flex flex-wrap items-center gap-2">
    <Input
      v-model="search"
      :placeholder="t('adminPackages.filterByNameRegistry')"
      :aria-label="t('adminPackages.filterPackages')"
      class="max-w-sm h-8 text-sm"
    />
    <!-- Both go to the server. `blocked_only` answers half this page's
         question and had no control at all. -->
    <Label for="ap-registry" class="sr-only">{{ t("common.registry") }}</Label>
    <Select
      id="ap-registry"
      v-model="registryFilter"
      :options="[
        { value: '', label: t('adminPackages.allRegistries') },
        ...(registries ?? []).map((r) => ({ value: r.name, label: r.name })),
      ]"
      class="h-8 w-auto"
    />
    <div class="flex items-center gap-2 h-8">
      <Switch id="ap-blocked" v-model="blockedOnly" />
      <Label for="ap-blocked" class="text-sm">{{ t("adminPackages.blockedOnly") }}</Label>
    </div>
  </div>
</template>
