---
reference: true
sourcePath: use/cli.md
sourceHash: 91ca4dbf20513c85
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
batlehub-cli config init
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

---

## 3. Options globales

Ces options sont disponibles sur toutes les commandes :

| Option | Courte | Description |
|------|-------|-------------|
| `--profile <name>` | `-P` | Utiliser un profil de configuration nommé |
| `--server <url>` | | Remplacer l'URL du serveur |
| `--token <tok>` | | Remplacer le token d'authentification |
| `--registry <name>` | `-r` | Définir un registre par défaut |
| `--json` | | Émettre du JSON exploitable par une machine plutôt que des tables |

---

## 4. Commandes — registry

```
batlehub-cli registry list
batlehub-cli registry info <name>
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

### `registry info <name>`

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
  `static.rust-lang.org`, `dl.google.com`, `get.helm.sh`, `dl.min.io`,
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
batlehub-cli package versions <registry> <name>
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

### `package versions <registry> <name>`

Liste toutes les versions en cache d'un paquet, avec leur état et leur nombre de
téléchargements.

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

## 6. Commandes — version {#commands-version}

```
batlehub-cli version yank   <registry> <name> <version>
batlehub-cli version unyank <registry> <name> <version>
batlehub-cli version delete <registry> <name> <version> [--yes]
batlehub-cli version pin    <registry> <name> <version>
batlehub-cli version unpin  <registry> <name> <version>
```

Ces commandes exigent un token d'admin.

| Commande | Effet |
|---------|--------|
| `yank` | Marque une version indisponible (elle reste en stockage, le téléchargement est bloqué) |
| `unyank` | Annule un `yank` |
| `delete` | Supprime l'artefact **et consomme définitivement le numéro de version** |
| `pin` | Exempte une version de la rétention — elle n'est jamais reprise automatiquement |
| `unpin` | Lève l'épinglage : la politique de rétention du registre s'applique de nouveau |

> **Casse des noms de paquets** : les noms sont normalisés en minuscules à la
> publication (NuGet met l'identifiant en minuscules, cargo et npm le font par
> convention). Employez la forme en minuscules avec `version yank/unyank/delete`
> pour correspondre au nom stocké — `serilog`, pas `Serilog`.

`delete` demande confirmation, sauf si `--yes` est passé :

```
$ batlehub-cli version delete internal serilog 2.0.0
Delete internal/serilog@2.0.0? The artifact is dropped and the version number is
spent permanently — 2.0.0 can never be published again. [y/N] y
Deleted internal/serilog@2.0.0
```

Un numéro de version supprimé n'est jamais réutilisé. Republier `2.0.0` est
refusé par un `409`, quel que soit le demandeur et quel que soit le délai :
« supprimer et renvoyer pour corriger » n'est donc pas un plan — publiez
`2.0.1`, ou faites un `yank` si vous voulez seulement que la version cesse d'être
installée. Le raisonnement, et ce que la suppression laisse à un auditeur, sont
dans [Supprimer une version publiée](/fr/guide/admin-policies#deleting-versions).

---

## 7. Commandes — owners

```
batlehub-cli owners list   <registry> <name>
batlehub-cli owners add    <registry> <name> <principal> [--type user|group] [--role admin|maintainer]
batlehub-cli owners remove <registry> <name> <principal> [--type user|group]
```

La propriété décide qui peut publier de nouvelles versions sur un registre local
ou hybrid. Exige un token d'admin.

```
$ batlehub-cli owners list internal Serilog
+------+------------------+------------+------------+
| Type | Principal        | Role       | Granted By |
+------+------------------+------------+------------+
| user | alice@example.com| admin      | -          |
| group| nuget-maintainers| maintainer | alice      |
+------+------------------+------------+------------+

$ batlehub-cli owners add internal Serilog bob --type user --role maintainer
Added user 'bob' as maintainer on internal/Serilog
```

---

## 8. Commandes — publish

```
batlehub-cli publish <file> [--registry <r>] [--name <n>] [--version <v>] [--type <t>]
                             [--distribution <d>] [--component <c>] [--platform <p>]
