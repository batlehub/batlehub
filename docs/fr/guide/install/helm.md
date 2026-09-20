---
sourcePath: guide/install/helm.md
sourceHash: 88aa0f4786ab789b
---

# Chart Helm

Déployez BatleHub sur Kubernetes avec le chart Helm fourni. Trois fichiers de
valeurs complets suivent — [un réplica sans Redis ni S3](#helm-basic),
[S3 avec plusieurs réplicas](#helm-s3) et un
[setup prêt pour la production](#helm-prod) — chacun un `my-values.yaml` entier
plutôt qu'un fragment, et chacun est un fichier du dépôt, sous `deploy/helm/`.
[Quel setup](#helm-setups) en est la comparaison en un tableau, et
[Fichiers de valeurs prêts à l'emploi](#helm-examples) liste les quatre autres
exemples — secrets externes, tous les types de registre, et une configuration
permissive face à une configuration verrouillée.

**Prérequis :** Helm 3+, un cluster Kubernetes en fonctionnement, PostgreSQL
joignable depuis le cluster.

## Installation rapide

```sh
# Clonez le dépôt (le chart est fourni dans helm/batlehub/)
git clone https://github.com/batlehub/batlehub
cd batlehub

helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  --set config.database.url="postgresql://batlehub:changeme@postgres-svc:5432/batlehub" \
  --set "config.auth[0].type=token" \
  --set "config.auth[0].tokens[0].value=my-admin-token" \
  --set "config.auth[0].tokens[0].role=admin" \
  --set "config.auth[0].tokens[0].user_id=admin"
```

::: warning
Toutes les clés vivent sous `config`, l'objet sérialisé tel quel dans
`config.toml`. Helm accepte un `--set` sur une clé que le chart ne connaît pas
sans rien dire : un chemin mal orthographié installe donc silencieusement les
valeurs par défaut — la base d'exemple et le token d'admin
`change-me-admin-token`. Vérifiez ce que vous vous apprêtez à installer avec
`helm template` avant `helm install`.
:::

Passé un premier essai, installez depuis un fichier de valeurs plutôt qu'avec un
mur de `--set`. Les trois qui suivent sont complets : chacun est un
`my-values.yaml` entier, installé par

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub --create-namespace -f my-values.yaml
```

## Quel setup {#helm-setups}

Les trois se distinguent par une décision chacun — où vivent les artefacts, et
si plus d'un réplica peut les servir. Tout le reste en découle.

| | [Basique](#helm-basic) | [S3](#helm-s3) | [Production](#helm-prod) |
|---|---|---|---|
| Artefacts | PVC (`ReadWriteOnce`) | bucket S3 | bucket S3 |
| Réplicas | 1 | 2+ | 2+, anti-affinité, PDB |
| Cache de métadonnées | en mémoire du processus | PostgreSQL | Redis |
| Services supplémentaires | PostgreSQL | PostgreSQL, S3 | PostgreSQL, S3, Redis |
| Secrets | dans les valeurs | dans les valeurs | Secret dédié, cycle de vie propre |
| Identités | un token statique | un token statique | OIDC + tokens CI |
| Analyse | dans le pod du proxy | dans le pod du proxy | Deployment worker dédié |
| Pour | une équipe, un labo, une première installation | cache partagé, mises à jour progressives | une instance dont d'autres dépendent |

Un nombre de réplicas supérieur à 1 est la ligne qui impose le reste : un
`ReadWriteOnce` ne se partage pas entre nœuds, et un cache de métadonnées en
mémoire du processus ne se partage pas du tout — un deuxième réplica signifie
donc S3 et un cache partagé. En deçà de cette ligne, le setup basique n'est pas
une version diminuée des autres : c'est le produit entier sur un seul pod.

## Setup 1 — un réplica, sans Redis ni S3 {#helm-basic}

PostgreSQL est la seule chose à apporter. Les artefacts vont sur un PVC, les
métadonnées sont mises en cache dans le processus, et aucun bloc `[cache]`
n'est écrit — absent signifie en mémoire.

Le fichier est `deploy/helm/values-minimal.yaml` :

```yaml
# deploy/helm/values-minimal.yaml
replicaCount: 1

config:
  database:
    type: "postgresql"
    url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"

  auth:
    - type: "token"
      tokens:
        - value: "my-admin-token"
          role: "admin"
          user_id: "admin"

  registries:
    - type: "npm"
      name: "npm"
      rbac:
        anonymous: ["releases:read", "source:read"]
        user: ["releases:read", "source:read"]
        admin: ["*"]

    - type: "cargo"
      name: "internal"
      mode: "local"
      rbac:
        user: ["source:read"]
        admin: ["*"]

# Aucun bloc [cache] : absent signifie en mémoire, ce qui est juste pour un
# réplica et faux pour deux.

persistence:
  enabled: true
  size: 50Gi

ingress:
  enabled: true
  className: nginx
  host: batlehub.example.com
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com
```

Trois choses à savoir avant de faire grandir celui-ci :

- **`persistence.enabled: false` ne veut pas dire « pas de cache ».** En
  stockage fichier le cache devient un `emptyDir`, plafonné par
  `persistence.ephemeralSizeLimit` — la racine du conteneur est en lecture
  seule, il lui faut un volume inscriptible de toute façon. Chaque redémarrage
  repart à froid.
- **Le PodDisruptionBudget n'est pas rendu à un seul réplica**, volontairement :
  un budget sur un pod unique bloque les vidanges de nœud sans rien apporter en
  disponibilité.
- **Dimensionnez le PVC pour le cache visé, pas pour celui d'aujourd'hui.**
  [Dimensionnement](/fr/guide/capacity-planning) convertit un nombre de paquets
  en gigaoctets.

## Setup 2 — artefacts sur S3, plusieurs réplicas {#helm-s3}

Deux changements par rapport au précédent : les artefacts passent dans un bucket
que tous les réplicas peuvent écrire, et le cache de métadonnées passe dans
PostgreSQL pour que les réplicas s'accordent. Aucun service supplémentaire — la
base est déjà là.

Le fichier complet est `deploy/helm/values-multi-replica.yaml` ; voici les blocs
qui diffèrent du Setup 1 :

```yaml
replicaCount: 2

config:
  database:
    type: "postgresql"
    url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"
    # Chaque réplica ouvre son propre pool. Abaissez cette valeur derrière un pooler.
    max_connections: 5

  storage:
    type: "s3"
    bucket: "batlehub-artifacts"
    region: "us-east-1"
    # prefix: "prod"          # facultatif : ranger sous un sous-dossier du bucket
    # endpoint_url: "http://rustfs:9000"   # à renseigner pour RustFS et assimilés
    # force_path_style: true               # exigé par la plupart des stores non-AWS

  # Cache de métadonnées partagé. C'est aussi ce que lisent la limitation de
  # débit et le blocage d'IP : laissé en mémoire, chaque réplica compte seul.
  cache:
    type: "postgres"
    # url reprend database.url par défaut — ne la fixez que pour une connexion distincte

persistence:
  enabled: false   # pas de PVC : chaque artefact est dans le bucket

# Identifiants S3, pour un cluster sans rôle IAM à endosser.
envFrom:
  - secretRef:
      name: batlehub-s3-credentials   # AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
```

Le bloc de stockage ne porte aucune clé d'accès, parce qu'un tel champ n'existe
pas — et un `access_key_id` ajouté là n'est pas refusé, il est ignoré, ce qui
ressemble trait pour trait à un problème de politique de bucket. Les
identifiants S3 viennent de la chaîne standard du SDK AWS : le rôle IAM du pod
(IRSA, Workload Identity — rien à configurer ici), ou `AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY` fournis via `env` ou `envFrom` comme ci-dessus.

Au-delà d'un réplica, le chart rend le PodDisruptionBudget (`minAvailable: 1`
par défaut) et une mise à jour progressive ne coupe plus le registre. Répartir
ces réplicas entre les nœuds est l'affaire du [Setup 3](#helm-prod).

## Setup 3 — prêt pour la production {#helm-prod}

Le Setup 2, plus les quatre choses qui séparent une instance que l'on exploite
d'une instance dont d'autres dépendent : des secrets au cycle de vie propre, de
vraies identités, des réplicas qui survivent à un nœud, et un rayon d'explosion.

Le fichier est `deploy/helm/values-production.yaml` :

```yaml
# deploy/helm/values-production.yaml
replicaCount: 3

resources:
  requests: { cpu: 500m, memory: 512Mi, ephemeral-storage: 256Mi }
  limits:   { memory: 2Gi, ephemeral-storage: 1Gi }

# Garder les réplicas hors d'un même nœud — un PDB protège une vidange, pas une panne.
affinity:
  podAntiAffinity:
    preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 100
        podAffinityTerm:
          topologyKey: kubernetes.io/hostname
          labelSelector:
            matchLabels:
              app.kubernetes.io/name: batlehub

podDisruptionBudget:
  enabled: true
  minAvailable: 2

config:
  server:
    roles: ["proxy"]          # l'analyse part vers le Deployment worker ci-dessous
    # Le CIDR des pods du contrôleur d'ingress. Obligatoire dès que le routage
    # par hôte est actif, et ce qui rend X-Forwarded-For digne de confiance pour
    # la limitation de débit et l'audit.
    trusted_proxies: ["10.42.0.0/16"]

  storage:
    type: "s3"
    bucket: "batlehub-artifacts"
    region: "us-east-1"

  # Redis plutôt que postgres : le cache de métadonnées est sur le chemin chaud
  # de chaque requête, et c'est la lecture que la base ne devrait pas servir.
  cache:
    type: "redis"
    url: "redis://redis:6379"

  auth:
    # Les personnes, via votre IdP. Le secret est un marqueur — voir `env` plus bas.
    - type: "oidc"
      issuer_url: "https://sso.example.com/application/o/batlehub/"
      client_id: "batlehub"
      client_secret: "${OIDC_CLIENT_SECRET}"
      redirect_uri: "https://batlehub.example.com/api/v1/auth/oidc/callback"
      scopes: ["openid", "profile", "email", "groups"]
      user_id_claim: "preferred_username"
      role_claim: "groups"
      role_mappings:
        "platform-admins": "admin"
        "developers": "user"
    # La CI, via le cluster où elle tourne — aucun token durable à faire tourner.
    - type: "kubernetes"

  registries:
    - type: "npm"
      name: "npm"
      rbac:
        anonymous: []          # aucune lecture anonyme
        user: ["releases:read", "source:read"]
        admin: ["*"]

  limits:
    max_artifact_size_bytes: 524288000   # 500 Mio

  ip_blocking:
    enabled: true
    violation_threshold: 10
    violation_window_secs: 300
    ban_duration_secs: 3600

  otel:
    endpoint: "http://otel-collector:4317"
    service_name: "batlehub"

env:
  - name: OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef: { name: batlehub-secrets, key: oidc-client-secret }

envFrom:
  - secretRef:
      name: batlehub-s3-credentials

# L'URL de la base et les tokens d'upstream, dans un Secret que ce chart ne rend
# pas et pour lequel il ne redéploie pas les pods. Voir plus bas.
credentials:
  enabled: true
  existingSecret: batlehub-credentials   # depuis ESO, Sealed Secrets, Vault Agent
  key: credentials.toml

persistence:
  enabled: false

ingress:
  enabled: true
  className: nginx
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
  host: batlehub.example.com
  tls:
    - secretName: batlehub-tls
      hosts: ["batlehub.example.com"]

# Seul le worker a besoin de sortir vers Internet. Nommez le contrôleur, sinon
# la règle d'entrée signifie « n'importe quelle source » — voir la note de values.yaml.
networkPolicy:
  enabled: true
  ingressFrom:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: ingress-nginx

# Les scanners retiennent un artefact jusqu'à son verdict, sur leurs propres pods.
worker:
  enabled: true
  runtimeClassName: gvisor       # vide si le nœud n'a ni gVisor ni Kata
  resources:
    requests: { cpu: 500m, memory: 1Gi }
    limits:   { memory: 4Gi }
  autoscaling:
    enabled: true                # nécessite un adaptateur de métriques, voir plus bas
    minReplicas: 1
    maxReplicas: 6

trivy:
  enabled: true
```

Les quatre décisions de ce fichier, et où chacune est expliquée en entier :

- **Des secrets hors du chart.** `credentials` monte un second fichier de
  configuration, fusionné par-dessus le premier, depuis un Secret au cycle de
  vie propre — sa rotation recharge sur place au lieu de redéployer les pods, et
  l'éditeur de configuration de la console ne le lit jamais.
  [Un fichier de configuration séparé pour les identifiants](#helm-credentials) ;
  la forme `${VAR}` à côté est
  [Injecter des secrets par variables d'environnement](#helm-env-vars).
- **Des identités, pas un token partagé.** OIDC pour les personnes,
  `type = "kubernetes"` pour la CI interne au cluster, et `anonymous: []` pour
  qu'une lecture exige l'une des deux.
  [Contrôle d'accès](/fr/guide/access-control).
- **Une disponibilité qui survit à un nœud.** L'anti-affinité répartit les
  réplicas, le PDB protège la vidange, et les deux supposent le cache partagé et
  le S3 du [Setup 2](#helm-s3). [Haute disponibilité](/fr/guide/high-availability)
  en est la version longue, avec le même fichier pour Docker Compose.
- **Un rayon d'explosion.** Une NetworkPolicy, une taille limite, le blocage
  d'IP, et les scanners dans un pod isolé qui leur est propre.
  [Worker d'analyse](#helm-worker) plus bas ; `worker.autoscaling` a besoin de
  prometheus-adapter ou KEDA pour exposer `batlehub_scan_jobs_queued` avant que
  le HPA puisse le lire.

Deux choses que ce fichier ne fait **pas**, parce que le chart ne les fait pas :
il ne déploie ni PostgreSQL ni Redis (apportez les vôtres, ou un sous-chart de
votre choix), et il ne crée aucun bucket.

## Fichiers de valeurs prêts à l'emploi {#helm-examples}

Les trois setups ci-dessus sont des fichiers du dépôt, et quatre autres
couvrent les cas qu'ils laissent de côté. Chacun est complet et s'installe
seul :

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub --create-namespace \
  -f deploy/helm/values-minimal.yaml
```

| Fichier | Ce que c'est |
|---|---|
| `values-minimal.yaml` | [Setup 1](#helm-basic) — un réplica, un PVC, ni Redis ni S3 |
| `values-multi-replica.yaml` | [Setup 2](#helm-s3) — S3, un cache partagé, trois réplicas répartis entre les nœuds |
| `values-production.yaml` | [Setup 3](#helm-prod) — le précédent, plus OIDC, des identifiants externes, une NetworkPolicy et le worker d'analyse |
| `values-external-secrets.yaml` | Tous les identifiants hors du chart : le fichier `credentials` depuis un `existingSecret`, les marqueurs `${VAR}` depuis `env`, et les deux ExternalSecret qui les remplissent |
| `values-all-registries.yaml` | Les 27 types de registre, un bloc `[[registries]]` chacun — la forme d'une instance complète, pas une recommandation d'en exploiter 27 |
| `values-open.yaml` | Open bar : lecture **et** publication anonymes, aucune barrière. Une installation de laboratoire — lisez son en-tête avant qu'elle n'atteigne un réseau qui n'est pas le vôtre |
| `values-restricted.yaml` | Son exact opposé, bloc par bloc : aucun accès anonyme, aucun token statique, quatre règles entre une requête et l'upstream |

Lisez les deux derniers en paire : ils diffèrent sur les blocs qui comptent, ce
qui se voit mieux côte à côte que décrit.

::: tip
`task helm:examples` rend les sept et charge chaque `config.toml` produit dans
le serveur — la vérification qu'un fichier s'installe encore, pas qu'il se lit
encore bien. Elle tourne au même titre que les autres gates : un exemple d'ici
ne peut donc pas pourrir en silence pour échouer dans votre cluster.
:::

## Mise à jour

```sh
helm upgrade batlehub ./helm/batlehub \
  --namespace batlehub \
  -f my-values.yaml
```

Toute modification des valeurs qui change le `config.toml` rendu déclenche un
redéploiement des pods, via l'annotation `checksum/config` présente sur les deux
Deployments. La couche `credentials` en est délibérément exclue : elle ne porte
pas d'annotation de checksum, donc sa rotation recharge sur place. Voir
[Un fichier de configuration séparé pour les identifiants](#helm-credentials).

## Worker d'analyse (RFC 0018) {#helm-worker}

Les scanners qui retiennent un artefact jusqu'à son verdict ont besoin de
chaînes d'outils que l'image du proxy ne porte pas : le chart sait donc les faire
tourner dans un déploiement à part. Activez le worker et retirez le rôle `worker`
du proxy :

```yaml
config:
  server:
    roles: ["proxy"]   # le proxy cesse d'analyser

worker:
  enabled: true
  replicaCount: 1
  # gVisor ou Kata autour du pod entier, en plus de bubblewrap autour de chaque
  # scanner. Laissez vide quand le nœud n'a ni l'un ni l'autre.
  runtimeClassName: ""
  autoscaling:
    enabled: false     # nécessite un adaptateur de métriques, voir ci-dessous
```

L'image du worker (`ghcr.io/batleforc/batlehub-worker`) porte tous les scanners
sauf GuardDog. Activer `[scanners.guarddog]` suppose de pointer
`worker.image.repository` vers `ghcr.io/batleforc/batlehub-worker-guarddog`,
faute de quoi le scanner est refusé au chargement de la configuration.

Chaque scanner tourne sous bubblewrap, qui exige des namespaces utilisateur non
privilégiés sur le nœud. Là où le nœud les interdit, faites tourner le pod sous
une runtime class isolée et mettez `runtime = "none"` dans `[worker.sandbox]`.

`worker.autoscaling` produit un HorizontalPodAutoscaler sur la métrique externe
`batlehub_scan_jobs_queued`, qu'un adaptateur de métriques comme
prometheus-adapter ou le scaler Prometheus de KEDA doit exposer au préalable.
`worker.replicaCount` est ignoré tant qu'il est actif.

Seul le worker a besoin d'un accès sortant vers les artefacts amont, le serveur
Trivy et Rekor. `trivy.enabled` inclut le sous-chart du serveur Trivy, dont
l'endpoint devient `http://<release>-trivy:4954`.

Quels scanners tournent, sur quels registres, et ce que fait une erreur de
scanner relèvent de la configuration, pas des valeurs du chart : voir
[`[scanners]` et `[worker]`](/fr/guide/configuration#scanners-and-worker).

## Les valeurs principales

Le `README.md` du chart porte la table complète, générée depuis `values.yaml` et
tenue à jour par une porte de garde. Voici la liste courte.

| Clé | Défaut | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/batleforc/batlehub` | Image de conteneur |
| `image.tag` | appVersion du chart | Tag de l'image |
| `replicaCount` | `1` | Nombre de réplicas |
| `config` | voir `values.yaml` | Toute la configuration applicative, sérialisée telle quelle dans `config.toml` |
| `config.database.url` | — | Chaîne de connexion PostgreSQL |
| `config.storage.type` | `filesystem` | `filesystem` ou `s3` |
| `config.cache.type` | absent (en mémoire) | Backend du cache de métadonnées : `memory`, `postgres` ou `redis`. Un backend partagé est ce qu'exigent plusieurs réplicas |
| `config.auth` | un token d'admin statique | Blocs `[[auth]]` : `token`, `oidc`, `kubernetes`, `actions-oidc` |
| `config.registries` | exemple npm | Blocs `[[registries]]` |
| `credentials.enabled` | `false` | Monter un second fichier de configuration, fusionné par-dessus `config`, depuis son propre Secret |
| `credentials.existingSecret` | `""` | Utiliser un Secret géré ailleurs plutôt qu'un Secret rendu par ce chart |
| `credentials.config` | `{}` | Le contenu de ce second fichier, de même forme que `config` |
| `ingress.enabled` | `false` | Créer une ressource Ingress |
| `persistence.enabled` | `true` | Créer un PVC pour le cache |
| `persistence.size` | `10Gi` | Capacité du PVC |
| `externalManifest[].mount.asConfig` | — | Remplacer le Secret de configuration géré par le chart par le vôtre |
| `worker.enabled` | `false` | Faire tourner le worker d'analyse dans son propre Deployment |
| `worker.image.repository` | `ghcr.io/batleforc/batlehub-worker` | Image du worker ; la variante `-worker-guarddog` ajoute GuardDog |
| `worker.autoscaling.enabled` | `false` | HPA sur la métrique de jobs en file |
| `worker.runtimeClassName` | `""` | Runtime class isolée autour du pod du worker |
| `trivy.enabled` | `false` | Déployer le sous-chart du serveur Trivy |
| `networkPolicy.enabled` | `false` | Créer une NetworkPolicy pour le service |

## Injecter des secrets par variables d'environnement {#helm-env-vars}

Le fichier de configuration de BatleHub accepte des marqueurs `${VAR_NAME}`,
résolus au démarrage. Le chart Helm sait injecter des variables d'environnement
dans le conteneur pour que ces marqueurs se résolvent à l'exécution — ce qui
garde les secrets entièrement hors du Secret de configuration.

**1. Écrivez des marqueurs `${...}` dans vos valeurs :**

```yaml
# my-values.yaml
config:
  auth:
    - type: "oidc"
      issuer_url: "https://sso.example.com/application/o/batlehub/"
      client_id: "batlehub"
      client_secret: "${OIDC_CLIENT_SECRET}"   # résolu à l'exécution
      redirect_uri: "https://hub.example.com/api/v1/auth/oidc/callback"

  registries:
    - type: "npm"
      name: "internal-npm"
      upstreams:
        - "https://registry.corp.example.com/npm"
      upstream_auth:
        type: "bearer"
        token: "${INTERNAL_NPM_TOKEN}"   # résolu à l'exécution
```

**2a. Injectez chaque secret individuellement (`env` avec `secretKeyRef`) :**

```yaml
# my-values.yaml (suite)
env:
  - name: OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef:
        name: batlehub-secrets
        key: oidc-client-secret
  - name: INTERNAL_NPM_TOKEN
    valueFrom:
      secretKeyRef:
        name: batlehub-secrets
        key: npm-token
```

**2b. Ou importez toutes les clés d'un Secret d'un coup (`envFrom`) :**

```yaml
# my-values.yaml (suite)
envFrom:
  - secretRef:
      name: batlehub-secrets   # toutes les clés de ce Secret deviennent des variables
```

**3. Créez le Secret Kubernetes séparément :**

```yaml
# batlehub-secrets.yaml — géré par Sealed Secrets / ESO / Vault, pas par Helm
apiVersion: v1
kind: Secret
metadata:
  name: batlehub-secrets
  namespace: batlehub
type: Opaque
stringData:
  oidc-client-secret: "my-actual-secret"
  npm-token: "npat-xxxxxxxxxxxx"
```

```sh
kubectl apply -f batlehub-secrets.yaml
helm install batlehub ./helm/batlehub --namespace batlehub -f my-values.yaml
```

::: tip
Si un marqueur référence une variable absente du conteneur, BatleHub s'arrête
immédiatement au démarrage avec un message d'erreur clair nommant la variable
manquante — ce qui évite une mauvaise configuration silencieuse.
:::

---

## Un fichier de configuration séparé pour les identifiants {#helm-credentials}

Les marqueurs ci-dessus mettent un secret dans un champ. Quand ce sont des
*sections* entières qui relèvent d'un autre cycle de vie — l'URL de la base, le
bloc `[[auth]]`, le token amont d'un registre — le second fichier de
configuration convient mieux : le chart le monte depuis son propre Secret, et
`--config` le fusionne par-dessus le premier.

```yaml
# my-values.yaml
config:
  # Tout ce qui n'est pas secret, rendu dans le Secret géré par le chart.
  registries:
    - type: "npm"
      name: "internal-npm"
      upstreams:
        - "https://registry.corp.example.com/npm"

credentials:
  enabled: true
  config:
    database:
      type: "postgresql"
      url: "postgresql://batlehub:the-real-password@postgres:5432/batlehub"
    auth:
      - type: "token"
        tokens:
          - value: "the-real-admin-token"
            role: "admin"
            user_id: "admin"
    # Fusionné sur le registre déclaré plus haut, apparié par `name` — la liste
    # des amonts n'est pas répétée.
    registries:
      - name: "internal-npm"
        upstream_auth:
          type: "bearer"
          token: "npat-xxxxxxxxxxxx"
```

Trois conséquences valent d'être connues avant de s'y fier :

- **Faire tourner le Secret d'identifiants ne redéploie pas les pods.** Le
  kubelet met à jour le fichier monté sur place et le surveillant de
  configuration prend le changement en compte. Rien dans le chart ne pose
  d'annotation `checksum/` sur ce Secret, et c'est voulu — un redéploiement est
  précisément ce que le rechargement à chaud évite.
- **L'éditeur de configuration de la console ne le voit jamais.** L'éditeur lit
  et réécrit la première couche seulement : un fichier qui porte des identifiants
  n'est donc pas servi à un navigateur et ne peut pas être écrasé depuis un
  navigateur. Ce que l'éditeur valide et compare reste la configuration
  fusionnée.
- **Les tableaux fusionnent par `name`, pas par position.** C'est ce qui permet
  au bloc ci-dessus d'ajouter un token à un registre sans redéclarer les vingt
  autres. Les règles complètes sont dans
  [Configuration en couches](/fr/guide/configuration#layered-config-files).

Pour garder les identifiants entièrement hors de Helm, pointez vers un Secret
géré ailleurs et laissez `credentials.config` vide :

```yaml
credentials:
  enabled: true
  existingSecret: batlehub-credentials   # depuis ESO, Sealed Secrets, Vault Agent
  key: credentials.toml
```

Le chart ne rend alors aucun Secret de son côté et monte celui-là. Sa `key` doit
contenir un document TOML — de même forme que `config`, et limité aux parties que
vous voulez tenir à l'écart.

---

## Utiliser un Secret externe (GitOps / Sealed Secrets)

Si vous gérez vos secrets ailleurs (Sealed Secrets, External Secrets Operator,
Vault), créez le Secret vous-même :

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: batlehub-config
  namespace: batlehub
type: Opaque
stringData:
  config.toml: |
    [server]
    host = "0.0.0.0"
    port = 8080

    [database]
    type = "postgresql"
    url  = "postgresql://..."

    [[auth]]
    type = "token"

    [[auth.tokens]]
    value = "my-token"
    role  = "admin"
    user_id = "admin"

    [[registries]]
    type = "npm"
    name = "npm"

    [registries.rbac]
    anonymous = ["releases:read", "source:read"]
```

Dites ensuite au chart de le monter comme configuration, avec une entrée
`externalManifest` qui ne porte pas de `manifest` propre — rien n'est rendu, le
Secret est seulement référencé :

```yaml
# my-values.yaml
externalManifest:
  - name: batlehub-config
    kind: Secret
    mount:
      asConfig: true
      items:
        - key: config.toml
          path: config.toml
```

```sh
helm install batlehub ./helm/batlehub --namespace batlehub -f my-values.yaml
```

Une entrée au plus peut porter `asConfig`. Une couche `credentials` reste
utilisable à côté, et sera fusionnée par-dessus ce que ce Secret contient.

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
