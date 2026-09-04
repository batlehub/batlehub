import type { MeResponse } from "@/client/types.gen";

export interface SnippetContext {
  /** Origin of the BatleHub API itself — the main host. */
  base: string;
  registryName: string;
  /**
   * Base URL clients should use for `registryName`: its own hostname-rooted URL
   * (`https://npm.acme.io`) when one is configured, otherwise
   * `${base}/proxy/${registryName}`. Snippets append paths to this and never
   * write a literal `/proxy/` — where the registry lives is not their business.
   */
  registryUrl: string;
  /** {@link registryUrl} for *another* registry, by name. */
  urlFor: (name: string) => string;
  mode: string;
  isAuthenticated: boolean;
  token: string;
  netrcHost: string;
  netrcLogin: string;
  identity: MeResponse | null;
  /** All configured registries keyed by API type — used by composite tabs like mise. */
  selectedNames: Record<string, string>;
  /**
   * Whether the selected registry mints signed download URLs (RFC 0012).
   *
   * Terraform fetches the provider archive with no `Authorization` header and
   * cannot send one, so this page used to tell every Terraform operator to grant
   * anonymous reads across the whole registry. When the registry signs, that
   * advice is wrong — and telling someone to open a registry that is already
   * closed and working is the kind of wrong that gets followed.
   */
  signedDownloads: boolean;
}

export interface SnippetDef {
  key: string;
  label?: string;
  lang: string;
  /** Trusted internal HTML displayed below the code block. */
  note?: string | ((ctx: SnippetContext) => string);
  template: (ctx: SnippetContext) => string;
  showWhen?: (ctx: SnippetContext) => boolean;
}

export interface RegistryTypeDef {
  id: string;
  label: string;
  fileHint?: string;
  /** Trusted internal HTML for the card description. */
  description: string;
  /** API `type` values that activate this tab. Defaults to `[id]`. */
  apiTypes?: string[];
  snippets: SnippetDef[];
}

const isPublishMode = (ctx: SnippetContext) => ctx.mode === "local" || ctx.mode === "hybrid";

/** Returns the user's token when authenticated, or a placeholder for unauthenticated previews. */
const authTokenOrPlaceholder = (ctx: SnippetContext) =>
  ctx.isAuthenticated ? ctx.token : "<your-token>";

/** Builds `registry=<url>` plus an optional `_authToken` line for npm-compatible `.npmrc` files. */
function buildNpmAuthLines(ctx: SnippetContext): string[] {
  const regUrl = `${ctx.registryUrl}/`;
  const lines = [`registry=${regUrl}`];
  if (ctx.isAuthenticated) {
    try {
      const { host, pathname } = new URL(regUrl);
      lines.push(`//${host}${pathname}:_authToken=${ctx.token}`);
    } catch {
      /* skip */
    }
  }
  return lines;
}

/** Embeds `netrcLogin`/`token` as HTTP Basic Auth credentials in `rawUrl`, when authenticated. */
function withCredentials(rawUrl: string, ctx: SnippetContext): string {
  if (!ctx.isAuthenticated) return rawUrl;
  try {
    const u = new URL(rawUrl);
    u.username = ctx.netrcLogin;
    u.password = ctx.token;
    return u.toString();
  } catch {
    return rawUrl;
  }
}

/**
 * `https://host/path` → `https://login:password@host/path`.
 *
 * Unlike {@link withCredentials} this keeps `login`/`password` literal, so a
 * `<your-token>` placeholder stays readable instead of being percent-encoded by
 * the URL parser.
 */
function embedCredentials(rawUrl: string, login: string, password: string): string {
  return rawUrl.replace(/^(https?:\/\/)/i, `$1${login}:${password}@`);
}

