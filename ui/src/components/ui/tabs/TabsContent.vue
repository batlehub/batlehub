<script setup lang="ts">
import { TabsContent, type TabsContentProps } from "radix-vue";
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<TabsContentProps & { class?: HTMLAttributes["class"] }>();
</script>

<template>
  <TabsContent v-bind="props" :class="cn('mt-4 ring-offset-background', props.class)">
    <slot />
  </TabsContent>
</template>
