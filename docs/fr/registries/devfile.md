---
sourcePath: registries/devfile.md
sourceHash: f314861bcad6598e
---

# Registre devfile (Che, odo)

Un registre devfile — `registry.devfile.io` par défaut — est mandaté et mis en cache comme un registre *typé*, afin qu'une version de pile puisse être **bloquée** plutôt que simplement mise en cache. Un registre devfile est le catalogue qu'un environnement de développement en nuage lit pour proposer « démarrer un espace de travail Node.js » : la page *Get Started* d'Eclipse Che le lit pour ses tuiles, et `registry-library` — la bibliothèque qu'embarquent `odo` et les extensions d'IDE — le lit pour récupérer une pile.

Ce qui est servi, et comment :

- **Les documents d'index** — `index`, `v2index`, chacun avec `/sample`, `/stack` et `/all` — sans les versions bloquées. Ils décrivent le registre entier, donc un nouveau blocage les atteint dans la durée de vie de 30 secondes de l'instantané des blocages, comme le `repodata.json` de conda.
- **Le devfile de chaque version de pile**, son **manifeste OCI** et les **couches** que ce manifeste nomme (`devfile.yaml`, `archive.tar`, …), octet pour octet, chacun vérifié contre son empreinte avant qu'un octet ne soit stocké ou servi.
- **Les projets de démarrage**, les zips que récupère `registry-library download`.
- **Vos propres piles**, en mode `local` ou `hybrid` : `batlehub publish` téléverse un répertoire de pile, et le serveur en dérive l'entrée d'index et les objets OCI qu'une récupération lit ([Publication](#publication)).

Les images de conteneur qu'un devfile nomme **ne sont pas** mandatées : c'est le cluster qui les tire, et ceci n'est pas un registre de conteneurs.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `devfile` |
| **Amont par défaut** | `registry.devfile.io` |
| **Modes** | proxy, local, hybride |
| **Adressage** | un paquet par pile, une version par version de pile, un artefact par fichier de celle-ci |
| **Publication privée** | ✅ `batlehub publish <répertoire de pile>` — un paquet défini par ce serveur, l'amont n'ayant pas de protocole de publication |
| **Commutateur client** | Che : `externalDevfileRegistries` ; `registry-library`/`odo` : l'URL du registre |

## Donnez-lui un hôte dédié

`registry-library` résout l'index relativement à l'URL qu'on lui donne, puis demande le manifeste OCI et les couches — ainsi que les projets de démarrage — à la **racine de l'hôte de cette URL**, en abandonnant tout préfixe de chemin. Pointé sur `https://batlehub.example.com/proxy/<registry>/`, il lit l'index puis reçoit un 404 à chaque récupération sur `https://batlehub.example.com/v2/…`.

Liez donc le registre à un hôte ([routage par hôte](../guide/host-routing)), et donnez cet hôte aux clients. Che conserve un préfixe de chemin et fonctionne dans les deux cas ; le serveur journalise un avertissement, et la carte du registre dans la console l'affiche, pour un registre devfile sans hôte.

## Configuration du proxy

Le bloc de registre de votre administrateur :

```toml
[[registries]]
name  = "<registry>"
type  = "devfile"
mode  = "proxy"                                  # or "local" / "hybrid" to publish — see Publishing
hosts = ["devfile.batlehub.example.com"]         # registry-library needs the host root
# upstreams = ["https://registry.devfile.io"]    # the default; one entry only

[registries.rbac]
# Neither client sends a credential — see Authentication below.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list"]
admin     = ["*"]
```

### Eclipse Che

Le tableau de bord lit `index/all` et suit chaque tuile vers `devfiles/{stack}/{version}` :

```yaml
# CheCluster — the dashboard reads index/all and follows each tile to devfiles/…
spec:
  components:
    devfileRegistry:
      externalDevfileRegistries:
        - url: https://batlehub.example.com/proxy/<registry>/
```

Si le `CheCluster` définit `devEnvironments.allowedSources.urls`, ajoutez-y aussi l'URL du registre. Utilisez un nom d'hôte, pas une adresse IP : le résolveur de Che refuse une URL dont l'hôte est une adresse IP privée littérale, avec son propre `403`.

