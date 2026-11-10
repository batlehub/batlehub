#!/usr/bin/env node
/**
 * Report user-visible text that is still hardcoded in templates.
 *
 * The RFC calls for a lint gate that "fails on literal user-visible text in
 * templates, so the catalogue cannot silently rot as pages are added". That gate
 * is only honest once the extraction is finished — turning it on mid-migration
 * would mean a permanently red build that everyone learns to ignore, which is
 * worse than no gate at all.
 *
 * So this reports by default and fails only with `--max <n>`, letting the
 * remaining count be ratcheted down as surfaces are converted, and pinned at 0
 * when Phase 8 closes.
 *
 *   node build/i18n-audit.mjs           # report
 *   node build/i18n-audit.mjs --max 0   # gate
 *
 * Four places text hides, because it reached zero while three of them were
 * unscanned and most of the console was still English:
 *
 *   1. text nodes                  `<th>Registry</th>`
 *   2. human-facing attributes     `title="Dashboard"`, `label="Registries"`
 *   3. literals in expressions     `{{ busy ? 'Loading…' : 'Refresh' }}`
 *   4. literals in `<script>`      `{ label: "All Packages" }`
 *
 * (1) missed every single capitalised word, because the "is this an identifier"
 * test was case-insensitive and unanchored to spaces, so `Registries` and `You`
 * read as identifiers. (2) covered four HTML attributes but no component prop.
 * (3) could not see anything, because attribute values are blanked before the
 * tag split and interpolations are stripped. (4) was out of scope entirely,
 * which is how the whole admin navigation stayed English behind a green gate.
 *
 * It stays conservative about what counts as prose: identifiers, numbers, code,
 * single symbols, Tailwind class lists and the domain terms §4.6 keeps verbatim
 * are all skipped. A gate nobody can drive to zero is a gate nobody reads.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { isTranslatable, templateBridges } from "./i18n-shared.mjs";

const ROOT = new URL("../src", import.meta.url).pathname;

/**
 * Attributes whose value a person reads aloud or sees. `label` and
 * `description` are here because a component prop is just as visible as an HTML
 * attribute: `<Facet label="Registries">` rendered the catalog's facet heading
 * in English in both locales.
 *
 * The second half of this list is RFC 0004-bis §4.1. Four HTML attributes and
 * two props were not enough: `confirm-label="Clear Cache"` sat above a French
 * confirmation sentence and this gate read zero, because a *prop* carrying the
 * button's own text was not among the six names it knew. Adding names is the
 * narrow fix; the rule the audit actually needs is `looksHuman` below, and this
 * list is now only the set of names that are human text *whatever* they hold.
 */
const HUMAN_ATTRS = new Set([
  "title",
  "placeholder",
  "aria-label",
  "alt",
  "label",
  "description",
  "empty-message",
  "confirm-label",
  "loading-label",
  "item-noun",
  "scope",
  "action",
  "value-text",
  "message",
  "hint",
  "heading",
  "summary",
  "legend",
]);

/**
 * The rule, rather than the list.
 *
 * `HUMAN_ATTRS` enumerates positions text has been *found* in before, which is
 * why this gate has now read zero twice over live English — once for the whole
 * admin navigation (`adminSections.ts`'s header records it) and once for five
 * strings in two dialogs. A name ending in `-label`, `-message`, `-text`,
 * `-title`, `-hint` or `-description` is human-readable text by construction:
 * that is what those suffixes mean. Anything matching is scanned, and
 * `isTranslatable` is still the one that decides whether the value is prose.
 */
const looksHuman = (attr) =>
  HUMAN_ATTRS.has(attr) ||
  /(^|-)(label|message|text|title|hint|description|caption|tooltip|placeholder)$/.test(attr);

/**
 * Files whose string data the RFC keeps as data (§6.7): registry setup snippets
 * are tool names, config keys and CLI invocations, which §4.6 keeps verbatim in
 * both locales. Scanning them would bury a real finding under 200 that must not
 * be touched — and a gate nobody can get to zero is a gate nobody reads.
 */
const DATA_FILES = /(registryTypes|registryPathFields)\.ts$/;

