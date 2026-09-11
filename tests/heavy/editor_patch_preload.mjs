// The che-code credential patch, loaded into a built editor.
//
// `patches/che-code/vsxRegistryAuth.ts` is carried, not applied, and
// `patches/che-code/che-code-main-<sha>.diff` is its validated integration
// into `vs/platform/request/node/requestService.ts` — the one place every
// request the *server* makes passes through. A built editor's node request
// service does `await import("https")` and calls `module.request`, so
// wrapping `http.request`/`https.request` at the process boundary reaches the
// same requests, and this file carries the same glue the diff adds there:
//
//   - the credential for a request is the contract file's entry for the
//     request's **own** origin, else `VSX_REGISTRY_AUTH_TOKEN` when the
//     origin is the configured gallery's (`VSX_REGISTRY_URL`, or the
//     `OPENVSX_REGISTRY_URL` che-code's launcher sets);
//   - a credential this file attached is dropped again when the same request
//     is re-issued for a foreign origin — node follows a redirect with the
//     original headers, and that is the leak RFC 0011 §4.2 scopes against.
//
// Loaded with `NODE_OPTIONS=--import=<this file>` by tests/heavy/vscode_patch.sh
// (the stock VS Code server build) and tests/heavy/che_code_patch.sh (the
// che-code build). What is not reproduced at this level is the single 401
// retry, which needs the request body replayed; it is held by the diff and by
// the module's unit tests.
import http from "node:http";
import https from "node:https";
import { syncBuiltinESMExports } from "node:module";

import { mayAttachCredential, originOf, resolveGalleryToken } from "../../patches/che-code/vsxRegistryAuth.ts";

/** The diff's `registryCredentialFor`, verbatim in spirit. */
function registryCredentialFor(url, env) {
  if (!url || !originOf(url)) {
    return undefined;
  }
  const fromFile = resolveGalleryToken(url, { ...env, VSX_REGISTRY_AUTH_TOKEN: undefined });
  if (fromFile) {
    return fromFile;
  }
  const gallery = env.VSX_REGISTRY_URL || env.OPENVSX_REGISTRY_URL;
  if (gallery && mayAttachCredential(url, gallery)) {
    return resolveGalleryToken(gallery, env);
  }
  return undefined;
}

/** Credentials this file attached, so a redirect can be told from a header the editor set itself. */
const attached = new Set();

function urlOf(defaultProtocol, options) {
  const protocol = options.protocol || defaultProtocol;
  const host = options.hostname || options.host || "localhost";
  const port = options.port ? `:${options.port}` : "";
  return `${protocol}//${host}${port}${options.path || "/"}`;
}

function wrap(mod, defaultProtocol) {
  const original = mod.request;
  mod.request = function patchedRequest(input, options, callback) {
    let url;
    let opts;
    let cb;
    const urlFirst = typeof input === "string" || input instanceof URL;
    if (urlFirst) {
      url = input.toString();
      if (typeof options === "function") {
        cb = options;
        opts = {};
      } else {
        opts = options || {};
        cb = callback;
      }
    } else {
      opts = input || {};
      cb = typeof options === "function" ? options : callback;
      url = urlOf(defaultProtocol, opts);
    }
    const headers = { ...(opts.headers || {}) };
    const credential = registryCredentialFor(url, process.env);
    if (credential) {
      headers.Authorization = `Bearer ${credential}`;
      attached.add(credential);
    } else if (typeof headers.Authorization === "string" && attached.has(headers.Authorization.replace(/^Bearer /, ""))) {
      // Re-issued for an origin that holds no credential: a redirect away
      // from the registry. The header was ours; it does not travel.
      delete headers.Authorization;
    }
    opts = { ...opts, headers };
    return urlFirst ? original.call(this, input, opts, cb) : original.call(this, opts, cb);
  };
}

wrap(http, "http:");
wrap(https, "https:");
syncBuiltinESMExports();
process.stderr.write("[batlehub-heavy] gallery credential patch armed (contract file by origin; VSX_REGISTRY_URL / OPENVSX_REGISTRY_URL for the variable)\n");
