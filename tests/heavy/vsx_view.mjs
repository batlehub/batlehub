#!/usr/bin/env node
// The browser half of tests/heavy/vsx_view.sh (RFC 0011 §4.4.2, §10): drives
// a real VS Code web workbench's Extensions view over the Chrome DevTools
// protocol and prints one JSON line per measurement. The shell suite owns the
// servers and the assertions; this file only looks and clicks.
//
//   node vsx_view.mjs --url <workbench> --shots <dir> [--after-anon <cmd>]
//        (--cdp <http://host:port> | --chrome <binary>)
//        [--search <text>] [--real <publisher.name>]
//
// Phases, all in one page — the workbench is never reloaded between them,
// which is the property under test ("nothing needs restarting"):
//   browse    the view with an empty search box (Popular asks the gallery)
//   search    `--search` typed in; the entries listed
//   readme    the first entry opened; the extension editor's header, status
//             line and rendered readme
//   install   the entry's Install button: enabled or not, the editor's
//             reason when not, and the entry's state after a click when it is
//   after-anon  `--after-anon` run (the suite signs in)
//   refresh   the view's Refresh clicked — the same text typed again is
//             answered from the view's own cache
//   search2   the entries listed after the refresh
//   readme2 / install2  the same two measurements on the first entry listed
//             (install2 also answers the editor's "Do you trust the
//             publisher?" dialog, and reports the notifications when the
//             click did not end in an install)
//   csp       how many gallery requests the browser itself refused
//
// `--phase signed` runs search2 and install2 only, for a second look at an
// editor that is already signed in (after a settings change). `--phase icon`
// runs one search and reports what the view drew for each entry's icon —
// `naturalWidth`, which is 0 for an image the browser refused or could not
// decode and is therefore the only honest way to ask "did the icon render".
//
// Browser: `--cdp` connects to a running Chrome (a workspace's sidecar); the
// page lives in its own browser context, so nothing of the browser's other
// tabs or storage is touched or seen. `--chrome` launches a headless one.
// puppeteer-core is resolved from ui/node_modules, the one place this
// repository already carries it.
//
// Selectors are the workbench's own class names, stable from 1.96 to 1.136:
// `.extensions-viewlet`, `.extension-list-item` with `.name` and
// `.publisher-name`, `.extension-action.install`, `.extension-editor`.
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(here, "..", "..", "ui", "package.json"));
const puppeteer = require("puppeteer-core");

const args = {};
for (let i = 2; i < process.argv.length; i += 2) args[process.argv[i].replace(/^--/, "")] = process.argv[i + 1];
const need = (k) => { if (!args[k]) { console.error(`missing --${k}`); process.exit(2); } return args[k]; };
const URL_ = need("url"), SHOTS = need("shots");
const SEARCH = args.search ?? "weebo";
// `--phase signed`: the sign-in already happened (a second look at the same
// editor after a settings change); skip the unauthenticated phases.
const PHASE = args.phase ?? "all";
const REAL = args.real ?? "batleforc.weebo-bridge-notify";
const SIGN_IN = "Sign in to BatleHub";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const emit = (o) => console.log(JSON.stringify(o));
let shot = 0;
const snap = (page, name) => page.screenshot({ path: path.join(SHOTS, `${String(++shot).padStart(2, "0")}-${name}.png`) }).catch(() => {});

// Every entry the view currently lists: display name, publisher, and the
// visible actions with their enablement (Install / Installing / Manage …).
const listed = (page) => page.$$eval(".extensions-viewlet .extension-list-item", (els) =>
  els.map((e) => {
    // The icon the editor actually drew. `naturalWidth` is the fact worth
    // having: it is 0 for an image the browser refused or could not decode,
    // and a `src` alone cannot tell those apart from one that rendered. This
    // is how "the marketplace serves an SVG icon as application/octet-stream"
    // is visible from outside the server (RFC 0007-bis §11 q1).
    const img = e.querySelector("img.icon, .icon img, .icon-container img");
    return {
      name: e.querySelector(".name")?.textContent?.trim() ?? "",
      publisher: e.querySelector(".publisher-name, .publisher")?.textContent?.trim() ?? "",
      icon: img
        ? { src: img.getAttribute("src") ?? "", naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight, complete: img.complete }
        : null,
      actions: [...e.querySelectorAll(".extension-action")]
        .filter((a) => !a.classList.contains("hide") && a.offsetParent !== null)
        .map((a) => (a.textContent.trim() || a.getAttribute("aria-label") || [...a.classList].filter((c) => c.startsWith("codicon-")).join(" ")) + (a.classList.contains("disabled") ? " (disabled)" : "")),
    };
  }));