```

Envoie un artefact sur un registre local ou hybrid. La CLI détecte
automatiquement le type de registre et les métadonnées du paquet à partir du
fichier :

| Extension | Type de registre | Source des métadonnées |
|-----------|---------------|-----------------|
| `.nupkg` | nuget | le `.nuspec` embarqué |
| `.whl` | pypi | le nom de fichier (`name-version-*.whl`) |
| `.gem` | rubygems | le nom de fichier (`name-version.gem`) |
| `.pkg.tar.{zst,xz,gz}` | pacman | le nom de fichier (`name-pkgver-pkgrel-arch.pkg.tar.*`) |
| `.tgz` | npm | le nom de fichier (`name-version.tgz`, tel que produit par `npm pack`) |
| `.crate` | cargo | le nom de fichier (`name-version.crate`, tel que produit par `cargo package`) |
| `.vsix` | openvsx | le nom de fichier (`extension_id-version.vsix`) |
| `.deb` | deb | côté serveur, depuis le fichier de contrôle du paquet — exige `--distribution` et `--component` |
| `.rpm` | rpm | côté serveur, depuis l'en-tête du paquet |
| `.tar.bz2` / `.conda` | conda | côté serveur, depuis l'`info/index.json` du paquet (`--platform` n'est qu'un repli) |

Les ZIP Composer partagent l'extension générique `.zip` avec d'autres formats et
ne sont donc pas détectés automatiquement — passez `--type composer`
explicitement. Composer, comme conda, deb et rpm, lit le nom et la version côté
serveur depuis `composer.json` : ni `--name` ni `--version` ne sont nécessaires
(un `--version` facultatif remplace la version de l'archive).

`--type` remplace entièrement la détection automatique — utile pour les
extensions ambiguës ou lorsqu'un fichier ne suit pas la convention de nommage
attendue.

Maven (fichiers jar + pom + sommes de contrôle séparés), Terraform (les providers
exigent des fichiers de shasums et de signature ; les modules exigent une étape
d'empaquetage) et les modules Go (qui exigent un triplet `.info` / `.mod` /
`.zip`) ne rentrent pas, à dessein, dans le modèle à fichier unique de cette
commande. Servez-vous de votre outillage habituel (`mvn deploy`, les conventions
de publication du registre Terraform, `go mod`) configuré pour pointer vers
l'endpoint BatleHub — les instructions de mise en place par registre sont dans
[Publier des paquets](publishing.md).

```bash
# NuGet
batlehub-cli publish Serilog.3.1.1.nupkg --registry internal

# Remplacer les métadonnées détectées
batlehub-cli publish dist/mylib-1.2.3.tar.gz --type pypi --name mylib --version 1.2.3

# Composer (extension .zip ambiguë — le type doit être explicite)
batlehub-cli publish acme-widget.zip --type composer --registry internal

# Debian (distribution et composant ne sont pas dans le nom de fichier)
batlehub-cli publish hello_1.0-1_amd64.deb --registry internal --distribution stable --component main

# Conda (la plateforme n'est qu'un repli pour les paquets sans subdir embarqué)
batlehub-cli publish numpy-1.26.0-py311h0.conda --registry internal --platform linux-64
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
batlehub-cli auth token revoke <uuid>
batlehub-cli auth write-token-file [--path <p>] [--from-file <p>]
batlehub-cli auth status [--path <p>] [--json]
batlehub-cli proxy serve --registry <url> [--bind 127.0.0.1:0] [--contract <p>]
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
$ batlehub-cli proxy serve --registry https://hub.example.dev/proxy/vsx
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
    --registry https://hub.example.dev/proxy/vsx --id batleforc.weebo-bridge-notify --version 0.5.0
