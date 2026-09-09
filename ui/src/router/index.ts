import { watch, type Ref } from "vue";
import {
  createRouter,
  createWebHistory,
  type RouteLocationNormalized,
  type RouteLocationRaw,
} from "vue-router";
import { useAuth, storeTokens } from "@/composables/useAuth";
import { LEGACY_REDIRECTS, SECTION_INDEXES } from "@/config/navigation";

const OIDC_STATE_KEY = "oidc_state";

type AuthState = ReturnType<typeof useAuth>;

/* The backend delivers the tokens in the URL **fragment**, not the query string:
   a fragment is never transmitted to a server, so it stays out of the access log
   of whatever serves this SPA and out of any proxy in front of it. Returning a
   route without the hash is what then clears it from the address bar, so it
   survives only until the first navigation. */
function oidcCallbackParams(to: RouteLocationNormalized): URLSearchParams | null {
  /* `window.location.hash`, not `to.hash`. vue-router's `parseURL` returns
     `hash: decode(hash)` — it runs `decodeURIComponent` over the *whole*
     fragment before any guard sees it. The backend percent-encodes each value
     individually (`url_encode` in `sso.rs`), so by the time `to.hash` exists
     that encoding has already been undone once and `URLSearchParams` would
     decode a second time. The access token and state survive it (base64url and
     hex have nothing to decode), but the refresh token is an opaque
     provider-issued string: one containing `+` comes back with a space, and one
     containing `&` splits into two parameters. The user is signed in and then
     unrecoverably signed out at the first refresh, with nothing to explain it.

     `to.hash` is still what the guard is triggered on; this reads the raw bytes
     of the same fragment. */
  const strip = (h: string) => (h.startsWith("#") ? h.slice(1) : h);
  const live = typeof window !== "undefined" ? strip(window.location.hash) : "";
  /* An OIDC callback is always a full page load — the IdP redirected here — so
     the address bar holds this navigation's own fragment. Falling back to
     `to.hash` keeps an in-app navigation (where the address bar still shows the
     *previous* URL) reading the route it was given. */
  const raw = live.includes("oidc_access_token=") ? live : strip(to.hash);
  if (!raw) return null;
  const params = new URLSearchParams(raw);
  return params.has("oidc_access_token") ? params : null;
}

function handleOidcCallback(to: RouteLocationNormalized): RouteLocationRaw | null {
  const params = oidcCallbackParams(to);
  if (!params) return null;

  /* Consume the fragment the moment it is read, because it is read from the
     address bar and a guard redirect does not touch the address bar.
     `finalizeNavigation` is what calls `pushState`, and a guard that returns a
     location never gets there — so `window.location.hash` still carried the
     callback fragment when `beforeEach` re-ran for `/packages`, this function
     parsed the same parameters a second time, found the state already taken out
     of `sessionStorage` by the first pass, and bounced a sign-in that had just
     succeeded to `/login?error=oidcStateMismatch`. (Vue Router's
     infinite-redirect counter is `NODE_ENV !== "production"`-only, so a release
     build has nothing to stop the third pass doing it again.)

     Doing it here rather than leaving it to the returned route also takes the
     token out of the address bar one navigation earlier, which is where the
     comment above wanted it. */
  if (typeof window !== "undefined" && window.location.hash.includes("oidc_access_token=")) {
    window.history.replaceState(
      window.history.state,
      "",
      window.location.pathname + window.location.search,
    );
  }

  const incomingState = params.get("oidc_state") ?? "";
  const expectedState = sessionStorage.getItem(OIDC_STATE_KEY) ?? "";
  sessionStorage.removeItem(OIDC_STATE_KEY);

  /* The server has already checked that this state is one it issued, has not
     expired and has not been redeemed before. What it cannot check is that the
     login started in *this* browser — it has no way to tell two browsers apart.
     That is what this comparison is for, and why removing it would leave a real
     gap even though the server-side check exists. */
  if (!incomingState || incomingState !== expectedState) {
    /* A catalogue *key*, not a sentence. The router has no `setup` and the
       destination does — and putting a rendered sentence in a query string also
       pins it to the locale that was active when the redirect was built, which
       for an OIDC round-trip is a different page load entirely. `LoginPage`
       translates anything it recognises and shows the rest verbatim, so an
       error forwarded from the backend still reads. */
    return { path: "/login", query: { error: "loginPage.oidcStateMismatch" } };
  }

  const expiresIn = params.get("oidc_expires_in");
  storeTokens(
    String(params.get("oidc_access_token")),
    params.get("oidc_refresh_token"),
    expiresIn ? Number(expiresIn) : null,
    params.get("oidc_provider"),
  );
  return { path: "/packages" };
}

function handleOidcError(to: RouteLocationNormalized): RouteLocationRaw | null {
  if (!to.query.oidc_error) return null;
  sessionStorage.removeItem(OIDC_STATE_KEY);
  return { path: "/login", query: { error: String(to.query.oidc_error) } };
}

