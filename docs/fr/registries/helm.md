---
sourcePath: registries/helm.md
sourceHash: be67779afba05d7e
---

# Helm

Un dépôt de charts Helm classique — celui à `index.yaml` que prend `helm repo add` — est mandaté et mis en cache comme un registre *typé*, afin qu'une version de chart puisse être **bloquée** plutôt que simplement mise en cache. `index.yaml` est le listing filtré et le point d'application ; l'archive `{name}-{version}.tgz`, et le `.prov` à côté d'elle, sont l'artefact. Les modes `local` et `hybrid` acceptent l'API d'envoi de ChartMuseum, l'appel que fait `helm cm-push`.

Ce qui est servi, et comment :

- **`index.yaml`**, sans les versions bloquées et avec **chaque URL de chart réécrite vers cette instance** — relatives, absolues ou sur un autre hôte —, si bien que `helm` ne télécharge rien d'ailleurs.
- **L'archive et son `.prov`**, octet pour octet, récupérés à l'URL que nomme l'index amont. `helm pull --verify` vérifie la signature sur des octets que cette instance n'a jamais modifiés.
- **Les listings `api/charts` de ChartMuseum**, en mode `local` et `hybrid`.

Les registres de charts OCI (`helm pull oci://…`) ne sont pas un dépôt de charts et ne sont pas servis ; voir `oci_entries` plus bas pour les entrées d'index qui en désignent un.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `helm` |
| **Amont par défaut** | aucun — un registre est un dépôt de charts, et c'est vous qui le nommez |
| **Modes** | proxy · local · hybrid |
| **Adressage** | un paquet par chart, une version par entrée d'index, le `.tgz` et son `.prov` comme fichiers de cette version |
| **Publication privée** | ✅ `POST api/charts` de ChartMuseum (`curl`, `helm cm-push`) |
| **Bascule côté client** | `helm repo add` |

## Configuration du proxy

```sh
helm repo add batlehub https://batlehub.example.com/proxy/<registry>/helm \
  --username <user> --password <token>
helm repo update
```

```sh
helm install my-release batlehub/cert-manager --version v1.21.2
```

Le bloc de registre de votre administrateur :

```toml
[[registries]]
name      = "<registry>"
type      = "helm"
mode      = "proxy"
upstreams = ["https://charts.jetstack.io"]   # le répertoire du dépôt, pas …/index.yaml
# oci_entries = "drop"                       # le défaut — voir plus bas

[registries.rbac]
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list", "releases:publish"]
admin     = ["*"]
```

Un registre helm a **exactement un** amont : un second dépôt de charts est un second bloc de registre, ajouté sous un second nom par `helm repo add`. L'amont est le répertoire que prend `helm repo add` ; le chargeur refuse un amont qui se termine par `/index.yaml`, et refuse `hybrid` sans aucun amont.

## Authentification

`helm` envoie `--username`/`--password` en HTTP Basic, sur l'index **et** sur chaque URL de chart du même hôte. Comme toutes les URL de l'index servi sont sur cet hôte, un registre fermé aux lectures anonymes fonctionne sans `--pass-credentials`. Le mot de passe est un jeton BatleHub ; le nom d'utilisateur n'est pas vérifié.

## Blocage

Une version bloquée est absente d'`index.yaml`. L'index décrit le dépôt entier, donc un nouveau blocage l'atteint dans la durée de vie de 30 secondes de l'instantané des blocages. Mais `helm` lit **sa propre copie en cache** de l'index jusqu'au prochain `helm repo update` — d'ici là il propose encore la version, et c'est le téléchargement de l'archive qui la refuse.

Après une mise à jour, un épinglage exact sur une version bloquée échoue avec l'erreur propre à helm, *« chart … version … not found »*, et une plage se résout vers la version autorisée la plus récente.

## `oci_entries`

L'index d'un dépôt de charts peut nommer une version dont `urls[0]` est `oci://…` — le chart vit dans un registre OCI, pas à côté de l'index. Cette instance ne peut pas servir ces octets ; elle décide donc de ce que devient l'entrée :

