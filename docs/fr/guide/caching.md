---
sourcePath: guide/caching.md
sourceHash: 9be45cf333d73969
---

# Mise en cache

BatleHub se place entre vos outils de build et les registres amont. Cette page
explique exactement comment le cache fonctionne : depuis l'instant où un client
émet une requête, en passant par la consultation du cache, jusqu'à la réponse — et
comment régler chaque étape de ce trajet.

---

## Comment fonctionne le cache

### Le cycle de vie d'une requête

Toute requête vers `/proxy/{registry}/...` traverse ce pipeline :

```
Client
  │
  ▼
AuthMiddleware          ← valide le token bearer / OIDC / compte de service Kubernetes
  │  pose l'Identity (user_id, role, groups)
  ▼
RateLimitMiddleware     ← contrôle et incrémente les compteurs dans le backend de cache
  │  429 si un seau est épuisé (mode block)
  ▼
Contrôle RBAC           ← valide les permissions de registre pour le rôle de l'appelant
  │  403 si la permission est refusée
  ▼
Contrôle des règles     ← garde-fou d'âge, deny_latest, require_signed_release
  │  403 si une règle se déclenche
  ▼
Cache de métadonnées    ← consulté dans le backend de cache (memory / postgres / redis)
  │
  ├─ HIT :  on continue avec les métadonnées en cache
  │
  └─ MISS : récupération en amont, stockage dans le backend avec un TTL
              │
              ▼
          Stockage de l'artefact   ← consulté dans le stockage de blobs (fichiers / S3)
            │
            ├─ HIT :  l'artefact est diffusé du stockage vers le client
            │
            └─ MISS : récupération en amont, stockage du blob, diffusion vers le client
```

### Deux couches d'état

BatleHub sépare deux sortes d'état mis en cache, aux durées de vie et aux
backends distincts :

| Couche | Ce qui est stocké | Où | Durée de vie |
|-------|---------------|-------|---------|
| **Cache de métadonnées** | Listes de versions, informations de release, métadonnées de paquet | Backend de cache (`[cache]`) | `metadata_ttl_secs` (5 min par défaut), puis récupéré à nouveau |
| **Stockage des artefacts** | Tarballs, fichiers `.crate`, paquets VSIX, zips de modules Go | Stockage de blobs (`[storage]`) | Permanent par défaut ; gouverné par la politique d'éviction |
| **Compteurs de limitation** | Nombre de requêtes par utilisateur ou par groupe | Backend de cache (`[cache]`) | Une fenêtre fixe (`window_secs`), puis remise à zéro |

Les métadonnées sont délibérément éphémères : les listes de versions changent à
mesure que des paquets sont publiés en amont. Les artefacts sont stockés
définitivement, parce qu'un `.crate` ou un tarball à une version donnée ne change
jamais.

### Ce qu'une réponse d'artefact dit d'elle-même

Toute réponse d'artefact nomme l'endroit où les octets sont gardés et ce sous
quoi ils sont classés :

| En-tête | Signification |
| --- | --- |
| `X-BatleHub-Storage-Key` | la clé dans le stockage de blobs — `artifact:{registry}/{name}/{version}[/{file}]` |
| `X-BatleHub-Package` | le nom du paquet, qui peut contenir des barres obliques (`cli/cli`, `@scope/pkg`) |
| `X-BatleHub-Version` | la version, ou le commit sur une archive de forge |

Ils ne divulguent rien de neuf : chaque partie figure dans l'URL demandée. Ils
existent parce que la clé est fonction de la **route** plutôt que de l'URL
amont : un client ne peut donc pas la dériver — le même asset GitHub est classé
sous `…/{tag}/filename/{file}` quand il est récupéré par nom et sous
`…/unknown/{id}` quand il l'est par identifiant, et une archive de forge est
classée sous le commit vers lequel sa ref s'est résolue, pas sous la branche
demandée.

`batlehub mise export` lit les trois
([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate) §14.1), et c'est ainsi qu'un
lot de coupure réseau nomme des clés que l'instance déconnectée cherchera
réellement.

---

## Le backend de cache — `[cache]` {#cache-backend}

La section `[cache]` choisit le moteur de stockage des **métadonnées** et des
**compteurs de limitation de débit**. Trois backends existent :

