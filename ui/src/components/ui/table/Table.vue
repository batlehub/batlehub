<script setup lang="ts">
import { type HTMLAttributes } from "vue";
import { useI18n } from "vue-i18n";
import { cn } from "@/lib/utils";

const props = defineProps<{
  class?: HTMLAttributes["class"];
  label?: string;
  /**
   * RFC 0050 §6.7: below `md`, each row stacks into a block instead of
   * clipping its columns at 390 px. The header stays for screen readers.
   */
  stacked?: boolean;
}>();

const STACKED =
  "max-md:[&_thead]:sr-only max-md:[&_tr]:block max-md:[&_tr]:py-2 max-md:[&_td]:block max-md:[&_td]:px-0 max-md:[&_td]:py-1";

const { t } = useI18n();
</script>

<template>
  <!--
    The scroll container is focusable, and has to be. A region that scrolls but
    cannot be reached by keyboard is unreachable content for anyone not using a
    pointer — axe reports it as `scrollable-region-focusable`, and RFC 0003
    §4.7 makes tables keyboard-operable explicitly. `tabindex="0"` puts it in
    the tab order so arrow keys can scroll it; `role="region"` plus a name is
    what makes it announce as something worth stopping on rather than as an
    anonymous focus stop.

    This is also the Own-Container Overflow Rule's container: wide content
    scrolls here so the body never scrolls sideways.

    SonarCloud flags the `tabindex` here ("should only be declared on
    interactive elements") and is wrong: its rule has no exception for scroll
    containers, and removing the attribute trades a heuristic finding for a real
    WCAG 2.1.1 failure that axe catches. Web:S6845 is ignored for this one file
    in sonar-project.properties, with the reasoning there — a per-issue
    resolution in the UI does not survive the next branch that touches this file,
    which is how the finding came back. See
    docs/internal/sonar-triage-2026-08-30.md. Do not "fix" it.
  -->
  <!-- `<section>`, not `<div role="region">`: a named section carries the role
       natively, and the native element is the one assistive technology has the
       fewest ways to get wrong. -->
  <section
    class="relative w-full overflow-auto"
    tabindex="0"
    :aria-label="label ?? t('a11y.tableScroll')"
  >
    <table :class="cn('w-full caption-bottom text-sm', props.stacked && STACKED, props.class)">
      <slot />
    </table>
  </section>
</template>