ok: weebo-bridge-notify-0.5.0.vsix is signed by key 3f1e0a9c7b2d4e61… (24503 bytes, manifest matches)
```

`--signature <archive> --public-key <pem|hex|file>` vérifie hors ligne. La
commande ne fait pas tourner le vérificateur propre à l'éditeur, qui n'accepte
que la signature de la place de marché.


## 12. Commandes — admin

Ces commandes exigent un token d'admin.

### Quota

```
batlehub-cli admin quota list   [--registry <r>]
batlehub-cli admin quota reset  <registry> <user>
```

### Blocages d'IP

```
batlehub-cli admin ip-block list
batlehub-cli admin ip-block add    <ip> [--reason <text>]
batlehub-cli admin ip-block remove <ip>
```

### Configuration

```
batlehub-cli admin config reload    # déclencher un rechargement à chaud sur le serveur
batlehub-cli admin config changes   # consulter l'historique des changements
```

### Cache

```
batlehub-cli admin cache warm  <registry> [--packages pkg1,pkg2]
batlehub-cli admin cache clear <registry>
```

### Import

Exécute maintenant les `[[release_imports]]` configurés d'un registre, quel que
soit leur intervalle
([RFC 0021](/rfc/0021-forge-releases-into-registries)).

```
batlehub-cli admin import <registry> [--tag <tag>] [--repo <owner/name>]
```

`--tag` importe ce tag plutôt que ce que la configuration sélectionne, et c'est
le seul moyen d'atteindre délibérément une pre-release : `latest` n'en choisira
pas. `--repo` exécute l'un des imports configurés dans un même registre.

```
$ batlehub-cli admin import vsx-local
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
batlehub-cli admin banner set   "Maintenance at 22:00 UTC" [--level info|warning|error]
batlehub-cli admin banner clear
```

### Statistiques et santé

```
batlehub-cli admin stats     # taux de hit du cache, octets servis, compteurs agrégés
batlehub-cli admin health    # santé par registre et par backend
```

Les deux sont en lecture seule et ne prennent aucun argument. `health` est la
commande à lancer quand un client signale des échecs sur un registre et pas sur
les autres.

### Notifications

Les canaux sont déclarés dans `config.toml` sous `[notifications]` et sont en
lecture seule ici ; les abonnements sont des lignes que cette commande gère.

```
batlehub-cli admin notifications channels        # les canaux sortants configurés
batlehub-cli admin notifications list            # les abonnements
batlehub-cli admin notifications delete <id>
```

Aucune des deux listes n'imprime d'URL de webhook ni de secret de signature. Voir
[`[notifications]`](/fr/guide/configuration#_3-8d-notifications-optional) pour le
côté configuration.

### Autorisations

Les niveaux paquet et version de la hiérarchie d'autorisation — les deux qu'un
fichier de configuration ne peut pas énumérer. Les autorisations de niveau
registre et namespace restent dans `config.toml`.

```
batlehub-cli admin grants list <registry> <name>[@<version>]
batlehub-cli admin grants set  <registry> <name>[@<version>] --subject <s> --actions <a1,a2>
batlehub-cli admin grants rm   <registry> <name>[@<version>] --subject <s>
```

```
$ batlehub-cli admin grants set npm1 @acme/billing \
      --subject group:oidc1:eng --actions releases:read,releases:list
Granted releases:read, releases:list on npm1/@acme/billing to group:oidc1:eng

$ batlehub-cli admin grants list npm1 @acme/billing
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

- **`name@version` adresse une version**, en coupant sur le *dernier* `@` — donc
  `@acme/billing` est un paquet et `@acme/billing@2.4.0` une version.
- **Ce que `set` imprime est ce qui a été stocké.** `--actions releases:*` nomme
  un verbe et en stocke plusieurs ; la sortie est l'ensemble développé. La
  commande imprime aussi des avertissements pour une autorisation légale mais
  inerte — une qu'un niveau plus large accorde déjà, ou une posée sur une version
  retirée.
- **Les lignes `Source: ownership` ne s'éditent pas ici.** Elles suivent la liste
  des propriétaires du paquet ; changez-les avec `admin owner`. En éditer une
  vaut un `409`.
- Exige `grants:write` (`grants:read` pour `list`), que `role:admin` détient.

### Exposition et signalements

