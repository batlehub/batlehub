---
sourcePath: operations/check-registries.md
sourceHash: e72d94f4ba685fd1
---

# Contrôle de santé des registres

`scripts/check-registries.sh` valide qu'une instance BatleHub en fonctionnement
marche correctement pour chaque type de registre. Le script va au-delà des codes
de statut HTTP en employant de vrais outils de gestion de paquets —
`npm install`, `cargo add`, `go get` — de sorte que vous attrapez des erreurs de
configuration qu'un simple `curl` manquerait.

## 1. Prérequis

Le script lui-même n'exige que `bash` et `curl`. Les contrôles par outil sont
sautés proprement quand l'outil correspondant n'est pas installé :

| Registre | Outil requis | Version minimale |
| --- | --- | --- |
| npm | `npm` | quelconque |
| Cargo | `cargo` | 1.62 (pour `cargo add`) |
| Go | `go` | 1.21 (pour la variable `NETRC`) |
| GitHub | `curl` | quelconque |
| OpenVSX | `curl` | quelconque |
| Place de marché VS Code | `curl` | quelconque |
| Maven | `curl` | quelconque |
| Terraform | `curl` | quelconque |
| RubyGems | `curl` | quelconque |

La validation des champs JSON emploie `jq` quand il est disponible ; à défaut, le
script se rabat sur des contrôles à base de `grep`.

---

## 2. Utilisation

```sh
./scripts/check-registries.sh [options]

  --url <url>        URL de base du proxy en fonctionnement (défaut : http://localhost:8080)
  --token <tok>      Token Bearer pour les endpoints authentifiés (facultatif)
  --npm <name>       Tester le registre npm nommé <name>
  --cargo <name>     Tester le registre cargo nommé <name>
  --go <name>        Tester le registre go nommé <name>
  --github <name>    Tester le registre github nommé <name>
  --openvsx <name>              Tester le registre openvsx nommé <name>
  --vscode-marketplace <name>   Tester le registre vscode-marketplace nommé <name>
  --maven <name>     Tester le registre maven nommé <name>
  --terraform <name> Tester le registre terraform nommé <name>
  --rubygems <name>  Tester le registre rubygems nommé <name>
  --nuget <name>     Tester le registre nuget nommé <name>
```

La valeur `<name>` de chaque option est le champ `name` que vous avez attribué à
ce registre dans votre `config.toml`, pas son `type`. Seuls les registres que
vous nommez sont testés.

**Tester tous les types de registre contre une instance locale :**

```sh
./scripts/check-registries.sh \
  --npm npm \
  --cargo cargo \
  --go go \
  --github github \
  --openvsx openvsx \
  --vscode-marketplace vscode \
  --maven maven \
  --terraform terraform \
  --rubygems gems \
  --nuget nuget
```

**Tester une instance distante, avec des noms de registre personnalisés et une
authentification :**

```sh
./scripts/check-registries.sh \
  --url https://registry.example.com \
  --token mytoken \
  --npm public-npm \
  --cargo internal-crates
```

**Ne tester que npm et cargo :**

```sh
./scripts/check-registries.sh --npm npm --cargo cargo
```

---

## 3. Ce que fait chaque contrôle

Chaque registre reçoit deux contrôles : un **contrôle HTTP** (un `curl` direct
sur l'endpoint du proxy) et un **contrôle par outil** (une vraie invocation du
gestionnaire de paquets, dans un répertoire temporaire isolé). Les deux doivent
passer pour que le registre soit considéré sain.

### 3.1 npm

Le proxy diffuse le tarball brut du paquet (`.tgz`) pour chaque endpoint npm —
c'est un cache de téléchargements binaires, pas un registre npm qui sert des
packuments. Les endpoints npm ne renvoient pas de JSON.

**Contrôle HTTP** — télécharge le tarball du paquet `ms` et vérifie les octets
magiques gzip (`1f 8b`) :

```text
GET /proxy/<name>/ms  →  200, .tgz binaire
```

**Contrôle par outil** — télécharge un tarball versionné (`ms@2.1.3`) et valide
sa structure tar :

```text
GET /proxy/<name>/ms/2.1.3/tarball  →  200, .tgz valide (vérifié par tar tzf)
```

Cela exerce à la fois le chemin de résolution des métadonnées (`/ms`) et celui du
téléchargement de tarball versionné (`/ms/2.1.3/tarball`), qui exige la
permission `source:read`.

