<script setup lang="ts">
import { Resolution } from "@/components/ui/resolution";
import { packagePath } from "@/config/navigation";
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { useRouter } from "vue-router";
import {
  blockPackage,
  unblockPackage,
  bulkBlockPackages,
  bulkUnblockPackages,
  invalidatePackage,
} from "@/client/sdk.gen";
import type { PackageVersionDetail } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import { formatDate as fmtDate } from "@/lib/format";
import { Button } from "@/components/ui/button";
import BulkActionBar from "@/components/admin/BulkActionBar.vue";
import { useRowSelection } from "@/composables/useRowSelection";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import { MISE_BACKEND_TYPES, miseBlockGuess } from "@/lib/miseBlock";
import { Badge } from "@/components/ui/badge";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

type BlockedStatus = Extract<PackageVersionDetail["status"], { status: "blocked" }>;

const props = defineProps<{
  registry: string;
  name: string;
  versions: PackageVersionDetail[];
}>();

const emit = defineEmits<{ reload: [] }>();

const router = useRouter();

function isPreRelease(v: string) {
  return v.includes("-");
}

function severityVariant(severity: string): "default" | "destructive" | "secondary" | "outline" {
  switch (severity) {
    case "critical":
    case "high":
      return "destructive";
    case "medium":
      return "default";
    default:
      return "secondary";
  }
}

function viewArtifact(v: PackageVersionDetail) {
  /* One canonical package URL; the version and artifact stay as query, since
     they select *within* a package rather than name a different one. */
  void router.push({
    path: packagePath(props.registry, props.name),
    query: {
      version: v.version,
      ...(v.artifact ? { artifact: v.artifact } : {}),
    },
  });
}

// ── Single-item actions ──────────────────────────────────────────────────────

/**
 * The server's refusal, or `null` when it did as it was asked.
 *
 * The generated client is built without `throwOnError`, so a `403`, a `409` or a
 * `500` resolves normally with the failure sitting in `error`. Discarding it made
 * the confirmation dialog close on `actionError === null` and the table redraw
 * unchanged — an operator was told a block had happened when the server had
 * refused it.
 */
function refusal(res: { error?: unknown }): string | null {
  return res.error ? extractMessage(res.error) : null;
}

async function doBlock(v: PackageVersionDetail) {
  const reason = blockReason.value.trim();
  if (!reason) {
    actionError.value = t("packageVersionsTable.blockNeedsReason");
    return;
  }
  const res = await blockPackage({
    body: {
      registry: props.registry,
      name: props.name,
      version: v.version,
      artifact: v.artifact ?? undefined,
      reason,
    },
  });
  actionError.value = refusal(res);
  if (actionError.value) return;
  emit("reload");
  await blockOnMise(reason);
}

// ── The matching mise block (RFC 0043 decision 1) ────────────────────────────

const { registries, typeOf } = useRegistryOptions();

/** One `mise-versions` registry's offered block: pre-filled, editable, opt-in. */
interface MiseOffer {
  registry: string;
  tool: string;
  version: string;
  chosen: boolean;
}
const miseOffers = ref<MiseOffer[]>([]);

/** A release of a forge mise installs from: offer a block on each versions host. */
function offerMiseBlocks(v: PackageVersionDetail): MiseOffer[] {
  if (!MISE_BACKEND_TYPES.includes(typeOf(props.registry) ?? "")) return [];
  const guess = miseBlockGuess(props.name, v.version);
  return registries.value
    .filter((r) => r.type === "mise-versions")
    .map((r) => ({ registry: r.name, ...guess, chosen: false }));
}

/** After the forge block landed: the chosen mise blocks, each with the same reason. */
async function blockOnMise(reason: string): Promise<void> {
  for (const offer of miseOffers.value) {
    const tool = offer.tool.trim();
    const version = offer.version.trim();
    if (!offer.chosen || !tool || !version) continue;
    const res = await blockPackage({
      body: { registry: offer.registry, name: tool, version, reason },
    });
    const error = refusal(res);
    if (error) {
      actionError.value = t("packageVersionsTable.miseBlockFailed", {
        registry: offer.registry,
        error,
      });
      return;
    }
  }
}

