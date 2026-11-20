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

type Target = {
  to: string | { path: string; query: Record<string, string> } | null;
  label: string;
};
/**
 * Each reason opens the page that acts on it, when the viewer may read that
 * page; otherwise the package, which every viewer of the item can open.
 */
function decisionTarget(d: DecisionItem): Target {
  if (d.kind === "failed_scan")
    return {
      to: { path: "/security/scanners", query: { tab: "failures" } },
      label: "securityOverview.act.failedScan",
    };
  if (d.kind === "hold_on_hard_block")
    return {
      to: { path: "/security/holds", query: { reason: "SOC_VERDICT", registry: d.registry } },
      label: "securityOverview.act.holdOnHardBlock",
    };
  if (d.kind === "pulled_after_flag")
    // Exposure names the consumers (personal data), so it stays behind
    // audit:read; without it, say who can answer rather than link elsewhere.
    return can("audit:read")
      ? {
          to: {
            path: "/security/exposure",
            query: { registry: d.registry, package: d.name, when: "after_flag" },
          },
          label: "securityOverview.act.pulledAfterFlag",
        }
      : { to: null, label: "securityOverview.act.pulledAfterFlagNoAudit" };
  if (d.kind === "exemption_expiring" && can("authz:read"))
    return { to: "/admin/access/authorization", label: "securityOverview.act.exemptionExpiring" };
  return { to: packagePath(d.registry, d.name), label: "securityOverview.act.package" };
}

/** One entry per version, its reasons in urgency order (the server keeps them together). */
const decisionGroups = computed(() => {
  const groups = new Map<
    string,
    { registry: string; name: string; version: string; items: DecisionItem[] }
  >();
  for (const d of overview.value?.decisions.items ?? []) {
    const key = `${d.registry}|${d.name}@${d.version}`;
    const g = groups.get(key) ?? {
      registry: d.registry,
      name: d.name,
      version: d.version,
      items: [],
    };
    g.items.push(d);
    groups.set(key, g);
  }
  return [...groups.entries()].map(([key, g]) => ({ key, ...g }));
});

/** "→ all N" per kind, where the list shows fewer than there are and a page lists them. */
const KIND_PAGE: Record<string, () => string | null> = {
  hold_on_hard_block: () => "/security/holds?reason=SOC_VERDICT",
  pulled_after_flag: () =>
    can("audit:read") ? "/security/exposure?when=after_flag" : "/security/flags",
  exemption_expiring: () => (can("authz:read") ? "/admin/access/authorization" : null),
  failed_scan: () => "/security/scanners?tab=failures",
};
const ALL_LABEL: Record<string, string> = {
  hold_on_hard_block: "securityOverview.all.hold_on_hard_block",
  pulled_after_flag: "securityOverview.all.pulled_after_flag",
  exemption_expiring: "securityOverview.all.exemption_expiring",
  failed_scan: "securityOverview.all.failed_scan",
};
const moreByKind = computed(() => {
  const d = overview.value?.decisions;
  if (!d) return [];
  return Object.entries(d.by_kind ?? {}).flatMap(([kind, n]) => {
    const shown = d.items.filter((i) => i.kind === kind).length;
    const to = KIND_PAGE[kind]?.() ?? null;
    const label = ALL_LABEL[kind];
    return n > shown && to && label ? [{ kind, n, to, label }] : [];
  });
});
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
          <ul class="space-y-3" data-testid="decisions">
            <li v-for="g in decisionGroups" :key="g.key" class="space-y-1 text-sm">
              <span class="flex flex-wrap items-baseline gap-x-2">
                <RouterLink
                  :to="packagePath(g.registry, g.name)"
                  class="font-mono underline underline-offset-[3px]"
                  >{{ g.name }}@{{ g.version }}</RouterLink
                >
                <span class="font-mono text-xs text-muted-foreground">{{ g.registry }}</span>
              </span>
              <ul class="space-y-1">
                <li v-for="d in g.items" :key="d.kind" class="flex gap-2" data-testid="decision">
                  <span aria-hidden="true" class="font-mono">{{ GLYPH[d.kind] ?? "•" }}</span>
                  <span class="flex min-w-0 flex-wrap items-baseline gap-x-2">
                    <span class="text-muted-foreground">{{ decisionText(d) }}</span>
                    <CodeLabel
                      v-if="d.reason_code"
                      :code="d.reason_code"
                      :labels="REASON_LABEL_KEYS"
                    />
                    <RouterLink
                      v-if="decisionTarget(d).to"
                      :to="decisionTarget(d).to!"
                      class="text-primary underline underline-offset-[3px]"
                      :data-testid="`act-${d.kind}`"
                      >→ {{ t(decisionTarget(d).label) }}</RouterLink
                    >
                    <span v-else class="text-muted-foreground" :data-testid="`act-${d.kind}`">{{
                      t(decisionTarget(d).label)
                    }}</span>
                  </span>
                </li>
              </ul>
            </li>
          </ul>
          <ul v-if="moreByKind.length" class="space-y-1">
            <li v-for="m in moreByKind" :key="m.kind">
              <RouterLink
                :to="m.to"
                class="text-sm text-primary underline underline-offset-[3px]"
                :data-testid="`all-${m.kind}`"
                >→ {{ t(m.label, { n: m.n }) }}</RouterLink
              >
            </li>
          </ul>
        </section>
      </template>
    </AsyncState>
  </div>
</template>
