<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { useRoute } from "vue-router";
import { computed, ref } from "vue";
import { checkAccess } from "@/client/sdk.gen";
import type { AccessCheckResponse } from "@/client/types.gen";
import { API_BASE_URL } from "@/config";
import { extractMessage } from "@/composables/useApi";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Resolution } from "@/components/ui/resolution";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Select } from "@/components/ui/select";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import { Combobox } from "@/components/ui/combobox";
import { usePackageNameSuggestions, useVersionSuggestions } from "@/composables/useSuggestions";

import { EXAMPLE_COORDINATES, FALLBACK_COORDINATE } from "@/config/registryTypes";
const { t } = useI18n();

/** RFC 0004-bis §6.2: the registry set is closed, small and already fetched. */
const { options: registryOptions, typeOf, error: registriesError } = useRegistryOptions();

/** An example coordinate in the selected registry's own shape. */
const example = computed(
  () => EXAMPLE_COORDINATES[typeOf(registry.value) ?? ""] ?? FALLBACK_COORDINATE,
);

/**
 * Prefilled from the query when arriving from a denial (RFC 0003 §4.4).
 *
 * This is the one place the diagnostics earn their keep: someone reaches this
 * page because something was refused, and asking them to retype the coordinate
 * they just looked at is how a tool becomes the thing nobody opens.
 */
const route = useRoute();
const q = (key: string, fallback = ""): string => {
  const value = route.query[key];
  return typeof value === "string" && value ? value : fallback;
};

const registry = ref(q("registry", "github"));
const name = ref(q("name"));
const version = ref(q("version"));

/* Suggested from what this instance holds, never blocking a value it does not
   — the tool exists to explain a refusal, and a refusal is often about a
   coordinate the instance has never cached (RFC 0004-bis §6.2). */
const packageSuggestions = usePackageNameSuggestions(name, registry);
const versionSuggestions = useVersionSuggestions(registry, name);
const artifact = ref(q("artifact"));
const result = ref<AccessCheckResponse | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

async function check() {
  loading.value = true;
  error.value = null;
  result.value = null;
  try {
    const {
      data,
      error: apiErr,
      response,
    } = await checkAccess({
      query: {
        registry: registry.value,
        name: name.value,
        // Empty asks which version a client would receive (RFC 0040 §4.8).
        version: version.value || null,
        artifact: artifact.value || null,
      },
    });
    if (apiErr || !data) {
      // `String(apiErr)` on the API's error object rendered `[object Object]`
      // to the operator asking why a pull was refused — the one page whose
      // whole job is to answer that question. `extractMessage` reads the
      // object's `message`/`error` field, which is where the answer is.
      // A 404 is one answer for "no such package" and "one you may not see"
      // (RFC 0040 T1), so it is said as one sentence, not the raw body.
      error.value = !apiErr
        ? t("accessCheck.noResponse")
        : response?.status === 404
          ? t("accessCheck.notFound")
          : extractMessage(apiErr);
    } else {
      result.value = data;
    }
  } catch (e) {
    error.value = extractMessage(e);
  } finally {
    loading.value = false;
  }
}

/** The headline's three answers, each with its word, matrix and hue. */
const HEADLINE = {
  pullable: { variant: "known", state: "cached", key: "accessCheck.pullable" },
  waiting: { variant: "copper", state: "held", key: "accessCheck.waiting" },
  refused: { variant: "destructive", state: "blocked", key: "accessCheck.refused" },
} as const;
const headline = computed(() => (result.value ? HEADLINE[result.value.headline] : null));
const nextStep = computed(
  () => result.value?.causes.find((c) => c.state === "refuse" || c.state === "wait")?.next ?? null,
);
const STATE_ICON: Record<string, string> = { pass: "✅", warn: "⚠️", wait: "⏳", refuse: "⛔" };
/** Spelled out so the catalogue check can see every key. */
const STATE_LABEL: Record<string, string> = {
  pass: "accessCheck.state.pass",
  warn: "accessCheck.state.warn",
  wait: "accessCheck.state.wait",
  refuse: "accessCheck.state.refuse",
};
const formatTime = (iso: string) => new Date(iso).toLocaleString();
</script>

