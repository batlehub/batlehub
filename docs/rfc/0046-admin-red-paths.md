---
reference: true
---

# RFC 0046 — Admin red paths

| Field       | Value                                                        |
| ----------- | ------------------------------------------------------------ |
| Status      | Draft                                                         |
| Short       | Admin red paths                                               |
| Settles     | Where each administrative task lives, how an operator reaches it from anywhere in the console, and what keeps the admin findable as pages are added |
| Author      | Max Batleforc <maxleriche.60@gmail.com>                       |
| Co-author   | Claude Opus 5.5 <noreply@anthropic.com>                       |
| Created     | 2026-11-11                                                    |
| Supersedes  | —                                                             |
| Touches     | `ui` (`config/adminSections.ts`, `config/navigation.ts`, `layouts/AdminLayout.vue`, `components/admin/SectionTabs.vue`, `components/admin/ConfigEditorCard.vue`, `components/admin/ConfigReadOnlyView.vue`, `components/app/AppHeader.vue`, a new `components/app/CommandPalette.vue` and `config/redPaths.ts`, the listbox extracted from `components/ui/combobox/Combobox.vue`, `router/index.ts`, `pages/AdminRegistry.vue`, `pages/AdminConfigReload.vue`, `pages/AdminFindings.vue` and the registry-scoped pages of §6.3, the pages holding hard-coded admin links (§6.4), `locales/{en,fr}.json`, `build/design-routes.mjs`, `build/fixtures/captured.json`), `sonar-project.properties`, `tests/heavy` (`console_security.mjs`, `console_fetch.sh`), docs (every `Admin →` and `/admin/` path in `docs/` and `docs/fr/`, §11) |
| Risk        | low — §7 (the palette lists destinations, and must list no more than the router already lets the viewer open) |

---

## 1. Summary

The admin console has grown from the 17 destinations RFC 0003 §4.2 sized its
seven sidebar sections for to 23 routed pages, and the sections have not
moved. An operator under pressure — an upstream is down, a user cannot
download, a CVE has landed — has to know *which of seven category words*
hides the page that fixes it, because the sidebar shows only the categories
and a section's pages appear only once you are inside it.

This RFC names those pressured journeys **red paths** (§2.1), measures each
one against the console as it is, and changes three things, in this order:

1. **The sidebar shows every page** it can open, grouped under its section,
   with no URL changed (§4.5 M1).
2. **A command palette** (`Ctrl+K` / `⌘K`, and a visible button) finds any
   page, registry or red-path action by what the operator would call it,
   in both locales, listing only what the viewer may open (§4.5 M2).
3. **The sections are regrouped by task**, with a **registry hub** that
   gathers what one registry's incident needs, and every moved path kept as
   a redirect (§4.5 M3, M4).

No endpoint, schema, configuration key or dependency is added.

### Before / after

```text
# today — "npm's upstream is failing, what do I do?"
Admin → Observability → (Health) → Registries tab → npm      (status)
Admin → Operations → (Upstream)                              (recheck)
Admin → Operations → Warming → pick npm again                (re-warm)
Admin → Observability → (Health) → Refresh explorer          (cache)
Registry page → Edit configuration → top of a 600-line file  (?registry= ignored)

# with this RFC
Ctrl+K "npm" ⏎                                               (registry hub)
  hub: status · Recheck upstream · Warm · Refresh explorer · Edit config ⏎
                                                  (caret on [[registries]] npm)
```

```mermaid
flowchart LR
    subgraph T["today: one incident, three sections"]
        A1["/admin/dashboard"] --> O1["Observability"] --> R1["Registries → npm"]
        A1 --> O2["Operations"] --> U1["Upstream"]
        O2 --> W1["Warming (pick npm)"]
        O1 --> H1["Health → Refresh explorer"]
        R1 --> C1["Config reload<br/>(registry ignored)"]
    end
    subgraph W["with this RFC"]
        K["Ctrl+K anywhere"] --> R2["Registries → npm (hub)"]
        R2 --> U2["Upstream ?registry=npm"]
        R2 --> W2["Warming ?registry=npm"]
        R2 --> C2["Configuration ?registry=npm<br/>caret on the block"]
    end
```

The diagram proves the point of §2: today one registry's incident crosses
three sections and drops the registry on every hop; after, every hop starts
from the registry and carries it.

---

## 2. Motivation

### 2.1 What a red path is

A **red path** is the route from an operator's *trigger* — an alert, a
ticket, a user's complaint, a published CVE — to the control that resolves
it. It is the path taken under pressure, by someone who may open the admin
once a month, so it is measured by how many **guesses** it takes, not by
clicks: a click on a label that names the task is free; a click on a label
that does not ("Observability" for "the registry is broken") is a guess.

The twelve red paths, from the code as it stands at `6eb196a4`
(`ui/src/config/adminSections.ts`, `ui/src/config/navigation.ts`,
`ui/src/router/index.ts`). *Clicks* count from `/admin/dashboard` with
perfect knowledge; *guesses* count section or tab labels that do not name
the task.

| # | Trigger | Where the control is today | Clicks | Guesses |
| --- | --- | --- | --- | --- |
| RP-1 | "I can't download X" | Security & Access → Access Check | 2 | 1 |
| RP-2 | An upstream is degraded | Observability → Registries → *name*; recheck under Operations → Upstream | 3 + 1 | 2 |
| RP-3 | Change one registry's config | registry page → Edit configuration, then find the block by hand | 4 + scroll | 2 |
| RP-4 | A CVE: which versions carry it? | Security & Access → Findings → group by reference | 3 | 1 |
| RP-5 | A scan failed / rescan | Security & Access → Scans → Failures | 3 | 1 |
| RP-6 | Block one version | the package page, Admin tab | catalog + 2 | 0 |
| RP-7 | Block many versions | Packages → Bulk Block | 2 | 0 |
| RP-8 | A leaked token or an abusive IP | Security & Access → (Blocks) | 1 | 1 |
| RP-9 | A package looks stale | Observability → (Health) → Refresh explorer | 1 | 1 |
| RP-10 | Who did this? | Observability → Audit Log | 2 | 1 |
| RP-11 | Announce maintenance | Operations → Global banner | 2 | 1 |
| RP-12 | A team needs a namespace | Namespaces & Channels → Team Namespaces | 1 | 0 |

The clicks are not the problem; ten of the twelve are three or fewer. The
guesses are, and §2.2 is why they are worse than the column suggests.

### 2.2 Motivations

1. **The sidebar names 1 of 23 destinations.** `ADMIN_SIDEBAR` has seven
   entries and `AdminLayout.vue` renders only those: *Dashboard* names a
   page, the other six name a section and open its first tab. The pages of a section
   are `SectionTabs` *inside* the page, so they are visible only after the
   section has been guessed. RFC 0003 §4.2 kept the seven groups because
   "the sidebar is the right pattern for 17 destinations"; there are now 23
   routes (22 in `ADMIN_PAGE_VERBS` plus `observability/registries/:name`),
   and Operations alone holds seven tabs. Falsifiable: `ADMIN_PAGE_VERBS`
   has 22 keys, the router adds `observability/registries/:name`, and
   `ADMIN_SIDEBAR` has seven entries (the 28 `to:` in `adminSections.ts`
   count the sidebar and the tabs together).
