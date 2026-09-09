---
reference: true
sourcePath: guide/access-control.md
sourceHash: 949d02893b382a55
---

# Contrôle d'accès

Deux couches, qui répondent à des questions différentes.

**[Le modèle d'autorisation](#authorization)** décide si un appelant a le droit
d'effectuer une action sur une ressource : un vocabulaire de verbes, accordés à
des sujets, sur une hiérarchie à quatre niveaux. Tout y passe.

**Trois fonctionnalités plus étroites** l'accompagnent, chacune plus ancienne
que le modèle et chacune faisant encore un travail que le modèle ne fait pas :
le [filtrage des pre-releases](#beta-channel), le [blocage par IP](#ip-blocking),
et les
[namespaces d'équipe et la visibilité par paquet](#team-namespaces).

---


Toute requête vers BatleHub est traitée par un seul modèle : un **sujet** demande
à effectuer une **action** sur une **ressource**, et la réponse est oui ou non.

Cette page est la référence de l'opérateur sur ce modèle — les verbes, à qui vous
les accordez, où vous les écrivez, et comment savoir pourquoi quelque chose a été
refusé.

Pour le raisonnement de conception qui sous-tend tout cela, voir la
[RFC 0015](/rfc/0015-grants-on-the-resource-hierarchy).

## Le modèle d'autorisation {#authorization}

### La forme d'une décision {#shape}

Un appelant a besoin de **deux** choses, et elles vont en sens opposés :

| | Dit | Se compose | Direction |
| --- | --- | --- | --- |
| **autorisations** | *ce sujet peut* | union le long du chemin | ne fait qu'élargir |
| **visibilité** | *l'audience est large comme ça* | le plus profond gagne | ne fait que rétrécir |

Les deux doivent passer. Une autorisation `releases:read` ne rend pas public un
paquet en `team`, et un namespace `public` ne sert pas un appelant qu'aucune
autorisation ne couvre.

Il n'y a délibérément **aucune règle de refus**. Une autorisation ne peut jamais
être révoquée par un nœud plus profond, seulement rester sans correspondance — ce
qui signifie qu'une erreur dans un bloc d'autorisations échoue *en se fermant*,
puisqu'une union de rien n'accorde rien.

### Les verbes {#verbs}

L'ensemble est fermé. Un verbe absent de cette liste est une erreur de démarrage,
pas une permission accordée à personne.

| Verbe | Ce qu'il autorise |
| --- | --- |
| `releases:read` | télécharger un artefact |
| `releases:list` | lire une liste de versions ou un document d'index |
| `releases:publish` | publier une nouvelle version |
| `releases:overwrite` | remplacer les octets d'une version existante |
| `releases:yank` | retirer une version et annuler le retrait |
| `releases:delete` | supprimer une version |
| `source:read` | télécharger une archive de sources |
| `catalogue:browse` | utiliser l'explorateur de paquets de la console |
| `owners:read` | voir les propriétaires d'un paquet |
| `owners:write` | les modifier |
| `packages:block` | bloquer administrativement un paquet ou une version |
| `gates:exempt` | exempter une version d'un garde-fou ([plus bas](#exemptions)) |
| `stats:read` | lire les agrégats du tableau de bord |
| `audit:read` | lire le journal d'audit |
| `audit:purge` | supprimer les entrées d'audit antérieures à une coupure |
| `quarantine:read` | voir qu'une version est retenue ou refusée par la couche de chaîne d'approvisionnement, ses codes de motif et sa date de disponibilité ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)) |
| `findings:read` | voir les constats derrière ces codes — identifiants CVE, sortie de scanner, texte SOC |
| `flags:read` | lister les signalements de vulnérabilités poussés par `[[flag_sources]]` — quelle source a dit quoi sur quelle version ([RFC 0002](/rfc/0002-vulnerability-flags-and-exposure)) |

Quatorze autres autorisent les **surfaces de contrôle** — le serveur lui-même
plutôt que ce qui y est publié. Ils ne formaient qu'un seul contrôle
`require_admin` avant d'être séparés : un administrateur les détient donc tous,
et chacun est désormais délégable seul.

| Verbe | Ce qu'il autorise |
| --- | --- |
| `config:read` | lire la configuration en cours |
| `config:write` | la recharger, ou modifier un registre |
| `system:read` | la santé, les métriques, le câblage des notifications |
| `system:write` | modifier ce câblage |
| `blocks:read` | lire les listes de blocage |
| `blocks:write` | les modifier |
| `authz:read` | les diagnostics d'autorisation (`explain`, mode fantôme) |
| `grants:read` | lire les autorisations écrites sur un paquet ou une version |
| `grants:write` | les écrire et les retirer ([Autorisations sur un paquet](#package-grants)) |
| `cache:evict` | retirer des artefacts du cache |
| `cache:warm` | les récupérer à l'avance |
| `quota:read` | lire l'usage des quotas |
| `quota:write` | remettre à zéro les compteurs de quota d'un utilisateur |
| `retention:run` | lancer la rétention, et épingler une version contre elle |
| `tombstones:read` | lire les pierres tombales, et compacter leur détail |
| `packages:read` | la liste administrative des paquets |

Quatre sont **rapportés à un écosystème** et ne s'accordent que sur les types de
registre qui les définissent :

| Verbe | Type de registre | Ce qu'il autorise |
| --- | --- | --- |
| `openvsx:namespace:claim` | `openvsx` | revendiquer un namespace d'éditeur |
| `terraform:signing-keys:write` | `terraform` | enregistrer la clé GPG dont sont signés les providers d'un namespace |
| `jetbrains:channel:assign` | `jetbrains-marketplace` | déplacer un build publié d'un canal de publication à l'autre |
| `npm:dist-tags:write` | `npm` | *réservé* — les dist-tags sont dérivés ici, donc rien ne le demande (l'argument est en [RFC 0015](/rfc/0015-grants-on-the-resource-hierarchy) §4.2) |

::: tip La liste ci-dessus est tout le vocabulaire
Les 35 verbes, confrontés à l'énumération par un test plutôt que maintenus à la
main — une version antérieure de cette table listait trois verbes qui
n'existaient pas, et en copier un dans un fichier de configuration faisait
échouer le serveur au démarrage.
:::

`releases:*` se développe en tous les verbes `releases:` ; `*` se développe en
tout ce que définit l'écosystème du registre. **Le développement a lieu au
chargement de la configuration** : ce qu'un sujet détient est donc un fait à
propos du modèle chargé, plutôt que quelque chose de recalculé à chaque
requête — et `task config:explain` l'imprime.

::: warning `releases:*` n'atteint pas `gates:exempt`
Faire taire un constat de sécurité n'est pas une opération de publication.
`gates:exempt` s'accorde délibérément, ou pas du tout.
:::

### À qui vous accordez {#subjects}

Cinq formes de sujet :

| Forme | Correspond à |
| --- | --- |
| `*` | tout le monde, appelants anonymes compris |
| `role:anonymous`, `role:user`, `role:admin` | les appelants à ce rôle ou au-dessus |
| `group:<provider>:<name>` | les membres de ce groupe chez ce fournisseur d'authentification |
| `group:*:<name>` | ce nom de groupe chez **n'importe quel** fournisseur |
| `group::<name>` | ce nom de groupe sans préfixe de fournisseur |
| `user:<id>` | un principal unique |

Répéter un sujet est une **union**, pas un second avis : deux blocs qui accordent
des verbes différents à `role:user` lui donnent les deux.

**Un token d'accès personnel ne correspond à un sujet `group:` que pour les
groupes avec lesquels il a été créé.** Un token porte un instantané des groupes
de son créateur, restreint à un sous-ensemble choisi à la création — jamais plus
que ce que son propriétaire détient, et jamais re-résolu ensuite, puisqu'un token
n'a pas de session d'où se re-résoudre. Deux conséquences pour une autorisation
que vous écrivez :

- Un token créé sans groupe ne correspond à aucun sujet `group:`, quels que
  soient les groupes de son propriétaire. C'est le défaut, et c'est ce que porte
  tout token émis avant l'existence de l'instantané. Si un pipeline ne voit pas
  quelque chose que son propriétaire voit, c'est la première chose à vérifier :
  `batlehub auth token list` montre ce que chaque token porte.
- Un token garde ses groupes quand son propriétaire les perd. C'est la révocation
  du token qui y met fin, pas le changement chez le fournisseur d'identité — un
  départ s'accompagne donc d'une révocation de tokens, et l'expiration d'un token
  (obligatoire, 90 jours au plus) est la borne extérieure.

`user:<id>` correspond à un token comme à une session : un token se résout vers
l'identifiant de son créateur.

### Où vous les écrivez {#tiers}

Cinq niveaux, du plus extérieur au plus profond :

```
instance                                 (le serveur lui-même)
  └── registre           npm1
        └── namespace     @acme/billing  (apparié, non énuméré)
              └── paquet  @acme/billing/cards
                    └── version 1.4.2
```

Les trois premiers vivent dans le fichier de configuration. Les deux derniers ne
le peuvent pas — un registre de 200 000 paquets ne les énumérera pas en TOML — et
s'écrivent par l'API d'administration.

Le niveau **instance** existe parce qu'une douzaine d'endpoints ne nomment aucun
registre : la configuration, la santé et les métriques, le câblage des
notifications, les listes de blocage, les diagnostics d'autorisation. Il n'y a
pas de registre contre lequel les résoudre : ils se résolvent donc ici. C'est
aussi là que se trouve le plancher administratif.

```toml
# Niveau instance : s'applique au-dessus de tous les registres. C'est le seul
# endroit où une autorisation peut atteindre un endpoint qui ne nomme aucun
# registre.
[grants]
"group:oidc1:sre" = ["system:read", "cache:evict"]

[[registries]]
type = "npm"
name = "npm1"
mode = "local"

# Niveau registre : le défaut de tout ce qui est en dessous.
[registries.grants]
"*"                = ["releases:read", "releases:list"]
"group:*:engineer" = ["releases:publish"]

[[registries.namespaces]]
match      = "@acme/billing"
visibility = "team"

[registries.namespaces.grants]
"group:oidc1:platform" = ["releases:*", "owners:write"]
```

Un namespace est **apparié sur les frontières de segment**, avec le séparateur
propre à l'écosystème : `@acme/billing` ne correspond donc jamais à
`@acme/billing-internal`. Le séparateur est `/` pour les scopes npm et les
modules Go, `.` pour les éditeurs OpenVSX et les identifiants NuGet, `:` pour les
groupId Maven, le canal pour conda, le segment de namespace pour Terraform, le
composant pour deb.

Le séparateur est enregistré **sur l'attribution**, pas dérivé à chaque
recherche : un namespace survit au `type` du registre, et le dériver
re-pointerait silencieusement toutes les attributions existantes le jour où
quelqu'un en changerait un.

::: warning Les namespaces attribués avant cette évolution s'apparient sur `/`
La colonne vaut `/` par défaut, ce à quoi toute attribution s'appariait déjà —
rien ne change donc de sens à la mise à jour. Mais un namespace attribué sur un
écosystème à **point ou à deux-points** (OpenVSX, NuGet, Maven) avant la mise à
jour ne continue de s'apparier qu'à son nom exact : `digital` couvre `digital` et
non `digital.exts`. Ré-attribuez-le pour reprendre le bon séparateur. Les
nouvelles attributions le tiennent du type du registre.
:::

#### Le scellement {#sealing}

`grants = {}` sur un namespace le **scelle** : rien n'est hérité d'au-dessus, et
seul ce qui est écrit sur ce nœud ou en dessous s'applique.

Un bloc absent hérite. Un bloc vide scelle. Ce sont deux états différents, et
cette différence est toute la raison pour laquelle la clé est facultative plutôt
que dotée d'un défaut.

Le scellement est la seule construction du modèle qui retire de l'accès : il est
donc confiné au fichier de configuration — il n'y a aucun moyen de sceller un
paquet par l'API. Un **plancher administratif** survit à tout scellement, de
sorte qu'un sous-arbre scellé n'est jamais un sous-arbre qu'un administrateur ne
peut pas rouvrir. Il siège au niveau `instance` et donne à `role:admin` les
quatorze verbes de contrôle, les deux verbes `audit:`, `stats:read`,
`packages:block`, les deux verbes `owners:`, `releases:yank`, `releases:delete`
et les trois verbes d'écosystème qu'aucun réglage hérité ne traduit.

`gates:exempt` n'est **pas** sur ce plancher, délibérément : c'est le seul verbe
qui fait taire un constat de sécurité, il n'est donc détenu que là où quelqu'un
l'a écrit.

#### Autorisations sur un paquet, ou sur une version {#package-grants}

Les deux niveaux les plus profonds s'écrivent par l'API d'administration plutôt
que par le fichier de configuration, pour la raison que donne la liste des
niveaux : un registre de 200 000 paquets ne les énumérera pas en TOML.

```sh
# tous les verbes du vocabulaire, sur un paquet, pour un groupe
batlehub admin grants set npm1 @acme/billing --subject group:oidc1:eng \
    --actions releases:read,releases:list

# …ou sur une version. `name@version` est la coordonnée, coupée sur le dernier `@`,
# de sorte qu'un nom npm à scope reste un nom de paquet
batlehub admin grants set npm1 @acme/billing@2.4.0-rc.1 \
    --subject group:oidc1:release-managers --actions releases:read

batlehub admin grants list npm1 @acme/billing   # les deux niveaux
batlehub admin grants rm   npm1 @acme/billing --subject group:oidc1:eng
```

La console offre les mêmes contrôles sur la page de détail d'un paquet, sous
**Qui peut atteindre ce paquet**. Les lignes de niveau version y sont affichées,
mais s'éditent depuis la CLI.

Quatre choses à savoir avant d'en écrire une :

- **Les lire et les écrire sont deux verbes distincts**, `grants:read` et
  `grants:write`, détenus par `role:admin` au niveau instance et délégables par
  registre comme tout autre verbe de contrôle. Une liste d'autorisations énumère
  qui peut atteindre un paquet privé : c'est pourquoi elle n'est pas repliée dans
  `audit:read`.
- **`releases:*` est développé au moment où vous l'écrivez**, pas au moment où
  une requête est évaluée. Ce que `set` vous renvoie est l'ensemble stocké —
  vérifiez-le, ce n'est pas toujours ce que vous avez tapé.
- **Les lignes de propriété ne s'éditent pas ici.** Ajouter un propriétaire écrit
  une ligne de niveau paquet portant `releases:publish`, `owners:read` et
  `owners:write` ; une écriture qui en retirerait une est refusée par un `409`
  qui nomme `admin owner rm`. Deux écrivains sur une table, cela va ; deux
  écrivains sur un verbe, c'est une course.
- **Une autorisation de niveau version ne fait qu'ajouter.** Les autorisations
  s'unissent le long du chemin : accorder la lecture de `2.4.0-rc.1` à un groupe
  ne masque donc cette version pour personne d'autre. Ce que cela change, c'est
  l'index de versions servi à un appelant qui détient `releases:list` et **pas**
  `releases:read` : il voit les versions qui lui ont été accordées, et rien
  d'autre. Si toutes les versions sont filtrées, le document est un `404` plutôt
  qu'un index vide — masqué veut dire absent.

### Les autres politiques {#policies}

Les autorisations sont l'une des six choses que porte un niveau. Les autres :

| Politique | Ce qu'elle dit | Se compose |
| --- | --- | --- |
| `grants` | qui peut faire quoi | **union** le long du chemin |
| `visibility` / `prerelease_visibility` | quelle est la largeur de l'audience | le plus profond gagne |
| `versioning` | comment une version peut s'appeler, et si elle peut changer | le plus profond gagne, **en bloc** |
| `quota` | combien peut être publié | le plus profond gagne, en bloc |
| `rules` | quels garde-fous jugent l'artefact | le plus profond gagne, **garde-fou par garde-fou** |
| `retention` | ce qui est conservé ([RFC 0016](/rfc/0016-retention-and-the-permanence-of-a-published-name)) | le plus profond gagne, en bloc |

::: warning `versioning` et `quota` se composent en bloc
Un bloc plus profond **remplace** entièrement celui de son parent. Un namespace
qui omet `enforce_semver` l'abandonne plutôt que d'en hériter.

C'est ce qui rend exprimable « ce paquet-là suit une autre convention de
publication », et c'est un bord tranchant : chaque rechargement avertit d'une
contrainte qu'un niveau plus profond a abandonnée.
:::

`rules` est l'exception et se compose **garde-fou par garde-fou** : un namespace
peut donc régler à nouveau `release_age` sans redéclarer `cve_gate`. Un
remplacement en bloc ferait d'un garde-fou oublié un garde-fou silencieusement
désactivé.

```toml
[[registries.namespaces]]
match = "@acme/ci"

# Les builds de CI internes n'ont pas besoin de quarantaine. Les autres garde-fous
# du registre continuent de tourner — seul `release_age_gate` est remplacé.
[[registries.namespaces.rules]]
kind = "release_age_gate"
min_age_secs = 0
```

#### La visibilité {#visibility}

Quatre valeurs, de la plus large à la plus étroite :

| Valeur | Audience |
| --- | --- |
| `public` | tout le monde, y compris les anonymes |
| `internal` | tout appelant authentifié |
| `team` | les membres du groupe propriétaire |
| `private` | **uniquement les autorisations écrites sur ce nœud ou en dessous** — les autorisations héritées ne s'appliquent pas |

`private` est une valeur de niveau paquet et version. Plus haut, elle ne dit rien
ou duplique un scellement, et le chargement de la configuration l'y refuse.

`prerelease_visibility` est le même réglage pour les seules pre-releases, et
c'est ce que devient `[registries.beta_channel]`. Quand elle n'est pas déclarée,
elle **suit** `visibility` — mettre un paquet en `team` ne laisse pas ses
pre-releases publiques.

#### Immuabilité et ordre {#versioning}

```toml
[registries.namespaces.versioning]
enforce_semver = true
immutable      = "released"   # never | released | always
monotonic      = true
```

`immutable` décide si des octets publiés peuvent être remplacés. `released` est
la forme Maven : un SNAPSHOT bouge, une release non.

::: tip L'immuabilité est une propriété de la ressource, pas de l'appelant
Un remplacement exige à la fois une ressource mutable **et**
`releases:overwrite`. C'est cette séparation qui permet à un namespace d'être en
ajout seul pour *tout le monde, administrateurs compris* — il n'y a
délibérément aucun rôle qui contourne.
:::

`monotonic` refuse une publication dont la version ne se classe pas strictement
au-dessus de la plus récente existante, ce qui attrape la republication d'un
numéro *plus ancien* après une mauvaise release. Une version retirée ou supprimée
compte toujours comme la plus récente : supprimer `2.0.0` ne libère donc pas
`1.9.9` pour qu'on le reprenne.

Un import en masse est incompatible avec `monotonic` par construction, puisqu'un
historique se publie du plus ancien au plus récent. Importez-le désactivé, puis
activez-le.

### Les exemptions de garde-fou {#exemptions}

« Cette CVE ne s'applique pas à l'usage que nous faisons de cette bibliothèque »
est un jugement réel, et sans moyen de le consigner, la seule option est de
désactiver le garde-fou pour tout le registre.

```http
PUT /api/v1/admin/registries/{registry}/policy/version/{package}/{version}/rules/cve_gate
```

```json
{
  "exempt_until": "2026-12-01T00:00:00Z",
  "reason": "GHSA-… — the affected code path is not reachable from our usage"
}
```

**Seuls `cve_gate`, `license_gate`, `security_verdict` et `flags` sont
exemptibles**, et la frontière n'est pas arbitraire : un garde-fou exemptible
rapporte un constat qu'un humain peut *apprécier* — une CVE, une licence, le
verdict d'un scanner, un signalement poussé par un SOC — tandis que tout autre
garde-fou établit un *invariant*. Une quarantaine qu'une version peut esquiver
n'est pas une quarantaine, et un artefact non signé est une absence de preuve
plutôt qu'un constat à accepter.

En écrire une exige **`gates:exempt`**, que rien n'accorde par défaut.
`exempt_until` et `reason` sont tous deux obligatoires, de sorte qu'une exemption
expire d'elle-même — l'échec réaliste n'est pas une mauvaise appréciation, c'est
une bonne appréciation que personne n'a revisitée.

Quand le principal qui accorde une exemption a aussi publié la version, elle est
acceptée et **marquée** `self_approved` plutôt que refusée. Un contrôle à quatre
yeux imposé par l'outil est une friction qu'une petite équipe contourne, le plus
souvent en accordant le verbe plus largement.

### Le mode fantôme {#shadow}

Le réglage de migration, et le plus dangereux de cette page.

```toml
[registries.grants_shadow]
until = "2026-12-01"
```

Un nœud en mode fantôme résout ses autorisations, enregistre ce qu'il **aurait**
refusé, et ne refuse rien. C'est ce qui rend l'adoption du modèle survivable :
activez-le, observez une semaine de trafic réel, puis appliquez.

::: danger Le mode fantôme sur les autorisations laisse passer
Une requête qui serait refusée est **servie**. Oublié, c'est un contournement
d'autorisation configuré exprès.

`until` est obligatoire — un mode fantôme sans expiration ne peut pas s'écrire —
et le chargement de la configuration refuse de démarrer avec une date déjà
passée. Un mode fantôme expiré **applique**.
:::

Chaque rechargement avertit, en nommant chaque nœud et son expiration, et
l'avertissement apparaît sur la page de rechargement de la configuration plutôt
que seulement dans un log. Ce qu'un mode fantôme a servi est sur la
[page d'autorisation](#watching) et dans `batlehub authz shadow`.

`versioning` accepte aussi un `dry_run = true`. Sa direction est plus douce — une
version mal nommée ou en double est acceptée, donc de mauvaises données arrivent
mais rien ne fuit — ce pourquoi il n'a pas besoin d'expiration.

### La surveiller {#watching}

**`/admin/security/authorization`** rassemble les cinq choses qui sont sinon
éparpillées :

| Panneau | Répond à |
| --- | --- |
| **Fantôme** | ce qui est servi et que les autorisations refuseraient, par nœud, avec chaque expiration |
| **Exemptions** | les exemptions de garde-fou vivantes, leur expiration et leur motif, filtrables sur les auto-approuvées |
| **Explain** | résoudre n'importe quel sujet contre n'importe quelle coordonnée, avec la provenance |
| **Refus récents** | ce qui a été réellement refusé |
| **Rétention** | où examiner ce qu'une passe réelle reprendrait |

Trois de ces cinq sont les directions « laisser passer » ou destructives de
fonctionnalités décidées ailleurs. Elles sont sur une même page à dessein :
individuellement, chacune est facile à oublier, et collectivement elles forment
la liste de tout ce qui compte actuellement sur votre mémoire.

#### Depuis un terminal {#cli}

```bash
batlehub authz explain npm1 --subject role:user --action releases:read \
  --package @acme/billing/cards

batlehub authz shadow --detail
```

`explain` répond en nommant **le niveau qui a accordé chaque verbe**, et c'est
toute la différence entre savoir ce qu'un sujet détient et savoir quelle ligne
modifier. Il signale aussi ce qu'il n'a *pas* considéré — la visibilité par
paquet, les garde-fous d'artefact et les couches de blocage se trouvent tous
derrière les autorisations — parce qu'un verdict nu est ambigu entre « rien ne le
refuse » et « rien de ce que j'ai regardé ne le refuse ».

::: tip Un refus sous un mode fantôme le dit
`explain` répond `deny` *et* nomme le nœud qui sert quand même la requête. Sans
cela, le diagnostic contredirait le serveur précisément sur la configuration où
se tromper coûte le plus cher.
:::

Pour la moitié « fichier de configuration » — ce à quoi un bloc se développe
avant toute requête — servez-vous de `task config:explain`.

### Migrer depuis `[registries.rbac]` {#migrating}

`[registries.rbac]` est toujours lu et le restera. Il n'y a pas de jour de
bascule.

Il se traduit en autorisations de niveau registre : `anonymous`, `user` et
`admin` deviennent `*`, `role:user` et `role:admin` ; les entrées de `groups`
deviennent des sujets `group:*:<name>`. Votre configuration existante garde son
sens exact — la traduction est vérifiée contre l'évaluateur précédent sur toutes
les fixtures, toutes les formes de sujet et tous les verbes, plutôt que confiée à
une relecture.

Deux choses à savoir en migrant :

- Un `"*"` dans `[registries.rbac]` signifie *les deux verbes de lecture
  d'aujourd'hui*, pas le nouveau joker. Il se développe en `releases:read`,
  `releases:list`, `source:read` et `catalogue:browse` — jamais en publication ni
  en suppression.
- `[registries.beta_channel]` devient `prerelease_visibility = "team"`, et son
  groupe de membres devient une autorisation de niveau registre.

Commencez par le [mode fantôme](#shadow) si vous réécrivez des autorisations à la
main.

---

## Les trois fonctionnalités plus étroites {#features}

Chacune précède le modèle ci-dessus et chacune fait encore un travail qu'il ne
fait pas :

- **[Canal bêta / de pre-release](#beta-channel)** — réserver les versions de
  pre-release à des utilisateurs ou des groupes approuvés. Dépassée dans son
  expression par `prerelease_visibility` ([plus haut](#visibility)), en quoi un
  bloc `[registries.beta_channel]` se traduit désormais ; le bloc et sa liste de
  membres continuent de fonctionner et restent la façon de gérer
  l'appartenance.
- **[Blocage par IP](#ip-blocking)** — bloquer les adresses abusives, façon
  fail2ban. Orthogonal au modèle : il juge *d'où vient une requête*, ce qui n'est
  ni un sujet, ni une action, ni une ressource.
- **[Namespaces d'équipe et visibilité des paquets](#team-namespaces)** —
  attribuer des préfixes de nom à des groupes du fournisseur d'authentification
  et régler la visibilité par paquet. L'attribution est ce contre quoi
  `visibility = "team"` se résout : les deux sont donc les moitiés d'un même
  mécanisme, pas des alternatives.

---

## Canal bêta / de pre-release {#beta-channel}

### Comment ça marche {#beta-how-it-works}

BatleHub détermine qu'une version est une pre-release à partir de la chaîne de
version elle-même. La règle est [semver](https://semver.org/), après les mêmes
normalisations qu'applique l'*ordre* des versions du serveur — un cœur à deux
composantes est complété et un `v` initial est retiré avant l'analyse :

| Version | Pre-release ? | |
|---------|-------------|---|
| `1.0.0` | Non | |
| `1.0.0-beta.1` | **Oui** | |
| `1.0.0-rc.2` | **Oui** | |
| `1.0.0-alpha` | **Oui** | |
| `1.0-SNAPSHOT` | **Oui** | l'orthographe de Maven ; complétée en `1.0.0-SNAPSHOT` avant l'analyse |
| `1.0.0rc1` | **Oui** | la PEP 440 attache son marqueur sans séparateur |
| `dev-main`, `1.x-dev` | **Oui** | alias de branche de développement Composer |
| `2.0.0+build-1` | Non | des métadonnées de build ne sont pas une pre-release |

::: warning Cela a changé à la phase 4 de la RFC 0015
Il existait auparavant deux définitions de « pre-release » dans le code, et elles
divergeaient — l'une appelait `1.0-SNAPSHOT` une release, l'autre appelait
`2.0.0+build-1` une pre-release. Elles n'en font plus qu'une, celle ci-dessus.

Deux conséquences à la mise à jour : une version de forme SNAPSHOT devient
filtrée par le canal bêta là où elle ne l'était pas, et la table des versions de
la console étiquette ces lignes correctement. Rien ne devient *plus* visible.
:::

Il n'y a **ni drapeau ni étape de publication séparée** — c'est la chaîne de
version elle-même qui décide du filtrage. Publiez `mylib@1.0.0-beta.1` comme
n'importe quelle autre version ; BatleHub déduit du suffixe `-beta.1` que c'est
une pre-release.

Quand `beta_channel.enabled = true` sur un registre :

- **Les non-membres** — les versions de pre-release sont masquées des listes de
  versions, et le téléchargement des artefacts renvoie 404.
- **Les membres** — les versions de pre-release sont visibles et téléchargeables
  à côté des versions stables.

Les versions stables restent visibles de tous, quelle que soit l'appartenance.

### Configuration {#beta-config}

Ajoutez un bloc `[registries.beta_channel]` à tout registre en mode `local` ou
`hybrid` :

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.beta_channel]
enabled = true
```

`enabled` est la seule option. Les membres se gèrent à l'exécution, par l'API
d'administration.

Omettre le bloc (ou mettre `enabled = false`) rend toutes les versions visibles
de tous.

### Gérer les membres {#beta-members}

Tous ces endpoints exigent un token de rôle `Admin`.

#### Lister les membres

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel
```

```json
[
  { "principal_type": "user",  "principal_id": "alice",   "granted_by": "admin" },
  { "principal_type": "group", "principal_id": "qa-team", "granted_by": null }
]
```

#### Ajouter un membre

`principal_type` vaut `"user"` ou `"group"`. Une entrée `"group"` accorde l'accès
à tout utilisateur portant ce claim de groupe (par OIDC ou par l'authentification
Kubernetes).

```sh
# Ajouter un utilisateur précis
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"user","principal_id":"alice","granted_by":"admin"}' \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel

# Ajouter un groupe entier
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"group","principal_id":"qa-team"}' \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel
```

Renvoie `204 No Content` en cas de succès, `409 Conflict` si le principal est
déjà membre.

#### Retirer un membre

```sh
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel/user/alice

curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel/group/qa-team
```

### Ce que voient les utilisateurs {#beta-user-experience}

#### En non-membre

```sh
# npm — seules les versions stables sont listées
npm view my-package versions --registry https://batlehub.example.com/proxy/my-npm
# [ '1.0.0', '1.1.0' ]

# Tenter d'installer une pre-release → 404
npm install my-package@1.0.0-beta.1 --registry https://batlehub.example.com/proxy/my-npm
# npm error 404 Not Found
```

#### En membre

```sh
# Toutes les versions listées, pre-releases comprises
npm view my-package versions --registry https://batlehub.example.com/proxy/my-npm
# [ '1.0.0', '1.0.0-beta.1', '1.0.0-rc.2', '1.1.0' ]

npm install my-package@1.0.0-beta.1 --registry https://batlehub.example.com/proxy/my-npm
# added 1 package
```

### Prise en charge par registre {#beta-registries}

Le filtrage ne s'applique qu'en **mode local et hybrid** — un registre en proxy
seul relaie l'amont tel quel.

| Registre | Listing filtré | Téléchargement filtré |
|----------|:------------:|:--------------:|
| npm | ✓ | ✓ |
| Cargo | ✓ | ✓ |
| Modules Go | ✓ | ✓ |
| RubyGems | ✓ | ✓ |
| Maven | ✓ | ✓ |
| Modules Terraform | ✓ | ✓ |
| Providers Terraform | ✓ | ✓ |
| PyPI | ✓ | ✓ |
| Conda | ✓ | ✓ |

::: warning Maven et les versions non semver
Les versions Maven qui ne sont pas du semver valide (par exemple
`1.0-SNAPSHOT`) ne sont jamais traitées comme des pre-releases et restent
toujours visibles. Filtrer les SNAPSHOT demanderait une fonctionnalité à part.
:::

::: tip Détection des pre-releases PyPI et Conda
Pour **PyPI**, les versions de pre-release de la PEP 440 (suffixes `.aN`, `.bN`,
`.rcN`) sont détectées à partir de la chaîne de version — sans exiger de semver.
Pour **Conda**, la détection emploie la même heuristique sur la chaîne (toute
version contenant `alpha`, `beta`, `rc`, `dev`, ou une composante de pre-release
semver).
:::

---

## Blocage par IP {#ip-blocking}

### Comment ça marche {#ip-how-it-works}

BatleHub compte les événements de violation par adresse IP dans une fenêtre de
temps glissante. Quand le compte dépasse le seuil configuré, l'IP est bloquée
automatiquement pour la durée configurée.

Une **violation** est toute réponse dont le code de statut figure dans
`trigger_on_status` (par défaut 429 et 401). Ce qui signifie :

- des limites de débit atteintes à répétition → les violations s'accumulent →
  blocage automatique ;
- des tentatives d'authentification en force brute → les violations
  s'accumulent → blocage automatique.

Une IP bloquée reçoit `403 Forbidden` avec un en-tête `X-Block-Expires` portant
l'horodatage Unix de la levée du blocage. Le contrôle a lieu **avant
l'authentification**, de sorte qu'une IP bloquée ne consomme aucune ressource
d'authentification.

Le magasin laisse passer en cas de panne : s'il est indisponible, les requêtes
sont autorisées plutôt que bloquées durement.

### Configuration {#ip-config}

Ajoutez une section `[ip_blocking]` à la **racine** de `config.toml` (pas à
l'intérieur d'un bloc `[[registries]]`) :

```toml
[ip_blocking]
enabled               = true
violation_threshold   = 10       # violations avant blocage automatique
violation_window_secs = 300      # fenêtre de comptage (5 minutes)
ban_duration_secs     = 3600     # durée du blocage (1 heure)
trigger_on_status     = [429, 401]
```

| Champ | Défaut | Description |
|-------|---------|-------------|
| `enabled` | `false` | Activer le blocage par IP |
| `violation_threshold` | `10` | Violations dans la fenêtre avant blocage automatique |
| `violation_window_secs` | `300` | Durée de la fenêtre, en secondes |
| `ban_duration_secs` | `3600` | Durée d'un blocage automatique |
| `trigger_on_status` | `[429, 401]` | Codes de statut HTTP comptés comme violations |

Seul `enabled = true` est obligatoire ; tous les autres champs ont des défauts
raisonnables.

::: tip Derrière un répartiteur de charge
Si BatleHub est derrière un proxy, les vraies IP clientes arrivent par
`X-Forwarded-For`. BatleHub emploie la **première** IP de cet en-tête.
Assurez-vous que votre répartiteur de charge le pose correctement et retire toute
valeur fournie par le client, pour empêcher l'usurpation.
:::

### Gestion manuelle des blocages {#ip-admin}

Tous ces endpoints exigent un token de rôle `Admin`.

#### Lister les IP bloquées

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/ip-blocks
```

```json
[
  {
    "ip":         "1.2.3.4",
    "blocked_at": 1748304000,
    "unblock_at": 1748307600,
    "reason":     "auto"
  }
]
```

#### Bloquer une IP à la main

```sh
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"ip":"1.2.3.4","reason":"known bad actor","duration_secs":86400}' \
  https://batlehub.example.com/api/v1/admin/ip-blocks
```

| Champ | Obligatoire | Description |
|-------|:--------:|-------------|
| `ip` | Oui | L'adresse IP à bloquer |
| `reason` | Non | Conservé à des fins d'audit |
| `duration_secs` | Non | Vaut `3600` par défaut |

#### Débloquer une IP

```sh
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/ip-blocks/1.2.3.4
```

Le blocage automatique reprendra si l'IP continue de déclencher des violations
après avoir été débloquée.

### Backends de stockage {#ip-storage}

Les compteurs de violations et les enregistrements de blocage partagent le
backend choisi par `config.cache.cache_type` :

| `cache_type` | Stockage | Survit au redémarrage | Partagé entre instances |
|-------------|---------|:---------------:|:----------------------:|
| `memory` (défaut) | Dans le processus | Non | Non |
| `postgres` | Tables `ip_violation_counters` et `ip_blocks` | Oui | Oui |
| `redis` | Clés avec TTL | Oui (si Redis persiste) | Oui |

Employez `postgres` ou `redis` en production, pour que les blocages survivent aux
redémarrages et s'appliquent de façon cohérente sur plusieurs réplicas BatleHub.

---

## Combiner les deux fonctionnalités {#combining}

Les deux fonctionnalités sont indépendantes et se marient bien. Une mise en place
courante pour un registre privé :

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.rate_limit]
requests_per_window = 100
window_secs         = 60
enforcement         = "block"

[registries.beta_channel]
enabled = true

[ip_blocking]
enabled               = true
violation_threshold   = 10
violation_window_secs = 300
ban_duration_secs     = 3600
trigger_on_status     = [429, 401]
```

Le déroulé :
1. La limitation de débit bloque les requêtes excessives → le 429 compte comme
   une violation.
2. Les échecs d'authentification (401) comptent aussi → les tentatives en force
   brute bloquent automatiquement l'IP source.
3. Les publications bêta ne sont visibles que des utilisateurs ou groupes ajoutés
   par l'API d'administration.

---

## Namespaces d'équipe et visibilité des paquets {#team-namespaces}

### Comment ça marche {#ns-how-it-works}

Un **namespace d'équipe** associe un préfixe de nom de paquet à un groupe du
fournisseur d'authentification. Une fois attribué, seuls les membres de ce groupe
— et les admins — peuvent publier des paquets dont le nom commence par `prefix`
ou `prefix/`.

**Exemple :** attribuer le préfixe `frontend` au groupe `oidc:frontend-team`
réserve aux membres de ce groupe la publication de `frontend/utils`, de
`frontend/components` et de tout paquet nommé exactement `frontend`. Publier
`backend/api` n'en est pas affecté.

Les groupes ne se gèrent pas dans BatleHub. L'appartenance est lue dans le claim
`groups` que délivre le fournisseur d'authentification configuré (OIDC,
Kubernetes ou token statique) à chaque requête — aucune synchronisation séparée
n'est nécessaire.

La **visibilité d'un paquet** gouverne qui peut le _télécharger_, indépendamment
de qui l'a publié :

| Visibilité | Qui peut télécharger |
|------------|-----------------|
| `public` (défaut) | Tout le monde, y compris les utilisateurs non authentifiés |
| `internal` | Tout utilisateur authentifié |
| `team` | Les membres du groupe propriétaire du namespace |

La visibilité porte sur le **paquet** — toutes ses versions partagent le même
réglage. Une nouvelle version publiée hérite automatiquement de la visibilité
existante. Les admins passent toujours outre les contrôles de visibilité.

Aucune configuration TOML n'est nécessaire. Les attributions de namespace et la
visibilité se gèrent entièrement à l'exécution, par l'API d'administration.

### Gérer les attributions de namespace {#ns-claims}

Tous ces endpoints exigent un token de rôle `Admin`.

#### Lister les attributions

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces
```

```json
[
  { "registry": "internal-npm", "prefix": "frontend", "group_id": "oidc:frontend-team", "claimed_by": "admin" },
  { "registry": "internal-npm", "prefix": "backend",  "group_id": "oidc:backend-team",  "claimed_by": null }
]
```

#### Attribuer un namespace

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"prefix":"frontend","group_id":"oidc:frontend-team","claimed_by":"admin"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces
```

| Champ | Obligatoire | Description |
|-------|----------|-------------|
| `prefix` | Oui | Le préfixe de nom de paquet (sans barre oblique finale). Peut en contenir : `org/team`. |
| `group_id` | Oui | Le nom du groupe tel qu'il apparaît dans le claim du fournisseur, par exemple `oidc:frontend-team`. |
| `claimed_by` | Non | Note en texte libre ; en général l'admin qui a créé l'attribution. |

Renvoie `204 No Content` ; `409 Conflict` si le préfixe est déjà attribué.

#### Libérer une attribution

Un préfixe contenant des barres obliques est passé tel quel dans le chemin de
l'URL :

```sh
# Préfixe simple
curl -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/frontend

# Préfixe contenant une barre oblique
curl -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/org/team
```

Renvoie `204 No Content` même si l'attribution n'existait pas.

### La visibilité d'un paquet {#ns-visibility}

#### Lire la visibilité courante

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility
```

```json
{ "visibility": "public" }
```

:::tip Encodage d'URL
Un nom de paquet contenant des barres obliques doit être encodé en pourcent dans
l'URL : `/` → `%2F`.
:::

#### Régler la visibilité

```sh
# Équipe uniquement
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"team"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# Tout utilisateur authentifié
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"internal"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# Rétablir l'accès public
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"public"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility
```

Valeurs acceptées : `public`, `internal`, `team`. Renvoie `204 No Content` ;
`404` si le paquet n'a jamais été publié ; `400` sur une valeur inconnue.

#### L'application au téléchargement

Quand une requête arrive pour un paquet dont la visibilité n'est pas publique,
BatleHub évalue dans cet ordre :

1. **Admin ?** → autoriser.
2. **`public` ?** → autoriser.
3. **`internal` ?** → autoriser si l'appelant a au moins le rôle `User`
   (c'est-à-dire s'il est authentifié).
4. **`team` ?** → autoriser si les claims de groupe de l'appelant incluent le
   groupe propriétaire du namespace. Si aucun claim n'est trouvé, refuser tout
   accès non administrateur.

Le même contrôle s'applique à tous les chemins d'accès : téléchargement
d'artefact, réponses d'index et de métadonnées, listes de versions. Un
utilisateur qui ne peut pas télécharger un paquet ne le voit pas non plus dans
`npm view`, `cargo search`, etc.

### Prise en charge par registre {#ns-registries}

Les namespaces d'équipe et la visibilité s'appliquent à tous les types de
registre en mode `local` ou `hybrid` :

| Registre | Exemple de préfixe |
|----------|---------------|
| npm | `@scope` ou `team/` |
| Cargo | `my-prefix/` ou un nom de crate exact |
| Modules Go | `github.com/org/` |
| RubyGems | `my-gem` |
| Maven | `com.example.group:` |
| Modules Terraform | `namespace/module/provider` |
| Providers Terraform | `namespace/type` |
| Composer | `vendor/` |
| OpenVSX / VSIX | `publisher.name` |
| PyPI | `my-org-` (préfixe de nom de paquet) |
| Conda | `my-org-` (préfixe de nom de paquet) |

Les préfixes sont appariés par une **règle du préfixe le plus long** : si
`frontend` et `frontend/ui` sont tous deux attribués, `frontend/ui/button` relève
de l'attribution `frontend/ui`.

### Le tableau de bord de namespace pour l'utilisateur {#ns-user-dashboard}

Une fois les attributions en place, les utilisateurs gèrent leurs propres paquets
sans accès administrateur. La page **Namespace d'équipe** (`/my-namespace` dans
la console) permet aux membres d'un groupe :

- de voir tous les préfixes de namespace que leurs groupes possèdent, sur tous
  les registres ;
- de parcourir les versions publiées et de changer leur visibilité sur place ;
- de téléverser de nouveaux paquets par un formulaire du navigateur (pris en
  charge pour RubyGems, Composer, OpenVSX, les modules Go, PyPI et Conda), ou de
  copier les instructions CLI pour les autres types de registre.

::: tip Normalisation des noms de groupe
Les espaces des noms de groupe sont retirés avant comparaison — `"oidc:my team"`
et `"oidc:myteam"` sont traités comme le même groupe. Déclarez `group_id` sans
espaces à la création d'une attribution, pour éviter toute ambiguïté.
:::

Voir la
[section du tableau de bord de namespace dans le guide utilisateur](/fr/use/#team-namespace)
pour les instructions destinées à l'utilisateur final.
