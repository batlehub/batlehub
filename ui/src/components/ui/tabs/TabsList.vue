<script setup lang="ts">
import { TabsList, type TabsListProps } from "radix-vue";
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = defineProps<TabsListProps & { class?: HTMLAttributes["class"] }>();
</script>

<template>
  <TabsList
    v-bind="props"
    :class="
      cn(
        // One frame, 1px dividers, no gap (DESIGN.md's choice-set grammar). The
        // frame is split between list (left/top) and cells (right/bottom) so it
        // stays 1px everywhere when the cells wrap onto a second line.
        'inline-flex flex-wrap items-stretch border-l border-t border-border text-muted-foreground',
        props.class,
      )
    "
  >
    <slot />
  </TabsList>
</template>
