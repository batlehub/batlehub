import { describe, expect, it, vi } from "vitest";

// The router imports `useAuth`, which reaches the generated SDK; nothing here
// navigates, so the SDK only has to exist.
vi.mock("@/client/sdk.gen", () => ({ me: vi.fn(), oidcRefresh: vi.fn() }));
vi.mock("@/client/client.gen", () => ({ client: { setConfig: vi.fn() } }));

import { router } from "@/router";
import { ADMIN_PAGE_VERBS, ADMIN_SECTIONS, firstAdminPage } from "./adminSections";
import { LEGACY_REDIRECTS, SECTION_INDEXES } from "./navigation";
import { RED_PATHS } from "./redPaths";
import {
  firstSecurityPage,
  SECURITY_PAGE_VERBS,
  SECURITY_SECTIONS,
  securitySectionOf,
} from "./securitySections";

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
    expect(pages).toHaveLength(new Set(pages).size);
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
      const path = to.split("#")[0] ?? to;
      expect(router.resolve(path).matched.length, `${from} → ${to}`).toBeGreaterThan(0);
    }
  });

  /**
   * The landing page is the first page of the table a verb set holds, and
   * `tests/heavy/console_fetch.sh:234` asserts the security group's. Pinned
   * here so a reorder of the table shows up as a unit failure first.
   */
  it.each<[string[], string | undefined]>([
    // RFC 0050: scans left the admin, so the security group's first admin
    // page is the audit log.
    [["quarantine:read", "findings:read", "flags:read", "audit:read"], "/admin/system/audit-log"],
    [["audit:read"], "/admin/system/audit-log"],
    [["stats:read", "config:read"], "/admin/dashboard"],
    [["config:read"], "/admin/system/configuration"],
    [["cache:warm"], "/admin/registries/warming"],
    [[], undefined],
  ])("lands %j on %s", (verbs, landing) => {
    expect(firstAdminPage((v) => verbs.includes(v))).toBe(landing);
  });
});

/** RFC 0050 §6.6: the security space's table, checked against the router the same way (T9). */
describe("SECURITY_SECTIONS", () => {
  const securityRoutes = router
    .getRoutes()
    .filter(
      (r) =>
        (r.path === "/security" || r.path.startsWith("/security/")) &&
        r.components &&
        r.children.length === 0,
    );

  it("gives every /security route a meta verb", () => {
    expect(securityRoutes.filter((r) => r.meta.verb === undefined).map((r) => r.path)).toEqual([]);
  });

  it("routes exactly the table's pages, plus one registry's coverage", () => {
    const routed = securityRoutes.map((r) => r.path).filter((p) => !p.includes(":"));
    expect(new Set(routed)).toEqual(new Set(Object.keys(SECURITY_PAGE_VERBS)));
    expect(securityRoutes.filter((r) => r.path.includes(":")).map((r) => r.path)).toEqual([
      "/security/coverage/:registry",
    ]);
  });

  it("names every page once", () => {
    const pages = SECURITY_SECTIONS.flatMap((s) => s.pages.map((p) => p.to));
    expect(pages.length).toBe(new Set(pages).size);
  });

  /** §4.2: the space opens on the first page the viewer may read. */
  it.each<[string[], string | undefined]>([
    [["quarantine:read"], "/security"],
    [["flags:read"], "/security"],
    [["findings:read"], "/security/findings"],
    [["audit:read"], "/security/exposure"],
    [[], undefined],
  ])("opens %j on %s", (verbs, landing) => {
    expect(firstSecurityPage((v) => verbs.includes(v))).toBe(landing);
  });

  it("finds a path's group by its longest page, the overview prefixing all", () => {
    expect(securitySectionOf("/security/holds")?.to).toBe("/security/triage");
    expect(securitySectionOf("/security/coverage/npm")?.to).toBe("/security/follow-up");
    expect(securitySectionOf("/security")?.to).toBe("/security");
  });
});

/** RFC 0050 §6.6: a red path whose target has no verb would vanish from the palette. */
describe("RED_PATHS", () => {
  it("resolves every target to a verb", () => {
    const missing = RED_PATHS.filter(
      (rp) => !(rp.verb ?? ADMIN_PAGE_VERBS[rp.to] ?? SECURITY_PAGE_VERBS[rp.to]),
    ).map((rp) => rp.id);
    expect(missing).toEqual([]);
  });
});
