import { describe, expect, it, vi } from "vitest";

// The router imports `useAuth`, which reaches the generated SDK; nothing here
// navigates, so the SDK only has to exist.
vi.mock("@/client/sdk.gen", () => ({ me: vi.fn(), oidcRefresh: vi.fn() }));
vi.mock("@/client/client.gen", () => ({ client: { setConfig: vi.fn() } }));

import { router } from "@/router";
import { ADMIN_PAGE_VERBS, ADMIN_SECTIONS, firstAdminPage } from "./adminSections";
import { LEGACY_REDIRECTS, SECTION_INDEXES } from "./navigation";

/** The router's `/admin` children that render a page (not the redirects). */
const adminPageRoutes = router
  .getRoutes()
  .filter((r) => r.path.startsWith("/admin/") && !r.redirect && r.components);

/**
 * RFC 0046 §4.3: the static tables every admin consumer is derived from are
 * checked against the router here, so a page missing from one fails the build
 * instead of the guard silently redirecting it away. The file-reading half is
 * `adminSections.files.test.ts`.
 */
describe("ADMIN_SECTIONS", () => {
  it("gives every /admin page route a verb", () => {
    const unguarded = adminPageRoutes.filter(
      (r) => r.meta.verb === undefined && ADMIN_PAGE_VERBS[r.path] === undefined,
    );
    expect(unguarded.map((r) => r.path)).toEqual([]);
  });

  it("puts every page in exactly one section", () => {
    const pages = ADMIN_SECTIONS.flatMap((section) => section.pages.map((page) => page.to));
    expect(pages.length).toBe(new Set(pages).size);
    for (const section of ADMIN_SECTIONS) {
      const own = section.pages.every((p) => p.to === section.to || p.to.startsWith(section.to));
      expect(own, section.to).toBe(true);
    }
  });

  it("derives exactly the router's page routes that carry no meta verb", () => {
    const routed = adminPageRoutes.filter((r) => r.meta.verb === undefined).map((r) => r.path);
    expect(new Set(Object.keys(ADMIN_PAGE_VERBS))).toEqual(new Set(routed));
  });

  it("points every legacy redirect and section index at a route", () => {
    for (const [from, to] of Object.entries({ ...LEGACY_REDIRECTS, ...SECTION_INDEXES })) {
      expect(router.resolve(to).matched.length, `${from} → ${to}`).toBeGreaterThan(0);
    }
  });

  /**
   * The landing page is the first page of the table a verb set holds, and
   * `tests/heavy/console_fetch.sh:234` asserts the security group's. Pinned
   * here so a reorder of the table shows up as a unit failure first.
   */
  it.each<[string[], string | undefined]>([
    [["quarantine:read", "findings:read", "flags:read", "audit:read"], "/admin/security/scans"],
    [["audit:read"], "/admin/observability/audit-log"],
    [["stats:read", "config:read"], "/admin/dashboard"],
    [["config:read"], "/admin/operations/config-reload"],
    [[], undefined],
  ])("lands %j on %s", (verbs, landing) => {
    expect(firstAdminPage((v) => verbs.includes(v))).toBe(landing);
  });
});
