import { check } from "k6";
import encoding from "k6/encoding";
import http from "k6/http";
import { ADMIN_TOKEN, BASE_URL } from "./config.js";

export function authHeaders(token) {
  return { Authorization: `Bearer ${token || ADMIN_TOKEN}` };
}

/** GET with auth, assert status, return response. */
export function getOk(url, params) {
  const res = http.get(url, { headers: authHeaders(), ...params });
  check(res, { "status 200": (r) => r.status === 200 });
  return res;
}

/** Generate a minimal npm publish payload for a package of ~sizeKb. */
export function npmPublishPayload(name, version, sizeKb) {
  const tarball = generateBase64(sizeKb * 1024);
  return JSON.stringify({
    name,
    versions: {
      [version]: {
        name,
        version,
        description: "k6 perf test package",
        dist: {
          tarball: `${BASE_URL}/proxy/perf-npm/${name}/-/${name}-${version}.tgz`,
          shasum: "aabbccdd112233445566778899aabbccdd112233", // DevSkim: ignore DS173237
        },
      },
    },
    _attachments: {
      [`${name}-${version}.tgz`]: {
        content_type: "application/octet-stream",
        data: tarball,
      },
    },
  });
}

/**
 * Return a base64 string of ~byteLen random bytes (padded to nearest 3).
 *
 * Native: `getRandomValues` and `b64encode`. The previous version appended one
 * random character at a time in the script engine — 833 ms per 32 KiB with one
 * VU, and 14.8 s per iteration across the 100 VUs of the 2026-10-09 profile run's
 * `npm_publish` arm, which starved k6 to 5 req/s and took the runner's CPU from
 * the server under measurement. 4.6 ms now.
 */
function generateBase64(byteLen) {
  const bytes = new Uint8Array(Math.ceil(byteLen / 3) * 3);
  // Web Crypto fills at most 65 536 bytes per call.
  for (let off = 0; off < bytes.length; off += 65536) {
    crypto.getRandomValues(bytes.subarray(off, off + 65536));
  }
  return encoding.b64encode(bytes.buffer);
}
