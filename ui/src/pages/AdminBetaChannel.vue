<script setup lang="ts">
import { computed, toRef } from "vue";
import { useI18n } from "vue-i18n";
import { listBetaMembers, addBetaMember, removeBetaMember } from "@/client/sdk.gen";
import type { BetaChannelMemberDto } from "@/client/types.gen";
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

const { t } = useI18n();

interface AddMemberForm {
  principal_type: string;
  principal_id: string;
  granted_by: string;
}

const {
  registryOptions,
  selectedRegistry,
  items: members,
  itemsError: membersError,
  itemsLoading: membersLoading,
  reloadItems: reloadMembers,
  addDialogOpen,
  addForm,
  addLoading,
  addError,
  submitAdd,
  removeTarget,
  removeLoading,
  removeError,
  confirmRemove,
} = useAdminCrudList<BetaChannelMemberDto, AddMemberForm>({
  listFn: (registry) =>
    listBetaMembers({ path: { registry } }) as Promise<{ data?: unknown; error?: unknown }>,
  addFn: (registry, form) =>
    addBetaMember({
      path: { registry },
      body: {
        principal_type: form.principal_type,
        principal_id: form.principal_id.trim(),
        granted_by: form.granted_by.trim() || undefined,
      },
    }) as Promise<{ data?: unknown; error?: unknown }>,
  removeFn: (registry, item) =>
    removeBetaMember({
      path: { registry, principal_type: item.principal_type, principal_id: item.principal_id },
    }) as Promise<{ data?: unknown; error?: unknown }>,
  initialAddForm: () => ({ principal_type: "user", principal_id: "", granted_by: "" }),
  canSubmitAdd: (form) => !!form.principal_id.trim(),
});

/* A8: identities this instance has seen, offered rather than typed blind.

   A getter ref, not `toRef(addForm.value, …)`: `useAdminCrudList.submitAdd`
   replaces `addForm.value` outright after a successful grant, which would leave
   a property ref bound to an orphaned object and the field dead from the second
   grant onwards. */
const grantedBySuggestions = useSubjectSuggestions(toRef(() => addForm.value.granted_by));

