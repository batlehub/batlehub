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
} from "radix-vue";
import { computed } from "vue";
import { ChevronDown } from "@lucide/vue";
import { cn } from "@/lib/utils";

const props = defineProps<{
  modelValue?: string;
  placeholder?: string;
  options: { value: string; label: string }[];
  class?: string;
  id?: string;
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
      :class="
        cn(
          'flex h-10 w-full items-center justify-between rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50',
          $props.class,
        )
      "
    >
      <SelectValue :placeholder="placeholder" />
      <ChevronDown class="h-4 w-4 opacity-50" />
    </SelectTrigger>
    <SelectPortal>
      <SelectContent
        class="relative z-50 min-w-[8rem] overflow-hidden rounded-md border bg-popover text-popover-foreground data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
        position="popper"
        :side-offset="4"
      >
        <SelectViewport class="p-1">
          <SelectItem
            v-for="opt in items"
            :key="opt.value"
            :value="opt.value"
            class="relative flex w-full cursor-default select-none items-center rounded-sm py-1.5 pl-8 pr-2 text-sm outline-none focus:bg-accent focus:text-accent-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50"
          >
            <SelectItemText>{{ opt.label }}</SelectItemText>
          </SelectItem>
        </SelectViewport>
      </SelectContent>
    </SelectPortal>
  </SelectRoot>
</template>
