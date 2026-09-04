<script setup lang="ts">
import { ref, onMounted, watch } from "vue";
import { useI18n } from "vue-i18n";
import { GitBranch } from "@lucide/vue";

import { useAuthFetch } from "@/composables/useAuthFetch";
import { API_BASE_URL } from "@/config";
import { Badge } from "@/components/ui/badge";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { formatDate } from "@/lib/format";

/**
 * What this instance has resolved for a forge repository (RFC 0019 §6.5).
 *
 * A package registry's version list is the whole truth about it; a forge's is
 * not. `main` is a version that changes under the same name, and a tag can be
 * moved. This panel is the answer to "what did this instance actually serve",
 * so it lists what BatleHub *remembers* rather than what the forge currently
 * has — a branch nobody pulled through the proxy is not here, and that is the
 * honest answer rather than an omission.
 *
 * Absent on every non-forge registry: the endpoint answers `404` there and the
 * panel renders nothing, because "no moving refs" is not a fact about npm.
 */
const props = defineProps<{ registry: string; name: string }>();

type ForgeRef = {
  git_ref: string;
  ref_kind: string;
  sha: string;
  resolved_at: string;
  previous_sha?: string | null;
};

const { t } = useI18n();
const { authFetch } = useAuthFetch();

const refs = ref<ForgeRef[]>([]);
const remembered = ref(true);
const isForge = ref(false);
const loading = ref(false);

async function load() {
  refs.value = [];
  isForge.value = false;
  loading.value = true;
  try {
    const resp = await authFetch(
      `${API_BASE_URL}/api/v1/explore/${encodeURIComponent(props.registry)}/${encodeURIComponent(
        props.name,
      )}/refs`,
    );
    if (!resp.ok) return;
    const data = await resp.json();
    isForge.value = true;
    refs.value = data.refs ?? [];
    remembered.value = data.remembered ?? true;
  } catch {
    isForge.value = false;
  } finally {
    loading.value = false;
  }
}

/** The short SHA every row shows, as git itself abbreviates it. */
function short(sha: string): string {
  return sha.slice(0, 7);
}

onMounted(load);
watch(() => [props.registry, props.name], load);
</script>

<template>
  <Card v-if="isForge && !loading" data-testid="moving-refs-panel">
    <CardHeader>
      <CardTitle class="flex items-center gap-2">
        <GitBranch class="h-4 w-4 shrink-0" aria-hidden="true" />
        {{ t("movingRefs.title") }}
      </CardTitle>
    </CardHeader>
    <CardContent class="space-y-3">
      <p class="text-sm text-muted-foreground">{{ t("movingRefs.intro") }}</p>

      <p v-if="!remembered" class="text-sm text-muted-foreground" data-testid="moving-refs-none">
        {{ t("movingRefs.notRemembered") }}
      </p>
      <Table v-else-if="refs.length" data-testid="moving-refs-rows">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("movingRefs.ref") }}</TableHead>
            <TableHead>{{ t("movingRefs.kind") }}</TableHead>
            <TableHead>{{ t("movingRefs.commit") }}</TableHead>
            <TableHead>{{ t("movingRefs.resolved") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="r in refs" :key="r.git_ref">
            <TableCell class="font-mono text-sm">{{ r.git_ref }}</TableCell>
            <TableCell>
              <Badge :variant="r.ref_kind === 'branch' ? 'copper' : 'secondary'" class="text-xs">
                {{ r.ref_kind }}
              </Badge>
            </TableCell>
            <TableCell class="font-mono text-xs">
              {{ short(r.sha) }}
              <!-- A ref that resolves elsewhere than it did is the whole
                   point of the table: say what it was, not just what it is. -->
              <span v-if="r.previous_sha" class="text-muted-foreground">
                {{ t("movingRefs.wasAt", { sha: short(r.previous_sha) }) }}
              </span>
            </TableCell>
            <TableCell class="whitespace-nowrap text-xs tabular-nums">
              {{ formatDate(r.resolved_at) }}
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p v-else class="text-sm text-muted-foreground" data-testid="moving-refs-empty">
        {{ t("movingRefs.empty") }}
      </p>
    </CardContent>
  </Card>
</template>
