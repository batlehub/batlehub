import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

/* Read the token file as text off disk, resolved from the vitest root (ui/).
   Neither of the tidier routes works here: `import.meta.url` is an http URL
   under Vite's pipeline, and a `?raw` import goes through the CSS transform. */
const CSS = readFileSync(resolve(process.cwd(), "src/design/tokens.css"), "utf8");

import {
  contrastRatio,
  isInGamut,
  maxChroma,
  oklchToLinearSrgb,
  parseOklch,
  toHex,
  type Rgb,
} from "./color.ts";

/**
 * The design system's colour rules, enforced against the token file itself.
 *
 * DESIGN.md quotes a ratio for every pair that carries text or a boundary.
 * Those numbers are the product's accessibility promise (PRODUCT.md makes
 * WCAG 2.2 AA binding), and a number in a document is not a promise — a
 * failing test is. Editing a token to a value that breaks a floor should stop
 * the build, in *both* renditions, which is the half that is easy to forget.
 */

/** Pull `--name: value;` declarations out of one selector's block. */
function block(selector: string): Record<string, string> {
  const start = CSS.indexOf(selector);
  if (start === -1) throw new Error(`selector not found in tokens.css: ${selector}`);
  const open = CSS.indexOf("{", start);
  const close = CSS.indexOf("}", open);
  const body = CSS.slice(open + 1, close);

  const out: Record<string, string> = {};
  for (const [, name, value] of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    out[name] = value.trim().replace(/\s*\/\*[\s\S]*?\*\/\s*$/, "");
  }
  return out;
}

const dark = block(":root {");
const light = { ...dark, ...block(':root[data-theme="light"]') };

const COLOR_TOKENS = [
  "--ground",
  "--ground-raised",
  "--ground-sunk",
  "--ink",
  "--ink-dim",
  "--rule-soft",
  "--rule-strong",
  "--accent",
  "--accent-ink",
  "--copper",
  "--focus",
] as const;

function rgb(tokens: Record<string, string>, name: string): Rgb {
  const raw = tokens[name];
  expect(raw, `${name} is missing from tokens.css`).toBeDefined();
  const parsed = parseOklch(raw);
  expect(parsed, `${name} is not an oklch() value: ${raw}`).not.toBeNull();
  return oklchToLinearSrgb(parsed!.l, parsed!.c, parsed!.h);
}

const RENDITIONS = [
  ["dark", dark],
  ["light", light],
] as const;

