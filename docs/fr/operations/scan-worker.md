---
title: Le worker d'analyse
sourcePath: operations/scan-worker.md
sourceHash: c62e392f2459ad0d
---

# Le worker d'analyse

Pour l'opérateur dont les installations sont refusées avec `SCAN_PENDING`, et
pour celui qui se demande s'il faut donner au worker son propre Deployment. La
conception est en [RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts) ;
cette page décrit à quoi cela ressemble depuis le pod, la file et les métriques.

---

## Ce que c'est

Le worker est un **rôle du même binaire**, pas un second programme. `[server]
roles` vaut `["proxy", "worker"]` par défaut : une instance mono-processus en
fait donc déjà tourner un, embarqué. `batlehub --roles worker` démarre un
processus qui ne fait qu'analyser, et `--roles proxy` un processus qui ne fait
que servir.

Les deux rôles ne partagent que la base de données. Le worker ne répond à aucun
HTTP, et le proxy ne bloque jamais une requête sur lui. C'est la propriété à
garder en tête quand quelque chose ne va pas : un worker mort ou saturé
**dégrade** les registres qu'il couvre, il ne les casse pas.

Séparer les rôles est ce que fait `worker.enabled` dans le chart, et cela vaut
la peine pour deux raisons. Les outils d'analyse — bubblewrap, `postmortem`, le
client Trivy, éventuellement GuardDog — ne vivent que dans l'image du worker, et
seul le worker a besoin de sortir vers les artefacts amont, le serveur Trivy et
Rekor. Voir [Installation](/fr/guide/installation) pour les valeurs du chart et
[Ce qui sort de cette instance](/fr/operations/egress) pour les flux sortants.

## Comment un travail arrive dans la file

La file est une table Postgres, `scan_jobs`, et elle est alimentée depuis quatre
endroits. Le déclencheur est aussi la priorité : un utilisateur qui attend sur
une première requête passe avant une passe de fond.

| Déclencheur | Mis en file par | Priorité |
| --- | --- | --- |
| `first_seen` | le proxy, à la première requête d'une version sans verdict | 0 |
| `webhook` | un signalement du SOC, ou un administrateur qui marque un paquet | 1 |
| `rescan` | l'ordonnanceur de réanalyse, et `verdicts rescan` | 2 |
| `backfill` | `verdicts backfill` — toutes les versions en cache d'un registre | 3 |

La mise en file est **idempotente sur la coordonnée** : un index unique partiel
n'autorise qu'un seul travail *ouvert* par version, si bien qu'une rafale de
premières requêtes sur le même paquet crée un travail, pas cent.

L'ordonnanceur de réanalyse est un seul minuteur pour tout le parc, et non un
par processus. Le leadership est un verrou consultatif PostgreSQL, le tick de
tout autre processus ne fait rien, et il cherche chaque minute les verdicts plus
vieux que le `rescan.interval_secs` du registre. Un plafond par registre et par
tick évite qu'un premier tick sur un grand registre ne mette toute la table en
file d'un coup.

## Ce que fait une passe

Les travaux sont **loués, pas consommés**. Une passe, c'est :

1. **Battement de cœur et publication des profondeurs.** Le worker écrit dans
   `worker_heartbeats` et met à jour la jauge des travaux en file. Ni l'un ni
   l'autre n'est porteur, donc aucun des deux ne peut faire échouer la passe.
2. **Louer un lot** — au plus `max_concurrent`, filtré sur les registres de
   `[worker]` quand cette liste est renseignée, pris avec
   `FOR UPDATE SKIP LOCKED` pour que n'importe quel nombre de workers puisse
   partager la file.
3. **Planifier.** Le profil `[registries.security]` du registre, son type, et
   les analyseurs qui s'appliquent à ce type. Un registre qui a quitté le profil
   depuis la mise en file abandonne le travail plutôt que de retenir la version.
4. **Lancer chaque analyseur à son tour**, en rebattant la location entre
   chacun, puis lancer les enrichisseurs sur ce qu'ils ont trouvé.
5. **Enregistrer le verdict** et fermer la ligne.

Entre deux passes, la boucle ne dort `idle_poll` que si la file était vide.

