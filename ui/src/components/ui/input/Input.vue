<script setup lang="ts">
import { type HTMLAttributes, computed, useAttrs } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<{
  class?: HTMLAttributes["class"];
  type?: string;
  placeholder?: string;
  disabled?: boolean;
  modelValue?: string | number;
  /**
   * Declared rather than left to attribute fallthrough, because the accessible
   * name below has to *read* it: an `id` is what a sibling `<label for>` binds
   * to, and a field that has one is already named.
   */
  id?: string;
  /** The id of an element that names this field, when it is not a `<label>`. */
  ariaLabelledby?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

const attrs = useAttrs();

const delegatedProps = computed(() => {
  const {
    class: _class,
    modelValue: _modelValue,
    // Spread as-is this would render `arialabelledby`, which names nothing.
    ariaLabelledby: _ariaLabelledby,
    ...rest
  } = props;
  return rest;
});

/**
 * The placeholder as a *last-resort* accessible name.
 *
 * This was bound unconditionally, and `aria-label` outranks `<label for>` in
 * the accessible name computation — so every labelled field in the console
 * announced its placeholder instead of its label. The bulk page's reason field
 * announced "CVE-2025-XXXX or policy violation"; the token page's name field
 * announced "e.g. CI pipeline". Both in English, to a French screen-reader
 * user, over a label that was translated and correct.
 *
 * A placeholder is still better than no name at all, so it stays for a field
 * that has neither a label, a `labelledby`, nor an `aria-label` of its own —
 * but it now yields to every one of them.
 *
 * WCAG 2.5.3 Label in Name (A), 3.3.2 Labels or Instructions (A).
 */
const fallbackLabel = computed(() => {
  const alreadyNamed =
    props.id !== undefined ||
    props.ariaLabelledby !== undefined ||
    attrs["aria-labelledby"] !== undefined ||
    attrs["aria-label"] !== undefined;
  return alreadyNamed ? undefined : props.placeholder;
});
</script>

<template>
  <input
    v-bind="delegatedProps"
    :aria-label="fallbackLabel"
    :aria-labelledby="ariaLabelledby"
    :value="modelValue"
    :class="
      cn(
        'flex h-9 w-full border border-border bg-ground-sunk px-3 py-2 font-mono text-sm focus-visible:border-primary placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50 file:border-0 file:bg-transparent file:text-sm file:font-medium',
        props.class,
      )
    "
    @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
  />
</template>
