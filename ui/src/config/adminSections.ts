/**
 * Admin navigation data: the sidebar of every `/admin/<section>` route.
 *
 * `label` holds an **i18n key**, not a string, matching `navigation.ts`. These
 * were English literals until the audit learned to look inside `<script>`:
 * every one of them rendered untranslated through the tab strips and
 * `AdminLayout`, and the template-only scan could not see them, so the gate
 * read zero while the whole admin navigation stayed in English.
 *
 * The sidebar lives here rather than inside `AdminLayout.vue` for the same
 * reason: `catalogues.test.ts` can only prove a label resolves if it can import
 * it, and an array inside `<script setup>` cannot be imported.
 */
import { FolderKey, LayoutDashboard, Package, Server, Settings, ShieldAlert } from "@lucide/vue";

/**
 * The verb a page asks for, or (RFC 0050's security overview) a list of which
 * any one opens it.
 */
export type PageVerb = string | readonly string[];

/** Whether `can` opens a page gated on `verb`. */
export function canOpen(can: (verb: string) => boolean, verb: PageVerb): boolean {
  return typeof verb === "string" ? can(verb) : verb.some((v) => can(v));
}

/** One console page: its path, its i18n label and the verb its own reads ask for. */
export interface AdminPage {
  to: string;
  label: string;
  verb: PageVerb;
}

export interface AdminSection {
  /** The section root, a `SECTION_INDEXES` key (or the page itself). */
  to: string;
  label: string;
  icon: typeof LayoutDashboard;
  /** Another path prefix that belongs to the section (one registry's page). */
  also?: string;
  pages: AdminPage[];
}

/**
 * RFC 0046 §5: the one table every admin consumer is derived from — the router
 * guard (`ADMIN_PAGE_VERBS`), the sidebar, the palette, and the design
 * gate's route list (checked by `adminSections.test.ts`). There used to be
 * five hand-kept copies, and RFC 0046 §2.2(6) is what drifting copies produce:
 * eight admin pages no design gate measured.
 *
 * A section whose one page is the section's own path is drawn as a plain link
 * (the dashboard); every other section is a heading over its pages.
 */
export const ADMIN_SECTIONS: AdminSection[] = [
  {
    to: "/admin/dashboard",
    label: "adminNav.dashboard",
    icon: LayoutDashboard,
    pages: [{ to: "/admin/dashboard", label: "adminNav.dashboard", verb: "stats:read" }],
  },
  /*
   * RFC 0046 M3: the sections are named by the trigger an operator arrives
   * with — a registry is broken, a package is wrong, a vulnerability, someone's
   * access, the instance itself — not by the kind of control.
   */
  {
    to: "/admin/registries",
    label: "adminNav.registriesSection",
    icon: Server,
    // One registry's own page is `/admin/registry/:name` (singular, so a
    // registry named `upstream` is never shadowed by a sibling page).
    also: "/admin/registry/",
    pages: [
      // RFC 0044 phase 5. The list reads /admin/stats; one registry's page (a
      // dynamic path) carries its own `meta.verb` in the router.
      { to: "/admin/registries", label: "adminNav.allRegistries", verb: "stats:read" },
      // RFC 0014 §4.6.
      { to: "/admin/registries/upstream", label: "adminNav.upstream", verb: "system:read" },
      { to: "/admin/registries/warming", label: "adminNav.warming", verb: "cache:warm" },
      // RFC 0021 §6.5: the publish-side twin of warming.
      { to: "/admin/registries/imports", label: "adminNav.releaseImports", verb: "cache:warm" },
      // RFC 0008 §6.6: what came across the air gap, and what the next bundle needs.
      { to: "/admin/registries/air-gap", label: "adminNav.airGap", verb: "system:read" },
    ],
  },
  {
    to: "/admin/packages",
    label: "adminNav.packages",
    icon: Package,
    pages: [
      { to: "/admin/packages/all", label: "adminNav.allPackages", verb: "packages:read" },
      { to: "/admin/packages/bulk", label: "adminNav.bulkBlock", verb: "packages:block" },
      // The SBOM export observes nothing about a registry: it is about packages.
      { to: "/admin/packages/sbom", label: "adminNav.sbomExport", verb: "packages:read" },
    ],
  },
  {
    to: "/admin/access",
    label: "adminNav.access",
    icon: FolderKey,
    pages: [
      // RFC 0050 §6.7: the access check is Authorization's *Explain* panel now
      // (one simulator, not two), and blocks are Instance security's.
      { to: "/admin/access/authorization", label: "adminNav.authorization", verb: "authz:read" },
      {
        to: "/admin/access/team-namespaces",
        label: "adminNav.teamNamespaces",
        verb: "owners:read",
      },
      { to: "/admin/access/beta-channel", label: "adminNav.betaChannel", verb: "owners:read" },
    ],
  },
  /*
   * RFC 0050 §4.2: the security of this server — who failed to sign in, who
   * is banned, tokens kept in clear — as opposed to the packages it serves,
   * which are the `/security` space's.
   */
  {
    to: "/admin/instance-security",
    label: "adminNav.instanceSecurity",
    icon: ShieldAlert,
    pages: [
      {
        to: "/admin/instance-security",
        label: "adminNav.instanceSecuritySummary",
        verb: "system:read",
      },
      { to: "/admin/instance-security/blocks", label: "adminNav.blocks", verb: "blocks:read" },
    ],
  },
  {
    to: "/admin/system",
    label: "adminNav.system",
    icon: Settings,
    pages: [
      { to: "/admin/system/configuration", label: "adminNav.configReload", verb: "config:read" },
      { to: "/admin/system/banner", label: "adminNav.globalBanner", verb: "config:write" },
      { to: "/admin/system/health", label: "adminNav.health", verb: "system:read" },
      { to: "/admin/system/audit-log", label: "adminNav.auditLog", verb: "audit:read" },
      /*
       * RFC 0004 Phase 5 (*split*): channels stayed with subscriptions because
       * the subscription form's channel `datalist` is populated from them.
       */
      {
        to: "/admin/system/notifications",
        label: "adminNav.subscriptions",
        verb: "system:read",
      },
      {
        to: "/admin/system/inbound-events",
        label: "adminNav.inboundEvents",
        verb: "system:read",
      },
    ],
  },
];

/**
 * The verb each admin page's own reads ask for (RFC 0042 §4.3), derived from
 * `ADMIN_SECTIONS`. The router guard reads it, so a page is never linked where
 * the router would refuse it — and never gated on the role.
 */
export const ADMIN_PAGE_VERBS: Record<string, string> = Object.fromEntries(
  // Every admin page asks one verb; a list-of-verbs page (the security
  // overview's shape) would be left out here and so refused — fail closed.
  ADMIN_SECTIONS.flatMap((section) =>
    section.pages.flatMap((page) => (typeof page.verb === "string" ? [[page.to, page.verb]] : [])),
  ),
);

/** The first page under `prefix` the caller may open, in table order. */
export function firstAdminPage(can: (verb: string) => boolean, prefix = "/admin/") {
  return Object.entries(ADMIN_PAGE_VERBS).find(
    ([path, verb]) => path.startsWith(prefix) && can(verb),
  )?.[0];
}
