<script setup lang="ts">
import { useI18n } from "vue-i18n";
import {
  DialogRoot,
  DialogPortal,
  DialogOverlay,
  DialogContent,
  DialogClose,
  DialogTitle,
  DialogDescription,
} from "radix-vue";
import { X } from "@lucide/vue";
import { cn } from "@/lib/utils";

const { t } = useI18n();

defineProps<{
  open?: boolean;
  class?: string;
}>();

defineEmits<{
  "update:open": [value: boolean];
}>();
</script>

<template>
  <DialogRoot :open="open" @update:open="$emit('update:open', $event)">
    <slot name="trigger" />
    <DialogPortal>
      <DialogOverlay class="fixed inset-0 z-50 bg-black/60" />
      <!--
        No entrance: DESIGN's One Motion Rule spends motion on an artifact
        resolving and on nothing else, so the dialog is simply there.

        `null`, not the string `'undefined'`, when there is no description.
        The string is what radix's warning text suggests, but it left an IDREF
        to an element that does not exist *and* radix still warned (measured);
        `null` removes the attribute and the warning with it.
      -->
      <DialogContent
        :aria-describedby="$slots.description ? undefined : null"
        :class="
          cn(
            'fixed left-[50%] top-[50%] z-50 grid w-full max-w-lg translate-x-[-50%] translate-y-[-50%] gap-4 border bg-background p-6',
            $props.class,
          )
        "
      >
        <div v-if="$slots.title" class="space-y-1 pr-6">
          <DialogTitle class="text-lg font-semibold leading-none tracking-tight">
            <slot name="title" />
          </DialogTitle>
          <DialogDescription v-if="$slots.description" class="text-sm text-muted-foreground mt-1">
            <slot name="description" />
          </DialogDescription>
        </div>
        <!-- Fallback: visually hidden title when no title slot is provided -->
        <DialogTitle v-else class="sr-only">{{ t("common.dialog") }}</DialogTitle>
        <slot />
        <DialogClose
          class="absolute right-4 top-4 text-muted-foreground hover:text-foreground disabled:pointer-events-none"
        >
          <X class="h-4 w-4" />
          <span class="sr-only">{{ t("common.close") }}</span>
        </DialogClose>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>
