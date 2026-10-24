<script setup lang="ts">
/**
 * A text field that suggests, and never insists (RFC 0004-bis §6.2).
 *
 * `ui/` had `Input` and `Select` and nothing in between, so the two places that
 * needed one improvised with `<datalist>` — unstylable, inconsistently
 * keyboard-navigable across browsers, invisible to assistive tech as a listbox,
 * and unable to render a "no match" message, which is the one behaviour this
 * component exists to get.
 *
 * Three behaviours are not optional, because each is how this pattern usually
 * fails:
 *
 *  1. **A suggestion never blocks a submission.** Every field this replaces
 *     must still accept a value the server has not seen — warming a package
 *     that is not cached yet is the *point* of `AdminWarming`, and a combobox
 *     that only accepted what it could already offer would break it. The model
 *     is the typed text; selecting a suggestion is a shortcut for typing it.
 *  2. **No result is a stated answer**, not an empty popup: "nothing cached
 *     matches `lodahs`" is the message that catches the typo. A blank that
 *     reads as a fact is the defect this whole RFC is about.
 *  3. **Keystrokes never reach a third party.** `loading` and the option list
 *     are the caller's; a source that costs an upstream round trip is an
 *     explicit affordance the caller renders in the `#footer` slot, not
 *     something this component triggers.
 *
 * It obeys the world: no card, no shadow, the ground and ink of the surface it
 * sits on, and the same 1px border as `Input`.
 */
