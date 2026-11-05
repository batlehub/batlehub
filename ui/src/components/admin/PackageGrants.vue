<script setup lang="ts">
/**
 * The package tier of RFC 0015's grant hierarchy, editable — RFC 0017 phase 3.
 *
 * The version tier is deliberately absent. §11 open question 3: the package tier
 * is a table of subjects, the version tier is that table *per version*, and a
 * package with four hundred versions is a different design problem. The CLI
 * carries the version tier (`batlehub-cli grant set reg/pkg@1.2.3 …`) until
 * someone asks for the panel.
 *
 * Version-tier rows are still *shown* here, read-only, because an operator
 * asking "who can reach this package" means both tiers and would otherwise
 * believe the answer is the package rows alone.
 */
import { ref, computed } from "vue";
import { useI18n } from "vue-i18n";
import { Plus, Trash2, AlertCircle, Lock } from "@lucide/vue";
import { listGrants, putGrant, deleteGrant } from "@/client/sdk.gen";
import type { GrantDto } from "@/client/types.gen";
import { useApi, extractMessage } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Alert } from "@/components/ui/alert";
import { Dialog } from "@/components/ui/dialog";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Announcer } from "@/components/ui/announcer";

const { t } = useI18n();
const { token } = useAuth();

const props = defineProps<{ registry: string; name: string }>();

const {
  data: grantData,
  loading,
  error,
  reload,
} = useApi<{ grants: GrantDto[] }>(() => {
  if (!props.registry || !props.name) return Promise.resolve({ data: undefined });
  return listGrants({
    path: { registry: props.registry },
    query: { package: props.name },
  }) as Promise<{ data?: unknown; error?: unknown }>;
}, [token]);

const grants = computed<GrantDto[]>(() => grantData.value?.grants ?? []);

/** The two tiers, kept apart: only one of them is editable here. */
const packageGrants = computed(() => grants.value.filter((g) => g.node_kind === "package"));
const versionGrants = computed(() => grants.value.filter((g) => g.node_kind === "version"));

// ── Write ─────────────────────────────────────────────────────────────────────

const showAdd = ref(false);
const saving = ref(false);
const writeError = ref<string | null>(null);
const announcement = ref("");
const form = ref({ subject: "", actions: "" });

function openAdd(existing?: GrantDto) {
  form.value = existing
    ? { subject: existing.subject, actions: existing.actions.join(", ") }
    : { subject: "", actions: "" };
  writeError.value = null;
  showAdd.value = true;
}

async function save() {
  const subject = form.value.subject.trim();
  const actions = form.value.actions
    .split(",")
    .map((a) => a.trim())
    .filter(Boolean);
  if (!subject || actions.length === 0) {
    writeError.value = t("packageGrants.subjectAndActionsRequired");
    return;
  }
  saving.value = true;
  writeError.value = null;
  try {
    const { data, error: apiErr } = await putGrant({
      path: { registry: props.registry },
      body: { package: props.name, subject, actions },
    });
    if (apiErr) {
      writeError.value = extractMessage(apiErr);
      return;
    }
    // The server's expanded set, not the form's text: `releases:*` names one
    // verb and stores several, and the operator has to be able to see which.
    const stored = (data as { actions?: string[]; warnings?: string[] } | undefined) ?? {};
    warnings.value = stored.warnings ?? [];
    announcement.value = t("packageGrants.grantSaved", {
      subject,
      actions: (stored.actions ?? actions).join(", "),
    });
    showAdd.value = false;
    reload();
  } finally {
    saving.value = false;
  }
}

/** Legal but probably not intended — reported by the server, not inferred here. */
const warnings = ref<string[]>([]);

// ── Remove ────────────────────────────────────────────────────────────────────

const removeTarget = ref<GrantDto | null>(null);
const removing = ref(false);

async function confirmRemove() {
  const target = removeTarget.value;
  if (!target) return;
  removing.value = true;
  writeError.value = null;
  try {
    const { error: apiErr } = await deleteGrant({
      path: { registry: props.registry },
      body: { package: props.name, subject: target.subject },
    });
    if (apiErr) {
      writeError.value = extractMessage(apiErr);
      return;
    }
    announcement.value = t("packageGrants.grantRemoved", { subject: target.subject });
    removeTarget.value = null;
    reload();
  } finally {
    removing.value = false;
  }
}
</script>

