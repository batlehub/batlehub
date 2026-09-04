<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, computed, onMounted, onBeforeUnmount, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ArrowLeft, Check, Copy, FileJson, FileCode, Download, Search } from "@lucide/vue";
import {
  explorePackageDetail,
  exploreFetchVersion,
  listRegistries,
  packageDetail,
} from "@/client/sdk.gen";
import type {
  ExplorePackageDetailResponse,
  ExploreVersionDto,
  FirewallDto,
  PackageDetailResponse,
  RegistryInfo,
} from "@/client/types.gen";
import { installCommandFor } from "@/config/registryTypes";
import { useAuth } from "@/composables/useAuth";
import PackageVersionsTable from "@/components/admin/PackageVersionsTable.vue";
import PackageBetaChannel from "@/components/admin/PackageBetaChannel.vue";
import PackageVisibility from "@/components/admin/PackageVisibility.vue";
import PackageGrants from "@/components/admin/PackageGrants.vue";
import PackageEventsTable from "@/components/admin/PackageEventsTable.vue";
import ReadmePanel from "@/components/package/ReadmePanel.vue";
import VerdictPanel from "@/components/package/VerdictPanel.vue";
import MovingRefsPanel from "@/components/package/MovingRefsPanel.vue";
import UpstreamNotice from "@/components/package/UpstreamNotice.vue";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { useApi, extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import { formatBytes, formatCount, formatDay } from "@/lib/format";
import { severityVariant } from "@/lib/badge-variants";
import { Badge } from "@/components/ui/badge";
import { Resolution, type ResolutionState } from "@/components/ui/resolution";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Pagination } from "@/components/ui/pagination";
import {
  Table,
  TableHeader,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

const { token, isAdmin, isAuthenticated } = useAuth();
const { authFetch } = useAuthFetch();
const route = useRoute();
const router = useRouter();

const registry = computed(() => String(route.params.registry ?? ""));
const name = computed(() => String(route.params.name ?? ""));

const { data: registriesList } = useApi<RegistryInfo[]>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const registryType = computed(
  () => registriesList.value?.find((r) => r.name === registry.value)?.type ?? null,
);

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

/**
 * Whether a row describes a version this instance holds no bytes for.
 *
 * Every cell that would be a fact about what we hold reads *unknown* on such a
 * row rather than `0` or `—`: nobody has downloaded it *through here*, which is
 * not the same as nobody having downloaded it (RFC 0007 §4.2).
 */
/**
 * A URL shortened to the part a reader recognises: `github.com/owner/repo`.
 *
 * The scheme is noise on every one of these — they are all `https` by the time
 * the server has finished with them — and a trailing slash makes two links to
 * the same page look like two pages. The full URL stays in `href`, so hovering
 * and copying give what was actually declared.
 *
 * Falls back to the raw string if `URL` cannot parse it. That should not happen
 * (the server allow-lists these to http(s)) and it is not a security decision
 * either way — this is a *label*, and the `href` is what a click follows.
 */
function linkLabel(url: string): string {
  try {
    const parsed = new URL(url);
    return `${parsed.host}${parsed.pathname}`.replace(/\/$/, "");
  } catch {
    return url;
  }
}

function isUpstreamOnly(source: string) {
  return source === "upstream";
}

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

// ── Per-artifact SBOM download ─────────────────────────────────────────────

const sbomLoading = ref<string | null>(null); // "registry/name/version:format"
/**
 * Formats that answered `404` since the page loaded, keyed
 * `registry/name/version:format`.
 *
 * A second line of defence, not the answer: the server now says up front which
 * formats it holds (`version.sbom`), so a button is only ever drawn for one that
 * exists. This still exists because an SBOM can be re-recorded — or a version
 * yanked — between the response and the click, and a format that has just gone
 * should stop being offered rather than keep failing.
 *
 * Keyed per **format**: it used to be keyed per artifact, so one missing SPDX
 * removed the CycloneDX button too.
 */
const sbomMissing = ref<Set<string>>(new Set());
/** The SBOM error a reader can act on, keyed by artifact. */
const sbomError = ref<Record<string, string>>({});

const artifactKey = (version: string) => `${registry.value}/${name.value}/${version}`;
const formatKey = (version: string, fmt: string) => `${artifactKey(version)}:${fmt}`;
const withoutKey = (set: Set<string>, key: string) => {
  if (!set.has(key)) return set;
  const next = new Set(set);
  next.delete(key);
  return next;
};

async function downloadSbom(version: string, fmt: "spdx" | "cyclonedx") {
  const key = `${registry.value}/${name.value}/${version}:${fmt}`;
  sbomLoading.value = key;
  delete sbomError.value[artifactKey(version)];
  try {
    const ext = fmt === "cyclonedx" ? "cyclonedx.json" : "spdx.json";
    const url = `/api/v1/sbom/${encodeURIComponent(registry.value)}/${encodeURIComponent(name.value)}/${encodeURIComponent(version)}?format=${fmt}`;
    const resp = await authFetch(`${API_BASE_URL}${url}`);
    if (resp.status === 404) {
      sbomMissing.value = new Set([...sbomMissing.value, formatKey(version, fmt)]);
      return;
    }
    // A format that *is* there clears its own mark, which was set once and never
    // lifted: one 404 on SPDX hid the CycloneDX button too, for the rest of the
    // session, including after a fetch had populated the row.
    if (resp.ok) sbomMissing.value = withoutKey(sbomMissing.value, formatKey(version, fmt));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const disposition = resp.headers.get("Content-Disposition") ?? "";
    const match = disposition.match(/filename="([^"]+)"/);
    const filename = match?.[1] ?? `${name.value}-${version}.${ext}`;
    const blob = await resp.blob();
    const a = Object.assign(document.createElement("a"), {
      href: URL.createObjectURL(blob),
      download: filename,
    });
    a.click();
    URL.revokeObjectURL(a.href);
  } catch (e) {
    // Said, not swallowed. This was a bare `catch {}`: every failure that was
    // not a 404 — a 502, an expired session, a network drop — produced a click
    // that did nothing, forever, with no way for the reader to tell a missing
    // SBOM from a broken one.
    sbomError.value[artifactKey(version)] = extractMessage(e);
  } finally {
    sbomLoading.value = null;
  }
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
  if (copyTimer) clearTimeout(copyTimer);
});