// computed, not a bare const: a const is evaluated once at module load and
// would keep the English labels after a locale change.
const principalTypeOptions = computed(() => [
  { value: "user", label: t("common.user") },
  { value: "group", label: t("common.group") },
]);
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="NAMESPACES_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminBetaChannel.betaChannel')"
      :description="t('adminBetaChannel.manageWhoCanAccessPre')"
    >
      <template #actions>
        <Button size="sm" :disabled="!selectedRegistry" @click="addDialogOpen = true">{{
          t("adminBetaChannel.addMember")
        }}</Button>
      </template>
    </PageHeader>

    <!-- Registry selector -->
    <div class="space-y-2 max-w-xs">
      <Label for="beta-registry">{{ t("common.registry") }}</Label>
      <Select
        id="beta-registry"
        v-model="selectedRegistry"
        :placeholder="t('adminBetaChannel.selectARegistry')"
        :options="registryOptions"
      />
    </div>

    <!-- Members table -->
    <Card>
      <CardHeader>
        <div class="flex items-center justify-between">
          <CardTitle>
            {{ t("common.members") }}
            <span v-if="selectedRegistry" class="font-mono text-muted-foreground text-sm ml-1"
              >({{ selectedRegistry }})</span
            >
          </CardTitle>
          <Button variant="outline" size="sm" :disabled="membersLoading" @click="reloadMembers">
            {{ membersLoading ? t("common.loading") : t("common.refresh") }}
          </Button>
        </div>
      </CardHeader>
      <CardContent class="p-0">
        <!-- Empty only once loaded: an empty state shown while the request was
             in flight read as "none for this registry", and an operator added
             something that already existed. -->
        <AsyncState
          :loading="membersLoading"
          :error="membersError"
          :empty="!members?.length"
          :empty-message="
            selectedRegistry
              ? t('adminBetaChannel.noBetaChannelMembersFor')
              : t('adminBetaChannel.selectARegistryToView')
          "
          :on-retry="reloadMembers"
        >
          <Table :label="t('common.members')">
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("common.type") }}</TableHead>
                <TableHead>{{ t("adminBetaChannel.principalId") }}</TableHead>
                <TableHead>{{ t("adminBetaChannel.grantedBy") }}</TableHead>
                <TableHead class="text-right">{{ t("common.actions") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="m in members" :key="m.principal_type + ':' + m.principal_id">
                <TableCell>
                  <Badge
                    :variant="m.principal_type === 'user' ? 'known' : 'secondary'"
                    class="text-xs capitalize"
                  >
                    {{ m.principal_type }}
                  </Badge>
                </TableCell>
                <TableCell class="font-mono text-sm">
                  {{ m.principal_id }}
                </TableCell>
                <TableCell class="text-sm text-muted-foreground">
                  {{ m.granted_by ?? "—" }}
                </TableCell>
                <TableCell class="text-right">
                  <Button variant="outline" size="sm" @click="removeTarget = m">
                    {{ t("common.remove") }}
                  </Button>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </AsyncState>
      </CardContent>
    </Card>
  </div>

  <!-- Add member dialog -->
  <Dialog
    :open="addDialogOpen"
    @update:open="
      (v) => {
        if (!v) {
          addDialogOpen = false;
          addError = null;
        }
      }
    "
  >
    <template #title>{{ t("adminBetaChannel.addBetaChannelMember") }}</template>
    <template #description>
      <i18n-t keypath="adminBetaChannel.addToBetaChannelFor" tag="span">
        <template #registry
          ><span class="font-mono">{{ selectedRegistry }}</span></template
        >
      </i18n-t>
    </template>
    <div class="space-y-4">
      <div class="space-y-3">
        <div class="space-y-2">
          <Label for="beta-principal-type">{{ t("common.type") }}</Label>
          <Select
            id="beta-principal-type"
            v-model="addForm.principal_type"
            :options="principalTypeOptions"
          />
        </div>
        <div class="space-y-2">
          <Label for="beta-principal-id"
            >{{ t("adminBetaChannel.principalId") }} <span class="text-destructive">*</span></Label
          >
          <Input
            id="beta-principal-id"
            v-model="addForm.principal_id"
            placeholder="e.g. alice or team-frontend"
            class="font-mono"
          />
        </div>
        <div class="space-y-2">
          <Label for="beta-granted-by">{{ t("adminBetaChannel.grantedBy") }}</Label>
          <Combobox
            id="beta-granted-by"
            v-model="addForm.granted_by"
            :options="grantedBySuggestions.options.value"
            :loading="grantedBySuggestions.loading.value"
            :placeholder="t('adminBetaChannel.optionalYourUserId')"
          />
        </div>
      </div>
      <p v-if="addError" class="text-sm text-destructive">
        {{ addError }}
      </p>
      <div class="flex justify-end gap-2">
        <Button
          variant="outline"
          size="sm"
          :disabled="addLoading"
          @click="
            addDialogOpen = false;
            addError = null;
          "
        >
          {{ t("common.cancel") }}
        </Button>
        <Button size="sm" :disabled="addLoading || !addForm.principal_id.trim()" @click="submitAdd">
          {{ addLoading ? t("adminBetaChannel.adding") : t("adminBetaChannel.addMember") }}
        </Button>
      </div>
    </div>
  </Dialog>

  <!-- Remove confirmation dialog -->
  <Dialog
    :open="removeTarget !== null"
    @update:open="
      (v) => {
        if (!v) {
          removeTarget = null;
          removeError = null;
        }
      }
    "
  >
    <template #title>{{ t("adminBetaChannel.removeMember") }}</template>
    <template #description>
      <i18n-t keypath="adminBetaChannel.willLoseAccess" tag="span">
        <template #principal
          ><span class="font-mono">{{ removeTarget?.principal_id }}</span></template
        >
        <template #registry
          ><span class="font-mono">{{ selectedRegistry }}</span></template
        >
      </i18n-t>
    </template>
    <div class="space-y-4">
      <p v-if="removeError" class="text-sm text-destructive">
        {{ removeError }}
      </p>
      <div class="flex justify-end gap-2">
        <Button
          variant="outline"
          size="sm"
          :disabled="removeLoading"
          @click="
            removeTarget = null;
            removeError = null;
          "
        >
          {{ t("common.cancel") }}
        </Button>
        <Button variant="destructive" size="sm" :disabled="removeLoading" @click="confirmRemove">
          {{ removeLoading ? t("adminBetaChannel.removing") : t("common.remove") }}
        </Button>
      </div>
    </div>
  </Dialog>
</template>
