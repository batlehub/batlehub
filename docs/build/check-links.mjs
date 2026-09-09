#!/usr/bin/env node
/**
 * Every cross-reference in the documentation resolves, and no published page is
 * unreachable (RFC 0005 phase 8).
 *
 * Nothing has ever checked this, and two references were already dead when the
 * RFC was written: `incident-response.md` pointed at `docs/post-mortem-template.md`
 * and `soc2-checklist.md` at `docs/monitoring.md`, neither of which existed.
 * VitePress's own dead-link check would not have found either, and that is the
 * gap this fills rather than duplicates:
 *
 *   - VitePress checks markdown links in *published* pages only. This also
 *     checks `internal/` — excluded from the build, still read by people — and
 *     the repository's own markdown (README, CLAUDE.md, CONTRIBUTING.md, …),
 *     which is where most links into the documentation actually live.
 *
 *   - VitePress checks `[text](target)`. Both of the already-dead references
 *     above were written as inline code, `docs/monitoring.md`, which is a link
 *     in every sense except the syntactic one. Any code span that contains a
 *     slash and ends in `.md` is treated as a path and checked.
 *
 *   - VitePress cannot know what an orphan is. A page that no sidebar, nav
 *     entry or other page links to is published and unreachable, which is the
 *     same defect as a dead link seen from the other end.
 *
 *   - VitePress does not know about the second tree. `/fr/` is a translation of
 *     the site, not a second site, and the two ways it can go wrong are both
 *     silent: a French page that links `/guide/caching` when
 *     `/fr/guide/caching` exists drops the reader back into English mid-page,
 *     and a French page that links `/fr/guide/x` where no translation exists
 *     is a 404, because VitePress falls back to nothing. The first is checked
 *     here; the second is the ordinary dead-link check, which resolves
 *     `/fr/…` against `docs/fr/` like any other path.
 *
 *   node build/check-links.mjs
 */
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, dirname, resolve, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

import * as navEn from "../.vitepress/nav/en.ts";
import * as navFr from "../.vitepress/nav/fr.ts";

const DOCS = fileURLToPath(new URL("..", import.meta.url));
const REPO = resolve(DOCS, "..");

/** Directories that hold no documentation, or hold generated copies of it. */
const SKIP = new Set([
  "node_modules",
  "target",
  ".git",
  "dist",
  "coverage",
  ".vitepress",
  ".desloppify",
  ".claude",
  ".impeccable",
  "fuzz",
]);

function markdownFiles(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP.has(entry)) continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) markdownFiles(full, acc);
    else if (entry.endsWith(".md")) acc.push(full);
  }
  return acc;
}

/**
 * Files that are not documentation but point at it.
 *
 * A path to a page is a link wherever it is written, and the two that had gone
 * stale were not in markdown at all: `config.example.toml` sent a reader to
 * `docs/configuration.md` and `mise.toml` to `docs/cli.md`, months after both
 * moved. Neither was reachable by a checker that only opened `.md`, which is
 * the same shape as the code-span gap this file already closes one level down.
 */
const ALSO_SCAN = /\.(toml|ya?ml|rs|sh|ts|mjs|js|json|properties|txt)$/;

function pathBearingFiles(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP.has(entry)) continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) pathBearingFiles(full, acc);
    else if (ALSO_SCAN.test(entry)) acc.push(full);
  }
  return acc;
}

/** Published pages: every markdown file under docs/ that VitePress builds. */
function publishedPages() {
  return markdownFiles(DOCS)
    .filter((f) => !relative(DOCS, f).startsWith("internal" + sep))
    .filter((f) => !relative(DOCS, f).startsWith("build" + sep));
}

/**
 * Turn a link target into the file it names, or null if it names nothing.
 *
 * VitePress accepts four spellings of the same page — `./foo.md`, `./foo`,
 * `/guide/foo`, and a directory whose `index.md` is implied — so a checker that
 * only understood one of them would report as broken exactly the links that
 * work.
 */
