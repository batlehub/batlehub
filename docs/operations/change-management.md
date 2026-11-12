# Change Management Policy — BatleHub

This document describes how changes to BatleHub code, configuration, and dependencies are reviewed, approved, and deployed **in this project's own repository**.

::: warning Guidance, not a commitment
Two audiences read this and they need different things from it. If you are
looking for evidence against SOC 2 CC8, this is what the project offers — a
description of its own practice, not an attestation that it has been verified
(see the [SOC 2 mapping](/operations/soc2-checklist)). If you are running
BatleHub, this is a worked example to adapt: your change process is yours, and
nothing here binds the project to a release cadence, a review turnaround or a
deprecation window.
:::

---

## Scope

This policy covers:
- Source code changes (Rust crates, Vue frontend, CLI)
- Configuration changes (TOML config files, environment variables)
- Dependency updates (Cargo.lock, pnpm-lock.yaml)
- Infrastructure changes (container images, Kubernetes manifests, CI/CD pipelines)
- Database schema changes (migrations in `crates/adapters/migrations/`)

---

## Code and Infrastructure Changes

### Standard changes (all non-emergency changes)

1. **Branch** — create a feature branch from `main`.
2. **Develop** — write code locally; run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` before pushing.
3. **Pull request** — open a PR; describe *what* changed and *why*. Link to issue or ticket.
4. **Automated gates** (all must pass before merge):
   - `cargo test --workspace` — unit + integration tests
   - `cargo clippy --workspace -- -D warnings` — no warnings
   - `cargo fmt --all --check` — formatting
   - `cargo audit` — no unpatched RUSTSEC advisories
   - `cargo deny check` — license, ban, and source policy
   - `pnpm audit --audit-level high` — no high/critical JS CVEs
   - `gitleaks` — no secrets in diff
   - `trivy` — no fixable HIGH/CRITICAL in built image
5. **Peer review** — at least one approved review required.
6. **Merge** — squash-merge to `main`.
7. **Deploy** — CI builds and pushes the image; deployment pipeline applies to staging first, then production.

### Emergency changes (P0 incident)

When containment requires an immediate patch to production:

1. Implement fix on a branch; at minimum self-review the diff.
2. Run `cargo test --workspace` locally — do not skip tests.
3. Fast-track automated gates (CI still runs; do not bypass).
4. Notify the security lead and on-call that an emergency merge is in progress.
5. Merge and deploy.
6. Open a follow-up PR within 24 hours with a retroactive post-mortem comment.

---

## Configuration Changes

### Runtime config (TOML / env vars)

All configuration changes that affect runtime behavior (new registries, RBAC rule changes, rate-limit overrides, IP block additions) must be:

1. **Reviewed** — at least one other person must see the diff.
2. **Applied via config reload** where possible — use `POST /api/v1/admin/config/reload` to hot-swap the config without a restart. BatleHub logs and stores every reload in the `config_changes` table.
3. **Auditable** — config change history is queryable:

   ```bash
   batlehub-cli server config changes
   # or via Admin UI → System → Configuration → History
   ```

4. **Reverting** — keep the previous config version in version control. Roll back by deploying the prior commit.

### Database migrations

SQL migrations live in `crates/adapters/migrations/` with sequential numbers. Rules:

- Never modify an existing migration (it may already be applied in production).
- New migrations must be additive where possible (add columns, not drop them).
- All migrations run automatically on server startup via the embedded migrator.
- Test migrations with `task test:pg-cache` before merging.

---

## Dependency Updates

### Routine updates

- Dependabot / Renovate opens PRs automatically for patch and minor bumps.
- Review the changelog for any breaking changes or security implications.
- Verify `cargo audit` and `cargo deny check` still pass after the update.

### Security patches

When a RUSTSEC advisory is published for a dependency we use:

1. BatleHub's CI `back-dep-audit` job will fail within 24 hours of the advisory being published.
2. Check if the advisory is in the direct or transitive dependency tree: `cargo tree -i <crate>`.
3. Bump the dependency version (or apply a `[patch.crates-io]` stub as done for `sqlx-macros` and `sqlx-mysql`).
4. Verify `cargo audit` and `cargo deny check` pass locally.
5. Open and fast-track the PR (standard review still required).

### Prohibited operations

| Action | Reason |
|--------|--------|
| `features = ["macros"]` on `sqlx` | Pulls in `rsa` crate (RUSTSEC-2023-0071) |
| Default features on `aws-sdk-s3` or `aws-config` | Pulls in legacy `rustls` (RUSTSEC-2026-0098) |
| `advisories.ignore` or `.cargo/audit.toml` suppressions | No suppressions policy — fix or patch instead |
| `--no-verify` on commits | Bypasses pre-commit hooks |

These are enforced by `cargo deny` rules in `deny.toml` and will fail CI.

---

## RBAC Policy Changes

Changes to per-registry RBAC rules affect what packages users can and cannot download. Before applying:

1. Use the RBAC simulator to validate the intended effect:

   ```bash
   batlehub-cli authz check npm/lodash@4.17.21 --resource releases:read --role user
   ```

2. Test with at least one "deny" case and one "allow" case.
3. Apply via config reload (changes are logged automatically).
4. Monitor the audit log for unexpected `denied` events after applying.

---

## Audit Trail

The following operations are automatically logged in the `access_events` table (with user identity, IP address, and user-agent):

- Package downloads (allowed and denied)
- Package publishes, yanks, deletes, blocks, unblocks
- Admin token revocations

Config changes are logged in the `config_changes` table.

To export all audit events for a period:

```bash
batlehub-cli audit export \
  --from 2026-01-01T00:00:00Z --to 2026-03-31T23:59:59Z \
  --format csv --output q1-2026-audit.csv
```

---

## Annual Review

This policy should be reviewed at least annually by the security lead and updated to reflect changes in tooling, team size, or compliance requirements.

Last reviewed: 2026-06-28
