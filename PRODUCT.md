# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Three real audiences share one instance, and every one of them meets the same web UI.

- **The self-hoster (solo / homelab).** Installs BatleHub once for their own machines and side
  projects. Admin and consumer are the same person. Success is that it keeps working without
  attention; they visit the UI rarely, and when they do it is because something is wrong or they
  need a snippet.
- **The internal platform owner (team of 5–50).** Owns the TOML config, RBAC, quotas and namespaces
  for everyone else. Lives in the admin surfaces, needs to scan state quickly, and is the only
  person who can perform destructive actions (bulk yank/delete, config reload, IP blocks).
- **The developer consuming or publishing packages.** Wants a registry URL, a token, and the right
  snippet for their tool, then to leave. Uses `/explore`, `/packages`, `/setup`, `/tokens`,
  `/my-namespace`.
- **CI pipelines, as a first-class non-human user.** A large share of traffic is build agents
  authenticating with static tokens, Kubernetes service-account tokens, or GitHub/Forgejo Actions
  OIDC. They never see the UI — but explaining and debugging *what CI sees* (access checks, quota
  and rate-limit headers, audit entries, `batlehub-cli`) is a job the UI has to do for a human.

**End-goal audience, not yet served:** corporate / air-gapped platform teams who answer to security
review. Confirmed as a direction, not as a current user. Do not write copy or design decisions that
claim this audience already exists.

## Product Purpose

BatleHub is a self-hosted server that sits between build tools and the internet for **21 package
registry protocols** (npm, Cargo, PyPI, Maven, NuGet, Go, RubyGems, Composer, Conda, Terraform,
Deb, RPM, Pacman, OpenVSX, VS Code Marketplace, JetBrains + JetBrains Marketplace, GitHub, GitLab,
Forgejo, and a generic path-addressed file mirror). Per registry it can act as a caching proxy
(`mode = "proxy"`), an authoritative private registry (`mode = "local"`), or both at once
(`mode = "hybrid"`).

It exists so that a team can cache what it pulls, host what it publishes, and enforce policy on
both, without running a separate service per ecosystem.

Success is that the instance is boring: artifacts resolve fast from cache, publishes work with the
ecosystem's own tooling (`npm publish`, `cargo publish`, `twine upload`, `mvn deploy`,
`dotnet nuget push`, …), and the operator only opens the UI deliberately.

## Positioning

One binary is simultaneously the cache, the private registry, and the policy engine — and the
policy engine is the part a neighbouring product cannot truthfully copy. Every request is evaluated
against per-registry rules before any bytes reach a developer or a pipeline: RBAC by role and OIDC
group, a release-age gate (block anything published less than N seconds ago), deny-`latest`,
blocklists, checksum/integrity verification on fetch *and* on re-serve, and vulnerability flags.
Config is hot-reloadable, so policy changes land without a restart or a dropped in-flight request.

Adjacent products stake out different ground: Harbor owns OCI (explicitly out of scope here),
Artifactory and Nexus are heavyweight and commercially licensed. BatleHub is the single-binary,
open-source, Rust option that covers the long tail of ecosystems — including ones no proxy usually
bothers with, like Pacman, JetBrains plugins, and Conda.

## Operating Context

- **Configuration is a TOML file**, hot-reloaded at runtime. In Kubernetes it is frequently mounted
  read-only from a ConfigMap — the UI already carries a read-only warning for that case, and the
  config editor must degrade to "here is what to change externally".
- **The clients are package managers, not browsers.** `.npmrc`, `.cargo/config.toml`, `pip.conf`,
  `settings.xml`, `NuGet.Config`, `pacman.conf`, IDE plugin-host settings. The Setup Guide's job is
  to emit the correct snippet for a registry the user picked, ready to paste.
- **Auth providers**: static tokens (plain or Argon2id), OIDC (Keycloak, Authentik, Dex),
  Kubernetes service-account tokens, GitHub/Forgejo Actions OIDC with claim-to-group mapping.
  Roles are `anonymous`, `user`, `admin`; an instance may or may not grant anonymous registry
  access, which changes what the first screen even is.
- **Deployment**: container images and a Helm chart; PostgreSQL plus filesystem or S3-compatible
  storage; Prometheus `/metrics` and `/healthz` already exist.
- **Two web surfaces, one product**: the Vue SPA in `ui/` (a task surface — Operate) and the
  VitePress documentation site in `docs/` (a comprehension surface — Read). They currently share
  a hand-copied design language, not a shared token package.