2. **One registry's incident spans three sections and loses the registry on
   every hop** (RP-2, RP-3). The registry page (`AdminRegistry.vue`, RFC 0044
   §6.4) shows status but no action: the upstream recheck is on
   `operations/upstream`, warming on `operations/warming`, the explorer
   flush on `observability/health`. Of the registry-scoped pages, only
   `AdminScans.vue` and `AdminAccessCheck.vue` (`:107-110`, with `name`,
   `version` and `user_id`) read `?registry=`; upstream, warming, imports,
   health and findings make the operator pick the registry again.
3. **A promised hop is dead.** RFC 0044 §4.5 specifies that *Edit
   configuration* opens `AdminConfigReload` with `?registry=` and "puts the
   caret on the first line of the matching `[[registries]]` block". The link
   sends the parameter (`AdminRegistry.vue:272–274`); nothing reads it — no
   `route.query` in `AdminConfigReload.vue`, no `setSelectionRange` anywhere
   in `ui/src`. RP-3 ends at the top of the file.
4. **Labels collide or misfile.** "Blocks" (accounts and addresses, under
   Security & Access) and "Bulk Block" (package versions, under Packages)
   are different objects with the same verb. "SBOM Export" is a package
   artifact filed under Operations. "Health" is where the explorer cache is
   flushed (`adminHealth.refreshExplore`), and the published docs still send
   readers to **Admin → Explore Cache** (`docs/use/package-explorer-cache.md:44`),
   a page that is a redirect since RFC 0003.
5. **There is no way to ask.** No search, no shortcut: `ui/src` binds no
   global key (`ctrlKey`/`metaKey` appear only in `PackageBreadcrumb.vue`'s
   middle-click guard). The operator who knows the *word* — "rescan",
   "banner", "audit" — cannot type it.
6. **Eight admin pages were never measured by the rendered gate.**
   `ADMIN_ROUTES` in `ui/build/design-routes.mjs` lists 15 paths; scans,
   findings, authorization, registries, a registry, upstream, banner and
   air-gap are absent. The pages most recently added are the ones nobody
   checked at 390 px — the same blind spot RFC 0004 §10 closed once.

---

## 3. Goals / non-goals

**Goals**

- Every red path of §2.1 reaches its control with **zero guesses** from the
  palette, and with at most one from the sidebar.
- Every admin page the viewer may open is **visible** in the sidebar without
  entering its section.
- A registry-scoped control opened from the registry hub **arrives with the
  registry selected**, including the configuration editor's caret.
- No bookmark breaks: every path that resolves today resolves after, through
  `LEGACY_REDIRECTS`.
- The palette and the sidebar **list exactly what the router lets the viewer
  open**, from the same table, so they cannot drift from the guard.
- A test fails when an admin page is added to the router without a sidebar
  entry, a palette entry, a verb and a rendered-gate route.

**Non-goals**

- **New admin features.** A red path whose control does not exist yet is
  listed in §15, not built here.
- **Backend or API changes.** Every list the palette shows already has a
  reader (§4.4); a server-side search would be a new enumeration surface for
  no gain at this size.
- **Package search in the palette.** The catalog already searches, ranked and
  paginated; the palette hands the query to `/packages?q=` instead (§4.2).
- **Merging the security console into the admin.** RFC 0042 placed
  `/security` beside the admin on purpose, for a group without admin verbs;
  the palette reaches both.
- **Usage analytics to rank red paths.** It would record what each operator
  opens; the list in §2.1 is reviewed, not measured.
- **Customisable navigation** (pinning, reordering). Twelve red paths do not
  need it, and per-user state is what §9 keeps at zero.
- **A redesign of the pages themselves.** Their composition is RFC 0004's;
  this RFC moves where they are, not what is on them.

---

## 4. User-facing design

### 4.1 Configuration

No configuration key is added. The palette, the expanded sidebar and the new
section tree are always on; there is nothing for an operator to set and no
closed side to default to, because nothing here grants or widens access.

### 4.2 Behaviour rules

**Sidebar (phase 1).**

