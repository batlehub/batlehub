<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-sm border px-2 py-px text-xs font-sans font-medium transition-colors",
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
        /* Was `border-secondary bg-secondary`: a --ground-raised border (1.06:1)
           around a --ground-raised fill, so the fill was the only channel and
           the boundary did not exist. Same ink, on a rule that can be seen. */
        /* DESIGN.md (The Interface Face Rule): a neutral label is a flat chip, not a bordered cell.
           Bordered it was the same shape as a `sm` outline button and sat
           beside them in the same row (AdminHealth: CARGO · PROXY · Clear
           cache), so a label read as a control. The fill is not carrying a
           state here — the word is — and the ink on it is measured on
           --ground-raised (16.88/5.28:1 dark, 16.64/6.52:1 light). */
        secondary: "border-transparent bg-muted text-foreground",
        destructive: "border-destructive text-destructive",
        outline: "border-transparent bg-muted text-muted-foreground",
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
        known: "border-transparent bg-muted text-foreground",
        /* Healthy / allowed / serving — the --ok signal (DESIGN.md, The Interface Face Rule). */
        ok: "border-ok text-ok",
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
