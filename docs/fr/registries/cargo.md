---
sourcePath: registries/cargo.md
sourceHash: e367b1d384402bc4
---

# Cargo

Fait proxy et cache de crates.io, ou héberge des crates privées. BatleHub
implémente le [protocole de registre sparse](https://doc.rust-lang.org/cargo/reference/registry-protocols.html#sparse-protocol)
de Cargo, et sert l'index comme les téléchargements de `.crate` à travers le
cache. Les sommes de contrôle de l'index correspondent aux fichiers `.crate` mis
en cache : la vérification continue donc de fonctionner.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `cargo` |
| **Amont par défaut** | `crates.io` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `cargo publish` |
| **Coupure réseau** | hors ligne, l'index sparse est composé à partir des crates détenues, leurs dépendances étant lues dans le manifeste de chaque crate à l'import ; une crate importée sans son manifeste n'est pas listée |

## Mise en place du proxy

Remplacez la source crates.io par défaut pour que tous les `cargo add` et
`cargo build` passent par BatleHub :

```toml
# .cargo/config.toml (projet) ou ~/.cargo/config.toml (global)
[source.crates-io]
replace-with = "batlehub"

[source.batlehub]
registry = "sparse+https://batlehub.example.com/proxy/<registry>/registry/"
```

L'URL `index` / `registry` porte le préfixe `sparse+` et doit se terminer par
`/registry/`.

## Publication (local / hybrid) {#publishing-local-hybrid}

### Configuration du serveur

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"          # ou "hybrid" pour se rabattre sur crates.io

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez :
```toml
upstreams = ["https://static.crates.io/crates"]
index_url = "https://index.crates.io"
```

### Mise en place côté client

Modifiez `~/.cargo/config.toml` ou le `.cargo/config.toml` à la racine du
projet :

```toml
[registries.internal]
index = "sparse+https://batlehub.example.com/proxy/internal/registry/"
token = "<your-token>"
```

Vous pouvez aussi exporter le token dans une variable d'environnement (pratique
en CI) :

```sh
export CARGO_REGISTRIES_INTERNAL_TOKEN=<your-token>
```

### Publier

```sh
cargo publish --registry internal
```

Cargo sérialise les métadonnées de la crate et l'archive `.crate` en une seule
charge binaire, envoyée à `PUT /proxy/internal/api/v1/crates/new`. La somme de
contrôle est vérifiée côté serveur.

### Dépendre d'une crate publiée en privé

```toml
# Cargo.toml
[dependencies]
my-lib = { version = "0.1", registry = "internal" }
```

### Retirer une version, ou annuler le retrait

```sh
cargo yank --registry internal my-lib@0.1.0
cargo yank --undo --registry internal my-lib@0.1.0
```

### Vérifier

```sh
cargo add my-lib --registry internal
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/cargo -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{name}/{version}/download` | Download a `.crate` file for a specific version. |
| `GET` | `/proxy/{registry}/api/v1/crates` | `cargo search`. |
| `PUT` | `/proxy/{registry}/api/v1/crates/{name}/{version}/unyank` | Unyank a previously yanked crate version. |
| `DELETE` | `/proxy/{registry}/api/v1/crates/{name}/{version}/yank` | Yank a published crate version. |
| `GET` | `/proxy/{registry}/api/v1/crates/{name}/owners` | List owners of a crate (`cargo owner --list`). |
| `PUT` | `/proxy/{registry}/api/v1/crates/{name}/owners` | `cargo owner --add`. |
| `DELETE` | `/proxy/{registry}/api/v1/crates/{name}/owners` | `cargo owner --remove`. |
| `PUT` | `/proxy/{registry}/api/v1/crates/new` | Publish a new crate version (`cargo publish`). |
| `GET` | `/proxy/{registry}/registry/{path}` | Cargo sparse registry index entries. |
| `GET` | `/proxy/{registry}/registry/config.json` | Cargo sparse registry `config.json`. |
<!-- END endpoints -->

---

## Versions bloquées

Cargo est le seul écosystème où une version bloquée est **marquée plutôt que
retirée**. La ligne de l'index sparse reste, avec `"yanked": true` posé dessus.

C'est le mécanisme propre de cargo pour dire « ceci existe, ne le choisis pas » :
la résolution saute une version retirée, tandis qu'un `Cargo.lock` existant qui
l'épingle déjà se résout encore — et rencontre alors le garde-fou du
téléchargement, qui répond avec le motif de l'opérateur. Supprimer la ligne
ferait au contraire dire à cargo que la crate n'a jamais eu cette version, et le
développeur recevrait « no matching package found » au lieu d'une explication.

La route de l'index sparse est autorisée comme toute autre lecture par le proxy :
un client sans accès en lecture au registre reçoit un `403`, pas la liste des
crates.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Cargo envoie le `token` du bloc `[registries.<name>]`. En CI, définissez-le
plutôt par l'environnement :
`export CARGO_REGISTRIES_INTERNAL_TOKEN=$BATLEHUB_TOKEN` (le nom du registre en
majuscules).

## Notes

Si `cargo publish` échoue avec « invalid token », vérifiez que l'URL `index` se
termine bien par `/registry/`. Les sommes de contrôle renvoyées par l'index
sparse correspondent aux fichiers `.crate` en cache, donc `cargo verify-project`
continue de fonctionner.

## Recherche

``cargo search`` est traité en trois étapes : un résultat en cache pour cette
requête, puis l'amont, puis — quand l'amont est injoignable — **les paquets que
ce registre détient déjà**. Une panne dégrade la recherche vers ce dont BatleHub
peut honnêtement répondre, plutôt que vers une erreur ou une liste vide.

Chaque réponse porte `X-BatleHub-Cache: hit | miss | stale`. `stale` signifie que
l'amont n'a pas pu être joint et que la réponse vient du cache ou de l'ensemble
détenu : une liste courte n'est donc jamais présentée silencieusement comme
complète.

::: warning Les requêtes de recherche atteignent l'amont
La deuxième étape transmet la chaîne de requête à l'amont configuré. Les termes
de recherche sont un relevé de ce que votre organisation cherche. Mettez
`serve_stale = false` et laissez le registre sans amont si vous voulez la réponse
des paquets détenus et aucune sortie réseau.
:::

Les versions bloquées sont retirées des résultats, et le total annoncé est ajusté
en conséquence — les clients paginent par décalage, donc une page silencieusement
raccourcie ferait sauter un résultat à la suivante.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
