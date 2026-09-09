---
sourcePath: registries/nodedist.md
sourceHash: d16b13a332915f3f
---

# Distributions Node (nvm, fnm, n, mise)

Fait proxy et cache de l'arborescence `nodejs.org/dist` comme registre *typé*,
de sorte qu'une publication de Node puisse être **bloquée** et pas seulement mise
en cache. `index.tab` et `index.json` sont des listings filtrés ; les archives,
`SHASUMS256.txt` et ses signatures détachées sont servis octet pour octet sous
`{version}/{file}`. La même arborescence est lue par nvm, fnm, `n`, volta et
mise, ce pourquoi le type est nommé d'après le protocole et non d'après l'un
d'eux.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `nodedist` |
| **Amont par défaut** | `nodejs.org/dist` (io.js demande un second registre pointé vers `iojs.org/dist`) |
| **Modes** | proxy seul |
| **Adressage** | un paquet (`node`), une version par publication, un fichier par plateforme |
| **Publication privée** | ❌ proxy seul |
| **Coupure réseau** | hors ligne, `index.tab` et `index.json` sont composés à partir des fichiers de distribution détenus, datés à leur réception et avec `lts` inconnu |

## Mise en place du proxy

Chaque gestionnaire lit sa propre variable de miroir. Exportez-la avant de
sourcer `nvm.sh` — dans `/etc/profile.d`, dans un `Containerfile`, ou dans le
bloc `env:` d'un job de CI. Remplacez `<registry>` par le nom de registre que
vous avez configuré :

```sh
export NVM_NODEJS_ORG_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"   # nvm
export FNM_NODE_DIST_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"    # fnm
export N_NODE_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"           # n
export NODEJS_ORG_MIRROR="https://batlehub.example.com/proxy/<registry>/nodedist"       # mise

nvm ls-remote        # lit index.tab à travers le proxy
nvm install 22.11.0  # SHASUMS256.txt et l'archive, mis en cache sous node/v22.11.0/
```

Le bloc de registre côté administrateur :

```toml
[[registries]]
name      = "node"
type      = "nodedist"
mode      = "proxy"                       # le seul mode : pas de protocole de publication
upstreams = ["https://nodejs.org/dist"]  # le défaut ; io.js prend son propre bloc

[registries.rbac]
# index.tab et index.json sont des listings (`releases:list`) ; les fichiers sont
# des lectures (`releases:read`). Une installation exige les deux.
anonymous = ["releases:read", "releases:list"]
```

`path_allow` est refusé ici : le type est typé, pas adressé par chemin. Un
opérateur qui veut un miroir Node sans aucune politique garde le
[miroir générique](/fr/registries/generic) ; `nodedist` existe pour le blocage.

## Versions bloquées

nvm résout **toutes** ses installations par `index.tab` — y compris un
`nvm install 22.11.0` entièrement spécifié — et imprime son propre *« Version
'22.11.0' not found »* quand la ligne est absente. Une publication bloquée est
retirée d'`index.tab` (en-tête conservé : nvm supprime la ligne 1 sans condition,
donc un en-tête perdu mangerait la publication la plus récente) et d'`index.json`,
la même table que lisent fnm et mise. Aucun téléchargement n'est tenté, et rien
sous le répertoire de la publication n'est demandé.

Les alias `lts/*` que nvm dérive de la colonne `lts` bougent par construction :
retirer la ligne Jod la plus récente fait de la ligne Jod survivante suivante
l'alias. La seule limite honnête : nvm met ces alias en cache dans
`$NVM_DIR/alias/lts/` au dernier `nvm ls-remote`, de sorte qu'une publication
bloquée *après* cela peut encore être demandée par alias — auquel cas la
récupération de l'archive est refusée par un `403` et nvm signale un échec de
téléchargement plutôt qu'un « non trouvé » propre. Le blocage tient ; c'est le
message qui se dégrade.

`SHASUMS256.txt` n'est **jamais réécrit** : nvm vérifie chaque téléchargement
contre lui, et un `.asc` ou `.sig` voisin le signe. Bloquer une publication
retire sa ligne du listing ; cela ne trafique pas ses sommes de contrôle.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
et [quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered).

## Authentification

nvm et fnm construisent leur propre commande `curl` et n'ont nulle part où
placer un en-tête. libcurl lit `~/.netrc` sans qu'on le lui demande : une
instance authentifiée a donc besoin d'une entrée pour l'hôte du proxy.

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

## Notes

- **Le garde-fou d'âge fonctionne, avec une décision à prendre.** La date de
  publication est lue dans la deuxième colonne d'`index.tab` : les publications
  courantes portent donc un horodatage, et un `release_age_gate` retient
  réellement une publication d'hier. Une publication que l'index ne liste plus
  atteint le garde-fou sans date ; sur ce type, `deny_missing_timestamp` est
  **obligatoire** — `true` refuse les publications retirées du listing, `false`
  les sert — parce qu'hériter silencieusement du défaut de npm est la façon dont
  un opérateur finit par croire qu'une chaîne d'outils est en quarantaine alors
  qu'elle ne l'est pas. La date est le jour de l'index à 00:00 UTC, donc un
  garde-fou de 24 heures ne retient jamais moins d'une journée.
- **io.js** est un second registre `nodedist` pointé vers
  `https://iojs.org/dist`. Son paquet est `iojs`, décidé par l'URL amont ; son
  `index.tab` a neuf colonnes là où celui de Node en a onze, et les deux sont lus
  par nom d'en-tête.
- **Migrer depuis un miroir `generic`.** Changez `type`, retirez `path_allow`, et
  pointez la variable client vers `…/nodedist` plutôt que `…/generic`. Les
  artefacts en cache ne suivent pas — `generic` stocke sous
  `{registry}/repo/_/{path}`, `nodedist` sous `{registry}/node/{version}/{file}` —
  donc le coût est d'une récupération à froid par publication encore utilisée.
- **Le préchauffage a besoin d'une plateforme.**
  `warm_packages = ["node@v22.11.0"]` préchauffe une archive par entrée de
  `cache.warm_platforms` (`linux-x64`, `darwin-arm64`, …), avec pour défaut la
  plateforme sur laquelle tourne ce serveur.
  `batlehub-cli registry suggest` lit `.nvmrc` et écrit les deux ; un alias
  (`lts/*`) ne nomme aucune publication et ne préchauffe rien.

## Endpoints

<!-- BEGIN endpoints: proxy/nodedist -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/nodedist/{version}/{file}` | One file of one release: a tarball, `SHASUMS256.txt`, or a signature. |
| `GET` | `/proxy/{registry}/nodedist/index.json` | The same release table as JSON — what fnm and mise read. |
| `GET` | `/proxy/{registry}/nodedist/index.tab` | The release table nvm resolves every install through, blocked releases |
<!-- END endpoints -->

## Voir aussi

- [SDKMAN](/fr/registries/sdkman) — l'autre type de chaîne d'outils de la RFC 0010
- [Miroir générique](/fr/registries/generic) — la même arborescence, mise en cache sans politique
- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
