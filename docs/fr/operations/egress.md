---
sourcePath: operations/egress.md
sourceHash: e2f435454ac941c6
---

# Ce qui sort de cette instance

Pour l'opérateur qui doit répondre à *à quoi cette machine parle-t-elle, et
quand*.

Un proxy de cache existe en partie pour que les postes des développeurs cessent
de parler à Internet. Cela ne tient que si vous savez à quoi le proxy lui-même
parle, et ce qui l'y pousse. Cette page est la liste.

Rien ici n'est une *infrastructure* sortante nouvelle : toutes les requêtes
ci-dessous vont vers un amont de registre que vous avez configuré, par le même
client HTTP, avec les mêmes délais, les mêmes réglages TLS, la même
authentification amont et les mêmes garde-fous SSRF que n'importe quelle autre
récupération. Ce qui distingue les entrées, c'est **ce qui déclenche la
requête**.

## Un build demande quelque chose

Le cas ordinaire, et la raison d'être du logiciel. Un gestionnaire de paquets
résout une version ou télécharge un artefact, le proxy n'a ni l'un ni l'autre en
cache, et il récupère depuis l'amont configuré.

Pour désactiver : ne faites pas tourner de registre en mode proxy —
`mode = "local"` ne consulte jamais aucun amont.

**Un type de registre récupère depuis des hôtes que vous n'avez pas
configurés.** Un registre `sdkman` demande un téléchargement à
`broker.sdkman.io` et reçoit un `302` vers l'endroit où l'éditeur publie —
`github.com` (et `objects.githubusercontent.com`), `repo.maven.apache.org`,
`services.gradle.org`, `groovy.jfrog.io` au minimum. La chaîne est suivie côté
serveur à travers le garde-fou SSRF et les identifiants de l'opérateur s'arrêtent
aux deux origines configurées, mais la sortie réseau vers ces hôtes de CDN est un
prérequis de ce type. La liste est *observée, pas exhaustive* : le courtier peut
ajouter un hôte sans le dire à personne, ce qui est en soi un argument pour
préchauffer avant une coupure réseau
([RFC 0010](/rfc/0010-toolchain-managers) §9).

## On saisit quelque chose dans un champ de recherche

`GET /api/v1/explore/upstream` diffuse une requête vers l'API de recherche amont
de chaque registre accessible. **La requête en texte libre de l'utilisateur est
transmise en amont**, ce qui, pour un opérateur dont le modèle de menace est
« cette machine ne dit à personne ce que nous cherchons », est une divulgation
qu'il vaut mieux connaître.

Pour la désactiver, registre par registre : `search_url = ""`.

Une requête demande à chaque registre au plus 100 résultats — le plafond du
paramètre `limit` — de sorte que la sortie réseau qu'une recherche peut causer
est bornée par le nombre de registres que l'appelant peut parcourir, pas par ce
qu'il demande.

## La lecture de découverte de la console {#the-console-s-discovery-read}

**Comportement récent, actif par défaut.** Quand quelqu'un ouvre la page d'un
paquet dont cette instance ne détient rien, BatleHub demande à l'amont du
registre quelles versions existent — un document de métadonnées, le même
qu'aurait provoqué le premier `npm install` de ce paquet.

Avant cela, la page disait *« aucune version pour l'instant »* d'un paquet dont
la recherche de la console venait d'annoncer l'existence au lecteur. C'est le
défaut que cela corrige, et c'est aussi le seul défaut du changement qui modifie
le trafic sortant d'une instance sans que l'opérateur l'ait demandé. Il est nommé
ici plutôt que laissé à découvrir dans un graphe de trafic.

**Ce que cela ne fait pas.** Regarder un paquet n'est pas le télécharger. La
lecture récupère un document de métadonnées et rien d'autre :

- aucun artefact n'est récupéré, et aucune entrée de stockage n'est créée ;
- aucune ligne `package_statuses` n'est écrite : le paquet n'apparaît donc pas
  dans `GET /api/v1/explore/packages` parce que quelqu'un l'a regardé ;
- aucun événement d'accès n'est enregistré, aucun compteur de téléchargement ne
  bouge, aucun `last_accessed` n'est touché ;
- aucun quota n'est consommé, et la comptabilité du service d'éviction ne change
  pas.

Une consultation de page ne doit pas pouvoir changer ce que le catalogue prétend
que cette instance détient — sinon, naviguer dans la console réécrirait
silencieusement l'inventaire que vous lisez pour décider.

**Ce qui la borne.**

