/**
 * The English navigation: the navbar and the page-grouped sidebars.
 *
 * These moved out of `config.ts` when the site gained a second locale. They are
 * a locale's property rather than the site's — VitePress resolves
 * `themeConfig.nav` and `themeConfig.sidebar` per locale, and between them they
 * hold every navigation label on the site, which is the largest single block of
 * translatable strings it has. `nav/fr.ts` is the same shape, translated.
 *
 * Two gates import this module rather than parsing the config for `link:`:
 * `check-audience.mjs` (one page, one sidebar *of its locale*) and
 * `check-links.mjs` (what the navigation makes reachable). Importing means a
 * typo here is a load error, where a regex over a moved literal quietly matches
 * nothing and reports every page as an orphan.
 */

// Navbar = top-level section entry points (plain links). The detailed
// page tree for each section lives in the sidebar, so the two don't repeat
// the same items. Reference topics (Caching, Access Control, Package
// Explorer, SBOM, HA) and Config Generator are reached via the guide sidebar.
// No "Home" entry: the wordmark to its left already links there, and the
// navbar has exactly as much room as it has. Spending a slot on the one
// destination every visitor can already reach is what pushed the appearance
// toggle off the edge when the Config Generator was added.
export const nav = [
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
];

// Page-grouped sidebars: each entry links a sibling *page*, not an in-page
// anchor. VitePress's on-this-page outline (right aside) covers the headings
// within a page, so they are not duplicated in the left sidebar.
export const sidebar = {
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
        { text: "The scan worker", link: "/operations/scan-worker" },
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
        { text: "Translating the docs", link: "/contributing/translating" },
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
          text: "0018-bis — Worker under load",
          link: "/rfc/0018-bis-the-worker-under-load-invocations-slots-and-results-by-content",
        },
        {
          text: "0019 — Forge registries: refs, releases, raw",
          link: "/rfc/0019-git-forge-registries-refs-releases-raw",
        },
        {
          text: "0020 — Signed VSIX assets",
          link: "/rfc/0020-signing-at-the-vscode-marketplace-registry",
        },
        {
          text: "0021 — releases into registries",
          link: "/rfc/0021-forge-releases-into-registries",
        },
        {
          text: "0022 — Sandbox runtimes",
          link: "/rfc/0022-sandbox-runtimes",
        },
        // END rfc-sidebar
      ],
    },
  ],
};
