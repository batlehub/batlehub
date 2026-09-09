---
sourcePath: registries/forgejo.md
sourceHash: 8f0cb0fd0a0db3ef
---

# Forgejo / Gitea

Fait proxy et cache des assets de release, des archives de sources et des
fichiers bruts d'une instance [Forgejo](https://forgejo.org) ou Gitea. Le schéma
d'URL est celui de GitHub, et le registre de paquets Forgejo / Gitea est
également proxifié sous `/api/packages/`.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `forgejo` |
| **Amont par défaut** | `codeberg.org` |
| **Modes** | proxy seul |
| **Adressage** | par paquet |
| **Publication privée** | ❌ proxy seul |
| **Coupure réseau** | hors ligne, la liste des releases et la release par tag sont composées à partir des assets détenus |

## Mise en place du proxy

Donnez à `upstreams` la racine de l'instance (par exemple
`https://codeberg.org`). Un dépôt s'adresse par `<owner>/<repo>` ; remplacez
`<registry>` par le nom de registre que vous avez configuré et ajoutez
`-H "Authorization: Bearer $BATLEHUB_TOKEN"` si nécessaire :

```bash
REG="https://batlehub.example.com/proxy/<registry>"

# Lister les releases / obtenir une release par tag
curl $REG/<owner>/<repo>/releases
curl $REG/<owner>/<repo>/releases/tags/v1.0.0

# Télécharger l'asset d'une release par son nom de fichier
curl -L -O $REG/<owner>/<repo>/releases/download/v1.0.0/app.tar.gz

# Tarball ou zip des sources pour un tag, une branche ou un commit
curl -L -O $REG/<owner>/<repo>/tarball/v1.0.0
curl -L -O $REG/<owner>/<repo>/zipball/v1.0.0

# Fichier brut
curl -L $REG/<owner>/<repo>/raw/main/README.md
```

Un registre `forgejo` met aussi en cache de façon transparente le **registre de
paquets** Forgejo / Gitea sur `/api/packages/{owner}/…` — idéal pour le registre
de paquets **generic** :

```bash
curl -L -O https://batlehub.example.com/proxy/<registry>/api/packages/<owner>/generic/<name>/<version>/<file>
```

Pour les registres **d'écosystème** (npm, Maven, PyPI, Composer, NuGet, …),
pointez plutôt l'adaptateur typé correspondant vers l'endpoint de paquets, afin
que les URL de métadonnées soient réécrites et mises en cache.

## Versions bloquées

La liste des releases retire une release bloquée, l'ordre du plus récent au plus
ancien restant intact, de sorte qu'un client ne sélectionne jamais une release
dont les assets lui seront ensuite refusés.

Une release est identifiée par son **tag**, et la même release est taguée `1.2.3`
dans un dépôt et `v1.2.3` dans le suivant. Un blocage correspond aux deux
orthographes : il ne dépend donc pas de l'habitude du dépôt.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Ce qu'est une version ici

Un registre de paquets nomme des choses immuables. Une forge, non : `main` est ce
vers quoi il pointe au moment où vous demandez, et un tag peut être déplacé.
Chaque requête vers une forge résout donc sa ref en un **commit** avant de
récupérer quoi que ce soit, et c'est ce commit qui sert de clé au cache, aux
métadonnées et au verdict de chaîne d'approvisionnement
([RFC 0019](/rfc/0019-git-forge-registries-refs-releases-raw)).

Toute réponse de forge porte la résolution :

| En-tête | Signification |
| --- | --- |
| `X-BatleHub-Ref-Kind` | `tag`, `branch` ou `commit` |
| `X-BatleHub-Resolved-Commit` | le commit qui a répondu |
| `X-BatleHub-Ref-Previous-Commit` | ce que la même ref avait donné la fois précédente, quand cela diffère |
| `X-BatleHub-Ref-Requested` | la ref telle que vous l'avez écrite — le seul endroit où elle survit une fois la coordonnée devenue le commit |

Trois faits à propos d'une ref changent ce qu'obtient une requête, et
`[registries.refs]` décide de ce que fait chacun :

| Fait | Code | Défaut |
| --- | --- | --- |
| la requête a suivi une branche | `MUTABLE_REF` | `warn` — servi, et signalé |
| un tag résout maintenant vers un autre commit | `TAG_MOVED` | `deny` |
| l'empreinte de l'asset d'une release a changé depuis la mise en cache des octets | `ASSET_REPLACED` | `deny` |

