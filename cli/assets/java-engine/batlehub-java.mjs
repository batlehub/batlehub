#!/usr/bin/env node

// cli.ts
import { statSync as statSync4 } from "node:fs";
import os6 from "node:os";
import path8 from "node:path";
import { parseArgs } from "node:util";
import { pathToFileURL as pathToFileURL2 } from "node:url";

// ../packages/java-rules/src/verbs.ts
var GENERATE_KINDS = [
  "accessors",
  "getters",
  "setters",
  "builder",
  "withers",
  "surroundWith"
];
var ACCESSOR_KIND = {
  accessors: 0,
  getters: 1,
  setters: 2
};
var dryRun = {
  type: "boolean",
  doc: "Return the WorkspaceEdit without applying it. In the live editor an applied edit is unsaved and undoable in one step; nothing is ever saved by the tool.",
  default: { live: false, stdio: true }
};
var paths = {
  type: "array",
  doc: "Files or directories, relative to the workspace root (default: the whole workspace). Directories are searched for *.java."
};
var VERBS = [
  {
    name: "java_status",
    doc: "The JDK and Java language server this workspace runs on, the server mode, and whether the BatleHub JDT bundle is loaded.",
    args: {},
    needsServer: false
  },
  {
    name: "java_inspect",
    doc: "The BatleHub inspections of Java files, one row per finding, at the project's severities (not the developer's personal overrides; a row they change is marked differsFromEditor).",
    args: { paths },
    needsServer: true
  },
  {
    name: "java_fix",
    doc: "Apply every inspection fix in the given files, or only one rule's fixes, as one edit.",
    args: {
      paths,
      rule: {
        type: "string",
        doc: "A rule code such as collections/sizeIsZero (default: every rule with a fix)."
      },
      dryRun
    },
    needsServer: true
  },
  {
    name: "java_generate",
    doc: "Generate code with no prompt: getters and/or setters, a builder or withers for the type at a line, or surround lines with a construct.",
    args: {
      what: {
        type: "string",
        enum: [...GENERATE_KINDS],
        doc: "accessors = getters and setters; builder, withers (RFC 0015) for the type at line; surroundWith wraps line..endLine.",
        required: true
      },
      file: {
        type: "string",
        doc: "The Java file, relative to the workspace root.",
        required: true
      },
      line: {
        type: "integer",
        doc: "1-based line inside the type to generate for, or the first line to surround.",
        required: true
      },
      endLine: {
        type: "integer",
        doc: "surroundWith: the last line to surround, 1-based (default: line)."
      },
      options: {
        type: "object",
        doc: 'builder, withers: fields (names; default all), methodPrefix, lombok; withers: style "copy" or "mutate". surroundWith: construct (if, ifElse, while, for, tryCatch, tryFinally, tryWithResources, synchronized, runnable; default tryCatch), catchType "Exception" for no precise type. placement "file" is refused: a tool never creates a file.'
      },
      getterPrefix: { type: "string", doc: "Getter prefix (default get)." },
      booleanPrefix: {
        type: "string",
        doc: "Getter prefix of a boolean field (default is)."
      },
      fluentSetters: {
        type: "boolean",
        doc: "Setters return this (default false)."
      },
      dryRun
    },
    needsServer: true
  },
  {
    name: "java_rename",
    doc: "Rename a symbol across the workspace, every module and every caller included, through the Java language server.",
    args: {
      symbol: {
        type: "string",
        doc: "Fully.Qualified.Type, Fully.Qualified.Type#member, or path:line:col (1-based).",
        required: true
      },
      newName: { type: "string", doc: "The new name.", required: true },
      dryRun
    },
    needsServer: true
  }
];
var byName = new Map(VERBS.map((v) => [v.name, v]));
function generateCall(a) {
  const at = { line: a.line - 1, character: 0 };
  const params = {
    textDocument: { uri: a.uri },
    range: { start: at, end: at },
    context: { diagnostics: [] }
  };
  const kind = ACCESSOR_KIND[a.what];
  if (kind !== void 0)
    return {
      command: "batlehub.generate.accessors",
      args: [params, JSON.stringify({ ...a.accessorOptions, kind })]
    };
  if (a.options?.placement === "file")
    throw new Error(
      `${a.what}: placement "file" creates a file, which this tool never does \u2014 generate it in the editor`
    );
  const options = JSON.stringify(a.options ?? {});
  if (a.what === "builder" || a.what === "withers")
    return { command: `batlehub.generate.${a.what}`, args: [params, options] };
  if (a.what === "surroundWith") {
    const end = a.endLine ?? a.line;
    if (end < a.line)
      throw new Error(`surroundWith: endLine ${end} is before line ${a.line}`);
    const range = { start: at, end: { line: end, character: 0 } };
    return {
      command: "batlehub.generate.surroundWith",
      args: [a.uri, range, options]
    };
  }
  throw new Error(
    `generate ${a.what || "?"}: expected ${GENERATE_KINDS.join(", ")}`
  );
}
function defaultOf(a, surface) {
  return a.default && typeof a.default === "object" && surface in a.default ? a.default[surface] : a.default;
}
function toolSchemas(surface) {
  return VERBS.map((v) => ({
    name: v.name,
    description: v.doc,
    inputSchema: {
      type: "object",
      properties: Object.fromEntries(
        Object.entries(v.args).map(([k, a]) => {
          const d = defaultOf(a, surface);
          return [
            k,
            {
              type: a.type,
              description: a.doc,
              ...a.type === "array" ? { items: { type: "string" } } : {},
              ...a.enum ? { enum: a.enum } : {},
              ...d === void 0 ? {} : { default: d }
            }
          ];
        })
      ),
      required: Object.entries(v.args).filter(([, a]) => a.required).map(([k]) => k),
      additionalProperties: false
    }
  }));
}
function checkArgs(name, args, surface) {
  const v = byName.get(name);
  if (!v) throw new Error(`unknown tool ${name}`);
  const out = {};
  for (const k of Object.keys(args ?? {}))
    if (!(k in v.args)) throw new Error(`${name}: unknown argument ${k}`);
  for (const [k, a] of Object.entries(v.args)) {
    const x = args?.[k] ?? defaultOf(a, surface);
    if (x === void 0) {
      if (a.required) throw new Error(`${name}: ${k} is required`);
      continue;
    }
    const ok = a.type === "array" ? Array.isArray(x) && x.every((s) => typeof s === "string") : a.type === "integer" ? Number.isInteger(x) : a.type === "object" ? !!x && typeof x === "object" && !Array.isArray(x) : typeof x === a.type;
    if (!ok) throw new Error(`${name}: ${k} must be ${a.type}`);
    if (a.enum && !a.enum.includes(x))
      throw new Error(`${name}: ${k} must be one of ${a.enum.join(", ")}`);
    out[k] = x;
  }
  return out;
}
var ID = "[A-Za-z_$][\\w$]*";
var FQN = new RegExp(`^(${ID}(?:\\.${ID})*)(?:#(${ID}))?$`);
function parseSymbol(s) {
  const pos = /^(.+):(\d+):(\d+)$/.exec(s);
  if (pos) {
    const line = Number(pos[2]);
    const col = Number(pos[3]);
    if (line < 1 || col < 1)
      throw new Error(`symbol ${s}: line and column are 1-based`);
    return { kind: "position", path: pos[1], line, col };
  }
  const m = FQN.exec(s);
  if (!m)
    throw new Error(
      `symbol ${s}: expected Fully.Qualified.Type[#member] or path:line:col`
    );
  return { kind: "type", type: m[1], member: m[2] };
}

// server.ts
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
var PIN = {
  version: "1.56.0",
  sha256: "3c6a821d7612eb879fa40280b6c585342eec82d6b1b3e8dcfe4d0395e0624343"
};
var SERVER_MIN_JAVA = 21;
var cacheDir = () => path.join(
  process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache"),
  "batlehub",
  "jdtls"
);
var vsixUrl = (v) => `https://open-vsx.org/api/redhat/java/${v}/file/redhat.java-${v}.vsix`;
function serverIn(dir) {
  for (const s of [dir, path.join(dir, "extension", "server")])
    if (existsSync(path.join(s, "plugins"))) return s;
  return void 0;
}
function locate() {
  const home = process.env.BATLEHUB_JDTLS_HOME;
  if (home) return { server: serverIn(home), from: home };
  const dir = path.join(cacheDir(), PIN.version);
  return { server: serverIn(dir), from: dir };
}
function escapes(entry) {
  return path.isAbsolute(entry) || /^[A-Za-z]:/.test(entry) || entry.split(/[/\\]/).includes("..");
}
function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}
var jarTool = (jdk, args, cwd) => new Promise(
  (resolve3, reject) => execFile(
    path.join(jdk, "bin", "jar"),
    args,
    { cwd, maxBuffer: 64 * 1024 * 1024 },
    (err, stdout) => err ? reject(err) : resolve3(String(stdout))
  )
);
async function ensure(jdk, say) {
  const { server, from } = locate();
  if (server) return server;
  if (process.env.BATLEHUB_JDTLS_HOME)
    throw new Error(
      `BATLEHUB_JDTLS_HOME=${from} holds no JDT.LS (no plugins/)`
    );
  const vsix = `${from}.vsix`;
  mkdirSync(path.dirname(vsix), { recursive: true });
  if (!existsSync(vsix)) {
    say(
      `downloading redhat.java ${PIN.version} (once, ~55 MB) from open-vsx.org`
    );
    const res = await fetch(vsixUrl(PIN.version));
    if (!res.ok)
      throw new Error(
        `download of redhat.java ${PIN.version}: HTTP ${res.status}`
      );
    const part = `${vsix}.part`;
    writeFileSync(part, Buffer.from(await res.arrayBuffer()));
    renameSync(part, vsix);
  }
  const got = sha256(vsix);
  if (got !== PIN.sha256) {
    rmSync(vsix, { force: true });
    throw new Error(
      `redhat.java-${PIN.version}.vsix has sha256 ${got}, not the pinned ${PIN.sha256}: deleted, nothing unpacked`
    );
  }
  const bad = (await jarTool(jdk, ["tf", vsix], os.tmpdir())).split("\n").filter((e) => e && escapes(e));
  if (bad.length)
    throw new Error(
      `the VSIX has entries outside its directory (${bad[0]}): not unpacked`
    );
  const tmp = `${from}.unpacking`;
  rmSync(tmp, { recursive: true, force: true });
  mkdirSync(tmp, { recursive: true });
  await jarTool(jdk, ["xf", vsix], tmp);
  rmSync(from, { recursive: true, force: true });
  renameSync(tmp, from);
  const s = serverIn(from);
  if (!s)
    throw new Error(`redhat.java ${PIN.version} unpacked with no server in it`);
  return s;
}
function bundleJar() {
  const forced = process.env.BATLEHUB_JDT_BUNDLE;
  if (forced) return existsSync(forced) ? forced : void 0;
  const here = path.dirname(fileURLToPath(import.meta.url));
  return [
    path.join(here, "batlehub-jdt-core.jar"),
    path.resolve(
      here,
      "..",
      "extensions",
      "java-core",
      "jdt",
      "batlehub-jdt-core.jar"
    )
  ].find((j) => existsSync(j));
}

// apply.ts
import { realpathSync, readFileSync as readFileSync2, statSync, writeFileSync as writeFileSync2 } from "node:fs";
import path2 from "node:path";
import { fileURLToPath as fileURLToPath2 } from "node:url";
function offsetOf(text2, p) {
  let at = 0;
  for (let l = 0; l < p.line; l++) {
    const nl = text2.indexOf("\n", at);
    if (nl < 0) return text2.length;
    at = nl + 1;
  }
  return Math.min(at + p.character, text2.length);
}
function applyEdits(text2, edits) {
  const spans = edits.map((e) => ({
    s: offsetOf(text2, e.range.start),
    e: offsetOf(text2, e.range.end),
    t: e.newText
  })).sort((a, b) => b.s - a.s || b.e - a.e);
  let out = text2;
  let floor = Infinity;
  for (const x of spans) {
    if (x.e > floor) throw new Error("overlapping edits");
    out = out.slice(0, x.s) + x.t + out.slice(x.e);
    floor = x.s;
  }
  return out;
}
function plan(edit) {
  return Object.entries(edit.changes ?? {}).filter(([, l]) => l.length).map(([uri, l]) => {
    const file = fileURLToPath2(uri);
    const before = readFileSync2(file, "utf8");
    return { file, before, after: applyEdits(before, l), edits: l.length };
  }).filter((p) => p.after !== p.before).sort((a, b) => a.file.localeCompare(b.file));
}
var Refused = class extends Error {
};
function write(planned, root, mtimes) {
  const realRoot = realpathSync(root);
  for (const p of planned) {
    const real = realpathSync(p.file);
    const rel = path2.relative(realRoot, real);
    if (rel.startsWith("..") || path2.isAbsolute(rel))
      throw new Refused(
        `${p.file} resolves to ${real}, outside the workspace: not written`
      );
    const was = mtimes.get(real);
    if (was === void 0 || statSync(p.file).mtimeMs !== was || readFileSync2(p.file, "utf8") !== p.before)
      throw new Refused(
        `${p.file} changed on disk since the server read it: nothing written`
      );
  }
  for (const p of planned) writeFileSync2(p.file, p.after);
}

// config.ts
import { existsSync as existsSync2 } from "node:fs";
import path3 from "node:path";