describe.each(RENDITIONS)("%s rendition", (_name, tokens) => {
  /**
   * The In-Gamut Rule. Two of Monofolio's own `--primary` values were outside
   * sRGB when measured — one per rendition — so this is the check that earns
   * its place rather than a formality.
   */
  it.each(COLOR_TOKENS)("%s is in-gamut for sRGB as authored", (name) => {
    const parsed = parseOklch(tokens[name])!;
    const colour = oklchToLinearSrgb(parsed.l, parsed.c, parsed.h);
    const limit = maxChroma(parsed.l, parsed.h);
    expect(
      isInGamut(colour),
      `${name} = ${tokens[name]} is outside sRGB; max chroma at this L/H is ${limit.toFixed(4)}`,
    ).toBe(true);
  });

  /** Body text and small text: 4.5:1. */
  it.each([
    ["--ink", "--ground"],
    ["--ink", "--ground-raised"],
    ["--ink", "--ground-sunk"],
    ["--ink-dim", "--ground"],
    ["--ink-dim", "--ground-raised"],
    ["--ink-dim", "--ground-sunk"],
    ["--accent", "--ground"],
    ["--copper", "--ground"],
    ["--accent-ink", "--accent"],
  ])("%s on %s clears 4.5:1", (fg, bg) => {
    const ratio = contrastRatio(rgb(tokens, fg), rgb(tokens, bg));
    expect(ratio, `${fg} on ${bg} = ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
  });

  /** Non-text contrast (WCAG 1.4.11): boundaries and the focus ring, 3:1. */
  it.each([
    ["--rule-strong", "--ground"],
    ["--rule-strong", "--ground-raised"],
    ["--rule-strong", "--ground-sunk"],
    ["--focus", "--ground"],
  ])("%s on %s clears 3:1", (fg, bg) => {
    const ratio = contrastRatio(rgb(tokens, fg), rgb(tokens, bg));
    expect(ratio, `${fg} on ${bg} = ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
  });

  /**
   * The Undependable Fill Rule, asserted from the other direction.
   *
   * These fills are *supposed* to be imperceptible — the WCAG ratio compresses
   * at both ends of the lightness range, so no fill step separates two surfaces
   * in either rendition, and state is carried by rule weight and ink instead.
   * Pinning it stops someone "fixing" the low number by lightening the token
   * and quietly reintroducing fill as a state channel, which would then fail
   * for colour-blind and low-vision users while looking fine to the author.
   */
  it.each(["--ground-raised", "--ground-sunk"])("%s is not a dependable surface", (name) => {
    const ratio = contrastRatio(rgb(tokens, name), rgb(tokens, "--ground"));
    expect(ratio, `${name} vs --ground = ${ratio.toFixed(2)}:1`).toBeLessThan(1.5);
  });

  /** The reversed segment cell (active nav, chosen setting) is a real pair. */
  it("the reversed segment pair clears 4.5:1", () => {
    expect(contrastRatio(rgb(tokens, "--ground"), rgb(tokens, "--ink"))).toBeGreaterThanOrEqual(
      4.5,
    );
  });
});

/**
 * The Integer Em Rule. Silkscreen is drawn on an 8px em, so its pixel only
 * stays square at integer multiples — which is why --t-display is discrete per
 * breakpoint rather than a clamp() that lands on the grid at a handful of
 * viewport widths.
 */
describe("display ramp", () => {
  it("every Silkscreen size is an integer multiple of 8px", () => {
    const sizes = [...CSS.matchAll(/--t-(?:px-sm|px-md|display):\s*(\d+)px/g)].map((m) =>
      Number(m[1]),
    );
    expect(sizes.length).toBeGreaterThanOrEqual(4); // 2 pixel steps + 2 display steps
    for (const size of sizes) expect(size % 8, `${size}px is not a multiple of 8`).toBe(0);
  });

  it("never uses clamp() for the display size", () => {
    expect(CSS).not.toMatch(/--t-display:\s*clamp/);
  });
});

/** A report, not an assertion — makes the measured palette visible in CI logs. */
describe("measured palette", () => {
  it.each(RENDITIONS)("%s renders as expected hexes", (name, tokens) => {
    const report = COLOR_TOKENS.map((t) => `${t}=${toHex(rgb(tokens, t))}`).join(" ");
    expect(report, `${name}: ${report}`).toContain("--ground=");
  });
});

/**
 * The One Synthetic Rule, made executable (DESIGN.md, Colors).
 *
 * "Crimson is the world's only invented colour and stays on its four jobs.
 * Copper carries 'waiting'; ink carries 'known'; dim ink carries everything
 * ordinary. A fifth hue does not get added to signal a fifth condition."
 *
 * Tailwind ships a full palette, so a fifth hue is always one utility away —
 * and 31 of them had accumulated across five files: green for "healthy" and
 * "allowed", yellow for config warnings, red beside the crimson that already
 * meant refused. Two of those pairings failed WCAG AA, and none had ever been
 * measured, because a colour that is not a token cannot be.
 *
 * Source-level rather than rendered: this is the point where the hue enters,
 * and a rendered scan only sees the ones that happen to be on screen.
 */
describe("the One Synthetic Rule", () => {
  const OFF_PALETTE =
    /\b(?:text|bg|border|ring|from|to|via|outline|decoration|shadow|fill|stroke|accent|caret|divide)-(?:red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|slate|gray|zinc|neutral|stone)-\d{2,3}\b/g;

  it("uses no colour outside the palette anywhere in src/", async () => {
    const { globSync } = await import("node:fs");
    const files = globSync("src/**/*.{vue,ts,css}", { cwd: process.cwd() }).filter(
      (f: string) => !f.includes("/client/"),
    );
    const offenders: string[] = [];
    for (const file of files) {
      const text = readFileSync(resolve(process.cwd(), file), "utf8");
      const hits = [...new Set(text.match(OFF_PALETTE) ?? [])];
      if (hits.length) offenders.push(`${file}: ${hits.join(", ")}`);
    }
    expect(offenders, "state is crimson (refused), copper (waiting) or ink (known)").toEqual([]);
  });
});
