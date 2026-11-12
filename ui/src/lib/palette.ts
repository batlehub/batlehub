/**
 * The command palette's index and matcher (RFC 0046 §4.2), kept out of the
 * component so the access rule — the palette lists no more than the router
 * guard lets the viewer open (§7.1 T2) — is a unit test over plain data.
 *
 * Every admin row is gated with `can(verb)` at the instance tier, the call
 * `checkMetaGuards` makes. The per-registry `can(verb, name)` is never used for
 * a link: a viewer holding a verb on one registry only would be offered a page
 * the guard then bounces.
 */
import type { RouteLocationRaw } from "vue-router";
import { ADMIN_PAGE_VERBS, ADMIN_SECTIONS } from "@/config/adminSections";
import { RED_PATHS } from "@/config/redPaths";
import { accountTabs, registryHubPath, SECURITY_VERBS, TOOLS_TABS } from "@/config/navigation";

export type PaletteKind = "page" | "action" | "registry" | "search";

export interface PaletteEntry {
  key: string;
  kind: PaletteKind;
  label: string;
  hint?: string;
  to: RouteLocationRaw;
  /** Extra words that find the entry, never shown. */
  keywords: string[];
}

export interface PaletteViewer {
  can: (verb: string) => boolean;
  canAny: (verbs: readonly string[]) => boolean;
  isAuthenticated: boolean;
  isOidc: boolean;
  hasRegistryAccess: boolean;
}

export interface PaletteRegistry {
  name: string;
  type: string;
  mode: string;
}

type Translate = (key: string) => string;

/** Case- and accent-insensitive: "débloquer" is found by "debloquer". */
export function normalize(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

/**
 * One word of 32 characters or more: the shape of a pasted token (RP-8). The
 * search row would write it into the browser history (§7.3), so it is not
 * offered for such a query.
 */
export function looksLikeCredential(query: string): boolean {
  const q = query.trim();
  return q.length >= 32 && !/\s/.test(q);
}

/** Every page the viewer may open, the ones the guard would refuse left out. */
export function pageEntries(viewer: PaletteViewer, t: Translate): PaletteEntry[] {
  const admin = ADMIN_SECTIONS.flatMap((section) =>
    section.pages
      .filter((page) => viewer.can(page.verb))
      .map<PaletteEntry>((page) => ({
        key: `page:${page.to}`,
        kind: "page",
        label: t(page.label),
        hint: t(section.label),
        to: page.to,
        keywords: [t(section.label)],
      })),
  );
  return [...admin, ...ownPages(viewer, t)];
}

/** The pages that are the viewer's own, admin or not. */
export function ownPages(viewer: PaletteViewer, t: Translate): PaletteEntry[] {
  const page = (to: string, label: string): PaletteEntry => ({
    key: `page:${to}`,
    kind: "page",
    label: t(label),
    to,
    keywords: [],
  });
  const out: PaletteEntry[] = [];
  if (viewer.canAny(SECURITY_VERBS)) out.push(page("/security", "nav.security"));
  if (viewer.isAuthenticated) {
    out.push(...accountTabs({ isOidc: viewer.isOidc }).map((l) => page(l.to, l.label)));
  }
  out.push(...TOOLS_TABS.map((l) => page(l.to, l.label)), page("/setup", "nav.setup"));
  return out;
}

/** The red-path actions whose page the viewer may open, in §2.1 order. */
export function actionEntries(viewer: PaletteViewer, t: Translate, query: string): PaletteEntry[] {
  return RED_PATHS.flatMap<PaletteEntry>((rp) => {
    const verb = rp.verb ?? ADMIN_PAGE_VERBS[rp.to];
    if (!verb || !viewer.can(verb)) return [];
    // RP-6 needs a package before it can block one: it hands the query to the
    // catalog, like the search row, instead of naming a fixed page.
    const handoff = rp.verb !== undefined;
    if (handoff && looksLikeCredential(query)) return [];
    const to: RouteLocationRaw =
      handoff && query.trim()
        ? { path: rp.to, query: { q: query.trim() } }
        : { path: rp.to, query: rp.query ?? {} };
    return [
      {
        key: `action:${rp.id}`,
        kind: "action",
        label: t(rp.label),
        to,
        keywords: [...rp.keywords.en, ...rp.keywords.fr],
      },
    ];
  });
}

/** A registry links to its admin page when the guard would open it. */
export function registryEntries(
  viewer: PaletteViewer,
  registries: PaletteRegistry[],
): PaletteEntry[] {
  const hub = viewer.can("system:read");
  return registries.map((r) => ({
    key: `registry:${r.name}`,
    kind: "registry",
    label: r.name,
    hint: `${r.type} · ${r.mode}`,
    to: hub ? registryHubPath(r.name) : { path: "/packages", query: { registry: r.name } },
    keywords: [r.type],
  }));
}

const RANK: Record<PaletteKind, number> = { page: 0, action: 1, registry: 2, search: 3 };

/**
 * Every query word must prefix a word of the label or a keyword; ranked page >
 * action > registry, then the shorter label first. An empty query matches
 * nothing here: the empty state is the caller's.
 */
export function matchEntries(entries: PaletteEntry[], query: string): PaletteEntry[] {
  const words = normalize(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return [];
  const hits = entries.filter((entry) => {
    const haystack = [entry.label, ...entry.keywords].flatMap((s) =>
      normalize(s).split(/[^\p{L}\p{N}]+/u),
    );
    return words.every((w) => haystack.some((h) => h.startsWith(w)));
  });
  return hits.sort((a, b) => RANK[a.kind] - RANK[b.kind] || a.label.length - b.label.length);
}

/** The last row: hand the query to the catalog, unless it looks like a token. */
export function searchEntry(
  viewer: PaletteViewer,
  query: string,
  label: string,
): PaletteEntry | undefined {
  const q = query.trim();
  if (!q || !viewer.hasRegistryAccess || looksLikeCredential(q)) return undefined;
  return {
    key: "search",
    kind: "search",
    label,
    to: { path: "/packages", query: { q } },
    keywords: [],
  };
}
