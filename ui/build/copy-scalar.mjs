// Copy the Scalar API-reference bundle into the build output, so `/scalar` is
// served from this origin instead of a CDN.
//
// # Why this exists
//
// `utoipa-scalar`'s stock template loads
// `https://cdn.jsdelivr.net/npm/@scalar/api-reference` — unversioned, so it
// executed whatever was newest at request time, on the same origin the console
// keeps its bearer and refresh tokens on. Pinning the URL and adding an SRI hash
// fixed the integrity half. This removes the dependency itself:
//
//   * a private registry is frequently run with no egress at all, and `/scalar`
//     was simply a blank page there;
//   * every load leaked the operator's IP and referrer to a third party;
//   * the bundle's own supply chain (25 direct dependencies, ~190 transitive)
//     was invisible to `pnpm audit`, postmortem and the SBOM. As a declared
//     devDependency it is now covered by all three — that code was always
//     shipped to the browser, it just was not declared.
//
// The bundle still calls `api.scalar.com` on load; those URLs are hardcoded and
// ignore the `apiBaseUrl` setting. `connect-src 'self'` in the server's
// `API_DOCS_CSP` is what stops them, and self-hosting does not change that.
//
// # Which artifact
//
// `dist/browser/standalone.js` — the self-contained IIFE. Not
// `standalone.esm.js`, which is smaller only because it code-splits into
// `chunks/` and would need every chunk copied and a `type="module"` tag.

import { copyFileSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const uiRoot = resolve(here, '..')

const SOURCE = resolve(
  uiRoot,
  'node_modules/@scalar/api-reference/dist/browser/standalone.js',
)

// Under `assets/`, which `crates/web/src/spa.rs` treats as build-owned: a
// missing file there stays a 404 instead of falling through to `index.html`.
// That is what makes the server's degraded `/scalar` page reachable rather than
// the console being served where a `.js` was expected.
const TARGET = resolve(uiRoot, 'dist/assets/scalar/standalone.js')

let source
try {
  source = statSync(SOURCE)
} catch {
  console.error(
    `copy-scalar: ${SOURCE} not found.\n` +
      'Run `pnpm install` first — @scalar/api-reference is a devDependency.',
  )
  process.exit(1)
}

mkdirSync(dirname(TARGET), { recursive: true })
copyFileSync(SOURCE, TARGET)

const kb = Math.round(source.size / 1024)
console.log(`copy-scalar: assets/scalar/standalone.js (${kb} kB)`)

// The BatleHub skin: the console's tokens, with the light rendition re-scoped
// to the class Scalar puts on <body>, then build/scalar-theme.css mapping them
// onto `--scalar-*`. Generated so the reference cannot drift from the console.
const LIGHT = ':root[data-theme="light"]'
const tokens = readFileSync(resolve(uiRoot, 'src/design/tokens.css'), 'utf8')
if (!tokens.includes(LIGHT)) {
  console.error(`copy-scalar: tokens.css no longer declares ${LIGHT}`)
  process.exit(1)
}
writeFileSync(
  resolve(dirname(TARGET), 'batlehub.css'),
  tokens.replaceAll(LIGHT, '.light-mode') +
    readFileSync(resolve(here, 'scalar-theme.css'), 'utf8'),
)
console.log('copy-scalar: assets/scalar/batlehub.css')
