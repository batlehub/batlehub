<script setup lang="ts" generic="TItem, TForm extends object">
/**
 * An admin list scoped to one registry: pick the registry, see its rows, add
 * one through a dialog. Beta Channel and Team Namespaces were this page line
 * for line; each now supplies its columns, rows and form fields, and keeps
 * its own removal confirmation (their consequences differ).
 */
import { useI18n } from "vue-i18n";
import type { AdminCrudList } from "@/composables/useAdminCrudList";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { ACCESS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Table, TableHeader, TableBody, TableRow } from "@/components/ui/table";
import { Dialog } from "@/components/ui/dialog";

const props = defineProps<{
  crud: AdminCrudList<TItem, TForm>;
  /** Prefix of the element ids, e.g. `beta` → `beta-registry`. */
  idPrefix: string;
  title: string;
  description: string;
  /** The add verb, on the header button and the dialog's submit. */
  addLabel: string;
  dialogTitle: string;
  addingLabel: string;
  selectPlaceholder: string;
  listTitle: string;
  emptyMessage: string;
  noRegistryMessage: string;
  /** Keypath of the dialog description; its `{registry}` slot is filled here. */
  dialogDescriptionKey: string;
  rowKey: (item: TItem) => string;
}>();

defineSlots<{
  head(): unknown;
  row(props: { item: TItem }): unknown;
  form(props: { form: TForm }): unknown;
}>();

const { t } = useI18n();
const {
  canAdd,
  registryOptions,
  selectedRegistry,
  items,
  itemsError,
  itemsLoading,
  reloadItems,
  addDialogOpen,
  addForm,
  addLoading,
  addError,
  submitAdd,
} = props.crud;

function closeAdd() {
  addDialogOpen.value = false;
  addError.value = null;
}
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="ACCESS_TABS" />
    <PageHeader variant="display" :title="title" :description="description">
      <template #actions>
        <Button size="sm" :disabled="!selectedRegistry" @click="addDialogOpen = true">{{
          addLabel
        }}</Button>
      </template>
    </PageHeader>

    <!-- Registry selector -->
    <div class="space-y-2 max-w-xs">
      <Label :for="`${idPrefix}-registry`">{{ t("common.registry") }}</Label>
      <Select
        :id="`${idPrefix}-registry`"
        v-model="selectedRegistry"
        :placeholder="selectPlaceholder"
        :options="registryOptions"
      />
    </div>

    <Card>
      <CardHeader>
        <div class="flex items-center justify-between">
          <CardTitle>
            {{ listTitle }}
            <span v-if="selectedRegistry" class="font-mono text-muted-foreground text-sm ml-1"
              >({{ selectedRegistry }})</span
            >
          </CardTitle>
          <Button variant="outline" size="sm" :disabled="itemsLoading" @click="reloadItems">
            {{ itemsLoading ? t("common.loading") : t("common.refresh") }}
          </Button>
        </div>
      </CardHeader>
      <CardContent class="p-0">
        <!-- Empty only once loaded: an empty state shown while the request was
             in flight read as "none for this registry", and an operator added
             something that already existed. -->
        <AsyncState
          :loading="itemsLoading"
          :error="itemsError"
          :empty="!items?.length"
          :empty-message="selectedRegistry ? emptyMessage : noRegistryMessage"
          :on-retry="reloadItems"
        >
          <Table :label="listTitle">
            <TableHeader>
              <TableRow>
                <slot name="head" />
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="item in items" :key="rowKey(item)">
                <slot name="row" :item="item" />
              </TableRow>
            </TableBody>
          </Table>
        </AsyncState>
      </CardContent>
    </Card>
  </div>

  <Dialog :open="addDialogOpen" @update:open="(v) => !v && closeAdd()">
    <template #title>{{ dialogTitle }}</template>
    <template #description>
      <i18n-t :keypath="dialogDescriptionKey" tag="span">
        <template #registry
          ><span class="font-mono">{{ selectedRegistry }}</span></template
        >
      </i18n-t>
    </template>
    <div class="space-y-4">
      <div class="space-y-3">
        <slot name="form" :form="addForm" />
      </div>
      <p v-if="addError" class="text-sm text-destructive">
        {{ addError }}
      </p>
      <div class="flex justify-end gap-2">
        <Button variant="outline" size="sm" :disabled="addLoading" @click="closeAdd">
          {{ t("common.cancel") }}
        </Button>
        <Button size="sm" :disabled="addLoading || !canAdd" @click="submitAdd">
          {{ addLoading ? addingLabel : addLabel }}
        </Button>
      </div>
    </div>
  </Dialog>
</template>
