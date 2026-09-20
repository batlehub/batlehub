---
sourcePath: guide/install/container.md
sourceHash: badd759c01fd4d7a
---

# Image de conteneur

L'image que publient les releases, pour un orchestrateur ou une machine qui a
déjà une base. Sur Kubernetes, le [chart Helm](/fr/guide/install/helm) enveloppe
la même image avec sa configuration, son stockage et son ingress.

Une image multi-architecture (`linux/amd64` + `linux/arm64`) est poussée sur le
GitHub Container Registry :

```sh
docker pull ghcr.io/batleforc/batlehub:<version>

# Ou toujours la dernière version taguée (pas :latest — en production, figez une version)
docker pull ghcr.io/batleforc/batlehub:0.2.0
```

Pour la lancer :

```sh
docker run -p 8080:8080 \
  -v /path/to/config.toml:/etc/batlehub/config.toml:ro \
  -v /path/to/cache:/var/cache/batlehub \
  ghcr.io/batleforc/batlehub:<version>
```

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
