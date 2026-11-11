/**
 * Admin navigation data: the sidebar, and the tab strip for each
 * `/admin/<section>` route.
 *
 * `label` holds an **i18n key**, not a string, matching `navigation.ts`. These
 * were English literals until the audit learned to look inside `<script>`:
 * every one of them rendered untranslated through `SectionTabs` and
 * `AdminLayout`, and the template-only scan could not see them, so the gate
 * read zero while the whole admin navigation stayed in English.
 *
 * The sidebar lives here rather than inside `AdminLayout.vue` for the same
 * reason: `catalogues.test.ts` can only prove a label resolves if it can import
 * it, and an array inside `<script setup>` cannot be imported.
 */
import {
  Bell,
  FolderKey,
  HeartPulse,
  LayoutDashboard,
  Package,
  RefreshCw,
  Shield,
} from "@lucide/vue";

/** One admin page: its path, its i18n label and the verb its own reads ask for. */
export interface AdminPage {
  to: string;
  label: string;
  verb: string;
}

export interface AdminSection {
  /** The section root, a `SECTION_INDEXES` key (or the page itself). */
  to: string;
  label: string;
  icon: typeof LayoutDashboard;
  pages: AdminPage[];
}

/**
 * RFC 0046 §5: the one table every admin consumer is derived from — the router
 * guard (`ADMIN_PAGE_VERBS`), the sidebar, the tab strips, and the design
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
  {
    to: "/admin/packages",
    label: "adminNav.packages",
    icon: Package,
    pages: [
      { to: "/admin/packages/all", label: "adminNav.allPackages", verb: "packages:read" },
      { to: "/admin/packages/bulk", label: "adminNav.bulkBlock", verb: "packages:block" },
    ],
  },
  {
    to: "/admin/security",
    label: "adminNav.securityAccess",
    icon: Shield,
    /*
     * RFC 0004 Phase 5 (*merge*): three tabs became two. "Who is blocked, and
     * why" was split across an account page and an address page, and an
     * operator arrives with a symptom rather than a mechanism.
     */
    pages: [
      { to: "/admin/security/blocks", label: "adminNav.blocks", verb: "blocks:read" },
      { to: "/admin/security/access-check", label: "adminNav.accessCheck", verb: "authz:read" },
      // RFC 0015 §4.8. Beside the access checker: both answer "why was this
      // refused?", whichever of two mechanisms produced it.
      {
        to: "/admin/security/authorization",
        label: "adminNav.authorization",
        verb: "authz:read",
      },
      // RFC 0044 phase 2: GET /admin/scans is instance-tier quarantine:read.
      { to: "/admin/security/scans", label: "adminNav.scans", verb: "quarantine:read" },
      // RFC 0044 phase 3. shortcut: the server also answers findings:read held
      // on one registry only; the console checks the instance tier, like the
      // security console's panel.
      { to: "/admin/security/findings", label: "adminNav.findings", verb: "findings:read" },
    ],
  },
  {
    to: "/admin/namespaces",
    label: "adminNav.namespacesChannels",
    icon: FolderKey,
    pages: [
      {
        to: "/admin/namespaces/team-namespaces",
        label: "adminNav.teamNamespaces",
        verb: "owners:read",
      },
      { to: "/admin/namespaces/beta-channel", label: "adminNav.betaChannel", verb: "owners:read" },
    ],
  },
  {
    to: "/admin/operations",
    label: "adminNav.operations",
    icon: RefreshCw,
    pages: [
      // RFC 0014 §4.6. First, because it only reads: the section's index lands
      // on it rather than on config reload.
      { to: "/admin/operations/upstream", label: "adminNav.upstream", verb: "system:read" },
      {
        to: "/admin/operations/config-reload",
        label: "adminNav.configReload",
        verb: "config:read",
      },
      // Was a card at the bottom of config reload: a message to every visitor
      // filed under "change the running config".
      { to: "/admin/operations/banner", label: "adminNav.globalBanner", verb: "config:write" },
      { to: "/admin/operations/warming", label: "adminNav.warming", verb: "cache:warm" },
      // RFC 0021 §6.5: the publish-side twin of warming.
      { to: "/admin/operations/imports", label: "adminNav.releaseImports", verb: "cache:warm" },
      // RFC 0004 Phase 5: the SBOM export observes nothing; it is an operation.
      { to: "/admin/operations/sbom", label: "adminNav.sbomExport", verb: "packages:read" },
      // RFC 0008 §6.6: what came across the air gap, and what the next bundle needs.
      { to: "/admin/operations/air-gap", label: "adminNav.airGap", verb: "system:read" },
    ],
  },
  {
    to: "/admin/observability",
    label: "adminNav.observability",
    icon: HeartPulse,
    pages: [
      { to: "/admin/observability/health", label: "adminNav.health", verb: "system:read" },
      // RFC 0044 phase 5. The list reads /admin/stats; one registry's page (a
      // dynamic path) carries its own `meta.verb` in the router.
      {
        to: "/admin/observability/registries",
        label: "adminNav.registries",
        verb: "stats:read",
      },
      { to: "/admin/observability/audit-log", label: "adminNav.auditLog", verb: "audit:read" },
    ],
  },
  {
    to: "/admin/notifications",
    label: "adminNav.notifications",
    icon: Bell,
    /*
     * RFC 0004 Phase 5 (*split*): two routes, not three. Channels stayed with
     * subscriptions because the subscription form's channel `datalist` is
     * populated from the channel list.
     */
    pages: [
      {
        to: "/admin/notifications/subscriptions",
        label: "adminNav.subscriptions",
        verb: "system:read",
      },
      { to: "/admin/notifications/inbound", label: "adminNav.inboundEvents", verb: "system:read" },
    ],
  },
];

/**
 * The verb each admin page's own reads ask for (RFC 0042 §4.3), derived from
 * `ADMIN_SECTIONS`. The router guard reads it, so a page is never linked where
 * the router would refuse it — and never gated on the role.
 */
export const ADMIN_PAGE_VERBS: Record<string, string> = Object.fromEntries(
  ADMIN_SECTIONS.flatMap((section) => section.pages.map((page) => [page.to, page.verb])),
);

/** The first page under `prefix` the caller may open, in table order. */
export function firstAdminPage(can: (verb: string) => boolean, prefix = "/admin/") {
  return Object.entries(ADMIN_PAGE_VERBS).find(
    ([path, verb]) => path.startsWith(prefix) && can(verb),
  )?.[0];
}

export const ADMIN_SIDEBAR = ADMIN_SECTIONS.map(({ to, label, icon }) => ({ to, label, icon }));

function tabsOf(root: string) {
  const section = ADMIN_SECTIONS.find((s) => s.to === root);
  return (section?.pages ?? []).map(({ to, label }) => ({ to, label }));
}

export const PACKAGES_TABS = tabsOf("/admin/packages");
export const SECURITY_TABS = tabsOf("/admin/security");
export const NAMESPACES_TABS = tabsOf("/admin/namespaces");
export const OPERATIONS_TABS = tabsOf("/admin/operations");
export const NOTIFICATIONS_TABS = tabsOf("/admin/notifications");
export const OBSERVABILITY_TABS = tabsOf("/admin/observability");
