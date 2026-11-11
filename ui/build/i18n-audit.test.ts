import { describe, it, expect } from "vitest";

// @ts-expect-error — plain ESM build script, no type declarations by design.
import { scanSource } from "./i18n-audit.mjs";

const scan = (source: string, isVue = true): string[] => scanSource(source, isVue) as string[];

const vue = (script: string, template = "") =>
  `<script setup lang="ts">\n${script}\n</script>\n\n<template>\n${template}\n</template>\n`;

/**
 * The gate's own regression signal.
 *
 * `pnpm run i18n:check` reported `0 untranslated strings across 0 files` while
 * five English strings rendered to a French operator, and it had reported the
 * same zero once before while the *entire admin navigation* was in English. Both
 * times the scanner was correct about everything it looked at and silent about
 * where the text actually was.
 *
 * So the assertions here are about what the scanner can *see*, not about the
 * current count. A count of zero over a tree proves nothing unless something
 * independently proves a non-zero is reachable — which is exactly the property
 * RFC 0004-bis §2 says four of our gates were missing.
 */
describe("i18n audit", () => {
  it("a generic component's type parameters are not text", () => {
    const source = `<script setup lang="ts" generic="TItem, TForm extends object">\nconst x = 1;\n</script>\n\n<template>\n<div />\n</template>\n`;
    expect(scan(source)).toEqual([]);
  });

  describe("finds text wherever it is written", () => {
    it("a component prop carrying English fails", () => {
      // §2.1 verbatim: this rendered a "Clear Cache" button above a French
      // confirmation sentence, and the gate read zero — `confirm-label` was not
      // one of the six attribute names it knew.
      const findings = scan(vue("", `<ConfirmDialog confirm-label="Clear Cache" />`));
      expect(findings).toContain('confirm-label="Clear Cache"');
    });

    it("an attribute name nobody enumerated still fails", () => {
      // The rule, not the list: `looksHuman` accepts any `*-label`/`*-message`
      // name, so a prop invented tomorrow is covered without a scanner change.
      const findings = scan(vue("", `<Thing rollback-message="Nothing to roll back." />`));
      expect(findings).toContain('rollback-message="Nothing to roll back."');
    });

    it("a string assigned to a ref fails", () => {
      const findings = scan(vue(`error.value = "Please enter a token.";`));
      expect(findings).toContain('error.value = "Please enter a token."');
    });

    it("a string in an object-literal lookup table fails", () => {
      // `theme.dark` was translated, correct and referenced zero times while
      // `ThemeToggle` held this map. Scanning `label:`/`title:` only is what
      // made the key's *name* decide whether its value counted as text.
      const findings = scan(vue(`const LABELS = { dark: "Theme: dark" };`));
      expect(findings).toContain('dark: "Theme: dark"');
    });

    it("a string passed to a call fails", () => {
      const findings = scan(vue(`globalThis.prompt("Block reason:");`));
      expect(findings.join("\n")).toContain("Block reason:");
    });

    it("a backtick sentence in an interpolation fails", () => {
      const findings = scan(vue("", "{{ `Sign in with ${provider}` }}"));
      expect(findings.join("\n")).toContain("Sign in with");
    });

    it("a plain `.ts` file is scanned too", () => {
      const findings = scan(`const q = { error: "State mismatch — try again." };`, false);
      expect(findings).toContain('error: "State mismatch — try again."');
    });

    /**
     * The three positions that reported `0 across 0` over fifteen live English
     * strings. Each one is a *half* of a shape the scanner already knew: the
     * else-branch of a ternary it only read the then-branch of, the second
     * argument of a call it only read the first of, and the middle of a
     * template it only read the ends of.
     */
    it("the else-branch of a ternary fails, not just the then-branch", () => {
      // Seven of the fifteen. The then-branch carries the value that was
      // found; the hardcoded sentence is always in the branch for when it was
      // not, which is the half this pass could not see.
      const findings = scan(vue(`msg.value = e instanceof Error ? e.message : "Export failed";`));
      expect(findings.join("\n")).toContain("Export failed");
    });

    it("a fallback message that is not the first argument fails", () => {
      const findings = scan(vue(`err.value = apiErrorMessage(e, "Failed to revoke token.");`));
      expect(findings.join("\n")).toContain("Failed to revoke token.");
    });

    it("a connective between two interpolations fails", () => {
      // Two characters, all lowercase: under the length floor *and* caught by
      // the bare-word rule, so the only English word in the sentence was the
      // only part of it the gate could not read.
      const findings = scan(vue("const s = `${head} of ${scope}`;"));
      expect(findings.join("\n")).toContain("of");
    });
  });

  describe("stays quiet about text that is not prose", () => {
    it("a cva variant map of Tailwind classes passes", () => {
      const findings = scan(
        vue(`const v = { outline: "border border-primary/40 bg-transparent hover:bg-accent" };`),
      );
      expect(findings).toEqual([]);
    });

    it("a hyphenated sentence is not mistaken for a class list", () => {
      // The counter-case to the one above: all-lowercase tokens with a single
      // hyphen between them is prose, and a rule that demanded *every* token
      // carry a separator would have swallowed it.
      const findings = scan(vue(`row.error = "reason is required (set per-row or default)";`));
      expect(findings).toContain('row.error = "reason is required (set per-row or default)"');
    });

    it("a defineModel name passes", () => {
      const findings = scan(vue(`const open = defineModel<boolean>("mobileOpen");`));
      expect(findings).toEqual([]);
    });

    it("an HTTP header value passes", () => {
      const findings = scan(vue("const h = {}; h[\"Authorization\"] = `Bearer ${token}`;"));
      expect(findings).toEqual([]);
    });

    it("a console message passes — it reaches a log, not a screen", () => {
      const findings = scan(vue(`console.error("Failed to refresh the token.");`));
      expect(findings).toEqual([]);
    });

    it("a catalogue key passed to t() passes", () => {
      const findings = scan(vue(`const label = t("adminHealth.clearCache");`));
      expect(findings).toEqual([]);
    });

    it("a CLI invocation under a `code:` key passes", () => {
      const findings = scan(vue(`const s = { code: "batlehub-cli registry list" };`));
      expect(findings).toEqual([]);
    });

    it("a class list bound with :class passes", () => {
      const findings = scan(vue("", `<div :class="ok ? 'text-foreground' : 'text-destructive'" />`));
      expect(findings).toEqual([]);
    });

    /**
     * The cost of reading a ternary's `:`. A colon is the one operator in that
     * pass that also occurs inside strings and inside object literals, so each
     * of these is a way the widened rule could have started lying.
     */
    it("an object key is reported once, under its key, not twice", () => {
      // `label: "Registries"` is a colon followed by a literal too. Reporting
      // it from both passes would inflate the count and make the budget
      // un-drivable to zero for reasons that have nothing to do with the text.
      const findings = scan(vue(`const m = { label: "Registries" };`));
      expect(findings).toEqual(['label: "Registries"']);
    });

    it("a namespace separator inside a template passes", () => {
      // `` `${registry}::` `` ends in two colons, and one glued to the
      // template's own closing backtick pairs with the *next* backtick in the
      // file: this reported five lines of cache eviction as a sentence.
      const findings = scan(
        vue(
          "for (const k of _store.keys()) {\n" +
            "  if (k.startsWith(`${registry}::`)) _store.delete(k);\n" +
            "}\n" +
            "for (const k of _upstream.keys()) {\n" +
            "  if (k.endsWith(`::${registry}`)) _upstream.delete(k);\n" +
            "}",
        ),
      );
      expect(findings).toEqual([]);
    });

    it("a separator between two interpolations is not a connective", () => {
      // The counter-case to the bridge rule: position says "between two
      // values", but `/`, `://` and `.` are punctuation joining a path, not a
      // word joining a sentence.
      const findings = scan(
        vue("const u = `${scheme}://${host}/${owner}/${repo}@${maj}.${min}`;"),
      );
      expect(findings).toEqual([]);
    });
  });

  /**
   * Stripping is repeated to a fixed point, because deleting one match can
   * splice its neighbours into a new one. Code scanning flagged the single pass
   * as incomplete sanitisation; the reader-visible cost is that a comment the
   * scanner meant to drop came back as prose it then graded.
   */
  describe("strips markup it cannot see through in one pass", () => {
    it("a comment re-formed by removing an inner comment is still removed", () => {
      // `<!-` + `<!-- -->` + `-` leaves `<!--` behind after one pass, and the
      // `>` inside then splits the survivor so its tail reads as prose: one
      // pass reported `tout de suite -->` as an untranslated string.
      const findings = scan(vue("", `<!-<!-- -->- Supprimer le cache > tout de suite -->`));
      expect(findings).toEqual([]);
    });

    it("a code sample re-formed the same way is still removed", () => {
      const findings = scan(vue("", `<cod<code>x</code>e>batlehub-cli registry list</code>`));
      expect(findings).toEqual([]);
    });

    /**
     * The loop has to span both patterns, not sit inside each. Stripping
     * comments to a fixed point and *then* code samples to a fixed point leaves
     * the second pass free to re-form a comment the first has already finished
     * looking for.
     */
    it("a comment re-formed by removing a code sample is still removed", () => {
      const findings = scan(vue("", `<!-<code>x</code>-- Supprimer le cache > maintenant -->`));
      expect(findings).toEqual([]);
    });

    it("a comment re-formed by removing a pre block is still removed", () => {
      const findings = scan(vue("", `<!-<pre>x</pre>-- Vider le cache > tout de suite -->`));
      expect(findings).toEqual([]);
    });
  });
});
