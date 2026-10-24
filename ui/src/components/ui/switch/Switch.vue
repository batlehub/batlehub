<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<{
  class?: HTMLAttributes["class"];
  modelValue?: boolean;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
}>();

function toggle() {
  if (props.disabled) return;
  emit("update:modelValue", !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :disabled="disabled"
    :class="
      cn(
        'peer inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-sm border border-border disabled:cursor-not-allowed disabled:opacity-50',
        modelValue ? 'bg-foreground' : 'bg-transparent',
        props.class,
      )
    "
    @click="toggle"
  >
    <span
      :class="
        cn(
          'pointer-events-none block h-3.5 w-3.5 rounded-sm',
          modelValue ? 'translate-x-4 bg-background' : 'translate-x-0.5 bg-muted-foreground',
        )
      "
    />
  </button>
</template>
