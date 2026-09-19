---
sourcePath: registries/galaxy.md
sourceHash: b75d9cc1b7d45c52
---

# Ansible Galaxy

`galaxy.ansible.com` est mandaté et mis en cache comme un registre *typé*, afin qu'une version de collection puisse être **bloquée** plutôt que simplement mise en cache. La liste des versions d'une collection — `v3/collections/{namespace}/{name}/versions/` — constitue le listing filtré et le point d'application ; le tarball `{namespace}-{name}-{version}.tar.gz` est l'artefact. Les modes `local` et `hybrid` acceptent `ansible-galaxy collection publish`.

Les rôles — l'ancienne API v1 — sont servis en lecture seule, et l'étendue de cette surface est un choix de l'opérateur (`roles`, plus bas).

## En un coup d'œil

| | |
|---|---|
| **Type de config** | `galaxy` |
| **Amont par défaut** | `https://galaxy.ansible.com/api/` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | `{namespace}.{name}` — l'orthographe qu'utilise `requirements.yml` — avec un tarball par version |
| **Publication privée** | ✅ `ansible-galaxy collection publish` |
| **Bascule client** | `server_list` dans `ansible.cfg` |

## Configuration du proxy

`ansible.cfg` a une seule bascule. `server_list` nomme les serveurs qu'utilise `ansible-galaxy`, dans l'ordre ; n'y déclarer que celui-ci garde toute résolution sur le proxy :

```ini
[galaxy]
server_list = batlehub

[galaxy_server.batlehub]
url   = https://batlehub.example.com/proxy/<registre>/galaxy/api/
token = <your-token>
```

```sh
ansible-galaxy collection install community.general
ansible-galaxy collection install -r requirements.yml
```

Le bloc de registre côté administrateur :

```toml
[[registries]]
name      = "<registre>"
type      = "galaxy"
mode      = "proxy"                              # proxy · local · hybrid
upstreams = ["https://galaxy.ansible.com/api/"]  # la valeur par défaut
roles     = "proxy"                              # proxy · index · off

[registries.rbac]
# La liste des versions est un listing ; le document de version et le tarball sont des lectures.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list", "releases:publish"]
admin     = ["*"]
```

`upstreams` est la **racine de l'API** — l'URL qui répond `available_versions`. Écrire l'hôte sans `/api/` fonctionne également : l'adaptateur la sonde exactement comme le fait `g_connect`, le code d'`ansible-galaxy` lui-même, et réessaie avec `/api/` ajouté.

## Authentification

`token` dans un bloc `[galaxy_server.*]` est envoyé comme **`Authorization: Token <jeton>`** sur **chaque** appel, lectures comprises — un registre `galaxy` peut donc être fermé aux lectures anonymes sans casser le client. C'est inhabituel : la plupart des gestionnaires de paquets n'envoient rien sur une lecture.

