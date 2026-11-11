<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import RichText from "@/components/RichText.vue";
import { Input } from "@/components/ui/input";
import { Combobox } from "@/components/ui/combobox";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import type { PathFieldDef, RegistryPathTypeDef } from "@/config/registryPathFields";

const { t } = useI18n();

const props = defineProps<{
  typeDef: RegistryPathTypeDef;
  registries: { name: string; type: string }[];
}>();

const registryName = defineModel<string>("registryName", { required: true });

/** The prop's registries as combobox options — the set is already here. */
const registryOptions = computed(() =>
  props.registries.map((r) => ({ value: r.name, hint: r.type })),
);
const values = defineModel<Record<string, string>>("values", { required: true });

const GRID_COLS: Record<number, string> = { 2: "sm:grid-cols-2", 3: "sm:grid-cols-3" };

const rows = computed<PathFieldDef[][]>(() => {
  const result: PathFieldDef[][] = [];
  for (const field of props.typeDef.fields) {
    const last = result.at(-1)?.[0];
    if (field.row !== undefined && last?.row === field.row) {
      result.at(-1)?.push(field);
    } else {
      result.push([field]);
    }
  }
  return result;
});
</script>

<template>
  <div class="space-y-4">
    <div class="space-y-1">
      <Label :for="`${typeDef.id}-registry`">{{ t("registryPathForm.registryName") }}</Label>
      <!--
        A `<datalist>` here was the improvisation RFC 0004-bis §2.8 names: it
        cannot be styled to the world, its keyboard behaviour differs per
        browser, it exposes no listbox to assistive tech, and it cannot say
        "nothing matches" — which is the behaviour the combobox exists for.
      -->
      <Combobox
        :id="`${typeDef.id}-registry`"
        v-model="registryName"
        :options="registryOptions"
        :placeholder="typeDef.id"
        class="font-mono"
      />
    </div>

    <div
      v-for="(row, i) in rows"
      :key="i"
      :class="row.length > 1 ? ['grid gap-3', GRID_COLS[row.length]] : ''"
    >
      <div v-for="field in row" :key="field.key" class="space-y-1">
        <Label :for="`${typeDef.id}-${field.key}`">
          {{ field.label }}
          <span v-if="field.suffix" class="text-muted-foreground">{{ field.suffix }}</span>
        </Label>
        <Input
          :id="`${typeDef.id}-${field.key}`"
          v-model="values[field.key]"
          :placeholder="field.placeholder"
          :class="cn(field.mono && 'font-mono')"
        />
      </div>
    </div>

    <p v-if="typeDef.note" class="text-xs text-muted-foreground">
      <RichText :markup="typeDef.note" />
    </p>
  </div>
</template>
