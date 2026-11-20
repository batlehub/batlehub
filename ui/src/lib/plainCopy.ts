/**
 * Server-authored prose, made readable for an operator.
 *
 * The server's warnings and errors cite the design documents they come from —
 * `(RFC 0019 §4.1)` — which is right in a log a contributor greps and noise on
 * a console a platform team reads. The citation is dropped from what is
 * displayed; the stable `code` beside the message is what links back to it.
 */

/** The opening of an RFC citation's contents: `RFC 0019`, `see RFC-0004`. */
const RFC_HEAD = /^\s*(?:see\s+)?RFC[\s-]?\d{4}/i;
/** What may follow it: a `-bis` suffix, a `§ 4.1`, then a `,`/`;` list or the end. */
const RFC_TAIL = /^(?:-[a-z]+)?(?:\s*§\s*[\d.]+)?(?:[,;]|$)/i;

/** Whether a parenthesis's contents are an RFC citation. */
function isRfcCitation(inner: string): boolean {
  const head = RFC_HEAD.exec(inner);
  return head !== null && RFC_TAIL.test(inner.slice(head[0].length));
}

/**
 * `text` without its parenthesised RFC citations, and the whitespace that led
 * into each. Scanned by hand rather than with one regex: the single pattern
 * backtracked super-linearly on long server messages.
 */
export function withoutDocRefs(text: string): string {
  let out = "";
  let from = 0;
  for (;;) {
    const close = text.indexOf(")", from);
    if (close < 0) break;
    // The innermost parenthesis that this `)` closes.
    const open = text.lastIndexOf("(", close);
    if (open >= from && isRfcCitation(text.slice(open + 1, close))) {
      out += text.slice(from, open).trimEnd();
    } else {
      out += text.slice(from, close + 1);
    }
    from = close + 1;
  }
  return withoutSpaceBeforePunctuation(out + text.slice(from));
}

/** `text` with the whitespace in front of `.`, `,`, `;` and `:` removed. */
function withoutSpaceBeforePunctuation(text: string): string {
  const out: string[] = [];
  for (const ch of text) {
    if (".,;:".includes(ch)) {
      while (out.length > 0 && /\s/.test(out.at(-1) ?? "")) out.pop();
    }
    out.push(ch);
  }
  return out.join("");
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
