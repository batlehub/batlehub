---
sourcePath: registries/generic.md
sourceHash: fe649cef9d1f85ef
---

# Miroir générique

Un miroir en proxy seul, adressé par chemin, de n'importe quelle arborescence de
fichiers en HTTP — pour les amonts qui n'ont aucun protocole de paquets :
archives de chaînes d'outils (`nodejs.org/dist`, `static.rust-lang.org`,
`dl.google.com/go`) et CDN d'éditeurs à binaire unique (`get.helm.sh`,
`binaries.sonarsource.com`). Chaque requête diffuse `{upstream}/{path}` et le met en cache au
premier défaut. Il n'y a ni publication, ni index, ni modèle de signature.

## En un coup d'œil

| Propriété | Valeur |
|---|---|
| **Type de configuration** | `generic` |
| **Amont par défaut** | aucun — déclarez `upstreams` explicitement (obligatoire) |
| **Modes** | proxy seul |
| **Adressage** | par chemin |
| **Publication privée** | proxy seul |
| **Coupure réseau** | aucun index à composer ; un fichier détenu est servi par chemin |

## Mise en place du proxy {#proxy-setup}

`upstreams` **et** une liste d'autorisation `path_allow` sont obligatoires — sans
elle, un miroir d'un hôte partagé relaierait tous les chemins sans rapport qui
s'y trouvent. Votre administrateur les définit dans la configuration du
registre :

```toml
[[registries]]
name       = "node-dist"
type       = "generic"
mode       = "proxy"
upstreams  = ["https://nodejs.org/dist"]   # obligatoire — il n'existe pas de défaut
path_allow = ["v*/**"]                      # obligatoire — ["**"] autorise tout
```

Les motifs de `path_allow` sont des globs où `*` reste dans un seul segment de
chemin et où seul `**` traverse les `/` : `v*/**` admet
`v24.18.0/node-v24.18.0-linux-x64.tar.gz`, et `go*` admet
`go1.25.4.linux-amd64.tar.gz` mais pas `go1.25/sub/file`.

::: warning Changé par la phase 2 de la RFC 0047
Avant elle, `*` traversait aussi les `/`, si bien que `go*` admettait tout chemin
commençant par `go`. Un `path_allow` qui s'appuyait là-dessus refuse désormais
ces chemins par un `403` ; écrivez `**` là où un séparateur doit être franchi :

```toml
path_allow = ["go*"]     # avant : admettait aussi go1.25/sub/file
path_allow = ["go*/**"]  # après : les mêmes chemins, dits explicitement
```

Le changement échoue fermé : rien n'est admis qui était refusé auparavant. Il
ne touche que `generic` ; `deb`, `rpm`, `pacman` et `jetbrains` gardent un `*`
qui traverse les `/`.
:::

Le chemin qui suit `/generic/` correspond un pour un à l'amont configuré.
Remplacez `<registry>` par le nom de registre que vous avez configuré :

```bash
REG="https://batlehub.example.com/proxy/<registry>/generic"

# nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz
#   → $REG/v24.18.0/node-v24.18.0-linux-x64.tar.gz
curl -fL -o node.tar.gz $REG/v24.18.0/node-v24.18.0-linux-x64.tar.gz
```

La plupart des chaînes d'outils exposent une variable d'environnement de miroir
que vous faites pointer vers la racine du registre
(`…/proxy/<registry>/generic`) :

```sh
export NODEJS_ORG_MIRROR=https://batlehub.example.com/proxy/node-dist/generic
export RUSTUP_DIST_SERVER=https://batlehub.example.com/proxy/rust-dist/generic
```

Des outils comme `mise` les lisent automatiquement, et savent aussi router leurs
téléchargements directs par un bloc `[settings.url_replacements]`.
`batlehub-cli registry suggest` analyse un projet (`mise.toml` et `mise.lock`
compris) et imprime à la fois les blocs de configuration de registre et les
variables d'environnement client correspondantes.

## Préréglages

`batlehub-cli registry suggest --mise` écrit ces registres quand `mise.lock`
enregistre une URL sur leur hôte. Chaque `path_allow` commence par un segment de
produit littéral, si bien qu'un préréglage ne relaie jamais l'hôte entier ;
`revalidate` énumère les documents que mise relit pour y chercher du nouveau
([RFC 0047](/rfc/0047-mise-backends-coverage) §6.1).

