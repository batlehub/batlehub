#!/usr/bin/env node
/**
 * axe *and* a type-ramp check over **every rendered route** (RFC 0003 §4.7,
 * §13; RFC 0004-bis §4.4).
 *
 * The `impeccable detect` / `@axe-core/cli` gates scan by URL and have no way
 * to carry a session, so `/me/*` and every `/admin/*` page — the largest
 * surface in the console — were never measured: they redirect to `/login`, and
 * scanning them would have graded the login page eighteen times. This seeds the
 * token into `localStorage` before the app boots, then runs axe-core in the
 * page. It talks to an already-running Chrome over CDP (`puppeteer-core`, so no
 * second browser is downloaded) — in this workspace, the `che-browser` sidecar.
 *
 * RFC 0004-bis §4.4 merged the public routes in. They *were* covered — by
 * `ui:design:rendered`, which runs `impeccable detect` and axe, neither of
 * which knows what a type ramp is. So the ramp and display-face assertions ran
 * on `/admin/*`, `/me/*` and `/` and on nothing else, and `/packages` — the one
 * page in the console with a checked-in specification of its own appearance
 * (`ui/design-proof/index.html`) — was the one significant page no ramp check
 * ran against. That is how its 104px display element became 24px with every
 * gate green.
 *
 * The two lists are now one. An anonymous plan is a plan with no token, which
 * is a flag rather than a second script.
 *
 *   BATLEHUB_ADMIN_TOKEN=… BATLEHUB_USER_TOKEN=… node build/design-routes.mjs
 *   node build/design-routes.mjs --public-only     # skip the two authed plans
 *
 * Tokens come from the environment and are never written to disk: they are
 * credentials for a running instance, not fixtures. Missing tokens are an
 * error, not a quiet skip — `--public-only` has to be asked for, because a
 * gate that measures less on the way to reporting green is the exact failure
 * this RFC is about.
 */
import puppeteer from "puppeteer-core";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const AXE_SOURCE = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

const BASE = process.env.BASE ?? "http://localhost:5174";
/*
 * Prefer the WebSocket endpoint when the launcher published one.
 *
 * `browserURL` makes puppeteer discover the endpoint by GETting
 * `/json/version` first, which is a second thing that has to work — and in CI
 * it was the thing that did not: Chrome logged `DevTools listening on
 * ws://127.0.0.1:9222/...` three seconds in while that URL stayed unreachable
 * for the full minute. `CDP_WS_URL` comes from Chrome's own
 * `DevToolsActivePort` file, so there is nothing left to discover.
 *
 * `127.0.0.1`, not `localhost`, in the fallback: `localhost` resolves to ::1
 * first on the runners, and Chrome binds IPv4.
 */
const CDP_WS = process.env.CDP_WS_URL ?? "";
const CDP = process.env.CDP_URL ?? "http://127.0.0.1:9222";
const TOKEN_KEY = "batlehub_access_token";
/* Read by `isOidcSession`: `/me/tokens` is OIDC-only, and a static token
   alone is sent to the profile (UI audit lot B). The provider name is only a
   marker; nothing here refreshes against it. */
const OIDC_PROVIDER_KEY = "batlehub_oidc_provider";

/** WCAG 2.2 AA, the same tag set the unauthenticated gate uses. */
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

/**
 * DESIGN.md's two ramps: JetBrains Mono at 12/13/15/16/20 and Silkscreen on its
 * 8px em at 16/24/56/72/88/104. A size outside this set is a step nobody
 * declared, and "the admin pages are not redrawn in the specimen grammar" stops
 * being an assertion the moment it is measured.
 */
// DESIGN.md (The Interface Face Rule) added the interface face's 14 (body) and 28 (page title).
const RAMP = new Set([12, 13, 14, 15, 16, 20, 24, 28, 40, 56, 72, 88, 104]);

/**
 * Both widths, because for fifteen pages neither gate looked at the narrow one.
 *
 * The rendered detector runs 390x844 but only over the *unauthenticated*
 * routes; this gate covered `/admin/*` but at 1440x900 only. So the entire
 * admin surface — the largest in the console — was never measured on a phone,
 * and every one of its pages scrolled sideways: `AdminLayout` held the mobile
 * tab strip and the page content as siblings of one flex row, and documents
 * came out 496-705px wide on a 390px viewport (RFC 0004 Phase 5).
 */