// ../packages/java-rules/src/gradle-script.ts
var GRADLE_BUILD_FILES = [
  "build.gradle.kts",
  "build.gradle",
  "settings.gradle.kts",
  "settings.gradle"
];
var clean = (s) => s.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|\s)\/\/.*$/gm, "$1");
function requiredJavaOf(text2) {
  const s = clean(text2);
  const tc = /JavaLanguageVersion\.of\(\s*(\d+)\s*\)/.exec(s);
  if (tc) return { min: Number(tc[1]), origin: "toolchain languageVersion" };
  const compat = /(?:sourceCompatibility|targetCompatibility)\s*=?\s*(?:JavaVersion\.VERSION_(\d+)(?:_(\d+))?|['"]?(?:1\.)?(\d+)['"]?)/.exec(
    s
  );
  if (compat) {
    const n = compat[2] ? Number(compat[2]) : Number(compat[1] ?? compat[3]);
    return { min: n, origin: "sourceCompatibility" };
  }
  const release = /options\.release(?:\.set\(|\s*=\s*)(\d+)/.exec(s);
  if (release) return { min: Number(release[1]), origin: "options.release" };
  return void 0;
}

// ../packages/java-rules/src/pom.ts
var tag = (xml, name) => new RegExp(`<${name}(?:\\s[^>]*)?>([^<]*)</${name}>`).exec(xml)?.[1]?.trim();
function section(xml, name) {
  const re = new RegExp(`<${name}(?:\\s[^>]*)?>([\\s\\S]*?)</${name}>`, "g");
  let m;
  while (m = re.exec(xml)) {
    return m[1];
  }
  return void 0;
}
var clean2 = (xml) => xml.replace(/<!--[\s\S]*?-->/g, "");
function parsePom(text2) {
  const xml = clean2(text2);
  const parent = section(xml, "parent");
  const own = xml.replace(
    /<(parent|dependencies|dependencyManagement|build|profiles|properties|reporting|distributionManagement|scm|licenses|developers)(?:\s[^>]*)?>[\s\S]*?<\/\1>/g,
    ""
  );
  const props = section(xml, "properties") ?? "";
  const properties = {};
  for (const m of props.matchAll(/<([\w.-]+)>([^<]*)<\/\1>/g))
    properties[m[1]] = m[2].trim();
  const modules = [
    ...(section(xml, "modules") ?? "").matchAll(/<module>([^<]+)<\/module>/g)
  ].map((m) => m[1].trim());
  const profiles = [
    ...(section(xml, "profiles") ?? "").matchAll(
      /<profile>[\s\S]*?<id>([^<]+)<\/id>/g
    )
  ].map((m) => m[1].trim());
  return {
    groupId: tag(own, "groupId") ?? (parent ? tag(parent, "groupId") : void 0),
    artifactId: tag(own, "artifactId"),
    version: tag(own, "version") ?? (parent ? tag(parent, "version") : void 0),
    packaging: tag(own, "packaging") ?? "jar",
    modules,
    profiles,
    properties,
    hasParent: !!parent,
    parentRelativePath: parent ? tag(parent, "relativePath") ?? "../pom.xml" : void 0
  };
}
function requiredJavaOf2(text2, inherited = {}) {
  const xml = clean2(text2);
  const props = { ...inherited, ...parsePom(xml).properties };
  const resolve3 = (v) => {
    if (!v) return void 0;
    const m = /^\$\{([\w.-]+)\}$/.exec(v.trim());
    return m ? props[m[1]] : v.trim();
  };
  const major = (v) => {
    const r = resolve3(v);
    if (!r) return void 0;
    const n = /^(?:1\.)?(\d+)/.exec(r);
    return n ? Number(n[1]) : void 0;
  };
  const candidates = [
    [props["maven.compiler.release"], "maven.compiler.release"],
    [props["maven.compiler.target"], "maven.compiler.target"],
    [props["maven.compiler.source"], "maven.compiler.source"]
  ];
  const plugin = /<artifactId>maven-compiler-plugin<\/artifactId>[\s\S]*?<configuration>([\s\S]*?)<\/configuration>/.exec(
    xml
  )?.[1];
  if (plugin) {
    candidates.push([
      tag(plugin, "release"),
      "maven-compiler-plugin <release>"
    ]);
    candidates.push([tag(plugin, "target"), "maven-compiler-plugin <target>"]);
  }
  for (const [v, origin] of candidates) {
    const n = major(v);
    if (n) return { min: n, origin };
  }
  return void 0;
}

// ../node_modules/.pnpm/jsonc-parser@3.3.1/node_modules/jsonc-parser/lib/esm/impl/scanner.js
function createScanner(text2, ignoreTrivia = false) {
  const len = text2.length;
  let pos = 0, value = "", tokenOffset = 0, token = 16, lineNumber = 0, lineStartOffset = 0, tokenLineStartOffset = 0, prevTokenLineStartOffset = 0, scanError = 0;
  function scanHexDigits(count, exact) {
    let digits = 0;
    let value2 = 0;
    while (digits < count || !exact) {
      let ch = text2.charCodeAt(pos);
      if (ch >= 48 && ch <= 57) {
        value2 = value2 * 16 + ch - 48;
      } else if (ch >= 65 && ch <= 70) {
        value2 = value2 * 16 + ch - 65 + 10;
      } else if (ch >= 97 && ch <= 102) {
        value2 = value2 * 16 + ch - 97 + 10;
      } else {
        break;
      }
      pos++;
      digits++;
    }
    if (digits < count) {
      value2 = -1;
    }
    return value2;
  }
  function setPosition(newPosition) {
    pos = newPosition;
    value = "";
    tokenOffset = 0;
    token = 16;
    scanError = 0;
  }
  function scanNumber() {
    let start2 = pos;
    if (text2.charCodeAt(pos) === 48) {
      pos++;
    } else {
      pos++;
      while (pos < text2.length && isDigit(text2.charCodeAt(pos))) {
        pos++;
      }
    }
    if (pos < text2.length && text2.charCodeAt(pos) === 46) {
      pos++;
      if (pos < text2.length && isDigit(text2.charCodeAt(pos))) {
        pos++;
        while (pos < text2.length && isDigit(text2.charCodeAt(pos))) {
          pos++;
        }
      } else {
        scanError = 3;
        return text2.substring(start2, pos);
      }
    }
    let end = pos;
    if (pos < text2.length && (text2.charCodeAt(pos) === 69 || text2.charCodeAt(pos) === 101)) {
      pos++;
      if (pos < text2.length && text2.charCodeAt(pos) === 43 || text2.charCodeAt(pos) === 45) {
        pos++;
      }
      if (pos < text2.length && isDigit(text2.charCodeAt(pos))) {
        pos++;
        while (pos < text2.length && isDigit(text2.charCodeAt(pos))) {
          pos++;
        }
        end = pos;
      } else {
        scanError = 3;
      }
    }
    return text2.substring(start2, end);
  }
  function scanString() {
    let result = "", start2 = pos;
    while (true) {
      if (pos >= len) {
        result += text2.substring(start2, pos);
        scanError = 2;
        break;
      }
      const ch = text2.charCodeAt(pos);
      if (ch === 34) {
        result += text2.substring(start2, pos);
        pos++;
        break;
      }
      if (ch === 92) {
        result += text2.substring(start2, pos);
        pos++;
        if (pos >= len) {
          scanError = 2;
          break;
        }
        const ch2 = text2.charCodeAt(pos++);
        switch (ch2) {
          case 34:
            result += '"';
            break;
          case 92:
            result += "\\";
            break;
          case 47:
            result += "/";
            break;
          case 98:
            result += "\b";
            break;
          case 102:
            result += "\f";
            break;
          case 110:
            result += "\n";
            break;
          case 114:
            result += "\r";
            break;
          case 116:
            result += "	";
            break;
          case 117:
            const ch3 = scanHexDigits(4, true);
            if (ch3 >= 0) {
              result += String.fromCharCode(ch3);
            } else {
              scanError = 4;
            }
            break;
          default:
            scanError = 5;
        }
        start2 = pos;
        continue;
      }
      if (ch >= 0 && ch <= 31) {
        if (isLineBreak(ch)) {
          result += text2.substring(start2, pos);
          scanError = 2;
          break;
        } else {
          scanError = 6;
        }
      }
      pos++;
    }
    return result;
  }
  function scanNext() {
    value = "";
    scanError = 0;
    tokenOffset = pos;
    lineStartOffset = lineNumber;
    prevTokenLineStartOffset = tokenLineStartOffset;
    if (pos >= len) {
      tokenOffset = len;
      return token = 17;
    }
    let code = text2.charCodeAt(pos);
    if (isWhiteSpace(code)) {
      do {
        pos++;
        value += String.fromCharCode(code);
        code = text2.charCodeAt(pos);
      } while (isWhiteSpace(code));
      return token = 15;
    }
    if (isLineBreak(code)) {
      pos++;
      value += String.fromCharCode(code);
      if (code === 13 && text2.charCodeAt(pos) === 10) {
        pos++;
        value += "\n";
      }
      lineNumber++;
      tokenLineStartOffset = pos;
      return token = 14;
    }
    switch (code) {
      // tokens: []{}:,
      case 123:
        pos++;
        return token = 1;
      case 125:
        pos++;
        return token = 2;
      case 91:
        pos++;
        return token = 3;
      case 93:
        pos++;
        return token = 4;
      case 58:
        pos++;
        return token = 6;
      case 44:
        pos++;
        return token = 5;
      // strings
      case 34:
        pos++;
        value = scanString();
        return token = 10;
      // comments
      case 47:
        const start2 = pos - 1;
        if (text2.charCodeAt(pos + 1) === 47) {
          pos += 2;
          while (pos < len) {
            if (isLineBreak(text2.charCodeAt(pos))) {
              break;
            }
            pos++;
          }
          value = text2.substring(start2, pos);
          return token = 12;
        }
        if (text2.charCodeAt(pos + 1) === 42) {
          pos += 2;
          const safeLength = len - 1;
          let commentClosed = false;
          while (pos < safeLength) {
            const ch = text2.charCodeAt(pos);
            if (ch === 42 && text2.charCodeAt(pos + 1) === 47) {
              pos += 2;
              commentClosed = true;
              break;
            }
            pos++;
            if (isLineBreak(ch)) {
              if (ch === 13 && text2.charCodeAt(pos) === 10) {
                pos++;
              }
              lineNumber++;
              tokenLineStartOffset = pos;
            }
          }
          if (!commentClosed) {
            pos++;
            scanError = 1;
          }
          value = text2.substring(start2, pos);
          return token = 13;
        }
        value += String.fromCharCode(code);
        pos++;
        return token = 16;
      // numbers
      case 45:
        value += String.fromCharCode(code);
        pos++;
        if (pos === len || !isDigit(text2.charCodeAt(pos))) {
          return token = 16;
        }
      // found a minus, followed by a number so
      // we fall through to proceed with scanning
      // numbers
      case 48:
      case 49:
      case 50:
      case 51:
      case 52:
      case 53:
      case 54:
      case 55:
      case 56:
      case 57:
        value += scanNumber();
        return token = 11;
      // literals and unknown symbols
      default:
        while (pos < len && isUnknownContentCharacter(code)) {
          pos++;
          code = text2.charCodeAt(pos);
        }
        if (tokenOffset !== pos) {
          value = text2.substring(tokenOffset, pos);
          switch (value) {
            case "true":
              return token = 8;
            case "false":
              return token = 9;
            case "null":
              return token = 7;
          }
          return token = 16;
        }
        value += String.fromCharCode(code);
        pos++;
        return token = 16;
    }
  }
  function isUnknownContentCharacter(code) {
    if (isWhiteSpace(code) || isLineBreak(code)) {
      return false;
    }
    switch (code) {
      case 125:
      case 93:
      case 123:
      case 91:
      case 34:
      case 58:
      case 44:
      case 47:
        return false;
    }
    return true;
  }
  function scanNextNonTrivia() {
    let result;
    do {
      result = scanNext();
    } while (result >= 12 && result <= 15);
    return result;
  }
  return {
    setPosition,
    getPosition: () => pos,
    scan: ignoreTrivia ? scanNextNonTrivia : scanNext,
    getToken: () => token,
    getTokenValue: () => value,
    getTokenOffset: () => tokenOffset,
    getTokenLength: () => pos - tokenOffset,
    getTokenStartLine: () => lineStartOffset,
    getTokenStartCharacter: () => tokenOffset - prevTokenLineStartOffset,
    getTokenError: () => scanError
  };
}
function isWhiteSpace(ch) {
  return ch === 32 || ch === 9;
}
function isLineBreak(ch) {
  return ch === 10 || ch === 13;
}
function isDigit(ch) {
  return ch >= 48 && ch <= 57;
}
var CharacterCodes;
(function(CharacterCodes2) {
  CharacterCodes2[CharacterCodes2["lineFeed"] = 10] = "lineFeed";
  CharacterCodes2[CharacterCodes2["carriageReturn"] = 13] = "carriageReturn";
  CharacterCodes2[CharacterCodes2["space"] = 32] = "space";
  CharacterCodes2[CharacterCodes2["_0"] = 48] = "_0";
  CharacterCodes2[CharacterCodes2["_1"] = 49] = "_1";
  CharacterCodes2[CharacterCodes2["_2"] = 50] = "_2";
  CharacterCodes2[CharacterCodes2["_3"] = 51] = "_3";
  CharacterCodes2[CharacterCodes2["_4"] = 52] = "_4";
  CharacterCodes2[CharacterCodes2["_5"] = 53] = "_5";
  CharacterCodes2[CharacterCodes2["_6"] = 54] = "_6";
  CharacterCodes2[CharacterCodes2["_7"] = 55] = "_7";
  CharacterCodes2[CharacterCodes2["_8"] = 56] = "_8";
  CharacterCodes2[CharacterCodes2["_9"] = 57] = "_9";
  CharacterCodes2[CharacterCodes2["a"] = 97] = "a";
  CharacterCodes2[CharacterCodes2["b"] = 98] = "b";
  CharacterCodes2[CharacterCodes2["c"] = 99] = "c";
  CharacterCodes2[CharacterCodes2["d"] = 100] = "d";
  CharacterCodes2[CharacterCodes2["e"] = 101] = "e";
  CharacterCodes2[CharacterCodes2["f"] = 102] = "f";
  CharacterCodes2[CharacterCodes2["g"] = 103] = "g";
  CharacterCodes2[CharacterCodes2["h"] = 104] = "h";
  CharacterCodes2[CharacterCodes2["i"] = 105] = "i";
  CharacterCodes2[CharacterCodes2["j"] = 106] = "j";
  CharacterCodes2[CharacterCodes2["k"] = 107] = "k";
  CharacterCodes2[CharacterCodes2["l"] = 108] = "l";
  CharacterCodes2[CharacterCodes2["m"] = 109] = "m";
  CharacterCodes2[CharacterCodes2["n"] = 110] = "n";
  CharacterCodes2[CharacterCodes2["o"] = 111] = "o";
  CharacterCodes2[CharacterCodes2["p"] = 112] = "p";
  CharacterCodes2[CharacterCodes2["q"] = 113] = "q";
  CharacterCodes2[CharacterCodes2["r"] = 114] = "r";
  CharacterCodes2[CharacterCodes2["s"] = 115] = "s";
  CharacterCodes2[CharacterCodes2["t"] = 116] = "t";
  CharacterCodes2[CharacterCodes2["u"] = 117] = "u";
  CharacterCodes2[CharacterCodes2["v"] = 118] = "v";
  CharacterCodes2[CharacterCodes2["w"] = 119] = "w";
  CharacterCodes2[CharacterCodes2["x"] = 120] = "x";
  CharacterCodes2[CharacterCodes2["y"] = 121] = "y";
  CharacterCodes2[CharacterCodes2["z"] = 122] = "z";
  CharacterCodes2[CharacterCodes2["A"] = 65] = "A";
  CharacterCodes2[CharacterCodes2["B"] = 66] = "B";
  CharacterCodes2[CharacterCodes2["C"] = 67] = "C";
  CharacterCodes2[CharacterCodes2["D"] = 68] = "D";
  CharacterCodes2[CharacterCodes2["E"] = 69] = "E";
  CharacterCodes2[CharacterCodes2["F"] = 70] = "F";
  CharacterCodes2[CharacterCodes2["G"] = 71] = "G";
  CharacterCodes2[CharacterCodes2["H"] = 72] = "H";
  CharacterCodes2[CharacterCodes2["I"] = 73] = "I";
  CharacterCodes2[CharacterCodes2["J"] = 74] = "J";
  CharacterCodes2[CharacterCodes2["K"] = 75] = "K";
  CharacterCodes2[CharacterCodes2["L"] = 76] = "L";
  CharacterCodes2[CharacterCodes2["M"] = 77] = "M";
  CharacterCodes2[CharacterCodes2["N"] = 78] = "N";
  CharacterCodes2[CharacterCodes2["O"] = 79] = "O";
  CharacterCodes2[CharacterCodes2["P"] = 80] = "P";
  CharacterCodes2[CharacterCodes2["Q"] = 81] = "Q";
  CharacterCodes2[CharacterCodes2["R"] = 82] = "R";
  CharacterCodes2[CharacterCodes2["S"] = 83] = "S";
  CharacterCodes2[CharacterCodes2["T"] = 84] = "T";
  CharacterCodes2[CharacterCodes2["U"] = 85] = "U";
  CharacterCodes2[CharacterCodes2["V"] = 86] = "V";
  CharacterCodes2[CharacterCodes2["W"] = 87] = "W";
  CharacterCodes2[CharacterCodes2["X"] = 88] = "X";
  CharacterCodes2[CharacterCodes2["Y"] = 89] = "Y";
  CharacterCodes2[CharacterCodes2["Z"] = 90] = "Z";
  CharacterCodes2[CharacterCodes2["asterisk"] = 42] = "asterisk";
  CharacterCodes2[CharacterCodes2["backslash"] = 92] = "backslash";
  CharacterCodes2[CharacterCodes2["closeBrace"] = 125] = "closeBrace";
  CharacterCodes2[CharacterCodes2["closeBracket"] = 93] = "closeBracket";
  CharacterCodes2[CharacterCodes2["colon"] = 58] = "colon";
  CharacterCodes2[CharacterCodes2["comma"] = 44] = "comma";
  CharacterCodes2[CharacterCodes2["dot"] = 46] = "dot";
  CharacterCodes2[CharacterCodes2["doubleQuote"] = 34] = "doubleQuote";
  CharacterCodes2[CharacterCodes2["minus"] = 45] = "minus";
  CharacterCodes2[CharacterCodes2["openBrace"] = 123] = "openBrace";
  CharacterCodes2[CharacterCodes2["openBracket"] = 91] = "openBracket";
  CharacterCodes2[CharacterCodes2["plus"] = 43] = "plus";
  CharacterCodes2[CharacterCodes2["slash"] = 47] = "slash";
  CharacterCodes2[CharacterCodes2["formFeed"] = 12] = "formFeed";
  CharacterCodes2[CharacterCodes2["tab"] = 9] = "tab";
})(CharacterCodes || (CharacterCodes = {}));

// ../node_modules/.pnpm/jsonc-parser@3.3.1/node_modules/jsonc-parser/lib/esm/impl/string-intern.js
var cachedSpaces = new Array(20).fill(0).map((_, index) => {
  return " ".repeat(index);
});
var maxCachedValues = 200;
var cachedBreakLinesWithSpaces = {
  " ": {
    "\n": new Array(maxCachedValues).fill(0).map((_, index) => {
      return "\n" + " ".repeat(index);
    }),
    "\r": new Array(maxCachedValues).fill(0).map((_, index) => {
      return "\r" + " ".repeat(index);
    }),
    "\r\n": new Array(maxCachedValues).fill(0).map((_, index) => {
      return "\r\n" + " ".repeat(index);
    })
  },
  "	": {
    "\n": new Array(maxCachedValues).fill(0).map((_, index) => {
      return "\n" + "	".repeat(index);
    }),
    "\r": new Array(maxCachedValues).fill(0).map((_, index) => {
      return "\r" + "	".repeat(index);
    }),
    "\r\n": new Array(maxCachedValues).fill(0).map((_, index) => {
      return "\r\n" + "	".repeat(index);
    })
  }
};

// ../node_modules/.pnpm/jsonc-parser@3.3.1/node_modules/jsonc-parser/lib/esm/impl/parser.js
var ParseOptions;
(function(ParseOptions2) {
  ParseOptions2.DEFAULT = {
    allowTrailingComma: false
  };
})(ParseOptions || (ParseOptions = {}));
function parseTree(text2, errors = [], options = ParseOptions.DEFAULT) {
  let currentParent = { type: "array", offset: -1, length: -1, children: [], parent: void 0 };
  function ensurePropertyComplete(endOffset) {
    if (currentParent.type === "property") {
      currentParent.length = endOffset - currentParent.offset;
      currentParent = currentParent.parent;
    }
  }
  function onValue(valueNode) {
    currentParent.children.push(valueNode);
    return valueNode;
  }
  const visitor = {
    onObjectBegin: (offset) => {
      currentParent = onValue({ type: "object", offset, length: -1, parent: currentParent, children: [] });
    },
    onObjectProperty: (name, offset, length) => {
      currentParent = onValue({ type: "property", offset, length: -1, parent: currentParent, children: [] });
      currentParent.children.push({ type: "string", value: name, offset, length, parent: currentParent });
    },
    onObjectEnd: (offset, length) => {
      ensurePropertyComplete(offset + length);
      currentParent.length = offset + length - currentParent.offset;
      currentParent = currentParent.parent;
      ensurePropertyComplete(offset + length);
    },
    onArrayBegin: (offset, length) => {
      currentParent = onValue({ type: "array", offset, length: -1, parent: currentParent, children: [] });
    },
    onArrayEnd: (offset, length) => {
      currentParent.length = offset + length - currentParent.offset;
      currentParent = currentParent.parent;
      ensurePropertyComplete(offset + length);
    },
    onLiteralValue: (value, offset, length) => {
      onValue({ type: getNodeType(value), offset, length, parent: currentParent, value });
      ensurePropertyComplete(offset + length);
    },
    onSeparator: (sep, offset, length) => {
      if (currentParent.type === "property") {
        if (sep === ":") {
          currentParent.colonOffset = offset;
        } else if (sep === ",") {
          ensurePropertyComplete(offset);
        }
      }
    },
    onError: (error, offset, length) => {
      errors.push({ error, offset, length });
    }
  };
  visit(text2, visitor, options);
  const result = currentParent.children[0];
  if (result) {
    delete result.parent;
  }
  return result;
}
function findNodeAtLocation(root, path9) {
  if (!root) {
    return void 0;
  }
  let node = root;
  for (let segment of path9) {
    if (typeof segment === "string") {
      if (node.type !== "object" || !Array.isArray(node.children)) {
        return void 0;
      }
      let found = false;
      for (const propertyNode of node.children) {
        if (Array.isArray(propertyNode.children) && propertyNode.children[0].value === segment && propertyNode.children.length === 2) {
          node = propertyNode.children[1];
          found = true;
          break;
        }
      }
      if (!found) {
        return void 0;
      }
    } else {
      const index = segment;
      if (node.type !== "array" || index < 0 || !Array.isArray(node.children) || index >= node.children.length) {
        return void 0;
      }
      node = node.children[index];
    }
  }
  return node;
}
function getNodeValue(node) {
  switch (node.type) {
    case "array":
      return node.children.map(getNodeValue);
    case "object":
      const obj = /* @__PURE__ */ Object.create(null);
      for (let prop of node.children) {
        const valueNode = prop.children[1];
        if (valueNode) {
          obj[prop.children[0].value] = getNodeValue(valueNode);
        }
      }
      return obj;
    case "null":
    case "string":
    case "number":
    case "boolean":
      return node.value;
    default:
      return void 0;
  }
}
function visit(text2, visitor, options = ParseOptions.DEFAULT) {
  const _scanner = createScanner(text2, false);
  const _jsonPath = [];
  let suppressedCallbacks = 0;
  function toNoArgVisit(visitFunction) {
    return visitFunction ? () => suppressedCallbacks === 0 && visitFunction(_scanner.getTokenOffset(), _scanner.getTokenLength(), _scanner.getTokenStartLine(), _scanner.getTokenStartCharacter()) : () => true;
  }
  function toOneArgVisit(visitFunction) {
    return visitFunction ? (arg) => suppressedCallbacks === 0 && visitFunction(arg, _scanner.getTokenOffset(), _scanner.getTokenLength(), _scanner.getTokenStartLine(), _scanner.getTokenStartCharacter()) : () => true;
  }
  function toOneArgVisitWithPath(visitFunction) {
    return visitFunction ? (arg) => suppressedCallbacks === 0 && visitFunction(arg, _scanner.getTokenOffset(), _scanner.getTokenLength(), _scanner.getTokenStartLine(), _scanner.getTokenStartCharacter(), () => _jsonPath.slice()) : () => true;
  }
  function toBeginVisit(visitFunction) {
    return visitFunction ? () => {
      if (suppressedCallbacks > 0) {
        suppressedCallbacks++;
      } else {
        let cbReturn = visitFunction(_scanner.getTokenOffset(), _scanner.getTokenLength(), _scanner.getTokenStartLine(), _scanner.getTokenStartCharacter(), () => _jsonPath.slice());
        if (cbReturn === false) {
          suppressedCallbacks = 1;
        }
      }
    } : () => true;
  }
  function toEndVisit(visitFunction) {
    return visitFunction ? () => {
      if (suppressedCallbacks > 0) {
        suppressedCallbacks--;
      }
      if (suppressedCallbacks === 0) {
        visitFunction(_scanner.getTokenOffset(), _scanner.getTokenLength(), _scanner.getTokenStartLine(), _scanner.getTokenStartCharacter());
      }
    } : () => true;
  }
  const onObjectBegin = toBeginVisit(visitor.onObjectBegin), onObjectProperty = toOneArgVisitWithPath(visitor.onObjectProperty), onObjectEnd = toEndVisit(visitor.onObjectEnd), onArrayBegin = toBeginVisit(visitor.onArrayBegin), onArrayEnd = toEndVisit(visitor.onArrayEnd), onLiteralValue = toOneArgVisitWithPath(visitor.onLiteralValue), onSeparator = toOneArgVisit(visitor.onSeparator), onComment = toNoArgVisit(visitor.onComment), onError = toOneArgVisit(visitor.onError);
  const disallowComments = options && options.disallowComments;
  const allowTrailingComma = options && options.allowTrailingComma;
  function scanNext() {
    while (true) {
      const token = _scanner.scan();
      switch (_scanner.getTokenError()) {
        case 4:
          handleError(
            14
            /* ParseErrorCode.InvalidUnicode */
          );
          break;
        case 5:
          handleError(
            15
            /* ParseErrorCode.InvalidEscapeCharacter */
          );
          break;
        case 3:
          handleError(
            13
            /* ParseErrorCode.UnexpectedEndOfNumber */
          );
          break;
        case 1:
          if (!disallowComments) {
            handleError(
              11
              /* ParseErrorCode.UnexpectedEndOfComment */
            );
          }
          break;
        case 2:
          handleError(
            12
            /* ParseErrorCode.UnexpectedEndOfString */
          );
          break;
        case 6:
          handleError(
            16
            /* ParseErrorCode.InvalidCharacter */
          );
          break;
      }
      switch (token) {
        case 12:
        case 13:
          if (disallowComments) {
            handleError(
              10
              /* ParseErrorCode.InvalidCommentToken */
            );
          } else {
            onComment();
          }
          break;
        case 16:
          handleError(
            1
            /* ParseErrorCode.InvalidSymbol */
          );
          break;
        case 15:
        case 14:
          break;
        default:
          return token;
      }
    }
  }
  function handleError(error, skipUntilAfter = [], skipUntil = []) {
    onError(error);
    if (skipUntilAfter.length + skipUntil.length > 0) {
      let token = _scanner.getToken();
      while (token !== 17) {
        if (skipUntilAfter.indexOf(token) !== -1) {
          scanNext();
          break;
        } else if (skipUntil.indexOf(token) !== -1) {
          break;
        }
        token = scanNext();
      }
    }
  }
  function parseString(isValue) {
    const value = _scanner.getTokenValue();
    if (isValue) {
      onLiteralValue(value);
    } else {
      onObjectProperty(value);
      _jsonPath.push(value);
    }
    scanNext();
    return true;
  }
  function parseLiteral() {
    switch (_scanner.getToken()) {
      case 11:
        const tokenValue = _scanner.getTokenValue();
        let value = Number(tokenValue);
        if (isNaN(value)) {
          handleError(
            2
            /* ParseErrorCode.InvalidNumberFormat */
          );
          value = 0;
        }
        onLiteralValue(value);
        break;
      case 7:
        onLiteralValue(null);
        break;
      case 8:
        onLiteralValue(true);
        break;
      case 9:
        onLiteralValue(false);
        break;
      default:
        return false;
    }
    scanNext();
    return true;
  }
  function parseProperty() {
    if (_scanner.getToken() !== 10) {
      handleError(3, [], [
        2,
        5
        /* SyntaxKind.CommaToken */
      ]);
      return false;
    }
    parseString(false);
    if (_scanner.getToken() === 6) {
      onSeparator(":");
      scanNext();
      if (!parseValue()) {
        handleError(4, [], [
          2,
          5
          /* SyntaxKind.CommaToken */
        ]);
      }
    } else {
      handleError(5, [], [
        2,
        5
        /* SyntaxKind.CommaToken */
      ]);
    }
    _jsonPath.pop();
    return true;
  }
  function parseObject() {
    onObjectBegin();
    scanNext();
    let needsComma = false;
    while (_scanner.getToken() !== 2 && _scanner.getToken() !== 17) {
      if (_scanner.getToken() === 5) {
        if (!needsComma) {
          handleError(4, [], []);
        }
        onSeparator(",");
        scanNext();
        if (_scanner.getToken() === 2 && allowTrailingComma) {
          break;
        }
      } else if (needsComma) {
        handleError(6, [], []);
      }
      if (!parseProperty()) {
        handleError(4, [], [
          2,
          5
          /* SyntaxKind.CommaToken */
        ]);
      }
      needsComma = true;
    }
    onObjectEnd();
    if (_scanner.getToken() !== 2) {
      handleError(7, [
        2
        /* SyntaxKind.CloseBraceToken */
      ], []);
    } else {
      scanNext();
    }
    return true;
  }
  function parseArray() {
    onArrayBegin();
    scanNext();
    let isFirstElement = true;
    let needsComma = false;
    while (_scanner.getToken() !== 4 && _scanner.getToken() !== 17) {
      if (_scanner.getToken() === 5) {
        if (!needsComma) {
          handleError(4, [], []);
        }
        onSeparator(",");
        scanNext();
        if (_scanner.getToken() === 4 && allowTrailingComma) {
          break;
        }
      } else if (needsComma) {
        handleError(6, [], []);
      }
      if (isFirstElement) {
        _jsonPath.push(0);
        isFirstElement = false;
      } else {
        _jsonPath[_jsonPath.length - 1]++;
      }
      if (!parseValue()) {
        handleError(4, [], [
          4,
          5
          /* SyntaxKind.CommaToken */
        ]);
      }
      needsComma = true;
    }
    onArrayEnd();
    if (!isFirstElement) {
      _jsonPath.pop();
    }
    if (_scanner.getToken() !== 4) {
      handleError(8, [
        4
        /* SyntaxKind.CloseBracketToken */
      ], []);
    } else {
      scanNext();
    }
    return true;
  }
  function parseValue() {
    switch (_scanner.getToken()) {
      case 3:
        return parseArray();
      case 1:
        return parseObject();
      case 10:
        return parseString(true);
      default:
        return parseLiteral();
    }
  }
  scanNext();
  if (_scanner.getToken() === 17) {
    if (options.allowEmptyContent) {
      return true;
    }
    handleError(4, [], []);
    return false;
  }
  if (!parseValue()) {
    handleError(4, [], []);
    return false;
  }
  if (_scanner.getToken() !== 17) {
    handleError(9, [], []);
  }
  return true;
}
function getNodeType(value) {
  switch (typeof value) {
    case "boolean":
      return "boolean";
    case "number":
      return "number";
    case "string":
      return "string";
    case "object": {
      if (!value) {
        return "null";
      } else if (Array.isArray(value)) {
        return "array";
      }
      return "object";
    }
    default:
      return "null";
  }
}

// ../node_modules/.pnpm/jsonc-parser@3.3.1/node_modules/jsonc-parser/lib/esm/main.js
var ScanError;
(function(ScanError2) {
  ScanError2[ScanError2["None"] = 0] = "None";
  ScanError2[ScanError2["UnexpectedEndOfComment"] = 1] = "UnexpectedEndOfComment";
  ScanError2[ScanError2["UnexpectedEndOfString"] = 2] = "UnexpectedEndOfString";
  ScanError2[ScanError2["UnexpectedEndOfNumber"] = 3] = "UnexpectedEndOfNumber";
  ScanError2[ScanError2["InvalidUnicode"] = 4] = "InvalidUnicode";
  ScanError2[ScanError2["InvalidEscapeCharacter"] = 5] = "InvalidEscapeCharacter";
  ScanError2[ScanError2["InvalidCharacter"] = 6] = "InvalidCharacter";
})(ScanError || (ScanError = {}));
var SyntaxKind;
(function(SyntaxKind2) {
  SyntaxKind2[SyntaxKind2["OpenBraceToken"] = 1] = "OpenBraceToken";
  SyntaxKind2[SyntaxKind2["CloseBraceToken"] = 2] = "CloseBraceToken";
  SyntaxKind2[SyntaxKind2["OpenBracketToken"] = 3] = "OpenBracketToken";
  SyntaxKind2[SyntaxKind2["CloseBracketToken"] = 4] = "CloseBracketToken";
  SyntaxKind2[SyntaxKind2["CommaToken"] = 5] = "CommaToken";
  SyntaxKind2[SyntaxKind2["ColonToken"] = 6] = "ColonToken";
  SyntaxKind2[SyntaxKind2["NullKeyword"] = 7] = "NullKeyword";
  SyntaxKind2[SyntaxKind2["TrueKeyword"] = 8] = "TrueKeyword";
  SyntaxKind2[SyntaxKind2["FalseKeyword"] = 9] = "FalseKeyword";
  SyntaxKind2[SyntaxKind2["StringLiteral"] = 10] = "StringLiteral";
  SyntaxKind2[SyntaxKind2["NumericLiteral"] = 11] = "NumericLiteral";
  SyntaxKind2[SyntaxKind2["LineCommentTrivia"] = 12] = "LineCommentTrivia";
  SyntaxKind2[SyntaxKind2["BlockCommentTrivia"] = 13] = "BlockCommentTrivia";
  SyntaxKind2[SyntaxKind2["LineBreakTrivia"] = 14] = "LineBreakTrivia";
  SyntaxKind2[SyntaxKind2["Trivia"] = 15] = "Trivia";
  SyntaxKind2[SyntaxKind2["Unknown"] = 16] = "Unknown";
  SyntaxKind2[SyntaxKind2["EOF"] = 17] = "EOF";
})(SyntaxKind || (SyntaxKind = {}));
var parseTree2 = parseTree;
var findNodeAtLocation2 = findNodeAtLocation;
var getNodeValue2 = getNodeValue;
var ParseErrorCode;
(function(ParseErrorCode2) {
  ParseErrorCode2[ParseErrorCode2["InvalidSymbol"] = 1] = "InvalidSymbol";
  ParseErrorCode2[ParseErrorCode2["InvalidNumberFormat"] = 2] = "InvalidNumberFormat";
  ParseErrorCode2[ParseErrorCode2["PropertyNameExpected"] = 3] = "PropertyNameExpected";
  ParseErrorCode2[ParseErrorCode2["ValueExpected"] = 4] = "ValueExpected";
  ParseErrorCode2[ParseErrorCode2["ColonExpected"] = 5] = "ColonExpected";
  ParseErrorCode2[ParseErrorCode2["CommaExpected"] = 6] = "CommaExpected";
  ParseErrorCode2[ParseErrorCode2["CloseBraceExpected"] = 7] = "CloseBraceExpected";
  ParseErrorCode2[ParseErrorCode2["CloseBracketExpected"] = 8] = "CloseBracketExpected";
  ParseErrorCode2[ParseErrorCode2["EndOfFileExpected"] = 9] = "EndOfFileExpected";
  ParseErrorCode2[ParseErrorCode2["InvalidCommentToken"] = 10] = "InvalidCommentToken";
  ParseErrorCode2[ParseErrorCode2["UnexpectedEndOfComment"] = 11] = "UnexpectedEndOfComment";
  ParseErrorCode2[ParseErrorCode2["UnexpectedEndOfString"] = 12] = "UnexpectedEndOfString";
  ParseErrorCode2[ParseErrorCode2["UnexpectedEndOfNumber"] = 13] = "UnexpectedEndOfNumber";
  ParseErrorCode2[ParseErrorCode2["InvalidUnicode"] = 14] = "InvalidUnicode";
  ParseErrorCode2[ParseErrorCode2["InvalidEscapeCharacter"] = 15] = "InvalidEscapeCharacter";
  ParseErrorCode2[ParseErrorCode2["InvalidCharacter"] = 16] = "InvalidCharacter";
})(ParseErrorCode || (ParseErrorCode = {}));

// ../packages/java-rules/src/profile.ts
var LEVELS = ["error", "warning", "info", "hint", "off"];
var RANK = {
  off: 0,
  hint: 1,
  info: 2,
  warning: 3,
  error: 4
};
var BRIDGES = ["sonar", "spelling"];
var IMPORTERS = ["intellij"];
var PROFILE = ".batlehub/java/inspections.json";
var BRIDGED_DEFAULTS = {
  "spelling/unknownWord": "info"
};
var lineOf = (text2, offset) => text2.slice(0, offset).split("\n").length - 1;
function parseProfile(text2) {
  const errors = [];
  const tree = parseTree2(text2, errors, { allowTrailingComma: true });
  if (errors.length || !tree || tree.type !== "object")
    return {
      problems: [
        {
          line: errors.length ? lineOf(text2, errors[0].offset) : 0,
          severity: "error",
          message: "inspections.json does not parse: the profile is ignored until it does"
        }
      ],
      lines: {}
    };
  const version = findNodeAtLocation2(tree, ["version"]);
  if (!version || getNodeValue2(version) !== 1)
    return {
      problems: [
        {
          line: version ? lineOf(text2, version.offset) : 0,
          severity: "error",
          message: `inspections.json: "version" must be 1 (this java-core reads version 1): the profile is ignored`
        }
      ],
      lines: {}
    };
  const rulesNode = findNodeAtLocation2(tree, ["rules"]);
  const lines = {};
  const rules = {};
  for (const prop of rulesNode?.type === "object" ? rulesNode.children ?? [] : []) {
    const [k, v] = prop.children;
    const key = String(getNodeValue2(k));
    lines[key] = lineOf(text2, k.offset);
    rules[key] = getNodeValue2(v);
  }
  return { profile: { version: 1, rules }, problems: [], lines };
}
function closest(key, known) {
  let best;
  let bestD = Infinity;
  for (const k of known) {
    const d = distance(key, k);
    if (d < bestD) [best, bestD] = [k, d];
  }
  return best;
}
function distance(a, b) {
  let prev = Array.from({ length: b.length + 1 }, (_, j) => j);
  for (let i = 1; i <= a.length; i++) {
    const cur = [i];
    for (let j = 1; j <= b.length; j++)
      cur[j] = Math.min(
        prev[j] + 1,
        cur[j - 1] + 1,
        prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1)
      );
    prev = cur;
  }
  return prev[b.length];
}
function validate(profile, lines, known, defaults = {}) {
  const out = {
    merged: {},
    reasons: {},
    imported: [],
    problems: []
  };
  const row = (key, severity, message) => out.problems.push({ line: lines[key] ?? 0, severity, key, message });
  for (const [key, rule] of Object.entries(profile.rules)) {
    if (!key.includes("/")) {
      const guess = known?.find((k) => k.endsWith(`/${key}`));
      row(
        key,
        "warning",
        `${key}: a rule is written area/ruleId${guess ? ` \u2014 ${guess}` : ""}; entry ignored`
      );
      continue;
    }
    const bridged = BRIDGES.includes(key.split("/")[0]);
    if (!bridged && known && !known.includes(key)) {
      const near = closest(key, known);
      row(
        key,
        "warning",
        `unknown rule ${key} \u2014 the loaded bundle has: ${[near, ...known.filter((k) => k !== near)].join(", ")}; entry ignored`
      );
      continue;
    }
    if (!rule || typeof rule !== "object" || !LEVELS.includes(rule.severity)) {
      row(
        key,
        "warning",
        `${key}: "severity" must be one of ${LEVELS.join(", ")}; entry ignored`
      );
      continue;
    }
    const level = rule.severity;
    const def = defaults[key] ?? "error";
    const why = typeof rule.why === "string" ? rule.why.trim() : "";
    if (RANK[level] < RANK[def] && !why) {
      row(
        key,
        "warning",
        level === "off" ? `${key}: "off" needs a "why"` : `${key}: "${level}" is below the rule's default (${def}) and needs a "why"`
      );
      continue;
    }
    out.merged[key] = level;
    if (why) out.reasons[key] = why;
    if (rule.imported !== void 0) {
      out.imported.push(key);
      if (!IMPORTERS.includes(rule.imported))
        row(
          key,
          "info",
          `${key}: "imported" is "${rule.imported}", not a known importer (${IMPORTERS.join(", ")}); marked imported all the same`
        );
    }
    if (rule.options !== void 0)
      row(
        key,
        "info",
        `${key}: no rule reads "options" yet; applied without them`
      );
  }
  return out;
}
function mergeSeverities(project, overrides) {
  const out = { ...project };
  for (const [k, v] of Object.entries(overrides)) {
    const full = Object.keys(project).find((p) => p.endsWith(`/${k}`));
    if (full && !k.includes("/")) delete out[full];
    out[k] = v;
  }
  return out;
}
function bulkFixSkip(project, overrides) {
  const effective = mergeSeverities(project, overrides);
  return Object.entries(project).filter(([code, level]) => level === "off" && effective[code] === "off").map(([code]) => code.split("/").pop());
}

// ../packages/java-rules/src/project-keys.ts
var PROJECT_KEYS = {
  "jdk.requirement": {
    "type": "string",
    "pattern": "^[0-9]+$",
    "description": 'The Java major version the project needs, e.g. "21"; overrides what pom.xml or build.gradle ask for. Absent: the build tool decides.'
  },
  "maven.configuration": {
    "type": "string",
    "description": "The Maven configuration to use: a name from maven.configurations here, from batlehub.java.maven.configurations, or a detected ~/.m2/settings*.xml."
  },
  "maven.configurations": {
    "type": "array",
    "items": {
      "type": "object",
      "required": [
        "name"
      ],
      "properties": {
        "name": "string",
        "settingsFile": "string",
        "toolchainsFile": "string",
        "mavenHome": "string",
        "profiles": "string[]"
      }
    },
    "description": "Named Maven configurations, the same shape as batlehub.java.maven.configurations. A committed settingsFile is announced before it is used."
  },
  "maven.activeProfiles": {
    "type": "array",
    "items": {
      "type": "string"
    },
    "description": "Maven profiles: -P on every goal and the m2e preference."
  },
  "gradle.activeProfiles": {
    "type": "array",
    "items": {
      "type": "string"
    },
    "description": "Reserved: Gradle has no profiles; kept so the file has one shape per tool."
  },
  "registry.enabled": {
    "enum": [
      "ask",
      "true",
      "false"
    ],
    "description": "Whether Maven and Gradle resolve through BatleHub: ask, true, or false (a public repository can say never)."
  }
};
var PROJECT_SECTIONS = ["jdk", "maven", "gradle", "registry", "satellites"];

// ../packages/java-rules/src/project-config.ts
var PROJECT_FILE = ".batlehub/java/project.json";
var lineOf2 = (text2, offset) => text2.slice(0, offset).split("\n").length - 1;
function misfit(leaf, v) {
  if ("enum" in leaf)
    return leaf.enum.includes(v) ? void 0 : `must be one of ${leaf.enum.join(", ")}`;
  if (leaf.type === "string") {
    if (typeof v !== "string") return "must be a string";
    if ("pattern" in leaf && !new RegExp(leaf.pattern).test(v))
      return `must match ${leaf.pattern}`;
    return void 0;
  }
  if (leaf.type === "array") {
    if (!Array.isArray(v)) return "must be an array";
    const items = leaf.items;
    if (items.type === "string")
      return v.every((x) => typeof x === "string") ? void 0 : "must be an array of strings";
    const { required, properties } = items;
    for (const [i, x] of v.entries()) {
      if (!x || typeof x !== "object" || Array.isArray(x))
        return `item ${i} must be an object`;
      for (const r of required) if (!(r in x)) return `item ${i} needs "${r}"`;
      for (const [k, val] of Object.entries(x)) {
        const t = properties[k];
        if (!t) return `item ${i}: unknown key "${k}"`;
        const ok = t === "string[]" ? Array.isArray(val) && val.every((s) => typeof s === "string") : typeof val === t;
        if (!ok)
          return `item ${i}: "${k}" must be ${t === "string[]" ? "an array of strings" : `a ${t}`}`;
      }
    }
    return void 0;
  }
  return void 0;
}
function parseProjectFile(text2) {
  const errors = [];
  const tree = parseTree2(text2, errors, { allowTrailingComma: true });
  const empty = { values: {}, satellites: {} };
  if (errors.length || !tree || tree.type !== "object")
    return {
      ...empty,
      problems: [
        {
          path: "",
          line: errors.length ? lineOf2(text2, errors[0].offset) : 0,
          severity: "error",
          message: "project.json does not parse: the whole file is ignored until it does"
        }
      ]
    };
  const problems = [];
  const values = {};
  let satellites = {};
  for (const prop of tree.children ?? []) {
    const [k, v] = prop.children;
    const name = String(getNodeValue2(k));
    const at = lineOf2(text2, k.offset);
    if (name === "$schema") continue;
    if (name === "version") {
      const ver = getNodeValue2(v);
      if (ver !== 1)
        problems.push({
          path: "version",
          line: at,
          severity: "warning",
          message: `project.json is version ${JSON.stringify(ver)}; this java-core reads version 1 and applies the keys it knows`
        });
      continue;
    }
    if (!PROJECT_SECTIONS.includes(name)) {
      problems.push({
        path: name,
        line: at,
        severity: "warning",
        message: `unknown section "${name}" (known: ${PROJECT_SECTIONS.join(", ")}); ignored`
      });
      continue;
    }
    if (v.type !== "object") {
      problems.push({
        path: name,
        line: at,
        severity: "warning",
        message: `"${name}" must be an object; ignored`
      });
      continue;
    }
    if (name === "satellites") {
      satellites = getNodeValue2(v);
      continue;
    }
    for (const leafProp of v.children ?? []) {
      const [lk, lv] = leafProp.children;
      const key = `${name}.${String(getNodeValue2(lk))}`;
      const line = lineOf2(text2, lk.offset);
      const leaf = PROJECT_KEYS[key];
      if (!leaf) {
        problems.push({
          path: key,
          line,
          severity: "warning",
          message: `unknown key "${key}"; ignored`
        });
        continue;
      }
      const value = getNodeValue2(lv);
      const why = misfit(leaf, value);
      if (why) {
        problems.push({
          path: key,
          line,
          severity: "warning",
          message: `${key} ${why}; treated as absent`
        });
        continue;
      }
      values[key] = value;
    }
  }
  return { values, satellites, problems };
}
var PROJECT_DEFAULTS = {
  "maven.activeProfiles": [],
  "gradle.activeProfiles": [],
  "registry.enabled": "ask"
};
function resolve(settings, project, detected) {
  const out = {};
  for (const key of Object.keys(PROJECT_KEYS)) {
    const p = project[key];
    const named = p !== void 0;
    let r;
    if (settings[key] !== void 0)
      r = {
        value: settings[key],
        origin: "settings",
        ...named ? { project: p } : {},
        ...named && JSON.stringify(settings[key]) !== JSON.stringify(p) ? { differsFromProject: true } : {}
      };
    else if (named) r = { value: p, origin: "project.json", project: p };
    else if (detected[key] !== void 0)
      r = { value: detected[key], origin: "detected" };
    else if (PROJECT_DEFAULTS[key] !== void 0)
      r = { value: PROJECT_DEFAULTS[key], origin: "default" };
    out[key] = r;
  }
  return out;
}

// config.ts
function committedBase(root) {
  for (let d = root; ; d = path3.dirname(d)) {
    if (existsSync2(path3.join(d, ".batlehub", "java"))) return d;
    if (existsSync2(path3.join(d, ".git")) || path3.dirname(d) === d) return root;
  }
}
function teamProfile(root, bundle, read, file) {
  const known = bundle?.inspections ?? [];
  const defaults = { ...BRIDGED_DEFAULTS, ...bundle?.defaults };
  const f = file ? path3.resolve(file) : path3.join(committedBase(root), PROFILE);
  const text2 = read(f);
  const none = { levels: {}, reasons: {}, problems: [], known, defaults };
  if (text2 === void 0) {
    if (file) throw new Error(`${file}: no such profile`);
    return none;
  }
  const { profile, lines, problems } = parseProfile(text2);
  if (!profile)
    throw new Error(
      `${f}: not an inspection profile \u2014 ${problems.map((p) => `line ${p.line + 1}: ${p.message}`).join("; ")}`
    );
  const v = validate(profile, lines, known, defaults);
  return {
    file: f,
    levels: v.merged,
    reasons: v.reasons,
    problems: [...problems, ...v.problems],
    known,
    defaults
  };
}
function printedProfile(p) {
  const codes = [.../* @__PURE__ */ new Set([...p.known, ...Object.keys(p.levels)])].sort();
  return {
    file: p.file ?? null,
    rules: Object.fromEntries(
      codes.map((c) => [
        c,
        {
          level: p.levels[c] ?? p.defaults[c] ?? "error",
          origin: p.levels[c] !== void 0 ? "profile" : "default",
          default: p.defaults[c] ?? "error",
          ...p.reasons[c] ? { why: p.reasons[c] } : {}
        }
      ])
    )
  };
}
function requiredJava(root, read) {
  const pom = read(path3.join(root, "pom.xml"));
  if (pom !== void 0) return requiredJavaOf2(pom)?.min;
  const gradle = GRADLE_BUILD_FILES.map((f) => path3.join(root, f)).find(
    (f) => existsSync2(f)
  );
  const text2 = gradle ? read(gradle) : void 0;
  return text2 === void 0 ? void 0 : requiredJavaOf(text2)?.min;
}
function projectConfig(root, read) {
  const file = path3.join(committedBase(root), PROJECT_FILE);
  const text2 = read(file);
  const parsed = text2 === void 0 ? void 0 : parseProjectFile(text2);
  const min = requiredJava(root, read);
  const detected = min === void 0 ? {} : { "jdk.requirement": String(min) };
  return {
    config: resolve({}, parsed?.values ?? {}, detected),
    problems: parsed?.problems ?? [],
    file
  };
}

// mcp.ts
import os5 from "node:os";
import path7 from "node:path";

// ../packages/java-rules/src/protocol.ts
var VERSIONS = ["2025-06-18", "2025-03-26", "2024-11-05"];
var McpSession = class {
  buffer = "";
  // Plain fields, not parameter properties: Node runs this file by
  // stripping types, which only works for erasable syntax.
  surface;
  version;
  call;
  send;
  constructor(surface, version, call, send) {
    this.surface = surface;
    this.version = version;
    this.call = call;
    this.send = send;
  }
  feed(chunk) {
    this.buffer += chunk;
    let nl;
    while ((nl = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, nl).trim();
      this.buffer = this.buffer.slice(nl + 1);
      if (line) void this.handle(line).then((r) => r && this.send(`${r}
`));
    }
  }
  /** One frame in, the framed reply out (undefined for a notification). */
  async handle(line) {
    let m;
    try {
      m = JSON.parse(line);
    } catch {
      return reply(null, void 0, { code: -32700, message: "parse error" });
    }
    if (m.id === void 0) return void 0;
    switch (m.method) {
      case "initialize": {
        const asked = m.params?.protocolVersion;
        return reply(m.id, {
          protocolVersion: asked && VERSIONS.includes(asked) ? asked : VERSIONS[0],
          capabilities: { tools: {} },
          serverInfo: { name: "batlehub-java", version: this.version }
        });
      }
      case "ping":
        return reply(m.id, {});
      case "tools/list":
        return reply(m.id, { tools: toolSchemas(this.surface) });
      case "tools/call": {
        const name = String(m.params?.name ?? "");
        try {
          const out = await this.call(
            name,
            checkArgs(name, m.params?.arguments, this.surface)
          );
          return reply(m.id, {
            content: [{ type: "text", text: JSON.stringify(out, null, 2) }],
            structuredContent: out
          });
        } catch (e) {
          return reply(m.id, {
            content: [{ type: "text", text: e.message }],
            isError: true
          });
        }
      }
      default:
        return reply(m.id, void 0, {
          code: -32601,
          message: `method not found: ${m.method}`
        });
    }
  }
};
function reply(id, result, error) {
  return JSON.stringify({
    jsonrpc: "2.0",
    id,
    ...error ? { error } : { result }
  });
}

// package.json
var package_default = {
  name: "@batlehub/java-engine",
  version: "0.1.0",
  private: true,
  description: "The headless engine of RFC 0002: a JDT.LS launcher, and from phase 2 the batlehub java verbs over it. Erasable TypeScript, run by Node 24 as is.",
  license: "Apache-2.0",
  type: "module",
  exports: {
    "./*": "./*.ts"
  },
  scripts: {
    typecheck: "tsc --noEmit -p tsconfig.json",
    lint: 'oxlint . && prettier --check "*.ts" "test/**/*.ts"',
    format: 'prettier --write "*.ts" "test/**/*.ts"',
    test: "vitest run",
    bundle: "node bundle.mjs"
  },
  devDependencies: {
    "@types/node": "^24.0.0",
    oxlint: "^1.0.0",
    prettier: "^3.6.0",
    typescript: "^5.9.0",
    vitest: "^5.0.3",
    esbuild: "^0.25.0"
  },
  dependencies: {
    "@batlehub/java-rules": "workspace:*"
  }
};

// launch.ts
import { spawn } from "node:child_process";
import { cpSync, existsSync as existsSync3, mkdtempSync, readdirSync, rmSync as rmSync2 } from "node:fs";
import { tmpdir } from "node:os";
import path4 from "node:path";
import { pathToFileURL } from "node:url";
var fileUri = (p) => pathToFileURL(p).href;
async function launch(o) {
  const plugins = path4.join(o.server, "plugins");
  if (!existsSync3(plugins)) throw new Error(`no JDT.LS at ${o.server}`);
  const launcher = readdirSync(plugins).find(
    (f) => /^org\.eclipse\.equinox\.launcher_.*\.jar$/.test(f)
  );
  if (!launcher) throw new Error(`no equinox launcher in ${plugins}`);
  const config2 = mkdtempSync(path4.join(tmpdir(), "batlehub-jdtls-config-"));
  const data = o.data ?? mkdtempSync(path4.join(tmpdir(), "batlehub-jdtls-data-"));
  const owned = [config2, ...o.data ? [] : [data]];
  const cleanup = () => {
    for (const d of owned) rmSync2(d, { recursive: true, force: true });
  };
  cpSync(
    path4.join(o.server, "config_linux", "config.ini"),
    path4.join(config2, "config.ini")
  );
  const java = o.java ?? (process.env.JAVA_HOME ? path4.join(process.env.JAVA_HOME, "bin", "java") : "java");
  const proc = spawn(
    java,
    [
      "-Declipse.application=org.eclipse.jdt.ls.core.id1",
      "-Dosgi.bundles.defaultStartLevel=4",
      "-Declipse.product=org.eclipse.jdt.ls.core.product",
      "-Dlog.level=ALL",
      `-Xmx${o.heap ?? "1G"}`,
      "-jar",
      path4.join(plugins, launcher),
      "-configuration",
      config2,
      "-data",
      data
    ],
    { stdio: ["pipe", "pipe", "pipe"] }
  );
  const err = [];
  proc.stderr.on("data", (d) => err.push(String(d)));
  const exited = new Promise((r) => proc.once("exit", () => r()));
  let buf = Buffer.alloc(0);
  let id = 0;
  const pending = /* @__PURE__ */ new Map();
  const notes = [];
  const send = (msg) => {
    const s = JSON.stringify(msg);
    proc.stdin.write(`Content-Length: ${Buffer.byteLength(s)}\r
\r
${s}`);
  };
  proc.stdout.on("data", (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    for (; ; ) {
      const i = buf.indexOf("\r\n\r\n");
      if (i < 0) return;
      const len = Number(
        /Content-Length: (\d+)/.exec(buf.subarray(0, i).toString())?.[1]
      );
      if (buf.length < i + 4 + len) return;
      const msg = JSON.parse(buf.subarray(i + 4, i + 4 + len).toString());
      buf = buf.subarray(i + 4 + len);
      if (msg.id !== void 0 && pending.has(msg.id)) {
        pending.get(msg.id)(msg);
        pending.delete(msg.id);
      } else if (msg.method && msg.id !== void 0) {
        send({
          jsonrpc: "2.0",
          id: msg.id,
          result: msg.method === "workspace/configuration" ? (msg.params?.items ?? []).map(() => null) : null
        });
      } else notes.push(msg);
    }
  });
  const request = (method, params, timeoutMs = 12e4) => new Promise((resolve3, reject) => {
    const my = ++id;
    const t = setTimeout(
      () => reject(new Error(`${method} timed out`)),
      timeoutMs
    );
    pending.set(my, (m) => {
      clearTimeout(t);
      if (m.error)
        reject(
          new Error(
            m.error.message ?? `${method}: ${JSON.stringify(m.error)}`
          )
        );
      else resolve3(m.result);
    });
    send({ jsonrpc: "2.0", id: my, method, params });
  });
  const notify = (method, params) => send({ jsonrpc: "2.0", method, params });
  const server = {
    notes,
    request,
    notify,
    exec: (command, ...args) => request("workspace/executeCommand", { command, arguments: args }),
    pid: proc.pid,
    async ready(timeoutMs = 3e5, progress) {
      const t0 = Date.now();
      let told = 6e4;
      const statuses = () => [
        ...new Set(
          notes.filter((n) => n.method === "language/status").map((n) => String(n.params?.type))
        )
      ];
      while (!statuses().includes("ServiceReady")) {
        if (Date.now() - t0 > timeoutMs)
          throw new Error(
            `no ServiceReady in ${Math.round(timeoutMs / 1e3)} s (last: ${statuses().at(-1) ?? "none"})`
          );
        if (progress && Date.now() - t0 >= told) {
          told += 1e4;
          const last = notes.filter((n) => n.method === "language/status").at(-1)?.params?.message;
          progress({ ms: Date.now() - t0, last });
        }
        await new Promise((r) => setTimeout(r, 500));
      }
      return { ms: Date.now() - t0, statuses: statuses() };
    },
    stderr: () => err.join(""),
    async stop() {
      if (proc.exitCode === null) {
        await request("shutdown", null, 5e3).catch(() => {
        });
        notify("exit", null);
        const t = setTimeout(() => proc.kill("SIGTERM"), 5e3);
        await exited;
        clearTimeout(t);
      }
      cleanup();
    }
  };
  const root = fileUri(o.workspace);
  let init;
  try {
    init = await request("initialize", {
      processId: process.pid,
      rootUri: root,
      workspaceFolders: [{ uri: root, name: path4.basename(o.workspace) }],
      capabilities: {
        workspace: { executeCommand: { dynamicRegistration: false } },
        textDocument: {}
      },
      initializationOptions: {
        bundles: o.bundles ?? [],
        workspaceFolders: [root],
        settings: o.settings ?? {},
        extendedClientCapabilities: { classFileContentsSupport: false }
      }
    });
  } catch (e) {
    proc.kill("SIGTERM");
    cleanup();
    throw e;
  }
  notify("initialized", {});
  return { ...server, init };
}

// ops.ts
import {
  existsSync as existsSync5,
  readdirSync as readdirSync3,
  readFileSync as readFileSync5,
  realpathSync as realpathSync3,
  statSync as statSync3
} from "node:fs";
import os4 from "node:os";
import path6 from "node:path";

// ../packages/java-rules/src/discover.ts
function parseRelease(text2) {
  const get = (k) => new RegExp(`^${k}="([^"]*)"`, "m").exec(text2)?.[1];
  const version = get("JAVA_VERSION");
  if (!version) return void 0;
  return { version, vendor: get("IMPLEMENTOR") };
}
function majorOf(version) {
  const m = /^(\d+)(?:\.(\d+))?/.exec(version);
  if (!m) return 0;
  const first = Number(m[1]);
  return first === 1 && m[2] ? Number(m[2]) : first;
}
var runtimeName = (major) => major <= 8 ? `JavaSE-1.${major}` : `JavaSE-${major}`;
function runtimeAt(io, home, source) {
  const rel = io.readFile(`${home}/release`);
  const parsed = rel ? parseRelease(rel) : void 0;
  if (!parsed) return void 0;
  const major = majorOf(parsed.version);
  if (!major) return void 0;
  return {
    name: runtimeName(major),
    path: home,
    version: parsed.version,
    major,
    vendor: parsed.vendor,
    source
  };
}
function parseMiseLs(json2) {
  try {
    const arr = JSON.parse(json2);
    if (!Array.isArray(arr)) return [];
    return arr.filter((e) => e.installed !== false && e.install_path).map((e) => e.install_path);
  } catch {
    return [];
  }
}
async function fromMise(io) {
  const out = await io.exec("mise", ["ls", "java", "--json"]);
  if (!out) return [];
  return parseMiseLs(out).map((p) => runtimeAt(io, p, "mise")).filter((r) => !!r);
}
function fromSdkman(io) {
  const base = io.env.SDKMAN_DIR || `${io.home}/.sdkman`;
  const dir = `${base}/candidates/java`;
  return io.readDir(dir).filter((n) => n !== "current").map((n) => runtimeAt(io, `${dir}/${n}`, "sdkman")).filter((r) => !!r);
}
function fromEnv(io) {
  const out = [];
  for (const k of ["JAVA_HOME", "JDK_HOME"]) {
    const p = io.env[k];
    if (!p) continue;
    const r = runtimeAt(io, p.replace(/[/\\]+$/, ""), "env");
    if (r) out.push(r);
  }
  return out;
}
function wellKnownDirs(io) {
  const dirs = [
    `${io.home}/.jdks`,
    "/usr/lib/jvm",
    "/opt/java",
    "/usr/java",
    "/usr/local/java"
  ];
  if (io.platform === "darwin") dirs.push("/Library/Java/JavaVirtualMachines");
  if (io.platform === "win32")
    dirs.push("C:\\Program Files\\Java", "C:\\Program Files\\Eclipse Adoptium");
  return dirs;
}
function fromWellKnown(io) {
  const out = [];
  for (const dir of wellKnownDirs(io)) {
    for (const n of io.readDir(dir)) {
      const home = `${dir}/${n}`;
      const r = runtimeAt(io, home, "wellKnown") ?? runtimeAt(io, `${home}/Contents/Home`, "wellKnown");
      if (r) out.push(r);
    }
  }
  return out;
}
async function discover(io, sources2, settings = []) {
  const found = [];
  for (const s of sources2) {
    if (s === "mise") found.push(...await fromMise(io));
    else if (s === "sdkman") found.push(...fromSdkman(io));
    else if (s === "env") found.push(...fromEnv(io));
    else if (s === "wellKnown") found.push(...fromWellKnown(io));
  }
  for (const s of settings) {
    const path9 = s.path.replace(/[/\\]+$/, "");
    const r = runtimeAt(io, path9, "settings");
    found.push(
      r ?? {
        name: s.name,
        path: path9,
        version: s.name.replace(/^JavaSE-(1\.)?/, ""),
        major: majorOf(s.name.replace(/^JavaSE-/, "")),
        source: "settings"
      }
    );
  }
  const byPath = /* @__PURE__ */ new Map();
  for (const r of found) if (!byPath.has(r.path)) byPath.set(r.path, r);
  return [...byPath.values()].sort(
    (a, b) => b.major - a.major || b.version.localeCompare(a.version)
  );
}

// ../packages/java-rules/src/io.ts
import { execFile as execFile2 } from "node:child_process";
import * as fs from "node:fs";
import * as os2 from "node:os";
function realIo(opts) {
  const env = opts.env ?? process.env;
  return {
    readFile: (p) => {
      try {
        return fs.readFileSync(p, "utf8");
      } catch {
        return void 0;
      }
    },
    readDir: (p) => {
      try {
        return fs.readdirSync(p);
      } catch {
        return [];
      }
    },
    isDir: (p) => {
      try {
        return fs.statSync(p).isDirectory();
      } catch {
        return false;
      }
    },
    exec: (cmd, args) => new Promise((resolve3) => {
      if (!opts.trusted) return resolve3(void 0);
      execFile2(
        cmd,
        args,
        { cwd: opts.cwd, env, maxBuffer: 64 * 1024 * 1024, timeout: 12e4 },
        (err, stdout) => resolve3(err ? void 0 : String(stdout))
      );
    }),
    env,
    home: os2.homedir(),
    platform: process.platform
  };
}
function homeIo() {
  return realIo({ trusted: true, cwd: os2.homedir() });
}

// ../packages/java-rules/src/ledger.ts
import crypto from "node:crypto";
import {
  existsSync as existsSync4,
  mkdirSync as mkdirSync2,
  readFileSync as readFileSync4,
  realpathSync as realpathSync2,
  renameSync as renameSync2,
  writeFileSync as writeFileSync3
} from "node:fs";
import os3 from "node:os";
import path5 from "node:path";
var cacheHome = (env = process.env, home = os3.homedir()) => env.XDG_CACHE_HOME || path5.join(home, ".cache");
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
}
function ledgerFile(root, cacheHome2) {
  let repo = root;
  for (let d = root; ; d = path5.dirname(d)) {
    if (existsSync4(path5.join(d, ".git"))) {
      repo = d;
      break;
    }
    if (path5.dirname(d) === d) break;
  }
  let real = repo;
  try {
    real = realpathSync2(repo);
  } catch {
  }
  const key = crypto.createHash("sha256").update(real).digest("hex");
  return path5.join(
    cacheHome2,
    "batlehub",
    "java",
    "ledger",
    `${key.slice(0, 16)}.json`
  );
}
function readLedger(file, isAlive = alive) {
  try {
    const lines = JSON.parse(readFileSync4(file, "utf8"));
    return Array.isArray(lines) ? lines.filter((l) => isAlive(l.pid)) : [];
  } catch {
    return [];
  }
}
function update(file, f, isAlive) {
  mkdirSync2(path5.dirname(file), { recursive: true, mode: 448 });
  const part = `${file}.${process.pid}.part`;
  writeFileSync3(part, JSON.stringify(f(readLedger(file, isAlive))), {
    mode: 384
  });
  renameSync2(part, file);
}
function putLine(file, line, isAlive = alive) {
  update(
    file,
    (ls) => [
      ...ls.filter((l) => !(l.pid === line.pid && l.owner === line.owner)),
      line
    ],
    isAlive
  );
}
function removeLine(file, pid, owner, isAlive = alive) {
  update(
    file,
    (ls) => ls.filter((l) => !(l.pid === pid && l.owner === owner)),
    isAlive
  );
}

