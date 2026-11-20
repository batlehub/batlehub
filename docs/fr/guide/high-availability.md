---
sourcePath: guide/high-availability.md
sourceHash: 68764c5c3d74933b
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
nécessaire — n'importe quel réplica peut servir n'importe quelle requête, y
compris pour un artefact froid : les réplicas partagent un seul remplissage par
artefact ([Remplissages à froid](#cold-fills)).

### Ce qui change entre mono-instance et haute disponibilité

| Composant | Défaut mono-instance | Exigence multi-instance |
|-----------|------------------------|---------------------------|
| Cache de métadonnées | `InMemoryCacheStore` (par processus) | PostgreSQL ou Redis (`[cache]`) |
| Limitation de débit | `InMemoryRateLimitStore` (par processus) | Le même backend `[cache]` — automatique |
| Blocage d'IP | `InMemoryIpBlockStore` (par processus) | Le même backend `[cache]` — automatique |
| **Bandeau global** | `InMemoryBannerStore` (par processus) | Le même backend `[cache]` — automatique |
| Bail de remplissage à froid | Dans le processus | Ligne PostgreSQL, dès que `[cache]` est partagé — automatique |
| Stockage des artefacts | Système de fichiers (`/var/cache/batlehub`) | Magasin d'objets compatible S3 |
| Données canoniques | PostgreSQL | PostgreSQL — déjà partagé |

La section `[cache]` gouverne les quatre magasins en mémoire, et le choix de
qui remplit un artefact froid, par un seul réglage. En la changeant, vous
corrigez du même coup la limitation de débit, le blocage d'IP, le bandeau global
et les récupérations amont en double, sans configuration supplémentaire.

---

## Prérequis {#prerequisites}

Avant de passer à plus d'un réplica :

- **PostgreSQL 14+** — déjà requis ; rien à changer.
- **Un magasin d'objets compatible S3** — AWS S3 ou RustFS ([MinIO n'est plus officiellement pris en charge](/fr/guide/configuration#_3-4-storage)). Le stockage
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
changement couvre le cache de métadonnées, la limitation de débit, le blocage
d'IP et le [bail de remplissage à froid](#cold-fills). **Plus d'un réplica
l'exige** : en `memory`, chaque pod remplit, limite le débit et bannit de son
côté. Le chart Helm refuse de rendre `replicaCount > 1` sans `config.cache.type`
à `postgres` ou `redis`, sauf si la configuration vient d'une source qu'il ne
peut pas lire (une entrée `externalManifest` avec `mount.asConfig`, ou
`credentials.existingSecret`) — c'est alors à vous de la régler correctement.

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

# Pour un S3 auto-hébergé (RustFS) :
# endpoint_url     = "http://rustfs:9000"
# force_path_style = true
```

Il n'y a aucun champ d'identifiant ici : le bloc accepte `bucket`, `region`,
`prefix`, `endpoint_url` et `force_path_style`, et rien d'autre. Les clés
inconnues ne sont pas refusées, elles sont ignorées — un `access_key_id` écrit
là ressemble à un problème de politique de bucket au premier défaut de cache.
Les identifiants viennent de la chaîne standard du SDK AWS : le rôle de
l'instance ou du pod, ou `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` dans
l'environnement du processus.

### Le pool de connexions à la base {#config-db}

Chaque réplica ouvre son propre pool de connexions. Baissez `max_connections` par
réplica derrière un PgBouncer, ou laissez le défaut (20) en connexion directe — voir
[dimensionner `max_connections`](./configuration#dimensionner-max-connections).

```toml
[database]
type            = "postgresql"
url             = "postgresql://batlehub:changeme@postgres:5432/batlehub"
max_connections = 5   # recommandé par réplica avec un pooler de connexions
```

#### Le cache de lecture et `LISTEN` {#config-db-listen}

Chaque réplica garde en mémoire les lectures que chaque téléchargement répète — le blocage du
paquet, ses flags, sa visibilité, le jeton présenté, le blocage de l'utilisateur, l'endroit où
l'artefact est stocké — et Postgres prévient chaque réplica quand l'une d'elles change
(`LISTEN/NOTIFY`, alimenté par des triggers). Un blocage ou un jeton révoqué s'applique donc
sur tous les réplicas dans le temps d'une notification, pas au bout d'un délai.

Le cache ne s'active qu'une fois que le réplica a vu revenir une notification qu'il a lui-même
envoyée, et se coupe dès qu'une n'arrive pas. **Derrière un pooler en mode transaction**
(`pool_mode = transaction` de PgBouncer), les notifications n'atteignent jamais le listener :
le cache reste coupé, chaque lecture va à la base comme avant, et le journal le dit une fois.
Pour garder le cache, donnez au listener une connexion directe ou en mode session :

```toml
[database]
url        = "postgresql://batlehub:changeme@pgbouncer:6432/batlehub"
listen_url = "postgresql://batlehub:changeme@postgres:5432/batlehub"  # directe, pour LISTEN seulement
```

Sans `listen_url`, le listener occupe une connexion du pool. `batlehub_read_cache_live`
(1 ou 0) indique si le cache est actif ; `batlehub_read_cache_invalidations_total` compte
les notifications reçues.

Un listener dont la connexion meurt sans reset (une partition réseau) est repéré par son
heartbeat, envoyé toutes les 10 s : une réponse peut alors être périmée 20 s au plus, et
chaque entrée est de toute façon abandonnée au bout de 5 minutes. Une écriture faite *par* un
réplica vide la copie de ce réplica au commit, sans attendre la notification.

Le cache est actif par défaut, y compris avec un seul réplica : le scan worker
(`--roles worker`), un autre réplica ou une requête SQL d'un opérateur écrivent les mêmes
lignes, et seule la notification en informe ce processus. Pour le couper — chaque lecture va
à la base, et aucun listener n'est démarré :

```toml
[database]
read_cache = false
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

## Remplissages à froid {#cold-fills}

Un MISS à froid donne **un seul remplissage par artefact pour tous les
réplicas** qui partagent la base, le `[cache]` et le bucket. La façon dont un
remplissage diffuse, vérifie et stocke les octets est décrite dans
[Un MISS à froid](./caching#cold-fill).

- **Qui remplit est un bail.** Avec `[cache]` à `postgres` ou `redis`, c'est
  une ligne PostgreSQL (`fill_leases` ; PostgreSQL même avec un cache Redis).
  En `memory` — un seul réplica — il reste dans le processus et ne coûte
  aucune requête.
- **Les autres réplicas suivent le remplissage au lieu de récupérer.** Un
  réplica qui reçoit une requête pour un artefact qu'un autre remplit relit
  depuis le stockage les segments de 8 Mio déjà validés et n'envoie aucune
  requête amont. Il est réveillé par un `NOTIFY` sur la
  [connexion `LISTEN`](#config-db-listen) qu'il tient déjà, ou interroge le bail
  une fois par seconde tant que cette connexion est coupée. Son premier octet
  arrive au plus le temps de transfert d'un segment après celui du réplica qui
  remplit : 0,84 s à 10 Mo/s, 8,4 s à 1 Mo/s.
- **La mort d'un pod en plein remplissage fait repartir de zéro.** Le bail
  vit 30 s et le réplica qui remplit le renouvelle toutes les 10 s, par une
  minuterie à part : un amont silencieux ou un long assemblage S3 ne le laisse
  pas expirer. Quand le réplica qui
  remplit meurt, ses lecteurs sur les autres réplicas sont coupés une fois le
  bail expiré (jusqu'à ~30 à 60 s) ; la nouvelle tentative du client reprend le
  bail, supprime les segments du remplissage mort et récupère tout depuis le
  début.
- **Les restes sont balayés.** Toutes les 5 minutes, un réplica (élu par un
  verrou consultatif PostgreSQL) réclame chaque bail expiré, puis en supprime
  les segments et la ligne, ainsi que les clés de préparation de plus de 24 h.

::: warning Ajoutez une règle de cycle de vie au bucket S3
Le balayage n'abandonne pas les envois multipart incomplets. Dotez le bucket
d'une règle de cycle de vie qui abandonne les envois multipart incomplets après
1 jour et fait expirer les objets sous le préfixe `staging:` (sous votre
`prefix`, s'il est défini) après 1 jour.
:::

Ce qu'un remplissage coûte en stockage, en mémoire, en requêtes S3 et en
requêtes SQL figure dans [Dimensionnement](./capacity-planning#fill-cost).

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
    image: ghcr.io/batlehub/batlehub:1.7.0
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
  repository: ghcr.io/batlehub/batlehub
  tag: "1.1.0"    # figez une version précise

# La configuration applicative : le chart sérialise `config` en config.toml.
config:
  database:
    type: "postgresql"
    url:  "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"
  storage:
    type:   "s3"
    bucket: "batlehub-artifacts"
    region: "us-east-1"
    path:   null             # retire le défaut système de fichiers du chart
  # Requis au-delà d'un réplica — le chart refuse de rendre sans lui.
  cache:
    type: "redis"            # ou "postgres" (url vaut database.url par défaut)
    url:  "redis://redis-svc:6379"

# Les identifiants S3 viennent de la chaîne du SDK AWS : un rôle de pod, ou ces variables.
envFrom:
  - secretRef:
      name: batlehub-s3      # AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY

# Pas besoin de PVC — tous les artefacts vont sur S3.
persistence:
  enabled: false

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
# La stratégie du Deployment — pas une valeur du chart ; appliquez-la par le patch ci-dessous :
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
config:
  otel:
    endpoint:     "http://otel-collector:4317"
    service_name: "batlehub"
```

Chaque réplica émet ses propres spans ; le collecteur les recoud en traces
complètes par identifiant de trace. Voir le
[guide d'administration](/fr/guide/admin-storage-health#health) pour le démarrage
rapide avec Jaeger.

### Métriques des remplissages à froid {#fill-metrics}

| Métrique | Ce qu'elle indique |
| --- | --- |
| `batlehub_proxy_fill_active` | Remplissages en cours sur ce réplica |
| `batlehub_proxy_fill_joined_total{registry,scope}` | Requêtes qui ont rejoint un remplissage, sur ce réplica (`local`) ou un autre (`remote`) |
| `batlehub_proxy_fill_first_byte_seconds{scope}` | Délai jusqu'au premier octet d'une requête jointe |
| `batlehub_proxy_fill_segments_total{registry}` | Segments validés dans le stockage |
| `batlehub_proxy_fill_assembly_seconds` | Durée de l'assemblage des segments en blob |
| `batlehub_proxy_fill_follow_polls_total` | Interrogations du bail par un suiveur dont la connexion `LISTEN` est coupée |
| `batlehub_proxy_fill_takeovers_total{registry}` | Remplissages repris sur un bail expiré |
| `batlehub_proxy_fill_lease_lost_total{registry}` | Remplissages qui ont perdu leur bail |
| `batlehub_proxy_fill_lease_errors_total` | Échecs du magasin de baux — le réplica remplit alors de son côté, comme un réplica seul |
| `batlehub_proxy_fill_orphans_deleted_total` | Restes supprimés par le balayage |

Deux alertes valent la peine, et aucune ne doit réveiller quelqu'un : toutes
deux dégradent le coût (trafic amont, une nouvelle tentative du client), pas
la justesse :

```promql
increase(batlehub_proxy_fill_takeovers_total[1h]) > 0     # hors déploiement : des pods meurent en plein remplissage
increase(batlehub_proxy_fill_lease_errors_total[15m]) > 0 # des réplicas remplissent de leur côté
```

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

- **Un rechargement de configuration n'atteint qu'un réplica** — l'éditeur de
  configuration et un changement de fichier approuvé sont appliqués par le
  réplica qui traite la requête, et par aucun autre : le rechargement en attente
  vit dans la mémoire de ce processus et le fichier sur le montage de ce pod.
  Appliquez un changement sur chaque réplica, ou déployez-les depuis les mêmes
  fichiers (`helm upgrade`). Tant que les réplicas qui se sont signalés dans la
  dernière minute ne servent pas tous la même configuration, la page
  **Configuration** affiche un avertissement `config.replica-drift` qui nomme
  chacun d'eux et la configuration qu'il sert. Chaque réplica envoie toutes les
  quinze secondes une empreinte de sa configuration fusionnée à PostgreSQL
  (`system_kv`), si bien qu'un fichier reformaté mais identique ne compte pas
  comme une différence.
  L'accepter avec `[console] accepted = ["warning:config.replica-drift"]` ne
  prend effet que lorsque la configuration de chaque réplica signalé le dit :
  la réponse ne dépend donc pas du réplica qui sert la page.

- **Le catalogue suit en quinze secondes** — un paquet qu'un réplica publie,
  supprime ou met en cache pour la première fois vide les listes en cache de ce
  registre sur tous les réplicas, à leur prochaine interrogation de PostgreSQL,
  toutes les quinze secondes.

  Les deux ne tournent que lorsque `[cache]` est partagé (`redis` ou
  `postgres`), ce qu'exige de toute façon plus d'un réplica : un serveur seul en
  `type = "memory"` n'a aucun réplica avec qui s'accorder, et ne paie pas la
  requête toutes les quinze secondes.

- **Des magasins en mémoire si la configuration est fautive** — si `[cache]`
  reste au défaut `type = "memory"`, chaque réplica tient son propre état de
  limitation de débit et de blocage d'IP. Les limites sont alors N fois plus
  permissives que configurées (N étant le nombre de réplicas), et un blocage
  d'IP posé sur un réplica ne se propage pas aux autres, et chaque réplica
  remplit un artefact froid de son côté. **Déclarez toujours un backend de
  cache partagé pour un déploiement multi-réplicas** — le chart Helm refuse de
  rendre sans lui lorsqu'il peut lire la configuration
  ([Le backend de cache](#config-cache)).
