/** Base URL of the backend API, e.g. "http://localhost:8080". Empty string means same origin. */
export const API_BASE_URL: string = import.meta.env.VITE_API_BASE_URL ?? "";

/**
 * URL of the documentation site, always with a trailing slash: a deep link is
 * `DOCS_URL + path`, and an override written without one would otherwise
 * glue the host to the path (`…fruse/cli`).
 */
export const DOCS_URL: string = (import.meta.env.VITE_DOCS_URL ?? "https://batlehub.dev/").replace(
  /\/?$/,
  "/",
);

/**
 * A page of the documentation site, in the reader's language when it has one:
 * the French tree mirrors the English one under `fr/`.
 */
export function docsUrl(path: string, locale?: string): string {
  const prefix = locale?.startsWith("fr") ? "fr/" : "";
  return `${DOCS_URL}${prefix}${path.replace(/^\//, "")}`;
}

/** The section of the refusals page that explains one reason code. */
export function reasonCodeDocsUrl(code: string, locale?: string): string {
  return docsUrl(`use/refused-downloads#${code.toLowerCase().replaceAll("_", "-")}`, locale);
}

/** Pre-filled "report a bug" issue link, using the repo's bug issue template. */
export const REPORT_BUG_URL = "https://github.com/batlehub/batlehub/issues/new?template=new-bug.md";

/**
 * "Report a security issue": GitHub's private vulnerability reporting form, so
 * a report is readable only by the maintainers until a fix ships (SECURITY.md,
 * RFC 0036 §6.5). Never an issue link — an issue is public the moment it is filed.
 */
export const REPORT_SECURITY_URL = "https://github.com/batlehub/batlehub/security/advisories/new";
