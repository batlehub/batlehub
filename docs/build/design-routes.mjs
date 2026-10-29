#!/usr/bin/env node
/**
 * The documentation site's rendered design gate (RFC 0005 §10, phase 4).
 *
 * Until this existed, `impeccable detect ui/src website/.vitepress` was the
 * site's entire design coverage: a *static source* scan. The console has had a
 * rendered gate since RFC 0003 — axe plus the type ramp and the display face,
 * over 30 route/role pairs at two viewports — and the surface every prospective
 * user actually sees had nothing measured in a browser at all.
 *
 * This is the console's `ui/build/design-routes.mjs` pointed at a static site,
 * with the differences that follow from that: there is no session to seed, so
 * the route list is not a hand-maintained array but *every page in the build*,
 * read off `.vitepress/dist`. A page that is added and never listed anywhere is
 * exactly the page that drifts, so the list cannot be a list.
 *
 *   node build/design-routes.mjs                 # every page, every plan
 *   node build/design-routes.mjs --survey        # report, never fail
 *   ROUTES=/,/guide/installation.html node build/design-routes.mjs
 *
 * It talks to an already-running Chrome over CDP (`puppeteer-core`, so no
 * second browser is downloaded) — in this workspace, the `che-browser` sidecar.
 * BASE points at a `vitepress preview` of the build under test.
 *
 * ── What this gate cannot see ───────────────────────────────────────────────
 *
 * Both of these were found by asking why it had passed the config generator
 * while that page carried a colour measuring 4.15:1. Neither is a bug in the
 * script; both are limits worth stating, because a gate whose blind spots are
 * undocumented gets trusted for things it never checked.
 *
 * 1. ONE STATE PER PAGE — closed, for the one page it applied to. Every page is
 *    loaded once, in whatever state it opens in, and for prose that is the
 *    whole page. `/guide/config-generator` is a form, and its preview only
 *    emits the token classes the current form state produces: with the defaults
 *    there are twelve `.cg-hl-bracket` spans and *zero* `.cg-hl-comment`, which
 *    is where the failing colour was hiding. `SEEDS` below now drives that page
 *    before measuring it, and asserts the classes it meant to produce actually
 *    appeared. Verified by reintroducing the colour: the gate reports
 *    `4.15:1 (#6e7781 on #faf3f3)` at 1440·light, where it used to report
 *    nothing. Any *other* page whose appearance depends on interaction is still
 *    measured in its opening state — the mechanism exists, the entry does not.
 *
 * 2. AXE SKIPS PUNCTUATION-ONLY TEXT. `color-contrast` returns no verdict at
 *    all — not a pass, not an incomplete — for a node whose visible text has no
 *    word characters. Proven by swapping one span's text on a live page and
 *    changing nothing else: `"["` gets no verdict, `"section"` is judged at
 *    4.74:1. A syntax highlighter is mostly brackets and equals signs, so a
 *    large share of its tokens are invisible to the contrast rule.
 *
 * The ramp and material assertions below have neither limit — they walk every
 * element with its own text, whatever that text is. It is specifically the
 * contrast half that is axe's, and axe's rules are axe's.
 */
import puppeteer from "puppeteer-core";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const AXE_SOURCE = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const DIST = join(ROOT, ".vitepress", "dist");
const BASE = process.env.BASE ?? "http://localhost:4173";
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
const SURVEY = process.argv.includes("--survey");

/** WCAG 2.2 AA — the same tag set the console's gate uses. */
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

/**
 * DESIGN.md's two ramps. JetBrains Mono runs 12 / 13 / 15 / 16 / 20; Silkscreen
 * is drawn on an 8px em and only ever appears at a multiple of it.
 *
 * The display set is not `--t-display`'s four steps. This surface spends 24 on
 * a page title, 16 on a section heading and 40 / 72 on the home hero — every
 * one an integer multiple, which is the rule that actually governs the face
 * (The Integer Em Rule). `--t-display`'s own 56 / 88 / 104 are a property of
 * the console's full-bleed specimen head, not of Silkscreen.
 */
