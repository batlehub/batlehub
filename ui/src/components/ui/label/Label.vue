<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { Label as LabelPrimitive, type LabelProps } from "radix-vue";
import { cn } from "@/lib/utils";

const props = defineProps<
  LabelProps & {
    class?: HTMLAttributes["class"];
  }
>();
</script>

<template>
  <LabelPrimitive
    v-bind="props"
    :class="
      cn(
        'font-sans text-sm font-medium text-foreground leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70',
        props.class,
      )
    "
  >
    <slot />
  </LabelPrimitive>
</template>
