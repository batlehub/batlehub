<script setup lang="ts">
import { ref, onMounted, computed } from "vue";
import { useI18n } from "vue-i18n";

import { useAuthFetch } from "@/composables/useAuthFetch";
import { API_BASE_URL } from "@/config";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
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
 * The air gap (RFC 0008 §6.6): what came across it, and what this instance
 * was asked for and did not hold.
 *
 * Both halves are the same argument. A disconnected instance has no upstream
 * to point at, so the bundle history *is* the provenance of everything it
 * holds; and the miss log is how the next bundle's contents are decided by
 * the estate rather than guessed by an operator.
 *
 * The page renders on a connected instance too, and says so: an empty miss
 * log there means "nothing refuses", not "nothing is missing".
 */
const { t } = useI18n();
const { authFetch } = useAuthFetch();

type Bundle = {
  bundle_id: string;
  signer_key: string;
  imported_at: string;
  imported_by?: string | null;
  entries: number;
  blobs: number;
  rejected: number;
  rejected_sample?: string | null;
};

type Miss = {
  registry: string;
  storage_key: string;
  kind: string;
  coordinate?: string | null;
  first_seen: string;
  last_seen: string;
  count: number;
};

const bundles = ref<Bundle[]>([]);
const misses = ref<Miss[]>([]);
const total = ref(0);
const airGapped = ref(false);
/**
 * Registries holding no cached artifact at all.
 *
 * On an air-gapped instance such a registry answers 503 to *everything*, and
 * an empty miss log beside it means "nobody has asked yet", not "complete".
 * Saying which ones is the difference between a page that reports and a page
 * that explains.
 */
const emptyRegistries = ref<string[]>([]);
const loading = ref(true);
const error = ref<string | null>(null);

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const [b, m] = await Promise.all([
      authFetch(`${API_BASE_URL}/api/v1/admin/bundle`),
      authFetch(`${API_BASE_URL}/api/v1/admin/air-gap/missing?per_page=100`),
    ]);
    if (b.ok) bundles.value = (await b.json()).items ?? [];
    if (m.ok) {
      const body = await m.json();
      misses.value = body.items ?? [];
      total.value = body.total ?? 0;
      airGapped.value = body.air_gapped ?? false;
      emptyRegistries.value = body.empty_registries ?? [];
    } else if (m.status !== 503) {
      error.value = t("airGap.loadFailed");
    }
  } catch {
    error.value = t("airGap.loadFailed");
  } finally {
    loading.value = false;
  }
}

/** The short form an operator recognises a signer by. */
function shortKey(key: string): string {
  return key.slice(0, 8);
}

function kindVariant(kind: string) {
  return kind === "unmirrored_host" ? "destructive" : "secondary";
}

/**
 * The miss log as the next plan's input: one coordinate per line, most-asked
 * first. Copied rather than downloaded — it is a short list an operator
 * pastes into a ticket or a lock file, not a file format.
 */
const missesAsText = computed(() =>
  misses.value.map((m) => `${m.registry}\t${m.storage_key}\t${m.count}`).join("\n"),
);

const copied = ref(false);
async function copyMisses() {
  try {
    await navigator.clipboard.writeText(missesAsText.value);
    copied.value = true;
    setTimeout(() => (copied.value = false), 2000);
  } catch {
    copied.value = false;
  }
}

onMounted(load);
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <PageHeader :title="t('airGap.title')" variant="display" />

    <p v-if="error" class="text-sm text-destructive">{{ error }}</p>

    <!-- The state of the instance, in one sentence: it decides how to read
         everything below. -->
    <p class="text-sm text-muted-foreground" data-testid="air-gap-state">
      {{ airGapped ? t("airGap.enabled") : t("airGap.disabled") }}
    </p>

    <!-- A registry with nothing in it refuses every request. That is worth
         saying next to the state, not leaving to be inferred from a miss log
         that is empty for the opposite reason. -->
    <p
      v-if="emptyRegistries.length"
      class="text-sm text-destructive"
      data-testid="air-gap-empty-registries"
    >
      {{ t("airGap.emptyRegistries", { registries: emptyRegistries.join(", ") }) }}
    </p>

    <Card>
      <CardHeader>
        <CardTitle>{{ t("airGap.bundles") }}</CardTitle>
      </CardHeader>
      <CardContent class="space-y-3">
        <p class="text-sm text-muted-foreground">{{ t("airGap.bundlesIntro") }}</p>
        <Table v-if="bundles.length" data-testid="air-gap-bundles">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("airGap.bundle") }}</TableHead>
              <TableHead>{{ t("airGap.signer") }}</TableHead>
              <TableHead>{{ t("airGap.imported") }}</TableHead>
              <TableHead>{{ t("airGap.blobs") }}</TableHead>
              <TableHead>{{ t("airGap.rejected") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="b in bundles" :key="b.bundle_id">
              <TableCell class="font-mono text-xs">{{ b.bundle_id }}</TableCell>
              <TableCell class="font-mono text-xs">{{ shortKey(b.signer_key) }}</TableCell>
              <TableCell class="whitespace-nowrap text-xs tabular-nums">
                {{ formatDate(b.imported_at) }}
                <span v-if="b.imported_by" class="text-muted-foreground">
                  — {{ b.imported_by }}
                </span>
              </TableCell>
              <TableCell class="tabular-nums">{{ b.blobs }} / {{ b.entries }}</TableCell>
              <TableCell class="tabular-nums">
                <span v-if="!b.rejected">—</span>
                <span v-else class="text-destructive" :title="b.rejected_sample ?? undefined">
                  {{ b.rejected }}
                </span>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <p v-else-if="!loading" class="text-sm text-muted-foreground" data-testid="air-gap-no-bundles">
          {{ t("airGap.noBundles") }}
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle>
          {{ t("airGap.missing") }}
          <span v-if="total" class="font-mono text-base font-normal text-muted-foreground"
            >({{ total }})</span
          >
        </CardTitle>
      </CardHeader>
      <CardContent class="space-y-3">
        <p class="text-sm text-muted-foreground">{{ t("airGap.missingIntro") }}</p>
        <Table v-if="misses.length" data-testid="air-gap-missing">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead>{{ t("airGap.kind") }}</TableHead>
              <TableHead>{{ t("airGap.key") }}</TableHead>
              <TableHead>{{ t("airGap.asked") }}</TableHead>
              <TableHead>{{ t("airGap.lastSeen") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="m in misses" :key="`${m.registry}|${m.storage_key}`">
              <TableCell class="font-mono text-sm">{{ m.registry }}</TableCell>
              <TableCell>
                <Badge :variant="kindVariant(m.kind)" class="text-xs">{{ m.kind }}</Badge>
              </TableCell>
              <TableCell class="font-mono text-xs">{{ m.storage_key }}</TableCell>
              <TableCell class="tabular-nums">{{ m.count }}</TableCell>
              <TableCell class="whitespace-nowrap text-xs tabular-nums">
                {{ formatDate(m.last_seen) }}
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <p v-else-if="!loading" class="text-sm text-muted-foreground" data-testid="air-gap-no-misses">
          {{ airGapped ? t("airGap.noMisses") : t("airGap.noMissesConnected") }}
        </p>

        <button
          v-if="misses.length"
          type="button"
          class="text-sm underline text-muted-foreground hover:text-foreground"
          data-testid="air-gap-copy"
          @click="copyMisses"
        >
          {{ copied ? t("airGap.copied") : t("airGap.copy") }}
        </button>
      </CardContent>
    </Card>
  </div>
</template>
