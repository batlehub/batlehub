<script setup lang="ts">
import { ref } from "vue";
import { useI18n } from "vue-i18n";

import type { ConfigWarning } from "@/client/types.gen";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import ConfigWarningList from "@/components/admin/ConfigWarningList.vue";
import ConfigReadOnlyView from "@/components/admin/ConfigReadOnlyView.vue";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Alert } from "@/components/ui/alert";

/**
 * The TOML editor: validate, then stage the content as a pending reload. The
 * content and the candidate's warnings are the page's — it loads the one and
 * clears the other on apply — so both come down as models.
 */
const props = defineProps<{
  configLoading: boolean;
  configLoadError: string | null;
  configReadonly: boolean;
  hotReloadEnabled: boolean | null;
  /** Whether a reload is staged; the editor then folds behind one control. */
  hasPending: boolean;
  /** Re-reads the pending reload once one is staged; awaited, so the button waits for it. */
  refreshPending: () => Promise<void>;
}>();
const emit = defineEmits<{ reloadFromDisk: [] }>();
const configContent = defineModel<string>("content", { required: true });
const candidateWarnings = defineModel<ConfigWarning[]>("candidateWarnings", { required: true });

const { t } = useI18n();
const { authFetch } = useAuthFetch();

/**
 * The editor is a tool and the pending diff is the decision; while a reload
 * is staged the editor folds behind one control instead of putting an
 * 18-row textarea between the operator and the Apply they came for.
 */
const editorOpen = ref(false);
const editorValidating = ref(false);
const editorCreating = ref(false);
const editorSuccess = ref<string | null>(null);
/**
 * Neither success nor failure: the call was accepted but staged nothing, so the
 * green box would promise an apply step that is not there.
 */
const editorNotice = ref<string | null>(null);
const editorError = ref<string | null>(null);
const validatedContent = ref<string | null>(null);

async function validateConfigContent() {
  editorValidating.value = true;
  editorSuccess.value = null;
  editorNotice.value = null;
  editorError.value = null;
  validatedContent.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/config/validate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content: configContent.value }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    const body = (await res.json().catch(() => ({}))) as { warnings?: ConfigWarning[] };
    candidateWarnings.value = body.warnings ?? [];
    validatedContent.value = configContent.value;
    editorSuccess.value = t("adminConfigReload.configValidStageIt", {
      action: t("adminConfigReload.createPendingReload"),
    });
  } catch (e) {
    editorError.value = extractMessage(e);
  } finally {
    editorValidating.value = false;
  }
}

async function createPendingFromContent() {
  editorCreating.value = true;
  editorSuccess.value = null;
  editorNotice.value = null;
  editorError.value = null;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/config/from-content`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content: validatedContent.value ?? configContent.value }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(body.error ?? `HTTP ${res.status}`);
    }
    const body = (await res.json().catch(() => ({}))) as {
      warnings?: ConfigWarning[];
      pending_created?: boolean;
    };
    candidateWarnings.value = body.warnings ?? [];
    validatedContent.value = null;
    if (body.pending_created) {
      editorSuccess.value = t("adminConfigReload.pendingCreatedReviewIt");
    } else {
      // The server deduped: these exact bytes were the last config it loaded, so
      // there is nothing to rebuild and nothing staged. Saying "created" here is
      // what sends the admin to an Apply button that answers "no pending reload".
      editorNotice.value = t("adminConfigReload.nothingToStage");
    }
    await props.refreshPending();
  } catch (e) {
    editorError.value = extractMessage(e);
  } finally {
    editorCreating.value = false;
  }
}
</script>

<template>
  <!--
    Config Editor — only once the config has loaded and is writable. A
    failed load used to leave an empty 18-row textarea under the error, as
    if the config were blank; the read-only case is its own screen rather
    than this one with the controls greyed out.
  -->
  <template v-if="!configLoading && !configLoadError">
    <Card v-if="!configReadonly">
      <CardHeader class="flex-row flex-wrap items-center justify-between gap-2">
        <CardTitle>{{ t("adminConfigReload.configEditor") }}</CardTitle>
        <Button
          v-if="hasPending && !editorOpen"
          variant="outline"
          size="sm"
          @click="editorOpen = true"
        >
          {{ t("adminConfigReload.editConfig") }}
        </Button>
      </CardHeader>
      <CardContent v-if="!hasPending || editorOpen" class="space-y-3">
        <textarea
          v-model="configContent"
          rows="18"
          class="w-full border border-border bg-background font-mono text-xs p-3 resize-y"
          spellcheck="false"
          :aria-label="t('adminConfigReload.configTomlContent')"
          @input="validatedContent = null"
        />
        <Alert v-if="editorSuccess" variant="success">{{ editorSuccess }}</Alert>
        <div v-if="editorNotice" class="border border-copper px-4 py-2 text-sm text-copper">
          {{ editorNotice }}
        </div>
        <div v-if="candidateWarnings.length" class="space-y-2">
          <p class="text-sm text-muted-foreground">
            <i18n-t keypath="adminConfigReload.validWithWarnings" tag="span">
              <template #count>{{ candidateWarnings.length }}</template>
            </i18n-t>
          </p>
          <ConfigWarningList :warnings="candidateWarnings" />
        </div>
        <Alert v-if="editorError" variant="destructive">{{ editorError }}</Alert>
        <!-- One filled action per view: staging is it here — unless a reload
             is already staged, where Apply above is, and staging another
             becomes the secondary control. -->
        <div class="flex gap-2 flex-wrap">
          <Button
            variant="outline"
            :disabled="editorValidating || editorCreating || hotReloadEnabled !== true"
            @click="validateConfigContent"
          >
            {{
              editorValidating ? t("adminConfigReload.validating") : t("adminConfigReload.validate")
            }}
          </Button>
          <Button
            :variant="hasPending ? 'outline' : 'default'"
            :disabled="
              editorCreating || editorValidating || !validatedContent || hotReloadEnabled !== true
            "
            @click="createPendingFromContent"
          >
            {{
              editorCreating ? t("tokensPage.creating") : t("adminConfigReload.createPendingReload")
            }}
          </Button>
          <Button variant="ghost" @click="emit('reloadFromDisk')">{{
            t("adminConfigReload.reloadFromDisk")
          }}</Button>
        </div>
      </CardContent>
    </Card>

    <ConfigReadOnlyView v-else :content="configContent" :error="configLoadError" />
  </template>
</template>
