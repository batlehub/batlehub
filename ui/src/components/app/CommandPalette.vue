<script setup lang="ts">
/**
 * "Go to…" (RFC 0046 §4.2, M2): every page, red-path action and registry the
 * viewer may open, one `Ctrl/⌘+K` away from anywhere in the console.
 *
 * The filter is a convenience; the router guard is the control. Both make the
 * same instance-tier `can(verb)` call (`lib/palette.ts`), so the palette cannot
 * offer a page the guard would refuse. Each row is a real link built by
 * `router.resolve`, so middle-click and `Ctrl+Enter` open it in a new tab.
 *
 * Nothing is remembered between openings (§15 Q4), and the registry list is
 * the composable's own, fetched with the current token — never a module-level
 * copy a later viewer on the same machine could see (§4.4).
 */
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRouter } from "vue-router";
import { Dialog } from "@/components/ui/dialog";
import { Listbox, type ListboxItem } from "@/components/ui/listbox";
import { Announcer } from "@/components/ui/announcer";
import { useAuth } from "@/composables/useAuth";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import {
  actionEntries,
  looksLikeCredential,
  matchEntries,
  ownPages,
  pageEntries,
  registryEntries,
  searchEntry,
  type PaletteEntry,
  type PaletteKind,
  type PaletteViewer,
} from "@/lib/palette";

const open = defineModel<boolean>("open", { required: true });

const { t } = useI18n();
const router = useRouter();
const { can, canAny, identity, isAuthenticated, isOidcSession } = useAuth();
const { registries, loading, error, reload } = useRegistryOptions();

const query = ref("");
const active = ref(0);
const listId = "command-palette-listbox";

const viewer = computed<PaletteViewer>(() => ({
  can: (verb) => can(verb),
  canAny,
  isAuthenticated: isAuthenticated.value,
  isOidc: isOidcSession.value,
  hasRegistryAccess: identity.value?.has_registry_access === true,
}));

watch(open, (isOpen) => {
  if (!isOpen) return;
  query.value = "";
  active.value = 0;
  // Only a successful list is kept: a palette opened after a failure refetches.
  if (error.value) reload();
});

const trimmed = computed(() => query.value.trim());

/** With an empty query: the red paths the viewer may open, else their own pages. */
const commonTasks = computed(() => actionEntries(viewer.value, t, ""));

const entries = computed<PaletteEntry[]>(() => {
  const v = viewer.value;
  if (!trimmed.value) {
    return commonTasks.value.length > 0 ? commonTasks.value : ownPages(v, t);
  }
  const index = [
    ...pageEntries(v, t),
    ...actionEntries(v, t, trimmed.value),
    ...registryEntries(
      v,
      registries.value.map((r) => ({ name: r.name, type: r.type, mode: r.mode })),
    ),
  ];
  const search = searchEntry(
    v,
    trimmed.value,
    t("palette.searchPackages", { query: trimmed.value }),
  );
  return [...matchEntries(index, trimmed.value), ...(search ? [search] : [])];
});

const GROUP: Record<PaletteKind, string> = {
  page: "palette.group.pages",
  action: "palette.group.actions",
  registry: "palette.group.registries",
  search: "",
};

const items = computed<ListboxItem[]>(() =>
  entries.value.map((entry) => ({
    key: entry.key,
    label: entry.label,
    hint: entry.hint,
    href: router.resolve(entry.to).href,
    group: !trimmed.value
      ? commonTasks.value.length > 0
        ? t("palette.group.commonTasks")
        : undefined
      : GROUP[entry.kind]
        ? t(GROUP[entry.kind])
        : undefined,
  })),
);

watch(items, (list) => {
  if (active.value >= list.length) active.value = Math.max(0, list.length - 1);
});

/** The rows other than the hand-off: what "nothing matches" is about. */
const matchCount = computed(() => entries.value.filter((e) => e.kind !== "search").length);

const announcement = computed(() =>
  open.value && trimmed.value
    ? t("palette.resultCount", { count: matchCount.value }, matchCount.value)
    : "",
);

function go(index: number, newTab: boolean) {
  const entry = entries.value[index];
  if (!entry) return;
  if (newTab) {
    window.open(router.resolve(entry.to).href, "_blank", "noopener");
    return;
  }
  open.value = false;
  void router.push(entry.to);
}

/** A plain click is an in-app navigation; a modified one is left to the browser. */
function onChoose(index: number, event: MouseEvent) {
  if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) {
    return;
  }
  event.preventDefault();
  go(index, false);
}

function onKeydown(event: KeyboardEvent) {
  const count = items.value.length;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    if (count === 0) return;
    const delta = event.key === "ArrowDown" ? 1 : -1;
    active.value = (active.value + delta + count) % count;
    document.getElementById(`${listId}-opt-${active.value}`)?.scrollIntoView({ block: "nearest" });
  } else if (event.key === "Enter") {
    event.preventDefault();
    go(active.value, event.ctrlKey || event.metaKey);
  }
}
</script>

<template>
  <Dialog v-model:open="open" class="max-w-xl gap-0 p-0">
    <template #title>
      <span class="block px-4 pt-4 font-mono text-sm uppercase tracking-wider">{{
        t("palette.title")
      }}</span>
    </template>
    <div class="border-b border-border px-4 pb-3 pt-3">
      <input
        v-model="query"
        type="text"
        role="combobox"
        autocomplete="off"
        spellcheck="false"
        aria-expanded="true"
        :aria-controls="listId"
        aria-autocomplete="list"
        :aria-activedescendant="items.length ? `${listId}-opt-${active}` : undefined"
        :aria-label="t('palette.inputLabel')"
        :placeholder="t('palette.placeholder')"
        class="flex h-9 w-full border border-border bg-ground-sunk px-3 py-1 font-mono text-sm focus-visible:border-primary placeholder:text-muted-foreground"
        @keydown="onKeydown"
      />
    </div>

    <Announcer :message="announcement" />

    <Listbox
      :id="listId"
      :items="items"
      :active-index="active"
      :aria-label="t('palette.title')"
      class="max-h-[60vh] overflow-y-auto pb-2"
      @choose="onChoose"
      @hover="(i) => (active = i)"
    >
      <template #before>
        <!-- A viewer with nothing in "Common tasks" is told what to type (M2). -->
        <p
          v-if="!trimmed && commonTasks.length === 0"
          class="px-3 pt-3 text-xs text-muted-foreground"
        >
          {{ t("palette.emptyHint") }}
        </p>
        <!-- No match: stated, and the pasted token is not echoed back (§7.3). -->
        <p v-if="trimmed && matchCount === 0" class="px-3 pt-3 text-sm text-muted-foreground">
          {{
            looksLikeCredential(trimmed)
              ? t("palette.noMatchBare")
              : t("palette.noMatch", { query: trimmed })
          }}
        </p>
      </template>
      <template #after>
        <template v-if="trimmed">
          <p v-if="loading" class="px-3 py-2 text-xs text-muted-foreground" aria-hidden="true">
            {{ t("palette.registriesLoading") }}
          </p>
          <p v-else-if="error" role="status" class="px-3 py-2 text-xs text-muted-foreground">
            {{ t("palette.registriesFailed") }}
          </p>
        </template>
      </template>
    </Listbox>

    <p class="border-t border-border px-4 py-2 font-mono text-xs text-muted-foreground">
      {{ t("palette.keys") }}
    </p>
  </Dialog>
</template>
