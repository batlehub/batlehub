---
sourcePath: guide/configuration-examples.md
sourceHash: 830b43be5e79efa1
---

# Exemples commentés

## 6.1 Développement local

Configuration minimale pour le développement local : authentification par token
statique, cache sur système de fichiers, npm et Cargo ouverts aux lectures
anonymes.

```toml
[server]
port = 8080

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value = "dev-admin-token"
role = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "./tmp/cache"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user = ["releases:read", "source:read"]
admin = ["*"]

[[registries]]
type = "cargo"
name = "cargo"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user = ["releases:read", "source:read"]
admin = ["*"]
```

## 6.2 Production avec OIDC (Authentik)

SSO OIDC par Authentik, registre GitHub réservé aux utilisateurs authentifiés, et
garde-fou d'âge de publication pour empêcher le téléchargement d'un paquet dans
sa première heure d'existence.

```toml
[server]
host = "0.0.0.0"
port = 8080
static_dir = "/app/ui/dist"

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@db:5432/batlehub"

[[auth]]
type = "oidc"
issuer_url = "https://sso.example.com/application/o/batlehub/"
client_id = "batlehub"
client_secret = "my-client-secret"
redirect_uri = "https://batlehub.example.com/api/v1/auth/oidc/callback"
scopes = ["openid", "profile", "email", "groups"]
user_id_claim = "preferred_username"
role_claim = "groups"

[auth.role_mappings]
"authentik Admins" = "admin"
"proxy-users"      = "user"

# Token statique pour les pipelines de CI qui ne savent pas faire d'OIDC
[[auth]]
type = "token"

[[auth.tokens]]
value = "ci-pipeline-token"
role = "user"
user_id = "ci"

[storage]
type = "filesystem"
path = "/data/cache"

[[registries]]
type = "github"
name = "github"

[registries.rbac]
anonymous = []
user = ["releases:read", "source:read"]
admin = ["*"]

[registries.rbac.groups]
"oidc:developers" = ["releases:read", "source:read"]
"*:ops"           = ["*"]

[[registries.rules]]
kind = "release_age_gate"
min_age_secs = 3600
bypass_roles = ["admin"]
```

## 6.3 Déploiement Kubernetes

Authentification par compte de service Kubernetes avec les valeurs intra-cluster
par défaut, et stockage S3 dont les identifiants viennent de variables
d'environnement.

```toml
[server]
port = 8080
static_dir = "/app/ui/dist"

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"

[[auth]]
type = "kubernetes"
# api_server, ca_cert_path et token_path valent par défaut les valeurs intra-cluster

[auth.role_mappings]
"system:serviceaccount:prod:ci-deployer"  = "admin"
"system:serviceaccounts:staging"          = "user"
"system:serviceaccounts:dev"              = "user"

[storage]
type = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"
# Les identifiants AWS viennent du rôle IAM du pod, ou d'AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = []
user = ["releases:read", "source:read"]
admin = ["*"]

[[registries]]
type = "github"
name = "github"

[registries.rbac]
anonymous = []
user = ["releases:read"]
admin = ["*"]
```

