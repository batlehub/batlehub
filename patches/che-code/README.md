# The gallery-credential patch (RFC 0011)

VS Code sends no `Authorization` header to its extension gallery, and
`product.json` has nowhere to put a token. A Batlehub registry used as a
gallery therefore has to be readable anonymously, or every query comes back
empty and the editor reports that no extensions were found — which looks like
a broken proxy rather than a configuration choice. That warning is on
[the OpenVSX registry page](../../docs/registries/openvsx.md), and this
directory is the answer for a build you compile yourself.

`vsxRegistryAuth.ts` is the whole of it: a credential in, a header out.

## What it is not

**Not applied to anything here.** This repository does not build an editor.
The module is written against the contract in
[RFC 0011 §4.1](../../docs/rfc/0011-openvsx-login.md), the same contract
`batlehub-cli auth write-token-file` writes and
[`cli/schema/vsx-token.schema.json`](../../cli/schema/vsx-token.schema.json)
defines, and it is carried here so the two halves live in one repository and
change together.

**No fabricated diff.** The one diff here, `che-code-main-e2e91b70.diff`, was
generated on a checkout of that commit and is checked against it by
`validate.sh` — apply, type-check, tests. It names the commit in its file
name because that is the only tree it is known to fit; on a newer tree the
script says what moved, and the steps below say what to do about it.

## Integrating it

**Validated against che-code `main` at `e2e91b70` (VS Code 1.128.1) on
2026-09-11** — `che-code-main-e2e91b70.diff` is that integration, applied to a
pristine checkout with `git apply --check`, both touched TypeScript files
type-checked under che-code's own `tsconfig.base.json` with no error the tree
did not already have, and the module's tests green in place.
`validate.sh` repeats all three; run it after a rebase, or with
`CHE_CODE_REF=main` to learn what moved.

Reading the tree changed three things about the steps first written here,
which named `extensionGalleryService.ts` in `common/`:

1. **The module lives in a `node/` folder, not `common/`.** It reads a file,
   so it imports `node:fs`; VS Code's layering (`import/no-restricted-paths`
   in `eslint.config.js`) forbids that from `**/common/**`, and the gallery
   service in `common/` also runs in the browser, where there is no file to
   read. The place every gallery request made by the *server* passes through
   is `vs/platform/request/node/requestService.ts` — its `request()` — and
   that is where the credential is attached:

   ```ts
   import { mayAttachCredential, originOf, resolveGalleryToken } from './vsxRegistryAuth.js';
   // in request(), after the proxy headers:
   const credential = registryCredentialFor(options.url, env);   // see the diff
   if (credential) { options.headers = { ...options.headers, 'Authorization': `Bearer ${credential}` }; }
   const context = await this.logAndRequest(options, () => nodeRequest(options, token));
   // 401 → re-resolve once, retry once if the credential changed
   ```

   `registryCredentialFor` is a dozen lines in the diff: the contract file's
   entry for the request's **own** origin first — so only an origin the user
   wrote a credential for ever receives one, and no gallery URL has to be
   plumbed in — then `VSX_REGISTRY_AUTH_TOKEN`, only when the origin is the
   configured gallery's (`VSX_REGISTRY_URL`, or the `OPENVSX_REGISTRY_URL`
   che-code's launcher already sets). The 401 retry is there too. What is not
   in the diff is any change to the module: it is the file beside this README,
   byte for byte.

2. **Redirects are re-scoped.** `nodeRequestAttempt` follows a `Location`
   with the original headers; the diff drops `Authorization` when the
   redirect leaves the origin (`mayAttachCredential` per hop). Without it the
   header would follow a CDN redirect off the registry — the leak §4.2 scopes
   against.

3. **The web workbench's own requests go through the server.** In a web
   build the Extensions view queries the gallery from the *browser*
   (`vs/workbench/services/request/browser/requestService.ts`), and only hands
   a request to the remote when the fetch throws or answers `405`. A gallery
   that requires a credential answers the browser's bare fetch with a clean
   `403`, so the view would show an empty gallery while installs — made by
   the server — succeed. The diff routes requests whose origin is the
   configured gallery's through the remote connection first, where the
   credential is. This is the hunk that makes the *view* work on a web build;
   a desktop build does not need it.

4. **`vsxRegistryAuthSupport: true`** is set by the launcher
   (`launcher/src/openvsix-registry.ts`, which already rewrites
   `extensionsGallery` at start-up) through a `ProductJSON` setter, rather
   than at build time.

5. Keep it as a rebase-friendly commit series: one file added, two edited in
   `code/`, two in `launcher/`. The diff is against a subtree of upstream
   `microsoft/vscode`, which is how che-code carries its own changes
   (`git subtree pull --prefix code`).

## What it does, exactly

| | |
| --- | --- |
| Resolution order | `VSX_REGISTRY_AUTH_TOKEN_FILE` → `$BATLEHUB_HOME/state/vsx-token.json` → `VSX_REGISTRY_AUTH_TOKEN`. First source yielding a credential wins. |
| In the file | A literal `token` string, for the entry whose key is the gallery's origin. Nothing else. |
| Header scope | Origin equality with the configured gallery. A redirect to any other origin drops the header. |
| On `401` | Re-resolve once and retry once — and only if the credential actually changed. |
| On anything unexpected | No credential. It never throws, and it says why once. |

The three things it will not do are the reason it is short: it never reads the
contract's `refresh` block, never resolves a token *source*, and never sends a
request of its own. Each would mean an IDP client id or a second file open in
the editor's own process, and each belongs to a broker — the CLI, or the local
gallery proxy a follow-up RFC describes.

## Testing it

```sh
task test:patch          # or: node --test patches/che-code/vsxRegistryAuth.test.ts
```

Node strips the types itself, so there is no toolchain, no bundler and no
dependency — which is what makes it reasonable to carry tests for code this
repository does not compile. The suite asserts the five properties
[RFC 0011 §10](../../docs/rfc/0011-openvsx-login.md) names for the patch:
resolution order, origin scoping including the redirect drop, the single 401
retry, an unparseable file as no credential, and a token *source* object
falling through to the environment variable.

Run it after a rebase. It is the one thing that can tell you in a second
whether the module still does what the contract says.

## Checking it without an editor

The contract file it reads is the one the CLI writes, so the two can be
checked against each other from a shell:

```sh
batlehub-cli --server https://hub.example.dev auth write-token-file
batlehub-cli auth status          # what a consumer will resolve, and whether it does
cat "${BATLEHUB_HOME:-$HOME/.batlehub}/state/vsx-token.json"
```

`auth status` resolves every entry at the moment you ask, which is the
question that matters: a cached "ok" from before a token file rotated is the
failure being debugged.