const TEXT_RAMP = new Set([12, 13, 15, 16, 20]);
const DISPLAY_RAMP = new Set([16, 24, 40, 56, 72, 88, 104]);

/**
 * The two plans. Contrast and hierarchy are width-blind; overflow and reflow
 * are not — and a rendition change moves every ratio on the page, so the light
 * ground is a plan rather than a spot check. 390 runs dark only: it is the
 * viewport where reflow is measured, and reflow does not depend on the ground.
 */
const PLANS = [
  { name: "1440·dark", width: 1440, height: 900, theme: "dark" },
  { name: "1440·light", width: 1440, height: 900, theme: "light" },
  { name: "390·dark", width: 390, height: 844, theme: "dark" },
];

/** Every page in the build. Not a list — see the header. */
function builtRoutes(dir = DIST, acc = []) {
  for (const entry of readdirSync(dir)) {
    if (entry === "assets" || entry === "fonts") continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) builtRoutes(full, acc);
    else if (entry.endsWith(".html"))
      acc.push("/" + relative(DIST, full).split(sep).join("/"));
  }
  return acc.sort((a, b) => a.localeCompare(b));
}

/**
 * What the page must be true of, measured in the page rather than inferred from
 * the stylesheet. Returns findings; an empty array is a pass.
 */
