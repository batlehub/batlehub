#!/usr/bin/env node
/**
 * One space, one reader — enforced (RFC 0005-bis §4.4, §5.1).
 *
 * RFC 0005 sorted the documentation by who reads it, at the top level, and put
 * nothing below that to hold the line. `guide/` re-acquired both audiences
 * inside a year: 25 links in one sidebar mixing "deploy this behind Postgres"
 * with "point npm at it". A rule with no call site is a rule nobody adopted,
 * which is the finding RFC 0005 kept making about itself.
 *
 * Five assertions. `check-links.mjs` already owns the orphan half — a page in
 * *no* sidebar — and this owns its mirror image and the counts.
 *
 *   one sidebar     A page listed in two sidebars will be edited for one reader
 *                   and read by the other. Scoped to a locale: `/guide/roadmap`
 *                   is in the English guide sidebar and in the French one too,
 *                   because the roadmap is generated from `ROADMAP.md` and is
 *                   not translated. Those are two lists for two readers, not
 *                   one page filed twice.
 *
 *   no loops        A "See also" must not point at an index that lists the page
 *                   it is on. Twenty-one registry pages ended with
 *                   "User Guide → npm", which led to a table of links back to
 *                   the registry pages. A link that returns you to where you
 *                   started is not a link.
 *
 *   sidebar size    A sidebar is one person's list, and a list nobody can hold
 *                   in their head is a list they scroll past.
 *
 *   no typed TOC    The theme draws the outline from the headings. Sixteen
 *                   pages carried a second one, and the nine that were typed by
 *                   hand had every numbered entry dead, because VitePress
 *                   prefixes a leading digit with `_`.
 *
 *   locale shape    A French sidebar entry points inside `/fr/`, unless the page
 *                   it names is one of the three the project decided not to
 *                   translate. Getting this wrong is invisible in review and a
 *                   404 in the browser, because VitePress does not fall back.
 *
 *   node build/check-audience.mjs
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

import * as en from "../.vitepress/nav/en.ts";
import * as fr from "../.vitepress/nav/fr.ts";

const DOCS = fileURLToPath(new URL("..", import.meta.url));
/**
 * A ratchet, not a target. The measured defect was 25 links in one sidebar
 * mixing two audiences; the split left `/guide/` at 20 and created `/use/` at
 * 10. Twenty may only go down — the number exists so the next page has to
 * displace one rather than join the pile.
 */
const MAX_SIDEBAR = 20;

/**
 * A catalogue is one entry per thing catalogued, and its length is a property
 * of the domain rather than of anyone's editing. `/registries/` has 21 registry
 * types because BatleHub supports 21, and `/rfc/` has one entry per RFC ever
 * written — that list only grows, and the way to shorten it would be to stop
 * publishing design history. The rule this file enforces is one audience per
 * sidebar, and the size cap is a proxy for it that does not apply where the
 * list is an index.
 */
const CATALOGUE = new Set(["/registries/", "/rfc/", "/fr/registries/"]);
const HOME_CARDS = 3;
/** A page with this many links into one directory is an index of it. */
const INDEX_THRESHOLD = 10;

/**
 * The locales, and the navigation each one publishes. Imported rather than
 * scraped out of `config.ts`: these are ordinary modules, `config.ts` imports
 * the same two, and a regex over a literal that has moved matches nothing and
 * reports every page as an orphan instead of failing.
 */
const LOCALES = [
  { code: "root", prefix: "/", nav: en.nav, sidebar: en.sidebar },
  { code: "fr", prefix: "/fr/", nav: fr.nav, sidebar: fr.sidebar },
];

/**
 * What the French navigation may point at outside `/fr/`, and why.
 *
 * The three spaces the project decided not to translate: `contributing/` and
 * `rfc/` are read by people changing the code, and the roadmap page is
 * generated from `ROADMAP.md`, which is canonical and English. Linking them at
 * their English URL is the decided behaviour — see `nav/fr.ts`. Anything else
 * outside `/fr/` is a prefix someone forgot.
 */
