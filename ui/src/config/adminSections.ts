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

export const ADMIN_SIDEBAR = [
  { to: "/admin/dashboard", label: "adminNav.dashboard", icon: LayoutDashboard },
  { to: "/admin/packages", label: "adminNav.packages", icon: Package },
  { to: "/admin/security", label: "adminNav.securityAccess", icon: Shield },
  { to: "/admin/namespaces", label: "adminNav.namespacesChannels", icon: FolderKey },
  { to: "/admin/operations", label: "adminNav.operations", icon: RefreshCw },
  { to: "/admin/observability", label: "adminNav.observability", icon: HeartPulse },
  { to: "/admin/notifications", label: "adminNav.notifications", icon: Bell },
];

export const PACKAGES_TABS = [
  { to: "/admin/packages/all", label: "adminNav.allPackages" },
  { to: "/admin/packages/bulk", label: "adminNav.bulkBlock" },
];

/**
 * RFC 0004 Phase 5 (*merge*): three tabs became two. "Who is blocked, and why"
 * was split across an account page and an address page, and an operator arrives
 * with a symptom rather than a mechanism — so answering it meant visiting both,
 * with neither page mentioning the other.
 */
export const SECURITY_TABS = [
  { to: "/admin/security/blocks", label: "adminNav.blocks" },
  { to: "/admin/security/access-check", label: "adminNav.accessCheck" },
  // RFC 0015 §4.8. Beside the access checker rather than under its own section:
  // both answer "why was this refused?", and an operator arriving with that
  // question should not have to know which of two mechanisms produced it.
  { to: "/admin/security/authorization", label: "adminNav.authorization" },
];

export const NAMESPACES_TABS = [
  { to: "/admin/namespaces/team-namespaces", label: "adminNav.teamNamespaces" },
  { to: "/admin/namespaces/beta-channel", label: "adminNav.betaChannel" },
];

export const OPERATIONS_TABS = [
  { to: "/admin/operations/config-reload", label: "adminNav.configReload" },
  { to: "/admin/operations/warming", label: "adminNav.warming" },
  // RFC 0004 Phase 5: the SBOM export observes nothing — it has no server-state
  // read at all, and nothing on it changes when the instance changes. It is an
  // operation you perform, which is this section, not something you watch.
  { to: "/admin/operations/sbom", label: "adminNav.sbomExport" },
  // RFC 0008 §6.6: the air gap is an operation too — what came across it,
  // and what the next bundle needs.
  { to: "/admin/operations/air-gap", label: "adminNav.airGap" },
];

/**
 * RFC 0004 Phase 5 (*split*): `/admin/notifications` carried three nouns and
 * two questions behind a hand-rolled tab strip that had no `role`, no
 * `aria-selected`, no arrow-key navigation and no URL state — so an operator
 * could not send a colleague the inbound view.
 *
 * Two routes, not three. Channels stayed with subscriptions because the
 * subscription form's channel `datalist` is populated from the channel list
 * (R8, verified in the source); inbound events read nothing either produces.
 */
export const NOTIFICATIONS_TABS = [
  { to: "/admin/notifications/subscriptions", label: "adminNav.subscriptions" },
  { to: "/admin/notifications/inbound", label: "adminNav.inboundEvents" },
];

export const OBSERVABILITY_TABS = [
  { to: "/admin/observability/health", label: "adminNav.health" },
  { to: "/admin/observability/audit-log", label: "adminNav.auditLog" },
];
