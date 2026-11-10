<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, onMounted } from "vue";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Announcer } from "@/components/ui/announcer";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { addressingOf } from "@/config/registryAddressing";
import { useRegistryOptions } from "@/composables/useRegistryOptions";

const { t } = useI18n();

interface WarmableRegistry {
  name: string;
  latest_n: number;
  concurrency: number;
}

interface WarmFailure {
  package: string;
  version?: string | null;
  error: string;
}

interface WarmResult {
  warmed: number;
  skipped: number;
  errors: number;
  failures: WarmFailure[];
}

const { authFetch } = useAuthFetch();

/**
 * The registry's declared type, which decides what its warm field even means
 * (RFC 0004-bis §6.1). This page rendered eleven identical cards, each with a
 * package field *and* a path field, and a JetBrains path placeholder on cargo
 * and npm — a placeholder suggesting an input the registry cannot accept.
 */
const { typeOf } = useRegistryOptions();
const addressing = (name: string) => addressingOf(typeOf(name));

/** Announced as well as rendered: a warm result is an async outcome (§2.6). */
const announcement = ref("");

const registries = ref<WarmableRegistry[]>([]);
const loading = ref(false);
const loadError = ref<string | null>(null);

// Per-registry form state
const packageInputs = ref<Record<string, string>>({});
const pathInputs = ref<Record<string, string>>({});
const warming = ref<Record<string, boolean>>({});
const results = ref<Record<string, WarmResult>>({});
const errors = ref<Record<string, string>>({});

