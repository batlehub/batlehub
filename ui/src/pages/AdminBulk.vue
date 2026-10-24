<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { Upload, CheckCircle2, XCircle } from "@lucide/vue";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { PACKAGES_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { bulkBlockPackages, bulkUnblockPackages } from "@/client/sdk.gen";
import { extractMessage } from "@/composables/useApi";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import type {
  BulkActionResponse,
  BulkBlockRequestItem,
  BulkUnblockRequestItem,
} from "@/client/types.gen";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
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

type Action = "block" | "unblock";

interface ParsedRow {
  registry: string;
  name: string;
  version: string;
  artifact: string;
  reason: string;
  error?: string;
}

const action = ref<Action>("block");
const csvText = ref("");
const defaultReason = ref("");
const parsedRows = ref<ParsedRow[]>([]);
const parseError = ref<string | null>(null);
const submitting = ref(false);
const result = ref<BulkActionResponse | null>(null);
const submitError = ref<string | null>(null);

/** The bulk outcome as one sentence, for the live region. */
const resultAnnouncement = computed(() =>
  result.value
    ? t("adminBulk.doneSummary", {
        succeeded: result.value.succeeded_count,
        failed: result.value.failed_count,
      })
    : "",
);

const validRows = computed(() => parsedRows.value.filter((r) => !r.error));
const invalidRows = computed(() => parsedRows.value.filter((r) => !!r.error));

function parseCSV() {
  result.value = null;
  submitError.value = null;
  parseError.value = null;
  parsedRows.value = [];

  const text = csvText.value.trim();
  if (!text) {
    parseError.value = t("adminBulk.pasteCsvFirst");
    return;
  }

  const lines = text.split(/\r?\n/);
  // Skip header if present
  const dataLines = lines[0].toLowerCase().startsWith("registry") ? lines.slice(1) : lines;

  if (dataLines.length === 0 || (dataLines.length === 1 && !dataLines[0].trim())) {
    parseError.value = t("adminBulk.noDataRows");
    return;
  }

  parsedRows.value = dataLines
    .filter((l) => l.trim())
    .map((line) => {
      const cols = line.split(",").map((c) => c.trim());
      const [registry = "", name = "", version = "", artifact = "", reason = ""] = cols;
      const row: ParsedRow = { registry, name, version, artifact, reason };

      if (!registry) row.error = t("adminBulk.registryRequired");
      else if (!name) row.error = t("adminBulk.nameRequired");
      else if (!version) row.error = t("adminBulk.versionRequired");
      else if (action.value === "block" && !reason && !defaultReason.value) {
        row.error = t("adminBulk.reasonRequiredForBlock");
      }

      return row;
    });
}

async function handleFileUpload(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0];
  if (!file) return;
  csvText.value = await file.text();
  parseCSV();
}

async function submit() {
  if (validRows.value.length === 0) return;
  submitting.value = true;
  submitError.value = null;
  result.value = null;

  // `res.error`, not `catch`. The generated client is built without
  // `throwOnError`, so an HTTP failure resolves rather than throws: the
  // `catch` below never fired, `result` was set to `null`, and a bulk block
  // that returned 405 rendered *nothing at all* — no error, no result, the
  // button simply re-enabled. Verified in a browser before this fix.
  try {
    if (action.value === "block") {
      const items: BulkBlockRequestItem[] = validRows.value.map((r) => ({
        registry: r.registry,
        name: r.name,
        version: r.version,
        artifact: r.artifact || null,
        reason: r.reason || defaultReason.value,
      }));
      const res = await bulkBlockPackages({ body: { items } });
      if (res.error) {
        submitError.value = extractMessage(res.error);
        return;
      }
      result.value = res.data ?? null;
    } else {
      const items: BulkUnblockRequestItem[] = validRows.value.map((r) => ({
        registry: r.registry,
        name: r.name,
        version: r.version,
        artifact: r.artifact || null,
      }));
      const res = await bulkUnblockPackages({ body: { items } });
      if (res.error) {
        submitError.value = extractMessage(res.error);
        return;
      }
      result.value = res.data ?? null;
    }
  } catch (e) {
    submitError.value = extractMessage(e);
  } finally {
    submitting.value = false;
  }
}

