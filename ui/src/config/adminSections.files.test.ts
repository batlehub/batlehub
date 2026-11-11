import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { ADMIN_PAGE_VERBS } from "./adminSections";
import { LEGACY_REDIRECTS } from "./navigation";

/*
 * The half of RFC 0046 §4.3's checks that reads files, typed under
 * `tsconfig.node.json` like the repo's other file-reading tests; the router
 * half is `adminSections.test.ts`.
 */

describe("the design gate's admin routes (RFC 0046 §6.5)", () => {
  /**
   * `design-routes.mjs` is plain Node and cannot import the `.ts` table, so
   * its list stays a literal; this keeps the copy from drifting, which is how
   * eight admin pages went unmeasured.
   */
  it("measures every admin page, plus one registry's page", () => {
    const source = readFileSync(resolve(process.cwd(), "build/design-routes.mjs"), "utf8");
    const literal = /const ADMIN_ROUTES = \[([\s\S]*?)\];/.exec(source)?.[1] ?? "";
    const routes = [...literal.matchAll(/"(\/admin\/[^"]+)"/g)].map((m) => m[1] ?? "");
    const pages = Object.keys(ADMIN_PAGE_VERBS);
    expect(new Set(routes.filter((r) => pages.includes(r)))).toEqual(new Set(pages));
    // The one route that is not a table page is a concrete registry's page.
    expect(routes.filter((r) => !pages.includes(r))).toEqual([
      expect.stringMatching(/^\/admin\/observability\/registries\/[^/]+$/),
    ]);
  });
});

describe("hard-coded admin links (RFC 0046 §6.4)", () => {
  /**
   * A `RouterLink` to a `LEGACY_REDIRECTS` key resolves its `href` to "No match
   * found": a click redirects, but middle-click and copy-link break.
   */
  it("names no legacy path in a to= or path: literal", () => {
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const name of readdirSync(dir)) {
        const path = join(dir, name);
        if (statSync(path).isDirectory()) walk(path);
        else if (/\.(vue|ts)$/.test(name) && !name.endsWith(".test.ts")) files.push(path);
      }
    };
    walk(resolve(process.cwd(), "src"));
    const legacy = new Set(Object.keys(LEGACY_REDIRECTS));
    const offenders = files
      .filter((file) => !file.endsWith("navigation.ts"))
      .flatMap((file) =>
        [...readFileSync(file, "utf8").matchAll(/(?:\bto=|\bpath:\s*)"(\/[^"?#]*)"/g)]
          .filter((m) => legacy.has(m[1] ?? ""))
          .map((m) => `${file}: ${m[1]}`),
      );
    expect(offenders).toEqual([]);
  });
});
