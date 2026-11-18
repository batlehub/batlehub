import { i18n } from "@/i18n";

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB"] as const;

/**
 * The locale every formatter here uses: the app's, never the browser's.
 *
 * `toLocaleString()` with no argument takes the *browser* locale, which is a
 * different question from the one the reader answered. Someone who chose
 * Français in the settings popover on an `en-US` browser was reading
 * `Aug 20, 2026` and `1,234` on a page whose every other word was French —
 * and DESIGN.md's Data Face Rule asks for one shared formatter keyed off the
 * resolved locale, which is exactly what this is.
 *
 * Read per call rather than captured: the locale is a live preference, and a
 * value captured at module load would freeze whatever it was when the first
 * table rendered.
 */
function activeLocale(): string {
  return i18n.global.locale.value;
}

/** Formats a byte count using binary (1024) units, e.g. `1.5 MiB`. */
export function formatBytes(
  bytes: number | null | undefined,
  opts?: { fallback?: string },
): string {
  const fallback = opts?.fallback ?? "—";
  if (bytes === null || bytes === undefined || Number.isNaN(bytes)) return fallback;
  if (bytes < 1024) return `${bytes} B`;
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < BYTE_UNITS.length - 1) {
    value /= 1024;
    unitIndex++;
  }
  return `${value.toFixed(1)} ${BYTE_UNITS[unitIndex]}`;
}

/**
 * Formats an ISO date and time in the app's locale, e.g. `Jan 2, 2026, 3:04 PM`.
 *
 * Month spelled, seconds dropped. The numeric default printed `1/2/2026`,
 * which reads as February in most of the world, beside date inputs that the
 * browser renders `dd/mm/yyyy`; a console read by distributed teams cannot
 * leave the day and the month to the reader's guess.
 */
export function formatDate(iso: string | null | undefined, opts?: { fallback?: string }): string {
  const fallback = opts?.fallback ?? "—";
  if (!iso) return fallback;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return fallback;
  return date.toLocaleString(activeLocale(), { dateStyle: "medium", timeStyle: "short" });
}

/**
 * Formats an ISO date string as a calendar day, e.g. `2 Jan 2026`.
 *
 * The day without the clock, for a column where the hour is noise — a publish
 * date, a block date. Separate from {@link formatDate} rather than a flag on it,
 * because the two are read in different places for different reasons.
 */
export function formatDay(iso: string | null | undefined, opts?: { fallback?: string }): string {
  const fallback = opts?.fallback ?? "—";
  if (!iso) return fallback;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return fallback;
  return date.toLocaleDateString(activeLocale(), {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/**
 * Formats an ISO date string as a short relative time, e.g. `5m ago`.
 *
 * The four words are translated rather than written here. They used to be
 * English literals in this file, which the i18n audit does not reach — it learnt
 * about component props and `ref` assignments, and a string returned from a
 * library function is neither. Same class of leak as the three labels
 * `PackageDetailPage` had, one directory over.
 */
export function formatRelative(
  iso: string | null | undefined,
  opts?: { fallback?: string },
): string {
  const fallback = opts?.fallback ?? i18n.global.t("common.never");
  if (!iso) return fallback;
  const time = new Date(iso).getTime();
  if (Number.isNaN(time)) return fallback;
  const diff = Date.now() - time;
  const minutes = Math.floor(diff / 60_000);
  if (minutes < 1) return i18n.global.t("common.justNow");
  if (minutes < 60) return i18n.global.t("common.minutesAgo", { n: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return i18n.global.t("common.hoursAgo", { n: hours });
  return i18n.global.t("common.daysAgo", { n: Math.floor(hours / 24) });
}

/** Formats a count using the app locale's thousands separators, e.g. `1,234`. */
export function formatCount(n: number | null | undefined, opts?: { fallback?: string }): string {
  const fallback = opts?.fallback ?? "—";
  if (n === null || n === undefined || Number.isNaN(n)) return fallback;
  return n.toLocaleString(activeLocale());
}
