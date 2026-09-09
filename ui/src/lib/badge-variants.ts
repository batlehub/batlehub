export type BadgeVariant =
  | "default"
  | "secondary"
  | "destructive"
  | "outline"
  | "copper"
  /** An affirmative answer: full ink on a rule. See `Badge.vue`. */
  | "known";

/** Looks up a badge variant from a `key -> variant` map, falling back when the key is unmapped. */
export function variantFromMap<K extends string>(
  key: K,
  map: Partial<Record<K, BadgeVariant>>,
  fallback: BadgeVariant = "outline",
): BadgeVariant {
  return map[key] ?? fallback;
}

/** Registry-type badge coloring, shared by any page that lists registries by type. */
export const REGISTRY_TYPE_VARIANTS: Record<string, BadgeVariant> = {
  npm: "default",
  cargo: "secondary",
  github: "outline",
  forgejo: "outline",
  gitlab: "outline",
  openvsx: "secondary",
  goproxy: "outline",
  deb: "secondary",
  rpm: "secondary",
};

/** Explore-results "source" badge (local-only / upstream-only / both). */
export function sourceVariant(source: string): BadgeVariant {
  if (source === "local") return "secondary";
  if (source === "both") return "default";
  return "outline";
}

/** Firewall/gate status badge for a package version (blocked / yanked / clear). */
export function firewallVariant(status: string | undefined): BadgeVariant {
  if (!status) return "outline";
  if (status === "blocked") return "destructive";
  if (status === "yanked") return "secondary";
  return "outline";
}

/**
 * Vulnerability severity badge.
 *
 * `medium` used to be `default`, which resolves to the same `--accent` as
 * `destructive` — so critical, high and medium all rendered in one crimson and
 * the badge distinguished nothing but its own text. Copper is what DESIGN.md
 * extended to "a value going the wrong way without being a refusal yet", which
 * is exactly what a medium finding is.
 */
export function severityVariant(severity: string): BadgeVariant {
  switch (severity) {
    case "critical":
    case "high":
      return "destructive";
    case "medium":
      return "copper";
    default:
      return "secondary";
  }
}

/** Notification event-type badge (published / yanked / unyanked / other). */
export function eventBadgeVariant(eventType: string): BadgeVariant {
  if (eventType === "package_published") return "default";
  if (eventType === "package_yanked") return "destructive";
  if (eventType === "package_unyanked") return "secondary";
  // RFC 0014 §4.5: a confirmed disappearance and a voided sweep are the
  // two an operator acts on; a reappearance is the all-clear.
  if (eventType === "package_disappeared_upstream") return "destructive";
  if (eventType === "upstream_unreachable") return "copper";
  if (eventType === "package_reappeared_upstream") return "secondary";
  return "outline";
}
