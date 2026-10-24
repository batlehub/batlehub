<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

/**
 * The heading level is a prop because it is a property of where the card sits,
 * not of what a card is. It was hardcoded `h3`, so every page whose title is an
 * `h1` and whose first card follows it skipped `h2` — `/tools` renders
 * "Tools" then "Access Check" with nothing between them, which is a broken
 * document outline for anyone navigating by heading.
 *
 * `h2` is the default because directly under the page `h1` is where cards
 * mostly are; pass `as="h3"` for one nested under a section heading.
 *
 * `h1` is allowed for the one shape where a card *is* the page: `/login` is a
 * single centred card and nothing else, so its title had to be the document's
 * only heading — and was an `h2`, leaving the page with no `h1` at all. The
 * ramp gate found it the moment RFC 0004-bis §4.4 pointed it at public routes.
 *
 * The size is the Head step (`text-lg`, 20px, regular). It was `text-2xl`
 * bold — 24px, the Pixel Medium step DESIGN reserves for the wordmark, set in
 * the mono face where no 24px step exists — so every card title competed with
 * the page `h1` above it.
 */
const props = withDefaults(
  defineProps<{ class?: HTMLAttributes["class"]; as?: "h1" | "h2" | "h3" | "h4" }>(),
  { as: "h2" },
);
</script>

<template>
  <component :is="as" :class="cn('font-mono text-lg font-normal leading-snug', props.class)">
    <slot />
  </component>
</template>
