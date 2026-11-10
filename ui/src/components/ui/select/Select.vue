<script setup lang="ts">
import {
  SelectRoot,
  SelectTrigger,
  SelectValue,
  SelectPortal,
  SelectContent,
  SelectViewport,
  SelectItem,
  SelectItemText,
  SelectItemIndicator,
} from "radix-vue";
import { computed } from "vue";
import { Check, ChevronDown } from "@lucide/vue";
import { cn } from "@/lib/utils";

const props = defineProps<{
  modelValue?: string;
  placeholder?: string;
  options: { value: string; label: string }[];
  class?: string;
  id?: string;
  /**
   * Declared so it reaches the trigger. As a fallthrough attribute it landed on
   * radix's hidden native `<select>` (`aria-hidden`), and the combobox a reader
   * actually focuses had no name at all.
   */
  ariaLabel?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

/**
 * An option meaning "no value" — "All registries", "Any severity".
 *
 * radix refuses `<SelectItem value="">`, because the empty string is how a
 * `Select` *clears* itself. But a genuinely optional field needs blank to be
 * something you can choose, not only something you can fail to fill in: RFC
 * 0004-bis §6.2 names the case — a subscription's registry is documented as
 * "leave blank for all registries", and a `Select` that cannot express blank
 * would silently narrow every subscription that relied on it.
 *
 * So the empty string is carried through radix as a sentinel and translated
 * back at the boundary. The sentinel never leaves this component, and a caller
 * that wants an "all" option just passes `{ value: "", label: "All …" }`.
 */
const EMPTY = "\0empty";

const items = computed(() =>
  props.options.map((o) => ({ ...o, value: o.value === "" ? EMPTY : o.value })),
);

const selected = computed(() => (props.modelValue === "" ? EMPTY : props.modelValue));

function onUpdate(value: string) {
  emit("update:modelValue", value === EMPTY ? "" : value);
}
</script>

<template>
  <SelectRoot :model-value="selected" @update:model-value="onUpdate($event as string)">
    <SelectTrigger
      :id="id"
      :aria-label="ariaLabel"
      :class="
        cn(
          'flex h-9 w-full items-center justify-between border border-border bg-ground-sunk px-3 py-2 font-mono text-sm placeholder:text-muted-foreground data-[state=open]:border-primary disabled:cursor-not-allowed disabled:opacity-50',
          $props.class,
        )
      "
    >
      <SelectValue :placeholder="placeholder" />
      <ChevronDown class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
    </SelectTrigger>
    <SelectPortal>
      <SelectContent
        class="relative z-50 min-w-[8rem] overflow-hidden border border-border bg-ground-sunk text-foreground"
        position="popper"
        :side-offset="8"
      >
        <SelectViewport class="p-1">
          <SelectItem
            v-for="opt in items"
            :key="opt.value"
            :value="opt.value"
            class="relative flex w-full cursor-default select-none items-center py-2 pl-10 pr-2 font-mono text-sm text-muted-foreground outline-none data-[highlighted]:bg-accent data-[highlighted]:text-foreground data-[highlighted]:underline data-[highlighted]:underline-offset-[3px] data-[state=checked]:text-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50"
          >
            <!-- The highlight fill is 1.06:1, so ink and underline carry it, and
                 the chosen option gets a mark of its own. -->
            <SelectItemIndicator class="absolute left-3 inline-flex items-center">
              <Check class="h-4 w-4" aria-hidden="true" />
            </SelectItemIndicator>
            <SelectItemText>{{ opt.label }}</SelectItemText>
          </SelectItem>
        </SelectViewport>
      </SelectContent>
    </SelectPortal>
  </SelectRoot>
</template>
