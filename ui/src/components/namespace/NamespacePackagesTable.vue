<script setup lang="ts">
import { Resolution } from "@/components/ui/resolution";
import { useI18n } from "vue-i18n";
import { EmptyState } from "@/components/ui/empty-state";
import { AsyncState } from "@/components/ui/async-state";
import { computed, ref, watch, toRef } from "vue";
import { myNamespacePackages, setPackageVisibility } from "@/client/sdk.gen";
import type { Visibility, TeamNamespaceDto, NamespacePackageDto } from "@/client/types.gen";
import { VISIBILITY_OPTIONS } from "@/config/visibility";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { Pagination } from "@/components/ui/pagination";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { Select } from "@/components/ui/select";

const { t } = useI18n();

const props = defineProps<{ namespace: TeamNamespaceDto }>();

const { token } = useAuth();
const page = ref(0);
const PAGE_SIZE = 50;

const nsRef = toRef(props, "namespace");

const {
  data: pkgsData,
  error,
  loading,
  reload,
} = useApi<NamespacePackageDto[]>(
  () =>
    myNamespacePackages({
      path: {
        registry: props.namespace.registry,
        prefix: props.namespace.prefix,
      },
      query: { page: page.value, per_page: PAGE_SIZE },
    }) as Promise<{ data?: unknown; error?: unknown }>,
  [token, nsRef, page],
);

watch(nsRef, () => {
  page.value = 0;
});

// ── Inline visibility editing ─────────────────────────────────────────────────

const editing = ref<Record<string, Visibility>>({});
const saving = ref<Record<string, boolean>>({});
const saveError = ref<Record<string, string>>({});

// The short form is its own message rather than the long one cut at the dash:
// French does not put the dash where English does. Both keys live in
// `config/visibility.ts` so they are greppable and the reference gate sees them.
const visibilityOptions = computed(() =>
  VISIBILITY_OPTIONS.map((o) => ({ value: o.value, label: t(o.shortLabel) })),
);

function pkgKey(pkg: NamespacePackageDto) {
  return `${props.namespace.registry}|${pkg.name}|${pkg.version}`;
}

function startEdit(pkg: NamespacePackageDto) {
  editing.value = { ...editing.value, [pkgKey(pkg)]: pkg.visibility };
}

function cancelEdit(pkg: NamespacePackageDto) {
  const copy = { ...editing.value };
  delete copy[pkgKey(pkg)];
  editing.value = copy;
}

async function saveVis(pkg: NamespacePackageDto) {
  const k = pkgKey(pkg);
  const vis = editing.value[k];
  saving.value = { ...saving.value, [k]: true };
  saveError.value = { ...saveError.value, [k]: "" };
  try {
    const { error: apiErr } = await setPackageVisibility({
      path: { registry: props.namespace.registry, name: pkg.name },
      body: { visibility: vis },
    });
    if (apiErr) throw new Error((apiErr as { message?: string })?.message ?? t("common.apiError"));
    pkg.visibility = vis;
    cancelEdit(pkg);
  } catch (e) {
    saveError.value = {
      ...saveError.value,
      [k]: e instanceof Error ? e.message : t("common.unknownError"),
    };
  } finally {
    saving.value = { ...saving.value, [k]: false };
  }
}

function visVariant(v: Visibility): "default" | "secondary" | "outline" {
  if (v === "public") return "default";
  return v === "internal" ? "secondary" : "outline";
}

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString(undefined, { dateStyle: "medium" });
}
</script>

<template>
  <AsyncState :loading="loading" :error="error" :empty="!pkgsData?.length" :on-retry="reload">
    <template #empty>
      <EmptyState
        :title="t('namespace.noPackagesTitle')"
        :description="t('namespace.noPackagesBody')"
      />
    </template>
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>{{ t("common.package") }}</TableHead>
          <TableHead>{{ t("common.version") }}</TableHead>
          <TableHead>{{ t("common.visibility") }}</TableHead>
          <TableHead>{{ t("namespacePackagesTable.publishedBy") }}</TableHead>
          <TableHead>{{ t("common.date") }}</TableHead>
          <TableHead />
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRow
          v-for="pkg in pkgsData"
          :key="`${pkg.name}@${pkg.version}`"
          :class="pkg.yanked ? 'text-muted-foreground' : ''"
        >
          <TableCell class="font-mono text-xs">{{ pkg.name }}</TableCell>
          <TableCell class="font-mono text-xs">
            {{ pkg.version }}
            <!-- The state's own grammar (matrix + word), not a faded row and a
                 crimson word: half-opacity text failed AA, and crimson means
                 refused, which a yank is not. -->
            <Resolution
              v-if="pkg.yanked"
              class="ml-2"
              state="yanked"
              :label="t('namespacePackagesTable.yanked')"
            />
          </TableCell>
          <TableCell>
            <template v-if="editing[pkgKey(pkg)] !== undefined">
              <div class="flex items-center gap-1">
                <Select
                  v-model="editing[pkgKey(pkg)]"
                  :options="visibilityOptions"
                  :aria-label="t('namespacePackagesTable.packageVisibility')"
                  class="w-32 text-xs"
                />
                <Button
                  size="sm"
                  variant="default"
                  :disabled="saving[pkgKey(pkg)]"
                  class="text-xs h-7 px-2"
                  @click="saveVis(pkg)"
                >
                  {{ saving[pkgKey(pkg)] ? "…" : t("common.save") }}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  class="text-xs h-7 px-2"
                  @click="cancelEdit(pkg)"
                  >{{ t("common.cancel") }}</Button
                >
              </div>
              <p v-if="saveError[pkgKey(pkg)]" class="text-xs text-destructive mt-1">
                {{ saveError[pkgKey(pkg)] }}
              </p>
            </template>
            <Badge v-else :variant="visVariant(pkg.visibility)" class="capitalize text-xs">
              {{
                visibilityOptions.find((o) => o.value === pkg.visibility)?.label ?? pkg.visibility
              }}
            </Badge>
          </TableCell>
          <TableCell class="text-xs">{{ pkg.published_by }}</TableCell>
          <TableCell class="text-xs">{{ formatDate(pkg.published_at) }}</TableCell>
          <TableCell>
            <Button
              v-if="editing[pkgKey(pkg)] === undefined"
              size="sm"
              variant="ghost"
              class="text-xs h-7 px-2"
              @click="startEdit(pkg)"
              >{{ t("namespacePackagesTable.editVisibility") }}</Button
            >
          </TableCell>
        </TableRow>
      </TableBody>
    </Table>
    <Pagination
      class="mt-3"
      :page="page"
      :has-next="(pkgsData?.length ?? 0) >= PAGE_SIZE"
      @update:page="page = $event"
    />
  </AsyncState>
</template>
