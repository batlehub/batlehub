---
sourcePath: guide/installation.md
sourceHash: 830a7400051cbeea
---

# Installation

BatleHub est un binaire unique adossé à PostgreSQL.

**Sans raison particulière de préférer autre chose, prenez
[Docker Compose](#docker-compose).** Il démarre le serveur et sa base ensemble,
n'exige rien d'installé sinon un moteur de conteneurs, et c'est le chemin le plus
court entre rien et un registre vers lequel pointer un gestionnaire de paquets.
Les trois autres méthodes valent quand votre environnement a déjà tranché pour
vous : un binaire précompilé si vous ne faites pas tourner de conteneurs, une
compilation depuis les sources si vous modifiez le code, et le
[chart Helm](#helm-chart) si vous déployez sur Kubernetes.

---

## Prérequis

Toutes les méthodes d'installation exigent une base **PostgreSQL 14+**. Le
serveur crée son schéma automatiquement au premier démarrage.

---

## Versions précompilées

Chaque version taguée publie des artefacts prêts à l'emploi sur GitHub :

### Image de conteneur (recommandé en production)

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

### Binaire précompilé

Un binaire `batlehub` lié statiquement pour Linux est joint à chaque
[release GitHub](https://github.com/batlehub/batlehub/releases).
Téléchargez-le, rendez-le exécutable et lancez-le :

```sh
curl -L -o batlehub https://github.com/batlehub/batlehub/releases/download/<version>/batlehub
chmod +x batlehub
./batlehub --config config.toml
```

---

## Docker Compose — commencez ici {#docker-compose}

Le moyen le plus rapide d'obtenir une instance qui tourne, pour le développement
local ou pour évaluer le produit.

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

### Avec un stockage S3 (RustFS)

Un fichier Compose distinct ajoute un backend de stockage RustFS (compatible S3)
et l'OIDC d'Authentik :

```sh
podman compose -f docker-compose.s3.yml up -d postgres rustfs
# Puis lancez le serveur avec la configuration S3 :
task run:s3
```

---

## Binaire depuis les sources

**Prérequis :** Rust 1.87+, Node 24+, PostgreSQL

**1. Compilez le backend :**

```sh
cargo build --release -p batlehub-server
```

**2. Compilez la SPA du frontend (facultatif — embarque la console dans le
serveur) :**

```sh
cd ui
pnpm install --frozen-lockfile
pnpm run build
cd ..
```

**3. Générez la spécification OpenAPI et le client TypeScript (nécessaire si vous
compilez la console) :**

```sh
cargo run -p batlehub-server -- --config config.example.toml dump-spec > ui/openapi.json
cd ui && pnpm run generate && pnpm run build && cd ..
```

**4. Créez un fichier de configuration et lancez :**

```sh
cp config.example.toml config.toml
./target/release/batlehub --config config.toml
```

### Raccourcis Task

Si [Task](https://taskfile.dev) est installé :

```sh
task compose:db    # démarrer uniquement PostgreSQL
task run           # cargo run avec la configuration d'exemple
task ui:dev        # serveur de développement Vite, proxy de /api et /proxy vers :8080
task dev           # backend et frontend ensemble
task test          # cargo test --workspace
```

---

## Chart Helm {#helm-chart}

Déployez BatleHub sur Kubernetes avec le chart Helm fourni.

**Prérequis :** Helm 3+, un cluster Kubernetes en fonctionnement, PostgreSQL
joignable depuis le cluster.

### Installation rapide

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

### Recommandé : un fichier de valeurs

Créez un `my-values.yaml` pour une installation reproductible :

```yaml
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

ingress:
  enabled: true
  className: nginx
  host: batlehub.example.com
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com

persistence:
  enabled: true
  size: 50Gi
```

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  -f my-values.yaml
```

### Mise à jour

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

### Stockage S3

```yaml
config:
  storage:
    type: "s3"
    bucket: "batlehub-artifacts"
    region: "us-east-1"
    # endpoint_url et force_path_style servent à MinIO, RustFS et consorts ;
    # omettez les deux pour AWS S3.

persistence:
  enabled: false   # pas besoin de PVC avec S3
```

Le bloc de stockage ne porte aucune clé d'accès, parce qu'il n'existe pas de tel
champ : les identifiants S3 viennent de la chaîne standard du SDK AWS — le rôle
IAM du pod, ou `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` fournis par `env` ou
`envFrom`.

```yaml
envFrom:
  - secretRef:
      name: batlehub-s3-credentials   # AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
```

### Worker d'analyse (RFC 0018) {#helm-worker}

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

### Les valeurs principales

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

### Injecter des secrets par variables d'environnement {#helm-env-vars}

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

### Un fichier de configuration séparé pour les identifiants {#helm-credentials}

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

### Utiliser un Secret externe (GitOps / Sealed Secrets)

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

## Première mise en route

Quelle que soit la méthode d'installation, une fois le serveur démarré :

**1. Vérifiez l'endpoint de santé :**

```sh
curl -H "Authorization: Bearer my-admin-token" \
  http://localhost:8080/api/v1/admin/health
```

**2. Ouvrez la console et le guide de mise en place :**

Rendez-vous sur `http://localhost:8080` — la page de mise en place (`/setup`)
génère les extraits de configuration client pour tous les outils enregistrés.

**3. Faites pointer un client vers le proxy :**

```sh
# npm
npm install --registry http://localhost:8080/proxy/npm/ some-package

# Go
GOPROXY=http://localhost:8080/proxy/go,direct go get golang.org/x/text@latest

# Cargo — à ajouter dans .cargo/config.toml
# [source.crates-io]
# replace-with = "batlehub"
# [source.batlehub]
# registry = "sparse+http://localhost:8080/proxy/cargo/registry/"
```
