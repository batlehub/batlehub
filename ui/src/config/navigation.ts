/**
 * Navigation as data (RFC 0003 §4.1, §4.2).
 *
 * Two things live here that were previously scattered:
 *
 *  1. **Which links a viewer gets**, derived from their identity rather than
 *     rendered-then-hidden. A nav item is shown when the viewer can *use* it,
 *     so nothing in the bar leads to a redirect. This is a clarity rule, never
 *     an authorisation one — the router guards and the server's RBAC are what
 *     actually stop anyone, and typing the URL still hits both.
 *
 *  2. **The legacy redirect table.** These were ~20 hand-written entries in the
 *     router, added one per moved page, which is how the section list and its
 *     aliases drifted apart. One table means a moved route is one edit.
 */

export interface NavLink {
  to: string;
  /** A catalogue key, not a phrase: this is data, and data is not language. */
  label: string;
}

export interface NavViewer {
  isAuthenticated: boolean;
  isAdmin: boolean;
  /** An anonymous viewer on a locked instance can reach nothing but /login. */
  hasRegistryAccess: boolean;
}

/**
 * The primary bar. Deliberately short: diagnostics moved to `/tools` and account
 * surfaces to `/me`, because a tool used twice a year should not hold the same
 * weight as the catalog. They are reachable from the user menu and, more
 * usefully, from the errors that make you want them.
 */
export function primaryNav(viewer: NavViewer): NavLink[] {
  if (!viewer.hasRegistryAccess && !viewer.isAuthenticated) return [];

  const links: NavLink[] = [
    { to: "/packages", label: "nav.packages" },
    { to: "/setup", label: "nav.setup" },
  ];
  if (viewer.isAdmin) links.push({ to: "/admin", label: "nav.admin" });
  return links;
}

/** The account hub's tabs. `/me/tokens` is OIDC-only; the router enforces it. */
export function accountTabs(viewer: { isOidc: boolean }): NavLink[] {
  return [
    { to: "/me/profile", label: "account.profile" },
    ...(viewer.isOidc ? [{ to: "/me/tokens", label: "account.tokens" }] : []),
    { to: "/me/namespace", label: "account.namespace" },
    { to: "/me/cli", label: "account.cli" },
  ];
}

/** The diagnostics hub's tabs. */
export const TOOLS_TABS: NavLink[] = [
  { to: "/tools/access-check", label: "tools.accessCheck" },
  { to: "/tools/url-mapper", label: "tools.urlMapper" },
];

/**
 * Every path that used to resolve and now lives somewhere else.
 *
 * Deep links in `docs/`, bookmarks and CI scripts point at these, so a redirect
 * that breaks a documented URL is a review blocker (RFC 0003 §9). Query and
 * hash are preserved by the router guard that consumes this table.
 */
export const LEGACY_REDIRECTS: Record<string, string> = {
  // Account surfaces, gathered into one hub.
  "/profile": "/me/profile",
  "/tokens": "/me/tokens",
  "/my-namespace": "/me/namespace",
  "/cli": "/me/cli",

  // Diagnostics, demoted out of the primary nav.
  "/access-check": "/tools/access-check",
  "/path-mapper": "/tools/url-mapper",

  // Admin pages that moved when the sections were introduced.
  "/admin/bulk": "/admin/packages/bulk",
  // RFC 0004 Phase 5 (*merge*). The two pre-section aliases are re-pointed at
  // the destination in the same change as the two section paths: a redirect
  // that lands on another redirect key is a chain, which `navigation.test.ts`
  // refuses.
  "/admin/users": "/admin/security/blocks",
  "/admin/ip-blocks": "/admin/security/blocks",
  "/admin/security/users": "/admin/security/blocks",
  "/admin/security/ip-blocks": "/admin/security/blocks",
  "/admin/access-check": "/admin/security/access-check",
  "/admin/team-namespaces": "/admin/namespaces/team-namespaces",
  "/admin/beta-channel": "/admin/namespaces/beta-channel",
  "/admin/config-reload": "/admin/operations/config-reload",
  "/admin/warming": "/admin/operations/warming",
  // RFC 0004 Phase 5 (*remove*): the explore-cache page's one job was pressing
  // a button, and the control now sits beside Clear Cache on the health page —
  // where an operator actually notices the explorer is stale. Both entries
  // point at the destination directly: a redirect that lands on another
  // redirect key is a chain, and `navigation.test.ts` refuses those.
  "/admin/explore-cache": "/admin/observability/health",
  "/admin/operations/explore-cache": "/admin/observability/health",
  "/admin/health": "/admin/observability/health",
  "/admin/sbom": "/admin/operations/sbom",
  "/admin/observability/sbom": "/admin/operations/sbom",
  "/admin/audit-log": "/admin/observability/audit-log",
};

/** Section index paths, which land on their first tab. */
export const SECTION_INDEXES: Record<string, string> = {
  "/me": "/me/profile",
  "/tools": "/tools/access-check",
  "/admin": "/admin/dashboard",
  "/admin/packages": "/admin/packages/all",
  "/admin/security": "/admin/security/blocks",
  "/admin/namespaces": "/admin/namespaces/team-namespaces",
  // The read-only tab, not config reload: a section should open on what it
  // knows, not on the control that changes a running instance.
  "/admin/operations": "/admin/operations/upstream",
  "/admin/observability": "/admin/observability/health",
  // Not a LEGACY_REDIRECTS entry: `/admin/notifications` is not removed, it
  // becomes a section root like `/admin/packages`. The sidebar still points at
  // it, and `AdminLayout`'s `isActive` matches by prefix.
  "/admin/notifications": "/admin/notifications/subscriptions",
};
