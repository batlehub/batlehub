import { describe, expect, it } from "vitest";

import { LEGACY_REDIRECTS, SECTION_INDEXES, accountTabs, primaryNav } from "./navigation";

/**
 * The shell follows the viewer (RFC 0003 §4.1).
 *
 * The rule these protect: a nav item appears when the viewer can *use* it, so
 * nothing in the bar leads to a redirect. That is a clarity rule and never an
 * authorisation one — hiding a link stops nobody, and the router guards plus the
 * server's RBAC are what actually refuse. The router suite owns that half.
 */
const ANON_OPEN = {
  isAuthenticated: false,
  hasAdminPage: false,
  hasSecurity: false,
  hasRegistryAccess: true,
};
const ANON_LOCKED = {
  isAuthenticated: false,
  hasAdminPage: false,
  hasSecurity: false,
  hasRegistryAccess: false,
};
const USER = {
  isAuthenticated: true,
  hasAdminPage: false,
  hasSecurity: false,
  hasRegistryAccess: true,
};
const ADMIN = {
  isAuthenticated: true,
  hasAdminPage: true,
  hasSecurity: true,
  hasRegistryAccess: true,
};

/* Labels are catalogue keys, not phrases — the component translates at render. */
describe("primaryNav", () => {
  it("offers nothing to an anonymous viewer on a locked instance", () => {
    expect(primaryNav(ANON_LOCKED)).toEqual([]);
  });

  it("offers the catalog and setup to an anonymous viewer on a public instance", () => {
    expect(primaryNav(ANON_OPEN).map((l) => l.to)).toEqual(["/packages", "/setup"]);
  });

  it("does not show a signed-in user an admin entry they cannot use", () => {
    expect(primaryNav(USER).some((l) => l.to === "/admin")).toBe(false);
  });

  it("gives a security delegate the console and not the admin (RFC 0042)", () => {
    const paths = primaryNav({ ...USER, hasSecurity: true }).map((l) => l.to);
    expect(paths).toContain("/security");
    expect(paths).not.toContain("/admin");
  });

  it("gives an admin the operator entry, last", () => {
    expect(primaryNav(ADMIN).at(-1)).toEqual({ to: "/admin", label: "nav.admin" });
  });

  /**
   * Diagnostics used to sit between Explore and Setup at equal weight. They are
   * excellent for the ten minutes a year a pull is mysteriously 403ing, and
   * dead weight for the rest of it.
   */
  it("keeps diagnostics out of the primary bar for every viewer", () => {
    for (const viewer of [ANON_OPEN, USER, ADMIN]) {
      const paths = primaryNav(viewer).map((l) => l.to);
      expect(paths).not.toContain("/tools");
      expect(paths).not.toContain("/access-check");
    }
  });
});

describe("accountTabs", () => {
  /** Tokens are OIDC-only; a static-token user should never see the tab. */
  it("hides the tokens tab from a non-OIDC session", () => {
    expect(accountTabs({ isOidc: false }).map((t) => t.to)).toEqual([
      "/me/profile",
      "/me/namespace",
      "/me/cli",
    ]);
  });

  it("shows it to an OIDC session", () => {
    expect(accountTabs({ isOidc: true }).map((t) => t.to)).toContain("/me/tokens");
  });
});

describe("the redirect tables", () => {
  /** A target that is itself a legacy path would redirect twice, or loop. */
  it("never points a legacy path at another legacy path", () => {
    for (const [from, to] of Object.entries(LEGACY_REDIRECTS)) {
      expect(LEGACY_REDIRECTS[to], `${from} → ${to} → …`).toBeUndefined();
    }
  });

  it("never lets a section index point at another index", () => {
    for (const [from, to] of Object.entries(SECTION_INDEXES)) {
      expect(SECTION_INDEXES[to], `${from} → ${to} → …`).toBeUndefined();
    }
  });

  it("keeps the two tables disjoint, so one path has one meaning", () => {
    const overlap = Object.keys(LEGACY_REDIRECTS).filter((p) => p in SECTION_INDEXES);
    expect(overlap).toEqual([]);
  });

  it("uses absolute paths throughout", () => {
    for (const path of [
      ...Object.keys(LEGACY_REDIRECTS),
      ...Object.values(LEGACY_REDIRECTS),
      ...Object.keys(SECTION_INDEXES),
      ...Object.values(SECTION_INDEXES),
    ]) {
      expect(path.startsWith("/"), path).toBe(true);
    }
  });
});
