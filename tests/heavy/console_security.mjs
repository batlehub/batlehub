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
import { args, emit, need, openPage, poll } from "./browser.mjs";

const BASE = need("base");
const TOKEN = args.token ?? "";

const { page, close } = await openPage(1440);
try {
  // Seeded before the app's first script, for the reason console_fetch.mjs gives.
  await page.evaluateOnNewDocument((value) => {
    localStorage.clear();
    if (value) localStorage.setItem("batlehub_access_token", value);
  }, TOKEN);

  const nav = async () =>
    page.$$eval("header nav a", (as) => as.map((a) => new URL(a.href).pathname));
  await page.goto(`${BASE}/packages`, { waitUntil: "networkidle2", timeout: 60000 });
  const links = await poll(nav, (l) => l.includes("/packages"));

  await page.goto(`${BASE}/security`, { waitUntil: "networkidle2", timeout: 60000 });
  const panels = await page.$$eval("[data-testid$='-panel']", (els) =>
    els.map((e) => e.getAttribute("data-testid")),
  );
  const securityLanded = new URL(page.url()).pathname;

  await page.goto(`${BASE}/admin/operations/config-reload`, {
    waitUntil: "networkidle2",
    timeout: 60000,
  });
  const adminLanded = new URL(page.url()).pathname;

  emit({ phase: "security", links, securityLanded, panels, adminLanded });
} finally {
  await close();
}
