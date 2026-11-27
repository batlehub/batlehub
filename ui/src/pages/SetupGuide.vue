<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed } from "vue";
import { RouterLink } from "vue-router";
import { API_BASE_URL, DOCS_URL, docsUrl } from "@/config";
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
import { Facet } from "@/components/ui/facet";
import {
  REGISTRY_GROUPS,
  REGISTRY_TYPE_DEFS,
  hostOf,
  netrcHostsFor,
  type RegistryGroup,
  type RegistryTypeDef,
  type SnippetContext,
} from "@/config/registryTypes";

import { AsyncState } from "@/components/ui/async-state";
const { t, locale } = useI18n();

const base = computed(() => API_BASE_URL || globalThis.location.origin);

const { token, identity, isAuthenticated, can, expiresAt } = useAuth();

const netrcLogin = computed(() => identity.value?.user_id ?? "token");
const isOidc = computed(() => expiresAt.value > 0);

const {
  data: registries,
  loading,
  error,
  reload,
} = useApi<Array<RegistryInfo>>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [],
);

/** This origin plus every host-routed registry's own host — see `netrcHostsFor`. */
const netrcHosts = computed(() => netrcHostsFor(base.value, registries.value ?? []));

/**
 * The token a snippet carries — never the live bearer on screen by default.
 *
 * Every snippet printed the session token in clear, which is what a screen
 * share or a colleague over a shoulder reads. On screen it is masked until the
 * reader asks; Copy always takes the real one, so the snippet stays ready to
 * paste. An OIDC session token expires within the hour, so writing it into a
 * `~/.netrc` built a credential that stops working silently: those sessions
 * get the placeholder, and the note beside it points at `/me/tokens`.
 */
const TOKEN_PLACEHOLDER = "<your-token>";
const TOKEN_MASK = "••••••••";
const tokenRevealed = ref(false);
function tokenFor(forCopy: boolean): string {
  if (isOidc.value) return TOKEN_PLACEHOLDER;
  if (forCopy || tokenRevealed.value) return token.value ?? "";
  return TOKEN_MASK;
}

function netrcSnippetFor(forCopy: boolean): string {
  return netrcHosts.value
    .map((host) => `machine ${host}\nlogin ${netrcLogin.value}\npassword ${tokenFor(forCopy)}`)
    .join("\n\n");
}

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

/**
 * The tool on screen, mirrored in the URL fragment (`/setup#npm`) so a link to
 * one recipe can be sent to a colleague. `replaceState` keeps the router's own
 * history state; a fragment that names no served tool falls back to the first.
 */
/**
 * Without a fragment, the tool picked last time: a developer comes back for the
 * same package manager. Failing that, the first tool *as listed* (languages
 * first) — not the first definition, which was mise's regex wall.
 */
const LAST_TOOL_KEY = "batlehub.setupTool";
const lastTool = (): string => {
  try {
    return globalThis.localStorage?.getItem(LAST_TOOL_KEY) ?? "";
  } catch {
    return ""; // storage refused (privacy mode): no memory, not an error
  }
};
const selectedTool = ref(globalThis.location?.hash.slice(1) || lastTool());
function selectTool(id: string | null) {
  if (!id) return;
  selectedTool.value = id;
  globalThis.history?.replaceState(globalThis.history.state, "", `#${id}`);
  try {
    globalThis.localStorage?.setItem(LAST_TOOL_KEY, id);
  } catch {
    // remembered for this visit only
  }
}
const currentDef = computed(() => {
  const listedFirst = groupedDefs.value[0]?.options[0]?.value;
  return (
    activeDefs.value.find((d) => d.id === selectedTool.value) ??
    visibleDefs.value.find((d) => d.id === listedFirst) ??
    visibleDefs.value[0] ??
    activeDefs.value[0]
  );
});

/** Spelled out, not built from the group id, so the catalogue check sees every key. */
const groupLabel = computed<Record<RegistryGroup, string>>(() => ({
  languages: t("setupGuide.groups.languages"),
  toolchains: t("setupGuide.groups.toolchains"),
  system: t("setupGuide.groups.system"),
  ide: t("setupGuide.groups.ide"),
  infra: t("setupGuide.groups.infra"),
  forges: t("setupGuide.groups.forges"),
}));

/** The visible tools folded into their families, empty families dropped. */
const groupedDefs = computed(() =>
  REGISTRY_GROUPS.map((group) => ({
    group,
    options: visibleDefs.value
      .filter((d) => d.group === group)
      .map((d) => ({ value: d.id, label: d.label, count: registryCount(d) })),
  })).filter((g) => g.options.length > 0),
);

