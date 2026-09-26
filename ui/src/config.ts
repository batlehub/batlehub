/** Base URL of the backend API, e.g. "http://localhost:8080". Empty string means same origin. */
export const API_BASE_URL: string = import.meta.env.VITE_API_BASE_URL ?? "";

/** URL of the documentation site. */
export const DOCS_URL: string =
  import.meta.env.VITE_DOCS_URL ?? "https://batleforc.git.batleforc.fr/batlehub/";

/** Pre-filled "report a bug" issue link, using the repo's bug issue template. */
export const REPORT_BUG_URL = "https://github.com/batlehub/batlehub/issues/new?template=new-bug.md";

/**
 * "Report a security issue": GitHub's private vulnerability reporting form, so
 * a report is readable only by the maintainers until a fix ships (SECURITY.md,
 * RFC 0036 §6.5). Never an issue link — an issue is public the moment it is filed.
 */
export const REPORT_SECURITY_URL = "https://github.com/batlehub/batlehub/security/advisories/new";