- Each section is a heading followed by its pages, all visible. A section
  whose pages the viewer cannot open is absent, as today
  (`AdminLayout.vue`'s `sidebar` computed, RFC 0042 §4.3).
- The current page carries `aria-current="page"` and the 1 px selection edge;
  its section heading is not a link and is not focusable, so Tab moves page
  to page.
- `SectionTabs` stays on each page: it is the mobile navigation and the
  in-page reminder of siblings. On desktop it becomes redundant and is kept
  anyway — removing it is §15 question 6.

**Palette (phase 2).**

- Opened by `Ctrl+K` / `⌘K`, or by the *Go to…* button in the header (every
  viewer who is signed in) and at the top of the admin sidebar. The button
  shows the platform's chord: `⌘K` on macOS, `Ctrl K` elsewhere. The
  shortcut works from a single-line `input` (the catalog search included,
  where the browser's own `Ctrl+K` would otherwise move focus to the address
  bar) and does nothing while focus is in a `textarea` or a
  `contenteditable` — the configuration editor is a `textarea` an operator
  types into for minutes.
- Entries come from four sources, filtered **before** they are shown, every
  admin check at the **instance tier the guard uses** — `can(verb)` with no
  registry argument (`router/index.ts:168`). The per-registry form
  `can(verb, name)` (`useAuth.ts:214-218`) is never used for a link: a viewer
  holding `system:read` on one registry only would be shown a hub the guard
  then bounces.
  1. *Pages*: `ADMIN_PAGE_VERBS` where `can(verb)`, plus `/security` when
     `canAny(SECURITY_VERBS)`, `/me/*`, `/tools/*` and `/setup`.
  2. *Registries*: `GET /api/v1/registries` (`useRegistryOptions`), which
     `accessible_registries_for` already filters. A registry links to its
     hub when `can("system:read")` holds, otherwise to
     `/packages?registry=<name>`.
  3. *Red-path actions*: `config/redPaths.ts`, one entry per row of §2.1
     with keywords in both locales ("unblock", "débloquer", "CVE", "rescan",
     "bannière", …), each pointing at a page that is itself filtered by
     rule 1. An action whose page the viewer cannot open is not listed.
     RP-6 (block one version) needs a package, so its row hands off to the
     catalog search with the query, like rule 4, instead of a fixed page.
  4. *Search packages for "…"*: the last row when the query is non-empty and
     the viewer has registry access; it routes to `/packages?q=<query>`
     (`useCatalogQuery.applyQuery` reads `q`). It is **not offered** when the
     query looks like a credential (one word, 32 characters or more, no
     space): RP-8 is "a leaked token", and the operator who pastes it would
     write it into the browser history (§7.3).
- Matching is client-side: case- and accent-insensitive, every query word
  must prefix a word of the label or a keyword; ranked page > action >
  registry, then shorter label first. With an empty query the palette shows
  the red-path actions the viewer can open, in §2.1 order, under "Common
  tasks"; a heading with no row under it is not drawn.
- Each row is an `<a href>` built from `router.resolve`, and the active row
  is tracked with `aria-activedescendant` on the input. `Enter` calls
  `router.push` on the same location; `Ctrl+Enter` / `⌘Enter` and a
  middle-click open the `href` in a new tab, because the row is a link.
  `Esc` closes and returns focus to whatever had it. Nothing is remembered
  between openings (§15 question 4).
- The palette is a lazily imported chunk. If the chunk fails to load (hashes
  rotated by a redeploy under a long-lived tab), the button and the shortcut
  show "The console was updated — reload the page." as a `role=status`
  notice instead of doing nothing (`defineAsyncComponent`'s
  `errorComponent`, §10.2).
- The uninteresting case: a viewer with no admin verb opens the palette and
  sees their own pages, `/setup`, the registries they read and the package
  search — no admin entry, no empty "Admin" group (§4.5 M2 states).

**Section tree (phase 3).**

- Six sections, named by task (§4.5 M3). Every moved path gains a
  `LEGACY_REDIRECTS` entry; query and hash are preserved, as
  `handleLegacyPath` already does.
- The registry hub is `observability/registries/:name` moved into the
  *Registries* section (`/admin/registry/:name` in the §15 question 8
  proposal). It gains an *Actions* strip whose links carry
  `?registry=<name>`; each link is drawn only when `can(verb)` holds for the
  target page's verb — the guard's instance tier, as for the palette.
- Each registry-scoped page reads `?registry=` and **holds** it until its own
  list (or, for the configuration editor, its content) first settles, then
  preselects it (§6.3). Read on mount against a list still loading, the name
  would be dropped, and the slower the API the likelier — during an incident.
  A name the settled list does not contain is dropped with the page's normal
  "choose a registry" state, never an error; a list that fails to load keeps
  the value and shows the page's error.

### 4.3 Validation

Rejected:

| Condition | Rationale |
| --- | --- |
| A route under `/admin` with no `ADMIN_PAGE_VERBS` entry and no `meta.verb` | Already refused by the guard (fail closed); phase 1 adds the unit test that fails the build instead of the page silently redirecting (§14). |
| An `ADMIN_PAGE_VERBS` path in no section, or in two | One home per page; the sidebar and palette are generated from the sections. |
| A `redPaths.ts` entry whose target is not in `ADMIN_PAGE_VERBS` or the public pages | A palette row that the guard would bounce is a lie. |
| A `LEGACY_REDIRECTS` target that is not a route | A redirect chain that ends in "No match found". |

All four are unit tests over static tables, not runtime checks: nothing a
user types reaches them.

Warnings: none. `?registry=` naming an unknown registry is not a warning; the
page simply has nothing preselected (§4.2), as a hand-edited query on any
other page behaves today.

### 4.4 API surface

No API change. The palette reads `GET /api/v1/me` (verbs, already loaded by
`useAuth`) and `GET /api/v1/registries` (already called through
`useRegistryOptions` by seven consumers). No new call is made per keystroke.
`useRegistryOptions` builds a fresh `useApi` per call and refetches when the
token changes (`useRegistryOptions.ts:24-28`), so nothing is shared today:
the palette calls it once when it first opens and keeps **no module-level
copy** — a sign-out and sign-in as someone else on a shared machine refetches
with the new token, and the previous viewer's registries are never shown.
Only a successful result is kept; opening the palette after a failed load
refetches (the composable gains `useApi`'s `reload`, which it drops
today). A registry
added or removed by a config hot reload appears in the palette after a page
reload, as in every other registry picker.

### 4.5 Mocks

**M1 — sidebar, desktop, phase 1 (an admin holding every verb).**

```text
┌──────────────────────────┐
│ ⛨ ADMIN                  │
│                          │
│ ▦ Dashboard              │
│ PACKAGES                 │
│   All Packages           │
│   Bulk Block             │
│ SECURITY & ACCESS        │
│   Blocks                 │
│ ┃ Access Check           │  ← current: ink + 1px crimson edge
│   Authorization          │
│   Scans                  │
│   Findings               │
│ NAMESPACES & CHANNELS    │
│   Team Namespaces        │
│   Beta Channel           │
│ OPERATIONS               │
│   Upstream  … (7 rows)   │
│ OBSERVABILITY            │
│   Health · Registries ·  │
│   Audit Log              │
│ NOTIFICATIONS            │
│   Subscriptions ·        │
│   Inbound events         │
└──────────────────────────┘
```

It decides that phase 1 changes only visibility: the labels of `en.json`'s
`adminNav`, the same order, the same URLs, so it can ship before anyone
agrees on M3. The section headings are small caps text, not links. The
*Go to…* button above *Dashboard* arrives with the palette in phase 2.

**M1′ — the same, for a security group holding only `quarantine:read` and
`findings:read`.**

```text
┌──────────────────────────┐
│ ⛨ ADMIN                  │
│ SECURITY & ACCESS        │
│ ┃ Scans                  │
│   Findings               │
└──────────────────────────┘
```

It decides that a section shows only the pages the viewer holds, never a
greyed-out row: a disabled entry still announces a page exists, and RFC 0042
§4.3 already chose "absent" over "disabled".

**M1″ — phone, 390 px, phase 1 (proposal; §15 question 11).**

```text
┌──────────────────────────────────────┐
│ ☰ Admin · Security & Access   ▾      │  ← trigger names the section
├──────────────────────────────────────┤
│ (open) Dashboard                     │
│        Packages                      │
│      ┃ Security & Access             │
│        Namespaces & Channels         │
│        Operations                    │
│        Observability                 │
│        Notifications                 │
└──────────────────────────────────────┘
  [ Blocks | Access Check | Authorization | Scans | Findings ]  ← SectionTabs
```

It decides that the expansion is desktop-only. `AdminLayout.vue` draws the
sidebar and the phone menu from one template (`DefineSectionLinks`), and the
phone menu exists because a long link list pushed the page down; expanding
both would undo that. On a phone the pages stay one tap away in
`SectionTabs`, and the palette is the fast route.

**M1 labels in French at `w-52` (208 px).** Labels wrap onto a second line,
never truncate: a truncated label hides the word the operator scans for.

```text
SÉCURITÉ ET ACCÈS          SYSTÈME (M3)
  Blocages                   Rechargement de config
  Vérification d'accès       Bandeau global
  Autorisation               Santé et cache
  Analyses                   Journal d'audit
  Constats                   Abonnements aux
                             notifications
```

The left column is `fr.json`'s `adminNav` today; on the right, *Santé et
cache* and *Abonnements aux notifications* are proposals for M3's renames,
named from behaviour, and become new catalogue keys (§11).

**M2 — palette, nominal (admin, query `npm`).**

```text
┌ Go to… ─────────────────────────────────────────────── Esc ┐
│ > npm▏                                                      │
├─────────────────────────────────────────────────────────────┤
│ REGISTRIES                                                  │
│ ▸ npm                         npm · proxy      Registry hub │  ← active
│   npm-internal                npm · local      Registry hub │
│ ACTIONS                                                     │
│   Recheck an upstream                     Operations › Up…  │
│ ─────────────────────────────────────────────────────────── │
│   Search packages for “npm”                       Catalog → │
├─────────────────────────────────────────────────────────────┤
│ ↑↓ move   ⏎ open   Ctrl ⏎ new tab                           │
└─────────────────────────────────────────────────────────────┘
```

**M2 states.**

```text
empty query      → "Common tasks": the red paths the viewer may open
empty query,     → no "Common tasks" heading at all; the list shows the
 no admin verb      viewer's own pages (Profile, Tokens, Setup) and a hint
                   "Type a package or registry name."
no match         → "Nothing matches “zzz”." + the Search packages row
credential-like  → no Search packages row; "Nothing matches." only — the
 query              pasted token is not turned into a URL
chunk failed     → no dialog; a role=status notice under the header:
                   "The console was updated — reload the page."
registries load  → pages and actions shown at once; REGISTRIES group shows
                   a one-line skeleton, never blocks typing
registries fail  → REGISTRIES group: "Registries could not be loaded." (text,
                   role=status); pages and actions unaffected
refusal (M1′'s   → query "config": "Nothing matches “config”." — the
 viewer)            Configuration page is not listed, and typing its URL
                   still lands on the first Security page the viewer holds
```

It decides that the refusal looks like an absence, not an error: the palette
is not the place to tell someone which pages exist above their verbs.

**M3 — section tree, phase 3 (proposal; §15 question 1).**

```text
Overview        Dashboard
Registries      All registries · ‹registry hub› · Upstream · Warming ·
                Release imports · Air gap
Packages        All packages · Block in bulk · SBOM export
Security        Scans · Findings
Access          Access check · Authorization · Blocked accounts & addresses ·
                Team namespaces · Beta channel
System          Configuration · Global banner · Health & cache · Audit log ·
                Notification subscriptions · Inbound events
```

It decides the grouping question by the trigger: what is broken (a registry),
what is wrong with it (a package, a vulnerability), who is affected
(access), or the instance itself (system). Renames in M3 remove both
collisions of §2.2(4).

**M4 — registry hub, phase 3 (`/admin/registry/npm`, §15 question 8).**

```text
← All registries
npm                                   npm · proxy       [Monitoring ↗]
┌ Upstream ─────┬ Cache ────────┬ Scans ───────┬ Suggestions ────────┐
│ degraded      │ 81 % hit      │ 0 queued     │ 1 → see             │
└───────────────┴───────────────┴──────────────┴─────────────────────┘
Actions  Recheck upstream · Warm · Refresh explorer · Release imports ·
         Edit configuration
[ Overview | Configuration | Activity | Scan ]
```

It decides that actions are links to the existing pages with the registry
preselected, not forms embedded in the hub: one implementation per control.
An action the viewer lacks the verb for is not drawn. With every action
hidden, the strip is absent, not empty.

**M5 — configuration editor reached from the hub.** The editor is the plain
`textarea` of `ConfigEditorCard.vue:142`, with no line gutter.

```text
Configuration
┌──────────────────────────────────────────────────────────────┐
│ [[registries]]                                               │
│ name = "npm"▏         ← caret here, line scrolled into the   │
│ type = "npm"             top third of the textarea           │
│ mode = "proxy"                                               │
└──────────────────────────────────────────────────────────────┘
ⓘ Jumped to registry “npm”.                         (role=status)
```

States:

- **A reload is staged.** The editor is collapsed behind the staged diff
  (`editorOpen` / `hasPending`, `ConfigEditorCard.vue:131-140`); the jump is
  not applied, and the notice reads "A reload is staged — review or discard
  it, then *Edit configuration* again from the registry." No caret is set on
  a hidden textarea.
- **Defined in another layer.** No `name = "npm"` line in the first layer
  (the only one the editor reads, `load_layered`): the caret stays at the
  start and the notice reads "Registry “npm” is defined in another config
  layer; this editor changes only the first file. See *Layered config
  files* in the configuration guide." with the link.
- **Read-only mount.** `ConfigReadOnlyView.vue:35` is a `CodeBlock`, with no
  caret: the block's first line is scrolled into view and highlighted for
  two seconds, and the same `role=status` notice is announced.

All three notices are new catalogue strings in `en.json` and `fr.json`
(RFC 0044 §4.5 specified the staged one but it was never added).

---

## 5. Architecture

```mermaid
graph LR
    S["adminSections.ts<br/>ADMIN_SECTIONS (pages + verbs)"] --> SB["AdminLayout sidebar"]
    S --> ST["SectionTabs"]
    S --> G["router guard<br/>checkMetaGuards"]
    S --> P["CommandPalette"]
    RP["redPaths.ts<br/>actions + keywords"] --> P
    ME["useAuth: /me verbs"] --> SB & ST & G & P
    RG["useRegistryOptions<br/>GET /api/v1/registries"] --> P
    S --> DR["design-routes.mjs<br/>ADMIN_ROUTES"]
```

One table feeds every consumer. Today `ADMIN_PAGE_VERBS`, `ADMIN_SIDEBAR`
and six `*_TABS` arrays are written separately in `adminSections.ts` and
`ADMIN_ROUTES` is a fifth copy in `design-routes.mjs`; §2.2(6) is what
drifting copies produce. Phase 1 replaces them with one
`ADMIN_SECTIONS: { key, label, icon, pages: { to, label, verb, keywords }[] }[]`
from which `ADMIN_PAGE_VERBS`, the sidebar and the tabs are derived, and
against which a test checks `design-routes.mjs`'s list (§6.5).

```mermaid
sequenceDiagram
    participant O as Operator
    participant P as CommandPalette
    participant A as useAuth (can)
    participant R as router guard
    O->>P: Ctrl+K "config"
    P->>A: can("config:read")?
    A-->>P: false
    P-->>O: "Nothing matches"
    O->>R: types /admin/system/configuration
    R->>A: can(meta verb)?
    A-->>R: false
    R-->>O: redirect to first page held
```

The palette filter is a convenience; the guard is the control. Both read the
same table and make the same call — `can(verb)` at the instance tier, never
the per-registry `can(verb, name)` the guard does not use — so the palette
cannot offer what the guard refuses, and if it did, the guard would still
refuse it.

---

## 6. Detailed design

### 6.1 `ui/src/config/adminSections.ts`

- `ADMIN_SECTIONS` as in §5. `ADMIN_PAGE_VERBS`, `ADMIN_SIDEBAR`,
  `firstAdminPage` and the `*_TABS` exports become derived values with the
  same names and shapes, so every file `grep -rl adminSections ui/src`
  lists (29 at `234b095c`) keeps compiling in phase 1.
- Each page gains `keywords: { en: string[]; fr: string[] }` for the palette.
  Keywords are data, not i18n catalogue entries: they are never displayed,
  so the catalogue gate (`i18n-audit.mjs`) has nothing to check.

### 6.2 `ui/src/components/app/CommandPalette.vue` and `config/redPaths.ts`

- Built on `radix-vue`'s `DialogRoot` (already wrapped by
  `components/ui/dialog/Dialog.vue`) and the ARIA 1.2 combobox pattern
  `components/ui/combobox/Combobox.vue` implements: an `input role=combobox`
  with `aria-activedescendant` over a `listbox`. Not a second combobox
  implementation: the list rendering is extracted from `Combobox.vue` if it
  does not already expose it (§15 question 5). The extracted file needs its
  own entry beside `comboboxRoles` in `sonar-project.properties:191-192`,
  whose `resourceKey` names `Combobox.vue` alone; it lands under
  `components/ui/`, where `vue/no-v-html` is off (§7.1 T4).
