---
sourcePath: guide/high-availability.md
sourceHash: fc2725d91361c397
---

# Haute disponibilité

BatleHub est un **serveur HTTP sans état** — toutes les données durables vivent
dans PostgreSQL et dans un magasin d'objets, pas dans le processus. Faire tourner
plusieurs réplicas sans risque suppose de remplacer deux défauts mono-instance
par des backends partagés : le cache en mémoire et le stockage sur système de
fichiers local.

---

## Vue d'ensemble de l'architecture {#overview}

```
                 ┌───────────────┐
                 │  Load balancer │
                 └──────┬────────┘
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
   ┌────────────┐ ┌────────────┐ ┌────────────┐
   │ BatleHub 1 │ │ BatleHub 2 │ │ BatleHub 3 │
   └─────┬──────┘ └─────┬──────┘ └─────┬──────┘
         │               │               │
         └───────────────┼───────────────┘
                         │
           ┌─────────────┼─────────────┐
           ▼             ▼             ▼
     ┌──────────┐  ┌──────────┐  ┌──────────┐
     │PostgreSQL│  │  Redis   │  │    S3    │
     │(primary) │  │ (cache)  │  │(storage) │
     └──────────┘  └──────────┘  └──────────┘
```

Tout l'état est partagé à l'extérieur. Aucune affinité de session n'est
nécessaire — n'importe quel réplica peut servir n'importe quelle requête.

### Ce qui change entre mono-instance et haute disponibilité

| Composant | Défaut mono-instance | Exigence multi-instance |
|-----------|------------------------|---------------------------|
| Cache de métadonnées | `InMemoryCacheStore` (par processus) | PostgreSQL ou Redis (`[cache]`) |
| Limitation de débit | `InMemoryRateLimitStore` (par processus) | Le même backend `[cache]` — automatique |
| Blocage d'IP | `InMemoryIpBlockStore` (par processus) | Le même backend `[cache]` — automatique |
| **Bandeau global** | `InMemoryBannerStore` (par processus) | Le même backend `[cache]` — automatique |
| Stockage des artefacts | Système de fichiers (`/var/cache/batlehub`) | Magasin d'objets compatible S3 |
| Données canoniques | PostgreSQL | PostgreSQL — déjà partagé |

La section `[cache]` gouverne les quatre magasins en mémoire par un seul réglage.
En la changeant, vous corrigez du même coup la limitation de débit, le blocage
d'IP et le bandeau global, sans configuration supplémentaire.

---

## Prérequis {#prerequisites}

Avant de passer à plus d'un réplica :

- **PostgreSQL 14+** — déjà requis ; rien à changer.
- **Un magasin d'objets compatible S3** — AWS S3, MinIO ou RustFS. Le stockage
  sur système de fichiers est mono-nœud.
- **Un backend de cache partagé** — soit la même instance PostgreSQL (le plus
  simple), soit une instance Redis 7+.
- **Un répartiteur de charge ou un ingress** — n'importe quoi qui fasse du HTTP
  en tourniquet (nginx, Traefik, Ingress Kubernetes). Aucune affinité de session
  n'est nécessaire.

---

## Les changements de configuration {#config}

Ce sont les seuls changements nécessaires pour passer de mono-instance à
multi-instance. Tout le reste est identique.

### Le backend de cache {#config-cache}

Remplacez le cache en mémoire par défaut par un backend partagé. Ce seul
changement couvre le cache de métadonnées, la limitation de débit et le blocage
d'IP.

**Option A — PostgreSQL** (emploie votre base existante, aucun service en plus) :

```toml
[cache]
type = "postgres"
# url vaut database.url par défaut — omettez-la sauf si vous voulez une autre chaîne de connexion
```

**Option B — Redis** (latence plus basse, recommandé en fort volume de
requêtes) :

```toml
[cache]
type  = "redis"
url   = "redis://redis:6379"
```

### Le stockage des artefacts {#config-storage}

Passez du système de fichiers à S3. Tous les réplicas lisent et écrivent dans le
même bucket.

```toml
[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

# Pour un S3 auto-hébergé (MinIO, RustFS) :
# endpoint         = "http://minio:9000"
# force_path_style = true

# Identifiants (à omettre sur AWS avec un rôle IAM) :
# access_key_id     = "AKIAIOSFODNN7EXAMPLE"
# secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
```

### Le pool de connexions à la base {#config-db}

Chaque réplica ouvre son propre pool de connexions. Baissez `max_connections` par
réplica derrière un PgBouncer, ou laissez le défaut (10) en connexion directe.

```toml
[database]
type            = "postgresql"
url             = "postgresql://batlehub:changeme@postgres:5432/batlehub"
max_connections = 5   # recommandé par réplica avec un pooler de connexions
```