async function doUnblock(v: PackageVersionDetail) {
  const res = await unblockPackage({
    body: {
      registry: props.registry,
      name: props.name,
      version: v.version,
      artifact: v.artifact ?? undefined,
    },
  });
  actionError.value = refusal(res);
  if (actionError.value) return;
  emit("reload");
}

async function doInvalidate(v: PackageVersionDetail) {
  const res = await invalidatePackage({
    body: {
      registry: props.registry,
      name: props.name,
      version: v.version,
      artifact: v.artifact ?? undefined,
    },
  });
  actionError.value = refusal(res);
  if (actionError.value) return;
  emit("reload");
}

// ── Multi-select ──────────────────────────────────────────────────────────────

const bulkLoading = ref(false);
const bulkMsg = ref<string | null>(null);

/** The `AdminPackages` sentence, over versions rather than packages. */
const BULK_KEYS = {
  blocked: "packageVersionsTable.bulkBlocked",
  unblocked: "packageVersionsTable.bulkUnblocked",
} as const;

function bulkOutcome(verb: keyof typeof BULK_KEYS, ok: number, failed: number): string {
  const done = t(BULK_KEYS[verb], { count: ok }, ok);
  return failed ? t("packageVersionsTable.bulkWithFailures", { done, failed }, failed) : done;
}

const {
  selected: selectedIds,
  allSelected,
  toggleAll,
  toggle,
  clear: clearSelection,
} = useRowSelection(
  () => props.versions,
  (v) => v.id,
);

const selected = computed(() => props.versions.filter((v) => selectedIds.value.has(v.id)));

// ── The destructive contract ─────────────────────────────────────────────────
//
// These four actions went through `prompt()` and `confirm()`. A native dialog
// cannot state a count, a scope or a consequence; it is not translated, it
// cannot be styled, and Firefox and Chrome both let a page suppress it after
// the second one — so the third block of a session could go through with no
// prompt at all. `DestructiveConfirm` is the primitive RFC 0003 §4.5 built for
// this, and the sibling admin pages already route every comparable verb
// through it.
//
// The block effect text is not lost in the move: it was the most useful thing
// the old prompt said, and it is exactly what `consequence` is for.

type PendingAction =
  | { kind: "block-one"; version: PackageVersionDetail }
  | { kind: "purge-one"; version: PackageVersionDetail }
  | { kind: "bulk-block" }
  | { kind: "bulk-unblock" };

const pending = ref<PendingAction | null>(null);
/** The reason a block carries, collected by the dialog rather than a prompt. */
const blockReason = ref("");
const actionError = ref<string | null>(null);

/** Open the dialog for one action, from a clean slate. */
function ask(action: PendingAction): void {
  blockReason.value = "";
  actionError.value = null;
  miseOffers.value = action.kind === "block-one" ? offerMiseBlocks(action.version) : [];
  pending.value = action;
}

const needsReason = computed(
  () => pending.value?.kind === "block-one" || pending.value?.kind === "bulk-block",
);

/** What a block does, in the words the old prompt used. */
function blockEffect(v?: PackageVersionDetail): string {
  return v?.artifact
    ? t("packageVersionsTable.blockEffectArtifact", { version: v.version, artifact: v.artifact })
    : t("packageVersionsTable.blockEffect");
}

const confirmProps = computed(() => {
  const action = pending.value;
  if (!action) return null;

  if (action.kind === "block-one") {
    return {
      action: t("common.block"),
      count: 1,
      itemNoun: t("packageVersionsTable.versionNoun"),
      scope: action.version.artifact ?? action.version.version,
      consequence: blockEffect(action.version),
      reversible: true,
    };
  }
  if (action.kind === "purge-one") {
    return {
      action: t("packageVersionsTable.purge"),
      count: 1,
      itemNoun: t("packageVersionsTable.artifactNoun"),
      scope: action.version.version,
      // Not irreversible: the bytes come back on the next download. What it
      // costs is one upstream round trip, which is the fact worth stating.
      consequence: t("packageVersionsTable.purgeEffect"),
      reversible: true,
    };
  }
  if (action.kind === "bulk-block") {
    return {
      action: t("common.block"),
      count: selectedIds.value.size,
      itemNoun: t("packageVersionsTable.versionNoun"),
      itemNounPlural: t("packageVersionsTable.versionNounPlural"),
      scope: props.name,
      consequence: blockEffect(),
      reversible: true,
    };
  }
  return {
    action: t("common.unblock"),
    count: selectedIds.value.size,
    itemNoun: t("packageVersionsTable.versionNoun"),
    itemNounPlural: t("packageVersionsTable.versionNounPlural"),
    scope: props.name,
    reversible: true,
  };
});

