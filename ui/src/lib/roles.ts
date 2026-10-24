/**
 * A role as the reader's language says it. `identity.role` is a wire value —
 * `"anonymous"`, `"user"`, `"admin"` — and was printed as one, in English, on a
 * French session, on the home page, the profile and the user menu.
 */
export function roleLabel(t: (key: string) => string, role: string | null | undefined): string {
  switch (role ?? "anonymous") {
    case "admin":
      return t("common.admin");
    case "user":
      return t("common.user");
    case "anonymous":
      return t("common.roleAnonymous");
    default:
      return role as string;
  }
}
