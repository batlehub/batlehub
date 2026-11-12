/**
 * The red paths of RFC 0046 §2.1 as palette actions: one entry per row, with
 * the words an operator under pressure types in either language.
 *
 * `to` is an admin page (a key of `ADMIN_PAGE_VERBS`, gated by its verb like
 * every other palette row) or, for an action that needs a package first, the
 * catalog — then `verb` names what the action needs and the palette hands the
 * query to the search (RP-6). `adminSections.test.ts` refuses a target the
 * router does not know (§4.3).
 */
export interface RedPath {
  id: string;
  /** An i18n key. */
  label: string;
  keywords: { en: string[]; fr: string[] };
  to: string;
  query?: Record<string, string>;
  /** Only for a non-admin target: the verb the action itself needs. */
  verb?: string;
}

export const RED_PATHS: RedPath[] = [
  {
    id: "RP-1",
    label: "palette.rp.accessCheck",
    keywords: {
      en: ["download", "denied", "refused", "forbidden", "why", "access"],
      fr: ["téléchargement", "refusé", "interdit", "pourquoi", "accès"],
    },
    to: "/admin/access/check",
  },
  {
    id: "RP-2",
    label: "palette.rp.upstream",
    keywords: {
      en: ["upstream", "degraded", "recheck", "down", "outage"],
      fr: ["amont", "dégradé", "revérifier", "panne"],
    },
    to: "/admin/registries/upstream",
  },
  {
    id: "RP-3",
    label: "palette.rp.config",
    keywords: {
      en: ["config", "configuration", "edit", "registry", "reload"],
      fr: ["config", "configuration", "modifier", "registre", "recharger"],
    },
    to: "/admin/system/configuration",
  },
  {
    id: "RP-4",
    label: "palette.rp.cve",
    keywords: {
      en: ["cve", "vulnerability", "advisory", "finding", "ghsa"],
      fr: ["cve", "vulnérabilité", "alerte", "constat", "ghsa"],
    },
    to: "/admin/security/findings",
  },
  {
    id: "RP-5",
    label: "palette.rp.rescan",
    keywords: {
      en: ["rescan", "scan", "failed", "failure", "retry"],
      fr: ["réanalyser", "analyse", "échec", "échoué", "relancer"],
    },
    to: "/admin/security/scans",
    query: { tab: "failures" },
  },
  {
    id: "RP-6",
    label: "palette.rp.blockVersion",
    keywords: {
      en: ["block", "version", "yank", "ban"],
      fr: ["bloquer", "version", "retirer", "interdire"],
    },
    to: "/packages",
    verb: "packages:block",
  },
  {
    id: "RP-7",
    label: "palette.rp.bulkBlock",
    keywords: {
      en: ["block", "bulk", "many", "versions", "mass"],
      fr: ["bloquer", "masse", "plusieurs", "versions"],
    },
    to: "/admin/packages/bulk",
  },
  {
    id: "RP-8",
    label: "palette.rp.blocks",
    keywords: {
      en: ["token", "leaked", "ip", "abuse", "block", "unblock", "user"],
      fr: ["jeton", "fuite", "ip", "abus", "bloquer", "débloquer", "utilisateur"],
    },
    to: "/admin/access/blocks",
  },
  {
    id: "RP-9",
    label: "palette.rp.stale",
    keywords: {
      en: ["stale", "explorer", "refresh", "cache", "outdated"],
      fr: ["obsolète", "explorateur", "rafraîchir", "cache", "périmé"],
    },
    to: "/admin/system/health",
  },
  {
    id: "RP-10",
    label: "palette.rp.audit",
    keywords: {
      en: ["who", "audit", "log", "history", "changed"],
      fr: ["qui", "audit", "journal", "historique", "modifié"],
    },
    to: "/admin/system/audit-log",
  },
  {
    id: "RP-11",
    label: "palette.rp.banner",
    keywords: {
      en: ["announce", "maintenance", "banner", "message", "outage"],
      fr: ["annoncer", "maintenance", "bannière", "bandeau", "message"],
    },
    to: "/admin/system/banner",
  },
  {
    id: "RP-12",
    label: "palette.rp.namespace",
    keywords: {
      en: ["team", "namespace", "owner", "scope"],
      fr: ["équipe", "namespace", "propriétaire", "portée"],
    },
    to: "/admin/access/team-namespaces",
  },
];
