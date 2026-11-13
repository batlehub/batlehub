---
sourcePath: registries/godist.md
sourceHash: 24c93bc798625458
---

# Chaîne d'outils Go (mise)

Fait proxy et cache de la distribution de la chaîne d'outils Go
(`dl.google.com/go`) comme registre *typé*, de sorte qu'une publication de Go
puisse être **bloquée** et pas seulement mise en cache. Chaque archive est
contrôlée contre le document de publication de Go lui-même
(`go.dev/dl/?mode=json&include=all`) et vérifiée contre son `sha256` avant
d'être mise en cache ; le `.sha256` posé à côté de chaque archive est répondu à
partir de ce document. Lu par le `core:go` de mise. Les *modules* Go relèvent
d'un autre protocole : ils passent par un registre
[proxy de modules Go](/fr/registries/goproxy).

## En un coup d'œil

| Propriété | Valeur |
|---|---|
| **Type de configuration** | `godist` |
| **Amont par défaut** | `dl.google.com/go` pour les archives, `go.dev/dl/?mode=json&include=all` (`metadata_url`) pour le document de publication |
| **Modes** | proxy seul |
| **Adressage** | un paquet (`go`), une version par publication (`1.25.4`, sans le préfixe `go`), une archive par plateforme |
| **Publication privée** | proxy seul |
| **Coupure réseau** | archives détenues et leur `.sha256` servis par chemin ; `index.json` et la liste de refs git composés à partir des archives détenues. mise lit sa liste de versions sur son hôte de versions ou dans cette liste de refs, donc une installation coupée du réseau se fait à partir du fichier de verrouillage |

## Mise en place du proxy

mise télécharge `{go.download_mirror}/go{version}.{os}-{arch}.tar.gz`, puis
`{cette URL}.sha256`, dont il lit le corps comme l'empreinte entière. Une seule
règle de réécriture envoie les deux ici. Remplacez `<registry>` par le nom de
registre que vous avez configuré :

```toml
# ~/.config/mise/config.toml
[settings.url_replacements]
"regex:^https://dl\\.google\\.com/go/(.+)" = "https://batlehub.example.com/proxy/<registry>/godist/$1"
```

Définir `MISE_GO_DOWNLOAD_MIRROR=https://batlehub.example.com/proxy/<registry>/godist`
fait la même chose pour un seul shell.

mise lit sa liste de versions de Go sur son hôte de versions (`data/go.toml`,
que sert un registre [hôte de versions de mise](/fr/registries/mise-versions)),
ou — quand il n'utilise pas l'hôte de versions (`MISE_USE_VERSIONS_HOST=false`) —
par `git ls-remote --tags` sur `go.repo`, `https://github.com/golang/go` par
défaut. Ce registre répond aussi à cette seconde voie : faites pointer `go.repo`
vers lui, et les tags viennent du document de publication de Go, publications
bloquées retirées.

```toml
# ~/.config/mise/config.toml
[settings.go]
repo = "https://batlehub.example.com/proxy/<registry>/godist"
```

ou `MISE_GO_REPO=https://batlehub.example.com/proxy/<registry>/godist` pour un
seul shell. `batlehub-cli setup apply --only mise` écrit ce réglage et la règle
de réécriture ci-dessus. Faites passer l'une des deux listes par l'instance,
sans quoi `mise install go@1.25` se résout contre la liste publique.

Le « dépôt » n'est qu'une liste de refs : `GET …/godist/info/refs` répond sous
la forme HTTP *dumb* de git (`text/plain`, une ligne
`<id>\trefs/tags/go{version}` par publication), que `git ls-remote` accepte
quelle que soit la version de protocole demandée. Les identifiants ne sont pas
des commits — chacun est le SHA-1 du nom de sa ref, si bien que la liste est
stable — et un clone ou un fetch ne trouve aucun objet à télécharger.

## Authentification

mise envoie l'entrée `~/.netrc` de l'hôte vers lequel une règle a réécrit
l'URL ; une instance authentifiée demande donc une entrée pour l'hôte du proxy :

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Le bloc de registre de votre administrateur :

```toml
[[registries]]
name      = "go-toolchain"
type      = "godist"
mode      = "proxy"                        # le seul mode : pas de protocole de publication
upstreams = ["https://dl.google.com/go"]  # la valeur par défaut : archives et .sha256
# metadata_url = "https://go.dev/dl/?mode=json&include=all"  # la valeur par défaut

[registries.rbac]
# index.json et info/refs sont des listings (`releases:list`) ; les archives sont des lectures (`releases:read`).
anonymous = ["releases:read", "releases:list"]
```

## Versions bloquées

Un blocage s'écrit sur `go@1.25.4` — la version telle que mise l'écrit ; le
`go1.25.4` du document y correspond aussi. La version est lue dans le nom de
fichier, si bien qu'une publication bloquée est refusée par un `403` sur son
archive **et** sur son `.sha256`, quelle que soit l'URL construite par le client
et quoi qu'épingle un `mise.lock`. `index.json` et la liste de refs git
retirent la publication : `mise ls-remote go` ne la propose plus quand mise liste
par `go.repo`, et un registre [hôte de versions de mise](/fr/registries/mise-versions)
la retire de la même façon. Par la liste publique, le refus est celui du
téléchargement, et mise le signale comme un téléchargement échoué.

Un fichier que le document de publication ne liste pas — une autre version,
une autre plateforme, une faute de frappe — reçoit un `404` sans requête vers
l'amont, si bien qu'un client ne peut pas faire récupérer un chemin arbitraire
à ce registre.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
et [quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered).

## Notes

- **Pas de garde-fou d'âge.** Le document de publication de Go ne porte aucune
  date de publication : un `release_age_gate` n'aurait ici rien à lire et
  laisserait passer toutes les versions. Le serveur en refuse un sur un registre
  `godist` au chargement plutôt que de le laisser passer pour une quarantaine.
- **Les empreintes viennent d'un autre hôte.** L'empreinte est celle de
  `go.dev`, les octets ceux de `dl.google.com`, et le cache vérifie l'une contre
  les autres avant de stocker quoi que ce soit. mise vérifie l'archive contre le
  `.sha256` que répond ce registre, qui est la même empreinte — un `mise.lock`
  versionné avec sa propre empreinte reste donc la défense de bout en bout.
- **Migrer depuis un miroir générique `go-dl`.** Changez `type`, retirez
  `path_allow`, et pointez la règle vers `…/godist/` au lieu de `…/generic/`.
  Les artefacts en cache ne suivent pas — `generic` stocke sous
  `{registry}/repo/_/{path}`, `godist` sous `{registry}/go/{version}/{file}` —
  le coût est donc une récupération à froid par publication encore utilisée.

## Endpoints

::: details Afficher tous les endpoints

<!-- BEGIN endpoints: proxy/godist -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/godist/{file}` | One archive of one Go release, or its `.sha256`. |
| `GET` | `/proxy/{registry}/godist/index.json` | Go's release document, blocked releases removed. |
| `GET` | `/proxy/{registry}/godist/info/refs` | Go's releases as git tags, for `git ls-remote` (mise's `go.repo`). |
<!-- END endpoints -->

:::

## Voir aussi

- [Distributions Node](/fr/registries/nodedist) — la même conception, une chaîne d'outils plus loin
- [Proxy de modules Go](/fr/registries/goproxy) — les *modules* Go, pas la chaîne d'outils
- [Hôte de versions de mise](/fr/registries/mise-versions) — là où mise lit sa liste de versions de Go
- [Miroir générique](/fr/registries/generic) — les mêmes fichiers, en cache sans politique
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