| Borne | Effet |
| --- | --- |
| Cache d'abord | Le document atterrit dans le cache de métadonnées, sous la clé qu'emploie déjà le chemin proxy : il obéit donc au `metadata_ttl_secs` du registre. N consultations dans un même TTL produisent une requête. |
| …y compris pour les galeries | Open VSX, les places de marché VS Code et JetBrains, et conda n'ont pas de document de listing à récupérer et répondent par *requête*. Cette réponse est mise en cache au même endroit, sous le même TTL, derrière la même coalescence — ce qui n'était pas le cas avant : chaque consultation de page était une requête amont, et pour conda un `repodata.json` par plateforme de canal. |
| Coalescence | Dix personnes ouvrant le même paquet nouveau en même temps produisent une requête amont, pas dix. |
| Cache négatif | Un `404` de l'amont est mémorisé pendant `negative_ttl_secs` (300 par défaut), de sorte qu'une mauvaise URL ou un robot ne fasse pas de chaque rechargement une requête. Un *échec de connexion* n'est pas mémorisé — ce n'est pas un fait à propos du paquet. |
| Accès au registre | La lecture n'a lieu que pour un registre que l'appelant peut déjà explorer. Quelqu'un qui ne voit pas un registre ne peut pas lui faire émettre de trafic. |
| Limitation de débit | Le `rate_limit` du registre s'applique par-dessus, pour un appelant qui énumérerait des noms *différents*. |

**Un nom privé n'est jamais envoyé en amont.** Si le backend local héberge le
paquet, la lecture est entièrement supprimée — quel que soit le mode. Sur un
registre `hybrid`, un paquet privé partage un espace de noms avec un index
public, et envoyer son nom là-bas à chaque consultation divulguerait l'existence
d'un logiciel interne à un tiers. Cela inviterait aussi une réponse de confusion
de dépendances, où la page montrerait les versions amont d'un nom qui signifie
autre chose ici.

**Pour la désactiver.** Registre par registre :

```toml
[registries.upstream_detail]
enabled = false
```

La page répond alors depuis les lignes locales exactement comme avant, sans
tentative et sans bandeau. C'est le bon réglage pour un
[parc coupé du réseau](/rfc/0008-mise-in-an-air-gapped-estate) : sans route vers
l'extérieur, la lecture échouerait une fois par TTL et la page dirait que l'amont
n'a pas pu être joint, ce qui est un résultat pris en charge mais bruyant quand
c'est l'état permanent du monde.

## Les images d'un README

**Aucune par défaut.** Les images d'un README ne sont pas chargées : elles vivent
en général sur des hôtes tiers, et les rendre signifierait qu'à chaque
consultation de page de la console, une requête part — avec un `Referer` — vers
un hôte choisi par l'*auteur du paquet*, annonçant que quelqu'un de votre réseau
lit à propos de ce paquet à cet instant.

Chaque image est remplacée par une pastille en ligne portant son texte
alternatif et son hôte, pour qu'un lecteur voie qu'il y avait une image et vers
où elle pointait.

Avec `remote_images = "proxy"`, c'est **ce serveur** qui les récupère, une fois,
et les sert depuis sa propre origine. Le navigateur du lecteur ne parle toujours
pas à un hôte choisi par l'auteur du paquet ; ce qui change, c'est que cette
instance le fait, au premier rendu de chaque image. Les requêtes sont celles de
ce serveur, coalescées et mises en cache sous le `metadata_ttl_secs` du registre,
et une URL qui échoue est mémorisée pour qu'un badge mort ne soit pas recomposé à
chaque consultation.