function assertions(textRampArr, displayRampArr) {
  // The ramps arrive as arrays: this function is serialised into the page, so
  // it closes over nothing from this module.
  const TEXT_RAMP = new Set(textRampArr);
  const DISPLAY_RAMP = new Set(displayRampArr);
  const findings = [];
  const add = (rule, detail) => findings.push({ rule, detail });

  // 1. The specimen faces actually painted. This is the defect `ui/` shipped:
  //    a Google Fonts import its own CSP refused, so every surface fell back to
  //    `ui-monospace` while every gate stayed green. A self-hosted face that
  //    404s fails exactly the same way and looks exactly as fine.
  for (const spec of ['700 16px "Silkscreen"', '400 16px "JetBrains Mono"']) {
    if (!document.fonts.check(spec)) add("face-not-painted", spec);
  }

  // 2. The body never scrolls horizontally. Wide content scrolls inside its own
  //    container (The Own-Container Overflow Rule).
  const de = document.documentElement;
  if (de.scrollWidth > de.clientWidth)
    add("body-overflow", `${de.scrollWidth} > ${de.clientWidth}`);

  const label = (el) =>
    `${el.tagName.toLowerCase()}${el.type && el.tagName === "INPUT" ? `[${el.type}]` : ""}${el.className && typeof el.className === "string" ? "." + el.className.trim().split(/\s+/).slice(0, 2).join(".") : ""}`;

  // Every helper below is declared *inside* `assertions` for the same reason the
  // ramps arrive as arguments: the whole function is serialised into the page,
  // so it may close over nothing this module holds.

  /**
   * One box-shadow list, split on its top-level commas. Hand-walked rather than
   * `split(/,(?![^(]*\))/)`: that lookahead rescans the rest of the string from
   * every comma, which is quadratic on a long shadow list.
   */
  const shadowLayers = (value) => {
    if (value === "none") return [];
    const layers = [];
    let depth = 0;
    let start = 0;
    for (let i = 0; i < value.length; i++) {
      const c = value[i];
      if (c === "(") depth++;
      else if (c === ")") depth--;
      else if (c === "," && depth === 0) {
        layers.push(value.slice(start, i));
        start = i + 1;
      }
    }
    layers.push(value.slice(start));
    return layers;
  };

  /** A layer's `<length>` components, in order: offset-x, offset-y, blur, spread. */
  const shadowLengths = (layer) =>
    layer
      // Drops `rgb(…)`/`rgba(…)` bodies so their numbers are not read as
      // lengths. The function *name* can stay — it carries no `px` token.
      .replace(/\([^()]*\)/g, "")
      .split(/\s+/)
      .filter((token) => token.endsWith("px"));

  // 3. Nothing glows, and depth is inked rather than lit. Blur is what the
  //    Flat-At-Rest Rule is actually about — the system's own two shadows are
  //    box-shadows, and both are zero-blur. A zero-blur shadow is a hairline
  //    drawn with the shadow property (which is how the default theme draws
  //    the code-group tab rule); a blurred one is elevation, and this world
  //    has none.
  const checkDepth = (el, cs) => {
    for (const layer of shadowLayers(cs.boxShadow)) {
      const blur = Number.parseFloat(shadowLengths(layer)[2] ?? "0");
      if (blur > 0) add("shadow-at-rest", `${label(el)} blur ${blur}px`);
    }
    if (cs.textShadow !== "none") add("text-shadow", `${label(el)} ${cs.textShadow}`);
  };

  // 4. Zero radius everywhere. The world has no rounded corner.
  const checkRadius = (el, cs) => {
    for (const corner of [
      cs.borderTopLeftRadius,
      cs.borderTopRightRadius,
      cs.borderBottomLeftRadius,
      cs.borderBottomRightRadius,
    ]) {
      if (corner !== "0px" && corner !== "0%")
        add("non-zero-radius", `${label(el)} ${corner}`);
    }
  };

  // 5. The type ramp, per face. A size outside the ramp is a step nobody
  //    declared; a Silkscreen size off the 8px em is a pixel that is no
  //    longer square. Only elements with text of their own are measured.
  const checkTypeRamp = (el, cs) => {
    const hasOwnText = [...el.childNodes].some((n) => n.nodeType === 3 && n.textContent.trim());
    if (!hasOwnText) return;
    const size = Math.round(Number.parseFloat(cs.fontSize) * 100) / 100;
    if (/silkscreen/i.test(cs.fontFamily)) {
      if (size % 8 !== 0 || !DISPLAY_RAMP.has(size))
        add("off-display-ramp", `${label(el)} ${size}px`);
    } else if (!TEXT_RAMP.has(size)) {
      add("off-text-ramp", `${label(el)} ${size}px`);
    }
  };

  for (const el of document.querySelectorAll("body *")) {
    const cs = getComputedStyle(el);
    if (cs.display === "none" || cs.visibility === "hidden") continue;
    const r = el.getBoundingClientRect();
    if (!r.width && !r.height) continue;

    checkDepth(el, cs);
    checkRadius(el, cs);
    checkTypeRamp(el, cs);
  }
  return findings;
}

const browser = await puppeteer.connect(
  CDP_WS ? { browserWSEndpoint: CDP_WS } : { browserURL: CDP },
);
const routes = process.env.ROUTES ? process.env.ROUTES.split(",") : builtRoutes();
if (!routes.length) {
  console.error(`no built pages under ${DIST} — run \`vitepress build\` first`);
  process.exit(2);
}
/* ────────────────────────────────────────────────────────────────────────────
   Pages that have to be driven before they are worth measuring.

   Limit 1 in the header above: a page is loaded once, in the state it opens in.
   For every page on this site that is prose, that is the whole page. For the
   config generator it is not — it is a form, and its output pane only emits the
   token classes the current form state produces. With the defaults it renders
   twelve `.cg-hl-bracket` spans and zero `.cg-hl-comment`, which is how a
   colour measuring 4.15:1 sat in it behind a green gate.

   So the generator gets driven first. Each seed returns the selectors it means
   to have produced, and the gate fails if they are still absent — a seed that
   silently stops working would restore the blind spot it exists to close,
   which is the failure mode RFC 0003 named when six console pages reported
   clean while none of them had rendered.
   ──────────────────────────────────────────────────────────────────────────── */
