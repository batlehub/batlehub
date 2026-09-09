---
sourcePath: registries/gitlab.md
sourceHash: 6e64c1bcd63a1b31
---

# GitLab

Fait proxy et cache des releases, des assets de liens de release et des
archives de sources d'une instance GitLab. Un chemin de projet peut comporter des
groupes imbriqués ; le sous-chemin de release est séparé par `/-/`, comme dans
les URL de GitLab. L'API Packages de GitLab est également proxifiée sous
`/api/v4/`.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `gitlab` |
| **Amont par défaut** | `gitlab.com` |
| **Modes** | proxy seul |
| **Adressage** | par paquet |
| **Publication privée** | ❌ proxy seul |
| **Coupure réseau** | hors ligne, la liste des releases et la release par tag sont composées à partir des liens de téléchargement détenus |

## Mise en place du proxy

Donnez à `upstreams` la racine de l'instance (par exemple `https://gitlab.com`).
Remplacez `<registry>` par le nom de registre que vous avez configuré ; ajoutez
`-H "Authorization: Bearer $BATLEHUB_TOKEN"` si nécessaire :

```bash
REG="https://batlehub.example.com/proxy/<registry>"

# Lister les releases / obtenir une release par tag (groupes imbriqués autorisés)
curl $REG/<group>/<project>/-/releases
curl $REG/<group>/<subgroup>/<project>/-/releases/v1.0.0

# Télécharger l'asset d'un lien de release (apparié par nom de lien)
curl -L -O $REG/<group>/<project>/-/releases/v1.0.0/downloads/app.bin

# Archive des sources pour un tag (le format est déduit de l'extension)
curl -L -O $REG/<group>/<project>/-/archive/v1.0.0/source.tar.gz

# Fichier brut du dépôt
curl -L $REG/<group>/<project>/-/raw/main/README.md
```

Un registre `gitlab` met aussi en cache de façon transparente l'**API Packages**
de GitLab sous `/api/v4/…` — idéal pour le registre de paquets **generic** :

```bash
curl -L -O https://batlehub.example.com/proxy/<registry>/api/v4/projects/<id>/packages/generic/<name>/<version>/<file>
```

Pour les registres **d'écosystème** (npm, Maven, PyPI, NuGet, Composer, …),
pointez plutôt l'adaptateur typé correspondant vers l'endpoint de paquets GitLab,
afin que les URL de métadonnées soient réécrites et mises en cache.

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
`[registries.refs]` décide de ce que fait chacun : suivre une branche est
`MUTABLE_REF` (`warn` par défaut), un tag qui résout désormais ailleurs est
`TAG_MOVED` (`deny`), et un asset de release dont l'empreinte a changé est
`ASSET_REPLACED` (`deny`). La première résolution d'un tag est toujours
acceptée ; un changement est remarqué à la résolution suivante, après
`tag_ttl_secs`.

```toml
[registries.refs]
branch_ttl_secs = 60
tag_ttl_secs    = 3600
mutable_refs    = "warn"   # "warn" (défaut) | "deny"
tag_moved       = "deny"   # "deny" (défaut) | "warn"
```

GitLab résout un tag en un seul appel : `/repository/tags/{tag}` porte le commit
en ligne, et c'est le `created_at` d'un tag annoté qui le date. Chaque appel
d'API puise dans le même budget de limitation de débit que les deux autres
forges, de sorte que le proxy et le worker d'analyse ne peuvent pas dépenser deux
fois le même jeton.

## Fichiers bruts

**Le contenu brut est désactivé tant que vous ne l'activez pas** —
`/-/raw/{ref}/{path}` était autrefois servi implicitement, et un registre sans
bloc `[registries.raw]` le refuse désormais, avec un corps qui le dit.

```toml
[registries.raw]
enabled        = true
max_size_bytes = 10485760
repos          = ["group/*"]
require_pinned = false
scripts        = "warn"     # "deny" par défaut quand le registre a [registries.security]
```

## Lectures d'API typées

```toml
[registries.api_reads]
families = ["tags", "commits", "branches"]
```

Trois routes JSON en lecture seule, dans la forme propre de BatleHub — `tags`,
`commits/{sha}`, `branches/{name}` — sous `/proxy/<registry>/<project>/`. Une
famille que le registre n'a pas demandée répond `404`, et `contents` comme
`git/blobs` ne sont jamais acceptées.

**Les documents de release sont réécrits** : `assets.sources[].url` pointe vers
la route d'archive de ce proxy, `assets.links[].url` et `direct_asset_url` vers
sa route de téléchargement, et le bloc `_links` propre à GitLab est retiré. Un
client qui suit le document de release reste donc derrière le proxy.

## Authentification

Passez un token BatleHub dans un en-tête Bearer
(`-H "Authorization: Bearer $BATLEHUB_TOKEN"`) quand le RBAC du registre
l'exige. Les jetons d'accès personnels GitLab passent par l'en-tête
`PRIVATE-TOKEN` — configurez-le comme en-tête d'authentification amont
personnalisé du registre pour atteindre des projets privés.

## Notes

- Proxy et cache uniquement : la première requête est diffusée depuis l'amont et
  mise en cache.
- Le sous-chemin de release est séparé par `/-/`, exactement comme dans les URL
  de GitLab ; les chemins de groupes imbriqués sont pris en charge.
- **La provenance est `unverifiable` ici, et seulement ici.** GitLab collecte un
  blob JSON de preuves par release et ne signe rien : une release accompagnée de
  preuves signale donc `PROVENANCE_UNVERIFIABLE` — informatif, jamais un refus à
  lui seul. Un commit signé est lu depuis
  `/repository/commits/{sha}/signature` et signale « vérifié » ou « invalide » ;
  GitHub et Forgejo ne signalent jamais `unverifiable`.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