function walk(dir) {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return entry === "client" ? [] : walk(path);
    if (DATA_FILES.test(path) || /\.(test|spec)\.ts$/.test(path)) return [];
    return path.endsWith(".vue") || path.endsWith(".ts") ? [path] : [];
  });
}

function templateOf(source) {
  const open = source.indexOf("<template>");
  const close = source.lastIndexOf("</template>");
  return open === -1 || close === -1 ? "" : source.slice(open + 10, close);
}

/**
 * The `<script>` region, comments and imports removed.
 *
 * Imports carry module specifiers, JSDoc carries English by design, and neither
 * reaches a screen — leaving them in buries the findings under the prose that
 * explains them.
 */
function scriptOf(source) {
  const region = source.includes("<template>") ? source.slice(0, source.indexOf("<template>")) : source;
  return region
    // The opening tag's own attributes (`generic="TItem, TForm extends object"`)
    // are TypeScript, not text, and read as an assignment otherwise.
    .replace(/<script\b[^>]*>/g, "")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:"'`\\])\/\/[^\n]*/g, "$1")
    // No trailing `(from …)?` group: `[^;\n]*` has already eaten the rest of the
    // line, so the optional group could only ever re-match what it consumed —
    // the overlap that makes this pattern backtrack super-linearly.
    .replace(/^[ \t]*(?:import|export)\s[^;\n]*;?/gm, "");
}

/**
 * Text that reaches a human through a variable rather than through a tag.
 *
 * `parseError.value = "Paste some CSV content first."` renders in an alert two
 * lines later and no template scan can see it, which is how §2.1's examples
 * shipped behind a green gate. `isTranslatable` already returned `true` for
 * every one of them — the audit simply never asked it about this position.
 *
 * Two sinks: assignment (to a ref, a property, or a plain local) and a call
 * argument. `SILENT_SINK` is the one concession to enumeration, and it is
 * deliberately about *destinations that are not a screen* rather than about
 * shapes of text: a message handed to `console.error` or `new Error` is for
 * whoever reads the log, and translating it would make a stack trace harder to
 * search. `t(…)`/`te(…)` are excluded for the opposite reason — their argument
 * is a catalogue key, which is the fixed form this gate is driving toward.
 */
const SILENT_CALLEES = [
  // Diagnostics. The reader is whoever reads the log, and a translated message
  // is a stack trace that no longer greps.
  String.raw`console\.\w+`,
  String.raw`new\s+Error`,
  "Error",
  // Machine constructors and DOM plumbing: the argument is a URL, a key, a
  // selector or a class list, never a sentence.
  "URL",
  "URLSearchParams",
  "RegExp",
  String.raw`JSON\.\w+`,
  String.raw`localStorage\.\w+`,
  String.raw`sessionStorage\.\w+`,
  "querySelector(?:All)?",
  "getElementById",
  "createElement",
  "setAttribute",
  "getAttribute",
  "matchMedia",
  "cva",
  "cn",
  "defineModel",
  "defineEmits",
  // The i18n helpers themselves, for the opposite reason: their argument is a
  // catalogue key, which is the fixed form this gate is driving toward.
  String.raw`\bt`,
  String.raw`\bte`,
  String.raw`\btm`,
  String.raw`\bd`,
  String.raw`\bn`,
];
const SILENT_SINK = new RegExp(String.raw`(?:${SILENT_CALLEES.join("|")})\s*$`);

/**
 * Identifiers whose assigned string is machine-facing whatever it says: a route
 * path, a storage key, a CSS class list, a sort order, a CLI invocation. Matched
 * on the *target* rather than the value, because "asc" and "grid" are not prose
 * either way but `"Paste some CSV content first."` is.
 *
 * Two forms and no third: the bare word, or the word as a camelCase suffix
 * (`emptyPath`, `storageKey`). Matching a bare suffix would silence `valid` for
 * ending in `id` and `monkey` for ending in `key`, which is how a denylist
 * quietly turns into the same blind spot the list above exists to close.
 */
