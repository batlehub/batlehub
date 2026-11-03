---
reference: true
sourcePath: use/cli.md
sourceHash: 901485ab9a0886a7
---

# batlehub-cli

`batlehub-cli` est le client en ligne de commande officiel de BatleHub. Il offre
à la fois une CLI classique pour les scripts et les pipelines de CI, et une TUI
interactive pour la navigation et l'administration au quotidien.

## 1. Installation

**par mise** (recommandé — gère la version automatiquement) :

```bash
mise use "github:batleforc/batlehub[asset_pattern=batlehub-cli-*]"
```

**par cargo** (compile depuis les sources — exige la chaîne d'outils Rust) :

```bash
cargo install --git https://github.com/batlehub/batlehub batlehub-cli
```

**Binaires précompilés** — à télécharger depuis les
[releases GitHub](https://github.com/batlehub/batlehub/releases/latest) :

```bash
# Linux x86_64
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-linux-amd64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# Linux aarch64
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-linux-arm64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# macOS Apple Silicon (M1/M2/M3)
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-darwin-arm64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# macOS Intel
curl -fSL https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-darwin-amd64.tar.gz | tar xz
sudo mv batlehub-cli /usr/local/bin/batlehub-cli

# Windows (PowerShell)
Invoke-WebRequest https://github.com/batlehub/batlehub/releases/latest/download/batlehub-cli-windows-amd64.zip -OutFile batlehub-cli.zip
Expand-Archive batlehub-cli.zip -DestinationPath .
Move-Item batlehub-cli.exe "$env:LOCALAPPDATA\Microsoft\WindowsApps\batlehub-cli.exe"
```

Ou lancez-le sans installer, depuis le dépôt :

```bash
task cli -- registry list
task cli:tui
task cli:help
```

---

## 2. Configuration

`batlehub-cli` lit `~/.config/batlehub/config.toml`. L'assistant de mise en place
le crée :

```bash
batlehub-cli profile init
```

Le fichier est en TOML et gère des profils nommés :

```toml
[default]
server_url = "http://localhost:8080"
token      = "my-secret-token"
registry   = "my-registry"        # registre par défaut, facultatif

[profiles.prod]
server_url = "https://batlehub.example.com"
token      = "prod-secret-token"
```

### Surcharges par variables d'environnement

Chaque réglage de connexion peut être surchargé par une variable
d'environnement — pratique en CI, sans toucher au fichier de configuration :

| Variable            | Option équivalente |
|---------------------|------------------|
| `BATLEHUB_SERVER`   | `--server`       |
| `BATLEHUB_TOKEN`    | `--token`        |
| `BATLEHUB_REGISTRY` | `--registry`     |
| `BATLEHUB_PROFILE`  | `--profile`      |
| `BATLEHUB_FORMAT`   | `--format`       |

---

## 3. Options globales

Ces options sont disponibles sur toutes les commandes :

| Option | Courte | Description |
|------|-------|-------------|
| `--profile <name>` | `-P` | Utiliser un profil de configuration nommé |
| `--server <url>` | | Remplacer l'URL du serveur |
| `--token <tok>` | | Remplacer le token d'authentification |
| `--registry <name>` | `-r` | Le registre par défaut : pour un nom de paquet nu, et pour le filtre de registre de chaque commande. À défaut, `BATLEHUB_REGISTRY`, puis le `registry` du profil |
| `--format <table\|json\|csv\|raw>` | | Forme de la sortie (ci-dessous) ; `BATLEHUB_FORMAT` la fixe une fois pour un job de CI |
| `--json` | | Équivaut à `--format json` |
| `--yes` | `-y` | Répondre oui à la confirmation que demande une commande destructrice |

### Nommer un paquet {#coordinate}

Une commande qui agit sur un paquet prend une coordonnée,
`registry/name[@version]` : le registre est tout ce qui précède le premier `/`, la
version tout ce qui suit le dernier `@` qui n'est pas le premier caractère du nom.

```text
npm-local/lodash@4.17.21
npm-local/@scope/pkg@1.2.3        # nom @scope/pkg
go/github.com/x/y@v1.2.0          # le chemin de module reste entier
maven/org.slf4j:slf4j-api@2.0.9   # le group:artifact de Maven reste entier
```

Une coordonnée sans `/` est un nom nu, placé dans le registre par défaut (`-r`,
`BATLEHUB_REGISTRY`, le `registry` du profil) ; si aucun n'est défini, la
commande échoue avec le code 2. Un premier segment qui n'est pas un registre que
vous voyez est lu comme une partie du nom quand un registre par défaut est
défini (`-r go github.com/x/y`), et échoue sinon avec le code 5, en listant les
registres que vous voyez. L'ancienne forme `registry:name@version` est toujours
lue, avec un `warning:`.

Toute commande qui agit sur un paquet prend une coordonnée : `package`, `owner`,
`visibility`, `grant`, `sbom show`, `authz check`, `why`, `wait` et
`verdict pullers`. Les verbes de cycle de vie en prennent plusieurs à la fois.

### Sortie et erreurs {#output}

- **`table`** est le format par défaut, dans un terminal comme dans un pipe.
- **`json`** imprime ce que l'API a envoyé, sans renommer les champs : une
  liste comme un tableau (ou l'objet de page de l'API), une action comme sa
  réponse, ou `{"ok": true}` quand elle n'en a pas.
- **`csv`** fonctionne sur toute liste, avec les noms de champs JSON comme
  en-têtes. C'est une erreur d'usage (code 2) sur une commande qui imprime un
  seul objet.
- **`raw`** n'est accepté que par `auth token`, qui imprime une seule valeur.

Les données vont sur stdout. La progression, les avertissements, les indices et
les confirmations vont sur stderr : `batlehub-cli … --format json | jq` ne lit
jamais une invite.

Une erreur est un bloc sur stderr :

```text
error: version 4.17.21 of npm-local/lodash is held until 2026-11-04 14:00 UTC
  code: MIN_AGE_NOT_MET — https://batlehub.dev/use/refused-downloads#min-age-not-met
hint: `batlehub-cli wait npm-local/lodash@4.17.21` returns when it becomes pullable
```

`error:` est le message du serveur lui-même, `code:` le code de motif quand le
serveur en a donné un, relié à [Pourquoi un téléchargement est
refusé](/fr/use/refused-downloads). Avec `--format json`, la même erreur est
aussi écrite sur stdout sous la forme
`{"error": {"message", "code", "docs", "hint", "exit"}}`.

### Codes de sortie {#exit-codes}

| Code | Signification | Source typique |
| --- | --- | --- |
| 0 | succès | — |
| 1 | échec générique | erreur réseau, 5xx inattendu, E/S |
| 2 | usage | une option que clap refuse, une coordonnée sans registre, un artefact écrit dans un terminal, `--format csv` sur un seul objet |
| 3 | non authentifié | HTTP 401 |
| 4 | refusé | HTTP 403 ; `wait` quand attendre n'y changera rien |
| 5 | introuvable | HTTP 404, un registre inconnu |
| 6 | conflit | HTTP 409 ; `setup` en `conflict`/`ambiguous`/`orphaned` |
| 7 | échec partiel | une opération en masse avec des éléments en échec, `mise seed`/`import` avec des échecs |
| 8 | délai dépassé | `wait --timeout` |
| 9 | confirmation requise | une commande destructrice sans terminal et sans `--yes`, un « non » à l'invite |
| *n* | code de l'enfant | `java` transmet le code de sortie de son processus enfant |

### Commandes destructrices {#confirmation}

Une commande qui supprime, purge, efface, retire (`yank`), masque (`unlist`),
bloque, révoque ou récupère imprime sur stderr ce qu'elle va faire et demande
`Proceed? [y/N]`. Toute autre réponse que `y`/`yes` sort avec le code 9. **Sans
terminal**, elle ne demande rien, ne fait rien et sort avec le code 9 : un script
qui oublie `--yes` échoue franchement au lieu d'attendre une réponse ou de
détruire en silence.

```text
$ batlehub-cli package delete internal/serilog@2.0.0 </dev/null
this will delete:
  internal/serilog@2.0.0
the artifact is dropped and the version number is spent permanently — 2.0.0 can never be published again
error: refusing to delete without a terminal; pass --yes
$ echo $?
9
```

Les commandes qui confirment : `package yank`, `delete` et `unlist`,
`cache delete`, `cache evict` et `coherence` (pas avec `--dry-run`),
`audit purge`, `user block`, `gdpr erase`, `retention apply`,
`notification delete`, `auth token delete`, et `download` sur un fichier
existant. `setup apply` et `setup remove` gardent leur propre contrat : un essai
à blanc par défaut, `--yes` écrit.

### Ce que couvre chaque type de registre {#coverage}

Pour chaque type, ce que fait la CLI pour six capacités : **configurer** le
client (`setup`), **publier**, **télécharger** par coordonnée, **consulter**
(`package versions`/`readme`) et le **cycle de vie** d'une version (`package
yank`, …). Une cellule vaut ✅ (implémenté), **print** (la CLI imprime quoi
lancer, parce que le lancer exige root ou revient au client natif) ou **n.a.**
(avec la raison). Le tableau est généré depuis `cli/src/coverage.rs`, et le
build échoue quand une cellule revendiquée n'a pas de test. Les raisons restent
en anglais : ce sont celles que la CLI imprime.

<!-- cli-matrix:start (generated by `task docs:cli-matrix`) -->
| Kind | Configure | Publish | Download | Consult | Lifecycle |
| --- | --- | --- | --- | --- | --- |
| github | ✅ mise | n.a. — proxy only | n.a. — a release asset is addressed by its filename, which the page does not know | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank |
| forgejo | ✅ mise | n.a. — proxy only | n.a. — a release asset is addressed by its filename, which the page does not know | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank |
| gitlab | ✅ mise | n.a. — proxy only | n.a. — a release asset is addressed by its filename, which the page does not know | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank |
| cargo | ✅ `~/.cargo/config.toml` | ✅ `.crate` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| npm | ✅ `~/.npmrc`, `~/.yarnrc.yml` | ✅ `.tgz` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| openvsx | ✅ `setup ide` | ✅ `.vsix` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| goproxy | ✅ `go env` file | ✅ module directory → one `{version}.zip` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| pypi | ✅ `pip.conf`, `uv.toml` | ✅ wheel, sdist | n.a. — a PyPI version is a set of distribution files — an sdist and one wheel per interpreter and platform — each addressed by its own filename, so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| conda | ✅ `~/.condarc` | ✅ `.conda`, `.tar.bz2` | n.a. — a conda artifact is addressed by channel platform and build string as well as version, so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| composer | ✅ composer `config.json` | ✅ `.zip` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| vscode-marketplace | ✅ `setup ide` | ✅ `.vsix` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| maven | ✅ `~/.m2/settings.xml` | ✅ `.jar` + `--pom`, or `target/` | n.a. — a Maven version is a set of files — a jar, a pom, sources, javadoc — so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| terraform | ✅ `~/.terraformrc` | ✅ module `.tar.gz`; provider directory of `_<os>_<arch>.zip` | n.a. — a Terraform provider binary is addressed by OS and architecture as well as version, and a module is fetched by the client from a URL this instance rewrites rather than as one artifact | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| rubygems | ✅ `~/.gemrc` | ✅ `.gem` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| nuget | ✅ `NuGet.Config` | ✅ `.nupkg` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| deb | print — the sources file is root's | ✅ `.deb` + `--distribution`/`--component` | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| rpm | print — the sources file is root's | ✅ `.rpm` | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| pacman | print — the sources file is root's | ✅ `.pkg.tar.zst` | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| apk | print — the sources file is root's | ✅ `.apk` | n.a. — a version needs a branch, a repo and an architecture as well, so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| jetbrains | n.a. — serves IDE downloads by path; `setup ide` configures the plugin marketplace | n.a. — proxy only | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank |
| jetbrains-marketplace | ✅ `setup ide` | ✅ plugin `.zip`/`.jar` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| generic | ✅ mise | n.a. — proxy only | n.a. — path-addressed: there is no version to fetch by | ✅ `package versions`, `package readme` | n.a. — proxy only: nothing published here to yank |
| nodedist | ✅ env + mise | n.a. — a vendor's releases, mirrored | n.a. — a Node release is a set of files — one per platform, plus headers, source and checksums — so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | n.a. — a vendor's releases: nothing published here to yank |
| sdkman | ✅ env | n.a. — a vendor's releases, mirrored | n.a. — an SDKMAN artifact is addressed by platform as well as version — one archive per platform — so "fetch this version" has no single meaning | ✅ `package versions`, `package readme` | n.a. — a vendor's releases: nothing published here to yank |
| rustup | ✅ env | n.a. — a vendor's releases, mirrored | n.a. — a Rust release is a manifest plus one tarball per component per target, so "fetch this version" needs a target and a profile | ✅ `package versions`, `package readme` | n.a. — a vendor's releases: nothing published here to yank |
| galaxy | ✅ `~/.ansible.cfg` | ✅ collection `.tar.gz` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| nix | ✅ user `nix.conf` | print — `nix copy --to` signs and uploads the closure | n.a. — a version is one or more store paths, each a separate build with its own 32-character hash, and the hash is not derivable from the version | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| devfile | print — Che's `CheCluster` is cluster configuration | ✅ `devfile.yaml` or its directory | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
| helm | print — `helm repo add`: helm rewrites its own repository file | ✅ chart `.tgz` | ✅ by coordinate, checksum verified | ✅ `package versions`, `package readme` | ✅ the generic lifecycle endpoint |
<!-- cli-matrix:end -->

---

## 4. Commandes — registry

```
batlehub-cli registry list
batlehub-cli registry show <name>
batlehub-cli registry suggest [--dir <path>] [--depth N] [--client-env] [--mise [--mise-commented]] [--include-existing]
```

### `registry list`

Liste tous les registres visibles par l'identité courante.

```
$ batlehub-cli registry list
+----------+---------+--------+
| Name     | Type    | Mode   |
+----------+---------+--------+
| cargo    | cargo   | proxy  |
| internal | nuget   | hybrid |
| pypi     | pypi    | local  |
+----------+---------+--------+
3 registry/registries
```

### `registry show <name>`

Affiche le type et le mode d'un seul registre.

### `registry suggest`

Détermine les registres dont un projet a réellement besoin, et imprime les blocs
`[[registries]]` à coller dans `config.toml`.

Deux entrées possibles, par précision décroissante :

- **`mise.lock`** — la meilleure source disponible : il enregistre l'URL de
  téléchargement exacte de chaque outil, plateforme par plateforme. Chaque URL
  correspond soit à un registre typé (un asset de release `github.com` →
  `type = "github"`), soit, pour les hôtes qui ne parlent aucun protocole de
  paquets, à un miroir `generic` de cet hôte.
- **`mise.toml`** et les manifestes de projet habituels (`Cargo.toml`, `go.mod`,
  `package.json`, `pyproject.toml`, `pom.xml`, `composer.json`, `*.gemspec`,
  `*.nuspec`, `*.csproj`, `*.tf`, `environment.yml`) — pas d'URL : la
  correspondance se fait donc par préfixe de backend ou nom d'outil, au mieux.
  Quand un fichier de verrouillage est présent, il l'emporte, puisqu'il nomme les
  mêmes outils plus précisément.

```console
$ batlehub-cli registry suggest --client-env
+------------------+---------+--------------------------------------+----------------------------+
| Name             | Type    | Upstream                             | Detected from              |
+------------------+---------+--------------------------------------+----------------------------+
| cargo            | cargo   | (adapter default)                    | manifest, mise.lock: …     |
| github           | github  | (adapter default)                    | mise.lock: gitleaks, …     |
| node-dist        | generic | https://nodejs.org/dist              | mise.lock: node            |
| rust-dist        | generic | https://static.rust-lang.org         | mise.lock: rust            |
| helm-bin         | generic | https://get.helm.sh                  | mise.lock: helm            |
+------------------+---------+--------------------------------------+----------------------------+

Add to config.toml:
…
Point clients at the proxy:

# node-dist (generic)
export NODEJS_ORG_MIRROR="https://batlehub.example.com/proxy/node-dist/generic"
```

| Option | Description |
|------|-------------|
| `--dir <path>`, `-d` | Répertoire à analyser (défaut : le répertoire courant) |
| `--depth N` | Niveaux de sous-répertoires à analyser pour trouver des manifestes (défaut 0 = la racine seule). Sans effet sur `mise.lock`, qui n'est lu qu'à la racine. |
| `--client-env` | Imprimer aussi les variables d'environnement qui font pointer chaque chaîne d'outils vers le proxy |
| `--mise` | Imprimer aussi un bloc `[settings.url_replacements]` de mise, qui route mise lui-même par le proxy |
| `--mise-commented` | Commenter chaque ligne du bloc `--mise`, pour l'ajouter à un `mise.toml` partagé |
| `--include-existing` | Émettre des suggestions même quand le serveur a déjà un registre de ce type |

#### Router mise lui-même (`--mise`)

`[settings.url_replacements]` réécrit les URL que **la couche HTTP de mise**
récupère elle-même, ce qui couvre les backends aqua / ubi / releases GitHub et
tous les miroirs `generic`. Vérifié contre `mise install` :

- aqua résout *et télécharge* les assets par
  `api.github.com/repos/…/releases/assets/{id}` : c'est donc la règle sur
  `api.github.com` qui porte tout — pas celle sur
  `github.com/…/releases/download/…`.
- `core:node` récupère **à la fois** l'archive de la plateforme et l'archive des
  sources `node-v<ver>.tar.gz`, ce pourquoi le `path_allow` suggéré est `v*/**`
  plutôt qu'un motif limité à une plateforme.

Les backends qui délèguent à un autre outil (`cargo:`, `pipx:`, `npm:`, `go:`) ne
sont **pas** couverts — ces processus lisent leur propre configuration, pas celle
de mise. Le bloc généré les nomme explicitement plutôt que de les laisser
silencieusement absents ; pour ceux-là, servez-vous de `--client-env` et de la
configuration propre à chaque écosystème.

> Les clés d'expression régulière doivent arriver dans le fichier avec des
> antislashs **doublés** (`\\.`). TOML traite un `\` isolé comme une séquence
> d'échappement invalide, et mise répond en journalisant une ligne puis en
> continuant, tout le bloc de réglages abandonné — un échec
> silencieux. Le générateur s'en occupe ; une édition à la main doit y penser.

`--mise-commented` préfixe chaque ligne, y compris les commentaires d'en-tête du
générateur, de sorte que retirer exactement un `#` (et l'espace qui le suit) par
ligne donne un fichier valide. Le `mise.toml` de ce dépôt porte un tel bloc, en
exemple travaillé.

Chaque bloc `generic` généré porte les champs `upstreams` et `path_allow` que le
serveur exige pour ce type : la sortie est donc directement utilisable. Notez la
différence de précision des listes d'autorisation :

- Pour les hôtes qui ont un **préréglage curaté** (`nodejs.org`,
  `static.rust-lang.org`, `dl.google.com`, `get.helm.sh`,
  `binaries.sonarsource.com`), la liste est un motif indifférent à la version, qui
  continue de fonctionner d'une montée de version à l'autre.
- Pour **tout autre hôte**, la liste est l'ensemble des chemins exacts trouvés
  dans le verrou — étroite et démontrablement suffisante pour les versions
  figées, mais à régénérer (ou à élargir à la main) quand ces versions changent.
  Le TOML généré le dit en commentaire au-dessus du bloc.

Sur les hôtes de stockage objet (`storage.googleapis.com`, `s3.amazonaws.com`),
le segment du bucket est replié dans `upstreams` et non laissé dans le chemin —
sans quoi le miroir relaierait tous les autres buckets publics du même hôte.

L'analyse est entièrement locale : le serveur n'est contacté que pour annoter
quels types sont déjà configurés, et un serveur injoignable dégrade cette
annotation plutôt que de faire échouer la commande. `--json` émet les
suggestions structurées, plus le TOML rendu sous une clé `toml`.

---

## 5. Commandes — package

```
batlehub-cli package list   [--registry <r>] [--search <q>] [--blocked-only] [--page N] [--per-page N]
batlehub-cli package versions <registry>/<name>
batlehub-cli package readme   <registry>/<name>[@<version>] [--no-upstream]
```

### `package list`

Liste les paquets de tous les registres accessibles (ou d'un seul, avec
`--registry`).

```
$ batlehub-cli package list --registry internal --search serilog
+----------+----------+-------------------+-----------+---------+
| Registry | Name     | Version           | Status    | Accesses|
+----------+----------+-------------------+-----------+---------+
| internal | Serilog  | 3.1.1             | available | 1234    |
| internal | Serilog  | 3.0.0             | blocked:… | 89      |
+----------+----------+-------------------+-----------+---------+
```

`--json` donne le tableau JSON brut — utile dans un script :

```bash
batlehub-cli --json package list --registry internal | jq '.[].name' | sort -u
```

Les éléments JSON portent un champ `status` dont l'étiquette est à l'intérieur
de l'objet :

```json
[
  { "registry": "internal", "name": "serilog", "version": "3.1.1",
    "status": {"status": "available"}, "access_count": 1234 },
  { "registry": "internal", "name": "serilog", "version": "3.0.0",
    "status": {"status": "blocked", "reason": "yanked"}, "access_count": 89 }
]
```

Filtrez dans un script avec `jq` :
```bash
# Lister uniquement les paquets bloqués
batlehub-cli --json package list | jq '[.[] | select(.status.status == "blocked")]'
```

### `package versions <registry>/<name>`

Toutes les versions d'un paquet, y compris celles que seul l'amont connaît, une
page à la fois, avec l'état que le serveur donne à chacune (détenue, amont
seulement, retirée, bloquée…) et sa provenance (`local`, `proxied`, `upstream`).

```
batlehub-cli package versions <registry>/<name> [--limit N] [--page N] [--all] [--no-upstream]
```

`--page` part de 0. Quand d'autres pages existent, `next: --page N` va sur
stderr, de sorte que `--format json | jq '.[].version'` continue de fonctionner.
`--all` suit les pages, jusqu'à 10 000 lignes. `--no-upstream` ne répond que
d'après ce que l'instance détient.

### `package readme <registry>/<name>[@<version>]`

Imprime le README d'une version — la **source**, pas un rendu. Le markdown se lit
bien dans un terminal, et le transformer en ANSI est un autre sujet.

```
$ batlehub-cli package readme internal/mylib@1.4.2
# mylib

Does a thing.
```

Sans version, c'est la plus récente à porter un README qui répond. Quand la version
demandée n'en fournit aucun, la plus récente qui en a un répond à sa place — et
le dit :

```
$ batlehub-cli package readme internal/mylib@2.0.0-rc1 > README.md
note: showing 1.4.2's README; version 2.0.0-rc1 ships none
```

**Toutes les précisions vont sur stderr**, de sorte que rediriger stdout écrit le
document et rien d'autre. Les notes que vous pouvez voir : un repli sur une autre
version, un README qui est celui du *paquet* et non de cette version, un README
lu dans la réponse de l'amont parce que rien de cette version n'est détenu ici,
un README tronqué au `max_bytes` du registre, et un README qui n'est pas du
markdown.

`--no-upstream` répond à partir de ce que cette instance détient, sans interroger
l'amont du registre sur une version dont elle n'a rien — pour un script, ou pour
un hôte sans route vers l'extérieur. Voir
[ce qui sort de cette instance](/fr/operations/egress#the-console-s-discovery-read).

`--json` imprime la réponse entière : un script peut alors lire `is_fallback`,
`stored` et `truncated` plutôt que d'analyser les notes.

```bash
batlehub-cli --json package readme internal/mylib@1.4.2 | jq -r .source_text
```

Quels registres portent un README, et d'où vient celui de chacun, est dans la
[table de couverture des README](/fr/registries/#readmes).

---

## 6. Commandes — cycle de vie d'un paquet {#commands-version}

```
batlehub-cli package yank        <registry>/<name>@<version>...
batlehub-cli package unyank      <registry>/<name>@<version>...
batlehub-cli package delete      <registry>/<name>@<version>...
batlehub-cli package pin         <registry>/<name>@<version>...
batlehub-cli package unpin       <registry>/<name>@<version>...
batlehub-cli package deprecate   <registry>/<name>@<version>... [--message <text>]
batlehub-cli package undeprecate <registry>/<name>@<version>...
batlehub-cli package unlist      <registry>/<name>@<version>...
batlehub-cli package relist      <registry>/<name>@<version>...
```

Chaque verbe prend une ou plusieurs coordonnées, de registres différents au
besoin ; avec `-r`, un `name@version` nu désigne une version du registre par
défaut. Ces commandes exigent `releases:yank` au niveau de l'instance
(`releases:delete` pour `delete`), à une exception près : sur les registres
cargo, composer, rubygems, terraform et nuget, dont les clients natifs laissent
un propriétaire retirer une version, un **propriétaire explicite de chaque
paquet nommé** peut aussi lancer `yank` et `unyank`. Une requête qui nomme un
paquet dont l'appelant n'est pas propriétaire est refusée en bloc, et un paquet
sans propriétaire n'appartient à personne. `yank`, `delete` et `unlist`
sont [destructrices](#confirmation) : elles demandent confirmation dans un
terminal et exigent `--yes` dans un script.

| Commande | Effet |
|---------|--------|
| `yank` | Marque une version indisponible (elle reste en stockage, le téléchargement est bloqué) |
| `unyank` | Annule un `yank` |
| `delete` | Supprime l'artefact **et consomme définitivement le numéro de version** |
| `pin` | Exempte une version de la rétention — elle n'est jamais reprise automatiquement |
| `unpin` | Lève l'épinglage : la politique de rétention du registre s'applique de nouveau |
| `deprecate` | Avertit le consommateur et continue de servir |
| `undeprecate` | Retire la dépréciation |
| `unlist` | Masque la version de la recherche et des listes, et la sert toujours à qui la demande par sa version exacte |
| `relist` | Annule un `unlist` |

> **Casse des noms de paquets** : les noms sont normalisés en minuscules à la
> publication (NuGet met l'identifiant en minuscules, cargo et npm le font par
> convention). Employez la forme en minuscules pour correspondre au nom stocké —
> `serilog`, pas `Serilog`.

Avec plusieurs coordonnées, chacune est tentée : la sortie nomme chaque réussite
et chaque échec (`FAILED <coordinate>: <error>`), et la commande sort avec le
code 7 quand certaines ont échoué. `--format json` imprime
`{"processed", "succeeded": [...], "failed": [{"coordinate", "error"}]}`.

`delete` demande confirmation, sauf si `--yes` est passé :

```
$ batlehub-cli package delete internal/serilog@2.0.0
this will delete:
  internal/serilog@2.0.0
the artifact is dropped and the version number is spent permanently — it can never be published again
Proceed? [y/N] y
Deleted internal/serilog@2.0.0
```

Un numéro de version supprimé n'est jamais réutilisé. Republier `2.0.0` est
refusé par un `409`, quel que soit le demandeur et quel que soit le délai :
« supprimer et renvoyer pour corriger » n'est donc pas un plan — publiez
`2.0.1`, ou faites un `yank` si vous voulez seulement que la version cesse d'être
installée. Le raisonnement, et ce que la suppression laisse à un auditeur, sont
dans [Supprimer une version publiée](/fr/guide/admin-policies#deleting-versions).

---

## 7. Commandes — owner {#commands-owner}

```
batlehub-cli owner list   <registry>/<name>
batlehub-cli owner add    <registry>/<name> <principal> [--type user|group] [--role admin|maintainer]
batlehub-cli owner remove <registry>/<name> <principal> [--type user|group]
```

La propriété décide qui peut publier de nouvelles versions sur un registre local
ou hybrid. Elle porte sur le paquet : une coordonnée avec une version est donc
refusée. Exige un token d'admin.

```
$ batlehub-cli owner list internal/Serilog
+------+------------------+------------+------------+
| Type | Principal        | Role       | Granted By |
+------+------------------+------------+------------+
| user | alice@example.com| admin      | -          |
| group| nuget-maintainers| maintainer | alice      |
+------+------------------+------------+------------+

$ batlehub-cli owner add internal/Serilog bob --type user --role maintainer
Added user 'bob' as maintainer on internal/Serilog
```

---

## 8. Commandes — publish

```
batlehub-cli publish <input> [-r <r>] [--name <n>] [--version <v>]
                             [--pom <pom.xml>] [--distribution <d>] [--component <c>] [--platform <p>]
```

Envoie un artefact vers un registre local ou hybrid. **C'est le registre qui
décide du type** : la CLI demande au serveur ce qu'est `-r` et emprunte la route
d'envoi de ce type ; un `.tgz` de chart Helm va donc vers Helm même si npm
utilise la même extension. Le nom du fichier n'est lu que pour un nom et une
version, et seulement s'il se lit comme un artefact de ce type.

| Type | Entrée | Nom et version tirés de |
|------|--------|-------------------------|
| nuget | `.nupkg` | le serveur, depuis le `.nuspec` embarqué |
| pypi | wheel ou sdist | le nom du fichier |
| rubygems | `.gem` | le serveur |
| npm | `.tgz` | le nom du fichier (`name-version.tgz`, tel que `npm pack` l'écrit) |
| cargo | `.crate` | le nom du fichier (`name-version.crate`) |
| conda | `.conda`, `.tar.bz2` | le serveur, depuis `info/index.json` (`--platform` n'est qu'un repli) |
| composer | `.zip` | le serveur, depuis `composer.json` (`--version` le remplace) |
| openvsx, vscode-marketplace | `.vsix` | le nom du fichier (`publisher.name-version.vsix`) |
| jetbrains-marketplace | `.zip`/`.jar` de plugin | le serveur, depuis `plugin.xml` |
| galaxy | `.tar.gz` de collection | le serveur, depuis `MANIFEST.json` |
| helm | `.tgz` de chart | le serveur, depuis `Chart.yaml` |
| devfile | répertoire de stack ou `devfile.yaml` | le serveur, depuis `metadata` |
| deb | `.deb` | le serveur — exige `--distribution` et `--component` |
| rpm, pacman, apk | le paquet | le serveur |
| goproxy | un répertoire de module (zippé ici), ou un `.zip` de module avec `--name <module>` | la ligne `module` de `go.mod` ; `--version v1.2.3` est obligatoire |
| maven | un `.jar` avec `--pom`, un `.pom` seul, ou un répertoire `target/` | le `groupId`/`artifactId`/`version` du pom |
| terraform | un `.tar.gz` de module, avec `--name <namespace>/<name>/<provider> --version` | les options |
| terraform | un répertoire de provider contenant des `terraform-provider-<type>_<version>_<os>_<arch>.zip`, avec `--name <namespace>/<type>` | le nom des archives |

Quand un nom ou une version ne peut pas être lu et qu'aucune option ne le
donne, `publish` sort avec le code 2 avant d'envoyer quoi que ce soit.

- **goproxy** envoie un seul `PUT` de `{version}.zip` ; le serveur en déduit
  `.mod` et `.info`, la publication est donc atomique. Le zip ne contient ni
  lien symbolique, ni répertoire `vendor/` ou de gestion de versions, ni module
  imbriqué.
- **maven** envoie le jar, le pom et leurs fichiers `.sha1`/`.md5`, le pom en
  dernier. Une exécution interrompue sort avec le code 7 et nomme ce qui n'a pas
  été envoyé ; **relancez-la** : un fichier que le serveur détient déjà avec les
  mêmes octets compte comme envoyé.
- **terraform** envoie, pour un provider, le manifeste de version puis une
  archive par plateforme. Une relance reprend après le manifeste de la même
  façon, et une exécution interrompue sort avec le code 7.
- **nix** est refusé avec la commande à lancer à la place : `nix copy --to`
  signe et envoie une closure, qui n'est pas un fichier. Les types proxy
  seulement (jetbrains, github, forgejo, gitlab, generic) et les miroirs
  d'éditeur (nodedist, sdkman, rustup) n'ont rien à recevoir.

`-t/--type` est déprécié : il est comparé au type du registre et jamais suivi ;
un `-t` en désaccord avec le registre sort donc avec le code 2.

```bash
# NuGet
batlehub-cli publish Serilog.3.1.1.nupkg -r internal

# Un module Go, depuis son répertoire
batlehub-cli publish ./mymod -r go-local --version v0.1.0

# Maven, après `mvn package`
batlehub-cli publish target/ -r maven-local

# Un répertoire de release de provider Terraform
batlehub-cli publish dist/ -r tf-local --name acme/null

# Debian (distribution et composant ne sont pas dans le fichier)
batlehub-cli publish hello_1.0-1_amd64.deb -r internal --distribution stable --component main
```

---

## 9. Commandes — auth

```
batlehub-cli auth whoami
batlehub-cli auth token                      # imprime un identifiant (après l'avoir rafraîchi)
batlehub-cli auth token [--output raw|json] [--min-ttl <seconds>]
batlehub-cli auth token list
batlehub-cli auth token create --name <n> [--days <d>] [--role user|admin]
                               [--groups <g1,g2> | --all-groups]
batlehub-cli auth token delete <uuid>
batlehub-cli auth write-token-file [--path <p>] [--from-file <p>]
batlehub-cli auth status [--path <p>] [--json]
batlehub-cli proxy serve --registry-url <url> [--bind 127.0.0.1:0] [--contract <p>]
                         [--state-dir <d>] [--print-gallery-url]
```

### `auth whoami`

Imprime l'identité résolue depuis le token courant :

```
$ batlehub-cli auth whoami
+----------+-----------------------+
| User ID  | alice@example.com     |
| Role     | admin                 |
| Provider | oidc                  |
| Groups   | nuget-maintainers, …  |
+----------+-----------------------+
```

### `auth token`

Sans sous-commande, imprime un identifiant pour le serveur configuré — la seule
commande dont le métier est d'émettre un secret, et celle qu'un courtier appelle.
Elle rafraîchit d'abord s'il reste moins de `--min-ttl` secondes, par défaut les
mêmes 120 secondes sur lesquelles toutes les autres commandes rafraîchissent
déjà : il n'y a donc pas une seconde notion de fraîcheur à tenir en phase.

```
$ batlehub-cli auth token --output json
{ "registry": "https://hub.example.dev", "token": "…", "kind": "oidc",
  "expires_at": "2026-09-04T21:40:00Z" }
```

Code de sortie non nul quand il n'y a pas d'identifiant, pour qu'un appelant qui
le substitue dans un en-tête n'envoie pas un bearer vide.

### `auth logout`

Supprime l'identifiant stocké.

```
batlehub-cli auth logout [--profile <name>] [--keep-contract] [--path <file>]
```

Elle vide deux emplacements, et seulement ceux-là : les `token`,
`oidc_refresh_token` et `kubernetes_token_path` du profil dans
`~/.config/batlehub/config.toml`, et l'entrée de ce serveur dans le fichier de
contrat d'identifiants. `server_url` et `registry` survivent — ce sont des
réglages, et une déconnexion qui oublierait à quel serveur vous parlez serait une
moins bonne commande.

**Local uniquement.** Il n'y a pas de session côté serveur ni d'endpoint de
révocation de refresh token : un refresh token OIDC jeté reste donc valide chez
le fournisseur d'identité jusqu'à son expiration. Révoquez-le là-bas si cela
compte ; la commande le rappelle à chaque exécution qui a effacé quelque chose.

Trois détails à connaître :

- **C'est par profil.** Chaque profil nommé est un identifiant distinct :
  `--profile ci` déconnecte celui-là et laisse `default` tranquille.
- **Elle ne supprime jamais un fichier vers lequel une entrée pointe.** Une
  entrée de contrat `from = "file"` nomme un chemin que la CLI ne possède pas —
  un token Kubernetes projeté, par exemple — donc l'entrée disparaît et le
  fichier reste.
- **Elle n'a pas besoin du serveur.** Toutes les autres commandes résolvent
  d'abord un token, ce qui peut vouloir dire un rafraîchissement réseau ;
  celle-ci en est exemptée, de sorte que se déconnecter d'un serveur en panne
  fonctionne quand même.

`--keep-contract` vide le profil et laisse le fichier de contrat intact, pour les
cas où l'éditeur doit continuer de fonctionner.

### `auth write-token-file` et `auth status` {#credential-contract}

Le fichier de contrat d'identifiants ([RFC 0011](/rfc/0011-openvsx-login) §4.1)
est la façon dont un processus qui n'est *pas* la CLI trouve un identifiant — un
éditeur modifié, un script, tout ce qu'une session de bureau démarre sans hériter
ni de votre connexion ni de votre environnement.

```
$ batlehub-cli --server https://hub.example.dev auth write-token-file
https://hub.example.dev written to /home/you/.batlehub/state/vsx-token.json
```

`$BATLEHUB_HOME/state/vsx-token.json`, en `0600`, écrit atomiquement. Le fichier
est indexé par origine et seule l'entrée de ce serveur est touchée : un poste
pointé vers trois BatleHub garde donc trois identifiants dans un seul fichier — et
les champs inconnus sont préservés, de sorte que les ajouts d'un programme plus
récent survivent à une CLI plus ancienne.

`--from-file <path>` enregistre **un chemin à lire** plutôt que la valeur : pour
un token Kubernetes projeté, ou tout secret que quelque chose d'autre garde
frais. L'identifiant ne repose alors jamais dans le fichier de contrat.

```
$ batlehub-cli auth status
+-------------------------+------------+---------------------------+-------+---------+-----------+
| Registry                | Kind       | Token source              | State | Expires | Refresh   |
+-------------------------+------------+---------------------------+-------+---------+-----------+
| https://hub.example.dev | oidc       | inline (written by cli)   | ok    | 4m12s   | cli       |
| https://hub.k8s.dev     | kubernetes | file /var/run/…/token     | unset | —       | reresolve |
+-------------------------+------------+---------------------------+-------+---------+-----------+
  https://hub.k8s.dev: reading /var/run/…/token: No such file or directory
```

Chaque état est une résolution effectuée **maintenant**, jamais un avis mis en
cache : un `ok` périmé, datant d'avant la rotation d'un fichier de token, est
précisément la panne qu'on est en train de déboguer. `unset` et une source mal
configurée sont indiscernables depuis un éditeur et appellent des corrections
opposées, ce pourquoi le motif est imprimé. Aucun chemin de sortie ne peut
émettre un identifiant — le type de ligne n'a aucun champ capable d'en contenir
un.

### `auth token create`

Crée un token d'API à longue durée de vie (exige une session OIDC active). Le
token brut est imprimé exactement une fois — enregistrez-le immédiatement :

```
$ batlehub-cli auth token create --name ci-pipeline --days 90
Created token 'ci-pipeline' (role: user, expires: 2026-09-02)
Groups: none — this token sees only public and internal packages

Token (store this — it will not be shown again):
  bh_pat_XXXXXXXXXXXXXXXXXXXX
```

#### Les groupes portés par un token

Un token ne porte **aucun groupe par défaut** : il voit donc les paquets
`public` et `internal`, et rien de ce qui est accordé à une équipe. Nommez les
groupes qu'il doit porter :

```
$ batlehub-cli auth token create --name ci-pipeline --groups platform,release
Created token 'ci-pipeline' (role: user, expires: 2026-10-01)
Groups: platform, release
```

`--all-groups` est un raccourci pour tous les groupes que vous détenez à
l'instant — la commande lit `auth whoami` et envoie cette liste :

```
$ batlehub-cli auth token create --name laptop --all-groups
```

Trois choses à savoir avant de vous en servir :

- **Vous ne pouvez donner à un token que des groupes que vous détenez.** En
  nommer un que vous n'avez pas est refusé par un `403` qui le nomme, et non
  silencieusement ignoré — un token discrètement plus étroit que demandé
  réapparaît plus tard sous la forme d'un pipeline qui ne voit pas un paquet,
  sans que rien ne relie les deux.
- **Écrivez le groupe tel que le serveur le résout.** `auth whoami` imprime les
  identifiants résolus, et ce ne sont pas toujours ceux que l'opérateur a en
  tête : un groupe Kubernetes arrive dans ce modèle préfixé du nom de son
  fournisseur (`k8s:system:serviceaccounts:digital`, et non
  `system:serviceaccounts:digital`), sauf si une entrée `role_mappings` le
  renomme.
- **C'est un instantané, pas un abonnement.** Les groupes sont pris une fois, à
  la création, et ne sont jamais re-résolus — un token n'a pas de session d'où
  les re-résoudre. Quitter une équipe ne rétrécit pas un token qui la porte
  déjà ; ce sont l'expiration du token (90 jours au plus) et sa révocation qui
  bornent cela, ce pourquoi un départ doit s'accompagner d'une révocation de
  tokens. `auth token list` montre ce que chacun porte.

```
$ batlehub-cli auth token list
+--------------------------------------+-------------+------+------------+---------------------+
| ID                                   | Name        | Role | Expires    | Groups              |
+--------------------------------------+-------------+------+------------+---------------------+
| 0c0f…                                | ci-pipeline | user | 2026-10-01 | platform, release   |
| 7a31…                                | laptop      | user | 2026-09-20 | -                   |
+--------------------------------------+-------------+------+------------+---------------------+
```

Utilisez le token obtenu comme `BATLEHUB_TOKEN` en CI :

```yaml
# Exemple GitHub Actions
- run: cargo publish --registry batlehub
  env:
    BATLEHUB_TOKEN: ${{ secrets.BATLEHUB_TOKEN }}
```

---

## 10. Commandes — proxy {#gallery-proxy}

Le proxy de galerie local ([RFC 0011](/rfc/0011-openvsx-login) §4.4), pour un
éditeur dont le cœur ne sait pas envoyer d'identifiant — VS Code d'origine et
toutes ses variantes qui lisent leur galerie depuis `product.json`. C'est un
serveur en boucle locale que votre propre CLI fait tourner devant un registre VSX
de BatleHub ; la galerie de l'éditeur pointe vers lui, et il attache
l'identifiant tiré du fichier de contrat, de sorte que l'éditeur n'en détient
jamais.

```
$ batlehub-cli proxy serve --registry-url https://hub.example.dev/proxy/vsx
gallery proxy for https://hub.example.dev/proxy/vsx on 127.0.0.1:41873
  extensionsGallery.serviceUrl = http://127.0.0.1:41873/9f2c…/vsx/vscode/gallery
  credential: /home/you/.batlehub/state/vsx-token.json
  state:      /home/you/.batlehub/state/gallery-proxy.json
  not signed in: a search shows the sign-in entry until you are
```

Trois choses à savoir :

- **Le secret, c'est le chemin, pas le port.** Tout est servi sous un segment
  aléatoire propre à chaque exécution ; tout ce qui est en dehors donne un `404`.
  Dans un pod d'espace de travail, la boucle locale est partagée par tous les
  conteneurs : un proxy sur un port bien connu remettrait donc votre identifiant
  à n'importe quel processus qui s'y trouve. `--bind` n'accepte que des adresses
  de boucle locale. À l'intérieur du segment, le chemin est encore vérifié plutôt
  que présumé : un `.` ou un `..` donne un `404` (formes encodées comprises),
  parce que l'identifiant est attaché à ce vers quoi le chemin transmis se
  résout, et qu'une requête sortie du préfixe du registre atteindrait le reste de
  l'API en portant votre token. Seuls `GET`, `HEAD` et `POST` sont transmis, ce
  qui couvre tout le protocole de galerie. `--print-gallery-url` imprime la seule
  URL, pour un script de démarrage qui l'écrit dans le `product.json` de
  l'éditeur ; la même URL est dans `gallery-proxy.json`, en mode `0600`.
- **Se connecter est quelque chose que l'éditeur vous montre, pas une erreur
  qu'il cache.** Sans identifiant, une recherche répond une seule entrée, *Sign
  in to BatleHub*, dont le détail donne les étapes de connexion ; une recherche
  par nom ne répond rien, de sorte que les extensions installées de l'éditeur ne
  sont jamais marquées indisponibles. Lancez `auth login`, puis
  `auth write-token-file` : le proxy relit le fichier à chaque requête, et la
  recherche suivante est celle du registre — appuyez sur Actualiser dans la vue,
  car elle répond à une recherche répétée depuis son propre cache.
- **Toutes les URL d'une réponse de galerie sont réécrites vers le proxy**, de
  sorte que le `.vsix` téléchargé après un clic sur Installer passe aussi par
  lui, avec l'identifiant. Une URL sur une autre origine est laissée telle
  quelle.

Une chose que le proxy ne peut pas changer : **un VS Code récent n'installe que
des paquets signés depuis une galerie.** La vue Extensions grise le bouton
Installer sur toute entrée sans asset de signature, avec le message *This
extension is not signed by the Extension Marketplace* — ce pourquoi un registre
signe ce qu'il héberge
([`[registries.vsx_signing]`](/fr/guide/configuration#vsx-signing)) et relaie la
signature de l'amont pour ce dont il fait proxy. Cela rallume le bouton. Le
vérificateur de l'éditeur, lui, n'accepte toujours que la signature de la place
de marché Microsoft : sur une version d'origine, l'installation elle-même exige
donc `extensions.verifySignature: false` ; les variantes livrées avec une galerie
non Microsoft (code-server, VSCodium, che-code) la livrent désactivée pour cette
raison. Sur une version d'origine, désactivez-la vous-même dans les paramètres de
l'éditeur (pour une version serveur, dans
`<server-data-dir>/data/User/settings.json`). L'entrée de connexion n'est
délibérément pas signée : c'est une page à lire, et son bouton reste gris.

`tests/heavy/vsx_login.sh` pilote la CLI du vrai cœur de VS Code à travers le
proxy, avec et sans identifiant ; `tests/heavy/vsx_view.sh` ouvre la vue
Extensions de la même version dans un navigateur et lit ce qu'elle affiche.

## 11. Commandes — vsx {#vsx}

Le côté client de la signature VSIX d'un registre
([RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry)). `keygen`
imprime une graine pour `[registries.vsx_signing]` et l'identifiant de clé
qu'elle dérive, et n'écrit rien :

```
$ batlehub-cli vsx keygen
seed_hex   = "9d61b19d…"
key_id     = "3f1e0a9c7b2d4e61"
public_key = "d75a9801…"   # the trusted_keys form
```

`verify` contrôle un `.vsix` téléchargé contre l'archive que le registre sert
comme asset `VsixSignature` et contre la clé que son asset `PublicKey` nomme — la
signature Ed25519 sur les octets du fichier, et le manifeste contre les entrées
du fichier :

```
$ batlehub-cli vsx verify weebo-bridge-notify-0.5.0.vsix \
    --registry-url https://hub.example.dev/proxy/vsx --id batleforc.weebo-bridge-notify --version 0.5.0
ok: weebo-bridge-notify-0.5.0.vsix is signed by key 3f1e0a9c7b2d4e61… (24503 bytes, manifest matches)
```

`--signature <archive> --public-key <pem|hex|file>` vérifie hors ligne. La
commande ne fait pas tourner le vérificateur propre à l'éditeur, qui n'accepte
que la signature de la place de marché.


## 12. Commandes d'administration

Ces groupes agissent sur le serveur plutôt que sur un paquet. Une commande
marquée `[admin]` dans son `--help` exige une autorisation de niveau instance ;
c'est le serveur qui décide, de sorte qu'une commande déléguée par un opérateur
fonctionne pour quiconque détient l'autorisation. Les groupes lisent le registre
dans `-r` (ou `BATLEHUB_REGISTRY`, ou le profil) partout où le registre est un
filtre plutôt que l'objet.

### Quota

```
batlehub-cli quota list  [-r <registry>]
batlehub-cli quota reset -r <registry> <user>
```

### Blocages d'IP

```
batlehub-cli ip-block list
batlehub-cli ip-block add    <ip> [--reason <text>]
batlehub-cli ip-block remove <ip>
```

### Serveur

```
batlehub-cli server config reload     # déclencher le rechargement à chaud
batlehub-cli server config changes    # l'historique des changements
batlehub-cli server config show       # la configuration active, en TOML
batlehub-cli server config validate <file>
batlehub-cli server config from-file <file>
batlehub-cli server stats             # taux de succès du cache, octets servis, compteurs
batlehub-cli server health            # santé par registre et par backend
```

`stats` et `health` sont en lecture seule. `health` est la commande à lancer
quand un client signale des échecs sur un registre et pas sur les autres.

### Cache

```
batlehub-cli cache warm      -r <registry> [--packages pkg1,pkg2] [--paths p1,p2]
batlehub-cli cache delete    -r <registry>
batlehub-cli cache evict     -r <registry> [--dry-run]
batlehub-cli cache coherence -r <registry> [--dry-run]
```

`delete`, ainsi qu'`evict` et `coherence` sans `--dry-run`, sont
[destructrices](#confirmation).

### Import

Exécute maintenant les `[[release_imports]]` configurés d'un registre, quel que
soit leur intervalle
([RFC 0021](/rfc/0021-forge-releases-into-registries)).

```
batlehub-cli import <registry> [--tag <tag>] [--repo <owner/name>]
```

`--tag` importe ce tag plutôt que ce que la configuration sélectionne, et c'est
le seul moyen d'atteindre délibérément une pre-release : `latest` n'en choisira
pas. `--repo` exécute l'un des imports configurés dans un même registre.

```
$ batlehub-cli import vsx-local
Release import on vsx-local: imported 3, skipped 11, errors 1

+--------+-----------------------------+-------------------------------+
| tag    | asset                       | error                         |
+=======================================================================+
| v2.1.0 | weebo-bridge-2.1.0.vsix     | manifest names no publisher   |
+--------+-----------------------------+-------------------------------+
```

`skipped` compte les assets dont ce registre détient déjà la version. Ce n'est
pas une erreur — c'est ce qui rend une réexécution gratuite. Les échecs sont
listés plutôt que comptés, parce qu'un tag accompagné d'un motif est quelque
chose sur quoi agir.

Chaque import est une **publication**, et il s'exécute sous le principal que la
configuration nomme, pas sous le vôtre. Le demander exige `cache:warm` ; la
publication elle-même exige le `releases:publish` de ce principal : un import
peut donc être refusé en tant que publication, sous un sujet qui n'est pas le
vôtre.

### Bandeau

```
batlehub-cli banner set    "Maintenance at 22:00 UTC" [--level info|warning|error]
batlehub-cli banner delete
```

### Notifications

Les canaux sont déclarés dans `config.toml` sous `[notifications]` et sont en
lecture seule ici ; les abonnements sont des lignes que cette commande gère.

```
batlehub-cli notification channels        # les canaux sortants configurés
batlehub-cli notification list            # les abonnements
batlehub-cli notification delete <id>
```

Aucune des deux listes n'imprime d'URL de webhook ni de secret de signature. Voir
[`[notifications]`](/fr/guide/configuration#_3-8d-notifications-optional) pour le
côté configuration.

### Autorisations

Les niveaux paquet et version de la hiérarchie d'autorisation — les deux qu'un
fichier de configuration ne peut pas énumérer. Les autorisations de niveau
registre et namespace restent dans `config.toml`.

```
batlehub-cli grant list   <registry>/<name>[@<version>]
batlehub-cli grant set    <registry>/<name>[@<version>] --subject <s> --actions <a1,a2>
batlehub-cli grant remove <registry>/<name>[@<version>] --subject <s>
```

```
$ batlehub-cli grant set npm1/@acme/billing \
      --subject group:oidc1:eng --actions releases:read,releases:list
Granted releases:read, releases:list on npm1/@acme/billing to group:oidc1:eng

$ batlehub-cli grant list npm1/@acme/billing
+----------------------------------+------------------------------+---------------------------+-----------+
| Node                             | Subject                      | Actions                   | Source    |
+----------------------------------+------------------------------+---------------------------+-----------+
| package:@acme/billing            | group:oidc1:eng              | releases:read,            | root      |
|                                  |                              | releases:list             |           |
| package:@acme/billing            | user:alice                   | releases:publish,         | ownership |
|                                  |                              | owners:read, owners:write |           |
| version:@acme/billing@2.4.0-rc.1 | group:oidc1:release-managers | releases:read             | root      |
+----------------------------------+------------------------------+---------------------------+-----------+
```

- **Une coordonnée avec une version adresse une version** : `npm1/@acme/billing`
  est un paquet et `npm1/@acme/billing@2.4.0` une version.
- **Ce que `set` imprime est ce qui a été stocké.** `--actions releases:*` nomme
  un verbe et en stocke plusieurs ; la sortie est l'ensemble développé. La
  commande imprime aussi des avertissements pour une autorisation légale mais
  inerte — une qu'un niveau plus large accorde déjà, ou une posée sur une version
  retirée.
- **Les lignes `Source: ownership` ne s'éditent pas ici.** Elles suivent la liste
  des propriétaires du paquet ; changez-les avec [`owner`](#commands-owner). En
  éditer une vaut un `409`.
- Exige `grants:write` (`grants:read` pour `list`), que `role:admin` détient.

### Exposition et signalements

Qui a récupéré une version signalée, et ce que les sources configurées ont
signalé ([RFC 0002](/rfc/0002-vulnerability-flags-and-exposure)).

```
batlehub-cli exposure list [-r <registry>] [--package <p>] [--source <s>]
                           [--min-effect inform|warn|gate|hard_block]
                           [--when any|before-flag|after-flag]
                           [--from <t>] [--to <t>] [--after <cursor>] [--limit 100]

batlehub-cli flag list [-r <registry>] [--package <p>] [--source <s>]
                       [--effect inform|warn|gate|hard_block] [--include-dead]
                       [--page 0] [--per-page 50]
```

`exposure list` donne une ligne par consommateur, le pull le plus récent en
premier, avec le nombre de pulls qui ont précédé le signalement — le cas
rétroactif, où l'alerte est arrivée après le téléchargement. La pagination se
fait par curseur : `--after` prend celui qu'a imprimé la page précédente.
`--min-effect` signifie « ou plus fort ».

`flag list` ne montre que les signalements vivants, sauf si `--include-dead`
ajoute les révoqués et les expirés.

### Vérification d'accès

```
batlehub-cli authz check <registry>/<name>@<version>
                         [--resource releases:read] [--user <id>]
                         [--role anonymous|user|admin] [--groups <g1,g2>]
```

Simule une décision sans émettre de requête sous cette identité. Voir aussi
[`authz explain`](#commands-authz), qui répond à la question plus large de ce
qu'un sujet a le droit de faire, en nommant le niveau qui a accordé chaque verbe.

### Visibilité, namespaces et utilisateurs

```
batlehub-cli visibility show <registry>/<name>
batlehub-cli visibility set  <registry>/<name> public|internal|team

batlehub-cli namespace list   -r <registry>
batlehub-cli namespace claim  -r <registry> <prefix> <group-id>
batlehub-cli namespace remove -r <registry> <prefix>

batlehub-cli user list                      # les utilisateurs bloqués
batlehub-cli user block   <user-id> [--reason <text>]
batlehub-cli user unblock <user-id>
```

Les trois changent qui peut faire quoi, et prennent effet immédiatement.
`namespace remove` rouvre un préfixe à quiconque peut publier dans le registre :
c'est ici la commande dont la portée est plus large qu'il n'y paraît.

### SBOM

```
batlehub-cli sbom show   <registry>/<name>@<version> [--sbom-format cyclonedx]
batlehub-cli sbom export [-r <registry>] [--from <t>] [--to <t>]
                         [--sbom-format cyclonedx] [-o <file>]
```

`show` imprime toujours du JSON. `export` écrit dans le fichier nommé, ou sur
stdout sans `-o`. Voir [`docs/guide/sbom.md`](/fr/guide/sbom) pour les formats et
leur contenu.

### Journal d'audit

```
batlehub-cli audit log    [-r <registry>] [--user <id>] [--from <date>] [--to <date>] [--denied-only]
batlehub-cli audit purge  --before <instant RFC 3339>
batlehub-cli audit verify [--from <t>] [--to <t>] [--head <digest>]
```

`purge` est [destructrice](#confirmation).

#### Export pour la conformité

```
batlehub-cli audit export [--from <t>] [--to <t>] [-r <registry>]
                          [--action delete,retention_reclaim]
                          [--format json|csv] [-o <file>]
```

À distinguer d'`audit log` ci-dessus, qui est la requête paginée interactive.
Celle-ci est l'export qu'un auditeur conserve : une fenêtre entière en un seul
document, écrit vers `-o` ou sur stdout. Pour la question centrée sur une
identité — qu'a récupéré *ce compte* — voir [`audit pulls`](#commands-audit), qui
agrège au lieu de lister chaque événement.

### Rétention

```
batlehub-cli retention show  <registry> [--show-kept]
batlehub-cli retention apply <registry> [--show-kept] [--dry-run]
```

Récupère les versions publiées localement que la politique
`[registries.retention]` du registre ne conserve plus. `show` rend compte et ne
change rien. `apply` n'est que la moitié du verrou : le registre exige aussi
`dry_run = false`, et `apply` est [destructrice](#confirmation). Deux décisions
à deux endroits, parce qu'un artefact récupéré peut n'exister nulle part
ailleurs.

`--show-kept` imprime chaque version conservée et la condition qui l'a sauvée :
c'est ainsi que vous confrontez une politique à ce qu'elle fait réellement avant
de l'armer.

Épingler une seule version contre la rétention se fait avec
[`package pin`](#commands-version).

### Air gap

```
batlehub-cli air-gap missing [-r <registry>] [--kind artifact|document|checksum|ref|unmirrored_host]
batlehub-cli bundle list
```

Ce qu'une instance déconnectée s'est vu demander sans le détenir, et ce qui a
traversé l'air gap. Voir [le runbook air gap](/fr/operations/air-gap).

### Demandes des personnes concernées

```
batlehub-cli gdpr erase  --user <subject> [--ignore-open-decisions]
batlehub-cli gdpr export --user <subject>
```

`erase` est [destructrice](#confirmation). `--ignore-open-decisions` efface même
quand une décision ouverte nomme la personne ; ce n'est pas un moyen d'éviter la
confirmation, qui exige toujours `--yes` dans un script.

### Anciennes formes {#deprecated-spellings}

Chacune des formes ci-dessous fonctionne encore jusqu'à la 1.10, en imprimant un
`warning:` qui nomme son remplacement.

| Avant | Maintenant |
| --- | --- |
| `config init/show/set` | `profile init/show/set` |
| `registry info <name>` | `registry show <name>` |
| `version yank/unyank/delete/pin/unpin <reg> <name> <ver>` | `package yank/… <reg>/<name>@<ver>…` |
| `admin bulk yank/unyank/delete <reg> <name@ver>…` | `package yank/unyank/delete <reg>/<name>@<ver>…` |
| `admin deprecate/undeprecate/unlist/relist <reg> <name> <ver>` | `package deprecate/… <reg>/<name>@<ver>…` |
| `package versions <reg> <name>` | `package versions <reg>/<name>` |
| `owners list/add/remove <reg> <name>` | `owner list/add/remove <reg>/<name>` |
| `admin visibility get/set <reg> <name>` | `visibility show/set <reg>/<name>` |
| `admin grants list/set/rm <reg> <target>` | `grant list/set/remove <reg>/<target>` |
| `admin namespace list/claim/release <reg> …` | `namespace list/claim/remove -r <reg> …` |
| `admin users list-blocked/block/unblock` | `user list/block/unblock` |
| `admin quota list/reset` | `quota list/reset` |
| `admin ip-block …` | `ip-block …` |
| `admin config reload/changes/view/validate/from-file` | `server config reload/changes/show/validate/from-file` |
| `admin stats`, `admin health` | `server stats`, `server health` |
| `admin cache warm/clear/evict/coherence <reg>` | `cache warm/delete/evict/coherence -r <reg>` |
| `admin import <reg>` | `import <reg>` |
| `admin banner set/clear` | `banner set/delete` |
| `admin retention <reg> [--reclaim]` | `retention show <reg>` · `retention apply <reg>` |
| `admin exposure` | `exposure list` |
| `admin bundles` | `bundle list` |
| `admin air-gap-missing` | `air-gap missing` |
| `admin flags list` | `flag list` |
| `admin audit-log [--purge-before]` | `audit log` · `audit purge --before` |
| `admin export-audit-log` | `audit export` |
| `admin audit verify` | `audit verify` |
| `admin gdpr erase --force` | `gdpr erase --ignore-open-decisions` |
| `admin sbom get/export` | `sbom show/export` |
| `admin notifications …` | `notification …` |
| `admin access-check --registry --package --version` | `authz check <reg>/<name>@<ver>` |
| `verdicts …` | `verdict …` |
| `auth token revoke` | `auth token delete` |
| `--csv` | `--format csv` |
| `mise plan/export --out` | `--output` (`-o`) |
| `proxy serve --registry`, `vsx verify --registry` (une URL) | `--registry-url`, conservé en alias pour les extensions d'éditeur |

Deux formes ont changé sans alias, parce que l'option globale `--format` a pris
leur nom : `admin sbom get/export --format` devient `--sbom-format`, et
`admin export-audit-log --format` devient l'option globale `--format`.
`auth token --output raw|json` fonctionne toujours ; `--format raw|json` est la
nouvelle forme.

---

## 13. Commandes — mise {#commands-mise}

Les quatre verbes de la coupure réseau
([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate)). Les trois premiers
s'exécutent contre une instance **connectée** ; le dernier contre l'instance
coupée du réseau.

```
batlehub-cli mise plan   [--lock mise.lock] [--platform <p>[,<p>]|all] [--include-mise] [-o plan.json]
batlehub-cli mise seed   [--plan plan.json] [--verify]
batlehub-cli mise export [--plan plan.json] --sign-key <file> [-o estate.bhub] [--bundle-id <id>]
batlehub-cli mise import <bundle>
```

`plan` transforme un verrou en nomenclature, hors ligne : il lit le verrou et la
liste des registres du serveur, et ne résout rien par le réseau. `--platform`
vaut par défaut celle de cette machine ; `all` prend toutes les plateformes que
le verrou enregistre. `--include-mise` embarque la release de mise elle-même, de
sorte que le parc puisse mettre à jour l'outil qui lira le prochain plan.

`seed` récupère chaque entrée planifiée *à travers* BatleHub — récupérer, c'est
préchauffer — et compare l'empreinte de ce que le serveur a servi à celle du
verrou. Un code de sortie non nul signifie que le lot serait incomplet ou faux :
la commande fonctionne donc comme porte de CI. `--verify` ajoute ce qu'a dit la
couche de chaîne d'approvisionnement et échoue sur un verdict de refus.

`export` construit le lot signé et adressé par le contenu, et imprime la clé
**publique** avec laquelle il a signé, qui est la valeur dont l'instance coupée a
besoin dans `air_gap.bundle_trusted_keys`. La clé de signature est un fichier,
pas une option : une clé sur une ligne de commande est une clé dans l'historique
du shell.

`import` vérifie la signature avant de lire le moindre blob, puis écrit chaque
blob, l'entrée de métadonnées qui le trouve, et le verdict et la résolution de
ref qu'il portait. Importer deux fois le même lot n'écrit qu'une fois, et le dit.

---

## 14. Commandes — profile {#commands-profile}

Le fichier de configuration local : quel serveur, quel identifiant, quel
registre par défaut. La configuration du serveur lui-même est
[`server config`](#serveur).

```
batlehub-cli profile init                # assistant interactif de premier lancement
batlehub-cli profile list                # les profils, `*` sur celui en usage
batlehub-cli profile show [<name>] [--key <key>]   # token masqué
batlehub-cli profile set server_url https://batlehub.example.com
batlehub-cli -P prod profile set registry internal
batlehub-cli profile use prod            # le profil utilisé quand -P est absent
```

Clés valides pour `profile set` : `server_url`, `token`, `registry`. `set` écrit
dans le profil `-P` (ou celui en usage). `use` écrit une clé `current` en tête du
fichier ; une CLI plus ancienne l'ignore et continue de lire `default`.
`profile set token …` stocke un token statique, qu'`auth login` rafraîchirait et
que celui-ci ne rafraîchit jamais.

---

## 15. Commandes — setup

```
batlehub-cli setup detect [--dir <path>] [--depth <n>] [--offline] [--json]
batlehub-cli setup ide [--offline] [--json]
```

`setup detect` analyse un répertoire à la recherche de manifestes de projet
(`Cargo.toml`, `go.mod`, `package.json`, `pyproject.toml`, `pom.xml`,
`composer.json`, `*.gemspec`, `*.nuspec`, `*.csproj`, `*.tf`, `environment.yml`)
et imprime l'extrait de configuration de chaque gestionnaire de paquets trouvé.
`setup ide` fait de même pour l'éditeur depuis lequel vous le lancez (VS Code /
VSCodium → OpenVSX ou la place de marché VS Code ; JetBrains → la place de marché
JetBrains).

Les deux demandent au serveur quels registres existent, de sorte que les extraits
portent le vrai nom de registre et l'URL sur laquelle ce registre répond
réellement — son propre sous-domaine quand le
[routage par hôte](/rfc/0001-subdomain-routing) en annonce un,
`{server}/proxy/{name}` sinon. Chaque exécution se termine par les strophes
`~/.netrc` correspondantes, une par hôte : les identifiants sont appariés par nom
d'hôte, donc un registre routé par hôte a besoin de sa propre entrée.

Si le serveur est injoignable, les commandes fonctionnent quand même — elles
impriment des marqueurs `<registry>` et le disent sur stderr. `--offline` saute
entièrement la requête.

### Configurer la machine : `setup apply`, `status`, `remove`, `env` {#setup-apply}

```
batlehub-cli setup apply  [--yes] [--only <client>,…] [--skip <client>,…]
                          [--pick <type>=<name>]… [--maven-mirror-of central|external|all]
                          [--shell-rc auto|none] [--insecure-http] [--allow-foreign-hosts] [--json]
batlehub-cli setup status [--only …] [--skip …] [--pick …] [--shell-rc auto|none]
                          [--insecure-http] [--allow-foreign-hosts] [--json]
batlehub-cli setup remove [--yes] [--only …] [--skip …] [--json]
batlehub-cli setup env    [--shell sh|fish] [--only …] [--skip …] [--pick …]
                          [--insecure-http] [--allow-foreign-hosts] [--refresh]
```

`detect` imprime des extraits pour un projet ; `apply` les écrit pour une
machine. La commande demande au serveur quels registres existent, repère les
clients présents dans le `PATH` et écrit la configuration **utilisateur** de
chacun ([RFC 0038](/rfc/0038-client-setup-apply)). Seule l'URL du serveur est à
fournir — `--server`, `BATLEHUB_SERVER` ou le profil — si bien que le hook d'un
environnement de développement tient en une ligne :

```sh
BATLEHUB_SERVER=https://pkg.example.com batlehub-cli setup apply --yes
```

`apply`, `status`, `remove` et `env` fonctionnent sous Linux et macOS ; sous
Windows, ils refusent de s'exécuter, et `detect` imprime les extraits à la place.

| Client | Fichier | Ce qui est écrit |
| --- | --- | --- |
| cargo | `$CARGO_HOME/config.toml` | `[source.crates-io] replace-with`, plus `[registries.batlehub]` pour `cargo publish` |
| npm, pnpm, bun, yarn 1 | `~/.npmrc` | `registry=` |
| yarn 2+ | `~/.yarnrc.yml` | `npmRegistryServer:`, plus `unsafeHttpWhitelist` pour un serveur en HTTP simple, que yarn refuse sinon |
| pip | `~/.config/pip/pip.conf` | `[global] index-url` |
| uv | `~/.config/uv/uv.toml` | un `[[index]]` par défaut |
| go | `$GOENV`, sinon `~/.config/go/env` | `GOPROXY`, sans repli `,direct` |
| maven | `~/.m2/settings.xml` | le fichier entier, un `<mirror>` (seulement s'il est absent ; dès que vous le modifiez, il vous appartient) |
| mise | `$MISE_CONFIG_DIR/conf.d/batlehub.toml` (`~/.config/mise`) | `[settings.url_replacements]` pour les registres github, forgejo, npm, cargo et openvsx qui relaient l'hôte public (`api.github.com`, `codeberg.org`, … ; un registre devant une autre forge n'en reçoit aucune, et la commande le signale) ; pour chaque registre generic, une règle par amont qu'il reflète (`^<amont>/(.+)` vers le registre), après les autres ; `node.mirror_url` pour un registre nodedist. Les deux sortes de règle ont besoin des amonts, que la liste des registres ne montre qu'à un appelant qui détient [`upstreams:read`](/fr/guide/access-control#verbs) ; la liste est lue anonymement, donc cette autorisation va à `*` |
| rustup, node | `~/.config/batlehub/env.sh` | `RUSTUP_DIST_SERVER`, `NODEJS_ORG_MIRROR` et les miroirs nvm/fnm/n, sourcé depuis le **début** de `~/.bashrc` (au-dessus de son `return` non interactif) ou depuis `~/.zshenv` (`$ZDOTDIR/.zshenv` quand `ZDOTDIR` vient d'ailleurs ; un fichier `conf.d` pour fish). Sur macOS, bash reçoit le fichier de connexion qu'il lit — le premier de `~/.bash_profile`, `~/.bash_login`, `~/.profile`. Le shell est `$SHELL`, sinon celui du compte dans `/etc/passwd`. `bash -c` ne lit aucun fichier rc : utilisez-y `BASH_ENV` ou `setup env` |
| conda, mamba, micromamba | `~/.condarc` | `channels:` avec le registre, puis `nodefaults` |
| composer | le `config.json` de composer (`$COMPOSER_HOME`, sinon le choix XDG ou `~/.composer` de Composer) | le fichier entier — une clé `"//"` le marque — avec le registre sous `repositories` et packagist désactivé ; seulement quand `setup` a créé le fichier. Aucun identifiant n'est écrit |
| gem | `~/.gemrc` | `:sources:` |
| dotnet, nuget | `~/.nuget/NuGet/NuGet.Config` (sous `$DOTNET_CLI_HOME` s'il est défini) | le fichier entier : `<clear/>` et une source ; seulement quand `setup` l'a créé |
| terraform, tofu | `~/.terraformrc` | un bloc `provider_installation` avec le registre comme `network_mirror` (https seulement : terraform refuse un miroir en HTTP simple) |
| ansible-galaxy | `~/.ansible.cfg` | `[galaxy] server_list = batlehub` et `[galaxy_server.batlehub]` |
| nix | `~/.config/nix/nix.conf` | `extra-substituters` ; votre utilisateur doit figurer dans les `trusted-users` du démon, ce que `setup` dit au lieu de l'écrire |
| sdkman | `~/.config/batlehub/env.sh` | `SDKMAN_CANDIDATES_API`, `SDKMAN_BROKER_API` |
| mise, pour gitlab | le drop-in mise ci-dessus | des `url_replacements` pour `gitlab.com`, comme pour github et forgejo |
| helm | rien | helm réécrit lui-même `repositories.yaml` et en retire les commentaires, aucun bloc n'y survivrait : `status` lit le fichier et rend `manual` avec le `helm repo add` à lancer, jusqu'à ce qu'un dépôt y porte l'URL du registre |
| apt, dnf/yum, pacman, apk, devfile | rien | `print` : l'extrait, parce que le fichier appartient à root (ou au cluster, pour un registre devfile de Che) |

- **Sans `--yes`, rien n'est écrit** : le diff de chaque fichier est imprimé.
- **Seul ce qui se trouve entre les marqueurs est modifié.** Un bloc commence
  par `# >>> batlehub setup … >>>` et finit par `# <<< batlehub setup <<<` ;
  les fichiers gérés en entier (`settings.xml`, les fichiers déposés) portent le
  marqueur de début en première ligne. `remove` rend chaque fichier à
  l'identique, octet pour octet.
- **Ce que vous modifiez cesse d'être géré.** Le marqueur consigne une
  empreinte de ce qui a été écrit : un bloc ou un fichier déposé modifié
  depuis (à la main, ou par `go env -w`) passe en `conflict` et n'est plus
  touché ni par `apply` ni par `remove` — votre `GOPROXY`, les `<servers>`
  ajoutés à `settings.xml`. Supprimez les lignes du marqueur pour vous
  approprier le réglage, ou tout le bloc (tout le fichier déposé) pour qu'il
  soit réécrit. Les lignes exactes qu'`apply`
  écrirait, trouvées sans marqueurs — collées depuis le guide de
  configuration, ou un bloc que `npm config set`, `npm login`,
  `pip config set` ou `yarn config set` a réécrit sans commentaires — sont
  `up-to-date` et restent les vôtres : `apply` ne se les approprie pas, et
  `remove` se contente de signaler un tel fichier (`--server` connu) sans y
  toucher.
- **Un réglage qui vous appartient n'est jamais modifié.** Si votre partie du
  fichier règle déjà ce que le bloc réglerait — une ligne `registry=`, une
  section `[global]` dans `pip.conf`, votre propre `settings.xml` — ou si une
  variable d'environnement prime sur le fichier (`GOPROXY`,
  `NPM_CONFIG_REGISTRY`, `PIP_INDEX_URL`, un `PIP_CONFIG_FILE` qui existe,
  le `rc`/`auth.ini` global de pnpm, …), le client est en `conflict` :
  ignoré, avec l'extrait à fusionner à la main. De même pour un second index
  que le client interroge en plus de l'instance — `extra-index-url` ou un
  `[install] index-url` dans `pip.conf`, un autre `[[index]]` dans `uv.toml`,
  `PIP_EXTRA_INDEX_URL`, `UV_EXTRA_INDEX_URL` — puisqu'une version que
  l'instance retient s'installerait depuis celui-ci ; et pour un autre fichier
  de configuration mise (`config.local.toml`, un autre fichier `conf.d`) qui
  règle la même table, que mise ne garde que d'un seul fichier. Un réglage qui
  va dans le sens du bloc n'est pas un conflit : l'URL qu'il écrit, une
  variable vide, une variable de fichier de configuration qui désigne ce même
  fichier, le `config` de cargo lié symboliquement à `config.toml`. Les variables sont lues dans
  l'environnement où `apply` s'exécute et dans le fichier rc qu'il gère ; une
  variable exportée depuis un autre fichier n'est pas vue. Quand `env.sh` est
  en conflit, aucune ligne rc ni aucun fichier fish n'est écrit pour le sourcer.
- **Seuls les registres `proxy` et `hybrid` sont retenus**, et seulement ceux
  depuis lesquels le serveur indique que vous pouvez installer (`installable`) :
  un registre `local` ne peut pas remplacer crates.io ou npmjs, et un registre
  listé pour ses seules métadonnées ferait échouer chaque téléchargement. Deux candidats d'un même type sont
  `ambiguous` jusqu'à ce que `--pick npm=<nom>` en choisisse un (et non
  `--registry`, l'option globale du registre par défaut) ; le choix est
  consigné dans le bloc et les exécutions suivantes le conservent. Si le
  registre choisi disparaît, la commande bascule sur l'unique candidat restant
  en le signalant ; s'il n'en reste aucun ou plusieurs, le bloc est
  `orphaned` : conservé sur son URL morte, pour que le client échoue au lieu de
  revenir au registre public, jusqu'à un `--pick` ou un `remove`.
  `--maven-mirror-of` est conservé de la même façon.
- **`--only` / `--skip` choisissent des fichiers, pas des lignes** : un fichier
  partagé par plusieurs clients (`~/.npmrc`, `env.sh`) n'est touché que si l'un
  d'eux est choisi et qu'aucun n'est exclu, et il est toujours écrit pour tous.
- **La liste est lue anonymement**, de sorte qu'un registre fermé aux lectures
  anonymes n'est jamais écrit dans un client qui n'envoie aucun identifiant ; la
  publication passe toujours par `auth login` et `.netrc`.
- **Un serveur injoignable n'écrit rien** (code 1), contrairement à `detect`.
  De même pour un serveur en `http://` hors boucle locale (`--insecure-http`
  pour l'accepter). Un registre dont le `public_url` n'est ni sur l'hôte du
  serveur ni sur l'un de ses sous-domaines (`--allow-foreign-hosts`), ou est en
  HTTP simple sur un serveur HTTPS (`--insecure-http`), n'arrête que ses propres
  clients : ils sont en `conflict`, tous les autres fichiers sont écrits, et la
  commande se termine avec le code 6. Une URL ou un nom de registre contenant un
  caractère qu'un fichier de configuration pourrait mal lire (saut de ligne,
  guillemet, identifiants) est refusé de la même façon.
- **Un fichier cassé est en `conflict` à lui seul** : une ligne de marqueur
  manquante ou un TOML invalide n'arrête que ce client, et `remove` retire
  quand même le reste. Un fichier qui n'est pas en UTF-8 (un `~/.bashrc` en
  latin-1) garde chacun de ses octets.
- **Une liste de registres vide est refusée** (code 1) : un serveur à moitié
  démarré, ou dont tous les registres sont fermés aux lectures anonymes,
  ferait sinon retirer tous les blocs par le `apply --yes` d'un hook — c'est
  le rôle de `remove`, sur demande. La liste est lue sans suivre de
  redirection hors de l'hôte du serveur ni vers du HTTP simple.
- **Un fichier de configuration en lien symbolique** est écrit à travers le
  lien (sa cible est créée si elle manque ; `remove` supprime la cible et
  garde le lien), sauf si la cible appartient à un autre utilisateur que le
  lien. Un fichier en lecture seule est laissé tel quel ; un lien physique est
  rompu par le renommage atomique ; un fichier monté (montage `subPath` de
  Che) est écrit sur place.
- **Lancez-le en tant qu'utilisateur, pas en root** : `apply --yes` et
  `remove --yes` refusent de tourner en root avec un `HOME` qui n'appartient
  pas à root — chaque chemin y est un lien que l'utilisateur peut faire
  pointer vers des fichiers système. Dans un hook root :
  `runuser -u <utilisateur> -- batlehub-cli setup apply --yes`.
- **Maven** lit `${user.home}/.m2`, et Java prend `user.home` dans la base des
  mots de passe, pas dans `$HOME` : là où les deux diffèrent (certains
  conteneurs), passez `-Duser.home` ou alignez `HOME`.

`status` imprime une ligne par fichier : `not-installed`, `no-registry`, `manual` (helm : une commande à lancer), `print` (un extrait pour un fichier que `setup` n'écrit jamais),
`ambiguous`, `absent`, `up-to-date`, `drifted`, `orphaned` (son registre a
disparu ; le bloc est conservé — `--pick` un autre ou `remove`) ou `conflict`. `env` imprime les variables des
clients configurés par l'environnement seul, pour
`eval "$(batlehub-cli setup env)"`, dans un shell que `--shell-rc none` laisse
intact (`none` écarte entièrement les fichiers rc : il n'y écrit ni n'en retire
aucun bloc). `env` tourne à chaque ouverture de shell : il imprime donc
l'`env.sh` écrit par `apply` sans interroger le serveur quand ce fichier n'a pas
été modifié, vise le même serveur (ou n'importe lequel quand ce shell n'en
définit aucun) et qu'aucune variable déjà définie ne le contredit — un registre renommé depuis apparaît au prochain `apply`, ou avec
`--refresh` ou `--pick`, qui interrogent toujours le serveur. Sinon il attend le
serveur 2 s, puis imprime l'`env.sh` écrit la dernière fois quand le serveur ne
peut pas répondre (injoignable, statut
d'erreur, liste illisible ou vide — un `--pick` erroné reste une erreur), selon
les mêmes règles : les URL de ce serveur seulement, rien par-dessus une variable
que vous avez définie, et seulement les variables qu'`apply` écrit, avec des
URL simples. Il n'imprime rien, code 6, quand `env.sh` est en
`conflict` ou `ambiguous` : `eval` écraserait la variable qu'`apply` a refusé
de toucher. `status` prend le `--shell-rc` donné à `apply`.

Codes de sortie, pour un hook ou une étape de CI : `0` plus rien à faire, `2`
une erreur d'usage, `1` une erreur (un fichier impossible à écrire n'arrête pas les autres ; chacun est
signalé), `3` terminé mais un client demande votre attention (`conflict`,
`ambiguous` ; pour `status`, tout ce qui n'est pas réglé ; pour `remove`, un
fichier laissé en place ; pour `apply`, aussi un bloc `orphaned`, et des variables qu'aucun fichier rc ne source
— shell inconnu : passez `--shell-rc none` et évaluez
`eval "$(batlehub-cli setup env)"` vous-même). `apply` sans `--yes` sort comme le ferait la vraie
exécution : 0 même s'il reste des fichiers à écrire.

---

## 16. Le mode TUI

```
batlehub-cli tui
# ou
task cli:tui
```

La TUI est une interface plein écran en terminal, construite avec
[ratatui](https://ratatui.rs).

### Écrans

```
╔ BatleHub — Registries ═══════════════════════════════╗
║ > cargo    (cargo  ) [proxy ]                         ║
║   internal (nuget  ) [hybrid]                         ║
║   pypi     (pypi   ) [local ]                         ║
╚══════════════════════════════════════════════════════╝
 q:quit  ↑↓:navigate  Enter:select  p:publish  ?:help
```

| Écran | Comment y arriver |
|--------|--------------|
| Liste des registres | Au lancement, ou `Échap` depuis la liste des paquets |
| Liste des paquets | `Entrée` sur un registre |
| Détail d'une version | `Entrée` sur un paquet |
| Assistant de publication | `p` depuis la liste des registres |
| Aide | `?` depuis n'importe quel écran |

### Raccourcis clavier

| Touche | Action |
|-----|--------|
| `q` / `Ctrl-C` | Quitter |
| `Échap` | Revenir à l'écran précédent |
| `↑` / `k` | Monter dans la sélection |
| `↓` / `j` | Descendre dans la sélection |
| `Entrée` | Ouvrir l'élément sélectionné |
| `/` | Activer le filtre de recherche de paquets |
| `y` | Retirer la version sélectionnée (écran de détail d'une version) |
| `u` | Annuler le retrait de la version sélectionnée |
| `p` | Ouvrir l'assistant de publication |
| `?` | Afficher ou masquer l'aide |
| `Tab` / `Maj-Tab` | Passer d'un champ à l'autre dans l'assistant de publication |

## 17. Commandes — why et wait {#commands-security}

Les deux verbes d'une quarantaine de chaîne d'approvisionnement
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)). Un registre placé
derrière `[registries.security]` refuse une version qu'il n'a pas encore jugée,
ou qu'il a jugée défavorablement ; le refus nomme la coordonnée, l'état, les
codes de motif et cette commande.

### `why <registry>/<name>@<version>`

Le verdict derrière un refus : l'état, les codes de motif, la date de levée d'une
retenue, quels scanners ont répondu et — pour un token portant `findings:read` —
les constats eux-mêmes. Exige `quarantine:read` sur le registre (`user` et
`admin` le détiennent par défaut ; `anonymous` non, de sorte qu'un miroir public
répond un simple 404 tant que l'opérateur ne l'accorde pas).

```bash
batlehub-cli why npm/left-pad@1.3.1
batlehub-cli why npm/left-pad@1.3.1 --json
batlehub-cli why npm/left-pad@1.3.1 --rescan     # met aussi une réanalyse en file (exige gates:exempt)
batlehub-cli -r npm why left-pad@1.3.1           # un nom nu, dans le registre par défaut
```

```text
npm/left-pad@1.3.1
  state        quarantined
  reasons      MIN_AGE_NOT_MET
  docs         https://batlehub.dev/use/refused-downloads#min-age-not-met
  available    2026-09-05T14:12:00Z
  policy       npm/default
  evaluated    2026-09-04T14:12:03Z
  scanned      2026-09-04T14:12:01Z
  scanners     osv
  findings     none

Held until 2026-09-05T14:12:00Z. `batlehub-cli wait` waits for it.
```

La ligne `docs` relie chaque code de raison à sa section de [Pourquoi un téléchargement est refusé](/fr/use/refused-downloads), qui explique ce que signifie le code et ce que vous pouvez faire.

### `wait <registry>/<name>@<version> [--timeout 1h] [--interval 30s]`

Le contrat pour la CI. La commande interroge le verdict et sort avec **0** quand
la version devient servable, **4 quand attendre n'y changera rien** — un verdict
`denied`, ou une retenue sans horloge comme `TIMESTAMP_MISSING` — et **8** en cas
de dépassement de délai ([codes de sortie](#exit-codes)). Le code 4 revient dès la première interrogation, avec le
motif, jamais après avoir consommé le délai : un pipeline échoue donc vite sur
une décision et n'attend que sur une horloge. Quand la retenue nomme un
`available_at`, l'attente dort jusque-là au lieu d'interroger en boucle.

```bash
batlehub-cli wait npm/left-pad@1.3.1 --timeout 2h && npm ci
```

Une version que cette instance ne s'est jamais vu demander n'a pas de verdict à
attendre : demandez l'artefact une fois (c'est la première requête qui crée la
retenue et met l'analyse en file), puis attendez.

---

## 18. Commandes — download {#commands-download}

Récupère un fichier à travers le proxy, qui le met en cache au passage. C'est
ainsi qu'un registre adressé par chemin (`deb`, `rpm`, `pacman`, `apk`,
`jetbrains`, `generic`) se préchauffe fichier par fichier, et ainsi qu'on vérifie qu'un chemin
que le proxy est censé servir se résout bien.

```
batlehub-cli download <target> [-o <file>]
```

`<target>` est une coordonnée ou un chemin :

```sh
# une coordonnée : l'artefact unique de la version, vérifié contre ses métadonnées
batlehub-cli download npm-local/left-pad@1.3.0
batlehub-cli -r crates download serde@1.0.200

# une URL complète
batlehub-cli download https://hub.example.dev/proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz

# un chemin de serveur, contre le serveur configuré
batlehub-cli download /proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz

# relatif au registre, ce qui exige -r
batlehub-cli download -r jb jetbrains/idea/idea-2026.1.3.tar.gz
```

**Par coordonnée**, le type doit avoir un artefact par version : npm, cargo,
composer, rubygems, goproxy, nuget, openvsx, vscode-marketplace,
jetbrains-marketplace et galaxy. Tout autre type sort avec le code 2 et dit
pourquoi (une version PyPI est plusieurs fichiers, une version Maven un jar et un
pom) ; donnez alors son chemin `/proxy/…`. Le fichier est nommé d'après la
coordonnée (`left-pad-1.3.0.tgz`), jamais d'après ce que le serveur envoie.

Les octets vont dans `<file>.part` et sont hachés au fil du flux, contre la
somme de contrôle qu'annoncent les métadonnées du type (`dist.integrity` pour
npm, le `cksum` de l'index pour cargo). Seule une correspondance est renommée à
sa place ; une discordance, un refus ou un Ctrl-C supprime le `.part` et sort
avec un code non nul. Un type dont les métadonnées ne portent aucune somme que
la CLI sache lire est écrit avec un `warning:` qui le dit non vérifié.

`-o` nomme le fichier de sortie, et un fichier existant n'est remplacé qu'après
une [confirmation](#confirmation). `-o -` écrit sur la sortie standard pour que
le fichier puisse être redirigé ; une discordance y est signalée après le
dernier octet, et le code de sortie (1) en est le signal.

---

## 19. Commandes — authz {#commands-authz}

Demander au serveur ce qu'un sujet a le droit de faire, et lire ce que le mode
fantôme a laissé passer
([RFC 0015](/rfc/0015-grants-on-the-resource-hierarchy) §4.8). Les deux répondent
d'après la configuration en cours d'exécution, pas d'après un fichier que vous
leur passez.

```
batlehub-cli authz explain --subject <s> --action <verb> <registry> [--package <p>] [--version <v>]
batlehub-cli authz shadow [--limit <n>] [--detail]
```

`--subject` prend l'orthographe des autorisations : `*`, `role:user`,
`user:alice`, `group:oidc1:eng`, ou `group:*:eng` pour ce groupe chez n'importe
quel fournisseur. La réponse nomme **le niveau qui a accordé chaque verbe**, et
c'est toute la différence avec la lecture de la configuration à la main.

```sh
batlehub-cli authz explain --subject group:oidc1:eng --action releases:publish internal-npm
```

`--package` compte : les niveaux namespace et paquet ne s'appliquent que si un
paquet est donné, de sorte qu'un `explain` sans paquet répond à une question plus
étroite qu'il n'y paraît. `--version` atteint le niveau version.

`authz shadow` signale ce que le mode fantôme a servi et que l'application aurait
refusé — la liste à vider avant d'activer l'application. Il résume par nœud par
défaut ; `--detail` imprime chaque entrée.

---

## 20. Commandes — verdict {#commands-verdicts}

Le côté administrateur d'un verdict de chaîne d'approvisionnement
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)). Là où `why` répond
à *pourquoi mon installation est-elle retenue*, celles-ci répondent à *qu'est-ce
qui est retenu, et qui l'a déjà récupéré*.

```
batlehub-cli verdict pullers  <registry>/<name>@<version> [--since <window>] [--format csv]
batlehub-cli verdict list     -r <r> [--state <s>] [--limit <n>]
batlehub-cli verdict backfill -r <r>
batlehub-cli verdict rescan   -r <r> [--state <s>]
```

`pullers` est la question d'incident : qui a récupéré cette version dans une
fenêtre donnée, depuis la même requête de journal d'accès que porte l'alerte de
bascule. `--since` accepte `30d`, `12h`, `90m` ou un instant RFC 3339, et vaut
par défaut le `pullers_window_days` du registre. `--format csv` imprime le CSV que rend
l'endpoint d'export, pour le transmettre à quelqu'un qui n'a pas la CLI.

```sh
batlehub-cli verdict pullers npm/left-pad@1.3.1 --since 30d --format csv
```

`list` montre les verdicts d'un registre par état — `allowed`, `warned`,
`quarantined`, `denied`, ou tous les états quand `--state` est omis. `--limit`
s'applique par état, vaut 100 par défaut et plafonne à 1000.

`backfill` met en file une analyse de basse priorité de toutes les versions en
cache d'un registre, pour celles qui étaient déjà là quand l'analyse a été
activée. `rescan` remet en file des verdicts qui existent déjà, éventuellement
d'un seul état — ce sont deux travaux différents, et la distinction compte sur un
gros registre.

---

## 21. Commandes — completion {#commands-completion}

Imprime un script de complétion shell sur la sortie standard.

```
batlehub-cli completion <bash|elvish|fish|powershell|zsh>
```

La commande n'écrit rien elle-même : où va le script vous appartient.

```sh
# bash, pour cet utilisateur
batlehub-cli completion bash > ~/.local/share/bash-completion/completions/batlehub-cli

# zsh, dans un répertoire déjà présent dans $fpath
batlehub-cli completion zsh > ~/.zfunc/_batlehub-cli

# fish
batlehub-cli completion fish > ~/.config/fish/completions/batlehub-cli.fish
```

---

## 22. Commandes — audit {#commands-audit}

Ce qu'une identité a récupéré
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts) §4.2). La transposée
de [`verdict pullers`](#commands-verdicts) : celle-là fixe une version et
demande qui l'a prise, celle-ci fixe une identité et demande ce qu'elle a pris.

```
batlehub-cli audit pulls --identity <id> [-r <r>] [--package <p>] [--since <window>] [--format csv]
```

`--identity` est un identifiant d'utilisateur, ou `ip:<addr>` pour un appelant
anonyme — souvent la seule prise qu'un incident ait sur un runner qui ne présente
aucun identifiant. `--since` accepte `30d`, `12h`, `90m` ou un instant RFC 3339,
et vaut 30 jours par défaut. `--format csv` imprime le CSV que rend l'endpoint, octet
pour octet, pour le transmettre à quelqu'un qui n'a pas la CLI.

```
$ batlehub-cli audit pulls --identity ci-bot --since 7d
ci-bot pulled 2 coordinates since 2026-09-01T00:00:00Z
registry       package                      version         pulls  last
npm-local      demo-pkg                     1.0.0               3  2026-09-08T23:20:33Z
crates         serde                        1.0.219            11  2026-09-08T21:04:11Z
```

Celle-ci et `verdict pullers` exigent `audit:read`, et toutes deux ne
comptent **que les octets livrés** : un refus n'a rien transféré et relève d'une
autre question. Les lignes sont groupées par coordonnée, le pull le plus récent
en premier.

::: tip
Les colonnes `client_user_agent` et `source_ip` sont remplies à chaque
téléchargement livré, qu'il soit passé par le proxy ou servi depuis le stockage
propre d'un registre local ou hybrid. Les lignes enregistrées avant que cette
seconde moitié n'arrive n'ont ni l'une ni l'autre, et rien ne les complète
rétroactivement — une paire vide à côté d'un `count` non nul date donc la ligne
plutôt qu'elle ne décrit l'appelant.
:::

## 23. Commandes — java {#commands-java}

Le moteur Java des extensions VS Code de BatleHub : les inspections, correctifs,
générateurs et renommages de leur support Java, sur un vrai serveur de langage
Java et sans éditeur — pour une porte de CI et pour des agents. Tout ce qui suit
`java` appartient au moteur, code de sortie compris :

```
$ batlehub-cli java inspect . --fail-on warning --format sarif > java.sarif
$ batlehub-cli java fix src/main/java/com/acme/Greeter.java --write
$ batlehub-cli java mcp          # les mêmes verbes, en outils MCP sur stdio
$ batlehub-cli java --help       # chaque verbe, option et code de sortie
```

Il lui faut un `node` ≥ 22 sur le `PATH` (`mise use node@24`) et un JDK ≥ 21
que sa découverte sache trouver. Le moteur est intégré à ce binaire et écrit
sous le répertoire de cache (`~/.cache/batlehub/java-engine/<hash>/`) à la
première utilisation ; sa première exécution télécharge aussi, une seule fois,
le serveur de langage Java épinglé. `BATLEHUB_JAVA_ENGINE=<chemin vers batlehub-java.mjs>`
exécute un autre build. Aucun serveur, jeton ni profil n'est lu : `--profile` et
`--json` après `java` sont des options du moteur, pas de la CLI.

Les verbes, la configuration qu'ils lisent (uniquement les fichiers commités de
`.batlehub/java/`) et l'extrait de CI sont documentés avec le moteur, dans
[le guide des extensions](https://github.com/batlehub/batlehub-vsx/blob/main/docs/guide/java/engine.md).
