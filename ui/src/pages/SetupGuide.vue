<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { RouterLink } from "vue-router";
import { API_BASE_URL, DOCS_URL } from "@/config";
import { listRegistries } from "@/client/sdk.gen";
import type { RegistryInfo } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import RichText from "@/components/RichText.vue";
import { PageHeader } from "@/components/ui/page-header";
import { Input } from "@/components/ui/input";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Select } from "@/components/ui/select";
import { CodeBlock } from "@/components/ui/code-block";
import { Card, CardHeader, CardDescription, CardContent } from "@/components/ui/card";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import {
  REGISTRY_TYPE_DEFS,
  hostOf,
  netrcHostsFor,
  type RegistryTypeDef,
  type SnippetContext,
} from "@/config/registryTypes";

const { t } = useI18n();

const base = computed(() => API_BASE_URL || globalThis.location.origin);
const copied = ref<string | null>(null);

const { token, identity, isAuthenticated, isAdmin, expiresAt } = useAuth();

const netrcLogin = computed(() => identity.value?.user_id ?? "token");
const isOidc = computed(() => expiresAt.value > 0);

const { data: registries, loading } = useApi<Array<RegistryInfo>>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [],
);

/** This origin plus every host-routed registry's own host — see `netrcHostsFor`. */
const netrcHosts = computed(() => netrcHostsFor(base.value, registries.value ?? []));

const netrcSnippet = computed(() =>
  netrcHosts.value
    .map((host) => `machine ${host}\nlogin ${netrcLogin.value}\npassword ${token.value}`)
    .join("\n\n"),
);

// Group API registries by type
const registriesByType = computed<Record<string, RegistryInfo[]>>(() => {
  const map: Record<string, RegistryInfo[]> = {};
  for (const r of registries.value ?? []) {
    map[r.type] ??= [];
    map[r.type].push(r);
  }
  return map;
});

/**
 * Base URL clients should use for `name`: the registry's own hostname-rooted URL
 * when the server advertises one (`public_url`, set by host-based routing),
 * otherwise the `/proxy/{name}` subpath on this origin.
 */
function urlFor(name: string): string {
  const registry = (registries.value ?? []).find((r) => r.name === name);
  return registry?.public_url ?? `${base.value}/proxy/${name}`;
}

// Per-type selected registry name; defaults to first in the list
const selectedByType = ref<Record<string, string>>({});

function getSelected(typeId: string): string {
  const list = registriesByType.value[typeId] ?? [];
  return selectedByType.value[typeId] ?? list[0]?.name ?? "";
}

function getMode(typeId: string): string {
  const name = getSelected(typeId);
  return registriesByType.value[typeId]?.find((r) => r.name === name)?.mode ?? "proxy";
}

// Map of all type → selected name, used by composite tabs (mise)
const selectedNames = computed<Record<string, string>>(() => {
  const result: Record<string, string> = {};
  for (const typeId of Object.keys(registriesByType.value)) {
    result[typeId] = getSelected(typeId);
  }
  return result;
});

// Show only tabs for registry types that are actually configured
const activeDefs = computed(() =>
  REGISTRY_TYPE_DEFS.filter((def) => {
    const types = def.apiTypes ?? [def.id];
    return types.some((t) => t in registriesByType.value);
  }),
);

/**
 * The tool filter. With 21 registry types the tab strip stopped being a chooser
 * and became a wall — this narrows it by name so the thing you came for is one
 * keystroke away rather than one scan away (RFC 0003 §2.9).
 */
const toolFilter = ref("");
const visibleDefs = computed(() => {
  const q = toolFilter.value.trim().toLowerCase();
  if (!q) return activeDefs.value;
  return activeDefs.value.filter(
    (d) => d.label.toLowerCase().includes(q) || d.id.toLowerCase().includes(q),
  );
});

const defaultTab = computed(
  () => activeDefs.value[0]?.id ?? (isAuthenticated.value ? "netrc" : ""),
);