- Mounted once in `App.vue` beside `AppHeader`; the shortcut listener is a
  single `keydown` on `window`, removed on unmount.
- `redPaths.ts`: `{ id: "RP-1", label, keywords, to, query? }[]`, one per
  §2.1 row. `to` must be a key of `ADMIN_PAGE_VERBS` or a public page
  (§4.3).

### 6.3 Registry-scoped pages read `?registry=`

| Page | Today | Change |
| --- | --- | --- |
| `AdminConfigReload.vue` | ignores it | caret on the `name = "<registry>"` line of the first `[[registries]]` block that has it; RFC 0044 §4.5's text, implemented |
| `AdminUpstream.vue` | `<Select>` | preselect |
| `AdminWarming.vue` | registry field | preselect |
| `AdminImports.vue` | registry list | filter to it |
| `AdminHealth.vue` | per-registry clear | scroll to and focus that registry's row |
| `AdminFindings.vue` | registry `<Select>` (`:297`), RP-4 | preselect |
| `AdminScans.vue` | reads it | unchanged |
| `AdminAccessCheck.vue` | reads it (`:107-110`) | unchanged |

The value is held until the page's list (the content, for the
configuration editor) first settles, then matched against it (§4.2); it is
never interpolated into a request the page would not otherwise make. The
configuration editor's three non-nominal cases — staged reload, block in
another layer, read-only mount — are M5's states.