const SILENT_WORDS = [
  "class", "className", "classes", "style", "path", "href", "src", "url", "key", "id",
  "mode", "sort", "order", "type", "variant", "status", "state", "locale", "lang",
  "theme", "token", "method", "format", "code", "command", "snippet", "example",
  // HTTP header names. `Authorization: \`Bearer ${token}\`` strips to "Bearer",
  // which is prose by shape and a protocol constant by destination.
  "authorization", "accept", "header", "scheme",
];
const capitalise = (w) => w[0].toUpperCase() + w.slice(1);
// The suffix half is case-*sensitive* on purpose: `/id$/i` matches `valid`, and
// only the capital in `packageId` distinguishes a suffix from a coincidence.
const SILENT_SUFFIX = new RegExp(`[a-z0-9](?:${SILENT_WORDS.map(capitalise).join("|")})$`);
const SILENT_EXACT = new RegExp(`^(?:${SILENT_WORDS.join("|")})$`, "i");
const SILENT_TARGET = { test: (name) => SILENT_EXACT.test(name) || SILENT_SUFFIX.test(name) };

/**
 * An object-literal value: `dark: "Theme: dark"`, `{ error: "…" }`.
 *
 * Named because the fallback pass has to recognise the same shape in order to
 * *not* report it — a ternary's `:` and a key's `:` are one character, and the
 * only thing that tells them apart is what sits in front. Two passes reading
 * one shape from one definition is the difference between counting a string
 * once and counting it twice.
 */
const OBJECT_VALUE =
  /(?<![\w.$])([A-Za-z_$][\w$]*)\s*:\s*(?:"([^"\\\n]*)"|'([^'\\\n]*)'|`([^`\\]*)`)/g;

/** `OBJECT_VALUE`'s left half, anchored to the end of the text before a `:`. */
const OBJECT_KEY = /(?<![\w.$])[A-Za-z_$][\w$]*\s*$/;

// A `${…}` residue is a value, not a sentence. Without this, a code snippet
// inside a backtick template reads as prose the moment it contains a quoted
// attribute.
const strip = (text) => text.replace(/\$\{[^}]*\}/g, "").trim();

/**
 * Every user-visible string one source literal yields.
 *
 * Usually one: the literal with its interpolations stripped. A template can
 * yield a second kind, though — the connective *between* two interpolations,
 * which survives neither the length floor nor the bare-word rule and so was
 * invisible however the literal was reached. See `templateBridges`.
 *
 * Every literal in this file goes through here, so a template found in an
 * assignment, a call argument, a ternary or an object value is read the same
 * way. The alternative is four call sites that agree today and drift apart the
 * first time one of them is fixed.
 */
const textsOf = (raw) => {
  const whole = strip(raw);
  const out = isTranslatable(whole) ? [whole] : [];
  out.push(...templateBridges(raw));
  return out;
};