// The primary API type for a tab (null for composite tabs with multiple types)
function primaryType(def: RegistryTypeDef): string | null {
  if (def.apiTypes && def.apiTypes.length > 1) return null;
  return def.apiTypes?.[0] ?? def.id;
}

function ctxFor(def: RegistryTypeDef): SnippetContext {
  const pt =
    primaryType(def) ??
    (def.apiTypes ?? [def.id]).find((t) => t in registriesByType.value) ??
    def.id;
  const registryName = getSelected(pt);
  const registryUrl = urlFor(registryName);
  return {
    base: base.value,
    registryName,
    registryUrl,
    urlFor,
    mode: getMode(pt),
    isAuthenticated: isAuthenticated.value,
    token: token.value ?? "",
    // Credentials are keyed by the host the client actually talks to, which is
    // the registry's own host once it has one.
    netrcHost: hostOf(registryUrl),
    netrcLogin: netrcLogin.value,
    identity: identity.value,
    selectedNames: selectedNames.value,
    signedDownloads:
      (registries.value ?? []).find((r) => r.name === registryName)?.signed_downloads ?? false,
  };
}

function selectorOptions(def: RegistryTypeDef) {
  const pt = primaryType(def);
  if (!pt) return [];
  return (registriesByType.value[pt] ?? []).map((r) => ({
    value: r.name,
    label: r.name,
  }));
}

function showSelector(def: RegistryTypeDef): boolean {
  return selectorOptions(def).length > 1;
}

async function copy(key: string, text: string) {
  await navigator.clipboard.writeText(text);
  copied.value = key;
  setTimeout(() => {
    copied.value = null;
  }, 1500);
}
</script>

