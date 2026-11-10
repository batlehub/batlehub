<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { listNamespaces, claimNamespace, releaseNamespace } from "@/client/sdk.gen";
import type { TeamNamespaceDto } from "@/client/types.gen";
import { useAdminCrudList } from "@/composables/useAdminCrudList";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { TableHead, TableCell } from "@/components/ui/table";
import RegistryScopedList from "@/components/admin/RegistryScopedList.vue";
import { Combobox } from "@/components/ui/combobox";
import { useSubjectSuggestions } from "@/composables/useSuggestions";
import { toRef } from "vue";

const { t } = useI18n();

interface ClaimForm {
  prefix: string;
  group_id: string;
  claimed_by: string;
}

const crud = useAdminCrudList<TeamNamespaceDto, ClaimForm>({
  listFn: (registry) =>
    listNamespaces({ path: { registry } }) as Promise<{ data?: unknown; error?: unknown }>,
  addFn: (registry, form) =>
    claimNamespace({
      path: { registry },
      body: {
        prefix: form.prefix.trim(),
        group_id: form.group_id.trim(),
        claimed_by: form.claimed_by.trim() || undefined,
      },
    }) as Promise<{ data?: unknown; error?: unknown }>,
  // The prefix may contain slashes — passed verbatim; the backend route uses {prefix:.*}
  removeFn: (registry, item) =>
    releaseNamespace({
      path: { registry, prefix: item.prefix },
    }) as Promise<{ data?: unknown; error?: unknown }>,
  initialAddForm: () => ({ prefix: "", group_id: "", claimed_by: "" }),
  canSubmitAdd: (form) => !!form.prefix.trim() && !!form.group_id.trim(),
});

const {
  addForm: claimForm,
  removeTarget: releaseTarget,
  removeLoading: releaseLoading,
  removeError: releaseError,
  confirmRemove: confirmRelease,
} = crud;

/* A8: identities this instance has seen. A claim may still name someone it
   has not — a namespace is often claimed before its owner's first pull.

   A getter ref, not `toRef(claimForm.value, …)`: `useAdminCrudList.submitAdd`
   does `addForm.value = initialAddForm()` after a successful claim, so a
   property ref bound to the object held at setup time points at an orphaned
   object from the second claim onwards and the field suggests nothing for the
   rest of the session. A getter re-reads `claimForm.value` on every access. */
const claimedBySuggestions = useSubjectSuggestions(toRef(() => claimForm.value.claimed_by));
</script>

<template>
  <RegistryScopedList
    :crud="crud"
    id-prefix="team-ns"
    :title="t('adminTeamNamespaces.teamNamespaces')"
    :description="t('adminTeamNamespaces.assignPackageNamePrefixesTo')"
    :add-label="t('adminTeamNamespaces.claimNamespace')"
    :adding-label="t('adminTeamNamespaces.claiming')"
    :dialog-title="t('adminTeamNamespaces.claimNamespace')"
    dialog-description-key="adminTeamNamespaces.restrictPublishingIn"
    :select-placeholder="t('adminTeamNamespaces.selectARegistry')"
    :list-title="t('adminTeamNamespaces.namespaceClaims')"
    :empty-message="t('adminTeamNamespaces.noNamespaceClaimsForThis')"
    :no-registry-message="t('adminTeamNamespaces.selectARegistryToView')"
    :row-key="(ns) => ns.registry + ':' + ns.prefix"
  >
    <template #head>
      <TableHead>{{ t("common.prefix") }}</TableHead>
      <TableHead>{{ t("common.group") }}</TableHead>
      <TableHead>{{ t("adminTeamNamespaces.claimedBy") }}</TableHead>
      <!-- RFC 0004-bis A6. `count_packages_in_namespace` had been on
           the port since it was written, with the delete confirmation
           as its only caller — so this list showed a row per claim with
           no way to tell a namespace holding four hundred packages from
           an abandoned one, which is the question an operator opens
           this page with. -->
      <TableHead class="text-right">{{ t("common.packages") }}</TableHead>
      <TableHead class="text-right">{{ t("common.actions") }}</TableHead>
    </template>
    <template #row="{ item: ns }">
      <TableCell class="font-mono text-sm">
        {{ ns.prefix }}
      </TableCell>
      <TableCell>
        <Badge variant="secondary" class="text-xs font-mono">
          {{ ns.group_id }}
        </Badge>
      </TableCell>
      <TableCell class="text-sm text-muted-foreground">
        {{ ns.claimed_by ?? "—" }}
      </TableCell>
      <TableCell class="text-right font-mono text-sm tabular-nums">
        <!-- Zero is the answer that matters here, so it is rendered
             rather than blanked. -->
        <span :class="ns.package_count === 0 ? 'text-muted-foreground' : ''">{{
          ns.package_count
        }}</span>
      </TableCell>
      <TableCell class="text-right">
        <Button variant="outline" size="sm" @click="releaseTarget = ns">
          {{ t("common.release") }}
        </Button>
      </TableCell>
    </template>
    <template #form>
      <div class="space-y-2">
        <Label for="team-ns-prefix">
          {{ t("adminTeamNamespaces.prefix") }} <span class="text-destructive">*</span>
        </Label>
        <Input
          id="team-ns-prefix"
          v-model="claimForm.prefix"
          placeholder="frontend/ui"
          class="font-mono"
        />
        <p class="text-xs text-muted-foreground">
          <i18n-t keypath="adminTeamNamespaces.packagesStartingWith" tag="span">
            <template #prefix><span class="font-mono">prefix/</span></template>
          </i18n-t>
        </p>
      </div>
      <div class="space-y-2">
        <Label for="team-ns-group-id">
          {{ t("adminTeamNamespaces.groupId") }} <span class="text-destructive">*</span>
        </Label>
        <Input
          id="team-ns-group-id"
          v-model="claimForm.group_id"
          placeholder="oidc:frontend-team"
          class="font-mono"
        />
        <p class="text-xs text-muted-foreground">
          {{ t("adminTeamNamespaces.mustMatchTheGroup") }}
        </p>
      </div>
      <div class="space-y-2">
        <Label for="team-ns-claimed-by">{{ t("adminTeamNamespaces.claimedBy") }}</Label>
        <!-- A8: identities this instance has actually seen. Typing a subject
             it has not seen is still allowed — a namespace can be claimed for
             someone before their first pull. -->
        <Combobox
          id="team-ns-claimed-by"
          v-model="claimForm.claimed_by"
          :options="claimedBySuggestions.options.value"
          :loading="claimedBySuggestions.loading.value"
          :placeholder="t('adminTeamNamespaces.optionalYourUserId')"
        />
      </div>
    </template>
  </RegistryScopedList>

  <!-- Release confirmation dialog -->
  <DestructiveConfirm
    :open="releaseTarget !== null"
    :action="t('adminTeamNamespaces.release')"
    :count="1"
    :item-noun="t('adminTeamNamespaces.claimNoun')"
    :scope="releaseTarget?.prefix ?? ''"
    :consequence="
      t('adminTeamNamespaces.releaseExplanation', {
        prefix: releaseTarget?.prefix,
        group: releaseTarget?.group_id,
      })
    "
    reversible
    :loading="releaseLoading"
    :error="releaseError"
    @update:open="
      (v) => {
        if (!v) {
          releaseTarget = null;
          releaseError = null;
        }
      }
    "
    @confirm="confirmRelease"
  />
</template>
