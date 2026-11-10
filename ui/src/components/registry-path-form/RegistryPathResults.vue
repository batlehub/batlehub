<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { Badge } from "@/components/ui/badge";
import { CopyButton } from "@/components/ui/copy-button";
import type { ProxyPath } from "@/config/registryPathFields";

const { t } = useI18n();

const props = defineProps<{
  paths: ProxyPath[];
  baseUrl: string;
}>();

function fullUrl(path: string): string {
  return `${props.baseUrl}${path}`;
}
</script>

<template>
  <div v-if="paths.length" class="space-y-2">
    <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
      {{ t("registryPathResults.proxyPaths") }}
    </h2>
    <div class="rounded-sm border divide-y">
      <div
        v-for="entry in paths"
        :key="entry.url"
        class="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-3"
        :class="entry.available ? '' : 'text-muted-foreground'"
      >
        <span class="w-full shrink-0 text-xs text-muted-foreground sm:w-44">{{ entry.label }}</span>
        <!-- Wrapped, not truncated: on a phone the URL is the datum. -->
        <code class="min-w-0 flex-1 text-xs font-mono [overflow-wrap:anywhere]">
          {{ fullUrl(entry.url) }}
        </code>
        <CopyButton
          v-if="entry.available"
          :text="fullUrl(entry.url)"
          class="shrink-0 h-7 px-2 text-xs"
        />
        <Badge v-else variant="outline" class="shrink-0 text-xs">{{
          t("registryPathResults.needsMoreFields")
        }}</Badge>
      </div>
    </div>
  </div>

  <p v-else class="text-sm text-muted-foreground text-center py-4">
    {{ t("registryPathResults.fillInTheFields") }}
  </p>
</template>