function waitForIdentity(identityReady: Ref<boolean>): Promise<void> {
  if (identityReady.value) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const stop = watch(identityReady, (ready) => {
      if (ready) {
        stop();
        resolve();
      }
    });
  });
}

function checkAnonymousAccess(
  to: RouteLocationNormalized,
  { identity }: Pick<AuthState, "identity">,
): RouteLocationRaw | null {
  if (
    to.path !== "/login" &&
    identity.value?.role === "anonymous" &&
    identity.value?.has_registry_access === false
  ) {
    return { path: "/login" };
  }
  return null;
}

function checkMetaGuards(
  to: RouteLocationNormalized,
  {
    isAuthenticated,
    identity,
    isAdmin,
  }: Pick<AuthState, "isAuthenticated" | "identity" | "isAdmin">,
): RouteLocationRaw | undefined {
  if (to.meta.requiresAuth && !isAuthenticated.value) {
    return { path: "/login", query: { redirect: to.fullPath } };
  }
  if (to.meta.requiresOidcAuth && !identity.value?.auth_provider) {
    return { path: "/login", query: { redirect: to.fullPath } };
  }
  if (to.meta.requiresAdmin && !isAdmin.value) {
    return { path: "/login", query: { redirect: to.fullPath } };
  }
}

/**
 * Legacy paths resolve through one guard fed by `LEGACY_REDIRECTS`, rather than
 * ~20 hand-written `redirect` entries added one per moved page — which is how
 * the section list and its aliases drifted apart in the first place. Query and
 * hash are preserved: a bookmark with `?registry=npm1` keeps it (RFC 0003 §9).
 */
function handleLegacyPath(to: RouteLocationNormalized): RouteLocationRaw | null {
  const target = LEGACY_REDIRECTS[to.path] ?? SECTION_INDEXES[to.path];
  if (!target) return null;
  return { path: target, query: to.query, hash: to.hash };
}

/**
 * The catalog redirects, which need more than a table lookup.
 *
 * A package had two addresses — `/packages/detail?registry=&name=` and
 * `/explore/packages/:registry/:name` — and only one of them survived a
 * copy-paste. Both now resolve to the canonical path form, so old bookmarks and
 * anything in `docs/` keep working while a package gains a single identity.
 */
function handleCatalogPath(to: RouteLocationNormalized): RouteLocationRaw | null {
  if (to.path === "/explore") return { path: "/packages", query: to.query, hash: to.hash };

  const explore = /^\/explore\/packages\/([^/]+)\/(.+)$/.exec(to.path);
  if (explore) {
    return { path: `/packages/${explore[1]}/${explore[2]}`, query: to.query, hash: to.hash };
  }

  if (to.path === "/packages/detail" || to.path === "/admin/packages/detail") {
    const registry = String(to.query.registry ?? "");
    const name = String(to.query.name ?? "");
    // Without both we cannot name a package; the catalog is the honest landing.
    if (!registry || !name) return { path: "/packages" };
    const { registry: _r, name: _n, ...rest } = to.query;
    return {
      path: `/packages/${encodeURIComponent(registry)}/${encodeURIComponent(name)}`,
      query: rest,
      hash: to.hash,
    };
  }
  return null;
}

