# Disaster Recovery

This document describes what data lives where, how to back it up, and how to restore a BatleHub instance after a failure.

::: warning Guidance, not a commitment
The data inventory is a fact about the software; the procedures are a starting
point for yours. Backups are not taken for you, no recovery point or recovery
time objective stated here is guaranteed, and the restore path below is only as
good as the last backup you actually verified.
:::

## Data inventory

| Store | What lives there | Loss impact |
|-------|-----------------|-------------|
| PostgreSQL | Package metadata, access events, quota, rate-limit counters, SBOM records, config change history, user/IP blocks, notification subscriptions | All package listings, audit log, and policy state lost |
| Artifact storage (S3 / filesystem) | Cached proxy artifacts and published local-mode artifacts | Cached proxy artifacts re-fetched on demand; **local-mode artifacts are permanently lost** unless a separate backup exists |
| Redis (optional) | Metadata cache, rate-limit counters (short-lived) | Harmless: server refills automatically on restart |

## Backup strategy

### PostgreSQL

Schedule `pg_dump` to a durable location (e.g. S3 bucket with versioning enabled):

```bash
pg_dump -Fc "$DATABASE_URL" > batlehub-$(date +%Y%m%d%H%M%S).pgdump
```

Recommended retention: 7 daily + 4 weekly + 12 monthly snapshots.

For WAL-based continuous backup use `pgBackRest` or `Barman`. Point-in-time recovery allows you to restore to any second, not just snapshot boundaries.

### Artifact storage

**S3:** Enable S3 versioning on the bucket and configure lifecycle rules to retain deleted objects for 30 days. Use Cross-Region Replication for a warm standby.

**Filesystem:** Run `rclone sync` to a remote destination on a schedule, e.g.:

```bash
rclone sync /var/lib/batlehub/artifacts s3:my-backup-bucket/batlehub-artifacts \
  --transfers 16 --checksum
```

## Restore procedure

### Scenario 1 — DB corruption or accidental table drop

1. Stop all BatleHub instances to prevent further writes.
2. Create a fresh database: `createdb batlehub`.
3. Restore: `pg_restore -d batlehub batlehub-YYYYMMDDHHMMSS.pgdump`.
4. Restart BatleHub — it will run migrations automatically on startup.
5. Verify with `batlehub-cli registry list`.

### Scenario 2 — S3 bucket lost (cached artifacts only, no local-mode)

This is survivable with no data loss for proxy-mode registries. Cached artifacts are re-fetched from upstream on the next request.

1. Create a new bucket, update `[storage] bucket` in config.
2. Trigger a config reload: `batlehub-cli server config reload`.
3. Optionally pre-warm critical packages: `batlehub-cli cache warm -r <registry> --packages <name>`.

### Scenario 3 — S3 bucket lost (local-mode artifacts present)

Local-mode artifacts are not re-fetchable. Without a bucket backup, those artifacts are gone.

1. If a bucket backup exists: restore with `rclone sync s3:my-backup-bucket/batlehub-artifacts /restore/`.
2. Point the new bucket at the restored data, or configure filesystem storage pointing at the restored path.
3. Restart BatleHub.

Without a backup, re-publish the affected artifacts using `batlehub-cli publish` or the registry-specific client.

### Scenario 4 — Full disaster (DB + storage lost)

1. Provision a new Postgres instance and restore from the latest `pg_dump`.
2. Provision a new storage backend and restore from the artifact backup.
3. Deploy BatleHub with the restored DB URL and storage config.
4. Run `batlehub-cli cache warm` for each registry to avoid a cold-start miss storm.

## Cross-region failover

BatleHub supports active/passive failover with a read replica and a standby storage bucket:

1. **Promote the replica DB**: `pg_ctl promote` (Postgres 12+) or use the managed-DB failover UI.
2. **Switch the storage bucket**: update `[storage] bucket` to the replica region bucket (populated by CRR).
3. **Update DNS** to point `batlehub.example.com` at the standby region.
4. **Restart** BatleHub instances in the standby region.

Expected RTO with a pre-warmed standby: < 5 minutes. RPO depends on replication lag (typically seconds for WAL streaming, minutes for scheduled pg_dump).

## Pending publish orphans

If a BatleHub process dies during a publish, the `local_packages` table may contain rows with `status = 'pending'`. These are cleaned up automatically by the background `spawn_pending_publish_cleanup` task (hourly by default). To clean up immediately:

```sql
DELETE FROM local_packages WHERE status = 'pending' AND created_at < now() - interval '2 hours';
```

Or trigger via the API: restart the server (the task runs on startup tick after the first interval).

## Runbook checklist

- [ ] Postgres backup job runs daily, retention ≥ 7 days
- [ ] S3 versioning enabled on artifact bucket
- [ ] Artifact rclone sync runs at least hourly for local-mode registries
- [ ] Recovery procedure tested quarterly (restore to staging)
- [ ] Alert on backup job failure (e.g. CloudWatch, Prometheus alertmanager)
- [ ] `deploy/prometheus-alerts.yaml` loaded — `BatleHubDown` alert active
