/**
 * Scenario 13 — What regenerating a signed APKINDEX costs as a repository grows
 *
 * Every publish into a local `apk` registry re-renders the whole `APKINDEX` for
 * that architecture and RSA-signs it: O(n) in packages, on the upload path,
 * holding a lock (RFC 0026 §6.4). `v3.22/main/x86_64` is 5 647 packages and
 * 2.26 MB, so a plausible internal repository is not small, and "the re-render
 * is free in the steady state" is a measurement rather than a claim — the same
 * question scenario 12 asked about conda's channel index, one format over.
 *
 * **One arm per run**, and the arm is the *index size the publish lands in*:
 *
 *   n100    publish into an index of ~100 entries
 *   n1000   …of ~1 000
 *   n5000   …of ~5 000, about the size of `v3.22/main/x86_64`
 *   n20000  …of ~20 000, a repository four times the size of Alpine's main
 *
 * The seeding is not part of the measurement: `task perf:apk:seed COUNT=n` fills
 * the registry first, and this scenario publishes *one more* package per
 * iteration and records what that costs. Comparing the four numbers is the
 * result; if publish latency is flat the design ships as written, and if it
 * grows with n the index needs to be incremental.
 *
 * Every package is built in the VU, in the v2 container, because `apk mkpkg`
 * writes v3 (RFC 0026 §13) — and built *once* per VU, with only the name
 * changing per iteration, so k6's own gzip time stays out of the server's
 * number.
 *
 * Pre-requisites: task perf:apk:server, task perf:apk:seed COUNT=<n>
 * Run: BATLEHUB_APK_ARM=n5000 k6 run perf/k6/scenarios/13_apk_index_regeneration.js
 */
import http from "k6/http";
import { check } from "k6";
import { BASE_URL, ADMIN_TOKEN } from "../config.js";

const REGISTRY = __ENV.BATLEHUB_APK_REGISTRY || "perf-apk";
const ARMS = { n100: 100, n1000: 1000, n5000: 5000, n20000: 20000 };
const ARM_NAME = __ENV.BATLEHUB_APK_ARM || "n1000";
const EXPECTED = ARMS[ARM_NAME];
if (!EXPECTED) {
  throw new Error(
    `BATLEHUB_APK_ARM=${ARM_NAME} is not one of ${Object.keys(ARMS).join(", ")}`,
  );
}

export const options = {
  // Low and fixed. The regeneration holds a per-registry lock, so a high VU
  // count would measure queueing on that lock rather than the cost of one
  // regeneration — which is the number this scenario exists to produce.
  vus: Number(__ENV.BATLEHUB_VUS || 2),
  duration: __ENV.BATLEHUB_DURATION || "60s",
  thresholds: {
    http_req_failed: ["rate<0.01"],
  },
};

const INDEX_URL = `${BASE_URL}/proxy/${REGISTRY}/apk/x86_64/APKINDEX.tar.gz`;
const UPLOAD_URL = `${BASE_URL}/proxy/${REGISTRY}/apk/upload`;
const HEADERS = { Authorization: `Bearer ${ADMIN_TOKEN}` };

export function setup() {
  const res = http.get(INDEX_URL, { headers: HEADERS });
  if (res.status !== 200) {
    throw new Error(
      `${INDEX_URL} answered ${res.status} — run 'task perf:apk:seed COUNT=${EXPECTED}' first`,
    );
  }
  const bytes = res.body.length;
  console.log(
    `arm=${ARM_NAME}  index is ${bytes} bytes  (expecting about ${EXPECTED} entries)`,
  );
  // A run seeded to the wrong size answers the wrong question and looks like a
  // result, so the size is checked rather than assumed. ~120 bytes an entry,
  // signed and gzipped; the band is wide because the shape is what matters.
  const perEntry = bytes / EXPECTED;
  if (perEntry < 20 || perEntry > 400) {
    throw new Error(
      `the index is ${bytes} bytes for a nominal ${EXPECTED} entries — seed it for this arm`,
    );
  }
  return { bytes };
}

export default function apk_index_regeneration(data) {
  // One unique coordinate per iteration: publishing the same one twice is a
  // 409 after RFC 0016, and a 409 costs no regeneration at all.
  const name = `perf-pub-${__VU}-${__ITER}`;
  const pkg = buildApkV2(name, "1.0.0-r0");

  const res = http.put(UPLOAD_URL, pkg, {
    headers: { ...HEADERS, "Content-Type": "application/octet-stream" },
  });
  check(res, {
    "status 201": (r) => r.status === 201,
  });

  // Once per VU: the index really did grow, so the number above is the cost of
  // a regeneration and not of a rejected upload.
  if (__ITER === 0) {
    const after = http.get(INDEX_URL, { headers: HEADERS });
    check(after, {
      "the index grew": (r) => r.body.length > data.bytes,
    });
  }
}