### CORS {#config-cors}

Depuis la 1.1.0, un `cors_allowed_origins` non défini signifie **même origine
uniquement**. Si BatleHub sert lui-même la SPA — le défaut, et ce que fait le
chart Helm — vous n'avez rien à faire ici : une requête de même origine ne
consulte jamais CORS.

Définissez-le quand un client navigateur est servi depuis une origine
*différente* de celle de l'API :

```toml
[server]
host                 = "0.0.0.0"
port                 = 8080
cors_allowed_origins = ["https://batlehub.example.com"]
```

::: warning Changement de comportement en 1.1.0
Une liste vide ou absente autorisait auparavant **toutes** les origines. Mettre
`cors_allowed_origins = ["*"]` restaure ce comportement et lève un avertissement
de configuration `cors.any-origin`.
:::

### Exemple complet de configuration multi-instance

```toml
[server]
host                 = "0.0.0.0"
port                 = 8080
static_dir           = "/app/ui/dist"
cors_allowed_origins = ["https://batlehub.example.com"]

[database]
type            = "postgresql"
url             = "postgresql://batlehub:changeme@postgres:5432/batlehub"
max_connections = 5

[cache]
type = "redis"
url  = "redis://redis:6379"

[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

[[auth]]
type = "token"

[[auth.tokens]]
value   = "change-me-admin-token"
role    = "admin"
user_id = "admin"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user      = ["releases:read", "source:read"]
admin     = ["*"]
```

---

## Docker Compose — redondance sur un seul hôte {#compose}

Docker Compose sait faire tourner plusieurs réplicas du serveur sur un seul
hôte. Cela protège d'un plantage de processus, pas d'une panne d'hôte.
Servez-vous-en pour un environnement de préproduction, ou quand vous voulez une
redondance au niveau processus sans un cluster Kubernetes complet.

```yaml
# docker-compose.ha.yml
services:
  postgres:
    image: postgres:17-alpine
    environment:
      POSTGRES_DB:       batlehub
      POSTGRES_USER:     batlehub
      POSTGRES_PASSWORD: changeme
    volumes:
      - postgres_data:/var/lib/postgresql/data

  redis:
    image: redis:7-alpine
    command: redis-server --save "" --appendonly no

  batlehub:
    image: ghcr.io/batleforc/batlehub:1.1.0
    deploy:
      replicas: 2
      restart_policy:
        condition: on-failure
    depends_on: [postgres, redis]
    volumes:
      - ./config.toml:/etc/batlehub/config.toml:ro
    # Pas besoin de volume de cache — le stockage est sur S3.

  proxy:
    image: nginx:alpine
    ports:
      - "8080:80"
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf:ro
    depends_on: [batlehub]

volumes:
  postgres_data:
```

Un `nginx.conf` minimal :

```nginx
events {}
http {
  upstream batlehub {
    server batlehub:8080;   # le DNS interne de Docker répartit en tourniquet entre les réplicas
  }
  server {
    listen 80;
    location / {
      proxy_pass http://batlehub;
      proxy_set_header Host              $host;
      proxy_set_header X-Real-IP         $remote_addr;
      proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
      proxy_set_header X-Forwarded-Proto $scheme;
    }
  }
}
```

::: warning Le blocage d'IP derrière un proxy
Si `[ip_blocking]` est actif, BatleHub lit l'IP du client dans `X-Real-IP` ou
`X-Forwarded-For`. Assurez-vous que votre répartiteur de charge pose ces
en-têtes ; sinon, toutes les requêtes semblent venir de l'IP du proxy.
:::

---

## Kubernetes et Helm — haute disponibilité en production {#kubernetes}

Le chart Helm fourni gère les déploiements multi-réplicas d'emblée, dès que S3 et
un backend de cache partagé sont configurés.

### Le fichier de valeurs de haute disponibilité

```yaml
# ha-values.yaml
replicaCount: 3

image:
  repository: ghcr.io/batleforc/batlehub
  tag: "1.1.0"    # figez une version précise

database:
  url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"

storage:
  type: s3
  s3:
    bucket:          batlehub-artifacts
    region:          us-east-1
    accessKeyId:     "AKIAIOSFODNN7EXAMPLE"
    secretAccessKey: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"

# Pas besoin de PVC — tous les artefacts vont sur S3.
persistence:
  enabled: false

# Injecter le backend de cache partagé par extraConfig.
extraConfig: |
  [cache]
  type = "redis"
  url  = "redis://redis-svc:6379"

ingress:
  enabled: true
  className: nginx
  host: batlehub.example.com
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com

# Répartir les réplicas entre les nœuds.
affinity:
  podAntiAffinity:
    preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 100
        podAffinityTerm:
          topologyKey: kubernetes.io/hostname
          labelSelector:
            matchLabels:
              app.kubernetes.io/name: batlehub

resources:
  requests:
    cpu:    200m
    memory: 256Mi
  limits:
    cpu:    1000m
    memory: 512Mi
```

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  -f ha-values.yaml
```

### Le Horizontal Pod Autoscaler

Ajuste le nombre de réplicas automatiquement selon la charge processeur :

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: batlehub
  namespace: batlehub
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: batlehub
  minReplicas: 2
  maxReplicas: 10
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
```