| `oci_entries` | Effet |
|---|---|
| `drop` (défaut) | L'entrée est omise. Cette version de chart n'est pas installable via ce registre — une entrée que le proxy ne peut pas servir est une entrée que sa politique ne peut pas atteindre. |
| `relay` | L'entrée est servie telle que l'amont l'a écrite, et `helm` la récupère directement depuis le registre OCI, **en contournant cette instance** : sans cache, sans blocage possible, hors de la piste d'audit. Signalé à chaque rechargement, et sur la carte du registre dans la console. |

Le champ est refusé sur tout autre type de registre.

## Publication

Les registres `local` et `hybrid` acceptent un chart empaqueté :

```sh
helm package ./mychart
curl --data-binary @mychart-0.1.0.tgz \
  -H "Authorization: Bearer <token>" \
  https://batlehub.example.com/proxy/<registry>/helm/api/charts
```

`helm cm-push ./mychart batlehub` fait le même appel. Les deux formes de corps de ChartMuseum sont acceptées : l'archive comme corps entier, ou un formulaire multipart avec un champ `chart` et un champ `prov` facultatif portant son fichier de provenance.

Une version en double répond `409`. `?force` la remplace et demande `releases:overwrite`. `DELETE api/charts/{name}/{version}` supprime une version.

L'index que sert un registre local est composé à partir de ce qui a été publié — chaque entrée construite à partir du `Chart.yaml` de l'archive et de son condensé SHA-256 —, il ne peut donc pas nommer un chart qui n'est pas ici.

### Référence des endpoints

<!-- BEGIN endpoints: proxy/helm -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/helm/api/charts` | Every chart published here and its versions — ChartMuseum's listing, paged by chart with `offset`/`limit`. |
| `POST` | `/proxy/{registry}/helm/api/charts` | Publish a packaged chart — ChartMuseum's upload, the call `helm cm-push` makes. |
| `GET` | `/proxy/{registry}/helm/api/charts/{name}` | One chart's versions as ChartMuseum lists them. |
| `GET` | `/proxy/{registry}/helm/api/charts/{name}/{version}` | One chart version as ChartMuseum describes it. |
| `DELETE` | `/proxy/{registry}/helm/api/charts/{name}/{version}` | Delete a published chart version (`releases:delete`); it leaves the next index. |
| `GET` | `/proxy/{registry}/helm/charts/{file}` | A chart archive, or with `.prov` appended its provenance file — byte-exact, fetched from the URL the upstream index names. |
| `GET` | `/proxy/{registry}/helm/index.yaml` | The repository index `helm repo add` and `helm repo update` fetch — blocked versions removed, every chart URL on this host. |
<!-- END endpoints -->

## Mode hybride : le local l'emporte version par version

Sur un registre `hybrid`, l'index servi est celui de l'amont, avec les entrées locales posées par-dessus. Une version de chart publiée ici remplace l'entrée amont pour le **même `{name, version}`** ; les autres versions amont de ce chart restent listées. C'est plus étroit que la règle du nom entier qu'appliquent d'autres types, parce qu'un index de charts est un seul document dont les entrées sont indépendantes.

## Parcs hors ligne

Un registre hors ligne compose `index.yaml` à partir des archives que le lot contient réellement, chaque entrée reconstruite à partir du `Chart.yaml` du chart, et sert l'archive et tout `.prov` depuis le stockage. Une version que le lot ne contient pas n'est pas dans l'index (voir la [RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap)).

La porte d'âge de publication date une version par le champ `created` de son entrée d'index, que `helm repo index` écrit sur chaque entrée.

## Non servi

- Les registres de charts OCI (`helm push`/`pull oci://…`), qui sont des registres de conteneurs
- Les chemins multi-locataires de ChartMuseum et ses endpoints `/health` et `/info`
- L'API de recherche d'Artifact Hub ; `registry search` répond à partir de ce que cette instance détient, comme pour tous les types

## Voir aussi

- [RFC 0029](/rfc/0029-helm-charts) — la réécriture des URL, le choix `oci_entries` et la régénération de l'index
- [RFC 0006](/rfc/0006-blocked-versions-hidden-everywhere) — les versions bloquées masquées des listings
