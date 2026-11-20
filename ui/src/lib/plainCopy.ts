/**
 * Server-authored prose, made readable for an operator.
 *
 * The server's warnings and errors cite the design documents they come from —
 * `(RFC 0019 §4.1)` — which is right in a log a contributor greps and noise on
 * a console a platform team reads. The citation is dropped from what is
 * displayed; the stable `code` beside the message is what links back to it.
 */

/** A parenthesised RFC citation, with the whitespace that led into it. */
const RFC_CITATION =
  /\s*\((?:see\s+)?RFC[\s-]?\d{4}(?:-[a-z]+)?(?:\s*§\s*[\d.]+)?(?:[,;]\s*[^)]*)?\)/gi;

/** `text` without its parenthesised RFC citations. */
export function withoutDocRefs(text: string): string {
  return text.replace(RFC_CITATION, "").replace(/\s+([.,;:])/g, "$1");
}

/**
 * The first sentence of `text`, and whatever follows it.
 *
 * Splits on the first `. ` / `! ` / `? ` that ends a sentence, so a long
 * warning can lead with its claim and keep the explanation one click away.
 * `rest` is empty when the text is a single sentence.
 */
export function leadSentence(text: string): { lead: string; rest: string } {
  // Not after an abbreviation: "e.g. Foo" is one sentence.
  const match = /^(.+?(?<!\b(?:e\.g|i\.e|etc|vs))[.!?])\s+(?=[A-Z0-9'"`[(])/s.exec(text);
  if (!match) return { lead: text, rest: "" };
  return { lead: match[1], rest: text.slice(match[0].length) };
}

/**
 * `text` with a capital first letter, unless it opens on an identifier — a
 * TOML key (`overlay = true`, `on_confirmed`), a path or a tool spec
 * (`github:jdx/mise`) — whose spelling is what the operator searches for.
 */
export function sentenceStart(text: string): string {
  const first = text.split(/\s/, 1)[0] ?? "";
  if (/[_=:./[\]]/.test(first) || /^\S+\s+=/.test(text)) return text;
  return text.charAt(0).toUpperCase() + text.slice(1);
}
