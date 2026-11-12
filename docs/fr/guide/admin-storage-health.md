---
sourcePath: guide/admin-storage-health.md
sourceHash: 255ffad1895e7b1c
---

# Stockage et santé

## Stockage {#storage}

### Système de fichiers

```toml
[storage]
type = "filesystem"
path = "/var/cache/batlehub"
```

### Compatible S3 (AWS S3, RustFS)

```toml
[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

# Pour un S3 auto-hébergé (RustFS) : déclarez un endpoint personnalisé
# endpoint_url     = "http://rustfs:9900"
# force_path_style = true
```

Le bloc n'accepte aucun identifiant, et une clé inconnue y est ignorée plutôt
que refusée. Ils viennent de la chaîne du SDK AWS — un rôle IAM, ou
`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` dans l'environnement.

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
rc alias set local http://localhost:9900 rustfsadmin rustfsadmin
rc bucket create local/artifacts   # ou : task compose:s3:bucket:create
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

### La page d'un registre {#registry-page}

*Admin → Registres → Tous les registres* liste chaque registre ; chaque nom ouvre sa
page (`GET /api/v1/admin/registries/{name}`). Un bandeau en haut porte les cinq
signaux pour lesquels on l'ouvre — amont, cache, analyse, file et conseils — et
quatre onglets (`?tab=overview|activity|scan|config`) portent le détail :

- **Vue d'ensemble** : les suggestions, une liste fermée calculée par le serveur
  — un amont dégradé alors que les métadonnées périmées ne sont pas servies, une
  sécurité active sans intervalle de réanalyse, des versions dont l'analyse a
  épuisé ses tentatives.
- **Activité** : le taux d'erreur et la latence de l'amont **pour ce réplica
  depuis son démarrage** (chaque réplica compte les siens), et l'historique
  horaire de `[stats]` (hits, misses, lectures de listing). BatleHub ne garde
  pas l'historique des erreurs et de la latence : c'est le rôle du lien de
  monitoring.
- **Analyse** : si le registre analyse, son intervalle de réanalyse, la
  dernière et la prochaine revérification SBOM, les compteurs de la file, et un
  lien vers la page Scans filtrée sur lui.
- **Configuration** : les réglages effectifs, **construits depuis une liste
  blanche** : un identifiant apparaît comme `configured (bearer)`, jamais comme
  une valeur, et les URL amont sont affichées sans identifiants ni requête.

La page demande `system:read`. L'onglet Configuration demande aussi
`config:read`, les URL amont `upstreams:read`, et les compteurs de la file
`quarantine:read` sur le registre. Avec `[stats] registry_dashboard_url`
([configuration](/fr/guide/configuration)), un bouton [Monitoring ↗] ouvre votre
tableau de bord filtré sur le registre, dans un nouvel onglet qui ne reçoit pas
de référent ; `[stats] dashboard_url` met le même bouton sur le Tableau de bord.

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
