<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { X } from "@lucide/vue";

import { listVerdicts } from "@/client/sdk.gen";
import type { Verdict } from "@/client/types.gen";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Select } from "@/components/ui/select";
import { Resolution } from "@/components/ui/resolution";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { severityVariant } from "@/lib/badge-variants";
import { SEVERITY_KEYS } from "@/config/severity";
import { formatDay } from "@/lib/format";
import { packagePath } from "@/config/navigation";
import { HOLD_LABEL_KEYS, HOLD_RESOLUTION } from "@/config/verdict";

/**
 * The *Findings* section of the security console (RFC 0042 §6.5): one
 * registry's held versions with what each was held for. Opened from a row of
 * the holds summary; drawn only for `findings:read` and `quarantine:read` at
 * the instance tier, the two verbs the listing checks.
 */
const props = defineProps<{ registry: string }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useI18n();

const STATES = ["denied", "quarantined"] as const;
const state = ref<string>("denied");
const stateOptions = STATES.map((s) => ({ value: s, label: t(HOLD_LABEL_KEYS[s]!) }));

const items = ref<Verdict[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);
const heading = ref<HTMLElement | null>(null);

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const {
      data,
      error: err,
      response,
    } = await listVerdicts({ query: { registry: props.registry, state: state.value } });
    if (err || !data) {
      error.value =
        response?.status === 404 ? t("findingsPanel.noProfile") : t("findingsPanel.loadFailed");
      items.value = [];
      return;
    }
    items.value = data.items;
  } catch {
    error.value = t("findingsPanel.loadFailed");
  } finally {
    loading.value = false;
  }
}

// Opening another registry moves the reader to the section it opened.
watch(
  () => props.registry,
  async () => {
    await load();
    await nextTick();
    heading.value?.focus();
  },
  { immediate: true },
);
watch(state, load);
</script>

<template>
  <Card data-testid="findings-panel">
    <CardHeader class="flex flex-row items-center justify-between gap-4">
      <CardTitle>
        <span ref="heading" tabindex="-1" class="outline-none">
          {{ t("findingsPanel.titleFor", { registry: props.registry }) }}
        </span>
      </CardTitle>
      <Button
        variant="ghost"
        size="sm"
        :aria-label="t('findingsPanel.close')"
        data-testid="findings-close"
        @click="emit('close')"
      >
        <X class="h-4 w-4" aria-hidden="true" />
      </Button>
    </CardHeader>
    <CardContent class="space-y-4">
      <Select
        v-model="state"
        class="w-48"
        :options="stateOptions"
        :aria-label="t('holdsPanel.state')"
      />
      <AsyncState
        :loading="loading"
        :error="error"
        :empty="!items.length"
        :empty-message="t('findingsPanel.empty')"
        :on-retry="load"
      >
        <Table :label="t('findingsPanel.titleFor', { registry: props.registry })">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.package") }}</TableHead>
              <TableHead>{{ t("holdsPanel.state") }}</TableHead>
              <TableHead>{{ t("findingsPanel.findings") }}</TableHead>
              <TableHead class="hidden sm:table-cell">{{ t("holdsPanel.evaluated") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="v in items"
              :key="`${v.package.name}@${v.package.version}`"
              data-testid="findings-row"
            >
              <TableCell class="font-mono text-sm align-top">
                <RouterLink :to="packagePath(props.registry, v.package.name)" class="underline">
                  {{ v.package.name }}@{{ v.package.version }}
                </RouterLink>
              </TableCell>
              <TableCell class="align-top">
                <Resolution
                  v-if="HOLD_RESOLUTION[v.state]"
                  :state="HOLD_RESOLUTION[v.state]!"
                  :label="t(HOLD_LABEL_KEYS[v.state]!)"
                />
              </TableCell>
              <TableCell class="align-top">
                <ul v-if="v.findings.length" class="space-y-1 text-sm">
                  <li
                    v-for="(f, i) in v.findings"
                    :key="i"
                    class="flex flex-wrap items-baseline gap-2"
                  >
                    <Badge :variant="severityVariant(f.judged_as ?? f.severity)" class="text-xs">
                      {{ t(SEVERITY_KEYS[f.judged_as ?? f.severity]) }}
                    </Badge>
                    <span class="font-mono text-xs">{{ f.code }}</span>
                    <span v-if="f.reference" class="font-mono text-xs text-muted-foreground">
                      {{ f.reference }}
                    </span>
                  </li>
                </ul>
                <span v-else class="font-mono text-xs text-muted-foreground">
                  {{ v.reason_codes.join(", ") }}
                </span>
              </TableCell>
              <TableCell class="hidden sm:table-cell text-xs align-top">
                {{ formatDay(v.evaluated_at) }}
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </AsyncState>
    </CardContent>
  </Card>
</template>
