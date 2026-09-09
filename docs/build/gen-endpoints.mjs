// Generate each registry page's endpoint reference from `ui/openapi.json`.
//
// RFC 0009 §6. These tables used to be hand-maintained, and they drifted the
// way a hand-maintained inventory always does: `rubygems.md` listed six routes
// where ten exist, `terraform.md` documented a protocol the code does not
// implement, and `npm.md` published an audit path npm never sends. Three
// independent records of the API surface — routes, tests, docs — all agreed
// with each other and disagreed with the client.
//
// The spec is already built (`task dump-spec`), every proxy handler is already
// tagged, and `crates/web/tests/openapi_contract.rs` already fails on a `200`
// without a body. So the inventory costs nothing to generate and cannot drift.
//
// Only the block between the markers is replaced. The prose around it — which
// is the part worth reading — stays hand-written.
//
//   <!-- BEGIN endpoints: proxy/npm -->
//   | Method | Path | Description |
//   ...
//   <!-- END endpoints -->
//
// A page may name several tags, comma-separated, for the kinds that share a
// handler module (openvsx and vscode-marketplace both render from `proxy/openvsx`).
//
// Usage: node build/gen-endpoints.mjs [--check]

import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const docsDir = join(here, "..");
const specPath = join(docsDir, "..", "ui", "openapi.json");

// One tree per locale. The rows are the API surface — a method, a path, and the
// operation's own summary from the spec — so they are the same in both: a route
// is not translated, and a summary written in the Rust source is the string the
// spec carries. The header is the one part of the block that is prose, so it is
// the one part that changes. A locale whose tree does not exist yet is skipped
// rather than reported: the translation lands in waves, and this generator has
// nothing to say about a page nobody has written.
const TREES = [
  { dir: join(docsDir, "registries"), label: "registries", header: "| Method | Path | Description |" },
  { dir: join(docsDir, "fr", "registries"), label: "fr/registries", header: "| Méthode | Chemin | Description |" },
];

// The tag list is captured raw and trimmed by the caller: a `\s*(…*?)\s*`
// sandwich around a lazy group is ambiguous, and the scanner has to backtrack
// over every split of the trailing whitespace to prove a non-match.
const BEGIN = /^<!--\s*BEGIN endpoints:([^>]*)-->\s*$/;
const END = /^<!--\s*END endpoints\s*-->\s*$/;

const METHOD_ORDER = ["get", "post", "put", "patch", "delete"];

/** Rows for a set of tags, sorted stably: by path, then by method. */
function rowsFor(spec, tags) {
  const wanted = new Set(tags);
  const rows = [];
  for (const [path, ops] of Object.entries(spec.paths)) {
    for (const [method, op] of Object.entries(ops)) {
      if (!METHOD_ORDER.includes(method)) continue;
      if (!(op.tags || []).some((t) => wanted.has(t))) continue;
      const summary = (op.summary || op.description || "")
        .split("\n")[0]
        .trim()
        // Backslash first: escaping `|` introduces backslashes of its own, so
        // doing it the other way round would double-escape them.
        .replaceAll("\\", String.raw`\\`)
        .replaceAll("|", String.raw`\|`);
      rows.push({ method: method.toUpperCase(), path, summary });
    }
  }
  rows.sort(
    (a, b) =>
      a.path.localeCompare(b.path) ||
      METHOD_ORDER.indexOf(a.method.toLowerCase()) -
        METHOD_ORDER.indexOf(b.method.toLowerCase()),
  );
  return rows;
}

function renderTable(rows, header) {
  const out = [header, "|--------|------|-------------|"];
  for (const r of rows) {
    out.push(`| \`${r.method}\` | \`${r.path}\` | ${r.summary} |`);
  }
  return out.join("\n");
}

function rewrite(source, spec, file, header) {
  const lines = source.split("\n");
  const out = [];
  let i = 0;
  let blocks = 0;
  while (i < lines.length) {
    const m = lines[i].match(BEGIN);
    if (!m) {
      out.push(lines[i++]);
      continue;
    }
    const tags = m[1]
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);
    if (tags.length === 0) {
      throw new Error(`${file}: BEGIN endpoints marker names no tag`);
    }
    const rows = rowsFor(spec, tags);
    if (rows.length === 0) {
      throw new Error(
        `${file}: no operations in ui/openapi.json carry ${tags.join(", ")} — ` +
          `either the tag is misspelled or the spec is stale (run 'task dump-spec')`,
      );
    }
    out.push(lines[i++], renderTable(rows, header)); // the BEGIN marker, then the table
    while (i < lines.length && !END.test(lines[i])) i++;
    if (i >= lines.length) {
      throw new Error(`${file}: BEGIN endpoints marker has no matching END`);
    }
    out.push(lines[i++]); // the END marker
    blocks++;
  }
  return { text: out.join("\n"), blocks };
}

const spec = JSON.parse(readFileSync(specPath, "utf8"));
const check = process.argv.includes("--check");

let pages = 0;
let stale = [];
for (const tree of TREES) {
  if (!existsSync(tree.dir)) continue;
  for (const name of readdirSync(tree.dir).sort((a, b) => a.localeCompare(b))) {
    if (!name.endsWith(".md")) continue;
    const file = join(tree.dir, name);
    const before = readFileSync(file, "utf8");
    if (!before.split("\n").some((l) => BEGIN.test(l))) continue;
    const where = `${tree.label}/${name}`;
    const { text, blocks } = rewrite(before, spec, where, tree.header);
    pages += blocks;
    if (text !== before) {
      if (check) stale.push(where);
      else writeFileSync(file, text);
    }
  }
}

if (check && stale.length) {
  console.error(
    `endpoint tables are stale in:\n  ${stale.join("\n  ")}\n` +
      `run 'task docs:endpoints' and commit the result`,
  );
  process.exit(1);
}

console.log(
  check
    ? `every endpoint table matches the API surface — ${pages} generated blocks`
    : `regenerated ${pages} endpoint tables from ui/openapi.json`,
);