Le ServiceAccount de batlehub a besoin du droit d'appeler l'API TokenReview de
Kubernetes :

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: batlehub-tokenreview
rules:
  - apiGroups: ["authentication.k8s.io"]
    resources: ["tokenreviews"]
    verbs: ["create"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: batlehub-tokenreview
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: batlehub-tokenreview
subjects:
  - kind: ServiceAccount
    name: batlehub
    namespace: batlehub
```

## 6.4 Proxy de modules Go

Faire proxy des modules Go par `proxy.golang.org`, avec un garde-fou d'âge de
publication et une dérogation réservée aux admins. Les cinq endpoints GOPROXY
(`.info`, `.mod`, `.zip`, `@latest`, `@v/list`) sont servis de façon
transparente.

```toml
[server]
port = 8080

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value = "admin-token"
role  = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "./cache"

[[registries]]
type     = "goproxy"
name     = "go"
# L'amont par défaut est https://proxy.golang.org.
# Pour un environnement coupé du réseau, pointez vers un miroir interne :
# upstreams = ["https://goproxy.internal.example.com"]

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
admin     = ["*"]

# Refuser les modules publiés dans la dernière heure (délai d'attente de chaîne d'approvisionnement).
[[registries.rules]]
kind         = "release_age_gate"
min_age_secs = 3600
bypass_roles = ["admin"]
```

Configurez la chaîne d'outils go :

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="http://localhost:8080/proxy/go,direct"

# Récupérer une version précise — servie depuis le cache après le premier téléchargement
go get golang.org/x/text@v0.3.7
```

## 6.5 Registres privés auto-hébergés {#_6-5-self-hosted-private-registries}

Faire proxy d'un registre npm Gitea privé, avec un token Bearer et un certificat
d'autorité auto-signé. Le même motif fonctionne pour Cargo, Go et OpenVSX.

```toml
[server]
port = 8080

[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value   = "admin-token"
role    = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "./cache"

# Registre npm public (aucune authentification nécessaire)
[[registries]]
type = "npm"
name = "npm-public"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user      = ["releases:read", "source:read"]
admin     = ["*"]

# Registre npm Gitea privé
[[registries]]
type      = "npm"
name      = "npm-internal"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/npm"]

[registries.upstream_auth]
type  = "bearer"
token = "npat-xxxx"

[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
admin     = ["*"]

# Registre Cargo privé sur Nexus, en authentification basic
[[registries]]
type      = "cargo"
name      = "cargo-internal"
upstreams = ["https://nexus.corp.example.com/repository/cargo-proxy/"]
index_url = "https://nexus.corp.example.com/repository/cargo-index/"

[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "s3cr3t"

[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
admin     = ["*"]
```

## 6.6 Registre Cargo privé (mode local / hybrid) {#66-private-cargo-registry-local--hybrid-mode}

> Pour un parcours de publication pas à pas, voir
> [la page Cargo](/fr/registries/cargo#publishing-local-hybrid).

### Registre purement local (sans amont)

Employez ceci quand vous voulez un registre Cargo entièrement privé, qui ne fait
pas proxy de crates.io.

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"          # BatleHub est la seule source ; aucun amont nécessaire

[registries.rbac]
anonymous = []
user      = ["source:read"]  # autorise le téléchargement mais pas la publication (le service vérifie le rôle)
admin     = ["*"]
```

Configurez Cargo côté client (`~/.cargo/config.toml`, ou le
`.cargo/config.toml` à la racine du projet) :

```toml
[registries.internal]
index = "sparse+https://batlehub.example.com/proxy/internal/registry/"

[registry]
token = "<your-user-token>"   # ou définissez la variable CARGO_REGISTRIES_INTERNAL_TOKEN
```

Publiez une crate :

```sh
cargo publish --registry internal
```

Dépendez d'une crate publiée en privé :

```toml
# Cargo.toml
[dependencies]
my-lib = { version = "0.1", registry = "internal" }
```

### Registre hybrid (crates locales et repli sur crates.io)

Employez ceci quand vous voulez publier des crates internes tout en faisant proxy
du registre public crates.io par le même endpoint.

```toml
[[registries]]
type      = "cargo"
name      = "everything"
mode      = "hybrid"
upstreams = ["https://static.crates.io/crates"]
index_url = "https://index.crates.io"

[registries.rbac]
anonymous = ["source:read"]   # les crates publiques sont lisibles sans authentification
user      = ["source:read"]
admin     = ["*"]
```

Configuration côté client :

```toml
[registries.everything]
index = "sparse+https://batlehub.example.com/proxy/everything/registry/"
token = "<your-user-token>"
```

En mode hybrid, `cargo fetch` et `cargo build` fonctionnent de façon
transparente :
- une dépendance publiée sur BatleHub est servie depuis le stockage local ;
- toute autre dépendance se rabat sur crates.io par l'amont configuré.

### Endpoints exposés par un registre local ou hybrid

| Méthode | Chemin | Employé par |
|--------|------|---------|
| `GET` | `/proxy/{registry}/registry/config.json` | le client `cargo` à la première connexion |
| `GET` | `/proxy/{registry}/registry/{path}` | la consultation de l'index sparse |
| `GET` | `/proxy/{registry}/{name}/{version}/download` | le téléchargement du `.crate` |
| `PUT` | `/proxy/{registry}/api/v1/crates/new` | `cargo publish` |
| `DELETE` | `/proxy/{registry}/api/v1/crates/{name}/{version}/yank` | `cargo yank` |
| `PUT` | `/proxy/{registry}/api/v1/crates/{name}/{version}/unyank` | `cargo yank --undo` |
| `GET` | `/proxy/{registry}/api/v1/crates/{name}/owners` | `cargo owner --list` |

---

## 6.7 Registre npm privé (mode local / hybrid) {#67-private-npm-registry-local--hybrid-mode}

> Pour un parcours de publication pas à pas, voir
> [la page npm](/fr/registries/npm#publishing-local-hybrid).

### Registre npm purement local (sans amont)

Employez ceci quand vous voulez un registre npm entièrement privé pour vos
paquets internes.

```toml
[[registries]]
type = "npm"
name = "internal-npm"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

Configurez npm côté client :

```sh
# ~/.npmrc, ou le .npmrc du projet
@myorg:registry=https://batlehub.example.com/proxy/internal-npm/
//batlehub.example.com/proxy/internal-npm/:_authToken=<your-user-token>
```

Publiez et installez :

```sh
# publier
npm publish --registry https://batlehub.example.com/proxy/internal-npm/

# installer un paquet à scope
npm install @myorg/my-package
```

### Registre npm hybrid (paquets locaux et repli amont)

```toml
[[registries]]
type      = "npm"
name      = "everything-npm"
mode      = "hybrid"
upstreams = ["https://registry.npmjs.org"]

[registries.rbac]
anonymous = ["releases:read"]
user      = ["releases:read", "source:read"]
admin     = ["*"]
```

En mode hybrid, `npm install` sert de façon transparente les paquets internes
depuis le stockage local et les paquets publics depuis le registre amont.

### Endpoints exposés par un registre npm local ou hybrid

| Méthode | Chemin | Employé par |
|--------|------|---------|
| `GET` | `/proxy/{registry}/{package}` | le packument (toutes les versions) |
| `GET` | `/proxy/{registry}/{package}/{version}` | les métadonnées d'une version |
| `GET` | `/proxy/{registry}/{package}/{version}/tarball` | le téléchargement du tarball |
| `PUT` | `/proxy/{registry}/{package}` | `npm publish` |
| `POST` | `/proxy/{registry}/-/npm/v1/audit/quick` | `npm audit` (relayé vers l'amont) |

---

## 6.8 Registre privé d'extensions VS Code (mode local / hybrid) {#68-private-vs-code-extension-registry-local--hybrid-mode}

> Pour un parcours de publication pas à pas, voir
> [la page OpenVSX](/fr/registries/openvsx#publishing-local-hybrid).

Employez ceci quand vous voulez distribuer des extensions VS Code privées par un
registre auto-hébergé.

### Registre d'extensions purement local

```toml
[[registries]]
type = "openvsx"     # ou "vscode-marketplace"
name = "internal-ext"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

Pointez l'éditeur vers la galerie du registre, dans `product.json` :

```jsonc
{
  "extensionsGallery": {
    "serviceUrl": "https://batlehub.example.com/proxy/internal-ext/vscode/gallery",
    "itemUrl": "https://batlehub.example.com/proxy/internal-ext/vscode/item",
    "resourceUrlTemplate": "https://batlehub.example.com/proxy/internal-ext/vscode/unpkg/{publisher}/{name}/{version}/{path}"
  }
}
```

L'éditeur n'envoie aucun identifiant à sa galerie : c'est l'autorisation
`anonymous` ci-dessus qui fait fonctionner l'ensemble — voir
[OpenVSX](/fr/registries/openvsx#use-batlehub-as-your-extension-gallery).

Envoyez une extension (les octets bruts du VSIX) :

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-user-token>" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-org.my-ext-1.0.0.vsix \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-ext/1.0.0/vsix"
```

Téléchargez une extension :

```sh
curl -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-ext/1.0.0/vsix" \
  -o my-org.my-ext-1.0.0.vsix
```

### Endpoints exposés par un registre d'extensions VS Code local ou hybrid

| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{extension_id}/{version}/vsix` | Télécharger un VSIX |
| `PUT` | `/proxy/{registry}/{extension_id}/{version}/vsix` | Envoyer un VSIX |

Les identifiants d'extension suivent la convention `{publisher}.{name}` (par
exemple `my-org.my-ext`).

---

## 6.9 Proxy de modules Go privé (mode local / hybrid) {#69-private-go-module-proxy-local--hybrid-mode}

> Pour un parcours de publication pas à pas, voir
> [la page des modules Go](/fr/registries/goproxy#publishing-local-hybrid).

### Proxy de modules Go purement local (sans amont)

Employez ceci pour héberger des modules Go privés sans les exposer à Internet.

```toml
[[registries]]
type = "goproxy"
name = "internal-go"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

**Envoyez un module** en poussant son archive zip. BatleHub en extrait
automatiquement le `go.mod` et génère les métadonnées de version depuis
l'horodatage de l'envoi :

```sh
# Construire le zip du module (format standard de zip de module Go)
go mod zip example.com/mymod@v1.0.0 . --mod-zip /tmp/mymod-v1.0.0.zip

# Envoyer à BatleHub
curl -X PUT -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/zip" \
  --data-binary @/tmp/mymod-v1.0.0.zip \
  "https://batlehub.example.com/proxy/internal-go/example.com/mymod/@v/v1.0.0.zip"
```

**Employez le proxy privé** dans la chaîne d'outils go :

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="https://batlehub.example.com/proxy/internal-go,direct"
go get example.com/mymod@v1.0.0
```

Ou ajoutez-le à `go.env` :

```sh
go env -w GONOSUMCHECK="*"
go env -w GONOSUMDB="*"
go env -w GOPROXY="https://batlehub.example.com/proxy/internal-go,direct"
```

### Proxy de modules Go hybrid (modules locaux et repli amont)

```toml
[[registries]]
type      = "goproxy"
name      = "everything-go"
mode      = "hybrid"
upstreams = ["https://proxy.golang.org"]

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user      = ["releases:read", "source:read"]
admin     = ["*"]
```

En mode hybrid, `go get` et `go mod download` servent de façon transparente les
modules internes depuis le stockage local et les modules publics depuis
`proxy.golang.org` (ou l'amont que vous configurez).

### Endpoints exposés par un proxy de modules Go local ou hybrid

| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{module}/@latest` | JSON d'information de la dernière version |
| `GET` | `/proxy/{registry}/{module}/@v/list` | Liste de versions séparées par des sauts de ligne |
| `GET` | `/proxy/{registry}/{module}/@v/{version}.info` | JSON de métadonnées d'une version |
| `GET` | `/proxy/{registry}/{module}/@v/{version}.mod` | Le contenu du `go.mod` |
| `GET` | `/proxy/{registry}/{module}/@v/{version}.zip` | L'archive zip des sources du module |
| `PUT` | `/proxy/{registry}/{module}/@v/{version}.zip` | Envoyer le zip d'un module (déclenche une publication) |

Un chemin de module peut contenir des barres obliques (par exemple
`golang.org/x/text`).

---

## 6.10 Stockage multi-backend {#610-multi-backend-storage}

Backend système de fichiers par défaut pour tous les registres, et backend S3
dédié aux gros artefacts de releases GitHub.

```toml
[server]
port = 8080

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value = "admin-token"
role = "admin"

[storage]
default = "local"

[[storage.backends]]
name = "local"
type = "filesystem"
path = "./cache"

[[storage.backends]]
name = "s3-releases"
type = "s3"
bucket = "github-releases"
region = "us-east-1"

[[registries]]
type = "github"
name = "github"
storage = "s3-releases"       # les gros assets de release vont sur S3

[registries.rbac]
anonymous = []
user = ["releases:read", "source:read"]
admin = ["*"]

[[registries]]
type = "npm"
name = "npm"
# storage non défini — emploie le backend par défaut "local"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user = ["releases:read", "source:read"]
admin = ["*"]
```

---

## 6.11 Cache de providers Terraform {#611-terraform-provider-cache}

Mettre en cache les binaires de providers Terraform en local, pour que
`terraform init` n'atteigne pas `registry.terraform.io` à chaque exécution de CI.

```toml
[[registries]]
type = "terraform"
name = "terraform"
# upstreams vaut ["https://registry.terraform.io"] par défaut

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
admin     = ["*"]

[registries.cache]
metadata_ttl_secs = 300   # revérifier les listes de versions toutes les 5 min
# artifact_ttl_secs non défini — les binaires de providers sont gardés indéfiniment
```

Configurez la CLI Terraform de chaque développeur ou runner de CI :

```hcl
# ~/.terraformrc  (ou %APPDATA%/terraform.rc sous Windows)
# En CI : écrivez ce fichier pendant la préparation du pipeline
provider_installation {
  network_mirror {
    url = "https://batlehub.example.com/proxy/terraform/"
  }
}
```

Après le premier `terraform init`, les exécutions suivantes emploient les
binaires du cache local. Les sommes de contrôle des providers sont mises en cache
à côté des métadonnées de téléchargement, de sorte que la vérification de
Terraform passe toujours.

---

## 6.12 Registre Maven privé (mode local / hybrid) {#612-private-maven-registry-local--hybrid-mode}

Héberger des artefacts Maven ou Gradle privés (`mvn deploy`,
`gradle publish`), pour que les équipes n'aient jamais besoin d'une instance
Nexus ou Artifactory externe.

```toml
[[registries]]
type = "maven"
name = "internal-maven"
mode = "local"          # BatleHub est la seule source ; aucun amont nécessaire

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

En mode hybrid (servir d'abord les artefacts privés, se rabattre sur Maven Central
pour tout le reste) :

```toml
[[registries]]
type      = "maven"
name      = "internal-maven"
mode      = "hybrid"
upstreams = ["https://repo1.maven.org/maven2"]

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

### Mise en place côté client — Maven

Ajoutez les identifiants à `~/.m2/settings.xml` (l'`<id>` doit correspondre à
celui du `<distributionManagement>` de votre POM) :

```xml
<settings>
  <servers>
    <server>
      <id>internal-maven</id>
      <username>your-user-id</username>
      <password>your-bearer-token</password>
    </server>
  </servers>
  <mirrors>
    <mirror>
      <id>internal-maven</id>
      <name>BatleHub Maven</name>
      <url>https://batlehub.example.com/proxy/internal-maven/maven2/</url>
      <mirrorOf>*</mirrorOf>
    </mirror>
  </mirrors>
</settings>
```

### Mise en place de la publication — pom.xml

```xml
<distributionManagement>
  <repository>
    <id>internal-maven</id>
    <url>https://batlehub.example.com/proxy/internal-maven/maven2/</url>
  </repository>
</distributionManagement>
```

```sh
mvn deploy
```

### Mise en place de la publication — Gradle (settings.gradle.kts)

```kotlin
dependencyResolutionManagement {
    repositories {
        maven {
            url = uri("https://batlehub.example.com/proxy/internal-maven/maven2/")
            credentials {
                username = "your-user-id"
                password = "your-bearer-token"
            }
        }
    }
}
```

### Comment ça marche

Maven et Gradle envoient le `.jar` et les fichiers de sommes de contrôle
**avant** le `.pom`. BatleHub range chaque fichier autre que le POM directement
dans le stockage objet. À l'arrivée du `.pom`, BatleHub l'analyse (en extrayant
`groupId`, `artifactId`, `version`, `packaging`, `description`) et valide une
ligne `local_packages` par le protocole de publication en trois phases. Les
requêtes `GET` suivantes sur `maven-metadata.xml` renvoient un XML généré depuis
la base, et non un fichier en cache.

### Endpoints exposés par un registre Maven local ou hybrid

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/maven2/{path}` | GET | Sert l'artefact depuis le stockage local (ou par proxy en mode hybrid) |
| `/proxy/{registry}/maven2/{group}/{artifact}/maven-metadata.xml` | GET | Généré depuis la base ; jamais mis en cache |
| `/proxy/{registry}/maven2/{path}` | PUT | Envoyer un artefact (le `.pom` valide la version, les autres fichiers sont stockés directement) |

---

## 6.13 Registre Terraform privé (mode local / hybrid) {#613-private-terraform-registry-local--hybrid-mode}

Publier et servir des modules et des providers Terraform privés, sans registre
externe.

```toml
[[registries]]
type = "terraform"
name = "internal-tf"
mode = "local"

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

En mode hybrid (servir d'abord les providers et modules privés, faire proxy de
`registry.terraform.io` pour tout le reste) :

```toml
[[registries]]
type      = "terraform"
name      = "internal-tf"
mode      = "hybrid"
upstreams = ["https://registry.terraform.io"]

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

### Mise en place côté client — .terraformrc

```hcl
# ~/.terraformrc  (ou %APPDATA%/terraform.rc sous Windows)
provider_installation {
  network_mirror {
    url = "https://batlehub.example.com/proxy/internal-tf/"
  }
}

credentials "batlehub.example.com" {
  token = "your-bearer-token"
}
```

### Publier un module privé

```sh
# Empaquetez votre module en tar.gz, puis envoyez-le :
tar czf my-module.tar.gz -C ./module-dir .
curl -X POST \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/gzip" \
  --data-binary @my-module.tar.gz \
  "https://batlehub.example.com/proxy/internal-tf/v1/modules/namespace/name/provider/1.0.0"
```

La réponse porte un en-tête `X-Terraform-Get` qui pointe vers l'URL de
téléchargement de l'artefact stocké.

### Publier un provider privé

Étape 1 — envoyez le manifeste de version (un JSON décrivant les protocoles et
les plateformes disponibles) :

```sh
curl -X POST \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "version": "5.0.0",
    "protocols": ["5.0"],
    "platforms": [
      {"os": "linux",  "arch": "amd64",  "filename": "terraform-provider-mycloud_5.0.0_linux_amd64.zip",  "shasum": "abc123..."},
      {"os": "darwin", "arch": "arm64",  "filename": "terraform-provider-mycloud_5.0.0_darwin_arm64.zip", "shasum": "def456..."}
    ]
  }' \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/myorg/mycloud/versions"
```

Étape 2 — envoyez chaque binaire de plateforme :

```sh
curl -X PUT \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/zip" \
  --data-binary @terraform-provider-mycloud_5.0.0_linux_amd64.zip \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/myorg/mycloud/5.0.0/artifact/linux/amd64"
```

### Retirer une version (admin)

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages":[{"name":"modules/namespace/name/provider","version":"1.0.0"}]}' \
  "https://batlehub.example.com/api/v1/admin/registries/internal-tf/bulk-yank"
```

---

## 6.14 Limitation de débit — par utilisateur et par groupe {#614-rate-limiting}

Protéger un registre npm exposé au public : chaque utilisateur reçoit 200
requêtes par minute ; les membres du groupe des bots de CI partagent un réservoir
plus large de 2000 requêtes par minute ; le groupe de l'offre gratuite est limité
à 50 par minute.

```toml
[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
admin     = ["*"]

[registries.rate_limit]
requests_per_window = 200    # par utilisateur authentifié
window_secs         = 60
enforcement         = "block"

# Les bots de CI partagent un unique réservoir de 2000/min entre tous leurs membres :
[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 2000
window_secs         = 60

# Les utilisateurs de l'offre gratuite partagent un réservoir plus strict de 50/min :
[[registries.rate_limit.groups]]
name                = "oidc:free-tier"
requests_per_window = 50
window_secs         = 60
enforcement         = "warn"   # avertir plutôt que bloquer pour l'offre gratuite
```

Un bot de CI qui appartient à `oidc:ci-bots` consomme un jeton dans son seau
personnel de 200/min *et* dans le seau partagé `oidc:ci-bots` de 2000/min à
chaque requête. Si l'un des deux est épuisé, la requête est bloquée (ou
avertie, selon la dérogation d'application propre au groupe).

La réponse quand un utilisateur dépasse sa limite :

```
HTTP/1.1 429 Too Many Requests
X-RateLimit-Limit: 200
Retry-After: 42
X-RateLimit-Reset: 1716556842
Content-Type: application/json

{"error":"rate limit exceeded","retry_after_secs":42}
```

---

## 6.15 Registre Composer privé (mode local / hybrid) {#615-private-composer-registry-local--hybrid-mode}

Publier et servir des paquets PHP privés, sans registre externe compatible
Packagist.

```toml
[[registries]]
type = "composer"
name = "internal-composer"
mode = "local"

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

En mode hybrid (servir d'abord les paquets privés, faire proxy de Packagist pour
tout le reste) :

```toml
[[registries]]
type      = "composer"
name      = "internal-composer"
mode      = "hybrid"
upstreams = ["https://repo.packagist.org"]

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

### Mise en place côté client — composer.json

Ajoutez une entrée de dépôt au `composer.json` de votre projet :

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/internal-composer/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer your-token"]
        }
      }
    }
  ]
}
```

Vous pouvez aussi garder les identifiants hors de `composer.json` en les stockant
dans `auth.json` :

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "user",
      "password": "your-token"
    }
  }
}
```

