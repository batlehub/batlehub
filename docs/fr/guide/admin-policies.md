---
# Un peu plus de 4 000 mots : la surface blocage / masquage / quota, et chacune
# des trois se comporte différemment sur un registre `[security]`, d'où la
# longueur. `docs:structure` demande cette ligne au-delà de 4 000 mots — pas un
# plafond, une déclaration que quelqu'un a dû écrire (RFC 0005-bis §4.5).
reference: true
sourcePath: guide/admin-policies.md
sourceHash: 851ce5fe2cda21b9
---

# Politiques et paquets

## La politique de cache {#cache-policy}

Pour une explication complète du fonctionnement du cache de bout en bout — cycle
de vie d'une requête, choix du backend, compteurs de limitation, déduplication —
voir le **[guide de la mise en cache](/fr/guide/caching)**.

Tous les réglages de cache vivent sous `[registries.cache]` et sont propres à
chaque registre.

### L'éviction

```toml
[registries.cache]
metadata_ttl_secs = 300      # revérifier les listes de versions après 5 minutes (défaut)
serve_stale       = true     # servir des métadonnées en cache quand l'amont est en panne (défaut)

artifact_ttl_secs = 2592000  # supprimer les artefacts de plus de 30 jours
idle_days         = 14       # supprimer les artefacts non accédés depuis 14 jours
max_size_bytes    = 10737418240  # plafond de 10 Gio — évince les moins récemment utilisés au-delà
keep_latest_n     = 5        # ne garder que les 5 versions les plus récemment mises en cache par paquet
```

Tous les champs d'éviction sont facultatifs. Omettre un champ désactive cette
stratégie. Les stratégies se combinent : un artefact est évincé dès que
**n'importe quelle** stratégie active se déclenche.

| Champ | Défaut | Description |
|-------|---------|-------------|
| `metadata_ttl_secs` | `300` | TTL du cache de métadonnées, en secondes |
| `serve_stale` | `true` | Servir des métadonnées périmées sur un 5xx amont plutôt que de propager l'erreur |
| `artifact_ttl_secs` | — | Évincer les artefacts de plus de N secondes |
| `idle_days` | — | Évincer les artefacts non accédés depuis N jours |
| `max_size_bytes` | — | Plafond de stockage ; les artefacts les moins récemment utilisés sont retirés au-delà |
| `keep_latest_n` | — | Ne garder que les N versions les plus récentes par paquet |

#### Lancer une passe, et la prévisualiser {#cache-eviction-run}

```sh
batlehub admin cache evict acme-npm --dry-run   # ce qui partirait, et rien ne part
batlehub admin cache evict acme-npm             # évincer réellement
```