// ../packages/java-rules/src/resolve.ts
function resolve2(runtimes, required, matchProject = true) {
  const sorted = [...runtimes].sort((a, b) => b.major - a.major);
  if (required && matchProject) {
    const inRange = sorted.filter(
      (r) => r.major >= required.min && (required.max === void 0 || r.major <= required.max)
    );
    const exact = inRange.find((r) => r.major === required.min) ?? inRange.at(-1);
    if (exact) return { runtime: exact, required, reason: "matches" };
  }
  const newest = sorted[0];
  return newest ? { runtime: newest, required, reason: "newest" } : { required, reason: "none" };
}

// ../packages/java-rules/src/resources.ts
var CGROUP2 = "/sys/fs/cgroup/memory.max";
var CGROUP1 = "/sys/fs/cgroup/memory/memory.limit_in_bytes";
var UNLIMITED = 2 ** 50;
function readCgroupLimit(read) {
  const v2 = read(CGROUP2)?.trim();
  if (v2 !== void 0) {
    if (v2 === "max") return {};
    const n = Number(v2);
    return Number.isFinite(n) ? { limit: n, source: "cgroup2" } : {};
  }
  const v1 = read(CGROUP1)?.trim();
  if (v1 !== void 0) {
    const n = Number(v1);
    return Number.isFinite(n) && n < UNLIMITED ? { limit: n, source: "cgroup1" } : {};
  }
  return {};
}
var UNITS = {
  k: 2 ** 10,
  m: 2 ** 20,
  g: 2 ** 30,
  t: 2 ** 40
};
function parseSize(s) {
  const m = /^\s*(\d+(?:\.\d+)?)\s*([kmgt])?i?b?\s*$/i.exec(s);
  if (!m) return void 0;
  const n = Number(m[1]);
  const unit = m[2]?.toLowerCase();
  return Math.round(unit ? n * UNITS[unit] : n);
}
function formatSize(bytes) {
  if (bytes >= 2 ** 30)
    return `${(bytes / 2 ** 30).toFixed(bytes % 2 ** 30 ? 1 : 0)} GiB`;
  if (bytes >= 2 ** 20) return `${Math.round(bytes / 2 ** 20)} MiB`;
  return `${bytes} B`;
}
var GRADLE_DAEMON = 512 * 2 ** 20;
var BASELINE = 768 * 2 ** 20;

