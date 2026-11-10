<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed, ref, watch } from "vue";
import { getPackageVisibility, setPackageVisibility } from "@/client/sdk.gen";
import type { Visibility } from "@/client/types.gen";
import { VISIBILITY_OPTIONS } from "@/config/visibility";
import { useApi, extractMessage } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardHeader, CardTitle, CardContent, CardDescription } from "@/components/ui/card";
import { Select } from "@/components/ui/select";
import { Label } from "@/components/ui/label";
import { AsyncState } from "@/components/ui/async-state";

const { t } = useI18n();

const visibilityOptions = computed(() =>
  VISIBILITY_OPTIONS.map((o) => ({ value: o.value, label: t(o.label) })),
);

const props = defineProps<{ registry: string; name: string }>();

const { token } = useAuth();

const {
  data: visibilityData,
  loading,
  error: loadError,
  reload,
} = useApi<{ visibility: Visibility }>(() => {
  if (!props.registry || !props.name) return Promise.resolve({ data: undefined });
  return getPackageVisibility({ path: { registry: props.registry, name: props.name } }) as Promise<{
    data?: unknown;
    error?: unknown;
  }>;
}, [token]);

const selected = ref<Visibility>("public");
watch(visibilityData, (v) => {
  if (v) selected.value = v.visibility;
});

const saving = ref(false);
const error = ref<string | null>(null);

async function save() {
  saving.value = true;
  error.value = null;
  try {
    const { error: apiErr } = await setPackageVisibility({
      path: { registry: props.registry, name: props.name },
      body: { visibility: selected.value },
    });
    if (apiErr) throw new Error((apiErr as { message?: string })?.message ?? t("common.apiError"));
    reload();
  } catch (e) {
    error.value = extractMessage(e);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t("packageVisibility.packageVisibility") }}</CardTitle>
      <CardDescription>{{ t("packageVisibility.controlsWhoCanDownload") }}</CardDescription>
    </CardHeader>
    <CardContent>
      <!-- A failed load is not "public": the badge's fallback would say it is.
           A reload after a save keeps the controls on screen. -->
      <AsyncState :loading="loading && !visibilityData" :error="loadError" :on-retry="reload">
        <div class="flex items-center gap-3 flex-wrap">
          <Badge
            :variant="
              selected === 'public' ? 'known' : selected === 'internal' ? 'secondary' : 'outline'
            "
            class="capitalize text-xs"
          >
            {{ visibilityData?.visibility ?? "public" }}
          </Badge>
          <Label for="package-visibility" class="sr-only">{{
            t("packageVisibility.packageVisibility")
          }}</Label>
          <Select
            id="package-visibility"
            v-model="selected"
            :options="visibilityOptions"
            class="w-72"
          />
          <Button
            size="sm"
            :disabled="saving || selected === (visibilityData?.visibility ?? 'public')"
            @click="save"
          >
            {{ saving ? t("packageVisibility.saving") : t("packageVisibility.save") }}
          </Button>
        </div>
        <p v-if="error" class="mt-2 text-sm text-destructive">{{ error }}</p>
      </AsyncState>
    </CardContent>
  </Card>
</template>