async function runPending(): Promise<void> {
  const action = pending.value;
  if (!action) return;
  // A missing reason is refused *in* the dialog rather than by closing it —
  // `doBlock` and `bulkBlock` write `actionError` and return.
  actionError.value = null;
  // `@confirm` binds this directly, so an unhandled rejection would leave the
  // dialog open, the button live and nothing on screen saying why: the client
  // rejects (rather than resolving with `error`) when `fetch` itself fails —
  // offline, DNS, TLS.
  try {
    if (action.kind === "block-one") await doBlock(action.version);
    else if (action.kind === "purge-one") await doInvalidate(action.version);
    else if (action.kind === "bulk-block") await bulkBlock();
    else await bulkUnblock();
  } catch (e) {
    actionError.value = extractMessage(e);
  }
  if (!actionError.value) pending.value = null;
}

async function bulkBlock() {
  const reason = blockReason.value.trim();
  if (!reason) {
    actionError.value = t("packageVersionsTable.blockNeedsReason");
    return;
  }
  bulkLoading.value = true;
  bulkMsg.value = null;
  try {
    const res = await bulkBlockPackages({
      body: {
        items: selected.value.map((v) => ({
          registry: props.registry,
          name: props.name,
          version: v.version,
          artifact: v.artifact ?? null,
          reason,
        })),
      },
    });
    const r = res.data;
    if (r) {
      bulkMsg.value = bulkOutcome("blocked", r.succeeded_count, r.failed_count);
    } else {
      // No body means the request itself was refused; `finally` still clears the
      // selection and reloads, so without this the dialog closed on silence.
      actionError.value = refusal(res) ?? t("packageVersionsTable.bulkFailed");
    }
  } finally {
    bulkLoading.value = false;
    // The selection survives a refusal, so the operator can fix the cause and
    // press the button again rather than re-tick every row.
    if (!actionError.value) clearSelection();
    emit("reload");
  }
}

async function bulkUnblock() {
  bulkLoading.value = true;
  bulkMsg.value = null;
  try {
    const res = await bulkUnblockPackages({
      body: {
        items: selected.value.map((v) => ({
          registry: props.registry,
          name: props.name,
          version: v.version,
          artifact: v.artifact ?? null,
        })),
      },
    });
    const r = res.data;
    if (r) {
      bulkMsg.value = bulkOutcome("unblocked", r.succeeded_count, r.failed_count);
    } else {
      actionError.value = refusal(res) ?? t("packageVersionsTable.bulkFailed");
    }
  } finally {
    bulkLoading.value = false;
    if (!actionError.value) clearSelection();
    emit("reload");
  }
}
</script>

