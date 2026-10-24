<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed, reactive, ref, watch } from "vue";
import { API_BASE_URL } from "@/config";
import { listRegistries } from "@/client/sdk.gen";
import { useApi } from "@/composables/useApi";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent } from "@/components/ui/card";
import { PageHeader } from "@/components/ui/page-header";
import RegistryPathForm from "@/components/registry-path-form/RegistryPathForm.vue";
import RegistryPathResults from "@/components/registry-path-form/RegistryPathResults.vue";
import { REGISTRY_PATH_TYPES } from "@/config/registryPathFields";

const { t } = useI18n();

const pastedUrl = ref("");
const registry = ref("github");

const { data: registries } = useApi<Array<{ name: string; type: string }>>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [],
);

// Registry name overrides (default to type id for backward compat).
const registryNameByType = reactive<Record<string, string>>(
  Object.fromEntries(REGISTRY_PATH_TYPES.map((t) => [t.id, t.id])),
);

const valuesByType = reactive<Record<string, Record<string, string>>>(
  Object.fromEntries(
    REGISTRY_PATH_TYPES.map((t) => [
      t.id,
      Object.fromEntries(t.fields.map((f) => [f.key, f.default ?? ""])),
    ]),
  ),
);

watch(registries, (regs) => {
  if (!regs) return;
  for (const t of REGISTRY_PATH_TYPES) {
    const found = regs.find((r) => r.type === t.id);
    if (found) registryNameByType[t.id] = found.name;
  }
});

const registriesByType = computed(() => {
  const map: Record<string, { name: string; type: string }[]> = {};
  for (const t of REGISTRY_PATH_TYPES) {
    map[t.id] = registries.value?.filter((r) => r.type === t.id) ?? [];
  }
  return map;
});

const groupedTypes = computed(() => {
  const order: string[] = [];
  const byGroup = new Map<string, typeof REGISTRY_PATH_TYPES>();
  for (const t of REGISTRY_PATH_TYPES) {
    if (!byGroup.has(t.group)) {
      byGroup.set(t.group, []);
      order.push(t.group);
    }
    byGroup.get(t.group)!.push(t);
  }
  return order.map((name) => ({ name, types: byGroup.get(name)! }));
});

const activeTypeDef = computed(
  () => REGISTRY_PATH_TYPES.find((t) => t.id === registry.value) ?? REGISTRY_PATH_TYPES[0],
);

const activePaths = computed(() => {
  const reg = registryNameByType[activeTypeDef.value.id]?.trim() || activeTypeDef.value.id;
  return activeTypeDef.value.buildPaths(reg, valuesByType[activeTypeDef.value.id] ?? {});
});

// ── URL parser ─────────────────────────────────────────────────────────────────

function parseUrl(raw: string): void {
  const str = raw.trim();
  if (!str) return;
  try {
    const u = new URL(str);
    const parts = u.pathname.split("/").filter(Boolean);
    for (const t of REGISTRY_PATH_TYPES) {
      for (const parser of t.urlParsers ?? []) {
        if (parser.matchesHost(u.hostname)) {
          registry.value = t.id;
          Object.assign(valuesByType[t.id], parser.parse(parts));
          return;
        }
      }
    }
  } catch {
    // not a valid URL — ignore silently
  }
}

watch(pastedUrl, parseUrl);
</script>

<template>
  <div class="max-w-2xl space-y-6">
    <PageHeader
      :title="t('pathMapper.urlMapper')"
      :description="t('pathMapper.pasteAnUpstreamUrlOr')"
      variant="display"
    />

    <!-- Universal paste input -->
    <Card>
      <CardContent class="pt-5">
        <Label for="paste-url" class="text-xs text-muted-foreground">{{
          t("pathMapper.pasteAnUpstreamUrl")
        }}</Label>
        <Input
          id="paste-url"
          v-model="pastedUrl"
          :placeholder="t('pathMapper.httpsPypiOrgProject')"
          class="mt-1.5 font-mono text-sm"
        />
      </CardContent>
    </Card>

    <!-- Registry selector -->
    <div class="space-y-1">
      <Label for="registry-select" class="text-xs uppercase tracking-wide text-muted-foreground">{{
        t("pathMapper.registryType")
      }}</Label>
      <select
        id="registry-select"
        v-model="registry"
        class="w-full rounded-sm border border-border bg-background px-3 py-2 font-mono text-sm"
      >
        <optgroup v-for="group in groupedTypes" :key="group.name" :label="group.name">
          <option v-for="type in group.types" :key="type.id" :value="type.id">
            {{ type.label }}
          </option>
        </optgroup>
      </select>
    </div>

    <RegistryPathForm
      v-model:registry-name="registryNameByType[activeTypeDef.id]"
      v-model:values="valuesByType[activeTypeDef.id]"
      :type-def="activeTypeDef"
      :registries="registriesByType[activeTypeDef.id] ?? []"
    />

    <RegistryPathResults :paths="activePaths" :base-url="API_BASE_URL" />
  </div>
</template>
