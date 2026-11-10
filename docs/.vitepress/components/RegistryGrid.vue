<script setup lang="ts">
import { computed } from "vue";
import { useData, withBase } from "vitepress";

/**
 * The registries index's directory: every registry page, grouped, one tile
 * each. A reader arrives here looking for *their* package manager; the tables
 * below answer what each one proxies, this answers where to click.
 *
 * Read from the locale's own `/registries/` sidebar rather than from a list of
 * its own, so a new registry page appears here the moment it is in the
 * sidebar — which `docs:audience` already requires — and a second list cannot
 * drift from the first. The first sidebar group is the overview itself, which
 * is this page, so it is skipped.
 */
type Item = { text: string; link: string };
type Group = { text: string; items: Item[] };

const { theme } = useData();

const all = computed<Group[]>(() => {
  const sidebar = (theme.value.sidebar ?? {}) as Record<string, Group[]>;
  const key = Object.keys(sidebar).find((k) => k.endsWith("/registries/"));
  return key ? sidebar[key] : [];
});
const groups = computed(() => all.value.slice(1));
// The overview group's own title ("Registries" / "Registres") names the nav,
// so the landmark is announced in the page's language.
const label = computed(() => all.value[0]?.text ?? "Registries");

// `/registries/npm` → `npm`: the page slug is the registry's config `type`.
const type = (link: string) => link.split("/").filter(Boolean).pop();
</script>

<template>
  <nav class="registry-grid" :aria-label="label">
    <section v-for="group in groups" :key="group.text">
      <h2 class="label">{{ group.text }}</h2>
      <ul>
        <li v-for="item in group.items" :key="item.link">
          <!-- cleanUrls is off: a bare path 404s on a host that does not guess the extension. -->
          <a :href="withBase(`${item.link}.html`)">
            <span class="name">{{ item.text }}</span>
            <code class="type">{{ type(item.link) }}</code>
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
.registry-grid {
  margin: var(--s5) 0 var(--s6);
}

.registry-grid section + section {
  margin-top: var(--s5);
}

/* A standalone section label: Meta, 0.16em. Not a document heading in the
   page's outline (the h2 is scoped out of `.vp-doc`'s heading rules below). */
.vp-doc .registry-grid .label {
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

.registry-grid ul {
  list-style: none;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
  gap: var(--s2);
  margin: 0;
  padding: 0;
}

.vp-doc .registry-grid li {
  margin: 0;
  max-width: none;
}

.vp-doc .registry-grid a {
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

.vp-doc .registry-grid a:hover {
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
.vp-doc .registry-grid .type {
  align-self: flex-start;
  font-size: var(--t-meta);
  color: var(--ink-dim);
  border-color: var(--rule-soft);
}
</style>
