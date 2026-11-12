<script setup lang="ts">
/**
 * The ARIA 1.2 listbox half of the combobox pattern (RFC 0046 §15 Q5).
 *
 * Extracted from `Combobox.vue` so the command palette does not grow a second
 * implementation of the same pattern: the input owning `aria-activedescendant`
 * stays with each caller, the list it points at lives here. An option with an
 * `href` renders as a link, so a palette row keeps middle-click, open-in-new-tab
 * and copy-link-address; everything else is a plain option.
 *
 * Text interpolation only, never `v-html`: an option label can be a registry
 * name, which is data an operator typed (RFC 0046 §7.1 T4).
 */
import { computed } from "vue";

export interface ListboxItem {
  key: string;
  label: string;
  hint?: string;
  href?: string;
  /** Consecutive items with the same group are drawn under one heading. */
  group?: string;
}

const props = defineProps<{
  id: string;
  items: ListboxItem[];
  activeIndex: number;
  ariaLabel?: string;
}>();

const emit = defineEmits<{
  /** A pointer took an option; the event lets a link keep its modifiers. */
  choose: [index: number, event: MouseEvent];
  hover: [index: number];
}>();

const optionId = (i: number) => `${props.id}-opt-${i}`;

/** Items folded into runs of one group, keeping each item's flat index. */
const runs = computed(() => {
  const out: { group?: string; entries: { item: ListboxItem; index: number }[] }[] = [];
  props.items.forEach((item, index) => {
    const last = out.at(-1);
    if (last && last.group === item.group) last.entries.push({ item, index });
    else out.push({ group: item.group, entries: [{ item, index }] });
  });
  return out;
});

const rowClass = (i: number) => [
  "flex cursor-pointer items-baseline gap-2 px-3 py-2 text-sm",
  i === props.activeIndex ? "bg-accent text-foreground underline underline-offset-[3px]" : "",
];
</script>

<template>
  <div :id="id" role="listbox" :aria-label="ariaLabel || undefined">
    <slot name="before" />
    <div
      v-for="(run, r) in runs"
      :key="`${run.group ?? ''}-${r}`"
      :role="run.group ? 'group' : undefined"
      :aria-labelledby="run.group ? `${id}-group-${r}` : undefined"
    >
      <div
        v-if="run.group"
        :id="`${id}-group-${r}`"
        class="px-3 pt-3 pb-1 font-mono text-xs uppercase tracking-wider text-muted-foreground"
      >
        {{ run.group }}
      </div>
      <template v-for="{ item, index } in run.entries" :key="item.key">
        <!--
          `mousedown`, not `click`, on a plain option: the input's `blur` fires
          first and would close the list out from under the pointer. A link is
          left to `click`, so the browser keeps its own modifiers.
        -->
        <a
          v-if="item.href"
          :id="optionId(index)"
          role="option"
          :href="item.href"
          :aria-selected="index === activeIndex"
          :class="rowClass(index)"
          @click="emit('choose', index, $event)"
          @mousedown.prevent
          @mousemove="emit('hover', index)"
        >
          <span class="font-mono">{{ item.label }}</span>
          <span v-if="item.hint" class="ml-auto text-xs text-muted-foreground">{{
            item.hint
          }}</span>
        </a>
        <div
          v-else
          :id="optionId(index)"
          role="option"
          :aria-selected="index === activeIndex"
          :class="rowClass(index)"
          @mousedown.prevent="emit('choose', index, $event)"
          @mousemove="emit('hover', index)"
        >
          <span class="font-mono">{{ item.label }}</span>
          <span v-if="item.hint" class="text-xs text-muted-foreground">{{ item.hint }}</span>
        </div>
      </template>
    </div>
    <slot name="after" />
  </div>
</template>