Un tag déplacé et un asset remplacé sont les deux façons, propres aux forges, de
substituer des octets sous une coordonnée stable : d'où leur refus par défaut. La
première résolution d'un tag est toujours acceptée — il n'y a rien à quoi la
comparer — et un changement est remarqué à la résolution suivante, après
`tag_ttl_secs` (une heure par défaut).

Sur un registre doté de
[`[registries.security]`](/fr/guide/configuration#security), ces faits voyagent
avec le verdict de la version : une branche avertie répond donc avec
`X-BatleHub-Verdict: warned`, et `batlehub why` l'explique. Sans cette section,
il n'y a pas de verdict pour les porter : un `deny` est un simple `403` qui nomme
le code, et un `warn` se réduit aux en-têtes ci-dessus. C'est la dégradation
honnête, et c'est la raison d'être de `X-BatleHub-Ref-Previous-Commit`.

```toml
[registries.refs]
branch_ttl_secs = 60      # durée de confiance d'une résolution branche → commit
tag_ttl_secs    = 3600    # et celle d'un tag ; aussi la latence de détection d'un tag déplacé
mutable_refs    = "warn"  # "warn" (défaut) | "deny"
tag_moved       = "deny"  # "deny" (défaut) | "warn"
```

## Fichiers bruts

**Le contenu brut est désactivé tant que vous ne l'activez pas.** Il était
autrefois servi implicitement ; un registre sans bloc `[registries.raw]` le
refuse désormais, et le refus le dit dans son corps.

```toml
[registries.raw]
enabled        = true
max_size_bytes = 10485760          # 10 Mio ; ne doit pas dépasser [limits].max_artifact_size_bytes
repos          = ["forgejo/*"]     # motifs owner/repo ; vide autorise tout dépôt
require_pinned = false             # true refuse une ref de branche pour le contenu brut
scripts        = "warn"            # "warn" | "deny" | "ignore"
```

Un fichier au-dessus du plafond est refusé, jamais tronqué. `scripts` regarde
l'extension du fichier (`.sh`, `.bash`, `.ps1`, `.py`, `.bat`, …) et, sous
`deny`, ses premiers octets également — une charge sans extension qui commence
par un shebang est donc refusée aussi. `scripts` vaut **`deny`** par défaut sur
un registre qui a un profil `[registries.security]`, et `warn` sur tout autre :
choisir une quarantaine, c'est choisir « rien de non analysé n'est servi », et un
script laissé passer est le seul artefact qu'aucun scanner d'ici ne lit.

Le contenu brut est toujours servi en `application/octet-stream` avec
`X-Content-Type-Options: nosniff`, de sorte qu'un fichier HTML brut ne peut pas
s'exécuter comme document sur cette origine.

## Lectures d'API typées

La liste des releases et la release par tag sont déjà servies. Trois autres
routes JSON en lecture seule sont disponibles sur demande :

```toml
[registries.api_reads]
families = ["tags", "commits", "branches"]
```

| Famille | Route |
| --- | --- |
| `tags` | `GET /proxy/<registry>/<owner>/<repo>/tags` |
| `commits` | `GET /proxy/<registry>/<owner>/<repo>/commits/<sha>` |
| `branches` | `GET /proxy/<registry>/<owner>/<repo>/branches/<name>` |

Elles répondent dans la forme propre de BatleHub, pas dans celle de la forge : on
n'y trouve aucune URL amont à suivre, aucun champ dont le sens change d'une forge
à l'autre, et aucune place pour qu'un passe-plat en devienne un. Une famille que
le registre n'a pas demandée répond `404`. `contents` et `git/blobs` ne sont
jamais acceptées — c'est du contenu brut par une autre porte, et cette décision
vit dans `[registries.raw]`.

**Les documents de release sont réécrits.** `tarball_url`, `zipball_url` et le
`browser_download_url` de chaque asset sont redirigés vers ce proxy, et les liens
d'API propres à la forge sont retirés. Un client qui lit le document de release
plutôt que de construire un chemin — `mise`, `gh` — reste donc derrière le proxy,
avec sa politique, son cache et sa piste d'audit.

## Authentification

Passez un token BatleHub dans un en-tête Bearer
(`-H "Authorization: Bearer $BATLEHUB_TOKEN"`) quand le RBAC du registre
l'exige. Pour une **instance privée**, configurez un token bearer comme
authentification amont du registre dans la configuration du serveur.

## Notes

- Proxy et cache uniquement : la première requête est diffusée depuis l'amont et
  mise en cache.
- Le schéma d'URL est identique à celui de [GitHub](/fr/registries/github).

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
