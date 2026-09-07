#!/usr/bin/env node
// The browser half of tests/heavy/console_fetch.sh (RFC 0007-bis §11 q3):
// drives a real browser through the console's package catalogue and presses the
// Fetch button on an upstream row. Prints one JSON line per measurement; the
// shell suite owns the server and every assertion about what the server did.
//
//   node console_fetch.mjs --base <console origin> --search <text>
//        [--token <bearer>] [--shots <dir>]
//        (--cdp <http://host:port> | --chrome <binary>)
//
// Phases:
//   listing   the rows the catalogue drew for `--search`, each with its state
//             chip, its version cell, and the label of its Fetch button if it
//             has one
//   click     the button pressed, and what the row looked like afterwards
//
// The token is seeded into `localStorage` before the app's first script runs:
// `initAuth` reads it synchronously at import time and the router resolves
// identity once, so seeding after `goto` races that — the same reason
// `ui/build/design-routes.mjs` gives.
//
// Selectors are the catalogue's own: `tbody tr` for the rows, and the button
// is found by its accessible name, which the page composes from the package
// and the version (`packageCatalog.fetchVersionLabel`). Finding it by name
// rather than by a test id is deliberate: if the accessible name stops naming
// the package, a screen-reader user loses the row's identity, and this run
// should notice.
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(here, "..", "..", "ui", "package.json"));
const puppeteer = require("puppeteer-core");

const args = {};
for (let i = 2; i < process.argv.length; i += 2) args[process.argv[i].replace(/^--/, "")] = process.argv[i + 1];
const need = (k) => { if (!args[k]) { console.error(`missing --${k}`); process.exit(2); } return args[k]; };

const BASE = need("base");
// Empty rather than absent for the anonymous run: `--token ""` is a reader with
// no session, and seeding the string "null" or "undefined" would make
// `initAuth` read it as a bearer credential — the trap `design-routes.mjs`
// records.
const TOKEN = args.token ?? "";
const SEARCH = need("search");
const SHOTS = args.shots ?? "";
const TOKEN_KEY = "batlehub_access_token";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const emit = (o) => console.log(JSON.stringify(o));

async function snap(page, name) {
  if (!SHOTS) return;
  await page.screenshot({ path: path.join(SHOTS, `console-fetch-${name}.png`), fullPage: true }).catch(() => {});
}

// One entry per **package** row, with its action folded in.
//
// The catalogue draws a package as more than one `<tr>`: the row itself, then
// the prose snippet, the refusal note and the Fetch action each in a row of
// their own beneath it. A driver that mapped `tbody tr` one-to-one — the first
// version of this did — reported the button as a row with no package, no
// version and no link, and then watched the wrong text for a change.
//
// So the walk carries a subject: a `<tr>` with the package link starts one,
// and every row after it without a link belongs to it.
const rows = (page) => page.$$eval("tbody tr", (trs) => {
  const out = [];
  for (const tr of trs) {
    const link = tr.querySelector("a[href*='/packages/']");
    const cells = [...tr.querySelectorAll("td")].map((td) => td.textContent.trim());
    const text = tr.textContent.replace(/\s+/g, " ").trim();
    const button = tr.querySelector("button");
    if (link) {
      out.push({
        // The coordinate, taken from the page rather than parsed out of a
        // label: the name is the row's own link, `/packages/{registry}/{name}`,
        // percent-encoded exactly as the console encodes it, and the version is
        // the version cell. The suite then asks the server about that
        // coordinate, so what it verifies is what the reader pressed.
        href: link.getAttribute("href"),
        name: link.textContent.trim(),
        state: cells[0] ?? "",
        version: cells[2] ?? "",
        text,
        button: null,
        under: [],
      });
    } else if (out.length) {
      const subject = out[out.length - 1];
      subject.under.push(text);
      if (button) {
        subject.button = {
          label: button.textContent.trim(),
          ariaLabel: button.getAttribute("aria-label") ?? "",
          disabled: button.disabled,
        };
      }
    }
  }
  return out;
});

