<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, onMounted, onBeforeUnmount, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useExploreCache, useUpstreamCache } from "@/composables/useExploreCache";
import { extractMessage } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { formatBytes, formatCount, formatRelative } from "@/lib/format";
import { Facet } from "@/components/ui/facet";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { Pagination } from "@/components/ui/pagination";
import { Search, RefreshCw } from "@lucide/vue";
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
import { Resolution, type ResolutionState } from "@/components/ui/resolution";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableHeader,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

// ── Unified row type for the table ────────────────────────────────────────────

type CachedRow = ExploreEntryDto & { kind: "cached" };
type UpstreamRow = UpstreamPackageDto & { kind: "upstream" };
type ExploreRow = CachedRow | UpstreamRow;

// ── State ─────────────────────────────────────────────────────────────────────

const router = useRouter();
const route = useRoute();

const selectedRegistry = ref<string | null>(null);
const search = ref("");
/**
 * Where the search looks: names, README prose, or both (RFC 0007-bis §4.3).
 *
 * `name` is what this page has always done, and it stays the default — the
 * control is an opt-in to a *wider* search, never a narrowing of the one people
 * already know.
 */
type SearchScope = "name" | "readme" | "both";
const searchIn = ref<SearchScope>("name");
/** `[search] readmes` on this instance, as the last response reported it. */
const readmeSearchEnabled = ref(false);
/** The prose search hit its cap; there may be more than is shown. */
const searchTruncated = ref(false);

type SortKey = "fetched" | "downloads" | "name" | "recent";

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

// ── The specimen (RFC 0004-bis §14.9) ────────────────────────────────────────
//
// `ui/design-proof/index.html` spends the Display step on **the registry you
// are looking at**, so the page announces its subject. This page spent 24px on
// the word "Packages" — a label on a door — because `--t-display` was mapped to
// no utility and `text-2xl` was the largest step that existed.
//
// The subject is the selected registry, or the instance itself when the facet
// is on "all": "every registry" is still an answer to "what am I looking at",
// and a blank specimen would be the §2.4 defect in a headline.

/** The registry whose name the Display step carries, or null for "all". */
const specimenRegistry = computed(() =>
  selectedRegistry.value
    ? (allRegistries.value.find((r) => r.name === selectedRegistry.value) ?? null)
    : null,
);

/**
 * The caption's facts, in the proof's order — *"hybrid · registry.npmjs.org ·
 * 1,284 packages · 6.1 GB cached"*: how it runs, what is behind it, how much of
 * it there is, how much of it we hold.
 *
 * The last two used to be missing outright. `upstream` was not on
 * `/api/v1/registries` at all, and the cached size was not on any endpoint this
 * page calls — a previous pass noted it and left the fact out rather than
 * estimate it, which was the right call and the wrong resting place. Both are
 * now served, so the caption is the proof's four facts rather than two of them.
 *
 * Still only what the API actually returned: each fact is pushed on its own
 * condition, so a registry with no upstream (local mode) or an instance whose
 * stats query failed drops that item instead of printing a zero.
 */
const specimenFacts = computed<string[]>(() => {
  const reg = specimenRegistry.value;
  const facts: string[] = [];
  if (reg) {
    facts.push(reg.type, reg.mode);
    if (reg.upstream) facts.push(reg.upstream);
    if (countsKnown.value) {
      const stat = registryStats.value.get(reg.name);
      facts.push(
        t("packageCatalog.packageCount", {
          count: formatCount(stat?.package_count ?? 0),
        }),
      );
      // `null` is "we never recorded sizes", which is not "0 B".
      if (stat?.cached_bytes != null) {
        facts.push(t("packageCatalog.cachedSize", { size: formatBytes(stat.cached_bytes) }));
      }
    }
  } else if (countsKnown.value) {
    facts.push(
      t("packageCatalog.registryCount", { count: allRegistries.value.length }),
      t("packageCatalog.packageCount", { count: formatCount(totalPackages.value) }),
    );
    if (totalCachedBytes.value != null) {
      facts.push(t("packageCatalog.cachedSize", { size: formatBytes(totalCachedBytes.value) }));
    }
  }
  return facts;
});

/** Summed only over the registries that reported a size; `null` when none did,
    so "nobody recorded any sizes" never renders as "0 B held". */
const totalCachedBytes = computed<number | null>(() => {
  let total: number | null = null;
  for (const stat of registryStats.value.values()) {
    if (stat.cached_bytes != null) total = (total ?? 0) + stat.cached_bytes;
  }
  return total;
});

