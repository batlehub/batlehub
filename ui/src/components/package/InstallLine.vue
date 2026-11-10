<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { Check, Copy } from "@lucide/vue";
import { installCommandFor } from "@/config/registryTypes";

const props = defineProps<{
  registryType: string | null;
  name: string;
  version: string | null;
}>();

const { t } = useI18n();

/** The one line that installs the selected version, when its type has an honest one. */
const installLine = computed(() =>
  props.version ? installCommandFor(props.registryType, props.name, props.version) : null,
);

/**
 * Copy confirmed in place, and reset.
 *
 * A copy that says nothing leaves the reader wondering whether it took; a copy
 * that says so forever stops being feedback. Two seconds, and no motion — the
 * system animates exactly one thing and this is not it.
 */
const copied = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | null = null;

async function copyInstall() {
  if (!installLine.value) return;
  try {
    await navigator.clipboard.writeText(installLine.value.command);
    copied.value = true;
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => {
      copied.value = false;
      copyTimer = null;
    }, 2000);
  } catch {
    // A clipboard the browser refuses (no permission, no secure context) is not
    // an error worth a banner: the command is on screen and selectable, which is
    // the fallback every terminal user already uses.
  }
}

onBeforeUnmount(() => {
  if (copyTimer) clearTimeout(copyTimer);
});
</script>

<template>
  <!-- The end of the journey, which the page did not have: a reader who
       found their version had nowhere to go and the page stopped on an em
       dash. The command is data (`installCommandFor`), not a switch in
       this component, and a type with no honest one-liner says so rather
       than printing a plausible wrong one. -->
  <div v-if="installLine" class="mt-6">
    <div class="flex items-stretch border border-border">
      <pre
        class="min-w-0 flex-1 overflow-x-auto bg-ground-sunk px-3 py-3 font-mono text-sm text-foreground"
      ><code>{{ installLine.command }}</code></pre>
      <button
        type="button"
        class="flex shrink-0 items-center gap-2 border-l border-border px-3 text-xs uppercase tracking-[0.1em] text-muted-foreground hover:text-foreground"
        @click="copyInstall"
      >
        <component :is="copied ? Check : Copy" class="h-4 w-4" aria-hidden="true" />
        {{ copied ? t("common.copied") : t("common.copy") }}
      </button>
    </div>
    <p class="mt-2 text-xs text-muted-foreground">
      {{ t("packageDetailPage.installAssumesSetup") }}
      <RouterLink class="text-primary underline underline-offset-[3px]" to="/setup">{{
        t("packageDetailPage.setupGuide")
      }}</RouterLink>
    </p>
  </div>
</template>
