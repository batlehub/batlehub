// How long a listing paints the page instantly. It is never the answer:
// `PackageCatalog` revalidates every hit against the server.
const TTL_MS = 5 * 60 * 1_000; // 5 minutes

/**
 * Upstream results expire sooner, and deliberately so: this is a third party's
 * freshness, not our own data. Long enough that a debounced field and a
 * backspace do not re-query npm; short enough that a package published
 * five minutes ago is findable.
 */
const UPSTREAM_TTL_MS = 60 * 1_000; // 1 minute

interface CacheEntry<T> {
  data: T;
  expiresAt: number;
}

const _store = new Map<string, CacheEntry<unknown>>();

/**
 * Upstream search results, keyed `name::registry`.
 *
 * `exploreUpstreamSearch` was cached at no layer at all. Behind it,
 * `explore_upstream_search` fans out with `join_all` across every accessible
 * registry client on every request, and the adapters' `search_packages` goes
 * straight to the network — `npm.rs` builds `/-/v1/search?text=…` and calls
 * `.send()` with no cache store. So typing `lodash` was five debounced
 * requests, each fanning out to N upstream registries — 5N third-party calls —
 * and backspacing re-queried from scratch.
 *
 * **Client-side, pending O4.** A server-side cache changes who the upstream
 * sees the query from: the instance would query once for everyone rather than
 * each operator querying for themselves, which is a policy decision on an
 * instance whose operators may have chosen a proxy partly *not* to send partial
 * package names to npm. Caching in the client is the change that is correct
 * under either answer.
 */
const _upstream = new Map<string, CacheEntry<unknown>>();

// `scope` is part of the key, not an afterthought: the same term searched over
// names and over READMEs are two different questions with two different answers,
// and a key that ignored it would serve whichever ran first to the other.
function key(registry: string, page: number, sort: string, query: string, scope: string): string {
  return `${registry}::${page}::${sort}::${query}::${scope}`;
}

/**
 * The viewer the current entries were fetched for, or `null` before anyone has
 * said. Compared by value, not identity — `scopeExploreCacheTo` is called on
 * every auth transition and most of them are no-ops.
 */
let _viewer: string | null = null;

/**
 * Empty the store when the viewer changes.
 *
 * The server scopes explore results by viewer on purpose: `explore_viewer_for`
 * carries `is_admin`, `is_authenticated` and `groups`, and the listing applies
 * them so a caller cannot see packages in a private namespace they are not in.
 * None of that is in the cache key — and it deliberately stays out of it. A key
 * carrying the viewer would still hold the previous viewer's rows in memory,
 * and the whole point is that they stop being readable.
 *
 * `logout()` is only one of the transitions that matter. An anonymous visitor
 * signing in is the other, and `handleLogout()` does `router.push("/login")` —
 * a client-side navigation, so nothing reloads and a module-level map survives
 * intact. On a shared browser that made the next person to sign in read the
 * previous viewer's group-scoped rows for up to five minutes.
 *
 * @param viewer a fingerprint of everything the server scopes on. `null` means
 *   "no viewer resolved yet" and is itself a distinct scope.
 */
export function scopeExploreCacheTo(viewer: string | null): void {
  if (viewer === _viewer) return;
  _viewer = viewer;
  _store.clear();
  // Upstream results too: `explore_upstream_search` fans out across every
  // registry the *caller* can reach, so they are as viewer-scoped as the
  // listing is.
  _upstream.clear();
}

/** Read through a TTL map, evicting on the way out rather than on a timer. */
function read<T>(store: Map<string, CacheEntry<unknown>>, k: string): T | undefined {
  const entry = store.get(k) as CacheEntry<T> | undefined;
  if (!entry) return undefined;
  if (Date.now() > entry.expiresAt) {
    store.delete(k);
    return undefined;
  }
  return entry.data;
}

function invalidate(registry?: string): void {
  if (!registry) {
    _store.clear();
    _upstream.clear();
    return;
  }
  for (const k of _store.keys()) {
    if (k.startsWith(`${registry}::`)) _store.delete(k);
  }
  // The upstream key is `name::registry`, so the registry is the *suffix*.
  for (const k of _upstream.keys()) {
    if (k.endsWith(`::${registry}`)) _upstream.delete(k);
  }
}

export function useExploreCache<T>() {
  function get(
    registry: string,
    page: number,
    sort: string,
    query: string,
    scope = "name",
  ): T | undefined {
    return read<T>(_store, key(registry, page, sort, query, scope));
  }

  function set(
    registry: string,
    page: number,
    sort: string,
    query: string,
    data: T,
    scope = "name",
  ): void {
    _store.set(key(registry, page, sort, query, scope), {
      data,
      expiresAt: Date.now() + TTL_MS,
    });
  }

  return { get, set, invalidate };
}

/**
 * The upstream-search half, on its own shorter TTL.
 *
 * Separate from `useExploreCache` rather than a fourth key segment: the two
 * hold different things, expire on different clocks and are invalidated by
 * different events. One store with a mixed TTL would have to carry the TTL per
 * entry and would make "refresh the listing" also discard upstream results,
 * which is a third-party round trip nobody asked for.
 */
export function useUpstreamCache<T>() {
  const upstreamKey = (name: string, registry: string) => `${name}::${registry}`;

  function get(name: string, registry: string): T | undefined {
    return read<T>(_upstream, upstreamKey(name, registry));
  }

  function set(name: string, registry: string, data: T): void {
    _upstream.set(upstreamKey(name, registry), {
      data,
      expiresAt: Date.now() + UPSTREAM_TTL_MS,
    });
  }

  return { get, set };
}
