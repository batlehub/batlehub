<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed, ref, watch } from "vue";

import { listExemptions } from "@/client/sdk.gen";
import type { ExemptionListEntry } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import { AsyncState } from "@/components/ui/async-state";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

/**
 * RFC 0015 §4.5: the gate exemptions of the registry the Explain form names,
 * re-read whenever that registry changes.
 */
const props = defineProps<{ registry: string }>();

const { t } = useI18n();

const exemptions = ref<ExemptionListEntry[]>([]);
const exemptionsError = ref<string | null>(null);
/** §4.8 asks the panel for this filter by name: *show me every exemption nobody else looked at.* */
const selfApprovedOnly = ref(false);

const shownExemptions = computed(() =>
  selfApprovedOnly.value ? exemptions.value.filter((e) => e.self_approved) : exemptions.value,
);

async function loadExemptions() {
  exemptionsError.value = null;
  if (!props.registry) {
    exemptions.value = [];
    return;
  }
  try {
    const res = await listExemptions({ path: { registry: props.registry } as never });
    if (res.error) {
      exemptionsError.value = extractMessage(res.error);
      return;
    }
    exemptions.value = res.data ?? [];
  } catch (e: unknown) {
    exemptionsError.value = e instanceof Error ? e.message : String(e);
  }
}

watch(() => props.registry, loadExemptions);
</script>

<template>
  <section class="space-y-3" data-testid="panel-exemptions">
    <div class="flex items-center justify-between">
      <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
        {{ t("adminAuthorization.exemptionsTitle") }}
      </h2>
      <div class="flex items-center gap-2">
        <Switch id="az-self-approved" v-model="selfApprovedOnly" />
        <Label for="az-self-approved">{{ t("adminAuthorization.selfApprovedOnly") }}</Label>
      </div>
    </div>
    <AsyncState
      :loading="false"
      :error="exemptionsError"
      :empty="!registry || !shownExemptions.length"
      :empty-message="
        registry
          ? t('adminAuthorization.exemptionsNone')
          : t('adminAuthorization.exemptionsNeedRegistry')
      "
      :on-retry="loadExemptions"
    >
      <Table :label="t('adminAuthorization.exemptionsTitle')">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("common.package") }}</TableHead>
            <TableHead>{{ t("common.version") }}</TableHead>
            <TableHead>{{ t("adminAuthorization.gate") }}</TableHead>
            <TableHead>{{ t("adminAuthorization.until") }}</TableHead>
            <TableHead>{{ t("adminAuthorization.reason") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="(e, i) in shownExemptions" :key="i">
            <TableCell class="font-mono text-xs">{{ e.package }}</TableCell>
            <TableCell class="font-mono text-xs">{{ e.version }}</TableCell>
            <TableCell class="font-mono text-xs">{{ e.gate }}</TableCell>
            <TableCell>
              {{ e.exempt_until }}
              <span v-if="e.expired" class="ml-1 text-xs text-muted-foreground">{{
                t("adminAuthorization.expired")
              }}</span>
            </TableCell>
            <TableCell>
              {{ e.reason }}
              <span v-if="e.self_approved" class="ml-2 text-xs text-copper">{{
                t("adminAuthorization.selfApproved")
              }}</span>
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </AsyncState>
  </section>
</template>