// ../packages/java-rules/src/rules.ts
function applyOverrides(rows, overrides) {
  const out = [];
  for (const r of rows) {
    const o = overrides[r.code] ?? overrides[r.ruleId];
    if (o === "off") continue;
    out.push(o ? { ...r, severity: o } : r);
  }
  return out;
}

// ops.ts
var EXIT = {
  ok: 0,
  findings: 1,
  unusable: 2,
  refused: 3,
  timeout: 4
};
var Exit = class extends Error {
  code;
  constructor(code, message) {
    super(message);
    this.code = code;
  }
};
var MARKERS = [
  "pom.xml",
  "build.gradle",
  "build.gradle.kts",
  "settings.gradle",
  "settings.gradle.kts",
  ".git"
];
var inside = (p, dir) => {
  const r = path6.relative(dir, p);
  return r === "" || !r.startsWith("..") && !path6.isAbsolute(r);
};
function workspaceRoot(from, opts) {
  const exists = opts.exists ?? existsSync5;
  let root = opts.explicit ? path6.resolve(opts.cwd, opts.explicit) : void 0;
  if (!root) {
    const start2 = path6.resolve(opts.cwd, from);
    for (let d = start2; ; d = path6.dirname(d)) {
      if (MARKERS.some((m) => exists(path6.join(d, m)))) {
        root = d;
        break;
      }
      if (path6.dirname(d) === d) break;
    }
    root ??= start2;
  }
  if (root === path6.parse(root).root || !(inside(root, opts.home) || inside(root, opts.cwd) || inside(opts.cwd, root)))
    throw new Error(
      `the workspace ${root} is neither under your home nor under, or above, the current directory: name one with --workspace`
    );
  return root;
}
async function serverJdk(io) {
  const forced = io.env.BATLEHUB_JAVA_HOME;
  const runtimes = forced ? [runtimeAt(io, forced.replace(/[/\\]+$/, ""), "env")].filter(
    (r2) => !!r2
  ) : await discover(io, ["mise", "sdkman", "env", "wellKnown"]);
  const r = resolve2(runtimes, {
    min: SERVER_MIN_JAVA,
    origin: `redhat.java ${PIN.version}`
  }).runtime;
  if (!r || r.major < SERVER_MIN_JAVA)
    throw new Exit(
      EXIT.unusable,
      forced ? `BATLEHUB_JAVA_HOME=${forced} is not a JDK \u2265 ${SERVER_MIN_JAVA}` : `no JDK \u2265 ${SERVER_MIN_JAVA}: install one with 'mise use java@temurin-21'`
    );
  return r;
}
function capCheck(heap, read) {
  const cap = parseSize(heap);
  if (!cap)
    throw new Exit(
      EXIT.unusable,
      `--heap ${heap} is not a size (512m, 1G, 2G)`
    );
  const { limit } = readCgroupLimit(read);
  if (limit !== void 0 && cap > limit)
    throw new Exit(
      EXIT.unusable,
      `the server's declared cap ${formatSize(cap)} does not fit the container's limit ${formatSize(limit)}: lower --heap, or use the live editor's tools, which start nothing`
    );
  return { cap, limit };
}
function ledgerCheck(cap, limit, running) {
  if (limit === void 0) return;
  const total = running.reduce((n, l) => n + l.bytes, cap);
  if (total > limit)
    throw new Exit(
      EXIT.unusable,
      `the server's declared cap ${formatSize(cap)} and what already runs on this repository \u2014 ${running.map((l) => `${l.label}: ${formatSize(l.bytes)}`).join(", ")} \u2014 do not fit the container's limit ${formatSize(limit)}: lower --heap, or use the live editor's tools, which start nothing`
    );
}
var readOrUndefined = (p) => {
  try {
    return readFileSync5(p, "utf8");
  } catch {
    return void 0;
  }
};
var tilde = (p) => inside(p, os4.homedir()) ? `~/${path6.relative(os4.homedir(), p)}` : p;
async function statusOf() {
  const jdk = await serverJdk(homeIo());
  const { server, from } = locate();
  const jar = bundleJar();
  const { cap, limit } = capCheck("1G", readOrUndefined);
  return {
    jdk: `${jdk.name} (${jdk.version}, ${jdk.source})`,
    server: `redhat.java ${PIN.version} ${server ? `at ${tilde(server)}` : `not downloaded yet (the first inspect fetches it into ${tilde(from)})`}`,
    bundle: jar ? tilde(jar) : "not found: run 'task jdt:build', or set BATLEHUB_JDT_BUNDLE",
    cap: `${formatSize(cap)} declared, container limit ${limit ? formatSize(limit) : "none"}`
  };
}
function javaFiles(sourceRoots, paths2) {
  const out = /* @__PURE__ */ new Set();
  for (const root of sourceRoots)
    for (const rel of readdirSync3(root, { recursive: true })) {
      const f = path6.join(root, rel);
      if (f.endsWith(".java") && paths2.some((p) => inside(f, p))) out.add(f);
    }
  return [...out].sort();
}
async function start(root, o, warn) {
  if (o.verbose && /"batlehub\.java\./.test(
    readOrUndefined(path6.join(root, ".vscode", "settings.json")) ?? ""
  ))
    warn(
      ".vscode/settings.json holds batlehub.java.* keys: not read \u2014 CI and agents report at the project's values (RFC 0002 \xA74.1)"
    );
  const jdk = await serverJdk(homeIo());
  const { cap, limit } = capCheck(o.heap, readOrUndefined);
  const ledger = ledgerFile(root, cacheHome());
  ledgerCheck(cap, limit, readLedger(ledger));
  const jar = bundleJar();
  if (!jar)
    throw new Exit(
      EXIT.unusable,
      "no BatleHub JDT bundle: run 'task jdt:build', or set BATLEHUB_JDT_BUNDLE"
    );
  let serverDir;
  try {
    serverDir = await ensure(jdk.path, warn);
  } catch (e) {
    throw new Exit(EXIT.unusable, e.message);
  }
  if (o.verbose)
    warn(
      `workspace ${root}, JDK ${jdk.name} (${jdk.version}), -Xmx${o.heap}, a fresh -data`
    );
  const server = await launch({
    server: serverDir,
    workspace: root,
    bundles: [jar],
    java: path6.join(jdk.path, "bin", "java"),
    heap: o.heap,
    settings: { java: { import: { gradle: { enabled: true } } } }
  });
  if (server.pid !== void 0)
    putLine(ledger, {
      pid: server.pid,
      owner: "engine",
      label: `batlehub java (pid ${server.pid})`,
      bytes: cap
    });
  const stop = server.stop;
  server.stop = async () => {
    try {
      await stop();
    } finally {
      if (server.pid !== void 0) removeLine(ledger, server.pid, "engine");
    }
  };
  try {
    await server.ready(
      o.timeout * 1e3,
      (s) => warn(
        `still importing after ${Math.round(s.ms / 1e3)} s (last status: ${s.last ?? "none"})`
      )
    );
  } catch (e) {
    await server.stop();
    throw new Exit(EXIT.timeout, e.message);
  }
  return {
    server,
    root,
    peak: () => {
      const hwm = /VmHWM:\s+(\d+) kB/.exec(
        readOrUndefined(`/proc/${server.pid}/status`) ?? ""
      )?.[1];
      return hwm ? formatSize(Number(hwm) * 1024) : "unknown";
    }
  };
}
async function sources(server, paths2) {
  const listed = await server.exec("java.project.listSourcePaths");
  const roots = (listed?.data ?? []).map((d) => d.path).filter((p) => existsSync5(p));
  return javaFiles(roots, paths2);
}
var mtimesOf = (files) => new Map(files.map((f) => [realpathSync3(f), statSync3(f).mtimeMs]));
var bundleRules = async (server) => await server.exec("batlehub.ping");
async function findings(server, files, base, levels = {}) {
  const out = [];
  for (const f of files) {
    const rows = await server.exec(
      "batlehub.inspections.list",
      fileUri(f)
    ) ?? [];
    for (const r of applyOverrides(rows, levels))
      out.push({ path: path6.relative(base, f), ...r });
  }
  return out;
}
async function fixEdit(server, files, rule, levels = {}) {
  const edit = { changes: {} };
  const skip = rule ? [] : bulkFixSkip(levels, {});
  for (const f of files) {
    const e = await server.exec(
      "batlehub.inspections.fixAll",
      fileUri(f),
      rule ? rule.split("/").pop() : null,
      skip
    );
    for (const [u, l] of Object.entries(e?.changes ?? {}))
      if (l.length) edit.changes[u] = l;
  }
  return edit;
}
async function generateEdit(server, file, line, o) {
  let call;
  try {
    call = generateCall({
      what: o.what,
      uri: fileUri(file),
      line,
      endLine: o.endLine,
      options: o.options,
      accessorOptions: {
        getterPrefix: o.getterPrefix ?? "get",
        booleanPrefix: o.booleanPrefix ?? "is",
        fluentSetters: o.fluentSetters ?? false,
        finalFields: "keepSetters"
      }
    });
  } catch (e) {
    throw new Exit(EXIT.unusable, e.message);
  }
  return await server.exec(call.command, ...call.args) ?? {};
}
function symbolRef(symbol, base) {
  try {
    const ref = parseSymbol(symbol);
    return ref.kind === "position" ? { ...ref, path: path6.resolve(base, ref.path) } : ref;
  } catch (e) {
    throw new Exit(EXIT.unusable, e.message);
  }
}
async function renameEdit(server, ref, symbol, newName) {
  try {
    return await (ref.kind === "position" ? server.exec(
      "batlehub.rename",
      fileUri(ref.path),
      ref.line - 1,
      ref.col - 1,
      newName
    ) : server.exec("batlehub.rename", symbol, newName)) ?? {};
  } catch (e) {
    throw new Exit(
      EXIT.findings,
      e.message.replace(/^batlehub: /, "")
    );
  }
}

