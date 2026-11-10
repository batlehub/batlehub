<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import {
  listSubscriptions,
  updateSubscription,
  deleteSubscription,
  testSubscription as testSubscriptionApi,
} from "@/client/sdk.gen";
import type { NotificationSubscription } from "@/client/types.gen";
import { useApi, extractMessage } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { eventBadgeVariant } from "@/lib/badge-variants";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { NOTIFICATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Announcer } from "@/components/ui/announcer";
import { Card, CardContent } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import SubscriptionFormDialog from "@/components/admin/SubscriptionFormDialog.vue";

const { t } = useI18n();

const { token } = useAuth();

// ── State ─────────────────────────────────────────────────────────────────────

const {
  data: subscriptions,
  error: subsError,
  loading: subsLoading,
  reload: reloadSubs,
} = useApi<NotificationSubscription[]>(() => listSubscriptions(), [token]);

// ── Create / edit dialog ──────────────────────────────────────────────────────

const dialogOpen = ref(false);
/** The subscription being edited, or `null` when the dialog creates one. */
const editing = ref<NotificationSubscription | null>(null);

function openCreate() {
  editing.value = null;
  dialogOpen.value = true;
}

function openEdit(sub: NotificationSubscription) {
  editing.value = sub;
  dialogOpen.value = true;
}

// ── Delete ────────────────────────────────────────────────────────────────────

const deleteTarget = ref<string | null>(null);
const deleteChannel = computed(
  () => subscriptions.value?.find((s) => s.id === deleteTarget.value)?.channel_name ?? "",
);
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
    <SectionTabs :tabs="NOTIFICATIONS_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminNav.subscriptions')"
      :description="t('adminNotifications.manageOutboundNotificationSubscriptionsAnd')"
    >
      <template #actions>
        <Button size="sm" @click="openCreate">{{ t("adminNotifications.newSubscription") }}</Button>
      </template>
    </PageHeader>

    <div class="space-y-4">
      <AsyncState :loading="subsLoading && !subscriptions" :error="subsError">
        <Card>
          <CardContent class="p-0">
            <Table :label="t('adminNav.subscriptions')">
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
                      <Button variant="outline" size="sm" @click="deleteTarget = sub.id">{{
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

      <p v-if="toggleError" role="alert" class="text-sm text-destructive">{{ toggleError }}</p>
      <Announcer :message="testMsg ?? ''" :assertive="testFailed" />
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
  <SubscriptionFormDialog v-model:open="dialogOpen" :subscription="editing" @saved="reloadSubs" />

  <!-- Delete confirmation -->
  <DestructiveConfirm
    :open="deleteTarget !== null"
    :action="t('common.delete')"
    :count="1"
    :item-noun="t('adminNotifications.subscriptionNoun')"
    :scope="deleteChannel"
    :consequence="t('adminNotifications.deleteConsequence')"
    :confirm-name="deleteChannel"
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
