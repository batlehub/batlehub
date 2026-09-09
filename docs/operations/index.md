# Operations

For the person on call, and for the auditor.

Everything here assumes BatleHub is already running. If you are still setting it
up, start with [Installation](/guide/installation) and
[Configuration](/guide/configuration).

::: warning Guidance, not a commitment
BatleHub is self-hosted software under the Apache 2.0 licence, and **you are the
operator**. Every page in this space is material to help you run your own
instance: what the software makes possible, what the project itself does, and
what a sensible procedure looks like.

None of it is a service-level agreement, a support commitment, a warranty or a
certification. There is no one on call but you. Severity thresholds,
notification recipients, retention periods and recovery objectives are examples
to adapt, not obligations the project takes on — adapt them to your own policies
and to what your organisation has actually agreed with its own users.
:::

## Runbooks

- **[Incident response](/operations/incident-response)** — what to do when
  BatleHub is down, slow, or serving the wrong thing, and who to tell.
- **[Disaster recovery](/operations/disaster-recovery)** — restoring from
  backups, and what "restored" means for the database, the artifact store and
  the cache.
- **[Production hardening](/operations/production-hardening)** — the settings
  that differ between a working instance and one you would put in front of a
  company.
- **[What leaves this instance](/operations/egress)** — every outbound request
  this server makes, what starts it, and how to stop it.
- **[The scan worker](/operations/scan-worker)** — the role that scans what the
  proxy is about to serve: the queue, the sandbox, and what a held install means.

## Compliance

- **[Change management](/operations/change-management)** — how a change reaches
  production, and what is recorded about it.
- **[SOC 2 checklist](/operations/soc2-checklist)** — the controls an auditor
  asks about, mapped onto what BatleHub actually does.
- **[MD5 and SHA-1](/operations/weak-hashes)** — every weak digest a scanner
  finds, what each one is for, and whether the protocol allows better.

Related reading in the guide: [High Availability](/guide/high-availability),
[SBOM](/guide/sbom) and [Security scanning](/contributing/security-scanning).
