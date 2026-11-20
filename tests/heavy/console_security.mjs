#!/usr/bin/env node
// RFC 0042 §6.6 case 7, the browser half: what the console shows a caller,
// decided from `/me`'s verbs. Prints one JSON line; tests/heavy/console_fetch.sh
// owns the server and the assertions.
//
//   node console_security.mjs --base <console origin> --token <bearer>
//        (--cdp <http://host:port> | --chrome <binary>)
//
// Measures, in order: the primary nav's links once identity has resolved; the
// panels `/security` draws (or where it sends the caller instead); and where an
// admin page the caller holds no verb for sends them.
//
// RFC 0046 §6.6 adds the command palette: `--palette <query>` opens it with
// Ctrl+K, types the query and reads the rows' hrefs, then (with `--enter 1`)
// presses Enter and records the API paths the landing page requests
// (observations 2 and 4). `--editor 1` measures the configuration editor:
// Ctrl+K typed inside it opens nothing (obs. 5), and an unsaved edit asks
// before the sidebar or the palette leaves it and before the tab closes
// (obs. 7). Every API path the page requests is recorded with
// `page.on("request")`, the "requests made" source of obs. 2 and 4.
// `--bookmarks 1` loads each old path of RFC 0046 §15 Q8 with a query string
// and records where it lands (obs. 6).
//
// RFC 0050 obs. 11 (`--nav 1`): on an admin page, no tab strip is left in the
// DOM, and a sidebar group opened by the reader is still open after a reload.
import { args, emit, need, openPage, poll, sleep } from "./browser.mjs";

const BASE = need("base");
const TOKEN = args.token ?? "";

const { page, close } = await openPage(1440);
const requested = [];
page.on("request", (req) => {
  const url = new URL(req.url());
  if (url.pathname.startsWith("/api/v1/")) requested.push(url.pathname);
});
const since = (mark) => [...new Set(requested.slice(mark))];

/** Ctrl+K from wherever focus is, then wait for the palette's input. */
async function openPalette() {
  await page.keyboard.down("Control");
  await page.keyboard.press("k");
  await page.keyboard.up("Control");
  return page
    .waitForSelector('[role="dialog"] input[role="combobox"]', { timeout: 5000 })
    .then(() => true)
    .catch(() => false);
}
const paletteRows = () =>
  page.$$eval('[role="dialog"] [role="option"]', (as) =>
    as.map((a) => {
      const u = new URL(a.href);
      return u.pathname + u.search;
    }),
  );
/** The house leave confirmation (DestructiveConfirm): a dialog that is not the palette. */
const confirmShown = () =>
  page.$('[role="dialog"]:not(:has(input[role="combobox"]))').then((el) => el !== null);
async function cancelConfirm() {
  await page.keyboard.press("Escape");
  await poll(confirmShown, (shown) => !shown, 5000);
}

