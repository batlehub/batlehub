<script setup lang="ts">
import { cn } from "@/lib/utils";

withDefaults(
  defineProps<{
    title?: string;
    description?: string;
    variant?: "default" | "poster";
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
  <!-- One header height across the console: room for the title and two lines
       of description, reserved even when a page says less, so content starts
       at the same place on every page and the plate is never a thin band. -->
  <div
    :class="
      cn(
        'relative flex flex-wrap items-start justify-between gap-4',
        variant === 'poster' ? 'min-h-22' : 'min-h-19',
      )
    "
  >
    <!-- The halftone plate behind a page title (DESIGN.md, The material layer).
         Only on a page's own h1 — a hub's h2 sits under a header that has one —
         and only from xl: `plate--header` is narrower than the catalog's, so the
         description (64ch, in dim ink that would fall to 3.2:1 on the plate)
         ends before the plate begins. Below xl the column is too narrow for that. -->
    <div v-if="as === 'h1'" class="plate plate--header hidden xl:block" aria-hidden="true"></div>
    <div class="relative z-10 min-w-0">
      <component
        :is="as"
        :class="
          cn(
            'text-2xl font-semibold tracking-tight flex items-center gap-2',
            // Page titles are set in the interface face (DESIGN.md, The Interface Face Rule): the
            // bitmap face is the wordmark's and the home greeting's, and a
            // console title in it read as a poster before it read as a page.
            // `poster` keeps its larger step for the few full-page surfaces.
            variant === 'poster' && 'text-title leading-tight break-words',
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
    <div
      v-if="$slots.actions"
      class="relative z-10 flex max-w-full flex-wrap items-center gap-2 shrink-0"
    >
      <slot name="actions" />
    </div>
  </div>
</template>
