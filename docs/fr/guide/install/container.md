---
sourcePath: guide/install/container.md
sourceHash: 7771d7b1953a4931
---

# Image de conteneur

L'image que publient les releases, pour un orchestrateur ou une machine qui a
déjà une base. Sur Kubernetes, le [chart Helm](/fr/guide/install/helm) enveloppe
la même image avec sa configuration, son stockage et son ingress.

Une image multi-architecture (`linux/amd64` + `linux/arm64`) est poussée sur le
GitHub Container Registry :

```sh
docker pull ghcr.io/batlehub/batlehub:<version>

# Ou toujours la dernière version taguée (pas :latest — en production, figez une version)
docker pull ghcr.io/batlehub/batlehub:1.7.0
```

Pour la lancer :

```sh
docker run -p 8080:8080 \
  -v /path/to/config.toml:/etc/batlehub/config.toml:ro \
  -v /path/to/cache:/var/cache/batlehub \
  ghcr.io/batlehub/batlehub:<version>
```

Les [piles Compose](/fr/guide/install/compose) de `deploy/compose/` lancent
cette image avec une base, et en option Dex, Valkey et S3, déjà câblés.

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
