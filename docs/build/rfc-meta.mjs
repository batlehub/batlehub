/**
 * What an RFC declares about itself, read from the document.
 *
 * Three surfaces quote these facts — the status banner on the page
 * (`.vitepress/config.ts` → `theme/RfcStatus.vue`), the table on `/rfc/`, and
 * the `/rfc/` sidebar — and none of them may hold a second copy. Every fact
 * lives in the RFC's own header table and is parsed back out here, so a
 * document whose status changes changes all three by being edited once.
 *
 * The header table is the form in `internal/0000-rfc-template.md`:
 *
 *   | Status  | Draft                          |   ← the vocabulary below
 *   | Short   | Subdomain routing              |   ← how it is listed
 *   | Settles | Reaching a registry by host …  |   ← the one line on /rfc/
 *
 * Consumers: `.vitepress/config.ts` (the banner) and `build/rfc.mjs`
 * (`task rfc:index`, `task rfc:status`, `task rfc:new`).
 */
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

/**
 * The RFC status vocabulary, exactly as `internal/0000-rfc-template.md` defines
 * it. Longest first, because "In review" would otherwise never match against a
 * looser pattern, and "Superseded by NNNN" carries the number it points at.
 */
export const RFC_STATUSES = [
  /^Superseded by (\d{4})\b/,
  /^In review\b/,
  /^Implemented\b/,
  /^Accepted\b/,
  /^Rejected\b/,
  /^Draft\b/,
];

/** A status the product can be described by: the page is history, not a proposal. */
const SETTLED = /^(Implemented|Rejected|Superseded)/;

