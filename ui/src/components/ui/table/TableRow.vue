<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<{ class?: HTMLAttributes["class"] }>();
</script>

<template>
  <tr
    :class="
      cn(
        // DESIGN.md: dashed at rest, solid when engaged. The hover fill alone is
        // 1.06:1, so the divider weight is what carries the state.
        'border-b border-dashed border-rule-soft hover:border-solid hover:border-border hover:bg-accent data-[state=selected]:bg-accent',
        props.class,
      )
    "
  >
    <slot />
  </tr>
</template>
