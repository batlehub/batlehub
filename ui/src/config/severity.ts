import type { Severity } from "@/client/types.gen";

/**
 * The i18n key of each severity. Spelled out rather than built as
 * `t(\`severity.${value}\`)`: a template-literal key is invisible to the i18n
 * audit, which reads templates, so a missing translation would ship as the
 * literal text `severity.critical`.
 */
export const SEVERITY_KEYS: Record<Severity, string> = {
  unknown: "severity.unknown",
  low: "severity.low",
  medium: "severity.medium",
  high: "severity.high",
  critical: "severity.critical",
};