<template>
  <!-- Bulk action bar -->
  <BulkActionBar
    v-if="selectedIds.size > 0"
    :label="t('packageVersionsTable.versionsSelected', selectedIds.size)"
    @clear="clearSelection"
  >
    <Button
      size="sm"
      variant="outline"
      :disabled="bulkLoading"
      @click="ask({ kind: 'bulk-block' })"
      >{{ t("packageVersionsTable.blockSelected") }}</Button
    >
    <Button
      size="sm"
      variant="outline"
      :disabled="bulkLoading"
      @click="ask({ kind: 'bulk-unblock' })"
      >{{ t("packageVersionsTable.unblockSelected") }}</Button
    >
  </BulkActionBar>
  <!-- Outside the selection-gated bar: a success clears the selection, which
       destroyed the message in the same tick (same fix as AdminPackages). -->
  <output v-if="bulkMsg" class="block text-sm text-muted-foreground">{{ bulkMsg }}</output>

  <!-- Versions table -->
  <Card>
    <CardHeader>
      <CardTitle>{{ t("packageVersionsTable.versionsArtifacts") }}</CardTitle>
    </CardHeader>
    <CardContent class="p-0">
      <Table :label="t('a11y.versionsTableLabel')">
        <TableHeader>
          <TableRow>
            <TableHead class="w-8">
              <input
                type="checkbox"
                :aria-label="t('packageVersionsTable.selectAllVersions')"
                :checked="allSelected"
                class="cursor-pointer"
                @change="toggleAll"
              />
            </TableHead>
            <TableHead>{{ t("common.version") }}</TableHead>
            <TableHead>{{ t("common.artifact") }}</TableHead>
            <TableHead>{{ t("common.status") }}</TableHead>
            <TableHead>{{ t("common.security") }}</TableHead>
            <TableHead>{{ t("common.cached") }}</TableHead>
            <TableHead>{{ t("common.downloads") }}</TableHead>
            <TableHead>{{ t("common.storage") }}</TableHead>
            <TableHead>{{ t("packageVersionsTable.lastAccessed") }}</TableHead>
            <TableHead>{{ t("packageVersionsTable.lastPulledBy") }}</TableHead>
            <TableHead class="text-right">{{ t("common.actions") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <!-- No row tint: 1.03:1 is not a channel. The status cell below
               carries it in words and in hue. -->
          <TableRow v-for="v in versions" :key="v.id">
            <TableCell class="w-8">
              <input
                type="checkbox"
                :aria-label="t('packageVersionsTable.selectVersion', { version: v.version })"
                :checked="selectedIds.has(v.id)"
                class="cursor-pointer"
                @change="toggle(v)"
              />
            </TableCell>
            <TableCell class="font-mono text-xs">
              {{ v.version }}
              <Badge
                v-if="isPreRelease(v.version)"
                variant="outline"
                class="ml-1 text-xs align-middle"
                >{{ t("packageVersionsTable.preRelease") }}</Badge
              >
              <!-- In the version cell rather than a column of its own: RFC
                   0004-bis §6.1's verdict on this table is that it is already
                   too wide at 1440, so a licence that earned a twelfth column
                   would push the row verbs further off screen.

                   A dash when unknown, with the reason in the title. Rendering
                   nothing would let "we never read a manifest for this registry
                   type" look identical to "this package declares no licence",
                   which is the §2.4 defect — a blank that reads as a fact. -->
              <p
                class="text-muted-foreground truncate max-w-[180px]"
                :title="v.license ?? t('packageVersionsTable.licenseUnknownHelp')"
              >
                {{ v.license ?? t("packageVersionsTable.licenseUnknown") }}
              </p>
            </TableCell>
            <TableCell class="font-mono text-xs text-muted-foreground">{{
              v.artifact ?? "—"
            }}</TableCell>
            <TableCell>
              <div class="space-y-1">
                <Resolution
                  v-if="v.status.status === 'blocked'"
                  state="blocked"
                  :label="t('common.blocked')"
                />
                <Badge v-else variant="secondary">{{ t("packageVersionsTable.available") }}</Badge>
                <p
                  v-if="v.status.status === 'blocked'"
                  class="text-xs text-muted-foreground max-w-[180px] truncate"
                  :title="(v.status as BlockedStatus).reason"
                >
                  {{ (v.status as BlockedStatus).reason }}
                </p>
              </div>
            </TableCell>
            <TableCell class="text-sm">
              <div class="flex flex-wrap items-center gap-1">
                <span
                  v-for="vuln in v.vulnerabilities"
                  :key="vuln.osv_id"
                  :title="`${vuln.osv_id}: ${vuln.summary}${vuln.fixed_version ? t('packageVersionsTable.fixedIn', { version: vuln.fixed_version }) : ''}`"
                >
                  <Badge :variant="severityVariant(vuln.severity)" class="text-xs cursor-help">
                    {{ vuln.severity }}
                  </Badge>
                </span>
                <a
                  v-if="v.socket_badge_url"
                  :href="v.socket_badge_url"
                  target="_blank"
                  rel="noopener noreferrer"
                  :title="t('packageVersionsTable.supplyChainReportOn')"
                >
                  <img :src="v.socket_badge_url" alt="socket.dev" class="h-4" />
                </a>
                <span
                  v-if="v.vulnerabilities.length === 0 && !v.socket_badge_url"
                  class="text-muted-foreground"
                  >—</span
                >
              </div>
            </TableCell>
            <TableCell>
              <Badge :variant="v.cached ? 'known' : 'outline'" class="text-xs">
                {{ v.cached ? t("common.cached") : t("packageVersionsTable.notCached") }}
              </Badge>
              <p v-if="v.cached_at" class="text-xs text-muted-foreground mt-1">
                {{ fmtDate(v.cached_at) }}
              </p>
              <p class="text-xs text-muted-foreground font-mono mt-1">{{ v.storage_key }}</p>
            </TableCell>
            <TableCell class="text-right tabular-nums">{{ v.access_count }}</TableCell>
            <TableCell>
              <Badge v-if="v.storage_backend" variant="outline" class="text-xs font-mono">{{
                v.storage_backend
              }}</Badge>
              <span v-else class="text-muted-foreground text-sm">—</span>
            </TableCell>
            <TableCell class="text-xs">{{ fmtDate(v.last_accessed) }}</TableCell>
            <TableCell class="text-sm">
              <span v-if="v.last_accessed_by" class="font-medium">{{ v.last_accessed_by }}</span>
              <span v-else-if="v.access_count > 0" class="text-muted-foreground italic"
                >anonymous</span
              >
              <span v-else class="text-muted-foreground">—</span>
            </TableCell>
            <TableCell class="text-right">
              <div class="flex justify-end gap-2">
                <Button variant="ghost" size="sm" @click="viewArtifact(v)">{{
                  t("common.view")
                }}</Button>
                <Button
                  v-if="v.cached"
                  variant="outline"
                  size="sm"
                  @click="ask({ kind: 'purge-one', version: v })"
                  >{{ t("packageVersionsTable.purgeCache") }}</Button
                >
                <Button
                  v-if="v.status.status === 'blocked'"
                  variant="outline"
                  size="sm"
                  @click="doUnblock(v)"
                  >{{ t("common.unblock") }}</Button
                >
                <Button
                  v-else
                  variant="outline"
                  size="sm"
                  @click="ask({ kind: 'block-one', version: v })"
                  >{{ t("common.block") }}</Button
                >
              </div>
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p v-if="versions.length === 0" class="p-6 text-sm text-muted-foreground text-center">
        {{ t("packageVersionsTable.noVersionsTrackedYet") }}
      </p>
    </CardContent>
  </Card>

  <!--
    One dialog for all four verbs. The reason field renders only for the two
    that need it, which is the same slot `AdminPackages` uses for the same
    purpose — a block reason is read back by whoever hits the 403, so it is
    collected rather than invented.
  -->
  <DestructiveConfirm
    v-if="confirmProps"
    :open="pending !== null"
    v-bind="confirmProps"
    :loading="bulkLoading"
    :error="actionError"
    @update:open="(open: boolean) => !open && (pending = null)"
    @confirm="runPending"
  >
    <div v-if="needsReason" class="space-y-1">
      <Label for="version-block-reason">{{ t("common.reason") }}</Label>
      <Input
        id="version-block-reason"
        v-model="blockReason"
        :placeholder="t('packageVersionsTable.blockReasonPlaceholder')"
      />
    </div>
    <!-- RFC 0043 decision 1: mise lists this release by its own tool name, so
         the forge block alone leaves it in `mise ls-remote`. Opt-in per
         registry; the guess is the admin's to correct. -->
    <fieldset
      v-for="(offer, i) in miseOffers"
      :key="offer.registry"
      class="space-y-2 border-t border-rule-soft pt-3"
    >
      <legend class="sr-only">{{ t("packageVersionsTable.miseBlockLegend") }}</legend>
      <label class="flex items-start gap-2 text-sm">
        <input
          :id="`mise-block-${i}`"
          v-model="offer.chosen"
          type="checkbox"
          class="mt-1 cursor-pointer"
        />
        <span>
          {{ t("packageVersionsTable.miseBlockOffer", { registry: offer.registry }) }}
          <span class="block text-muted-foreground">{{
            t("packageVersionsTable.miseBlockWhy")
          }}</span>
        </span>
      </label>
      <div v-if="offer.chosen" class="grid gap-2 sm:grid-cols-2">
        <div class="space-y-1">
          <Label :for="`mise-block-tool-${i}`">{{ t("packageVersionsTable.miseTool") }}</Label>
          <Input :id="`mise-block-tool-${i}`" v-model="offer.tool" />
        </div>
        <div class="space-y-1">
          <Label :for="`mise-block-version-${i}`">{{ t("common.version") }}</Label>
          <Input :id="`mise-block-version-${i}`" v-model="offer.version" />
        </div>
      </div>
    </fieldset>
  </DestructiveConfirm>
</template>
