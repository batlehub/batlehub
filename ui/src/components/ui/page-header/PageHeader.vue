<script setup lang="ts">
import { cn } from "@/lib/utils";

withDefaults(
  defineProps<{
    title?: string;
    description?: string;
    variant?: "default" | "display" | "poster";
    /** `h2` for a page rendered inside a hub, whose layout already owns the `h1`. */
    as?: "h1" | "h2";
    /** A count after the title, in parentheses; nothing when 0 or absent. */
    count?: number;
  }>(),
  { variant: "default", as: "h1" },
);
</script>

<template>
  <!--
    Wraps, and the title block may shrink.

    The actions row is `shrink-0` — correct, because a button that shrinks
    below its label is worse than one that moves — but with a non-wrapping
    parent and no `min-w-0` on the title, a header carrying two or three
    actions pushed the document past the viewport on a phone. Four admin pages
    scrolled sideways for this reason alone, after the layout row above them
    was fixed.
  -->
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div class="min-w-0">
      <component
        :is="as"
        :class="
          cn(
            'text-2xl font-semibold flex items-center gap-2',
            // The bitmap face at Pixel Medium — one per view, and the only step
            // above the data ramp. It carries no glow: depth in this world is
            // inked, not lit. Tracked 0.04em because Silkscreen's square pixel
            // sets tight at its own em.
            variant === 'display' && 'font-display font-bold tracking-[0.04em]',
            // The page-title step (`--t-display`), the same one the home
            // greeting and the two package surfaces set. It overrides
            // `text-2xl` above, so it comes after it.
            variant === 'poster' &&
              'font-display font-bold tracking-[0.02em] leading-[0.92] text-display break-words',
          )
        "
      >
        <slot name="title">{{ title }}</slot>
        <span v-if="count" class="font-mono text-base font-normal text-muted-foreground"
          >({{ count }})</span
        >
      </component>
      <p
        v-if="description || $slots.description"
        :class="
          cn('text-sm text-muted-foreground max-w-[64ch]', variant === 'poster' ? 'mt-3' : 'mt-1')
        "
      >
        <slot name="description">{{ description }}</slot>
      </p>
    </div>
    <!-- `max-w-full` + `flex-wrap`: two actions wider than a phone (one
         registry's Edit configuration + Monitoring, at 390px) wrap onto a
         second row instead of pushing the document 14px sideways. -->
    <div v-if="$slots.actions" class="flex max-w-full flex-wrap items-center gap-2 shrink-0">
      <slot name="actions" />
    </div>
  </div>
</template>
