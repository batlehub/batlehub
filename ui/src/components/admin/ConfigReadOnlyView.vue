<script setup lang="ts">
import { useI18n } from "vue-i18n";
/**
 * The read-only configuration screen.
 *
 * When the config is mounted read-only — a Kubernetes ConfigMap, most often —
 * editing it here is not merely disabled, it is *not a thing that exists*. The
 * previous shape was a yellow banner above a textarea that still looked like a
 * working editor, with Validate and Apply beneath it: an operator could type a
 * change, watch it validate, and only then discover nothing would be saved.
 *
 * So this is a different screen, not a disabled one. It shows what is running
 * and says where to change it (RFC 0003 §6.4).
 */
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { CodeBlock } from "@/components/ui/code-block";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { CopyButton } from "@/components/ui/copy-button";
import { docsUrl } from "@/config";
import { locateRegistry } from "@/lib/configLocate";

const { t, locale } = useI18n();

const props = defineProps<{ content: string; error?: string | null; jumpTo?: string | null }>();

/**
 * RFC 0046 M5, read-only mount: there is no caret, so the registry's line is
 * scrolled into view and highlighted for two seconds instead.
 */
const located = computed(() =>
  props.jumpTo && !props.error ? locateRegistry(props.content, props.jumpTo) : null,
);
const root = ref<HTMLElement | null>(null);
let timer: ReturnType<typeof setTimeout> | undefined;

/**
 * shortcut: the highlighted code renders asynchronously, so the line is
 * polled for (20 × 100 ms) rather than observed; upgrade to an event from
 * `CodeBlock` if a second caller needs it.
 */
function highlight(attempt = 0) {
  const at = located.value;
  if (!at) return;
  const line = root.value?.querySelectorAll<HTMLElement>(".line")[at.line];
  if (!line) {
    if (attempt < 20) timer = setTimeout(() => highlight(attempt + 1), 100);
    return;
  }
  line.scrollIntoView?.({ block: "center" });
  line.classList.add("outline", "outline-1", "outline-primary");
  timer = setTimeout(() => line.classList.remove("outline", "outline-1", "outline-primary"), 2000);
}

// `jumpTo` can arrive after mount: the page hands it down once it has settled.
watch(located, () => highlight(), { immediate: true, flush: "post" });
onBeforeUnmount(() => clearTimeout(timer));
</script>

<template>
  <Card>
    <CardHeader class="flex-row items-center justify-between gap-4 space-y-0">
      <CardTitle>{{ t("config.readOnlyTitle") }}</CardTitle>
      <CopyButton :text="content" size="sm" variant="outline" :label="t('config.copyToml')" />
    </CardHeader>
    <CardContent class="space-y-3">
      <p class="text-sm text-muted-foreground">
        {{ t("config.readOnlyBody") }}
      </p>
      <p
        v-if="jumpTo && !error"
        role="status"
        class="text-sm text-muted-foreground"
        data-testid="jump-notice"
      >
        <template v-if="located">{{ t("adminConfigReload.jumpedTo", { name: jumpTo }) }}</template>
        <i18n-t v-else keypath="adminConfigReload.jumpOtherLayer" tag="span">
          <template #name>{{ jumpTo }}</template>
          <template #link>
            <a
              class="underline underline-offset-2 hover:text-foreground"
              :href="docsUrl('guide/configuration#layered-config-files', locale)"
              target="_blank"
              rel="noopener"
              >{{ t("adminConfigReload.jumpOtherLayerLink") }}</a
            >
          </template>
        </i18n-t>
      </p>
      <p v-if="error" class="text-sm text-destructive">{{ error }}</p>
      <div v-else ref="root"><CodeBlock :code="content" lang="toml" /></div>
    </CardContent>
  </Card>
</template>