`Token` est le schéma de Django REST Framework, pas celui de HTTP, et c'est ce que nomme `GalaxyToken.token_type` ; BatleHub le normalise avant que ses fournisseurs d'authentification ne le voient. (`Bearer` est celui de `KeycloakToken`, propre à Automation Hub, qui n'est [pas servi](#non-servi).)

Un couple `username`/`password` envoie plutôt du HTTP Basic, que ce serveur accepte aussi.

::: warning `--api-key` ne remplace pas `token =`
Lorsque le serveur vient de `server_list`, `ansible-galaxy … --api-key <jeton>` n'attache **aucun identifiant** — mesuré sur ansible-core 2.19.3 : la requête ne portait aucun en-tête `Authorization`. Placez l'identifiant dans le bloc `[galaxy_server.*]`.
:::

Les lectures sans ligne `token` sont anonymes, ce qui relève de la décision ordinaire `anonymous = [...]` ci-dessus.

::: tip Ce n'est pas l'échange de jetons d'Automation Hub
`auth_url`, `client_id` et `client_secret` dans une section `[galaxy_server.*]` pilotent un rafraîchissement OAuth2 contre un Keycloak. Ce serveur émet ses propres jetons porteurs : utilisez `token`.
:::

## Ce que le blocage fait au client

Une version bloquée est **absente de la liste des versions**, et le résolveur d'`ansible-galaxy` ne choisit que dans cette liste — pour chaque exigence directe *et* chaque dépendance, y compris une exigence épinglée à une version exacte. Donc :

- un **intervalle** se résout vers la version la plus récente que vous autorisez, et l'installation réussit ;
- un **épinglage exact** sur une version bloquée s'arrête sur l'erreur propre à ansible, *« Failed to resolve the requested dependencies map. Could not satisfy the following requirements: »*, avant toute requête de métadonnées ou d'artefact ;
- un client qui détient un **listing périmé** d'avant le blocage est refusé au document de version (`404`) puis de nouveau au tarball (`403`).

Bloquer la version la plus récente déplace également le `highest_version` du document de collection vers la plus récente survivante, et fait avancer son `updated_at`. C'est cette seconde modification qui rend la première rapidement effective : `ansible-galaxy` met un listing en cache pendant 24 heures et relit le document de collection à chaque résolution pour décider si cette copie tient toujours. Sans cet avancement, un client tiède continuerait de proposer la version bloquée à son propre résolveur pendant une journée, et l'installation échouerait au lieu de se résoudre silencieusement.

## Quatre choses à savoir

**Le tarball est identique à l'octet près.** `ansible-galaxy` hache le corps au fil du téléchargement et le compare à `artifact.sha256` du document de version, en échouant sur *« Mismatch artifact hash with downloaded file »*. Ce serveur relaie les deux sans modification et ne réécrit jamais une archive de collection. Les signatures GPG que l'amont publie sur le `MANIFEST.json` d'une collection sont relayées telles quelles — rien n'en forge ici, et rien ne le pourrait.

**Chaque listing tient sur une seule page.** `links.next` vaut toujours `null`, et ce serveur parcourt lui-même les pages de l'amont. Ce n'est pas une simplification : `ansible-galaxy` résout un lien de continuation avec `urljoin(api_server, next)`, et les liens de l'amont sont des *chemins* absolus, qui remplacent tout le chemin — un lien relayé tel quel enverrait la requête suivante à la racine de l'hôte plutôt que vers `/proxy/<registre>/galaxy/…`. La moitié « rôles » est pire : son client supprime le chemin délibérément. L'amont plafonne une page à 100 entrées quoi qu'on demande, donc la plus grosse collection existante (`community.general`, 241 versions) représente trois requêtes amont par remplissage de cache.

**Un jeton est envoyé sur les lectures comme sur les écritures** — voir *Authentification* ci-dessus.

**Les rôles ont une limite que ce serveur ne peut pas lever.** `ansible-galaxy role install` construit lui-même `https://github.com/{user}/{repo}/archive/{version}.tar.gz`, et ne préfère un `download_url` que lorsque le listing des versions du rôle en porte un. galaxy.ansible.com en porte un pour chaque version publiée, donc `roles = "proxy"` fait passer ces octets par cette instance. Deux cas hors d'atteinte, quelle que soit la configuration :

- un rôle **sans version publiée** s'installe depuis sa branche par défaut, directement depuis GitHub ;
- une entrée de `requirements.yml` avec une URL `src:` explicite n'a jamais été une requête de registre.

## Le réglage `roles`

```sh
ansible-galaxy role install geerlingguy.docker
```

La part de cette commande que sert cette instance est un choix de l'opérateur :

| `roles` | Comportement |
|---|---|
| `proxy` (défaut) | Les endpoints de lecture v1 sont servis, le `download_url` de chaque version est réécrit, et ce serveur récupère l'archive depuis l'hôte nommé en amont — à travers le garde-fou SSRF, et uniquement depuis `github.com` ou l'amont du registre. |
| `index` | Les mêmes endpoints avec `download_url` relayé. Les *métadonnées* de rôles sont mandatées ; les *octets* ne le sont pas, donc le serveur n'émet aucune connexion sortante vers GitHub. |
| `off` | Les endpoints v1 répondent `404`, et `v1` est absent du document de découverte — donc `ansible-galaxy role install` échoue sur sa propre erreur *« requires API versions 'v1' »* plutôt que sur un `404` qui ressemble à un défaut du proxy. |

`roles` est rejeté sur tout autre type de registre.

## Publication

Les modes `local` et `hybrid` acceptent le vrai protocole de publication :

```sh
ansible-galaxy collection build
ansible-galaxy collection publish ./acme-util-1.0.0.tar.gz --server batlehub
```

L'identifiant est la ligne `token =` d'`ansible.cfg`, pas `--api-key` — voir l'avertissement sous *Authentification*.

La publication est **synchrone**. `ansible-galaxy` envoie le tarball puis interroge la tâche d'import ; ici le travail est terminé avant que ce POST ne réponde, donc la tâche est déjà `completed` au premier sondage et une installation immédiatement après ne peut pas la doubler.

Le POST répond avec un **identifiant** de tâche nu, et le client construit lui-même l'URL de sondage — `…/v3/imports/collections/{id}/`. Il ne suit pas une URL fournie par le serveur.

Trois vérifications s'exécutent avant tout stockage, chacune produisant un `400` :

- le champ de formulaire `sha256` doit être égal à l'empreinte des octets envoyés — une paire incohérente ferait échouer chaque installation, puisque le client vérifie la même empreinte (la partie fichier arrive encodée en base64, ce qu'`ansible-galaxy` signale par `Content-Transfer-Encoding` ; une partie binaire simple est acceptée également) ;
- le tarball doit contenir un `MANIFEST.json` à sa racine ;
- le nom du fichier doit concorder avec ce que ce manifeste déclare, pour qu'une version ne puisse pas être stockée sous une coordonnée et servie sous une autre.