| Nom | Amont | Sert |
| --- | --- | --- |
| `hashicorp-releases` | `https://releases.hashicorp.com` | boundary, consul, nomad, nomad-pack, packer, sentinel, terraform, terraform-ls, tfc-agent, vault, waypoint |
| `hashicorp-api` | `https://api.releases.hashicorp.com/v1/releases` | les dates de publication de nomad-pack, sentinel et tfc-agent — la première page de dix seulement, puisque la chaîne de requête n'est pas transmise à l'amont |
| `k8s-dl` | `https://dl.k8s.io` | kubectl, kubectl-convert, `release/stable.txt` |
| `apache-archive` | `https://archive.apache.org/dist` | ant, groovy, maven, spark, tomcat |
| `openshift-mirror` | `https://mirror.openshift.com/pub/openshift-v4/clients` | oc, openshift-install |
| `julia-dist` | `https://julialang-s3.julialang.org/bin` | julia et son `versions.json` |
| `docker-dl` | `https://download.docker.com` | le client docker statique pour Linux, macOS et Windows |
| `android-dl` | `https://dl.google.com/android` | les outils en ligne de commande d'android-sdk, aapt2, android-cli ; `dl.google.com/go/` relève du type [`godist`](/fr/registries/godist) |
| `deno-dl`, `zig-dl`, `hex-builds`, `swift-dl`, `swift-api`, `pypy-dl` | — | les chaînes d'outils `core:` de [mise](/fr/use/mise#core-toolchains) |
| `helm-bin`, `sonar-binaries` | `https://get.helm.sh`, `https://binaries.sonarsource.com` | helm ; sonar-scanner-cli et sonarqube-cli |

Un listage de répertoire — une URL terminée par `/`, comme l'index `ocp/` où oc
liste ses versions, ou `ant/binaries/` — est refusé par un `400` sur tout
registre `generic` : une installation qui doit d'abord lister les versions a
besoin de `mise.lock`. Sur un hôte de buckets (`storage.googleapis.com`,
`s3.amazonaws.com`) sans préréglage, le miroir suggéré admet le répertoire du
fichier verrouillé et la liste de versions que mise lit dans le même bucket.

## Authentification

Ajoutez `-H "Authorization: Bearer $BATLEHUB_TOKEN"` quand le registre exige une
authentification. Pour les outils pilotés par variables d'environnement, ajoutez
une entrée `~/.netrc` pour l'hôte du proxy — mise et tout ce qui est bâti sur
libcurl la lisent automatiquement :

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Embarquer des identifiants HTTP Basic dans l'URL du miroir fonctionne en repli,
mais le token vit alors dans une variable d'environnement qui fuit dans
l'historique du shell, les logs de CI, la liste des processus et les
diagnostics du genre `mise doctor` — préférez `~/.netrc`.

## Notes

- Un miroir `generic` d'un CDN Alpine pose le même problème, un arbre plus loin :
  le nom de fichier d'un `.apk` porte un nom et une version réels, et un registre
  `generic` n'a nulle part où les mettre. Utilisez [`apk`](/fr/registries/apk)
  pour bloquer, appliquer un délai de fraîcheur ou compter un paquet Alpine ;
  `generic` reste le bon choix pour un arbre Alpine que vous voulez seulement
  mettre en cache.
- Un miroir `generic` de `nodejs.org/dist` met Node en cache correctement et ne
  peut rien lui appliquer : un registre adressé par chemin n'a qu'un paquet
  synthétique et aucune version à bloquer. Pour appliquer une politique à une
  publication de Node — un blocage qui atteint `nvm ls-remote`, un garde-fou
  d'âge sur une version publiée hier — utilisez plutôt
  [`nodedist`](/fr/registries/nodedist) ; `generic` reste la bonne réponse pour
  une arborescence que vous voulez mettre en cache sans politique.
- Un miroir `generic` d'un cache binaire Nix reprend la même histoire une
  troisième fois, en pire : chaque chemin est un paquet synthétique, une CVE
  dans un chemin du store est donc invisible depuis la console et
  `nix copy --to` n'a nulle part où aller. C'est tout un parc — le compilateur
  compris — qui tire sa clôture par ce trou. Utilisez [`nix`](/fr/registries/nix)
  pour bloquer un chemin du store selon le paquet et la version que Nix lui-même
  en extrait ; `generic` reste la bonne réponse pour `channels.nixos.org`, qui
  est un autre hôte et un autre protocole.
- Une requête vers un chemin en dehors de la liste `path_allow` du registre
  renvoie `403`, pas 404 — c'est la liste d'autorisation qui la rejette
  localement, avant toute requête amont. Élargissez les motifs si
  `mise install` signale un 403.
- Les archives répliquées sont souvent volumineuses ; réglez
  `max_artifact_size_bytes` sur ce registre pour des archives de chaînes
  d'outils au-delà des 500 Mio par défaut de l'instance. Un téléchargement à
  froid est diffusé au client pendant sa mise en cache, et une nouvelle
  tentative rejoint la récupération en cours au lieu d'en lancer une autre
  ([Cache](/fr/guide/caching#cold-fill)).
- Une arborescence immuable à quelques documents près — un
  `release/stable.txt`, un `index.json` qui liste les versions — les nomme
  dans `revalidate`. Ils sont redemandés à chaque lecture, et servis périmés si
  l'amont est en panne ; tout autre chemin reste en cache pour toujours
  ([Cache](/fr/guide/caching#revalidated-paths)).
- Les registres adressés par chemin se préchauffent sur des **chemins**
  précis, par `[registries.cache] warm_paths`, et non par `warm_packages`.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Helm](/fr/registries/helm) — l'`index.yaml` d'un dépôt de charts mis en miroir ici est mis en cache mais ne peut pas être bloqué, et ses URL de chart absolues contournent le cache ; le type `helm` le réécrit et le filtre
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
