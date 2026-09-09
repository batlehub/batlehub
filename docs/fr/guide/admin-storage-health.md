---
sourcePath: guide/admin-storage-health.md
sourceHash: 1e73922d0122b4bb
---

# Stockage et santé

## Stockage {#storage}

### Système de fichiers

```toml
[storage]
type = "filesystem"
path = "/var/cache/batlehub"
```

### Compatible S3 (AWS S3, MinIO, RustFS)

```toml
[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

# Pour un S3 auto-hébergé (MinIO, RustFS) : déclarez un endpoint personnalisé
# endpoint = "http://rustfs:9900"

# Identifiants (à omettre pour utiliser le rôle IAM ou le profil d'instance sur AWS)
# access_key_id     = "AKIAIOSFODNN7EXAMPLE"
# secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
```

### Stockage multi-backend

Des registres différents peuvent employer des backends différents — par exemple
le système de fichiers pour la plupart et un S3 dédié aux gros artefacts de
releases GitHub :

```toml
[storage]
type = "filesystem"
path = "/var/cache/batlehub"

[[storage.backends]]
name = "github-s3"
type = "s3"
bucket = "batlehub-github"
region = "us-east-1"

[[registries]]
type    = "github"
name    = "github"
storage = "github-s3"
```

### S3 avec RustFS (auto-hébergé)

Démarrez RustFS par le fichier Compose fourni, puis créez le bucket :

```sh
task compose:s3:db            # démarre RustFS + Postgres + Authentik
mc alias set local http://localhost:9900 rustfsadmin rustfsadmin
mc mb local/artifacts         # ou : task compose:s3:bucket:create
task run:s3                   # lance le serveur avec la configuration S3
```

---

## Santé et observabilité {#health}

### L'endpoint de santé

```sh
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/health
```

Renvoie l'état par registre (joignabilité de l'amont, taux de hit du cache) et
l'état global du serveur.

### Vider le cache d'un registre

Force la prochaine requête sur n'importe quel paquet du registre à repasser par
l'amont :

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/registries/npm/clear-cache
```

### OpenTelemetry (Jaeger, Tempo)

Activez le traçage distribué en ajoutant un bloc `[otel]` :

```toml
[otel]
endpoint = "http://jaeger:4317"
```

Démarrez la pile d'observabilité complète en local :

```sh
task compose:otel   # démarre Postgres, le serveur et Jaeger
```

Puis ouvrez `http://localhost:16686` pour la console Jaeger.
