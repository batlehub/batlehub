---
sourcePath: registries/rustup.md
sourceHash: 5dbde37b974b5f8b
---

# Chaîne d'outils Rust (rustup)

La distribution `static.rust-lang.org` est mandatée et mise en cache comme un registre *typé*, afin qu'une chaîne d'outils puisse être **bloquée** plutôt que simplement mise en cache. Les manifestes de canal — `channel-rust-stable.toml`, `channel-rust-beta.toml`, `channel-rust-nightly.toml` et ceux datés sous `{date}/` — constituent la liste filtrée et le point d'application ; les archives de composants par cible et leurs fichiers `.sha256` sont les artefacts.

Il s'agit de la chaîne d'outils, pas des caisses. `cargo` résout ses dépendances auprès d'un registre [`cargo`](./cargo) distinct, et un parc fermé fait tourner les deux.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `rustup` |
| **Amont par défaut** | `static.rust-lang.org` |
| **Modes** | proxy seul |
| **Adressage** | un paquet par canal, une version par publication, un fichier par triplet cible |
| **Publication privée** | ❌ proxy seul |
| **Commutateur client** | `RUSTUP_DIST_SERVER` (et `RUSTUP_UPDATE_ROOT` pour les mises à jour de rustup lui-même) |

## Mise en place du proxy

`RUSTUP_DIST_SERVER` est le seul commutateur dont rustup a besoin. Exportez-le avant que rustup ne s'exécute — dans `/etc/profile.d`, un `Containerfile`, ou le bloc `env:` d'un travail de CI. Remplacez `<registry>` par le nom de votre registre :

```sh
# Le seul commutateur client. À exporter avant l'exécution de rustup — dans
# /etc/profile.d, un Containerfile, ou le bloc env: d'un travail de CI.
export RUSTUP_DIST_SERVER="https://batlehub.example.com/proxy/<registry>/rustup"
# Nécessaire uniquement si rustup doit aussi se mettre à jour *lui-même* par le proxy.
export RUSTUP_UPDATE_ROOT="https://batlehub.example.com/proxy/<registry>/rustup/rustup"

rustup toolchain install stable --profile minimal
cargo --version
```

Le bloc de registre côté administrateur :

```toml
[[registries]]
name      = "<registry>"
type      = "rustup"
mode      = "proxy"                              # le seul mode : pas de protocole de publication
upstreams = ["https://static.rust-lang.org"]     # la valeur par défaut

[registries.rbac]
# Les manifestes sont des listes ; les archives de composants sont des lectures.
# Une installation a besoin des deux.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list"]
admin     = ["*"]
```

## Authentification

rustup n'a **ni option de jeton, ni fichier d'identifiants, ni prise en charge de `~/.netrc`**. Ce qu'il fait en revanche — mesuré sur le fil plutôt que lu dans sa documentation — c'est envoyer une authentification HTTP Basic depuis l'userinfo de l'URL : l'identifiant se place donc dans la variable elle-même.

```sh
# rustup n'expose aucune option de jeton, ne lit aucun ~/.netrc et n'a pas de
# fichier d'identifiants. Ce qu'il fait — mesuré sur le fil — c'est envoyer une
# authentification HTTP Basic depuis l'userinfo de l'URL : l'identifiant se
# place donc dans la variable elle-même.
export RUSTUP_DIST_SERVER="https://<user>:<token>@batlehub.example.com/proxy/<registry>/rustup"
export RUSTUP_UPDATE_ROOT="https://<user>:<token>@batlehub.example.com/proxy/<registry>/rustup/rustup"
```

Le jeton voyage dans le champ **mot de passe** ; la partie utilisateur n'est pas lue par le serveur et n'est là que parce qu'une URL avec un mot de passe et sans utilisateur n'est pas une URL.

::: warning Une URL qui porte un secret est visible
Elle se retrouve dans l'historique du shell, dans la sortie de `ps` et dans tout journal qui recopie l'environnement. Préférez un secret de CI, ou un fichier de profil en mode `0600` que rien d'autre ne lit.
:::

## Ce que le blocage fait au client

Une chaîne d'outils ou un composant bloqué est **absent du manifeste de canal** : rustup s'arrête donc sur sa propre erreur avant toute tentative de téléchargement — le même échec qu'il donne pour une version que l'amont n'a jamais publiée. Il n'y a pas d'installation partielle à nettoyer.

## La réserve sur les signatures

L'amont publie une signature PGP détachée (`.asc`) à côté de chaque manifeste. BatleHub la relaie octet pour octet et ne resigne jamais : un manifeste *filtré* ne correspond donc plus à la signature qui l'accompagne.

En pratique, la vérification de signature de rustup est désactivée par défaut et avertit au lieu d'échouer, si bien qu'un manifeste filtré s'installe en affichant un avertissement. Un parc qui active la vérification doit accepter soit l'avertissement, soit un manifeste non filtré — les deux ne peuvent pas tenir ensemble, et le même arbitrage est consigné pour les listes du mode air-gap de la RFC 0008-bis.

## Voir aussi

- [`cargo`](./cargo) — les caisses, qui forment un registre distinct
- [RFC 0024](../../rfc/0024-rustup-dist) — pourquoi l'arborescence est un adaptateur typé plutôt qu'un miroir `generic`