// mcp.ts
var VERSION = package_default.version;
function tools(root, server) {
  const abs = (p) => {
    const f = path7.resolve(root, p);
    if (!inside(f, root)) throw new Error(`${p} is outside the workspace`);
    return f;
  };
  const rel = (f) => path7.relative(root, f);
  const levels = async (s) => teamProfile(root, await bundleRules(s.server), readOrUndefined).levels;
  const finish = async (s, edit, dryRun2, mtimes) => {
    const planned = plan(edit);
    const applied = !dryRun2 && planned.length > 0;
    if (applied) {
      try {
        write(planned, root, mtimes);
      } catch (e) {
        if (e instanceof Refused)
          throw new Error(`write refused: ${e.message}`);
        throw e;
      }
      s.server.notify("workspace/didChangeWatchedFiles", {
        changes: planned.map((p) => ({ uri: fileUri(p.file), type: 2 }))
      });
      await s.server.exec(
        "batlehub.refresh",
        planned.map((p) => fileUri(p.file))
      );
    }
    return {
      files: planned.length,
      edits: planned.reduce((n, p) => n + p.edits, 0),
      applied,
      edit: {
        changes: Object.fromEntries(
          Object.entries(edit.changes ?? {}).map(([u, l]) => [
            rel(new URL(u).pathname),
            l
          ])
        )
      }
    };
  };
  return {
    java_status: async () => ({ workspace: root, ...await statusOf() }),
    java_inspect: async (a) => {
      const s = await server();
      const files = await sources(
        s.server,
        (a.paths ?? ["."]).map(abs)
      );
      return {
        files: files.length,
        findings: await findings(s.server, files, root, await levels(s))
      };
    },
    java_fix: async (a) => {
      const s = await server();
      const files = await sources(
        s.server,
        (a.paths ?? ["."]).map(abs)
      );
      const mtimes = mtimesOf(files);
      const edit = await fixEdit(
        s.server,
        files,
        a.rule,
        await levels(s)
      );
      return finish(s, edit, a.dryRun, mtimes);
    },
    java_generate: async (a) => {
      const s = await server();
      const file = abs(a.file);
      const mtimes = mtimesOf([file]);
      const edit = await generateEdit(s.server, file, a.line, {
        what: a.what,
        endLine: a.endLine,
        options: a.options,
        getterPrefix: a.getterPrefix,
        booleanPrefix: a.booleanPrefix,
        fluentSetters: a.fluentSetters
      });
      if (edit.refused) throw new Error(edit.refused);
      const out = await finish(
        s,
        { changes: edit.changes ?? {} },
        a.dryRun,
        mtimes
      );
      return edit.note ? { ...out, note: edit.note } : out;
    },
    java_rename: async (a) => {
      const s = await server();
      const ref = symbolRef(a.symbol, root);
      if (ref.kind === "position") abs(ref.path);
      const mtimes = mtimesOf(await sources(s.server, [root]));
      const edit = await renameEdit(
        s.server,
        ref,
        a.symbol,
        a.newName
      );
      return finish(s, edit, a.dryRun, mtimes);
    }
  };
}
async function serve(o, warn) {
  const root = workspaceRoot(".", {
    cwd: process.cwd(),
    home: os5.homedir(),
    explicit: o.workspace
  });
  let started;
  const server = () => started ??= start(root, o, warn).catch((e) => {
    started = void 0;
    throw e;
  });
  const t = tools(root, server);
  const session2 = new McpSession(
    "stdio",
    VERSION,
    async (name, args) => {
      const t0 = Date.now();
      try {
        return await t[name](args);
      } finally {
        if (o.verbose) warn(`mcp: ${name} in ${Date.now() - t0} ms`);
      }
    },
    (line) => process.stdout.write(line)
  );
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (d) => session2.feed(d));
  await new Promise((r) => {
    process.stdin.once("end", r);
    process.once("SIGTERM", r);
  });
  if (started) await (await started.catch(() => void 0))?.server.stop();
  return 0;
}

