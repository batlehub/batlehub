<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute } from "vue-router";
import type { ExploreVersionDto, FetchOfferDto } from "@/client/types.gen";
import { useAuth } from "@/composables/useAuth";
import { isUpstreamOnly } from "@/composables/usePackageVersions";
import { Button } from "@/components/ui/button";

/** Fetch-this-version for a version this instance holds no bytes of. */
defineProps<{
  selectedRow: ExploreVersionDto;
  offer: FetchOfferDto;
  fetching: string | null;
  fetchResult: Record<string, string>;
}>();

const emit = defineEmits<{ "fetch-version": [version: string] }>();

const { t } = useI18n();
const { isAuthenticated } = useAuth();
const route = useRoute();
</script>

<template>
  <!-- The one filled action on the reader's half of the page, and the only
       act here that spends this instance's bandwidth and writes an audit
       row. It was a 24px outline chip in the second column of a table. -->
  <div v-if="isUpstreamOnly(selectedRow.source)" class="mt-6">
    <p class="mb-3 max-w-[64ch] text-sm text-muted-foreground">
      {{ t("packageDetailPage.notHeldHereHelp") }}
    </p>
    <Button
      v-if="offer.offered"
      :disabled="fetching !== null"
      @click="emit('fetch-version', selectedRow!.version)"
    >
      {{
        fetching === selectedRow.version
          ? t("packageDetailPage.fetching")
          : t("packageDetailPage.fetchVersion")
      }}
    </Button>
    <p v-else-if="offer.reason" class="text-sm text-muted-foreground">
      {{ t("packageDetailPage.fetchUnavailable", { reason: offer.reason }) }}
    </p>
    <RouterLink
      v-else-if="!isAuthenticated"
      :to="{ path: '/login', query: { redirect: route.fullPath } }"
      class="text-sm text-primary underline underline-offset-[3px]"
      >{{ t("packageDetailPage.fetchNeedsSession") }}</RouterLink
    >
    <p v-if="fetchResult[selectedRow.version]" class="mt-2 text-sm text-muted-foreground">
      {{ fetchResult[selectedRow.version] }}
    </p>
    <p class="mt-2 max-w-[64ch] text-xs text-muted-foreground">
      {{ t("packageDetailPage.fetchWhatItDoes") }}
    </p>
  </div>
</template>
