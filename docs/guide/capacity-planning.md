# Capacity planning

This section provides guidance on sizing disk, memory, CPU, and database resources based on registry type and expected usage.

## Artifact storage sizing

Artifact size varies dramatically by ecosystem. Use these typical values to estimate `max_artifact_size_bytes` and total storage needs:

| Registry type | Typical artifact size | Recommended `max_artifact_size_bytes` |
|---------------|----------------------|--------------------------------------|
| npm | 50 KB – 5 MB | 50 MiB |
| Cargo | 100 KB – 10 MB | 50 MiB |
| PyPI (wheel) | 1 MB – 100 MB | 200 MiB |
| Maven (JAR) | 1 MB – 200 MB | 256 MiB |
| NuGet | 100 KB – 50 MB | 100 MiB |
| Conda | 50 MB – 500 MB | 512 MiB |
| Docker (layer) | 1 MB – 2 GB | 4 GiB |
| JetBrains IDE | 500 MB – 2 GB | 4 GiB |
| Debian / RPM | 100 KB – 500 MB | 512 MiB |
| Terraform provider | 5 MB – 100 MB | 256 MiB |

For a **proxy-only** deployment: size storage at `(unique packages cached) × (average artifact size) × (versions per package)`. A practical starting point for a small team is 100 GB.

For a **local-mode** deployment: add the total size of all artifacts you plan to publish, and keep a separate backup (see [disaster recovery](/operations/disaster-recovery)).

## Memory

BatleHub's in-process memory use is low (< 200 MB baseline). Most memory is consumed by:

- The metadata cache (configurable TTL; use `[cache] type = "redis"` to offload it).
- Per-request buffering for streaming large artifacts.

Recommended minimum: **512 MB RAM** for a small deployment. For high concurrency (> 100 simultaneous downloads), allocate **2 GB** to accommodate in-flight buffers.

## CPU

BatleHub is I/O-bound, not CPU-bound. Even a 2-core instance handles several hundred requests/second for cached artifacts. CPU spikes occur during:

- SBOM extraction (ZIP decompression + JSON serialization).
- TLS termination (offload to a reverse proxy / load balancer for very high traffic).
- Vulnerability scanning (periodic, not on the hot path).

Recommended: **2–4 vCPUs** for most deployments.

## What security gates cost a request {#gate-cost}

Measured with `perf/scripts/gate_levels.sh` (2026-11-09, release build, mock upstream, 50 req/s per request shape): the median latency of eight download and metadata requests, at five levels of gating. Each level adds rules to every registry, all set to *allow* the traffic, so the number is the cost of evaluating the gates on a request that passes.

| Level | Rules | Added median latency | First download of a new version |
| --- | --- | --- | --- |
| none | — | — (≈ 5 ms per download, 1.3 ms per packument) | served at once |
| min | `release_age_gate` | +0.0 to +0.2 ms | served at once (if old enough) |
| medium | + `cve_gate` | +0.0 to +0.4 ms | served at once |
| high | `[registries.security]` (osv) + `cve_gate` | +0.3 to +0.65 ms | **held until the scan answers: ~9 s** |
| max | high + `license_gate`, `version_gate`, `require_signed_release`, SBOM | +0.2 to +0.75 ms | **held until the scan answers: ~10 s** |

Run to run, the same level moves by up to ±0.15 ms. The per-request cost of every level is below a millisecond. What a level really costs is the **hold**: a `[registries.security]` profile serves a version only once its required scanners have answered (here, OSV over the network plus the worker's 2-second poll), and a `release_age_gate` or `security.min_age_secs` holds a version younger than its age, which is hours or days by design. Plan for the hold, not for the CPU.

## Database (PostgreSQL)

- **Connections:** Default `max_connections = 10` is fine for < 50 concurrent requests. Increase to 25–50 for busy deployments. The DB itself should allow at least `max_connections * (number of instances) + 10` headroom.
- **Disk:** The `access_events` table grows ~1 KB per event. At 1,000 events/day, that is ~365 MB/year. Schedule periodic purges: `batlehub-cli audit purge --before 2024-01-01T00:00:00Z`.
- **IOPS:** Metadata reads and `record_access` writes are the hottest paths. An SSD with 3,000 IOPS is sufficient for most teams.

## Recommended `max_artifact_size_bytes` by deployment type

Set per registry, not globally, to avoid a large JetBrains IDE download from blocking smaller npm installs that share a connection pool:

```toml
[[registries]]
name = "npm-proxy"
type = "npm"
[registries.local_registry]
max_artifact_size_bytes = 52428800   # 50 MiB

[[registries]]
name = "jetbrains-proxy"
type = "jetbrains"
[registries.local_registry]
max_artifact_size_bytes = 4294967296  # 4 GiB
```
