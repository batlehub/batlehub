<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RefreshCw } from "@lucide/vue";
import { Button } from "@/components/ui/button";
import { SearchInput } from "@/components/ui/search-input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";

defineProps<{
  search: string;
  searchIn: string;
  sort: string;
  readmeSearchEnabled: boolean;
}>();

const emit = defineEmits<{
  search: [value: string];
  searchIn: [value: string];
  sort: [value: string];
  refresh: [];
}>();

const { t } = useI18n();
</script>

<template>
  <div class="flex gap-2 flex-wrap">
    <SearchInput
      :placeholder="t('packageCatalog.searchPackages')"
      :aria-label="t('packageCatalog.searchPackages2')"
      :value="search"
      @input="emit('search', ($event.target as HTMLInputElement).value)"
    />
    <!-- Where the search looks. Beside the box it modifies rather than in
       a settings panel, because it changes what the *next keystroke*
       means. Hidden entirely when the instance searches names only: a
       control whose other options do nothing is worse than no control
       (RFC 0007-bis §4.3). -->
    <!-- The label is visually hidden but `for`-bound: the `Select`
         trigger is a button, and an `aria-label` on the component lands
         on radix's hidden native select instead of on it. -->
    <template v-if="readmeSearchEnabled">
      <Label for="catalog-search-in" class="sr-only">{{ t("packageCatalog.searchIn") }}</Label>
      <Select
        id="catalog-search-in"
        class="w-auto gap-2"
        :model-value="searchIn"
        :options="[
          { value: 'name', label: t('packageCatalog.searchInName') },
          { value: 'readme', label: t('packageCatalog.searchInReadme') },
          { value: 'both', label: t('packageCatalog.searchInBoth') },
        ]"
        @update:model-value="emit('searchIn', $event)"
      />
    </template>
    <!-- The page's one action, in the toolbar beside search — where
       `design-proof/index.html` puts its own (RFC 0004-bis §14.9). It
       used to sit in `PageHeader`, which the specimen replaced. -->
    <Button variant="outline" size="sm" @click="emit('refresh')">
      <RefreshCw class="h-4 w-4 mr-1" />
      {{ t("common.refresh") }}
    </Button>
    <Label for="catalog-sort" class="sr-only">{{ t("packageCatalog.sortPackages") }}</Label>
    <Select
      id="catalog-sort"
      class="w-auto gap-2"
      :model-value="sort"
      :options="[
        { value: 'fetched', label: t('packageCatalog.lastFetch') },
        { value: 'downloads', label: t('packageCatalog.mostDownloaded') },
        { value: 'name', label: t('packageCatalog.nameAZ') },
        { value: 'recent', label: t('packageCatalog.recentlyAccessed') },
      ]"
      @update:model-value="emit('sort', $event)"
    />
  </div>
</template>