Qui a récupéré une version signalée, et ce que les sources configurées ont
signalé ([RFC 0002](/rfc/0002-vulnerability-flags-and-exposure)).

```
batlehub-cli admin exposure [--registry <r>] [--package <p>] [--source <s>]
                            [--min-effect inform|warn|gate|hard_block]
                            [--when any|before-flag|after-flag]
                            [--from <t>] [--to <t>] [--after <cursor>] [--limit 100]

batlehub-cli admin flags list [--registry <r>] [--package <p>] [--source <s>]
                              [--effect inform|warn|gate|hard_block] [--include-dead]
                              [--page 0] [--per-page 50]
```

`exposure` donne une ligne par consommateur, le pull le plus récent en premier,
avec le nombre de pulls qui ont précédé le signalement — le cas rétroactif, où
l'alerte est arrivée après le téléchargement. La pagination se fait par curseur :
`--after` prend celui qu'a imprimé la page précédente. `--min-effect` signifie
« ou plus fort ».

`admin flags list` ne montre que les signalements vivants, sauf si
`--include-dead` ajoute les révoqués et les expirés.

### Vérification d'accès

```
batlehub-cli admin access-check --registry <r> --package <p> --version <v>
                                [--resource releases:read] [--user <id>]
                                [--role anonymous|user|admin] [--groups <g1,g2>]
```

