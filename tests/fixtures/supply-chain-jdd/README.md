# Supply-chain test data (JDD)

Packages a gate must refuse or serve, one per failure shape of RFC 0041
(`docs/rfc/0041-test-depth-and-security-gate-scale.md`). Two halves:

| Path | For | Used by |
| --- | --- | --- |
| `cases.json` | the worker: each purl, what it exercises, the verdict expected once RFC 0041 phase 1 lands | `probe_osv_severity.py` |
| `osv/<purl>.json` | the worker: api.osv.dev's `/v1/query` answer for each case, recorded verbatim (date inside) | `probe_osv_severity.py`, a fake OSV replaying them |
| `lockfiles/npm/*.jdd` | CI: a project pinning a malicious, two vulnerable and one clean npm package | postmortem, materialised by hand (below) |

## Why the `.jdd` suffix

`package.json.jdd` and `package-lock.json.jdd` are not named as manifests on
purpose. Under their real names, GitHub's dependency graph and Dependabot
would read them as this repository's dependencies and raise alerts for a
malicious package and two vulnerable ones, and a scanner pointed at the tree
would gate on them. Nothing installs from them: a lockfile is read, never
resolved. Copy them out under their real names to use them:

```bash
d=$(mktemp -d)
cp lockfiles/npm/package.json.jdd "$d/package.json"
cp lockfiles/npm/package-lock.json.jdd "$d/package-lock.json"
postmortem tree "$d" --vulns --fail-on-vuln high   # needs VULN_MLAB_TOKEN, or 8 scans/hour
```

## Running the worker probe

```bash
uv run --with cvss==3.6 python3 -I probe_osv_severity.py cases            # recorded answers, offline
uv run --with cvss==3.6 python3 -I probe_osv_severity.py cases --record   # refresh from api.osv.dev
uv run --with cvss==3.6 python3 -I probe_osv_severity.py purls <file>     # any purl list, live
```

It prints, per purl, the verdict under the default profile three ways:
`today` (as shipped), `draft` (RFC 0041 as first written) and `rfc` (with
decision 21: a curated GHSA rating wins over a vector within a group of
twins, an unrated record takes its group's rating, informational RUSTSEC
records are `low`, withdrawn ones are ignored). A case whose `rfc` verdict
differs from `expect` is marked `✗` and the script exits 1. With the
recorded answers, 3 of the 18 change verdict against today: the two `MAL-`
packages and `golang.org/x/net@0.22.0` (decision 22).

## The cases, and what they showed (2026-11-07)

| Purl | Shape | Today |
| --- | --- | --- |
| `npm/ynodejs_mod_log@10.0.1` | `MAL-` only, no severity | **served** |
| `pypi/etherreeum@1.0.0` | `MAL-` only, no alias | **served** |
| `cargo/rustdecimal@1.23.1` | `MAL-` plus a GHSA rated HIGH | refused (the GHSA) |
| `npm/ua-parser-js@0.7.29`, `event-stream@3.3.6`, `node-ipc@10.1.1` | hijacked/backdoored, GHSA rated | refused |
| `npm/minimist@1.2.5`, `lodash@4.17.20` | GHSA CRITICAL / HIGH | refused |
| `pypi/idna@3.6` | GHSA MODERATE, PYSEC twin with an NVD vector scoring 7.5 | served; twins disagree |
| `cargo/rsa@0.9.6` | GHSA MODERATE/LOW, RUSTSEC vector-only (5.9) | served |
| `golang/golang.org/x/net@0.22.0` | 4 GHSA MODERATE, 21 `GO-` with no severity, 17 of them with no rated twin | served |
| `cargo/memmap2@0.5.10` | RUSTSEC `unsound` only, informational (a dependency of this repository) | served; stays served (decision 21) |
| `npm/left-pad@1.3.0`, `minimist@1.2.8` | clean | served |

A `MAL-` package is served today because OSV gives its record no severity and
`severity_of` (`crates/adapters/src/vulnerability/osv.rs`) maps that to
`unknown`, which no threshold reaches. Observed with a real npm 11 against a
real server on 2026-11-07: the recorded `MAL-2023-1000` answer replayed for
`left-pad@1.3.0` was stored `VULNERABILITY / unknown`, the verdict was
`allowed`, and `npm ci` installed the tarball (200).

The same is true in CI: postmortem 2.9.2 ranks `MAL-2023-1000` `Medium`
(SARIF level `warning`), so `fail-on-vuln: high` passes a project whose only
problem is that package.