### Publier un paquet

Créez une archive ZIP contenant un `composer.json` valide, à sa racine ou dans un
unique répertoire de premier niveau (la disposition d'une archive GitHub est
acceptée elle aussi). Ce `composer.json` doit comporter les champs `name` (au
format `vendor/package`) et `version` :

```sh
# Créer l'archive
zip -r symfony-console-7.1.0.zip symfony-console-7.1.0/

# Publier
curl -X POST \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/zip" \
  --data-binary @symfony-console-7.1.0.zip \
  "https://batlehub.example.com/proxy/internal-composer/api/upload"
```

Le champ `version` du `composer.json` envoyé détermine la version publiée. Il se
remplace en ajoutant `?version=<version>` à l'URL d'envoi.

### Retirer une version

```sh
curl -X DELETE \
  -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-composer/api/packages/my-vendor/my-package/versions/1.0.0"
```

Une version retirée est masquée des métadonnées `p2/` et renvoie 404 au
téléchargement.

---

## 6.16 Proxy HTTP d'entreprise (environnements coupés du réseau) {#616-corporate-http-proxy-air-gapped-environments}

Employez ceci quand BatleHub est déployé dans un périmètre réseau qui exige que
tout le trafic HTTP et HTTPS sortant passe par un proxy d'entreprise (Squid,
Zscaler, Tinyproxy…).

