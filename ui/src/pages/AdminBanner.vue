<script setup lang="ts">
/**
 * The global banner, on its own tab.
 *
 * It was the last card on config reload — a message to every visitor filed
 * under "change the running config", below a force-reload control, which is
 * where nobody looking for it during an incident would look.
 */
import { useI18n } from "vue-i18n";
import { ref } from "vue";
import { setBanner, clearBanner } from "@/client/sdk.gen";
import { extractMessage } from "@/composables/useApi";
import { useBanner } from "@/composables/useBanner";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Announcer } from "@/components/ui/announcer";

const { t } = useI18n();
const { banner } = useBanner();

const bannerMessage = ref("");
const bannerLevel = ref<"info" | "warning" | "error">("info");
const loadingSet = ref(false);
const loadingClear = ref(false);
const successMsg = ref<string | null>(null);
const errorMsg = ref<string | null>(null);

async function setBannerAction() {
  if (!bannerMessage.value.trim()) return;
  loadingSet.value = true;
  errorMsg.value = null;
  successMsg.value = null;
  try {
    const { error: apiErr } = await setBanner({
      body: { message: bannerMessage.value, level: bannerLevel.value },
    });
    if (apiErr) throw new Error(extractMessage(apiErr));
    successMsg.value = t("adminConfigReload.bannerSet");
    bannerMessage.value = "";
  } catch (e: unknown) {
    errorMsg.value = extractMessage(e);
  } finally {
    loadingSet.value = false;
  }
}

async function clearBannerAction() {
  loadingClear.value = true;
  errorMsg.value = null;
  successMsg.value = null;
  try {
    const { error: apiErr } = await clearBanner();
    if (apiErr) throw new Error(extractMessage(apiErr));
    successMsg.value = t("adminConfigReload.bannerCleared");
  } catch (e: unknown) {
    errorMsg.value = extractMessage(e);
  } finally {
    loadingClear.value = false;
  }
}
</script>

<template>
  <div class="space-y-6">
    <Announcer :message="successMsg ?? ''" />
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminNav.globalBanner')"
      :description="t('adminBanner.description')"
    />

    <Card>
      <CardHeader>
        <CardTitle>{{ t("common.currentLabel") }}</CardTitle>
      </CardHeader>
      <CardContent>
        <p v-if="banner" class="text-sm">
          <span class="font-mono text-xs uppercase tracking-wider text-muted-foreground">{{
            banner.level
          }}</span>
          {{ banner.message }}
          <span class="text-muted-foreground ml-2">
            <i18n-t keypath="adminConfigReload.setBy" tag="span">
              <template #user>{{ banner.set_by }}</template>
            </i18n-t>
          </span>
        </p>
        <p v-else class="text-sm text-muted-foreground">
          {{ t("adminConfigReload.noBannerCurrentlySet") }}
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader>
        <CardTitle>{{ t("adminConfigReload.setBanner") }}</CardTitle>
      </CardHeader>
      <CardContent class="space-y-4">
        <div class="flex gap-2 items-end flex-wrap">
          <div class="flex-1 min-w-[16rem] space-y-1">
            <Label for="banner-message">{{ t("common.message") }}</Label>
            <Input
              id="banner-message"
              v-model="bannerMessage"
              :placeholder="t('adminConfigReload.maintenanceInProgress')"
            />
          </div>
          <div class="space-y-1">
            <Label for="banner-level">{{ t("common.level") }}</Label>
            <select
              id="banner-level"
              v-model="bannerLevel"
              class="h-9 border border-input px-2 font-mono text-sm bg-background"
            >
              <option value="info">{{ t("common.info") }}</option>
              <option value="warning">{{ t("common.warning") }}</option>
              <option value="error">{{ t("common.error") }}</option>
            </select>
          </div>
          <Button :disabled="loadingSet || !bannerMessage.trim()" @click="setBannerAction">
            {{ loadingSet ? t("adminConfigReload.setting") : t("adminConfigReload.setBanner") }}
          </Button>
          <Button variant="outline" :disabled="loadingClear || !banner" @click="clearBannerAction">
            {{
              loadingClear ? t("adminConfigReload.clearing") : t("adminConfigReload.clearBanner")
            }}
          </Button>
        </div>
        <Alert v-if="successMsg" variant="success">{{ successMsg }}</Alert>
        <Alert v-if="errorMsg" variant="destructive">{{ errorMsg }}</Alert>
      </CardContent>
    </Card>
  </div>
</template>
