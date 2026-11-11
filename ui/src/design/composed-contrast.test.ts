import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import {
  composite,
  contrastRatio,
  fromHex,
  oklchToLinearSrgb,
  parseOklch,
  type Rgb,
} from "./color.ts";

/**
 * Contrast for the pairings the component layer *composes*.
 *
 * `tokens.test.ts` measures the tokens and finds them correct — they are, all
 * of them, within ±0.09 of the ratios DESIGN.md quotes. That is the whole
 * problem: it grades `--ink-dim` against `--ground` at full opacity, and the
 * component that paints them writes `text-muted-foreground/60`. The colour a
 * reader sees is a composite that no token file contains and nothing measured,
 * and it lands at 2.59:1 against a 4.5:1 floor.
 *
 * Nine pairings are graded here. They were found by reading the class strings
 * rather than the token file, so each case records the site it came from —
 * change the alpha there and this test is what notices.
 *
 * Two kinds of case sit side by side and both belong:
 *
 *   - an **alpha composite**, where the value painted is not any token;
 *   - a pairing DESIGN.md simply **does not declare**, like `--rule-strong` on
 *     `--ground-raised` or a syntax-highlighting colour that arrives from a
 *     third-party theme rather than from the palette. Those pass today. They
 *     are locked because passing is a fact about the current numbers, and the
 *     light rendition of the first clears its floor by 0.07.
 */

/* Same read-the-file-as-text route as `tokens.test.ts`, for the same reasons:
   `import.meta.url` is an http URL under Vite and `?raw` goes through the CSS
   transform. */
const CSS = readFileSync(resolve(process.cwd(), "src/design/tokens.css"), "utf8");

function block(selector: string): Record<string, string> {
  const start = CSS.indexOf(selector);
  if (start === -1) throw new Error(`selector not found in tokens.css: ${selector}`);
  const open = CSS.indexOf("{", start);
  const body = CSS.slice(open + 1, CSS.indexOf("}", open));

  const out: Record<string, string> = {};
  for (const [, name, value] of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    out[name] = value.trim().replace(/\s*\/\*[\s\S]*?\*\/\s*$/, "");
  }
  return out;
}

type Tokens = Record<string, string>;

const DARK = block(":root {");
const LIGHT: Tokens = { ...DARK, ...block(':root[data-theme="light"]') };

/** One token as linear sRGB. */
function token(tokens: Tokens, name: string): Rgb {
  const parsed = parseOklch(tokens[name]);
  if (!parsed) throw new Error(`not an oklch() value: ${name} = ${tokens[name]}`);
  return oklchToLinearSrgb(parsed.l, parsed.c, parsed.h);
}

/**
 * The alpha a component actually paints with, read out of its source.
 *
 * Not a number copied into this file. A transcribed alpha makes the test a
 * snapshot of what the components said the day it was written: removing the
 * `/60` from `Input.vue` would leave this asserting 2.59:1 against a
 * placeholder that now measures 5.62:1, failing for a defect that was fixed.
 * The opposite drift is worse — a `/40` widened to `/25` would keep passing
 * against the old number.
 *
 * `pattern` must be global and must capture the percentage. A utility with no
 * `/n` suffix is opaque, which is how a fixed site reports itself.
 *
 * Two rules beyond "find the number":
 *
 *   - **Every occurrence, worst case wins.** One token is usually painted in
 *     several states — a resting fill and a hover dim, a border and its
 *     variant — and the one that matters is the weakest. Reading only the
 *     first match measured the fill a pointer is *not* on.
 *
 *   - **`dark:` belongs to one rendition.** `index.css` wires
 *     `@custom-variant dark` to `:root:not([data-theme="light"])`, so the dark
 *     rendition is the default and `dark:border-destructive` really did make
 *     `Alert`'s border opaque there while the unprefixed `/50` applied in
 *     light. Reading the two as one number reported 2.09:1 against a dark
 *     border that measured 5.76:1 — a failure in a rendition that did not have
 *     one, which is the same class of error as grading a light syntax theme
 *     against a dark ground.
 */