<template>
  <div class="max-w-7xl space-y-8">
    <PageHeader
      :title="t('setupGuide.setupGuide')"
      :description="t('setupGuide.configureYourToolsToRoute')"
      variant="poster"
    />

    <!-- Loading state -->
    <div v-if="loading" class="text-sm text-muted-foreground">
      {{ t("setupGuide.loadingRegistries") }}
    </div>

    <!-- No registries configured -->
    <div v-else-if="activeDefs.length === 0 && !isAuthenticated">
      <EmptyState
        :title="t('setupGuide.nothingToConnectTo')"
        :description="t('setupGuide.noRegistriesAreConfiguredOn')"
      >
        <template #action>
          <Button v-if="isAdmin" as-child size="sm">
            <RouterLink to="/admin/operations/config-reload">{{
              t("setupGuide.openConfig")
            }}</RouterLink>
          </Button>
          <Button as-child size="sm" variant="outline">
            <a :href="DOCS_URL" target="_blank" rel="noopener noreferrer">{{
              t("setupGuide.configurationGuide")
            }}</a>
          </Button>
        </template>
      </EmptyState>
    </div>

    <!-- Tabs -->
    <Tabs v-else :default-value="defaultTab">
      <div v-if="activeDefs.length > 6" class="mb-3 max-w-xs">
        <label class="sr-only" for="tool-filter">{{ t("setupGuide.filterTools") }}</label>
        <Input
          id="tool-filter"
          v-model="toolFilter"
          type="search"
          :placeholder="t('setupGuide.filterTools2')"
        />
      </div>
      <p v-if="toolFilter && visibleDefs.length === 0" class="mb-3 text-sm text-muted-foreground">
        {{ t("setup.noToolMatch", { query: toolFilter }) }}
      </p>

      <TabsList
        class="flex flex-wrap h-auto gap-1 justify-start bg-transparent border-none p-0 mb-2"
      >
        <TabsTrigger v-for="def in visibleDefs" :key="def.id" :value="def.id" class="rounded-sm">
          {{ def.label }}
        </TabsTrigger>
        <TabsTrigger v-if="isAuthenticated" value="netrc" class="rounded-sm"> .netrc </TabsTrigger>
      </TabsList>

      <!-- Dynamic registry tabs -->
      <TabsContent v-for="def in activeDefs" :key="def.id" :value="def.id">
        <Card>
          <CardHeader>
            <div class="flex items-start justify-between gap-4">
              <div class="flex-1 space-y-3">
                <CardDescription>
                  <RichText
                    :markup="def.description"
                    code-class="text-xs font-mono bg-muted px-1 rounded-sm"
                  />
                </CardDescription>
                <!-- Registry selector (shown when multiple registries of same type) -->
                <div v-if="showSelector(def)" class="flex items-center gap-2">
                  <label
                    :for="`setup-registry-${def.id}`"
                    class="text-xs text-muted-foreground shrink-0"
                    >{{ t("common.registryLabel") }}</label
                  >
                  <Select
                    :id="`setup-registry-${def.id}`"
                    :model-value="getSelected(primaryType(def)!)"
                    :options="selectorOptions(def)"
                    class="h-7 text-xs w-48"
                    @update:model-value="selectedByType[primaryType(def)!] = $event"
                  />
                </div>
              </div>
              <Badge
                v-if="def.fileHint"
                variant="outline"
                class="shrink-0 font-mono text-xs mt-0.5"
              >
                {{ def.fileHint }}
              </Badge>
            </div>
          </CardHeader>

          <CardContent class="space-y-4">
            <template v-for="snippet in def.snippets" :key="snippet.key">
              <div v-if="!snippet.showWhen || snippet.showWhen(ctxFor(def))">
                <p v-if="snippet.label" class="text-xs text-muted-foreground mb-1.5">
                  {{ snippet.label }}
                </p>
                <CodeBlock :code="snippet.template(ctxFor(def))" :lang="snippet.lang">
                  <Button
                    size="sm"
                    variant="ghost"
                    class="absolute top-2 right-2 h-7 px-2 text-xs"
                    @click="copy(snippet.key, snippet.template(ctxFor(def)))"
                  >
                    {{ copied === snippet.key ? t("common.copied") : t("common.copy") }}
                  </Button>
                </CodeBlock>
                <p v-if="snippet.note" class="text-xs text-muted-foreground mt-1.5">
                  <RichText
                    :markup="
                      typeof snippet.note === 'function' ? snippet.note(ctxFor(def)) : snippet.note
                    "
                  />
                </p>
              </div>
            </template>
          </CardContent>
        </Card>
      </TabsContent>

      <!-- .netrc tab (authenticated users only) -->
      <TabsContent v-if="isAuthenticated" value="netrc">
        <Card>
          <CardHeader>
            <div class="flex items-center justify-between">
              <CardDescription>
                <i18n-t keypath="setupGuide.netrcHelp" tag="span">
                  <template #file
                    ><code class="text-xs font-mono bg-muted px-1 rounded-sm"
                      >~/.netrc</code
                    ></template
                  >
                  <template #chmod
                    ><code class="text-xs font-mono bg-muted px-1 rounded-sm"
                      >chmod 600 ~/.netrc</code
                    ></template
                  >
                </i18n-t>
              </CardDescription>
              <Badge variant="outline" class="shrink-0 font-mono text-xs ml-4"> ~/.netrc </Badge>
            </div>
          </CardHeader>
          <CardContent class="space-y-3">
            <CodeBlock :code="netrcSnippet" lang="ini">
              <Button
                size="sm"
                variant="ghost"
                class="absolute top-2 right-2 h-7 px-2 text-xs"
                @click="copy('netrc', netrcSnippet)"
              >
                {{ copied === "netrc" ? t("common.copied") : t("common.copy") }}
              </Button>
            </CodeBlock>
            <p v-if="isOidc" class="text-xs text-muted-foreground">
              <i18n-t keypath="setupGuide.oidcTokenNote" tag="span">
                <template #link
                  ><RouterLink
                    to="/me/tokens"
                    class="underline underline-offset-2 hover:text-foreground transition-colors"
                    >{{ t("setupGuide.personalApiToken") }}</RouterLink
                  ></template
                >
              </i18n-t>
            </p>
          </CardContent>
        </Card>
      </TabsContent>
    </Tabs>
  </div>
</template>
