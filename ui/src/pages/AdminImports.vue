<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, onMounted, computed } from "vue";
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

const { t } = useI18n();

interface ImportRun {
  started_at: string;
  finished_at: string;
  imported: number;
  skipped: number;
  errors: number;
  triggered_by?: string | null;
  failures?: string | null;
}

interface ConfiguredImport {
  repo: string;
  assets: string[];
  last_run?: ImportRun | null;
}

interface RegistryImports {
  registry: string;
  imports: ConfiguredImport[];
}

interface ImportFailure {
  tag: string;
  asset: string;
  error: string;
}

interface ImportResult {
  imported: number;
  skipped: number;
  errors: number;
  failures: ImportFailure[];
}

const { authFetch } = useAuthFetch();

const registries = ref<RegistryImports[]>([]);
const loading = ref(false);
const loadError = ref<string | null>(null);
const announcement = ref("");

// Keyed by `registry/repo`: one registry can have several imports configured
// into it and they run, fail and are retried independently.
const running = ref<Record<string, boolean>>({});
const results = ref<Record<string, ImportResult>>({});
const errors = ref<Record<string, string>>({});
const tagInputs = ref<Record<string, string>>({});

const key = (registry: string, repo: string) => `${registry}/${repo}`;

/** Flattened for one table: the row is an import, not a registry. */
const rows = computed(() =>
  registries.value.flatMap((r) => r.imports.map((i) => ({ registry: r.registry, ...i }))),
);

async function loadImports() {
  loading.value = true;
  loadError.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/imports`);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = (await res.json()) as { registries?: RegistryImports[] };
    registries.value = body.registries ?? [];
  } catch (e) {
    loadError.value = extractMessage(e);
  } finally {
    loading.value = false;
  }
}

async function triggerImport(registry: string, repo: string) {
  const k = key(registry, repo);
  running.value[k] = true;
  delete results.value[k];
  delete errors.value[k];

  const tag = (tagInputs.value[k] ?? "").trim();
  try {
    const res = await authFetch(
      `${API_BASE_URL}/api/v1/admin/registries/${encodeURIComponent(registry)}/import`,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        // `repo` always, so a registry with several imports runs the one whose
        // button was pressed rather than all of them.
        body: JSON.stringify(tag ? { repo, tag } : { repo }),
      },
    );
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string; message?: string };
      throw new Error(body.message ?? body.error ?? `HTTP ${res.status}`);
    }
    const result = (await res.json()) as ImportResult;
    results.value[k] = result;
    announcement.value = t("adminImports.importOutcome", {
      repo,
      imported: result.imported,
      skipped: result.skipped,
      errors: result.errors,
    });
    // The last-run column is now stale by definition — re-read rather than
    // patching it locally, so what the page shows is what the server stored.
    await loadImports();
  } catch (e) {
    errors.value[k] = extractMessage(e);
  } finally {
    running.value[k] = false;
  }
}

onMounted(() => void loadImports());
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <Announcer :message="announcement" />
    <PageHeader
      variant="display"
      :title="t('adminImports.releaseImports')"
      :description="t('adminImports.configuredImportsDescription')"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="loadImports">
          {{ loading ? t("common.loading") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>

    <AsyncState
      :loading="loading && rows.length === 0"
      :error="loadError"
      :empty="rows.length === 0"
    >
      <template #empty>
        <p class="text-sm text-muted-foreground">
          <i18n-t keypath="adminImports.noImportsConfigured" tag="span">
            <template #block><code>[[release_imports]]</code></template>
          </i18n-t>
        </p>
      </template>

      <Card>
        <CardContent class="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("adminImports.registry") }}</TableHead>
                <TableHead>{{ t("adminImports.repository") }}</TableHead>
                <TableHead>{{ t("adminImports.lastRun") }}</TableHead>
                <TableHead>{{ t("common.actions") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="row in rows" :key="key(row.registry, row.repo)">
                <TableCell class="font-medium">{{ row.registry }}</TableCell>
                <TableCell>
                  <div>{{ row.repo }}</div>
                  <div class="text-xs text-muted-foreground">{{ row.assets.join(", ") }}</div>
                </TableCell>

                <TableCell>
                  <!--
                    "Ran and found nothing new" and "has not run" are the two
                    states an operator needs to tell apart, and the scheduler's
                    log line could not answer either from a browser. An absent
                    run says so in words; a run that imported nothing shows its
                    zero.
                  -->
                  <span v-if="!row.last_run" class="text-sm text-muted-foreground">
                    {{ t("adminImports.neverRun") }}
                  </span>
                  <div v-else class="space-y-1">
                    <div class="text-sm">{{ row.last_run.finished_at }}</div>
                    <div class="flex gap-2">
                      <Badge variant="secondary">
                        {{ t("adminImports.imported", { n: row.last_run.imported }) }}
                      </Badge>
                      <Badge variant="outline">
                        {{ t("adminImports.skipped", { n: row.last_run.skipped }) }}
                      </Badge>
                      <Badge v-if="row.last_run.errors > 0" variant="destructive">
                        {{ t("adminImports.errors", { n: row.last_run.errors }) }}
                      </Badge>
                    </div>
                    <div class="text-xs text-muted-foreground">
                      {{
                        row.last_run.triggered_by
                          ? t("adminImports.askedBy", { who: row.last_run.triggered_by })
                          : t("adminImports.scheduled")
                      }}
                    </div>
                    <pre
                      v-if="row.last_run.failures"
                      class="text-xs text-destructive whitespace-pre-wrap"
                      >{{ row.last_run.failures }}</pre>
                  </div>
                </TableCell>

                <TableCell>
                  <div class="flex gap-2">
                    <Input
                      v-model="tagInputs[key(row.registry, row.repo)]"
                      :placeholder="t('adminImports.tagPlaceholder')"
                      :aria-label="t('adminImports.tagLabel')"
                      class="w-40"
                    />
                    <Button
                      size="sm"
                      :disabled="running[key(row.registry, row.repo)]"
                      @click="triggerImport(row.registry, row.repo)"
                    >
                      {{
                        running[key(row.registry, row.repo)]
                          ? t("adminImports.importing")
                          : t("adminImports.importNow")
                      }}
                    </Button>
                  </div>

                  <p
                    v-if="errors[key(row.registry, row.repo)]"
                    class="mt-2 text-sm text-destructive"
                  >
                    {{ errors[key(row.registry, row.repo)] }}
                  </p>

                  <div v-if="results[key(row.registry, row.repo)]" class="mt-2 space-y-1">
                    <!--
                      Failures are named, not counted: "3 errors" is not
                      something an operator can act on and a tag with a reason
                      is. The same rule the warming report next door follows.
                    -->
                    <p
                      v-for="(f, i) in results[key(row.registry, row.repo)]!.failures"
                      :key="i"
                      class="text-xs text-destructive"
                    >
                      {{ f.tag }} {{ f.asset }}: {{ f.error }}
                    </p>
                    <p
                      v-if="
                        results[key(row.registry, row.repo)]!.errors > 0 &&
                        results[key(row.registry, row.repo)]!.failures.length === 0
                      "
                      class="text-xs text-muted-foreground"
                    >
                      {{ t("adminImports.errorsWithoutDetail") }}
                    </p>
                  </div>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </AsyncState>
  </div>
</template>
