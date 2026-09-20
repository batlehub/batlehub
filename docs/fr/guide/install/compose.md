---
sourcePath: guide/install/compose.md
sourceHash: 890cabfc308f4c64
---

# Docker Compose

Le moyen le plus rapide d'obtenir une instance qui tourne, pour le développement
local ou pour évaluer le produit : le serveur et sa base démarrent ensemble.

**1. Clonez le dépôt :**

```sh
git clone https://github.com/batlehub/batlehub
cd batlehub
```

**2. Copiez et modifiez la configuration d'exemple :**

```sh
cp config.example.toml config.toml
# Modifiez config.toml : URL de la base, token d'admin, et au moins un registre
```

**3. Démarrez PostgreSQL et le serveur :**

```sh
podman compose up -d   # ou docker compose up -d
```

Le serveur écoute sur `http://localhost:8080`. L'interface Swagger est sur
`http://localhost:8080/swagger-ui/`.

**4. Vérifiez :**

```sh
curl http://localhost:8080/api/openapi.json
```

## Avec un stockage S3 (RustFS)

Un fichier Compose distinct ajoute un backend de stockage RustFS (compatible S3)
et l'OIDC d'Authentik :

```sh
podman compose -f docker-compose.s3.yml up -d postgres rustfs
# Puis lancez le serveur avec la configuration S3 :
task run:s3
```

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