/*
 * Four now, not two. The two-width gate has a blind spot between them, and the
 * package detail page shipped in it: at 768px the versions table hid 192px of
 * itself behind a scroll with no affordance, and at 1024 the fixed columns were
 * still fighting the flexible one — both green at 1440 and at 390, because 390
 * releases every fixed width and 1440 has room for all of them. The widths that
 * break a table are the ones in the middle.
 */
const VIEWPORTS = [1440, 1024, 768, 390];
/** The data face: a page title set in it is a fact, not a heading. */
const DATA_FACE = "JetBrains Mono";

/*
 * Every admin page of `ui/src/config/adminSections.ts`, plus one concrete
 * registry page. A literal because this script cannot import the `.ts` table;
 * `adminSections.test.ts` fails when the two differ. RFC 0046 §6.5 added the
 * eight pages the copy had drifted away from — scans, findings, authorization,
 * upstream, banner, air gap, registries and one registry — none of which any
 * gate had ever rendered.
 */
const ADMIN_ROUTES = [
  "/admin/dashboard",
  "/admin/registries",
  "/admin/registry/npm",
  "/admin/registries/upstream",
  "/admin/registries/warming",
  "/admin/registries/imports",
  "/admin/registries/air-gap",
  "/admin/packages/all",
  "/admin/packages/bulk",
  "/admin/packages/sbom",
  "/admin/access/authorization",
  "/admin/access/team-namespaces",
  "/admin/access/beta-channel",
  "/admin/instance-security",
  "/admin/instance-security/blocks",
  "/admin/system/configuration",
  "/admin/system/banner",
  "/admin/system/health",
  "/admin/system/audit-log",
  "/admin/system/notifications",
  "/admin/system/inbound-events",
];

/*
 * Every page of `ui/src/config/securitySections.ts` (RFC 0050 §6.6), plus one
 * registry's coverage. `adminSections.test.ts` fails when the two differ, as
 * for the admin list: a `/security` route escaped every gate until this.
 */
const SECURITY_ROUTES = [
  "/security",
  "/security/holds",
  "/security/findings",
  "/security/flags",
  "/security/exposure",
  "/security/trends",
  "/security/coverage",
  "/security/coverage/npm",
  "/security/scanners",
];

// `/me/tokens` is OIDC-only: the plan seeds `OIDC_PROVIDER_KEY` beside the
// token, as the OIDC callback does.
//
// `/` is here as well as in the unauthenticated scan, and both are needed: the
// quota and advisory widgets (RFC 0004 §4.2) render *only* for a signed-in
// viewer, so the anonymous pass measures a home page that is missing them —
// including the one meter in the system, whose whole point is an accessibility
// contract.
const USER_ROUTES = ["/", "/me/profile", "/me/tokens", "/me/namespace", "/me/cli"];

/**
 * Every route reachable without a session — the list `ui:design:rendered`
 * carries, verbatim, so the two cannot drift.
 *
 * `/` appears here *and* in `USER_ROUTES`, and both are needed: the quota and
 * advisory widgets (RFC 0004 §4.2) render only for a signed-in viewer, so the
 * anonymous pass measures a home page that is missing them — including the one
 * meter in the system, whose whole point is an accessibility contract.
 */
const PUBLIC_ROUTES = [
  "/",
  "/login",
  "/packages",
  /*
   * The densest page in the console, and until now the one no gate looked at.
   *
   * It is in neither list: not this file's, not the `impeccable detect` loop's.
   * So the only nine-column table in the console, its only `v-html` surface and
   * its only per-row policy explanation had zero rendered coverage and zero axe
   * coverage — which is how a hover-only denial note, a mouse-only row
   * selection and 539px of hidden columns at 390px all shipped green.
   * `/packages` migrated because a gate went red on it; this one had nothing to
   * go red.
   *
   * A concrete coordinate rather than a parameterised one, because a gate scans
   * URLs: `npm/react` is what `build/fixtures/captured.json` answers for, with a
   * blocked version, advisories and a pre-release in it — the states that
   * actually expose contrast and reflow.
   */
  "/packages/npm/react",
  "/setup",
  "/tools/access-check",
  "/tools/url-mapper",
];