// Assignment: `x = "…"`, `x.value = "…"`, `obj.prop = "…"`.
//
// The leading lookbehind is what keeps the scan linear: without it, a failed
// attempt restarts at every character of a long dotted name and re-walks the
// same run. A match can only ever begin where the name does, so refusing to
// start mid-name loses nothing.
function assignedStrings(script) {
  const out = [];
  for (const [, target, single, double, backtick] of script.matchAll(
    /(?<![\w.$[\]"'])([\w.$[\]"']+)\s*=(?!=)\s*(?:"([^"\\\n]*)"|'([^'\\\n]*)'|`([^`\\]*)`)/g,
  )) {
    /* The name being assigned to, however it was reached: `parseError.value`,
       `row.error`, `headers["Authorization"]`. The last identifier in the
       expression is the one that names the destination. */
    const name = target.replace(/\.value$/, "").match(/[A-Za-z_$][\w$]*/g)?.pop() ?? target;
    if (SILENT_TARGET.test(name)) continue;
    for (const text of textsOf(single ?? double ?? backtick)) out.push(`${target} = "${text}"`);
  }
  return out;
}

/* Call argument: `setError("…")`, `announce("…")`, `ref("…")`. The callee is
     read from the text *before* the paren — `match.index`, not `indexOf`, which
     would resolve every repeat of a common call to its first occurrence.

     The literal does not have to be the *first* argument, and the fallback
     message almost never is: `apiErrorMessage(err, "Failed to revoke token.")`
     is the shape this codebase writes every API error in, and anchoring the
     literal to the open paren meant not one of them was ever read. Preceding
     arguments are matched conservatively — identifiers and member access, no
     nested call and no earlier literal — so that `console.error("a", "b")`
     still resolves its callee to `console.error` and stays silent, and so that
     the 40-character window behind `match.index` still lands on the callee
     rather than inside an argument list. */
function argumentStrings(script) {
  const out = [];
  for (const match of script.matchAll(
    /\(\s*(?:[\w.$?![\]]+\s*,\s*)*(?:"([^"\\\n]*)"|'([^'\\\n]*)'|`([^`\\]*)`)/g,
  )) {
    if (SILENT_SINK.test(script.slice(Math.max(0, match.index - 40), match.index))) continue;
    for (const text of textsOf(match[1] ?? match[2] ?? match[3])) out.push(`(… "${text}")`);
  }
  return out;
}

/* A fallback expression: `x ?? "Unknown"`, `ok ? "Yes" : "No"`, `a || "—"`.
     This is where default human text hides — `errorMsg.value = e instanceof
     Error ? e.message : "Export failed"` is an assignment whose right-hand side
     is an expression, so the assignment sink above cannot see it, and it is the
     shape every "…or a default message" line takes.

     `:` is in the alternation because the *else* branch is where the default
     sits: the `?` branch carries the value that was found and the `:` branch
     the sentence somebody wrote for when it was not. Without it this pass read
     the half of the ternary that is never the hardcoded one, which is how
     seven `: "Unknown error"` fallbacks sat behind a zero.

     It cannot be matched blind, because `label: "Registries"` is the same two
     characters. A `:` preceded by a bare identifier is left to the
     object-literal pass below, which already reports it under the key's name —
     the point is not to hide it here but to count it once, in one place.

     The `:` branch alone demands a space before the literal and refuses a
     *preceding* `:`, because a colon is the one operator here that also occurs
     *inside* strings. `` `${registry}::` `` ends in two of them, and a colon
     glued to the template's own closing backtick pairs with the *next* backtick
     in the file — five lines of `_store.delete(k)` were reported as an
     untranslated sentence. Formatting puts a space after a ternary's colon and
     never after a namespace separator, so that space is what tells a colon
     between two expressions from a colon inside one string. It is also why the
     following colon needs no guard of its own: `\s` cannot match one.

     All three operators sit in one alternation, and the pass is one scan, on
     purpose. Read as two loops — `??`/`||`/`?` in one, `:` in the other — each
     regex is simpler and the result is wrong: a second independent scan
     re-reads what the first already consumed, and the `?` ending
     `"…</span>?", confirmLabel:` pairs with the quote after it to report
     `, confirmLabel:` as English. One scan cannot overlap itself. */
function fallbackStrings(script) {
  const out = [];
  for (const match of script.matchAll(
    /(\?{1,2}|\|\||(?<!:):(?=\s))\s*(?:"([^"\\\n]*)"|'([^'\\\n]*)'|`([^`\\]*)`)/g,
  )) {
    if (match[1] === ":" && OBJECT_KEY.test(script.slice(0, match.index))) continue;
    for (const text of textsOf(match[2] ?? match[3] ?? match[4])) out.push(`… ?? "${text}"`);
  }
  return out;
}

/* Object-literal value: `dark: "Theme: dark"` in a lookup table. This is the
   §2.2 defect in its purest form — `theme.dark` was translated, correct, and
   referenced zero times while `ThemeToggle` held the English in a `LABELS`
   map. This replaces a pass that scanned `label:` and `title:` only, because
   those are the two keys text had been found under before; the key's *name*
   is not what makes the value text. */
function objectValueStrings(script) {
  const out = [];
  for (const [, name, single, double, backtick] of script.matchAll(OBJECT_VALUE)) {
    if (SILENT_TARGET.test(name)) continue;
    for (const text of textsOf(single ?? double ?? backtick)) out.push(`${name}: "${text}"`);
  }
  return out;
}

/**
 * Every user-visible string the `<script>` region yields, deduplicated.
 *
 * Four sinks, four passes, one scan each — see each pass for what it reads and
 * why the shape it matches is the one the defect took.
 */
function scriptStrings(source) {
  const script = scriptOf(source);
  return [
    ...new Set([
      ...assignedStrings(script),
      ...argumentStrings(script),
      ...fallbackStrings(script),
      ...objectValueStrings(script),
    ]),
  ];
}

/**
 * String literals inside a *bound* expression — `:title="hit ? 'A' : 'B'"`, or
 * an interpolation like `{{ ok ? 'Yes' : 'No' }}`.
 *
 * The text-node pass has to blank every attribute value before it splits on
 * tags (a `>` inside an expression would end the tag early), and it strips
 * interpolations, so literals in either place were invisible. That is where the
 * catalog's four empty states and the whole fresh-instance path were hiding.
 *
 * `:class` and `:style` are skipped: their literals are Tailwind class lists,
 * not prose, and they outnumber the real findings roughly twenty to one — which
 * would bury this gate rather than sharpen it.
 */
/**
 * The `{{ … }}` spans in `text`, as `[open, afterClose)` index pairs.
 *
 * Hand-walked rather than `/\{\{[\s\S]*?\}\}/`: a lazy any-character run
 * followed by a literal re-scans the rest of the string from every `{{`, which
 * is quadratic on a long template. `indexOf` finds the same leftmost, shortest
 * spans in one pass.
 */
function interpolationSpans(text) {
  const spans = [];
  let from = 0;
  for (;;) {
    const open = text.indexOf("{{", from);
    if (open === -1) return spans;
    const close = text.indexOf("}}", open + 2);
    if (close === -1) return spans;
    spans.push([open, close + 2]);
    from = close + 2;
  }
}

/** The expression inside each `{{ … }}`, braces excluded. */
const interpolationBodies = (text) =>
  interpolationSpans(text).map(([open, end]) => text.slice(open + 2, end - 2));

/** `text` with every `{{ … }}` removed. */
function stripInterpolations(text) {
  let out = "";
  let last = 0;
  for (const [open, end] of interpolationSpans(text)) {
    out += text.slice(last, open);
    last = end;
  }
  return out + text.slice(last);
}

function boundLiterals(template) {
  const out = [];
  // A backtick literal may interpolate; its `${…}` parts are values, and what
  // is left around them is the sentence — `` `All registries (${n})` `` is
  // still English prose that needs a key.
  /* `*`, not `{3,}`: a minimum length here makes the engine mis-pair quotes.
     Against `? "…" : "Save"` it skips the one-character `"…"`, resumes at that
     literal's *closing* quote, and matches `" : "` — swallowing `"Save"`
     entirely. Length is `isProse`'s job; this regex's only job is finding where
     a literal starts and ends. */
  const literal = /'([^']*)'|`([^`]*)`/g;

  for (const [, colonAttr, directive, value] of template.matchAll(
    /(?::([\w.-]+)|v-([\w:.-]+))="([^"]*)"/g,
  )) {
    const attr = colonAttr ?? directive ?? "";
    if (/^(class|style)$/.test(attr) || /^bind:(class|style)$/.test(attr)) continue;
    for (const [, single, backtick] of value.matchAll(literal)) {
      for (const text of textsOf(single ?? backtick)) out.push(`:${attr}="… '${text}'"`);
    }
  }

  /* Inside an interpolation all three quote styles are fair game — it is not an
     attribute value, so a double quote does not terminate anything, and a
     backtick is the one an interpolated sentence is most likely to use.
     Omitting the backtick is how `` `Sign in with ${providerLabel(p.name)}` ``
     rendered English on the login page with this gate reading zero. */
  for (const expr of interpolationBodies(template)) {
    for (const [, single, double, backtick] of expr.matchAll(
      // See the note on `literal` above: a length floor here mis-pairs quotes.
      /'([^']*)'|"([^"]*)"|`([^`]*)`/g,
    )) {
      for (const text of textsOf(single ?? double ?? backtick)) out.push(`{{ … '${text}' }}`);
    }
  }
  return out;
}

