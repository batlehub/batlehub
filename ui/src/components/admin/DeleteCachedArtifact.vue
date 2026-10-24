<script setup lang="ts">
/**
 * Delete one cached artifact (RFC 0004 Phase 5, *split* out of
 * `/admin/operations/warming`).
 *
 * It sat on the warming page and read nothing warming produced: it did not use
 * the warmable-registry list, did not use `latest_n` or `concurrency`, and had
 * its own free-text registry field. It is the opposite verb on a different
 * object, and its natural home already existed — the health page owns "what has
 * this registry cached" and already carries the sibling destructive control
 * (Clear Cache), so the two eviction controls are finally one control set on
 * one route instead of two sections apart.
 *
 * Two things fixed in the move:
 *
 *   - The registry is chosen, not typed. Asking an operator to type a registry
 *     name for a *destructive* action while a validated list of the same names
 *     is on the same page is the error-prevention defect the review ranked
 *     highest here.
 *   - It goes through the generated client. The warming page hand-rolled
 *     `authFetch` against `API_BASE_URL` and re-declared the response shape,
 *     which is RFC 0004 §1's own complaint reproduced on an endpoint the API
 *     does describe.
 */
import { useI18n } from "vue-i18n";
import { computed, ref } from "vue";

import { deleteCachedArtifact } from "@/client/sdk.gen";
import { extractMessage } from "@/composables/useApi";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

const { t } = useI18n();
const props = defineProps<{ registries: string[] }>();

type Mode = "package" | "path";

const registry = ref("");
const mode = ref<Mode>("package");
const name = ref("");
const version = ref("");
const path = ref("");

const busy = ref(false);
const error = ref<string | null>(null);
const result = ref<{ deleted: boolean; artifact_key: string } | null>(null);
const confirmOpen = ref(false);

const ready = computed(() =>
  Boolean(
    registry.value &&
    (mode.value === "path" ? path.value.trim() : name.value.trim() && version.value.trim()),
  ),
);

/** What the operator is about to remove, in their words, for the dialog. */
const target = computed(() =>
  mode.value === "path" ? path.value.trim() : `${name.value.trim()}@${version.value.trim()}`,
);

async function remove() {
  confirmOpen.value = false;
  busy.value = true;
  error.value = null;
  result.value = null;
  try {
    const body =
      mode.value === "path"
        ? { path: path.value.trim() }
        : { name: name.value.trim(), version: version.value.trim() };
    const res = await deleteCachedArtifact({
      path: { registry: registry.value },
      body: body as never,
    });
    if (res.error) {
      error.value = extractMessage(res.error);
      return;
    }
    const data = res.data as { deleted?: boolean; artifact_key?: string } | undefined;
    result.value = { deleted: data?.deleted ?? false, artifact_key: data?.artifact_key ?? "" };
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t("adminWarming.deleteCachedArtifact") }}</CardTitle>
      <CardDescription>{{ t("adminHealth.deleteArtifactHelp") }}</CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="grid gap-4 sm:grid-cols-2">
        <div class="space-y-1.5">
          <Label for="del-registry">{{ t("common.registry") }}</Label>
          <select
            id="del-registry"
            v-model="registry"
            class="flex h-9 w-full rounded-sm border border-input bg-transparent px-3 text-sm"
          >
            <option value="">{{ t("adminHealth.chooseRegistry") }}</option>
            <option v-for="r in props.registries" :key="r" :value="r">{{ r }}</option>
          </select>
        </div>
        <div class="space-y-1.5">
          <Label for="del-mode">{{ t("adminHealth.identifyBy") }}</Label>
          <select
            id="del-mode"
            v-model="mode"
            class="flex h-9 w-full rounded-sm border border-input bg-transparent px-3 text-sm"
          >
            <option value="package">{{ t("adminWarming.packageMode") }}</option>
            <option value="path">{{ t("adminWarming.pathMode") }}</option>
          </select>
        </div>
      </div>

      <div v-if="mode === 'package'" class="grid gap-4 sm:grid-cols-2">
        <div class="space-y-1.5">
          <Label for="del-name">{{ t("common.name") }}</Label>
          <Input id="del-name" v-model="name" class="font-mono" />
        </div>
        <div class="space-y-1.5">
          <Label for="del-version">{{ t("common.version") }}</Label>
          <Input id="del-version" v-model="version" class="font-mono" />
        </div>
      </div>
      <div v-else class="space-y-1.5">
        <Label for="del-path">{{ t("adminWarming.pathMode") }}</Label>
        <Input id="del-path" v-model="path" class="font-mono" />
      </div>

      <div class="flex items-center gap-3">
        <Button
          size="sm"
          variant="destructive"
          :disabled="!ready || busy"
          @click="confirmOpen = true"
        >
          {{ busy ? t("adminHealth.deleting") : t("common.delete") }}
        </Button>
        <p v-if="error" class="text-sm text-destructive">{{ error }}</p>
        <p v-else-if="result" class="text-sm text-muted-foreground">
          {{
            result.deleted
              ? t("adminHealth.artifactDeleted", { key: result.artifact_key })
              : t("adminHealth.artifactNotCached")
          }}
        </p>
      </div>
    </CardContent>
  </Card>

  <DestructiveConfirm
    :open="confirmOpen"
    :action="t('common.delete')"
    :count="1"
    :item-noun="t('adminHealth.cachedArtifact')"
    :scope="`${registry} · ${target}`"
    reversible
    :loading="busy"
    @confirm="remove"
    @update:open="confirmOpen = $event"
  />
</template>