// ── The v2 container, built in the VU ────────────────────────────────────────
//
// k6 has no tar or gzip, so the archive is assembled by hand: gzip with no
// compression (stored blocks), which is a legal gzip stream and keeps the CPU
// this costs out of the measurement. The layout follows RFC 0026 §13 — one tar
// stream across two members, only the last terminated, `datahash` present.

function buildApkV2(name, version) {
  const data = tar([[`usr/share/${name}/hello.txt`, str("perf\n")]], true);
  const dataMember = gzipStored(data);
  const hash = sha256hex(dataMember);
  const pkginfo = str(
    `pkgname = ${name}\npkgver = ${version}\narch = x86_64\nsize = 5\n` +
      `builddate = 1700000000\npkgdesc = perf publish\nlicense = MIT\n` +
      `datahash = ${hash}\n`,
  );
  const controlMember = gzipStored(tar([[".PKGINFO", pkginfo]], false));
  return concat([controlMember, dataMember]);
}

function str(s) {
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i) & 0xff;
  return out;
}

function concat(parts) {
  let n = 0;
  for (const p of parts) n += p.length;
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out.buffer;
}

/** A POSIX ustar archive; `terminate` appends the two zero blocks that end a
 *  tar stream — which only the *last* gzip member of a `.apk` may carry. */
function tar(entries, terminate) {
  const blocks = [];
  for (const [path, content] of entries) {
    const h = new Uint8Array(512);
    const put = (off, s) => {
      for (let i = 0; i < s.length; i++) h[off + i] = s.charCodeAt(i);
    };
    const octal = (n, width) => n.toString(8).padStart(width - 1, "0");
    put(0, path);
    put(100, octal(0o644, 8));
    put(108, octal(0, 8));
    put(116, octal(0, 8));
    put(124, octal(content.length, 12));
    put(136, octal(0, 12));
    h[156] = 0x30; // regular file
    put(257, "ustar");
    h[262] = 0;
    put(263, "00");
    // The checksum field is **six** octal digits, then NUL, then a space —
    // 148..154, not 148..156. Writing seven digits and then clobbering the
    // seventh drops a digit: apk tolerates the result, every tar reader
    // refuses it, and the upload comes back `400 no member carries a
    // .PKGINFO control file`. Found by running this scenario once.
    for (let i = 148; i < 156; i++) h[i] = 0x20;
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += h[i];
    put(148, sum.toString(8).padStart(6, "0"));
    h[154] = 0;
    h[155] = 0x20;
    blocks.push(h);
    const padded = new Uint8Array(Math.ceil(content.length / 512) * 512);
    padded.set(content);
    blocks.push(padded);
  }
  if (terminate) blocks.push(new Uint8Array(1024));
  return new Uint8Array(concat(blocks));
}

/** A gzip stream of stored (uncompressed) deflate blocks. */
function gzipStored(bytes) {
  const header = new Uint8Array([0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0xff]);
  const chunks = [header];
  const MAX = 65535;
  for (let off = 0; off < bytes.length || off === 0; off += MAX) {
    const len = Math.min(MAX, bytes.length - off);
    const last = off + len >= bytes.length ? 1 : 0;
    const b = new Uint8Array(5 + len);
    b[0] = last;
    b[1] = len & 0xff;
    b[2] = (len >> 8) & 0xff;
    b[3] = ~len & 0xff;
    b[4] = (~len >> 8) & 0xff;
    b.set(bytes.subarray(off, off + len), 5);
    chunks.push(b);
    if (len === 0) break;
  }
  const trailer = new Uint8Array(8);
  const crc = crc32(bytes);
  const view = new DataView(trailer.buffer);
  view.setUint32(0, crc, true);
  view.setUint32(4, bytes.length >>> 0, true);
  chunks.push(trailer);
  return new Uint8Array(concat(chunks));
}

let CRC_TABLE = null;
function crc32(bytes) {
  if (!CRC_TABLE) {
    CRC_TABLE = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      CRC_TABLE[n] = c;
    }
  }
  let c = -1;
  for (let i = 0; i < bytes.length; i++) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

// k6 ships crypto as a module; the digest is over the *compressed* data member,
// which is what `datahash` covers (verified against busybox-1.37.0-r20.apk).
import crypto from "k6/crypto";
function sha256hex(bytes) {
  return crypto.sha256(bytes.buffer ? bytes.buffer : bytes, "hex");
}
