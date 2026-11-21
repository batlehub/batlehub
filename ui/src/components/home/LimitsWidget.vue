<script setup lang="ts">
/**
 * "Am I near a limit" (RFC 0004 §4.2), for the person it concerns: how close
 * the caller is to each registry's rate limit (`GET /api/v1/me/limits`) and to
 * their publish quota (`GET /api/v1/me/quota`), one ring per registry.
 *
 * Downloads come first: everyone pulls, and many never publish — a block that
 * only spoke of publishing said nothing to most readers.
 *
 * Rendering rules the server has already applied, and which decide what this
 * draws:
 *
 *   - A registry with no limit is not in either response. Nothing to measure.
 *   - A group with no rows is not drawn, and with neither the widget does not
 *     render: an empty meter invents a limit where the operator set none.
 *   - The state is the server's verdict per counter or per dimension, never a
 *     threshold recomputed here: a second copy of the rule is a copy that can
 *     disagree with the 429.
 */
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { myLimits, myQuota } from "@/client/sdk.gen";
import type { MyQuotaDto, MyRateLimitDto } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { formatBytes, formatCount } from "@/lib/format";
import type { MeterState } from "@/components/ui/meter";
import { RingGauge } from "@/components/ui/gauge";
import HomeSection from "./HomeSection.vue";

const { t } = useI18n();
const { token } = useAuth();

