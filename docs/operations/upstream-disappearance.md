---
title: Upstream disappearance
---

# Upstream disappearance

For the operator whose build broke because a package left its upstream, and
for the one deciding whether this instance should notice before a build does.
The design is [RFC 0014](/rfc/0014-upstream-disappearance); this page is what
it looks like from the console, the log and the wire.

---

## What the audit does

A periodic sweep asks each proxy or hybrid upstream whether the artifacts
cached from it are still there. A miss is recorded, not believed: a
disappearance is **confirmed** only after `confirm_after` consecutive sweeps
have missed it, at least `confirm_min_age_secs` apart, in sweeps where the
registry's other packages were still answering. An outage, a rate limit or
an expired credential affects nearly everything at once, so a sweep in which
more than `outage_ratio` of a registry's packages go missing is **void** and
records nothing.

On confirmation the instance does three things, whatever the policy:

- keeps the row — `disappeared`, with when it was first missed and how many
  sweeps confirmed it;
- **holds the artifact back from eviction** and re-pins its cached metadata
  each sweep, so the last copy in the estate is not garbage-collected
  precisely because upstream stopped refreshing it;
- tells whoever subscribed: a `package_disappeared_upstream` notification
  with the coordinate, the misses, the probe rung and the policy.

A package that answers again clears the row and sends
`package_reappeared_upstream`. A sweep voided by the ratio sends
`upstream_unreachable` for the registry, so a registry that is void every
cycle is itself an alert rather than a silent gap.

## The two policies

`[upstream_audit] on_confirmed` is `"audit"` or `"block"`.

| | `"audit"` (default) | `"block"` |
| --- | --- | --- |
| Row, hold, notification | yes | yes |
| Serving | unchanged | **refused**: every held version of the name is blocked through the admin block list, `blocked_by = system:upstream-audit` |
| Reappearance | row cleared | row cleared **and the audit's own block lifted** — an admin's block, or one an admin edited, stays, and the event says `unblock_skipped_reason: blocked_by_admin` |
| What a false confirmation costs | an admin reads a wrong alert | the estate blocks a package against itself |

The last row is why the default is `"audit"`. Under `"block"` the report is
the weapon: an attacker who can serve selective 404s to this instance —
control of the path to the upstream, held for the whole confirmation window,
narrowly enough not to trip the ratio — gets a targeted denial of service
against a package the estate depends on. That position already lets them
serve fabricated metadata on a cache miss, so the capability is not new; the
*cost* is. Choose `"block"` for an estate whose threat is a withdrawn or
hijacked package reaching a build, and accept that a sustained, narrow
interference with one upstream can take a package away from you.

Turning `"block"` off unblocks nothing: existing blocks are administrative
state and stay until an admin lifts them or the package reappears. The
console's block table filters on `system:upstream-audit`, so lifting them in
bulk is one filtered selection.

## Reading the console

- **Health** — the card names the active policy and, per registry, how many
  packages are `missing` (seen once) and `disappeared` (confirmed).
- **Operations → Upstream** — the table: every row, filterable by registry
  and state, with the first miss, the last check and the confirmation time.
  *Recheck* probes one package now, through the same ladder and state
  machine as the sweep. It is a probe, not an override: it cannot confirm
  early and cannot force a confirmation, but an admin who has heard from
  upstream does not wait an interval to see the row clear.
- **A package's page** — a badge on each affected version: *missing
  upstream* or *disappeared upstream*, and under `"block"`, *blocked by the
  upstream audit*.

## Reading the log and the metrics

A confirmation is logged at `WARN` with the coordinate and the misses; a
void sweep at `WARN` with the counts. `on_confirmed = "block"` is logged
once at `INFO` at startup, naming the actor the blocks will carry.

| Metric | Says |
| --- | --- |
| `batlehub_upstream_missing_total{registry}` | rows seen missing, unconfirmed |
| `batlehub_upstream_disappeared_total{registry}` | rows confirmed |
| `batlehub_upstream_audit_sweeps_total{registry,outcome}` | `ok` / `void` — a rising `void` rate is the feature failing, not the upstreams |
| `batlehub_upstream_audit_duration_seconds{registry}` | how long a sweep takes |

## The API

```text
GET  /api/v1/admin/upstream/disappeared?registry=&state=&page=&per_page=
GET  /api/v1/admin/upstream/status/{registry}/{name}
POST /api/v1/admin/upstream/recheck        { "registry", "package_name", "version"? }
```

All three take `system:read` (the listing and the status) or `system:write`
(`recheck`). The listing pages by `LimitsConfig.packages_per_page`.

## What the audit cannot see

The path-addressed kinds — `deb`, `rpm`, `pacman`, `jetbrains`, `generic` —
have no package identity to ask about and their client answers a probe
without asking upstream, so a vanished file is not detected. The forges
(`github`, `gitlab`, `forgejo`) are proxied by path too. For every other
kind the sweep reaches the listing document where there is one and probes
per version where there is not, at most 25 versions per package per sweep.

The block arm blocks the versions the estate *holds*. It cannot block a
name against re-registration: nothing has blocked a version that does not
exist yet. That is RFC 0002's `version = "*"` flag, pushed by whoever
watches the name.

## Configuration

The section, its floors and its warnings are in the
[configuration reference](/guide/configuration#38c-upstream_audit-optional).
