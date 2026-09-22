# SonarCloud triage — 2026-09-22

PR #195 (`feat/devfile`, RFC 0035), analysed at `e5f941e4`. The gate was red
on **duplicated lines on new code, 4.5 % against 3.0 %**, the first time this
condition has failed. Ten code smells were open beside it. Companion to
[`sonar-triage-2026-09-19.md`](./sonar-triage-2026-09-19.md).

Everything is fixed in code: `sonar-project.properties` does not change, and
`sonar.cpd.exclusions` is not widened.

## Duplication (200 lines, two files)

| File | Duplicated / new | What it was | What changed |
| --- | --- | --- | --- |
| `crates/web/tests/authz_matrix.rs` | 59 / 103 | fifteen devfile `Row`s, eight whole-registry and seven version-resolved, each spelled out with the same `vis(…)` and `no_control()` | `devfile_rows()` builds them from two path arrays; `matrix()` extends with it |
| `crates/web/src/handlers/proxy/devfile.rs` | 141 / 767 | the handler signature and opening lines (`map: web::Data<RegistryMap>` … `require_registry_type(&registry, KIND, &map)?`) matched the same run in ~50 other handler files | removed from every devfile handler, and the index macro; `devfile_oci_ping` takes no arguments |

The `require_registry_type` calls were redundant, and this is not only a way to
satisfy the metric. Every devfile route carries the `is_devfile` guard, which
reads the registry name from the same path segment and checks
`map.is_type(r, "devfile")` before the route matches. So a request for any other
kind never reaches a devfile handler. The guard is also stricter than the check
it replaces: it reads the raw path, so a percent-encoded registry name does not
match. The module doc says this now.

The utoipa blocks are still shaped like every other handler's. That is the
floor for any new registry, and it is why the other kinds' files show up as
the other side of each duplicated block.

## Code smells (10)

| Rule | Count | Where | What changed |
| --- | --- | --- | --- |
| `shelldre:S7682` explicit return | 5 | `devfile.sh` `rl`, `hits`, `code_of`; `devfile_che_canary.sh` `resolve`, `tile_link` | `return $?`, as in the 09-19 round |
| `shelldre:S7679` positional parameter | 4 | `devfile.sh` `code_of`; `devfile_che_canary.sh` `tile_link` | named `local`s (`verb`/`url`, `root`/`self`) |
| `shelldre:S1192` duplicated literal | 1 | `airgap.sh`, `'devfile-synth'` ×4 | `DEVFILE_MARK` |