Dans cet exemple, les paquets npm et Cargo sont récupérés à travers un proxy
Squid qui exige une authentification basic. Un registre npm Gitea interne privé
est aussi configuré — son trafic contourne le proxy par `no_proxy`, parce qu'il
est joignable directement.

```toml
[server]
host = "0.0.0.0"
port = 8080

[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@db:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value   = "admin-token"
role    = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "/data/cache"

# ── Registres publics (routés par le proxy d'entreprise) ─────────────────────

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read"]
user      = ["releases:read", "source:read"]
admin     = ["*"]

[registries.proxy]
url      = "http://squid.corp.example.com:3128"
username = "proxyuser"
password = "${PROXY_PASSWORD}"    # export PROXY_PASSWORD=s3cr3t

[[registries]]
type = "cargo"
name = "cargo"

[registries.rbac]
anonymous = ["source:read"]
user      = ["source:read"]
admin     = ["*"]

[registries.proxy]
url      = "http://squid.corp.example.com:3128"
username = "proxyuser"
password = "${PROXY_PASSWORD}"

# ── Registre Gitea interne (direct — contourne le proxy) ─────────────────────

[[registries]]
type      = "npm"
name      = "npm-internal"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/npm"]

[registries.upstream_auth]
type  = "bearer"
token = "${GITEA_TOKEN}"

[registries.proxy]
url      = "http://squid.corp.example.com:3128"
username = "proxyuser"
password = "${PROXY_PASSWORD}"
no_proxy = "gitea.corp.example.com"   # joindre Gitea directement

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
admin     = ["*"]
```

> **Proxy SOCKS5 :** remplacez `http://` par `socks5://` dans le champ `url` si
> votre environnement emploie un proxy SOCKS5 (un tunnel SSH, par exemple :
> `socks5://localhost:1080`).

> **Proxy global :** plutôt que de répéter `[registries.proxy]` sur chaque
> registre, ajoutez une unique section `[proxy]` au premier niveau — elle
> s'applique à tous les registres d'un coup. Un bloc `[registries.proxy]` propre
> à un registre remplace la valeur globale pour ce registre-là. Le proxy global
> se définit aussi sans toucher au fichier de configuration, par
> `PROXY_CACHE__PROXY__URL` (et les variables associées) — voir
> [§3.8](/fr/guide/configuration#_3-8-proxy-optional).



Chaque exemple de cette page est un `config.toml` complet. La référence champ par
champ dont ils se servent est [Configuration](/fr/guide/configuration).