/** The header table stops at the first `##`; `| Status |` also occurs in bodies. */
const headerOf = (raw) => raw.split(/^## /m)[0];

/** One `| Field | Value |` row out of a header table, or undefined. */
export function headerField(raw, name) {
  const row = new RegExp(String.raw`^\|\s*${name}\s*\|([^|]*)\|`, "m").exec(headerOf(raw));
  return row?.[1].replaceAll("**", "").trim() || undefined;
}

/**
 * Split a `Status` value into its state and the note that follows it.
 *
 * Two things the parser has to survive, because the existing files already do
 * them. The value is prose rather than an enum — RFC 0001 reads
 * `**Implemented** — all phases landed; see the implementation notes in §13` —
 * so the leading token is the status and the remainder is a note worth
 * rendering with it. And the status may be `Superseded by 0004`, which carries
 * the number it points at.
 */
export function parseStatus(value, where = "an RFC") {
  const pattern = RFC_STATUSES.find((p) => p.test(value));
  if (!pattern) {
    throw new Error(
      `${where}: status "${value}" is not in the template's vocabulary ` +
        `(Draft, In review, Accepted, Implemented, Rejected, Superseded by NNNN).`,
    );
  }
  const [state] = value.match(pattern);
  const note = value.slice(state.length).replace(/^\s*[—–-]\s*/, "").trim();
  return { state, note, settled: SETTLED.test(state) };
}

/**
 * Read one RFC's status out of its own header table, for the page banner.
 *
 * Generated, never hand-written on the page: an RFC that describes a proposal,
 * published under a label saying it shipped, would be a claim about the product
 * that is not true. An unparseable status fails the build rather than rendering
 * an unlabelled page (RFC 0005 §6.8).
 */
export function rfcStatus(srcDir, filePath) {
  const raw = readFileSync(join(srcDir, filePath), "utf8");
  const value = headerField(raw, "Status");
  if (!value) {
    throw new Error(
      `${filePath}: no parseable "| Status | … |" row in the header table. ` +
        `Every RFC needs one — an RFC published without a banner is exactly ` +
        `the page that misleads (RFC 0005 §6.8).`,
    );
  }
  return parseStatus(value, filePath);
}

/** `0004-bis-what-rfc-0004-left.md` → `{ num: 4, bis: true, id: "0004-bis" }`. */
export function parseFilename(file) {
  const m = file.match(/^(\d{4})(-bis)?-(.+)\.md$/);
  if (!m) return undefined;
  return {
    num: Number(m[1]),
    bis: Boolean(m[2]),
    id: m[1] + (m[2] ? "-bis" : ""),
    slug: file.replace(/\.md$/, ""),
  };
}

/**
 * The deferrals of one document: the choices an RFC took *not* to make, and
 * said so in the same breath.
 *
 * There is no front-matter for these and there should not be — a deferral is
 * an argument, and it belongs in the paragraph that makes it. What the report
 * reads instead is the **bold lead** these documents already write it with:
 *
 *   **Deferred, and documented instead** (0012 O7)
 *   **Deferred to Phase 5:** (0003 phase 4)
 *   **Decided: not now.** (0002 q1, 0008-bis q1 and q2)
 *
 * So the convention is one line, and it is the one already in use: a paragraph,
 * list item or table cell whose first bold run starts with `Deferred` or
 * `Decided: not now`. Prose that merely contains the word "deferred" is not
 * matched, on purpose: 39 lines across 20 documents do, nearly all of them
 * describing somebody else's deferral or a field's default, and a report that
 * listed those would be read once and never again.
 *
 * `where` is the nearest heading above the hit, so a row says which section
 * argued it; for a table row it is the heading of the table's own section,
 * which is as close as a Markdown table gets to a location.
 */
export function readDeferrals(raw) {
  const out = [];
  const lines = raw.split(/\r?\n/);
  let heading = "";
  for (const [i, line] of lines.entries()) {
    const h = /^#{2,4}[ \t]+(.+?)[ \t]*$/.exec(line);
    if (h) {
      heading = h[1].trim();
      continue;
    }
    // The bold lead, wherever it sits: a paragraph of its own, a list item
    // ("   **Decided: not now.**"), a blockquote (0015 writes both of its
    // deferrals as one), or a table cell (0012 and 0020 write theirs there).
    const m = /\*\*(Deferred[^*]*|Decided:[ \t]*not now[^*]*)\*\*/.exec(line);
    if (!m) continue;
    const lead = m[1].replace(/[:,.\s]+$/, "").trim();
    // What follows the lead on the same line is the argument's first clause;
    // a lead that ends its line (0015's blockquotes) has its sentence on the
    // next one, which the caller does not need — the lead names the choice.
    // These documents wrap at 80 columns, so the argument runs past the line
    // the lead is on. The claim is the rest of the *paragraph* — up to a blank
    // line — cut at its first sentence, which is where these leads put the
    // decision itself and the rest puts the reasoning.
    let rest = line.slice(m.index + m[0].length);
    if (!/^\|/.test(line)) {
      for (let j = i + 1; j < lines.length && lines[j].trim() !== ""; j++) {
        if (/^#{2,4}[ \t]/.test(lines[j])) break;
        // The *next* deferral ends this one. A markdown list writes its items
        // on adjacent lines with no blank between them, so without this a
        // second `**Decided: not now.**` item is swallowed into the first
        // one's claim and then reported again on its own — the same argument
        // printed twice, once with somebody else's sentence attached.
        if (/\*\*(Deferred[^*]*|Decided:[ \t]*not now[^*]*)\*\*/.test(lines[j])) break;
        // Two of these are written as blockquotes (0015): the `>` markers are
        // the quoting, not the sentence.
        rest += ` ${lines[j].replace(/^[ \t>]+/, "")}`;
      }
    }
    rest = rest
      .replace(/^[\s:—-]+/, "")
      .replace(/\s*\|\s*$/, "")
      .replace(/\s+/g, " ")
      .trim();
    // Whether the deferral says what would undo it is a property of the whole
    // argument, not of its first sentence: these paragraphs put the decision
    // first and the reopen condition last.
    const reopens = /\breopen(s|ed|ing)?\b|\bwhen (one|someone|an? \w+) (does|asks|wants)\b/i.test(rest);
    // The first sentence end that leaves a sentence behind. A cut at the
    // first `. ` alone gives "(§7.2)." for 0012, whose lead ends in a
    // cross-reference — true, and useless in a listing.
    for (const stop of rest.matchAll(/(?<!\b[A-Z])(?<!§\d)\.[ \t]/g)) {
      if (stop.index >= 40) {
        rest = rest.slice(0, stop.index + 1);
        break;
      }
    }
    // A table row starts with `| <id> |`: that first cell is the question's
    // own number (O7, 10), which is a better location than the section.
    const cell = /^\|[ \t]*([A-Za-z]?\d+)[ \t]*\|/.exec(line);
    out.push({
      lead,
      where: cell ? `${heading} (${cell[1]})` : heading,
      claim: rest,
      // A deferral that says what would undo it is a decision; one that does
      // not is a wish. The report says which, and does not judge.
      reopens,
    });
  }
  return out;
}

/**
 * Every RFC in `docs/rfc/`, oldest first — a base RFC before its own bis,
 * because they read in order: each one argues with the state the previous left.
 *
 * `Short` and `Settles` are required for the same reason `Status` is: both are
 * quoted by a listing this file generates, and a missing one would be filled in
 * by hand there and then drift.
 */
export function readRfcs(rfcDir) {
  const rfcs = [];
  for (const file of readdirSync(rfcDir).sort((a, b) => a.localeCompare(b))) {
    const parsed = parseFilename(file);
    if (!parsed) continue;

    const raw = readFileSync(join(rfcDir, file), "utf8");
    const require_ = (name) => {
      const value = headerField(raw, name);
      if (!value) {
        throw new Error(
          `${file}: no "| ${name} | … |" row in the header table. ` +
            `The /rfc/ index and sidebar are generated from these rows ` +
            `(\`task rfc:index\`) — see internal/0000-rfc-template.md.`,
        );
      }
      return value;
    };

    // Anchored on `[ \t]` rather than `\s` throughout: `\s` matches newlines, so
    // it both overlaps the `.`/`\S` that follows it — the ambiguity that makes
    // these patterns backtrack super-linearly — and lets a "heading" span lines.
    const title = /^# RFC \d{4}(?:-bis)?[ \t]*—[ \t]*(\S.*)$/m.exec(raw)?.[1].trim();
    if (!title) {
      throw new Error(`${file}: no "# RFC NNNN — Title" heading.`);
    }

    // "### Still open" is the template's own readiness test: the RFC is ready
    // for sign-off when the section is empty. Counted, not read, so
    // `task rfc:status` can say which documents still owe a decision.
    //
    // Split rather than one lazy `[\s\S]*?` with a lookahead: the section ends at
    // the next heading or at the end of the file, and JS has no end-of-input
    // escape to spell that second case with. The `\Z` this used to carry was not
    // one — in JS it is a literal "Z" — so the old pattern could only ever end a
    // section at a following `##`, and would have counted nothing in an RFC whose
    // final section was "Still open". Latent today; every such section is
    // currently followed by another heading.
    //
    // A struck-through item is not counted. `~~…~~` is how these documents
    // record a question that was answered while the RFC was being built —
    // 0007-bis question 2 is `~~Should `text_config` be validated…~~ **Answered
    // in the building: yes, and it had to be.**` — and reporting a decision that
    // has already been taken as one still owed is the single thing this report
    // exists to be right about. The lookahead sits after the list marker so it
    // tests the item's own first characters, not the indentation before them.
    // An HTML comment is not an open question. A section that says "None"
    // and then keeps the retired questions commented out below it — 0020
    // does, so the wording that was measured away is not lost — would
    // otherwise be counted as owing every one of them, which is the report
    // being wrong in the one direction that matters.
    const afterHeading = raw.split(/^### Still open[ \t]*\r?\n/m)[1] ?? "";
    const open = afterHeading.split(/^##/m)[0].replace(/<!--[\s\S]*?(?:-->|$)/g, "");
    const openQuestions = (open.match(/^[ \t]*(?:\d+\.|[-*])[ \t]+(?!~~)\S/gm) ?? []).length;

    rfcs.push({
      ...parsed,
      file,
      title,
      short: require_("Short"),
      settles: require_("Settles"),
      status: parseStatus(require_("Status"), file),
      openQuestions,
      deferrals: readDeferrals(raw),
    });
  }
  return rfcs.sort((a, b) => a.num - b.num || Number(a.bis) - Number(b.bis));
}