function alphaOf(file: string, pattern: RegExp, rendition: "dark" | "light"): number {
  /* Comments blanked, newlines kept. A file that *removes* an alpha usually
     says so in a note — `Button.vue` explains why `hover:bg-primary/85` is
     gone — and a scanner that reads the note finds the utility it was told
     was deleted, then reports the defect as still present. Writing the reason
     down must not be what fails the test. */
  const source = readFileSync(resolve(process.cwd(), "src", file), "utf8")
    .replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, " "))
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
    .replace(/(^|[^:"'`\\])\/\/[^\n]*/g, (m, lead: string) =>
      lead.concat(m.slice(lead.length).replace(/[^\n]/g, " ")),
    );

  const darkOnly: number[] = [];
  const both: number[] = [];
  for (const match of source.matchAll(pattern)) {
    // What sits immediately before the utility, back to the last whitespace or
    // quote — that is where a `dark:`/`hover:`/`active:` prefix lives.
    const before = source.slice(Math.max(0, match.index - 24), match.index);
    const prefix = /[^\s"'`]*$/.exec(before)?.[0] ?? "";
    const alpha = match[1] === undefined ? 1 : Number(match[1]) / 100;
    (prefix.includes("dark:") ? darkOnly : both).push(alpha);
  }

  // In dark a `dark:` utility *overrides* the unprefixed one, so it replaces
  // rather than joins it; in light it does not apply at all.
  const pool = rendition === "dark" && darkOnly.length > 0 ? darkOnly : both;
  if (pool.length === 0) throw new Error(`utility not found in ${file}: ${pattern}`);
  return Math.min(...pool);
}

/** A plain CSS declaration, for the alphas written as `opacity` rather than as
    a Tailwind suffix. `selector` must be unique enough to find its own rule. */
function cssOpacity(file: string, selector: string): number {
  const source = readFileSync(resolve(process.cwd(), "src", file), "utf8");
  const start = source.indexOf(selector);
  if (start === -1) throw new Error(`selector not found in ${file}: ${selector}`);
  const body = source.slice(start, source.indexOf("}", start));
  const match = /opacity:\s*([\d.]+)/.exec(body);
  if (!match) throw new Error(`no opacity in ${file} ${selector}`);
  return Number(match[1]);
}

/**
 * WCAG 2.2 floors. `text` is 1.4.3 at normal weight and size; `boundary` is
 * 1.4.11, which covers a border or fill that is the only thing distinguishing
 * a state.
 */
const TEXT = 4.5;
const BOUNDARY = 3;

interface Pairing {
  /** What is painted on what. */
  what: string;
  /** Where the alpha is written. Change it there, and this case moves. */
  site: string;
  floor: number;
  ratio: (t: Tokens, rendition: "dark" | "light") => number;
}

const PAIRINGS: Pairing[] = [
  {
    what: "--ink-dim as a placeholder on --ground",
    site: "components/ui/input/Input.vue — placeholder:text-muted-foreground",
    floor: TEXT,
    // A placeholder is content here, not decoration: DESIGN.md specifies it in
    // `--ink-dim` and says it carries a real example of the input expected, so
    // an operator who cannot read it has lost the format, not an ornament.
    ratio: (t, rendition) => {
      const ground = token(t, "--ground");
      const alpha = alphaOf(
        "components/ui/input/Input.vue",
        /placeholder:text-muted-foreground(?:\/(\d+))?/g,
        rendition,
      );
      return contrastRatio(composite(token(t, "--ink-dim"), ground, alpha), ground);
    },
  },
  {
    what: "--accent-ink on the hovered primary fill",
    site: "components/ui/button/Button.vue — hover:bg-primary",
    floor: TEXT,
    // The hover state of the one filled action in the system. The label does
    // not change on hover; the fill it sits on does, and the fill is what
    // stops carrying it.
    ratio: (t, rendition) => {
      const alpha = alphaOf(
        "components/ui/button/Button.vue",
        /bg-primary(?:\/(\d+))?/g,
        rendition,
      );
      const fill = composite(token(t, "--accent"), token(t, "--ground"), alpha);
      return contrastRatio(token(t, "--accent-ink"), fill);
    },
  },
  {
    what: "the secondary control's resting boundary on --ground",
    site: "components/ui/button/Button.vue — the outline variant's border",
    floor: BOUNDARY,
    /* The edge of an interactive element, which is what 1.4.11 is about. This
       was `border-primary/40` — crimson at 40%, 1.70:1 in dark and 2.14:1 in
       light, and not what DESIGN.md specifies for this control in the first
       place. `--rule-strong` clears the floor by 0.68 in dark and 0.41 in
       light, which is close enough that a nudge to either token should say so
       here rather than in a browser. */
    ratio: (t, rendition) => {
      const ground = token(t, "--ground");
      const alpha = alphaOf(
        "components/ui/button/Button.vue",
        /border-border(?:\/(\d+))?/g,
        rendition,
      );
      return contrastRatio(composite(token(t, "--rule-strong"), ground, alpha), ground);
    },
  },
  {
    what: "the secondary control's resting label on --ground",
    site: "components/ui/button/Button.vue — the outline variant's text",
    floor: TEXT,
    // `--ink-dim`, per the `.ctl` rule. The label of a control is content, so
    // it takes the text floor rather than the boundary one.
    ratio: (t) => contrastRatio(token(t, "--ink-dim"), token(t, "--ground")),
  },
  {
    what: "--accent as a badge border on --ground-raised",
    site: "components/ui/badge/Badge.vue — border-primary",
    floor: BOUNDARY,
    // The badge's own comment says the border carries the state because the
    // fill cannot. At 40% the border does not carry it either.
    ratio: (t, rendition) => {
      const card = token(t, "--ground-raised");
      const alpha = alphaOf(
        "components/ui/badge/Badge.vue",
        /border-primary(?:\/(\d+))?/g,
        rendition,
      );
      return contrastRatio(composite(token(t, "--accent"), card, alpha), card);
    },
  },
  {
    what: "--copper as a badge border on --ground",
    site: "components/ui/badge/Badge.vue — border-copper",
    floor: BOUNDARY,
    ratio: (t, rendition) => {
      const ground = token(t, "--ground");
      const alpha = alphaOf(
        "components/ui/badge/Badge.vue",
        /border-copper(?:\/(\d+))?/g,
        rendition,
      );
      return contrastRatio(composite(token(t, "--copper"), ground, alpha), ground);
    },
  },
  {
    what: "--accent as an alert border on --ground",
    site: "components/ui/alert/Alert.vue — border-destructive",
    floor: BOUNDARY,
    ratio: (t, rendition) => {
      const ground = token(t, "--ground");
      const alpha = alphaOf(
        "components/ui/alert/Alert.vue",
        /border-destructive(?:\/(\d+))?/g,
        rendition,
      );
      return contrastRatio(composite(token(t, "--accent"), ground, alpha), ground);
    },
  },
  {
    what: "the text on the halftone plate composite",
    site: "components/catalog/CatalogSpecimen.vue + assets/index.css .plate",
    floor: TEXT,
    /* The specimen facts run under the plate from roughly 600px of viewport
       to 1181px, where the plate is 52% of the width and the text is not.
       The renditions ink it differently — `--copper` at .34 on black, `--ink`
       at .20 on paper — so the composite is built per rendition rather than
       from one token.

       Alpha is the declared opacity, un-attenuated by the mask: the raster
       runs 99.7% ink at its dense end, so the worst case a glyph can land on
       is the full value.

       Graded against `--ink`, which is what the facts are set in. This asked
       about `--ink-dim` and reported 3.20:1 — a colour that was *declared* on
       the paragraph and painted on no glyph, because every child overrides it
       (`text-foreground` on the facts, `text-border` on the `aria-hidden`
       separators). The declaration is gone now, so the paragraph inherits
       `--ink` and this measures what a reader sees; the lock that keeps dim
       ink off this surface is in `system-rules.test.ts`, where a rule about
       source belongs. */
    ratio: (t, rendition) => {
      const ground = token(t, "--ground");
      const plate =
        rendition === "dark"
          ? composite(token(t, "--copper"), ground, cssOpacity("assets/index.css", ".plate {"))
          : composite(
              token(t, "--ink"),
              ground,
              cssOpacity("assets/index.css", ':root[data-theme="light"] .plate {'),
            );
      return contrastRatio(token(t, "--ink"), plate);
    },
  },
  {
    what: "the syntax comment colour on --ground-raised",
    site: "composables/useShiki.ts — LIGHT_CONTRAST_FLOOR",
    floor: TEXT,
    /* Not an alpha composite — a pairing that crosses out of the palette. The
       colour comes from a GitHub theme and the ground from ours, so no token
       file can hold the pair and `tokens.test.ts` cannot see it.

       Each rendition is graded against the theme *it actually loads*:
       `codeToHtml` is called with `{ light: github-light-high-contrast, dark:
       github-dark-high-contrast }`, so the light theme's comment colour never
       lands on the dark ground. Grading it there would invent a pairing and
       report a failure nobody can see. */
    ratio: (t, rendition) =>
      contrastRatio(
        fromHex(rendition === "dark" ? "#bdc4cc" : "#57606a"),
        token(t, "--ground-raised"),
      ),
  },
  {
    what: "--rule-strong on --ground-raised",
    site: "components/ui/table — every row and header boundary inside a card",
    floor: BOUNDARY,
    /* Passes, in both renditions, and is asserted for that reason. DESIGN.md
       declares `--rule-strong` against `--ground` (3.68:1 dark, 3.41:1 light)
       and says nothing about the raised half, which is where every table
       actually draws it — at 3.46:1 and 3.07:1. Seven hundredths of margin on
       paper is close enough that a future nudge to `--ground-raised` would
       take it under with nothing making a sound. */
    ratio: (t) => contrastRatio(token(t, "--rule-strong"), token(t, "--ground-raised")),
  },
];

const contrastLabel = (ratio: number) => `${ratio.toFixed(2)}:1`;

for (const [rendition, tokens] of [
  ["dark", DARK],
  ["light", LIGHT],
] as const) {
  describe(`composed contrast — ${rendition}`, () => {
    for (const pairing of PAIRINGS) {
      it(`${pairing.what} clears ${pairing.floor}:1`, () => {
        const measured = contrastLabel(pairing.ratio(tokens, rendition));
        expect(
          pairing.ratio(tokens, rendition),
          `${pairing.site}\n  measured ${measured}, floor ${pairing.floor}:1`,
        ).toBeGreaterThanOrEqual(pairing.floor);
      });
    }
  });
}

/**
 * The two results above that are *not* failures, recorded so nobody fixes them.
 *
 * **`disabled:opacity-50` on the primary button** composites `--accent` to
 * 2.07:1 against `--ground`, and that is not a WCAG defect: 1.4.3 exempts
 * "inactive user interface components", and a disabled control has no contrast
 * requirement at all. Dimming it is what tells you it is disabled. There is no
 * assertion for that ratio, on purpose — one would be a floor the spec does
 * not impose, and it would be "fixed" by making a disabled button look
 * enabled.
 *
 * It is a **DESIGN.md** defect, which is a different claim with a different
 * test — the one below.
 *
 * The second non-failure is `--rule-strong` on `--ground-raised`, which is in
 * the table above with its margin written down.
 */
describe("the disabled primary button", () => {
  it("drops the fill rather than dimming the crimson", () => {
    // DESIGN.md, Buttons: "disabled drops the fill entirely and becomes a dim
    // outlined control (crimson never appears in a disabled state)" — and
    // §Colour repeats it: "don't let it appear in a disabled control".
    //
    // `disabled:opacity-50` sits on the shared base string, so it dims every
    // variant uniformly and the `default` variant keeps `bg-primary`
    // underneath: a disabled primary action is a 50% crimson plate, which is
    // the one thing the rule names.
    const button = readFileSync(
      resolve(process.cwd(), "src/components/ui/button/Button.vue"),
      "utf8",
    );
    const base = /cva\(\s*"([^"]*)"/.exec(button)?.[1] ?? "";
    expect(
      base,
      "a shared disabled:opacity-* dims the crimson fill instead of removing it",
    ).not.toMatch(/disabled:opacity-/);
  });
});