/** Hostname of `url`; falls back to the input when it does not parse. */
export function hostOf(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

/**
 * Every host a client may have to authenticate against: the main host, plus the
 * host of each registry that is advertised on one of its own (`public_url`,
 * i.e. host-based routing — RFC 0001).
 *
 * `.netrc` entries are matched by hostname, so a file that lists only the main
 * host sends no credentials to a registry the setup snippets point at by
 * subdomain, and every authenticated install would 401.
 */
export function netrcHostsFor(
  base: string,
  registries: ReadonlyArray<{ public_url?: string | null }>,
): string[] {
  const hosts = [hostOf(base)];
  for (const registry of registries) {
    if (!registry.public_url) continue;
    const host = hostOf(registry.public_url);
    if (!hosts.includes(host)) hosts.push(host);
  }
  return hosts;
}

/**
 * The commented-out `machine / login / password` stanzas for a `~/.netrc`.
 *
 * One stanza per **distinct host**, deduplicated in order. `.netrc` is matched
 * by hostname, so a snippet that rewrites downloads to several registries needs
 * an entry for each of them once they are host-routed onto their own subdomain
 * (RFC 0001) — a single stanza would leave every other host unauthenticated,
 * and the install would 401 on the first package it did not fetch from the one
 * host that was listed. Without host routing the hosts collapse to the main
 * one and this emits exactly the single stanza it always did.
 */
function netrcStanzas(hosts: string[], ctx: SnippetContext): string[] {
  const distinct = [...new Set(hosts.filter(Boolean))];
  if (distinct.length === 0) distinct.push(ctx.netrcHost);
  return distinct.flatMap((host, i) => [
    ...(i > 0 ? [`#`] : []),
    `# machine ${host}`,
    `# login ${ctx.netrcLogin}`,
    `# password ${ctx.token}`,
  ]);
}

export const REGISTRY_TYPE_DEFS: RegistryTypeDef[] = [
  // ── mise (composite: github + npm + cargo) ─────────────────────────────────
  {
    id: "mise",
    label: "mise",
    fileHint: "mise.toml",
    description:
      `URL replacements intercept all HTTP requests made by mise (aqua, ubi, and other backends). ` +
      `Add to your global <code>~/.config/mise/config.toml</code> ` +
      `or a project-local <code>mise.toml</code>.`,
    apiTypes: ["github", "npm", "cargo"],
    snippets: [
      {
        key: "mise",
        lang: "toml",
        template: (ctx) => {
          const { urlFor, isAuthenticated, selectedNames } = ctx;
          const gh = selectedNames["github"];
          const np = selectedNames["npm"];
          const cg = selectedNames["cargo"];
          const lines: string[] = [];
          if (isAuthenticated) {
            lines.push(
              `# Authentication: mise reads ~/.netrc for HTTP Basic Auth`,
              // One stanza per registry host: these three rules can point at
              // three different subdomains.
              ...netrcStanzas(
                [gh, np, cg].filter(Boolean).map((n) => hostOf(urlFor(n))),
                ctx,
              ),
              ``,
            );
          }
          lines.push(`[settings.url_replacements]`);
          if (gh) {
            lines.push(
              ``,
              `# ── GitHub (registry: ${gh}) ─────────────────────────────────────────────────`,
              `# API (release listings, tag metadata, asset lists)`,
              String.raw`"regex:^https://api\\.github\\.com/repos/(.+)" = "${urlFor(gh)}/$1"`,
              ``,
              `# Release asset binaries (browser_download_url from API responses)`,
              String.raw`"regex:^https://github\\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)" = "${urlFor(gh)}/$1/$2/releases/download/$3/$4"`,
              ``,
              `# Source tarballs`,
              String.raw`"regex:^https://github\\.com/([^/]+)/([^/]+)/archive/(?:refs/tags/)?(.+?)\\.tar\\.gz" = "${urlFor(gh)}/$1/$2/tarball/$3"`,
              String.raw`"regex:^https://codeload\\.github\\.com/([^/]+)/([^/]+)/tar\\.gz/(?:refs/tags/)?(.+)" = "${urlFor(gh)}/$1/$2/tarball/$3"`,
              ``,
              `# Zip archives`,
              String.raw`"regex:^https://github\\.com/([^/]+)/([^/]+)/archive/(?:refs/tags/)?(.+?)\\.zip" = "${urlFor(gh)}/$1/$2/zipball/$3"`,
              ``,
              `# Raw files (install scripts, manifests, …)`,
              String.raw`"regex:^https://raw\\.githubusercontent\\.com/([^/]+)/([^/]+)/([^/]+)/(.+)" = "${urlFor(gh)}/$1/$2/raw/$3/$4"`,
            );
          }
          if (np) {
            lines.push(
              ``,
              `# ── npm (registry: ${np}) ───────────────────────────────────────────────────`,
              String.raw`"regex:^https://registry\\.npmjs\\.org/(.+)" = "${urlFor(np)}/$1"`,
            );
          }
          if (cg) {
            lines.push(
              ``,
              `# ── Cargo (registry: ${cg}) — downloads only, use .cargo/config.toml for full support`,
              String.raw`"regex:^https://static\\.crates\\.io/crates/([^/]+)/([^/]+)/.+\\.crate" = "${urlFor(cg)}/$1/$2/download"`,
            );
          }
          return lines.join("\n");
        },
      },
    ],
  },

  // ── npm ────────────────────────────────────────────────────────────────────
  {
    id: "npm",
    label: "npm",
    fileHint: ".npmrc",
    description:
      `Sets the registry for all packages. Place in your project root or ` +
      `<code>~/.npmrc</code> for global use.`,
    snippets: [
      {
        key: "npmrc",
        label: "npm / npm workspaces",
        lang: "ini",
        template: (ctx) => buildNpmAuthLines(ctx).join("\n"),
        note: (ctx) =>
          `To route only a specific scope through the proxy, use ` +
          `<code>@myorg:registry=${ctx.registryUrl}/</code> instead.`,
      },
      {
        key: "yarn",
        label: "Yarn Berry (.yarnrc.yml)",
        lang: "yaml",
        template: (ctx) => {
          const lines = [`npmRegistryServer: "${ctx.registryUrl}/"`];
          if (ctx.isAuthenticated) lines.push(`npmAuthToken: "${ctx.token}"`);
          return lines.join("\n");
        },
      },
      {
        key: "pnpm",
        label: "pnpm (.npmrc)",
        lang: "ini",
        template: (ctx) => buildNpmAuthLines(ctx).join("\n"),
      },
      {
        key: "npm-audit",
        label: "npm audit",
        lang: "bash",
        template: () => [`npm audit`, `npm audit --fix`].join("\n"),
        note:
          `Both audit modes (<code>quick</code> and ` +
          `<code>bulk</code>) are proxied automatically ` +
          `once the registry is configured — no extra setup needed.`,
      },
    ],
  },

  // ── Cargo ──────────────────────────────────────────────────────────────────
  {
    id: "cargo",
    label: "Cargo",
    fileHint: ".cargo/config.toml",
    description:
      `Replaces crates.io as the default source. Cargo fetches the sparse index and ` +
      `<code>.crate</code> files through the proxy. ` +
      `Add to your project's <code>.cargo/config.toml</code> ` +
      `or the global <code>~/.cargo/config.toml</code>.`,
    snippets: [
      {
        key: "cargo",
        lang: "toml",
        template: (ctx) => {
          const lines = [
            `[source.crates-io]`,
            `replace-with = "batlehub"`,
            ``,
            `[source.batlehub]`,
            `registry = "sparse+${ctx.registryUrl}/registry/"`,
          ];
          if (ctx.isAuthenticated) {
            lines.push(``, `[registries.batlehub]`, `token = "${ctx.token}"`);
          }
          return lines.join("\n");
        },
        note:
          `The proxy implements the ` +
          `<a href="https://doc.rust-lang.org/cargo/reference/registry-protocols.html#sparse-protocol">` +
          `sparse registry protocol</a>. ` +
          `Checksums from the index match the cached <code>.crate</code> files, ` +
          `so <code>cargo verify-project</code> continues to work.`,
      },
    ],
  },

  // ── OpenVSX ────────────────────────────────────────────────────────────────
  {
    id: "openvsx",
    label: "OpenVSX",
    fileHint: "OpenVSX",
    description:
      `Proxy VS Code extension downloads from ` +
      `<a href="https://open-vsx.org">open-vsx.org</a>. ` +
      `Extension IDs follow the <code>publisher.name</code> convention.`,
    snippets: [
      {
        key: "openvsx-direct",
        label: "Direct VSIX download URL",
        lang: "text",
        template: (ctx) => `${ctx.registryUrl}/{publisher}.{extension}/{version}/vsix`,
        note:
          `Example: download and install via CLI — ` +
          `<code>` +
          `curl -L {proxy}/ms-python.python/2024.0.0/vsix -o ext.vsix &amp;&amp; code --install-extension ext.vsix` +
          `</code>`,
      },
      {
        key: "openvsx-mise",
        label: "mise — URL replacement to intercept VSIX downloads",
        lang: "toml",
        template: (ctx) => {
          const lines: string[] = [];
          if (ctx.isAuthenticated) {
            lines.push(
              `# Authentication: mise reads ~/.netrc for HTTP Basic Auth`,
              `# machine ${ctx.netrcHost}`,
              `# login ${ctx.netrcLogin}`,
              `# password ${ctx.token}`,
              ``,
            );
          }
          lines.push(
            `[settings.url_replacements]`,
            ``,
            `# ── OpenVSX VSIX downloads ────────────────────────────────────────────────────`,
            `# Intercepts VSIX file downloads from open-vsx.org and routes them through the proxy.`,
            `# The extension ID is joined as publisher.name to match the proxy convention.`,
            String.raw`"regex:^https://open-vsx\\.org/api/([^/]+)/([^/]+)/([^/]+)/file/.+\\.vsix$" = "${ctx.registryUrl}/$1.$2/$3/vsix"`,
          );
          return lines.join("\n");
        },
      },
      {
        key: "openvsx-vscodium",
        label: "VSCodium / Code - OSS extension gallery (product.json)",
        lang: "jsonc",
        template: (ctx) =>
          [
            `// ~/.config/VSCodium/User/product.json  (or merge into existing product.json)`,
            `{`,
            `  "extensionsGallery": {`,
            `    "serviceUrl": "${ctx.registryUrl}/vscode/gallery",`,
            `    "itemUrl": "${ctx.registryUrl}/vscode/item",`,
            `    "resourceUrlTemplate": "${ctx.registryUrl}/vscode/unpkg/{publisher}/{name}/{version}/{path}"`,
            `  }`,
            `}`,
          ].join("\n"),
        note: (ctx) =>
          `The editor sends no credentials to its gallery, and ` +
          `<code>product.json</code> has nowhere to put a token — so this ` +
          `registry needs <code>anonymous = ["releases:read", "source:read"]</code> ` +
          `under <code>[registries.rbac]</code>, or an ingress that authenticates ` +
          `in front of BatleHub. Without it the editor finds no extensions.` +
          (ctx.isAuthenticated
            ? ` VSCodium does not support HTTP Basic Auth in ` +
              `<code>product.json</code>. ` +
              `Add your credentials to <code>~/.netrc</code> — see the <strong>.netrc</strong> tab.`
            : ""),
      },
    ],
  },

  // ── VS Code Marketplace ────────────────────────────────────────────────────
  {
    id: "vscode-marketplace",
    label: "VS Code Marketplace",
    fileHint: "marketplace.visualstudio.com",
    description:
      `Proxy VS Code extension downloads from Microsoft's ` +
      `<a href="https://marketplace.visualstudio.com">Visual Studio Marketplace</a> ` +
      `(marketplace.visualstudio.com). Use this for extensions that are only on the Microsoft marketplace and not mirrored on open-vsx.org. ` +
      `Extension IDs follow the <code>publisher.name</code> convention.`,
    snippets: [
      {
        key: "vscode-marketplace-direct",
        label: "Direct VSIX download URL",
        lang: "text",
        template: (ctx) => `${ctx.registryUrl}/{publisher}.{extension}/{version}/vsix`,
        note:
          `Example: download and install via CLI — ` +
          `<code>` +
          `curl -L {proxy}/ms-python.python/2024.0.0/vsix -o ext.vsix &amp;&amp; code --install-extension ext.vsix` +
          `</code>. Use <code>latest</code> as the version to fetch the newest release.`,
      },
      {
        key: "vscode-marketplace-mise",
        label: "mise — URL replacement to intercept VSIX downloads",
        lang: "toml",
        template: (ctx) => {
          const lines: string[] = [];
          if (ctx.isAuthenticated) {
            lines.push(
              `# Authentication: mise reads ~/.netrc for HTTP Basic Auth`,
              `# machine ${ctx.netrcHost}`,
              `# login ${ctx.netrcLogin}`,
              `# password ${ctx.token}`,
              ``,
            );
          }
          lines.push(
            `[settings.url_replacements]`,
            ``,
            `# ── VS Code Marketplace VSIX downloads ────────────────────────────────────────`,
            `# Intercepts VSIX downloads from marketplace.visualstudio.com and routes them`,
            `# through the proxy. The publisher and extension name are joined as publisher.name.`,
            String.raw`"regex:^https://marketplace\\.visualstudio\\.com/_apis/public/gallery/publishers/([^/]+)/vsextensions/([^/]+)/([^/]+)/vspackage$" = "${ctx.registryUrl}/$1.$2/$3/vsix"`,
          );
          return lines.join("\n");
        },
      },
      {
        key: "vscode-marketplace-vscodium",
        label: "VS Code / VSCodium extension gallery (product.json)",
        lang: "jsonc",
        template: (ctx) =>
          [
            `// ~/.config/VSCodium/User/product.json  (or merge into existing product.json)`,
            `{`,
            `  "extensionsGallery": {`,
            `    "serviceUrl": "${ctx.registryUrl}/vscode/gallery",`,
            `    "itemUrl": "${ctx.registryUrl}/vscode/item",`,
            `    "resourceUrlTemplate": "${ctx.registryUrl}/vscode/unpkg/{publisher}/{name}/{version}/{path}"`,
            `  }`,
            `}`,
          ].join("\n"),
        note: (ctx) =>
          `The editor sends no credentials to its gallery, and ` +
          `<code>product.json</code> has nowhere to put a token — so this ` +
          `registry needs <code>anonymous = ["releases:read", "source:read"]</code> ` +
          `under <code>[registries.rbac]</code>, or an ingress that authenticates ` +
          `in front of BatleHub. Without it the editor finds no extensions.` +
          (ctx.isAuthenticated
            ? ` VSCodium does not support HTTP Basic Auth in ` +
              `<code>product.json</code>. ` +
              `Add your credentials to <code>~/.netrc</code> — see the <strong>.netrc</strong> tab.`
            : ""),
      },
    ],
  },

  // ── JetBrains Marketplace ──────────────────────────────────────────────────
  {
    id: "jetbrains-marketplace",
    label: "JetBrains Marketplace",
    fileHint: "plugins.jetbrains.com",
    description:
      `Proxy the <a href="https://plugins.jetbrains.com">JetBrains Marketplace</a> ` +
      `plugin ecosystem — IDE search, compatible updates, and plugin downloads — with local/hybrid publishing. ` +
      `Distinct from the <code>jetbrains</code> IDE-archive type.`,
    snippets: [
      {
        key: "jbm-host",
        label: "Full replacement — idea.plugins.host custom property",
        lang: "properties",
        template: (ctx) =>
          [
            `# Help → Edit Custom Properties… (idea.properties)`,
            `# Replaces plugins.jetbrains.com entirely for this IDE.`,
            `idea.plugins.host=${ctx.registryUrl}`,
          ].join("\n"),
      },
      {
        key: "jbm-custom-repo",
        label: "Additive — Manage Plugin Repositories URL",
        lang: "text",
        template: (ctx) => `${ctx.registryUrl}/updatePlugins.xml`,
        note:
          `Settings → Plugins → ⚙ → <strong>Manage Plugin Repositories…</strong> → add the URL above. ` +
          `Lists plugins published to this registry alongside the public marketplace.`,
      },
      {
        key: "jbm-download",
        label: "Direct plugin download",
        lang: "bash",
        template: (ctx) =>
          `curl -L "${ctx.registryUrl}/plugin/download?pluginId={xmlId}&version={version}" -o plugin.zip`,
      },
      {
        key: "jbm-publish",
        label: "Publish a plugin (marketplace-compatible upload)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) =>
          [
            `curl -X POST "${ctx.registryUrl}/api/updates/upload" \\`,
            `  -H "Authorization: Bearer ${authTokenOrPlaceholder(ctx)}" \\`,
            `  -F "xmlId={xmlId}" \\`,
            `  -F "channel=" \\`,
            `  -F "file=@my-plugin.zip"`,
          ].join("\n"),
        note:
          `Also works with JetBrains' <code>plugin-repository-rest-client</code> ` +
          `and the Gradle <code>publishPlugin</code> task pointed at this host.`,
      },
    ],
  },

  // ── Go ─────────────────────────────────────────────────────────────────────
  {
    id: "goproxy",
    label: "Go",
    fileHint: "Go",
    description:
      `Set <code>GOPROXY</code> to route ` +
      `Go module downloads through this proxy. Modules are cached after the first download. ` +
      `Append <code>,direct</code> so the ` +
      `go tool falls back to the original source when the proxy returns 404.`,
    snippets: [
      {
        key: "go",
        label: "Environment variables",
        lang: "bash",
        template: (ctx) => {
          const proxyUrl = withCredentials(`${ctx.registryUrl}`, ctx);
          return [
            `# Shell / CI environment — set before running go commands`,
            `export GONOSUMCHECK="*"`,
            `export GONOSUMDB="*"`,
            `export GOPROXY="${proxyUrl},direct"`,
          ].join("\n");
        },
        note:
          `The proxy implements the ` +
          `<a href="https://go.dev/ref/mod#goproxy-protocol">GOPROXY protocol</a>. ` +
          `Module zip archives are cached permanently after first download. ` +
          `<code>@latest</code> and ` +
          `<code>@v/list</code> responses are also cached — ` +
          `clear the proxy storage if you need to pick up newly published versions immediately.`,
      },
      {
        key: "govulncheck",
        label: "govulncheck",
        lang: "bash",
        template: (ctx) => {
          const proxyUrl = withCredentials(`${ctx.registryUrl}`, ctx);
          const lines = [
            `# Point govulncheck at BatleHub (same base URL as GOPROXY)`,
            `export GOVULNDB="${proxyUrl}"`,
            `govulncheck ./...`,
          ];
          if (ctx.isAuthenticated) {
            lines.push(
              ``,
              `# Or put credentials in ~/.netrc so govulncheck picks them up:`,
              `# machine ${ctx.netrcHost}`,
              `# login ${ctx.netrcLogin}`,
              `# password ${ctx.token}`,
            );
          }
          return lines.join("\n");
        },
        note:
          `BatleHub proxies the ` +
          `<a href="https://vuln.go.dev">Go Vulnerability Database</a> ` +
          `(<code>/v1/index.json</code>, ` +
          `<code>/v1/ID/{id}.json</code>, ` +
          `<code>POST /v1/query</code>) so ` +
          `<code>govulncheck</code> works without direct internet access. ` +
          `The upstream vuln DB URL defaults to ` +
          `<code>https://vuln.go.dev</code> and can be ` +
          `overridden per-registry with <code>vuln_db_url</code> in ` +
          `<code>config.toml</code>.`,
      },
    ],
  },

  // ── Maven ──────────────────────────────────────────────────────────────────
  {
    id: "maven",
    label: "Maven",
    fileHint: "Maven",
    description:
      `Route Maven/Gradle dependency downloads through this proxy, or publish private artifacts ` +
      `(<code>mvn deploy</code>) when the registry ` +
      `is configured in <code>Local</code> ` +
      `or <code>Hybrid</code> mode.`,
    snippets: [
      {
        key: "maven-settings",
        label: "~/.m2/settings.xml — proxy all Maven dependencies",
        lang: "xml",
        template: (ctx) => {
          const { registryUrl, registryName: reg, isAuthenticated, token, netrcLogin } = ctx;
          const lines = [`<!-- ~/.m2/settings.xml -->`];
          if (isAuthenticated) {
            lines.push(
              `<settings>`,
              `  <servers>`,
              `    <server>`,
              `      <id>batlehub-${reg}</id>`,
              `      <username>${netrcLogin}</username>`,
              `      <password>${token}</password>`,
              `    </server>`,
              `  </servers>`,
              `  <mirrors>`,
              `    <mirror>`,
              `      <id>batlehub-${reg}</id>`,
              `      <name>BatleHub Maven Proxy</name>`,
              `      <url>${registryUrl}/maven2/</url>`,
              `      <mirrorOf>*</mirrorOf>`,
              `    </mirror>`,
              `  </mirrors>`,
              `</settings>`,
            );
          } else {
            lines.push(
              `<settings>`,
              `  <mirrors>`,
              `    <mirror>`,
              `      <id>batlehub-${reg}</id>`,
              `      <name>BatleHub Maven Proxy</name>`,
              `      <url>${registryUrl}/maven2/</url>`,
              `      <mirrorOf>*</mirrorOf>`,
              `    </mirror>`,
              `  </mirrors>`,
              `</settings>`,
            );
          }
          return lines.join("\n");
        },
      },
      {
        key: "maven-publish",
        label: "pom.xml — publish private artifacts (Local / Hybrid mode)",
        lang: "xml",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl, registryName: reg } = ctx;
          return [
            `<!-- pom.xml — add <distributionManagement> inside <project> -->`,
            `<distributionManagement>`,
            `  <repository>`,
            `    <id>batlehub-${reg}</id>`,
            `    <url>${registryUrl}/maven2/</url>`,
            `  </repository>`,
            `</distributionManagement>`,
            ``,
            `<!-- Then publish with: -->`,
            `<!-- mvn deploy -->`,
          ].join("\n");
        },
        note:
          `The registry must be configured with <code>mode = "local"</code> or ` +
          `<code>mode = "hybrid"</code> in ` +
          `<code>config.toml</code> to accept publishes. ` +
          `The <code>server</code> id in ` +
          `<code>settings.xml</code> must match the ` +
          `<code>repository id</code> in ` +
          `<code>distributionManagement</code>.`,
      },
    ],
  },

  // ── Terraform ──────────────────────────────────────────────────────────────
  {
    id: "terraform",
    label: "Terraform",
    fileHint: "Terraform",
    description:
      `Proxy Terraform provider downloads via network mirror, or publish private modules ` +
      `and providers when the registry is configured in ` +
      `<code>Local</code> ` +
      `or <code>Hybrid</code> mode.`,
    snippets: [
      {
        key: "terraformrc",
        label: "~/.terraformrc — provider network mirror",
        lang: "terraform",
        template: (ctx) => {
          const { registryUrl, isAuthenticated, token } = ctx;
          // Terraform keys its credentials block by hostname, which is the
          // registry's own host when it has one.
          let hostPart = registryUrl;
          try {
            hostPart = new URL(registryUrl).hostname;
          } catch {
            /* keep */
          }
          const lines = [
            `# ~/.terraformrc`,
            `provider_installation {`,
            `  network_mirror {`,
            `    url = "${registryUrl}/"`,
            `  }`,
            `}`,
          ];
          if (isAuthenticated) {
            lines.push(``, `credentials "${hostPart}" {`, `  token = "${token}"`, `}`);
          }
          return lines.join("\n");
        },
        note: (ctx) =>
          `The <code>network_mirror</code> block redirects all ` +
          `provider downloads through this proxy. Providers are cached after first download in ` +
          `Proxy/Hybrid mode, or served entirely locally in Local mode.` +
          (ctx.signedDownloads
            ? ` This registry <strong>signs its download URLs</strong>, so it can ` +
              `stay closed: Terraform fetches the provider archive without ` +
              `credentials, and the signature inside the document it did ` +
              `authenticate stands in for the header. No <code>anonymous</code> ` +
              `grant is needed.`
            : ` Terraform fetches the provider archive <strong>without ` +
              `credentials</strong> and has no way to send any, so this registry ` +
              `needs <code>anonymous = ["releases:read", "source:read"]</code> ` +
              `under <code>[registries.rbac]</code> — or set ` +
              `<code>signed_downloads = true</code> on it and keep it closed.`),
      },
      {
        key: "terraform-module",
        label: "Upload a private module (Local / Hybrid mode)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl } = ctx;
          return [
            `# Upload a module (tar.gz archive)`,
            `curl -X POST \\`,
            `  -H "Authorization: Bearer ${authTokenOrPlaceholder(ctx)}" \\`,
            `  -H "Content-Type: application/gzip" \\`,
            `  --data-binary @module.tar.gz \\`,
            `  "${registryUrl}/v1/modules/namespace/name/provider/1.0.0"`,
            ``,
            `# Download artifact URL returned as X-Terraform-Get header:`,
            `# ${registryUrl}/v1/modules/namespace/name/provider/1.0.0/artifact`,
          ].join("\n");
        },
        note:
          `The response includes an ` +
          `<code>X-Terraform-Get</code> ` +
          `header pointing to the artifact download URL. Modules can also be yanked via the admin API.`,
      },
    ],
  },

  // ── RubyGems ───────────────────────────────────────────────────────────────
  {
    id: "rubygems",
    label: "RubyGems",
    fileHint: "RubyGems",
    description:
      `Mirror rubygems.org through this proxy for Bundler and the gem CLI. ` +
      `Gems are cached after the first download. Publish private gems with ` +
      `<code>gem push</code> when the registry ` +
      `is configured in <code>Local</code> ` +
      `or <code>Hybrid</code> mode.`,
    snippets: [
      {
        key: "gemsrc",
        label: "Bundler mirror / gem CLI source",
        lang: "bash",
        template: (ctx) => {
          const { registryUrl } = ctx;
          const proxyUrl = withCredentials(`${registryUrl}/`, ctx);
          return [
            `# Bundler — mirror rubygems.org through the proxy`,
            `# Run once, or commit to .bundle/config`,
            `bundle config set mirror.https://rubygems.org/ ${proxyUrl}`,
            ``,
            `# gem CLI — replace the default source`,
            `# gem sources --remove https://rubygems.org/`,
            `# gem sources --add ${proxyUrl}`,
          ].join("\n");
        },
        note:
          `The <code>bundle config</code> mirror setting ` +
          `intercepts all rubygems.org requests transparently — no changes to your ` +
          `<code>Gemfile</code> needed.`,
      },
      {
        key: "gem-publish",
        label: "Publish a private gem (Local / Hybrid mode)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl, isAuthenticated, token } = ctx;
          const lines = [
            `# Publish a gem (local / hybrid mode only)`,
            `gem push name-version.gem --host ${registryUrl}`,
          ];
          if (isAuthenticated) {
            lines.push(
              ``,
              `# Credentials: set GEM_HOST_API_KEY or pass --key`,
              `export GEM_HOST_API_KEY="${token}"`,
            );
          }
          return lines.join("\n");
        },
        note:
          `The registry must be configured with <code>mode = "local"</code> or ` +
          `<code>mode = "hybrid"</code> in ` +
          `<code>config.toml</code> to accept publishes.`,
      },
    ],
  },

  // ── Composer ───────────────────────────────────────────────────────────────
  {
    id: "composer",
    label: "Composer",
    fileHint: "Composer",
    description:
      `Proxy PHP Composer package downloads from ` +
      `<a href="https://packagist.org">Packagist</a> ` +
      `or publish private packages via ZIP upload when the registry is configured in ` +
      `<code>Local</code> ` +
      `or <code>Hybrid</code> mode. ` +
      `Authentication uses <code>auth.json</code> ` +
      `(HTTP Basic) rather than a token header — this is a Composer convention.`,
    snippets: [
      {
        key: "composer-json",
        label: "composer.json — add the proxy as a repository",
        lang: "jsonc",
        template: (ctx) => {
          const { registryUrl, isAuthenticated, token } = ctx;
          const lines = [
            `// composer.json — add inside the root object`,
            `"repositories": [`,
            `  {`,
            `    "type": "composer",`,
            `    "url": "${registryUrl}/",`,
          ];
          if (isAuthenticated) {
            lines.push(
              `    "options": {`,
              `      "http": {`,
              `        "header": ["Authorization: Bearer ${token}"]`,
              `      }`,
              `    }`,
            );
          }
          lines.push(`  }`, `]`);
          return lines.join("\n");
        },
      },
      {
        key: "composer-auth",
        label: "auth.json — credentials (never commit this file)",
        lang: "jsonc",
        template: (ctx) => {
          // The credentials are scoped to the host Composer actually talks to,
          // which is the registry's own host when it has one.
          let hostPart = ctx.registryUrl;
          try {
            hostPart = new URL(ctx.registryUrl).hostname;
          } catch {
            /* keep */
          }
          return [
            `// auth.json — project root or ~/.config/composer/auth.json`,
            `// Never commit this file!`,
            `{`,
            `  "http-basic": {`,
            `    "${hostPart}": {`,
            `      "username": "${ctx.isAuthenticated ? (ctx.netrcLogin ?? "user") : "user"}",`,
            `      "password": "${ctx.isAuthenticated ? ctx.token : "<your-token>"}"`,
            `    }`,
            `  }`,
            `}`,
          ].join("\n");
        },
        note:
          `Place <code>auth.json</code> in your project root or ` +
          `<code>~/.config/composer/auth.json</code> for global use. ` +
          `When present, Composer sends HTTP Basic credentials automatically — no ` +
          `<code>options.http.header</code> needed in ` +
          `<code>composer.json</code>.`,
      },
      {
        key: "composer-audit",
        label: "composer audit",
        lang: "bash",
        template: () => `composer audit`,
        note:
          `<code>composer audit</code> queries the ` +
          `<code>/api/security-advisories/</code> endpoint on the ` +
          `configured repository. BatleHub proxies this request to upstream Packagist automatically — ` +
          `no extra configuration needed once the repository is set up.`,
      },
      {
        key: "composer-publish",
        label: "Publish a private package (Local / Hybrid mode)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl } = ctx;
          const tok = authTokenOrPlaceholder(ctx);
          return [
            `# Publish a package (Local / Hybrid mode only)`,
            `# ZIP must contain composer.json with "name" (vendor/pkg) and "version"`,
            `zip -r vendor-pkg-1.0.0.zip vendor-pkg-1.0.0/`,
            ``,
            `curl -X POST \\`,
            `  -H "Authorization: Bearer ${tok}" \\`,
            `  -H "Content-Type: application/zip" \\`,
            `  --data-binary @vendor-pkg-1.0.0.zip \\`,
            `  "${registryUrl}/api/upload"`,
            ``,
            `# Yank a version`,
            `curl -X DELETE \\`,
            `  -H "Authorization: Bearer ${tok}" \\`,
            `  "${registryUrl}/api/packages/vendor/pkg/versions/1.0.0"`,
          ].join("\n");
        },
        note:
          `The ZIP must contain a valid <code>composer.json</code> ` +
          `at its root or inside a single top-level directory (GitHub archive layout). ` +
          `The <code>name</code> field must use the ` +
          `<code>vendor/package</code> format and the ` +
          `<code>version</code> field determines the published version.`,
      },
    ],
  },

  // ── PyPI ───────────────────────────────────────────────────────────────────
  {
    id: "pypi",
    label: "PyPI",
    fileHint: "PyPI",
    description:
      `Proxy <a href="https://pypi.org">PyPI</a> ` +
      `through BatleHub for pip, uv, Poetry, and other Python package managers. ` +
      `Wheels and source distributions are cached after the first download. ` +
      `Publish private packages with <code>twine upload</code> ` +
      `when the registry is configured in <code>Local</code> ` +
      `or <code>Hybrid</code> mode.`,
    snippets: [
      {
        key: "pip-conf",
        label: "~/.pip/pip.conf — global pip configuration",
        lang: "ini",
        template: (ctx) => {
          const { registryUrl, isAuthenticated, token, netrcLogin } = ctx;
          const simpleUrl = `${registryUrl}/simple/`;
          const lines = [
            `# ~/.pip/pip.conf  (Linux/macOS)`,
            String.raw`# %APPDATA%\pip\pip.ini  (Windows)`,
            `[global]`,
            `index-url = ${simpleUrl}`,
          ];
          if (isAuthenticated) {
            lines.push(
              ``,
              `# Credentials: use ~/.netrc (recommended) or embed in the URL:`,
              `# index-url = ${embedCredentials(simpleUrl, netrcLogin, token)}`,
            );
          }
          return lines.join("\n");
        },
        note:
          `Alternatively, pass <code>--index-url</code> ` +
          `on the command line or set the ` +
          `<code>PIP_INDEX_URL</code> environment variable.`,
      },
      {
        key: "uv-index",
        label: "pyproject.toml — uv index configuration",
        lang: "toml",
        template: (ctx) => {
          const { registryUrl, isAuthenticated, token, netrcLogin, netrcHost } = ctx;
          const lines = [
            `# pyproject.toml — add inside [tool.uv]`,
            `[[tool.uv.index]]`,
            `name = "batlehub"`,
            `url = "${registryUrl}/simple/"`,
            `default = true`,
          ];
          if (isAuthenticated) {
            lines.push(
              ``,
              `# Credentials: uv reads ~/.netrc automatically`,
              `# machine ${netrcHost}`,
              `# login ${netrcLogin}`,
              `# password ${token}`,
            );
          }
          return lines.join("\n");
        },
      },
      {
        key: "twine-publish",
        label: "Publish a private package (Local / Hybrid mode)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl } = ctx;
          const tok = authTokenOrPlaceholder(ctx);
          return [
            `# Publish a wheel or sdist (Local / Hybrid mode only)`,
            `# Build first: python -m build`,
            ``,
            `twine upload \\`,
            `  --repository-url ${registryUrl}/legacy/ \\`,
            `  --username __token__ \\`,
            `  --password ${tok} \\`,
            `  dist/*`,
            ``,
            `# Or via ~/.pypirc:`,
            `# [batlehub]`,
            `# repository = ${registryUrl}/legacy/`,
            `# username = __token__`,
            `# password = ${tok}`,
          ].join("\n");
        },
        note:
          `The registry must be configured with ` +
          `<code>mode = "local"</code> or ` +
          `<code>mode = "hybrid"</code>. ` +
          `The filename, name, and version are derived from the wheel or sdist metadata automatically.`,
      },
    ],
  },

  // ── Conda ──────────────────────────────────────────────────────────────────
  {
    id: "conda",
    label: "Conda",
    fileHint: "Conda",
    description:
      `Proxy conda channels (conda-forge, defaults, or custom) through BatleHub. ` +
      `<code>repodata.json</code> and package files ` +
      `are cached after the first request. Publish private conda packages in ` +
      `<code>Local</code> ` +
      `or <code>Hybrid</code> mode — packages ` +
      `appear in the channel's <code>repodata.json</code> automatically.`,
    snippets: [
      {
        key: "condarc",
        label: "~/.condarc — point conda at the proxy",
        lang: "yaml",
        template: (ctx) => {
          const { registryUrl, isAuthenticated, token, netrcLogin, netrcHost } = ctx;
          const lines = [
            `# ~/.condarc  (or .condarc in the project root)`,
            `channels:`,
            `  - ${registryUrl}`,
            `  - nodefaults`,
          ];
          if (isAuthenticated) {
            lines.push(
              ``,
              `# Credentials: conda reads ~/.netrc automatically`,
              `# machine ${netrcHost}`,
              `# login ${netrcLogin}`,
              `# password ${token}`,
            );
          }
          return lines.join("\n");
        },
        note:
          `Credentials are read automatically from ` +
          `<code>~/.netrc</code>. ` +
          `Set <code>ssl_verify: false</code> ` +
          `only for development with self-signed certificates.`,
      },
      {
        key: "conda-env",
        label: "environment.yml — reproducible environment",
        lang: "yaml",
        template: (ctx) =>
          [
            `# environment.yml`,
            `channels:`,
            `  - ${ctx.registryUrl}`,
            `  - nodefaults`,
            `dependencies:`,
            `  - python=3.11`,
            `  - numpy`,
          ].join("\n"),
      },
      {
        key: "conda-publish",
        label: "Publish a private conda package (Local / Hybrid mode)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl } = ctx;
          const tok = authTokenOrPlaceholder(ctx);
          return [
            `# Publish a conda package (Local / Hybrid mode only)`,
            `# Build first: conda build my-recipe/`,
            ``,
            `curl -X POST \\`,
            `  -H "Authorization: Bearer ${tok}" \\`,
            `  -H "Content-Type: application/octet-stream" \\`,
            `  --data-binary @my-pkg-1.0.0-py311h0_0.tar.bz2 \\`,
            `  "${registryUrl}/linux-64/"`,
            ``,
            `# Verify: repodata.json will list your package`,
            `curl -s "${registryUrl}/linux-64/repodata.json" | \\`,
            `  python3 -c "import sys,json; d=json.load(sys.stdin); print(list(d['packages'].keys())[:5])"`,
          ].join("\n");
        },
        note:
          `Both <code>.tar.bz2</code> and ` +
          `<code>.conda</code> package formats are supported. ` +
          `The name, version, and build string are extracted from ` +
          `<code>info/index.json</code> inside the archive.`,
      },
    ],
  },

  // ── NuGet ──────────────────────────────────────────────────────────────────
  {
    id: "nuget",
    label: "NuGet",
    description:
      `Configure <code>dotnet</code> or ` +
      `<code>nuget.config</code> to use this proxy as a ` +
      `NuGet package source. Compatible with ` +
      `<code>dotnet add package</code>, ` +
      `<code>dotnet restore</code>, and ` +
      `<code>dotnet nuget push</code>.`,
    snippets: [
      {
        key: "nuget-source",
        label: "Add NuGet source (CLI)",
        lang: "bash",
        template: (ctx) => {
          const { registryUrl, registryName: reg, isAuthenticated } = ctx;
          const tok = authTokenOrPlaceholder(ctx);
          const lines = [
            `# Register the proxy as a NuGet source`,
            `dotnet nuget add source \\`,
            `  "${registryUrl}/nuget/v3/index.json" \\`,
            `  --name ${reg}`,
          ];
          if (isAuthenticated) {
            lines.push(
              ``,
              `# Or with authentication`,
              `dotnet nuget add source \\`,
              `  "${registryUrl}/nuget/v3/index.json" \\`,
              `  --name ${reg} \\`,
              `  --username __token__ --password ${tok}`,
            );
          }
          return lines.join("\n");
        },
      },
      {
        key: "nuget-config",
        label: "nuget.config (XML)",
        lang: "xml",
        template: (ctx) =>
          [
            `<?xml version="1.0" encoding="utf-8"?>`,
            `<configuration>`,
            `  <packageSources>`,
            `    <add key="${ctx.registryName}" value="${ctx.registryUrl}/nuget/v3/index.json" />`,
            `  </packageSources>`,
            `</configuration>`,
          ].join("\n"),
        note:
          `Place <code>nuget.config</code> in your project root ` +
          `or user profile (<code>~/.nuget/NuGet/NuGet.Config</code>).`,
      },
      {
        key: "nuget-vulnerable",
        label: "dotnet list package --vulnerable",
        lang: "bash",
        template: () =>
          [
            `# Check all packages in the solution for known vulnerabilities`,
            `dotnet list package --vulnerable`,
            ``,
            `# Include transitive dependencies`,
            `dotnet list package --vulnerable --include-transitive`,
          ].join("\n"),
        note:
          `BatleHub exposes a ` +
          `<code>VulnerabilitiesUrl/6.7.0</code> resource in the ` +
          `v3 service index, so <code>dotnet list package --vulnerable</code> ` +
          `discovers and queries the vulnerability catalogue automatically through the proxy. ` +
          `No extra configuration needed.`,
      },
      {
        key: "nuget-publish",
        label: "Publish a package (Local / Hybrid mode only)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) => {
          const { registryUrl } = ctx;
          const tok = authTokenOrPlaceholder(ctx);
          return [
            `# Publish a .nupkg (Local / Hybrid mode only)`,
            `dotnet nuget push MyLib.1.0.0.nupkg \\`,
            `  --api-key ${tok} \\`,
            `  --source "${registryUrl}/nuget/v3/index.json"`,
            ``,
            `# Yank a version`,
            `curl -X DELETE \\`,
            `  -H "Authorization: Bearer ${tok}" \\`,
            `  "${registryUrl}/nuget/v2/package/mylib/1.0.0"`,
          ].join("\n");
        },
        note:
          `The registry accepts <code>.nupkg</code> files ` +
          `via <code>multipart/form-data</code> ` +
          `(as sent by <code>dotnet nuget push</code>). ` +
          `The <code>.nuspec</code> is automatically ` +
          `extracted from the archive to record package metadata.`,
      },
    ],
  },

  // ── Forgejo / Gitea (releases) ───────────────────────────────────────────────
  {
    id: "forgejo",
    label: "Forgejo",
    fileHint: "Releases",
    description:
      `Proxy release assets, source archives, and raw files from a ` +
      `<a href="https://forgejo.org">Forgejo</a> ` +
      `or Gitea instance. Forgejo registries reuse the GitHub-style URL scheme.`,
    snippets: [
      {
        key: "forgejo-curl",
        label: "Download release assets & archives",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}`;
          const auth = ctx.isAuthenticated ? ` \\\n  -H "Authorization: Bearer ${ctx.token}"` : "";
          return [
            `# List releases for owner/repo`,
            `curl${auth} ${reg}/<owner>/<repo>/releases`,
            ``,
            `# Release metadata by tag`,
            `curl${auth} ${reg}/<owner>/<repo>/releases/tags/v1.0.0`,
            ``,
            `# Download a release asset by filename`,
            `curl -L -O${auth} ${reg}/<owner>/<repo>/releases/download/v1.0.0/app.tar.gz`,
            ``,
            `# Source tarball / zip for a tag, branch, or commit`,
            `curl -L -O${auth} ${reg}/<owner>/<repo>/tarball/v1.0.0`,
            `curl -L -O${auth} ${reg}/<owner>/<repo>/zipball/v1.0.0`,
            ``,
            `# Raw file`,
            `curl -L${auth} ${reg}/<owner>/<repo>/raw/main/README.md`,
            ``,
            `# Package registry passthrough (generic packages)`,
            `curl -L -O${auth} ${reg}/api/packages/<owner>/generic/<name>/<version>/<file>`,
          ].join("\n");
        },
        note:
          `Configure the upstream instance URL (e.g. ` +
          `<code>https://codeberg.org</code>) as the ` +
          `registry's upstream. The same adapter serves both Forgejo and Gitea. For ecosystem ` +
          `package registries (npm, Maven, PyPI, …) use the matching typed adapter pointed at the ` +
          `<code>/api/packages/{owner}/{type}</code> endpoint.`,
      },
    ],
  },

  // ── GitLab (releases) ────────────────────────────────────────────────────────
  {
    id: "gitlab",
    label: "GitLab",
    fileHint: "Releases",
    description:
      `Proxy GitLab releases, release link assets, and source archives. Project paths ` +
      `may include nested groups; the release sub-path is separated by ` +
      `<code>/-/</code>, mirroring GitLab's own URLs.`,
    snippets: [
      {
        key: "gitlab-curl",
        label: "Download releases & archives",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}`;
          const auth = ctx.isAuthenticated ? ` \\\n  -H "Authorization: Bearer ${ctx.token}"` : "";
          return [
            `# List releases for a project (nested groups allowed)`,
            `curl${auth} ${reg}/<group>/<project>/-/releases`,
            ``,
            `# Release metadata by tag`,
            `curl${auth} ${reg}/<group>/<project>/-/releases/v1.0.0`,
            ``,
            `# Download a release link asset (matched by link name)`,
            `curl -L -O${auth} ${reg}/<group>/<project>/-/releases/v1.0.0/downloads/app.bin`,
            ``,
            `# Source archive for a tag (format inferred from the extension)`,
            `curl -L -O${auth} ${reg}/<group>/<project>/-/archive/v1.0.0/source.tar.gz`,
            ``,
            `# Raw file from the repository`,
            `curl -L${auth} ${reg}/<group>/<project>/-/raw/main/README.md`,
            ``,
            `# Package registry passthrough (generic packages)`,
            `curl -L -O${auth} ${reg}/api/v4/projects/<id>/packages/generic/<name>/<version>/<file>`,
          ].join("\n");
        },
        note:
          `GitLab personal access tokens use the ` +
          `<code>PRIVATE-TOKEN</code> header — configure ` +
          `it as a custom upstream auth header on the registry. Set the upstream URL to your ` +
          `instance root (e.g. <code>https://gitlab.com</code>). ` +
          `For ecosystem package registries (npm, Maven, PyPI, …) use the matching typed adapter.`,
      },
    ],
  },

  // ── Debian APT (deb) ─────────────────────────────────────────────────────────
  {
    id: "deb",
    label: "Debian (APT)",
    fileHint: "/etc/apt/sources.list.d/",
    description:
      `Proxy and host Debian/Ubuntu APT repositories. In local/hybrid mode, publish ` +
      `<code>.deb</code> packages and BatleHub ` +
      `regenerates the <code>Packages</code>/` +
      `<code>Release</code> indexes (Ed25519 ` +
      `OpenPGP-signed when a key is configured).`,
    snippets: [
      {
        key: "apt-source",
        label: "APT source",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/deb`;
          if (isPublishMode(ctx)) {
            // Local/hybrid: BatleHub signs Release with its own key (served at /key.gpg).
            return [
              `# Import BatleHub's repository signing key`,
              `curl -fsSL ${reg}/key.gpg | sudo tee /usr/share/keyrings/${ctx.registryName}.asc >/dev/null`,
              ``,
              `# Add the source (adjust suite/component to your repo)`,
              `echo "deb [signed-by=/usr/share/keyrings/${ctx.registryName}.asc] ${reg} stable main" \\`,
              `  | sudo tee /etc/apt/sources.list.d/${ctx.registryName}.list`,
              ``,
              `sudo apt update`,
            ].join("\n");
          }
          // Proxy: the upstream repo's own (relayed) signature is what apt verifies,
          // so the client must trust the UPSTREAM's archive key. For official
          // Debian/Ubuntu mirrors that key is already installed.
          return [
            `# Proxy mode relays the upstream repo's Release/InRelease and its signature.`,
            `# Verify with the UPSTREAM's archive key. Official Debian/Ubuntu mirrors`,
            `# ship it already (packages: debian-archive-keyring / ubuntu-keyring):`,
            `KEYRING=/usr/share/keyrings/debian-archive-keyring.gpg   # ubuntu: ubuntu-archive-keyring.gpg`,
            `echo "deb [signed-by=$KEYRING] ${reg} stable main" \\`,
            `  | sudo tee /etc/apt/sources.list.d/${ctx.registryName}.list`,
            ``,
            `# For a third-party upstream, import ITS key instead:`,
            `#   curl -fsSL <upstream-key-url> | gpg --dearmor \\`,
            `#     | sudo tee /usr/share/keyrings/${ctx.registryName}.gpg >/dev/null`,
            ``,
            `sudo apt update`,
          ].join("\n");
        },
        note: (ctx) =>
          isPublishMode(ctx)
            ? `For an unsigned local repository (no <code>repo_signing</code> key), replace ` +
              `<code>[signed-by=…]</code> with <code>[trusted=yes]</code>.`
            : `Proxy registries relay the upstream's signature, so apt verifies against the <strong>upstream's</strong> key — ` +
              `<code>${ctx.registryUrl}/deb/key.gpg</code> is not served (it is a local/hybrid signing artifact). ` +
              `A <code>NO_PUBKEY</code> error means that key isn't in the keyring named by ` +
              `<code>signed-by</code> — install <code>debian-archive-keyring</code>/` +
              `<code>ubuntu-keyring</code> (or import the upstream key), or use <code>[trusted=yes]</code>.`,
      },
      {
        key: "apt-auth",
        label: "Private registry auth",
        lang: "bash",
        template: (ctx) => {
          const { registryUrl, netrcHost, netrcLogin, token, registryName: reg } = ctx;
          const login = ctx.isAuthenticated ? netrcLogin : "<your-username>";
          const password = ctx.isAuthenticated ? token : "<your-token>";
          const debUrl = `${registryUrl}/deb`;
          return [
            `# APT reads credentials from /etc/apt/auth.conf.d/ (Debian 9+ / Ubuntu 19.04+).`,
            `# The sources.list entry is unchanged — credentials are kept in a separate file.`,
            ``,
            `sudo tee /etc/apt/auth.conf.d/${reg}.conf > /dev/null <<'EOF'`,
            `machine ${netrcHost}`,
            `login ${login}`,
            `password ${password}`,
            `EOF`,
            `sudo chmod 0600 /etc/apt/auth.conf.d/${reg}.conf`,
            ``,
            `sudo apt update`,
            ``,
            `# Alternative: embed credentials directly in the source URL`,
            `# (less secure — credentials visible in sources.list)`,
            `# echo "deb [signed-by=...] ${embedCredentials(debUrl, login, password)} stable main" \\`,
            `#   | sudo tee /etc/apt/sources.list.d/${reg}.list`,
          ].join("\n");
        },
        note:
          `<code>/etc/apt/auth.conf.d/</code> is the recommended approach — credentials ` +
          `are stored separately from the source list and are never shown in ` +
          `<code>apt-cache policy</code> output. ` +
          `On older systems without <code>auth.conf.d</code> support ` +
          `(pre-Debian 9 / Ubuntu 18.10), use <code>/etc/apt/auth.conf</code> ` +
          `with the same <code>machine / login / password</code> stanza.`,
      },
      {
        key: "apt-publish",
        label: "Publish a .deb (local/hybrid)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) =>
          [
            `# Upload to pool/{distribution}/{component}`,
            `curl -X PUT \\`,
            `  -H "Authorization: Bearer ${authTokenOrPlaceholder(ctx)}" \\`,
            `  --data-binary @hello_1.0_amd64.deb \\`,
            `  ${ctx.registryUrl}/deb/pool/stable/main/upload`,
          ].join("\n"),
      },
    ],
  },

  // ── RPM / YUM (rpm) ──────────────────────────────────────────────────────────
  {
    id: "rpm",
    label: "RPM (YUM/DNF)",
    fileHint: "/etc/yum.repos.d/",
    description:
      `Proxy and host RPM repositories for DNF/YUM. In local/hybrid mode, publish ` +
      `<code>.rpm</code> packages and BatleHub ` +
      `regenerates <code>repodata/</code> ` +
      `(Ed25519 OpenPGP-signed <code>repomd.xml.asc</code> ` +
      `when a key is configured).`,
    snippets: [
      {
        key: "dnf-repo",
        label: ".repo file",
        lang: "ini",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/rpm`;
          const { isAuthenticated, token, netrcLogin } = ctx;
          const login = isAuthenticated ? netrcLogin : "<your-username>";
          const password = isAuthenticated ? token : "<your-token>";
          const lines = [
            `[${ctx.registryName}]`,
            `name=${ctx.registryName}`,
            `baseurl=${reg}`,
            `enabled=1`,
          ];
          if (isAuthenticated) {
            lines.push(`username=${login}`, `password=${password}`);
          }
          if (isPublishMode(ctx)) {
            // Local/hybrid: repomd.xml.asc is signed by BatleHub's key (served at the URL below).
            lines.push(`repo_gpgcheck=1`, `gpgcheck=0`, `gpgkey=${reg}/repodata/repomd.xml.key`);
          } else {
            // Proxy: metadata (and any repomd.xml.asc) is relayed from upstream; there is
            // no BatleHub key. Verify against the upstream's key or disable the repo check.
            lines.push(`repo_gpgcheck=0`, `gpgcheck=0`, `# gpgkey=<upstream-project-gpg-key-url>`);
          }
          return lines.join("\n");
        },
        note: (ctx) =>
          isPublishMode(ctx)
            ? `Save to <code>/etc/yum.repos.d/${"{name}"}.repo</code>. ` +
              `For an unsigned local repo (no <code>repo_signing</code> key), set ` +
              `<code>repo_gpgcheck=0</code> and omit <code>gpgkey</code>.`
            : `Proxy registries have no BatleHub key — <code>repodata/repomd.xml.key</code> ` +
              `is only served for local/hybrid registries with a <code>repo_signing</code> key. ` +
              `To verify, point <code>gpgkey</code> at the upstream project's key and set ` +
              `<code>repo_gpgcheck=1</code>. ` +
              `Credentials in <code>.repo</code> files are read by DNF/YUM; ` +
              `alternatively, use a <code>~/.netrc</code> entry for the proxy host.`,
      },
      {
        key: "rpm-publish",
        label: "Publish a .rpm (local/hybrid)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) =>
          [
            `curl -X PUT \\`,
            `  -H "Authorization: Bearer ${authTokenOrPlaceholder(ctx)}" \\`,
            `  --data-binary @hello-1.0-1.x86_64.rpm \\`,
            `  ${ctx.registryUrl}/rpm/upload`,
          ].join("\n"),
      },
    ],
  },
  // ── Arch Linux (pacman) ──────────────────────────────────────────────────────
  {
    id: "pacman",
    label: "Arch Linux (pacman)",
    fileHint: "/etc/pacman.conf",
    description:
      `Proxy and host Arch Linux pacman repositories. In local/hybrid mode, publish ` +
      `<code>.pkg.tar.zst</code> packages and BatleHub ` +
      `regenerates the repository database (<code>&lt;repo&gt;.db</code>), ` +
      `signing it and each package (Ed25519 OpenPGP) when a ` +
      `<code>repo_signing</code> key is configured.`,
    snippets: [
      {
        key: "pacman-conf",
        label: "pacman.conf repository",
        lang: "ini",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/pacman`;
          const lines = [
            `[${ctx.registryName}]`,
            // $arch is expanded by pacman (e.g. x86_64); $repo resolves to the section name.
            `Server = ${reg}/$arch`,
          ];
          if (isPublishMode(ctx)) {
            lines.push(
              `# Signed local repo: import the key (snippet below), then:`,
              `SigLevel = Required DatabaseOptional`,
              `# Unsigned local repo (no repo_signing key): use instead:`,
              `# SigLevel = Optional TrustAll`,
            );
          } else {
            lines.push(
              `SigLevel = Required DatabaseOptional  # verify with the upstream's keyring`,
            );
          }
          return lines.join("\n");
        },
        note: (ctx) =>
          `Add the block to <code>/etc/pacman.conf</code>. ` +
          `The DB is served as <code>$arch/${ctx.registryName}.db</code>, ` +
          `so the section name must match the registry name.`,
      },
      {
        key: "pacman-key",
        label: "Import the signing key (signed local/hybrid)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) =>
          [
            `# Import BatleHub's repo key and locally trust it`,
            `curl -fsSL ${ctx.registryUrl}/pacman/key.gpg \\`,
            `  | sudo pacman-key --add -`,
            `# Find the imported key id, then locally sign it:`,
            `sudo pacman-key --lsign-key <KEYID>`,
          ].join("\n"),
        note:
          `Only served for local/hybrid registries with a ` +
          `<code>repo_signing</code> key. ` +
          `<code>pacman-key --add</code> prints the key id to lsign.`,
      },
      {
        key: "pacman-publish",
        label: "Publish a package (local/hybrid)",
        lang: "bash",
        showWhen: isPublishMode,
        template: (ctx) =>
          [
            `curl -X PUT \\`,
            `  -H "Authorization: Bearer ${authTokenOrPlaceholder(ctx)}" \\`,
            `  --data-binary @hello-1.0-1-x86_64.pkg.tar.zst \\`,
            `  ${ctx.registryUrl}/pacman/upload`,
          ].join("\n"),
        note:
          `The package name, version, and architecture are read from the archive's ` +
          `<code>.PKGINFO</code>; the stored filename is derived from them.`,
      },
    ],
  },
  // ── JetBrains IDE archives (proxy-only cache) ──────────────────────────────
  {
    id: "jetbrains",
    label: "JetBrains IDE",
    fileHint: "download.jetbrains.com",
    description:
      `Cache JetBrains IDE installer archives (proxy-only). The first download is ` +
      `streamed from <code>download.jetbrains.com</code> ` +
      `and cached; later downloads of the same file are served locally. ` +
      `IDE archives are large (~1–1.7 GB), so raise ` +
      `<code>[limits] max_artifact_size_bytes</code> ` +
      `(e.g. 2 GiB) or they will be rejected.`,
    snippets: [
      {
        key: "jetbrains-curl",
        label: "Download an IDE archive",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/jetbrains`;
          const auth = ctx.isAuthenticated ? ` \\\n  -H "Authorization: Bearer ${ctx.token}"` : "";
          return [
            `# The path after /jetbrains/ maps to download.jetbrains.com/<path>`,
            `curl -fL -o idea.tar.gz${auth} \\`,
            `  ${reg}/idea/idea-2026.1.3.tar.gz`,
          ].join("\n");
        },
        note:
          `Use the same path as the upstream URL: ` +
          `<code>download.jetbrains.com/idea/idea-2026.1.3.tar.gz</code> → ` +
          `<code>/proxy/{name}/jetbrains/idea/idea-2026.1.3.tar.gz</code>. ` +
          `<code>download.jetbrains.com</code> redirects to a CDN ` +
          `(<code>download-cdn.jetbrains.com</code>) — the redirect is followed ` +
          `automatically, so always use the canonical path, not the CDN host. Use real archive names: ` +
          `<code>idea-…</code> (unified installer, 2025.3+); ` +
          `the legacy <code>ideaIU-…</code>/` +
          `<code>ideaIC-…</code> names only exist for releases ≤ 2025.2.`,
      },
      {
        key: "jetbrains-config",
        label: "Server config",
        lang: "toml",
        template: (ctx) =>
          [
            `[limits]`,
            `max_artifact_size_bytes = 2147483648  # 2 GiB — IDE archives are large`,
            ``,
            `[[registries]]`,
            `name = "${ctx.registryName}"`,
            `type = "jetbrains"`,
            `mode = "proxy"            # upstream defaults to https://download.jetbrains.com`,
            ``,
            `[registries.rbac]`,
            `anonymous = ["releases:read"]`,
          ].join("\n"),
        note:
          `Override <code>upstreams</code> to cache another host ` +
          `(e.g. <code>https://plugins.jetbrains.com</code>).`,
      },
    ],
  },
  // ── Generic file mirror (proxy-only, path-addressed) ───────────────────────
  {
    id: "generic",
    label: "Generic mirror",
    fileHint: "any HTTP file tree",
    description:
      `Mirror any plain HTTP file tree — for upstreams with no package protocol at all: ` +
      `toolchain tarballs (Node, rustup, the Go toolchain) and single-binary vendor CDNs ` +
      `(Helm, MinIO, SonarScanner). Proxy-only: there is no publish or index model. ` +
      `Both <code>upstreams</code> and ` +
      `<code>path_allow</code> are required — ` +
      `without the allowlist a mirror of a shared host would relay every unrelated path on it.`,
    snippets: [
      {
        key: "generic-curl",
        label: "Download a file",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/generic`;
          const auth = ctx.isAuthenticated ? ` \\\n  -H "Authorization: Bearer ${ctx.token}"` : "";
          return [
            `# The path after /generic/ maps 1:1 onto the configured upstream`,
            `curl -fL -o node.tar.gz${auth} \\`,
            `  ${reg}/v24.18.0/node-v24.18.0-linux-x64.tar.gz`,
          ].join("\n");
        },
        note:
          `A path outside the registry's ` +
          `<code>path_allow</code> allowlist returns ` +
          `<code>403</code>, not 404 — that is the allowlist ` +
          `rejecting it locally, before any upstream request is made.`,
      },
      {
        key: "generic-config",
        label: "Server config",
        lang: "toml",
        template: (ctx) =>
          [
            `[limits]`,
            `max_artifact_size_bytes = 2147483648  # 2 GiB — toolchain archives are large`,
            ``,
            `[[registries]]`,
            `name       = "${ctx.registryName}"`,
            `type       = "generic"`,
            `mode       = "proxy"`,
            `upstreams  = ["https://nodejs.org/dist"]   # required — no default exists`,
            `path_allow = ["v*/**"]                     # required — use ["**"] to allow all`,
            ``,
            `[registries.rbac]`,
            `anonymous = ["releases:read"]`,
            ``,
            `# Pre-warm specific paths on startup (path-addressed registries use`,
            `# warm_paths, not warm_packages).`,
            `[registries.cache]`,
            `warm_paths = ["v24.18.0/node-v24.18.0-linux-x64.tar.gz"]`,
          ].join("\n"),
        note:
          `Run <code>batlehub-cli registry suggest</code> in a project ` +
          `to generate these blocks from its <code>mise.toml</code> / ` +
          `<code>mise.lock</code> and manifests.`,
      },
      {
        key: "generic-presets",
        label: "Toolchain presets",
        lang: "toml",
        template: (ctx) =>
          [
            `# One [[registries]] entry per upstream host. Names below are examples —`,
            `# the client env vars in the next tab must match whatever you pick.`,
            ``,
            `[[registries]]`,
            `type       = "generic"`,
            `name       = "node-dist"`,
            `upstreams  = ["https://nodejs.org/dist"]`,
            `path_allow = ["v*/**"]                      # mise also fetches the source tarball`,
            ``,
            `[[registries]]`,
            `type       = "generic"`,
            `name       = "rust-dist"`,
            `upstreams  = ["https://static.rust-lang.org"]`,
            `path_allow = ["dist/**", "rustup/**"]`,
            ``,
            `[[registries]]`,
            `type       = "generic"`,
            `name       = "go-dl"                        # the Go *toolchain* tarballs;`,
            `upstreams  = ["https://dl.google.com/go"]    # Go *modules* use type = "goproxy"`,
            `path_allow = ["go*.linux-amd64.tar.gz"]`,
            ``,
            `[[registries]]`,
            `type       = "generic"`,
            `name       = "helm-bin"`,
            `upstreams  = ["https://get.helm.sh"]`,
            `path_allow = ["helm-v*-linux-amd64.tar.gz"]`,
            ``,
            `[[registries]]`,
            `type       = "generic"`,
            `name       = "minio-dl"`,
            `upstreams  = ["https://dl.min.io"]`,
            `path_allow = ["client/mc/release/linux-amd64/**"]`,
            ``,
            `[[registries]]`,
            `type       = "generic"`,
            `name       = "sonar-binaries"`,
            `upstreams  = ["https://binaries.sonarsource.com"]`,
            `path_allow = ["Distribution/sonar-scanner-cli/**"]`,
            ``,
            `# Not listed here: anything mise fetches from GitHub releases (the aqua,`,
            `# ubi and python-build-standalone backends) — those go through a`,
            `# type = "github" registry, which also avoids GitHub's API rate limit.`,
          ].join("\n") + `\n\n# Proxy base: ${ctx.base}`,
      },
      {
        key: "generic-client-env",
        label: "Client env vars",
        lang: "bash",
        template: (ctx) => {
          const p = (name: string) => `${ctx.urlFor(name)}/generic`;
          return [
            `# Each toolchain has its own mirror variable. Substitute the registry`,
            `# names you configured — these match the "Toolchain presets" tab.`,
            ``,
            `# Node.js (also honoured by nvm, fnm, mise's core:node backend)`,
            `export NODEJS_ORG_MIRROR="${p("node-dist")}"`,
            ``,
            `# Rust toolchains via rustup`,
            `export RUSTUP_DIST_SERVER="${p("rust-dist")}"`,
            `export RUSTUP_UPDATE_ROOT="${p("rust-dist")}/rustup"`,
            ``,
            `# Everything else is a plain URL swap in your install script, e.g.`,
            `curl -fL "${p("helm-bin")}/helm-v4.2.3-linux-amd64.tar.gz" | tar xz`,
            ``,
            `# Current registry (${ctx.registryName}):`,
            `# ${ctx.registryUrl}/generic/<path>`,
          ].join("\n");
        },
        note:
          `Variables are read at download time, so export them before ` +
          `<code>mise install</code> / ` +
          `<code>rustup update</code> — a toolchain already ` +
          `on disk is not re-fetched.`,
      },
      {
        key: "generic-mise",
        label: "mise url_replacements",
        lang: "toml",
        template: (ctx) => {
          const mirrors = [
            "node-dist",
            "rust-dist",
            "go-dl",
            "helm-bin",
            "minio-dl",
            "sonar-binaries",
          ];
          const p = (name: string) => `${ctx.urlFor(name)}/generic`;
          const lines: string[] = [];
          if (ctx.isAuthenticated) {
            lines.push(
              `# Authentication: mise reads ~/.netrc for HTTP Basic Auth`,
              // Each mirror is its own registry, so each may sit on its own
              // subdomain — one stanza per host actually referenced below.
              ...netrcStanzas(
                mirrors.map((name) => hostOf(ctx.urlFor(name))),
                ctx,
              ),
              ``,
            );
          }
          lines.push(
            `# Routes mise's direct downloads through generic mirrors, without`,
            `# per-tool env vars. Complements the GitHub/npm/cargo rules on the`,
            `# "mise" tab — keep both in the same [settings.url_replacements].`,
            `[settings.url_replacements]`,
            ``,
            String.raw`"regex:^https://nodejs\\.org/dist/(.+)" = "${p("node-dist")}/$1"`,
            String.raw`"regex:^https://static\\.rust-lang\\.org/(.+)" = "${p("rust-dist")}/$1"`,
            String.raw`"regex:^https://dl\\.google\\.com/go/(.+)" = "${p("go-dl")}/$1"`,
            String.raw`"regex:^https://get\\.helm\\.sh/(.+)" = "${p("helm-bin")}/$1"`,
            String.raw`"regex:^https://dl\\.min\\.io/(.+)" = "${p("minio-dl")}/$1"`,
            String.raw`"regex:^https://binaries\\.sonarsource\\.com/(.+)" = "${p("sonar-binaries")}/$1"`,
          );
          return lines.join("\n");
        },
        note:
          `Each rewritten URL must still pass its registry's ` +
          `<code>path_allow</code> allowlist — widen the globs ` +
          `if <code>mise install</code> reports a 403.`,
      },
    ],
  },
  {
    id: "nodedist",
    label: "Node (nvm, fnm, n, mise)",
    fileHint: ".nvmrc",
    description:
      `The <code>nodejs.org/dist</code> tree as a typed registry, so a Node release can be ` +
      `<em>blocked</em> rather than merely cached: <code>index.tab</code> and ` +
      `<code>index.json</code> are filtered listings, the tarballs and ` +
      `<code>SHASUMS256.txt</code> are served byte-exact. Read by nvm, fnm, ` +
      `<code>n</code> and mise. Proxy-only: there is no publish protocol.`,
    snippets: [
      {
        key: "nodedist-env",
        label: "Client setup",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/nodedist`;
          return [
            `# Export before sourcing nvm.sh — in /etc/profile.d, a Containerfile,`,
            `# or a CI job's env: block. fnm and n read their own variable.`,
            `export NVM_NODEJS_ORG_MIRROR="${reg}"`,
            `export FNM_NODE_DIST_MIRROR="${reg}"`,
            `export N_NODE_MIRROR="${reg}"`,
            `export NODEJS_ORG_MIRROR="${reg}"   # mise`,
            ``,
            `nvm ls-remote        # reads index.tab through the proxy`,
            `nvm install 22.11.0  # SHASUMS256.txt and the tarball, cached`,
          ].join("\n");
        },
        note: (ctx) =>
          ctx.isAuthenticated
            ? `nvm builds its own <code>curl</code> command and has nowhere to put a header; ` +
              `libcurl reads <code>~/.netrc</code> without being asked, so add an entry for ` +
              `this host (see the <em>~/.netrc</em> tab).`
            : `A blocked release disappears from <code>nvm ls-remote</code> and ` +
              `<code>nvm install &lt;that version&gt;</code> stops on nvm's own ` +
              `<em>"Version … not found"</em> — no download is attempted.`,
      },
      {
        key: "nodedist-netrc",
        label: "~/.netrc",
        lang: "text",
        showWhen: (ctx) => ctx.isAuthenticated,
        template: (ctx) =>
          [`machine ${ctx.netrcHost}`, `login ${ctx.netrcLogin}`, `password ${ctx.token}`].join(
            "\n",
          ),
        note: `Neither nvm nor fnm can send an <code>Authorization</code> header; both use libcurl, which reads this file.`,
      },
      {
        key: "nodedist-config",
        label: "Server config",
        lang: "toml",
        template: (ctx) =>
          [
            `[[registries]]`,
            `name      = "${ctx.registryName}"`,
            `type      = "nodedist"`,
            `mode      = "proxy"                       # the only mode: no publish protocol`,
            `upstreams = ["https://nodejs.org/dist"]  # the default; io.js takes its own block`,
            ``,
            `[registries.rbac]`,
            `# index.tab / index.json are listings; the files are reads. An install needs both.`,
            `anonymous = ["releases:read", "releases:list"]`,
            ``,
            `# An age gate here must say what it does with a release index.tab no`,
            `# longer lists (it reaches the gate with no date) — there is no default.`,
            `# [[registries.rules]]`,
            `# kind = "release_age_gate"`,
            `# min_age_secs = 86400`,
            `# deny_missing_timestamp = false`,
          ].join("\n"),
        note:
          `Run <code>batlehub-cli registry suggest</code> in a project with an ` +
          `<code>.nvmrc</code> to generate this block with the pinned release under ` +
          `<code>warm_packages</code>.`,
      },
    ],
  },
  {
    id: "sdkman",
    label: "SDKMAN",
    fileHint: ".sdkmanrc",
    description:
      `SDKMAN's candidates API and download broker as one registry: the JDK, Gradle, ` +
      `Maven, Kotlin and every other candidate. <code>sdk list</code> and ` +
      `<code>sdk install</code> resolve through filtered listings, a blocked version answers ` +
      `<code>invalid</code> at <code>candidates/validate</code>, and the broker's redirect to ` +
      `the vendor's CDN is followed server-side so the archive is cached here. Proxy-only.`,
    snippets: [
      {
        key: "sdkman-env",
        label: "Client setup",
        lang: "bash",
        template: (ctx) => {
          const reg = `${ctx.registryUrl}/sdkman`;
          return [
            `# Export before sourcing sdkman-init.sh — it sets each variable only`,
            `# when empty. /etc/profile.d, a Containerfile, or a CI job's env: block.`,
            `export SDKMAN_CANDIDATES_API="${reg}"`,
            `export SDKMAN_BROKER_API="${reg}/broker"`,
            ``,
            `sdk list java                # the rendered table, blocked versions removed`,
            `sdk install java 21.0.5-tem  # validate, download through the broker route, hook`,
          ].join("\n");
        },
        note: (ctx) =>
          ctx.isAuthenticated
            ? `<code>sdk</code> builds its own <code>curl</code> command and has nowhere to put ` +
              `a header; libcurl reads <code>~/.netrc</code> without being asked, so add an ` +
              `entry for this host (see the <em>~/.netrc</em> tab).`
            : `A blocked version answers <code>invalid</code> at ` +
              `<code>candidates/validate</code>, so <code>sdk install</code> stops on ` +
              `SDKMAN's own refusal before any download.`,
      },
      {
        key: "sdkman-netrc",
        label: "~/.netrc",
        lang: "text",
        showWhen: (ctx) => ctx.isAuthenticated,
        template: (ctx) =>
          [`machine ${ctx.netrcHost}`, `login ${ctx.netrcLogin}`, `password ${ctx.token}`].join(
            "\n",
          ),
        note: `<code>sdk</code> cannot send an <code>Authorization</code> header; it uses libcurl, which reads this file.`,
      },
      {
        key: "sdkman-config",
        label: "Server config",
        lang: "toml",
        template: (ctx) =>
          [
            `[[registries]]`,
            `name       = "${ctx.registryName}"`,
            `type       = "sdkman"`,
            `mode       = "proxy"                        # the only mode: no publish protocol`,
            `upstreams  = ["https://api.sdkman.io/2"]    # the candidates API (the /2 is part of it)`,
            `broker_url = "https://broker.sdkman.io"     # the download broker`,
            ``,
            `[registries.rbac]`,
            `# candidates, validate, hooks and healthcheck are listings; the download is a read.`,
            `anonymous = ["releases:read", "releases:list"]`,
            ``,
            `# SDKMAN publishes no dates, so an age gate here is decided entirely by`,
            `# deny_missing_timestamp: true refuses every download, false makes it inert.`,
            `# [[registries.rules]]`,
            `# kind = "release_age_gate"`,
            `# min_age_secs = 86400`,
            `# deny_missing_timestamp = false`,
          ].join("\n"),
        note:
          `Egress: <code>api.sdkman.io</code>, <code>broker.sdkman.io</code> and the CDNs the ` +
          `broker redirects to (<code>github.com</code>, <code>repo.maven.apache.org</code>, ` +
          `<code>services.gradle.org</code>, …). Run <code>batlehub-cli registry suggest</code> ` +
          `in a project with an <code>.sdkmanrc</code> to generate this block with its ` +
          `pinned versions under <code>warm_packages</code>.`,
      },
    ],
  },
];