<template>
  <Card class="max-w-lg">
    <CardHeader>
      <CardTitle class="text-lg">{{ t("accessCheck.accessCheck") }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="grid gap-3">
        <div class="space-y-1">
          <Label for="registry">{{ t("common.registry") }}</Label>
          <Select
            id="registry"
            v-model="registry"
            :options="registryOptions"
            :placeholder="t('adminHealth.chooseRegistry')"
          />
          <!-- An empty list with no reason read as "this instance has no
               registries"; it was a failed request. -->
          <p v-if="registriesError" class="text-xs text-destructive">
            {{ t("homePage.registriesCouldNotLoad") }} {{ registriesError }}
          </p>
        </div>
        <div class="space-y-1">
          <Label for="name">{{ t("common.package") }}</Label>
          <Combobox
            id="name"
            v-model="name"
            :options="packageSuggestions.options.value"
            :loading="packageSuggestions.loading.value"
            :placeholder="example.name"
          />
        </div>
        <div class="space-y-1">
          <Label for="version">{{ t("common.version") }}</Label>
          <Combobox
            id="version"
            v-model="version"
            :options="versionSuggestions.options.value"
            :loading="versionSuggestions.loading.value"
            :disabled="!versionSuggestions.ready()"
            :disabled-reason="t('accessCheck.versionNeedsPackage')"
            :placeholder="example.version"
          />
        </div>
        <div class="space-y-1">
          <Label for="artifact">{{ t("accessCheck.artifactOptional") }}</Label>
          <Input id="artifact" v-model="artifact" placeholder="12345678" />
        </div>
      </div>

      <Button :disabled="loading" class="w-full" @click="check">
        {{ loading ? t("accessCheck.checking") : t("accessCheck.checkAccess") }}
      </Button>

      <!-- One live region, always in the DOM, so the verdict is announced
           when it lands rather than only painted. -->
      <div aria-live="polite" class="space-y-2">
        <p v-if="error" class="text-sm text-destructive">
          {{ error }}
        </p>

        <div v-if="result" class="border border-rule-strong p-4 space-y-2">
          <div class="flex items-center gap-2">
            <!-- Three channels, never one (DESIGN.md). `default` and
               `destructive` both resolve to `--accent` — `assets/index.css`
               sets `--destructive: var(--accent)` — so the page whose entire
               job is to answer "was I allowed?" painted both answers the same
               crimson, and hue was the only channel it used.

               `Resolution` sits *inside* the badge rather than beside it: it
               already renders the word next to its matrix, so pairing them
               would say "Allowed" twice, once to the eye and once to a screen
               reader. Inside, the badge is the frame, the matrix is the
               pattern, the word is the word, and the hue Resolution sets on
               itself is the one its variant already carries. -->
            <Badge v-if="headline" :variant="headline.variant">
              <Resolution :state="headline.state" :label="t(headline.key)" class="gap-2" />
            </Badge>
            <span class="text-sm text-muted-foreground">
              {{ result.package.version }}
              <template v-if="result.headline === 'waiting' && result.until">
                — {{ t("accessCheck.pullableFrom", { time: formatTime(result.until) }) }}
              </template>
              <template v-else-if="result.headline === 'waiting'">
                — {{ t("accessCheck.whenScanned") }}
              </template>
            </span>
          </div>
          <p v-if="result.resolved_version" class="text-sm">
            {{ t("accessCheck.resolvesTo", { version: result.resolved_version }) }}
          </p>
          <!-- In the server's order, which is also the reading and focus
               order; every icon has its state word beside it (RFC 0040 §11). -->
          <ol class="space-y-1 text-sm">
            <li v-for="(cause, i) in result.causes" :key="i" class="flex gap-2">
              <span aria-hidden="true">{{ STATE_ICON[cause.state] ?? "•" }}</span>
              <span class="font-mono w-14 shrink-0">{{
                STATE_LABEL[cause.state] ? t(STATE_LABEL[cause.state]) : cause.state
              }}</span>
              <span class="font-mono w-20 shrink-0 text-muted-foreground">{{ cause.kind }}</span>
              <span class="break-words">
                <code v-if="cause.reason_code">{{ cause.reason_code }}</code>
                {{ cause.detail }}
              </span>
            </li>
          </ol>
          <p v-if="nextStep" class="text-sm select-text">
            <span class="font-medium">{{ t("accessCheck.next") }}</span> {{ nextStep }}
          </p>
          <p v-if="result.proxy_url" class="text-xs text-muted-foreground break-all">
            URL:
            <a
              :href="`${API_BASE_URL}${result.proxy_url}`"
              target="_blank"
              rel="noopener"
              class="font-mono underline underline-offset-2 hover:text-foreground"
              >{{ API_BASE_URL }}{{ result.proxy_url }}</a
            >
          </p>
        </div>
      </div>
    </CardContent>
  </Card>
</template>
