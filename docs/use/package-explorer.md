# Package Explorer

The Package Explorer is a browsable catalog of every package BatleHub knows about. It collapses all versions of a package into a single row, combines proxied packages with locally published ones, and lets you search for packages that haven't been proxied yet by querying upstream registries in real time.

---

## Overview {#overview}

The Explorer is available to any user at `/explore` in the web UI. It has two views:

- **Catalog** (`/explore`) — one row per unique package name, across all accessible registries or filtered to a single one. Sortable by download count, name, or last accessed.
- **Package detail** (`/explore/packages/<registry>/<name>`) — all known versions with their source (proxied vs. locally published), per-version firewall status, and a gate summary showing your access level for that registry.

### Data sources {#sources}

| Source | Where the data lives |
| --- | --- |
| **Proxied** | `package_statuses` — every package ever requested through the proxy |
| **Local** | `local_packages` — packages published directly to a BatleHub `local` or `hybrid` registry |
| **Upstream** | Live search call to the upstream registry API (npm, crates.io, RubyGems) when you type a query |

Proxied and local versions of the same package are merged into a single entry showing `Both` as the source.

---

## Using the catalog {#catalog}

### Registry sidebar {#sidebar}

The left panel lists **every accessible registry**, including those that have not yet had any packages pass through them (shown with a count of `0`). Click a registry to filter the table; click **All registries** to see everything.

### Search {#search}

Type in the search box. After a 300 ms debounce two things happen:

1. The main table is filtered by substring match on the package name (server-side, case-insensitive).
2. An **upstream search** fires for registries that support it (see [Upstream search](/use/package-explorer-search#upstream-search)). Results appear at the bottom of the same table, marked **upstream**.

An upstream row is a package this instance holds nothing of. If the operator has
left [`console_fetch`](/guide/admin-config#console-fetch) on and the registry can
be addressed by version alone, the row offers a button naming the version the
upstream search returned — **Fetch 4.17.21** — which pulls it through this
instance under your own identity, gates, quota and audit row included. The row
then joins the held half of the table. Where the button is not offered the row
shows none, and the package page says why.

### Sort {#sort}

| Option | Behaviour |
| --- | --- |
| Most Downloaded | Packages with the highest total access-event count first |
| Name A–Z | Alphabetical by package name |
| Recently Accessed | Packages last requested most recently first |

### Table columns {#columns}

| Column | Notes |
| --- | --- |
| **Package** | Package name (monospace). |
| **Registry** | Registry the package belongs to. |
| **Versions** | Number of known versions (cached). For upstream-only rows: the latest version string from the upstream registry. |
| **Downloads** | Total access-event count across all versions. `—` for upstream-only rows. |
| **Source** | `Proxied`, `Local`, or `Both` for cached packages. For upstream-only rows: the package's description (if available). |
| **Proxy** | `Proxied` (solid badge) for packages already in the cache; `Not Yet Proxied` (dashed outline badge) for upstream-only results. |

A `Has blocked` badge appears alongside the **Source** badge when at least one version is currently blocked.

---

## Package detail {#detail}

Click any cached row in the catalog to open the detail page for that package.

### Gate summary {#gate}

The **Access Gate** card shows two checks against your current session:

| Check | Green | Red / Grey |
| --- | --- | --- |
| **Registry access** | Your role can proxy from this registry | Your role cannot access this registry |
| **Beta channel** | You are a beta-channel member — pre-release versions are visible | You are not a member; pre-release versions are hidden |

The gate card reflects what your current token allows. If the registry is accessible but a specific version is blocked, that appears in the Firewall column of the versions table rather than in the gate card.

### Versions table {#versions}

Each row is one version of the package. Columns:

| Column | Notes |
| --- | --- |
| **Version** | Version string. Pre-release versions (containing `-`) are shown in italic with a `pre-release` badge. |
| **Source** | `Proxied` (from upstream cache) or `Local` (published directly). |
| **Firewall** | See below. |
| **Downloads** | Total access-event count for this exact version. |
| **Last Accessed** | Most recent access-event timestamp. |
| **Published** | `published_at` timestamp for local packages; `—` for proxied. |

#### Firewall status {#firewall}

| Badge | Meaning |
| --- | --- |
| `Clear` | Version is available. |
| `Blocked` | An administrator blocked this version. Hover the badge to see the reason, who blocked it, and when. |
| `Yanked` | Version was yanked after publish (local packages only). |

---

## More

The rest of the Package Explorer documentation is split across these pages:

- [Upstream search](/use/package-explorer-search#upstream-search) — querying upstream registries for packages you haven't proxied yet, supported registries, and configuring the search URL.
- [Access control](/use/package-explorer-access#access-control) — separating proxy access from explore access, RBAC configuration, and inheritance rules.
- [Cache & API](/use/package-explorer-cache#cache) — the Explorer in-memory cache, performance notes, and the REST API reference.
