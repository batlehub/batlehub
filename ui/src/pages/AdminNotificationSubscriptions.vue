<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, toRef } from "vue";
import {
  listSubscriptions,
  listNotificationChannels,
  createSubscription,
  updateSubscription,
  deleteSubscription,
  testSubscription as testSubscriptionApi,
} from "@/client/sdk.gen";
import type {
  NotificationSubscription,
  NotificationEventType,
  ChannelInfo,
} from "@/client/types.gen";
import { useApi, extractMessage } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { eventBadgeVariant } from "@/lib/badge-variants";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { NOTIFICATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { Dialog } from "@/components/ui/dialog";
import ConfirmDialog from "@/components/ConfirmDialog.vue";
import { Select } from "@/components/ui/select";
import { Combobox } from "@/components/ui/combobox";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import { usePackageNameSuggestions } from "@/composables/useSuggestions";

const { t } = useI18n();

/** RFC 0004-bis §6.2: the registry set is closed, small and already fetched. */
const { options: registryOptions } = useRegistryOptions();

/**
 * The registry set plus an explicit "all registries" entry.
 *
 * The field is optional by contract, so blank has to be a *choosable* value —
 * a `Select` that cannot express it would silently narrow every use that
 * relied on leaving it empty (RFC 0004-bis §6.2).
 */
const registryOptionsWithAll = computed(() => [
  { value: "", label: t("adminPackages.allRegistries") },
  ...registryOptions.value,
]);

const { token } = useAuth();

// ── State ─────────────────────────────────────────────────────────────────────

const {
  data: subscriptions,
  error: subsError,
  loading: subsLoading,
  reload: reloadSubs,
} = useApi<NotificationSubscription[]>(() => listSubscriptions(), [token]);

// Only the data: the channel list is supplementary reference beside a form
// field, so a spinner or an error block for it would compete with the form it
// exists to support. An unreadable list degrades to the "none configured"
// hint, which is the same advice either way.
const { data: channelsResp } = useApi<ChannelInfo[]>(() => listNotificationChannels(), [token]);

/**
 * The channel list is a *dependency of this page's form*, not a peer view: the
 * `datalist` on the required Channel field is populated from it (RFC 0004 R8).
 * That is why channels stayed on this route — a separate route would have had
 * to fetch it anyway.
 */
const channels = computed(() => channelsResp.value ?? []);
const channelOptions = computed(() => channels.value.map((ch) => ({ value: ch.name })));

// ── Create / edit dialog ──────────────────────────────────────────────────────

const ALL_EVENT_TYPES: NotificationEventType[] = [
  "package_published",
  "package_yanked",
  "package_unyanked",
  "package_deleted",
  // RFC 0014 §4.5 — the upstream audit's three. `upstream_unreachable` is
  // registry-scoped (its package is `*`), so a subscription with a package
  // filter never sees it.
  "package_disappeared_upstream",
  "package_reappeared_upstream",
  "upstream_unreachable",
  // RFC 0018 §4.2 — the verdict pipeline's two. Without them the chip row this
  // array renders offered no way to subscribe to a verdict flip or a released
  // hold at all, and `unknownEventTypes` flagged an existing subscription for
  // either one as a type this console has no chip for.
  "verdict_changed",
  "artifact_released",
];

const dialogOpen = ref(false);
const editingId = ref<string | null>(null);
const form = ref({
  registry: "",
  package_name: "",
  event_types: ["package_published"] as NotificationEventType[],
  channel_name: "",
  enabled: true,
});

/* Suggested from `name_contains`, still accepting a package this instance has
   never cached — subscribing *before* the first pull is a real workflow
   (RFC 0004-bis §6.2). */
/* Getter refs, not `toRef(form.value, …)`. The property form binds to the
   object `form` held *at setup time*, and both `openCreate` and `openEdit`
   replace `form.value` outright — so by the time the dialog is ever visible the
   refs point at an orphaned object that nothing writes to, and the field
   suggests nothing for the rest of the session. A getter re-reads `form.value`
   on every access, so it survives the replacement. */
const packageSuggestions = usePackageNameSuggestions(
  toRef(() => form.value.package_name),
  toRef(() => form.value.registry),
);
/**
 * The channel field is required but was never validated — `submitForm` checked
 * only that it was non-empty, so a typo saved cleanly and the subscription
 * silently never dispatched. A warning rather than a block: channels come from
 * `config.toml`, and an operator may legitimately be creating a subscription
 * for a channel they are about to add.
 */
const channelWarning = computed(
  () =>
    form.value.channel_name.trim().length > 0 &&
    channels.value.length > 0 &&
    !channels.value.some((c) => c.name === form.value.channel_name.trim()),
);

const formLoading = ref(false);
const formError = ref<string | null>(null);

function openCreate() {
  editingId.value = null;
  form.value = {
    registry: "",
    package_name: "",
    event_types: ["package_published"],
    channel_name: "",
    enabled: true,
  };
  formError.value = null;
  dialogOpen.value = true;
}

function openEdit(sub: NotificationSubscription) {
  editingId.value = sub.id;
  form.value = {
    registry: sub.registry ?? "",
    package_name: sub.package_name ?? "",
    event_types: [...sub.event_types],
    channel_name: sub.channel_name,
    enabled: sub.enabled,
  };
  formError.value = null;
  dialogOpen.value = true;
}

/**
 * Event types on the subscription that this console has no chip for.
 *
 * `openEdit` copies `sub.event_types` verbatim and the chip row only renders
 * `ALL_EVENT_TYPES`, so a type the server knows and the console does not was
 * invisible in the form and re-saved on submit — an operator editing a
 * subscription silently kept a rule they could not see, and could not remove.
 * A server that grows an event type before the console does is the *expected*
 * order of deployment, so this is a normal state, not a corruption.
 */
const unknownEventTypes = computed(() =>
  form.value.event_types.filter((et) => !(ALL_EVENT_TYPES as string[]).includes(et as string)),
);

function dropUnknownEventType(et: string) {
  form.value.event_types = form.value.event_types.filter((e) => (e as string) !== et);
}

function toggleEventType(et: NotificationEventType) {
  const idx = form.value.event_types.indexOf(et);
  if (idx === -1) form.value.event_types.push(et);
  else form.value.event_types.splice(idx, 1);
}

async function submitForm() {
  if (!form.value.channel_name.trim() || form.value.event_types.length === 0) return;
  formLoading.value = true;
  formError.value = null;
  const body = {
    registry: form.value.registry.trim() || null,
    package_name: form.value.package_name.trim() || null,
    event_types: form.value.event_types,
    channel_name: form.value.channel_name.trim(),
    enabled: form.value.enabled,
  };
  try {
    if (editingId.value) {
      const result = await updateSubscription({
        path: { id: editingId.value },
        body: { ...body, enabled: form.value.enabled },
      });
      if (result.error) {
        formError.value = extractMessage(result.error);
        return;
      }
    } else {
      const result = await createSubscription({ body });
      if (result.error) {
        formError.value = extractMessage(result.error);
        return;
      }
    }
    dialogOpen.value = false;
    reloadSubs();
  } catch (e) {
    formError.value = extractMessage(e);
  } finally {
    formLoading.value = false;
  }
}

// ── Delete ────────────────────────────────────────────────────────────────────

const deleteTarget = ref<string | null>(null);
const deleteLoading = ref(false);
const deleteError = ref<string | null>(null);

async function confirmDelete() {
  if (!deleteTarget.value) return;
  deleteLoading.value = true;
  deleteError.value = null;
  try {
    const result = await deleteSubscription({ path: { id: deleteTarget.value } });
    if (result.error) {
      deleteError.value = extractMessage(result.error);
      return;
    }
    deleteTarget.value = null;
    reloadSubs();
  } catch (e) {
    deleteError.value = extractMessage(e);
  } finally {
    deleteLoading.value = false;
  }
}

// ── Toggle enabled ────────────────────────────────────────────────────────────

const toggleError = ref<string | null>(null);

async function toggleEnabled(sub: NotificationSubscription) {
  toggleError.value = null;
  const result = await updateSubscription({
    path: { id: sub.id },
    body: {
      registry: sub.registry ?? null,
      package_name: sub.package_name ?? null,
      event_types: sub.event_types,
      channel_name: sub.channel_name,
      enabled: !sub.enabled,
    },
  });
  if (result.error) {
    toggleError.value = extractMessage(result.error);
  }
  reloadSubs();
}

// ── Test dispatch ─────────────────────────────────────────────────────────────

const testLoading = ref<string | null>(null);
const testMsg = ref<string | null>(null);
/* Carried separately rather than recovered from the message. The template used
   to decide the colour with `testMsg.startsWith("Test failed")`, which is a
   translated sentence being parsed for its own meaning — correct in English and
   silently green in French. */
const testFailed = ref(false);

async function testSubscription(id: string) {
  testLoading.value = id;
  testMsg.value = null;
  try {
    const result = await testSubscriptionApi({ path: { id } });
    testFailed.value = !!result.error;
    testMsg.value = result.error
      ? t("adminNotifications.testFailed", { error: extractMessage(result.error) })
      : t("adminNotifications.testSent");
  } catch (e) {
    testFailed.value = true;
    testMsg.value = t("adminNotifications.testFailed", { error: extractMessage(e) });
  } finally {
    testLoading.value = null;
  }
}
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      variant="display"
      :title="t('adminNotifications.webhooksNotifications')"
      :description="t('adminNotifications.manageOutboundNotificationSubscriptionsAnd')"
    />

    <SectionTabs :tabs="NOTIFICATIONS_TABS" />
    <PageHeader variant="display" :title="t('adminNav.subscriptions')">
      <template #actions>
        <Button size="sm" @click="openCreate">{{ t("adminNotifications.newSubscription") }}</Button>
      </template>
    </PageHeader>

    <div class="space-y-4">
      <AsyncState :loading="subsLoading && !subscriptions" :error="subsError">
        <Card>
          <CardContent class="p-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{{ t("common.registry") }}</TableHead>
                  <TableHead>{{ t("common.package") }}</TableHead>
                  <TableHead>{{ t("common.events") }}</TableHead>
                  <TableHead>{{ t("common.channel") }}</TableHead>
                  <TableHead>{{ t("common.enabled") }}</TableHead>
                  <TableHead class="text-right">{{ t("common.actions") }}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                <TableRow v-for="sub in subscriptions" :key="sub.id">
                  <TableCell class="font-mono text-sm">{{ sub.registry ?? "*" }}</TableCell>
                  <TableCell class="font-mono text-sm">{{ sub.package_name ?? "*" }}</TableCell>
                  <TableCell>
                    <div class="flex flex-wrap gap-1">
                      <Badge
                        v-for="et in sub.event_types"
                        :key="et"
                        :variant="eventBadgeVariant(et)"
                        class="text-xs"
                      >
                        {{ et.replace("package_", "") }}
                      </Badge>
                    </div>
                  </TableCell>
                  <TableCell class="font-mono text-sm">{{ sub.channel_name }}</TableCell>
                  <TableCell>
                    <Switch
                      :model-value="sub.enabled"
                      :aria-label="
                        t('adminNotifications.toggleSubscription', { channel: sub.channel_name })
                      "
                      @update:model-value="toggleEnabled(sub)"
                    />
                  </TableCell>
                  <TableCell class="text-right">
                    <div class="flex justify-end gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        :disabled="testLoading === sub.id"
                        @click="testSubscription(sub.id)"
                      >
                        {{ testLoading === sub.id ? "…" : t("adminNotifications.test") }}
                      </Button>
                      <Button variant="outline" size="sm" @click="openEdit(sub)">{{
                        t("common.edit")
                      }}</Button>
                      <Button variant="destructive" size="sm" @click="deleteTarget = sub.id">{{
                        t("common.delete")
                      }}</Button>
                    </div>
                  </TableCell>
                </TableRow>
              </TableBody>
            </Table>
            <p
              v-if="!subscriptions || subscriptions.length === 0"
              class="p-6 text-sm text-muted-foreground text-center"
            >
              {{ t("adminNotifications.noSubscriptionsConfigured") }}
            </p>
          </CardContent>
        </Card>
      </AsyncState>

      <p v-if="toggleError" class="text-sm text-destructive">{{ toggleError }}</p>
      <p
        v-if="testMsg"
        class="text-sm"
        :class="testFailed ? 'text-destructive' : 'text-foreground'"
      >
        {{ testMsg }}
      </p>
    </div>
  </div>

  <!-- Create/Edit Subscription dialog -->
  <Dialog
    :open="dialogOpen"
    @update:open="
      (v) => {
        if (!v) dialogOpen = false;
      }
    "
  >
    <template #title>{{
      editingId ? t("adminNotifications.editSubscription") : t("adminNotifications.newSubscription")
    }}</template>
    <div class="space-y-4">
      <div class="space-y-3">
        <div class="space-y-1.5">
          <Label for="notif-registry"
            >{{ t("common.registry") }}
            <span class="text-muted-foreground text-xs">{{
              t("adminNotifications.leaveBlankForAll")
            }}</span></Label
          >
          <!-- Optional by contract ("leave blank for all registries"), so the
               blank is a real option rather than an absence — a `Select` that
               cannot express it would silently narrow every subscription that
               relied on it. -->
          <Select
            id="notif-registry"
            v-model="form.registry"
            :options="registryOptionsWithAll"
            :placeholder="t('adminPackages.allRegistries')"
            class="font-mono"
          />
        </div>
        <div class="space-y-1.5">
          <Label for="notif-package-name"
            >{{ t("adminNotifications.packageName") }}
            <span class="text-muted-foreground text-xs">{{
              t("adminNotifications.leaveBlankForAll")
            }}</span></Label
          >
          <Combobox
            id="notif-package-name"
            v-model="form.package_name"
            :options="packageSuggestions.options.value"
            :loading="packageSuggestions.loading.value"
            placeholder="e.g. serde"
            class="font-mono"
          />
        </div>
        <fieldset class="space-y-1.5 border-0 p-0 m-0">
          <legend
            class="font-mono text-xs font-semibold uppercase tracking-wide text-muted-foreground leading-none"
          >
            {{ t("adminNotifications.eventTypes") }} <span class="text-destructive">*</span>
          </legend>
          <div class="flex flex-wrap gap-2">
            <button
              v-for="et in ALL_EVENT_TYPES"
              :key="et"
              type="button"
              class="px-2 py-1 rounded-sm border text-xs font-mono transition-colors"
              :class="
                form.event_types.includes(et)
                  ? 'bg-foreground text-background border-foreground'
                  : 'border-muted-foreground text-muted-foreground'
              "
              @click="toggleEventType(et)"
            >
              {{ et.replace("package_", "") }}
            </button>
          </div>
          <!-- Named, not silently carried. -->
          <p v-if="unknownEventTypes.length" class="text-xs text-copper">
            {{ t("adminNotifications.unknownEventTypes") }}
          </p>
          <div v-if="unknownEventTypes.length" class="flex flex-wrap gap-1.5">
            <button
              v-for="et in unknownEventTypes"
              :key="et"
              type="button"
              class="rounded-sm border border-copper/50 px-2 py-1 font-mono text-xs text-copper"
              :aria-label="t('adminNotifications.dropEventType', { type: et })"
              @click="dropUnknownEventType(et)"
            >
              {{ et }} ✕
            </button>
          </div>
        </fieldset>
        <div class="space-y-1.5">
          <Label for="notif-channel"
            >{{ t("common.channel") }} <span class="text-destructive">*</span></Label
          >
          <!-- Was a `<datalist>`, for the same reasons as the registry field
               above: no listbox for assistive tech, no "nothing matches", and
               no way to style it to the surface it sits on. -->
          <Combobox
            id="notif-channel"
            v-model="form.channel_name"
            :options="channelOptions"
            placeholder="e.g. my-slack"
            class="font-mono"
          />
          <!--
            Inline, not behind a tab. A native `datalist` shows nothing until
            the operator types, so "does the channel I am about to type exist"
            was answerable only by leaving the form. This is the whole of what
            the Channels tab rendered — one sentence and a row of badges, with
            no actions — moved to where its question is actually asked.
          -->
          <div v-if="channels.length" class="flex flex-wrap items-center gap-1.5 pt-1">
            <span class="text-xs text-muted-foreground">{{
              t("adminNotifications.configuredChannels")
            }}</span>
            <button
              v-for="ch in channels"
              :key="ch.name"
              type="button"
              class="rounded-sm border border-border px-2 py-0.5 font-mono text-xs text-muted-foreground transition-colors hover:border-primary/40 hover:text-primary focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              @click="form.channel_name = ch.name"
            >
              {{ ch.name }}
            </button>
          </div>
          <p v-else class="pt-1 text-xs text-muted-foreground">
            <i18n-t keypath="adminNotifications.noChannelsConfigured" tag="span">
              <template #block
                ><code class="font-mono text-xs">[[notifications.channels]]</code></template
              >
            </i18n-t>
          </p>
          <p v-if="channelWarning" class="pt-1 text-xs text-copper">
            {{ t("adminNotifications.unknownChannel", { name: form.channel_name }) }}
          </p>
        </div>
        <div class="flex items-center gap-2">
          <Switch id="notif-enabled" v-model="form.enabled" />
          <Label for="notif-enabled">{{ t("common.enabled") }}</Label>
        </div>
      </div>

      <p v-if="formError" class="text-sm text-destructive">{{ formError }}</p>
      <div class="flex justify-end gap-2">
        <Button variant="outline" size="sm" :disabled="formLoading" @click="dialogOpen = false">{{
          t("common.cancel")
        }}</Button>
        <Button
          size="sm"
          :disabled="formLoading || !form.channel_name.trim() || form.event_types.length === 0"
          @click="submitForm"
        >
          {{
            formLoading
              ? t("packageVisibility.saving")
              : editingId
                ? t("adminNotifications.update")
                : t("adminNotifications.create")
          }}
        </Button>
      </div>
    </div>
  </Dialog>

  <!-- Delete confirmation -->
  <ConfirmDialog
    :open="deleteTarget !== null"
    :title="t('adminNotifications.deleteSubscription')"
    :description="t('adminNotifications.thisActionCannotBeUndone')"
    :confirm-label="t('common.delete')"
    :loading-label="t('common.deleting')"
    destructive
    :loading="deleteLoading"
    :error="deleteError"
    @update:open="
      (v) => {
        if (!v) {
          deleteTarget = null;
          deleteError = null;
        }
      }
    "
    @confirm="confirmDelete"
  />
</template>
