<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import type { ExploreVersionDto, FirewallDto } from "@/client/types.gen";
import { formatDay } from "@/lib/format";
import { severityVariant } from "@/lib/badge-variants";
import { Badge } from "@/components/ui/badge";
import { RuleNote } from "@/components/ui/rule-note";

/** What the selected version is refused, deprecated or vulnerable for, in text. */
defineProps<{
  registry: string;
  name: string;
  selectedRow: ExploreVersionDto;
}>();

const { t } = useI18n();

/**
 * The blocked variant of a firewall status, or `null`.
 *
 * In the script rather than in the template because this is where TypeScript can
 * narrow the union: `FirewallDto` is a discriminated union that *does* declare
 * `reason`, `blocked_by` and `blocked_at`, and the template read them through
 * three `as any` casts because a `v-if` on one element does not narrow a type
 * for a sibling. The casts meant the one part of this page that renders a
 * policy decision was the one part unchecked against the OpenAPI contract.
 */
function blockedInfo(fw: FirewallDto) {
  return fw.status === "blocked" ? fw : null;
}
</script>

<template>
  <!-- The rule, beside the thing it refused, in text.
       It was a `hidden group-hover:block` span: no focus trigger, no
       `aria-describedby`, no dismiss — WCAG 2.2 1.4.13 failed on all three
       counts, unreadable to a screen reader and unreachable on a phone.
       It is the answer the page exists to give, so it is simply on the
       page. The tie bar is the denial note's own, ported from the catalog
       rather than invented here. -->
  <RuleNote v-if="blockedInfo(selectedRow.firewall)" class="mt-4">
    <span class="text-primary">{{ t("common.reasonLabel") }}</span>
    {{ blockedInfo(selectedRow.firewall)!.reason }}
    <span class="px-2" aria-hidden="true">·</span>
    {{
      t("packageDetailPage.blockedByOn", {
        who: blockedInfo(selectedRow.firewall)!.blocked_by,
        when: formatDay(blockedInfo(selectedRow.firewall)!.blocked_at),
      })
    }}
    <RouterLink
      class="text-primary underline underline-offset-[3px]"
      :to="{
        path: '/tools/access-check',
        query: { registry, name, version: selectedRow.version },
      }"
      >{{ t("packageDetailPage.why") }}</RouterLink
    >
  </RuleNote>

  <!-- The deprecation message, which was a `title=` on a chip in the
       table — the upstream's own sentence about why not to use this
       version, available only to a mouse held still. -->
  <RuleNote v-if="selectedRow.deprecated" class="mt-4">
    <span class="text-copper">{{ t("packageDetailPage.deprecated") }}</span>
    {{ selectedRow.deprecation_message ?? t("packageDetailPage.deprecatedNoReason") }}
  </RuleNote>

  <!-- Vulnerabilities as text, for the same reason. A severity chip whose
       identifier and summary appear only under a stationary mouse is a
       supply-chain fact this page is not really reporting. -->
  <ul v-if="selectedRow.vulnerabilities.length" class="mt-4 space-y-2">
    <RuleNote v-for="vuln in selectedRow.vulnerabilities" :key="vuln.osv_id" as="li">
      <Badge :variant="severityVariant(vuln.severity)" class="mr-2 text-xs">{{
        vuln.severity
      }}</Badge>
      <span class="font-mono text-foreground">{{ vuln.osv_id }}</span>
      <span class="px-2" aria-hidden="true">·</span>
      {{ vuln.summary }}
      <template v-if="vuln.fixed_version">
        <span class="px-2" aria-hidden="true">·</span>
        {{ t("packageDetailPage.fixedIn") }}
        <span class="font-mono text-foreground">{{ vuln.fixed_version }}</span>
      </template>
    </RuleNote>
  </ul>
  <p v-else-if="!selectedRow.vulnerabilities_scanned" class="mt-4 text-sm text-muted-foreground">
    {{ t("packageDetailPage.notScannedHelp") }}
  </p>
</template>