const SEEDS = {
  "/guide/config-generator.html": {
    /** Requests to abort before the page loads, and why. */
    block: [
      // `hash-wasm` is a dynamic import, so it is its own chunk and can be
      // refused. The component's documented fallback then writes the two `#`
      // lines that are the only path to `.cg-hl-comment` in the whole
      // generator — and it is a path a real reader on an old browser gets.
      //
      // Rollup names that chunk after the *file* it resolved, not the package,
      // and hash-wasm's ESM entry is `dist/index.esm.js` — so the chunk ships as
      // `index.esm.<hash>.js` and `/hash-wasm/` alone matched nothing. The
      // import then succeeded, the fallback never ran, and `expect` below caught
      // it. Keep both patterns: the package name in case the entry is ever
      // renamed, and the entry name for what actually ships today.
      /hash-wasm/,
      /\bindex\.esm\.[\w-]+\.js\b/,
    ],
    async drive(page) {
      // A token, because `[[auth.tokens]]` only emits for a non-empty value.
      const token = await page.$('input[placeholder="my-secret-token"]');
      if (token) {
        await token.click();
        await token.type("gate-seed-token");
      }
      // Every checkbox, because the booleans in the output are all optional
      // sections and flags. Ticking them by hand would encode this form's
      // current shape into the gate; ticking all of them does not.
      const boxes = await page.$$(".cg-root input[type=checkbox]");
      for (const box of boxes) await box.click().catch(() => {});
      // The preview is reactive; give Vue a frame to re-render it.
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
    },
    /** What driving it was for. Absent → the seed has stopped working. */
    expect: [".cg-hl-comment", ".cg-hl-bool", ".cg-hl-bracket", ".cg-hl-string"],
  },
};

/** Built pages that are only a `meta refresh` to somewhere else. */
const STUBS = new Set(
  routes.filter((r) =>
    /http-equiv="refresh"/i.test(readFileSync(join(DIST, r.slice(1)), "utf8")),
  ),
);
console.log(
  `${routes.length - STUBS.size} pages × ${PLANS.length} plans` +
    (STUBS.size ? ` (${STUBS.size} redirect stubs skipped)` : ""),
);

/** Anything the page fetches from off-box. The Google Fonts import this RFC
 *  removed was invisible to `pnpm audit` and to the postmortem gate, because it
 *  is not a package — the only place it shows up is here. */
const OFFBOX = new Set();
const failures = [];
const surveyed = new Map();

/**
 * A page at the plan's size and in its theme, recording every off-box origin it
 * asks for. `block` is a seed's list of requests to abort; arming interception
 * has to happen before the navigation it affects, so a seeded route gets a page
 * of its own.
 */
async function openPlanPage(plan, block = null) {
  const page = await browser.newPage();
  await page.setViewport({ width: plan.width, height: plan.height });
  if (block) await page.setRequestInterception(true);
  page.on("request", (req) => {
    const u = new URL(req.url());
    if (u.protocol !== "data:" && u.hostname !== new URL(BASE).hostname)
      OFFBOX.add(u.origin);
    if (!block) return;
    if (block.some((p) => p.test(req.url()))) req.abort().catch(() => {});
    else req.continue().catch(() => {});
  });
  // VitePress stores `system|light|dark` and resolves it before first paint;
  // the gate sets the stored preference rather than emulating the OS, so it
  // measures the same code path a reader's toggle does.
  await page.goto(BASE + "/", { waitUntil: "domcontentloaded" });
  await page.evaluate(
    (t) => localStorage.setItem("vitepress-theme-appearance", t),
    plan.theme,
  );
  return page;
}

/** A finding: counted under `--survey`, a failure otherwise. */
function record(plan, route, f) {
  if (SURVEY) {
    const k = `${f.rule} ${f.detail}`;
    surveyed.set(k, (surveyed.get(k) ?? 0) + 1);
  } else failures.push({ plan: plan.name, route, ...f });
}