/**
 * The destructive contract (PRODUCT.md principle 2): scope, count and
 * consequence before confirmation.
 *
 * Neither bulk path had one — a single click blocked or released an arbitrary
 * number of coordinates. `DestructiveConfirm` is the primitive RFC 0003 built
 * for exactly this, and the sibling tab already routes three actions through
 * it, so an operator was getting a typed-name confirmation for unblocking on
 * one route and nothing at all for the same verb on this one.
 *
 * `reversible: true` for both: a block is undone by an unblock and vice versa,
 * so the typed-name step would be ceremony rather than protection. The count
 * and the scope are what matter here.
 */
const confirmOpen = ref(false);

/** The verb, from the catalogue — the button and the dialog say the same one. */
const actionVerb = computed(() =>
  action.value === "block" ? t("adminBulk.block") : t("adminBulk.unblock"),
);

/**
 * The noun, agreeing with the count.
 *
 * The button read `` `${verb} ${n} package${n !== 1 ? "s" : ""}` ``: a French
 * verb, a count, and an English noun carrying an English plural rule. Both
 * halves now come from the catalogue, and the sentence that joins them is
 * `destructive.summary` — the same one the confirmation dialog below renders,
 * so the button and the dialog it opens cannot drift apart.
 */
const packageNoun = computed(() =>
  validRows.value.length === 1 ? t("adminBulk.packageNoun") : t("adminBulk.packageNounPlural"),
);

const submitLabel = computed(() =>
  t("destructive.summary", {
    action: actionVerb.value,
    count: validRows.value.length,
    item: packageNoun.value,
  }),
);

/** The registries the pending action touches, for the dialog's scope line. */
const affectedScope = computed(() => {
  const registries = [...new Set(validRows.value.map((r) => r.registry))].sort();
  if (registries.length === 1) return registries[0];
  return t("adminBulk.acrossRegistries", { count: registries.length });
});

function requestSubmit() {
  if (validRows.value.length === 0) return;
  confirmOpen.value = true;
}

async function confirmSubmit() {
  confirmOpen.value = false;
  await submit();
}

function reset() {
  csvText.value = "";
  parsedRows.value = [];
  result.value = null;
  parseError.value = null;
  submitError.value = null;
  defaultReason.value = "";
}
</script>

<template>
  <div class="space-y-6 max-w-4xl">
    <!-- An outcome announced, not only rendered (RFC 0004-bis §2.6). Every bulk
         result, block, warm and cache invalidation in this console reached
         sighted users only — on the surface whose audience includes the one
         person able to perform destructive actions. -->
    <Announcer :message="resultAnnouncement ?? ''" />
    <SectionTabs :tabs="PACKAGES_TABS" />
    <PageHeader
      :title="t('adminBulk.bulkBlock')"
      :description="t('adminBulk.blockOrUnblockMultiplePackages')"
      variant="display"
    />

    <!-- Format reference -->
    <Card>
      <CardHeader class="pb-2">
        <CardTitle class="text-sm font-medium text-muted-foreground">{{
          t("adminBulk.csvFormat")
        }}</CardTitle>
      </CardHeader>
      <CardContent>
        <!--
          A scroll container must be reachable by keyboard: a mouse user can
          drag it sideways, and without `tabindex` a keyboard user cannot read
          the part that is off-screen at all — axe calls it
          `scrollable-region-focusable`. Only visible at 390px, which is why it
          appeared the moment the gate started measuring narrow.

          The container is a **named `<section>`**, which is the same construct
          `ui/components/ui/table/Table.vue` uses for the same job, so the
          console has one answer for "wide content inside a narrow column"
          rather than two. It was a `<pre role="group">`: `group` is for a set of
          related form controls and there are none here, which is what Sonar's
          Web:S6819 caught — and the fix is not a different `role` but no `role`
          at all, because a `<section>` with an accessible name carries `region`
          natively and a native element is the one assistive technology has the
          fewest ways to get wrong.

          The box styling moves with the scrolling, so the `<pre>` is now just
          the text.
        -->
        <section
          tabindex="0"
          :aria-label="t('adminBulk.csvFormat')"
          class="bg-muted rounded-sm p-3 overflow-x-auto"
        >
          <pre class="text-xs font-mono">