### 3.2 Cargo

**Contrôle HTTP** — récupère la configuration de l'index sparse :

```text
GET /proxy/<name>/registry/config.json  →  200, { "dl": "...", ... }
```

**Contrôle par outil** — crée un projet Rust minimal avec un
`.cargo/config.toml` qui pointe vers le proxy, puis résout `serde` à travers lui :

```sh
cargo add serde --registry <name>
```

Le `.cargo/config.toml` qu'écrit le script :

```toml
[registries.<name>]
index = "sparse+http://HOST/proxy/<name>/registry/"

[source.crates-io]
replace-with = "<name>"

[source.<name>]
registry = "sparse+http://HOST/proxy/<name>/registry/"
```

Cela valide à la fois l'endpoint de l'index sparse et le chemin de téléchargement
des crates.

### 3.3 Go

**Contrôle HTTP** — récupère les informations de dernière version pour
`golang.org/x/text` :

```text
GET /proxy/<name>/golang.org/x/text/@latest  →  200, { "Version": "v0.x.y", ... }
```

**Contrôle par outil** — initialise un module Go temporaire et récupère la
version figée à travers le proxy :

```sh
GOPROXY=http://HOST/proxy/<name>,off \
GONOSUMDB=* \
GONOSUMCHECK=* \
go get golang.org/x/text@<version-from-http-check>
```

La version exacte est tirée de la réponse HTTP `@latest` (par exemple
`v0.37.0`). Employer une version figée évite l'endpoint `/@v/list`, qui n'est pas
nécessaire pour une recherche versionnée. Le `,off` fait échouer clairement le
test si le proxy n'atteint pas l'amont, plutôt que de le laisser se rabattre
silencieusement sur Internet.

### 3.4 GitHub

Les deux contrôles emploient l'endpoint de téléchargement d'asset plutôt que
celui des métadonnées JSON d'une release. Le chemin des métadonnées appelle l'API
REST de GitHub (limitée à 60 requêtes anonymes par heure), là où le chemin de
téléchargement d'asset est mis en cache par le proxy et servi sans appel d'API
après la première requête.

**Contrôle HTTP** — vérifie qu'un asset de release connu est accessible à travers
le proxy :

```text
GET /proxy/<name>/cli/cli/releases/download/v2.48.0/gh_2.48.0_linux_amd64.tar.gz  →  200
```

**Contrôle par outil** — télécharge l'asset et vérifie les octets magiques gzip
(`1f 8b`).

### 3.5 OpenVSX