/**
 * Back to the catalog **as the reader left it**, not to a fresh one.
 *
 * This pushed `/packages?registry=…`, which looked like it preserved something
 * and did not: the catalog never read `route.query`, so the registry was dropped
 * on arrival along with the search, the scope, the sort and the page. Opening a
 * package from the fifth page of a search and pressing Back gave an empty search
 * box and page one.
 *
 * `router.back()` when the previous entry is the catalog, because that restores
 * the URL *and* the scroll offset (`scrollBehavior` in `router/index.ts` returns
 * the saved position on a pop) — and the catalog now carries its whole state in
 * the query, so the URL is the search. Rebuilding a location by hand would
 * reconstruct the query and still lose the offset.
 *
 * The fallback is for arriving without that history: a pasted link, a refresh, a
 * new tab. `history.state.back` is Vue Router's own record of the previous
 * entry, so this asks the question directly rather than guessing from
 * `document.referrer`. A detail page also starts with `/packages`, hence the
 * exact-path test — going "back" from one package to another would be a
 * surprise, not a return.
 */
function isCatalogEntry(entry: unknown): boolean {
  if (typeof entry !== "string") return false;
  return entry.split("?")[0].split("#")[0] === "/packages";
}

function goBack() {
  if (isCatalogEntry(window.history.state?.back)) {
    router.back();
    return;
  }
  router.push({ path: "/packages", query: { registry: registry.value } });
}

/**
 * The first crumb is a link that also knows how to *return*.
 *
 * A plain click runs `goBack()` — which pops the history entry when the catalog
 * is the one behind us, and that is what restores its search, scope, sort, page
 * *and* scroll offset. A modified click (new tab, new window, download) is left
 * to the browser, and so is the href, which is why this is an `<a>` and not the
 * `<button>` it used to be: a control that navigates and cannot be middle-clicked
 * is a link wearing the wrong element.
 */
/** The href the first crumb carries, so it is a real link before it is a handler. */
const catalogHref = computed(() => router.resolve({ path: "/packages" }).href);

function onCrumbToCatalog(event: MouseEvent) {
  if (event.defaultPrevented) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0)
    return;
  event.preventDefault();
  goBack();
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
function rowState(row: { state?: string; firewall: FirewallDto }): ResolutionState {
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
const STATE_LABEL_KEYS: Record<ResolutionState, string> = {
  cached: "resolution.cached",
  stale: "resolution.stale",
  held: "resolution.held",
  pending: "resolution.pending",
  yanked: "resolution.yanked",
  blocked: "resolution.blocked",
};

const stateLabel = (row: { state?: string; firewall: FirewallDto }) =>
  t(STATE_LABEL_KEYS[rowState(row)]);

/**
 * The blocked variant of a firewall status, or `null`.
 *
 * In the script rather than in the template because this is where TypeScript can
 * narrow the union: `FirewallDto` is a discriminated union that *does* declare
 * `reason`, `blocked_by` and `blocked_at`, and the template read them through
 * three `as any` casts because a `v-if` on one element does not narrow a type
 * for a sibling. The casts meant the one part of this page that renders a
 * policy decision was the one part unchecked against the OpenAPI contract.
 */
function blockedInfo(fw: FirewallDto) {
  return fw.status === "blocked" ? fw : null;
}

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

/**
 * The SBOM formats the selected version can actually be downloaded in.
 *
 * The server answers this per version (`sbom.formats`), because it is the only
 * side that knows: which formats exist is decided per registry by
 * `[registries.sbom].formats`, and whether *this* version has any depends on
 * whether this instance has ever held its bytes. The page used to draw both
 * buttons unconditionally and find out on the click — so an SPDX-only registry
 * offered a CycloneDX download that could only 404, and every upstream-only row
 * offered two.
 */
const sbomFormats = computed<string[]>(() => {
  const row = selectedRow.value;
  if (!row || row.sbom?.state !== "available") return [];
  return (row.sbom.formats ?? []).filter(
    (fmt) => !sbomMissing.value.has(formatKey(row.version, fmt)),
  );
});

/**
 * What to say about the selected version's SBOM: offer it, say there is none, or
 * say it does not exist *yet*.
 *
 * `unknown` is not a hedge — it is the honest answer for a version this instance
 * holds no bytes of, where the SBOM is generated on the way in. Rendering that
 * as "no SBOM" would state as a fact about the package something that is only a
 * fact about our cache.
 */
const sbomState = computed<"available" | "none" | "unknown">(() => {
  const row = selectedRow.value;
  if (!row) return "unknown";
  if (sbomFormats.value.length > 0) return "available";
  // The server offered formats and every one of them has since 404'd. That is a
  // definite "none" now, whatever the response said when it was built.
  if (row.sbom?.state === "available") return "none";
  return row.sbom?.state === "none" ? "none" : "unknown";
});

/** The one line that installs the selected version, when its type has an honest one. */
const installLine = computed(() =>
  selectedVersion.value
    ? installCommandFor(registryType.value, name.value, selectedVersion.value)
    : null,
);

/**
 * Copy confirmed in place, and reset.
 *
 * A copy that says nothing leaves the reader wondering whether it took; a copy
 * that says so forever stops being feedback. Two seconds, and no motion — the
 * system animates exactly one thing and this is not it.
 */
const copied = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | null = null;

async function copyInstall() {
  if (!installLine.value) return;
  try {
    await navigator.clipboard.writeText(installLine.value.command);
    copied.value = true;
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => {
      copied.value = false;
      copyTimer = null;
    }, 2000);
  } catch {
    // A clipboard the browser refuses (no permission, no secure context) is not
    // an error worth a banner: the command is on screen and selectable, which is
    // the fallback every terminal user already uses.
  }
}

// ── Download URL construction ──────────────────────────────────────────────────

/**
 * Build the proxy download URL for a given version based on the registry type.
 * Returns null for registries whose URL can't be derived purely from name/version
 * (Maven, PyPI simple page, etc.).
 */
function downloadUrl(version: string): string | null {
  const n = name.value;
  const r = registry.value;
  const base = `${API_BASE_URL}/proxy/${encodeURIComponent(r)}`;
  switch (registryType.value) {
    case "cargo":
      return `${base}/${encodeURIComponent(n)}/${encodeURIComponent(version)}/download`;
    case "npm":
      // encodeURIComponent handles scoped packages: @scope/pkg → %40scope%2Fpkg (one path segment)
      return `${base}/${encodeURIComponent(n)}/${encodeURIComponent(version)}/tarball`;
    case "nuget":
      return `${base}/nuget/v3/flat/${encodeURIComponent(n.toLowerCase())}/${encodeURIComponent(version.toLowerCase())}/${encodeURIComponent(n.toLowerCase())}.${encodeURIComponent(version.toLowerCase())}.nupkg`;
    case "rubygems":
      return `${base}/gems/${encodeURIComponent(n)}-${encodeURIComponent(version)}.gem`;
    case "pypi":
      // PyPI has hashed filenames — link to the simple page instead
      return `${base}/simple/${encodeURIComponent(n)}/`;
    case "conda":
      return `${base}/noarch/${encodeURIComponent(n)}-${encodeURIComponent(version)}-py_0.conda`;
    case "vsix":
    case "openvsx": {
      const parts = n.split(".");
      if (parts.length === 2) {
        return `${base}/${encodeURIComponent(parts[0])}.${encodeURIComponent(parts[1])}/${encodeURIComponent(version)}/vsix`;
      }
      return null;
    }
    default:
      return null;
  }
}