registry,name,version,artifact,reason
npm,lodash,4.17.21,,CVE-2021-23337
cargo,serde,1.0.0,,License issue
github,org/repo,v2.0.0,binary.tar.gz,Supply chain risk</pre>
        </section>
        <p class="text-xs text-muted-foreground mt-2">
          <i18n-t keypath="adminBulk.csvNotes" tag="span">
            <template #artifact><code class="font-mono">artifact</code></template>
            <template #reason><code class="font-mono">reason</code></template>
          </i18n-t>
        </p>
      </CardContent>
    </Card>

    <!-- Action + input -->
    <Card>
      <CardHeader class="pb-3">
        <CardTitle class="text-base">{{ t("adminBulk.configureAction") }}</CardTitle>
      </CardHeader>
      <CardContent class="space-y-4">
        <!-- Action selector -->
        <div class="flex gap-2">
          <Button
            :variant="action === 'block' ? 'default' : 'outline'"
            size="sm"
            @click="
              action = 'block';
              parsedRows = [];
              result = null;
            "
          >
            {{ t("common.block") }}
          </Button>
          <Button
            :variant="action === 'unblock' ? 'default' : 'outline'"
            size="sm"
            @click="
              action = 'unblock';
              parsedRows = [];
              result = null;
            "
          >
            {{ t("common.unblock") }}
          </Button>
        </div>

        <!-- What a block does, stated once above the list rather than per row.
             A CSV row with an `artifact` column blocks one file, and that
             hides the whole version from listings — the one consequence an
             operator is likely to be surprised by. -->
        <p v-if="action === 'block'" class="text-sm text-muted-foreground max-w-2xl">
          {{ t("adminBulk.blockEffect") }}
        </p>

        <!-- Default reason (block only) -->
        <div v-if="action === 'block'" class="space-y-1 max-w-md">
          <Label for="default-reason"
            >{{ t("adminBulk.defaultReason") }}
            <span class="text-muted-foreground">{{ t("adminBulk.usedWhenTheCsv") }}</span></Label
          >
          <Input
            id="default-reason"
            v-model="defaultReason"
            placeholder="CVE-2025-XXXX or policy violation"
          />
        </div>

        <!-- CSV textarea -->
        <div class="space-y-1">
          <Label for="csv-input">{{ t("adminBulk.pasteCsv") }}</Label>
          <textarea
            id="csv-input"
            v-model="csvText"
            rows="8"
            placeholder="registry,name,version,artifact,reason&#10;npm,lodash,4.17.21,,CVE-2021-23337"
            class="flex w-full rounded-sm border border-input bg-transparent px-3 py-2 text-sm font-mono placeholder:text-muted-foreground resize-y"
          />
        </div>

        <!-- File upload -->
        <div class="flex items-center gap-3">
          <label
            class="flex items-center gap-2 px-3 py-1.5 rounded-sm border border-input text-sm cursor-pointer hover:bg-accent transition-colors"
          >
            <Upload class="h-3.5 w-3.5" />
            {{ t("adminBulk.uploadCsvFile") }}
            <input type="file" accept=".csv,text/csv" class="sr-only" @change="handleFileUpload" />
          </label>
          <span class="text-xs text-muted-foreground">{{ t("adminBulk.orPasteAbove") }}</span>
        </div>

        <p v-if="parseError" class="text-xs text-destructive">
          {{ parseError }}
        </p>

        <div class="flex gap-2">
          <Button variant="outline" @click="parseCSV">{{ t("adminBulk.previewRows") }}</Button>
          <Button variant="ghost" size="sm" @click="reset"> {{ t("common.reset") }} </Button>
        </div>
      </CardContent>
    </Card>

    <!-- Preview table -->
    <Card v-if="parsedRows.length > 0">
      <CardHeader class="pb-3">
        <div class="flex items-center justify-between">
          <CardTitle class="text-base">
            {{ t("common.preview") }}
            <span class="font-normal text-muted-foreground ml-1 text-sm">
              <i18n-t keypath="adminBulk.validInvalid" tag="span">
                <template #valid>{{ validRows.length }}</template>
                <template #invalid>{{ invalidRows.length }}</template>
              </i18n-t>
            </span>
          </CardTitle>
          <Button :disabled="validRows.length === 0 || submitting" @click="requestSubmit">
            {{ submitting ? t("adminBulk.processing") : submitLabel }}
          </Button>
        </div>
      </CardHeader>
      <CardContent class="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead>{{ t("common.name") }}</TableHead>
              <TableHead>{{ t("common.version") }}</TableHead>
              <TableHead>{{ t("common.artifact") }}</TableHead>
              <TableHead v-if="action === 'block'"> {{ t("common.reason") }} </TableHead>
              <TableHead>{{ t("common.status") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <!-- No row tint: 1.03:1 is not a channel. The last cell carries
                 the verdict, as a badge with its own word. -->
            <TableRow v-for="(row, i) in parsedRows" :key="i">
              <TableCell class="font-mono text-xs">
                {{ row.registry || "—" }}
              </TableCell>
              <TableCell class="font-mono text-xs">
                {{ row.name || "—" }}
              </TableCell>
              <TableCell class="font-mono text-xs">
                {{ row.version || "—" }}
              </TableCell>
              <TableCell class="font-mono text-xs text-muted-foreground">
                {{ row.artifact || "—" }}
              </TableCell>
              <TableCell v-if="action === 'block'" class="text-xs">
                {{ row.reason || defaultReason || "—" }}
              </TableCell>
              <TableCell>
                <Badge v-if="row.error" variant="destructive" class="text-xs">
                  {{ row.error }}
                </Badge>
                <Badge v-else variant="secondary" class="text-xs"> OK </Badge>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </CardContent>
    </Card>

    <DestructiveConfirm
      :open="confirmOpen"
      :action="actionVerb"
      :count="validRows.length"
      :item-noun="t('adminBulk.packageNoun')"
      :item-noun-plural="t('adminBulk.packageNounPlural')"
      :scope="affectedScope"
      reversible
      :loading="submitting"
      :error="submitError"
      @confirm="confirmSubmit"
      @update:open="confirmOpen = $event"
    />

    <!-- Results -->
    <Card v-if="result">
      <CardHeader class="pb-3">
        <CardTitle class="text-base flex items-center gap-2">
          <!-- Not unconditional: this rendered a crimson success check over
               "0 succeeded, 30 failed". The icon is the first thing read, and
               it was reporting the opposite of what happened. -->
          <XCircle v-if="result.succeeded_count === 0" class="h-4 w-4 text-destructive" />
          <CheckCircle2 v-else-if="result.failed_count === 0" class="h-4 w-4 text-primary" />
          <XCircle v-else class="h-4 w-4 text-copper" />
          <i18n-t keypath="adminBulk.doneSummary" tag="span">
            <template #succeeded>{{ result.succeeded_count }}</template>
            <template #failed>{{ result.failed_count }}</template>
          </i18n-t>
        </CardTitle>
      </CardHeader>
      <CardContent v-if="result.failures.length > 0" class="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead>{{ t("common.name") }}</TableHead>
              <TableHead>{{ t("common.version") }}</TableHead>
              <TableHead>{{ t("common.error") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <!-- Every row in this table is a failure and the error column says
                 so with an icon and the message; a 1.03:1 wash over all of them
                 added nothing. -->
            <TableRow v-for="(f, i) in result.failures" :key="i">
              <TableCell class="font-mono text-xs">
                {{ f.registry }}
              </TableCell>
              <TableCell class="font-mono text-xs">
                {{ f.name }}
              </TableCell>
              <TableCell class="font-mono text-xs">
                {{ f.version }}
              </TableCell>
              <TableCell class="text-xs text-destructive flex items-center gap-1">
                <XCircle class="h-3 w-3 shrink-0" />{{ f.error }}
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </CardContent>
    </Card>

    <p v-if="submitError" class="text-sm text-destructive">
      {{ submitError }}
    </p>
  </div>
</template>
