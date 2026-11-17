<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { useAuth } from "@/composables/useAuth";
import { securityOverview } from "@/client/sdk.gen";
import type { DecisionItem, SecurityOverview } from "@/client/types.gen";
import { usePoll } from "@/composables/usePoll";
import { formatRelative } from "@/lib/format";
import { PageHeader } from "@/components/ui/page-header";
import { Button } from "@/components/ui/button";
import { AsyncState } from "@/components/ui/async-state";
import SecurityStatusStrip from "@/components/admin/SecurityStatusStrip.vue";
import CodeLabel from "@/components/admin/CodeLabel.vue";
import StateMark from "@/components/admin/StateMark.vue";
import { REASON_LABEL_KEYS } from "@/config/codeLabels";
import { packagePath } from "@/config/navigation";
import { docsUrl } from "@/config";

/**
 * The security space's landing page (RFC 0050 §4.2, §4.5): whether anything
 * needs attention, per domain, before any list; then the few items that need
 * a decision. Refreshes every 30 s while visible, stops after 10 minutes.
 */
const { t, locale } = useI18n();

const overview = ref<SecurityOverview | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

async function load(): Promise<void> {
  loading.value = !overview.value;
  try {
    const { data, error: err } = await securityOverview();
    if (err || !data) throw new Error(t("securityOverview.loadFailed"));
    overview.value = data;
    error.value = null;
  } catch (e) {
    error.value = (e as Error).message;
    throw e;
  } finally {
    loading.value = false;
  }
}

const { stopped, restart } = usePoll(load, ref(true), { interval: 30_000 });
onMounted(() => void load().catch(() => {}));

/**
 * No `[security]` profile: holds and scanners have nothing to say (the "empty"
 * mock of §4.5). A registry under `cve_gate` alone is still covered, so the
 * hint shows only while some registry is not.
 */
const unprotected = computed(() => overview.value?.coverage?.protected === 0);
const showUnprotectedHint = computed(
  () => unprotected.value && !!overview.value?.coverage?.unprotected.length,
);

const DECISION_TEXT: Record<string, string> = {
  hold_on_hard_block: "securityOverview.decision.holdOnHardBlock",
  pulled_after_flag: "securityOverview.decision.pulledAfterFlag",
  exemption_expiring: "securityOverview.decision.exemptionExpiring",
  failed_scan: "securityOverview.decision.failedScan",
};
const GLYPH: Record<string, string> = {
  hold_on_hard_block: "✖",
  pulled_after_flag: "✖",
  exemption_expiring: "⧗",
  failed_scan: "✖",
};
const { can } = useAuth();
// An exemption opens the authorization page only for whoever may read it; the
// delegated security group (quarantine:read alone) gets the package instead.
function decisionTo(d: DecisionItem) {
  if (d.kind === "failed_scan") return { path: "/security/scanners", query: { tab: "failures" } };
  if (d.kind === "exemption_expiring" && can("authz:read")) return "/admin/access/authorization";
  return packagePath(d.registry, d.name);
}
/**
 * The decision sources that did not answer (`holds`, `scanners`, `exemptions`,
 * `flags`): the list is then incomplete, and says so rather than reading short.
 */
