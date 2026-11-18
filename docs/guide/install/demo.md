# Deploying the demo

One command stands up a complete BatleHub on one machine, from a checkout of
the repository: the server, its database, a scanner, five registries, six
personas — and real traffic, so that every page of the console has something
to show, including the states an evaluation rarely reaches on its own: a
version refused for malware, a flag from a SOC on a version already
downloaded, a registry nobody protected, a static token kept in clear.

It is a demo, not an installation. For a server you will keep, see
[Installation](/guide/installation).

## Prerequisites

- a checkout of the repository, and [mise](https://mise.jdx.dev) with the
  repository's tools installed (`mise install`): Rust, Node and pnpm come from
  it, and so does PostgreSQL when none is running;
- `python3` and `curl`;
- the network: the demo proxies npmjs.org, pypi.org, crates.io and
  proxy.golang.org;
- a few minutes for the first build, and the disk space of a debug build under `target/`.

PostgreSQL is optional. The script uses, in this order, `$DATABASE_URL`, a
server on `127.0.0.1:5432` (`batlehub` / `changeme`, the development
container's), or starts its own PostgreSQL 17 on `127.0.0.1:5433` in
`~/.local/share/pg-fallback`, without root.

## Starting it

```sh
scripts/demo.sh start
```

The script builds the server, the CLI and the console, writes its config to
`tmp/demo/config.toml`, starts the scanner and the server, seeds the data,
restarts the server once, and prints the address and the tokens. Everything it
writes lives under `tmp/demo/`, which git ignores.

The console is served by the server itself, on the same origin as the API:
`http://127.0.0.1:8080`. In a cloud development environment, port 8080 is the
one the workspace exposes, and the script prints its public address too.

| Command | What it does |
| --- | --- |
| `scripts/demo.sh start` | build, start, seed |
| `scripts/demo.sh start --reset` | the same on an empty database and an empty cache |
| `scripts/demo.sh start --no-build` | reuse the binaries and the console already built |
| `scripts/demo.sh start --no-seed` | start without traffic |
| `scripts/demo.sh seed` | add one more round of traffic to a running demo |
| `scripts/demo.sh status` | which of the server and the scanner run |
| `scripts/demo.sh logs` | follow the server's log |
| `scripts/demo.sh stop` | stop the server and the scanner; the database stays |

| Variable | Default | Meaning |
| --- | --- | --- |
| `DATABASE_URL` | — | a PostgreSQL to use; the script creates `batlehub_demo` beside the database it names |
| `DEMO_PORT` | `8080` | the server's port |
| `DEMO_OSV_PORT` | `8197` | the scanner's port |
| `CARGO_BUILD_JOBS` | `2` | parallel compile jobs |

## What it stands up

**A scanner that always answers the same.** The demo's OSV replays answers
api.osv.dev gave for a set of real packages, recorded in
`tests/fixtures/supply-chain-jdd/osv/`. The findings are real advisories —
`event-stream@3.3.6`, `ua-parser-js@0.7.29`, `minimist@1.2.5`,
`lodash@4.17.20` — and the demo holds and refuses the same versions on every
run, whatever the live database says that day. A package the recordings do
not name is clean.

**Five registries, one per security state:**

| Registry | Kind | Protection | What it shows |
| --- | --- | --- | --- |
| `npm-public` | npm | refuses *high* and above; receives the SOC's flags | holds for malware and vulnerabilities, a flag that blocks downloads |
| `crates-io` | cargo | refuses *high* and above, with a `cve_gate` | the *Gates* column of Coverage |
| `pypi-public` | PyPI | warns: a finding at *critical* is served with a warning, never refused; known malware still is | a registry that serves with a warning |
| `npm-young` | npm | holds every version on its age | holds waiting on `MIN_AGE_NOT_MET` |
| `go-public` | Go | none | Coverage's *not protected* row, and its `ATTENTION` line |

**Six personas**, each a token to sign in with:

| Token | Who | What they see |
| --- | --- | --- |
| `demo-admin-token` | an administrator | everything, the admin dashboard and Instance security |
| `demo-secops-token` | a delegated security team (RFC 0042) | all of `/security`; of the admin pages, only the audit log |
| `demo-soc-token` | a SOC that reads flags only | `/security`: the overview and *Flags* |
| `demo-auditor-token` | an auditor | `/security` opens on *Exposure*; the audit log |
| `demo-user-token` | a developer | the catalogue; no `/security`, no `/admin` |
| `demo-ci-token` | a CI job | a plain user, whose token is hashed in the config |

The administrator's token is stored in clear on purpose: it raises the
`auth.plain-static-token` warning that Instance security reports. The CI
token shows the other case — an Argon2id hash, no warning.

## What the seeding does

Every step is a real request through the server; nothing is written to the
database by hand.

1. **First downloads** on all five registries. Each version of a protected
   registry is held `SCAN_PENDING` and queued.
2. **The worker scans** — the script waits until the queue is empty.
3. **Second downloads.** The versions with a finding are refused now: these
   are the refusals Trends and Exposure count.
4. **A SOC pushes two signed flags**: a `hard_block` on `left-pad@1.3.0`,
   which was already downloaded and is refused when asked for again, and a
   `warn` on `minimist@1.2.8`, which was downloaded before the flag and three
   more times after it — a warning still serves, and only a download that was
   served counts as one.
5. **24 rejected credentials**, from tokens nobody issued.
6. **A manual ban** of `203.0.113.7`, a documentation address.
7. **An exemption** on `minimist@1.2.5` that ends in three days.

The server is then restarted once: the daily security rollup runs at
startup, so `/security/trends` has a first day.

## A tour

Signed in as `demo-secops-token`:

- `/security` — the status strip: *Holds*, *Flags* and *Coverage* at
  `ATTENTION`, *Scanners & worker* `OK`; under it, *Needs a decision* names
  `minimist@1.2.8` downloaded after its flag, the expiring exemption and the
  holds behind a hard block.
- `/security/holds`, `/security/findings` — the reason codes as labels, the
  raw code beside each.
- `/security/flags`, `/security/exposure` — who pulled `left-pad@1.3.0` before
  its hard block, and `minimist@1.2.8` before and after its warning.
- `/security/coverage` — four registries protected, `go-public` not;
  `/security/coverage/npm-public` details its profile.
- `/security/scanners` — the worker, the queue, the `osv` scanner's last
  success.
- `/security/trends` — the first day; switch to *Show as table*.

Signed in as `demo-admin-token`:

- `/admin/dashboard` — the cache sentence, then one line per security domain,
  and *Config* with the security warnings.
- `/admin/instance-security` — the rejected credentials, the ban, the token in
  clear and how to hash it.

Signed in as `demo-soc-token`, open `/security/holds`: you land on
`/security/flags`, the one page of that group the token reads.

From a terminal:

```sh
BATLEHUB_SERVER=http://127.0.0.1:8080 BATLEHUB_TOKEN=demo-secops-token \
  target/debug/batlehub-cli security status; echo "exit $?"
```

`1` means a line is at `ATTENTION`, `7` that a line is `UNKNOWN` or not
readable by the token, and `ATTENTION` wins. With `demo-soc-token` only the
*Flags* line prints, a `not shown:` line names the other three, and the exit is
still `1`: the hard block puts *Flags* at `ATTENTION`.

## What the demo does not show

- **An automatic ban.** The threshold is set far above what the seeding sends:
  behind a development environment's ingress every browser shares one
  address, and an automatic ban would lock the demo out.
- **A token used from a new address.** That signal is about the user tokens a
  person creates after signing in through single sign-on, which the demo does
  not configure.
- **History before the first run.** Held versions are counted from the day the
  demo starts; refusals, flags and scans from as far back as their own
  records go, which is the seeding's first minute.

## When it goes wrong

| Symptom | Where to look |
| --- | --- |
| `cargo build failed` | `tmp/demo/build-rust.log` |
| `the console build failed` | `tmp/demo/build-ui.log` |
| `port 8080 is taken` | another server runs there: `DEMO_PORT=8090 scripts/demo.sh start` |
| `the server exited` | `tmp/demo/server.log` — usually the database or a config key |
| the queue did not drain | the worker logs in `tmp/demo/server.log`; the overview still shows what is left |
| downloads answer `0` or `502` | the network: the demo needs the four public registries |

## Removing it

```sh
scripts/demo.sh stop
rm -rf tmp/demo
```

The `batlehub_demo` database stays; `--reset` drops it on the next start. A
PostgreSQL the script started itself stops with
`mise x conda:postgresql@17 -- pg_ctl -D ~/.local/share/pg-fallback/data stop`.
