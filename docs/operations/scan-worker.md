---
title: The scan worker
---

# The scan worker

For the operator whose installs are being refused with `SCAN_PENDING`, and for
the one deciding whether to give the worker its own Deployment. The design is
[RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts); this page is what it
looks like from the pod, the queue and the metrics.

---

## What it is

The worker is a **role of the same binary**, not a second program. `[server]
roles` defaults to `["proxy", "worker"]`, so a single-process instance already
runs one, embedded. `batlehub --roles worker` starts a process that only scans,
and `--roles proxy` one that only serves.

The two roles share nothing but the database. The worker answers no HTTP, and
the proxy never blocks a request on it. That is the property to hold on to when
something is wrong: a dead or saturated worker **degrades** the registries it
covers, it does not fail them.

Splitting the roles is what the chart's `worker.enabled` does, and it is worth
doing for two reasons. The scanner toolchains — bubblewrap, `postmortem`, the
Trivy client, optionally GuardDog — live only in the worker image, and only the
worker needs egress to upstream artifacts, the Trivy server and Rekor. See
[Installation](/guide/installation) for the chart values and
[What leaves this instance](/operations/egress) for the egress.

## How a job reaches the queue

The queue is a Postgres table, `scan_jobs`, and it is filled from four places.
The trigger is also the priority: a user waiting on a first request is dequeued
before a background sweep.

| Trigger | Queued by | Priority |
| --- | --- | --- |
| `first_seen` | the proxy, on the first request for a version it has no verdict for | 0 |
| `webhook` | a SOC flag, or an administrator marking a package | 1 |
| `rescan` | the rescan scheduler, and `verdicts rescan` | 2 |
| `backfill` | `verdicts backfill` — every cached version of a registry | 3 |

Enqueue is **idempotent on the coordinate**: a partial unique index allows one
*open* job per version, so a burst of first requests for the same package
creates one job, not a hundred.

The rescan scheduler is one timer for the whole estate rather than one per
process. Leadership is a PostgreSQL advisory lock, every other process's tick
is a no-op, and it looks every minute for verdicts older than the registry's
`rescan.interval_secs`. A batch cap per registry per tick keeps the first tick
on a large registry from queueing the whole table.

## What one pass does

Jobs are **leased, not consumed**. One pass is:

1. **Heartbeat and publish depths.** The worker writes to `worker_heartbeats`
   and sets the queued-jobs gauge. Neither is load-bearing, so neither can fail
   the pass.
2. **Lease a batch** — at most `max_concurrent`, filtered to `[worker]
   registries` when that list is set, taken with `FOR UPDATE SKIP LOCKED` so
   any number of workers can share the queue.
3. **Plan.** The registry's `[registries.security]` profile, its kind, and the
   scanners that apply to that kind. A registry that has left the profile since
   the job was queued drops the job rather than holding the version.
4. **Run each scanner in turn**, heartbeating the lease between them, then run
   the enrichers over what they found.
5. **Record the verdict** and close the row.

Between passes the loop sleeps `idle_poll` only when the queue was empty.