// render.ts
var RANK2 = {
  hint: 0,
  info: 1,
  warning: 2,
  error: 3
};
function failed(findings2, failOn) {
  return findings2.some((f) => RANK2[f.severity] >= RANK2[failOn]);
}
function text(findings2) {
  return findings2.map(
    (f) => `${f.path}:${f.range.start.line + 1}:${f.range.start.character + 1} ${f.code} ${f.message}${f.fixTitle ? " [fix]" : ""}`
  ).join("\n");
}
function json(findings2, exit) {
  return JSON.stringify({ findings: findings2, exit }, null, 2);
}
var LEVEL = {
  error: "error",
  warning: "warning",
  info: "note",
  hint: "note"
};
function sarif(findings2, version) {
  const rules = [...new Set(findings2.map((f) => f.code))].sort();
  return JSON.stringify(
    {
      $schema: "https://json.schemastore.org/sarif-2.1.0.json",
      version: "2.1.0",
      runs: [
        {
          tool: {
            driver: {
              name: "batlehub-java",
              version,
              informationUri: "https://batleforc.github.io/batlehub-vsx/guide/java/agents",
              rules: rules.map((id) => ({ id }))
            }
          },
          results: findings2.map((f) => ({
            ruleId: f.code,
            level: LEVEL[f.severity],
            message: { text: f.message },
            locations: [
              {
                physicalLocation: {
                  artifactLocation: { uri: f.path.split("\\").join("/") },
                  region: {
                    startLine: f.range.start.line + 1,
                    startColumn: f.range.start.character + 1,
                    endLine: f.range.end.line + 1,
                    endColumn: f.range.end.character + 1
                  }
                }
              }
            ]
          }))
        }
      ]
    },
    null,
    2
  );
}
function unifiedDiff(file, before, after) {
  const lines = (t) => (t.endsWith("\n") ? t.slice(0, -1) : t).split("\n");
  const a = lines(before);
  const b = lines(after);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail])
    tail++;
  const am = a.slice(head, a.length - tail);
  const bm = b.slice(head, b.length - tail);
  const lcs = Array.from(
    { length: am.length + 1 },
    () => new Array(bm.length + 1).fill(0)
  );
  for (let i2 = am.length - 1; i2 >= 0; i2--)
    for (let j2 = bm.length - 1; j2 >= 0; j2--)
      lcs[i2][j2] = am[i2] === bm[j2] ? lcs[i2 + 1][j2 + 1] + 1 : Math.max(lcs[i2 + 1][j2], lcs[i2][j2 + 1]);
  const ops = [];
  for (const l of a.slice(0, head)) ops.push({ k: " ", l });
  let i = 0;
  let j = 0;
  while (i < am.length || j < bm.length) {
    if (i < am.length && j < bm.length && am[i] === bm[j]) {
      ops.push({ k: " ", l: am[i++] });
      j++;
    } else if (i < am.length && (j === bm.length || lcs[i + 1][j] >= lcs[i][j + 1]))
      ops.push({ k: "-", l: am[i++] });
    else ops.push({ k: "+", l: bm[j++] });
  }
  for (const l of a.slice(a.length - tail)) ops.push({ k: " ", l });
  const changed = ops.flatMap((o, n2) => o.k === " " ? [] : [n2]);
  if (!changed.length) return "";
  const out = [`--- a/${file}`, `+++ b/${file}`];
  let n = 0;
  while (n < changed.length) {
    const from = Math.max(0, changed[n] - 3);
    let to = changed[n] + 3;
    while (n + 1 < changed.length && changed[n + 1] - 3 <= to + 1)
      to = changed[++n] + 3;
    n++;
    to = Math.min(ops.length - 1, to);
    const slice = ops.slice(from, to + 1);
    const before0 = ops.slice(0, from).filter((o) => o.k !== "+").length;
    const after0 = ops.slice(0, from).filter((o) => o.k !== "-").length;
    const aLen = slice.filter((o) => o.k !== "+").length;
    const bLen = slice.filter((o) => o.k !== "-").length;
    out.push(`@@ -${before0 + 1},${aLen} +${after0 + 1},${bLen} @@`);
    for (const o of slice) out.push(`${o.k}${o.l}`);
  }
  return out.join("\n");
}

