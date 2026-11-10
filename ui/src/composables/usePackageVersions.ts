import { computed, onBeforeUnmount, onMounted, ref, watch, type Ref } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import { explorePackageDetail, exploreFetchVersion } from "@/client/sdk.gen";
import type {
  ExplorePackageDetailResponse,
  ExploreVersionDto,
  FirewallDto,
} from "@/client/types.gen";
import type { ResolutionState } from "@/components/ui/resolution";
import { extractMessage } from "@/composables/useApi";
import { formatBytes } from "@/lib/format";

/**
 * One package's version list on the detail page: the detail request, the
 * filter, the page, the pre-release toggle, which row is selected, and the
 * fetch of a version this instance does not hold yet.
 *
 * The URL is both an input and an output of that state, so the query sync
 * lives here with it. Registers its own `onMounted` (the first fetch) and
 * `onBeforeUnmount` (the filter debounce), so call it from `setup`.
 */
export function usePackageVersions(registry: Ref<string>, name: Ref<string>) {
  const { t } = useI18n();
  const route = useRoute();
  const router = useRouter();

  const data = ref<ExplorePackageDetailResponse | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  /**
   * The version the README panel follows and the versions table marks.
   *
   * `null` only while the detail request is in flight, and for a package with no
   * versions at all; the endpoint's own `selected_version`/`default_version` set
   * it, the latter preferring the newest version this instance actually holds. A
   * `null` still means "newest that has a README" to the endpoint, which is the
   * right answer for the one case that reaches it.
   */
  const selectedVersion = ref<string | null>(null);

  // What the page selects when the reader has selected nothing — **the newest
  // stable version this instance holds** — is `default_version` in the response
  // now, and the rule lives in `explore/detail.rs`.
  //
  // It was this component's, over the whole list, and the rule does not survive
  // being handed one page of that list: "the first held stable row" read off page
  // one of a package held only at 2.1.0 picks an upstream row, which is the exact
  // defect RFC 0007 §4.2 wrote the rule to fix. One home for it, and the home is
  // the side that can see every version.

  // ── Pre-releases are opt-in ───────────────────────────────────────────────────
  //
  // A release candidate is not what a reader is looking for by default, and on a
  // package like `chalk` the pre-releases outnumber the releases at the top of the
  // list — so the first screenful of the table was versions nobody asked about.
  // Hidden by default, one control to show them, and the count says how many are
  // behind it: a filter that does not say what it removed reads as a short list
  // rather than as a filtered one.
  const showPrereleases = ref(false);

  // ── Filtering and paging a long version list ─────────────────────────────────
  //
  // `chalk` ships 44 versions, `@babel/plugin-transform-runtime` 169, and the
  // table drew every one of them: a page whose useful content — the gate, the
  // README — sat above a list nobody scrolls to the end of, and a browser laying
  // out a hundred-odd rows of buttons and badges on every render.
  //
  // A filter and a pager rather than a "show more": the question a reader has in
  // front of a long version list is "is 4.0.2 here", and scrolling is a poor
  // answer to it.
  //
  // **All three questions are the server's now** — the filter, the page, and which
  // pre-releases to include. They were this component's, over a list the endpoint
  // sent whole, and that arrangement had the endpoint building 169 rows (a
  // vulnerability read and an SBOM read each) so the browser could draw 25 of
  // them. Worse, once the answer is a *page*, a filter applied to it would search
  // only what happened to arrive: "is 4.0.2 here" would be answered *no* about a
  // version this server knows perfectly well it has. So the controls send, and
  // what comes back is already the answer (RFC 0013 §4.3).
  const versionFilter = ref("");
  const versionPage = ref(0);
  /** Set by `applyQuery`, consumed by the first fetch: see `versionQuery`. */
  let letServerChoosePage = false;
  /** What this page asks for. The operator's `[limits].versions_per_page` is the
      ceiling, not this: the console asks for the number of rows it draws, and
      reads back what the server actually applied. */
  const VERSIONS_PER_PAGE = 25;

  /** The rows the table draws — one page of them, exactly as sent. */
  const pagedVersions = computed(() => data.value?.versions ?? []);

  /** The page arithmetic, from the server's totals rather than from the rows in
      hand, which are one page and cannot say how many there are. */
  const versionsPerPage = computed(() => data.value?.versions_page?.per_page ?? VERSIONS_PER_PAGE);
  const filteredTotal = computed(() => data.value?.versions_page?.total ?? 0);
  const unfilteredTotal = computed(() => data.value?.versions_page?.unfiltered_total ?? 0);
  const prereleaseCount = computed(() => data.value?.versions_page?.prerelease_total ?? 0);
  const hiddenPrereleaseCount = computed(() => data.value?.versions_page?.hidden_prereleases ?? 0);

  const versionTotalPages = computed(() =>
    Math.max(1, Math.ceil(filteredTotal.value / versionsPerPage.value)),
  );

  /**
   * Upstream-only rows, for the notice's count.
   *
   * Counted over the rows on screen: the notice says "N version(s) below exist
   * upstream and are not held here", and `below` is the table as drawn. Promising
   * rows a reader would have to turn a page to find would be the same defect in a
   * new place.
   */
  const upstreamVersionCount = computed(
    () => pagedVersions.value.filter((v) => v.source === "upstream").length,
  );

  /*
   * Both controls change *what the pages contain*, so both start over at the
   * first one: a filter that left you on page 4 of a two-page result would look
   * like an empty list.
   *
   * The reset hangs off the gesture rather than off a watcher on the state,
   * because the state has a second author now — the URL. Hydrating `?q=1.5&page=2`
   * sets the filter, and a watcher could not tell that apart from a keystroke, so
   * it would take the page away again on every load of a link that named both.
   */
  function filterVersions(value: string): void {
    versionFilter.value = value;
    versionPage.value = 0;
    // Debounced, because this now costs a request. The pager and the pre-release
    // toggle are not: a click is one intent, and a delay after one reads as the
    // control not working.
    scheduleVersionFetch();
  }

  function togglePrereleases(): void {
    showPrereleases.value = !showPrereleases.value;
    versionPage.value = 0;
    void fetchVersions();
  }

  function turnToPage(page: number): void {
    versionPage.value = page;
    void fetchVersions();
  }

  // ── The version, the filter and the page live in the URL ──────────────────────
  //
  // A version is a destination: "look at 4.0.2 of this package" is a thing one
  // person sends another, and it was unsendable — the selection was component
  // state, so every link to this page landed on whatever the page chose for
  // itself. It also did not survive the page's own Refresh, which re-derived the
  // default and discarded whatever the reader had opened.
  //
  // The filter and the page followed, for the same reason one step further out. A
  // long version list is read by narrowing it, and "the four 4.0.x builds of this
  // package" or "the page where the 2019 releases are" was a position that existed
  // only inside the tab that produced it: reload, Refresh, or paste the address to
  // a colleague, and they got the whole list from the top. RFC 0013 §11 O1 argued
  // the other way — a version is a destination, a page is a position in a session
  // — and the position turned out to be worth sending too.
  //
  // The rule is the catalog's, so the two pages read the same way and use the same
  // key names: the query carries a value **only when it is not the default one**,
  // defaults are omitted, the page is 1-based because a human reads it, and an
  // unrecognised value falls back to the default rather than to nothing. A version
  // this package does not have — a typo, or one yanked since the link was sent —
  // is exactly that case, so the page opens on its default instead of marking no
  // row at all.
  //
  // `replace`, never `push`: reading down a version list is not ten destinations,
  // and a keystroke in the filter is not one either. Pushing would make Back walk
  // through every row the reader clicked and every letter they typed before it
  // returned them to the catalog — the journey RFC 0003 §9's back button exists to
  // make short.

  /** The version the URL asks for, as asked — whether this package has it is the
      server's answer, not one this page can give from a single page of rows. */
  function versionFromQuery(): string | null {
    const asked = Array.isArray(route.query.version) ? route.query.version[0] : route.query.version;
    return typeof asked === "string" && asked ? asked : null;
  }

  /**
   * Read the filter and the page back off the URL.
   *
   * The version is not read here: it is sent to the endpoint, which answers with
   * `selected_version` — the ask, if this package has it — because a typo or a
   * version yanked since the link was sent cannot be told apart from a version on
   * another page by anything holding one page.
   *
   * Anything absent or unrecognised falls to the default the ref is declared with,
   * so a hand-edited URL cannot put the page in a state its own controls could not
   * produce.
   */
  function applyQuery(): void {
    const one = (v: unknown) => (Array.isArray(v) ? v[0] : v);

    const term = one(route.query.q);
    versionFilter.value = typeof term === "string" ? term : "";

    const p = Number(one(route.query.page));
    versionPage.value = Number.isFinite(p) && p > 1 ? Math.floor(p) - 1 : 0;
    // The one request where the server picks the page: a link that named a version
    // and no page opens on the page holding it, and only the server can say which
    // that is. Every later request sends the page the reader is on — including
    // page 1, because a page turned back to is a choice.
    letServerChoosePage = one(route.query.page) === undefined;
  }

  /** What this page's state serialises to. Keys it does not own are carried
      through untouched — it writes its three and reads back everything. */
  function stateAsQuery(): Record<string, unknown> {
    const { version: _v, q: _q, page: _p, ...rest } = route.query;
    const next: Record<string, unknown> = { ...rest };

    const isDefault = selectedVersion.value === (data.value?.default_version ?? null);
    if (selectedVersion.value && !isDefault) next.version = selectedVersion.value;
    if (versionFilter.value.trim()) next.q = versionFilter.value;
    if (versionPage.value > 0) next.page = String(versionPage.value + 1); // 1-based for a human

    return next;
  }

  function syncQuery(): void {
    // Only ever writes to a package's own route: a detail request settling after
    // the reader has moved on must not rewrite the address they moved to.
    if (!route.path.startsWith("/packages/")) return;

    const next = stateAsQuery();
    const current = route.query;
    const same =
      Object.keys(next).length === Object.keys(current).length &&
      Object.entries(next).every(([k, v]) => current[k] === v);
    // Vue Router rejects a redundant navigation, and writing one per keystroke
    // would churn the address bar for no change.
    if (!same) void router.replace({ path: route.path, query: next as Record<string, string> });
  }

  watch([selectedVersion, versionFilter, versionPage], syncQuery);

  /**
   * The URL is also an *input* to the selection, not only an output of it.
   *
   * Selecting a version is a `RouterLink` now rather than a click handler on the
   * `<tr>` — the row was mouse-only, with no `tabindex`, no `role` and no key
   * handler, while `aria-current` announced a selection a keyboard user could not
   * change. A link gets the keyboard, the focus ring, middle-click-to-new-tab and
   * a copyable target for free, and it removes the six buttons nested inside a
   * clickable row.
   *
   * That inverts the flow: the link writes `?version=`, and this reads it back.
   * It cannot loop with `syncQuery` — that one omits the key when the selection is
   * the default, and this one ignores an absent key, so the pair settles either
   * way in one pass.
   */
  watch(
    () => route.query.version,
    (asked) => {
      const one = Array.isArray(asked) ? asked[0] : asked;
      if (typeof one !== "string" || !one || one === selectedVersion.value) return;
      selectedVersion.value = one;

      // **And go and get it, if the page cannot show it.**
      //
      // Not selecting a row costs a request, which is why this is guarded by
      // `selectedRow` rather than run unconditionally: for a version already on
      // screen the rows *are* the answer, and re-asking would be a round trip to
      // be told what the page is holding.
      //
      // But a version that is on neither the rendered page nor `selected` — a
      // `?version=` arriving from somewhere that did not render this table, which
      // is exactly what Administration → *View* does for a version on page 3 —
      // left `selectedRow` null, and `v-if="selectedRow"` then removed the state
      // chip, the blocked reason, the vulnerabilities, the install line, the
      // download and SBOM links and the Fetch button, while `ReadmePanel` went on
      // loading below the hole. A page that answers about a version by showing
      // nothing about it is worse than the request.
      //
      // `fetchVersions` and not `fetchDetail`: this changes one card beside a
      // README, and the page-wide loading state would blank the other half.
      // `versionQuery` sends `selectedVersion`, which is now the asked one, so the
      // server picks the page that holds it.
      if (!selectedRow.value) void fetchVersions();
    },
  );

  /** Where the row's version link points: this page, with that version selected. */
  function versionLink(version: string) {
    return { path: route.path, query: { ...route.query, version } };
  }

  /**
   * Which version is being fetched, and what came of the last attempt
   * (RFC 0007-bis §4.4).
   *
   * Keyed by version rather than a single flag: a reader may press one row while
   * reading another, and a shared spinner would put the wrong row in motion.
   */
  const fetching = ref<string | null>(null);
  const fetchResult = ref<Record<string, string>>({});

  /**
   * Ask this instance to fetch one version from upstream.
   *
   * Synchronous, with a spinner. Measured against real upstreams the median
   * version is 0.57 MB in 66 ms and the largest sampled was 41.7 MB in 417 ms
   * (RFC 0007-bis §13.4), which a spinner holds comfortably — and the response
   * reports the size, so the row can say what the wait bought rather than just
   * "done".
   */
  async function onFetchVersion(version: string) {
    if (fetching.value) return;
    fetching.value = version;
    delete fetchResult.value[version];
    try {
      const { data: res, error: apiErr } = await exploreFetchVersion({
        path: { registry: registry.value, name: name.value, version },
      });
      if (apiErr) {
        // The rule's own reason, shown verbatim. It is the same string the
        // download would have given, so the operator can take it to the RBAC
        // simulator and get the same verdict explained.
        const body = apiErr as { code?: string; message?: string };
        let failure: string;
        if (body.code === "fetch.already-held") {
          failure = t("packageDetailPage.fetchAlreadyHeld");
        } else if (body.message) {
          failure = t("packageDetailPage.fetchDenied", { reason: body.message });
        } else {
          failure = t("packageDetailPage.fetchFailed");
        }
        fetchResult.value[version] = failure;
        return;
      }
      const size = (res as { size_bytes?: number }).size_bytes ?? 0;
      // Refresh so the row says `proxied` rather than leaving the reader to
      // reload and wonder whether it worked — *then* say what arrived. The
      // refresh clears stale outcomes, so a message written before it would be
      // the one thing on screen that the refresh erased.
      await fetchDetail();
      fetchResult.value[version] = t("packageDetailPage.fetched", {
        size: formatBytes(size),
      });
    } catch (e) {
      fetchResult.value[version] = extractMessage(e);
    } finally {
      fetching.value = null;
    }
  }

  /**
   * What the version controls send.
   *
   * `version` goes on every request, not only when a link named one: it is what
   * keeps a selected pre-release in a list that hides them, and what lets the
   * endpoint answer with the page holding it.
   */
  function versionQuery(): Record<string, string | number> {
    const q: Record<string, string | number> = {
      per_page: VERSIONS_PER_PAGE,
      prereleases: showPrereleases.value ? "show" : "hide",
    };
    if (!letServerChoosePage) q.page = versionPage.value;
    if (versionFilter.value.trim()) q.q = versionFilter.value.trim();
    const pinned = selectedVersion.value ?? versionFromQuery();
    if (pinned) q.version = pinned;
    return q;
  }

  /**
   * The one fetch, in two moods.
   *
   * `silent` is what the version controls use: they change one card on a page
   * whose other half is a README, and flipping the whole page into its loading
   * state for a keystroke would blank that README, drop the pager, and take the
   * focus out of the filter box the reader is still typing in. The rows are
   * simply replaced when the answer lands.
   *
   * `seq` because a filter is typed: a slow answer for `1.` must never overwrite
   * a fast one for `1.55`, and the last request sent is the only one whose answer
   * is still the truth.
   */
  let versionSeq = 0;
  /** The `versionSeq` of the fetch that currently owns `loading`. */
  let loadingSeq = 0;

  async function fetchDetail(opts: { silent?: boolean } = {}) {
    const seq = ++versionSeq;
    if (!opts.silent) {
      loadingSeq = seq;
      loading.value = true;
      error.value = null;
    }
    try {
      const { data: res, error: apiErr } = await explorePackageDetail({
        path: { registry: registry.value, name: name.value },
        query: versionQuery(),
      });
      if (seq !== versionSeq) return; // superseded by a later keystroke or click
      // The server's own words, not the literal `HTTP error` this used to throw.
      // That literal made a 404 (wrong package), a 403 (no access) and a 502
      // (upstream down) render identically, with everything needed to tell them
      // apart sitting in `apiErr` and being discarded one line earlier.
      if (apiErr) throw apiErr;
      data.value = res as ExplorePackageDetailResponse;
      // A stale outcome must not sit beside a row that has since changed: "Refused:
      // …" left over from the previous answer would be describing a version this
      // response may now report as held.
      fetchResult.value = {};
      letServerChoosePage = false;
      // Read back rather than assume: the server clamps a page past the end, caps
      // `per_page` at the operator's ceiling, and picks the page itself when a
      // link named a version and no page.
      versionPage.value = data.value.versions_page?.page ?? 0;
      // An unknown version — a typo, or one yanked since the link was sent —
      // comes back as `selected_version: null`, and falls back rather than
      // marking no row at all.
      selectedVersion.value = data.value.selected_version ?? data.value.default_version ?? null;
    } catch (e) {
      if (seq !== versionSeq) return;
      // A silent fetch must not take the page down.
      //
      // The template's `v-else-if="error"` sits above `v-else-if="data"`, so
      // setting `error` here replaced the whole rendered page — README panel,
      // gate card, versions table and the filter box the reader is still typing
      // in — with one line of error text. `error` is only cleared by a
      // *non*-silent fetch, and the Refresh button lives inside the `data`
      // branch that just disappeared, so there was no way back short of
      // reloading the browser. One 502 on one keystroke, and the page is gone.
      //
      // Leaving the rows the page already has is what `silent` means: it changes
      // one card, and a card that could not be changed is the previous card.
      if (opts.silent && data.value) {
        console.warn("package detail: silent refresh failed, keeping the rendered page", e);
        return;
      }
      error.value = extractMessage(e);
    } finally {
      // Keyed on `loadingSeq`, not on `versionSeq`. A non-silent fetch superseded
      // by a *silent* one — Refresh, then any version control — failed the
      // `seq === versionSeq` test and skipped this line, leaving `loading` true
      // forever: it gates the whole reader half of the page, so Refresh and Retry
      // both left the DOM and only a browser reload recovered. `loadingSeq` moves
      // only when a non-silent fetch takes the flag, so a silent successor cannot
      // orphan it and a non-silent one still owns clearing it.
      if (!opts.silent && loadingSeq === seq) loading.value = false;
    }
  }

  /** A version-control change: same request, no page-wide loading state. */
  function fetchVersions() {
    return fetchDetail({ silent: true });
  }

  let filterTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Whether a keystroke is still on its way to an answer.
   *
   * `fetchVersions` is deliberately silent — it must not blank the README or take
   * the focus out of the box being typed in — and "silent" had been read as "says
   * nothing at all": for 300 ms of debounce plus a round trip the page gave no
   * sign it had heard, so a reader typed the term again. The status line beside
   * the filter is where that belongs; it is already a live region and it is
   * already about this list.
   */
  const filterPending = ref(false);

  /**
   * Which scheduled fetch owns `filterPending`.
   *
   * The debounce cancels the *timer*, not a request already in flight: type, wait
   * out the 300 ms, then type again while the first round trip is still open, and
   * the first one's `finally` clears the flag — so the status line drops back to
   * "42 of 44 shown" and reads as settled while the answer to what was actually
   * typed is still on its way. Same `seq` guard `fetchDetail` and the catalogue
   * already use.
   */
  let filterSeq = 0;

  /** Typing is debounced because it now costs a request; 300 ms, the catalog's
      number, so the two search boxes feel the same. */
  function scheduleVersionFetch() {
    if (filterTimer) clearTimeout(filterTimer);
    filterPending.value = true;
    const seq = ++filterSeq;
    filterTimer = setTimeout(() => {
      filterTimer = null;
      void fetchVersions().finally(() => {
        if (seq === filterSeq) filterPending.value = false;
      });
    }, 300);
  }

  // Routing away within 300 ms of a keystroke would otherwise run the fetch
  // against a destroyed component — the same footgun the catalog fixed.
  onBeforeUnmount(() => {
    if (filterTimer) clearTimeout(filterTimer);
  });

  // ── The subject: the selected version ─────────────────────────────────────────
  //
  // The page used to be a table with a README under it, and the reader's question
  // — *why was this blocked*, *what do I paste* — is answered by exactly one
  // version. So the selected version is the subject now and the table is the index
  // below it, which is also what makes the signature state matrix load-bearing:
  // one row, drawn large, with its own state, its own rule and its own snippet.
  //
  // The page's own rows first, `selected` second, and in that order because
  // selecting a version does **not** issue a request.
  //
  // Clicking a version is a `RouterLink` that writes `?version=`; a watcher reads
  // it back into `selectedVersion` and nothing else happens — the rows on screen
  // are already the answer, so re-asking the server for them would be a round trip
  // to be told what the page is holding. But `data.selected` is the server's
  // grading of whichever version was selected *when the response was built*, so
  // reading the subject from it alone left the heading, the state chip, the
  // refusal reason, the advisories, the download and SBOM links and the fetch
  // button all describing the previously selected version, while the install line,
  // the README panel and the row's own highlight had moved on. The sticky
  // breadcrumb — the strip that exists to make the selection visible from
  // anywhere — was the loudest of them.
  //
  // `selected` is still the fallback and still necessary: the endpoint pins the
  // selection into the *first* page only, so a link the page produces itself
  // (`?version=X&page=4`) names a version no row on the page carries. Both are
  // enriched by the same `enrich_page` funnel server-side, so the two are the same
  // row when they are both present.
  const selectedRow = computed<ExploreVersionDto | null>(() => {
    const answer = data.value;
    if (!answer) return null;
    const want = selectedVersion.value;
    if (!want) return answer.selected ?? null;
    const onPage = answer.versions.find((row) => row.version === want);
    if (onPage) return onPage;
    return answer.selected?.version === want ? answer.selected : null;
  });

  onMounted(() => {
    // Before the request, not after: the query *is* the initial state, so the list
    // must arrive into the filter and the page the URL asked for. Hydrating
    // afterwards would draw the whole list from the top and then move it.
    applyQuery();
    void fetchDetail();
  });

  return {
    data,
    loading,
    error,
    selectedVersion,
    selectedRow,
    showPrereleases,
    versionFilter,
    versionPage,
    pagedVersions,
    filteredTotal,
    unfilteredTotal,
    prereleaseCount,
    hiddenPrereleaseCount,
    versionTotalPages,
    upstreamVersionCount,
    filterPending,
    filterVersions,
    togglePrereleases,
    turnToPage,
    versionLink,
    fetching,
    fetchResult,
    onFetchVersion,
    fetchDetail,
  };
}

