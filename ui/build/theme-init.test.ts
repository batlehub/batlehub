import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { minify } from "vite";
import { afterEach, describe, expect, it, vi } from "vitest";

import { readThemeInit } from "./theme-init.ts";

const SOURCE = readThemeInit();
const DOCUMENT = readFileSync(resolve(process.cwd(), "index.html"), "utf8");

/**
 * The two forms that actually run: the source, which `vite dev` serves, and the
 * minified asset the plugin emits for production. Both are put through every
 * case below, because a minifier is a program and this one runs before the
 * first paint on every load — "it is only whitespace" is exactly the assumption
 * that makes a difference invisible until it is in front of a reader.
 */
const FORMS: [name: string, code: string][] = [
  ["source", SOURCE],
  ["minified", (await minify("theme-init.js", SOURCE)).code],
];

/**
 * The pre-paint theme script, run as a browser would run it.
 *
 * `tokens.css` makes dark the default and light opt-in through
 * `[data-theme="light"]`, and nothing set that attribute until `useColorMode`
 * mounted — so every reader on a light preference saw a full page of near-black
 * first, for as long as it took to fetch and parse the bundle.
 *
 * This file exists for one reason beyond checking the resolution: the script
 * and `useColorMode` run milliseconds apart over the same storage key, and if
 * they ever disagree about an input the reader gets a *second* flash at mount —
 * the same defect moved rather than removed. So the cases below are the ones
 * where the two could drift, and each is written against what `@vueuse/core`
 * actually does, read from `useColorMode` in the installed package:
 *
 *     const modes = { auto: "", light: "light", dark: "dark", ... }
 *     const system = computed(() => preferredDark.value ? "dark" : "light")
 *     const state  = computed(() => store.value === "auto" ? system.value : store.value)
 *     el.setAttribute(attribute, modes[mode] ?? mode)
 *
 * and `usePreferredDark` is `useMediaQuery("(prefers-color-scheme: dark)")`,
 * whose `matches` ref starts at `false` and stays there when `matchMedia` is
 * unavailable.
 */

/**
 * The three browser globals this test touches, named locally.
 *
 * `tsconfig.node.json` has no DOM lib, and that is deliberate: everything else
 * under `build/` is a build script, and a build script reaching for `document`
 * should fail to typecheck. This file is the one exception — it runs a browser
 * file under vitest's jsdom — so it declares what it needs instead of widening
 * the project for everything beside it.
 */
const dom = globalThis as unknown as {
  document: {
    documentElement: {
      getAttribute(name: string): string | null;
      removeAttribute(name: string): void;
    };
  };
  localStorage: { setItem(key: string, value: string): void; clear(): void };
  Storage: { prototype: { getItem: (key: string) => string | null } };
};

/** Run one form against the current fake window, as a `<script>` tag would. */
const run = (code: string) => new Function(code)();

/** Point `matchMedia` at a fixed answer for the dark query. */
function systemPrefers(dark: boolean): void {
  vi.stubGlobal(
    "matchMedia",
    vi.fn((query: string) => ({
      matches: query === "(prefers-color-scheme: dark)" ? dark : false,
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    })),
  );
}

const applied = () => dom.document.documentElement.getAttribute("data-theme");

afterEach(() => {
  vi.unstubAllGlobals();
  dom.localStorage.clear();
  dom.document.documentElement.removeAttribute("data-theme");
  dom.document.documentElement.removeAttribute("data-density");
});

