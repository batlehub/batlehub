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
  // A long line scrolls the `<pre>` sideways, and a keyboard reaches that only
  // if the region itself takes focus (axe `scrollable-region-focusable`).
  if (h) html.value = h.replace("<pre ", '<pre tabindex="0" ');
}

watch([() => props.code, () => props.lang, ready], update, { immediate: true });
</script>

<template>
  <!-- Below `sm` the button takes its own row above the code: floated over a
       phone-width block it sat on the end of the first line. -->
  <div class="relative flex flex-col">
    <div v-if="html" class="shiki-wrapper" v-html="html" />
    <pre
      v-else
      tabindex="0"
      class="bg-muted rounded-md p-4 text-xs font-mono overflow-x-auto leading-relaxed"
      >{{ code }}</pre>
    <CopyButton
      v-if="copy !== undefined"
      :text="copy"
      class="order-first mb-1 h-7 self-end px-2 text-xs sm:absolute sm:top-2 sm:right-2 sm:mb-0"
    />
    <slot />
  </div>
</template>
