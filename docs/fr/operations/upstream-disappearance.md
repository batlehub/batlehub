---
title: Disparition d'un amont
sourcePath: operations/upstream-disappearance.md
sourceHash: 5a79583fa694d6f1
---

# Disparition d'un amont

Pour l'opérateur dont le build a cassé parce qu'un paquet a quitté son amont, et
pour celui qui se demande si cette instance devrait s'en apercevoir avant qu'un
build ne le fasse. La conception est en
[RFC 0014](/rfc/0014-upstream-disappearance) ; cette page décrit à quoi cela
ressemble depuis la console, les logs et le fil.

---

## Ce que fait l'audit

Une passe périodique demande à chaque amont en mode proxy ou hybrid si les
artefacts qu'on lui a récupérés sont toujours là. Un défaut est enregistré, pas
cru : une disparition n'est **confirmée** qu'après `confirm_after` passes
consécutives l'ayant manqué, espacées d'au moins `confirm_min_age_secs`, dans des
passes où les autres paquets du registre répondaient toujours. Une panne, une
limitation de débit ou un identifiant expiré affectent presque tout d'un coup :
une passe dans laquelle plus d'`outage_ratio` des paquets d'un registre
disparaissent est donc **nulle** et n'enregistre rien.

À la confirmation, l'instance fait trois choses, quelle que soit la politique :

- elle garde la ligne — `disappeared`, avec la date du premier défaut et le
  nombre de passes qui l'ont confirmé ;
- elle **retient l'artefact hors de l'éviction** et ré-épingle ses métadonnées en
  cache à chaque passe, de sorte que la dernière copie du parc ne soit pas
  ramassée précisément parce que l'amont a cessé de la rafraîchir ;
- elle prévient qui s'est abonné : une notification
  `package_disappeared_upstream` avec la coordonnée, le nombre de défauts, le
  barreau d'échelle atteint par la sonde et la politique.

Un paquet qui répond de nouveau efface la ligne et envoie
`package_reappeared_upstream`. Une passe annulée par le ratio envoie
`upstream_unreachable` pour le registre, de sorte qu'un registre nul à chaque
cycle est lui-même une alerte plutôt qu'un trou silencieux.

## Les deux politiques

`[upstream_audit] on_confirmed` vaut `"audit"` ou `"block"`.

| | `"audit"` (défaut) | `"block"` |
| --- | --- | --- |
| Ligne, retenue, notification | oui | oui |
| Service | inchangé | **refusé** : toutes les versions détenues de ce nom sont bloquées par la liste de blocage d'administration, `blocked_by = system:upstream-audit` |
| Réapparition | ligne effacée | ligne effacée **et le blocage propre à l'audit levé** — le blocage d'un admin, ou un blocage qu'un admin a modifié, reste, et l'événement dit `unblock_skipped_reason: blocked_by_admin` |
| Ce que coûte une confirmation erronée | un admin lit une mauvaise alerte | le parc bloque un paquet contre lui-même |

La dernière ligne explique pourquoi le défaut est `"audit"`. Sous `"block"`, le
rapport devient l'arme : un attaquant capable de servir des 404 sélectifs à cette
instance — la maîtrise du chemin vers l'amont, tenue pendant toute la fenêtre de
confirmation, assez étroitement pour ne pas déclencher le ratio — obtient un déni
de service ciblé contre un paquet dont le parc dépend. Cette position lui permet
déjà de servir des métadonnées fabriquées en cas de défaut de cache : la capacité
n'est donc pas nouvelle ; c'est le *coût* qui l'est. Choisissez `"block"` pour un
parc dont la menace est un paquet retiré ou détourné qui atteindrait un build, et
acceptez qu'une interférence étroite et soutenue avec un amont puisse vous
retirer un paquet.

Désactiver `"block"` ne débloque rien : les blocages existants sont un état
administratif et restent jusqu'à ce qu'un admin les lève ou que le paquet
réapparaisse. La table des blocages de la console filtre sur
`system:upstream-audit` : les lever en masse tient donc en une sélection filtrée.