import { computed, nextTick, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { cn } from "@/lib/utils";
import { Announcer } from "@/components/ui/announcer";

export interface ComboboxOption {
  value: string;
  /** Shown instead of `value` when the two differ (`oidc:alice` vs `alice`). */
  label?: string;
  /** A second line: what the option is, or where it came from. */
  hint?: string;
}

const props = withDefaults(
  defineProps<{
    id?: string;
    modelValue: string;
    options: ComboboxOption[];
    placeholder?: string;
    /** Shown in place of the list while the source is still answering. */
    loading?: boolean;
    disabled?: boolean;
    /**
     * Why the field is disabled, stated rather than left to be guessed — a
     * version list has no meaning until a package is named.
     */
    disabledReason?: string;
    /** Overrides the default "nothing matches …" message. */
    emptyMessage?: string;
    class?: string;
  }>(),
  {
    id: undefined,
    placeholder: "",
    loading: false,
    disabled: false,
    disabledReason: "",
    emptyMessage: "",
    class: "",
  },
);

const emit = defineEmits<{
  "update:modelValue": [value: string];
  /** A suggestion was taken, as opposed to text being typed. */
  select: [value: string];
}>();

const { t } = useI18n();

const open = ref(false);
const activeIndex = ref(-1);
/** What to restore on Escape — the text as it was before arrowing about. */
const committed = ref(props.modelValue);
const listId = computed(() => `${props.id ?? "combobox"}-listbox`);
const optionId = (i: number) => `${listId.value}-opt-${i}`;

const hasOptions = computed(() => props.options.length > 0);
const showEmpty = computed(() => open.value && !props.loading && !hasOptions.value);

const emptyText = computed(
  () => props.emptyMessage || t("combobox.noMatches", { query: props.modelValue }),
);

/**
 * Announced, not merely rendered. This is the second consumer the console's one
 * live region has been waiting for (RFC 0004-bis §2.6) — a result count that
 * changes under a text field is invisible to a screen-reader user otherwise.
 */
const announcement = computed(() => {
  if (!open.value) return "";
  if (props.loading) return t("combobox.searching");
  if (!hasOptions.value) return emptyText.value;
  return t("combobox.resultCount", { count: props.options.length }, props.options.length);
});

watch(
  () => props.modelValue,
  (value) => {
    committed.value = value;
    activeIndex.value = -1;
  },
);

// A list that shrinks under the cursor must not leave the cursor past its end.
watch(
  () => props.options.length,
  (length) => {
    if (activeIndex.value >= length) activeIndex.value = length - 1;
  },
);

function onInput(event: Event) {
  const value = (event.target as HTMLInputElement).value;
  committed.value = value;
  open.value = true;
  activeIndex.value = -1;
  emit("update:modelValue", value);
}

function choose(index: number) {
  const option = props.options[index];
  if (!option) return;
  committed.value = option.value;
  emit("update:modelValue", option.value);
  emit("select", option.value);
  open.value = false;
  activeIndex.value = -1;
}

function move(delta: number) {
  if (!hasOptions.value) return;
  open.value = true;
  const count = props.options.length;
  /* `-1` is "the text I typed"; `0..count-1` are the suggestions. Wrapping
     *through* -1 rather than around it means ArrowUp from the first option
     returns you to your own words instead of jumping to the bottom of a list
     you were leaving. */
  const next = activeIndex.value + delta;
  if (next < -1) {
    activeIndex.value = count - 1;
  } else {
    activeIndex.value = next >= count ? -1 : next;
  }
  void nextTick(scrollActiveIntoView);
}

const listEl = ref<HTMLElement | null>(null);
function scrollActiveIntoView() {
  if (activeIndex.value < 0) return;
  listEl.value?.querySelector(`#${CSS.escape(optionId(activeIndex.value))}`)?.scrollIntoView({
    block: "nearest",
  });
}

function onKeydown(event: KeyboardEvent) {
  switch (event.key) {
    case "ArrowDown":
      event.preventDefault();
      move(1);
      break;
    case "ArrowUp":
      event.preventDefault();
      move(-1);
      break;
    case "Home":
      if (!open.value || !hasOptions.value) return;
      event.preventDefault();
      activeIndex.value = 0;
      void nextTick(scrollActiveIntoView);
      break;
    case "End":
      if (!open.value || !hasOptions.value) return;
      event.preventDefault();
      activeIndex.value = props.options.length - 1;
      void nextTick(scrollActiveIntoView);
      break;
    case "Enter":
      // Only when a suggestion is highlighted. Otherwise the keypress belongs
      // to the form: a combobox that swallows Enter turns "submit what I typed"
      // into "nothing happened".
      if (open.value && activeIndex.value >= 0) {
        event.preventDefault();
        choose(activeIndex.value);
      }
      break;
    case "Escape":
      if (!open.value) return;
      event.preventDefault();
      event.stopPropagation();
      open.value = false;
      activeIndex.value = -1;
      // Reverts to what was typed, which is what Escape means here — the typed
      // value is the answer, and arrowing was only a look around.
      emit("update:modelValue", committed.value);
      break;
    case "Tab":
      open.value = false;
      activeIndex.value = -1;
      break;
  }
}
</script>

<template>
  <div class="relative">
    <input
      :id="id"
      type="text"
      role="combobox"
      autocomplete="off"
      :value="modelValue"
      :placeholder="placeholder"
      :disabled="disabled"
      :aria-expanded="open"
      :aria-controls="listId"
      aria-autocomplete="list"
      :aria-activedescendant="activeIndex >= 0 ? optionId(activeIndex) : undefined"
      :aria-describedby="disabled && disabledReason ? `${listId}-reason` : undefined"
      :class="
        cn(
          'flex h-9 w-full rounded-sm border border-input bg-background px-3 py-1 text-sm transition-colors placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:opacity-50',
          props.class,
        )
      "
      @input="onInput"
      @keydown="onKeydown"
      @focus="open = true"
      @blur="open = false"
    />

    <!-- Stated, not implied: a field that is dark for a reason says the reason. -->
    <p
      v-if="disabled && disabledReason"
      :id="`${listId}-reason`"
      class="mt-1 text-xs text-muted-foreground"
    >
      {{ disabledReason }}
    </p>

    <Announcer :message="announcement" />

    <!--
      `v-show`, not `v-if`: the listbox has to exist for `aria-controls` to
      point at something, and a reference to an element that is not in the
      document is a broken relationship rather than a closed one.

      Flat and inked. No card and no shadow (DESIGN.md's Flat-At-Rest Rule);
      the border is `Input`'s, so an open combobox reads as the same object
      grown taller rather than as a panel that arrived.
    -->
    <div
      v-show="open && (loading || hasOptions || showEmpty)"
      :id="listId"
      ref="listEl"
      role="listbox"
      :aria-label="placeholder || undefined"
      class="absolute z-50 mt-1 max-h-60 w-full overflow-y-auto border border-border bg-background"
    >
      <p v-if="loading" class="px-3 py-2 text-xs text-muted-foreground">
        {{ t("combobox.searching") }}
      </p>

      <!-- The message that catches the typo. -->
      <p v-else-if="showEmpty" class="px-3 py-2 text-xs text-muted-foreground">
        {{ emptyText }}
      </p>

      <template v-else>
        <!--
          `mousedown`, not `click`: `blur` fires first and would close the list
          out from under the pointer, so a suggestion could be seen and not
          taken.
        -->
        <div
          v-for="(opt, i) in options"
          :id="optionId(i)"
          :key="opt.value"
          role="option"
          :aria-selected="i === activeIndex"
          class="cursor-pointer px-3 py-1.5 text-sm"
          :class="i === activeIndex ? 'bg-accent text-accent-foreground' : ''"
          @mousedown.prevent="choose(i)"
          @mousemove="activeIndex = i"
        >
          <span class="font-mono">{{ opt.label ?? opt.value }}</span>
          <span v-if="opt.hint" class="ml-2 text-xs text-muted-foreground">{{ opt.hint }}</span>
        </div>
      </template>

      <!-- Where an opt-in upstream search goes. Never triggered by typing. -->
      <div v-if="$slots.footer" class="border-t border-border px-3 py-1.5" @mousedown.prevent>
        <slot name="footer" />
      </div>
    </div>
  </div>
</template>