### 6.4 `ui/src/router/index.ts` and `config/navigation.ts`

Phase 3 moves the routes to M3's paths and adds one `LEGACY_REDIRECTS` entry
per old path that changes. The count follows from the path table of §15
question 8 — the moved pages, the four old section roots that disappear
(`/admin/operations`, `/admin/observability`, `/admin/namespaces`,
`/admin/notifications`, today `SECTION_INDEXES` keys,
`navigation.ts:126-141`), moved from `SECTION_INDEXES` to
`LEGACY_REDIRECTS` (the two stay disjoint, `navigation.test.ts:105`), and
`/admin/security`, whose landing changes. The old
`observability/registries/:name` pattern needs a function in
`handleLegacyPath` because the table is keyed by exact path; it re-encodes
the name with `encodeURIComponent` and returns a path-only location, never
a full URL, so `%2F..%2F` or `//evil.example` in the address bar cannot
become a path elsewhere or an origin. `SECTION_INDEXES` gains the six new
section roots. The existing redirects that point at moved pages are
rewritten to point at the final target directly, so no chain is two hops.

**Hard-coded links.** A `RouterLink` to a path that becomes a
`LEGACY_REDIRECTS` key resolves its `href` statically to "No match found"
(the failure `router/index.ts:304-313` documents): middle-click, new tab and
copy-link break although a plain click redirects. About twenty such links
exist and move with phase 3: `AdminDashboard.vue`, `HomePage.vue`,
`SetupGuide.vue`, `SecurityDashboard.vue`, `AdminHealth.vue`,
`AdminAccessCheck.vue`, `AdminAuthorization.vue`, `AdminPackages.vue`,
`AdminRegistries.vue`, `AdminRegistry.vue`. They take their paths from
`ADMIN_SECTIONS` where they can, and a unit test fails on any `to=` or
`path:` string literal in `ui/src` that is a `LEGACY_REDIRECTS` key.

### 6.5 `ui/build/design-routes.mjs`

`design-routes.mjs` is a plain Node script and cannot import the `.ts` table
(it pulls `@lucide/vue`). `ADMIN_ROUTES` therefore stays a literal and
gains the eight pages of §2.2(6) — one of which is a concrete registry
page, so the hub is not counted twice — and a unit test in
`adminSections.test.ts` reads the file and fails when the literal and the
derived list differ: the copy is kept, its drift is not.
`EXPECTED_COMBINATIONS` (`design-routes.mjs:262`, 31 today) moves in the
same commit.

The gate runs against `ui/build/stub-server.mjs`, which answers 404 for any
path missing from `ui/build/fixtures/captured.json`. The eight new pages'
endpoints (scans, findings, authorization, `admin/registries` and one
registry, upstream, air gap, banner) have no fixture there, so phase 1
captures them from a real backend in the same commit; without that the gate
would measure each page in its error state.

### 6.6 `tests/heavy/console_security.mjs`

Gains the palette measurements of §6.P: open with `Ctrl+K`, type, read the
listed rows' `href`s; one run per token `console_fetch.sh` already provisions.
It also records the API paths the page requests with Puppeteer's
`page.on("request")`, which is the "requests made" source of observations 2
and 4: `console_fetch.sh` keeps `$HEAVY_WORK/server.log` but does not parse
request paths from it, and the browser's own list is per session.

`console_fetch.sh:234` asserts the security group lands on
`/admin/security/scans`. That landing is the first entry of
`ADMIN_PAGE_VERBS` the group holds (`firstAdminPage`,
`adminSections.ts:59-65`), which M3 reorders, so phase 3 updates the
assertion, and `adminSections.test.ts` pins the landing page per verb set.

### 6.P Proof against a real client

The client is a browser: Chrome as pinned by
`.github/actions/headless-chrome` (and the browser sidecar on `9222` locally),
driven by `tests/heavy/console_security.mjs` under `console_fetch.sh`, which
already boots a server with an admin token and an RFC 0042 security-group
token. The observations to produce, none of them run yet:

1. **Admin, sidebar.** `/admin/dashboard` at 1440 px: the sidebar's `a`
   elements are exactly the derived `ADMIN_PAGE_VERBS` keys plus the hub
   list, in M1 order; one carries `aria-current="page"`.
2. **Admin, palette success.** `Ctrl+K`, type `rescan`: the first row is an
   `a` whose `href` is the scans page with `tab=failures`; `Enter` lands
   there, and the page's recorded requests (§6.6) show
   `GET /api/v1/admin/scans` and no other new path.
3. **Admin, carried registry.** The heavy config's registry is
   `npm-${HEAVY_RUN}` (`config.console-fetch.toml:60`). From its hub, *Edit
   configuration*: `document.activeElement` is the editor `textarea` and its
   `selectionStart` is the offset of its `name = "npm-…"` line in the served
   config. The same run with the registries response delayed by two seconds
   still lands the caret (the held value of §4.2).
4. **Refusal.** The security-group token opens the palette and types
   `config`: zero rows but *Search packages*; then `page.goto` on the
   configuration path ends on the first security page it holds, and the
   recorded requests show **no** `GET /api/v1/admin/config*` from that
   session — the refused page never fetched its data.
5. **Shortcut inside the editor.** Focus in the configuration `textarea`,
   `Ctrl+K`: the palette does not open and the textarea receives no
   character.
6. **Bookmarks.** Each old path of the §15 question 8 table, loaded
   directly after phase 3, ends on its M3 path with the query string
   intact.
7. **Unsaved edit.** With an edit typed in the configuration `textarea`, a
   click on another sidebar page (phase 1), then `Ctrl+K` and `Enter` on
   another page (phase 2): the *Discard 1 unsaved edit* confirmation
   appears each time, and *Cancel* leaves the edit in place. Closing the tab
   with the edit still typed raises the browser's own leave prompt
   (`beforeunload`); after the edit is reverted or staged, neither prompt
   appears (§15 question 9).