async function loadStatus() {
  loading.value = true;
  loadError.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/warming`);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = (await res.json()) as { registries: WarmableRegistry[] };
    registries.value = body.registries;
  } catch (e) {
    loadError.value = extractMessage(e);
  } finally {
    loading.value = false;
  }
}

async function triggerWarm(name: string) {
  warming.value[name] = true;
  delete results.value[name];
  delete errors.value[name];

  // Only the field this registry type actually has. Sending both meant a path
  // typed against a package registry was silently accepted and warmed nothing.
  const byPath = addressing(name) === "path";
  const pkgRaw = byPath ? "" : (packageInputs.value[name] ?? "").trim();
  const pathRaw = byPath ? (pathInputs.value[name] ?? "").trim() : "";

  const packages = pkgRaw
    ? pkgRaw
        .split(/[\n,]+/)
        .map((s) => s.trim())
        .filter(Boolean)
    : [];
  const paths = pathRaw
    ? pathRaw
        .split(/[\n,]+/)
        .map((s) => s.trim())
        .filter(Boolean)
    : [];

  if (packages.length === 0 && paths.length === 0) {
    errors.value[name] = t("adminWarming.specifyPackageOrPath");
    warming.value[name] = false;
    return;
  }

  try {
    const res = await authFetch(
      `${API_BASE_URL}/api/v1/admin/registries/${encodeURIComponent(name)}/warm`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ packages, paths }),
      },
    );
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    const result = (await res.json()) as WarmResult;
    results.value[name] = result;
    announcement.value = t("adminWarming.warmOutcome", {
      registry: name,
      warmed: result.warmed,
      skipped: result.skipped,
      errors: result.errors,
    });
  } catch (e) {
    errors.value[name] = extractMessage(e);
  } finally {
    warming.value[name] = false;
  }
}

// ── Delete cached artifact ────────────────────────────────────────────────────

// The delete-cached-artifact card moved to `/admin/observability/health`
// (RFC 0004 Phase 5, *split*): it read nothing this page produces — not the
// warmable-registry list, not `latest_n`, not `concurrency` — and it is the
// opposite verb on a different object. Health already owned "what has this
// registry cached" and the sibling Clear Cache control.

onMounted(() => void loadStatus());
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <Announcer :message="announcement" />
    <PageHeader
      variant="display"
      :title="t('adminWarming.cacheWarming')"
      :description="t('adminWarming.registriesWithWarmingConfiguredTrigger')"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="loadStatus">
          {{ loading ? t("common.loading") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>

    <AsyncState
      :loading="loading && registries.length === 0"
      :error="loadError"
      :empty="registries.length === 0"
    >
      <template #empty>
        <p class="text-sm text-muted-foreground">
          <i18n-t keypath="adminWarming.noWarmingConfigured" tag="span">
            <template #packages><code>warm_packages</code></template>
            <template #paths><code>warm_paths</code></template>
          </i18n-t>
        </p>
      </template>

      <!--
        One table, not eleven cards (RFC 0004-bis §6.1).

        Eleven cards repeated the same two help sentences twenty-two times and
        made "which registries can I warm" a scroll rather than a glance. The
        field is chosen by `registry_type`, which is data the page can already
        join — so a path-addressed registry offers a path and a package-centric
        one offers packages, instead of both offering both and one of them
        showing a placeholder for an input it cannot take.
      -->
      <Card>
        <CardContent class="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("common.registry") }}</TableHead>
                <TableHead class="w-[45%]">{{ t("adminWarming.whatToWarm") }}</TableHead>
                <TableHead class="text-right">{{ t("common.actions") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="reg in registries" :key="reg.name" class="align-top">
                <TableCell class="space-y-1">
                  <p class="font-mono text-sm">{{ reg.name }}</p>
                  <p class="text-xs text-muted-foreground">
                    {{
                      t("adminWarming.registryFacts", {
                        latest: reg.latest_n,
                        concurrency: reg.concurrency,
                      })
                    }}
                  </p>
                </TableCell>
                <TableCell class="space-y-1.5">
                  <Input
                    v-if="addressing(reg.name) === 'package'"
                    :id="`warm-${reg.name}`"
                    v-model="packageInputs[reg.name]"
                    :aria-label="t('common.packages')"
                    :placeholder="t('adminWarming.lodashReact180')"
                    class="font-mono text-xs"
                  />
                  <Input
                    v-else
                    :id="`warm-${reg.name}`"
                    v-model="pathInputs[reg.name]"
                    :aria-label="t('common.paths')"
                    placeholder="idea/idea-2026.1.3.tar.gz"
                    class="font-mono text-xs"
                  />
                  <p v-if="errors[reg.name]" class="text-xs text-destructive">
                    {{ errors[reg.name] }}
                  </p>

                  <div v-if="results[reg.name]" class="space-y-1.5">
                    <div class="flex gap-2 flex-wrap">
                      <Badge class="text-xs">{{
                        t("adminWarming.warmedCount", { count: results[reg.name].warmed })
                      }}</Badge>
                      <Badge class="bg-muted text-muted-foreground text-xs">{{
                        t("adminWarming.skippedCount", { count: results[reg.name].skipped })
                      }}</Badge>
                      <Badge
                        :class="
                          results[reg.name].errors > 0
                            ? 'text-destructive'
                            : 'bg-muted text-muted-foreground'
                        "
                        class="text-xs"
                      >
                        {{ t("adminWarming.errorCount", { count: results[reg.name].errors }) }}
                      </Badge>
                    </div>
                    <!--
                      Which ones failed, not just how many (A3). The count alone
                      left an operator warming eleven registries with "3 errors"
                      and no way to learn which three without shell access to
                      the instance they are administering through this console.
                    -->
                    <ul
                      v-if="results[reg.name].failures?.length"
                      class="space-y-1 border-l-2 border-destructive/40 pl-2"
                    >
                      <li
                        v-for="f in results[reg.name].failures"
                        :key="`${f.package}@${f.version ?? ''}`"
                        class="text-xs text-muted-foreground"
                      >
                        <span class="font-mono text-destructive"
                          >{{ f.package
                          }}<template v-if="f.version">@{{ f.version }}</template></span
                        >
                        — {{ f.error }}
                      </li>
                    </ul>
                    <!--
                      A count with nothing to name it is a panicked task, which
                      the report counts and cannot identify. Saying so beats a
                      silent mismatch between the badge and the list.
                    -->
                    <p
                      v-else-if="results[reg.name].errors > 0"
                      class="text-xs text-muted-foreground"
                    >
                      {{ t("adminWarming.failuresUnnamed") }}
                    </p>
                  </div>
                </TableCell>
                <TableCell class="text-right">
                  <Button
                    variant="outline"
                    size="sm"
                    :disabled="warming[reg.name]"
                    @click="triggerWarm(reg.name)"
                  >
                    {{ warming[reg.name] ? t("adminWarming.warming") : t("adminWarming.warmNow") }}
                  </Button>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <!-- Written once, not twenty-two times. -->
      <p class="text-xs text-muted-foreground">
        {{ t("adminWarming.commaSeparatedOmitVersion") }}
        {{ t("adminWarming.commaSeparatedForPath") }}
      </p>
    </AsyncState>
  </div>
</template>