<template>
  <Card>
    <CardHeader class="pb-3">
      <CardTitle class="text-base">{{ t("packageGrants.whoCanReachThis") }}</CardTitle>
      <CardDescription>{{ t("packageGrants.grantsOnlyWiden") }}</CardDescription>
    </CardHeader>
    <CardContent class="space-y-4">
      <Alert v-if="writeError" variant="destructive" class="text-sm py-2">
        <AlertCircle class="h-3.5 w-3.5" />
        <span class="pl-2">{{ writeError }}</span>
      </Alert>

      <Alert v-for="w in warnings" :key="w" variant="default" class="text-sm py-2">
        <AlertCircle class="h-3.5 w-3.5" />
        <span class="pl-2">{{ w }}</span>
      </Alert>

      <div v-if="error" class="text-sm text-destructive">{{ error }}</div>
      <div v-else-if="loading" class="text-sm text-muted-foreground">
        {{ t("common.loading") }}
      </div>

      <template v-else>
        <Table v-if="packageGrants.length">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("packageGrants.subject") }}</TableHead>
              <TableHead>{{ t("packageGrants.actions") }}</TableHead>
              <TableHead class="w-24" />
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="g in packageGrants" :key="g.subject">
              <TableCell class="font-mono text-xs">{{ g.subject }}</TableCell>
              <TableCell>
                <div class="flex flex-wrap gap-1">
                  <Badge v-for="a in g.actions" :key="a" variant="outline" class="text-xs">
                    {{ a }}
                  </Badge>
                </div>
              </TableCell>
              <TableCell>
                <!--
                  An ownership row is the projection's, not the editor's: the
                  server answers 409 for both edits. Saying so here rather than
                  letting the operator fill in a form and be refused.
                -->
                <span
                  v-if="g.from_ownership"
                  class="text-xs text-muted-foreground flex items-center gap-1"
                  :title="t('packageGrants.ownershipRowHint')"
                >
                  <Lock class="h-3 w-3" />
                  {{ t("packageGrants.fromOwnership") }}
                </span>
                <div v-else class="flex gap-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    class="h-7 text-xs"
                    :aria-label="t('packageGrants.editGrantFor', { subject: g.subject })"
                    @click="openAdd(g)"
                  >
                    {{ t("common.edit") }}
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    class="h-7 w-7 text-muted-foreground hover:text-destructive"
                    :aria-label="t('packageGrants.removeGrantFor', { subject: g.subject })"
                    @click="removeTarget = g"
                  >
                    <Trash2 class="h-3.5 w-3.5" />
                  </Button>
                </div>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <p v-else class="text-sm text-muted-foreground">
          {{ t("packageGrants.noGrantsWritten") }}
        </p>

        <Button size="sm" variant="outline" @click="openAdd()">
          <Plus class="h-4 w-4 mr-2" />
          {{ t("packageGrants.addGrant") }}
        </Button>

        <!--
          Read-only, and labelled as such. The version tier is edited from the
          CLI until it has a design of its own (§11 open question 3) — but
          hiding it would answer "who can reach this package" with half the
          rows.
        -->
        <div v-if="versionGrants.length" class="pt-2 space-y-2">
          <p class="font-mono text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {{ t("packageGrants.versionGrants") }}
          </p>
          <p class="text-xs text-muted-foreground">{{ t("packageGrants.versionGrantsCli") }}</p>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("packageGrants.version") }}</TableHead>
                <TableHead>{{ t("packageGrants.subject") }}</TableHead>
                <TableHead>{{ t("packageGrants.actions") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="g in versionGrants" :key="`${g.node_key}|${g.subject}`">
                <TableCell class="font-mono text-xs">
                  {{ g.node_key.slice(g.node_key.lastIndexOf("@") + 1) }}
                </TableCell>
                <TableCell class="font-mono text-xs">{{ g.subject }}</TableCell>
                <TableCell>
                  <div class="flex flex-wrap gap-1">
                    <Badge v-for="a in g.actions" :key="a" variant="outline" class="text-xs">
                      {{ a }}
                    </Badge>
                  </div>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </div>
      </template>
    </CardContent>

    <Dialog :open="showAdd" @update:open="showAdd = $event">
      <template #title>{{ t("packageGrants.addGrant") }}</template>
      <template #description>{{ t("packageGrants.dialogDescription") }}</template>
      <div class="space-y-4">
        <div class="space-y-1.5">
          <Label for="grant-subject">{{ t("packageGrants.subject") }}</Label>
          <Input id="grant-subject" v-model="form.subject" placeholder="group:oidc1:eng" />
          <p class="text-xs text-muted-foreground">{{ t("packageGrants.subjectHint") }}</p>
        </div>
        <div class="space-y-1.5">
          <Label for="grant-actions">{{ t("packageGrants.actions") }}</Label>
          <Input id="grant-actions" v-model="form.actions" placeholder="releases:read" />
          <p class="text-xs text-muted-foreground">{{ t("packageGrants.actionsHint") }}</p>
        </div>
        <Alert v-if="writeError" variant="destructive" class="text-sm py-2">
          <AlertCircle class="h-3.5 w-3.5" />
          <span class="pl-2">{{ writeError }}</span>
        </Alert>
        <div class="flex justify-end gap-2 pt-2">
          <Button variant="outline" :disabled="saving" @click="showAdd = false">
            {{ t("common.cancel") }}
          </Button>
          <Button :disabled="saving" @click="save">
            {{ saving ? t("common.saving") : t("common.save") }}
          </Button>
        </div>
      </div>
    </Dialog>

    <DestructiveConfirm
      :open="removeTarget !== null"
      :action="t('packageGrants.remove')"
      :count="1"
      :item-noun="t('packageGrants.grantNoun')"
      :scope="removeTarget?.subject ?? ''"
      :consequence="t('packageGrants.removeConsequence')"
      :loading="removing"
      :error="writeError"
      @confirm="confirmRemove"
      @update:open="
        (v: boolean) => {
          if (!v) {
            removeTarget = null;
            writeError = null;
          }
        }
      "
    />

    <Announcer :message="announcement" />
  </Card>
</template>