// cli.ts
async function session(from, o, warn, fn) {
  const root = workspaceRoot(
    statSync4(from).isDirectory() ? from : path8.dirname(from),
    { cwd: process.cwd(), home: os6.homedir(), explicit: o.workspace }
  );
  const s = await start(root, o, warn);
  try {
    const out = await fn(s);
    if (o.verbose) warn(`peak RSS ${s.peak()} (declared ${o.heap})`);
    return out;
  } finally {
    await s.server.stop();
  }
}
function existing(paths2) {
  const out = (paths2.length ? paths2 : ["."]).map(
    (p) => path8.resolve(process.cwd(), p)
  );
  for (const p of out)
    if (!statSync4(p, { throwIfNoEntry: false }))
      throw new Exit(EXIT.unusable, `${p}: no such file or directory`);
  return out;
}
async function team(s, file, warn) {
  let p;
  try {
    p = teamProfile(s.root, await bundleRules(s.server), readOrUndefined, file);
  } catch (e) {
    throw new Exit(EXIT.unusable, e.message);
  }
  for (const x of p.problems)
    warn(
      `${path8.relative(process.cwd(), p.file)}:${x.line + 1}: ${x.severity}: ${x.message}`
    );
  return p;
}
async function status(say) {
  const s = await statusOf();
  say(`JDK: ${s.jdk}`);
  say(`server: ${s.server}`);
  say(`bundle: ${s.bundle}`);
  say(`cap: ${s.cap}`);
  return EXIT.ok;
}
async function inspect(o, say, warn) {
  const paths2 = existing(o.paths);
  return session(paths2[0], o, warn, async (s) => {
    const { server } = s;
    const p = await team(s, o.profile, warn);
    if (o.printProfile) {
      const printed = printedProfile(p);
      if (o.format === "text")
        for (const [code, r] of Object.entries(printed.rules))
          say(
            `${code} ${r.level}${r.origin === "profile" ? ` (profile; default ${r.default})` : ""}${r.why ? ` \u2014 ${r.why}` : ""}`
          );
      else say(JSON.stringify(printed, null, 2));
      return EXIT.ok;
    }
    const files = await sources(server, paths2);
    const found = await findings(server, files, process.cwd(), p.levels);
    const exit = failed(found, o.failOn) ? EXIT.findings : EXIT.ok;
    if (o.format === "json") say(json(found, exit));
    else if (o.format === "sarif") say(sarif(found, PIN.version));
    else if (found.length) say(text(found));
    if (o.verbose) warn(`${files.length} file(s), ${found.length} finding(s)`);
    return exit;
  });
}
function summary(planned) {
  const n = planned.length;
  const e = planned.reduce((a, p) => a + p.edits, 0);
  return n ? `${n} file${n > 1 ? "s" : ""}, ${e} edit${e > 1 ? "s" : ""}` : "0 files";
}
function guardedWrite(planned, root, mtimes) {
  try {
    write(planned, root, mtimes);
  } catch (e) {
    if (e instanceof Refused) throw new Exit(EXIT.refused, e.message);
    throw e;
  }
}
function finishEdit(edit, o, root, mtimes, emptyExit, say, warn) {
  const planned = plan(edit);
  const exit = planned.length ? EXIT.ok : emptyExit;
  if (o.write) guardedWrite(planned, root, mtimes);
  if (o.format === "json")
    say(JSON.stringify({ edit, written: o.write, exit }, null, 2));
  else if (o.write) say(summary(planned));
  else {
    const diff = planned.map(
      (p) => unifiedDiff(path8.relative(process.cwd(), p.file), p.before, p.after)
    ).join("\n");
    if (diff) say(diff);
    warn(`${summary(planned)} (dry run: --write applies it)`);
  }
  return exit;
}
async function fix(o, say, warn) {
  const paths2 = existing(o.paths);
  return session(paths2[0], o, warn, async (s) => {
    const { server, root } = s;
    const p = await team(s, o.profile, warn);
    const files = await sources(server, paths2);
    const mtimes = mtimesOf(files);
    const edit = await fixEdit(server, files, o.rule, p.levels);
    return finishEdit(edit, o, root, mtimes, EXIT.ok, say, warn);
  });
}
function parseTarget(t) {
  const m = /^(.+):(\d+)$/.exec(t);
  if (!m || Number(m[2]) < 1)
    throw new Exit(EXIT.unusable, `${t}: expected <file>:<line>, 1-based`);
  return { file: m[1], line: Number(m[2]) };
}
async function generate(o, say, warn) {
  if (!GENERATE_KINDS.includes(o.what))
    throw new Exit(
      EXIT.unusable,
      `generate ${o.what}: expected ${GENERATE_KINDS.join(", ")}`
    );
  const t = parseTarget(o.target);
  const endLine = o.endLine === void 0 ? void 0 : Number(o.endLine);
  if (endLine !== void 0 && !(Number.isInteger(endLine) && endLine >= 1))
    throw new Exit(EXIT.unusable, "--end-line must be a 1-based line");
  let options;
  try {
    options = o.options === void 0 ? void 0 : JSON.parse(o.options);
  } catch {
    throw new Exit(EXIT.unusable, "--options must be a JSON object");
  }
  if (options !== void 0 && (typeof options !== "object" || Array.isArray(options) || !options))
    throw new Exit(EXIT.unusable, "--options must be a JSON object");
  const [file] = existing([t.file]);
  return session(file, o, warn, async ({ server, root }) => {
    const mtimes = mtimesOf([file]);
    const edit = await generateEdit(server, file, t.line, {
      what: o.what,
      endLine,
      options,
      getterPrefix: o.getterPrefix,
      booleanPrefix: o.booleanPrefix,
      fluentSetters: o.fluent
    });
    if (edit.refused) {
      warn(edit.refused);
      return EXIT.findings;
    }
    if (edit.note) warn(edit.note);
    return finishEdit(
      { changes: edit.changes ?? {} },
      o,
      root,
      mtimes,
      EXIT.findings,
      say,
      warn
    );
  });
}
async function rename(o, say, warn) {
  const ref = symbolRef(o.symbol, process.cwd());
  const [from] = existing([ref.kind === "position" ? ref.path : "."]);
  return session(from, o, warn, async ({ server, root }) => {
    const mtimes = mtimesOf(await sources(server, [root]));
    const edit = await renameEdit(server, ref, o.symbol, o.newName);
    return finishEdit(edit, o, root, mtimes, EXIT.findings, say, warn);
  });
}
function config(rest, print, o, say, warn) {
  if (!print) throw new Exit(EXIT.unusable, "config takes --print");
  const [from] = existing(rest);
  const root = workspaceRoot(
    statSync4(from).isDirectory() ? from : path8.dirname(from),
    { cwd: process.cwd(), home: os6.homedir(), explicit: o.workspace }
  );
  const c = projectConfig(root, readOrUndefined);
  for (const x of c.problems)
    warn(
      `${path8.relative(process.cwd(), c.file)}:${x.line + 1}: ${x.severity}: ${x.message}`
    );
  say(JSON.stringify(c.config, null, 2));
  return EXIT.ok;
}
var SEVERITIES = ["error", "warning", "info", "hint"];
var FORMATS = ["text", "json", "sarif"];
function usage() {
  const doc = (n) => VERBS.find((v) => v.name === `java_${n}`)?.doc ?? "";
  return [
    "batlehub java <verb> [paths\u2026] [options]",
    "",
    `  status                             ${doc("status")}`,
    `  inspect [paths\u2026]                   ${doc("inspect")}`,
    `  fix [paths\u2026]                       ${doc("fix")}`,
    `  generate accessors|getters|setters|builder|withers|surroundWith <file>:<line>   ${doc("generate")}`,
    `  rename <Type#member|file:line:col> <newName>   ${doc("rename")}`,
    "  mcp                                the tools above over stdio (Model Context Protocol), one server for the session",
    "  config --print                     the committed .batlehub/java/project.json resolved, as JSON \u2014 the editor's `project` view; no server",
    "",
    "  --format text|json|sarif   output form (sarif: inspect only); default text",
    "  --fail-on error|warning|info|hint   (inspect) exit 1 at or above; default hint (any finding)",
    "  --rule <code>       (fix) one rule's fixes, e.g. collections/sizeIsZero",
    "  --profile <file>    (inspect, fix) this inspection profile instead of the committed .batlehub/java/inspections.json",
    "  --print-profile     (inspect) print every rule at its effective level instead of inspecting",
    "  --write             (fix, generate, rename) apply the edit; without it, a dry run that prints the diff",
    "  --getter-prefix <p>, --boolean-prefix <p>, --fluent   (generate accessors) default get, is, off",
    "  --end-line <n>      (generate surroundWith) the last line to surround; default <line>",
    `  --options <json>    (generate builder|withers|surroundWith) e.g. '{"fields":["name"],"methodPrefix":"with"}', '{"construct":"tryCatch"}'`,
    "  --workspace <dir>   the workspace root; default the nearest build file or .git",
    "  --timeout <s>       wait for the server's import; default 300",
    "  --heap <size>       the server's declared cap (-Xmx); default 1G",
    "  --verbose           progress and peak RSS on stderr",
    "",
    "exit: 0 done \xB7 1 findings at or above --fail-on, or generate or rename made nothing \xB7 2 no usable JDK, server or cap \xB7 3 a write refused \xB7 4 no ServiceReady in --timeout"
  ].join("\n");
}
async function main(argv, say = (l) => console.log(l), warn = (l) => console.error(l)) {
  try {
    const { values, positionals } = parseArgs({
      args: argv[0] === "java" ? argv.slice(1) : argv,
      allowPositionals: true,
      options: {
        format: { type: "string", default: "text" },
        "fail-on": { type: "string", default: "hint" },
        rule: { type: "string" },
        profile: { type: "string" },
        "print-profile": { type: "boolean", default: false },
        print: { type: "boolean", default: false },
        write: { type: "boolean", default: false },
        "getter-prefix": { type: "string" },
        "boolean-prefix": { type: "string" },
        fluent: { type: "boolean", default: false },
        "end-line": { type: "string" },
        options: { type: "string" },
        workspace: { type: "string" },
        timeout: { type: "string", default: "300" },
        heap: { type: "string", default: "1G" },
        verbose: { type: "boolean", default: false },
        help: { type: "boolean", short: "h", default: false }
      }
    });
    const [verb, ...rest] = positionals;
    if (values.help || !verb) {
      say(usage());
      return values.help ? EXIT.ok : EXIT.unusable;
    }
    if (!FORMATS.includes(values.format))
      throw new Exit(
        EXIT.unusable,
        `--format must be one of ${FORMATS.join(", ")}`
      );
    if (values.format === "sarif" && verb !== "inspect")
      throw new Exit(EXIT.unusable, "--format sarif is for inspect only");
    if (!SEVERITIES.includes(values["fail-on"]))
      throw new Exit(
        EXIT.unusable,
        `--fail-on must be one of ${SEVERITIES.join(", ")}`
      );
    const timeout = Number(values.timeout);
    if (!(timeout > 0))
      throw new Exit(EXIT.unusable, "--timeout must be a number of seconds");
    const common = {
      workspace: values.workspace,
      timeout,
      heap: values.heap,
      verbose: values.verbose
    };
    const format2 = values.format;
    if (verb === "status") return await status(say);
    if (verb === "mcp") return await serve(common, warn);
    if (verb === "config")
      return config(rest, values.print, common, say, warn);
    if (verb === "inspect")
      return await inspect(
        {
          ...common,
          paths: rest,
          format: values.format,
          failOn: values["fail-on"],
          profile: values.profile,
          printProfile: values["print-profile"]
        },
        say,
        warn
      );
    if (verb === "fix")
      return await fix(
        {
          ...common,
          paths: rest,
          rule: values.rule,
          profile: values.profile,
          write: values.write,
          format: format2
        },
        say,
        warn
      );
    if (verb === "generate") {
      if (rest.length !== 2)
        throw new Exit(
          EXIT.unusable,
          `generate takes two arguments: ${GENERATE_KINDS.join("|")} <file>:<line>`
        );
      return await generate(
        {
          ...common,
          what: rest[0],
          target: rest[1],
          endLine: values["end-line"],
          options: values.options,
          getterPrefix: values["getter-prefix"],
          booleanPrefix: values["boolean-prefix"],
          fluent: values.fluent,
          write: values.write,
          format: format2
        },
        say,
        warn
      );
    }
    if (verb === "rename") {
      if (rest.length !== 2)
        throw new Exit(
          EXIT.unusable,
          "rename takes two arguments: <Fully.Qualified.Type#member | file:line:col> <newName>"
        );
      return await rename(
        {
          ...common,
          symbol: rest[0],
          newName: rest[1],
          write: values.write,
          format: format2
        },
        say,
        warn
      );
    }
    throw new Exit(
      EXIT.unusable,
      `unknown verb ${verb} (status, inspect, fix, generate, rename, mcp, config)
${usage()}`
    );
  } catch (e) {
    warn(`batlehub java: ${e.message}`);
    return e instanceof Exit ? e.code : EXIT.unusable;
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL2(process.argv[1]).href)
  process.exitCode = await main(process.argv.slice(2));
export {
  guardedWrite,
  main,
  parseTarget,
  summary,
  usage
};
