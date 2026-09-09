<script setup lang="ts">
// A verbs field of the config generator: the chosen verbs as removable chips
// and one `<select>` that appends from the closed vocabulary. The value stays
// the comma-separated string the rest of the generator binds and renders
// (`permsToToml`), so the picker changes how a verb is *entered*, not how the
// state is shaped or emitted.
//
// A select rather than a free-text input because the set is closed: a verb not
// on the list is a startup error, and `/guide/access-control` once documented
// three that did not exist. Offering only what the server accepts — for the
// tier and registry kind at hand — is the closed vocabulary reaching the form.
import { computed } from "vue";
import { addVerb, removeVerb, verbOptions, verbsOutOfPlace, csvToList } from "./configToml";
import type { VerbTier } from "./configToml";

const props = defineProps<{
  modelValue: string;
  /// Where this field sits — decides which verbs are offered.
  tier: VerbTier;
  /// Accessible name of the select, since the picker is not wrapped in a label.
  label: string;
}>();
const emit = defineEmits<{ (e: "update:modelValue", v: string): void }>();

const selected = computed(() => csvToList(props.modelValue));
const groups = computed(() => verbOptions(props.tier));
const outOfPlace = computed(() => new Set(verbsOutOfPlace(props.modelValue, props.tier)));

function onPick(e: Event) {
  const el = e.target as HTMLSelectElement;
  if (el.value) emit("update:modelValue", addVerb(props.modelValue, el.value));
  // Snap back to the prompt so the control always reads "add another".
  el.value = "";
}
function remove(v: string) {
  emit("update:modelValue", removeVerb(props.modelValue, v));
}
</script>

<template>
  <div class="vp-verbs">
    <ul v-if="selected.length" class="vp-chips" :aria-label="`${label} chosen`">
      <li
        v-for="v in selected"
        :key="v"
        class="vp-chip"
        :class="{ 'vp-chip-bad': outOfPlace.has(v) }"
      >
        <code>{{ v }}</code>
        <button
          type="button"
          class="vp-chip-remove"
          :aria-label="`Remove ${v}`"
          @click="remove(v)"
        >
          ×
        </button>
      </li>
    </ul>
    <select :aria-label="`Add a verb to ${label}`" value="" @change="onPick">
      <option value="">+ Add a verb…</option>
      <optgroup v-for="g in groups" :key="g.label" :label="g.label">
        <option
          v-for="v in g.verbs"
          :key="v"
          :value="v"
          :disabled="selected.includes(v)"
        >
          {{ v }}
        </option>
      </optgroup>
    </select>
  </div>
</template>

<style scoped>
.vp-verbs {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
}

/* Same box as the generator's other controls (its `select` rule is scoped to
   ConfigGenerator.vue and stops at this component's root), so a verbs field
   sits in a row with a subject field without a visible seam. */
select {
  padding: 0.35rem 0.6rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  font-size: var(--t-body);
  font-family: var(--vp-font-family-mono);
  width: 100%;
  box-sizing: border-box;
  transition: border-color 0.15s;
}
select:focus {
  outline: none;
  border-color: var(--vp-c-brand-1);
}

.vp-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 0.3rem;
  list-style: none;
  margin: 0;
  padding: 0;
}

.vp-chip {
  display: inline-flex;
  align-items: center;
  gap: 0.2rem;
  padding: 0.1rem 0.25rem 0.1rem 0.5rem;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--radius);
  background: var(--vp-c-brand-soft);
  margin: 0;
}
.vp-chip code {
  font-size: var(--t-meta);
  color: var(--vp-c-text-1);
  background: transparent;
  padding: 0;
}

/* A verb the server will refuse where it stands — an ecosystem verb after the
   registry's type changed under it, or one at the instance tier. Same colour
   as the generator's `.cg-hint-required`, which names the problem beside it. */
.vp-chip-bad {
  border-color: var(--vp-c-danger-1);
}
.vp-chip-bad code {
  color: var(--vp-c-danger-1);
  text-decoration: line-through;
}

.vp-chip-remove {
  border: 0;
  background: transparent;
  color: var(--vp-c-text-2);
  font-size: var(--t-body);
  line-height: 1;
  padding: 0.1rem 0.3rem;
  border-radius: var(--radius);
  cursor: pointer;
}
.vp-chip-remove:hover,
.vp-chip-remove:focus-visible {
  color: var(--vp-c-danger-1);
  background: var(--vp-c-bg);
  outline: none;
}
</style>