**Contrôle HTTP** — demande le VSIX d'une extension VS Code et accepte toute
réponse non 5xx (un 404 venu d'open-vsx.org en amont est acceptable) :

```text
GET /proxy/<name>/redhat.java/1.26.0/vsix  →  non-5xx
```

**Contrôle par outil** — télécharge le VSIX dans un fichier temporaire et vérifie
les octets magiques ZIP (`PK\x03\x04`), ce qui confirme que le proxy a renvoyé
une archive VSIX valide et non une page d'erreur. Un 404 venu de l'amont fait
sauter ce contrôle plutôt qu'échouer.

### 3.6 Place de marché VS Code

**Contrôle HTTP** — demande le VSIX d'une extension VS Code et accepte toute
réponse non 5xx (un 404 de l'amont est acceptable) :

```text
GET /proxy/<name>/ms-python.python/2024.2.1/vsix  →  non-5xx
```

**Contrôle par outil** — télécharge le VSIX dans un fichier temporaire et vérifie
les octets magiques ZIP (`PK\x03\x04`), ce qui confirme que le proxy a renvoyé
une archive VSIX valide et non une page d'erreur. Un 404 venu de l'amont fait
sauter ce contrôle plutôt qu'échouer.

### 3.7 Maven

Les deux contrôles n'emploient que `curl` — aucune installation de `mvn`
nécessaire. L'artefact choisi (`junit:junit`) est toujours présent sur Maven
Central.

**Contrôle HTTP** — récupère `maven-metadata.xml` pour `junit:junit` et vérifie
que la réponse contient un élément `<metadata>` :

```text
GET /proxy/<name>/maven2/junit/junit/maven-metadata.xml  →  200, XML avec <metadata>
```

**Contrôle par outil** — télécharge le fichier `junit-4.13.2.pom` et vérifie
qu'il contient un élément `<project>` :

```text
GET /proxy/<name>/maven2/junit/junit/4.13.2/junit-4.13.2.pom  →  200, XML avec <project>
```

Cela exerce à la fois le chemin des métadonnées (`maven-metadata.xml`) et celui
du téléchargement d'artefact (le POM versionné). En mode `hybrid`, le proxy
récupère depuis l'amont à la première requête et met le résultat en cache.

### 3.8 Terraform

Les deux contrôles emploient `curl` et `jq` (avec un repli sur `grep`). Le
provider choisi (`hashicorp/random`) est un petit provider stable, toujours
présent sur `registry.terraform.io`.

**Contrôle HTTP** — récupère la liste des versions du provider et vérifie que
`.versions` n'est pas vide :

```text
GET /proxy/<name>/v1/providers/hashicorp/random/versions  →  200, JSON { "versions": [...] }
```

**Contrôle par outil** — récupère le JSON d'informations de téléchargement de
`hashicorp/random 3.6.0` pour `linux/amd64` et vérifie la présence du champ
`download_url` :

```text
GET /proxy/<name>/v1/providers/hashicorp/random/3.6.0/download/linux/amd64
  →  200, JSON { "download_url": "...", ... }
```

Un 404 de l'amont (version retirée des listes) fait sauter le contrôle par outil
plutôt qu'échouer.

### 3.9 RubyGems

Les deux contrôles n'emploient que `curl` — aucune installation de `gem`
nécessaire. La gem choisie (`rake`) est toujours présente sur rubygems.org.

**Contrôle HTTP** — récupère le JSON d'information de la gem `rake` et vérifie
que `.name == "rake"` :

```text
GET /proxy/<name>/api/v1/gems/rake.json  →  200, JSON { "name": "rake", ... }
```

**Contrôle par outil** — télécharge `rake-13.2.1.gem` et vérifie que c'est une
archive tar valide :

```text
GET /proxy/<name>/gems/rake-13.2.1.gem  →  200, tar valide (vérifié par tar tf)
```

Un fichier `.gem` est une archive tar POSIX standard contenant `metadata.gz` et
`data.tar.gz`. La commande `tar tf` sert à inspecter la structure de l'archive
sans l'extraire. Quand `tar` n'est pas disponible, le contrôle se rabat sur la
vérification que le fichier téléchargé fait plus de 10 Kio.

### 3.10 NuGet

**Contrôle HTTP** — récupère l'index de services NuGet v3 et vérifie que la
réponse contient `"version": "3.0.0"` et un tableau `resources` non vide :

```text
GET /proxy/<name>/nuget/v3/index.json  →  200, JSON { "version": "3.0.0", "resources": [...] }
```

Cela exerce l'endpoint de découverte de services que tout client NuGet v3
récupère en premier. S'il réussit, le routage NuGet du proxy est correctement
câblé. L'index de services est généré dans le processus — aucune requête amont
n'est nécessaire.

**Dépannage :**

**`nuget:http — HTTP 404`**
Le proxy a renvoyé 404 pour l'index de services. Vérifiez le `name` du registre
dans votre configuration, et que `type = "nuget"`.

**`nuget:http — missing resources array`**
Le proxy a renvoyé 200 mais le corps n'est pas un index de services valide. Cela
ne devrait pas arriver, sauf si la configuration pointe vers un registre non
NuGet portant le même nom.

---

## 4. Authentification

Passez `--token <tok>` pour envoyer un token `Bearer` sur toutes les requêtes. Le
token est aussi transmis à chaque outil :

| Outil | Mécanisme |
| --- | --- |
| `curl` (tous les contrôles HTTP) | En-tête `Authorization: Bearer <tok>` |
| `npm` | Entrée `.npmrc` : `//HOST/proxy/<name>/:_authToken=<tok>` |
| `cargo` | Variable d'environnement `CARGO_REGISTRIES_<NAME>_TOKEN` |
| `go` | Fichier `.netrc` temporaire ; la variable `NETRC` pointe dessus |

---

## 5. Codes de sortie et usage en CI

| Code | Signification |
| --- | --- |
| `0` | Tous les contrôles ont passé (les contrôles sautés ne comptent pas comme des échecs) |
| `1` | Un contrôle ou plus a échoué |

Le script s'emploie sans risque dans un pipeline de CI. Il respecte `NO_COLOR` et
produit une sortie propre quand stdout n'est pas un terminal.

Exemple d'étape GitHub Actions :

```yaml
- name: Check registries
  run: |
    ./scripts/check-registries.sh \
      --url ${{ vars.PROXY_URL }} \
      --token ${{ secrets.PROXY_TOKEN }} \
      --npm npm \
      --cargo cargo \
      --go go
```

---

## 6. Les échecs courants

**`cargo:http — HTTP 404`**
Le chemin de l'index sparse est faux. Vérifiez le `name` du registre dans votre
configuration, et que le `type` est `"cargo"`.

**`cargo:tool — cargo add failed` avec « no matching package »**
Le proxy a renvoyé une réponse d'index valide mais la crate est introuvable en
amont. Vérifiez que le proxy atteint `index.crates.io`.

**`go:http — HTTP 200` mais `go:tool` échoue avec « disabled by GOPROXY=...off »**
L'endpoint HTTP fonctionne mais `go get` ne peut pas récupérer le zip du module.
Cela signifie en général que l'amont du proxy (`proxy.golang.org`) est
injoignable depuis l'endroit où le proxy tourne.

**`github:http — HTTP 403`**
L'authentification amont GitHub du proxy n'est pas configurée, et la limite de
débit anonyme de l'API GitHub a été atteinte. Ajoutez un token GitHub dans
l'`upstream_auth` du registre, dans `config.toml`.

**`npm:tool — SKIPPED` / `cargo:tool — SKIPPED`**
L'outil n'est pas installé dans l'environnement où tourne le script. Installez-le,
ou considérez le seul résultat du contrôle HTTP comme suffisant pour votre usage.

**`openvsx:tool — SKIP (HTTP 404)`**
La version d'extension demandée (`redhat.java/1.26.0`) n'existe pas sur
open-vsx.org. C'est attendu sur certains déploiements ; le contrôle HTTP (non
5xx) est le signal qui fait foi.

**`vscode-marketplace:tool — SKIP (HTTP 404)`**
La version d'extension demandée (`ms-python.python/2024.2.1`) est introuvable sur
marketplace.visualstudio.com. Essayez une autre extension ou une autre version,
ou vérifiez que le proxy atteint l'amont.

**`maven:http — HTTP 404`**
Le proxy ne trouve pas `junit/junit/maven-metadata.xml` sur `repo1.maven.org`.
Vérifiez le `name` du registre dans votre configuration, et que le `type` est
`"maven"`. En mode `local`, c'est attendu — l'artefact n'a pas été publié
localement.

**`maven:http — response is not valid maven-metadata.xml`**
Le proxy a renvoyé 200 mais le corps n'est pas du XML (peut-être une page
d'erreur de l'amont). Vérifiez que le proxy atteint `repo1.maven.org` et que les
certificats TLS sont reconnus.

