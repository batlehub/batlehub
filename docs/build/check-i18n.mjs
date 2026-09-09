#!/usr/bin/env node
/**
 * A translation says which page it translates, and at which revision.
 *
 * The failure this exists to prevent has already happened once here, in the
 * other direction: RFC 0005 found four documents maintained in two trees and
 * drifted by up to 296 lines, with nothing watching. A translation is the same
 * arrangement with a second language on top — two files saying the same thing,
 * one of them edited. Nothing about a French page goes red when its English
 * source changes; the page simply becomes wrong, and stays published.
 *
 * So every page under `fr/` declares two keys in its frontmatter:
 *
 *   sourcePath   the English page it translates, relative to `docs/`
 *   sourceHash   the first 16 hex of the SHA-256 of that file, as translated
 *
 * and this gate fails when the source has moved on. The remedy is to bring the
 * translation up to date and re-stamp it (`--stamp`) — never to stamp it alone,
 * which is why the stamp is a separate, deliberate command rather than
 * something the check does for you.
 *
 * Scope is a decision, not an accident: `contributing/`, `rfc/` and the
 * generated `guide/roadmap.md` are not translated. `--status` reports them as
 * out of scope rather than as work outstanding, so the number that is left is
 * the number that means something.
 *
 *   node build/check-i18n.mjs            # gate: every French page is current
 *   node build/check-i18n.mjs --status   # report: translated, stale, missing
 *   node build/check-i18n.mjs --stamp    # re-stamp, after updating the page
 */
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync, existsSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const DOCS = fileURLToPath(new URL("..", import.meta.url));
const LOCALE = "fr";
const LOCALE_DIR = join(DOCS, LOCALE);

const STATUS = process.argv.includes("--status");
const STAMP = process.argv.includes("--stamp");

/** Directories under `docs/` that are not published pages at all. */
const SKIP_DIRS = new Set([
  "node_modules",
  ".vitepress",
  "build",
  "public",
  "internal",
  LOCALE,
]);

/**
 * Pages that stay in English, and why — the §7 of the plan, in code.
 *
 * `contributing/` and `rfc/` are read by people changing this codebase, which
 * is 73% of the corpus written for the smallest and most English-reading
 * audience it has. `guide/roadmap.md` is generated from `ROADMAP.md`, which is
 * canonical: translating the output would create a second canonical roadmap
 * that no gate could keep true.
 */
const OUT_OF_SCOPE = [/^contributing\//, /^rfc\//, /^guide\/roadmap\.md$/];

/**
 * A redirect stub — a page that exists for an address rather than for a reader
 * (RFC 0005-bis §6.5). It holds one sentence and a `meta refresh` to where the
 * content went. The addresses it preserves are English ones that were published
 * before the French tree existed, so there is nothing for a French copy to
 * redirect and a translation of it would be a page nobody can reach.
 */
const isStub = (src) => /^moved:\s*\S/m.test(src.slice(0, 400));

const short = (bytes) => createHash("sha256").update(bytes).digest("hex").slice(0, 16);

function pages(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) pages(full, acc);
    else if (entry.endsWith(".md")) acc.push(full);
  }
  return acc;
}

function translatedPages(dir = LOCALE_DIR, acc = []) {
  if (!existsSync(dir)) return acc;
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) translatedPages(full, acc);
    else if (entry.endsWith(".md")) acc.push(full);
  }
  return acc;
}

const rel = (file) => relative(DOCS, file).split(sep).join("/");

/**
 * The frontmatter block, as text and as the two keys this cares about.
 *
 * Read as text rather than parsed: a YAML dependency for two scalars would be a
 * dependency the rest of these gates do without, and the keys are written by
 * `--stamp` in a shape it controls.
 */
function frontmatter(src) {
  const m = /^---\n([\s\S]*?)\n---/.exec(src);
  if (!m) return null;
  const read = (key) => new RegExp(`^${key}:\\s*(\\S+)\\s*$`, "m").exec(m[1])?.[1];
  return { block: m[1], sourcePath: read("sourcePath"), sourceHash: read("sourceHash") };
}

const findings = [];
const rows = [];
const translated = new Map(); // sourcePath → French page

for (const file of translatedPages()) {
  const here = rel(file);
  const src = readFileSync(file, "utf8");
  const fm = frontmatter(src);

  if (!fm?.sourcePath || !fm.sourceHash) {
    findings.push({
      kind: "undeclared source",
      detail: `${here} — needs \`sourcePath\` and \`sourceHash\` in its frontmatter`,
    });
    continue;
  }

  const source = join(DOCS, fm.sourcePath);
  if (!existsSync(source)) {
    findings.push({
      kind: "source is gone",
      detail: `${here} — translates ${fm.sourcePath}, which no longer exists`,
    });
    continue;
  }

  translated.set(fm.sourcePath, here);
  const actual = short(readFileSync(source));
  const stale = actual !== fm.sourceHash;
  rows.push({ here, source: fm.sourcePath, stale });

  if (!stale) continue;
  if (STAMP) {
    writeFileSync(
      file,
      src.replace(
        new RegExp(`^sourceHash:\\s*\\S+\\s*$`, "m"),
        `sourceHash: ${actual}`,
      ),
    );
    continue;
  }
  findings.push({
    kind: "stale translation",
    detail:
      `${here} — ${fm.sourcePath} has changed since it was translated ` +
      `(${fm.sourceHash} → ${actual})`,
  });
}

/* ── Report ───────────────────────────────────────────────────────────────── */

if (STATUS) {
  const english = pages(DOCS)
    .filter((f) => !isStub(readFileSync(f, "utf8")))
    .map(rel)
    .sort((a, b) => a.localeCompare(b));
  const inScope = english.filter((p) => !OUT_OF_SCOPE.some((r) => r.test(p)));
  const missing = inScope.filter((p) => !translated.has(p));
  const stale = rows.filter((r) => r.stale);

  console.log(
    `${LOCALE}: ${rows.length} of ${inScope.length} pages in scope translated · ` +
      `${stale.length} stale · ${missing.length} not started · ` +
      `${english.length - inScope.length} pages stay in English\n`,
  );
  if (stale.length) {
    console.log("stale — the English page has moved on:");
    for (const r of stale) console.log(`  ${r.source}`);
    console.log("");
  }
  if (missing.length) {
    console.log("not translated yet:");
    for (const p of missing) console.log(`  ${p}`);
  }
  process.exit(0);
}

if (STAMP) {
  const stamped = rows.filter((r) => r.stale).length;
  console.log(
    stamped
      ? `re-stamped ${stamped} page(s) — commit them only if the French text really is up to date`
      : "every translation was already stamped at its source's current revision",
  );
  if (findings.length) {
    for (const f of findings) console.error(`  ${f.kind}: ${f.detail}`);
    process.exit(1);
  }
  process.exit(0);
}

if (findings.length) {
  console.error(`${findings.length} finding(s):\n`);
  for (const f of findings) console.error(`  ${f.kind}: ${f.detail}`);
  console.error(
    `\nUpdate the French page against its source, then run ` +
      `\`task docs:i18n:stamp\`. Stamping without reading the diff is how a ` +
      `translation goes wrong quietly.`,
  );
  process.exit(1);
}
console.log(
  `every ${LOCALE} page names its source and matches it — ${rows.length} translated pages`,
);
