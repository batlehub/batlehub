<script setup lang="ts">
import { computed, toRef } from "vue";
import { useI18n } from "vue-i18n";
import { listBetaMembers, addBetaMember, removeBetaMember } from "@/client/sdk.gen";
import type { BetaChannelMemberDto } from "@/client/types.gen";
import { useAdminCrudList } from "@/composables/useAdminCrudList";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { TableHead, TableCell } from "@/components/ui/table";
import RegistryScopedList from "@/components/admin/RegistryScopedList.vue";
import { Combobox } from "@/components/ui/combobox";
import { useSubjectSuggestions } from "@/composables/useSuggestions";

const { t } = useI18n();

interface AddMemberForm {
  principal_type: string;
  principal_id: string;
  granted_by: string;
}

const crud = useAdminCrudList<BetaChannelMemberDto, AddMemberForm>({
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
const { selectedRegistry, addForm, removeTarget, removeLoading, removeError, confirmRemove } = crud;

const grantedBySuggestions = useSubjectSuggestions(toRef(() => addForm.value.granted_by));

// computed, not a bare const: a const is evaluated once at module load and
// would keep the English labels after a locale change.
const principalTypeOptions = computed(() => [
  { value: "user", label: t("common.user") },
  { value: "group", label: t("common.group") },
]);
</script>

<template>
  <RegistryScopedList
    :crud="crud"
    id-prefix="beta"
    :title="t('adminBetaChannel.betaChannel')"
    :description="t('adminBetaChannel.manageWhoCanAccessPre')"
    :add-label="t('adminBetaChannel.addMember')"
    :adding-label="t('adminBetaChannel.adding')"
    :dialog-title="t('adminBetaChannel.addBetaChannelMember')"
    dialog-description-key="adminBetaChannel.addToBetaChannelFor"
    :select-placeholder="t('adminBetaChannel.selectARegistry')"
    :list-title="t('common.members')"
    :empty-message="t('adminBetaChannel.noBetaChannelMembersFor')"
    :no-registry-message="t('adminBetaChannel.selectARegistryToView')"
    :row-key="(m) => m.principal_type + ':' + m.principal_id"
  >
    <template #head>
      <TableHead>{{ t("common.type") }}</TableHead>
      <TableHead>{{ t("adminBetaChannel.principalId") }}</TableHead>
      <TableHead>{{ t("adminBetaChannel.grantedBy") }}</TableHead>
      <TableHead class="text-right">{{ t("common.actions") }}</TableHead>
    </template>
    <template #row="{ item: m }">
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
    </template>
    <template #form>
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
          placeholder="alice"
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
    </template>
  </RegistryScopedList>

  <!-- Remove confirmation dialog -->
  <DestructiveConfirm
    :open="removeTarget !== null"
    :action="t('common.remove')"
    :count="1"
    :item-noun="t('adminBetaChannel.memberNoun')"
    :scope="selectedRegistry"
    :consequence="
      t('adminBetaChannel.willLoseAccess', {
        principal: removeTarget?.principal_id,
        registry: selectedRegistry,
      })
    "
    reversible
    :loading="removeLoading"
    :error="removeError"
    @update:open="
      (v) => {
        if (!v) {
          removeTarget = null;
          removeError = null;
        }
      }
    "
    @confirm="confirmRemove"
  />
</template>