Simule une décision sans émettre de requête sous cette identité. Les trois
options de coordonnée sont obligatoires. Voir aussi
[`authz explain`](#commands-authz), qui répond à la question plus large de ce
qu'un sujet a le droit de faire, en nommant le niveau qui a accordé chaque verbe.

### Visibilité, namespaces et utilisateurs

```
batlehub-cli admin visibility get <registry> <name>
batlehub-cli admin visibility set <registry> <name> public|internal|team

batlehub-cli admin namespace list    <registry>
batlehub-cli admin namespace claim   <registry> <prefix> <group-id>
batlehub-cli admin namespace release <registry> <prefix>

batlehub-cli admin users list-blocked
batlehub-cli admin users block   <user-id> [--reason <text>]
batlehub-cli admin users unblock <user-id>
```

Toutes les trois changent qui peut faire quoi, et prennent effet immédiatement.
`namespace release` rouvre un préfixe à quiconque peut publier dans le registre,
et c'est celle dont la portée est plus grande qu'elle n'en a l'air.

### Cycle de vie d'un paquet

Deux paires réversibles, chacune sur une version :

```
batlehub-cli admin deprecate   <registry> <name> <version> [--message <text>]
batlehub-cli admin undeprecate <registry> <name> <version>

batlehub-cli admin unlist <registry> <name> <version>
batlehub-cli admin relist <registry> <name> <version>
```

Déprécier avertit le consommateur et continue de servir ; retirer des listes
masque la version dans la recherche et les listings et continue de la servir à
qui la demande par version exacte. Aucune des deux ne supprime quoi que ce soit —
pour cela, voir [`version yank` et `version delete`](#commands-version).

### Opérations en masse

```
batlehub-cli admin bulk yank   <registry> <name@version>...
batlehub-cli admin bulk unyank <registry> <name@version>...
batlehub-cli admin bulk delete <registry> <name@version>...
```

::: danger
`admin bulk delete` est **irréversible et sans garde-fou**. Il n'y a ni
`--dry-run`, ni demande de confirmation, ni verrou `--yes`, et la commande agit
sur toutes les coordonnées de la ligne de commande dès que vous appuyez sur
Entrée. À comparer avec [`admin retention`](#retention), qui exige à la fois
`--reclaim` et un `dry_run = false` côté serveur avant de supprimer quoi que ce
soit. Vérifiez d'abord la liste avec `admin bulk yank` si vous n'êtes pas
certain.
:::

La sortie est `processed=N succeeded=N failed=N`, suivie d'une ligne
`FAILED name@version: error` par échec — les échecs sont nommés, pas seulement
comptés.

### SBOM

```
batlehub-cli admin sbom get    <registry> <name> <version> [--format cyclonedx]
batlehub-cli admin sbom export [--registry <r>] [--from <t>] [--to <t>]
                               [--format cyclonedx] [-o <file>]
```

`get` imprime toujours du JSON. `export` écrit dans le fichier nommé, ou sur la
sortie standard sans `-o`. Voir [SBOM](/fr/guide/sbom) pour les formats et leur
contenu.

### Journal d'audit

```
batlehub-cli admin audit-log [--registry <r>] [--user <id>] [--from <date>] [--to <date>] [--denied-only]
```

#### Export pour la conformité

```
batlehub-cli admin export-audit-log [--from <t>] [--to <t>] [--registry <r>]
                                    [--action delete,retention_reclaim]
                                    [--format json|csv] [-o <file>]
```

À distinguer d'`admin audit-log` ci-dessus, qui est la requête interactive
paginée. Celle-ci est l'export qu'un auditeur conserve : une fenêtre entière dans
un seul document, écrit dans `-o` ou sur la sortie standard. Pour la question
rapportée à une identité — qu'est-ce que *ce compte* a récupéré — voir
[`audit pulls`](#commands-audit), qui agrège plutôt que de lister chaque
événement.

### Rétention

```
batlehub-cli admin retention <registry> [--show-kept] [--reclaim]
```

Reprend les versions publiées localement que la politique
`[registries.retention]` du registre ne conserve plus. **Se contente de faire un
rapport par défaut** — `--reclaim` n'est que la moitié du verrou, et le registre
doit en plus être en `dry_run = false`. Deux décisions à deux endroits, parce
qu'un artefact repris peut n'exister nulle part ailleurs.

`--show-kept` imprime chaque version survivante et la condition qui l'a sauvée,
ce qui est la façon de confronter une politique à son effet réel avant de
l'armer.

Épingler une version précise contre la rétention, c'est
[`version pin`](#commands-version).

### Coupure réseau

```
batlehub-cli admin air-gap-missing [--registry <r>] [--kind artifact|document|checksum|ref|unmirrored_host]
batlehub-cli admin bundles
```

Ce qu'une instance coupée du réseau s'est vu demander et ne détenait pas, et ce
qui a traversé la coupure. Voir
[la procédure de coupure réseau](/fr/operations/air-gap).

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

## 14. Commandes — config

```
batlehub-cli config init           # assistant interactif de premier lancement
batlehub-cli config show           # imprimer la configuration résolue (le token est masqué)
batlehub-cli config set server_url https://batlehub.example.com
batlehub-cli config set token      my-token [--profile prod]
batlehub-cli config set registry   internal [--profile prod]
```

Clés valides pour `config set` : `server_url`, `token`, `registry`.

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

### `why <registry>:<name>@<version>`

Le verdict derrière un refus : l'état, les codes de motif, la date de levée d'une
retenue, quels scanners ont répondu et — pour un token portant `findings:read` —
les constats eux-mêmes. Exige `quarantine:read` sur le registre (`user` et
`admin` le détiennent par défaut ; `anonymous` non, de sorte qu'un miroir public
répond un simple 404 tant que l'opérateur ne l'accorde pas).

```bash
batlehub why npm:left-pad@1.3.1
batlehub why npm:left-pad@1.3.1 --json
batlehub why npm:left-pad@1.3.1 --rescan     # met aussi une réanalyse en file (exige gates:exempt)
```

```text
npm:left-pad@1.3.1
  state        quarantined
  reasons      MIN_AGE_NOT_MET
  available    2026-09-05T14:12:00Z
  policy       npm/default
  evaluated    2026-09-04T14:12:03Z
  scanned      2026-09-04T14:12:01Z
  scanners     osv
  findings     none

Held until 2026-09-05T14:12:00Z. `batlehub wait` waits for it.
```

### `wait <registry>:<name>@<version> [--timeout 1h] [--interval 30s]`

Le contrat pour la CI. La commande interroge le verdict et sort avec **0** quand
la version devient servable, **1 quand attendre n'y changera rien** — un verdict
`denied`, ou une retenue sans horloge comme `TIMESTAMP_MISSING` — et **2** en cas
de dépassement de délai. Le code 1 revient dès la première interrogation, avec le
motif, jamais après avoir consommé le délai : un pipeline échoue donc vite sur
une décision et n'attend que sur une horloge. Quand la retenue nomme un
`available_at`, l'attente dort jusque-là au lieu d'interroger en boucle.

```bash
batlehub wait npm:left-pad@1.3.1 --timeout 2h && npm ci
```

Une version que cette instance ne s'est jamais vu demander n'a pas de verdict à
attendre : demandez l'artefact une fois (c'est la première requête qui crée la
retenue et met l'analyse en file), puis attendez.

---

## 18. Commandes — download {#commands-download}

Récupère un fichier à travers le proxy, qui le met en cache au passage. C'est
ainsi qu'un registre adressé par chemin (`deb`, `rpm`, `pacman`, `jetbrains`,
`generic`) se préchauffe fichier par fichier, et ainsi qu'on vérifie qu'un chemin
que le proxy est censé servir se résout bien.

```
batlehub-cli download <target> [-o <file>]
```

`<target>` prend trois formes, et la troisième est la raison d'être de
`--registry` :

```sh
# une URL complète
batlehub-cli download https://hub.example.dev/proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz

# un chemin de serveur, contre le serveur configuré
batlehub-cli download /proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz

# relatif au registre, ce qui exige -r
batlehub-cli download -r jb jetbrains/idea/idea-2026.1.3.tar.gz
```

`-o` nomme le fichier de sortie ; par défaut c'est le nom de base du chemin, et
`-o -` écrit sur la sortie standard pour que le fichier puisse être redirigé.

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

## 20. Commandes — verdicts {#commands-verdicts}

Le côté administrateur d'un verdict de chaîne d'approvisionnement
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)). Là où `why` répond
à *pourquoi mon installation est-elle retenue*, celles-ci répondent à *qu'est-ce
qui est retenu, et qui l'a déjà récupéré*.

```
batlehub-cli verdicts pullers  <registry>:<name>@<version> [--since <window>] [--csv]
batlehub-cli verdicts list     --registry <r> [--state <s>] [--limit <n>]
batlehub-cli verdicts backfill --registry <r>
batlehub-cli verdicts rescan   --registry <r> [--state <s>]
```

`pullers` est la question d'incident : qui a récupéré cette version dans une
fenêtre donnée, depuis la même requête de journal d'accès que porte l'alerte de
bascule. `--since` accepte `30d`, `12h`, `90m` ou un instant RFC 3339, et vaut
par défaut le `pullers_window_days` du registre. `--csv` imprime le CSV que rend
l'endpoint d'export, pour le transmettre à quelqu'un qui n'a pas la CLI.

```sh
batlehub-cli verdicts pullers npm:left-pad@1.3.1 --since 30d --csv
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
de [`verdicts pullers`](#commands-verdicts) : celle-là fixe une version et
demande qui l'a prise, celle-ci fixe une identité et demande ce qu'elle a pris.

```
batlehub-cli audit pulls --identity <id> [--registry <r>] [--package <p>] [--since <window>] [--csv]
```

`--identity` est un identifiant d'utilisateur, ou `ip:<addr>` pour un appelant
anonyme — souvent la seule prise qu'un incident ait sur un runner qui ne présente
aucun identifiant. `--since` accepte `30d`, `12h`, `90m` ou un instant RFC 3339,
et vaut 30 jours par défaut. `--csv` imprime le CSV que rend l'endpoint, octet
pour octet, pour le transmettre à quelqu'un qui n'a pas la CLI.

```
$ batlehub-cli audit pulls --identity ci-bot --since 7d
ci-bot pulled 2 coordinates since 2026-09-01T00:00:00Z
registry       package                      version         pulls  last
npm-local      demo-pkg                     1.0.0               3  2026-09-08T23:20:33Z
crates         serde                        1.0.219            11  2026-09-08T21:04:11Z
```

Celle-ci et `verdicts pullers` exigent `audit:read`, et toutes deux ne
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
