<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { formatBytes, formatCount } from "@/lib/format";
import type { RegistryInfo, RegistryStatDto } from "@/client/types.gen";

const props = defineProps<{
  selectedRegistry: string | null;
  allRegistries: RegistryInfo[];
  registryStats: Map<string, RegistryStatDto>;
  countsKnown: boolean;
  totalPackages: number;
}>();

const { t } = useI18n();

// ── The specimen (RFC 0004-bis §14.9) ────────────────────────────────────────
//
// `ui/design-proof/index.html` spends the Display step on **the registry you
// are looking at**, so the page announces its subject. This page spent 24px on
// the word "Packages" — a label on a door — because `--t-display` was mapped to
// no utility and `text-2xl` was the largest step that existed.
//
// The subject is the selected registry, or the instance itself when the facet
// is on "all": "every registry" is still an answer to "what am I looking at",
// and a blank specimen would be the §2.4 defect in a headline.

/** The registry whose name the Display step carries, or null for "all". */
const specimenRegistry = computed(() =>
  props.selectedRegistry
    ? (props.allRegistries.find((r) => r.name === props.selectedRegistry) ?? null)
    : null,
);

/**
 * The caption's facts, in the proof's order — *"hybrid · registry.npmjs.org ·
 * 1,284 packages · 6.1 GB cached"*: how it runs, what is behind it, how much of
 * it there is, how much of it we hold.
 *
 * The last two used to be missing outright. `upstream` was not on
 * `/api/v1/registries` at all, and the cached size was not on any endpoint this
 * page calls — a previous pass noted it and left the fact out rather than
 * estimate it, which was the right call and the wrong resting place. Both are
 * now served, so the caption is the proof's four facts rather than two of them.
 *
 * Still only what the API actually returned: each fact is pushed on its own
 * condition, so a registry with no upstream (local mode) or an instance whose
 * stats query failed drops that item instead of printing a zero.
 */
const specimenFacts = computed<string[]>(() => {
  const reg = specimenRegistry.value;
  const facts: string[] = [];
  if (reg) {
    facts.push(reg.type, reg.mode);
    if (reg.upstream) facts.push(reg.upstream);
    if (props.countsKnown) {
      const stat = props.registryStats.get(reg.name);
      facts.push(
        t("packageCatalog.packageCount", {
          count: formatCount(stat?.package_count ?? 0),
        }),
      );
      // `null` is "we never recorded sizes", which is not "0 B".
      if (stat?.cached_bytes != null) {
        facts.push(t("packageCatalog.cachedSize", { size: formatBytes(stat.cached_bytes) }));
      }
    }
  } else if (props.countsKnown) {
    facts.push(
      t("packageCatalog.registryCount", { count: props.allRegistries.length }),
      t("packageCatalog.packageCount", { count: formatCount(props.totalPackages) }),
    );
    if (totalCachedBytes.value != null) {
      facts.push(t("packageCatalog.cachedSize", { size: formatBytes(totalCachedBytes.value) }));
    }
  }
  return facts;
});

/** Summed only over the registries that reported a size; `null` when none did,
    so "nobody recorded any sizes" never renders as "0 B held". */
const totalCachedBytes = computed<number | null>(() => {
  let total: number | null = null;
  for (const stat of props.registryStats.values()) {
    if (stat.cached_bytes != null) total = (total ?? 0) + stat.cached_bytes;
  }
  return total;
});
</script>

<template>
  <section class="relative overflow-hidden border-b border-rule-soft pb-6 pt-4">
    <div class="plate" aria-hidden="true"></div>
    <!-- The proof's `.display` rule, ported rather than approximated:
         `line-height:.92`, `letter-spacing:.02em`, uppercase. The body
         line-height is 1.625, which on a 104px element costs 169px per line
         — a two-line registry name came to 312px of headline before the
         measurement caught it. A display face is set tight; that is most of
         what makes it read as display rather than as a very large heading. -->
    <h1
      class="relative z-10 font-display font-bold uppercase tracking-[0.02em] leading-[0.92] text-display break-words"
      data-testid="specimen-name"
    >
      {{ specimenRegistry?.name ?? t("packageCatalog.allRegistriesTitle") }}
    </h1>
    <p
      v-if="specimenFacts.length"
      class="relative z-10 mt-3 border-t border-border pt-3 text-sm max-w-[72ch]"
    >
      <template v-for="(fact, i) in specimenFacts" :key="fact">
        <span v-if="i > 0" class="px-2 text-border" aria-hidden="true">·</span>
        <span class="text-foreground">{{ fact }}</span>
      </template>
    </p>
  </section>
</template>