---

## 7. Security

**Rating: low.** The change moves links. It adds no endpoint, no stored
state and no new read; the risk is that a convenience surface lists more
than the access model allows, or that a registry name becomes markup or a
path.

### 7.1 Threat model

```mermaid
flowchart LR
    subgraph B["browser (untrusted for authz)"]
        P["palette / sidebar<br/>filter by can()"]
        G["router guard"]
    end
    subgraph S["server (trust boundary)"]
        ME["/api/v1/me verbs"]
        RL["/api/v1/registries<br/>accessible_registries_for"]
        AD["/api/v1/admin/* require_verb"]
    end
    P -- "reads" --> ME
    P -- "reads" --> RL
    G -- "navigates" --> AD
```

Everything left of the boundary is presentation: a viewer can edit the
bundle and list every page; the server's `require_verb` is what refuses the
data.

| # | STRIDE | Threat | Mitigation and enforcement | Residual |
| --- | --- | --- | --- | --- |
| T1 | Information disclosure | Palette lists a registry the viewer may not read | Registries only from `GET /api/v1/registries`, filtered by `accessible_registries_for` (`front_office/registries.rs:90`); the palette adds none | Namespace-only grants are invisible to that list (known, `legacy-access-vs-grants`): a registry is *missing*, never extra. Accepted by the author. |
| T2 | Information disclosure | Palette or sidebar lists an admin page above the viewer's verbs | Same `ADMIN_SECTIONS` + instance-tier `can(verb)` as the guard (`checkMetaGuards`); a unit test asserts the palette's admin rows equal `Object.entries(ADMIN_PAGE_VERBS).filter(([, v]) => can(v))` for four verb sets: admin, security group, plain user, and a viewer holding `system:read` on one registry only (no instance verb) | The page *names* ship in the JS bundle anyway; nothing new is learned. |
| T3 | Elevation of privilege | A palette row or hub action reaches a page the guard would refuse | The guard is unchanged and fails closed (a page missing from the table is refused); every row and hub action is gated by the guard's own call, `can(verb)` at the instance tier, never `can(verb, name)`; a row is an `<a href>` from `router.resolve`, so it can only name a route the router knows | A link the guard would bounce is a UI defect, not an access: the server still answers 403. The registry-only test of T2 is what catches it. |
| T4 | Tampering / XSS | A registry name rendered as markup | Rendered with `{{ }}` text interpolation only; the palette has no `v-html`. The `vue/no-v-html` override in `ui/eslint.config.js:52-56` turns the rule off only for `src/components/ui/**` — exactly where the listbox extracted from `Combobox.vue` lands (§6.2) — so the guard is a unit test that renders the **extracted listbox** with a markup-shaped registry name (§14), not lint | A later edit adding `v-html` to the listbox is caught by that test only. |
| T5 | Tampering | A registry name or query string builds a path elsewhere | Hub links use `encodeURIComponent` (the `packagePath` pattern); `?registry=` is matched against the page's own list and otherwise ignored (§6.3); the legacy `observability/registries/:name` redirect re-encodes the segment and returns a path-only location (§6.4) | None. |
| T6 | Spoofing (UI) | A keyboard shortcut steals input in the config editor, causing a mistyped config | Shortcut suppressed in `textarea`/`contenteditable`/foreign inputs (§4.2); §6.P obs. 5 | A browser extension may claim the same chord; then the button still works. |

### 7.2 Authentication and authorisation

No new decision. Who may open what is `require_verb` on the server and
`checkMetaGuards` in the router, both unchanged; the palette reads the same
`can`. Phase 3's moves keep each page's verb: the move is in `ADMIN_SECTIONS`,
which carries the verb with the path, so a page cannot change sections and
lose its gate.

### 7.3 Input handling and injection