export const router = createRouter({
  history: createWebHistory(),
  /**
   * Returning to a list returns to the place in it.
   *
   * Without this the router leaves the scroll offset wherever the browser put
   * it, which for an SPA is "wherever the previous page ended" — so pressing
   * Back from a package landed at the top of the catalog, and the row the reader
   * had opened was somewhere below the fold. `savedPosition` is populated only
   * on a *pop* (Back/Forward), which is exactly when the reader is returning
   * rather than arriving.
   *
   * `{ top: 0 }` for everything else: a fresh destination starts at its own
   * beginning, and this is also what makes a route change behave like a page
   * load for anyone who expects one.
   *
   * **Except when nothing but the query changed.** A page that keeps its reading
   * position in the URL — the catalog's search and page, the detail page's
   * selected version and version filter — issues a navigation per keystroke and
   * per row click, and every one of them was landing here and slamming the
   * viewport to the top. On the detail page that meant typing blind into a
   * filter that had just scrolled off the bottom of a page which opens with a
   * 104px headline. The reader has not gone anywhere: `to.path === from.path`
   * is precisely the case where the browser's own scroll offset is the right
   * answer, and `false` is how vue-router says "leave it alone".
   */
  scrollBehavior(to, from, savedPosition) {
    if (savedPosition) return savedPosition;
    if (to.path === from.path) return false;
    return { top: 0 };
  },
  routes: [
    { path: "/", component: () => import("@/pages/HomePage.vue") },
    { path: "/login", component: () => import("@/pages/LoginPage.vue") },
    // One catalog and one canonical package URL (RFC 0003 §4.2). `/explore` and
    // both former detail URLs redirect here — see `handleCatalogPath`.
    { path: "/packages", component: () => import("@/pages/PackageCatalog.vue") },
    {
      path: "/packages/:registry/:name",
      component: () => import("@/pages/PackageDetailPage.vue"),
    },
    { path: "/setup", component: () => import("@/pages/SetupGuide.vue") },

    // ── Account hub ────────────────────────────────────────────────────────
    // Four former top-level routes, reachable only from a dropdown, gathered
    // behind one heading. Tabs are routed, so each still deep-links — and
    // tokens only render on the route the user deliberately opened.
    {
      path: "/me",
      component: () => import("@/layouts/AccountLayout.vue"),
      meta: { requiresAuth: true },
      children: [
        { path: "profile", component: () => import("@/pages/MyProfile.vue") },
        {
          path: "tokens",
          component: () => import("@/pages/TokensPage.vue"),
          meta: { requiresOidcAuth: true },
        },
        { path: "namespace", component: () => import("@/pages/MyNamespace.vue") },
        { path: "cli", component: () => import("@/pages/CliDownload.vue") },
      ],
    },

    // ── Diagnostics hub ────────────────────────────────────────────────────
    {
      path: "/tools",
      component: () => import("@/layouts/ToolsLayout.vue"),
      children: [
        { path: "access-check", component: () => import("@/pages/AccessCheck.vue") },
        { path: "url-mapper", component: () => import("@/pages/PathMapper.vue") },
      ],
    },

    {
      path: "/admin",
      component: () => import("@/layouts/AdminLayout.vue"),
      meta: { requiresAdmin: true },
      children: [
        /**
         * Real redirect routes for the section roots, not just `SECTION_INDEXES`.
         *
         * That table is consumed by a `beforeEach` guard, which runs on
         * *navigation*. `RouterLink` resolves its `href` statically, so a link
         * to `/admin/packages` — which had no route — logged
         * `[VUE_ROUTER_R0004] No match found` on every render and produced a
         * wrong `href`: clicking worked because the guard caught it, but
         * middle-click, open-in-new-tab and copy-link-address did not.
         *
         * Generated from the same table so the two can never disagree; the
         * guard stays as the fallback for typed URLs and old bookmarks.
         */
        { path: "", redirect: SECTION_INDEXES["/admin"] },
        ...Object.entries(SECTION_INDEXES)
          .filter(([from]) => from.startsWith("/admin/"))
          .map(([from, to]) => ({ path: from.slice("/admin/".length), redirect: to })),

        { path: "dashboard", component: () => import("@/pages/AdminDashboard.vue") },
        { path: "packages/all", component: () => import("@/pages/AdminPackages.vue") },
        { path: "packages/bulk", component: () => import("@/pages/AdminBulk.vue") },
        { path: "security/blocks", component: () => import("@/pages/AdminBlocks.vue") },
        { path: "security/access-check", component: () => import("@/pages/AdminAccessCheck.vue") },
        {
          path: "security/authorization",
          component: () => import("@/pages/AdminAuthorization.vue"),
        },
        {
          path: "namespaces/team-namespaces",
          component: () => import("@/pages/AdminTeamNamespaces.vue"),
        },
        {
          path: "namespaces/beta-channel",
          component: () => import("@/pages/AdminBetaChannel.vue"),
        },
        {
          path: "operations/config-reload",
          component: () => import("@/pages/AdminConfigReload.vue"),
        },
        { path: "operations/warming", component: () => import("@/pages/AdminWarming.vue") },
        { path: "operations/imports", component: () => import("@/pages/AdminImports.vue") },
        { path: "observability/health", component: () => import("@/pages/AdminHealth.vue") },
        { path: "operations/sbom", component: () => import("@/pages/AdminSbom.vue") },
        { path: "operations/air-gap", component: () => import("@/pages/AdminAirGap.vue") },
        { path: "operations/upstream", component: () => import("@/pages/AdminUpstream.vue") },
        { path: "observability/audit-log", component: () => import("@/pages/AuditLog.vue") },
        {
          path: "notifications/subscriptions",
          component: () => import("@/pages/AdminNotificationSubscriptions.vue"),
        },
        {
          path: "notifications/inbound",
          component: () => import("@/pages/AdminInboundEvents.vue"),
        },
      ],
    },
  ],
});

router.beforeEach(async (to) => {
  const auth = useAuth();

  const oidcResult = handleOidcCallback(to);
  if (oidcResult !== null) return oidcResult;

  const oidcError = handleOidcError(to);
  if (oidcError !== null) return oidcError;

  const legacy = handleLegacyPath(to);
  if (legacy !== null) return legacy;

  const catalog = handleCatalogPath(to);
  if (catalog !== null) return catalog;

  await waitForIdentity(auth.identityReady);

  const anonResult = checkAnonymousAccess(to, auth);
  if (anonResult !== null) return anonResult;

  return checkMetaGuards(to, auth);
});

/** Generate and store a fresh OIDC state value, then return it. */
export function generateOidcState(): string {
  const state = crypto.randomUUID();
  sessionStorage.setItem(OIDC_STATE_KEY, state);
  return state;
}
