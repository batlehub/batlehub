<script setup lang="ts">
import { AlertTriangle, Info, XCircle } from "@lucide/vue";
import { useBanner } from "@/composables/useBanner";

const { banner } = useBanner();
</script>

<template>
  <!-- Ruled below, not above: this is now the topmost strip on the page, so the
       hairline it needs is the one separating it from the masthead underneath.
       Edge padding matches the bars it sits with (`px-4 md:px-6`), so the three
       full-bleed rows of the shell share one margin. -->
  <div
    v-if="banner"
    :role="banner.level === 'error' ? 'alert' : 'status'"
    :class="[
      'border-b px-4 md:px-6 py-2 flex items-center gap-2 text-sm font-medium',
      banner.level === 'error'
        ? 'border-destructive/40 text-destructive'
        : banner.level === 'warning'
          ? 'border-copper/40 text-copper'
          : 'border-foreground/40 text-foreground',
    ]"
  >
    <XCircle v-if="banner.level === 'error'" class="h-3.5 w-3.5 shrink-0" />
    <AlertTriangle v-else-if="banner.level === 'warning'" class="h-3.5 w-3.5 shrink-0" />
    <Info v-else class="h-3.5 w-3.5 shrink-0" />
    <span>{{ banner.message }}</span>
  </div>
</template>
