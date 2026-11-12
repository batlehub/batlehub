import { describe, expect, it } from "vitest";
import { ADMIN_PAGE_VERBS } from "@/config/adminSections";
import { RED_PATHS } from "@/config/redPaths";
import {
  actionEntries,
  looksLikeCredential,
  matchEntries,
  pageEntries,
  registryEntries,
  searchEntry,
  type PaletteViewer,
} from "./palette";

const t = (key: string) => key;

/** A viewer holding `verbs` at the instance tier — and only there. */
function viewer(verbs: string[], extra: Partial<PaletteViewer> = {}): PaletteViewer {
  return {
    can: (verb) => verbs.includes(verb),
    canAny: (vs) => vs.some((v) => verbs.includes(v)),
    isAuthenticated: true,
    isOidc: false,
    hasRegistryAccess: true,
    ...extra,
  };
}

const ALL = [...new Set(Object.values(ADMIN_PAGE_VERBS))];

/**
 * RFC 0046 §7.1 T2: the palette's admin rows are exactly the pages the router
 * guard would open — `can(verb)` at the instance tier — for four verb sets.
 * The fourth holds `system:read` on one registry only: `can(verb)` (no
 * registry) is false for it, so it is offered no admin page and no hub.
 */
describe("palette access (T2, T3)", () => {
  const sets: [string, string[]][] = [
    ["admin", ALL],
    ["security group", ["quarantine:read", "findings:read"]],
    ["plain user", []],
    ["registry-only system:read", []],
  ];
  for (const [name, verbs] of sets) {
    it(`lists exactly the guard's pages for ${name}`, () => {
      const v = viewer(verbs);
      const admin = pageEntries(v, t)
        .map((e) => e.to)
        .filter((to) => typeof to === "string" && to.startsWith("/admin/"));
      const guard = Object.entries(ADMIN_PAGE_VERBS)
        .filter(([, verb]) => v.can(verb))
        .map(([to]) => to);
      expect(admin).toEqual(guard);
    });
  }

  it("offers no action whose page the guard would refuse", () => {
    const v = viewer(["quarantine:read"]);
    const actions = actionEntries(v, t, "");
    expect(actions.map((a) => a.key)).toEqual(["action:RP-5"]);
  });

  it("links a registry to its hub only with instance-tier system:read", () => {
    const regs = [{ name: "npm", type: "npm", mode: "proxy" }];
    expect(registryEntries(viewer(["system:read"]), regs)[0]?.to).toBe("/admin/registry/npm");
    expect(registryEntries(viewer([]), regs)[0]?.to).toEqual({
      path: "/packages",
      query: { registry: "npm" },
    });
  });

  it("encodes a registry name into the hub path (T5)", () => {
    const [entry] = registryEntries(viewer(["system:read"]), [
      { name: "../x", type: "npm", mode: "proxy" },
    ]);
    expect(entry?.to).toBe("/admin/registry/..%2Fx");
  });
});

describe("palette matching", () => {
  it("matches across accents and both languages' keywords", () => {
    const actions = actionEntries(viewer(ALL), t, "debloquer");
    expect(matchEntries(actions, "debloquer").map((e) => e.key)).toEqual(["action:RP-8"]);
    expect(matchEntries(actions, "bannière").map((e) => e.key)).toContain("action:RP-11");
  });

  it("requires every word to prefix a word, and ranks page > action > registry", () => {
    const v = viewer(ALL);
    const index = [
      ...registryEntries(v, [{ name: "audit-mirror", type: "npm", mode: "proxy" }]),
      ...actionEntries(v, t, "audit"),
      ...pageEntries(v, (k) => (k === "adminNav.auditLog" ? "Audit Log" : k)),
    ];
    const kinds = matchEntries(index, "audit").map((e) => e.kind);
    expect(kinds[0]).toBe("page");
    expect(kinds.at(-1)).toBe("registry");
    expect(matchEntries(index, "audit zzz")).toEqual([]);
  });

  it("matches nothing on an empty query: the empty state is the caller's", () => {
    expect(matchEntries(pageEntries(viewer(ALL), t), "  ")).toEqual([]);
  });
});

/** §4.2 rule 4 and §7.3: a pasted token is never turned into a URL. */
describe("credential-shaped queries", () => {
  const token = "bh_" + "a".repeat(40);

  it("recognises one long word, not a sentence", () => {
    expect(looksLikeCredential(token)).toBe(true);
    expect(looksLikeCredential("block one version of lodash please now")).toBe(false);
    expect(looksLikeCredential("lodash")).toBe(false);
  });

  it("offers no search row and no hand-off action for it", () => {
    const v = viewer(ALL);
    expect(searchEntry(v, token, "search")).toBeUndefined();
    expect(actionEntries(v, t, token).map((a) => a.key)).not.toContain("action:RP-6");
    expect(searchEntry(v, "lodash", "search")?.to).toEqual({
      path: "/packages",
      query: { q: "lodash" },
    });
  });

  it("offers no search row to a viewer without registry access", () => {
    expect(searchEntry(viewer([], { hasRegistryAccess: false }), "lodash", "s")).toBeUndefined();
  });
});

/** §4.3: a red path's target is a page the router knows. */
describe("red paths", () => {
  it("point at an admin page, or name the verb a hand-off needs", () => {
    for (const rp of RED_PATHS) {
      if (rp.verb) expect(rp.to.startsWith("/admin/")).toBe(false);
      else expect(ADMIN_PAGE_VERBS[rp.to], rp.id).toBeDefined();
    }
  });

  it("hands RP-6 the query, as the catalog search", () => {
    const rp6 = actionEntries(viewer(ALL), t, "lodash").find((a) => a.key === "action:RP-6");
    expect(rp6?.to).toEqual({ path: "/packages", query: { q: "lodash" } });
  });
});
