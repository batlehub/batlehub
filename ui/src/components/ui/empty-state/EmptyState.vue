<script setup lang="ts">
/**
 * Empty is two different states wearing one face, and conflating them is the
 * defect this component exists to prevent:
 *
 *   - nothing has ever been here  → say what would put something here
 *   - your filter matched nothing → say so, and offer to clear it
 *
 * A user who is told "no packages" while a filter is silently applied concludes
 * the registry is broken. `filtered` is therefore required rather than inferred.
 *
 * It is also DESIGN.md's *panel*, the one bounded box for empty, loading and
 * error states: a Pixel Small heading, copy capped at 64ch, and an `error`
 * variant that turns the frame and the heading crimson and nothing else.
 */
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(
  defineProps<{
    /** A short heading, set in the bitmap face. Leave it out when all there is
        to say is one sentence: Silkscreen is for a label, never for prose. */
    title?: string;
    description?: string;
    filtered?: boolean;
    variant?: "default" | "error";
    class?: HTMLAttributes["class"];
  }>(),
  { title: "", description: "", filtered: false, variant: "default" },
);
</script>

<template>
  <div
    :class="
      cn(
        'border px-6 py-10 text-center',
        variant === 'error' ? 'border-primary' : 'border-border',
        props.class,
      )
    "
    data-testid="empty-state"
    :data-filtered="filtered ? 'true' : 'false'"
  >
    <p
      v-if="title"
      :class="
        cn(
          'font-display text-sub tracking-[0.04em]',
          variant === 'error' ? 'text-primary' : 'text-foreground',
        )
      "
    >
      {{ title }}
    </p>
    <p
      v-if="description || $slots.description"
      :class="
        cn(
          'mx-auto max-w-[64ch] text-sm text-muted-foreground [overflow-wrap:anywhere]',
          title && 'mt-2',
        )
      "
    >
      <slot name="description">{{ description }}</slot>
    </p>
    <div v-if="$slots.action" class="mt-4 flex justify-center gap-2">
      <slot name="action" />
    </div>
  </div>
</template>
