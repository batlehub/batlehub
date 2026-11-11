<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";

import type { PendingReloadSnapshot } from "@/client/types.gen";
import ConfigWarningList from "@/components/admin/ConfigWarningList.vue";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { AsyncState } from "@/components/ui/async-state";
import { formatDate } from "@/lib/format";

/**
 * The staged reload an operator came to accept or refuse: its source, age,
 * diff and the warnings the candidate would raise. Apply only asks — the
 * confirmation and the call stay with the page, which owns the outcome.
 */
const props = defineProps<{
  pendingReload: PendingReloadSnapshot | null;
  loading: boolean;
  error: string | null;
  loadingApply: boolean;
  loadingDiscard: boolean;
}>();
const emit = defineEmits<{ retry: []; apply: []; discard: [] }>();

const { t } = useI18n();

/** A pending reload past its TTL cannot be applied; the button 404s. */
const pendingExpired = computed(() =>
  props.pendingReload?.expires_at ? new Date(props.pendingReload.expires_at) <= new Date() : false,
);

/** RFC 0037 §6.5's three lists, in the order an operator acts on them. */
const reloadLists = computed(() => {
  const d = props.pendingReload?.diff;
  return [
    { key: "adminConfigReload.reloadApplied", paths: d?.applied ?? [], restart: false },
    {
      key: "adminConfigReload.reloadPartiallyApplied",
      paths: d?.partially_applied ?? [],
      restart: false,
    },
    {
      key: "adminConfigReload.reloadRestartRequired",
      paths: d?.restart_required ?? [],
      restart: true,
    },
  ];
});

const expiresIn = computed(() => {
  if (!props.pendingReload) return "";
  const secs = Math.max(
    0,
    Math.round((new Date(props.pendingReload.expires_at).getTime() - Date.now()) / 1000),
  );
  if (secs > 60) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${secs}s`;
});
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t("adminConfigReload.pendingReload") }}</CardTitle>
    </CardHeader>
    <CardContent>
      <AsyncState
        :loading="loading && !pendingReload"
        :error="error"
        :empty="!pendingReload"
        :empty-message="t('adminConfigReload.noPendingReloadThe')"
        :title="t('adminConfigReload.pendingCouldNotLoad')"
        :on-retry="() => emit('retry')"
      >
        <div v-if="pendingReload" class="space-y-3">
          <div class="flex flex-wrap gap-x-4 gap-y-1 text-sm">
            <span
              ><strong>{{ t("adminConfigReload.sourceLabel") }}</strong>
              {{ pendingReload.source }}</span
            >
            <span
              ><strong>{{ t("adminConfigReload.createdLabel") }}</strong>
              {{ formatDate(pendingReload.created_at) }}</span
            >
            <span
              ><strong>{{ t("adminConfigReload.expiresIn") }}</strong> {{ expiresIn }}</span
            >
          </div>

          <!--
            What the *candidate* would warn about (RFC 0004-bis A4).

            `PendingReload` has held these since it was written — `apply()`
            promotes them to the live store — and `PendingReloadSnapshot`
            dropped them, so the one surface that exists to review a change
            before applying it could not show what the change would warn about.
            The page worked around it by remembering the warnings from the
            `validate` call that staged the reload, which is lost the moment
            anyone reloads the page or arrives from a file-watcher reload they
            did not stage themselves.
          -->
          <div v-if="pendingReload.warnings?.length" class="space-y-1">
            <p class="text-xs font-medium text-copper">
              {{
                t("adminConfigReload.candidateWarnsAbout", {
                  count: pendingReload.warnings.length,
                })
              }}
            </p>
            <ConfigWarningList :warnings="pendingReload.warnings" />
          </div>
          <div class="flex gap-2 flex-wrap">
            <Badge v-for="r in pendingReload.diff.added_registries" :key="r" variant="copper"
              >+{{ r }}</Badge
            >
            <Badge v-for="r in pendingReload.diff.removed_registries" :key="r" variant="copper"
              >-{{ r }}</Badge
            >
            <!--
              `fields` and `access_config_changed` are on the wire and were both
              discarded: a changed registry rendered as a bare `~npm`, and a
              change to RBAC alone rendered *nothing at all* — an empty badge
              row that invites a blind apply. This is the operator's decision
              surface; it has to say what it is asking them to accept.
            -->
            <Badge v-for="r in pendingReload.diff.changed_registries" :key="r.name" variant="copper"
              >~{{ r.name }}<span v-if="r.fields?.length"> ({{ r.fields.join(", ") }})</span></Badge
            >
            <Badge v-if="pendingReload.diff.access_config_changed" variant="copper">{{
              t("adminConfigReload.accessConfigChanged")
            }}</Badge>
            <Badge v-if="pendingReload.diff.limits_changed" variant="copper">{{
              t("adminConfigReload.limitsChanged")
            }}</Badge>
          </div>
          <!--
            Which keys this reload actually applies (RFC 0037 §6.5). Before
            these lists, a change to `database.url` staged, applied, and showed
            an empty diff: the one change that does nothing until a restart was
            the one the page could not show.
          -->
          <dl class="space-y-1 text-xs">
            <template v-for="row in reloadLists" :key="row.key">
              <div v-if="row.paths.length">
                <dt :class="row.restart ? 'font-medium text-copper' : 'font-medium'">
                  {{ t(row.key) }}
                </dt>
                <dd class="flex flex-wrap gap-1">
                  <code v-for="p in row.paths" :key="p" class="font-mono break-all">{{ p }}</code>
                  <span v-if="row.restart && pendingReload.diff.restart_reason">
                    — {{ pendingReload.diff.restart_reason }}</span
                  >
                </dd>
              </div>
            </template>
          </dl>
          <!-- The view's one filled action: Apply. -->
          <div class="flex gap-2">
            <Button size="sm" :disabled="loadingApply || pendingExpired" @click="emit('apply')">
              {{ loadingApply ? t("adminConfigReload.applying") : t("adminConfigReload.apply") }}
            </Button>
            <Button size="sm" variant="outline" :disabled="loadingDiscard" @click="emit('discard')">
              {{
                loadingDiscard ? t("adminConfigReload.discarding") : t("adminConfigReload.discard")
              }}
            </Button>
          </div>
        </div>
      </AsyncState>
    </CardContent>
  </Card>
</template>
