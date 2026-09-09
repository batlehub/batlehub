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
change together. Integrating it is the step below, and it is a rebase against
whatever the upstream file looks like on the day.

**No fabricated diff.** A patch with invented context lines fails to apply and
wastes the time of whoever tries. What follows is the change to make, not a
`git apply` target.

## Integrating it

1. Copy `vsxRegistryAuth.ts` into
   `src/vs/platform/extensionManagement/common/`.

2. In `extensionGalleryService.ts`, at each place that builds headers for a
   gallery request — the `extensionquery` POST and the asset/download GETs
   are the ones that matter — pass them through `withGalleryCredential`, or
   wrap the send in `sendWithCredential` to get the 401 retry as well:

   ```ts
   import { sendWithCredential, withGalleryCredential } from './vsxRegistryAuth.js';

   // headers-only:
   const headers = withGalleryCredential(baseHeaders, url, this.api(''));

   // with the single 401 retry, which is what makes a short-lived token work:
   const res = await sendWithCredential(url, this.api(''), baseHeaders,
       h => this.requestService.request({ type: 'GET', url, headers: h }, token));
   ```

   `this.api('')` stands for whatever the build calls the configured gallery
   URL; it is the origin the credential is scoped to.

3. Set `vsxRegistryAuthSupport: true` in the patched build's `product.json`.
   Nothing here reads it — it is how a Batlehub extension, when one exists,
   can tell a patched build from a stock one without probing.

4. Keep it as a rebase-friendly commit series. It touches one file and adds
   one, on purpose: the smaller the surface, the longer it survives an
   upstream refactor, and the likelier it is to be upstreamable.

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