/**
 * The table's caption, which the proof spends on the two things a reader needs
 * before scanning a column of rows: how many there are, and what order they are
 * in. The ordering half is not decoration — a table sorted by last fetch and a
 * table sorted by downloads look identical, and the proof's own caption is what
 * tells them apart.
 *
 * Named from the same catalogue entries as the sort control, so the caption
 * cannot describe an order the control does not offer.
 */
const SORT_LABEL_KEYS: Record<SortKey, string> = {
  fetched: "packageCatalog.lastFetch",
  downloads: "packageCatalog.mostDownloaded",
  name: "packageCatalog.nameAZ",
  recent: "packageCatalog.recentlyAccessed",
};

const tableCaption = computed(() =>
  t("packageCatalog.tableCaption", {
    count: formatCount(total.value),
    sort: t(SORT_LABEL_KEYS[sort.value]),
  }),
);

// Upstream-only hits (not already cached)
const freshUpstream = computed(() => upstreamResults.value.filter((p) => !p.already_cached));

// Unified rows: cached packages first, then upstream-only hits at the bottom
const tableRows = computed<ExploreRow[]>(() => [
  ...packages.value.map((p) => ({ ...p, kind: "cached" as const })),
  ...freshUpstream.value.map((p) => ({ ...p, kind: "upstream" as const })),
]);

const totalPages = computed(() => Math.max(1, Math.ceil(total.value / perPage.value)));

// ── Resolution as state ───────────────────────────────────────────────────────
//
// DESIGN.md's organising idea, and the reason this page exists in the form it
// does: what BatleHub holds and has verified renders at full resolution, what it
// does not renders coarse. The six states are named by the *server* — two of
// them need the registry's artifact TTL and the release-age gate's window and
// bypass roles, and `held` depends on who is asking — so this file's job is to
// map a string onto a mark and a word, not to decide anything.

const RESOLUTION_STATES = [
  "cached",
  "stale",
  "held",
  "pending",
  "yanked",
  "blocked",
] as const satisfies readonly ResolutionState[];

/** Spelled out, not `t(\`resolution.${s}\`)`: a template-literal key is invisible
    to the catalogue's reference gate, which is how 94 keys drifted out of use
    with everything green (RFC 0004-bis §2.2). */
const STATE_LABEL_KEYS: Record<ResolutionState, string> = {
  cached: "resolution.cached",
  stale: "resolution.stale",
  held: "resolution.held",
  pending: "resolution.pending",
  yanked: "resolution.yanked",
  blocked: "resolution.blocked",
};

/**
 * The three states that are a refusal, and the rule behind each.
 *
 * The proof states a denial's rule *in its own row*, tied to the package by
 * `aria-describedby`, rather than floating one note above the table — so the
 * reason travels with the thing it is about, for a screen reader as well as for
 * a reader scanning the column.
 *
 * The wording names the mechanism and stops there. The proof's notes quote
 * specifics ("published 11 minutes ago, held until 24 h"); this listing does not
 * carry the block reason, the yank date or the quarantine's remaining window —
 * those live on the package's own page, which each note links to. Stating a
 * mechanism we know beats interpolating numbers we do not have.
 */
const NOTE_KEYS: Partial<Record<ResolutionState, string>> = {
  blocked: "resolution.noteBlocked",
  yanked: "resolution.noteYanked",
  held: "resolution.noteHeld",
};

/** An upstream search hit has never been fetched, which is `pending`'s
    definition; a cached row carries whatever the server graded it. */
function rowState(row: ExploreRow): ResolutionState {
  if (row.kind !== "cached") return "pending";
  return (RESOLUTION_STATES as readonly string[]).includes(row.state)
    ? (row.state as ResolutionState)
    : "pending";
}

function noteKey(row: ExploreRow): string | undefined {
  return NOTE_KEYS[rowState(row)];
}

/**
 * Why this row is here, or `undefined` when it needs no saying.
 *
 * Only a prose match is labelled. A row that matched on its name is
 * self-explanatory, and labelling every row would make the one that actually
 * needs the label invisible (RFC 0007-bis §4.3).
 */
function matchLabel(row: ExploreRow): string | undefined {
  if (row.kind !== "cached") return undefined;
  const matched = (row as ExploreEntryDto).matched_in;
  if (matched === "readme") return t("packageCatalog.matchedReadme");
  if (matched === "both") return t("packageCatalog.matchedBoth");
  return undefined;
}

/** The matched fragment of a README, as plain text. Never markup. */
function rowSnippet(row: ExploreRow): string | undefined {
  if (row.kind !== "cached") return undefined;
  return (row as ExploreEntryDto).snippet ?? undefined;
}

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

