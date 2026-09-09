#!/usr/bin/env node
/**
 * The shape of a page, measured (RFC 0005-bis §4.5, The Structure Is Checked
 * Rule).
 *
 * Every gate this project owns reads *rendering*. None read structure, and the
 * biggest page in the documentation had drifted inside itself for long enough
 * that nobody remembers: `guide/configuration.md` carried a subsection titled
 * `### 6.16 Corporate HTTP Proxy (air-gapped environments)` between
 * `## 11. SBOM Generation` and its appendix. Either the number was wrong or the
 * placement was, and no build, no link check and no rendered pass could tell.
 * That is the same shape as RFC 0005's dead token copy: a fact nothing read.
 *
 * Three assertions, and one report.
 *
 *   depth       No heading deeper than h4. Past that the outline stops being a
 *               structure and becomes an index of an index.
 *
 *   numbering   A numbered heading's prefix must match the numbered section it
 *               sits under, and its last component must advance. This is the
 *               one that finds a 6.16 under an 11.
 *
 *   length      Over 4 000 words, a page declares `reference: true`. Not a cap
 *               — a reference is long because its subject is — but an exception
 *               someone had to type, and can therefore argue with. `rfc/` is
 *               out of scope for this one: an RFC is a record read once in
 *               order, not a page consulted, and `check-links.mjs` already
 *               carves records out for the same reason. Depth and numbering
 *               still apply there, because a record filed under the wrong
 *               number is a record that misleads.
 *
 *   node build/check-structure.mjs            # gate
 *   node build/check-structure.mjs --report   # words, minutes, headings, depth
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const DOCS = fileURLToPath(new URL("..", import.meta.url));
const REPORT = process.argv.includes("--report");

const MAX_DEPTH = 4;
const DECLARE_ABOVE = 4000;
/** DESIGN.md's Reading role is 16px/1.7; 180 wpm is the usual figure for
 *  technical prose and is only ever used to make a number legible. */
const WORDS_PER_MINUTE = 180;

const SKIP_DIRS = new Set(["node_modules", ".vitepress", "build", "public"]);

/** An RFC, in any locale that has one. A record read once in order, not a page
 *  consulted — see the `length` note in the header. */
const isRecord = (rel) => /^(fr\/)?rfc\//.test(rel);

function pages(dir = DOCS, acc = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) pages(full, acc);
    else if (entry.endsWith(".md")) acc.push(full);
  }
  return acc;
}

/** Frontmatter is read as text on purpose: this script has no dependencies, and
 *  the only key it needs is a boolean on its own line. */
function declaresReference(src) {
  const fm = src.match(/^---\n([\s\S]*?)\n---/);
  return Boolean(fm && /^reference:\s*true\s*$/m.test(fm[1]));
}

const findings = [];
const rows = [];

for (const file of pages()) {
  const rel = relative(DOCS, file).split(sep).join("/");
  if (rel.startsWith("internal/")) continue;

  const src = readFileSync(file, "utf8");
  const prose = src.replace(/```[\s\S]*?```/g, "");
  const words = prose.split(/\s+/).filter(Boolean).length;

  // `[ \t]+(\S.*)` rather than `\s+(.+?)\s*$`: `\s` and `.` overlap on a space,
  // and a lazy group between two overlapping classes backtracks super-linearly.
  const headings = [...prose.matchAll(/^(#{1,6})[ \t]+(\S.*)$/gm)].map((m) => ({
    level: m[1].length,
    text: m[2].trimEnd(),
  }));
  const depth = headings.reduce((d, h) => Math.max(d, h.level), 0);

  rows.push({ rel, words, headings: headings.length, depth });

  if (depth > MAX_DEPTH) {
    findings.push({ rel, kind: "heading depth", detail: `h${depth}` });
  }

  // Measured per page, in the language the page is written in. French runs
  // longer than English for the same content — around 15% — so a page that sits
  // just under the threshold in English can cross it once translated. That is
  // the answer rather than a defect in it: the declaration is about how long
  // this page is to read, and the French page really is longer.
  if (words > DECLARE_ABOVE && !declaresReference(src) && !isRecord(rel)) {
    findings.push({
      rel,
      kind: "undeclared length",
      detail: `${words} words and no \`reference: true\``,
    });
  }

  // Numbering. `## 6. Worked Examples` then `### 6.5 …` is well formed;
  // `## 11. SBOM` then `### 6.16 …` is not, and neither is a sibling that goes
  // backwards.
  const openNumber = []; // number parts of the innermost numbered ancestor, by level
  const lastSibling = new Map(); // "level|parent" → last last-component seen
  for (const h of headings) {
    const num = /^(\d+(?:\.\d+)*)\.?\s/.exec(h.text)?.[1];
    if (!num) continue;
    const parts = num.split(".");
    const parent = parts.slice(0, -1).join(".");
    const label = `${num} ${h.text.replace(/^\S+\s+/, "").slice(0, 48)}`;

    if (parent) {
      const enclosing = openNumber[h.level - 1];
      if (enclosing && enclosing !== parent) {
        findings.push({
          rel,
          kind: "numbering",
          detail: `\`${label}\` sits under section ${enclosing}`,
        });
      }
    }

    const key = `${h.level}|${parent}`;
    const last = Number(parts.at(-1));
    const prev = lastSibling.get(key);
    if (prev !== undefined && last <= prev) {
      findings.push({
        rel,
        kind: "numbering",
        detail: `\`${label}\` does not follow ${parent ? parent + "." : ""}${prev}`,
      });
    }
    lastSibling.set(key, last);

    openNumber[h.level] = num;
    for (let l = h.level + 1; l < openNumber.length; l++) openNumber[l] = undefined;
  }
}

if (REPORT) {
  rows.sort((a, b) => b.words - a.words);
  console.log(
    `${"words".padStart(6)} ${"mins".padStart(5)} ${"hdgs".padStart(5)} ${"depth".padStart(5)}  page`,
  );
  for (const r of rows) {
    console.log(
      `${String(r.words).padStart(6)} ${String(Math.round(r.words / WORDS_PER_MINUTE)).padStart(5)} ` +
        `${String(r.headings).padStart(5)} ${String(r.depth).padStart(5)}  ${r.rel}`,
    );
  }
  const total = rows.reduce((n, r) => n + r.words, 0);
  console.log(
    `\n${rows.length} published pages · ${total} words · ${Math.round(total / WORDS_PER_MINUTE)} minutes`,
  );
  process.exit(0);
}

if (findings.length) {
  console.error(`${findings.length} finding(s):\n`);
  for (const f of findings) console.error(`  ${f.kind}: ${f.rel} — ${f.detail}`);
  process.exit(1);
}
console.log("every page's structure holds: depth, numbering, and declared length");
