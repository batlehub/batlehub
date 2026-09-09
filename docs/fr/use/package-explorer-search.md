---
sourcePath: use/package-explorer-search.md
sourceHash: 9c864067d6a7651d
---

# Explorateur de paquets — la recherche

Trois recherches vivent derrière un seul champ, et chacune répond à une question
différente.

| Recherche | Répond à | Où |
| --- | --- | --- |
| Les noms, ici | *avons-nous quelque chose qui s'appelle `retry`* | toujours active |
| **La prose des README, ici** | *laquelle de nos bibliothèques gère la temporisation exponentielle* | sur activation — ci-dessous |
| Les noms, en amont | *est-ce que ça existe seulement, et devrions-nous le récupérer* | [ci-dessous](#upstream-search) |

## Chercher ce qu'un paquet dit {#readme-search}

Une recherche par nom ne peut pas répondre à *« laquelle de nos bibliothèques
internes gère la temporisation exponentielle »*. C'est pourtant la question avec
laquelle un développeur arrive, et pour un paquet interne c'est une question à
laquelle la page du paquet est le seul endroit au monde à pouvoir répondre — il n'y a pas
de npmjs.com à aller lire à la place.

Avec `[search] readmes = true`
([configuration](/fr/guide/admin-config#search-readmes)), l'endpoint de listing
accepte un périmètre :

```
GET /api/v1/explore/packages?q=exponential+backoff&in=name|readme|both
```

`in` vaut `name` par défaut, c'est-à-dire le comportement livré depuis toujours.
Chaque résultat gagne deux champs :

| Champ | Signification |
| --- | --- |
| `matched_in` | `name` \| `readme` \| `both` — pourquoi cette ligne est là |
| `snippet` | Le fragment de README correspondant, en texte brut, ou `null` |

**Une correspondance de nom prime toujours sur une correspondance de prose.** Un
paquet littéralement nommé `retry` passe devant un paquet qui parle de réessayer,
si dense soit-il. C'est ce que veut dire un lecteur qui saisit un nom ; ce n'est
pas un paramètre de réglage.

`matched_in` est là parce qu'un résultat qui ne correspond à rien de visible pour
le lecteur se lit comme un bug. Une ligne dont le nom n'a rien à voir avec la
requête et dont le README la mentionne en passant est un résultat *correct*, et
un résultat inexplicable sans cette étiquette.

**Seuls les README stockés sont interrogeables** — c'est-à-dire les seules
versions que cette instance détient ou héberge. Un README dérivé à la volée pour
une version dont l'instance n'a aucun octet n'a pas de ligne à indexer, et en
écrire une est précisément ce que la lecture de découverte refuse de faire.
L'état vide le dit, plutôt que de laisser croire que la requête n'a rien trouvé.

Quand la fonctionnalité est désactivée, `in=readme` et `in=both` sont acceptés
et répondent
exactement comme `in=name`, et la réponse porte `readme_search_enabled: false` —
un client peut ainsi distinguer *« aucun paquet ici ne dit cela »* de *« cette
instance ne cherche pas dans la prose »*.

## Recherche en amont {#upstream-search}

Quand vous saisissez une requête (≥ 2 caractères), l'explorateur interroge aussi
les registres amont pour faire apparaître des paquets que vous n'avez pas encore
fait passer par BatleHub. Les résultats sont ajoutés en bas de la table
principale, avec un badge **Pas encore passé par le proxy** dans la colonne
Proxy.

### Registres pris en charge {#upstream-supported}

| Type de registre | Endpoint de recherche par défaut | Notes |
| --- | --- | --- |
| `npm` | `{upstream}/-/v1/search` | Recherche plein texte |
| `openvsx` | `{upstream}/api/-/search` | Recherche plein texte ; les résultats utilisent le format `publisher.name` |
| `cargo` | `{upstream}/api/v1/crates` | Recherche plein texte |
| `rubygems` | `{upstream}/api/v1/search.json` | Recherche plein texte |
| `composer` | `https://packagist.org/search.json` | Recherche plein texte ; le champ de version vaut `"latest"` (Packagist ne le renvoie pas dans les résultats) |
| `maven` | `https://search.maven.org/solrsearch/select` | Recherche plein texte Solr sur Maven Central |
| `terraform` | `{upstream}/v1/modules/search` (modules) + recherche par namespace ou par paire exacte pour les providers | Le protocole du registre Terraform n'a pas de recherche plein texte de providers. Voir la note plus bas. |
| `pypi` | `{upstream}/pypi/{name}/json` | Recherche par nom exact uniquement (PyPI a supprimé son API de recherche publique) |
| `nuget` | `{upstream}/v3/query` | Service de recherche NuGet v3 ; recherche plein texte |
| `goproxy` | `https://pkg.go.dev/search` | Le protocole GOPROXY n'a pas d'endpoint de recherche : BatleHub interroge donc pkg.go.dev (en HTML). La version vaut `"latest"` ; configurable ou désactivable par `search_url`. |

Les autres types de registre **n'ont aucune API de recherche amont** :
l'explorateur n'y montre donc que les paquets en cache et publiés localement (pas
de lignes « Pas encore passé par le proxy ») — `github`, `forgejo`, `gitlab`
(proxys de releases : cherchez un dépôt directement par `owner/repo`),
`vscode-marketplace`, `conda`, et les formats de dépôt adressés par chemin `deb`
et `rpm`.

> **Limite de la recherche de providers Terraform**
>
> Le protocole v1 du registre Terraform n'a pas d'endpoint de recherche plein
> texte pour les providers. BatleHub contourne cela par deux stratégies de repli :
>
> - **Recherche par namespace** — la requête est traitée comme un namespace de
>   provider. Chercher `netbirdio` renvoie tous les providers publiés sous cette
>   organisation (par exemple `providers/NetBirdIO/netbird`).
> - **Recherche par paire exacte** — si la requête contient un `/`, elle est
>   traitée comme `namespace/type` et résolue directement (par exemple
>   `netbirdio/netbird`). La recherche est insensible à la casse.
>
> La recherche de modules tourne toujours en parallèle, en plein texte.

Les échecs de recherche amont sont ignorés silencieusement : si l'API de recherche
d'un registre est injoignable, les résultats en cache n'en sont pas affectés.

### Configurer l'URL de recherche {#search-url-config}

Pour `maven`, `composer` et `goproxy`, le service de recherche vit sur un hôte
différent du dépôt (respectivement le Solr de Maven Central, Packagist et
pkg.go.dev). BatleHub emploie les valeurs publiques par défaut ci-dessus, mais
`search_url` permet de les remplacer ou de les désactiver registre par registre :

```toml
# Utiliser une instance Nexus privée à la fois pour le proxy et pour la recherche
[[registries]]
type      = "maven"
name      = "nexus"
upstreams = ["https://nexus.internal/repository/maven-public"]
search_url = "https://nexus.internal/solrsearch"

# Utiliser un serveur Satis privé — l'endpoint de recherche est sur le même hôte
[[registries]]
type      = "composer"
name      = "satis"
upstreams = ["https://satis.internal"]
search_url = "https://satis.internal"

# Pointer la recherche Go vers un site privé compatible pkg.go.dev (défaut : https://pkg.go.dev)
[[registries]]
type      = "goproxy"
name      = "go"
search_url = "https://pkgsite.internal"

# Désactiver entièrement la recherche amont pour un registre sensible
[[registries]]
type      = "cargo"
name      = "internal-cargo"
upstreams = ["https://cargo.internal"]
search_url = ""
```

| Valeur | Comportement |
| --- | --- |
| Absente (défaut) | Utiliser l'endpoint de recherche intégré au type de registre |
| `"https://..."` | Utiliser cette URL de base pour la recherche |
| `""` (chaîne vide) | Désactiver la recherche amont pour ce registre |
