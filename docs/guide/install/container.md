# Container image

The image the releases publish, for an orchestrator or a host that already
has a database. For Kubernetes, the [Helm chart](/guide/install/helm) wraps the
same image with its config, storage and ingress.

A multi-arch image (`linux/amd64` + `linux/arm64`) is pushed to the GitHub Container Registry:

```sh
docker pull ghcr.io/batlehub/batlehub:<version>

# Or always pull the latest tagged version (not :latest — pin to a specific version in production)
docker pull ghcr.io/batlehub/batlehub:1.7.0
```

Run it:

```sh
docker run -p 8080:8080 \
  -v /path/to/config.toml:/etc/batlehub/config.toml:ro \
  -v /path/to/cache:/var/cache/batlehub \
  ghcr.io/batlehub/batlehub:<version>
```

The [Compose stacks](/guide/install/compose) under `deploy/compose/` run this
image with a database, and optionally Dex, Valkey and S3, already wired.

---

Every method needs a **PostgreSQL 14+** database, and ends the same way:
[First-time setup](/guide/installation#first-time-setup).
