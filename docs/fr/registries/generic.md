---
sourcePath: registries/generic.md
sourceHash: 55918216a2661f4c
---

# Miroir générique

Un miroir en proxy seul, adressé par chemin, de n'importe quelle arborescence de
fichiers en HTTP — pour les amonts qui n'ont aucun protocole de paquets :
archives de chaînes d'outils (`nodejs.org/dist`, `static.rust-lang.org`,
`dl.google.com/go`) et CDN d'éditeurs à binaire unique (`get.helm.sh`,
`dl.min.io`). Chaque requête diffuse `{upstream}/{path}` et le met en cache au
premier défaut. Il n'y a ni publication, ni index, ni modèle de signature.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `generic` |
| **Amont par défaut** | aucun — déclarez `upstreams` explicitement (obligatoire) |
| **Modes** | proxy seul |
| **Adressage** | par chemin |
| **Publication privée** | ❌ proxy seul |
| **Coupure réseau** | aucun index à composer ; un fichier détenu est servi par chemin |

## Mise en place du proxy

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
`batlehub registry suggest` analyse un projet (`mise.toml` et `mise.lock`
compris) et imprime à la fois les blocs de configuration de registre et les
variables d'environnement client correspondantes.

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

- Un miroir `generic` de `nodejs.org/dist` met Node en cache correctement et ne
  peut rien lui appliquer : un registre adressé par chemin n'a qu'un paquet
  synthétique et aucune version à bloquer. Pour appliquer une politique à une
  publication de Node — un blocage qui atteint `nvm ls-remote`, un garde-fou
  d'âge sur une version publiée hier — utilisez plutôt
  [`nodedist`](/fr/registries/nodedist) ; `generic` reste la bonne réponse pour
  une arborescence que vous voulez mettre en cache sans politique.
- Une requête vers un chemin en dehors de la liste `path_allow` du registre
  renvoie `403`, pas 404 — c'est la liste d'autorisation qui la rejette
  localement, avant toute requête amont. Élargissez les motifs si
  `mise install` signale un 403.
- Les archives répliquées sont souvent volumineuses ; le proxy met tout
  l'artefact en mémoire tampon avant de le mettre en cache : relevez donc
  `limits.max_artifact_size_bytes` (500 Mio par défaut) pour des archives de
  chaînes d'outils.
- Les registres adressés par chemin se préchauffent sur des **chemins**
  précis, par `[registries.cache] warm_paths`, et non par `warm_packages`.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
