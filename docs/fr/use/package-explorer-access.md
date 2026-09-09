---
sourcePath: use/package-explorer-access.md
sourceHash: a072b82b2b8f8eea
---

# Explorateur de paquets — contrôle d'accès

## Contrôle d'accès {#access-control}

### Accès proxy et accès exploration {#access-separation}

Par défaut, tout utilisateur qui peut passer par le proxy sur un registre peut
aussi le parcourir dans l'explorateur. `[registries.rbac.explore]` permet de
restreindre la navigation indépendamment du proxy.

C'est utile quand :

- vous voulez que les tokens de CI/CD puissent télécharger des paquets sans pouvoir énumérer le contenu du registre ;
- vous avez un registre interne sensible qui doit rester accessible à l'outillage mais invisible dans la console.

### Configuration {#rbac-config}

Ajoutez un bloc `explore` à l'intérieur de `[registries.rbac]` :

```toml
[[registries]]
name = "internal-cargo"
type = "cargo"
mode = "hybrid"
upstreams = ["https://index.crates.io"]

[registries.rbac]
user  = ["read"]    # les utilisateurs ordinaires peuvent télécharger via le proxy
admin = ["read"]    # les admins aussi

[registries.rbac.explore]
anonymous = false   # les anonymes ne peuvent pas parcourir
user      = false   # les utilisateurs ordinaires non plus (proxy seul)
admin     = true    # les admins peuvent parcourir
```

Les trois champs valent `true` par défaut : omettre le bloc `explore` (ou l'un de
ses champs) accorde donc la navigation à tout rôle qui a déjà l'accès proxy.

### Ce que « ne peut pas parcourir » recouvre {#rbac-surface}

Tous les endpoints du catalogue, pas seulement la liste. Un rôle privé
d'exploration sur un registre obtient :

- aucune ligne pour ce registre dans la liste des paquets, et aucun compte dans ses statistiques ;
- rien de la recherche dans les README, dont le périmètre est le même ensemble ;
- un `404` sur chacune de ses pages de détail de paquet — y compris la liste des versions amont que la page serait allée chercher pour l'appelant ;
- un `404` sur le README de chacun de ses paquets, et sur toute image contenue dans ce README.

Chacun de ces cas renvoie le même `404` qu'un paquet absent : un refus ne
confirme donc pas que le nom existe.

Le README compte ici parce que c'est la seule réponse du catalogue qui porte de
la prose : il nomme généralement la page d'accueil du projet, ses
dépendances et sa procédure de compilation. Un registre retiré de l'explorateur en est retiré par toutes les
portes.

Le proxy, lui, n'est pas touché. `user = false` sous
`[registries.rbac.explore]` signifie *« ce registre est fait pour les
gestionnaires de paquets, pas pour la lecture »* — un token autorisé à faire un
`GET` sur un artefact le reste, et il résout toujours les documents de protocole
du registre (un packument npm, une page simple PyPI), parce que c'est ainsi que
fonctionne un gestionnaire de paquets, pas une navigation.

### Héritage {#rbac-inheritance}

L'accès à l'explorateur est toujours plafonné par l'accès proxy. Un rôle qui ne
peut pas passer par le proxy sur un registre ne peut pas l'explorer non plus,
quels que soient les drapeaux `explore` :

```txt
accès exploration effectif = accès proxy ET permission d'exploration
```

Les permissions d'exploration ne se configurent pas séparément au niveau des
groupes — les membres d'un groupe héritent de l'accès exploration de leur rôle
(utilisateur ou anonyme).