```toml
# Mémoire du processus (défaut — aucune infrastructure supplémentaire)
[cache]
type = "memory"

# PostgreSQL — persistant, partagé par tous les réplicas du serveur
[cache]
type = "postgres"
# Emploie la même URL de base que [database] ; rien d'autre à configurer.

# Redis — persistant, partagé, éviction par TTL, latence plus basse que Postgres
[cache]
type = "redis"
url  = "redis://localhost:6379"
```

### Choisir un backend

| Backend | Persistance | Multi-instance | Infra supplémentaire | Convient à |
|---------|:-----------:|:--------------:|:-----------:|---------|
| `memory` | Non — remis à zéro au redémarrage | Non — chaque instance a ses compteurs | Aucune | Développement local, mono-nœud |
| `postgres` | Oui | Oui — toutes les instances partagent une base | PostgreSQL (déjà requis) | Production, multi-réplicas |
| `redis` | Oui | Oui — toutes les instances partagent un cluster | Redis | Production à fort débit |

::: tip Déploiements mono-nœud
`memory` est le défaut et ne demande aucune configuration. Passez à `postgres` ou
`redis` quand vous faites tourner plusieurs réplicas, ou quand les compteurs de
limitation et le cache de métadonnées doivent survivre aux redémarrages.
:::

::: warning Le drapeau de fonctionnalité Redis
Le backend Redis n'est compilé que si la fonctionnalité Cargo `cache-redis` est
activée. L'image Docker officielle l'inclut. En compilant depuis les sources,
ajoutez `--features cache-redis` à la commande `cargo build`.
:::

### Ce qui change d'un backend à l'autre