const UNTRANSLATED = [/^\/contributing\//, /^\/rfc\//, /^\/guide\/roadmap$/];

const findings = [];
const sizes = [];
const add = (kind, detail) => findings.push({ kind, detail });

/** Every `link:` in a nav tree, however deeply the dropdowns nest. */
function links(node, acc = []) {
  if (Array.isArray(node)) {
    for (const item of node) links(item, acc);
    return acc;
  }
  if (node && typeof node === "object") {
    if (typeof node.link === "string") acc.push(node.link);
    if (node.items) links(node.items, acc);
  }
  return acc;
}

/* ── One sidebar per page, within a locale ────────────────────────────────── */

for (const locale of LOCALES) {
  const listedIn = new Map(); // link → [sidebar key, …]

  for (const [key, tree] of Object.entries(locale.sidebar)) {
    const entries = links(tree);
    sizes.push(`${key} ${entries.length}`);
    if (entries.length > MAX_SIDEBAR && !CATALOGUE.has(key)) {
      add(
        "sidebar too long",
        `${key} — ${entries.length} links, over ${MAX_SIDEBAR}`,
      );
    }
    for (const l of entries) {
      if (!listedIn.has(l)) listedIn.set(l, []);
      listedIn.get(l).push(key);
    }
  }

  for (const [link, keys] of listedIn) {
    if (keys.length > 1) {
      add(
        "two sidebars",
        `${link} — listed in ${keys.join(" and ")} (${locale.code})`,
      );
    }
  }

  // A sidebar key and every link under it belong to their locale's prefix. The
  // exception is deliberate and enumerated: see UNTRANSLATED.
  if (locale.code === "root") continue;
  for (const key of Object.keys(locale.sidebar)) {
    if (!key.startsWith(locale.prefix)) {
      add("sidebar outside its locale", `${key} — expected ${locale.prefix}…`);
    }
  }
  for (const link of [...links(locale.nav), ...links(Object.values(locale.sidebar))]) {
    if (!link.startsWith("/") || link.startsWith(locale.prefix)) continue;
    if (UNTRANSLATED.some((p) => p.test(link))) continue;
    add(
      "link outside its locale",
      `${link} — in the ${locale.code} navigation, and not one of the pages ` +
        `that stay in English`,
    );
  }
}

/* ── No "See also" that points at an index of this page ───────────────────── */

const SKIP_DIRS = new Set(["node_modules", ".vitepress", "build", "public", "internal"]);
function pages(dir = DOCS, acc = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) pages(full, acc);
    else if (entry.endsWith(".md")) acc.push(full);
  }
  return acc;
}

const all = pages();
const urlOf = (file) =>
  "/" + relative(DOCS, file).split(sep).join("/").replace(/(index)?\.md$/, "");
const textOf = new Map(all.map((f) => [f, readFileSync(f, "utf8")]));

/** The heading a "See also" section carries, in each locale that has pages. */
const SEE_ALSO = /^## (?:See also|Voir aussi)\n/m;

for (const file of all) {
  const src = textOf.get(file);
  // Split rather than a lazy `[\s\S]*?` with a `(?=^## |\Z)` lookahead: JS has no
  // `\Z`, so that alternative was matching a literal "Z" and the section could
  // only ever be ended by a following `## ` — a "See also" that ran to the end of
  // the page was skipped entirely.
  // Test the split's arity rather than comparing `parts[1] === undefined`: the
  // element type is `string`, so that comparison reads as always-false to a
  // static analyser (sonar javascript:S3403) even though the index really is
  // out of range on a page with no "See also".
  const parts = src.split(SEE_ALSO);
  if (parts.length < 2) continue;
  const seeAlso = parts[1].split(/^## /m)[0];
  const here = urlOf(file);
  const dir = here.slice(0, here.lastIndexOf("/") + 1);

  // The whole `(…)` destination in one linear capture, then trimmed at the
  // fragment. Two adjacent greedy classes that both accept `#` backtrack.
  for (const [, dest] of seeAlso.matchAll(/\]\(([^)]*)\)/g)) {
    const target = dest.split(/[#\s]/)[0];
    if (!target.startsWith("/")) continue;
    const targetFile = all.find((f) => urlOf(f) === target || urlOf(f) === target + "/");
    if (!targetFile) continue;
    const back = [...textOf.get(targetFile).matchAll(/\]\((\/[^)#\s]*)/g)].filter((m) =>
      m[1].startsWith(dir),
    );
    if (back.length >= INDEX_THRESHOLD) {
      add(
        "see-also loop",
        `${relative(DOCS, file)} → ${target}, which links to ${back.length} pages under ${dir}`,
      );
    }
  }
}

/* ── The counts, because "we reduced it" is not a test ────────────────────── */

// One home page per locale, and the same three cards on each: a translation
// that drops a card is a translation that makes a different argument.
for (const home of ["index.md", join("fr", "index.md")]) {
  const src = readFileSync(join(DOCS, home), "utf8");
  const cards = (src.match(/^ {2}- icon:/gm) ?? []).length;
  if (cards !== HOME_CARDS) {
    add("home page", `${home} — ${cards} feature cards, expected ${HOME_CARDS}`);
  }
}

for (const file of all) {
  const src = textOf.get(file);
  if (
    /^## Table of [Cc]ontents$/m.test(src) ||
    /^## Sommaire$/m.test(src) ||
    /^\[\[toc\]\]$/m.test(src)
  ) {
    add("typed table of contents", relative(DOCS, file));
  }
}

/* ── Report ───────────────────────────────────────────────────────────────── */

if (findings.length) {
  console.error(`${findings.length} finding(s):\n`);
  for (const f of findings) console.error(`  ${f.kind}: ${f.detail}`);
  process.exit(1);
}
console.log(
  `each page is in one sidebar of its locale and no "see also" loops back — ` +
    sizes.join(" · "),
);
