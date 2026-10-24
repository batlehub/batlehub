<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center rounded-sm border px-2 py-0.5 text-xs font-mono font-semibold transition-colors",
  {
    variants: {
      // No alpha fill under the accent text. The `bg-…/10` utilities these drop
      // put each colour on a 10% tint of itself, which nothing had ever
      // measured: on paper that lands `#c50220` on `#ecd0d3` — 4.26:1, under the
      // AA floor, at 12px. It is R10's finding again in a different place, and
      // the Undependable Fill Rule's point exactly: a fill is not a state
      // channel. The border carries the state, and the text sits on the card
      // ground, where it measures 5.6:1.
      variant: {
        /* Full opacity, no `/40`. The paragraph above says the border carries
           the state because the fill cannot — and at 40% the border did not
           carry it either: 1.76:1 in dark and 2.08:1 in light against the
           3:1 that WCAG 1.4.11 asks of a boundary that is the only thing
           distinguishing a state. Opaque they measure 5.42/5.07 (accent) and
           7.57/5.16 (copper) on a card. */
        default: "border-primary text-primary",
        secondary: "border-secondary bg-secondary text-secondary-foreground",
        destructive: "border-destructive text-destructive",
        outline: "border-border text-muted-foreground",
        copper: "border-copper text-copper",
        /* An affirmative answer, in the One Synthetic Rule's terms — the same
           reasoning `Alert.vue`'s `success` already carries, ported rather
           than reinvented: a confirmation is full ink against a rule that
           carries more weight, not a fifth hue.

           It exists because `--destructive` resolves to `--accent`
           (`assets/index.css`), so `default` and `destructive` paint the same
           crimson. `AccessCheck` — the page whose entire job is to answer "was
           I allowed?" — rendered both answers in it, and `BadgeVariant` had no
           member that meant "known".

           `border-border`, not `border-rule-strong`: `--border` *is*
           `--rule-strong`, and there is no `--color-rule-strong` in `@theme`,
           so the latter would compile to nothing and quietly ship a
           borderless badge. It differs from `outline` on the ink — full
           `--ink` rather than `--ink-dim` — because an answer is not the same
           thing as a neutral label. */
        known: "border-border text-foreground",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

type BadgeVariants = VariantProps<typeof badgeVariants>;

const props = withDefaults(
  defineProps<{
    variant?: BadgeVariants["variant"];
    class?: HTMLAttributes["class"];
  }>(),
  { variant: "default" },
);
</script>

<template>
  <div :class="cn(badgeVariants({ variant }), props.class)">
    <slot />
  </div>
</template>
