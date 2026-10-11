---
sourcePath: registries/devfile.md
sourceHash: fb1d5868a7e42d5f
---

# Registre devfile (Che, odo)

Un registre devfile — `registry.devfile.io` par défaut — est mandaté et mis en cache comme un registre *typé*, afin qu'une version de pile puisse être **bloquée** plutôt que simplement mise en cache. Un registre devfile est le catalogue qu'un environnement de développement en nuage lit pour proposer « démarrer un espace de travail Node.js » : la page *Get Started* d'Eclipse Che le lit pour ses tuiles, et `registry-library` — la bibliothèque qu'embarquent `odo` et les extensions d'IDE — le lit pour récupérer une pile.

Ce qui est servi, et comment :

- **Les documents d'index** — `index`, `v2index`, chacun avec `/sample`, `/stack` et `/all` — sans les versions bloquées. Ils décrivent le registre entier, donc un nouveau blocage les atteint dans la durée de vie de 30 secondes de l'instantané des blocages, comme le `repodata.json` de conda.
- **Le devfile de chaque version de pile**, son **manifeste OCI** et les **couches** que ce manifeste nomme (`devfile.yaml`, `archive.tar`, …), octet pour octet, chacun vérifié contre son empreinte avant qu'un octet ne soit stocké ou servi.
- **Les projets de démarrage**, les zips que récupère `registry-library download`.

Les images de conteneur qu'un devfile nomme **ne sont pas** mandatées : c'est le cluster qui les tire, et ceci n'est pas un registre de conteneurs.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `devfile` |
| **Amont par défaut** | `registry.devfile.io` |
| **Modes** | proxy seul |
| **Adressage** | un paquet par pile, une version par version de pile, un artefact par fichier de celle-ci |
| **Publication privée** | ❌ proxy seul — un registre devfile est construit hors ligne dans une image |
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
mode  = "proxy"                                  # the only mode: no publish protocol
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

## Hors ligne

Une instance déconnectée ([air gap](../operations/air-gap)) sert les piles qu'un paquet a transportées. Transportez chaque version de pile sous la forme de son **manifeste** (`/v2/devfile-catalog/{stack}/manifests/{version}`) et de son **devfile** (`/devfiles/{stack}/{version}`). L'import lit les couches et leurs empreintes dans le manifeste, et les métadonnées affichées et les projets de démarrage dans le devfile. Avec `synthesise_listings` activé, les deux index sont composés à partir de ce qui est détenu :

- chaque version détenue est listée, et la plus haute est celle par défaut ;
- les exemples ne sont pas listés — leur source est un dépôt git distant que l'instance déconnectée n'a pas ;
- `icon` est présent et vide. Les icônes de l'amont sont des URL qu'un navigateur déconnecté ne peut pas atteindre, et Che écarte une entrée d'index qui n'a pas d'`icon` du tout ;
- les filtres `arch` et `deprecated=false` s'appliquent ; les filtres de version de schéma, non.

## Voir aussi

- [RFC 0035](../../rfc/0035-devfile-registry) — le protocole tel que le sert `registry.devfile.io`, et pourquoi l'étiquette est le point de contrôle
- [Miroir générique](./generic) — met en cache un registre devfile sans politique, et ne bloque rien
