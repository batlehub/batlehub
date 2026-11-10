<script setup lang="ts">
/**
 * "Am I near my quota" (RFC 0004 §4.2), answered on `/` for the person it
 * concerns.
 *
 * Before this, the only read paths were `/api/v1/admin/quota*` — all admin-only
 * — so a developer could not see their own usage until a publish failed with a
 * 429. This calls `/api/v1/me/quota`, which takes no `user_id` at all.
 *
 * Two rendering rules the server has already applied, restated here because
 * they decide what this component *does not* draw:
 *
 *   - Registries with no quota are not in the response. Nothing to measure.
 *   - If none of the caller's registries has a quota, the response is empty and
 *     the widget does not render. An empty meter is worse than no meter: it
 *     invents a limit where the operator configured none.
 */
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { myQuota } from "@/client/sdk.gen";
import type { MyQuotaDto } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { formatBytes, formatCount } from "@/lib/format";
import { Meter, type MeterState } from "@/components/ui/meter";
import HomeSection from "./HomeSection.vue";

const { t } = useI18n();
const { token } = useAuth();

const { data, error, loading } = useApi<MyQuotaDto[]>(
  () => myQuota() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const rows = computed<MyQuotaDto[]>(() => data.value ?? []);

type WireState = MyQuotaDto["state"];

/**
 * The server's verdict, not a threshold recomputed here. `warn_threshold_pct`
 * lives in config beside the limits that enforcement reads, and a second copy
 * of that rule in the browser is a copy that can disagree with the 429.
 *
 * Per *dimension*, not per row: a registry whose versions are at 82% while its
 * storage sits at 68% has crossed one threshold, not two, and colouring both
 * meters the same would say otherwise (RFC 0004 §4.2).
 */
function meterState(state: WireState | null | undefined): MeterState {
  if (state === "at_limit") return "at-limit";
  return state === "warning" ? "warning" : "ok";
}

/**
 * The caption is the state's *word*, which DESIGN.md requires alongside the
 * hue — state is never carried by hue alone. At the limit a publish is
 * genuinely refused (under `enforcement = "block"`); copper is a quota
 * approaching its limit, which is DESIGN.md's degradation job — moving the
 * wrong way, not yet refused. This widget and `AdminDashboard`'s trend both
 * assumed that job before the world had an entry for it; RFC 0004-bis §11/O1
 * added it to copper rather than growing the palette.
 *
 * It sits under the meter it describes, so which dimension it is about is
 * positional rather than something the sentence has to name.
 */
function caption(state: WireState | null | undefined, pct: number): string {
  if (state === "at_limit") return t("quotaWidget.atLimit");
  if (state === "warning") return t("quotaWidget.approaching", { pct });
  return "";
}

const bytesText = (row: MyQuotaDto) =>
  t("quotaWidget.ofLimit", {
    used: formatBytes(row.bytes_used),
    limit: formatBytes(row.bytes_limit ?? 0),
  });

const packagesText = (row: MyQuotaDto) =>
  t("quotaWidget.ofLimit", {
    used: formatCount(row.packages_used),
    limit: formatCount(row.packages_limit ?? 0),
  });

/** A row's limited dimensions, in display order — a dimension with no limit has no meter. */
function dimensions(row: MyQuotaDto) {
  return [
    row.bytes_limit != null && {
      key: "bytes",
      label: t("quotaWidget.storage"),
      used: row.bytes_used,
      max: row.bytes_limit,
      text: bytesText(row),
      state: row.bytes_state,
    },
    row.packages_limit != null && {
      key: "packages",
      label: t("quotaWidget.versions"),
      used: row.packages_used,
      max: row.packages_limit,
      text: packagesText(row),
      state: row.packages_state,
    },
  ].filter((d) => d !== false);
}
</script>

<template>
  <!-- No quota anywhere the caller can reach: render nothing at all. -->
  <HomeSection
    v-if="loading || error || rows.length"
    heading-id="quota-widget-heading"
    :title="t('quotaWidget.title')"
    :loading="loading"
    :error="error"
  >
    <ul class="space-y-4">
      <li v-for="row in rows" :key="row.registry" class="border border-border p-4">
        <p class="font-mono text-sm font-semibold text-foreground">{{ row.registry }}</p>

        <div class="mt-3 space-y-3">
          <div v-for="dim in dimensions(row)" :key="dim.key">
            <Meter
              :value="dim.used"
              :max="dim.max"
              :label="dim.label"
              :value-text="dim.text"
              :state="meterState(dim.state)"
            />
            <p
              v-if="caption(dim.state, row.warn_threshold_pct)"
              class="mt-1 text-xs"
              :class="dim.state === 'at_limit' ? 'text-destructive' : 'text-copper'"
              data-testid="quota-caption"
            >
              {{ caption(dim.state, row.warn_threshold_pct) }}
            </p>
          </div>
        </div>
      </li>
    </ul>
  </HomeSection>
</template>