Les valeurs par défaut sont quatre travaux simultanés, un `job_timeout` de dix
minutes, trois tentatives et une attente à vide de deux secondes. Elles vivent
dans `[worker]`, documenté dans la
[référence de configuration](/fr/guide/configuration#scanners-and-worker).

### Quand une tentative ne va pas au bout

Une location qui expire — le pod a été évincé, une archive hostile a emporté le
processus — remet le travail dans la file et incrémente `attempts`. C'est normal
et cela se répare tout seul.

Ce qui ne se répare pas tout seul, c'est un travail qui épuise `max_attempts`.
La ligne se ferme alors et le verdict est écrit pour lui : chaque analyseur que
le registre nomme dans `required_scanners` reçoit un constat `SCANNER_ERROR`
disant qu'aucune tentative n'a abouti à temps. C'est ensuite le réglage
`scanner_error` du registre qui décide si la version est retenue, servie avec un
avertissement, ou servie.

Un analyseur qui renvoie une erreur, au lieu de ne pas répondre, est encore
autre chose, et il n'est pas réessayé : le constat nomme cet analyseur, avec
`SCANNER_ERROR`, ou `SCANNER_UNSUPPORTED` quand l'analyseur avait besoin des
octets de l'artefact et qu'ils n'ont pas pu être obtenus. La distinction est
délibérée — un analyseur non applicable dit que l'analyse n'a pas eu lieu, au
lieu de faire comme si elle était passée.

## Ce sous quoi tournent les analyseurs

Tout analyseur binaire — `postmortem`, `guarddog`, `trivy` — passe par un seul
lanceur, et par `bwrap`. L'artefact est une entrée contrôlée par l'attaquant, et
le worker est le seul processus qui l'ouvre tout en détenant les identifiants de
la base et du stockage : le bac à sable est donc la frontière qui compte le plus
dans cette architecture.

- espaces de noms user, pid, ipc et uts neufs, `--die-with-parent` et
  `--new-session` ;
- **aucun réseau**, sauf si l'analyseur déclare en avoir besoin ;
- la racine montée en lecture seule, et le répertoire de travail du travail
  comme unique montage inscriptible, lui-même `nosuid` et `nodev` ;
- un environnement vide hormis `HOME` et `PATH`, et un argv passé directement —
  il n'y a aucun shell sur ce chemin ;
- les limites mémoire et CPU de `[worker.sandbox]`, et une extraction bornée par
  `max_extracted_mb` et `max_entries` ;
- la sortie standard lue comme une **donnée non fiable** : plafonnée, analysée
  en JSON sous un schéma strict, jamais interpolée dans quoi que ce soit.

`runtime = "none"` lance la commande nue. Cela existe pour les tests unitaires
et pour les environnements sans bac à sable, la validation de configuration le
garde, et ce n'est pas une option à prendre parce que `bwrap` manque — l'image
du worker l'embarque.

## Ce que fait le proxy pendant qu'une version n'est pas jugée

C'est la partie que les opérateurs rencontrent en premier, en général sous la
forme d'une installation en échec.

| La version est | Le proxy | Le verdict dit |
| --- | --- | --- |
| plus jeune que `mature_age_secs`, non jugée | la refuse | `quarantined`, `SCAN_PENDING`, avec `available_at` |
| plus vieille que `mature_age_secs`, non jugée | la sert, avec un avertissement | `warned` |
| jugée, rien au niveau du seuil ni au-dessus | la sert | `allowed` |
| jugée à charge | la refuse | `quarantined` ou `denied` |

Une retenue qui a une horloge se lève d'elle-même et dit quand. Une retenue sans
horloge — `TIMESTAMP_MISSING`, quand l'amont n'a donné aucune date de
publication — ne se lève pas, et attendre n'y changera rien.

Les utilisateurs ont deux commandes pour cela, documentées dans la
[référence CLI](/fr/use/cli#commands-security) : `batlehub why` explique un
refus, `batlehub wait` attend qu'une version retenue devienne servable et sort
immédiatement en erreur quand attendre ne sert à rien. Le côté administrateur —
lister les verdicts par état, réanalyser, rattraper, et demander qui a déjà
récupéré une version — est [`batlehub verdicts`](/fr/use/cli#commands-verdicts).

## Quand quelque chose ne va pas

| Symptôme | Regarder | Faire |
| --- | --- | --- |
| Toutes les installations d'un registre `[security]` sont refusées `SCAN_PENDING` | `batlehub_workers_live`, et l'avertissement au démarrage qui nomme les registres | Rien n'analyse. Démarrez un processus avec `--roles worker`, ou activez `worker.enabled` dans le chart |
| La profondeur de file monte, mais les travaux finissent par aboutir | `batlehub_scan_jobs_queued` par registre et par déclencheur | Augmentez `max_concurrent`, ou ajoutez des réplicas — l'autoscaler se règle sur cette métrique |
| Une version bloquée, les autres passent | `batlehub why <coordonnée>`, puis les `attempts` et le `last_error` du travail | Laissez les tentatives s'épuiser, ou relancez avec `batlehub why --rescan` une fois la cause corrigée |
| Des versions reviennent en masse en `SCANNER_ERROR` | `batlehub_scanner_errors_total` par analyseur et par classe | Un analyseur est en panne ou injoignable. Vérifiez ses flux sortants et son entrée `[scanners.<nom>]` |
| Les travaux retournent dans la file sans aboutir | `batlehub_scan_jobs_expired_total`, et la mémoire du pod | Une location expire. En général le `memory_limit_mb` du bac à sable face à une archive grosse ou hostile, ou le `job_timeout` face à un analyseur lent |
| Deux instances, une base, des travaux qui disparaissent de l'une | le `worker_id` des deux processus dans `worker_heartbeats` | La file est à l'échelle du parc, par conception. Cloisonnez chaque worker avec les registres de `[worker]` s'ils ne doivent pas la partager |

La dernière ligne mérite d'être dite franchement : **la file appartient à la
base, pas au processus**. N'importe quel worker sur la même base peut louer
n'importe quel travail, ce qui est exactement ce qui fait marcher la montée en
charge horizontale, et ce qui surprend quiconque fait tourner deux instances sur
un seul Postgres.

## Ce qu'il faut surveiller

| Métrique | Type | Ce qu'elle dit |
| --- | --- | --- |
| `batlehub_workers_live` | jauge | Workers vus dans les deux dernières minutes. Zéro avec une quarantaine configurée, c'est une panne de la barrière |
| `batlehub_scan_jobs_queued` | jauge | Profondeur de file, par registre et par déclencheur |
| `batlehub_scan_jobs_leased` | jauge | Ce que ce worker tient à l'instant |
| `batlehub_scan_job_duration_seconds` | histogramme | Par analyseur, avec une issue `ok`/`error` |
| `batlehub_scanner_errors_total` | compteur | Par analyseur et par classe d'erreur |
| `batlehub_scan_jobs_expired_total` | compteur | Travaux ayant épuisé leurs tentatives |
| `batlehub_verdicts_total`, `batlehub_verdict_transitions_total` | compteurs | Ce qui a été décidé, et ce qui a changé d'avis |

C'est sur la transition qu'il faut alerter. Une version qui était `allowed` et
qui devient `quarantined` signifie que quelque chose est arrivé après que des
gens l'avaient déjà installée, et l'alerte de bascule porte la liste de qui l'a
récupérée — la requête même à laquelle répond
[`batlehub verdicts pullers`](/fr/use/cli#commands-verdicts), et le point de
départ du chemin de [réponse à incident](/fr/operations/incident-response).

## Les deux images

`ghcr.io/batleforc/batlehub-worker` embarque tous les analyseurs sauf GuardDog.
`ghcr.io/batleforc/batlehub-worker-guarddog` est la même image avec GuardDog en
plus, construite sur elle, et c'est ce que `worker.image.repository` doit viser
quand un registre nomme `guarddog` dans ses analyseurs.

Les deux sont analysées à chaque construction et lors d'une reconstruction
quotidienne, et les analyseurs qu'elles embarquent sont eux-mêmes des binaires
tiers avec leurs propres dépendances — voir
[Security scanning](/contributing/security-scanning) pour la façon dont une CVE
dans l'un d'eux est traitée. Ajouter son propre analyseur, c'est
[Adding a vulnerability scanner](/contributing/adding-a-vulnerability-scanner).