for (const plan of PLANS) {
  const page = await openPlanPage(plan);

  for (const route of routes) {
    // Redirect stubs (RFC 0005-bis §6.5) carry a `meta refresh` and would be
    // measured as whatever they redirect to — the same page, twice, under the
    // wrong name. They are three lines of prose behind an instant redirect;
    // their target is in the sweep on its own account.
    if (STUBS.has(route)) continue;

    const seed = SEEDS[route];
    const view = seed ? await openPlanPage(plan, seed.block) : page;

    await view.goto(BASE + route, { waitUntil: "networkidle0" });

    if (seed) {
      await seed.drive(view);
      const missing = await view.evaluate(
        (sel) => sel.filter((s) => !document.querySelector(s)),
        seed.expect,
      );
      if (missing.length) {
        failures.push({
          plan: plan.name,
          route,
          rule: "seed",
          detail: `driving the page produced no ${missing.join(", ")} — the gate is measuring less than it thinks`,
        });
      }
    }

    // `?? null` keeps the absent case reading as `null`, the way `getAttribute`
    // reported it, rather than `undefined`.
    const rendition = await view.evaluate(
      () => document.documentElement.dataset.theme ?? null,
    );
    if (rendition !== plan.theme)
      failures.push({ plan: plan.name, route, rule: "rendition", detail: rendition });

    // A cold browser's first page can be measured before it has requested a
    // face at all: `networkidle0` saw nothing pending because nothing was asked
    // for yet, and `fonts.check` then reads "not painted" (the first route,
    // `/404.html`, failed this way twice in CI and never locally). Asking for
    // both faces first keeps what the check is for — a face its CSP refuses or
    // that 404s still fails to load, and still fails the check below.
    await view.evaluate(async () => {
      await document.fonts.ready;
      await Promise.all(
        ['700 16px "Silkscreen"', '400 16px "JetBrains Mono"'].map((s) =>
          document.fonts.load(s).catch(() => {}),
        ),
      );
    });
    const findings = await view.evaluate(assertions, [...TEXT_RAMP], [...DISPLAY_RAMP]);
    await view.evaluate(AXE_SOURCE);
    const axe = await view.evaluate(
      async (tags) => (await window.axe.run(document, { runOnly: { type: "tag", values: tags } })).violations,
      TAGS,
    );
    if (seed) await view.close();

    for (const f of findings) record(plan, route, f);
    for (const v of axe) {
      for (const n of v.nodes) {
        record(plan, route, {
          rule: `axe:${v.id}`,
          detail: `${n.target.join(" ")} — ${(n.failureSummary ?? v.help).replace(/\s+/g, " ").slice(0, 160)}`,
        });
      }
    }
  }
  await page.close();
  console.log(`  ${plan.name} done`);
}

if (OFFBOX.size) {
  const detail = [...OFFBOX].join(", ");
  if (SURVEY) surveyed.set(`offbox-request ${detail}`, 1);
  else failures.push({ plan: "-", route: "-", rule: "offbox-request", detail });
}

await browser.disconnect();

if (SURVEY) {
  for (const [k, n] of [...surveyed].sort((a, b) => b[1] - a[1]))
    console.log(`${String(n).padStart(5)}  ${k}`);
  process.exit(0);
}

if (failures.length) {
  const byRule = new Map();
  for (const f of failures) {
    if (!byRule.has(f.rule)) byRule.set(f.rule, []);
    byRule.get(f.rule).push(f);
  }
  console.error(`\n${failures.length} findings:\n`);
  for (const [rule, fs] of [...byRule].sort((a, b) => b[1].length - a[1].length)) {
    console.error(`${rule} — ${fs.length}`);
    for (const f of fs.slice(0, 8))
      console.error(`    ${f.plan} ${f.route} — ${f.detail}`);
    if (fs.length > 8) console.error(`    … ${fs.length - 8} more`);
  }
  process.exit(1);
}
console.log("clean");