const publicOnly = process.argv.includes("--public-only");

const plans = [
  { role: "anonymous", token: null, routes: PUBLIC_ROUTES },
  ...(publicOnly
    ? []
    : [
        {
          role: "admin",
          token: process.env.BATLEHUB_ADMIN_TOKEN,
          routes: [...ADMIN_ROUTES, ...SECURITY_ROUTES, ...USER_ROUTES],
        },
        { role: "user", token: process.env.BATLEHUB_USER_TOKEN, routes: USER_ROUTES },
      ]),
];

/**
 * Failures that are a known, *owned* disagreement rather than a regression.
 *
 * **Currently empty, and that is a result rather than a default.**
 *
 * RFC 0004-bis §4.4 landed this gate knowing it went red on `/packages`: the
 * proof spends the Display step (104px Silkscreen) on the registry being
 * viewed, and the page spent 24px on the word "Packages". O3 owned it, and the
 * pin held the disagreement visible instead of letting a green gate cover a
 * page nobody was comparing to anything.
 *
 * O3 was then decided in favour of the page moving (§14.9): `--t-display` was
 * mapped to a utility for the first time, the specimen replaced `PageHeader`
 * on that route, and the plate and the resolution matrix were ported from the
 * proof — which is runnable source, not a screenshot. `/packages` began
 * passing, this gate failed *because it was still pinned*, and the pin came out
 * in the commit that moved the page. That inverse assertion is the reason to
 * keep the mechanism: a pin nobody removes is a claim that quietly goes stale.
 *
 * A pinned route that *starts passing* fails the gate, by design. Add an entry
 * only with an owner named in the string.
 */
const EXPECTED_FAIL = {};

/**
 * Coverage is asserted, not merely regenerated (RFC 0004 §10).
 *
 * This list growing is expected — every route a future pass adds or splits
 * extends it. It *shrinking* is the failure this catches: a route quietly
 * dropped from the arrays above would make the gate pass by measuring less,
 * which reads identically to measuring clean.
 */
// RFC 0004 Phase 5 moved this twice, both deliberately: -1 for the removed
// `/admin/operations/explore-cache` (its control went to the health page),
// then +2 for the notifications split. RFC 0004-bis §4.4 moves it once more,
// +6, for the public routes this gate now also measures. RFC 0046 §6.5 moves
// it +8 for the admin pages the list had drifted away from:
//
//   admin  23 admin + 5 user = 28
//   user                       5
//   anonymous                  7
//                             ──
//                             40
//
// RFC 0050 §6.6 moves it +7: the admin list loses scans, findings, access
// check and blocks (-4) and gains instance security and its blocks (+2), and
// the nine `/security` routes join the admin plan (+9):
//
//   admin  21 admin + 9 security + 5 user = 35
//   user                                   5
//   anonymous                              7
//                                         ──
//                                         47
//
// The number tracks real coverage, which is the point — it may only change in
// the commit that changes the routes.
//
// It counts route/role pairs, NOT scans: every pair is measured at all four
// viewports, so the summary line at the end of a run reports four times this
// figure (40 pairs → "160 route/role/viewport combination(s) scanned"). Two
// numbers 4× apart in one run's output invite someone to "fix" the wrong one,
// so: this is the coverage floor, that is the work done.
const EXPECTED_COMBINATIONS = publicOnly ? PUBLIC_ROUTES.length : 47;
const planned = plans.reduce((n, p) => n + p.routes.length, 0);
if (planned < EXPECTED_COMBINATIONS) {
  console.error(
    `coverage shrank: ${planned} route/role combinations, expected at least ${EXPECTED_COMBINATIONS}. ` +
      `If a route was deliberately removed, lower EXPECTED_COMBINATIONS in the same commit.`,
  );
  process.exit(2);
}

// `token: null` is the anonymous plan and is meant to have none; `undefined`
// is a token that was supposed to be in the environment and is not.
const missing = plans.filter((p) => p.token === undefined).map((p) => p.role);
if (missing.length) {
  console.error(
    `missing token(s) for: ${missing.join(", ")} — set BATLEHUB_{ADMIN,USER}_TOKEN, ` +
      `or pass --public-only to measure the unauthenticated routes alone`,
  );
  process.exit(2);
}

