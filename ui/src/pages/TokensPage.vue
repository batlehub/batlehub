<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, onUnmounted } from "vue";
import { Key, Plus, Trash2, Copy, Check, AlertCircle, Clock } from "@lucide/vue";
import { createToken, listTokens, revokeToken as revokeTokenApi } from "@/client/sdk.gen";
import type { TokenListItem, CreateTokenResponse } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { CopyButton } from "@/components/ui/copy-button";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Dialog } from "@/components/ui/dialog";
import { Alert } from "@/components/ui/alert";
import { Select } from "@/components/ui/select";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Announcer } from "@/components/ui/announcer";
import { useAuth } from "@/composables/useAuth";

import { roleLabel } from "@/lib/roles";
const { t } = useI18n();

const { identity } = useAuth();

function apiErrorMessage(err: unknown, fallback: string): string {
  if (err != null && typeof err === "object" && "message" in err) {
    return String((err as Record<string, unknown>)["message"]);
  }
  return fallback;
}

// ── Token list ────────────────────────────────────────────────────────────────

const { data: tokens, loading, error, reload } = useApi<TokenListItem[]>(() => listTokens(), []);

// ── Create dialog ─────────────────────────────────────────────────────────────

const showCreate = ref(false);
const creating = ref(false);
const createError = ref<string | null>(null);

const form = ref({
  name: "",
  expires_in_days: 30,
  role: identity.value?.role === "admin" ? "admin" : "user",
  groups: [] as string[],
});

/**
 * What this token may be given, which is exactly what its creator holds.
 *
 * A PAT's groups are a snapshot capped to the creator's own (RFC 0011-bis
 * §4.4), so there is nothing else to offer: the server answers `403` for
 * anything outside this list, and a free-text field would only be a way to earn
 * that 403 with a typo. Offering the held groups is also what makes the
 * provider-prefixed spelling — `k8s:system:serviceaccounts:digital`, not
 * `system:serviceaccounts:digital` — impossible to get wrong.
 */
const myGroups = computed(() => identity.value?.groups ?? []);

function toggleGroup(g: string) {
  const at = form.value.groups.indexOf(g);
  if (at === -1) form.value.groups.push(g);
  else form.value.groups.splice(at, 1);
}

const allGroupsSelected = computed(
  () => myGroups.value.length > 0 && form.value.groups.length === myGroups.value.length,
);

function toggleAllGroups() {
  form.value.groups = allGroupsSelected.value ? [] : [...myGroups.value];
}

const roleOptions = computed(() => {
  const r = identity.value?.role;
  const opts = [{ value: "user", label: t("common.user") }];
  if (r === "admin") opts.push({ value: "admin", label: t("common.admin") });
  return opts;
});

function openCreate() {
  // Defaults to no groups: the same starting point every token had before the
  // snapshot existed, and the narrow direction to be wrong in.
  form.value = {
    name: "",
    expires_in_days: 30,
    role: identity.value?.role === "admin" ? "admin" : "user",
    groups: [],
  };
  createError.value = null;
  newToken.value = null;
  showCreate.value = true;
}

async function submitCreate() {
  if (!form.value.name.trim()) {
    createError.value = t("tokensPage.tokenNameRequired");
    return;
  }
  creating.value = true;
  createError.value = null;
  try {
    const { data, error: apiError } = await createToken({
      body: {
        name: form.value.name.trim(),
        expires_in_days: form.value.expires_in_days,
        role: form.value.role,
        groups: form.value.groups,
      },
    });
    if (apiError) {
      createError.value = apiErrorMessage(apiError, t("tokensPage.createFailed"));
    } else {
      showCreate.value = false;
      newToken.value = (data as CreateTokenResponse | undefined)?.token ?? null;
      newTokenExpiry.value = (data as CreateTokenResponse | undefined)?.expires_at ?? null;
      newTokenGroups.value = (data as CreateTokenResponse | undefined)?.groups ?? [];
      reload();
      startAutoClear();
    }
  } finally {
    creating.value = false;
  }
}

// ── New token reveal ──────────────────────────────────────────────────────────

const newToken = ref<string | null>(null);
const newTokenExpiry = ref<string | null>(null);
/** Echoed back from the server, not from the form: what the token *actually* carries. */
const newTokenGroups = ref<string[]>([]);
let autoClearTimer: ReturnType<typeof setTimeout> | null = null;