The defaults are four concurrent jobs, a ten-minute `job_timeout`, three
attempts and a two-second idle poll. They live in `[worker]`, documented in the
[configuration reference](/guide/configuration#scanners-and-worker).

### When an attempt does not finish

A lease that expires — the pod was evicted, a hostile archive took the process
out — returns the job to the queue and increments `attempts`. This is normal
and self-healing.

What is not self-healing is a job that exhausts `max_attempts`. Then the row
closes and the verdict is written for it: every scanner the registry names in
`required_scanners` gets a `SCANNER_ERROR` finding saying no attempt completed
in time. What the registry's `scanner_error` setting says then decides whether
the version is held, warned or served.

A scanner that returns an error rather than not returning is different again,
and is not retried: the finding names that scanner, with `SCANNER_ERROR`, or
`SCANNER_UNSUPPORTED` when the scanner needed artifact bytes that could not be
obtained. The distinction is deliberate — an unsupported scan says the scan did
not happen instead of pretending it passed.

## What the scanners run under

Every binary scanner — `postmortem`, `guarddog`, `trivy` — goes through one
runner, and through `bwrap`. The artifact is attacker-controlled input, and the
worker is the one process that opens it while holding database and storage
credentials, so the sandbox is the boundary that matters most in this
architecture:

- new user, pid, ipc and uts namespaces, `--die-with-parent` and
  `--new-session`;
- **no network** unless the scanner declares it needs one;
- the root filesystem bind-mounted read-only, and the per-job work directory as
  the only writable mount, itself `nosuid` and `nodev`;
- an empty environment but for `HOME` and `PATH`, and argv passed directly —
  there is no shell anywhere in this path;
- the memory and CPU limits of `[worker.sandbox]`, and extraction bounded by
  `max_extracted_mb` and `max_entries`;
- stdout read as **untrusted data**: capped, parsed as JSON under a strict
  schema, never interpolated into anything.

`runtime = "none"` runs the bare command. It exists for unit tests and for
sandboxless environments, config validation guards it, and it is not something
to reach for because `bwrap` is missing — the worker image ships it.

## What the proxy does while a version is unjudged

This is the part operators meet first, usually as a failed install.

| The version is | The proxy | The verdict says |
| --- | --- | --- |
| younger than `mature_age_secs`, unjudged | refuses it | `quarantined`, `SCAN_PENDING`, with `available_at` |
| older than `mature_age_secs`, unjudged | serves it, warned | `warned` |
| judged, nothing at or above the threshold | serves it | `allowed` |
| judged against | refuses it | `quarantined` or `denied` |

A hold with a clock lifts by itself and says when. A hold with no clock —
`TIMESTAMP_MISSING`, where the upstream gave no publication date — does not,
and waiting on it never helps.

Users have two commands for this and they are documented in the
[CLI reference](/use/cli#commands-security): `batlehub why` explains a refusal,
`batlehub wait` blocks until a held version becomes servable and exits non-zero
straight away when waiting cannot help. The administrator's side — listing
verdicts by state, rescanning, backfilling, and asking who already pulled a
version — is [`batlehub verdicts`](/use/cli#commands-verdicts).

## When something is wrong

| Symptom | Look at | Do |
| --- | --- | --- |
| Every install on a `[security]` registry is refused `SCAN_PENDING` | `batlehub_workers_live`, and the startup warning naming the registries | Nothing is scanning. Start a process with `--roles worker`, or set `worker.enabled` in the chart |
| Queue depth climbing, jobs eventually complete | `batlehub_scan_jobs_queued` by registry and trigger | Raise `max_concurrent`, or add worker replicas — the autoscaler scales on this metric |
| One version stuck, others fine | `batlehub why <coordinate>`, then the job's `attempts` and `last_error` | Let the attempts run out, or `batlehub why --rescan` after fixing the cause |
| Versions coming back `SCANNER_ERROR` in bulk | `batlehub_scanner_errors_total` by scanner and class | One scanner is down or unreachable. Check its egress and its `[scanners.<name>]` entry |
| Jobs keep returning to the queue without completing | `batlehub_scan_jobs_expired_total`, and the pod's memory | A lease is expiring. Usually the sandbox's `memory_limit_mb` against a large or hostile archive, or `job_timeout` under a slow scanner |
| Two instances, one database, jobs vanishing from one | both processes' `worker_id` in `worker_heartbeats` | The queue is estate-wide by design. Scope each worker with `[worker] registries` if they must not share |

The last row is worth stating plainly: **the queue belongs to the database, not
to the process**. Any worker against the same database can lease any job, which
is what makes horizontal scaling work and what surprises anyone running two
instances against one Postgres.

## What to watch

| Metric | Kind | What it tells you |
| --- | --- | --- |
| `batlehub_workers_live` | gauge | Workers seen in the last two minutes. Zero with a quarantine configured is an outage of the gate |
| `batlehub_scan_jobs_queued` | gauge | Queue depth, by registry and trigger |
| `batlehub_scan_jobs_leased` | gauge | What this worker holds right now |
| `batlehub_scan_job_duration_seconds` | histogram | Per scanner, with an `ok`/`error` outcome |
| `batlehub_scanner_errors_total` | counter | By scanner and error class |
| `batlehub_scan_jobs_expired_total` | counter | Jobs that exhausted their attempts |
| `batlehub_verdicts_total`, `batlehub_verdict_transitions_total` | counters | What was decided, and what changed its mind |

A transition is the one to alert on. A version that was `allowed` and is now
`quarantined` means something arrived after people had already installed it,
and the flip alert carries the list of who pulled it — the same query
[`batlehub verdicts pullers`](/use/cli#commands-verdicts) answers, and the
starting point of the [incident-response](/operations/incident-response) path.

## The two images

`ghcr.io/batleforc/batlehub-worker` carries every scanner but GuardDog.
`ghcr.io/batleforc/batlehub-worker-guarddog` is the same image with GuardDog
added, built on it, and is what `worker.image.repository` points at when a
registry names `guarddog` in its scanners.

Both are scanned on every build and on a daily rebuild, and the scanners they
carry are themselves third-party binaries with their own dependencies — see
[Security scanning](/contributing/security-scanning) for how a CVE in one of
them is handled. Adding a scanner of your own is
[Adding a vulnerability scanner](/contributing/adding-a-vulnerability-scanner).