// Poll until `ok` holds, then return what was seen; on timeout return the last
// observation anyway, so the suite reports what the page showed rather than
// "timed out".
async function settle(page, ok, timeout = 30000) {
  const t0 = Date.now();
  let seen = [];
  while (Date.now() - t0 < timeout) {
    seen = await rows(page);
    if (ok(seen)) return seen;
    await sleep(500);
  }
  return seen;
}

const browser = args.cdp
  ? await puppeteer.connect({ browserURL: args.cdp })
  : await puppeteer.launch({ executablePath: need("chrome"), headless: true, args: ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"] });
// Its own context: on a workspace's shared sidecar this must not read or write
// the profile the rest of the browser is using.
const ctx = await browser.createBrowserContext();
const page = await ctx.newPage();
await page.setViewport({ width: 1440, height: 900 });
const errors = [];
page.on("pageerror", (e) => errors.push(String(e.message).split("\n")[0].slice(0, 160)));
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text().slice(0, 160)); });

try {
  // Cleared either way, and before the app's first script runs: `initAuth`
  // reads the token synchronously at import time and the router resolves
  // identity once, so seeding after `goto` races that. On a shared browser
  // profile — a workspace's sidecar — an anonymous run that merely declined to
  // seed would read whatever the signed-in run left behind.
  await page.evaluateOnNewDocument((key, value) => {
    localStorage.clear();
    if (value) localStorage.setItem(key, value);
  }, TOKEN_KEY, TOKEN);

  await page.goto(`${BASE}/packages`, { waitUntil: "networkidle2", timeout: 60000 });
  await page.waitForSelector("table", { timeout: 30000 });

  // The catalogue debounces at 300 ms and only asks upstream from two
  // characters. Typing rather than setting `value`: the page listens for
  // `input`, and an assignment fires nothing.
  const box = await page.waitForSelector("input", { timeout: 15000 });
  await box.click();
  await box.type(SEARCH, { delay: 30 });

  // With a session, wait for the button — its absence would otherwise be
  // indistinguishable from a page that had not finished loading. Without one,
  // wait for the rows and then look again after a pause: the assertion is that
  // no button appears, and a check taken the instant the rows arrived would
  // pass against a button that was one tick behind them.
  const listing = TOKEN
    ? await settle(page, (r) => r.some((row) => row.button))
    : await (async () => {
        await settle(page, (r) => r.length > 0);
        await sleep(3000);
        return rows(page);
      })();
  await snap(page, "listing");
  emit({ phase: "listing", rows: listing, errors });

  const target = listing.find((r) => r.button && !r.button.disabled);
  if (!target) {
    emit({ phase: "click", clicked: false, reason: "no row offered a fetch" });
  } else {
    const { href, version, text: before } = target;
    const label = target.button.label;
    const ariaLabel = target.button.ariaLabel;
    // Clicked by its accessible name, so the control the suite presses is the
    // one a keyboard user reaches. If that name stops naming the package, this
    // run should notice.
    const clicked = await page.evaluate((name) => {
      const b = [...document.querySelectorAll("tbody tr button")]
        .find((el) => (el.getAttribute("aria-label") ?? el.textContent.trim()) === name);
      if (!b) return false;
      b.click();
      return true;
    }, ariaLabel || label);

    // The page refetches both halves of the catalogue on success, so this
    // package stops being an upstream discovery: its row either leaves the
    // table or comes back with a held state and no button. Watched on *this*
    // row, found by its link, rather than on the table as a whole — another
    // row changing is not this button working.
    const after = await settle(page, (r) => {
      const mine = r.find((row) => row.href === href);
      return !mine || mine.text !== before;
    }, 90000);
    await snap(page, "clicked");
    emit({ phase: "click", clicked, label, ariaLabel, before, href, version, rows: after, errors });
  }
} finally {
  await snap(page, "final");
  await ctx.close().catch(() => {});
  if (args.cdp) await browser.disconnect(); else await browser.close();
}
