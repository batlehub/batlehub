<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

/** No side padding: a region runs the full measure, like the rule above it. */
const props = defineProps<{ class?: HTMLAttributes["class"] }>();
</script>

<template>
  <div :class="cn('flex flex-col gap-1 pt-4 pb-3', props.class)">
    <slot />
  </div>
</template>