/**
 * Removes every match of every pattern, until *no* pattern matches any more.
 *
 * One pass is not enough when deleting a match splices its neighbours into a
 * new one — `<!-` + `<!-- -->` + `-` leaves `<!--` behind, and the `>` inside
 * the survivor then splits it, so the comment that was meant to be dropped
 * comes back as prose the scanner grades. Code scanning flagged the single pass
 * as incomplete multi-character sanitisation.
 *
 * The loop has to span the patterns rather than sit inside each one. Stripping
 * comments and then code samples in two separate fixed points leaves the second
 * to re-form the first: `<!-<code>x</code>-- … > … -->` has no comment in it
 * until the code sample is removed, and by then the comment pass is over.
 */
function stripAll(text, ...patterns) {
  let out = text;
  for (let prev; prev !== out; ) {
    prev = out;
    for (const re of patterns) out = out.replace(re, "");
  }
  return out;
}

/**
 * Every untranslated string in one file's source. Exported so the gate can be
 * tested on a fixture rather than on the tree it grades — a scanner that cannot
 * be shown to *fail* is the same green as one that found nothing, which is the
 * failure mode this whole file exists to close (RFC 0004-bis §2).
 *
 * @param source the file contents
 * @param isVue  whether to scan a `<template>` as well as the `<script>`
 */
