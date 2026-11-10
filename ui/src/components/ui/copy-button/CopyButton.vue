<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Button } from "@/components/ui/button";

type ButtonVariant = "default" | "destructive" | "outline" | "ghost";
type ButtonSize = "default" | "sm" | "icon";

const props = withDefaults(
  defineProps<{
    text: string;
    label?: string;
    copiedLabel?: string;
    size?: ButtonSize;
    variant?: ButtonVariant;
    resetMs?: number;
  }>(),
  {
    // Left undefined so the catalogue supplies the default: a literal here
    // would be evaluated once, in English, and never follow a locale change.
    label: undefined,
    copiedLabel: undefined,
    size: "sm",
    variant: "ghost",
    resetMs: 2000,
  },
);

const { t } = useI18n();

const resolvedLabel = computed(() => props.label ?? t("common.copy"));
const resolvedCopiedLabel = computed(() => props.copiedLabel ?? t("common.copied"));

const emit = defineEmits<{ copied: [] }>();

const copied = ref(false);

async function copy() {
  try {
    await navigator.clipboard.writeText(props.text);
  } catch {
    // Refused clipboard (no permission, plain-HTTP homelab): the text is on
    // screen and selectable, so stay silent rather than reject unhandled.
    return;
  }
  copied.value = true;
  emit("copied");
  setTimeout(() => {
    copied.value = false;
  }, props.resetMs);
}

defineOptions({ inheritAttrs: false });
</script>

<template>
  <!-- `aria-label` before `$attrs`, so a caller can still override it. In icon
       size the visible label is an icon, which leaves the control unnamed;
       naming it here means no caller can forget. The name also flips to the
       copied label, which assistive tech announces for the focused element —
       a separate live region here would just say the same word twice. -->
  <Button
    :variant="variant"
    :size="size"
    :aria-label="copied ? resolvedCopiedLabel : resolvedLabel"
    v-bind="$attrs"
    @click="copy"
  >
    <slot :copied="copied">{{ copied ? resolvedCopiedLabel : resolvedLabel }}</slot>
  </Button>
</template>
