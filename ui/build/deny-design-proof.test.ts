import { describe, expect, it, vi } from "vitest";

import { denyDesignProofPlugin, isDesignProofPath } from "./deny-design-proof.ts";

/**
 * Drive the plugin the way Vite does: install it, then hand the captured
 * middleware a request.
 *
 * The assertion that matters is not "the predicate returns true" — it is that a
 * request for the artefact ends in the middleware with a 404 and *never reaches
 * `next()`*, because everything downstream of it is what used to serve the file.
 */
function callMiddleware(url: string) {
  let handler: ((req: unknown, res: unknown, next: () => void) => void) | undefined;
  const server = { middlewares: { use: (fn: typeof handler) => (handler = fn) } };

  const plugin = denyDesignProofPlugin();
  const configureServer = plugin.configureServer as (s: typeof server) => void;
  configureServer(server);
  expect(handler, "the plugin installed no middleware").toBeTypeOf("function");

  const res = { statusCode: 200, end: vi.fn() };
  const next = vi.fn();
  handler!({ url }, res, next);
  return { res, next };
}

describe("isDesignProofPath", () => {
  it.each([
    "/design-proof/",
    "/design-proof/index.html",
    "/design-proof/fonts/silkscreen-400.woff2",
    "/design-proof/index.html?t=1",
    "/design-proof/index.html#top",
    // The absolute form Vite serves from `/@fs/`, which is the same file by
    // another route and was 200 alongside the direct one.
    "/@fs/projects/proxy-cache/ui/design-proof/index.html",
    // Percent-encoded separators arrive at the static middleware decoded.
    "/design-proof%2Findex.html",
    "/%64esign-proof/index.html",
  ])("refuses %s", (url) => {
    expect(isDesignProofPath(url)).toBe(true);
  });

  it.each([
    "/",
    "/packages",
    "/src/pages/PackageCatalog.vue",
    "/logo.svg",
    "/banner.png",
    "/fonts/silkscreen-400.woff2",
    // A whole segment, not a substring: neither of these is the artefact, and a
    // rule that swallowed them would be denying files it was never asked to.
    "/design-proofs/index.html",
    "/my-design-proof-notes.md",
  ])("serves %s", (url) => {
    expect(isDesignProofPath(url)).toBe(false);
  });

  it("keeps refusing when the URL has a malformed escape", () => {
    // `decodeURIComponent("%")` throws. The raw form still carries the segment,
    // and a decoder that gave up is not a reason to hand the file over.
    expect(isDesignProofPath("/design-proof/%")).toBe(true);
  });
});

describe("denyDesignProofPlugin", () => {
  it("answers 404 and stops the chain for the artefact", () => {
    const { res, next } = callMiddleware("/design-proof/index.html");

    expect(res.statusCode).toBe(404);
    expect(res.end).toHaveBeenCalledOnce();
    expect(next).not.toHaveBeenCalled();
  });

  it("passes every other request through untouched", () => {
    const { res, next } = callMiddleware("/packages");

    expect(next).toHaveBeenCalledOnce();
    expect(res.end).not.toHaveBeenCalled();
    expect(res.statusCode).toBe(200);
  });
});