function startAutoClear() {
  autoClearTimer = setTimeout(() => {
    newToken.value = null;
    newTokenExpiry.value = null;
    newTokenGroups.value = [];
  }, 60_000);
}

onUnmounted(() => {
  if (autoClearTimer) clearTimeout(autoClearTimer);
});

function dismissToken() {
  newToken.value = null;
  newTokenExpiry.value = null;
  newTokenGroups.value = [];
}

// ── Revoke ────────────────────────────────────────────────────────────────────

const revoking = ref<string | null>(null);
const revokeError = ref<string | null>(null);

/**
 * Revocation, confirmed.
 *
 * A click on the bin called `revokeTokenApi` directly: no dialogue, no scope,
 * no undo and no announcement. The first signal an operator got that they had
 * cut off the wrong pipeline was that pipeline failing, with nothing on screen
 * connecting the two.
 *
 * The button itself was fine — `aria-label` translated and naming the token.
 * It was the confirmation that was missing, not the name.
 */
const revokeTarget = ref<TokenListItem | null>(null);

/** What the console just did, for the live region. */
const announcement = ref("");

async function confirmRevoke() {
  const target = revokeTarget.value;
  if (!target) return;
  revoking.value = target.id;
  revokeError.value = null;
  try {
    const { error: apiError } = await revokeTokenApi({ path: { id: target.id } });
    if (apiError) {
      revokeError.value = apiErrorMessage(apiError, t("tokensPage.revokeFailed"));
      return;
    }
    announcement.value = t("tokensPage.tokenRevoked", { name: target.name });
    revokeTarget.value = null;
    reload();
  } finally {
    revoking.value = null;
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

function daysUntil(iso: string) {
  const diff = new Date(iso).getTime() - Date.now();
  return Math.ceil(diff / 86_400_000);
}

const lifetimePresets = [7, 30, 90];
</script>

<template>
  <div class="space-y-6">
    <PageHeader variant="display">
      <template #title>
        <Key class="h-5 w-5 text-primary" />
        {{ t("tokensPage.personalApiTokens") }}
      </template>
      <template #description>{{ t("tokensPage.createLongLivedTokens") }}</template>
      <template #actions>
        <Button class="shrink-0" @click="openCreate">
          <Plus class="h-4 w-4 mr-2" />
          {{ t("tokensPage.createToken") }}
        </Button>
      </template>
    </PageHeader>

    <!-- New token reveal alert -->
    <Alert v-if="newToken" variant="success" class="relative">
      <Check class="h-4 w-4" />
      <div class="pl-2 space-y-2">
        <p class="font-medium text-sm">{{ t("tokensPage.tokenCreatedCopyIt") }}</p>
        <div class="flex items-center gap-2">
          <code
            class="flex-1 block rounded-sm bg-muted px-3 py-2 text-xs font-mono break-all select-all"
          >
            {{ newToken }}
          </code>
          <CopyButton :text="newToken" variant="outline" size="icon" class="shrink-0 h-8 w-8">
            <template #default="{ copied }">
              <Check v-if="copied" class="h-3.5 w-3.5 text-primary" />
              <Copy v-else class="h-3.5 w-3.5" />
            </template>
          </CopyButton>
        </div>
        <p v-if="newTokenExpiry" class="text-xs text-muted-foreground">
          {{ t("common.expiresLabel") }} {{ formatDate(newTokenExpiry) }}
        </p>
        <!--
          Stated at the moment of creation because it is the only moment it can
          change: a snapshot is fixed for the life of the token, so a wrong one
          is re-created, never edited. Said even when empty — "sees nothing of
          your teams" is the sentence that saves an afternoon later.
        -->
        <p class="text-xs text-muted-foreground">
          <template v-if="newTokenGroups.length">
            {{ t("tokensPage.tokenSeesGroups") }}
            <span class="font-mono">{{ newTokenGroups.join(", ") }}</span>
          </template>
          <template v-else>{{ t("tokensPage.tokenSeesNoGroups") }}</template>
        </p>
        <Button variant="ghost" size="sm" class="h-7 text-xs" @click="dismissToken">{{
          t("tokensPage.dismissAutoClearsIn")
        }}</Button>
      </div>
    </Alert>

    <!-- Error -->
    <Alert v-if="revokeError" variant="destructive">
      <AlertCircle class="h-4 w-4" />
      <span class="pl-2">{{ revokeError }}</span>
    </Alert>

    <!-- Token table -->
    <Card>
      <CardHeader class="pb-3">
        <CardTitle class="text-base">{{ t("tokensPage.activeTokens") }}</CardTitle>
        <CardDescription>{{ t("tokensPage.tokensThatHaveNot") }}</CardDescription>
      </CardHeader>
      <CardContent>
        <AsyncState :loading="loading" :error="error" :empty="!tokens?.length">
          <template #empty>
            <div class="py-12 text-center space-y-2">
              <Key class="h-8 w-8 mx-auto text-muted-foreground/50" />
              <p class="text-sm text-muted-foreground">
                {{ t("tokensPage.noActiveTokensCreate") }}
              </p>
            </div>
          </template>

          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("common.name") }}</TableHead>
                <TableHead>{{ t("common.role") }}</TableHead>
                <TableHead>{{ t("tokensPage.seesColumn") }}</TableHead>
                <TableHead>{{ t("common.expires") }}</TableHead>
                <TableHead>{{ t("common.created") }}</TableHead>
                <TableHead class="w-16" />
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="tok in tokens" :key="tok.id">
                <TableCell class="font-medium">
                  {{ tok.name }}
                </TableCell>
                <TableCell>
                  <Badge :variant="tok.role === 'admin' ? 'default' : 'secondary'" class="text-xs">
                    {{ roleLabel(t, tok.role) }}
                  </Badge>
                </TableCell>
                <!--
                  A snapshot goes stale silently — nothing tells its owner the
                  token still carries a team they left. This column is the only
                  place they can see it, so it is a column and not a tooltip.
                -->
                <TableCell>
                  <div v-if="tok.groups?.length" class="flex flex-wrap gap-1">
                    <Badge v-for="g in tok.groups" :key="g" variant="outline" class="text-xs">
                      {{ g }}
                    </Badge>
                  </div>
                  <span v-else class="text-sm text-muted-foreground">{{
                    t("tokensPage.noGroupsShort")
                  }}</span>
                </TableCell>
                <TableCell>
                  <span
                    :class="
                      daysUntil(tok.expires_at) <= 7
                        ? 'text-destructive font-medium'
                        : 'text-muted-foreground'
                    "
                    class="text-sm flex items-center gap-1"
                  >
                    <Clock v-if="daysUntil(tok.expires_at) <= 7" class="h-3 w-3" />
                    {{ formatDate(tok.expires_at) }}
                    <span class="text-xs">({{ daysUntil(tok.expires_at) }}d)</span>
                  </span>
                </TableCell>
                <TableCell class="text-sm text-muted-foreground">
                  {{ formatDate(tok.created_at) }}
                </TableCell>
                <TableCell>
                  <Button
                    variant="ghost"
                    size="icon"
                    class="h-7 w-7 text-muted-foreground hover:text-destructive"
                    :disabled="revoking === tok.id"
                    :title="t('tokensPage.revokeTokenNamed', { name: tok.name })"
                    :aria-label="t('tokensPage.revokeTokenNamed', { name: tok.name })"
                    @click="revokeTarget = tok"
                  >
                    <Trash2 class="h-3.5 w-3.5" />
                  </Button>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </AsyncState>
      </CardContent>
    </Card>

    <!-- Create dialog -->
    <Dialog :open="showCreate" @update:open="showCreate = $event">
      <template #title>{{ t("tokensPage.createApiToken") }}</template>
      <template #description>{{ t("tokensPage.chooseANameRole") }}</template>
      <div class="space-y-4">
        <div class="space-y-1.5">
          <Label for="token-name">{{ t("common.name") }}</Label>
          <Input
            id="token-name"
            v-model="form.name"
            placeholder="e.g. CI pipeline"
            @keyup.enter="submitCreate"
          />
        </div>

        <div class="space-y-1.5">
          <Label for="token-role">{{ t("common.role") }}</Label>
          <Select
            id="token-role"
            v-model="form.role"
            :options="roleOptions"
            :placeholder="t('tokensPage.selectRole')"
          />
        </div>

        <!--
          Named for what it does — what the token can see — not for the column
          it writes. The choices are the caller's own groups and nothing else:
          the server refuses anything outside them (RFC 0011-bis §4.4), and a
          free-text field would only be a way to earn that refusal with a typo
          in a provider-prefixed id.
        -->
        <fieldset v-if="myGroups.length" class="space-y-2 border-0 p-0 m-0">
          <legend
            class="font-mono text-xs font-semibold uppercase tracking-wide text-muted-foreground leading-none"
          >
            {{ t("tokensPage.whatThisTokenCanSee") }}
          </legend>
          <p class="text-xs text-muted-foreground">
            {{ t("tokensPage.groupsAreASnapshot") }}
          </p>
          <div class="flex flex-wrap gap-2">
            <Button
              v-for="g in myGroups"
              :key="g"
              :variant="form.groups.includes(g) ? 'default' : 'outline'"
              size="sm"
              :aria-pressed="form.groups.includes(g)"
              @click="toggleGroup(g)"
            >
              {{ g }}
            </Button>
          </div>
          <Button variant="ghost" size="sm" class="h-7 text-xs" @click="toggleAllGroups">
            {{
              allGroupsSelected ? t("tokensPage.selectNoGroups") : t("tokensPage.selectAllGroups")
            }}
          </Button>
          <p v-if="!form.groups.length" class="text-xs text-muted-foreground">
            {{ t("tokensPage.tokenSeesNoGroups") }}
          </p>
        </fieldset>

        <fieldset class="space-y-2 border-0 p-0 m-0">
          <legend
            class="font-mono text-xs font-semibold uppercase tracking-wide text-muted-foreground leading-none"
          >
            {{ t("common.lifetime") }}
          </legend>
          <div class="flex gap-2">
            <Button
              v-for="days in lifetimePresets"
              :key="days"
              :variant="form.expires_in_days === days ? 'default' : 'outline'"
              size="sm"
              @click="form.expires_in_days = days"
            >
              {{ days }}d
            </Button>
          </div>
          <div class="flex items-center gap-2 text-sm text-muted-foreground">
            <span>{{ t("tokensPage.orCustom") }}</span>
            <Input
              type="number"
              min="1"
              max="90"
              :aria-label="t('tokensPage.customTokenExpiryIn')"
              :value="form.expires_in_days"
              class="w-24 h-8"
              @input="
                form.expires_in_days = Math.min(
                  90,
                  Math.max(1, +($event.target as HTMLInputElement).value),
                )
              "
            />
            <span>days</span>
          </div>
        </fieldset>

        <Alert v-if="createError" variant="destructive" class="text-sm py-2">
          <AlertCircle class="h-3.5 w-3.5" />
          <span class="pl-2">{{ createError }}</span>
        </Alert>

        <div class="flex justify-end gap-2 pt-2">
          <Button variant="outline" :disabled="creating" @click="showCreate = false">
            {{ t("common.cancel") }}
          </Button>
          <Button :disabled="creating" @click="submitCreate">
            {{ creating ? t("tokensPage.creating") : t("tokensPage.createToken") }}
          </Button>
        </div>
      </div>
    </Dialog>

    <!--
      Irreversible, so it takes the typed name — `confirmName` on a
      `reversible: false` is exactly the case the component's docstring
      describes: friction proportional to consequence.

      `consequence` rather than the stock `destructive.cannotUndo`, which
      reads "The artifacts and their metadata are removed permanently" and is
      about a delete. Revoking a token removes no artifact; what it removes is
      every caller's ability to authenticate, which is a different sentence.
    -->
    <DestructiveConfirm
      :open="revokeTarget !== null"
      :action="t('tokensPage.revoke')"
      :count="1"
      :item-noun="t('tokensPage.tokenNoun')"
      :scope="revokeTarget?.name ?? ''"
      :consequence="t('tokensPage.revokeConsequence')"
      :confirm-name="revokeTarget?.name ?? ''"
      :loading="revoking !== null"
      :error="revokeError"
      @confirm="confirmRevoke"
      @update:open="
        (v: boolean) => {
          if (!v) {
            revokeTarget = null;
            revokeError = null;
          }
        }
      "
    />

    <!--
      The outcome, announced. `Announcer` is mounted on six admin pages and on
      zero consumer surfaces; this is the first. A revocation that only renders
      is a change a screen-reader user makes and is never told happened.
    -->
    <Announcer :message="announcement" />
  </div>
</template>