/**
 * The one line that installs *this* version, per registry type.
 *
 * Data, not a `switch` in a page: the detail page's own `downloadUrl` was a
 * hand-written switch covering eight of the twenty-one protocols, and the other
 * thirteen rendered an em dash under a column headed "Download". Adding a
 * registry type should not mean editing a component — that is the defect
 * PRODUCT principle 5 names.
 *
 * **`null` is a real answer and the common one.** Maven declares a dependency in
 * a POM rather than installing it from a shell, Terraform pins a module in HCL,
 * a JetBrains plugin is installed from inside the IDE, and Pacman has no syntax
 * for pinning a version at all. A wrong-but-plausible command is worse than
 * none on a page whose whole discipline is not claiming what it cannot support —
 * so a type with no honest one-liner gets no line, and the page says which.
 *
 * Every command assumes the registry is already configured, which is the Setup
 * Guide's subject and not this one's; the caller says so beside the snippet.
 */
export type InstallCommand = { command: string; lang: string };

const INSTALL_COMMANDS: Record<string, (name: string, version: string) => InstallCommand> = {
  npm: (n, v) => ({ command: `npm install ${n}@${v}`, lang: "bash" }),
  cargo: (n, v) => ({ command: `cargo add ${n}@${v}`, lang: "bash" }),
  pypi: (n, v) => ({ command: `pip install ${n}==${v}`, lang: "bash" }),
  rubygems: (n, v) => ({ command: `gem install ${n} -v ${v}`, lang: "bash" }),
  nuget: (n, v) => ({ command: `dotnet add package ${n} --version ${v}`, lang: "bash" }),
  goproxy: (n, v) => ({ command: `go get ${n}@${v}`, lang: "bash" }),
  composer: (n, v) => ({ command: `composer require ${n}:${v}`, lang: "bash" }),
  conda: (n, v) => ({ command: `conda install ${n}=${v}`, lang: "bash" }),
  deb: (n, v) => ({ command: `apt install ${n}=${v}`, lang: "bash" }),
  rpm: (n, v) => ({ command: `dnf install ${n}-${v}`, lang: "bash" }),
  // `mvn dependency:get` takes the coordinate whole, and a Maven package's name
  // in this console *is* `group:artifact` — so the one shell command Maven has
  // for "fetch exactly this" composes without parsing the name apart.
  maven: (n, v) => ({ command: `mvn dependency:get -Dartifact=${n}:${v}`, lang: "bash" }),
  openvsx: (n, v) => ({ command: `code --install-extension ${n}@${v}`, lang: "bash" }),
  "vscode-marketplace": (n, v) => ({ command: `code --install-extension ${n}@${v}`, lang: "bash" }),
};

/** The install line for a coordinate, or `null` where the type has no honest one. */
export function installCommandFor(
  apiType: string | null | undefined,
  name: string,
  version: string,
): InstallCommand | null {
  if (!apiType) return null;
  return INSTALL_COMMANDS[apiType]?.(name, version) ?? null;
}