// Poll the list until `ok` holds or the timeout passes; the last observation
// either way, so the suite sees what the view showed rather than a timeout.
async function settle(page, ok, timeout = 30000) {
  const t0 = Date.now();
  let rows = [];
  while (Date.now() - t0 < timeout) {
    rows = await listed(page);
    if (ok(rows)) return rows;
    await sleep(500);
  }
  return rows;
}

async function typeSearch(page, text) {
  const box = await page.waitForSelector(".extensions-viewlet textarea", { timeout: 30000 });
  await box.focus();
  await page.keyboard.down("Control"); await page.keyboard.press("a"); await page.keyboard.up("Control");
  await page.keyboard.press("Backspace");
  await sleep(300);
  if (text) await page.keyboard.type(text);
}

// The extension editor for the first listed entry: header, the status line
// the editor prints under the actions (where "not signed" is said), and the
// readme the editor renders in its webview — an iframe on the webview
// endpoint whose innermost document holds the markdown.
async function open(page) {
  const first = await page.$(".extensions-viewlet .extension-list-item .name");
  if (first) await first.click();
  await page.waitForSelector(".extension-editor", { timeout: 30000 });
  await sleep(1500);
  const header = await page.$eval(".extension-editor .header", (e) => e.innerText.replace(/\s+/g, " ").trim()).catch(() => "");
  const status = await page.$$eval(".extension-editor .status", (els) => els.map((e) => e.innerText.replace(/\s+/g, " ").trim()).filter(Boolean).join(" | ")).catch(() => "");
  let md = { frame: "", text: "" };
  const t0 = Date.now();
  while (Date.now() - t0 < 30000 && !md.text) {
    for (const f of page.frames()) {
      if (f === page.mainFrame() || /webWorkerExtensionHost/.test(f.url())) continue;
      const text = await f.evaluate(() => document.body?.innerText ?? "").catch(() => "");
      if (text.trim().length > 40 && !text.trim().startsWith("(function")) { md = { frame: f.url().replace(/^(https?:\/\/[^/]+).*/, "$1/…"), text }; break; }
    }
    if (!md.text) await sleep(500);
  }
  return { header, status, frame: md.frame, text: md.text.replace(/\s+/g, " ").trim().slice(0, 700) };
}

// Click the entry's own Install button, once the row is found by name.
async function clickInstall(page, name) {
  for (const it of await page.$$(".extensions-viewlet .extension-list-item")) {
    const n = await it.$eval(".name", (e) => e.textContent.trim()).catch(() => "");
    if (n !== name) continue;
    for (const b of await it.$$(".extension-action.install")) {
      if (await b.evaluate((e) => !e.classList.contains("hide") && !e.classList.contains("disabled") && e.offsetParent !== null)) { await b.click(); break; }
    }
    return;
  }
}

// The editor's next gate after the signature (1.136): "Do you trust the
// publisher?" — a modal that holds the install until answered. True when this
// pass answered it.
async function answerTrustPrompt(page) {
  let trusted = false;
  for (const b of await page.$$(".monaco-dialog-box .dialog-buttons .monaco-button, .monaco-dialog-box .dialog-buttons a")) {
    const t = (await b.evaluate((e) => e.textContent)).trim();
    if (t.startsWith("Trust Publisher")) { await b.click(); trusted = true; await sleep(500); }
  }
  return trusted;
}

// Whatever the toasts say right now. An install the server's verifier refuses
// lands in a notification toast, and the toast does not wait for the list.
async function toastMessages(page) {
  return page.$$eval(".notifications-toasts .notification-list-item-message, .notification-list-item-message",
    (els) => els.map((e) => e.innerText.replace(/\s+/g, " ").trim()).filter(Boolean)).catch(() => []);
}

// Wait for the entry's state to move off Install/Installing, collecting what
// the editor says meanwhile.
async function settleInstall(page, name) {
  const notifications = new Set();
  const t0 = Date.now();
  let after;
  let trusted = false;
  while (Date.now() - t0 < 90000) {
    if (await answerTrustPrompt(page)) trusted = true;
    for (const n of await toastMessages(page)) notifications.add(n);
    after = (await listed(page)).find((x) => x.name === name);
    const busy = after?.actions.some((a) => a.startsWith("Installing"));
    const pending = after?.actions.some((a) => a === "Install" || a.startsWith("Install ("));
    if (after && !busy && !pending) break;
    if (!busy && [...notifications].some((n) => /signature|verif/i.test(n))) break;
    await sleep(500);
  }
  return { after, trusted, notifications: [...notifications] };
}

