# Administration

This page covers everything an administrator needs to operate BatleHub: configuration, storage, auth providers, registry management, health monitoring, cache cleanup, hot reloading, and the global banner.

For the complete TOML reference see [`docs/guide/configuration.md`](https://github.com/batlehub/batlehub/blob/main/docs/guide/configuration.md).

## Contents

The administration guide is split across four pages:

- **[Configuration](/guide/admin-config)** — the TOML config file and loading order, `${VAR_NAME}` secret injection, named `PROXY_CACHE__*` overrides, registry modes, auth providers (static tokens, OIDC, Kubernetes, user tokens), runtime **hot reload** of the config, and the **global banner**.
- **[Storage & Health](/guide/admin-storage-health)** — filesystem, S3-compatible, and multi-backend **storage**, plus **health & observability**: the health endpoint, cache clearing, and OpenTelemetry tracing.
- **[Policies & packages](/guide/admin-policies)** — **cache policy** (eviction, cache warming, deduplication), **package management** (list, block, bulk-block, invalidate), and per-registry **rules** (release age gate, deny latest, trusted publisher).
- **[Access & audit](/guide/admin-access)** — **team namespaces & package visibility**, the **audit log**, the **beta/pre-release channel**, and **IP-based blocking**.