### registry-library et odo

```sh
# The trailing slash matters under a path prefix: the index is resolved
# relative to the URL. The OCI requests always go to the host root.
registry-library pull https://batlehub.example.com/proxy/<registry>/ nodejs:2.2.1 --new-index-schema
odo preference add registry batlehub https://batlehub.example.com/proxy/<registry>/
```

Avec le registre sur son propre hôte, l'URL est la racine de cet hôte — `https://devfile.batlehub.example.com/`.

## Authentification

**Aucun des deux clients ne transporte d'identifiant.** Le tableau de bord de Che récupère l'index par son propre backend, sans `Authorization`, et `registry-library` construit ses requêtes OCI à partir du seul hôte de l'URL : des identifiants placés dans l'URL atteignent l'index et rien au-delà. Le registre doit donc accorder `releases:read` et `releases:list` aux anonymes, et le serveur avertit quand ce n'est pas le cas. Un catalogue qui doit rester privé se garde derrière une frontière réseau, pas derrière un jeton.

## Ce que le blocage fait à un client

- **`registry-library pull pile:version`** d'une version bloquée s'arrête à l'index, avec son propre *« the requested version … does not exist in the registry »*. Aucune requête OCI n'est émise. Un client qui a gardé l'étiquette de la version ou l'empreinte d'une couche et la demande directement est refusé lui aussi : une empreinte n'est servie que si une version encore listée par l'index filtré la nomme.
- **Bloquer la version par défaut d'une pile** déplace `default` dans l'index v2 vers la plus haute version restante : un `pull` sans version fonctionne toujours et récupère celle-ci. L'index historique ne nomme qu'une version par pile, celle par défaut, donc la pile y disparaît.
- **Dans Che**, la tuile de la pile disparaît dès que sa version par défaut est bloquée. Le tableau de bord met l'index en cache **une heure par session de navigateur** : une tuile pour une version bloquée après la dernière lecture d'un utilisateur reste affichée jusque-là ; l'ouvrir est refusé.

::: warning `registry-library` sort avec le code 0 à chaque échec
Chaque erreur rencontrée est affichée et le processus sort avec le code 0, et une couche qui échoue à sa vérification d'empreinte reste sur le disque. Ce registre vérifie chaque couche avant de la servir, et c'est cette vérification qui a des conséquences — mais un script qui enveloppe le client doit lire sa sortie, pas son code de retour.
:::

## Le garde-fou d'âge

Le `lastModified` de l'amont est l'heure de la dernière reconstruction du registre entier — le même instant pour chaque version — donc ce registre ne date aucune version de pile. Une règle `release_age_gate` sur ce registre doit définir `deny_missing_timestamp` : `true` refuse chaque téléchargement, `false` rend le garde-fou inopérant.

## Publication

Un registre `local` ou `hybrid` héberge ses propres piles. L'amont n'a pas d'API de téléversement — `registry-support` construit un registre hors ligne dans une image — : le téléversement est donc un paquet que ce serveur définit, et `batlehub publish` le compose à partir d'un répertoire de pile organisé comme `registry-support` en organise un :

```sh
# my-stack/devfile.yaml            metadata.name and metadata.version name the stack
# my-stack/kubernetes/deploy.yaml  anything else goes into archive.tar
# my-stack/starter-projects/hello.zip   optional, one per starterProjects[*].name
batlehub publish ./my-stack -r <registry>
batlehub publish ./my-stack/devfile.yaml -r <registry>     # the same thing
```

`--name` et `--version` sont refusés : le serveur lit les deux dans le `metadata` du devfile, et nulle part ailleurs. La composition est déterministe — chemins triés, horodatages et propriétaires à zéro — : le même répertoire donne toujours les mêmes empreintes, et une seconde publication de celui-ci est un `409` qui porte sur la version et non sur un horodatage.

Sans la CLI, le paquet est une archive tar non compressée dont les membres sont exactement `devfile.yaml`, un `archive.tar` facultatif (compressé en gzip malgré son nom : `registry-library` le lit comme un `.tar.gz`) et des `starter-projects/{name}.zip` facultatifs :

