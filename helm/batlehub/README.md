
# batlehub

![Version: 1.2.0](https://img.shields.io/badge/Version-1.2.0-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: 1.2.0](https://img.shields.io/badge/AppVersion-1.2.0-informational?style=flat-square)

Smart proxy and cache for package registries

**Homepage:** <https://github.com/batlehub/batlehub>

## Install

```sh
helm repo add batlehub https://batleforc.git.batleforc.fr/batlehub/charts
helm install batlehub batlehub/batlehub -f my-values.yaml
```

Or straight from a checkout of the repository:

```sh
helm dependency build helm/batlehub
helm install batlehub ./helm/batlehub -f my-values.yaml
```

The chart needs a PostgreSQL database it does not create. Point
`config.database.url` at one before installing.

## What you have to change before exposing it

The defaults start a working server, not a safe one. Three values decide that:

- `config.auth` ships a single static admin token, literally `change-me-admin-token`.
  Replace it, or replace the whole block with an OIDC provider.
- `config.database.url` carries a placeholder password and points at a host
  named `postgres`.
- `config.server.trusted_proxies` is unset, which keeps the permissive
  behaviour. It becomes a startup error the moment host routing is configured
  through `config.subdomain_routing` or a registry's `hosts`.

There are two ways to keep those out of the values file, and they compose:

- `env` as a `secretKeyRef`, referenced from `config` as a `${VAR_NAME}`
  placeholder. Best for one field inside a larger block.
- `credentials`, a second config file merged over `config` and mounted from its
  own Secret. Best when whole sections belong to a different lifecycle. Rotating
  that Secret does not roll the pods: the file watcher picks the change up, which
  is why nothing in this chart puts a checksum annotation on it.

## Storage and replicas

`config.storage.type` decides whether more than one replica is possible. The
default `filesystem` backend is backed by the `persistence` PVC, and
`ReadWriteOnce` cannot be shared by replicas on different nodes. Either keep one
replica, or switch to `s3`, which is the only backend every replica can write to
concurrently.

## Requirements

| Repository | Name | Version |
|------------|------|---------|
| https://aquasecurity.github.io/helm-charts/ | trivy | 0.14.1 |

## Values

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| replicaCount | int | `1` | Proxy replicas. Filesystem storage is effectively single-replica; see `persistence`. |
| image | object | `{"pullPolicy":"IfNotPresent","repository":"ghcr.io/batleforc/batlehub","tag":""}` | Proxy image. The worker has its own under `worker.image`. |
| image.tag | string | `Chart.appVersion` | Overrides the image tag. |
| imagePullSecrets | list | `[]` | Pull secrets, for images held in a private registry. |
| nameOverride | string | `""` | Replaces the chart name in generated resource names. |
| fullnameOverride | string | `""` | Replaces the full generated name of every resource. |
| serviceAccount | object | `{"annotations":{},"automountServiceAccountToken":false,"create":true,"name":"","rbac":{"autoTokenReview":true}}` | ServiceAccount for the proxy pod, and the TokenReview RBAC that `type = "kubernetes"` auth needs. |
| podAnnotations | object | `{}` | Annotations added to the proxy pod. |
| podSecurityContext | object | `{"fsGroup":65532,"runAsGroup":65532,"runAsNonRoot":true,"runAsUser":65532,"seccompProfile":{"type":"RuntimeDefault"}}` | Pod-level security context. Matches the `USER 65532` the image already declares, so a Pod Security Admission "restricted" namespace admits the pod unmodified. `fsGroup` is what makes the cache PVC writable. |
| securityContext | object | `{"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"readOnlyRootFilesystem":true}` | Container-level security context. The server writes only to the cache mount, so the root filesystem stays read-only. |
| service | object | `{"port":8080,"type":"ClusterIP"}` | Service fronting the proxy. |
| ingress | object | `{"annotations":{},"className":"","enabled":false,"extraHosts":[],"host":"batlehub.example.com","tls":[]}` | Ingress for the proxy. |
| ingress.extraHosts | list | `[]` | Additional hostnames routed to BatleHub, for host-based registry routing. Extends the Ingress only: host routing itself comes from `config.subdomain_routing` or a registry's `hosts`, and those make `config.server.trusted_proxies` mandatory. |
| ingress.tls | list | `[]` | TLS blocks for the Ingress. The certificate needs a SAN for every extra host, the wildcard included. |
| resources | object | `{"limits":{"ephemeral-storage":"1Gi","memory":"512Mi"},"requests":{"cpu":"100m","ephemeral-storage":"256Mi","memory":"128Mi"}}` | Resource requests and limits for the proxy container. |
| env | list | `[]` | Environment variables injected into the container. Reference them as `${VAR_NAME}` inside `config`; a placeholder with no variable behind it is a startup error naming the variable. |
| envFrom | list | `[]` | Bulk-import every key of a Secret or ConfigMap as environment variables. |
| nodeSelector | object | `{}` | Node selector for the proxy pod. |
| tolerations | list | `[]` | Tolerations for the proxy pod. |
| affinity | object | `{}` | Affinity rules for the proxy pod. |
| config | object | one npm registry, filesystem storage, one static admin token | The BatleHub application config, serialised verbatim to `config.toml`. Keys are snake_case to match the TOML field names, and an optional section must be absent rather than null or `{}` to be left out of the rendered file. Full reference: `docs/guide/configuration.md`. |
| config.server | object | `{"host":"0.0.0.0","port":8080}` | The `[server]` block: bind address, CORS origins and the proxy-trust policy that host routing requires. |
| config.database | object | `{"type":"postgresql","url":"postgresql://batlehub:changeme@postgres:5432/batlehub"}` | The `[database]` block. PostgreSQL is the only supported type. |
| config.storage | object | `{"path":"/var/cache/batlehub","type":"filesystem"}` | The `[storage]` block: `filesystem` (the default, backed by `persistence`) or `s3`. S3 is the only backend several replicas can write to at once. |
| config.auth | list | `[{"tokens":[{"role":"admin","user_id":"admin","value":"change-me-admin-token"}],"type":"token"}]` | One `[[auth]]` block per entry. Types: `token`, `oidc`, `kubernetes`, `actions-oidc`. Replace the default admin token before exposing the server. |
| config.registries | list | `[{"name":"npm","rbac":{"admin":["*"],"anonymous":["releases:read","source:read"],"user":["releases:read","source:read"]},"type":"npm"}]` | One `[[registries]]` block per entry, each with its own type, mode and RBAC. See the registry table in the repository README. |
| credentials | object | disabled | A second config file merged over `config`, for the values with a different lifecycle from the rest. Either rendered into a chart-managed Secret from `credentials.config`, or read from `credentials.existingSecret`. |
| podDisruptionBudget | object | `{"enabled":true,"minAvailable":1}` | Disruption budget. Rendered only when `replicaCount` is above 1. |
| worker | object | disabled | The scan worker: a second Deployment of the same binary run with `--roles worker` from the worker image (RFC 0018). Off by default, and turning it on means setting `config.server.roles = ["proxy"]` so the proxy stops scanning. |
| worker.autoscaling | object | `{"enabled":false,"maxReplicas":4,"metricName":"batlehub_scan_jobs_queued","minReplicas":1,"targetQueued":20}` | HorizontalPodAutoscaler on the external metric `batlehub_scan_jobs_queued`. Needs a metrics adapter to expose that gauge; `worker.replicaCount` is ignored while this is on. |
| worker.image.repository | string | `"ghcr.io/batleforc/batlehub-worker"` | Worker image. Point it at `batlehub-worker-guarddog` to enable the `[scanners.guarddog]` scanner, which is the only one not on the base image. |
| worker.runtimeClassName | string | `""` | Optional sandboxed runtime (gVisor, Kata) around the whole worker pod, on top of the bubblewrap sandbox around each scanner. |
| worker.scratch | object | `{"sizeLimit":"4Gi"}` | Per-job work directories, as a memory-backed emptyDir so a hostile archive never touches the node's disk. Size it for `max_concurrent` jobs of `[worker.sandbox] max_extracted_mb` each. |
| worker.resources | object | `{}` | Resource requests and limits for the worker container. |
| worker.nodeSelector | object | `{}` | Node selector for the worker pod. |
| worker.tolerations | list | `[]` | Tolerations for the worker pod. |
| trivy | object | `{"enabled":false}` | The Trivy server sub-chart, for `[scanners.trivy] endpoint`. Turning it on makes the endpoint `http://<release>-trivy:4954`. |
| networkPolicy | object | `{"egressTo":[],"enabled":false,"ingressFrom":[]}` | NetworkPolicy for the proxy. Off by default because the right egress set depends on which upstreams you proxy, and a default-deny policy written without that list breaks every cache miss. |
| networkPolicy.ingressFrom | list | `[]` | Peers allowed to reach the service port. An empty list renders an ingress rule with no `from`, which the NetworkPolicy spec reads as **any source** — name the ingress controller to actually narrow it. |
| networkPolicy.egressTo | list | `[]` | Extra egress rules, appended to the always-present DNS rule. |
| persistence | object | `{"accessMode":"ReadWriteOnce","enabled":true,"ephemeralSizeLimit":"1Gi","size":"10Gi","storageClass":""}` | PersistentVolumeClaim for the artifact cache in filesystem mode. `ReadWriteOnce` cannot be shared across nodes, so a multi-replica deployment needs either S3 storage or a `ReadWriteMany` class. |
| externalManifest | list | `[]` | Extra Kubernetes manifests this chart renders or merely references, each optionally mounted into the pod. One entry may set `mount.asConfig` to replace the chart-managed config Secret. |

## Further reading

The table above is the chart's surface. What goes *inside* `config` is the
server's own configuration, documented in full at
[`docs/guide/configuration.md`](../../docs/guide/configuration.md), with the
installation guide at
[`docs/guide/installation.md`](../../docs/guide/installation.md).

----------------------------------------------
Autogenerated from chart metadata using [helm-docs v1.14.2](https://github.com/norwoodj/helm-docs/releases/v1.14.2)
