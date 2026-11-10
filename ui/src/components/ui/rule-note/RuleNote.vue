<script setup lang="ts">
/**
 * A note tied to the line above it by a 1px rule: the refusal and snippet notes
 * under a catalogue row, and the reason / deprecation notes of a version.
 * Attributes (`id`, a margin) land on the root; `as="li"` for a list of them.
 */
withDefaults(defineProps<{ as?: string }>(), { as: "p" });
</script>

<template>
  <component :is="as" class="flex items-stretch gap-3 text-sm text-muted-foreground">
    <span class="w-px flex-none bg-border" aria-hidden="true" />
    <span class="min-w-0 [overflow-wrap:anywhere]"><slot /></span>
  </component>
</template>
