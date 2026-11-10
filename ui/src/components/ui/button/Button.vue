<script setup lang="ts">
import { type HTMLAttributes, computed } from "vue";
import { Primitive } from "radix-vue";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

/**
 * `disabled:opacity-50` is *not* on the shared base.
 *
 * It was, and it dimmed every variant uniformly — including the two crimson
 * fills, which kept their fill underneath. DESIGN.md is explicit that the
 * primary "disabled drops the fill entirely and becomes a dim outlined
 * control (crimson never appears in a disabled state)", and §Colour repeats
 * it: "don't let it appear in a disabled control".
 *
 * So the filled variants drop their fill and become the outlined control the
 * rule describes — `--ink-dim` on a `--rule-strong` boundary, 5.62:1 in dark
 * and 7.24:1 in light with a 3.68:1/3.41:1 border. The unfilled ones take the
 * `opacity: .5` DESIGN.md gives the secondary control, per variant rather than
 * from one shared string that could not tell them apart.
 */
const DISABLED_OUTLINED =
  "disabled:bg-transparent disabled:border disabled:border-border disabled:text-muted-foreground";
const DISABLED_DIM = "disabled:opacity-50";

const buttonVariants = cva(
  "inline-flex items-center justify-center whitespace-nowrap font-mono font-semibold text-sm rounded-sm transition-colors disabled:pointer-events-none",
  {
    variants: {
      variant: {
        /* The one primary action, and the only element in the system allowed a
           box-shadow (DESIGN.md, Flat-At-Rest Rule). Flat at rest; on hover a
           cut-out action ring rather than a halo; on :active the pixel step —
           the button displaces sideways and leaves two stacked plates behind
           it, like a mis-registered print pull. Both are zero-blur.

           The ring *is* the hover. `hover:bg-primary/85` also sat here, and
           dimming crimson toward a near-black ground takes `--accent-ink` on
           it from 5.69:1 to 4.28:1 — under the AA floor, on the label of the
           one filled action in the product, in the state a pointer is in when
           it is about to click. DESIGN.md never asked for it: it specifies the
           ring and the pixel step and nothing about the fill. */
        default: `bg-primary text-primary-foreground hover:[box-shadow:var(--action-ring)] active:[box-shadow:var(--pixel-step)] active:-translate-x-0.5 transition-[box-shadow,background-color,transform] ${DISABLED_OUTLINED}`,
        /* Same fill and same ink as `default` — `--destructive` resolves to
           `--accent` — so it takes the same hover for the same reason, rather
           than the fill dim that measured the same 4.28:1. */
        destructive: `bg-destructive text-destructive-foreground hover:[box-shadow:var(--action-ring)] active:[box-shadow:var(--pixel-step)] active:-translate-x-0.5 transition-[box-shadow,background-color,transform] ${DISABLED_OUTLINED}`,
        /* DESIGN.md's "Secondary — the control" (`.ctl`), transcribed from the
           rule and from `ui/design-proof/index.html:186-191`, which implements
           it: transparent, 1px `--rule-strong`, `--ink-dim` text; hover lifts
           the text to `--ink` and the border to `--ink-dim`; `aria-pressed`
           lifts the text and turns the border copper.

           It was crimson at 40% — `border-primary/40 text-primary` — which is
           not what the system specifies for this control and did not measure:
           1.70:1 in dark and 2.14:1 in light for the boundary of an
           interactive element, against the 3:1 of WCAG 1.4.11.

           The hover went with it, and had to. `hover:bg-accent` resolves to
           `--surface-hover`, which is `--ground-raised`, which `tokens.css`
           annotates as "1.06:1 — confirmation, not elevation": the fill was
           imperceptible, so the *only* thing that marked a hover was the
           border going `/40` to `/70`. Making the resting border opaque would
           have taken that away and left the control with no hover at all.
           Lifting the ink and the border instead is two channels, both
           measurable: 3.68/3.41 to 5.62/7.24 on the boundary, 5.62/7.24 to
           16.88/16.64 on the label. */
        outline: `border border-border bg-transparent text-muted-foreground hover:text-foreground hover:border-muted-foreground aria-pressed:text-foreground aria-pressed:border-copper ${DISABLED_DIM}`,
        ghost: `hover:bg-accent hover:text-accent-foreground ${DISABLED_DIM}`,
      },
      size: {
        // Vertical padding is declared even though the fixed height plus
        // inline-flex centring already places the label: the height is what
        // *happens* to space it, the padding is what says it should be. Without
        // it the label measures flush to the fill edge, which is what a rendered
        // scan reports and what would actually happen if the height were dropped.
        default: "h-9 px-4 py-2",
        sm: "h-8 px-3 py-1 text-xs",
        icon: "h-9 w-9",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

type ButtonVariants = VariantProps<typeof buttonVariants>;

const props = withDefaults(
  defineProps<{
    variant?: ButtonVariants["variant"];
    size?: ButtonVariants["size"];
    class?: HTMLAttributes["class"];
    disabled?: boolean;
    /**
     * Render the slotted child as the root instead of wrapping it in a
     * `<button>`. Callers already passed `as-child` with a `RouterLink` inside;
     * until this existed it fell through as a plain attribute and produced
     * `<button><a>` — a nested interactive control, which axe flags and screen
     * readers announce unreliably.
     */
    asChild?: boolean;
    as?: string;
  }>(),
  { variant: "default", size: "default", as: "button" },
);

const delegatedProps = computed(() => {
  const {
    class: _class,
    variant: _variant,
    size: _size,
    asChild: _asChild,
    as: _as,
    ...rest
  } = props;
  return rest;
});
</script>

<template>
  <Primitive
    v-bind="delegatedProps"
    :as="as"
    :as-child="asChild"
    :class="cn(buttonVariants({ variant, size }), props.class)"
  >
    <slot />
  </Primitive>
</template>