const SOURCE_LABEL: Record<string, string> = {
  holds: "securityOverview.source.holds",
  scanners: "securityOverview.source.scanners",
  exemptions: "securityOverview.source.exemptions",
  flags: "securityOverview.source.flags",
};
const unreadSources = computed(() =>
  (overview.value?.decisions.unknown ?? []).map((s) => (SOURCE_LABEL[s] ? t(SOURCE_LABEL[s]!) : s)),
);
const decisionText = (d: DecisionItem) =>
  t(DECISION_TEXT[d.kind] ?? "securityOverview.decision.other", {
    n: d.pulls_after ?? 0,
    when: formatRelative(d.expires_at ?? d.at),
  });
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      variant="display"
      :title="t('securityNav.overview')"
      :description="t('securityOverview.description')"
    />

    <AsyncState :loading="loading" :error="overview ? null : error" :on-retry="() => restart()">
      <template v-if="overview">
        <section class="space-y-2" aria-labelledby="status-heading">
          <div class="flex flex-wrap items-baseline justify-between gap-2">
            <h2 id="status-heading" class="font-mono text-xs uppercase tracking-wider">
              {{ t("securityOverview.status") }}
            </h2>
            <p class="text-xs text-muted-foreground" data-testid="updated">
              <template v-if="stopped">
                {{ t("securityOverview.stopped", { when: formatRelative(overview.computed_at) }) }}
                <Button variant="outline" size="sm" class="ml-2" @click="restart">{{
                  t("securityOverview.refresh")
                }}</Button>
              </template>
              <template v-else>{{
                t("securityOverview.updated", { when: formatRelative(overview.computed_at) })
              }}</template>
            </p>
          </div>
          <SecurityStatusStrip
            :overview="overview"
            :domains="
              unprotected ? ['coverage', 'flags'] : ['holds', 'flags', 'scanners', 'coverage']
            "
            @retry="restart"
          />
          <p
            v-if="showUnprotectedHint"
            class="text-sm text-muted-foreground"
            data-testid="unprotected"
          >
            {{ t("securityOverview.unprotectedHint") }}
            <a
              :href="docsUrl('guide/security-gates', locale)"
              class="text-primary underline underline-offset-[3px]"
              >{{ t("securityOverview.howToProtect") }}</a
            >
          </p>
        </section>

        <section
          v-if="overview.decisions.total || unreadSources.length"
          class="space-y-2"
          aria-labelledby="decisions-heading"
        >
          <div class="flex items-baseline justify-between gap-2">
            <h2 id="decisions-heading" class="font-mono text-xs uppercase tracking-wider">
              {{ t("securityOverview.needsDecision") }}
            </h2>
            <span class="font-mono text-xs tabular-nums text-muted-foreground">{{
              t("securityOverview.shown", {
                n: overview.decisions.items.length,
                total: overview.decisions.total,
              })
            }}</span>
          </div>
          <p
            v-if="unreadSources.length"
            class="text-sm text-muted-foreground"
            data-testid="decisions-incomplete"
          >
            <StateMark state="unknown" />
            {{ t("securityOverview.decisionsIncomplete", { sources: unreadSources.join(", ") }) }}
          </p>
          <ul class="space-y-2" data-testid="decisions">
            <li
              v-for="d in overview.decisions.items"
              :key="`${d.kind}|${d.registry}|${d.name}@${d.version}`"
              class="flex gap-2 text-sm"
            >
              <span aria-hidden="true" class="font-mono">{{ GLYPH[d.kind] ?? "•" }}</span>
              <span class="space-y-1">
                <span class="flex flex-wrap items-baseline gap-x-2">
                  <RouterLink :to="decisionTo(d)" class="font-mono underline underline-offset-[3px]"
                    >{{ d.name }}@{{ d.version }}</RouterLink
                  >
                  <span class="font-mono text-xs text-muted-foreground">{{ d.registry }}</span>
                  <CodeLabel
                    v-if="d.reason_code"
                    :code="d.reason_code"
                    :labels="REASON_LABEL_KEYS"
                  />
                </span>
                <span class="block text-muted-foreground">{{ decisionText(d) }}</span>
              </span>
            </li>
          </ul>
          <RouterLink
            v-if="overview.holds?.soc_verdict"
            to="/security/holds?reason=SOC_VERDICT"
            class="text-sm text-primary underline underline-offset-[3px]"
            data-testid="all-soc-holds"
            >→
            {{ t("securityOverview.allSocHolds", { n: overview.holds.soc_verdict }) }}</RouterLink
          >
        </section>
      </template>
    </AsyncState>
  </div>
</template>