Le mode réel est le défaut ici, à l'inverse d'[`admin retention`](#retention) :
ce que l'éviction retire est une copie que la requête suivante récupérera à
nouveau, de sorte que le verrou à deux clés qui protège une version publiée
localement serait ici une cérémonie. La prévisualisation existe pour l'autre
question — *combien ce nouveau plafond de taille prendrait-il réellement ?* — et
y répond avec les clés, pas seulement avec un compte.

Une prévisualisation de plafond est bornée : elle lit une page de candidats à
l'éviction, et le dit dans `incomplete_because` si le registre dépasse encore le
plafond quand la page est épuisée. Une passe réelle, elle, continue.

#### Ce qu'une passe laisse derrière elle {#cache-eviction-trail}

| | Passe réelle | Prévisualisation |
| --- | --- | --- |
| Événement de passe, rapporté au registre | `cache_evict_run` | `cache_evict_dry_run` |
| Événement par artefact | aucun — voir plus bas | aucun |

Il n'y a délibérément **aucun événement par artefact évincé**, et c'est là que
cela diverge de [la piste de la rétention](#retention-trail) : une passe LRU
évince par milliers, et enterrer les suppressions *irrécupérables* sous celles
qui ne le sont pas rendrait toute la piste illisible. Ce qui est parti est dans
le rapport de la passe et dans sa ligne de log.

Retirer un artefact du cache **à la main** est l'autre cas — un opérateur, une
décision, un paquet — et celui-là porte bien la coordonnée :

| Surface | Action |
| --- | --- |
| `DELETE /api/v1/admin/registries/{r}/cache` | `cache_evict` |
| `POST /api/v1/admin/packages/invalidate` | `cache_evict` |
| `POST /api/v1/admin/registries/{r}/clear-cache` | `cache_clear`, rapporté au registre |
| `POST /api/v1/admin/registries/{r}/coherence` | `cache_coherence_run` / `cache_coherence_dry_run` |

Aucune de ces actions n'est un `delete`. Une copie en cache n'est pas le paquet,
et un auditeur ne doit pas avoir à lire le mode d'un registre dans un fichier de
configuration pour les distinguer :

```sh
# Tout ce qui a réellement été supprimé, à la main ou par politique
batlehub admin audit-log --action delete,retention_reclaim

# Tout ce qui a seulement été sorti du cache
batlehub admin audit-log --action cache_evict,cache_clear,cache_evict_run
```

#### Ramasser les blobs orphelins {#cache-coherence}

Un artefact est mis en cache en deux temps — les octets sont stockés, puis la
ligne qui pointe vers eux est enregistrée. Un processus tué entre les deux laisse
un blob que rien ne référence : il occupe du disque, aucune requête ne peut
jamais l'atteindre, et aucune stratégie d'éviction ne le considérera, puisque
chacune lit la table dont il est absent. Supprimer une ligne de la base à la main
laisse la même chose.

```sh
batlehub admin cache coherence acme-npm --dry-run   # ce qui est orphelin
batlehub admin cache coherence acme-npm             # le ramasser
```

**Deux passes avant que quoi que ce soit ne parte.** Un blob n'est supprimé que
si la passe *précédente* l'a vu orphelin lui aussi — parce qu'une écriture de
cache en cours ressemble exactement à un orphelin, et que la fenêtre entre ses
deux étapes se compte en millisecondes. La première passe d'un parc neuf signale
donc `first_seen_orphaned` et ne supprime rien ; relancez-la pour ramasser. Le
rapport garde les deux séparés : `deleted_keys` est ce qui est parti,
`first_seen_keys` ce qu'une seconde passe prendrait.

`--dry-run` rapporte sans supprimer **et sans faire avancer quoi que ce soit vers
la suppression**. Prévisualiser deux fois n'est pas lancer deux fois — une
prévisualisation qui armerait ce qu'elle décrit serait un piège plutôt qu'une
prévisualisation.

Contrairement aux stratégies d'éviction, cela n'exige aucune configuration : les
orphelins n'attendent pas qu'un TTL soit défini, et la passe est donc disponible
sur tout registre.

Pour la lancer périodiquement plutôt qu'à la main, activez
[`[cache_coherence]`](/fr/guide/configuration#38b-cache-coherence-optional). Une
passe planifiée porte `user_id = "system"` dans la piste, ce qui la distingue de
celle d'un opérateur :

```sh
batlehub admin audit-log --action cache_coherence_run
```

### Le préchauffage du cache {#cache-warming}

Le préchauffage récupère des versions d'artefact à l'avance, pour qu'elles soient
disponibles sans latence à la première requête. Il se configure à côté de
l'éviction :

```toml
[registries.cache]
warm_packages    = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n    = 3   # préchauffer les 3 versions les plus récentes des entrées sans version
warm_concurrency = 4   # jusqu'à 4 téléchargements en parallèle
```

| Champ | Défaut | Description |
|-------|---------|-------------|
| `warm_packages` | `[]` | Les paquets à préchauffer au démarrage. `"name"` préchauffe les `warm_latest_n` versions les plus récentes ; `"name@version"` en préchauffe exactement une. |
| `warm_latest_n` | `1` | Versions à récupérer d'avance par entrée sans version |
| `warm_concurrency` | `2` | Nombre maximum de téléchargements en parallèle par passe |

BatleHub démarre le préchauffage immédiatement après avoir ouvert la socket du
serveur : le serveur HTTP est donc disponible pendant que le préchauffage
s'exécute en tâche de fond.

#### Le préchauffage à la demande par l'API d'administration

Repréchauffez un paquet à tout moment, sans redémarrer :

```sh
# Préchauffer avec le warm_latest_n configuré du registre
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash"}'

# Remplacer le nombre de versions pour cette requête seulement
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash", "versions": 10}'

# Préchauffer une version figée
curl -X POST http://localhost:8080/api/v1/admin/registries/cargo/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "serde@1.0.200"}'
```

Réponse :

```json
{"warmed": 3, "skipped": 0, "errors": 0}
```

- `warmed` — versions d'artefact récupérées et stockées lors de cette passe
- `skipped` — versions déjà présentes dans le cache (aucun téléchargement)
- `errors` — versions dont la récupération ou le stockage a échoué

::: tip Ce que chaque registre gère
L'énumération des versions (employée pour préchauffer un nom nu) est implémentée
pour tous les types de registre fondés sur le paquet. Une entrée figée s'écrit
toujours `name@version`, où la moitié « nom » est la coordonnée par laquelle ce
registre adresse ses paquets — `"lodash@4.17.21"` (npm),
`"com.google.guava:guava@33.0.0-jre"` (Maven),
`"providers/hashicorp/aws@5.0.0"` (Terraform), `"rails@7.1.0"` (RubyGems),
`"monolog/monolog@3.5.0"` (Composer). Pour **GitHub**, un nom nu énumère les
releases par l'API Releases (paginée). Pour la **place de marché VS Code**, il
énumère toutes les versions d'extension par l'API Gallery. Pour **Conda**,
BatleHub synthétise la liste des versions en parcourant `repodata.json` sur
`noarch`, `linux-64`, `osx-64`, `osx-arm64` et `win-64`. Pour la **place de
marché JetBrains**, une entrée est l'`xmlId` du plugin (`"org.rust.lang"`,
`"org.rust.lang@0.4.201"`) et un nom nu énumère les versions par
`/plugins/list` — ce qui ne couvre que le canal **Stable** : les builds EAP et
nocturnes ne sont donc pas récupérés d'avance.
:::

### La déduplication par le contenu

BatleHub stocke les octets d'un artefact sous une clé adressée par le contenu
(`blob/{sha256}`) et fait correspondre les clés d'artefact logiques (par exemple
`artifact:npm/lodash/4.17.21`) à ce blob par un compteur de références. Quand des
octets identiques apparaissent sous plusieurs clés logiques — le même paquet
répliqué sur deux registres, une version retirée puis republiée — une seule copie
est stockée sur disque ou sur S3.

C'est automatique et ne demande aucune configuration. Les artefacts stockés avant
la déduplication continuent d'être servis normalement.

---

## La gestion des paquets {#package-management}

### Lister les paquets

```sh
# Tous les paquets
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/packages"

# Filtrer par registre et par nom
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/packages?registry=npm&name=lodash"
```

### Bloquer une version de paquet {#block-a-package-version}

Un blocage fait deux choses, et les deux comptent :

1. **La version disparaît des listes de versions**, dans la forme que lisent les
   clients de l'écosystème — un packument npm, un index plat NuGet, un
   `maven-metadata.xml`, une page simple PyPI. Ce que ce protocole appelle « la
   plus récente » est réparé pour nommer une version encore autorisée :
   `dist-tags.latest` et le `<release>` de Maven sont recalculés, le `@latest` de
   Go est re-résolu. Un client qui demande `latest`, ou un intervalle comme
   `^4.17.0`, se résout donc vers une version autorisée et s'installe
   correctement — il ne sélectionne jamais la version bloquée. Voir
   [quels listings sont filtrés](#which-listings-are-filtered) pour la table
   protocole par protocole.
2. **Son téléchargement renvoie `403 Forbidden`** à tous les clients, quel que
   soit leur rôle, avec le motif que vous avez enregistré. Masquer gouverne la
   version qu'un résolveur *choisit* ; ceci gouverne si quelqu'un qui nomme la
   version explicitement peut l'obtenir. Épingler `lodash@4.17.20` dans un
   fichier de verrouillage échoue avec un message qui dit pourquoi, plutôt que de
   ressembler à un paquet manquant.

Sur un registre `[security]`, le garde-fou de téléchargement est le verdict de la
version : le blocage y est donc écrit sous forme d'un constat `BLOCK_LIST` dès
que vous l'enregistrez, et le scanner `block_list` le redérive à chaque analyse
ultérieure. Même `403`.

Un blocage enregistré contre une version couvre tous ses fichiers — le tarball
npm, un classifier Maven, le binaire d'un provider Terraform.

Bloquer un artefact précis (en passant `artifact`) est délibérément
asymétrique : le garde-fou de téléchargement ne refuse que ce fichier, mais **la
version entière disparaît des listings**. Un résolveur qui sélectionne une
version dont les octets sont partiellement refusés n'a aucun moyen de savoir
quels fichiers il peut obtenir : une version dont un artefact est bloqué n'est
donc pas annoncée comme installable. Qui connaît la coordonnée exacte d'un
fichier voisin non bloqué peut encore le récupérer.

Sur un registre `[security]`, la forme par fichier n'a **aucun effet sur les
téléchargements** : ce garde-fou est le verdict de la version, et un verdict est
indexé sur la version, il ne peut donc pas exprimer « ce fichier mais pas ses
voisins ». Le blocage est tout de même enregistré et masque toujours la version
des listings, et le serveur journalise un avertissement le disant. Bloquez la
version entière quand vous voulez que les octets y soient refusés.

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm", "name": "lodash", "version": "4.17.20", "reason": "CVE-2021-23337"}' \
  http://localhost:8080/api/v1/admin/packages/block
```

#### Quels listings sont filtrés {#which-listings-are-filtered}

Tout registre en mode **local ou hybrid** filtre ses listes de versions, par
l'unique point de passage contre lequel se résout le listing local de chaque
écosystème. Pour les registres **en proxy**, la couverture dépend du protocole —
un listing ne peut être filtré que si le protocole en a un et que le modifier est
sans danger :

<!-- BEGIN listing-coverage: generated by `task docs:listing-coverage`. Do not edit by hand. -->
| Registry | Listing document | Blocked versions hidden |
| --- | --- | --- |
| github | release listings | yes |
| forgejo | release listings | yes |
| gitlab | release listings | yes |
| cargo | sparse index | yes — blocked versions are marked `yanked` rather than removed, which is cargo's own mechanism for "exists, do not select" and keeps lockfile diagnostics honest |
| npm | packument | yes |
| openvsx | extension gallery (`extensionquery`) and the OpenVSX API | yes |
| goproxy | `@v/list` and `@latest` | yes |
| pypi | simple index (HTML and PEP 691 JSON) | yes |
| conda | `repodata.json`, `current_repodata.json` (and their `.zst`/`.bz2` encodings) | yes |
| conda | `channeldata.json` | yes — a blocked newest release drops the package from the channel summary rather than moving it to an older one: channeldata names one version and carries no list to pick a replacement from, so `conda search` stops showing it while `conda install` still resolves it from `repodata.json` |
| composer | p2 metadata | yes |
| vscode-marketplace | extension gallery (`extensionquery`) and the OpenVSX API | yes |
| maven | `maven-metadata.xml` | yes |
| terraform | module and provider versions | yes |
| rubygems | compact index (`/versions`, `/info/{gem}`) | yes — `/versions` describes the whole registry, so a new block reaches it within the blocked-set snapshot's 30-second TTL rather than instantly; `/info` is per-gem and immediate |
| rubygems | versions and gem JSON APIs | yes |
| rubygems | `specs.4.8.gz`, `quick/Marshal.4.8` | no — hiding a version from a Ruby Marshal index would need a Marshal encoder in Rust, and nothing reads it: Bundler resolves from the compact index above, and the JSON APIs answer every other client released this decade |
| nuget | flat index | yes |
| nuget | registration pages | yes — inline pages only; paged registrations pass through, and are logged |
| deb | signed repository indexes | no — editing one invalidates its signature and the client rejects the whole repository, which is a worse failure than the one filtering fixes |
| rpm | signed repository indexes | no — editing one invalidates its signature and the client rejects the whole repository, which is a worse failure than the one filtering fixes |
| pacman | signed repository indexes | no — editing one invalidates its signature and the client rejects the whole repository, which is a worse failure than the one filtering fixes |
| jetbrains | — | no listing document |
| jetbrains-marketplace | `updatePlugins.xml`, `/plugins/list` and the plugin-updates API | yes |
| generic | — | no listing document |
| nodedist | `index.tab` | yes |
| nodedist | `index.json` | yes |
| sdkman | `versions/all` | yes |
| sdkman | `candidates/default` | yes |
| sdkman | the rendered `versions/list` table (`sdk list`) | yes |
<!-- END listing-coverage -->

La table ci-dessus est générée depuis le code Rust et reste en anglais : ses
cellules sont de la prose que `RegistryKind::listing_filter()` possède.

Le filtrage est invisible quand il fonctionne, et c'est exactement là qu'on veut
la preuve qu'il a fonctionné. Le compteur Prometheus
`listing_versions_hidden_total{registry,kind,document}` enregistre combien
d'entrées chaque listing a retirées : « le blocage a-t-il pris effet » se répond
donc depuis l'endpoint de métriques, sans activer les logs de débogage en
production.

::: tip Les index de registre entier peuvent avoir jusqu'à 30 secondes de retard
La plupart des listings sont filtrés contre un ensemble de blocages interrogé à
chaque requête : un blocage y disparaît donc dès l'appel suivant. Trois documents
décrivent un **registre entier** plutôt qu'un paquet, et sont récupérés à chaque
installation :

| Registre | Document |
|---|---|
| conda | `repodata.json`, `current_repodata.json`, `channeldata.json` |
| rubygems | le `/versions` de l'index compact |

Pour ceux-là, l'ensemble des blocages est lu depuis un instantané rafraîchi
toutes les **30 secondes** plutôt qu'interrogé à chaque requête — relire la liste
complète des blocages d'un registre sur le chemin le plus chaud de l'écosystème
coûte plus que les secondes que cela ferait gagner. Un blocage peut donc mettre
jusqu'à une demi-minute à disparaître de l'un d'eux.

**Le `403` sur le téléchargement, lui, n'a jamais de retard**, sur aucun
registre. La fenêtre est donc celle où un client peut encore se voir *proposer*
une version qui lui sera ensuite refusée — l'échec en pleine résolution que cette
fonctionnalité existe pour éviter, réduit à une demi-minute plutôt
qu'éliminé. Les listings par paquet, y compris le `/info/{gem}` de RubyGems et
ses API JSON, sont immédiats.
:::

### Débloquer

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm", "name": "lodash", "version": "4.17.20"}' \
  http://localhost:8080/api/v1/admin/packages/unblock
```

### Bloquer en masse

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages": [{"registry":"npm","name":"bad-pkg","version":"1.0.0"}]}' \
  http://localhost:8080/api/v1/admin/packages/bulk-block
```

### Invalider le cache

Retire l'artefact du cache, de sorte que la requête suivante le récupère à
nouveau en amont :

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm", "name": "lodash", "version": "4.17.21"}' \
  http://localhost:8080/api/v1/admin/packages/invalidate
```

---

## Supprimer une version publiée {#deleting-versions}

Supprimer une version d'un registre `local` ou `hybrid` fait deux choses, et la
seconde surprend :

1. L'**artefact est supprimé**. Les octets ont disparu ; plus rien ne les sert.
2. Le **numéro de version est consommé**. `1.4.0` ne pourra plus jamais être
   publié dans ce registre — ni par vous, ni par personne, ni dans un an.

La seconde est délibérée, et aucun réglage ne la désactive.

::: warning Supprimer puis renvoyer n'est pas une correction
Si vous avez publié des octets défectueux sous `1.4.0`, les supprimer ne libère
pas `1.4.0` pour un envoi corrigé. Publiez `1.4.1`. Si vous voulez que la version
défectueuse cesse d'être installée *sans* consommer son numéro,
[retirez-la](/fr/use/cli) — une version retirée reste résolvable par épinglage
exact et peut être remise.
:::

### Pourquoi un nom n'est jamais réutilisé

Un fichier de verrouillage épingle `1.4.0` et enregistre sa somme de contrôle. Si
supprimer `1.4.0` libérait la coordonnée, une publication ultérieure — par une
autre personne, des mois après, pour d'excellentes raisons — pourrait l'occuper
avec des octets entièrement différents. Tout consommateur qui résout ce fichier
de verrouillage installe alors quelque chose qui partage seulement un nom avec ce
qu'il a relu.

C'est le modèle de npm, et npm a été exploité par ce biais. crates.io et PyPI ont
choisi l'autre, et BatleHub aussi. Cela compte davantage ici qu'en amont : un
registre privé est souvent la *seule* copie de ce qu'il détient, il n'y a donc pas
de seconde source pour remarquer la substitution.

Le mécanisme est une **pierre tombale** : la ligne de la version survit à la
suppression avec un horodatage `deleted_at`, et le chemin de publication la
consulte. Une publication sur une coordonnée consommée est refusée par un
`409` :

```
my-pkg@1.4.0 was published and deleted on 2026-08-27 in registry 'acme-npm';
a published version coordinate is never reused — publish under a new version
```

| | Après suppression de `1.4.0` |
| --- | --- |
| L'artefact | parti du stockage |
| Tous les listings du registre — packument, index sparse, index plat, page Simple, `maven-metadata.xml`, index compact, `@v/list` | `1.4.0` est absent |
| Télécharger `1.4.0` par coordonnée exacte | `404` |
| Republier `1.4.0` | `409`, définitivement |
| Le **nom** du paquet | libre, si toutes les versions ont disparu — voir plus bas |
| La piste d'audit | enregistre qui a supprimé, et quand |

Supprimer toutes les versions de `@acme/widgets` libère le *nom* : une personne
que les autorisations couvrent peut recréer `@acme/widgets`. Les numéros de
version qui ont existé restent consommés. Recréer `@acme/widgets` est permis ;
recréer `@acme/widgets@1.4.0` ne l'est pas.

**Ses propriétaires de paquet partent avec.** Quand la dernière version d'un
paquet est supprimée, toutes les entrées de propriété sur ce nom sont retirées, et
le publieur suivant devient propriétaire du nom qu'il crée. L'alternative est pire
d'une façon qu'on rate facilement : des lignes de propriété indexées par un nom,
survivant au paquet, signifient que l'ancien propriétaire conserve les droits de
publication et de gestion des propriétaires sur un paquet qu'il n'a jamais vu —
et, plus immédiatement, sa ligne périmée *refuse* le nouveau venu qui tente de
prendre le nom libéré. Les pierres tombales des versions, elles, restent, parce
qu'elles sont l'invariant ; les propriétaires partent, parce qu'ils sont une
décision à propos d'une chose qui n'existe plus.

### Supprimer

```sh
batlehub version delete acme-npm my-pkg 1.4.0
```

La commande demande confirmation, à cause du second effet ci-dessus. `-y` la
saute. Le même endpoint accepte une liste :

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages": [{"name": "my-pkg", "version": "1.4.0"}]}' \
  http://localhost:8080/api/v1/admin/registries/acme-npm/bulk-delete
```

Supprimer une coordonnée déjà supprimée, ou qui n'a jamais existé, compte comme
un succès — relancer une suppression en masse à moitié appliquée est sans
danger.

### Lire ce qui a été supprimé

```sh
curl -H "Authorization: Bearer <admin-token>" \
  'http://localhost:8080/api/v1/admin/registries/acme-npm/tombstones?name=my-pkg'
```

```json
{
  "registry": "acme-npm",
  "total": 1,
  "tombstones": [
    {
      "registry": "acme-npm",
      "name": "my-pkg",
      "version": "1.4.0",
      "deleted_at": "2026-08-27T09:14:22+00:00",
      "deleted_by": "alice",
      "published_at": "2026-03-02T11:40:05+00:00",
      "published_by": "ci-runner",
      "checksum": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    }
  ]
}
```

Le paramètre de requête `name` est facultatif ; sans lui, vous obtenez toutes les
pierres tombales du registre, la suppression la plus récente en premier.

On ne peut pas annuler une suppression. Restaurez les octets depuis une
sauvegarde et publiez-les sous un nouveau numéro de version. Si la suppression
est récente, la pierre tombale porte encore la somme de contrôle d'origine : vous
pouvez donc vérifier que ce que vous avez restauré est bien ce qui était là.

### Le compactage des pierres tombales {#tombstone-compaction}

Une ligne de pierre tombale contient deux choses aux durées de vie différentes :

| Partie | Exemple | Durée de vie |
| --- | --- | --- |
| **La revendication** | `(acme-npm, my-pkg, 1.4.0)` | permanente — c'est l'invariant lui-même |
| **Le détail** | métadonnées d'index, somme de contrôle, publieur, signature | historique d'audit |

Seul le détail grossit. Une ligne d'index cargo porte le graphe de dépendances
complet d'une version et un manifeste npm ses scripts et son bloc `dist` — des
kilo-octets chacun — là où la coordonnée en fait une centaine. Le compactage
retire le détail après une fenêtre et garde la revendication.

```toml
[registries.retention]
tombstone_detail_for_days = 730   # retirer le détail au bout de deux ans
dry_run = false                   # vaut true par défaut
```

**Non défini par défaut**, donc rien n'est retiré tant que vous ne le demandez
pas. Un auditeur qui enquête sur une suppression est le lecteur le plus
susceptible d'être surpris par un défaut ici, et le coût de garder le détail est
du disque — récupérable — là où le coût de le perdre est une question à laquelle
on ne peut plus répondre. Référence complète des champs :
[`[registries.retention]`](/fr/guide/configuration).

```sh
curl -X POST -H "Authorization: Bearer <admin-token>" \
  'http://localhost:8080/api/v1/admin/registries/acme-npm/tombstones/compact?dry_run=true'
```

```json
{ "registry": "acme-npm", "compacted": 412, "skipped": 38, "dry_run": true,
  "coordinates": ["my-pkg@1.4.0", "..."] }
```

`409` si le registre n'a pas de `tombstone_detail_for_days` — un registre non
configuré n'est pas une passe qui n'a rien trouvé.

- **`dry_run` vaut `true` par défaut.** Une fenêtre configurée rapporte et ne
  retire rien tant que vous n'avez pas mis `dry_run = false`. Une fois que c'est
  fait, le serveur lève `retention.compaction-live` à chaque rechargement de
  configuration ; c'est voulu, parce que c'est le seul réglage de ce bloc qui
  détruit quelque chose.
- **`?dry_run=` ne peut que rendre une passe plus prudente.** `?dry_run=true`
  prévisualise contre un registre configuré en mode réel ; `?dry_run=false` ne
  remplace *pas* un `dry_run = true` configuré.
- **Le compactage ne touche jamais une version vivante**, et ne touche jamais
  deux fois la même pierre tombale.
- **Il n'existe aucun moyen de supprimer une pierre tombale.** Ni un réglage
  qu'on aurait omis, ni un endpoint derrière un drapeau — le schéma n'a aucune
  représentation pour cela, parce que ramasser une pierre tombale rouvrirait
  précisément le trou qu'elles existent pour fermer.

### Reprendre les versions que personne n'utilise {#retention}

Tout ce qui précède concerne la suppression d'une version *à la main*. La
rétention le fait sur politique — et parce qu'un artefact publié localement est
souvent la seule copie au monde, tous les défauts sont réglés de sorte qu'une
politique fautive garde trop plutôt que trop peu.

```toml
[registries.retention]
keep_versions       = 10
keep_if_pulled_days = 90
dry_run             = false
```

**Une version survit si *n'importe quelle* condition configurée s'applique.** Il
n'y a pas d'expression à écrire ni d'ordre à ne pas se tromper ; la seule façon
de reprendre une version est que toutes les conditions déclinent. Un bloc sans
aucune condition de conservation est refusé au démarrage, parce que c'est celui
qui reprendrait tout dès sa première passe.

`keep_if_pulled_days` est celle qui compte. `keep_versions = 10` seul jette la
version sur laquelle la moitié de votre parc est épinglée, parce qu'elle se
trouve être la onzième par date. Avec le veto sur les pulls, ce que quelqu'un
utilise réellement reste, quel que soit son âge ou son rang — et configurer une
reprise sans lui avertit à chaque rechargement.

Pour la lancer :

```sh
batlehub admin retention acme-npm              # rapport ; ne change rien
batlehub admin retention acme-npm --show-kept  # …et pourquoi chaque survivante survit
batlehub admin retention acme-npm --reclaim    # reprendre réellement
```

`--reclaim` n'est que la moitié du verrou : le registre doit en plus être en
`dry_run = false`. Deux décisions à deux endroits, dont l'une dans un fichier de
configuration que quelqu'un a relu.

```
Retention on acme-npm: dry run — nothing was changed
  examined 1284   kept 1201   reclaimed 83

would reclaim:
  internal-tool@0.1.0
  …
```

Une version reprise est supprimée exactement comme une suppression à la main :
les octets partent, une pierre tombale reste, et **la coordonnée est consommée**.
Libérer du disque ne doit pas libérer le namespace, sans quoi la rétention
devient un mécanisme de chaîne d'approvisionnement par accident.

#### Ce qu'une passe laisse derrière elle {#retention-trail}

| | Passe réelle | Prévisualisation |
| --- | --- | --- |
| Événement de passe, rapporté au registre | `retention_run` | `retention_dry_run` |
| Événement par version | `retention_reclaim` | aucun |
| Pierre tombale | une par version | aucune |

Une passe est déclenchée avec le propre token d'un opérateur : le *sujet* de
l'événement ne permet donc pas de distinguer une politique d'une personne — c'est
l'action qui le fait. `retention_reclaim` n'est jamais `delete`, et signifie
toujours que la version a disparu :

```sh
# Ce que la politique a pris, et ce que quelqu'un a pris à la main
batlehub admin audit-log --registry acme-npm --action retention_reclaim
batlehub admin audit-log --registry acme-npm --action delete
```

Une prévisualisation n'enregistre **qu'elle-même et rien d'autre**. C'est voulu
dans les deux sens : la prévisualisation est la décision d'un opérateur contre un
registre de production et a sa place au dossier, mais une ligne disant qu'une
version a été reprise alors qu'elle est toujours là rendrait la piste illisible.
Ce qu'une prévisualisation *aurait* pris est dans le rapport qu'elle imprime et
nulle part ailleurs — gardez la sortie, ou relancez-la.

#### Épingler une version contre la rétention

L'échappatoire dont toute politique automatique a besoin — la version qu'un
client sous support long terme fait tourner, et que les statistiques de pull
finiront par cesser de défendre :

```sh
batlehub version pin   acme-npm my-pkg 2.4.0
batlehub version unpin acme-npm my-pkg 2.4.0
```

Une version épinglée n'est jamais reprise, quoi que dise la politique. Cela ne
change rien d'autre : la version se résout, se télécharge et se liste exactement
comme avant. Il n'y a délibérément pas d'inverse — aucun moyen de rendre la
rétention *plus* agressive pour une version — parce qu'une politique qui supprime
ne doit pas être atteignable une version à la fois.

#### Lire le signal de téléchargement, et ses trous

`keep_if_pulled_days` compte des **téléchargements**, pas des lectures d'index.
Une résolution `mvn` touche un `.jar`, un `.pom` et une somme de contrôle à côté
de chacun : la somme s'enregistre comme une consultation de métadonnées, le
`.pom` comme un téléchargement, parce qu'un `.pom` est un fichier qu'un build
consomme réellement. Une version maintenue en vie par les seules récupérations de
sommes de contrôle n'est donc *pas* conservée, et une version dont le `.pom` est
encore résolu l'est.

Les chemins d'artefact locaux de Maven et de NuGet n'enregistraient aucun
événement de téléchargement avant le 26 août 2026. La rétention ne lira pas ce
silence comme un abandon : une version sans enregistrement de téléchargement
publiée avant ce plancher est conservée. `download_signal_floor_days` déplace le
plancher si l'historique d'audit de cette instance commence plus tard — après une
restauration, ou après un `audit_purge`.

Une politique `keep_if_pulled_days` sur un déploiement sans dépôt de paquets
refuse de s'exécuter plutôt que de reprendre ce dont elle ne peut pas prouver
l'inactivité.

### Ce n'est pas l'éviction du cache

`[registries.retention]` et les clés d'éviction de `[registries.cache]` se
ressemblent et gouvernent des choses opposées.

| | Éviction de `[registries.cache]` | `[registries.retention]` |
| --- | --- | --- |
| Gouverne | les artefacts du cache proxy | les versions publiées localement et leurs pierres tombales |
| Une autre copie existe | oui, en amont | souvent non |
| Coût d'une reprise erronée | une nouvelle récupération | l'artefact |
| Défaut | configuré par registre | tout garder, pour toujours |
| Prévisualisation | `--dry-run`, sur demande | active sauf `dry_run = false` |
| Audité | par passe | par passe **et** par version |

Un bloc `[registries.retention]` sur un registre en mode `proxy` est une erreur
de configuration, pas une absence d'effet silencieuse : ce registre ne publie rien
localement, le bloc gouvernerait donc un ensemble vide — c'est
[`[registries.cache]`](#cache-policy) que vous vouliez.

Pourquoi cela fonctionne ainsi, et à quoi ressemblera la moitié « reprise » de la
rétention :
[RFC 0016](/rfc/0016-retention-and-the-permanence-of-a-published-name).

---

## Les règles {#rules}

Les règles sont des politiques facultatives, propres à chaque registre, évaluées
après le RBAC.

### Le garde-fou d'âge de publication

Refuse les paquets publiés il y a moins de `min_age_secs` :

```toml
[[registries.rules]]
kind         = "release_age_gate"
min_age_secs = 3600       # 1 heure
bypass_roles = ["admin"]  # les admins peuvent toujours installer un paquet récent
```

### Refuser le tag `latest`

Force les clients à épingler des versions exactes :

```toml
[[registries.rules]]
kind         = "deny_latest"
bypass_roles = ["admin"]
```

### Le publieur de confiance

Restreint les téléchargements aux paquets publiés par une organisation, un
utilisateur ou un scope autorisés. Le publieur est dérivé de métadonnées déjà
résolues pendant la récupération par le proxy — aucun appel amont supplémentaire.

```toml
[[registries.rules]]
kind         = "trusted_publisher"
allow        = ["my-org", "trusted-user"]
bypass_roles = ["admin"]
```

Prise en charge du publieur selon le type de registre (la comparaison est
insensible à la casse) :

- **GitHub**, **GitLab**, **Forgejo** — le segment propriétaire ou groupe de
  premier niveau du chemin du paquet (`"owner/repo"` → `"owner"`)
- **npm** — le scope pour les paquets à scope (`"@scope/name"` → `"scope"`) ;
  sinon l'utilisateur qui publie
- **OpenVSX**, **place de marché VS Code** — le segment d'éditeur de
  l'identifiant d'extension (`"publisher.extension"` → `"publisher"`)
- **Pas encore pris en charge : Cargo** et tout autre type de registre —
  configurer cette règle là-bas refuse toutes les requêtes (échec fermé)

Voir [Configuration](/fr/guide/configuration) pour la table complète des champs.