/**
 * The row's state, as the server graded it, in the vocabulary the catalogue's
 * rows already use.
 *
 * This used to be derived here from `firewall` alone, over three values, and it
 * was wrong in the one place it mattered: an upstream-only candidate carries
 * `Clear` — the firewall has nothing to refuse about a version nobody can
 * download from here yet — so a version this instance holds **no bytes of**
 * rendered as a full 3×3 matrix in ink, DESIGN.md's *held and verified*, right
 * beside a badge reading "not held here". The signature device of the whole
 * system was lying on precisely the rows the fetch button exists for.
 *
 * `ExploreVersionDto.state` is now graded on the side that knows what is held,
 * exactly as `ExploreEntry::state` already is for the catalogue. The fallback is
 * defensive only, for a response predating the field.
 */
export function rowState(row: { state?: string; firewall: FirewallDto }): ResolutionState {
  const graded = row.state as ResolutionState | undefined;
  if (graded && graded in STATE_LABEL_KEYS) return graded;
  if (row.firewall.status === "blocked") return "blocked";
  if (row.firewall.status === "yanked") return "yanked";
  return "cached";
}

/**
 * One vocabulary, shared with the catalogue.
 *
 * The two pages described the same versions in two languages — `Cached / Stale /
 * Held / Pending / Yanked / Blocked` on the index that points here, and
 * `Clear / (yanked) / Blocked` on the destination. A reader who followed a link
 * had to translate.
 */
export const STATE_LABEL_KEYS: Record<ResolutionState, string> = {
  cached: "resolution.cached",
  stale: "resolution.stale",
  held: "resolution.held",
  pending: "resolution.pending",
  yanked: "resolution.yanked",
  blocked: "resolution.blocked",
};

/**
 * Whether a row describes a version this instance holds no bytes for.
 *
 * Every cell that would be a fact about what we hold reads *unknown* on such a
 * row rather than `0` or `—`: nobody has downloaded it *through here*, which is
 * not the same as nobody having downloaded it (RFC 0007 §4.2).
 */
export function isUpstreamOnly(source: string) {
  return source === "upstream";
}
