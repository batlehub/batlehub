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
  /** Whether the viewer may open at least one admin page (RFC 0042 §4.3). */
  hasAdminPage: boolean;
  /** Whether the viewer holds a security verb at the instance tier. */
  hasSecurity: boolean;
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
  if (viewer.hasSecurity) links.push({ to: "/security", label: "nav.security" });
  if (viewer.hasAdminPage) links.push({ to: "/admin", label: "nav.admin" });
  return links;
}

/**
 * The verbs that open the security console (RFC 0042 §4.3), read at the
 * instance tier: the registry-tier `quarantine:read` every user holds by
 * default is their per-package view, not a console.
 */
export const SECURITY_VERBS = ["quarantine:read", "findings:read", "flags:read", "audit:read"];

/** The account hub's tabs. `/me/tokens` is OIDC-only; the router enforces it. */
export function accountTabs(viewer: { isOidc: boolean }): NavLink[] {
  return [
    { to: "/me/profile", label: "account.profile" },
    ...(viewer.isOidc ? [{ to: "/me/tokens", label: "account.tokens" }] : []),
    { to: "/me/namespace", label: "account.namespace" },
    { to: "/me/cli", label: "account.cli" },
  ];
}

/** The canonical package URL RFC 0003 settled on. */
export function packagePath(registry: string, name: string): string {
  return `/packages/${encodeURIComponent(registry)}/${encodeURIComponent(name)}`;
}

/**
 * One registry's admin page (RFC 0044 phase 5, RFC 0046 M4); a name is data, so
 * it is encoded. Singular `registry`, so no sibling page can shadow a registry
 * named `upstream` or `warming`.
 */
export function registryHubPath(name: string): string {
  return `/admin/registry/${encodeURIComponent(name)}`;
}

/** The registry page's old pattern (RFC 0044), redirected by the router. */
export const LEGACY_REGISTRY_HUB = /^\/admin\/observability\/registries\/([^/]+)$/;

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

  // Admin pages that moved when the sections were introduced. Every entry
  // points at its final page directly: a redirect that lands on another
  // redirect key is a chain, and `navigation.test.ts` refuses those.
  "/admin/bulk": "/admin/packages/bulk",
  // RFC 0004 Phase 5 (*merge*): blocked accounts and addresses on one page.
  "/admin/users": "/admin/access/blocks",
  "/admin/ip-blocks": "/admin/access/blocks",
  "/admin/security/users": "/admin/access/blocks",
  "/admin/security/ip-blocks": "/admin/access/blocks",
  "/admin/access-check": "/admin/access/check",
  "/admin/team-namespaces": "/admin/access/team-namespaces",
  "/admin/beta-channel": "/admin/access/beta-channel",
  "/admin/config-reload": "/admin/system/configuration",
  "/admin/warming": "/admin/registries/warming",
  // RFC 0004 Phase 5 (*remove*): the explore-cache page's one job was pressing
  // a button, and the control now sits beside Clear Cache on the health page.
  "/admin/explore-cache": "/admin/system/health",
  "/admin/operations/explore-cache": "/admin/system/health",
  "/admin/health": "/admin/system/health",
  "/admin/sbom": "/admin/packages/sbom",
  "/admin/observability/sbom": "/admin/packages/sbom",
  "/admin/audit-log": "/admin/system/audit-log",

  // RFC 0046 phase 3 (§15 Q8): the sections regrouped by trigger. Seventeen
  // pages and four section roots; one registry's page, a pattern, is
  // `handleLegacyPath`'s.
  "/admin/observability/registries": "/admin/registries",
  "/admin/operations/upstream": "/admin/registries/upstream",
  "/admin/operations/warming": "/admin/registries/warming",
  "/admin/operations/imports": "/admin/registries/imports",
  "/admin/operations/air-gap": "/admin/registries/air-gap",
  "/admin/operations/sbom": "/admin/packages/sbom",
  "/admin/security/access-check": "/admin/access/check",
  "/admin/security/authorization": "/admin/access/authorization",
  "/admin/security/blocks": "/admin/access/blocks",
  "/admin/namespaces/team-namespaces": "/admin/access/team-namespaces",
  "/admin/namespaces/beta-channel": "/admin/access/beta-channel",
  "/admin/operations/config-reload": "/admin/system/configuration",
  "/admin/operations/banner": "/admin/system/banner",
  "/admin/observability/health": "/admin/system/health",
  "/admin/observability/audit-log": "/admin/system/audit-log",
  "/admin/notifications/subscriptions": "/admin/system/notifications",
  "/admin/notifications/inbound": "/admin/system/inbound-events",
  "/admin/operations": "/admin/registries/upstream",
  "/admin/observability": "/admin/system/health",
  "/admin/namespaces": "/admin/access/team-namespaces",
  "/admin/notifications": "/admin/system/notifications",
};

/**
 * RFC 0046 §15 Q10: the section tree's paths → the paths before it, applied
 * only to a path **no route matches**.
 *
 * Shipped one release ahead of the regrouping, so that a revert of the
 * regrouping alone is still a pure revert: a bookmark taken on the new tree
 * then lands on its old page instead of on nothing. Once the new paths are
 * routes, nothing here ever applies; the table is inert, not wrong.
 */
export const REVERSE_REDIRECTS: Record<string, string> = {
  "/admin/registries": "/admin/observability/registries",
  "/admin/registries/upstream": "/admin/operations/upstream",
  "/admin/registries/warming": "/admin/operations/warming",
  "/admin/registries/imports": "/admin/operations/imports",
  "/admin/registries/air-gap": "/admin/operations/air-gap",
  "/admin/packages/sbom": "/admin/operations/sbom",
  "/admin/access": "/admin/security/access-check",
  "/admin/access/check": "/admin/security/access-check",
  "/admin/access/authorization": "/admin/security/authorization",
  "/admin/access/blocks": "/admin/security/blocks",
  "/admin/access/team-namespaces": "/admin/namespaces/team-namespaces",
  "/admin/access/beta-channel": "/admin/namespaces/beta-channel",
  "/admin/system": "/admin/observability/health",
  "/admin/system/configuration": "/admin/operations/config-reload",
  "/admin/system/banner": "/admin/operations/banner",
  "/admin/system/health": "/admin/observability/health",
  "/admin/system/audit-log": "/admin/observability/audit-log",
  "/admin/system/notifications": "/admin/notifications/subscriptions",
  "/admin/system/inbound-events": "/admin/notifications/inbound",
};

/** The new registry page's pattern, reversed; `to` takes an already-encoded segment. */
export const REVERSE_REGISTRY_HUB = {
  from: /^\/admin\/registry\/([^/]+)$/,
  to: (segment: string) => `/admin/observability/registries/${segment}`,
};

/** Section index paths, which land on their first tab. */
export const SECTION_INDEXES: Record<string, string> = {
  "/me": "/me/profile",
  "/tools": "/tools/access-check",
  "/admin": "/admin/dashboard",
  // `/admin/registries` is not here: it is a page, the list of registries.
  "/admin/packages": "/admin/packages/all",
  // RFC 0046 §15 Q8: Security is the scans and their findings now; an old
  // `/admin/security` bookmark opens Scans, and Blocks is under Access.
  "/admin/security": "/admin/security/scans",
  "/admin/access": "/admin/access/check",
  // The read-only page, not configuration: a section should open on what it
  // knows, not on the control that changes a running instance.
  "/admin/system": "/admin/system/health",
};