**Cache de métadonnées :** quand un client demande une liste de versions et que
le résultat est en cache, le backend est interrogé (recherche dans une table de
hachage, lecture d'une ligne en base, ou `GET` Redis). En cas de défaut, l'amont
est contacté et le résultat est stocké avec un TTL.

**Compteurs de limitation :** chaque appel d'incrément augmente atomiquement un
compteur indexé par `rl:{registry}:user:{user_id}` (ou
`rl:{registry}:group:{group}`) et renvoie le nouveau compte ainsi que
l'horodatage de réinitialisation de la fenêtre. Avec `memory`, c'est une
opération sur un `Mutex<HashMap>`. Avec `postgres`, c'est un
`INSERT … ON CONFLICT DO UPDATE … RETURNING count`, sérialisable sous charge
concurrente. Avec `redis`, c'est un `INCR` atomique, avec un `EXPIRE`
conditionnel à la première écriture.

---

## Politique de cache par registre — `[registries.cache]` {#registry-cache-policy}

Chaque registre a son bloc `[registries.cache]`, qui gouverne :

- combien de temps les métadonnées sont réputées fraîches ;
- s'il faut servir des métadonnées périmées quand l'amont est en panne ;
- quand les artefacts sont évincés du stockage de blobs ;
- comment préremplir le cache avant la première requête d'un client.

```toml
[registries.cache]
metadata_ttl_secs = 300       # revérifier les listes de versions toutes les 5 min (défaut)
serve_stale       = true      # servir des métadonnées en cache sur un 5xx amont (défaut)

# TTL des artefacts (facultatif) — re-récupérer les artefacts plus vieux que N secondes
artifact_ttl_secs = 2592000   # supprimer ou re-récupérer les artefacts de plus de 30 jours

# Stratégies d'éviction supplémentaires — toutes facultatives, elles se combinent
idle_days         = 14        # supprimer les artefacts non accédés depuis 14 jours
max_size_bytes    = 10737418240  # plafond de 10 Gio — évince les moins récemment utilisés au-delà
keep_latest_n     = 5         # ne garder que les 5 versions les plus récentes par paquet

# Préchauffage
warm_packages    = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n    = 3          # préchauffer les 3 versions les plus récentes des entrées sans version
warm_concurrency = 4          # jusqu'à 4 téléchargements en parallèle
```

### Le TTL des métadonnées et `serve_stale`

Les métadonnées (listes de versions, informations de release) sont mises en cache
pendant `metadata_ttl_secs` secondes. Après expiration :

- si l'amont répond, les données fraîches sont stockées et le TTL est
  réinitialisé ;
- si l'amont renvoie un 5xx **et** que `serve_stale = true` (le défaut), les
  données périmées du cache sont renvoyées, de sorte que les clients continuent
  de fonctionner pendant une panne amont ;
- si l'amont renvoie un 5xx **et** que `serve_stale = false`, l'erreur est
  propagée au client en `502 Bad Gateway`.

Mettez `metadata_ttl_secs = 0` pour revérifier l'amont à chaque requête (utile en
mode `local` et `hybrid`, où l'on veut un index toujours frais).

### Savoir quand vous servez des données périmées

BatleHub tient une moyenne glissante du taux d'erreur et de la latence par
registre, et le marque **dégradé** dès que les appels amont échouent souvent ou
répondent lentement — exactement la situation où `serve_stale` sert peut-être
discrètement de vieilles données. Vérifiez-le par :

- `GET /api/v1/admin/stats` — les `upstream_degraded`, `upstream_error_rate` et
  `upstream_latency_ms` par registre ;
- la jauge Prometheus `batlehub_upstream_health_degraded{registry}` (0/1), pour
  l'alerting ;
- une ligne de log `tracing::warn!` à la transition sain → dégradé (et un log
  d'information au retour à la normale).

### Le TTL des artefacts

Par défaut, une fois téléchargé, un artefact est conservé jusqu'à ce qu'une
politique d'éviction le retire. `artifact_ttl_secs` les fait expirer par âge — la
requête suivant l'expiration le récupère à nouveau en amont et remet le compteur
à zéro :

```toml
[registries.cache]
artifact_ttl_secs = 86400   # re-récupérer les artefacts de plus de 24 heures
```

C'est utile pour les registres où un paquet peut être modifié sur place (rare
pour les registres publics, mais possible avec certains miroirs privés).

### Les politiques d'éviction

L'éviction est évaluée paresseusement — un artefact est évincé au moment où il
serait servi, si une politique le dit expiré. Les stratégies se combinent : la
première qui se déclenche provoque l'éviction.

::: tip Les artefacts sans métadonnées de cache
Si un artefact a été stocké avant l'application de la migration
`artifact_cache_meta` (ajoutée dans une version récente), il n'a pas
d'horodatage `cached_at`. Quand `artifact_ttl_secs` est configuré, un tel
artefact est prudemment considéré comme expiré et re-récupéré en amont au
prochain accès — le bon comportement est d'obtenir une copie fraîche plutôt que
de servir indéfiniment un artefact d'âge inconnu.
:::

| Politique | Clé de configuration | Effet |
|--------|-----------|--------|
| **Âge** | `artifact_ttl_secs` | Retirer les artefacts plus vieux que N secondes |
| **Inactivité** | `idle_days` | Retirer les artefacts non accédés depuis N jours |
| **Plafond de taille** | `max_size_bytes` | Quand le stockage dépasse le plafond, les artefacts les moins récemment utilisés sont retirés jusqu'à repasser sous la limite |
| **Nombre de versions** | `keep_latest_n` | Ne garder que les N versions les plus récemment mises en cache par paquet ; les plus anciennes sont retirées à l'arrivée d'une nouvelle |

Omettre les quatre champs désactive l'éviction — les artefacts sont conservés
indéfiniment.

Pour lancer une passe à la demande, en prévisualiser une avant de lâcher un
nouveau plafond de taille, ou lire dans la piste d'audit ce qu'une passe a
retiré, voir
[Lancer une passe, et la prévisualiser](/fr/guide/admin-policies#cache-eviction-run).

Aucune stratégie d'éviction ne considère un blob dont la ligne de métadonnées
manque — toutes lisent cette table. Les récupérer est
[une passe distincte](/fr/guide/admin-policies#cache-coherence).

---

## Le préchauffage du cache {#cache-warming}

Le préchauffage récupère des paquets à l'avance, de sorte qu'ils soient
disponibles sans latence avant qu'un client ne les demande. Il se configure dans
`[registries.cache]` :

```toml
[registries.cache]
warm_packages    = ["lodash", "react@18.2.0", "serde"]
warm_latest_n    = 3      # préchauffer les 3 versions les plus récentes des entrées sans version
warm_concurrency = 4      # jusqu'à 4 téléchargements en parallèle pendant le préchauffage
```

- **Nom nu** (`"lodash"`) : préchauffe les `warm_latest_n` versions les plus
  récentes.
- **Version figée** (`"react@18.2.0"`) : préchauffe exactement une version.

Le préchauffage s'exécute au démarrage, en tâche de fond — le serveur HTTP est
immédiatement disponible pendant qu'il se déroule. Vous pouvez aussi le
déclencher à la demande par l'API d'administration :

```sh
# Préchauffer avec le warm_latest_n configuré du registre
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash"}'

# Préchauffer une version précise
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash@4.17.21"}'
```

::: tip Ce que chaque registre gère
L'énumération des versions (nécessaire pour préchauffer un nom nu) ne concerne
que les types de registre fondés sur le paquet — ceux adressés par un nom et une
version — et exclut ceux adressés par chemin (Deb, RPM, Pacman, IDE JetBrains,
Generic). Elle est implémentée pour tous les types fondés sur le paquet : les
noms nus fonctionnent donc partout ; passez une entrée `name@version` figée quand
vous voulez exactement une version. La moitié « nom » est la coordonnée par
laquelle le registre adresse ses paquets :

| Registre | Nom nu | Version figée |
| --- | --- | --- |
| npm | `lodash` | `lodash@4.17.21` |
| Maven | `com.google.guava:guava` | `com.google.guava:guava@33.0.0-jre` |
| Terraform | `providers/hashicorp/aws` | `providers/hashicorp/aws@5.0.0` |
| RubyGems | `rails` | `rails@7.1.0` |
| Composer | `monolog/monolog` | `monolog/monolog@3.5.0` |

Les noms npm à scope gardent leur `@` initial comme partie du nom
(`@scope/pkg`, `@scope/pkg@1.2.3`). Pour **GitHub**, un nom nu énumère les
releases par l'API Releases. Pour la **place de marché VS Code**, il énumère
toutes les versions d'extension par l'API Gallery. Pour **Conda**, la liste des
versions est synthétisée en parcourant `repodata.json` sur les plateformes
standard. Pour la **place de marché JetBrains**, une entrée est l'`xmlId` du
plugin et un nom nu n'énumère que le canal **Stable**. Les types adressés par
chemin (Deb, RPM, Pacman, IDE JetBrains, Generic) n'ont pas de modèle de version
du tout et préchauffent `warm_paths` plutôt que `warm_packages`. Les deux types
de chaînes d'outils (**distributions Node**, **SDKMAN**) sont une archive *par
plateforme* : une entrée comme `node@v22.11.0` ou `java@21.0.5-tem` préchauffe
donc un fichier par entrée de `warm_platforms` (`linux-x64`, `darwin-arm64`, …
pour Node ; `linuxx64`, `darwinarm64`, … pour SDKMAN), avec pour défaut la
plateforme sur laquelle tourne ce serveur — deviner toutes les plateformes
récupérerait 1,6 Go de JDK pour un `.sdkmanrc` d'une ligne (RFC 0010 §6.9).
:::

---

## Déduplication par le contenu {#deduplication}

Les octets d'un artefact sont stockés sous une clé adressée par le contenu
(`blob/{sha256}`) dans le stockage de blobs. Chaque chemin d'artefact logique
(par exemple `artifact:npm/lodash/4.17.21`) porte une référence vers le blob
plutôt que les octets eux-mêmes. Une table de compteurs de références suit
combien de clés logiques pointent vers chaque blob.

Ce qui implique que :
- le même paquet publié sur deux registres n'est stocké qu'une fois ;
- une version retirée puis republiée avec le même contenu n'est stockée qu'une
  fois ;
- la déduplication est transparente — aucune configuration nécessaire.

Les tables de déduplication (`artifact_dedup_index`, `artifact_dedup_refs`) sont
créées par la migration de base et maintenues automatiquement.

---

## Limitation de débit et backend de cache {#rate-limiting}

Les compteurs de limitation sont stockés dans le même backend que le cache de
métadonnées. Ce qui implique que :

- avec `[cache] type = "memory"`, les compteurs sont propres au processus.
  Redémarrer le serveur ou faire tourner plusieurs réplicas donne à chaque
  processus ses propres compteurs indépendants ;
- avec `[cache] type = "postgres"` ou `type = "redis"`, les compteurs sont
  partagés par tous les réplicas et survivent aux redémarrages. Un utilisateur
  qui atteint la limite sur un réplica ne peut pas la contourner en routant sa
  requête suivante vers un autre.

La limitation se configure registre par registre :

```toml
[registries.rate_limit]
requests_per_window = 200    # par utilisateur authentifié, ou par IP cliente pour les anonymes
window_secs         = 60
enforcement         = "block"   # "block" renvoie 429 ; "warn" laisse passer la requête

# Réservoir partagé pour tous les membres du groupe des bots de CI :
[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 5000
window_secs         = 60
```

### En-têtes de réponse

| En-tête | Condition | Valeur |
|--------|-----------|-------|
| `X-RateLimit-Limit` | Toute réponse proxifiée (quand la limitation est configurée) | La limite la plus restrictive qui s'est appliquée à cette requête |
| `Retry-After` | Réponse 429 (mode block) | Secondes avant la réinitialisation de la fenêtre courante |
| `X-RateLimit-Reset` | Réponse 429 (mode block) | Horodatage Unix de la réinitialisation de la fenêtre courante |
| `X-RateLimit-Warning: rate-limit-exceeded` | Réponse au-delà de la limite (mode warn) | Présent quand la requête a été autorisée malgré le dépassement |

### Sémantique de la fenêtre fixe

BatleHub emploie un compteur à **fenêtre fixe** (ni fenêtre glissante, ni seau à
jetons). Chaque fenêtre est alignée sur l'époque Unix :

```
window_start = floor(now_unix / window_secs) * window_secs
```

Par exemple, avec `window_secs = 60`, les fenêtres vont de `:00` à `:59` de
chaque minute, puis repartent. L'en-tête `X-RateLimit-Reset` donne l'horodatage
Unix exact de la prochaine frontière de fenêtre.

### Comportement en cas de panne : laisser passer

Si le backend de cache est indisponible au moment d'incrémenter un compteur, la
requête est **autorisée** plutôt que rejetée. Cela évite que le backend de cache
ne devienne un point de défaillance unique pour tout le proxy.

Quand un seau ne peut pas être incrémenté à cause d'une erreur du magasin,
BatleHub émet un log `WARN` (`rate-limit store unavailable … failing open`), de
sorte que la panne est visible dans votre outillage d'observabilité même si les
requêtes ne sont pas bloquées. Surveillez ces avertissements et vérifiez
l'endpoint de santé si vous soupçonnez que la limitation ne s'applique pas.

---

## Exemples commentés

### Mono-nœud avec cache en mémoire (défaut)

Aucune configuration supplémentaire — ça marche d'emblée :

```toml
[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@localhost:5432/batlehub"

# [cache] vaut type = "memory" par défaut

[[registries]]
type = "npm"
name = "npm"

[registries.cache]
metadata_ttl_secs = 300
keep_latest_n     = 10
```

### Multi-réplicas avec cache PostgreSQL

Tous les réplicas partagent la même base : le cache de métadonnées et les
compteurs de limitation sont donc cohérents.

```toml
[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@db:5432/batlehub"

[cache]
type = "postgres"

[[registries]]
type = "npm"
name = "npm"

[registries.cache]
metadata_ttl_secs = 60
serve_stale       = true

[registries.rate_limit]
requests_per_window = 1000
window_secs         = 60
enforcement         = "block"
```

### Fort débit avec cache Redis

Redis offre une latence par opération plus basse que PostgreSQL sur les chemins
chauds.

```toml
[cache]
type = "redis"
url  = "redis://redis:6379"

[[registries]]
type = "npm"
name = "npm"

[registries.cache]
metadata_ttl_secs = 120

[registries.rate_limit]
requests_per_window = 5000
window_secs         = 60
enforcement         = "block"

[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 50000
window_secs         = 60
```

### Éviction agressive (espace contraint)

```toml
[registries.cache]
metadata_ttl_secs = 600
artifact_ttl_secs = 604800   # 7 jours
idle_days         = 3
max_size_bytes    = 5368709120  # 5 Gio
keep_latest_n     = 3
warm_packages     = ["lodash", "react", "axios"]
warm_latest_n     = 1
```
