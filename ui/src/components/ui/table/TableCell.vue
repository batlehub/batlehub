<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<{
  class?: HTMLAttributes["class"];
  /**
   * The column's name, printed before the value only below `md`, where a
   * `stacked` table hides its header (RFC 0050 §6.7). Hidden from assistive
   * technology, which still reads the header.
   */
  label?: string;
}>();
</script>

<template>
  <td :class="cn('p-4 align-middle [&:has([role=checkbox])]:pr-0', props.class)">
    <span v-if="label" class="text-muted-foreground md:hidden" aria-hidden="true"
      >{{ label }}&nbsp;</span
    ><slot />
  </td>
</template>