const quota = useApi<MyQuotaDto[]>(
  () => myQuota() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const limits = useApi<MyRateLimitDto[]>(
  () => myLimits() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

type WireState = MyQuotaDto["state"];
const meterState = (s: WireState | null | undefined): MeterState =>
  s === "at_limit" ? "at-limit" : s === "warning" ? "warning" : "ok";
const RANK: Record<MeterState, number> = { "at-limit": 2, warning: 1, ok: 0 };
const pct = (used: number, limit: number) => (limit > 0 ? Math.round((used / limit) * 100) : 0);

/** `/min`, `/h`… — the window is per registry, so every row says its own. */
function perWindow(secs: number): string {
  if (secs === 1) return t("limitsWidget.perSecond");
  if (secs === 60) return t("limitsWidget.perMinute");
  if (secs === 3600) return t("limitsWidget.perHour");
  if (secs === 86400) return t("limitsWidget.perDay");
  return t("limitsWidget.perSeconds", { n: secs });
}

function resetsIn(iso: string): string {
  const secs = Math.max(0, Math.round((Date.parse(iso) - Date.now()) / 1000));
  if (secs < 60) return t("limitsWidget.inSeconds", { n: secs });
  if (secs < 3600) return t("limitsWidget.inMinutes", { n: Math.round(secs / 60) });
  return t("limitsWidget.inHours", { n: Math.round(secs / 3600) });
}

interface Gauge {
  key: string;
  label: string;
  value: number;
  max: number;
  center: string;
  sub: string;
  valueText: string;
  state: MeterState;
  advice: string | null;
}

const downloads = computed<Gauge[]>(() =>
  (limits.data.value ?? [])
    .map((r): Gauge => {
      const state = meterState(r.state);
      const window = perWindow(r.window_secs);
      const label = r.group ? `${r.registry} · ${r.group}` : r.registry;
      return {
        key: `${r.registry}/${r.group ?? ""}`,
        label,
        value: r.used,
        max: r.limit,
        center: `${pct(r.used, r.limit)}%`,
        sub: `${formatCount(r.used)}/${formatCount(r.limit)} ${window}`,
        valueText: t("limitsWidget.requestsText", {
          used: formatCount(r.used),
          limit: formatCount(r.limit),
          window,
        }),
        state,
        advice:
          state === "ok"
            ? null
            : t(
                state === "at-limit"
                  ? "limitsWidget.downloadsAtLimit"
                  : "limitsWidget.downloadsNear",
                {
                  used: formatCount(r.used),
                  limit: formatCount(r.limit),
                  window,
                  resets: resetsIn(r.resets_at),
                },
              ),
      };
    })
    .sort((a, b) => RANK[b.state] - RANK[a.state] || pct(b.value, b.max) - pct(a.value, a.max)),
);

/**
 * One ring per registry, for whichever dimension is closer to its limit —
 * versions or storage — with that dimension's own state, and its name under
 * the ring so a storage ring is never read as a version count.
 */
const publishing = computed<Gauge[]>(() =>
  (quota.data.value ?? [])
    .flatMap((r): Gauge[] => {
      const dims = [
        r.packages_limit != null && {
          kind: "versions" as const,
          used: r.packages_used,
          limit: r.packages_limit,
          state: meterState(r.packages_state),
        },
        r.bytes_limit != null && {
          kind: "storage" as const,
          used: r.bytes_used,
          limit: r.bytes_limit,
          state: meterState(r.bytes_state),
        },
      ].filter((d) => d !== false);
      const d = [...dims].sort(
        (a, b) => RANK[b.state] - RANK[a.state] || pct(b.used, b.limit) - pct(a.used, a.limit),
      )[0];
      // The server leaves out a registry with neither limit; nothing to draw if one arrives.
      if (!d) return [];
      const of =
        d.kind === "versions"
          ? `${formatCount(d.used)}/${formatCount(d.limit)}`
          : t("limitsWidget.ofLimit", { used: formatBytes(d.used), limit: formatBytes(d.limit) });
      return [
        {
          key: r.registry,
          label: r.registry,
          value: d.used,
          max: d.limit,
          center: d.kind === "versions" ? of : `${pct(d.used, d.limit)}%`,
          sub: t(d.kind === "versions" ? "limitsWidget.versions" : "limitsWidget.storage"),
          valueText: `${of} ${t(d.kind === "versions" ? "limitsWidget.versions" : "limitsWidget.storage")}`,
          state: d.state,
          advice:
            d.state === "ok"
              ? null
              : d.state === "at-limit"
                ? t("limitsWidget.publishAtLimit")
                : t("limitsWidget.publishNear", { pct: r.warn_threshold_pct }),
        },
      ];
    })
    .sort((a, b) => RANK[b.state] - RANK[a.state] || pct(b.value, b.max) - pct(a.value, a.max)),
);

const loading = computed(() => quota.loading.value || limits.loading.value);
/** Only when nothing could be read: one failed source leaves the other standing. */
const error = computed(() => (quota.error.value && limits.error.value ? quota.error.value : null));
const attention = computed(
  () => [...downloads.value, ...publishing.value].filter((g) => g.state !== "ok").length,
);
type Group = "downloads" | "publishing";
const GROUPS: Group[] = ["downloads", "publishing"];
const rowsOf = (g: Group) => (g === "downloads" ? downloads.value : publishing.value);
/** A source that failed while the other answered: said in its own group, not as a blank. */
const failedOf = (g: Group) => !!(g === "downloads" ? limits.error.value : quota.error.value);

/** Spelled out: a key assembled at runtime is one the catalogue check cannot see. */
const GROUP_TITLE: Record<Group, string> = {
  downloads: "limitsWidget.downloads",
  publishing: "limitsWidget.publishing",
};
const GROUP_WHAT: Record<Group, string> = {
  downloads: "limitsWidget.downloadsWhat",
  publishing: "limitsWidget.publishingWhat",
};

type Flagged = Exclude<MeterState, "ok">;
const advice = computed(() =>
  [...downloads.value, ...publishing.value].flatMap((g) =>
    g.state !== "ok" && g.advice
      ? [{ key: g.key, state: g.state, label: g.label, text: g.advice }]
      : [],
  ),
);

const WORD: Record<Flagged, string> = {
  warning: "limitsWidget.near",
  "at-limit": "limitsWidget.atLimit",
};
const GLYPH: Record<Flagged, string> = { warning: "▲", "at-limit": "■" };
const TONE: Record<Flagged, string> = {
  warning: "text-copper",
  "at-limit": "text-destructive",
};
</script>

<template>
  <HomeSection
    v-if="loading || GROUPS.some((g) => rowsOf(g).length || failedOf(g))"
    heading-id="limits-widget-heading"
    :title="t('limitsWidget.title')"
    :loading="loading"
    :error="error"
  >
    <div class="space-y-4">
      <p class="text-sm text-foreground" data-testid="limits-verdict">
        {{ attention ? t("limitsWidget.needAttention", attention) : t("limitsWidget.allClear") }}
      </p>

      <template v-for="group in GROUPS" :key="group">
        <div
          v-if="rowsOf(group).length || failedOf(group)"
          class="space-y-2 border-b border-dashed border-rule-soft pb-4"
          :data-testid="`limits-${group}`"
        >
          <h3 class="text-sm font-semibold text-foreground">{{ t(GROUP_TITLE[group]) }}</h3>
          <p class="text-xs text-muted-foreground">{{ t(GROUP_WHAT[group]) }}</p>
          <div class="grid grid-cols-3 gap-x-2 gap-y-4 pt-1 sm:grid-cols-4">
            <RingGauge
              v-for="g in rowsOf(group)"
              :key="g.key"
              :value="g.value"
              :max="g.max"
              :label="g.label"
              :value-text="g.valueText"
              :center="g.center"
              :state="g.state"
            >
              <span class="font-mono text-xs text-muted-foreground">{{ g.sub }}</span>
              <span
                v-if="g.state !== 'ok'"
                class="font-mono text-xs uppercase tracking-wider"
                :class="TONE[g.state]"
              >
                <span aria-hidden="true">{{ GLYPH[g.state] }}</span> {{ t(WORD[g.state]) }}
              </span>
            </RingGauge>
          </div>
          <p
            v-if="failedOf(group)"
            class="text-xs text-muted-foreground"
            data-testid="limits-failed"
          >
            {{ t("asyncState.couldNotLoad") }}
          </p>
        </div>
      </template>

      <ul v-if="advice.length" class="space-y-2" data-testid="limits-advice">
        <li v-for="a in advice" :key="a.key" class="text-xs text-muted-foreground">
          <span class="font-mono uppercase tracking-wider" :class="TONE[a.state]">
            <span aria-hidden="true">{{ GLYPH[a.state] }}</span>
            {{ t(WORD[a.state]) }}
          </span>
          <span class="ml-1 font-mono text-foreground">{{ a.label }}</span>: {{ a.text }}
        </li>
      </ul>
    </div>
  </HomeSection>
</template>
