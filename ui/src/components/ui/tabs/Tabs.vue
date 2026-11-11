<script setup lang="ts">
import { TabsRoot, type TabsRootEmits, type TabsRootProps } from "radix-vue";
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<TabsRootProps & { class?: HTMLAttributes["class"] }>();
// Forwarded by hand: `v-model` on this wrapper did not reach the root.
const emit = defineEmits<TabsRootEmits>();
</script>

<template>
  <TabsRoot
    v-bind="props"
    :class="cn('', props.class)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <slot />
  </TabsRoot>
</template>