try {
  // Seeded before the app's first script, for the reason console_fetch.mjs gives.
  await page.evaluateOnNewDocument((value) => {
    // A reload that measures what the store kept (obs. 11) must not wipe it.
    if (!sessionStorage.getItem("keep-nav")) localStorage.clear();
    if (value) localStorage.setItem("batlehub_access_token", value);
  }, TOKEN);

  const nav = async () =>
    page.$$eval("header nav a", (as) => as.map((a) => new URL(a.href).pathname));
  await page.goto(`${BASE}/packages`, { waitUntil: "networkidle2", timeout: 60000 });
  const links = await poll(nav, (l) => l.includes("/packages"));

  await page.goto(`${BASE}/security`, { waitUntil: "networkidle2", timeout: 60000 });
  const securityLanded = new URL(page.url()).pathname;
  // RFC 0050 obs. 10: the overview's status strip, line by line, with its state.
  const status = await page
    .waitForSelector("[data-testid^='status-']", { timeout: 15000 })
    .then(() =>
      page.$$eval("[data-testid^='status-']", (els) =>
        els.map((e) => ({
          line: e.dataset.testid,
          state: e.querySelector("[data-state]")?.dataset.state ?? null,
        })),
      ),
    )
    .catch(() => []);
  // The panels now live one per page (RFC 0050 §6.6).
  const panels = [];
  for (const sub of ["holds", "flags", "exposure"]) {
    await page.goto(`${BASE}/security/${sub}`, { waitUntil: "networkidle2", timeout: 60000 });
    if (new URL(page.url()).pathname !== `/security/${sub}`) continue;
    panels.push(
      ...(await page.$$eval("[data-testid$='-panel']", (els) =>
        els.map((e) => e.getAttribute("data-testid")),
      )),
    );
  }
  // Obs. 10: a refused sub-page lands on the first page of its group the
  // caller may read.
  await page.goto(`${BASE}/security/holds`, { waitUntil: "networkidle2", timeout: 60000 });
  const holdsLanded = new URL(page.url()).pathname;

  await page.goto(`${BASE}/admin/system/configuration`, {
    waitUntil: "networkidle2",
    timeout: 60000,
  });
  const adminLanded = new URL(page.url()).pathname;

  const adminRequests = since(0).filter((p) => p.startsWith("/api/v1/admin/config"));

  // ── RFC 0046 obs. 2 and 4: the palette, from the catalog ──────────────────
  let palette = null;
  if (args.palette) {
    await page.goto(`${BASE}/packages`, { waitUntil: "networkidle2", timeout: 60000 });
    await page.click("body");
    const opened = await openPalette();
    await page.keyboard.type(args.palette);
    const rows = await poll(paletteRows, (r) => r.length > 0, 5000);
    palette = { opened, rows };
    if (args.enter) {
      const mark = requested.length;
      await page.keyboard.press("Enter");
      await poll(() => new URL(page.url()).pathname, (p) => p !== "/packages", 10000);
      await page.waitForNetworkIdle({ idleTime: 500, timeout: 15000 }).catch(() => {});
      const u = new URL(page.url());
      palette.landed = u.pathname + u.search;
      palette.requests = since(mark).filter((p) => p.startsWith("/api/v1/admin/"));
    }
  }

  // ── RFC 0046 obs. 6: old bookmarks land on the section tree ───────────────
  // A copy of §15 Q8's table (this script cannot import the `.ts` one); the
  // unit test `router/index.test.ts` checks the same pairs against the router.
  let bookmarks = null;
  if (args.bookmarks) {
    const OLD = {
      "/admin/observability/registries": "/admin/registries",
      "/admin/operations/upstream": "/admin/registries/upstream",
      "/admin/operations/warming": "/admin/registries/warming",
      "/admin/operations/imports": "/admin/registries/imports",
      "/admin/operations/air-gap": "/admin/registries/air-gap",
      "/admin/operations/sbom": "/admin/packages/sbom",
      "/admin/security/access-check": "/admin/access/authorization",
      "/admin/security/authorization": "/admin/access/authorization",
      "/admin/security/blocks": "/admin/instance-security/blocks",
      "/admin/namespaces/team-namespaces": "/admin/access/team-namespaces",
      "/admin/namespaces/beta-channel": "/admin/access/beta-channel",
      "/admin/operations/config-reload": "/admin/system/configuration",
      "/admin/operations/banner": "/admin/system/banner",
      "/admin/observability/health": "/admin/system/health",
      "/admin/observability/audit-log": "/admin/system/audit-log",
      "/admin/notifications/subscriptions": "/admin/system/notifications",
      "/admin/notifications/inbound": "/admin/system/inbound-events",
      // RFC 0050 §6.6.
      "/admin/security/scans": "/security/scanners",
      "/admin/security/findings": "/security/findings",
      "/admin/access/blocks": "/admin/instance-security/blocks",
    };
    bookmarks = [];
    for (const [from, want] of Object.entries(OLD)) {
      await page.goto(`${BASE}${from}?bookmark=1`, { waitUntil: "networkidle2", timeout: 60000 });
      const u = new URL(page.url());
      bookmarks.push({ from, want: `${want}?bookmark=1`, landed: u.pathname + u.search });
    }
  }

  // ── RFC 0050 obs. 11: no tab strip; a group's state survives a reload ─────
  let sidebar = null;
  if (args.nav) {
    const health = `${BASE}/admin/system/health`;
    await page.goto(health, { waitUntil: "networkidle2", timeout: 60000 });
    const tabStrips = await page.$$eval(
      '[data-testid="section-tabs"], #section-tab-select',
      (els) => els.length,
    );
    const access = 'button[aria-controls="admin-group-admin-access-pages"]';
    const expanded = () => page.$eval(access, (b) => b.getAttribute("aria-expanded"));
    const before = await expanded();
    await page.click(access);
    const afterClick = await expanded();
    // evaluateOnNewDocument clears the store on every load; this reload must keep it.
    await page.evaluate(() => sessionStorage.setItem("keep-nav", "1"));
    await page.reload({ waitUntil: "networkidle2", timeout: 60000 });
    const afterReload = await expanded();
    sidebar = { tabStrips, before, afterClick, afterReload };
  }

  // ── RFC 0046 obs. 5 and 7: the configuration editor ────────────────────────
  let editor = null;
  if (args.editor) {
    const cfg = `${BASE}/admin/system/configuration`;
    await page.goto(cfg, { waitUntil: "networkidle2", timeout: 60000 });
    const area = await page.waitForSelector("textarea", { timeout: 15000 });
    await area.click();
    const before = await area.evaluate((el) => el.value);
    const shortcutInEditorOpened = await openPalette();
    const afterShortcut = await area.evaluate((el) => el.value);
    // An edit, then leave by the sidebar.
    await area.type("\n# unsaved edit\n");
    await page.click('aside a[href$="/admin/system/audit-log"]');
    const sidebarAsked = await poll(confirmShown, (s) => s, 5000);
    await cancelConfirm();
    const keptAfterSidebar = new URL(page.url()).pathname.endsWith("/system/configuration");
    // Then by the palette, focus out of the textarea first (obs. 5's rule).
    await page.click("h1");
    await openPalette();
    await page.keyboard.type("audit");
    await poll(paletteRows, (r) => r.length > 0, 5000);
    await page.keyboard.press("Enter");
    const paletteAsked = await poll(confirmShown, (s) => s, 5000);
    await cancelConfirm();
    const editKept = (await page.$eval("textarea", (el) => el.value)).includes("# unsaved edit");
    // Closing the tab: the browser's own prompt, only while dirty.
    let unloadPrompt = false;
    page.once("dialog", async (d) => {
      unloadPrompt = d.type() === "beforeunload";
      await d.dismiss();
    });
    await page.close({ runBeforeUnload: true });
    await sleep(1000);
    editor = {
      shortcutInEditorOpened,
      textareaUnchangedByShortcut: before === afterShortcut,
      sidebarAsked,
      keptAfterSidebar,
      paletteAsked,
      editKept,
      unloadPrompt,
    };
  }

  emit({
    phase: "security",
    links,
    securityLanded,
    status,
    panels,
    holdsLanded,
    adminLanded,
    adminRequests,
    palette,
    bookmarks,
    editor,
    sidebar,
  });
} finally {
  await close();
}