- **Registries can be reached two ways**: `/proxy/{registry}/…` and, since RFC 0001, at the root of
  a dedicated hostname. Any URL the UI shows a user must reflect the ingress they actually used.

## Capabilities and Constraints

- **Stack (existing, not up for decision):** Vue 3 + TypeScript + Vite 8, Tailwind CSS 4,
  `radix-vue` primitives, `@lucide/vue` icons, Shiki for code blocks, pnpm. Vitest + `@vue/test-utils`
  for component tests (39 test files under `ui/src` today) — a rework keeps them green or updates
  them deliberately.
- **The API client is generated, never hand-edited.** `ui/src/client/` is produced by
  `@hey-api/openapi-ts` from `ui/openapi.json` (`task dump-spec` → `task ui:generate`) and is
  git-ignored. UI work does not get to change the API contract by editing the client.
- **Current surface size** (RFC 0050): **21 admin pages** behind `/admin` in seven sidebar
  sections (Dashboard, Registries, Packages, Access, Instance security, System — groups that
  collapse and remember their state), and a separate **security space** at `/security`, with
  its own sidebar (Overview, Triage, Follow-up) and nine routes, so a delegated security group
  reaches it without entering the admin. Both are one shell, `ConsoleLayout`; there are no tab
  strips repeating the sidebar. The public shell has a short top nav plus a user menu.
- **Every string is translated.** `vue-i18n` with `en` and `fr` catalogues
  (`ui/src/locales/`), kept identical in keys and referenced from the source by
  `catalogues.test.ts`; server codes (reason codes, flag effects, audit actions) are drawn as a
  translated label with the raw code beside it.
- **Registry-type knowledge is data, not markup.** `ui/src/config/registryTypes.ts` holds the
  per-type definitions and setup snippets; new registry types are expected to appear there.
- **Undecided:** whether `ui/` and `docs/` should share one published token package or keep
  parallel copies of the same values.

## Brand Commitments

- **The name and wordmark `BatleHub.` are binding**, trailing period included. How it is drawn is
  open.
- **Monofolio lineage is binding.** Monofolio is the author's cross-project design language, used
  here and in `docs/`. A redesign may replace this instance's expression of it, but the result
  must stay recognisably part of that family. Current expression, for reference and as evidence of
  the lineage: OKLCH crimson primary with a copper accent, 2 px corner radius, JetBrains Mono +
  IBM Plex Sans, cyber-grid background, glow utilities (`--cyber-glow`, `--steam-glow`).
- **Existing asset:** `docs/public/logo.svg`.
- **Voice is deliberately not frozen.** Today's copy is terse and technical ("Release age gate",
  "Deny latest tag", "Access Check"); the author declined to make that binding, so tone is an open
  decision rather than a constraint.

## Evidence on Hand

- Real, checked-in: `README.md` (registry and feature matrices), `docs/` (the documentation site and tree, including
  `configuration.md` at 150 kB), `ROADMAP.md`, `CHANGELOG.md`, the RFC series in
  `docs/rfc/`, and `ui/openapi.json` as the authoritative API surface.
- The docs site's existing hero line: *"Your package hub. Proxy, cache, and host."*
- **Absent, and not to be fabricated:** customers, testimonials, case studies, named adopters,
  benchmark numbers, download counts, uptime figures, pricing, and any security certification.
  There are no product screenshots checked into the repository either.

## Product Principles

1. **The instance should be boring; the UI is for the moments it isn't.** Optimise for arriving
   with a question — "why was this blocked", "is the cache healthy", "what do I paste" — not for
   time-on-site.
2. **Every destructive action is reversible or clearly labelled as not.** Bulk yank, delete, IP
   blocks and config reload act on infrastructure other people depend on; the design owes them
   scope, count, and consequence before confirmation.
3. **Show the request, not the schema.** Users think in "my `npm install` got a 403", not in
   registries, rules and roles. Access checks, audit entries and errors should read back in the
   vocabulary of the tool that hit the wall.
4. **Three audiences, one instance — the UI adapts to the identity, not to a persona toggle.**
   Anonymous, user and admin see genuinely different products; the shell should not present
   surfaces the viewer cannot use.
5. **Registry types are data.** Anything that must be edited in 21 places to add a registry is a
   design defect.

## Accessibility & Inclusion

- **Target: WCAG 2.2 AA** — contrast, visible focus, full keyboard operability, and honoured
  `prefers-reduced-motion`. The current theme's glow-and-grid treatment has not been verified
  against AA, and the rework is expected to.
- **French / English internationalisation is required.** This is new capability, not a port: the
  translation layer and message extraction have to be introduced as part of the rework.