onMounted(() => {
  // Before the request, not after: the query *is* the initial state, so the list
  // must arrive into the filter and the page the URL asked for. Hydrating
  // afterwards would draw the whole list from the top and then move it.
  applyQuery();
  void fetchDetail();
});

/**
 * Administration is a *section of this page* now, not a parallel page at another
 * URL with its own layout and its own back button. One package, one address
 * (RFC 0003 §4.2).
 *
 * Fetched only when the viewer is an admin: the endpoint is admin-only
 * server-side, so firing it for everyone would mean a 403 on every package view.
 * Hiding the section is a rendering decision — the server still refuses.
 */
const {
  data: adminData,
  error: adminError,
  reload: reloadAdmin,
} = useApi<PackageDetailResponse>(
  () =>
    isAdmin.value
      ? (packageDetail({
          query: { registry: registry.value, name: name.value },
        }) as Promise<{
          data?: unknown;
          error?: unknown;
        }>)
      : Promise.resolve({ data: undefined }),
  [token, registry, name],
);
</script>

<template>
  <!-- Full-bleed, like the masthead, the footer and the catalog.

       This was `max-w-4xl`: 896px of page no matter how wide the window, so at
       1920 the content stopped 1009px short of the rails the header and footer
       draw, and the versions table — nine columns, of which SECURITY, SBOM and
       DOWNLOAD are the ones an operator is here for — sat inside 846px and
       scrolled them out of sight. The measure was also only half applied: the
       whole Administration section below already renders *outside* this div and
       ran edge to edge, so an admin was reading one page drawn to two different
       widths.

       No page-level measure replaces it. That is the rule App.vue states when
       it drops the global container: a page sets no width, and the surfaces
       that need one set their own — the specimen caption at `72ch`,
       `PageHeader` and `EmptyState` at `64ch`, and now the README body, which
       is the only long-form prose here (see `ReadmePanel.vue`). A table and a
       paragraph do not want the same width, which is exactly what one cap on
       the page was giving them. -->
  <div class="space-y-6">
    <!-- ── The trail ────────────────────────────────────────────────────────
         A back button became a breadcrumb, and the breadcrumb is sticky.

         It answers three separate complaints with one element. (1) It is real
         navigation: `Packages` was a `<button>` calling `goBack()`, so it had no
         href, could not be middle-clicked and told a reader nothing about where
         they were. (2) It carries **the selected version and its state**, and
         that is the point of the stickiness: the index sits below a README of
         arbitrary length, so clicking a version used to change a region the
         reader could no longer see — now the change is always on screen, in the
         one strip that never scrolls away. (3) It restores the location channel
         the console had nowhere else on this route.

         Sticky with a hairline and the page's own ground — no shadow, because
         nothing in this world casts one at rest, and a strip that needed a
         shadow to separate itself would be a strip that is not a rule.

         The first crumb is a plain `<a>` with a resolved href and its own click
         handler, deliberately not a `RouterLink`: a fallthrough `@click` on one
         is merged *after* the component's own `navigate`, so it would push
         before this handler could decline — the href is what makes it a link
         (middle-click, copy, focus), and `goBack()` is what restores the
         catalog's search, scope, sort, page and scroll offset when the reader
         came from there. A modified click stays the browser's.

         `top-16 z-30` is the offset the console's other two sticky bars already
         use (`AdminPackages.vue`, `PackageVersionsTable.vue`). `top-0 z-10`
         pinned this at viewport y=0 — the same line `AppHeader`'s `sticky top-0
         z-40` opaque bar occupies — so the crumb, the version and the Resolution
         chip vanished under the masthead the moment the page scrolled, which is
         the opposite of what the note above claims for it. -->
    <nav
      :aria-label="t('packageDetailPage.breadcrumb')"
      class="sticky top-16 z-30 -mx-4 border-b border-rule-soft bg-background px-4 py-3 md:-mx-6 md:px-6"
    >
      <ol class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
        <li>
          <a
            :href="catalogHref"
            class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px]"
            @click="onCrumbToCatalog"
          >
            <ArrowLeft class="h-4 w-4" aria-hidden="true" />
            {{ t("packageDetailPage.backToCatalog") }}
          </a>
        </li>
        <li aria-hidden="true" class="text-muted-foreground">/</li>
        <li>
          <RouterLink
            :to="{ path: '/packages', query: { registry } }"
            class="inline-block py-1 text-primary underline underline-offset-[3px]"
            >{{ registry }}</RouterLink
          >
        </li>
        <li aria-hidden="true" class="text-muted-foreground">/</li>
        <li class="min-w-0 [overflow-wrap:anywhere] font-mono text-foreground" aria-current="page">
          {{ name }}
        </li>
        <!-- The version, and its state, pinned to the top of the viewport. This
             is the selection feedback; without it a click twelve rows down the
             index changes only what is off screen above. -->
        <template v-if="selectedRow">
          <li aria-hidden="true" class="text-muted-foreground">/</li>
          <li class="font-mono text-foreground">{{ selectedRow.version }}</li>
          <li>
            <Resolution :state="rowState(selectedRow)" :label="stateLabel(selectedRow)" />
          </li>
        </template>
      </ol>
    </nav>

    <!-- Loading and error are *panels*, which is the only bounded box this
         system has besides the settings popover — 1px rule, 40/24 padding, a
         Pixel Small heading, copy capped at 64ch.

         Both used to be one bare `<p>`. The loading one rendered an empty body
         with no live region at all, so a screen reader was told nothing was
         happening; the error one printed the literal `HTTP error` with no
         heading, no `role`, no status code and no way back — the end of the
         journey, and therefore the thing the visit is remembered by. -->
    <template v-if="loading">
      <EmptyState
        class="mx-auto max-w-[64ch]"
        role="status"
        aria-live="polite"
        :title="t('packageDetailPage.loading')"
        :description="t('packageDetailPage.loadingHint', { name })"
      />
    </template>

    <template v-else-if="error">
      <EmptyState
        class="mx-auto max-w-[64ch] border-destructive"
        role="alert"
        :title="t('packageDetailPage.couldNotLoad', { registry, name })"
        :description="error"
      >
        <template #action>
          <Button variant="outline" size="sm" @click="fetchDetail()">
            {{ t("common.retry") }}
          </Button>
        </template>
      </EmptyState>
    </template>

    <template v-else-if="data">
      <!-- The specimen, as the catalog builds it: the page announces its
           subject at the Display step, and the facts sit under it as a caption.

           DESIGN.md gives Display ("Silkscreen 700, 56 → 72 → 88 → 104 per
           breakpoint, line-height 0.92, 0.02em") to *the subject at the top of
           the sheet, one per view*. This h1 was `text-2xl font-mono` — 24px of
           JetBrains Mono — so the one page dedicated to a single package set its
           name in the reading face at a size the doc reserves for the wordmark,
           and the rendered gate reports `h1 not in the display face` at every
           width and role. The catalog spends this step on the registry; here the
           subject is the package, so it spends it on the package.

           **No `uppercase`, and the caption carries the coordinate.** Silkscreen
           is caps-only — `abg` and `ABG` rasterise to identical pixels, checked
           on a canvas rather than assumed from a screenshot — so the transform
           the catalog's specimen applies would be a decision that changes
           nothing here, and, more to the point, *the headline cannot show a
           package's case at all*. That is fine for a registry label (`NPM`) and
           not fine for a coordinate the reader copies into a manifest, where
           `React-Smooth` and `react-smooth` are two different packages on
           Maven, NuGet and Go. So the exact name is repeated in the caption in
           the text face, which is the one place on this page it was otherwise
           unavailable in its true form.

           `break-words`, **not** `[overflow-wrap:anywhere]` — the opposite of
           the choice the catalog's name *cell* documents, and the difference is
           the size. `anywhere` opens a break opportunity at every character, so
           at 390px `react-smooth` came out as `REACT` / `-` / `SMOOT` / `H`: the
           hyphen alone on one line and an orphan glyph on the next.
           `break-words` breaks as a last resort only, so the browser takes the
           hyphen first (`REACT-` / `SMOOTH`) and splits mid-glyph just when a
           segment cannot fit at all — which at 104px a name like
           `@babel/plugin-transform-runtime` does need. In a 15px table cell the
           min-content sizing is what matters and `anywhere` wins; in a 104px
           headline the break *quality* is what matters. `min-w-0` on the flex
           child is what lets either act.

           The icon and the registry `Badge` came out of the heading line. A
           24px lucide glyph and a bordered chip beside a 104px headline read as
           three competing objects, and the registry is a *fact about* the
           subject, not part of its name — so it joins the caption, in the
           order the catalog's own caption uses. -->
      <!-- A column until `sm`, a row above it. The button was a sibling of the
           specimen in one wrapping flex row, and `flex-1` + `min-w-0` means the
           *text* yields, not the button: at 390 the h1 got 255px of a 358px
           line, so a 56px headline broke as `REACT` / `-` / `SMOOT` / `H`
           inside a column two glyphs narrower than the page. Stacking gives the
           subject the whole line — `REACT-` / `SMOOTH` — and costs a control
           nothing, since Refresh is the same size wherever it sits. -->
      <div class="flex flex-col gap-3 sm:flex-row sm:items-start">
        <div class="min-w-0 flex-1">
          <h1
            class="font-display font-bold tracking-[0.02em] leading-[0.92] text-display break-words"
          >
            {{ data.name }}
          </h1>
          <!-- The separator is `text-muted-foreground`, not `text-border`.
               `--rule-soft` is declared for separators and is explicitly *not
               contrast-carrying*; painting a glyph in it put a 13px character at
               3.40:1 in the light rendition and 3.67:1 in the dark, measured.
               It is `aria-hidden`, so it is decoration to a screen reader and
               still text to an eye. -->
          <p class="mt-3 text-sm text-muted-foreground [overflow-wrap:anywhere]">
            <span class="font-mono text-foreground">{{ data.name }}</span>
            <span class="px-2" aria-hidden="true">·</span>
            <span class="text-foreground">{{ data.registry }}</span>
            <span class="px-2" aria-hidden="true">·</span>
            {{ t("packageDetailPage.knownVersions", unfilteredTotal) }}
            <!-- Membership is stated only when it is true, and it is stated as
                 what it *does*. This was an "Access Gate" card whose one
                 surviving row read "Beta channel — Non-member" on every package
                 of every registry, including the ones with no beta channel
                 configured, where the words describe nothing. A member sees
                 versions others do not, which is a fact about what is on this
                 page; a non-member has nothing to be told. -->
            <template v-if="data.gate.beta_member">
              <span class="px-2" aria-hidden="true">·</span>
              <span class="text-copper">{{ t("packageDetailPage.betaMemberFact") }}</span>
            </template>
          </p>
          <!-- The package's own links, when its metadata declared any. Absent
               rather than disabled when it did not: a greyed-out "Source code"
               on a package that never named a repository states a fact we do not
               have. The server has already normalised and allow-listed these to
               http(s) — see `PackageLinksDto`. -->
          <p
            v-if="data.links?.repository || data.links?.homepage"
            class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm"
          >
            <a
              v-if="data.links?.repository"
              :href="data.links.repository"
              target="_blank"
              rel="noopener noreferrer"
              class="inline-flex items-center gap-1 py-1 underline underline-offset-2 [overflow-wrap:anywhere]"
            >
              {{ t("packageDetailPage.sourceCode") }}
              <span class="text-muted-foreground">{{ linkLabel(data.links.repository) }}</span>
            </a>
            <a
              v-if="data.links?.homepage"
              :href="data.links.homepage"
              target="_blank"
              rel="noopener noreferrer"
              class="inline-flex items-center gap-1 py-1 underline underline-offset-2 [overflow-wrap:anywhere]"
            >
              {{ t("packageDetailPage.homepage") }}
              <span class="text-muted-foreground">{{ linkLabel(data.links.homepage) }}</span>
            </a>
          </p>
        </div>
        <Button variant="outline" size="sm" @click="fetchDetail">
          {{ t("common.refresh") }}
        </Button>
      </div>

      <!-- ── The subject: one version ──────────────────────────────────────
           The page's centre of gravity, and the reason it is not a table with a
           README under it any more. Every question PRODUCT names — *why was this
           blocked*, *what do I paste* — is answered by exactly one version, and
           the page used to answer "here are twenty-five, click one to change a
           README". So the selected version is drawn as the subject: its state at
           full resolution, its rule in its own words, its vulnerabilities as
           text, and the one line that installs it.

           A region separated by a full-width hairline, not a card. The world has
           no card, and three of them stacked here were the page's largest
           disagreement with its own design system. -->
      <section
        v-if="selectedRow"
        class="border-t border-rule-soft pt-6"
        aria-labelledby="subject-heading"
      >
        <div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-4">
          <div class="min-w-0">
            <!-- The version, in the reading face, at the section step. No label
                 above it: the package name is the h1 immediately above, so
                 "react / 18.3.1" reads as a coordinate without a word spent
                 announcing that a version is a version. -->
            <h2
              id="subject-heading"
              class="font-mono text-xl text-foreground [overflow-wrap:anywhere]"
            >
              {{ selectedRow.version }}
            </h2>
            <p
              class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground"
            >
              <span>
                {{
                  selectedRow.source === "local"
                    ? t("packageDetailPage.local")
                    : isUpstreamOnly(selectedRow.source)
                      ? t("packageDetailPage.notHeldHere")
                      : t("common.proxied")
                }}
              </span>
              <span v-if="selectedRow.published_at">
                {{ t("common.published") }}
                <span class="text-foreground tabular-nums">{{
                  formatDay(selectedRow.published_at)
                }}</span>
              </span>
              <span>
                {{ t("common.downloads") }}
                <span class="text-foreground tabular-nums">{{
                  selectedRow.download_count === null || selectedRow.download_count === undefined
                    ? t("common.unknown")
                    : formatCount(selectedRow.download_count)
                }}</span>
              </span>
              <span>{{ selectedRow.license ?? t("packageDetailPage.licenseUnknown") }}</span>
            </p>
            <!-- Why it is unknown, as a sentence rather than as a `title=`.
                 "no manifest parser for this registry type" and "declares no
                 licence" are different facts and the page must not render them
                 identically; the explanation was written for exactly that and
                 was reachable only by holding a mouse still over a `<span>`. -->
            <p v-if="!selectedRow.license" class="mt-2 max-w-[64ch] text-xs text-muted-foreground">
              {{ t("packageDetailPage.licenseUnknownHelp") }}
            </p>
          </div>

          <!-- The supply-chain badge, which is a picture *about this version*
               and was drawn once per row — twenty-five requests to a third-party
               host on one page view, at 71×16px each. -->
          <a
            v-if="selectedRow.socket_badge_url"
            :href="selectedRow.socket_badge_url"
            target="_blank"
            rel="noopener noreferrer"
            class="order-last shrink-0 py-1"
            :title="t('packageDetailPage.supplyChainReportOn')"
          >
            <img :src="selectedRow.socket_badge_url" alt="socket.dev" class="h-5" />
          </a>

          <!-- The signature device, on the row it was written for.
               `blocked` used to be the one state of six that never reached it:
               the table's `v-else` handed that row a crimson chip instead, so
               the world's organising idea was switched off on precisely the row
               a reader opens the page for. -->
          <Resolution
            :state="rowState(selectedRow)"
            :label="stateLabel(selectedRow)"
            class="shrink-0"
          />
        </div>

        <!-- The rule, beside the thing it refused, in text.
             It was a `hidden group-hover:block` span: no focus trigger, no
             `aria-describedby`, no dismiss — WCAG 2.2 1.4.13 failed on all three
             counts, unreadable to a screen reader and unreachable on a phone.
             It is the answer the page exists to give, so it is simply on the
             page. The tie bar is the denial note's own, ported from the catalog
             rather than invented here. -->
        <p
          v-if="blockedInfo(selectedRow.firewall)"
          class="mt-4 flex items-stretch gap-3 text-sm text-muted-foreground"
        >
          <span class="w-px flex-none bg-border" aria-hidden="true" />
          <span class="min-w-0 [overflow-wrap:anywhere]">
            <span class="text-primary">{{ t("common.reasonLabel") }}</span>
            {{ blockedInfo(selectedRow.firewall)!.reason }}
            <span class="px-2" aria-hidden="true">·</span>
            {{
              t("packageDetailPage.blockedByOn", {
                who: blockedInfo(selectedRow.firewall)!.blocked_by,
                when: formatDay(blockedInfo(selectedRow.firewall)!.blocked_at),
              })
            }}
            <RouterLink
              class="text-primary underline underline-offset-[3px]"
              :to="{
                path: '/tools/access-check',
                query: { registry, name, version: selectedRow.version },
              }"
              >{{ t("packageDetailPage.why") }}</RouterLink
            >
          </span>
        </p>

        <!-- The deprecation message, which was a `title=` on a chip in the
             table — the upstream's own sentence about why not to use this
             version, available only to a mouse held still. -->
        <p
          v-if="selectedRow.deprecated"
          class="mt-4 flex items-stretch gap-3 text-sm text-muted-foreground"
        >
          <span class="w-px flex-none bg-border" aria-hidden="true" />
          <span class="min-w-0 [overflow-wrap:anywhere]">
            <span class="text-copper">{{ t("packageDetailPage.deprecated") }}</span>
            {{ selectedRow.deprecation_message ?? t("packageDetailPage.deprecatedNoReason") }}
          </span>
        </p>

        <!-- Vulnerabilities as text, for the same reason. A severity chip whose
             identifier and summary appear only under a stationary mouse is a
             supply-chain fact this page is not really reporting. -->
        <ul v-if="selectedRow.vulnerabilities.length" class="mt-4 space-y-2">
          <li
            v-for="vuln in selectedRow.vulnerabilities"
            :key="vuln.osv_id"
            class="flex items-stretch gap-3 text-sm text-muted-foreground"
          >
            <span class="w-px flex-none bg-border" aria-hidden="true" />
            <span class="min-w-0 [overflow-wrap:anywhere]">
              <Badge :variant="severityVariant(vuln.severity)" class="mr-2 text-xs">{{
                vuln.severity
              }}</Badge>
              <span class="font-mono text-foreground">{{ vuln.osv_id }}</span>
              <span class="px-2" aria-hidden="true">·</span>
              {{ vuln.summary }}
              <template v-if="vuln.fixed_version">
                <span class="px-2" aria-hidden="true">·</span>
                {{ t("packageDetailPage.fixedIn") }}
                <span class="font-mono text-foreground">{{ vuln.fixed_version }}</span>
              </template>
            </span>
          </li>
        </ul>
        <p
          v-else-if="!selectedRow.vulnerabilities_scanned"
          class="mt-4 text-sm text-muted-foreground"
        >
          {{ t("packageDetailPage.notScannedHelp") }}
        </p>

        <!-- The end of the journey, which the page did not have: a reader who
             found their version had nowhere to go and the page stopped on an em
             dash. The command is data (`installCommandFor`), not a switch in
             this component, and a type with no honest one-liner says so rather
             than printing a plausible wrong one. -->
        <div v-if="installLine" class="mt-6">
          <div class="flex items-stretch border border-border">
            <pre
              class="min-w-0 flex-1 overflow-x-auto bg-ground-sunk px-3 py-3 font-mono text-sm text-foreground"
            ><code>{{ installLine.command }}</code></pre>
            <button
              type="button"
              class="flex shrink-0 items-center gap-2 border-l border-border px-3 text-xs uppercase tracking-[0.1em] text-muted-foreground hover:text-foreground"
              @click="copyInstall"
            >
              <component :is="copied ? Check : Copy" class="h-4 w-4" aria-hidden="true" />
              {{ copied ? t("common.copied") : t("common.copy") }}
            </button>
          </div>
          <p class="mt-2 text-xs text-muted-foreground">
            {{ t("packageDetailPage.installAssumesSetup") }}
            <RouterLink class="text-primary underline underline-offset-[3px]" to="/setup">{{
              t("packageDetailPage.setupGuide")
            }}</RouterLink>
          </p>
        </div>

        <!-- The artifact itself and its bill of materials, for the version in
             hand. These were three chips repeated on every row of a nine-column
             table — where the Download one rendered a bare em dash for thirteen
             of the twenty-one registry types, under a column headed "Download",
             stating nothing about why. Here they are absent when there is
             nothing to offer, which is the same answer without the furniture. -->
        <div class="mt-4 flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
          <a
            v-if="downloadUrl(selectedRow.version)"
            :href="downloadUrl(selectedRow.version)!"
            target="_blank"
            rel="noopener noreferrer"
            class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px]"
          >
            <Download class="h-4 w-4" aria-hidden="true" />
            {{ t("packageDetailPage.downloadViaProxy", { version: selectedRow.version }) }}
          </a>
          <template v-if="token && sbomState === 'available'">
            <button
              v-if="sbomFormats.includes('spdx')"
              type="button"
              class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px] disabled:no-underline disabled:opacity-50"
              :disabled="sbomLoading === `${registry}/${name}/${selectedRow.version}:spdx`"
              @click="downloadSbom(selectedRow!.version, 'spdx')"
            >
              <FileJson class="h-4 w-4" aria-hidden="true" />
              {{ t("packageDetailPage.downloadSpdx23") }}
            </button>
            <button
              v-if="sbomFormats.includes('cyclonedx')"
              type="button"
              class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px] disabled:no-underline disabled:opacity-50"
              :disabled="sbomLoading === `${registry}/${name}/${selectedRow.version}:cyclonedx`"
              @click="downloadSbom(selectedRow!.version, 'cyclonedx')"
            >
              <FileCode class="h-4 w-4" aria-hidden="true" />
              {{ t("packageDetailPage.downloadCyclonedx14") }}
            </button>
          </template>
          <span v-else-if="token && sbomState === 'none'" class="text-muted-foreground">{{
            t("packageDetailPage.noSbom")
          }}</span>
          <span v-else-if="token" class="text-muted-foreground">{{
            t("packageDetailPage.sbomPending")
          }}</span>
        </div>
        <output
          v-if="sbomError[artifactKey(selectedRow.version)]"
          class="mt-2 block text-sm text-destructive"
        >
          {{
            t("packageDetailPage.sbomFailed", {
              reason: sbomError[artifactKey(selectedRow.version)],
            })
          }}
        </output>

        <!-- The one filled action on the reader's half of the page, and the only
             act here that spends this instance's bandwidth and writes an audit
             row. It was a 24px outline chip in the second column of a table. -->
        <div v-if="isUpstreamOnly(selectedRow.source)" class="mt-6">
          <p class="mb-3 max-w-[64ch] text-sm text-muted-foreground">
            {{ t("packageDetailPage.notHeldHereHelp") }}
          </p>
          <Button
            v-if="data.fetch.offered"
            :disabled="fetching !== null"
            @click="onFetchVersion(selectedRow!.version)"
          >
            {{
              fetching === selectedRow.version
                ? t("packageDetailPage.fetching")
                : t("packageDetailPage.fetchVersion")
            }}
          </Button>
          <p v-else-if="data.fetch.reason" class="text-sm text-muted-foreground">
            {{ t("packageDetailPage.fetchUnavailable", { reason: data.fetch.reason }) }}
          </p>
          <RouterLink
            v-else-if="!isAuthenticated"
            :to="{ path: '/login', query: { redirect: route.fullPath } }"
            class="text-sm text-primary underline underline-offset-[3px]"
            >{{ t("packageDetailPage.fetchNeedsSession") }}</RouterLink
          >
          <p v-if="fetchResult[selectedRow.version]" class="mt-2 text-sm text-muted-foreground">
            {{ fetchResult[selectedRow.version] }}
          </p>
          <p class="mt-2 max-w-[64ch] text-xs text-muted-foreground">
            {{ t("packageDetailPage.fetchWhatItDoes") }}
          </p>
        </div>
      </section>

      <!-- README, below the subject and above the index, bound to the selected
           version. Fetched separately from the detail response so the catalogue
           cache's TTL never holds a stale document and the detail payload does
           not grow by a megabyte per package (RFC 0007 §5.4). -->
      <ReadmePanel :registry="registry" :name="name" :version="selectedVersion" />

      <!-- The selected version's supply-chain verdict (RFC 0018), when the
           registry has one: what it is held or warned for, and the findings
           for a reader who may see them. Silent otherwise. -->
      <!-- RFC 0019 §6.5: on a forge, what a "version" is needs saying. -->
      <MovingRefsPanel :registry="registry" :name="name" />
      <VerdictPanel
        :registry="registry"
        :name="name"
        :version="selectedVersion"
        :can-rescan="isAdmin"
      />

      <!-- ── The index ─────────────────────────────────────────────────────
           What the table is now: the list you pick the subject from, not the
           page. Everything a reader compares *between* versions stays here;
           everything they read *about* one version moved up to the subject —
           which is what let this go from nine columns to six, three of which
           survive at 390px where five of the nine used to be behind a
           horizontal scroll with no affordance. -->
      <section class="border-t border-rule-soft pt-6" aria-labelledby="versions-heading">
        <div class="space-y-3">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <h2 id="versions-heading" class="font-mono text-base text-foreground">
              {{ t("common.versions") }}
            </h2>
            <!-- The filter says what it is hiding. A control reading "Show
                 pre-releases" over a list that has none is a promise of
                 something to reveal, so it renders only when there is; and the
                 count is in the label rather than in a tooltip, because "8
                 hidden" is the fact that decides whether a reader clicks. -->
            <!-- No `aria-pressed`. The label already inverts — "Hide 8
                 pre-releases" — so a pressed state on top of it announces as
                 "Hide 8 pre-releases, pressed", which is a double negative for
                 the one reader who cannot see the list change. One channel. -->
            <Button
              v-if="prereleaseCount > 0"
              variant="outline"
              size="sm"
              class="px-2 text-xs"
              @click="togglePrereleases()"
            >
              {{
                showPrereleases
                  ? t("packageDetailPage.hidePrereleases", prereleaseCount)
                  : t("packageDetailPage.showPrereleases", hiddenPrereleaseCount)
              }}
            </Button>
          </div>
          <div v-if="data.upstream.attempted">
            <UpstreamNotice
              :upstream="data.upstream"
              :upstream-version-count="upstreamVersionCount"
            />
          </div>
          <!-- The filter sits with the list it filters rather than in the card
               header with the pre-release control: that one changes *what the
               list is*, this one searches inside it, and putting them together
               would read as two halves of one filter. -->
          <div v-if="unfilteredTotal > 1" class="flex flex-wrap items-center gap-3">
            <div class="relative min-w-48 flex-1">
              <Search class="absolute left-3 top-3 h-4 w-4 text-muted-foreground" />
              <Input
                class="pl-10"
                :placeholder="t('packageDetailPage.filterVersions')"
                :aria-label="t('packageDetailPage.filterVersions')"
                :value="versionFilter"
                @input="filterVersions(($event.target as HTMLInputElement).value)"
              />
            </div>
            <!-- The live region that answers "did my filter do anything".
                 It is not the only one on the page — `Pagination` and
                 `UpstreamNotice` are shared components and carry their own, and
                 taking those away here would change every page that uses them —
                 so this comment states the position rather than a fix that was
                 never made: three announcements can still land on one page turn,
                 and consolidating them is a change to those components. -->
            <output class="text-xs text-muted-foreground" aria-live="polite">
              {{
                filterPending
                  ? t("packageDetailPage.filtering")
                  : t("packageDetailPage.versionsShown", {
                      shown: filteredTotal,
                      total: unfilteredTotal,
                    })
              }}
            </output>
          </div>
          <Table :label="t('packageDetailPage.tableLabel', { name })">
            <!-- A visible caption stating the count and the order.
                 The list had neither, so a reader on page 4 of 160 could not
                 know whether they were walking forward or backward through
                 time — and `Table` was called three times on an admin's view
                 with no `label`, which announced all three scroll regions with
                 the same generic name. -->
            <caption class="caption-top pb-3 text-left text-xs text-muted-foreground">
              {{
                t("packageDetailPage.tableCaption", {
                  shown: filteredTotal,
                  total: unfilteredTotal,
                })
              }}
            </caption>
            <TableHeader>
              <!-- One solid rule under the head, dashed between the rows: the
                   proof's grammar, ported from the catalog rather than restated.
                   Cells are flush left and padded right, so the first column
                   lines up with the caption and the specimen above it. -->
              <TableRow class="border-b border-solid border-border hover:bg-transparent">
                <TableHead class="pl-0 pr-3 lg:w-[9.5rem]">{{ t("common.state") }}</TableHead>
                <TableHead class="pl-0 pr-3">{{ t("common.version") }}</TableHead>
                <TableHead class="pl-0 pr-3 lg:w-[8rem]">{{ t("common.source") }}</TableHead>
                <!-- Dropped below `lg`, whole, rather than squeezed: at 390px
                     the nine columns this table used to draw claimed 895px of a
                     356px box, so five of them sat behind a scroll with no
                     affordance. Every fact they carry is on the subject above
                     for the version a reader has selected — which is what makes
                     dropping them honest rather than hiding them. -->
                <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-28">{{
                  t("common.downloads")
                }}</TableHead>
                <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-32">{{
                  t("common.published")
                }}</TableHead>
                <TableHead class="hidden px-0 lg:table-cell lg:w-32">{{
                  t("common.security")
                }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <!-- Selection rides on ink and a lit edge, never on fill.

                   It was `bg-muted/40`, and DESIGN.md's Undependable Fill Rule
                   is about exactly this: on this ground the raised fill computes
                   to about 1.06:1 against the sheet, so the marked row was
                   indistinguishable from every other one — the page chose a
                   default version and then had no way to say which. `Facet`
                   already solved this for the registry bay ("selection rides on
                   ink and a lit edge, not a fill"), and this is the same
                   statement in a table: a crimson left edge, the ink lifted to
                   `--ink` and the weight raised.

                   The transparent edge on unselected rows reserves the same 2px,
                   so marking a row moves nothing sideways.

                   `aria-current="true"` carries it to a screen reader, which
                   cannot see either the rule or the weight. -->
              <TableRow
                v-for="ver in pagedVersions"
                :key="`${ver.version}-${ver.source}`"
                :class="[
                  'border-l border-rule-soft align-baseline hover:border-solid hover:border-border',
                  /* The dashed/solid pair is per-row, not per-side: `border-l-solid`
                     is not a Tailwind utility and is defined nowhere in this repo,
                     so writing it left the whole row dashed — including the crimson
                     selection edge, which is the one edge the world says must read
                     as engaged. A selected row is solid on every side; the rest stay
                     dashed at rest and go solid under the pointer. */
                  selectedVersion === ver.version
                    ? 'border-solid border-l-primary font-semibold text-foreground'
                    : 'border-dashed border-l-transparent',
                  ver.is_prerelease && selectedVersion !== ver.version
                    ? 'text-muted-foreground italic'
                    : '',
                  ver.is_prerelease && selectedVersion === ver.version ? 'italic' : '',
                ]"
                :aria-current="selectedVersion === ver.version ? 'true' : undefined"
              >
                <TableCell class="pl-0 pr-3 py-3 align-baseline">
                  <Resolution :state="rowState(ver)" :label="stateLabel(ver)" />
                </TableCell>
                <TableCell class="pl-0 pr-3 py-3 align-baseline font-mono text-sm">
                  <!-- A link, not a click handler on the `<tr>`.
                       The row was `@click` with `cursor-pointer`, no `tabindex`,
                       no `role` and no key handler, so selecting a version — the
                       page's central interaction, which drives both the subject
                       above and the URL — was unavailable to a keyboard while
                       `aria-current` announced a selection that reader could not
                       change. A link brings the keyboard, the focus ring,
                       middle-click-to-a-new-tab and a copyable target with it,
                       and it takes the six nested buttons out of a clickable
                       row.

                       Selection is carried by the crimson 1px edge on the row,
                       the ink, the weight and `aria-current` — four channels,
                       because the fill this used to use computes to 1.06:1 and
                       the 2px coloured stripe it used to draw is a thing the
                       world refuses by name. -->
                  <!-- `replace`, never `push` — the rule stated at the top of
                       this file and not applied here when the click handler
                       became a link. Reading down a version list is not ten
                       destinations: without it, five clicks were five Back
                       presses, and worse, `history.state.back` stopped being
                       `/packages`, so `goBack()` fell through to its
                       reconstruct-by-hand path and dropped the reader on a
                       catalog with its search, scope, sort, page and scroll
                       offset gone — the exact loss the comment at `goBack` says
                       it prevents. One attribute restores both. -->
                  <RouterLink
                    replace
                    :to="versionLink(ver.version)"
                    class="inline-block py-1 [overflow-wrap:anywhere] hover:underline underline-offset-[3px]"
                    :class="selectedVersion === ver.version ? 'text-primary' : ''"
                  >
                    {{ ver.version }}
                  </RouterLink>
                  <Badge v-if="ver.is_prerelease" variant="outline" class="ml-2 text-xs">
                    {{ t("packageDetailPage.prerelease") }}
                  </Badge>
                  <!-- Copper, not crimson: the subject region already sets this
                       fact in copper, and crimson is spoken for. One page, one
                       fact, one hue. -->
                  <Badge v-if="ver.deprecated" variant="outline" class="ml-2 text-xs text-copper">
                    {{ t("packageDetailPage.deprecated") }}
                  </Badge>
                  <Badge v-if="ver.unlisted" variant="secondary" class="ml-2 text-xs">
                    {{ t("packageDetailPage.unlisted") }}
                  </Badge>
                  <!-- Under the version rather than in a column of its own: the
                       licence is an attribute of this version, and it is the
                       kind of fact a reader compares down a column at a glance.

                       A stated "unknown" rather than a blank when null —
                       rendering nothing would make "no manifest parser for this
                       registry type" indistinguishable from "declares no
                       licence" (RFC 0004-bis §13.1). -->
                  <p class="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
                    {{ ver.license ?? t("packageDetailPage.licenseUnknown") }}
                  </p>
                </TableCell>
                <TableCell class="pl-0 pr-3 py-3 align-baseline">
                  <!-- Three values, not two. `upstream` means this instance
                       holds no bytes for the version and knows about it only
                       because it asked — the badge says so rather than letting
                       it read as something we have.

                       The fetch button that used to live beside it moved to the
                       subject above: it is the one act on this page that spends
                       bandwidth and writes an audit row, and it was a 24px
                       outline chip repeated twenty-five times down a column. -->
                  <Badge
                    :variant="ver.source === 'local' ? 'secondary' : 'outline'"
                    class="text-xs"
                    :class="isUpstreamOnly(ver.source) ? 'border-dashed' : ''"
                  >
                    {{
                      ver.source === "local"
                        ? t("packageDetailPage.local")
                        : isUpstreamOnly(ver.source)
                          ? t("packageDetailPage.notHeldHere")
                          : t("common.proxied")
                    }}
                  </Badge>
                </TableCell>
                <!-- `unknown`, never `0`: a definite-looking number for a
                     version this instance has never held would be a claim we
                     cannot support (RFC 0007 §4.2). -->
                <TableCell
                  class="hidden pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  {{
                    ver.download_count === null || ver.download_count === undefined
                      ? t("common.unknown")
                      : formatCount(ver.download_count)
                  }}
                </TableCell>
                <TableCell
                  class="hidden whitespace-nowrap pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  {{ formatDay(ver.published_at ?? null) }}
                </TableCell>
                <TableCell
                  class="hidden px-0 py-3 align-baseline text-muted-foreground lg:table-cell"
                >
                  <!-- A count, not five hoverable chips. What each advisory
                       *says* is on the subject above, in text, for the version
                       the reader selected — which is the only place it was ever
                       readable without a mouse. -->
                  <span v-if="!ver.vulnerabilities_scanned" class="text-xs">
                    {{ t("packageDetailPage.notScanned") }}
                  </span>
                  <span v-else-if="ver.vulnerabilities.length" class="text-xs text-destructive">
                    {{ t("packageDetailPage.vulnerabilityCount", ver.vulnerabilities.length) }}
                  </span>
                  <span v-else class="text-xs">{{ t("packageDetailPage.noKnownVulns") }}</span>
                </TableCell>
              </TableRow>
              <!-- A filter that matched nothing is not a package with no
                   versions, and answering the first with the second's words
                   ("nothing has been pulled through") would describe the
                   instance to a reader who asked about their own typing. The
                   counter above already says `0 of 60 shown`; this says which
                   of the two absences the empty table is. -->
              <TableRow v-if="pagedVersions.length === 0 && unfilteredTotal > 0">
                <TableCell colspan="6" class="px-0 py-6 text-center text-muted-foreground">
                  <!-- `filtered`, which its own doc calls required rather than
                       inferred, and which this call site omitted — so
                       `data-filtered="false"` was being written on precisely the
                       state the flag exists to mark. -->
                  <EmptyState
                    filtered
                    :title="t('packageDetailPage.noVersionsMatch')"
                    :description="t('packageDetailPage.noVersionsMatchHint')"
                  />
                </TableCell>
              </TableRow>
              <TableRow v-else-if="pagedVersions.length === 0">
                <TableCell colspan="6" class="px-0 py-6 text-center text-muted-foreground">
                  <!-- Two different absences, and the reader needs to know
                       which: "nothing has been pulled through, and the upstream
                       does not have it either" is an answer; "the upstream
                       could not be reached" is a gap. -->
                  <EmptyState
                    :title="t('packageDetailPage.noVersionsYet')"
                    :description="
                      data.upstream.error
                        ? t('packageDetailPage.upstreamUnreachable')
                        : data.upstream.attempted
                          ? t('packageDetailPage.upstreamHasNoneEither')
                          : t('packageDetailPage.nothingHasBeenPulledThrough')
                    "
                  />
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
          <div v-if="versionTotalPages > 1" class="border-t border-rule-soft pt-3">
            <Pagination
              :page="versionPage"
              :total-pages="versionTotalPages"
              @update:page="turnToPage($event)"
            />
          </div>
        </div>
      </section>
    </template>
  </div>

  <!-- ── Administration ──────────────────────────────────────────────────
         Everything AdminPackageDetail used to be, in place. -->
  <template v-if="isAdmin">
    <!-- A full-width hairline, which is how this world separates two regions —
         `Separator` with a 24px margin either side is the same line drawn by a
         component that also carries a `my-6`; the region's own `pt-6` is the
         rhythm, on the scale. -->
    <section class="mt-6 space-y-4 border-t border-rule-soft pt-6" aria-labelledby="admin-heading">
      <!-- Not copper. Copper means *waiting or held* in this world — a stale
           entry, a quota approaching its limit — and it was painting a section
           label, which is neither. Dim ink is what every other label on the page
           is set in. -->
      <h2
        id="admin-heading"
        class="font-mono text-sm font-semibold uppercase tracking-wider text-muted-foreground"
      >
        {{ t("common.administration") }}
      </h2>

      <p v-if="adminError" class="text-sm text-destructive">{{ adminError }}</p>

      <template v-else-if="adminData">
        <PackageVersionsTable
          :registry="registry"
          :name="name"
          :versions="adminData.versions"
          @reload="reloadAdmin"
        />
        <PackageBetaChannel :registry="registry" />
        <PackageVisibility :registry="registry" :name="name" />
        <PackageGrants :registry="registry" :name="name" />
        <PackageEventsTable :events="adminData.recent_events" />
      </template>
    </section>
  </template>
</template>
