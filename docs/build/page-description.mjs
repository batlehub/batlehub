/**
 * The per-page meta description.
 *
 * Every page of this site used to ship the *site's* description — 211 pages
 * declaring themselves "Your package hub. Proxy, cache, and host npm, …" to a
 * search engine and to every chat unfurl. Duplicate descriptions are the one
 * SEO defect a crawler resolves by ignoring the tag entirely and inventing a
 * snippet of its own.
 *
 * The obvious fix is a `description:` line in each page's frontmatter, and it
 * is the wrong one here: the French tree stamps `sourceHash` against its
 * English source (see `check-i18n.mjs`), so adding a frontmatter key to 106
 * English pages would mark every translation of them stale in the same commit.
 * So the description is *derived* — from the page's own first paragraph, which
 * in this tree is written as a summary of the page almost everywhere — and a
 * hand-written `description:` in frontmatter still wins when one is worth
 * writing.
 */

/** The longest description worth emitting: Google renders ~155-160 chars. */
const MAX = 160;

/** Inline markdown → the text a reader would see. */
function plain(text) {
  return text
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "") // images carry no prose
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1") // links keep their label
    .replace(/<[^>]+>/g, "") // inline HTML
    .replace(/[`*_~]/g, "")
    .replace(/\\([\\`*_{}[\]()#+\-.!])/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

/** Cut on a sentence if one ends late enough, else on a word. */
function clamp(text) {
  if (text.length <= MAX) return text;
  const head = text.slice(0, MAX + 1);
  const sentence = Math.max(
    head.lastIndexOf(". "),
    head.lastIndexOf("? "),
    head.lastIndexOf("! "),
  );
  if (sentence >= MAX / 2) return head.slice(0, sentence + 1);
  const word = head.lastIndexOf(" ");
  return `${head.slice(0, word > 0 ? word : MAX).trimEnd()}…`;
}

/**
 * The first paragraph of prose in a markdown source, as a meta description.
 *
 * Skipped on the way there: the frontmatter block, headings, rules, HTML
 * comments, fenced code, tables, lists, block quotes and `:::` containers —
 * every shape that is structure rather than a sentence. An RFC opens with its
 * header table, so this lands on its `## 1. Summary`, which is what an RFC's
 * summary is for.
 *
 * Returns "" when the page has no prose at all (a `layout: home` page is the
 * one that matters; its caller falls back to the hero tagline).
 */
export function extractDescription(src) {
  const body = src.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, "");
  const para = [];
  let fenced = false;

  for (const raw of body.split(/\r?\n/)) {
    const line = raw.trim();

    if (/^(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;

    if (!line) {
      if (para.length) break; // the paragraph ended
      continue;
    }
    if (
      /^#/.test(line) || // heading
      /^(-{3,}|\*{3,}|_{3,})$/.test(line) || // rule
      /^:::/.test(line) || // container directive
      /^(<!--|<)/.test(line) || // comment or block HTML
      /^\|/.test(line) || // table row
      /^>/.test(line) || // quote
      /^([-*+]|\d+\.)\s/.test(line) // list item
    ) {
      if (para.length) break;
      continue;
    }
    para.push(line);
  }

  return clamp(plain(para.join(" ")));
}