The palette's query never leaves the browser except as `/packages?q=`, which
the catalog already accepts from any URL. That URL **persists in the browser
history** (and in a new tab's address with `Ctrl+Enter`), so a credential
pasted into the palette would be kept there; the search row is therefore not
offered for a credential-shaped query (§4.2), and a unit test covers it.
Server-side, a logged request target drops the query string
(`server/src/server_factory.rs:85`) and `Referrer-Policy: no-referrer` is
set (`crates/web/src/middleware/security_headers.rs:19`), so the query
reaches neither a log nor a third party. `?registry=` is compared with
values the page fetched itself.

### 7.4 Secrets and cryptography

None handled. The palette stores nothing (§9), so no token or registry name
reaches `localStorage`.

### 7.5 Abuse and resource exhaustion

One `GET /api/v1/registries` per token, on first open, and again only after
a failure (§4.4). Matching runs over at most a few hundred entries per
keystroke in memory.

### 7.6 Existing defences that still hold

`require_verb` on every `/api/v1/admin/*` route; the fail-closed guard of RFC
0042 §4.3; `accessible_registries_for`; the CSP of `ui/build/csp.ts` (the
palette adds no inline script or external origin).

---

## 8. Supply chain

No new dependency, image, action or tool. The dialog is `radix-vue` 1.9.17
and the combobox pattern is the in-repo `Combobox.vue`, both already in
`ui/pnpm-lock.yaml`; fuzzy-matching libraries (`fuse.js`, `cmdk`-style
packages) were considered and rejected in §12 on size of need. `task
security` is unchanged.

---

## 9. Data and privacy

No new data is stored or transmitted. The palette keeps no history, no
recents and no counters, in memory past closing or in browser storage
(§15 question 4 asks whether recents are worth `localStorage`), and §3 rules
out usage analytics. The registry list it shows is the one the viewer's
pages already fetch.

---

## 10. Operability

### 10.1 Observability

- **Metrics**: none; the change is client-side and makes no new request
  kind. A red path's use is deliberately not measured (§3).
- **Logs and audit trail**: unchanged. Every action a red path reaches is
  already audited by the page that performs it.
- **Alerts**: none, because nothing new can fail on the server.

### 10.2 Failure modes

| Failure | Detected by | Behaviour | Recovery |
| --- | --- | --- | --- |
| `GET /api/v1/registries` fails | the palette | pages and actions still listed; the REGISTRIES group says so (M2); the failure is not cached | reopen the palette: it refetches |
| `GET /api/v1/registries` slow | the registry-scoped page | `?registry=` is held until the list settles (§4.2) | automatic |
| Palette chunk fails to load (redeploy under an open tab) | `defineAsyncComponent`'s `errorComponent` | "The console was updated — reload the page." (M2 states) | reload |
| Navigation away from an unsaved config edit | dirty flag + leave guard (§15 question 9) | confirmation before leaving | *Stay* keeps the edit |
| `/me` not loaded yet | `useAuth.identityReady` | palette shows nothing admin until it resolves — fails closed | automatic |
| A moved page's old URL | `handleLegacyPath` | redirect, query kept | none needed |
| A page added without a section | unit test (§4.3) | build red | add it to `ADMIN_SECTIONS` |

### 10.3 Performance and capacity

The palette is a lazily imported chunk (`defineAsyncComponent`), loaded on
first open, so the initial bundle grows by the shortcut listener only. The
expanded sidebar adds ~23 links to the admin layout; measured by the
rendered gate's existing budgets at four viewports.

---

## 11. Other concerns

- **Accessibility**: WCAG 2.2 AA. The palette is a modal dialog with a
  labelled combobox; focus is trapped and returns on close; results are
  announced through the existing `Announcer` ("4 results"). The shortcut uses
  a modifier, so WCAG 2.1.4 (character key shortcuts) does not apply, and the
  visible button gives a pointer and a touch route. The sidebar's headings
  are not focus stops (M1). All mocks are checked by `ui:design:routes` at
  1440/1024/768/390 once phase 1 adds the missing routes.
- **Internationalisation**: every label in `en.json` and `fr.json`; palette
  keywords in both languages so a French-speaking operator finds "bannière"
  and "débloquer" (§6.1). The renames of M3 get French labels named from
  behaviour (*Blocage de masse* for bulk blocking, per the project's own
  rule).
- **Compliance**: no control touched; RFC 0036's audit trail is unchanged.
- **Licensing**: unchanged.
- **Documentation**: phase 1 corrects "Admin → Explore Cache" in
  `docs/use/package-explorer-cache.md:44` and its French mirror
  `docs/fr/use/package-explorer-cache.md:59`. Phase 3 rewrites **every**
  `Admin →` path and `/admin/` UI path in `docs/` and `docs/fr/` — among
  them `guide/administration*.md`, `configuration.md`'s *Admin → Dashboard*,
  `guide/security-gates.md`, `guide/admin-storage-health.md` and
  `guide/access-control.md` — enforced by a check that greps the docs for
  any `LEGACY_REDIRECTS` key, so a list cannot go stale.
  `guide/administration.md` gains a "Red paths" table — §2.1 as
  instructions.

---

## 12. Alternatives considered

| Alternative | Why rejected |
| --- | --- |
| Do nothing | Every new page deepens §2.2(1); RFC 0044 alone added five, and three of them were never rendered-gated. |
| Only the palette (point 3 alone) | Fixes the operator who knows the word; leaves the one who browses with seven opaque categories, and leaves RP-3's dead hop. |
| Only regroup (point 2 alone) | A better tree still hides 21 pages behind six headings, and a URL migration with nothing shipped first makes the whole RFC wait on M3's naming argument. |
| A top-bar mega-menu | Duplicates the sidebar on desktop and has no good phone form; the mobile admin menu RFC 0003 built is a list of sections, which M1″ keeps. |
| A "quick actions" tile grid on the dashboard | Reachable only from the dashboard; the red paths start wherever the operator is. |
| Server-side search endpoint | A new enumeration surface (§7) to search a static index of about 22 pages and 12 actions plus the viewer's registries. |
| `fuse.js` or a `cmdk` port | Several kilobytes (not measured here) for fuzzy ranking that prefix matching over keywords covers; a dependency to audit for a feature whose index is hand-written. |
| Fold `/security` into the admin | Undoes RFC 0042's reason for it: a delegated group without admin verbs. |

---

## 13. Rollout and compatibility

- **Default behaviour**: all three changes are on with the release that
  ships them; there is no flag, because no change alters what anyone may do.
- **Migration**: phase 3 only, and only URLs: the legacy redirects of the
  §15 question 8 table plus the registry pattern. No schema, config or data.
- **Operator prerequisites**: none. Runbooks quoting admin URLs keep working
  through the redirects; the docs are updated in the same change.
- **Rollback**: phases 1 and 2 roll back by reverting the front. Phase 3
  rolled back after release leaves *new* URLs that the old build does not
  know. The router has no catch-all route, so a bookmark made in between
  matches no route at all: no `adminArea` meta, so neither the admin layout
  nor any page renders — no data is fetched, nothing is exposed, but nothing
  tells the operator either. Phase 3 is therefore **rolled forward**, not
  back; see §15 question 10 for the reverse table that makes a revert safe
  if one is still needed. No risk of §7 reopens.
- **Staged**: the phases are the stages (§16); phase 1 alone is useful.

---

## 14. Test plan

- **Unit** (`ui/src/config/adminSections.test.ts`, new): every `/admin`
  route has a verb; every page is in exactly one section; derived
  `ADMIN_PAGE_VERBS` equals the router's admin **component** routes that
  carry no `meta.verb` (the empty-path and section-root redirects and
  `observability/registries/:name` are excluded); `redPaths.ts` targets all
  resolve; every `LEGACY_REDIRECTS` target is a route; the landing page per
  verb set is pinned; no `to=` / `path:` literal in `ui/src` is a
  `LEGACY_REDIRECTS` key (§6.4).
- **Unit** (`CommandPalette.test.ts`): filtering for four verb sets (T2);
  prefix and accent matching in `fr`; the shortcut is ignored in a
  `textarea` and works in a single-line `input`; `Esc` restores focus; rows
  are `a` elements with a resolved `href`; the registries-failed state and
  the refetch on reopen; sign-out then sign-in as another user shows only
  the new viewer's registries; a credential-shaped query offers no search
  row; the empty query of a viewer with no admin verb draws no heading.
- **Unit** (`AdminConfigReload.test.ts`): `?registry=` puts `selectionStart`
  on the block, including when the content resolves after mount; a block
  in another layer shows that notice; a staged reload sets no caret;
  `ConfigReadOnlyView` scrolls to and highlights the block.
- **Unit** (router): `handleLegacyPath` on
  `observability/registries/%2F..%2Fx` and `…/registries///evil.example`
  returns a path-only location under `/admin/registries/`.
- **Integration**: `AdminLayout.test.ts` renders M1, M1′ and M1″.
- **Proof**: §6.P, observations 1–7.
- **Security**: T1 — palette shows exactly `useRegistryOptions`' list
  (unit); T2 — admin rows equal the guard's set (unit, four verb sets, one
  registry-only); T3 — §6.P obs. 4 (heavy) and the registry-only case
  (unit); T4 — a registry named `<img src=x onerror=…>` renders as text in
  the extracted listbox (unit); T5 — `?registry=../x` preselects nothing
  (unit, per page of §6.3) and the legacy redirect case above; T6 — §6.P
  obs. 5 (heavy).
- **Audit**: `task security` unchanged.
- **Existing suites** that must pass unchanged: `ui:test` (the router guard
  tests prove the fail-closed redirect still holds), `ui:design:routes`
  (now over 23 admin routes, with the fixtures of §6.5), `docs:audience` and
  `docs:structure`. **Changed in phase 3**: `console_fetch.sh:234`, whose
  landing page moves with M3 (§6.6).

---

## 15. Decisions and open questions

### Resolved

| # | Question | Decision |
| --- | --- | --- |
| R1 | Scope | **The three changes of §1, in that order, as one RFC.** The user asked for all three; phasing keeps each shippable alone. |
| R2 | Backend changes? | **None.** Every list the palette needs already has a filtered reader (§4.4). |
| R3 | Package search in the palette | **A hand-off row to `/packages?q=`, not results.** The catalog ranks and pages; a second search would disagree with it. |
| 9 | An unsaved configuration edit is lost on navigation | **In phase 1: the page is dirty while the editor's text differs from the text last loaded from disk or last staged as a pending reload (returning to it clears the flag; a stage or a reload from disk resets the baseline); while dirty, `onBeforeRouteLeave` asks with the house `DestructiveConfirm` and a `beforeunload` handler raises the browser's own prompt, both removed when clean or on unmount. No draft in browser storage: the TOML can hold secrets.** Decided 2026-11-11 with the user; matches the recommendation. |