export function scanSource(source, isVue = true) {
  if (!isVue) return scriptStrings(source);
  // Comments are not user-visible; code samples are not prose. `</code\n>` is
  // still a close tag.
  const template = stripAll(
    templateOf(source),
    /<!--[\s\S]*?-->/g,
    /<(pre|code)[\s\S]*?<\/\1\s*>/g,
  );

  /* Attribute *values* are stripped before splitting on tags: an expression like
     `v-if="count > 0"` contains a `>` that would otherwise end a tag early and
     leak the rest of the expression in as if it were prose. Human-facing
     attributes are matched separately, below, against the original template. */
  const textOnly = template.replace(/=\s*"[^"]*"/g, '=""').replace(/=\s*'[^']*'/g, "=''");

  const out = [];

  // Text nodes: everything between tags that is not an interpolation. The tag
  // pattern excludes `<` from its own body so a failed match cannot rescan past
  // the next tag — and a tag cannot contain a `<` anyway.
  for (const chunk of textOnly.split(/<[^<>]*>/)) {
    const text = stripInterpolations(chunk).trim();
    if (!text || text.length < 3) continue;
    if (!isTranslatable(text)) continue;
    out.push(text.length > 60 ? `${text.slice(0, 57)}…` : text);
  }

  /* Human-facing attributes with a literal (non-bound) value. Every attribute
     is matched and `looksHuman` decides which are text — iterating a fixed list
     of names can only ever find text where text has already been found, and
     that is exactly how `confirm-label="Clear Cache"` shipped. */
  for (const [, attr, value] of template.matchAll(/(?<![:\w-])([a-zA-Z][\w-]*)="([^"{]*)"/g)) {
    if (!looksHuman(attr)) continue;
    const text = value.trim();
    if (isTranslatable(text)) out.push(`${attr}="${text}"`);
  }
  out.push(...boundLiterals(template), ...scriptStrings(source));
  return out;
}

const findings = (path) => scanSource(readFileSync(path, "utf8"), path.endsWith(".vue"));

// Importing this module must not run the report — `i18n-audit.test.ts` imports
// `scanSource` and would otherwise walk the whole tree on every test run.
if (process.argv[1]?.endsWith("i18n-audit.mjs")) main();

function main() {
const maxIndex = process.argv.indexOf("--max");
const max = maxIndex === -1 ? null : Number(process.argv[maxIndex + 1]);

const report = walk(ROOT)
  .map((path) => ({ path: relative(ROOT, path), items: findings(path) }))
  .filter((entry) => entry.items.length > 0)
  .sort((a, b) => b.items.length - a.items.length);

const total = report.reduce((sum, entry) => sum + entry.items.length, 0);

for (const { path, items } of report) {
  console.log(`\n${path}  (${items.length})`);
  for (const item of items.slice(0, 6)) console.log(`  ${item}`);
  if (items.length > 6) console.log(`  … ${items.length - 6} more`);
}

console.log(`\n${total} untranslated strings across ${report.length} files`);

if (max !== null && total > max) {
  console.error(`\ni18n audit: ${total} untranslated strings exceeds the budget of ${max}`);
  process.exit(1);
}
}