const browser = await puppeteer.connect(
  CDP_WS ? { browserWSEndpoint: CDP_WS } : { browserURL: CDP },
);
let failures = 0;
let scanned = 0;
/** Pinned routes that failed as expected, and pinned routes that did not. */
const pinnedRed = new Set();
const pinnedGreen = new Set();

for (const { role, token, routes } of plans) {
  for (const route of routes) {
   for (const width of VIEWPORTS) {
    const page = await browser.newPage();
    await page.setViewport({ width, height: 900 });
    const pageErrors = [];
    page.on("pageerror", (e) => pageErrors.push(String(e.message).split("\n")[0].slice(0, 120)));

    /* Failures on a pinned route are reported and not counted (see
       `EXPECTED_FAIL`); a pinned route that comes back clean is counted,
       because the pin has become the stale claim. */
    const pinned = EXPECTED_FAIL[route];
    let routeFailed = false;
    const fail = () => {
      routeFailed = true;
      if (!pinned) failures++;
    };

    try {
      // The token has to exist before the app's first script runs: `initAuth`
      // reads it synchronously at import time, and the router then resolves
      // identity once. Seeding after `goto` would race that. The anonymous plan
      // has no token, and seeding a `null` would make `initAuth` read the
      // string "null" as a bearer credential.
      await page.evaluateOnNewDocument(
        (key, value, providerKey) => {
          /* Cleared, not merely left unset. Every page here shares one browser
             profile — locally the `che-browser` sidecar — so an anonymous plan
             that only declines to seed still reads whatever an authenticated
             plan left behind, and the admin plan runs first. That made `/login`
             redirect to `/packages` and the "anonymous" pass measure a
             signed-in console. */
          if (value) {
            localStorage.setItem(key, value);
            localStorage.setItem(providerKey, "design-gate");
          } else localStorage.clear();
        },
        TOKEN_KEY,
        token,
        OIDC_PROVIDER_KEY,
      );
      /* `networkidle2` is the right settle signal for a *built* app and a poor
         one for a dev server: Vite transforms each module on first request, so
         a cold route legitimately keeps more than two requests in flight for
         longer than the default 30s and the whole run dies on a `goto`. The
         fallback measures the same page a moment later rather than aborting —
         a gate that fails on a cold cache is a gate people retry until it
         passes, which is the same thing as no gate. */
      try {
        // Short, because the fallback measures the same page correctly and a
        // long first attempt makes every route pay for the one case where the
        // network never goes quiet — which on a dev server is most of them.
        await page.goto(BASE + route, { waitUntil: "networkidle2", timeout: 15_000 });
      } catch {
        await page.goto(BASE + route, { waitUntil: "domcontentloaded", timeout: 30_000 });
        await new Promise((r) => setTimeout(r, 2_000));
      }
      await new Promise((r) => setTimeout(r, 900));

      const landed = new URL(page.url()).pathname;
      if (landed !== route) {
        // A redirect means the session did not take, so whatever axe measured
        // would be the login page. Reported rather than counted as a pass.
        /* For an authenticated plan a redirect means the session did not take,
           so whatever axe measured would be the login page. For the anonymous
           plan it means the route is not actually public, which is the same
           kind of finding: the gate did not measure what it claims to. */
        const why = token ? "session not applied" : "route is not public";
        console.log(`✗ ${role} ${route} @${width} → redirected to ${landed} (${why})`);
        fail();
        continue; // the `finally` below still records the pin state
      }

      await page.evaluate(AXE_SOURCE);
      const results = await page.evaluate(
        async (tags) => await window.axe.run(document, { runOnly: { type: "tag", values: tags } }),
        TAGS,
      );
      scanned++;

      const type = await page.evaluate(({ dataFace }) => {
        const sizes = new Set();
        for (const el of document.querySelectorAll("body *")) {
          if (!el.getClientRects().length) continue;
          const style = getComputedStyle(el);
          const size = Math.round(Number.parseFloat(style.fontSize));
          if (!el.children.length && el.innerText?.trim()) sizes.add(size);
        }
        const h1 = document.querySelector("main h1") ?? document.querySelector("h1");
        return {
          sizes: [...sizes].sort((a, b) => a - b),
          h1: h1 && {
            size: Math.round(Number.parseFloat(getComputedStyle(h1).fontSize)),
            data: getComputedStyle(h1).fontFamily.includes(dataFace),
          },
        };
      }, { dataFace: DATA_FACE });

      const offRamp = type.sizes.filter((s) => !RAMP.has(s));
      // The document must never scroll sideways. This is the assertion the
      // whole admin surface failed silently until RFC 0004 Phase 5, because
      // nothing measured these routes narrow.
      const overflow = await page.evaluate(() => {
        const vw = window.innerWidth;
        const doc = document.documentElement.scrollWidth;
        if (doc <= vw) return null;
        // Name a culprit that is not already inside its own scroll container —
        // a wide table that scrolls in its own wrapper is correct, per
        // DESIGN.md's Own-Container Overflow Rule.
        const clipped = (el) => {
          for (let n = el.parentElement; n; n = n.parentElement) {
            const o = getComputedStyle(n).overflowX;
            if (o === "auto" || o === "hidden" || o === "scroll") return true;
          }
          return false;
        };
        const culprit = [...document.querySelectorAll("body *")]
          .find((el) => el.getBoundingClientRect().right > vw + 1 && !clipped(el));
        return {
          doc,
          vw,
          culprit: culprit
            ? `${culprit.tagName.toLowerCase()}.${(culprit.className || "").toString().split(" ").slice(0, 4).join(".")}`
            : "(inside a scroll container — check the wrapper)",
        };
      });

      const typeProblems = [];
      if (overflow) {
        typeProblems.push(
          `document scrolls sideways: ${overflow.doc}px on a ${overflow.vw}px viewport — ${overflow.culprit}`,
        );
      }
      if (offRamp.length) typeProblems.push(`sizes off the ramp: ${offRamp.join(", ")}px`);
      if (!type.h1) typeProblems.push("no h1");
      else if (type.h1.data) typeProblems.push(`h1 set in the data face (${type.h1.size}px)`);

      if (typeProblems.length) {
        fail();
        console.log(`${pinned ? "⚠" : "✗"} ${role} ${route} @${width}`);
        for (const p of typeProblems) console.log(`    [type] ${p}`);
      }

      if (results.violations.length) {
        fail();
        if (!typeProblems.length) console.log(`${pinned ? "⚠" : "✗"} ${role} ${route} @${width}`);
        for (const v of results.violations) {
          console.log(`    [${v.id}] ${v.help} — ${v.nodes.length} node(s)`);
          for (const n of v.nodes.slice(0, 3)) {
            console.log(`      ${n.target.join(" ")}`);
            // The summary carries the measured ratio and the colours axe used,
            // which is the difference between fixing the contrast and guessing.
            const why = (n.failureSummary ?? "").split("\n").filter(Boolean).slice(1, 3);
            for (const line of why) console.log(`        ${line.trim()}`);
          }
        }
      } else if (!typeProblems.length) {
        console.log(`✓ ${role} ${route} @${width}`);
      }
      if (pageErrors.length) {
        fail();
        console.log(`  ! page error: ${[...new Set(pageErrors)].join(" | ")}`);
      }
    } finally {
      if (pinned) (routeFailed ? pinnedRed : pinnedGreen).add(route);
      await page.close();
    }
   }
  }
}

await browser.disconnect();

for (const route of pinnedRed) {
  console.log(`\n⚠ ${route} failed as expected — ${EXPECTED_FAIL[route]}`);
}
for (const route of pinnedGreen) {
  if (pinnedRed.has(route)) continue; // failed at one viewport, clean at the other
  failures++;
  console.log(
    `\n✗ ${route} is pinned in EXPECTED_FAIL and now passes. One side of the ` +
      `disagreement moved — remove the pin in the commit that moved it.`,
  );
}

console.log(
  `\n${scanned} route/role/viewport combination(s) scanned, ${failures} with unexpected findings` +
    (pinnedRed.size ? `, ${pinnedRed.size} pinned` : ""),
);
process.exit(failures ? 1 : 0);