// The entry named `name` in the list: whether its Install button is enabled;
// when it is, click it and wait for the state to move off Install.
async function install(page, name) {
  const before = (await listed(page)).find((x) => x.name === name);
  if (!before) return { name, listed: false };
  const enabled = before.actions.includes("Install");
  if (!enabled) return { name, listed: true, enabled, actions: before.actions };
  await clickInstall(page, name);
  const { after, trusted, notifications } = await settleInstall(page, name);
  const installed = !!after && !after.actions.some((a) => a.startsWith("Install"));
  return { name, listed: true, enabled, actions: after?.actions ?? [], installed, trusted, notifications };
}

const browser = args.cdp
  ? await puppeteer.connect({ browserURL: args.cdp })
  : await puppeteer.launch({ executablePath: need("chrome"), headless: true, args: ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"] });
const ctx = await browser.createBrowserContext();
const page = await ctx.newPage();
await page.setViewport({ width: 1280, height: 900 });
const csp = [];
page.on("console", (m) => { if (m.type() === "error" && /Content Security Policy/.test(m.text())) csp.push(m.text().slice(0, 160)); });

// What became of every icon request. `naturalWidth: 0` says the browser drew
// nothing and nothing else; it cannot tell "the request never left" from "it
// left and came back a 404" from "it arrived and would not decode". The first
// answer written down for this was a guess and it was wrong, so the driver
// reports the three facts that separate them instead.
const iconTraffic = [];
const isIcon = (url) => /Icons\.Default/.test(url);
page.on("response", (r) => {
  if (isIcon(r.url())) iconTraffic.push({ what: "response", status: r.status(), type: r.headers()["content-type"] ?? "", url: r.url().slice(-90) });
});
page.on("requestfailed", (r) => {
  if (isIcon(r.url())) iconTraffic.push({ what: "failed", reason: r.failure()?.errorText ?? "", url: r.url().slice(-90) });
});
page.on("requestfinished", (r) => {
  if (isIcon(r.url())) iconTraffic.push({ what: "finished", url: r.url().slice(-90) });
});

try {
  await page.goto(URL_, { waitUntil: "load", timeout: 60000 });
  await page.waitForSelector(".monaco-workbench", { timeout: 60000 });
  await sleep(3000);
  // A folder would raise the workspace-trust dialog; the suite opens none.
  // Should one appear anyway, trust it — this is a throwaway data dir.
  for (const b of await page.$$(".monaco-dialog-box .dialog-buttons .monaco-button, .monaco-dialog-box .dialog-buttons a")) {
    const t = (await b.evaluate((e) => e.textContent)).trim();
    if (t.startsWith("Yes")) { await b.click(); await sleep(1000); }
  }
  await (await page.waitForSelector('.activitybar [aria-label^="Extensions"]', { timeout: 30000 })).click();
  await page.waitForSelector(".extensions-viewlet", { timeout: 30000 });

  if (PHASE === "icon") {
    // One search, one measurement: the icon the view drew for `--real`. The
    // editor is already signed in (the suite wrote the token file before this
    // run), so no sign-in entry is expected and none is waited for.
    //
    // Settled on **that extension appearing**, not merely on "some row that is
    // not the sign-in entry": the view answers a new query from its own cache
    // first, so the looser predicate was satisfied by the previous search's
    // rows and the phase measured the wrong extension's icon.
    const [pub, name] = REAL.split(".");
    const mine = (r) => r.some((x) => x.publisher.toLowerCase() === pub.toLowerCase()
      || x.name.toLowerCase().replace(/[\s-]/g, "") === name.toLowerCase().replace(/[\s-]/g, ""));
    // Refresh first, and it is not belt and braces. The view answers a query
    // from its own cache, and this editor has been running since before the
    // extension was published — so without it the search returns the rows of an
    // earlier phase and the measurement is of the wrong extension's icon. The
    // signed-in phase clicks the same control for the same reason.
    await typeSearch(page, SEARCH);
    let entries = await settle(page, (r) => r.length > 0 && !r.some((x) => x.name === SIGN_IN) && mine(r), 8000);
    if (!mine(entries)) {
      const refresh = await page.$('.sidebar [aria-label^="Refresh"], .sidebar .codicon-extensions-refresh');
      if (refresh) await refresh.click();
      emit({ phase: "refresh", clicked: !!refresh });
      await sleep(1500);
      await typeSearch(page, SEARCH);
      entries = await settle(page, (r) => r.length > 0 && !r.some((x) => x.name === SIGN_IN) && mine(r));
    }
    // An `<img>` decodes after its row is listed, so a measurement taken the
    // instant the row appeared reads `naturalWidth: 0` on a perfectly good
    // icon. Give the pixels their own wait — and go on without them, because
    // whether the workbench *paints* a cross-origin icon is its CSP's decision
    // and the suite reports that rather than requiring it.
    const drawn = await settle(page, (r) => mine(r)
      && r.every((x) => !x.icon || !x.icon.src || x.icon.naturalWidth > 0), 15000);
    if (drawn.length && mine(drawn)) entries = drawn;
    await snap(page, "icon");
    emit({ phase: "icon", entries, iconTraffic });
    emit({ phase: "csp", refused: csp.length, sample: csp[0] ?? "" });
  } else if (PHASE === "signed") {
    await typeSearch(page, SEARCH);
    const [pub, name] = REAL.split(".");
    const search2 = await settle(page, (r) => r.length > 0 && !r.some((x) => x.name === SIGN_IN)
      && r.some((x) => x.publisher.toLowerCase() === pub.toLowerCase() || x.name.toLowerCase().includes(name.toLowerCase())));
    await snap(page, "signed-search");
    emit({ phase: "search2", entries: search2 });
    const realName = search2.find((x) => x.name !== SIGN_IN)?.name ?? "";
    const installed2 = realName ? await install(page, realName) : { name: "", listed: false };
    await snap(page, "signed-install");
    emit({ phase: "install2", ...installed2 });
    emit({ phase: "csp", refused: csp.length, sample: csp[0] ?? "" });
  } else {

  const browse = await settle(page, (r) => r.some((x) => x.name === SIGN_IN));
  await snap(page, "browse");
  emit({ phase: "browse", entries: browse });

  await typeSearch(page, SEARCH);
  const search = await settle(page, (r) => r.length > 0 && r.every((x) => x.name === SIGN_IN));
  await snap(page, "search");
  emit({ phase: "search", entries: search });

  const readme = await open(page);
  await snap(page, "readme");
  emit({ phase: "readme", ...readme });

  const installed = await install(page, SIGN_IN);
  await snap(page, "install");
  emit({ phase: "install", ...installed });

  if (args["after-anon"]) {
    // An absolute interpreter, not `bash` off PATH: this runs inside a
    // container whose PATH the harness does not own, and a writable directory
    // ahead of /bin there would decide what the hook executes.
    execFileSync("/bin/bash", ["-c", args["after-anon"]], { stdio: "inherit" });
    emit({ phase: "after-anon", ran: true });
  }

  // The view answers the same text from its own cache; a user who just
  // signed in presses the view's Refresh, and so does the driver.
  const [pub, name] = REAL.split(".");
  const fresh = (r) => r.length > 0 && !r.some((x) => x.name === SIGN_IN)
    && r.some((x) => x.publisher.toLowerCase() === pub.toLowerCase() || x.name.toLowerCase().includes(name.toLowerCase()));
  const refresh = await page.$('.sidebar [aria-label^="Refresh"], .sidebar .codicon-extensions-refresh');
  if (refresh) await refresh.click();
  emit({ phase: "refresh", clicked: !!refresh });
  const search2 = await settle(page, fresh);
  await snap(page, "search-signed-in");
  emit({ phase: "search2", entries: search2 });

  const readme2 = await open(page);
  await snap(page, "readme-signed-in");
  emit({ phase: "readme2", ...readme2 });

  const realName = search2.find((x) => x.name !== SIGN_IN)?.name ?? "";
  const installed2 = realName ? await install(page, realName) : { name: "", listed: false };
  await snap(page, "install-signed-in");
  emit({ phase: "install2", ...installed2 });
  emit({ phase: "csp", refused: csp.length, sample: csp[0] ?? "" });
  }
} finally {
  await snap(page, "final");
  await ctx.close().catch(() => {});
  if (args.cdp) await browser.disconnect(); else await browser.close();
}
