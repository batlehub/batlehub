import { ref, computed, onMounted, onBeforeUnmount, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import { useExploreCache, useUpstreamCache } from "@/composables/useExploreCache";
import { extractMessage } from "@/composables/useApi";
import {
  listRegistries,
  exploreRegistryStats,
  explorePackages,
  exploreUpstreamSearch,
  exploreFetchVersion,
} from "@/client/sdk.gen";
import type {
  RegistryInfo,
  RegistryStatDto,
  ExploreEntryDto,
  ExplorePackageListResponse,
  UpstreamPackageDto,
} from "@/client/types.gen";
import { rowId, type ExploreRow, type UpstreamRow } from "@/lib/catalogRow";

/**
 * Where the search looks: names, README prose, or both (RFC 0007-bis §4.3).
 *
 * `name` is what this page has always done, and it stays the default — the
 * control is an opt-in to a *wider* search, never a narrowing of the one people
 * already know.
 */
type SearchScope = "name" | "readme" | "both";

export type SortKey = "fetched" | "downloads" | "name" | "recent";

interface PageResult {
  items: ExploreEntryDto[];
  total: number;
  /** The size the answer was actually paged at. Cached with the rows rather
      than assumed, so a hot reload of `packages_per_page` cannot leave the
      pager doing arithmetic with a number the cached page was not built at. */
  perPage: number;
  /** Whether this instance searches prose at all. Cached for the same reason as
      `perPage`: it is the server's answer, not something a client can infer, and
      a cache hit that left it at its initial `false` made the scope selector
      disappear for the whole TTL — catalog → package → Back is a cache hit. */
  readmeSearchEnabled: boolean;
  /** Whether the answer was cut at the prose cap. Stale in the other direction:
      a truncated prose search followed by a cached name search left the
      "showing the first N matches" note under a list it was not about. */
  truncated: boolean;
  /** RFC 0044 §10.2: the status batch failed for this answer. */
  scanUnavailable: boolean;
}

/**
 * The package catalog's state: the registry, search, scope, sort and page the
 * reader set up, the listing and upstream hits they produce, and the fetch
 * offered from the listing — kept in the URL so back, reload and a pasted link
 * all land on the same list.
 */
export function useCatalogQuery() {
  const { t } = useI18n();

  // ── State ─────────────────────────────────────────────────────────────────────

  const router = useRouter();
  const route = useRoute();

  const selectedRegistry = ref<string | null>(null);
  const search = ref("");
  const searchIn = ref<SearchScope>("name");
  /** `[search] readmes` on this instance, as the last response reported it. */
  const readmeSearchEnabled = ref(false);
  /** RFC 0044: the page's scan-status batch failed; rows draw "—". */
  const scanUnavailable = ref(false);
  /** The prose search hit its cap; there may be more than is shown. */
  const searchTruncated = ref(false);

  /* The proof's catalog is ordered by last fetch, and says so in its caption.
     That was not an option here — the API offered downloads/name/recent, where
     `recent` is when a client last downloaded *from* us, a different fact — so
     `fetched` was added alongside it rather than by redefining `recent`. It is
     the default for the same reason the proof chooses it: a catalog answers
     "what has been moving through here lately" before it answers "what is
     popular". */
  const sort = ref<SortKey>("fetched");
  const page = ref(0);
  /**
   * How many rows a page holds — **the server's number**, not this component's.
   *
   * It was a hard-coded 20, which is still what an unconfigured instance answers
   * with; it is `[limits].packages_per_page` now. The catalog does not ask for a
   * size (unlike the version table on a package page, which asks for the rows it
   * draws above the README): this list *is* the page, so the operator's number is
   * the right one and a console that asked for its own would make the setting
   * inert on the one screen it exists for.
   *
   * Seeded at 20 so the pager arithmetic is sane before the first answer lands.
   */
  const perPage = ref(20);

  // All configured accessible registries (sidebar — always complete list)
  const allRegistries = ref<RegistryInfo[]>([]);
  // Per-registry package counts (only registries that have ≥1 package)
  const registryStats = ref<Map<string, RegistryStatDto>>(new Map());
  /** True when the counts could not be read at all — not when they are zero. */
  const statsUnavailable = ref(false);

  const packages = ref<ExploreEntryDto[]>([]);
  const total = ref(0);
  const upstreamResults = ref<UpstreamPackageDto[]>([]);

  const loading = ref(false);
  const loadingRegs = ref(false);
  const loadingUpstream = ref(false);
  const error = ref<string | null>(null);

  // ── Computed ──────────────────────────────────────────────────────────────────

  // Merged sidebar list: every registry with its package count (0 if not yet seen)
  const sidebarRegistries = computed(() =>
    allRegistries.value.map((r) => ({
      name: r.name,
      package_count: registryStats.value.get(r.name)?.package_count ?? 0,
    })),
  );

  /** The facet's options, from the same merged list the sidebar rendered. */
  const facetOptions = computed(() =>
    sidebarRegistries.value.map((r) => ({
      value: r.name,
      label: r.name,
      count: countsKnown.value ? r.package_count : undefined,
    })),
  );

  const totalPackages = computed(() =>
    sidebarRegistries.value.reduce((s, r) => s + r.package_count, 0),
  );

  /** Counts are shown only when they were actually read. */
  const countsKnown = computed(() => !statsUnavailable.value);

  // Upstream-only hits (not already cached)
  const freshUpstream = computed(() => upstreamResults.value.filter((p) => !p.already_cached));

  // Unified rows: cached packages first, then upstream-only hits at the bottom
  const tableRows = computed<ExploreRow[]>(() => [
    ...packages.value.map((p) => ({ ...p, kind: "cached" as const })),
    ...freshUpstream.value.map((p) => ({ ...p, kind: "upstream" as const })),
  ]);

  const totalPages = computed(() => Math.max(1, Math.ceil(total.value / perPage.value)));

  /**
   * A prose search actually ran and came back with nothing.
   *
   * Distinct from "the filter matched nothing": the empty state has to say what
   * was searched, and `in=readme` with the feature *off* answers as a name search
   * — so the scope alone is not enough to decide which words to show.
   */
  const searchedProse = computed(
    () => readmeSearchEnabled.value && searchIn.value !== "name" && search.value.trim().length > 0,
  );

  // ── Cache ─────────────────────────────────────────────────────────────────────

  const exploreCache = useExploreCache<PageResult>();
  const upstreamCache = useUpstreamCache<UpstreamPackageDto[]>();

  /**
   * Sequence guards (RFC 0004-bis §6.3).
   *
   * Neither fetch guarded its own ordering: `packages.value = body.items` was
   * unconditional, with no sequence token and no `AbortController`, and
   * `selectRegistry`/`onSortChange` are undebounced. So clicking registry A
   * (uncached, slow) then B (cached, instant) let A's response overwrite the
   * table while the sidebar showed B selected. The cache entries stayed correct
   * throughout — it is only the *display* that was wrong, which is why the fix is
   * a sequence number rather than a change to the keys.
   *
   * A late response is still written to the cache and not to the screen. That is
   * deliberate: the response is correct for its own key, it is only stale for
   * what is currently being looked at, and discarding it would mean re-fetching
   * it the moment the operator clicks back.
   */
  let packagesSeq = 0;
  let upstreamSeq = 0;

  // ── Data fetching ─────────────────────────────────────────────────────────────

  async function fetchAllRegistries() {
    loadingRegs.value = true;
    try {
      const [regsResult, statsResult] = await Promise.all([
        listRegistries(),
        exploreRegistryStats(),
      ]);
      if (regsResult.data) {
        allRegistries.value = (regsResult.data as RegistryInfo[]).sort((a, b) =>
          a.name.localeCompare(b.name),
        );
      }
      if (statsResult.data) {
        const body = statsResult.data as {
          registries?: RegistryStatDto[];
          upstream_unavailable?: boolean;
        };
        registryStats.value = new Map((body.registries ?? []).map((s) => [s.registry, s]));
        // The server answers `{ registries: [], upstream_unavailable: true }`
        // when the stats query fails, and this page discarded the flag — so a
        // failed query rendered as every registry showing 0, indistinguishable
        // from an instance that genuinely holds nothing. Counts unknown and
        // counts zero are different facts.
        statsUnavailable.value = body.upstream_unavailable === true;
      } else if (statsResult.error) {
        statsUnavailable.value = true;
      }
    } catch {
      // non-fatal
    } finally {
      loadingRegs.value = false;
    }
  }

  async function fetchPackages() {
    const reg = selectedRegistry.value ?? "";
    const q = search.value.trim();
    const s = sort.value;
    const p = page.value;
    const scope = searchIn.value;
    const seq = ++packagesSeq;

    // Shown at once and then revalidated, not served for its TTL: the copy
    // here paints the page without a skeleton, and the request below replaces
    // it. Serving it as the answer kept a package the server had just cached
    // out of the list for up to five minutes, invisible to every refresh short
    // of a reload. The server's own catalogue cache absorbs the extra request.
    const cached = exploreCache.get(reg, p, s, q, scope);
    if (cached) {
      error.value = null;
      packages.value = cached.items;
      total.value = cached.total;
      perPage.value = cached.perPage;
      readmeSearchEnabled.value = cached.readmeSearchEnabled;
      searchTruncated.value = cached.truncated;
      scanUnavailable.value = cached.scanUnavailable;
      // Cleared here rather than left to the `finally`: this call already bumped
      // `packagesSeq`, so a request still in flight skips its own `finally` and
      // would leave the skeleton row on screen.
      loading.value = false;
    } else {
      loading.value = true;
      error.value = null;
    }
    try {
      const { data: res, error: apiErr } = await explorePackages({
        query: {
          page: p,
          sort: s,
          registry: reg || undefined,
          // `q` is the parameter `in` applies to; `name` is the older one and
          // still filters names, which is why the scope is sent with `q` alone.
          q: q || undefined,
          in: scope,
        },
      });
      // The transport's own words go under the title, which already says what
      // failed — throwing the title here printed it twice.
      if (apiErr) throw new Error(extractMessage(apiErr));
      const body = res as ExplorePackageListResponse;
      // Written under the coordinates captured at call time, not the current
      // refs, so a late response cannot land under the wrong key.
      // `per_page` comes back rather than being assumed: the operator sets it,
      // it can change under a hot reload, and a pager sized from a stale number
      // would offer pages the list does not have.
      const applied = body.per_page ?? perPage.value;
      // Reported by the server rather than inferred: a client cannot tell "no
      // package here says that" from "this instance does not search prose", and
      // guessing would put the wrong empty state on screen. Cached alongside the
      // rows for exactly that reason — a cache hit that did not restore them
      // would be the guess, one TTL later.
      const prose = body.readme_search_enabled === true;
      const cut = body.truncated === true;
      exploreCache.set(
        reg,
        p,
        s,
        q,
        {
          items: body.items,
          total: body.total,
          perPage: applied,
          readmeSearchEnabled: prose,
          truncated: cut,
          scanUnavailable: body.scan_status_unavailable === true,
        },
        scope,
      );
      if (seq !== packagesSeq) return; // superseded — cached, not displayed
      packages.value = body.items;
      total.value = body.total;
      perPage.value = applied;
      readmeSearchEnabled.value = prose;
      searchTruncated.value = cut;
      scanUnavailable.value = body.scan_status_unavailable === true;
    } catch (e) {
      // A failed revalidation leaves the copy already on screen alone.
      if (seq === packagesSeq && !cached) error.value = extractMessage(e);
    } finally {
      if (seq === packagesSeq) loading.value = false;
    }
  }

  async function fetchUpstream() {
    const name = search.value.trim();
    if (!name) return;
    // The refusal messages belong to the result set being replaced, and they are
    // keyed by `kind-registry/name` — which is stable across searches. Left
    // standing, a "Refused: release-age gate" from an earlier query reappears
    // under the same package the next time a search lists it, with no button
    // having been pressed and nothing having been refused.
    fetchResult.value = {};
    const reg = selectedRegistry.value ?? "";
    const seq = ++upstreamSeq;

    // Every hit here is N third-party calls that do not happen.
    const cached = upstreamCache.get(name, reg);
    if (cached) {
      upstreamResults.value = cached;
      // Cleared here for the reason `fetchPackages` clears it on its own cache
      // hit: this call already bumped `upstreamSeq`, so a request still in flight
      // fails its `seq === upstreamSeq` check and skips the `finally` below. Left
      // set, "Searching upstream registries…" never goes away and the empty state
      // — gated on `!loadingUpstream` — can never render again for the life of
      // the page.
      loadingUpstream.value = false;
      return;
    }

    loadingUpstream.value = true;
    try {
      const { data: res } = await exploreUpstreamSearch({
        query: { name, limit: 10, registry: reg || undefined },
      });
      if (res) {
        const body = res as { items?: UpstreamPackageDto[] };
        const items = body.items ?? [];
        upstreamCache.set(name, reg, items);
        if (seq !== upstreamSeq) return; // superseded — cached, not displayed
        upstreamResults.value = items;
      }
    } catch {
      // non-fatal
    } finally {
      if (seq === upstreamSeq) loadingUpstream.value = false;
    }
  }

  // ── Actions ───────────────────────────────────────────────────────────────────

  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  // Routing away within 300 ms of a keystroke otherwise ran `fetchPackages` — and
  // possibly `fetchUpstream` — against a destroyed component. The tests never saw
  // it because `typeSearch` always waits 350 ms, past the deadline.
  onBeforeUnmount(() => {
    if (searchTimer) clearTimeout(searchTimer);
  });

  /**
   * Empty the box and re-run the unfiltered listing, at once.
   *
   * `search = ''` alone only emptied the ref: nothing refetches on a bare change
   * (the one watcher on `search` is `syncQuery`), so the address bar went back to
   * `/packages` while `packages` still held the failed search's zero rows — and
   * because `search.trim()` was now falsy the empty state changed its story from
   * "nothing matches that search" to "nothing has been pulled through", which is a
   * claim about the registry rather than about the query. Only a reload got out
   * of it.
   *
   * Undebounced, and without `fetchUpstream`, for the same reason `onScopeChange`
   * is: there is no query left to ask upstream about.
   */
  function onClearSearch() {
    search.value = "";
    upstreamResults.value = [];
    page.value = 0;
    if (searchTimer) clearTimeout(searchTimer);
    void fetchPackages();
  }

  function onSearchInput(val: string) {
    search.value = val;
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      page.value = 0;
      void fetchPackages();
      if (val.trim().length >= 2) void fetchUpstream();
      else upstreamResults.value = [];
    }, 300);
  }

  /**
   * Changing the scope re-runs the search at once, undebounced.
   *
   * The query has not changed — only what it is asked of — so there is nothing to
   * wait for, and a delay after a select would read as the control not working.
   */
  function onSearchInChange(val: string) {
    searchIn.value = (["name", "readme", "both"] as const).includes(val as SearchScope)
      ? (val as SearchScope)
      : "name";
    page.value = 0;
    void fetchPackages();
  }

  function selectRegistry(reg: string | null) {
    selectedRegistry.value = reg;
    page.value = 0;
    upstreamResults.value = [];
    void fetchPackages();
    if (search.value.trim().length >= 2) void fetchUpstream();
  }

  function onSortChange(val: string) {
    sort.value = val as SortKey;
    page.value = 0;
    void fetchPackages();
  }

  // ── Fetch, from the listing (RFC 0007-bis §11 q3) ────────────────────────────
  //
  // The question this settles was recommended *no*, and the recommendation's
  // reason was sound as far as it went: "the listing has no version, and fetching
  // the package means choosing one." An upstream row is the case where that is
  // not true. It came from an upstream search, and the version it names is the
  // one the search itself returned — the same string already shown in the version
  // column. Nothing is chosen here. The button names that version rather than
  // saying "Fetch", so a reader sees what they are asking for before they ask.
  //
  // A cached row gets no button: this instance holds it, there is nothing to go
  // and get, and "which version" would then be a real question with no answer on
  // this screen. That is question 17 one screen earlier, and it stays refused.
  //
  // When the offer is not made — the operator's switch, a kind with no single
  // artifact per version, or no session — the listing draws nothing and says
  // nothing. The reason belongs on the package page, which the name links to and
  // which states it in a sentence; repeating a per-registry sentence on every row
  // of a page of hits would drown the rows it explains.

  /** Which row is being fetched, and what came of the last attempt. */
  const fetching = ref<string | null>(null);
  const fetchResult = ref<Record<string, string>>({});

  /**
   * Ask this instance to fetch the version this row names.
   *
   * The same endpoint the package page's button calls, so everything §4.4 says
   * about it holds here unchanged: the rules run, quota is spent, the access event
   * names the caller. Nothing about being on a listing makes it a lighter act, and
   * nothing here tries to make it one.
   */
  async function onFetchRow(row: UpstreamRow) {
    if (fetching.value) return;
    const id = rowId(row);
    fetching.value = id;
    delete fetchResult.value[id];
    try {
      const { error: apiErr } = await exploreFetchVersion({
        path: { registry: row.registry, name: row.name, version: row.latest_version },
      });
      if (apiErr) {
        const body = apiErr as { code?: string; message?: string };
        if (body.code === "fetch.already-held") {
          fetchResult.value[id] = t("packageCatalog.fetchAlreadyHeld");
        } else if (body.message) {
          fetchResult.value[id] = t("packageCatalog.fetchDenied", { reason: body.message });
        } else {
          fetchResult.value[id] = t("packageCatalog.fetchFailed");
        }
        return;
      }
      // The package is held now, so both halves of this page are stale: the
      // listing does not have it, and the upstream results still call it a
      // discovery. Without this the ten-minute upstream cache serves the row
      // straight back as a discovery and the button looks as though it did
      // nothing.
      //
      // Scoped by the *selected* registry and not by the row's, which is what the
      // Refresh button above does and what the cache keys are actually built
      // from: with no registry selected both stores key on the empty string, so
      // invalidating `npm` would clear nothing at all and this would silently do
      // half its job. When a registry is selected the two are the same anyway —
      // the upstream search only asks that one.
      exploreCache.invalidate(selectedRegistry.value ?? undefined);
      await Promise.all([fetchPackages(), fetchUpstream()]);
    } catch (e) {
      fetchResult.value[id] = extractMessage(e);
    } finally {
      fetching.value = null;
    }
  }

  function goToPage(p: number) {
    page.value = p;
    void fetchPackages();
  }

  // ── The search lives in the URL ───────────────────────────────────────────────
  //
  // Everything the reader set up — the registry, the query, its scope, the sort
  // and the page — was component state and nothing else, so it existed only for as
  // long as the component did. Open a package from the fifth page of a search and
  // the way back was a catalog reset to defaults, with the search box empty. The
  // detail page's own back button pushed `/packages?registry=…`, which read like a
  // fix and was not: this page never looked at `route.query`, so even that one
  // value was dropped on arrival.
  //
  // Putting it in the query fixes the trip both ways at once — back, reload,
  // bookmark, and a link pasted to a colleague all land on the same list — and it
  // is what makes `router.back()` on the detail page restore a *search* rather
  // than a path.
  //
  // `replace`, never `push`: a keystroke is not a destination. Pushing would make
  // the browser's Back button walk backwards through every letter of the query
  // before it left the page.

  /** The query this page's state serialises to — defaults omitted, so a plain
      `/packages` stays plain. */
  function stateAsQuery(): Record<string, string> {
    const q: Record<string, string> = {};
    if (selectedRegistry.value) q.registry = selectedRegistry.value;
    if (search.value.trim()) q.q = search.value;
    if (searchIn.value !== "name") q.in = searchIn.value;
    if (sort.value !== "fetched") q.sort = sort.value;
    if (page.value > 0) q.page = String(page.value + 1); // 1-based for a human
    return q;
  }

  /** Read it back. Anything absent or unrecognised falls to the same default the
      refs are declared with, so a hand-edited URL cannot put the page in a state
      its own controls could not produce. */
  function applyQuery(): void {
    const q = route.query;
    const one = (v: unknown) => (Array.isArray(v) ? v[0] : v);

    const registry = one(q.registry);
    selectedRegistry.value = typeof registry === "string" && registry ? registry : null;

    const term = one(q.q);
    search.value = typeof term === "string" ? term : "";

    const scope = one(q.in);
    searchIn.value = (["name", "readme", "both"] as const).includes(scope as SearchScope)
      ? (scope as SearchScope)
      : "name";

    const order = one(q.sort);
    sort.value = (["fetched", "downloads", "name", "recent"] as const).includes(order as SortKey)
      ? (order as SortKey)
      : "fetched";

    const p = Number(one(q.page));
    page.value = Number.isFinite(p) && p > 1 ? Math.floor(p) - 1 : 0;
  }

  function syncQuery(): void {
    // Only ever writes to its own route. A late fetch settling while the reader is
    // already on a package would otherwise rewrite the address bar underneath a
    // page this component does not own.
    if (route.path !== "/packages") return;

    const next = stateAsQuery();
    const current = route.query;
    const same =
      Object.keys(next).length === Object.keys(current).length &&
      Object.entries(next).every(([k, v]) => current[k] === v);
    // Vue Router rejects a redundant navigation, and writing one per keystroke
    // would also churn the address bar for no change.
    if (!same) void router.replace({ path: "/packages", query: next });
  }

  watch([selectedRegistry, search, searchIn, sort, page], syncQuery);

  // ── Lifecycle ─────────────────────────────────────────────────────────────────

  onMounted(() => {
    // Before the first fetch: the query *is* the initial state, and fetching the
    // defaults first would show a list nobody asked for and then replace it.
    applyQuery();
    void fetchAllRegistries();
    void fetchPackages();
    if (search.value.trim().length >= 2) void fetchUpstream();
  });

  return {
    selectedRegistry,
    search,
    searchIn,
    readmeSearchEnabled,
    scanUnavailable,
    searchTruncated,
    sort,
    page,
    perPage,
    allRegistries,
    registryStats,
    total,
    loading,
    loadingUpstream,
    error,
    facetOptions,
    totalPackages,
    countsKnown,
    tableRows,
    totalPages,
    searchedProse,
    exploreCache,
    fetchPackages,
    fetchUpstream,
    onClearSearch,
    onSearchInput,
    onSearchInChange,
    selectRegistry,
    onSortChange,
    goToPage,
    fetching,
    fetchResult,
    onFetchRow,
  };
}