---

## Mises à jour progressives et déploiements sans coupure {#rolling-updates}

BatleHub applique les migrations de base automatiquement au démarrage. Ces
migrations sont conçues pour être additives — elles ne suppriment jamais une
colonne ni une table que la version précédente lit encore. Un déploiement
progressif est donc sûr :

1. Les nouveaux pods démarrent, exécutent les migrations et deviennent prêts.
2. Les anciens pods continuent de servir pendant que les nouveaux migrent.
3. Les anciens pods sont arrêtés une fois la sonde de disponibilité des nouveaux
   passée.

Configurez la stratégie du Deployment pour garantir l'absence de coupure :

```yaml
# Dans ha-values.yaml, sous l'extraConfig du chart, ou par kubectl patch :
strategy:
  type: RollingUpdate
  rollingUpdate:
    maxUnavailable: 0
    maxSurge: 1
```

Pour l'appliquer directement au Deployment du chart (qui ne l'expose pas comme
valeur de premier niveau), corrigez-le après l'installation :

```sh
kubectl patch deployment batlehub \
  -n batlehub \
  --type=json \
  -p='[{"op":"add","path":"/spec/strategy","value":{"type":"RollingUpdate","rollingUpdate":{"maxUnavailable":0,"maxSurge":1}}}]'
```

---

## Sondes de santé {#health}

Le chart Helm configure automatiquement les sondes de vivacité et de
disponibilité :

| Sonde | Endpoint | Délai initial | Période |
|-------|----------|--------------|--------|
| Disponibilité | `GET /api/v1/admin/health` | 5 s | 10 s |
| Vivacité | `GET /api/v1/admin/health` | 10 s | 30 s |

L'endpoint de santé n'exige **pas** d'en-tête `Authorization`. Kubernetes peut
l'atteindre directement depuis le kubelet.

Le trafic n'est routé vers un pod qu'une fois sa sonde de disponibilité passée —
un client n'est donc jamais envoyé vers un réplica encore en train de migrer ou
de préchauffer son cache.

---

## Observabilité {#observability}

Le traçage distribué fonctionne entre réplicas sans configuration
supplémentaire. Chaque span porte le même identifiant de trace, quel que soit le
réplica qui traite la requête. Pointez tous les réplicas vers le même collecteur
OpenTelemetry :

```toml
[otel]
endpoint     = "http://otel-collector:4317"
service_name = "batlehub"
```

En Helm :

```yaml
otel:
  enabled:  true
  endpoint: "http://otel-collector:4317"
```

Chaque réplica émet ses propres spans ; le collecteur les recoud en traces
complètes par identifiant de trace. Voir le
[guide d'administration](/fr/guide/admin-storage-health#health) pour le démarrage
rapide avec Jaeger.

---

## Limites connues {#limitations}

Ce sont des compromis assumés, consignés dans
`docs/contributing/contributing.md` §9 :

- **Course TOCTOU sur les quotas** — l'application du quota de publication lit
  l'usage courant puis l'incrémente en deux opérations de base distinctes. Sous
  des publications concurrentes réparties sur plusieurs réplicas, le quota peut
  être dépassé d'au plus un envoi par écrivain simultané. L'application est
  cohérente à terme, pas stricte.

- **Doublons de préchauffage** — chaque réplica exécute sa propre passe de
  préchauffage au démarrage. Plusieurs réplicas qui démarrent en même temps
  récupèrent chacun les mêmes paquets amont. Les téléchargements sont idempotents
  (le dernier écrivain gagne sur S3) mais génèrent du trafic amont en double.

- **Retour de quota asynchrone** — si une publication échoue après le stockage
  mais avant la validation en base, le compteur de quota est décrémenté de façon
  asynchrone. Il existe une courte fenêtre pendant laquelle le compteur est
  surévalué.

- **Des magasins en mémoire si la configuration est fautive** — si `[cache]`
  reste au défaut `type = "memory"`, chaque réplica tient son propre état de
  limitation de débit et de blocage d'IP. Les limites sont alors N fois plus
  permissives que configurées (N étant le nombre de réplicas), et un blocage
  d'IP posé sur un réplica ne se propage pas aux autres. **Déclarez toujours un
  backend de cache partagé pour un déploiement multi-réplicas.**
