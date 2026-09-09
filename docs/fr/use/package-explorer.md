---
sourcePath: use/package-explorer.md
sourceHash: 482acb6321e775b1
---

# Explorateur de paquets

L'explorateur de paquets est un catalogue navigable de tous les paquets que
BatleHub connaît. Il replie toutes les versions d'un paquet sur une seule ligne,
réunit les paquets passés par le proxy et ceux publiés localement, et permet de
chercher des paquets qui ne sont pas encore passés par le proxy en interrogeant
les registres amont en temps réel.

---

## Vue d'ensemble {#overview}

L'explorateur est accessible à tout utilisateur sur `/explore` dans la console.
Il a deux vues :

- **Catalogue** (`/explore`) — une ligne par nom de paquet, sur tous les registres accessibles ou filtré sur un seul. Triable par nombre de téléchargements, par nom ou par dernier accès.
- **Détail d'un paquet** (`/explore/packages/<registry>/<name>`) — toutes les versions connues avec leur source (via proxy ou publiée localement), l'état du pare-feu version par version, et un résumé des garde-fous montrant votre niveau d'accès sur ce registre.

### Sources de données {#sources}

| Source | Où vivent les données |
| --- | --- |
| **Via proxy** | `package_statuses` — chaque paquet jamais demandé à travers le proxy |
| **Local** | `local_packages` — les paquets publiés directement sur un registre BatleHub en mode `local` ou `hybrid` |
| **Amont** | Appel de recherche en direct sur l'API du registre amont (npm, crates.io, RubyGems) quand vous saisissez une requête |

Les versions locales et proxy d'un même paquet sont fusionnées en une seule
entrée dont la source affiche `Les deux`.

---

## Utiliser le catalogue {#catalog}

### Barre latérale des registres {#sidebar}

Le panneau de gauche liste **tous les registres accessibles**, y compris ceux
qu'aucun paquet n'a encore traversés (affichés avec un compte de `0`). Cliquez
sur un registre pour filtrer la table ; cliquez sur **Tous les registres** pour
tout voir.

### Recherche {#search}

Saisissez votre requête dans le champ de recherche. Après 300 ms d'anti-rebond,
deux choses se produisent :

1. La table principale est filtrée par correspondance de sous-chaîne sur le nom du paquet (côté serveur, insensible à la casse).
2. Une **recherche en amont** part vers les registres qui la prennent en charge (voir [Recherche en amont](/fr/use/package-explorer-search#upstream-search)). Les résultats apparaissent en bas de la même table, marqués **amont**.

Une ligne amont est un paquet dont cette instance ne détient rien. Si
l'opérateur a laissé [`console_fetch`](/fr/guide/admin-config#console-fetch)
actif et que le registre est adressable par la seule version, la ligne propose un
bouton nommant la version renvoyée par la recherche amont — **Récupérer
4.17.21** — qui la fait passer par cette instance sous votre propre identité,
garde-fous, quota et ligne d'audit compris. La ligne rejoint alors la moitié
détenue de la table. Là où le bouton n'est pas proposé, la ligne n'en affiche
aucun, et la page du paquet dit pourquoi.

### Tri {#sort}

| Option | Comportement |
| --- | --- |
| Les plus téléchargés | Les paquets au plus grand nombre d'événements d'accès en premier |
| Nom A–Z | Ordre alphabétique du nom de paquet |
| Accédés récemment | Les paquets demandés le plus récemment en premier |

### Colonnes de la table {#columns}

| Colonne | Notes |
| --- | --- |
| **Paquet** | Nom du paquet (en police à chasse fixe). |
| **Registre** | Registre auquel appartient le paquet. |
| **Versions** | Nombre de versions connues (en cache). Pour les lignes purement amont : la dernière version renvoyée par le registre amont. |
| **Téléchargements** | Total des événements d'accès, toutes versions confondues. `—` pour les lignes purement amont. |
| **Source** | `Via proxy`, `Local` ou `Les deux` pour les paquets en cache. Pour les lignes purement amont : la description du paquet, si elle existe. |
| **Proxy** | `Via proxy` (badge plein) pour les paquets déjà en cache ; `Pas encore passé par le proxy` (badge en pointillé) pour les résultats purement amont. |

Un badge `Contient des versions bloquées` apparaît à côté du badge **Source**
lorsqu'au moins une version est actuellement bloquée.

---

## Détail d'un paquet {#detail}

Cliquez sur n'importe quelle ligne en cache du catalogue pour ouvrir la page de
détail du paquet.

### Résumé des garde-fous {#gate}

La carte **Garde-fou d'accès** montre deux vérifications sur votre session
courante :

| Vérification | Vert | Rouge / gris |
| --- | --- | --- |
| **Accès au registre** | Votre rôle peut passer par le proxy sur ce registre | Votre rôle n'a pas accès à ce registre |
| **Canal bêta** | Vous êtes membre du canal bêta — les versions de pre-release sont visibles | Vous n'êtes pas membre ; les versions de pre-release sont masquées |

La carte reflète ce que votre token courant autorise. Si le registre est
accessible mais qu'une version précise est bloquée, cela apparaît dans la colonne
Pare-feu de la table des versions, pas dans la carte.

### Table des versions {#versions}

Chaque ligne est une version du paquet. Colonnes :

| Colonne | Notes |
| --- | --- |
| **Version** | La chaîne de version. Les versions de pre-release (contenant `-`) sont en italique, avec un badge `pre-release`. |
| **Source** | `Via proxy` (depuis le cache amont) ou `Local` (publiée directement). |
| **Pare-feu** | Voir ci-dessous. |
| **Téléchargements** | Total des événements d'accès pour cette version exacte. |
| **Dernier accès** | Horodatage du dernier événement d'accès. |
| **Publiée** | Horodatage `published_at` pour les paquets locaux ; `—` pour les paquets via proxy. |

#### État du pare-feu {#firewall}

| Badge | Signification |
| --- | --- |
| `Libre` | La version est disponible. |
| `Bloquée` | Un administrateur a bloqué cette version. Survolez le badge pour connaître le motif, l'auteur du blocage et sa date. |
| `Retirée` | La version a été retirée après publication (paquets locaux uniquement). |

---

## Pour aller plus loin

Le reste de la documentation de l'explorateur est réparti sur ces pages :

- [Recherche en amont](/fr/use/package-explorer-search#upstream-search) — interroger les registres amont pour des paquets pas encore passés par le proxy, les registres pris en charge, et la configuration de l'URL de recherche.
- [Contrôle d'accès](/fr/use/package-explorer-access#access-control) — séparer l'accès proxy de l'accès à l'explorateur, la configuration RBAC et les règles d'héritage.
- [Cache et API](/fr/use/package-explorer-cache#cache) — le cache en mémoire de l'explorateur, les notes de performance et la référence de l'API REST.
