/** Base URL of the backend API, e.g. "http://localhost:8080". Empty string means same origin. */
export const API_BASE_URL: string = import.meta.env.VITE_API_BASE_URL ?? "";

/** URL of the documentation site. */
export const DOCS_URL: string =
  import.meta.env.VITE_DOCS_URL ?? "https://batleforc.git.batleforc.fr/batlehub/";

/** Pre-filled "report a bug" issue link, using the repo's bug issue template. */
export const REPORT_BUG_URL = "https://github.com/batlehub/batlehub/issues/new?template=new-bug.md";

/** Pre-filled "report a security issue" link, using the repo's security issue template. */
export const REPORT_SECURITY_URL =
  "https://github.com/batlehub/batlehub/issues/new?template=security-issue.md";
