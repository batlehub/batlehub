<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

/**
 * A region of the page, drawn the way DESIGN.md draws every region: a
 * full-width hairline above it and nothing else. The name is the shadcn one
 * because 34 files import it; the shape is not. It was `border bg-card` — a
 * four-sided frame on `--ground-raised`, which is the inset container the
 * No-Container Rule refuses by name, and which turned every admin page into a
 * stack of boxes while the public pages were cut as ruled rows. Changing it
 * here is what moves all of them at once.
 */
const props = defineProps<{ class?: HTMLAttributes["class"] }>();
</script>

<template>
  <div :class="cn('border-t border-rule-soft text-foreground', props.class)">
    <slot />
  </div>
</template>