function resolveTarget(fromFile, target) {
  const clean = target.split("#")[0].split("?")[0];
  if (!clean) return { ok: true, file: fromFile };

  const inDocs = !relative(DOCS, fromFile).startsWith("..");
  // Root-absolute inside the site means the site root; outside it, the repo.
  const root = inDocs ? DOCS : REPO;
  const base = clean.startsWith("/") ? join(root, clean) : resolve(dirname(fromFile), clean);

  const candidates = [
    base,
    base + ".md",
    join(base, "index.md"),
    // A repo-root reference to a page by its published path, e.g. a README
    // pointing at `docs/guide/installation.md`.
    base.replace(/\.html$/, ".md"),
  ];
  const file = candidates.find((c) => existsSync(c));
  return { ok: Boolean(file), file };
}

/* ────────────────────────────────────────────────────────────────────────────
   Anchors.

   RFC 0005 phase 8 established that every cross-reference resolves — to a
   *file*. Nothing read the fragment, and two were already wrong: README.md
   linked twice to `configuration.md#9-self-hosted--private-registries` after
   that section was renumbered to 10, so the repository's front page sent
   readers to the top of an 87-minute page and left them to find the section.

   The slug rules are VitePress's own, transcribed rather than approximated —
   `slugify` in its `chunk-*.js`, which is `@mdit-vue/shared`'s. Approximating
   would produce a gate that is wrong in exactly the cases that matter: a
   heading with a colon, an em dash, or a leading digit. The `^(\d)` → `_$1`
   rule is why `## 3.7 [otel]` is reached as `#_3-7-otel-optional`.
   ──────────────────────────────────────────────────────────────────────────── */
