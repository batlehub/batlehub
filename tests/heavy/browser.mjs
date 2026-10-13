// What the heavy suites' browser drivers (`console_fetch.mjs`, `vsx_view.mjs`)
// share: the argument parser, the JSON-line output, the browser and its private
// context, and the poll every measurement is taken through.
//
// puppeteer-core is resolved from ui/node_modules, the one place this
// repository already carries it.
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(here, "..", "..", "ui", "package.json"));
const puppeteer = require("puppeteer-core");

/** `--key value` pairs, every flag taking a value. */
export const args = {};
for (let i = 2; i < process.argv.length; i += 2) args[process.argv[i].replace(/^--/, "")] = process.argv[i + 1];
export const need = (k) => { if (!args[k]) { console.error(`missing --${k}`); process.exit(2); } return args[k]; };

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const emit = (o) => console.log(JSON.stringify(o));

/**
 * `--cdp` connects to a running Chrome (a workspace's sidecar), `--chrome`
 * launches a headless one. The page lives in its own browser context either
 * way: on a shared sidecar it must not read or write the profile the rest of
 * the browser is using. `close()` disconnects from a browser it did not start.
 */
export async function openPage(width) {
  const browser = args.cdp
    ? await puppeteer.connect({ browserURL: args.cdp })
    : await puppeteer.launch({ executablePath: need("chrome"), headless: true, args: ["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"] });
  const ctx = await browser.createBrowserContext();
  const page = await ctx.newPage();
  await page.setViewport({ width, height: 900 });
  const close = async () => {
    await ctx.close().catch(() => {});
    if (args.cdp) await browser.disconnect(); else await browser.close();
  };
  return { page, close };
}

/**
 * Read until `ok` holds or the timeout passes, every 500 ms. Returns the last
 * observation either way, so the suite reports what the page showed rather
 * than "timed out". Sequential by nature: each read looks at the page the
 * previous one left.
 */
export async function poll(read, ok, timeout = 30000) {
  const t0 = Date.now();
  let seen = await read();
  while (!ok(seen) && Date.now() - t0 < timeout) {
    await sleep(500);
    seen = await read();
  }
  return seen;
}