**`terraform:http — HTTP 404`**
Le proxy ne trouve pas `hashicorp/random` sur `registry.terraform.io`. Vérifiez
que le `name` du registre est bien `"terraform"` dans votre configuration. En
mode `local`, c'est attendu — aucun provider n'a encore été envoyé.

**`terraform:http — response contains no versions`**
Le proxy a renvoyé 200 mais le tableau `.versions` du JSON est vide. Cela peut
arriver si le registre amont renvoie temporairement des réponses vides ;
relancez après un court délai.

**`terraform:tool — SKIP (HTTP 404)`**
`hashicorp/random 3.6.0` est introuvable sur `registry.terraform.io` (la version
a peut-être été retirée des listes). Dans ce cas, c'est le résultat du contrôle
HTTP qui fait foi.

**`rubygems:http — HTTP 404`**
Le proxy ne trouve pas `rake` sur `rubygems.org`. Vérifiez le `name` du registre
dans votre configuration, et que `type = "rubygems"`. En mode `local`, c'est
attendu — `rake` n'a pas été publiée localement.

**`rubygems:http — .name != "rake"`**
Le proxy a renvoyé 200 mais le corps JSON n'est pas l'objet d'information de gem
attendu. Vérifiez que l'amont (`rubygems.org`) est joignable et renvoie du JSON
valide.

**`rubygems:tool — not a valid tar archive`**
Le proxy a renvoyé 200 mais le fichier `.gem` téléchargé a échoué à `tar tf`.
Cela peut indiquer un téléchargement tronqué ou un artefact corrompu en cache.
Supprimez l'entrée du cache et réessayez.