```sh
curl --data-binary @acme-rust-1.2.0.devfile.tar -H 'Content-Type: application/x-tar' \
  -H "Authorization: Bearer <token>" \
  https://batlehub.example.com/proxy/<registry>/api/stacks
curl -X DELETE -H "Authorization: Bearer <token>" \
  https://batlehub.example.com/proxy/<registry>/api/stacks/acme-rust/1.2.0
```

Tout autre membre — un autre chemin, un répertoire, un lien, un doublon — est un `400` qui le nomme. Une version publiée n'est jamais remplacée.

Ce qu'un client voit d'une version publiée :

- son **entrée d'index** est composée à partir du `metadata` du devfile lors de la publication ; `icon` est `metadata.icon` tel qu'écrit : sur un parc déconnecté, faites-le pointer vers un fichier que cette instance sert (le chemin brut d'un registre `github`, par exemple) ;
- sa **version par défaut** est la plus haute version stable, jamais une préversion tant qu'une version stable existe — publiez un `3.0.0` sous la forme `3.0.0-rc.1` jusqu'à ce que Che doive le proposer ;
- son **manifeste OCI** est construit une fois à la publication puis stocké : son empreinte — celle que renvoie la publication — ne change jamais ;
- un **projet de démarrage** n'est servi que si le paquet en contenait le zip. Che n'utilise pas cette route : un espace de travail clone lui-même le dépôt git du projet.

Gardez les piles publiées `public`. Che lit l'index anonymement : une pile publiée en `internal` ou `team` est absente de *Get Started* ; le serveur avertit pour un registre dont la visibilité par défaut produirait cela.

### En mode hybride, une pile publiée possède son nom

Une pile publiée ici remplace **entièrement** la pile de même nom de l'amont : les entrées de l'amont pour ce nom quittent les deux index, et aucune de ses versions, étiquettes ou empreintes n'est récupérée. Une fusion version par version laisserait un `9.9.9` public devenir la version par défaut que Che propose à la place de la vôtre.

Comme cela remplace la pile pour chaque utilisateur de l'instance, la **première** publication d'un nom que l'amont liste demande `devfile:stack:claim` en plus de `releases:publish` ; les administrateurs le détiennent, et `[registries.rbac]` ou un bloc `[[registries.namespaces]]` peut l'accorder à d'autres. Quand l'amont est injoignable et que son index n'est pas en cache, une telle publication est refusée par un `503` plutôt qu'acceptée. Les versions suivantes d'un nom déjà publié ici ne demandent rien de plus, et supprimer la dernière version libère le nom.

Un nom que l'amont se met à lister *après* sa publication ici conserve la pile locale ; le serveur le journalise une fois et renseigne `batlehub_devfile_shadowed_upstream_stacks{registry}`.

## Hors ligne

Une instance déconnectée ([air gap](../operations/air-gap)) sert les piles qu'un paquet a transportées. Transportez chaque version de pile sous la forme de son **manifeste** (`/v2/devfile-catalog/{stack}/manifests/{version}`) et de son **devfile** (`/devfiles/{stack}/{version}`). L'import lit les couches et leurs empreintes dans le manifeste, et les métadonnées affichées et les projets de démarrage dans le devfile. Avec `synthesise_listings` activé, les deux index sont composés à partir de ce qui est détenu :

- chaque version détenue est listée, et la plus haute est celle par défaut ;
- les exemples ne sont pas listés — leur source est un dépôt git distant que l'instance déconnectée n'a pas ;
- `icon` est présent et vide. Les icônes de l'amont sont des URL qu'un navigateur déconnecté ne peut pas atteindre, et Che écarte une entrée d'index qui n'a pas d'`icon` du tout ;
- les filtres `arch` et `deprecated=false` s'appliquent ; les filtres de version de schéma, non.

Un paquet transporte les piles mises en cache, pas les piles publiées. Publiez une pile locale de chaque côté à partir du même répertoire : la composition étant déterministe, les deux instances servent les mêmes empreintes.

## Voir aussi

- [RFC 0035](../../rfc/0035-devfile-registry) — le protocole tel que le sert `registry.devfile.io`, et pourquoi l'étiquette est le point de contrôle
- [Miroir générique](./generic) — met en cache un registre devfile sans politique, et ne bloque rien