Une version en double répond `409`, qu'`ansible-galaxy` restitue sous la forme *« (HTTP Code: 409, Message: … Code: …) »*.

`FILES.json` voyage à l'intérieur de l'artefact et n'est jamais reconstruit : c'est ce document qu'un client confronte à l'arborescence extraite, donc une copie recalculée qui divergerait d'un octet ferait échouer une installation que ce serveur avait acceptée.

### Référence des endpoints

<!-- BEGIN endpoints: proxy/galaxy -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/galaxy/api/` | The API versions this registry serves — read by `g_connect` before any action, and `v1` is absent when `roles = "off"`. |
| `GET` | `/proxy/{registry}/galaxy/api/v1/roles/` | Look a role up by owner and name — what `lookup_role_by_name` reads for the numeric id every later v1 request uses. |
| `GET` | `/proxy/{registry}/galaxy/api/v1/roles/{id}/download/{filename}` | A role archive, fetched server-side from the host its listing named — served only under `roles = "proxy"`. |
| `GET` | `/proxy/{registry}/galaxy/api/v1/roles/{id}/versions/` | Every allowed version of a role, as one page, each `download_url` pointed here under `roles = "proxy"`. |
| `POST` | `/proxy/{registry}/galaxy/api/v3/artifacts/collections/` | Publish a collection — synchronous, so the import task it answers with is already finished. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/artifacts/collections/{filename}` | The collection tarball, byte-exact — the client hashes it against `artifact.sha256` from the version document. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/` | The collection document, with `highest_version` moved off a blocked version and `updated_at` bumped past the block. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/` | Every allowed version of a collection, as one page — the chokepoint the resolver picks from. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/{version}/` | One version's document — `download_url` pointed here, `artifact.sha256` relayed untouched, `404` when the version is blocked. |
| `GET` | `/proxy/{registry}/galaxy/api/v3/imports/collections/{task}/` | The import task a publish returned — always `completed`, because the work is done before the task exists. |
<!-- END endpoints -->

## Mode hybride : le local l'emporte en entier, pas version par version

Sur un registre `hybrid`, une collection est servie **soit** d'ici **soit** de
l'amont, jamais fusionnée : si cette instance a publié une version quelconque
d'`acme.util`, c'est cette collection que voit un client, et les versions amont
du même nom ne sont pas proposées. L'amont n'est consulté que pour une
collection que cette instance n'a jamais publiée.

C'est le comportement de tous les types de registre de BatleHub, et non une
règle propre à Galaxy — et c'est lui qui empêche qu'une collection privée soit
silencieusement remplacée par une collection publique du même nom. La
conséquence à connaître : publier ici une seule version corrigée d'une
collection publique confisque le nom entier.

## Parcs hors ligne

Un registre hors ligne compose les trois documents que lit une installation — la liste des versions, le document de collection et le document par version — à partir des versions que le lot transporte réellement, et sert le tarball depuis le stockage. Une version absente du lot est absente du listing, et une requête sur son document répond `503` plutôt que `404` : elle existe, elle n'est simplement pas ici (voir la [RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap)).

Une version importée ne porte aucune date de publication : une règle `release_age_gate` sur un registre `galaxy` doit donc définir explicitement `deny_missing_timestamp` — le chargeur de configuration refuse la règle sans ce champ.

## Non servi

- l'échange de jetons OAuth2 d'Automation Hub (`auth_url`/`client_id`/`client_secret`)
- les APIs d'**écriture** de rôles — `ansible-galaxy role import`, `delete`, `setup` — qui relèvent d'une intégration de forge plutôt que d'une opération de registre
- les APIs de recherche, de listing de namespaces et de dépréciation de Galaxy ; `registry search` répond à partir de ce que détient cette instance, comme pour tous les types
- `docs_blob` et les vues de contenu rendues
- les Ansible Execution Environments, qui sont des images OCI

## Voir aussi

- [RFC 0031](/rfc/0031-ansible-galaxy) — pourquoi la liste des versions est le point d'application, et pourquoi chaque listing tient sur une seule page
- [RFC 0006](/rfc/0006-blocked-versions-hidden-everywhere) — les versions bloquées masquées des listings
