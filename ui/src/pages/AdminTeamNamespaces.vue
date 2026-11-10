<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { listNamespaces, claimNamespace, releaseNamespace } from "@/client/sdk.gen";
import type { TeamNamespaceDto } from "@/client/types.gen";
import { useAdminCrudList } from "@/composables/useAdminCrudList";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { NAMESPACES_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { Dialog } from "@/components/ui/dialog";
import { Combobox } from "@/components/ui/combobox";
import { useSubjectSuggestions } from "@/composables/useSuggestions";
import { toRef } from "vue";

const { t } = useI18n();

interface ClaimForm {
  prefix: string;
  group_id: string;
  claimed_by: string;
}

const {
  registryOptions,
  selectedRegistry,
  items: namespaces,
  itemsError: namespacesError,
  itemsLoading: namespacesLoading,
  reloadItems: reloadNamespaces,
  addDialogOpen: claimDialogOpen,
  addForm: claimForm,
  addLoading: claimLoading,
  addError: claimError,
  submitAdd: submitClaim,
  removeTarget: releaseTarget,
  removeLoading: releaseLoading,
  removeError: releaseError,
  confirmRemove: confirmRelease,
} = useAdminCrudList<TeamNamespaceDto, ClaimForm>({
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
  <div class="space-y-6">
    <SectionTabs :tabs="NAMESPACES_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminTeamNamespaces.teamNamespaces')"
      :description="t('adminTeamNamespaces.assignPackageNamePrefixesTo')"
    >
      <template #actions>
        <Button size="sm" :disabled="!selectedRegistry" @click="claimDialogOpen = true">{{
          t("adminTeamNamespaces.claimNamespace")
        }}</Button>
      </template>
    </PageHeader>

    <!-- Registry selector -->
    <div class="space-y-2 max-w-xs">
      <Label for="team-ns-registry">{{ t("common.registry") }}</Label>
      <Select
        id="team-ns-registry"
        v-model="selectedRegistry"
        :placeholder="t('adminTeamNamespaces.selectARegistry')"
        :options="registryOptions"
      />
    </div>

    <!-- Namespaces table -->
    <Card>
      <CardHeader>
        <div class="flex items-center justify-between">
          <CardTitle>
            {{ t("adminTeamNamespaces.namespaceClaims") }}
            <span v-if="selectedRegistry" class="font-mono text-muted-foreground text-sm ml-1"
              >({{ selectedRegistry }})</span
            >
          </CardTitle>
          <Button
            variant="outline"
            size="sm"
            :disabled="namespacesLoading"
            @click="reloadNamespaces"
          >
            {{ namespacesLoading ? t("common.loading") : t("common.refresh") }}
          </Button>
        </div>
      </CardHeader>
      <CardContent class="p-0">
        <!-- Empty only once loaded: an empty state shown while the request was
             in flight read as "none for this registry", and an operator added
             something that already existed. -->
        <AsyncState
          :loading="namespacesLoading"
          :error="namespacesError"
          :empty="!namespaces?.length"
          :empty-message="
            selectedRegistry
              ? t('adminTeamNamespaces.noNamespaceClaimsForThis')
              : t('adminTeamNamespaces.selectARegistryToView')
          "
          :on-retry="reloadNamespaces"
        >
          <Table :label="t('adminTeamNamespaces.namespaceClaims')">
            <TableHeader>
              <TableRow>
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
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="ns in namespaces" :key="ns.registry + ':' + ns.prefix">
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
              </TableRow>
            </TableBody>
          </Table>
        </AsyncState>
      </CardContent>
    </Card>
  </div>

  <!-- Claim namespace dialog -->
  <Dialog
    :open="claimDialogOpen"
    @update:open="
      (v) => {
        if (!v) {
          claimDialogOpen = false;
          claimError = null;
        }
      }
    "
  >
    <template #title>{{ t("adminTeamNamespaces.claimNamespace") }}</template>
    <template #description>
      <i18n-t keypath="adminTeamNamespaces.restrictPublishingIn" tag="span">
        <template #registry
          ><span class="font-mono">{{ selectedRegistry }}</span></template
        >
      </i18n-t>
    </template>
    <div class="space-y-4">
      <div class="space-y-3">
        <div class="space-y-2">
          <Label for="team-ns-prefix">
            {{ t("adminTeamNamespaces.prefix") }} <span class="text-destructive">*</span>
          </Label>
          <Input
            id="team-ns-prefix"
            v-model="claimForm.prefix"
            placeholder="e.g. frontend or frontend/ui"
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
            placeholder="e.g. oidc:frontend-team"
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
      </div>
      <p v-if="claimError" class="text-sm text-destructive">
        {{ claimError }}
      </p>
      <div class="flex justify-end gap-2">
        <Button
          variant="outline"
          size="sm"
          :disabled="claimLoading"
          @click="
            claimDialogOpen = false;
            claimError = null;
          "
        >
          {{ t("common.cancel") }}
        </Button>
        <Button
          size="sm"
          :disabled="claimLoading || !claimForm.prefix.trim() || !claimForm.group_id.trim()"
          @click="submitClaim"
        >
          {{
            claimLoading
              ? t("adminTeamNamespaces.claiming")
              : t("adminTeamNamespaces.claimNamespace")
          }}
        </Button>
      </div>
    </div>
  </Dialog>

  <!-- Release confirmation dialog -->
  <Dialog
    :open="releaseTarget !== null"
    @update:open="
      (v) => {
        if (!v) {
          releaseTarget = null;
          releaseError = null;
        }
      }
    "
  >
    <template #title>{{ t("adminTeamNamespaces.releaseNamespaceClaim") }}</template>
    <template #description>
      <i18n-t keypath="adminTeamNamespaces.releaseExplanation" tag="span">
        <template #prefix
          ><span class="font-mono">{{ releaseTarget?.prefix }}</span></template
        >
        <template #group
          ><span class="font-mono">{{ releaseTarget?.group_id }}</span></template
        >
      </i18n-t>
    </template>
    <div class="space-y-4">
      <p v-if="releaseError" class="text-sm text-destructive">
        {{ releaseError }}
      </p>
      <div class="flex justify-end gap-2">
        <Button
          variant="outline"
          size="sm"
          :disabled="releaseLoading"
          @click="
            releaseTarget = null;
            releaseError = null;
          "
        >
          {{ t("common.cancel") }}
        </Button>
        <Button variant="destructive" size="sm" :disabled="releaseLoading" @click="confirmRelease">
          {{
            releaseLoading
              ? t("adminTeamNamespaces.releasing")
              : t("adminTeamNamespaces.releaseClaim")
          }}
        </Button>
      </div>
    </div>
  </Dialog>
</template>
