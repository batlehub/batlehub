<script setup lang="ts">
/**
 * The package and version fields of a registry coordinate, with the
 * suggestions both access tools offer for them. The version stays closed,
 * with a stated reason, until a package is named. Renders two grid cells, so
 * it sits inside the caller's form grid next to its registry field.
 */
import { toRef } from "vue";
import { useI18n } from "vue-i18n";
import { Combobox } from "@/components/ui/combobox";
import { usePackageNameSuggestions, useVersionSuggestions } from "@/composables/useSuggestions";

const props = defineProps<{ idPrefix: string; registry: string }>();
const name = defineModel<string>("name", { required: true });
const version = defineModel<string>("version", { required: true });

const { t } = useI18n();
const registry = toRef(props, "registry");
const packageSuggestions = usePackageNameSuggestions(name, registry);
const versionSuggestions = useVersionSuggestions(registry, name);
</script>

<template>
  <div class="space-y-1">
    <label :for="`${idPrefix}-package`" class="text-sm font-medium">{{
      t("adminAccessCheck.packageName")
    }}</label>
    <Combobox
      :id="`${idPrefix}-package`"
      v-model="name"
      :options="packageSuggestions.options.value"
      :loading="packageSuggestions.loading.value"
      placeholder="lodash"
    />
  </div>
  <div class="space-y-1">
    <label :for="`${idPrefix}-version`" class="text-sm font-medium">{{
      t("common.version")
    }}</label>
    <Combobox
      :id="`${idPrefix}-version`"
      v-model="version"
      :options="versionSuggestions.options.value"
      :loading="versionSuggestions.loading.value"
      :disabled="!versionSuggestions.ready()"
      :disabled-reason="t('adminAccessCheck.versionNeedsPackage')"
      placeholder="1.0.0"
    />
  </div>
</template>