/** How many registries a tool can point at; shown only when there is a choice. */
function registryCount(def: RegistryTypeDef): number | undefined {
  const pt = primaryType(def);
  const n = pt ? (registriesByType.value[pt]?.length ?? 0) : 0;
  return n > 1 ? n : undefined;
}

const quickCommands = computed(
  () =>
    `export BATLEHUB_SERVER=${base.value}\n` +
    (isAuthenticated.value ? "batlehub-cli auth login\n" : "") +
    "# preview the changes; add --yes to write them\nbatlehub-cli setup apply",
);

// The primary API type for a tab (null for composite tabs with multiple types)
function primaryType(def: RegistryTypeDef): string | null {
  if (def.apiTypes && def.apiTypes.length > 1) return null;
  return def.apiTypes?.[0] ?? def.id;
}

function ctxFor(def: RegistryTypeDef, forCopy = false): SnippetContext {
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
    token: tokenFor(forCopy),
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
</script>

<template>
  <div class="space-y-10 pt-2">
    <PageHeader
      :title="t('setupGuide.setupGuide')"
      :description="t('setupGuide.configureYourToolsToRoute')"
      variant="poster"
    />

    <!-- A failed list said nothing at all: the page fell through with no
         registry in it and left a lone `.netrc` block. -->
    <AsyncState
      v-if="loading || error"
      :loading="loading"
      :error="error"
      :title="t('setupGuide.registriesCouldNotLoad')"
      :on-retry="reload"
    />

    <!-- No registries configured -->
    <div v-else-if="activeDefs.length === 0 && !isAuthenticated">
      <EmptyState
        :title="t('setupGuide.nothingToConnectTo')"
        :description="t('setupGuide.noRegistriesAreConfiguredOn')"
      >
        <template #action>
          <Button v-if="can('config:read')" as-child size="sm">
            <RouterLink to="/admin/system/configuration">{{
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

    <template v-else>
      <!-- ① The fast path first: one command writes every snippet below for
           the clients on PATH (RFC 0038); the manual recipes are the fallback. -->
      <section aria-labelledby="setup-quick" class="space-y-3">
        <h2 id="setup-quick" class="text-lg font-semibold">{{ t("setupGuide.quickTitle") }}</h2>
        <CodeBlock :code="quickCommands" lang="sh" :copy="quickCommands" />
        <!-- The text keeps a reading measure (the rendered gate's ~80 characters). -->
        <i18n-t
          keypath="setupGuide.setupApplyNote"
          tag="p"
          class="max-w-[60ch] text-sm text-muted-foreground"
        >
          <template #apply
            ><code class="font-mono text-foreground">batlehub-cli setup apply</code></template
          >
          <template #yes><code class="font-mono text-foreground">--yes</code></template>
          <template #status><code class="font-mono text-foreground">setup status</code></template>
          <template #remove><code class="font-mono text-foreground">setup remove</code></template>
          <template #link
            ><a
              :href="docsUrl('use/cli#setup-apply', locale)"
              target="_blank"
              rel="noopener noreferrer"
              class="text-primary underline underline-offset-[3px]"
              >{{ t("setupGuide.setupApplyLink") }}</a
            ></template
          >
        </i18n-t>
      </section>

      <!-- ② Credentials: the token every snippet below carries, and the
           `.netrc` that is shared by several tools rather than being one. -->
      <section v-if="isAuthenticated" aria-labelledby="setup-creds" class="space-y-3">
        <h2 id="setup-creds" class="text-lg font-semibold">
          {{ t("setupGuide.credentialsTitle") }}
        </h2>
        <!-- Masked unless asked for. Not offered to an OIDC session, whose
             snippets carry the placeholder instead. -->
        <div v-if="!isOidc" class="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
          <span>{{ t("setupGuide.tokenHidden") }}</span>
          <Button
            variant="outline"
            size="sm"
            :aria-pressed="tokenRevealed"
            @click="tokenRevealed = !tokenRevealed"
          >
            {{ tokenRevealed ? t("setupGuide.hideToken") : t("setupGuide.showToken") }}
          </Button>
        </div>
        <p v-else class="max-w-[60ch] text-sm text-muted-foreground">
          <i18n-t keypath="setupGuide.oidcTokenNote" tag="span">
            <template #link
              ><RouterLink to="/me/tokens" class="text-primary underline underline-offset-[3px]">{{
                t("setupGuide.personalApiToken")
              }}</RouterLink></template
            >
          </i18n-t>
        </p>
        <details class="group border-t border-rule-soft pt-3">
          <summary class="cursor-pointer text-sm font-medium">
            <code class="font-mono">~/.netrc</code> — {{ t("setupGuide.netrcSummary") }}
          </summary>
          <div class="mt-3 space-y-3">
            <p class="max-w-[60ch] text-sm text-muted-foreground">
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
            </p>
            <CodeBlock :code="netrcSnippetFor(false)" lang="ini" :copy="netrcSnippetFor(true)" />
          </div>
        </details>
      </section>

      <!-- ③ By hand, one tool at a time: a rail grouped by family instead of
           a strip of thirty tabs, the recipe beside it. -->
      <section v-if="activeDefs.length > 0" aria-labelledby="setup-manual" class="space-y-3">
        <h2 id="setup-manual" class="text-lg font-semibold">{{ t("setupGuide.manualTitle") }}</h2>
        <div class="grid items-start gap-6 md:grid-cols-[14rem_1fr]">
          <!-- `min-w-0`: below `md` each family is a nowrap scrolling row,
               which must scroll inside itself, not widen the page. -->
          <aside class="min-w-0 space-y-4 md:border-r md:border-rule-soft md:pr-4">
            <div v-if="activeDefs.length > 6">
              <label class="sr-only" for="tool-filter">{{ t("setupGuide.filterTools") }}</label>
              <Input
                id="tool-filter"
                v-model="toolFilter"
                type="search"
                :placeholder="t('setupGuide.filterTools2')"
              />
            </div>
            <p
              v-if="toolFilter && visibleDefs.length === 0"
              class="px-3 text-sm text-muted-foreground"
            >
              {{ t("setup.noToolMatch", { query: toolFilter }) }}
            </p>
            <Facet
              v-for="g in groupedDefs"
              :key="g.group"
              :model-value="currentDef?.id ?? null"
              :options="g.options"
              :label="groupLabel[g.group]"
              @update:model-value="selectTool"
            />
          </aside>

          <div v-if="currentDef" class="min-w-0 space-y-4">
            <header class="space-y-3">
              <div class="flex flex-wrap items-baseline justify-between gap-3">
                <h3 class="text-base font-semibold">{{ currentDef.label }}</h3>
                <Badge v-if="currentDef.fileHint" variant="outline" class="font-mono text-xs">
                  {{ currentDef.fileHint }}
                </Badge>
              </div>
              <p class="max-w-[72ch] text-sm text-muted-foreground">
                <RichText
                  :markup="currentDef.description"
                  code-class="text-xs font-mono bg-muted px-1 rounded-sm"
                />
              </p>
              <!-- Registry selector (shown when multiple registries of same type) -->
              <div v-if="showSelector(currentDef)" class="flex items-center gap-2">
                <label
                  :for="`setup-registry-${currentDef.id}`"
                  class="text-xs text-muted-foreground shrink-0"
                  >{{ t("common.registryLabel") }}</label
                >
                <Select
                  :id="`setup-registry-${currentDef.id}`"
                  :model-value="getSelected(primaryType(currentDef)!)"
                  :options="selectorOptions(currentDef)"
                  class="h-7 text-xs w-48"
                  @update:model-value="selectedByType[primaryType(currentDef)!] = $event"
                />
              </div>
            </header>

            <!-- The snippets are alternatives as often as steps (npm, Yarn,
                 pnpm), so they are titled, not numbered. -->
            <template v-for="snippet in currentDef.snippets" :key="snippet.key">
              <div
                v-if="!snippet.showWhen || snippet.showWhen(ctxFor(currentDef))"
                class="space-y-2 border-t border-rule-soft pt-4"
              >
                <h4 v-if="snippet.label" class="text-sm font-medium">{{ snippet.label }}</h4>
                <CodeBlock
                  :code="snippet.template(ctxFor(currentDef))"
                  :lang="snippet.lang"
                  :copy="snippet.template(ctxFor(currentDef, true))"
                />
                <p v-if="snippet.note" class="max-w-[72ch] text-xs text-muted-foreground">
                  <RichText
                    :markup="
                      typeof snippet.note === 'function'
                        ? snippet.note(ctxFor(currentDef))
                        : snippet.note
                    "
                  />
                </p>
              </div>
            </template>
          </div>
        </div>
      </section>
    </template>
  </div>
</template>
