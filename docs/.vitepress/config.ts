import { fileURLToPath } from "node:url";
import { defineConfig } from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

// The banner, the table on /rfc/ and the sidebar below all quote the same
// header rows, so the parser lives in one place — `build/rfc-meta.mjs`, shared
// with `task rfc:index` and `task rfc:status`.
import { parseFilename, rfcStatus } from "../build/rfc-meta.mjs";

// `withMermaid` turns every ```mermaid fence into a rendered diagram. Without
// it the fences shipped as syntax-highlighted source: valid, readable, and not
// what a reader of an architecture section is looking for. It only wraps the
// config — `defineConfig` still types everything below.
//
// Diagram *syntax* is not checked by the build (the plugin renders on the
// client), which is why `task docs:mermaid` parses every fence separately.
const config = withMermaid(defineConfig({
  appearance: "dark",
  title: "BatleHub",
  description:
    "Your package hub. Proxy, cache, and host npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform, and more.",
  cleanUrls: false,
  base: process.env.BASE_URL || "/",

  // Merging the two documentation trees is not the same as publishing both
  // (RFC 0005 §6.7). Three classes stay in the repo and out of the build:
  // generated artifacts (`i18n-review-fr.md` is output from
  // `task ui:i18n:review`, not a document), point-in-time security findings,
  // and forms — the RFC template is something you copy, not something you read.
  //
  // The rule is worth stating plainly, because the RFCs next door *do* publish
  // and they are candid about this project's own past defects: design history
  // publishes, security findings do not. Visible rigour about one's own
  // mistakes reads as competence; a dated vulnerability survey reads as a map.
  srcExclude: ["internal/**"],

  // Generated from each RFC's own `Status` row — see `rfcStatus` above and the
  // banner in `theme/RfcStatus.vue`. An unparseable status fails the build
  // rather than rendering an unlabelled page.
  //
  // Only a numbered RFC carries one. `rfc/index.md` and `rfc/plan.md` are
  // about the RFCs — a listing and a build plan — and neither has a status of
  // its own to quote, so they are recognised by the same filename rule
  // `task rfc:index` uses to decide what is an RFC at all.
  transformPageData(pageData, ctx) {
    const inRfc = pageData.filePath.startsWith("rfc/");
    if (inRfc && parseFilename(pageData.filePath.slice("rfc/".length))) {
      pageData.frontmatter.rfcStatus = rfcStatus(
        ctx.siteConfig.srcDir,
        pageData.filePath,
      );
    }
  },
  vite: {
    server: {
      allowedHosts: true,
      host: true,
    },
    plugins: [
      // Drop the default theme's Inter. See theme/no-inter.css for why.
      //
      // A `resolve.alias` entry cannot do this: aliases match the import
      // specifier as written, and the default theme imports its own stylesheet
      // as the relative `./styles/fonts.css`. Matching that string alone would
      // catch any file of that name in any package. Resolving by importer is
      // what makes the redirect specific to VitePress's own theme.
      {
        name: "batlehub:no-inter",
        enforce: "pre" as const,
        resolveId(source: string, importer?: string) {
          if (
            source.endsWith("styles/fonts.css") &&
            importer?.includes("theme-default")
          ) {
            return fileURLToPath(
              new URL("./theme/no-inter.css", import.meta.url),
            );
          }
          return null;
        },
      },
    ],
  },
  head: [
    [
      "link",
      {
        rel: "icon",
        type: "image/svg+xml",
        href: (process.env.BASE_URL || "/") + "logo.svg",
      },
    ],

    // The two specimen faces, self-hosted (RFC 0005 §6.4). Preloaded because
    // both are in the first paint: headings and the wordmark are Silkscreen and
    // every other string on the page is JetBrains Mono. Silkscreen 400 is not
    // preloaded — every Silkscreen rule in the theme asks for 700.
    //
    // `@font-face` declares these in `theme/vp-bridge.css` with root-absolute
    // paths; the preload hrefs carry BASE_URL for the same reason logo.svg does,
    // since the site publishes under a prefix.
    [
      "link",
      {
        rel: "preload",
        as: "font",
        type: "font/woff2",
        crossorigin: "",
        href: (process.env.BASE_URL || "/") + "fonts/silkscreen-700.woff2",
      },
    ],
    [
      "link",
      {
        rel: "preload",
        as: "font",
        type: "font/woff2",
        crossorigin: "",
        href: (process.env.BASE_URL || "/") + "fonts/jetbrainsmono-latin.woff2",
      },
    ],

    // The rendition sync. `tokens.css` authors light under
    // `:root[data-theme="light"]`; VitePress carries its resolved appearance as
    // a `.dark` class on the same element. This mirrors one onto the other so
    // there is a single stored preference and a single resolution — The
    // Stored-Preference Rule wants one mechanism, and VitePress already has it
    // (it stores `system|light|dark` and resolves before first paint).
    //
    // The initial call plus the observer covers both injection orders: if
    // VitePress's appearance script has already run, the first call is right; if
    // it has not, the observer fires when it does. Both happen in the head, and
    // observer callbacks are microtasks, so neither case reaches a paint with
    // the wrong ground. The filter is `class`, so writing `data-theme` cannot
    // re-trigger it.
    [
      "script",
      {},
      `(()=>{const e=document.documentElement,s=()=>e.setAttribute("data-theme",e.classList.contains("dark")?"dark":"light");s();new MutationObserver(s).observe(e,{attributes:true,attributeFilter:["class"]})})()`,
    ],
    ["meta", { name: "theme-color", content: "#dc2626" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:site_name", content: "BatleHub" }],
    ["meta", { property: "og:title", content: "BatleHub" }],
    [
      "meta",
      {
        property: "og:description",
        content:
          "Your package hub. Proxy, cache, and host npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform, and more.",
      },
    ],
    [
      "meta",
      {
        property: "og:image",
        content: (process.env.BASE_URL || "/") + "logo.svg",
      },
    ],
    ["meta", { name: "twitter:card", content: "summary" }],
    ["meta", { name: "twitter:title", content: "BatleHub" }],
    [
      "meta",
      {
        name: "twitter:description",
        content:
          "Your package hub. Proxy, cache, and host npm, Cargo, Maven, PyPI, NuGet, Go, RubyGems, Terraform, and more.",
      },
    ],
    [
      "meta",
      {
        name: "twitter:image",
        content: (process.env.BASE_URL || "/") + "logo.svg",
      },
    ],
  ],

  markdown: {
    // Syntax highlighting is the one colour system on this site the design
    // tokens do not decide — a highlighter's palette is a mapping from grammar
    // to hue, and DESIGN.md's four colours cannot express one. What it *can*
    // decide is the floor: every token has to clear AA against the ground the
    // code block actually sits on, which is `--ground-sunk` in both renditions.
    //
    // VitePress's defaults do not. Measured by `docs:design:rendered`:
    // `github-dark`'s comment token (#6a737d) lands at 4.35:1 on near-black and
    // `github-light`'s string token (#22863a) at 4.49:1 on paper — the second
    // under the bar by a hundredth, which is exactly the kind of miss nothing
    // but a measurement finds. GitHub's Primer defaults fix the dark ground and
    // still leave the light comment at 4.41:1, because this world's paper is
    // `oklch(0.99 0.004 18)` rather than #ffffff and a palette tuned against
    // pure white does not survive the move. `light-plus`/`dark-plus` clear both.
    theme: { light: "github-light-high-contrast", dark: "github-dark-high-contrast" },
  },

  themeConfig: {
    logo: "/logo.svg",
    siteTitle: "BatleHub.",

    // Navbar = top-level section entry points (plain links). The detailed
    // page tree for each section lives in the sidebar, so the two don't repeat
    // the same items. Reference topics (Caching, Access Control, Package
    // Explorer, SBOM, HA) and Config Generator are reached via the guide sidebar.
    // No "Home" entry: the wordmark to its left already links there, and the
    // navbar has exactly as much room as it has. Spending a slot on the one
    // destination every visitor can already reach is what pushed the appearance
    // toggle off the edge when the Config Generator was added.
    nav: [
      {
        text: "Install",
        link: "/guide/installation",
        activeMatch: "/guide/installation",
      },
      // The one page that is a tool rather than a document. It is in the navbar
      // because a visitor who has decided to install needs a `config.toml`
      // next, and generating one beats reading a 12 000-word reference to
      // write it by hand — which is where it was buried until now.
      {
        text: "Config Generator",
        link: "/guide/config-generator",
        activeMatch: "/guide/config-generator",
      },
      {
        text: "User Guide",
        link: "/use/",
        activeMatch: "/use/",
      },
      {
        text: "Registries",
        link: "/registries/",
        activeMatch: "/registries/",
      },
      {
        text: "Admin",
        link: "/guide/administration",
        activeMatch: "/guide/admin",
      },
      {
        text: "Operations",
        link: "/operations/",
        activeMatch: "/operations/",
      },
      // Three sections that are about the project rather than about running it.
      // A dropdown rather than three more top-level entries: the navbar is the
      // list of things a visitor came for, and nobody arrives at a package
      // proxy's documentation wanting the RFC index.
      {
        text: "Project",
        items: [
          { text: "Roadmap", link: "/guide/roadmap" },
          { text: "Contributing", link: "/contributing/" },
          { text: "Design history", link: "/rfc/" },
        ],
        activeMatch: "/(contributing|rfc)/",
      },
    ],

    // Page-grouped sidebars: each entry links a sibling *page*, not an in-page
    // anchor. VitePress's on-this-page outline (right aside) covers the headings
    // within a page, so they are not duplicated in the left sidebar.
    sidebar: {
      "/registries/": [
        {
          text: "Registries",
          items: [{ text: "Overview & matrix", link: "/registries/" }],
        },
        {
          text: "Source hosting",
          items: [
            { text: "GitHub", link: "/registries/github" },
            { text: "Forgejo / Gitea", link: "/registries/forgejo" },
            { text: "GitLab", link: "/registries/gitlab" },
          ],
        },
        {
          text: "Language package managers",
          items: [
            { text: "npm", link: "/registries/npm" },
            { text: "Cargo", link: "/registries/cargo" },
            { text: "Go Modules", link: "/registries/goproxy" },
            { text: "Maven", link: "/registries/maven" },
            { text: "PyPI", link: "/registries/pypi" },
            { text: "Conda", link: "/registries/conda" },
            { text: "Composer", link: "/registries/composer" },
            { text: "RubyGems", link: "/registries/rubygems" },
            { text: "NuGet", link: "/registries/nuget" },
            { text: "Terraform", link: "/registries/terraform" },
          ],
        },
        {
          text: "Editor extensions",
          items: [
            { text: "OpenVSX", link: "/registries/openvsx" },
            { text: "VS Code Marketplace", link: "/registries/vscode-marketplace" },
            { text: "JetBrains Marketplace", link: "/registries/jetbrains-marketplace" },
          ],
        },
        {
          text: "OS / system packages",
          items: [
            { text: "Debian / APT", link: "/registries/deb" },
            { text: "RPM / YUM / DNF", link: "/registries/rpm" },
            { text: "Pacman / Arch", link: "/registries/pacman" },
          ],
        },
        {
          text: "Binaries & mirrors",
          items: [
            { text: "JetBrains IDEs", link: "/registries/jetbrains" },
            { text: "Generic mirror", link: "/registries/generic" },
          ],
        },
        {
          text: "Toolchains",
          items: [
            { text: "Node (nvm, fnm, n, mise)", link: "/registries/nodedist" },
            { text: "SDKMAN", link: "/registries/sdkman" },
          ],
        },
      ],
      // I run this server.
      "/guide/": [
        {
          text: "Getting started",
          items: [
            { text: "What it does", link: "/guide/features" },
            { text: "Installation", link: "/guide/installation" },
            { text: "Configuration", link: "/guide/configuration" },
            { text: "Worked examples", link: "/guide/configuration-examples" },
            { text: "Config Generator", link: "/guide/config-generator" },
          ],
        },
        {
          text: "Administration",
          items: [
            { text: "Overview", link: "/guide/administration" },
            { text: "Configuration", link: "/guide/admin-config" },
            { text: "Storage & health", link: "/guide/admin-storage-health" },
            { text: "Policies & packages", link: "/guide/admin-policies" },
            { text: "Access & audit", link: "/guide/admin-access" },
          ],
        },
        {
          text: "Reference",
          items: [
            { text: "Caching", link: "/guide/caching" },
            { text: "Access Control", link: "/guide/access-control" },
            { text: "Host-based routing", link: "/guide/host-routing" },
            { text: "Private upstreams", link: "/guide/private-upstreams" },
            { text: "Hot reload", link: "/guide/hot-reload" },
            { text: "SBOM", link: "/guide/sbom" },
            { text: "High Availability", link: "/guide/high-availability" },
            { text: "Server binary subcommands", link: "/guide/server-cli" },
            { text: "Capacity planning", link: "/guide/capacity-planning" },
          ],
        },
        {
          text: "Project",
          items: [{ text: "Roadmap", link: "/guide/roadmap" }],
        },
      ],

      // I have a package manager and a token, and I need this to work.
      "/use/": [
        {
          text: "Using BatleHub",
          items: [
            { text: "Overview", link: "/use/" },
            { text: "Publishing packages", link: "/use/publishing" },
            { text: "Command-line client", link: "/use/cli" },
          ],
        },
        {
          text: "Package Explorer",
          items: [
            { text: "Overview", link: "/use/package-explorer" },
            { text: "Upstream search", link: "/use/package-explorer-search" },
            { text: "Access control", link: "/use/package-explorer-access" },
            { text: "Cache & API", link: "/use/package-explorer-cache" },
          ],
        },
        {
          text: "When something is wrong",
          items: [
            { text: "Vulnerability proxy", link: "/use/vulnerability-proxy" },
            { text: "mise", link: "/use/mise" },
            { text: "Troubleshooting", link: "/use/troubleshooting" },
          ],
        },
      ],

      // Something is broken, or an auditor is asking.
      "/operations/": [
        {
          text: "Operations",
          items: [{ text: "Overview", link: "/operations/" }],
        },
        {
          text: "Runbooks",
          items: [
            { text: "Incident response", link: "/operations/incident-response" },
            { text: "Disaster recovery", link: "/operations/disaster-recovery" },
            { text: "What leaves this instance", link: "/operations/egress" },
            {
              text: "Production hardening",
              link: "/operations/production-hardening",
            },
            { text: "Registry health check", link: "/operations/check-registries" },
            { text: "Air gap", link: "/operations/air-gap" },
            { text: "Upstream disappearance", link: "/operations/upstream-disappearance" },
          ],
        },
        {
          text: "Compliance",
          items: [
            { text: "Change management", link: "/operations/change-management" },
            { text: "SOC 2 checklist", link: "/operations/soc2-checklist" },
            { text: "MD5 and SHA-1", link: "/operations/weak-hashes" },
          ],
        },
      ],

      // Someone changing the code.
      "/contributing/": [
        {
          text: "Contributing",
          items: [
            { text: "Overview", link: "/contributing/" },
            { text: "Working on BatleHub", link: "/contributing/contributing" },
            { text: "Testing", link: "/contributing/testing" },
            { text: "Vulnerability scanning & SBOMs", link: "/contributing/security-scanning" },
          ],
        },
        {
          text: "Extending",
          items: [
            { text: "Adding a registry", link: "/contributing/adding-a-registry" },
            {
              text: "Adding a vulnerability scanner",
              link: "/contributing/adding-a-vulnerability-scanner",
            },
          ],
        },
      ],

      // Someone asking why it is like this. Ordered oldest-first, because these
      // read as a sequence: each one argues with the state the previous left.
      "/rfc/": [
        {
          text: "Design history",
          items: [
            { text: "What these are", link: "/rfc/" },
            { text: "What is next", link: "/rfc/plan" },
            // BEGIN rfc-sidebar — generated by `task rfc:index` from each RFC's header table — do not edit by hand
            {
              text: "0001 — Subdomain routing",
              link: "/rfc/0001-subdomain-routing",
            },
            {
              text: "0002 — Vulnerability flags",
              link: "/rfc/0002-vulnerability-flags-and-exposure",
            },
            {
              text: "0003 — UI rework",
              link: "/rfc/0003-ui-rework",
            },
            {
              text: "0004 — Admin composition",
              link: "/rfc/0004-admin-composition-and-api-surface",
            },
            {
              text: "0004-bis — What 0004 left",
              link: "/rfc/0004-bis-what-rfc-0004-left",
            },
            {
              text: "0005 — One documentation tree",
              link: "/rfc/0005-docs-site-design-system",
            },
            {
              text: "0005-bis — Two readers, one home each",
              link: "/rfc/0005-bis-audience-split-and-one-home",
            },
            {
              text: "0006 — A block every ecosystem can see",
              link: "/rfc/0006-blocked-versions-hidden-everywhere",
            },
            {
              text: "0007 — The README, per version",
              link: "/rfc/0007-package-readmes",
            },
            {
              text: "0007-bis — The three 0007 deferred",
              link: "/rfc/0007-bis-images-search-and-fetch",
            },
            {
              text: "0008 — mise in an air-gapped estate",
              link: "/rfc/0008-mise-in-an-air-gapped-estate",
            },
            {
              text: "0008-bis — Listings across the gap",
              link: "/rfc/0008-bis-listings-across-the-gap",
            },
            {
              text: "0009 — Every endpoint the client actually calls",
              link: "/rfc/0009-protocol-coverage",
            },
            {
              text: "0010 — The toolchain layer",
              link: "/rfc/0010-toolchain-managers",
            },
            {
              text: "0011 — Authenticated OpenVSX access",
              link: "/rfc/0011-openvsx-login",
            },
            {
              text: "0011-bis — Namespace-scoped visibility",
              link: "/rfc/0011-bis-namespace-scoped-visibility",
            },
            {
              text: "0012 — Signed URLs for the credential-less request",
              link: "/rfc/0012-signed-urls-for-terraform",
            },
            {
              text: "0013 — What the console owes a reader",
              link: "/rfc/0013-console-answers-for-a-package",
            },
            {
              text: "0014 — Upstream disappearance",
              link: "/rfc/0014-upstream-disappearance",
            },
            {
              text: "0015 — Grants on the hierarchy",
              link: "/rfc/0015-grants-on-the-resource-hierarchy",
            },
            {
              text: "0016 — Retention and tombstones",
              link: "/rfc/0016-retention-and-the-permanence-of-a-published-name",
            },
            {
              text: "0017 — Grants editor",
              link: "/rfc/0017-writing-grants-at-the-package-and-version-tiers",
            },
            {
              text: "0018 — Supply-chain quarantine and verdicts",
              link: "/rfc/0018-supply-chain-quarantine-and-verdicts",
            },
            {
              text: "0019 — Forge registries: refs, releases, raw",
              link: "/rfc/0019-git-forge-registries-refs-releases-raw",
            },
            // END rfc-sidebar
          ],
        },
      ],
    },

    socialLinks: [
      { icon: "git", link: "https://git.batleforc.fr/batleforc/batlehub" },
      { icon: "github", link: "https://github.com/batleforc/batlehub" },
    ],

    footer: {
      message:
        "Released under the Apache 2.0 License. Made with ❤️ and too much ☕.",
      copyright: "Copyright © 2026 Batleforc",
    },

    search: {
      provider: "local",
    },
  },
}));

// `withMermaid` pre-declares mermaid's CommonJS dependencies for the dev
// server's dependency optimiser, and it does so with bare names —
// `optimizeDeps.include = ["@braintree/sanitize-url", "dayjs", "debug",
// "cytoscape", "cytoscape-cose-bilkent"]`. Those names resolve from the project
// root, and under pnpm's isolated `node_modules` none of them are there: they
// are mermaid's dependencies, not the docs site's, so they only exist under
// `.pnpm/…/node_modules/mermaid/node_modules`. Vite skips every entry it cannot
// resolve, so nothing gets pre-bundled.
//
// What that costs is not a warning, it is a broken page. `mermaid` itself is
// never scanned — the plugin injects its import from a `transform` hook with
// `enforce: "post"`, after the optimiser's scan — so it is served straight from
// source, and its `import dayjs from "dayjs"` lands on dayjs's raw UMD file.
// That file has no ESM exports, and the browser fails the module with
// "doesn't provide an export named: 'default'".
//
// Naming `mermaid` is the fix, and the reason the nested `mermaid > …` entries
// are here too rather than deleted: esbuild bundles mermaid *and* its CJS
// dependencies into one pre-bundled ESM chunk, so the interop happens at build
// time and no raw CJS file is ever requested. The `mermaid > x` form is Vite's
// own syntax for "resolve x the way mermaid would", which is exactly the step
// pnpm's layout breaks.
//
// `debug` is dropped rather than rewritten: mermaid 11 no longer depends on it,
// and an unresolvable entry is exactly the failure being fixed here.
config.vite!.optimizeDeps!.include = [
  "mermaid",
  "mermaid > @braintree/sanitize-url",
  "mermaid > dayjs",
  "mermaid > cytoscape",
  "mermaid > cytoscape-cose-bilkent",
];

export default config;