const R_CONTROL = /[\u0000-\u001f]/g;
const R_SPECIAL = /[\s~`!@#$%^&*()\-_+=[\]{}|\\;:"'“”‘’<>,.?/]+/g;
const R_COMBINING = /[\u0300-\u036F]/g;

function slugify(str) {
  return str
    .normalize("NFKD")
    .replace(R_COMBINING, "")
    .replace(R_CONTROL, "")
    .replace(R_SPECIAL, "-")
    .replace(/-{2,}/g, "-")
    // Single-character anchors, not `^-+|-+$`: the run has just been collapsed,
    // so one dash is all there can be at either end, and `-+$` is the shape that
    // rescans a long dash run from every start position.
    .replace(/^-/, "")
    .replace(/-$/, "")
    .replace(/^(\d)/, "_$1")
    .toLowerCase();
}

/**
 * Strips HTML tags, repeating until the result stops changing.
 *
 * Deleting a match can splice its neighbours into a new one, so a single
 * `replace` sanitises only what survives one pass — the incomplete
 * multi-character sanitisation code scanning flagged here. Today's pattern
 * happens to be its own fixed point (every `<` that has a later `>` is consumed
 * left to right, so nothing can re-form), but that is a property of this exact
 * regex rather than of the function, and it is not what the next person editing
 * the regex will check. The loop makes "no tags left" the postcondition.
 */
function stripTags(text) {
  let out = text;
  for (let prev; prev !== out; ) {
    prev = out;
    // `[^<>]`, not `[^>]`: excluding `<` from the body means a failed attempt
    // cannot run past the next `<`, so the scan stays linear — and a tag cannot
    // contain `<` anyway. Re-formation is what the fixed-point loop is for.
    out = out.replace(/<[^<>]*>/g, "");
  }
  return out;
}

/**
 * The anchors a page offers. `markdown-it-anchor` slugifies the heading's
 * *rendered text*, so the inline markup goes first; an explicit `{#id}` wins
 * outright, and a repeated slug gets `-1`, `-2`, … the way the plugin numbers
 * them.
 */
const anchorCache = new Map();
function anchorsOf(file) {
  if (anchorCache.has(file)) return anchorCache.get(file);
  const src = readFileSync(file, "utf8").replace(/```[\s\S]*?```/g, "");
  const seen = new Map();
  const anchors = new Set();
  // `[ \t]+(\S.*)` rather than `\s+(.+?)\s*$`: `\s` and `.` both match a space,
  // and that overlap is what makes the lazy group backtrack super-linearly. The
  // trailing whitespace `\s*$` used to absorb comes off in JS instead.
  for (const [, , headingLine] of src.matchAll(/^(#{1,6})[ \t]+(\S.*)$/gm)) {
    const raw = headingLine.trimEnd();
    const explicit = /\{#([^}]+)\}\s*$/.exec(raw);
    let slug;
    if (explicit) {
      slug = explicit[1];
    } else {
      const text = raw
        // `[^\][]`/`[^()]`: a class that excludes its own opener cannot rescan
        // across the next one, which is what keeps these two linear.
        .replace(/\[([^\][]*)\]\([^()]*\)/g, "$1")
        // Tag-stripping happens outside code spans only: `<name>` in
        // `registry info <name>` is a placeholder a reader types, not markup,
        // and VitePress keeps it — which is the difference between
        // `#registry-info-name` and `#registry-info`.
        .split("`")
        .map((seg, i) => (i % 2 ? seg : stripTags(seg).replace(/[*~]/g, "")))
        .join("")
        .trim();
      slug = slugify(text);
      const n = seen.get(slug) ?? 0;
      seen.set(slug, n + 1);
      if (n) slug = `${slug}-${n}`;
    }
    anchors.add(slug);
  }
  anchorCache.set(file, anchors);
  return anchors;
}

/**
 * Documents that record a state of the world rather than describe the current
 * one, and the *narrow* thing they are excused from.
 *
 * An RFC quoting `docs/configuration.md` is quoting the tree as it stood when
 * the argument was made; RFC 0005 quotes `docs/monitoring.md` and
 * `docs/post-mortem-template.md` precisely because they did not exist, which is
 * the defect it is reporting. "Fixing" any of those would falsify the record,
 * and a checker that demanded it would be asking for the wrong thing.
 *
 * So the exemption is by *kind of reference*, not by file. These documents are
 * excused from the code-span check — a quotation — and are still held to the
 * markdown-link check, because a link is something a reader clicks and a stale
 * one is broken whatever the document is. `rfc-0005-merge-conflicts.md` is the
 * exception to the exception: it is a generated `diff` of deleted files, so
 * even its `[text](target)` links are quotations of text that no longer exists.
 */
const QUOTES_HISTORY = [/^CHANGELOG\.md$/, /^todo\.md$/, /^docs\/rfc\//];
const GENERATED = [
  /^docs\/internal\/rfc-0005-bis-publishing-overlap\.md$/,
  /^docs\/internal\/rfc-0005-merge-conflicts\.md$/,
  /^docs\/internal\/rfc-0005-bis-sbom-overlap\.md$/,
];

const findings = [];
const linkedFromPages = new Set();

/**
 * Nav and sidebar entries, from both locales.
 *
 * Imported, not scraped. These used to be literals inside `config.ts` and this
 * check read them with a regex; they are now a module per locale, which
 * `config.ts` imports too. A regex over a literal that has moved matches
 * nothing and reports every page in the tree as an orphan — a gate that fails
 * loudly for the wrong reason is the one thing worse than a gate that passes
 * quietly for the wrong reason.
 */
function navLinks(node, acc = []) {
  if (Array.isArray(node)) {
    for (const item of node) navLinks(item, acc);
    return acc;
  }
  if (node && typeof node === "object") {
    if (typeof node.link === "string") acc.push(node.link);
    if (node.items) navLinks(node.items, acc);
  }
  return acc;
}
const reachable = new Set([
  ...navLinks(navEn.nav),
  ...navLinks(Object.values(navEn.sidebar)),
  ...navLinks(navFr.nav),
  ...navLinks(Object.values(navFr.sidebar)),
]);

for (const file of markdownFiles(REPO)) {
  const src = readFileSync(file, "utf8");
  const rel = relative(REPO, file).split(sep).join("/");
  if (GENERATED.some((p) => p.test(rel))) continue;

  // A record may point at an anchor that was accurate when it was written; the
  // same argument as the code-span carve-out below.
  const quotes = QUOTES_HISTORY.some((p) => p.test(rel));

  // Markdown links, minus the ones that do not name a file in this repository.
  for (const [, , target] of src.matchAll(/\[([^\][]*)\]\(([^()\s]+)\)/g)) {
    if (/^(https?:|mailto:|<)/.test(target)) continue;
    const { ok, file: targetFile } = resolveTarget(file, target);
    if (!ok) {
      findings.push({ rel, kind: "dead link", target });
      continue;
    }
    if (target.startsWith("/")) linkedFromPages.add(target.split("#")[0]);

    // A French page linking into the English tree. Legitimate exactly when
    // there is no French page to link — `contributing/`, `rfc/` and the
    // generated roadmap are not translated, and the fallback is the decision
    // recorded in `nav/fr.ts`. It is a defect when the translation exists,
    // because the reader is then dropped back into English by a link they had
    // no way to see coming.
    if (
      rel.startsWith("docs/fr/") &&
      target.startsWith("/") &&
      !target.startsWith("/fr/")
    ) {
      const translated = resolveTarget(file, "/fr" + target.split("#")[0]);
      if (translated.ok) {
        findings.push({
          rel,
          kind: "English link where a translation exists",
          target: `${target} — say /fr${target.split("#")[0]}`,
        });
      }
    }

    // The fragment. A `#` that names no heading lands the reader at the top of
    // the page it was meant to skip to — silently, which is why two of these
    // survived on the repository's front page.
    const fragment = target.split("#")[1];
    if (!fragment || quotes || !targetFile?.endsWith(".md")) continue;
    if (!anchorsOf(targetFile).has(fragment)) {
      findings.push({
        rel,
        kind: "dead anchor",
        target: `${relative(REPO, targetFile).split(sep).join("/")}#${fragment}`,
      });
    }
  }

  // Inline code that is a path to a markdown file. Both of the references this
  // gate was written for were written this way, so the syntactic definition of
  // "link" is the one thing it must not adopt.
  if (QUOTES_HISTORY.some((p) => p.test(rel))) continue;
  for (const [, span] of src.matchAll(/`([^`\n]+)`/g)) {
    if (!span.includes("/") || !span.endsWith(".md")) continue;
    if (/[\s*<>|]/.test(span)) continue;
    // A code span is written for a reader of the repository, so it is a
    // repo-relative path far more often than a page-relative one — but both
    // spellings occur, and neither is wrong. Either resolving is a pass.
    const fromRepo = existsSync(join(REPO, span));
    if (!fromRepo && !resolveTarget(file, span).ok) {
      findings.push({ rel, kind: "dead path in code span", target: span });
    }
  }
}

/* Paths to documentation written outside documentation — a config example's
   comment, a tool's manifest, a script's header. Only `docs/…md` shapes: a bare
   filename in a shell script is not a claim about this tree. The scripts in
   `docs/build/` are exempt because they quote the dead references they exist to
   catch, which is the same carve-out records get above. */
for (const file of pathBearingFiles(REPO)) {
  const rel = relative(REPO, file).split(sep).join("/");
  if (rel.startsWith("docs/build/")) continue;
  const src = readFileSync(file, "utf8");
  for (const [, target] of src.matchAll(/\b(docs\/[\w./-]+\.md)\b/g)) {
    if (!existsSync(join(REPO, target))) {
      findings.push({ rel, kind: "dead path outside markdown", target });
    }
  }
}

// Orphans. A published page nothing points at is unreachable, which is a dead
// link seen from the other end.
for (const page of publishedPages()) {
  const rel = relative(DOCS, page).split(sep).join("/");
  // Each locale's home page is its entry point: nothing links to it because it
  // is where the reader arrives, and the language switcher moves between them.
  if (rel === "index.md" || rel === "fr/index.md") continue;
  // A stub is a redirect this host cannot otherwise issue (RFC 0005-bis §6.5).
  // It is deliberately in no sidebar — it exists for an address, not a reader —
  // so it is not an orphan. Its target is checked like any other link above,
  // which is what stops a stub outliving the page it points at.
  const head = readFileSync(page, "utf8").slice(0, 400);
  if (/^moved:\s*\S/m.test(head)) continue;
  // `guide/index.md` → `/guide/`, `guide/install.md` → `/guide/install`: the
  // directory form keeps the trailing slash the router serves it under, which
  // dropping `index` already leaves in place.
  const url = "/" + rel.replace(/(index)?\.md$/, "");
  const bare = "/" + rel.replace(/\.md$/, "");
  if (
    !reachable.has(url) &&
    !reachable.has(bare) &&
    !linkedFromPages.has(url) &&
    !linkedFromPages.has(bare)
  ) {
    findings.push({ rel: "docs/" + rel, kind: "orphan page", target: bare });
  }
}

if (findings.length) {
  console.error(`${findings.length} finding(s):\n`);
  for (const f of findings) console.error(`  ${f.kind}: ${f.rel} → ${f.target}`);
  process.exit(1);
}
console.log("every documentation cross-reference resolves, and no page is orphaned");
