<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { useI18n } from "vue-i18n";

import type { ConfigWarning } from "@/client/types.gen";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { extractMessage } from "@/composables/useApi";
import { API_BASE_URL, docsUrl } from "@/config";
import { locateRegistry } from "@/lib/configLocate";
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
  /**
   * RFC 0046 §6.3: the registry `?registry=` named, handed down only once the
   * page knows whether a reload is staged. Applied once, when the content has
   * loaded — never on mount against an empty editor.
   */
  jumpTo?: string | null;
}>();
const emit = defineEmits<{
  reloadFromDisk: [];
  /** The content just staged as a pending reload: the page's new "saved" baseline. */
  staged: [content: string];
}>();
const configContent = defineModel<string>("content", { required: true });
const candidateWarnings = defineModel<ConfigWarning[]>("candidateWarnings", { required: true });

const { t, locale } = useI18n();
const { authFetch } = useAuthFetch();

/**
 * The editor is a tool and the pending diff is the decision; while a reload
 * is staged the editor folds behind one control instead of putting an
 * 18-row textarea between the operator and the Apply they came for.
 */
const editorOpen = ref(false);

// ── `?registry=` jump (RFC 0046 M5) ──────────────────────────────────────────

const textarea = ref<HTMLTextAreaElement | null>(null);
/** M5's notice: which state the jump ended in, announced as `role=status`. */
const jumpNotice = ref<{ key: "jumpedTo" | "jumpStaged" | "jumpOtherLayer"; name: string } | null>(
  null,
);
let jumped = false;

async function applyJump() {
  const name = props.jumpTo;
  if (jumped || !name || props.configLoading || props.configLoadError) return;
  jumped = true;
  // The read-only screen draws its own jump; it has no caret to place.
  if (props.configReadonly) return;
  // A staged reload folds the editor: no caret on a hidden textarea.
  if (props.hasPending && !editorOpen.value) {
    jumpNotice.value = { key: "jumpStaged", name };
    return;
  }
  const at = locateRegistry(configContent.value, name);
  jumpNotice.value = { key: at ? "jumpedTo" : "jumpOtherLayer", name };
  if (!at) return;
  await nextTick();
  const el = textarea.value;
  if (!el) return;
  el.focus();
  el.setSelectionRange(at.offset, at.offset);
  // The line into the top third of the textarea, not wherever focus left it.
  const lineHeight = el.scrollHeight / Math.max(1, configContent.value.split("\n").length);
  el.scrollTop = Math.max(0, at.line * lineHeight - el.clientHeight / 3);
}

watch(
  () => [props.jumpTo, props.configLoading, props.configLoadError] as const,
  () => void applyJump(),
  { immediate: true },
);

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
    const staged = validatedContent.value ?? configContent.value;
    validatedContent.value = null;
    // Staged — or identical to what is loaded — so nothing is left unsaved.
    emit("staged", staged);
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
      <!-- RFC 0046 M5: where the `?registry=` jump landed, announced. -->
      <CardContent v-if="jumpNotice" class="pb-3">
        <p role="status" class="text-sm text-muted-foreground" data-testid="jump-notice">
          <i18n-t
            v-if="jumpNotice.key === 'jumpOtherLayer'"
            keypath="adminConfigReload.jumpOtherLayer"
            tag="span"
          >
            <template #name>{{ jumpNotice.name }}</template>
            <template #link>
              <a
                class="underline underline-offset-2 hover:text-foreground"
                :href="docsUrl('guide/configuration#layered-config-files', locale)"
                target="_blank"
                rel="noopener"
                >{{ t("adminConfigReload.jumpOtherLayerLink") }}</a
              >
            </template>
          </i18n-t>
          <template v-else-if="jumpNotice.key === 'jumpStaged'">
            {{ t("adminConfigReload.jumpStaged", { name: jumpNotice.name }) }}
          </template>
          <template v-else>
            {{ t("adminConfigReload.jumpedTo", { name: jumpNotice.name }) }}
          </template>
        </p>
      </CardContent>
      <CardContent v-if="!hasPending || editorOpen" class="space-y-3">
        <textarea
          ref="textarea"
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

    <ConfigReadOnlyView
      v-else
      :content="configContent"
      :error="configLoadError"
      :jump-to="jumpTo ?? null"
    />
  </template>
</template>
