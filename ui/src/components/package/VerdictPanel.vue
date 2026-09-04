<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { ShieldAlert, ShieldCheck, ShieldQuestion, Clock, RefreshCw } from "@lucide/vue";

import { getVerdict, rescanVerdict } from "@/client/sdk.gen";
import type { VerdictResponse } from "@/client/types.gen";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { severityVariant } from "@/lib/badge-variants";
import { formatDay } from "@/lib/format";

/**
 * The supply-chain verdict of the selected version (RFC 0018 §4.2, phase 2):
 * what it is held, denied or warned for, when a hold lifts, which scanners
 * answered, and the findings for a reader with `findings:read`.
 *
 * Reads the verdict endpoint for **one** version — the selected one — rather
 * than one per row of the table: a page view must not be N requests, and the
 * badge a reader wants is on the version they are looking at. A `404` is
 * three things the server deliberately does not tell apart (never seen, no
 * security profile, no `quarantine:read`), and the panel says nothing for
 * any of them rather than guess: on a registry without a profile that is the
 * common case, and a banner reading "no verdict" on every package would be
 * noise.
 */
const props = defineProps<{
  registry: string;
  name: string;
  version: string | null;
  /** Whether the reader may queue a rescan (`gates:exempt` — admins). */
  canRescan: boolean;
}>();

const { t } = useI18n();

const verdict = ref<VerdictResponse | null>(null);
const loading = ref(false);
const rescanning = ref(false);
const rescanNote = ref<string | null>(null);

async function load() {
  verdict.value = null;
  rescanNote.value = null;
  if (!props.version) return;
  loading.value = true;
  try {
    const { data, error } = await getVerdict({
      path: { registry: props.registry, name: props.name, version: props.version },
    });
    verdict.value = error || !data ? null : data;
  } catch {
    verdict.value = null;
  } finally {
    loading.value = false;
  }
}

watch(() => [props.registry, props.name, props.version], load, { immediate: true });

const state = computed(() => verdict.value?.state ?? null);

/** The state's label — the four keys spelled out so the catalogue test sees them. */
const stateLabel = computed(() => {
  switch (state.value) {
    case "allowed":
      return t("verdictPanel.state.allowed");
    case "warned":
      return t("verdictPanel.state.warned");
    case "quarantined":
      return t("verdictPanel.state.quarantined");
    case "denied":
      return t("verdictPanel.state.denied");
    default:
      return "";
  }
});

const stateVariant = computed(() => {
  switch (state.value) {
    case "denied":
      return "destructive";
    case "quarantined":
      return "copper";
    case "warned":
      return "outline";
    default:
      return "secondary";
  }
});

const icon = computed(() => {
  switch (state.value) {
    case "allowed":
      return ShieldCheck;
    case "warned":
    case "denied":
      return ShieldAlert;
    case "quarantined":
      return Clock;
    default:
      return ShieldQuestion;
  }
});

/** One sentence for the state, in the reader's language. */
const explanation = computed(() => {
  const v = verdict.value;
  if (!v) return "";
  switch (v.state) {
    case "allowed":
      return t("verdictPanel.allowed");
    case "warned":
      return t("verdictPanel.warned");
    case "denied":
      return t("verdictPanel.denied");
    case "quarantined":
      return v.available_at
        ? t("verdictPanel.heldUntil", { when: formatDay(v.available_at) })
        : t("verdictPanel.heldOpen");
    default:
      return "";
  }
});

async function rescan() {
  if (!props.version) return;
  rescanning.value = true;
  rescanNote.value = null;
  try {
    const { data, error } = await rescanVerdict({
      path: { registry: props.registry, name: props.name, version: props.version },
    });
    if (error || !data) {
      rescanNote.value = t("verdictPanel.rescanFailed");
    } else {
      rescanNote.value = data.queued
        ? t("verdictPanel.rescanQueued")
        : t("verdictPanel.rescanAlreadyOpen");
    }
  } catch {
    rescanNote.value = t("verdictPanel.rescanFailed");
  } finally {
    rescanning.value = false;
  }
}
</script>

<template>
  <section
    v-if="verdict"
    class="rounded-md border border-rule-soft p-4 space-y-3"
    aria-labelledby="verdict-heading"
    data-testid="verdict-panel"
  >
    <div class="flex flex-wrap items-center gap-2">
      <component :is="icon" class="h-4 w-4 shrink-0" aria-hidden="true" />
      <h2 id="verdict-heading" class="font-mono text-sm text-foreground">
        {{ t("verdictPanel.title", { version: verdict.package.version }) }}
      </h2>
      <Badge :variant="stateVariant" class="text-xs" data-testid="verdict-state">
        {{ stateLabel }}
      </Badge>
      <Badge
        v-for="code in verdict.reason_codes ?? []"
        :key="code"
        variant="outline"
        class="font-mono text-xs"
      >
        {{ code }}
      </Badge>
    </div>

    <p class="text-sm text-muted-foreground">{{ explanation }}</p>

    <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs text-muted-foreground">
      <dt>{{ t("verdictPanel.policy") }}</dt>
      <dd class="font-mono">{{ verdict.policy_ref }}</dd>
      <dt>{{ t("verdictPanel.scanners") }}</dt>
      <dd class="font-mono">
        {{
          (verdict.scanners_done ?? []).length
            ? (verdict.scanners_done ?? []).join(", ")
            : t("verdictPanel.noScannerYet")
        }}
      </dd>
      <dt>{{ t("verdictPanel.lastScanned") }}</dt>
      <dd>
        {{ verdict.last_scanned_at ? formatDay(verdict.last_scanned_at) : t("verdictPanel.never") }}
      </dd>
    </dl>

    <!-- The findings, when the reader may see them. A withheld list is said
         so: "none" and "not for you" are different facts. -->
    <div v-if="verdict.findings_withheld" class="text-xs text-muted-foreground">
      {{ t("verdictPanel.findingsWithheld") }}
    </div>
    <ul
      v-else-if="(verdict.findings ?? []).length"
      class="space-y-1 text-sm"
      data-testid="verdict-findings"
    >
      <li
        v-for="(f, i) in verdict.findings ?? []"
        :key="i"
        class="flex flex-wrap items-baseline gap-2"
      >
        <Badge :variant="severityVariant(f.severity)" class="text-xs">{{ f.severity }}</Badge>
        <span class="font-mono text-xs text-muted-foreground">{{ f.scanner }}</span>
        <span class="font-mono text-xs">{{ f.code }}</span>
        <span>{{ f.summary }}</span>
        <span v-if="f.reference" class="font-mono text-xs text-muted-foreground">{{
          f.reference
        }}</span>
      </li>
    </ul>
    <p v-else class="text-xs text-muted-foreground">{{ t("verdictPanel.noFindings") }}</p>

    <div v-if="canRescan" class="flex items-center gap-3">
      <Button
        variant="outline"
        size="sm"
        :disabled="rescanning"
        data-testid="verdict-rescan"
        @click="rescan"
      >
        <RefreshCw class="mr-1 h-3.5 w-3.5" aria-hidden="true" />
        {{ t("verdictPanel.rescan") }}
      </Button>
      <span v-if="rescanNote" class="text-xs text-muted-foreground">{{ rescanNote }}</span>
    </div>
  </section>
</template>