### Still open

1. **The M3 tree and its names.** Six sections by trigger as drawn, or keep
   seven and only move the misfiled pages (SBOM to Packages, the explorer
   flush named, registries out of Observability)? Recommendation: M3 — the
   misfilings are symptoms of grouping by category, and a smaller move would
   be revisited at the next five pages.
2. **Where the registry-scoped operations live.** In M3 Upstream, Warming,
   Release imports and Air gap are pages under *Registries*, reached from the
   hub with `?registry=`. The alternative is hub tabs that embed them.
   Recommendation: pages + preselection (M4): one implementation per
   control, and the global views (every upstream at once) stay.
3. **A second shortcut.** `/` as in GitHub and many consoles is a character
   key shortcut, which WCAG 2.1.4 requires to be remappable or turnable off.
   Recommendation: `Ctrl/⌘+K` only.
4. **Recents.** Showing the last five destinations needs `localStorage`
   (per-viewer, survives sign-out on a shared machine). Recommendation: no,
   until someone asks; the empty query already shows the red paths.
5. **Reusing `Combobox.vue`.** Its list is internal; extracting it touches
   a component with its own Sonar exception (`comboboxRoles`,
   `sonar-project.properties:191-192`, keyed to `Combobox.vue` alone).
   Recommendation: extract the listbox, keep one ARIA implementation, and
   add a sibling multicriteria entry whose `resourceKey` names the extracted
   file. To be decided before phase 2 starts, since it sets phase 2's file
   list.
6. **`SectionTabs` on desktop** once the sidebar lists every page.
   Recommendation: keep them in phase 1, decide after M1 is used for a
   release.
7. **Red paths without a control today.** Two triggers have no page at all:
   "revoke every token of a user" (only account blocking, RP-8) and "what
   changed in the config last week" beyond `ConfigChangeHistory`.
   Recommendation: list them in the administration guide's red-path table as
   gaps and leave them to their own RFCs.
8. **The phase-3 path table.** M3 names sections but not URLs, and the
   redirect count, observation 6 and `console_fetch.sh:234` all depend on
   them. Proposal, to confirm (22 pages at `234b095c`):

   | Today | M3 |
   | --- | --- |
   | `/admin/dashboard` | unchanged |
   | `/admin/observability/registries` | `/admin/registries` |
   | `/admin/observability/registries/:name` | `/admin/registry/:name` |
   | `/admin/operations/upstream` | `/admin/registries/upstream` |
   | `/admin/operations/warming` | `/admin/registries/warming` |
   | `/admin/operations/imports` | `/admin/registries/imports` |
   | `/admin/operations/air-gap` | `/admin/registries/air-gap` |
   | `/admin/packages/all`, `/admin/packages/bulk` | unchanged |
   | `/admin/operations/sbom` | `/admin/packages/sbom` |
   | `/admin/security/scans`, `/admin/security/findings` | unchanged |
   | `/admin/security/access-check` | `/admin/access/check` |
   | `/admin/security/authorization` | `/admin/access/authorization` |
   | `/admin/security/blocks` | `/admin/access/blocks` |
   | `/admin/namespaces/team-namespaces` | `/admin/access/team-namespaces` |
   | `/admin/namespaces/beta-channel` | `/admin/access/beta-channel` |
   | `/admin/operations/config-reload` | `/admin/system/configuration` |
   | `/admin/operations/banner` | `/admin/system/banner` |
   | `/admin/observability/health` | `/admin/system/health` |
   | `/admin/observability/audit-log` | `/admin/system/audit-log` |
   | `/admin/notifications/subscriptions` | `/admin/system/notifications` |
   | `/admin/notifications/inbound` | `/admin/system/inbound-events` |
   | root `/admin/operations` | → `/admin/registries/upstream` |
   | root `/admin/observability` | → `/admin/system/health` |
   | root `/admin/namespaces` | → `/admin/access/team-namespaces` |
   | root `/admin/notifications` | → `/admin/system/notifications` |
   | root `/admin/security` | stays a section root; lands on Scans, not Blocks |

   That is 17 moved pages, four retired roots and the `:name` pattern: 21
   `LEGACY_REDIRECTS` entries plus one `handleLegacyPath` function. The hub
   is singular `/admin/registry/:name` because a registry may be named
   `upstream` or `warming`, and a static sibling route would shadow its hub.
   An old `/admin/security` bookmark therefore opens Scans; Blocks is one row
   further, under Access. Recommendation: this table.
10. **Phase-3 rollback.** A revert after phase 3 leaves new-path bookmarks
    that match nothing (§13). Recommendation: roll forward; and ship the
    reverse table (each M3 path → its old path) **one release before**
    phase 3, inert until the old paths stop being routes, so that a revert
    of phase 3 alone is still a pure revert. The reverse table passes the
    same no-chain test (`navigation.test.ts:94`).
11. **The phone menu.** `AdminLayout.vue` draws the desktop sidebar and the
    phone menu from one template (`DefineSectionLinks`), so expanding the
    sidebar would expand the phone menu to ~23 rows — the length the
    section menu was built to avoid. Recommendation: M1″ — expansion on
    desktop only; on a phone the menu stays a list of sections, the trigger
    names the section, and `SectionTabs` plus the palette reach the pages.

---

## 16. Implementation phases

| Phase | Content | Useful alone |
| --- | --- | --- |
| 1 | `ADMIN_SECTIONS` as the single table; expanded sidebar on desktop (M1, M1′, M1″); the four §4.3 unit tests; `design-routes.mjs` gains the eight missing pages, checked against the table, with their captured fixtures and `EXPECTED_COMBINATIONS` (§6.5); `?registry=` caret in the configuration editor with M5's states (RP-3, RFC 0044's promise); the unsaved-edit guard (§15 question 9); `docs/use/package-explorer-cache.md` and its French mirror corrected | yes — visibility, the dead hop and the gate gap are fixed with no URL changed |
| 2 | `CommandPalette` + `redPaths.ts` (M2, every state, the chunk-failure notice); the extracted listbox and its Sonar entry (§15 question 5); header and sidebar buttons; `console_security.mjs` observations 2, 4, 5, 7 | yes — zero-guess red paths over the old tree |
| 2½ | The reverse redirect table (§15 question 10), inert | makes a phase-3 revert safe |
| 3 | M3 routes, renames and `LEGACY_REDIRECTS` per the §15 question 8 table; hard-coded admin links moved and their unit test (§6.4); registry hub actions strip (M4) and `?registry=` on the §6.3 pages; `console_fetch.sh:234` updated; docs paths rewritten in `docs/` and `docs/fr/` with the grep check; administration guide's red-path table; observation 6 | completes the regrouping |

The security-relevant property — the palette lists no more than the guard
allows — lands in phase 2 with the palette, tested by the same commit; the
sidebar filter it shares is already enforced in phase 1.
