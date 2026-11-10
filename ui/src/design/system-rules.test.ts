import { globSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The rules that are properties of the *source* rather than of a rendering.
 *
 * It started as two of DESIGN.md's rules about utilities, and the reason it
 * collects the rest is the same one: `tokens.test.ts` grades the token file,
 * and a component that never asks the tokens is invisible to it. A Tailwind
 * class carrying its own hardcoded value, a `confirm()` that can state no
 * scope, an irreversible action inheriting somebody else's consequence — none
 * of those is a number anything can measure, and each is exactly readable in
 * the text of the file.
 *
 * These began as **frozen lists** — an equality rather than a ceiling, so that
 * a new occurrence failed *and* clearing one failed until the number was
 * lowered in the same commit, where a reviewer could see the two together. A
 * ceiling (`<=`) would have let the list rot at its high-water mark, which is
 * the failure mode the i18n gate spent three holes demonstrating.
 *
 * Both lists are empty now. They stay written out rather than collapsed into a
 * bare `toEqual({})`, because the shape is what lets the next piece of debt be
 * recorded the same way instead of argued about.
 */

const ROOT = process.cwd();

/**
 * Source with its comments blanked, newlines preserved.
 *
 * Both rules are quoted in prose by the code that obeys them — `AppNav` says
 * pills should not float "as separate rounded pills", `ReadmePanel` explains
 * that `--radius` is zero because "the world has no rounded corners". A scan
 * that graded those would be teaching people not to write the reason down.
 *
 * Newlines survive the blanking so that a reported line number still points at
 * the line it came from.
 */
function withoutComments(source: string): string {
  const blank = (match: string) => match.replace(/[^\n]/g, " ");
  return (
    source
      .replace(/<!--[\s\S]*?-->/g, blank)
      .replace(/\/\*[\s\S]*?\*\//g, blank)
      // `[^:"'`\\]` before the slashes, or every `https://` in the tree reads as
      // a comment and takes the rest of its line with it.
      .replace(/(^|[^:"'`\\])\/\/[^\n]*/g, (m, lead: string) => lead + blank(m.slice(lead.length)))
  );
}

/** Every scanned file, as `path -> comment-free source`. */
const SOURCES: Record<string, string> = Object.fromEntries(
  globSync("src/**/*.{vue,ts}", { cwd: ROOT })
    .filter((f: string) => !f.includes("/client/") && !/\.(test|spec)\.ts$/.test(f))
    .sort()
    .map((f: string) => [f, withoutComments(readFileSync(resolve(ROOT, f), "utf8"))]),
);

/** `path -> number of matches`, files with none omitted. */
function census(pattern: RegExp): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [file, source] of Object.entries(SOURCES)) {
    const hits = source.match(pattern)?.length ?? 0;
    if (hits > 0) out[file] = hits;
  }
  return out;
}

/**
 * A box-shadow utility: `shadow`, `shadow-sm`, `shadow-lg`, and any variant
 * prefix in front of them.
 *
 * `shadow` bare is in the alternation because that is what `Switch.vue` uses,
 * and it is the same defect as bare `rounded` below — a utility with no
 * suffix, resolving to a value the system never chose.
 *
 * The bounds keep out the two neighbouring shapes that are somebody else's
 * test: a `text-shadow` utility (the lookbehind rejects a leading `-`), and a
 * shadow in an off-palette Tailwind hue, which the One Synthetic Rule in
 * `tokens.test.ts` already grades by colour name.
 */
const SHADOW_UTILITY = /(?<![\w-])shadow(?:-(?:xs|sm|md|lg|xl|2xl|inner))?(?![\w-])/g;

/**
 * A bare `rounded` — no suffix.
 *
 * `rounded-sm`, `rounded-md` and `rounded-lg` are **legitimate**: they are
 * neutralised to `0` by `assets/index.css`, so a component that asks for one
 * gets the system's square corner and would pick up a new radius if the system
 * ever grew one. Suffix-less `rounded` does not go through that: Tailwind
 * compiles it to a hardcoded `border-radius: 4px`, which is the one radius in
 * the product nothing can change.
 */
const BARE_ROUNDED = /(?<![\w-])rounded(?![\w:-])/g;

/**
 * The two zero-blur shadows DESIGN.md sanctions, both on the primary button:
 * the hover action ring and the `:active` pixel step. Neither is a halo — they
 * are offset plates, which is why the Flat-At-Rest Rule admits them.
 */
const SANCTIONED_BOX_SHADOWS = new Set(["var(--action-ring)", "var(--pixel-step)"]);

/**
 * Seven sites spent the system's one reserved gesture on a sticky bar, a select
 * popover, a dialog and a switch thumb. Every one already carried a `border`,
 * which is what DESIGN.md gives them instead — `Combobox.vue` is the in-repo
 * precedent, "flat and inked" with `border border-border bg-background` and no
 * shadow at all. The dialog has a `bg-black/60` overlay besides; the switch's
 * thumb is told apart from its track by its own fill.
 */
const FROZEN_SHADOW: Record<string, number> = {};

/**
 * Seventeen bare `rounded`s, now `rounded-sm` — which `assets/index.css`
 * neutralises to `0`. The corner they draw is identical; the difference is
 * that it goes through the system, so a future radius would reach them.
 */
const FROZEN_ROUNDED: Record<string, number> = {};

describe("the Flat-At-Rest Rule", () => {
  it("adds no box-shadow utility anywhere in src/", () => {
    expect(
      census(SHADOW_UTILITY),
      "a shadow is the primary button's reserved gesture; nothing else gets one",
    ).toEqual(FROZEN_SHADOW);
  });

  /**
   * The arbitrary-value escape hatch, closed.
   *
   * `[box-shadow:…]` is not a `shadow-*` utility and the rule above cannot see
   * it, so a halo written that way would pass a green test. `Button.vue` is
   * the one file that legitimately uses the form — it is how the two zero-blur
   * plates are expressed at all — which is exactly why the form has to be
   * graded rather than exempted by filename.
   */
  it("writes an arbitrary box-shadow only as one of the two sanctioned plates", () => {
    const offenders: string[] = [];
    for (const [file, source] of Object.entries(SOURCES)) {
      for (const [, value] of source.matchAll(/\[box-shadow:([^\]]*)\]/g)) {
        if (!SANCTIONED_BOX_SHADOWS.has(value.trim())) offenders.push(`${file}: ${value}`);
      }
    }
    expect(offenders, "zero blur, or no shadow").toEqual([]);
  });

  it("still finds the two sanctioned plates on the primary button", () => {
    // The counter-assertion. Without it the test above passes just as well
    // over a Button that lost both shadows, and the rule would have quietly
    // become "no shadows at all" while still claiming to allow two.
    const button = SOURCES["src/components/ui/button/Button.vue"];
    expect(button).toContain("hover:[box-shadow:var(--action-ring)]");
    expect(button).toContain("active:[box-shadow:var(--pixel-step)]");
  });
});

/**
 * DESIGN.md's Undependable Fill Rule: a fill is not a state channel.
 *
 * Both of these were caught by measuring a ratio, and neither can be *kept*
 * fixed by a ratio — once the fill is gone there is no colour left to grade,
 * and a contrast test with nothing to measure is a test that passes because it
 * stopped looking. So they are locked here, on the source, where the rule is.
 */
describe("the Undependable Fill Rule", () => {
  it("marks no table row by a tint alone", () => {
    // `bg-destructive/5` on a blocked package's row measured 1.03:1 in dark and
    // 1.09:1 in light. The state is carried by the status cell — the word
    // "Blocked", the `destructive` badge and the reason under it — and the
    // tint was a fourth channel that no eye could receive.
    const offenders = Object.entries(SOURCES).filter(([, source]) =>
      /<TableRow[^>]*\bbg-(?:destructive|primary|copper)\/\d+/s.test(source),
    );
    expect(
      offenders.map(([file]) => file),
      "a row tint is not a state",
    ).toEqual([]);
  });

  it("sets no dim ink over the halftone plate", () => {
    /* The specimen section is the one surface in the product with an image
       under its text. `--ink` clears 4.5:1 on the plate with room (9.60:1 in
       dark, 7.70:1 in light); `--ink-dim` measures 3.20:1 and 3.35:1.

       `text-muted-foreground` sat on the facts paragraph and painted no glyph,
       because every child overrides it — so it was a trap rather than a
       defect: the next plain word added to that paragraph would have rendered
       under the floor with nothing to catch it. */
    const catalog = SOURCES["src/pages/PackageCatalog.vue"];
    const specimen = catalog.slice(
      catalog.indexOf('<div class="plate"'),
      catalog.indexOf("</section>", catalog.indexOf('<div class="plate"')),
    );
    expect(specimen, "the plate section must not exist without its own test").not.toBe("");
    expect(specimen).not.toContain("text-muted-foreground");
  });
});

/**
 * PRODUCT.md principle 2: a destructive action states its scope, its count and
 * its consequence before it happens. `confirm()` and `prompt()` can state none
 * of the three.
 *
 * They are also not translated, cannot be styled, and both Firefox and Chrome
 * offer to suppress them after the second one in a session — so the third
 * `confirm()` can simply not appear, and the action goes through with nothing
 * asked. `DestructiveConfirm` is the primitive built for this, and every verb
 * in the console now routes through it.
 */
describe("no native dialogs", () => {
  it("asks nothing through `confirm()` or `prompt()`", () => {
    const offenders: string[] = [];
    for (const [file, source] of Object.entries(SOURCES)) {
      // Bounded on the left so `confirmDialog(`, `runPending(` and the like are
      // not matched, and `globalThis.`/`window.` qualified forms are.
      for (const [, call] of source.matchAll(
        /(?:^|[^\w.$])(?:(?:globalThis|window)\.)?((?:confirm|prompt)\s*\()/g,
      )) {
        offenders.push(`${file}: ${call.trim()}`);
      }
    }
    expect(offenders, "a native dialog states no scope, count or consequence").toEqual([]);
  });
});

/**
 * DESIGN.md § Layout: "One 4px-based scale, six steps: 4 / 8 / 12 / 16 / 24 /
 * 40 … a value not on the scale is a defect." In Tailwind's 4px units that is
 * 1 / 2 / 3 / 4 / 6 / 10 (plus 0 and px). The audit found 41 files off it —
 * mostly 1.5 and 0.5, the half-steps shadcn ships with — and nothing stopped
 * the next one. Padding, margin, gap and space only: positions (`top-2.5`)
 * and sizes (`h-3.5`) are not rhythm.
 */
const OFF_SCALE_SPACING =
  /(?<![\w-])(?:[a-z0-9]+:)*-?(?:p|px|py|pt|pb|pl|pr|ps|pe|m|mx|my|mt|mb|ml|mr|gap|gap-x|gap-y|space-x|space-y)-(?!(?:0|px|1|2|3|4|6|10|auto)(?![\w.]))\d+(?:\.\d+)?(?![\w.\-\]])/g;

describe("the spacing scale", () => {
  it("spaces nothing off DESIGN.md's six steps", () => {
    expect(
      census(OFF_SCALE_SPACING),
      "4 / 8 / 12 / 16 / 24 / 40 px — Tailwind 1/2/3/4/6/10",
    ).toEqual({});
  });
});

describe("the radius scale", () => {
  it("uses no suffix-less `rounded` anywhere in src/", () => {
    expect(
      census(BARE_ROUNDED),
      "`rounded-sm|md|lg` resolve through the system to 0; bare `rounded` is a hardcoded 4px",
    ).toEqual(FROZEN_ROUNDED);
  });

  it("still accepts the suffixed radii the system neutralises", () => {
    // `rounded-sm` is on the primary button. A rule that caught it too would
    // be unfixable — the whole component library asks for those.
    expect(SOURCES["src/components/ui/button/Button.vue"]).toContain("rounded-sm");
    expect(census(BARE_ROUNDED)["src/components/ui/button/Button.vue"]).toBeUndefined();
  });
});

/**
 * Every irreversible action states *its own* consequence.
 *
 * `destructive.cannotUndo` read "The artifacts and their metadata are removed
 * permanently", and three of the four irreversible verbs in the console
 * inherited it while removing no artifact at all: a revoked token, a forced
 * config reload, an audit-log purge. The stock sentence is the generic truth
 * now and the specific one is the caller's to give — but `defineProps` cannot
 * express "required when another prop is false", so this is what requires it.
 */
describe("the destructive contract", () => {
  it("states a consequence at every irreversible call site", () => {
    const CALL_SITE = /<DestructiveConfirm\b[\s\S]*?(?:\/>|>)/g;

    const offenders: string[] = [];
    for (const [file, source] of Object.entries(SOURCES)) {
      if (file.includes("/destructive-confirm/")) continue;
      for (const [tag] of source.matchAll(CALL_SITE)) {
        // `v-bind` hands the props over as one object; those sites are graded
        // by the `confirmProps` they build, which TypeScript already reads.
        if (/\bv-bind=/.test(tag)) continue;
        if (!/\breversible\b/.test(tag) && !/:consequence=/.test(tag)) {
          offenders.push(`${file}: ${tag.split("\n")[0].trim()}`);
        }
      }
    }
    expect(offenders, "an irreversible action must say what it costs").toEqual([]);
  });
});

/**
 * A `new Error` whose message is a bare English sentence.
 *
 * The i18n audit skips `new Error(…)` by construction, on the grounds that an
 * error message reaches a log and translating it would make a stack trace
 * harder to search. That is right, and `NamespaceUpload` was the exception
 * that proves it: three of its throws were re-displayed through `error.value`,
 * so the gate read zero over English a publisher saw. They are catalogue keys
 * now.
 *
 * Whether a thrown message reaches a screen is not decidable from the source,
 * so this does the next best thing: it freezes the two that exist. Both are
 * control-flow throws whose text is never read — `LoginPage` discards the
 * error and shows `loginPage.invalidToken`, `useAuth` hands it to
 * `console.error` — and a third one appearing is the moment to check which
 * kind it is, rather than a year later.
 *
 * Only *bare literals* count. Everything else in the tree throws `t(…)`,
 * `extractMessage(…)` or protocol data (`HTTP ${status}`, a response body),
 * which are already the right shape.
 */
describe("thrown text", () => {
  const LITERAL_THROW = /new Error\(\s*["'][^"'\\]{4,}["']\s*\)/g;

  const FROZEN_LITERAL_THROWS: Record<string, number> = {
    "src/composables/useAuth.ts": 1,
    "src/pages/LoginPage.vue": 1,
  };

  it("throws a bare English sentence only where nothing displays it", () => {
    expect(
      census(LITERAL_THROW),
      "if this message can reach a screen it needs a catalogue key",
    ).toEqual(FROZEN_LITERAL_THROWS);
  });
});
