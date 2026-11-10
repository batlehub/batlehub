<script setup lang="ts">
import { computed } from "vue";
import { useData, withBase } from "vitepress";

/**
 * A section landing page's directory: every page of the section, grouped, one
 * tile each. On the registries index a reader arrives looking for *their*
 * package manager; the tables below answer what each one proxies, this answers
 * where to click. `/use/` and `/operations/` use it the same way.
 *
 * Read from the locale's own `/<section>/` sidebar rather than from a list of
 * its own, so a new page appears here the moment it is in the sidebar — which
 * `docs:audience` already requires — and a second list cannot drift from the
 * first. The section's own landing page is the one this grid sits on, so its
 * tile is dropped, and a group left empty by that (the overview group of
 * `/registries/` and `/operations/`) goes with it. The `type` caption is only
 * meaningful for registries, where the page slug is the config `type`.
 */
type Item = { text: string; link: string };
type Group = { text: string; items: Item[] };

const props = defineProps<{ section: string }>();
const { theme } = useData();

const all = computed<Group[]>(() => {
  const sidebar = (theme.value.sidebar ?? {}) as Record<string, Group[]>;
  const key = Object.keys(sidebar).find((k) => k.endsWith(`/${props.section}/`));
  return key ? sidebar[key] : [];
});
const groups = computed(() =>
  all.value
    .map((g) => ({ ...g, items: g.items.filter((i) => !i.link.endsWith(`/${props.section}/`)) }))
    .filter((g) => g.items.length),
);
// The first group's own title ("Registries" / "Registres", "Using BatleHub"…)
// names the nav, so the landmark is announced in the page's language.
const label = computed(() => all.value[0]?.text ?? props.section);

// `/registries/npm` → `npm`: the page slug is the registry's config `type`.
const type = (link: string) => link.split("/").filter(Boolean).pop();
</script>

<template>
  <nav class="section-grid" :aria-label="label">
    <section v-for="group in groups" :key="group.text">
      <h2 class="label">{{ group.text }}</h2>
      <ul>
        <li v-for="item in group.items" :key="item.link">
          <!-- cleanUrls is off: a bare path 404s on a host that does not guess the extension. -->
          <a :href="withBase(`${item.link}.html`)">
            <span class="name">{{ item.text }}</span>
            <code v-if="section === 'registries'" class="type">{{ type(item.link) }}</code>
          </a>
        </li>
      </ul>
    </section>
  </nav>
</template>

<style scoped>
/* Tiles are links, so their edge is the strong rule on hover and the soft one
   at rest — no fill (The Undependable Fill Rule), no radius, no shadow. The
   crimson edge is reserved for the *selected* registry, which is the sidebar's
   job, not a hover's. */
.section-grid {
  margin: var(--s5) 0 var(--s6);
}

.section-grid section + section {
  margin-top: var(--s5);
}

/* A standalone section label: Meta, 0.16em. Not a document heading in the
   page's outline (the h2 is scoped out of `.vp-doc`'s heading rules below). */
.vp-doc .section-grid .label {
  font-family: var(--face-text);
  font-size: var(--t-meta);
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.16em;
  color: var(--ink-dim);
  border: 0;
  margin: 0 0 var(--s2);
  padding: 0;
}

.section-grid ul {
  list-style: none;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
  gap: var(--s2);
  margin: 0;
  padding: 0;
}

.vp-doc .section-grid li {
  margin: 0;
  max-width: none;
}

.vp-doc .section-grid a {
  display: flex;
  flex-direction: column;
  gap: var(--s1);
  height: 100%;
  padding: var(--s3) var(--s4);
  border: 1px solid var(--rule-soft);
  color: var(--ink);
  text-decoration: none;
  transition: border-color var(--resolve);
}

.vp-doc .section-grid a:hover {
  border-color: var(--rule-strong);
}

/* Pixel Small: DESIGN.md's step for "the registry name in a list row". */
.name {
  font-family: var(--face-display);
  font-weight: 700;
  font-size: var(--t-px-sm);
  letter-spacing: 0.02em;
  line-height: 1.3;
}

/* Code inside a link takes the link's crimson by default; on 29 tiles that is
   29 red marks for a value that is only a caption. Dim ink, soft rule. */
.vp-doc .section-grid .type {
  align-self: flex-start;
  font-size: var(--t-meta);
  color: var(--ink-dim);
  border-color: var(--rule-soft);
}
</style>
