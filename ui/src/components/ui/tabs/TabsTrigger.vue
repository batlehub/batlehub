<script setup lang="ts">
import { TabsTrigger, type TabsTriggerProps } from "radix-vue";
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<TabsTriggerProps & { class?: HTMLAttributes["class"] }>();
</script>

<template>
  <TabsTrigger
    v-bind="props"
    :class="
      cn(
        // DESIGN's choice-set grammar: the chosen cell reversed to an ink block,
        // the others ink-dim on a 1px rule. It was a raised fill with a crimson
        // edge at 30% alpha, and `transition-all` animated the switch.
        'inline-flex items-center justify-center whitespace-nowrap border border-border px-3 py-1.5 text-sm font-mono text-muted-foreground hover:text-foreground disabled:pointer-events-none disabled:opacity-50 data-[state=active]:bg-foreground data-[state=active]:text-background data-[state=active]:border-foreground',
        props.class,
      )
    "
  >
    <slot />
  </TabsTrigger>
</template>
