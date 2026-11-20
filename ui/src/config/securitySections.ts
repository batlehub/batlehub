/**
 * The `/security` space's sidebar (RFC 0050 §6.6), shaped like
 * `adminSections.ts` and read by the same consumers: `SecurityLayout`, the
 * router guard, the palette and the design gate's route list.
 *
 * Its own table rather than an admin section, so a delegated security group
 * reaches it without any admin verb (RFC 0042, decision 1).
 */
import { Activity, LayoutDashboard, ListChecks } from "@lucide/vue";
import { canOpen, type AdminSection, type PageVerb } from "./adminSections";

export const SECURITY_SECTIONS: AdminSection[] = [
  {
    to: "/security",
    label: "securityNav.overview",
    icon: LayoutDashboard,
    pages: [
      // §4.2: the overview opens on either verb; each block asks its own.
      {
        to: "/security",
        label: "securityNav.overview",
        verb: ["quarantine:read", "flags:read"],
      },
    ],
  },
  {
    // Not a route: the group's key. Its pages share no prefix.
    to: "/security/triage",
    label: "securityNav.triage",
    icon: ListChecks,
    pages: [
      { to: "/security/holds", label: "securityNav.holds", verb: "quarantine:read" },
      { to: "/security/findings", label: "securityNav.findings", verb: "findings:read" },
      { to: "/security/flags", label: "securityNav.flags", verb: "flags:read" },
      { to: "/security/exposure", label: "securityNav.exposure", verb: "audit:read" },
    ],
  },
  {
    to: "/security/follow-up",
    label: "securityNav.followUp",
    icon: Activity,
    pages: [
      { to: "/security/trends", label: "securityNav.trends", verb: "quarantine:read" },
      { to: "/security/coverage", label: "securityNav.coverage", verb: "quarantine:read" },
      { to: "/security/scanners", label: "securityNav.scanners", verb: "quarantine:read" },
    ],
  },
];

/** The verb of each `/security` page, as `ADMIN_PAGE_VERBS` is for `/admin`. */
export const SECURITY_PAGE_VERBS: Record<string, PageVerb> = Object.fromEntries(
  SECURITY_SECTIONS.flatMap((section) => section.pages.map((page) => [page.to, page.verb])),
);

/**
 * The first page the caller may open, in sidebar order — of `section` when
 * given (a refused page lands in its own group first, as the admin guard
 * does), else of the whole space (§4.2).
 */
export function firstSecurityPage(
  can: (verb: string) => boolean,
  section?: AdminSection,
): string | undefined {
  const pages = section ? section.pages : SECURITY_SECTIONS.flatMap((s) => s.pages);
  return pages.find((page) => canOpen(can, page.verb))?.to;
}

/** The group a `/security/…` path belongs to: the page that prefixes it longest. */
export function securitySectionOf(path: string): AdminSection | undefined {
  let best: { section: AdminSection; length: number } | undefined;
  for (const section of SECURITY_SECTIONS) {
    for (const page of section.pages) {
      const hit = path === page.to || path.startsWith(page.to + "/");
      if (hit && page.to.length > (best?.length ?? -1)) best = { section, length: page.to.length };
    }
  }
  return best?.section;
}
