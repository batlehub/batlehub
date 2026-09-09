---
sourcePath: registries/conda.md
sourceHash: da6a0aa7e1e996e1
---

# Conda

Fait proxy et cache d'un canal conda, ou héberge des paquets conda privés.
BatleHub sert un `repodata.json` par plateforme et le téléchargement des paquets,
sous contrôle du RBAC et du garde-fou d'âge de publication. En mode `hybrid`, les
paquets publiés localement sont fusionnés dans le `repodata.json` de l'amont.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `conda` |
| **Amont par défaut** | `conda.anaconda.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `curl -X POST …/{platform}/` |
| **Coupure réseau** | hors ligne, le `repodata.json` de chaque sous-répertoire est composé à partir des paquets détenus, chaque entrée depuis son propre `info/index.json` lu à l'import ; un sous-répertoire sans rien de détenu répond vide, et `micromamba` s'y résout |

## Mise en place du proxy

Faites pointer conda vers votre registre. Remplacez `<registry>` par le nom de
registre que vous avez configuré :

```yaml
# ~/.condarc  (ou .condarc à la racine du projet)
channels:
  - https://batlehub.example.com/proxy/<registry>
  - nodefaults
```

Un `environment.yml` emploie le même canal :

```yaml
name: myenv
channels:
  - https://batlehub.example.com/proxy/<registry>
  - nodefaults
dependencies:
  - python=3.11
  - numpy
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Le registre doit être en mode `local` ou `hybrid`. Construisez avec
`conda build`, puis envoyez l'artefact en POST dans le répertoire de la
plateforme visée :

```bash
curl -X POST \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-pkg-1.0.0-py311h0_0.tar.bz2 \
  "https://batlehub.example.com/proxy/<registry>/linux-64/"
```

Les formats `.tar.bz2` et `.conda` sont tous deux acceptés. Le nom, la version,
le build et les dépendances sont lus dans l'`info/index.json` de l'archive, et le
`repodata.json` du canal est mis à jour immédiatement.

## Versions bloquées

`repodata.json` et `current_repodata.json` retirent un paquet bloqué dans ses
deux générations — l'entrée `.tar.bz2` sous `packages` et l'entrée `.conda` sous
`packages.conda` — puisqu'un canal sert les deux pour une même publication et que
laisser l'une des deux garderait la version installable.

::: tip Un blocage conda peut prendre jusqu'à 30 secondes
`repodata.json` décrit un canal entier et est récupéré à chaque
`conda install` : son ensemble de versions bloquées est donc lu depuis un
instantané rafraîchi toutes les **30 secondes**, plutôt qu'interrogé à chaque
requête. Conda est le seul registre à porter ce délai, et il ne concerne que le
*listing* — le `403` sur le téléchargement lui-même est immédiat.
:::

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Conda lit automatiquement les identifiants dans `~/.netrc` :

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

## Notes

- Les listes de versions sont synthétisées : BatleHub parcourt le
  `repodata.json` des plateformes standard (`noarch`, `linux-64`, `osx-64`,
  `osx-arm64`, `win-64`) pour assembler l'ensemble des versions disponibles.
- Le garde-fou d'âge de publication se base sur le champ `timestamp` du paquet
  dans `repodata.json`. Les paquets dont les métadonnées amont n'ont pas
  d'horodatage ne peuvent pas être filtrés par âge : le garde-fou est donc sauté
  pour eux.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