## Lire la console

- **Santé** — la carte nomme la politique active et, par registre, combien de
  paquets sont `missing` (vus une fois) et `disappeared` (confirmés).
- **Exploitation → Amont** — la table : chaque ligne, filtrable par registre et
  par état, avec le premier défaut, la dernière vérification et l'heure de
  confirmation. *Revérifier* sonde un paquet maintenant, par la même échelle et
  la même machine à états que la passe. C'est une sonde, pas une dérogation :
  elle ne peut pas confirmer en avance ni forcer une confirmation, mais un admin
  qui a eu des nouvelles de l'amont n'attend pas un intervalle pour voir la ligne
  s'effacer.
- **La page d'un paquet** — un badge sur chaque version concernée : *manquante en
  amont* ou *disparue en amont*, et sous `"block"`, *bloquée par l'audit amont*.

## Lire les logs et les métriques

Une confirmation est journalisée en `WARN` avec la coordonnée et le nombre de
défauts ; une passe nulle en `WARN` avec les comptes. `on_confirmed = "block"`
est journalisé une fois en `INFO` au démarrage, en nommant l'acteur que porteront
les blocages.

| Métrique | Ce qu'elle dit |
| --- | --- |
| `batlehub_upstream_missing_total{registry}` | lignes vues manquantes, non confirmées |
| `batlehub_upstream_disappeared_total{registry}` | lignes confirmées |
| `batlehub_upstream_audit_sweeps_total{registry,outcome}` | `ok` / `void` — un taux de `void` qui monte est la fonctionnalité qui échoue, pas les amonts |
| `batlehub_upstream_audit_duration_seconds{registry}` | la durée d'une passe |

## L'API

```text
GET  /api/v1/admin/upstream/disappeared?registry=&state=&page=&per_page=
GET  /api/v1/admin/upstream/status/{registry}/{name}
POST /api/v1/admin/upstream/recheck        { "registry", "package_name", "version"? }
```

Les trois exigent `system:read` (le listing et l'état) ou `system:write`
(`recheck`). Le listing pagine selon `LimitsConfig.packages_per_page`.

## Ce que l'audit ne peut pas voir

Les forges (`github`, `gitlab`, `forgejo`) sont proxifiées par chemin et ne sont
pas sondées. Les types adressés par chemin — `deb`, `rpm`, `pacman`,
`jetbrains`, `generic` — n'ont pas d'identité de paquet : la passe interroge donc
**chaque fichier détenu** par un `HEAD` sur son chemin amont (RFC 0014 §13.5).
La ligne, l'événement et le blocage nomment le chemin du fichier là où ceux d'un
type fondé sur le paquet nommeraient une version, sous l'unique paquet que ces
registres possèdent (`repo`), et un blocage porte sur ce seul fichier. Pour tous
les autres types, la passe atteint le document de listing quand il y en a un et
sonde version par version quand il n'y en a pas, à raison de 25 versions — ou
fichiers — au plus par paquet et par passe.

`on_confirmed` se décide registre par registre (RFC 0014 §13 O6) : la ligne
`on_confirmed` propre à un registre l'emporte sur la clé `[upstream_audit]` pour
ce registre, de sorte qu'un même parc peut bloquer un amont public et se
contenter d'auditer un miroir interne. La carte de politique de la console montre
la clé du parc et, à côté des compteurs de chaque registre, un badge quand la
ligne propre au registre diffère ; l'API rapporte les deux (`policy` sur la page,
`policy` et `overridden` par registre, et celle du registre dans l'état d'un
paquet). Le `policy` de l'événement est celle qui s'est appliquée.

Le bras « blocage » bloque les versions que le parc *détient*. Il ne peut pas
bloquer un nom contre une nouvelle inscription : rien n'a bloqué une version qui
n'existe pas encore. C'est le signalement `version = "*"` de la RFC 0002, poussé
par quiconque surveille le nom.

## Configuration

La section, ses planchers et ses avertissements sont dans la
[référence de configuration](/fr/guide/configuration#upstream-audit).
