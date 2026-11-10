<script setup lang="ts">
import { ref, watch } from "vue";
import { useShiki } from "@/composables/useShiki";
import { CopyButton } from "@/components/ui/copy-button";

/** `copy` is what the copy button copies (it may differ from what is shown, e.g. a
 *  snippet displayed with a masked token); no button without it. */
const props = defineProps<{ code: string; lang: string; copy?: string }>();

const { highlight, ready } = useShiki();
const html = ref("");

function update() {
  const h = highlight(props.code, props.lang);
  if (h) html.value = h;
}

watch([() => props.code, () => props.lang, ready], update, { immediate: true });
</script>

<template>
  <div class="relative">
    <div v-if="html" class="shiki-wrapper" v-html="html" />
    <pre v-else class="bg-muted rounded-md p-4 text-xs font-mono overflow-x-auto leading-relaxed">{{
      code
    }}</pre>
    <CopyButton
      v-if="copy !== undefined"
      :text="copy"
      class="absolute top-2 right-2 h-7 px-2 text-xs"
    />
    <slot />
  </div>
</template>
