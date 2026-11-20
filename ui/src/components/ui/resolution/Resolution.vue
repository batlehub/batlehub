<script setup lang="ts">
/**
 * Resolution as State — DESIGN.md's organising idea, and the system's signature.
 *
 * "What BatleHub holds and has verified renders at full resolution; what it does
 * not renders coarse." A fine 3×3 matrix of 5px cells means held and verified; a
 * coarse 2×2 of 8px cells means it is not. The count of lit cells and the hue
 * then say *which* not-held state it is.
 *
 * ## Why this exists as a component and not as a page's markup
 *
 * DESIGN.md specified it, `ui/design-proof/index.html` implements it, and
 * `ui/src` had it nowhere — for two RFCs. RFC 0004-bis §7 item 7 and §2.7 found
 * that independently, from opposite directions: a Phase 5 reviewer reading admin
 * pages, and a direct measurement of `/packages` against the proof it was built
 * from. The conclusion both reached was that it must land **once**, as one shared
 * component, rather than being invented per page — which is how the console
 * ended up with eleven crimson row-verbs and two `<datalist>` improvisations.
 *
 * ## Three channels, never one
 *
 * DESIGN.md: "Never colour alone — pattern, word and hue all carry it." So the
 * matrix is `aria-hidden` and *always* accompanied by its word: the pattern
 * carries it for a monochrome display, the word for a screen reader, the hue for
 * everyone else. `label` is required for that reason; there is no variant of
 * this component that renders the dots on their own.
 *
 * ## What is deliberately not here
 *
 * The `resolve` animation (520ms, `cubic-bezier(.16, 1, .3, 1)`). DESIGN.md is
 * emphatic that it plays "on rows that have just arrived or just changed state —
 * never on load of unchanged content, never on hover, never on more than the
 * rows that changed". A component cannot know which of those it is in; only the
 * list rendering it can. Putting the animation here would make it fire on every
 * render, which is the one thing the spec forbids by name.
 */
import { computed, type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

/** The six states, exactly as DESIGN.md's table names them. */
export type ResolutionState = "cached" | "stale" | "held" | "pending" | "yanked" | "blocked";

interface StateSpec {
  /** Fine 3×3 (held and verified) or coarse 2×2 (not). */
  fine: boolean;
  /** Which cells are lit, in reading order. Length 9 for fine, 4 for coarse. */
  on: readonly boolean[];
  /** The hue channel. */
  klass: string;
}

const T = true;
const F = false;

/**
 * DESIGN.md's table, transcribed rather than interpreted:
 *
 * | State   | Matrix     | Lit            | Colour      |
 * |---------|------------|----------------|-------------|
 * | Cached  | fine 3×3   | all 9          | `--ink`     |
 * | Stale   | fine 3×3   | 8 of 9, centre out | `--copper` |
 * | Held    | coarse 2×2 | 3 of 4         | `--copper`  |
 * | Pending | coarse 2×2 | 2 of 4         | `--ink-dim` |
 * | Yanked  | coarse 2×2 | 1 of 4         | `--ink-dim` |
 * | Blocked | coarse 2×2 | all 4          | `--accent`  |
 *
 * "Centre out" for stale is the middle cell of the 3×3 — index 4 — which is why
 * that row is written out rather than generated from a count: which cell is dark
 * is the point, not how many.
 */
const STATES: Record<ResolutionState, StateSpec> = {
  cached: { fine: true, on: [T, T, T, T, T, T, T, T, T], klass: "text-ok" },
  stale: { fine: true, on: [T, T, T, T, F, T, T, T, T], klass: "text-copper" },
  held: { fine: false, on: [T, T, T, F], klass: "text-copper" },
  pending: { fine: false, on: [T, F, F, T], klass: "text-muted-foreground" },
  yanked: { fine: false, on: [T, F, F, F], klass: "text-muted-foreground" },
  blocked: { fine: false, on: [T, T, T, T], klass: "text-destructive" },
};

const props = defineProps<{
  state: ResolutionState;
  /**
   * The word. Required, and translated by the caller: this component is data,
   * and data is not language.
   */
  label: string;
  class?: HTMLAttributes["class"];
}>();

const spec = computed(() => STATES[props.state]);
</script>

<template>
  <span
    :class="cn('inline-flex items-center gap-3 whitespace-nowrap', spec.klass, props.class)"
    :data-state="props.state"
  >
    <!-- Cells inherit `currentColor`, so the hue is set once on the wrapper and
         the matrix needs no colour of its own — which is what lets it sit inside
         a link or a heading and take that context's ink. -->
    <span
      aria-hidden="true"
      class="grid gap-px flex-none"
      :class="
        spec.fine
          ? 'grid-cols-[repeat(3,5px)] grid-rows-[repeat(3,5px)]'
          : 'grid-cols-[repeat(2,8px)] grid-rows-[repeat(2,8px)]'
      "
      data-testid="resolution-matrix"
    >
      <i
        v-for="(lit, i) in spec.on"
        :key="i"
        class="block bg-current"
        :class="[lit ? 'opacity-100' : 'opacity-[.18]', spec.fine ? 'size-[5px]' : 'size-[8px]']"
      />
    </span>
    <span class="font-sans text-xs font-medium uppercase tracking-wide">{{ props.label }}</span>
  </span>
</template>