Le résidu mérite d'être nommé plutôt que caché : un auteur de paquet choisit
toujours *à quel hôte ce serveur parle*, et peut donc apprendre que **quelqu'un**
sur cette instance a rendu son README, ainsi que l'IP de sortie de cette
instance. Ce qu'il ne peut pas apprendre, c'est qui, à quelle fréquence, ni
depuis quelle adresse interne. C'est une vraie réduction et pas une élimination ;
un opérateur pour qui même cela est inacceptable garde `"strip"`, qui reste le
défaut. Voir
[`remote_images`](/fr/guide/admin-config#readme-capture).

## Quelqu'un appuie sur Récupérer {#someone-presses-fetch}

La page d'un paquet liste les versions dont cette instance ne détient rien et
marque chacune **non détenue ici**. Sur ces lignes se trouve un bouton
**Récupérer cette version**, et l'actionner télécharge l'artefact depuis l'amont.
Le catalogue propose le même bouton sur un résultat de recherche dont l'instance
ne détient rien, pour la version que la recherche amont a nommée.

C'est la seule chose de cette liste qui soit **une décision plutôt qu'un effet de
bord**. Tout le reste ici arrive parce qu'une page a été ouverte ou qu'un build a
tourné ; ceci arrive parce qu'une personne a appuyé sur un bouton, et le journal
d'audit la nomme.

C'est le chemin de téléchargement ordinaire — la même requête qu'aurait faite un
gestionnaire de paquets, sous l'identité de l'appelant, à travers les règles, le
contrôle d'intégrité, le quota et l'audit. Ce n'est pas une tâche de
préchauffage et cela n'emploie pas le service de préchauffage, qui contourne tout
cela parce que son seul appelant est un administrateur.

Actif par défaut, registre par registre, et inerte sur un registre en mode
`local`. Désactivez-le avec `console_fetch = false` si vous voulez une console
strictement en lecture. Voir
[récupérer une version depuis la console](/fr/guide/admin-config#console-fetch).

## Un README référencé par lien {#a-linked-readme}

OpenVSX et la place de marché VS Code donnent une *URL* pour le README d'une
extension, plutôt que le texte. La suivre est une requête sortante, faite dans
une tâche de fond plutôt que sur le chemin de requête qu'attend un gestionnaire
de paquets, et seulement pour une version que cette instance met en cache. L'URL
est vérifiée comme étant sur la même origine que le registre configuré —
élargie, pour la seule place de marché VS Code publique, à son CDN d'assets
`*.gallerycdn.vsassets.io` — de sorte qu'un amont compromis ou mal configuré ne
puisse pas s'en servir pour pointer BatleHub vers un hôte interne.

Les redirections sont suivies par BatleHub plutôt que par son client HTTP, un
saut à la fois, et chaque saut est revérifié avant d'être composé : un amont qui
répond à sa propre URL de README par `302 Location: http://169.254.169.254/…`
n'obtient aucune requête. Les identifiants configurés du registre voyagent avec
la lecture tant qu'elle reste sur l'origine de ce registre — c'est une requête
vers l'amont pour lequel ils ont été configurés, et c'est la lecture anonyme qui
se fait limiter en débit — et sont abandonnés dès qu'une redirection en sort.

## L'analyse de vulnérabilités

La revérification OSV périodique, quand
`[vulnerability_scan] enabled = true`. Désactivée par défaut. Voir
[SBOM](/fr/guide/sbom).

## Un travail d'analyse s'exécute {#a-scan-job-runs}

Un registre doté d'un profil
[`[registries.security]`](/fr/guide/configuration#security) remet chaque nouvelle
version au worker, et les scanners que le profil nomme émettent leurs propres
requêtes. Chacun est désactivé tant que vous ne l'avez pas configuré dans
`[[scanners]]`.

La plupart composent vers un hôte que vous avez nommé : OSV, un serveur Trivy,
les API de Socket et de mlab. Les scanners binaires sous bac à sable n'atteignent
que ce que le bac à sable leur laisse. Deux entrées méritent d'être énoncées à
part.

**Rekor**, quand `sigstore` est activé : une consultation par entrée de journal
de transparence que cite une attestation, sur `rekor_url`
(`https://rekor.sigstore.dev` par défaut). Un hôte que vous avez configuré, comme
les autres.

**Un hôte choisi par l'amont**, dans ce même scanner. npm annonce les
attestations d'une version dans le packument, sous `dist.attestations.url`, et le
lot est récupéré à cette URL — c'est donc l'*index amont*, et non votre
configuration, qui nomme l'hôte. C'est traité comme
[un README référencé par lien](#a-linked-readme) : le schéma doit
être `http` ou `https`, les redirections sont suivies par BatleHub un saut à la
fois plutôt que par son client HTTP, chaque saut est revérifié contre les plages
privées, réservées, de boucle locale et de lien local avant d'être composé, et
aucun identifiant ne voyage avec la requête. Un amont qui répond
`302 Location: http://169.254.169.254/…` n'obtient aucune requête ; l'analyse
signale le refus comme une erreur de scanner plutôt que comme un constat, de
sorte que le corps de réponse de quelque chose d'interne ne puisse jamais
atteindre un constat que lit un opérateur.

Désactivez toute la classe en n'écrivant pas `[registries.security]`, ou cette
entrée seule en retirant `sigstore` de `[[scanners]]`.

## Voir aussi

- [Durcissement en production](/fr/operations/production-hardening) — les autres
  réglages qui distinguent une instance qui marche d'une instance que vous
  mettriez devant une entreprise.
- [Configuration → capture des README](/fr/guide/admin-config#readme-capture) et
  [→ la lecture de découverte de la console](/fr/guide/admin-config#the-console-s-discovery-read).
- [La recherche de l'explorateur de paquets](/fr/use/package-explorer-search#upstream-search).
