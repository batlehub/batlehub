<script setup lang="ts">
/**
 * A text field with the search glyph inside it, centred on the field rather
 * than offset by hand (the catalogue and the version filter disagreed by 2px).
 * Every attribute and listener goes to the input.
 */
import { Search } from "@lucide/vue";
import { Input } from "@/components/ui/input";

defineOptions({ inheritAttrs: false });
</script>

<template>
  <div class="relative min-w-48 flex-1">
    <Search
      class="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
      aria-hidden="true"
    />
    <Input class="pl-10" v-bind="$attrs" />
  </div>
</template>