/** Stable across re-renders because it is derived from the row's identity, not
    from its index: `aria-describedby` pointing at a recycled id is worse than
    pointing at nothing. */
function rowId(row: ExploreRow): string {
  return `${row.kind}-${row.registry}/${row.name}`;
}

// ── Cache ─────────────────────────────────────────────────────────────────────

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
}
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
    const [regsResult, statsResult] = await Promise.all([listRegistries(), exploreRegistryStats()]);
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

  const cached = exploreCache.get(reg, p, s, q, scope);
  if (cached) {
    error.value = null;
    packages.value = cached.items;
    total.value = cached.total;
    perPage.value = cached.perPage;
    readmeSearchEnabled.value = cached.readmeSearchEnabled;
    searchTruncated.value = cached.truncated;
    // `loading` has to be cleared here too, not only in the `finally` below.
    // This call already bumped `packagesSeq`, so any request still in flight
    // will fail its own `seq === packagesSeq` check and skip the `finally` —
    // leaving the skeleton row on screen forever. Reproduced by selecting an
    // uncached registry and then a cached one before the first lands.
    loading.value = false;
    return;
  }

  loading.value = true;
  error.value = null;
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
    if (apiErr) throw new Error(t("packageCatalog.loadFailed"));
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
      },
      scope,
    );
    if (seq !== packagesSeq) return; // superseded — cached, not displayed
    packages.value = body.items;
    total.value = body.total;
    perPage.value = applied;
    readmeSearchEnabled.value = prose;
    searchTruncated.value = cut;
  } catch (e) {
    if (seq === packagesSeq) error.value = extractMessage(e);
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

/**
 * Both kinds of row lead to the same page, and the upstream one used to lead
 * nowhere.
 *
 * `if (row.kind !== "cached") return` made a package this instance has not
 * pulled the one thing in the catalog you could not open — while the page it
 * would have opened was already built for exactly that case: `explore/detail.rs`
 * calls `upstream_detail` for anything not held here, and `PackageDetailPage`
 * renders the result under "N version(s) below exist upstream and are not held
 * here", with the per-version **Fetch this version** button RFC 0007-bis §6.4
 * put there. Measured against the running instance, `npm/chalk` — never pulled —
 * answers with 44 versions, their publication dates and their firewall verdicts.
 * So the capability was reachable only by typing the URL, which is not a
 * capability the console had.
 *
 * `UpstreamPackageDto` carries `registry` and `name`, the same two fields the
 * cached row uses, so there is nothing to resolve first: deciding *before* the
 * click whether a package deserves a page is the check that was wrong, not the
 * URL it built.
 *
 * A path rather than a handler, because the row used to be a `@click` on
 * `<TableRow>` with no `tabindex`, no `role` and no key handler, and the
 * package name was plain text rather than a link. The only keyboard route to a
 * package's page was the **details** link *inside a refusal note*, which
 * renders only when `noteKey(row)` is set — so for a normally cached package,
 * a keyboard user could not open its detail page at all. That is the product's
 * primary navigation path, and WCAG 2.1.1 Keyboard (A) is the floor it missed.
 *
 * The link navigates with a **push**, not a `replace`. The version list on the
 * detail page replaces, for the reason stated there; this one must not.
 * `goBack()` restores the catalog's search, scope, sort, page and scroll offset
 * only when `history.state.back` is `/packages`, and replacing the catalog
 * entry on the way in is exactly what would stop it being that.
 */
function detailPath(row: ExploreRow): string {
  return `/packages/${encodeURIComponent(row.registry)}/${encodeURIComponent(row.name)}`;
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

const { isAuthenticated } = useAuth();

/** Which row is being fetched, and what came of the last attempt. */
const fetching = ref<string | null>(null);
const fetchResult = ref<Record<string, string>>({});

/**
 * Whether this row offers a fetch: upstream-only, and the server said yes.
 *
 * `fetch?.` although the generated type says the field is always there. It is
 * always there in a *fresh* response; the upstream half of this page is served
 * from a ten-minute in-memory cache, so a reader whose tab outlived a deploy
 * has rows from the server that had no such field. Optional chaining draws no
 * button for them, which is the right answer, where a hard read throws inside
 * the render and takes the whole listing down — including its cached rows,
 * which had nothing to do with it.
 */
function canFetch(row: ExploreRow): boolean {
  return row.kind === "upstream" && row.fetch?.offered === true && isAuthenticated.value;
}

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
</script>

<template>
  <div>
    <!-- The specimen. Not `PageHeader`: this is the one surface with a
         checked-in statement of its own appearance, and that statement gives
         the Display step to the subject rather than to the page's nav label
         (RFC 0004-bis §2.7, §14.9). Every other page keeps PageHeader.

         Full width, above the sheet — the proof's own arrangement, where
         `.specimen` is a direct child of `<main>` and `.sheet` starts beneath
         it. It used to sit *inside* the right-hand column, which meant the bay
         ran alongside it and the display type began 232px in from the page
         edge. A specimen is the page announcing its subject; starting it
         indented, in the gutter of a list it is not part of, is the one
         placement that undercuts that.

         `overflow-hidden` contains the plate; `relative` gives it a frame.
         The h1 sits above it on z-index — the plate is `aria-hidden` and
         purely decorative.

         The bottom rule is `--rule-soft`, the separator token, rather than
         `--border` (`--rule-strong`): it now divides the specimen from the
         sheet across the whole window, which is the job the proof gives
         `.specimen{border-bottom:1px solid var(--rule-soft)}`. At full width a
         contrast-carrying weight reads as a second masthead. -->
    <section class="relative overflow-hidden border-b border-rule-soft pb-6 pt-4">
      <div class="plate" aria-hidden="true"></div>
      <!-- The proof's `.display` rule, ported rather than approximated:
           `line-height:.92`, `letter-spacing:.02em`, uppercase. The body
           line-height is 1.625, which on a 104px element costs 169px per line
           — a two-line registry name came to 312px of headline before the
           measurement caught it. A display face is set tight; that is most of
           what makes it read as display rather than as a very large heading. -->
      <h1
        class="relative z-10 font-display font-bold uppercase tracking-[0.02em] leading-[0.92] text-display break-words"
        data-testid="specimen-name"
      >
        {{ specimenRegistry?.name ?? t("packageCatalog.allRegistriesTitle") }}
      </h1>
      <p
        v-if="specimenFacts.length"
        class="relative z-10 mt-3 border-t border-border pt-3 text-sm max-w-[72ch]"
      >
        <template v-for="(fact, i) in specimenFacts" :key="fact">
          <span v-if="i > 0" class="px-2 text-border" aria-hidden="true">·</span>
          <span class="text-foreground">{{ fact }}</span>
        </template>
      </p>
    </section>

    <!-- The sheet: the proof's `grid-template-columns: 232px 1fr` with
         `align-items:start`, so the bay is its own column beside the catalog
         and neither stretches to match the other's height. Below `md` it
         collapses to one column, as the proof collapses it at 900px. -->
    <div class="grid min-h-[60vh] items-start md:grid-cols-[14rem_1fr]">
      <!-- The registry bay, the shared facet primitive rather than 40 lines of
           inline buttons. Selection rides on ink and a lit edge, not a fill.
           The separator is `--rule-soft` — it divides two regions, it does not
           carry contrast, and the proof rules it accordingly. -->
      <!-- Visible at every width. It used to be `hidden md:block`, which left a
           phone with no way to change registry at all — the filter existed and
           its only control did not. Below `md` it is a rule *under* the strip
           rather than beside it, matching the orientation the cells take. -->
      <!-- `min-w-0` for the same reason the catalog column below needs it, and
           it bites harder here: a grid item's default `min-width:auto` refuses
           to shrink below its content's min-content width, and this item's
           content is a nowrap flex row of every registry. Without it that row's
           full width propagates out through the grid and the *body* scrolls
           sideways — the one thing the Own-Container Overflow Rule forbids.
           With it, the row scrolls inside itself, which is the point. -->
      <aside
        class="min-w-0 border-b border-rule-soft py-3 md:border-b-0 md:border-r md:py-6 md:pr-4"
      >
        <Facet
          :model-value="selectedRegistry"
          :options="facetOptions"
          :label="t('packageCatalog.registries')"
          :all-label="
            countsKnown
              ? t('packageCatalog.allRegistries', { count: totalPackages })
              : t('packageCatalog.allRegistriesUnknown')
          "
          @update:model-value="selectRegistry"
        />
      </aside>

      <!-- `min-w-0` is load-bearing on a grid child: without it the track
           refuses to shrink below its content's min-content width, and the
           table's own scroll container never gets the chance to do its job. -->
      <div class="min-w-0 space-y-4 py-6 md:pl-6">
        <!-- Search + sort bar -->
        <div class="flex gap-2 flex-wrap">
          <div class="relative flex-1 min-w-48">
            <Search class="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input
              class="pl-8"
              :placeholder="t('packageCatalog.searchPackages')"
              :aria-label="t('packageCatalog.searchPackages2')"
              :value="search"
              @input="onSearchInput(($event.target as HTMLInputElement).value)"
            />
          </div>
          <!-- Where the search looks. Beside the box it modifies rather than in
             a settings panel, because it changes what the *next keystroke*
             means. Hidden entirely when the instance searches names only: a
             control whose other options do nothing is worse than no control
             (RFC 0007-bis §4.3). -->
          <select
            v-if="readmeSearchEnabled"
            :aria-label="t('packageCatalog.searchIn')"
            class="h-9 rounded-sm border border-input bg-background px-3 font-mono text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-ring"
            :value="searchIn"
            @change="onSearchInChange(($event.target as HTMLSelectElement).value)"
          >
            <option value="name">{{ t("packageCatalog.searchInName") }}</option>
            <option value="readme">{{ t("packageCatalog.searchInReadme") }}</option>
            <option value="both">{{ t("packageCatalog.searchInBoth") }}</option>
          </select>
          <!-- The page's one action, in the toolbar beside search — where
             `design-proof/index.html` puts its own (RFC 0004-bis §14.9). It
             used to sit in `PageHeader`, which the specimen replaced. -->
          <Button
            variant="outline"
            size="sm"
            @click="
              () => {
                exploreCache.invalidate(selectedRegistry ?? undefined);
                void fetchPackages();
                if (search.trim().length >= 2) void fetchUpstream();
              }
            "
          >
            <RefreshCw class="h-4 w-4 mr-1" />
            {{ t("common.refresh") }}
          </Button>
          <select
            :aria-label="t('packageCatalog.sortPackages')"
            class="h-9 rounded-sm border border-input bg-background px-3 font-mono text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-ring"
            :value="sort"
            @change="onSortChange(($event.target as HTMLSelectElement).value)"
          >
            <option value="fetched">{{ t("packageCatalog.lastFetch") }}</option>
            <option value="downloads">{{ t("packageCatalog.mostDownloaded") }}</option>
            <option value="name">{{ t("packageCatalog.nameAZ") }}</option>
            <option value="recent">{{ t("packageCatalog.recentlyAccessed") }}</option>
          </select>
        </div>

        <!-- Error -->
        <p v-if="error" class="text-sm text-destructive">{{ error }}</p>

        <!-- The catalog.

           No `Card`. The proof puts the table straight onto the sheet, and the
           card was adding a second boundary around content the ruled header row
           already bounds — two frames around one table, on a system whose
           Flat-At-Rest Rule allows no elevation to justify either.

           Column order is the proof's, and the order is the argument: State
           leads, because "does this instance hold it" is the question the whole
           world is organised around, and a reader scanning for what is wrong
           should not have to cross five columns to find out. It used to sit
           last, behind Registry, Versions, Downloads and Source. -->
        <!-- The floor under the five-column layout, and the reason the wrapper's
             `overflow-auto` is not decoration.

             `Table` sets `w-full`, whose min-width is 0, so the table could
             never be wider than its track: instead of overflowing into the
             scroll container the Own-Container Overflow Rule gives it, it
             crushed the one column with no declared width down to 74px. A
             scroll container with nothing that can exceed it is a region that
             never scrolls — measured `scrollWidth === clientWidth` at every
             width from 390 to 1440.

             44rem is 32.5rem of fixed columns plus 11.5rem for the name, so the
             narrowest legible five-column table. It applies only from `lg`,
             where the columns exist; below that the three remaining columns are
             content-sized and fit, and a min-width would make a phone scroll
             sideways for no gain. At `lg` the track is 728px against this
             704px, so the guard is dormant at every standard width and engages
             when the track is squeezed (a narrower window at that breakpoint, a
             zoom step, a longer version string). -->
        <Table :label="t('packageCatalog.tableLabel')" class="lg:min-w-[44rem]">
          <!-- Body, not Meta. DESIGN.md gives uppercase-and-tracked to labels —
               column heads, state words, nav — and prose to captions, and this
               is a sentence: "7 packages · sorted by last fetch" is 33
               characters of it. Set in caps it was the one run of body text on
               the page that the all-caps rule catches, because caps strips the
               ascenders and descenders a reader recognises words by. The
               tracking goes with it: letterspacing is a small-caps device. -->
          <caption class="caption-top pb-3 text-left text-xs text-muted-foreground">
            {{
              tableCaption
            }}
          </caption>
          <TableHeader>
            <!-- The one solid rule in the table, under the header: the proof
               separates the head from the body with `--rule-strong` and every
               row below with a dashed `--rule-soft`. One weight for "this is
               the boundary", a lighter dashed one for "these are the divisions
               inside it". -->
            <!-- Cells are padded on the right and flush left, per the proof's
               `padding: --s3 --s3 --s3 0`: the column gap belongs *between*
               columns, and a left-flush first column is what lines the table up
               with the caption and the specimen rule above it.

               The fixed widths only apply from `lg`, and they are released
               below it exactly as the proof releases them
               (`.c-state,.c-ver{width:auto}` under 900px) — 9.5rem of State
               inside a 358px box leaves the package name nowhere to go, and the
               columns collide.

               `lg`, not `md`, because that is the width at which the fixed
               columns actually fit *this* layout, measured rather than
               transcribed from the proof's 900px. The four fixed columns claim
               9.5+9+6+8 = 32.5rem (520px), and the catalog track is the
               viewport less the 14rem bay and 4rem of padding. At the `md`
               breakpoint that track is 457px — the whole of it owed to columns
               that want 520 — so the one flexible column was squeezed to 74px
               and `overflow-wrap:anywhere` shredded `strip-ansi` into two
               lines. The header read `PACKAGE VERSION` with no gap. `lg` puts
               728px in the track, which is the first step where the five
               columns are all legible. -->
            <TableRow class="border-b border-solid border-border hover:bg-transparent">
              <TableHead class="pl-0 pr-3 lg:w-[9.5rem]">{{ t("common.state") }}</TableHead>
              <TableHead class="pl-0 pr-3">{{ t("common.package") }}</TableHead>
              <TableHead class="pl-0 pr-3 lg:w-[9rem]">{{ t("common.version") }}</TableHead>
              <!-- Dropped below `lg`, exactly as the proof drops them: at 390px
                 the fixed columns alone claimed 260px of a 358px box, and the
                 two numeric ones are the least load-bearing. -->
              <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-24">{{
                t("common.size")
              }}</TableHead>
              <TableHead class="hidden px-0 text-right lg:table-cell lg:w-32">{{
                t("packageCatalog.lastFetch")
              }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-if="loading" class="border-b-0 hover:bg-transparent">
              <TableCell colspan="5" class="px-0 py-4">
                <Skeleton :lines="6" />
              </TableCell>
            </TableRow>

            <template v-for="row in tableRows" :key="rowId(row)">
              <!-- No `@click` and no `cursor-pointer`: the row is not the
                   control, the name is. A whole-row handler is unreachable by
                   keyboard and invisible to assistive tech, and dressing it as
                   clickable promised a target that only a mouse could take. -->
              <TableRow
                :class="[
                  'border-dashed border-rule-soft align-baseline hover:border-solid hover:border-border',
                  noteKey(row) ? 'border-b-0' : '',
                ]"
              >
                <TableCell class="pl-0 pr-3 py-3 align-baseline">
                  <Resolution :state="rowState(row)" :label="t(STATE_LABEL_KEYS[rowState(row)])" />
                </TableCell>
                <!-- `aria-describedby` is what ties a denial to the package it is
                   about. Without it the note below is a paragraph a screen
                   reader meets after the row and has to relate by proximity.

                   `overflow-wrap:anywhere` is what the proof sets on `.pkg` —
                   not `break-words`. The two differ in exactly the way that
                   matters here: `anywhere` is taken into account when the
                   browser computes the cell's *min-content* width, `break-word`
                   is not. So with `break-words` a name like
                   `github.com/spf13/cobra` still demanded its full width, the
                   table grew past the viewport, and the whole catalog sat
                   behind a horizontal scrollbar at 390px. -->
                <TableCell
                  class="[overflow-wrap:anywhere] pl-0 pr-3 py-3 align-baseline text-base"
                  :aria-describedby="noteKey(row) ? `note-${rowId(row)}` : undefined"
                >
                  <!-- The package name *is* the link — the pattern the version
                       list on the detail page already uses. `anywhere` rather
                       than `break-words` for the same min-content reason as the
                       cell above, and an underline on hover because the row no
                       longer carries any other affordance. -->
                  <RouterLink
                    :to="detailPath(row)"
                    class="[overflow-wrap:anywhere] hover:underline underline-offset-[3px]"
                  >
                    {{ row.name }}
                  </RouterLink>
                  <!-- One list, provenance stated per row. The question a reader
                     actually has is "does this instance already have it?", and
                     splitting the table into two makes that harder to answer,
                     not easier. -->
                  <span
                    v-if="row.kind === 'upstream'"
                    class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
                    >{{ t("packageCatalog.upstreamRow") }}</span
                  >
                  <span
                    v-else-if="!selectedRegistry"
                    class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
                    >{{ row.registry }}</span
                  >
                  <!-- Why this row is here. Shown only for a prose match: a row
                     that matched on its name needs no explanation, and labelling
                     every row would make the one that needs it invisible. -->
                  <span
                    v-if="matchLabel(row)"
                    class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
                    :title="t('packageCatalog.matchedInLabel', { where: matchLabel(row) })"
                    >{{ matchLabel(row) }}</span
                  >
                </TableCell>
                <TableCell class="pl-0 pr-3 py-3 align-baseline text-muted-foreground tabular-nums">
                  <template v-if="row.kind === 'cached'">{{ row.newest_version ?? "—" }}</template>
                  <template v-else>{{ row.latest_version ?? "—" }}</template>
                </TableCell>
                <TableCell
                  class="hidden pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  <template v-if="row.kind === 'cached'">{{
                    formatBytes(row.cached_bytes)
                  }}</template>
                  <template v-else>—</template>
                </TableCell>
                <TableCell
                  class="hidden whitespace-nowrap px-0 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  <template v-if="row.kind === 'cached'">{{
                    formatRelative(row.last_fetched_at, { fallback: "—" })
                  }}</template>
                  <template v-else>—</template>
                </TableCell>
              </TableRow>

              <!-- Every denial states its rule in its own row, tied to the
                 package by `aria-describedby` — not one note floating above the
                 table. The rule is indented under the package rather than under
                 the state, so the eye reads it as belonging to the name. -->
              <!-- The matched fragment, as **text**. It is interpolated, never
                 `v-html`: the README panel's markup boundary is a deliberate,
                 tested, single-component one, and a search snippet is a second
                 surface for package-authored content reached by a much cheaper
                 path — no navigation, just a query (RFC 0007-bis §7.4). -->
              <TableRow
                v-if="rowSnippet(row)"
                class="border-dashed border-rule-soft hover:bg-transparent"
              >
                <TableCell class="pl-0 pr-3 pb-3 pt-0" />
                <TableCell colspan="4" class="pl-0 pr-3 pb-3 pt-0">
                  <p class="flex items-stretch gap-3 text-sm text-muted-foreground">
                    <span class="w-px flex-none bg-border" aria-hidden="true" />
                    <span class="min-w-0 [overflow-wrap:anywhere]">{{ rowSnippet(row) }}</span>
                  </p>
                </TableCell>
              </TableRow>

              <!-- The action for an upstream row, in a row of its own rather
                 than as a chip in a cell — the shape the snippet and the
                 refusal note already use here, and the one the package page
                 arrived at after its own fetch button spent a while as a 24px
                 chip in a table column.

                 The label names the version, which is the version the upstream
                 search returned and the one the cell above shows. Nothing is
                 being chosen on this screen (RFC 0007-bis §11 q3).

                 On success no message is written: the listing and the upstream
                 results are both refetched, the row leaves the upstream half
                 and reappears as a held one, and its state chip is a better
                 answer than a sentence under a row that no longer exists. -->
              <TableRow
                v-if="canFetch(row) || fetchResult[rowId(row)]"
                class="border-dashed border-rule-soft hover:bg-transparent"
              >
                <TableCell class="pl-0 pr-3 pb-3 pt-0" />
                <!-- `colspan="2"` plus the two trailing cells the wide columns
                   use, exactly as the note row below does. A single
                   `colspan="4"` declares five columns on a row whose table
                   renders three below `lg` — the last two `<TableHead>`s are
                   `hidden lg:table-cell` — and the browser then widens the
                   whole table to five, shifting the header away from the body
                   on every viewport under the breakpoint. -->
                <TableCell colspan="2" class="pl-0 pr-3 pb-3 pt-0">
                  <div class="flex flex-wrap items-center gap-3">
                    <!-- The visible label names the version, which is all a
                       reader who can see the row it sits under needs. The
                       accessible name adds the package: a screen-reader user
                       arrives at this button out of that context, and "Fetch
                       9.9.9" on a page of fifty hits names nothing. -->
                    <Button
                      v-if="canFetch(row)"
                      variant="outline"
                      size="sm"
                      :disabled="fetching !== null"
                      :aria-label="
                        t('packageCatalog.fetchVersionLabel', {
                          name: row.name,
                          version: (row as UpstreamRow).latest_version,
                        })
                      "
                      @click="onFetchRow(row as UpstreamRow)"
                    >
                      {{
                        fetching === rowId(row)
                          ? t("packageCatalog.fetching")
                          : t("packageCatalog.fetchVersion", {
                              version: (row as UpstreamRow).latest_version,
                            })
                      }}
                    </Button>
                    <output v-if="fetchResult[rowId(row)]" class="text-sm text-muted-foreground">
                      {{ fetchResult[rowId(row)] }}
                    </output>
                  </div>
                </TableCell>
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
              </TableRow>

              <TableRow
                v-if="noteKey(row)"
                class="border-dashed border-rule-soft hover:bg-transparent"
              >
                <TableCell class="pl-0 pr-3 pb-3 pt-0" />
                <TableCell colspan="2" class="pl-0 pr-3 pb-3 pt-0">
                  <p
                    :id="`note-${rowId(row)}`"
                    class="flex items-stretch gap-3 text-sm text-muted-foreground"
                  >
                    <span class="w-px flex-none bg-border" aria-hidden="true" />
                    <span class="min-w-0 [overflow-wrap:anywhere]">
                      {{ t(noteKey(row)!) }}
                      <!-- Underlined at rest, not on hover. Crimson on the
                           note's dim ink measures 1.28:1 against the surrounding
                           text, so colour alone cannot mark it as a link — axe's
                           `link-in-text-block` wants 3:1 or a non-colour cue,
                           and the design gate failed on exactly that. The proof
                           underlines it too: its links take the browser default
                           and it only sets the offset. -->
                      <RouterLink
                        v-if="row.kind === 'cached'"
                        class="text-primary underline underline-offset-[3px]"
                        :to="detailPath(row)"
                        >{{ t("packageCatalog.noteDetails") }}</RouterLink
                      >
                    </span>
                  </p>
                </TableCell>
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
              </TableRow>
            </template>

            <!-- Upstream loading indicator -->
            <TableRow v-if="loadingUpstream" class="border-b-0 hover:bg-transparent">
              <TableCell colspan="5" class="px-0 py-2 text-center text-xs text-muted-foreground">{{
                t("packageCatalog.searchingUpstreamRegistries")
              }}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <!-- A cap that applied, said out loud. A silently shortened list reads
             as "that is all there is", which is a lie about the catalogue. -->
        <p v-if="searchTruncated" class="mt-2 text-xs text-muted-foreground">
          {{ t("packageCatalog.readmeSearchTruncated", { count: total }) }}
        </p>

        <!-- Empty is two states, and telling them apart is the point: a user
           shown "no packages" while a filter is applied concludes the registry
           is broken.

           Outside the table on purpose: in a `<td colspan>` this inherits the
           table's width, so at 390px the "nothing here" message sat off-screen
           behind a horizontal scroll — the one thing DESIGN.md's Own-Container
           Overflow Rule forbids the body to do. -->
        <div v-if="tableRows.length === 0 && !loadingUpstream && !loading" class="mt-4">
          <!-- A prose search that found nothing gets its own words. "Nothing
             matches that search" would imply the query was checked against every
             package here, and it was checked against the READMEs of the versions
             this instance holds — which is a narrower claim and the honest one
             (RFC 0007-bis §4.3). -->
          <EmptyState
            :filtered="Boolean(search.trim())"
            :title="
              searchedProse
                ? t('packageCatalog.readmeSearchEmptyTitle')
                : search.trim()
                  ? t('catalog.emptyFilteredTitle')
                  : t('catalog.emptyTitle')
            "
            :description="
              searchedProse
                ? t('packageCatalog.readmeSearchEmptyBody')
                : search.trim()
                  ? t('catalog.emptyFilteredBody')
                  : t('catalog.emptyBody')
            "
          >
            <template v-if="search.trim()" #action>
              <Button size="sm" variant="outline" @click="onClearSearch">{{
                t("packageCatalog.clearSearch")
              }}</Button>
            </template>
            <template v-else #action>
              <Button size="sm" variant="outline" as-child>
                <RouterLink to="/setup">{{ t("packageCatalog.pointAToolAt") }}</RouterLink>
              </Button>
            </template>
          </EmptyState>
        </div>

        <!-- Pagination (cached results only) -->
        <div
          v-if="total > perPage"
          class="flex items-center justify-between text-sm text-muted-foreground"
        >
          <span>{{ t("packageCatalog.cachedPackagesTotal", total) }}</span>
          <Pagination :page="page" :total-pages="totalPages" @update:page="goToPage" />
        </div>
      </div>
    </div>
  </div>
</template>