describe("theme-init", () => {
  describe.each(FORMS)("resolves the rendition the way useColorMode will (%s)", (_form, code) => {
    /**
     * The four cases that differ only in what storage holds, because they are
     * one rule: a stored `light` or `dark` wins over the system, and everything
     * else — `auto`, or a value nobody recognises — resolves from it.
     *
     * `auto` is a row here rather than a special case because `emitAuto: true`
     * makes it a value really written to storage, not just an internal state:
     * `useStorage` writes the default on first read, so a reader who has never
     * touched the toggle has a stored `auto`, not an empty key.
     */
    const STORED: [stored: string, systemDark: boolean, expected: string][] = [
      ["light", true, "light"],
      ["dark", false, "dark"],
      ["auto", true, "dark"],
      ["chartreuse", true, "dark"],
    ];

    it.each(STORED)("resolves a stored %p against a dark=%p system", (stored, dark, expected) => {
      dom.localStorage.setItem("batlehub.theme", stored);
      systemPrefers(dark);
      run(code);
      expect(applied()).toBe(expected);
    });

    /* Density follows the same pre-paint path: `compact` is written, anything
       else leaves the stylesheet's comfortable default alone. */
    it.each([
      ["compact", "compact"],
      ["comfortable", null],
      ["sideways", null],
    ])("applies a stored density of %p as %p", (stored, expected) => {
      dom.localStorage.setItem("batlehub.density", stored);
      systemPrefers(false);
      run(code);
      expect(dom.document.documentElement.getAttribute("data-density")).toBe(expected);
    });

    it("follows the system on a first visit, with nothing stored", () => {
      systemPrefers(false);
      run(code);
      expect(applied()).toBe("light");
    });

    /**
     * `useMediaQuery`'s `matches` ref is initialised to `false` and only ever
     * set from a real query, so vueuse resolves *light* where `matchMedia` does
     * not exist — not the palette's own default. Guessing dark here because the
     * palette is dark would be a divergence that only shows up on the browsers
     * least able to report it.
     */
    it("falls back to light where matchMedia does not exist", () => {
      vi.stubGlobal("matchMedia", undefined);
      run(code);
      expect(applied()).toBe("light");
    });

    /** Safari in private mode, or a cookie policy that blocks storage. */
    it("still answers when localStorage throws", () => {
      vi.spyOn(dom.Storage.prototype, "getItem").mockImplementation(() => {
        throw new Error("access denied");
      });
      systemPrefers(true);
      expect(() => run(code)).not.toThrow();
      expect(applied()).toBe("dark");
    });

    it("writes the resolved value, never the stored one", () => {
      // `useColorMode` sets the attribute from `state`, which is already
      // resolved. Writing "auto" here would leave `[data-theme="auto"]` on the
      // element, which matches no rule in `tokens.css` and would silently mean
      // "dark" until the bundle corrected it.
      dom.localStorage.setItem("batlehub.theme", "auto");
      systemPrefers(false);
      run(code);
      expect(applied()).not.toBe("auto");
      expect(applied()).toBe("light");
    });
  });

  /**
   * The reason the plugin exists. This is a blocking script in `<head>`, so its
   * size is time before anything is drawn — and the file is mostly comment,
   * because agreeing with `useColorMode` is the only thing standing between it
   * and a second flash at mount. Both are true, so the reader gets the
   * reasoning and the browser gets a quarter of a kilobyte.
   */
  it("ships without the reasoning it is mostly made of", () => {
    const [, minified] = FORMS[1];
    expect(minified).not.toContain("/*");
    expect(minified).not.toContain("useColorMode");
    expect(Buffer.byteLength(minified)).toBeLessThan(Buffer.byteLength(SOURCE) / 4);
  });

  /**
   * The other half of the fix. A script that resolves correctly but runs late
   * restores the flash it was written to remove, and nothing about the file
   * itself would say so.
   */
  describe("is loaded before the first paint", () => {
    const tag = /<script\b[^>]*\bsrc="\/theme-init\.js"[^>]*>/.exec(DOCUMENT)?.[0] ?? "";

    it("is referenced by the document", () => {
      expect(tag, "index.html must load /theme-init.js").not.toBe("");
    });

    it("is not deferred, in any of the three ways it could be", () => {
      // `type="module"` is the one that looks harmless: module scripts are
      // deferred by definition, so it would be as late as the bundle itself.
      expect(tag).not.toMatch(/\bdefer\b/);
      expect(tag).not.toMatch(/\basync\b/);
      expect(tag).not.toMatch(/type="module"/);
    });

    it("runs before the bundle that would otherwise decide the theme", () => {
      expect(DOCUMENT.indexOf('src="/theme-init.js"')).toBeLessThan(
        DOCUMENT.indexOf('src="/src/main.ts"'),
      );
    });

    /**
     * The reason this is a file at all. `buildCsp` emits `script-src 'self'`
     * with no nonce and no hash, and `crates/web/src/spa.rs` can only *narrow*
     * what the build produced — so an inline block here would not execute, in
     * production only, where nobody runs the tests.
     */
    it("carries no inline body, which the policy would refuse", () => {
      const inline = /<script(?![^>]*\bsrc=)[^>]*>[\s\S]*?<\/script>/.exec(DOCUMENT)?.[0];
      expect(inline, "script-src 'self' admits no inline script").toBeUndefined();
    });
  });
});
