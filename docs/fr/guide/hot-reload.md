---
sourcePath: guide/hot-reload.md
sourceHash: 9e8ff267c2df7cfd
---

# Rechargement à chaud et configuration dynamique

BatleHub sait recharger sa configuration à l'exécution, sans redémarrer le
processus — mais pas toute. Chaque clé relève de l'un de trois niveaux :

- **hot** — tous ses lecteurs voient la nouvelle valeur dès que le
  rechargement est appliqué ;
- **partial** — toutes les requêtes la voient, mais un composant construit une
  fois au démarrage (préchauffage, imports de versions, worker d'analyse) garde
  l'ancienne valeur jusqu'au redémarrage. Le cas qui compte est la rotation d'un
  identifiant amont : les installations utilisent aussitôt le nouveau, le
  préchauffage continue avec l'ancien ;
- **restart** — rien ne la relit avant un redémarrage.

Une clé qu'aucune ligne ci-dessous ne nomme relève de **restart** : une section
ajoutée plus tard à la configuration exige un redémarrage tant que personne n'a
montré qui la relit, car un redémarrage inutile coûte quelques secondes, et un
rechargement qui ne fait rien en silence coûte un incident. La ligne la plus
précise l'emporte : `registries[npm].cache.metadata_ttl_secs` relève de
*partial*, `registries[npm].cache.idle_days` de *restart*.

| Clé | Niveau | Lue par |
| --- | --- | --- |
| `grants` | hot | les droits de niveau instance |
| `ip_blocking.trusted_proxies` | hot | la politique de confiance des proxys, comme repli déprécié de `server.trusted_proxies` |
| `limits.packages_per_page` | hot | la pagination des listes |
| `limits.versions_per_page` | hot | la pagination des listes |
| `registries[*].cache.serve_stale` | hot | la politique de métadonnées du registre |
| `registries[*].grants` | hot | la hiérarchie de droits du registre |
| `registries[*].hosts` | hot | la table de routage hôte → registre |
| `registries[*].path_routing` | hot | la table de routage hôte → registre |
| `registries[*].rbac` | hot | la hiérarchie de droits du registre |
| `registries[*].rules` | hot | la chaîne de règles du registre |
| `scanners.*.escalation` | hot | la politique de sécurité de chaque registre |
| `search.readmes` | hot | l'interrupteur de recherche dans les README |
| `server.signed_urls` | hot | le signataire des URL de téléchargement |
| `server.trusted_proxies` | hot | la politique de confiance des proxys, basculée avec la table de routage par hôte |
| `subdomain_routing` | hot | la table de routage hôte → registre |
| `air_gap` | partial | l'enveloppe air-gap des clients de registre ; le préchauffage et les imports gardent les clients du démarrage |
| `limits.max_artifact_size_bytes` | partial | le chemin des requêtes ; le worker d'analyse garde la limite du démarrage |
| `proxy` | partial | les clients de registre et l'index cargo ; le récupérateur de README et le préchauffage gardent le proxy du démarrage |
| `registries[*]` | partial | les clients, politiques et tables des registres ; le préchauffage et les imports gardent le client du démarrage |
| `registries[*].cache.artifact_ttl_secs` | partial | la politique de téléchargement du registre ; l'éviction garde la durée du démarrage |
| `registries[*].cache.metadata_ttl_secs` | partial | la politique de métadonnées du registre ; le préchauffage garde le client du démarrage |
| `registries[*].cache` | restart | les services de préchauffage et d'éviction, construits une fois |
| `registries[*].quota` | restart | le service de quotas, construit une fois |
| `registries[*].rate_limit` | restart | le limiteur de débit, construit une fois |
| `registries[*].security.rescan` | restart | le planificateur de réanalyse, démarré au démarrage seulement si un registre en avait déjà un |
| `registries[*].storage` | restart | le routeur de stockage, construit une fois |
| tout le reste | restart | lu une fois au démarrage |

Ajouter ou retirer un registre entier relève de *partial* : le chemin des
requêtes le sert aussitôt ; le préchauffage, l'éviction, les limites de débit,
les quotas et les libellés de métriques gardent la liste du démarrage ; les
clés à redémarrage que porte un registre ajouté — `rate_limit`, `quota`,
`storage`, `cache.idle_days` — figurent donc à côté de lui dans
`restart_required`.

Sur une instance démarrée avec `BATLEHUB_DISABLE_HOT_RELOAD=1`, tout changement
exige un redémarrage, quoi qu'en dise la table.

### Ce que rapporte un rechargement

Chaque diff — le rechargement en attente, `POST /api/v1/admin/config/validate`,
le rechargement immédiat — porte trois listes de chemins pointés, une entrée de
tableau étant nommée par son `name` (ou son `type`) :
`registries[npm].upstreams`.

| Champ | Contient | Comparé à |
| --- | --- | --- |
| `applied` | les clés *hot* que ce candidat modifie | la configuration en vigueur |
| `partially_applied` | les clés *partial* que ce candidat modifie | la configuration en vigueur |
| `restart_required` | toute clé *restart* que le processus n'a pas | la configuration de **démarrage** |

`restart_required` est comparé au démarrage à dessein : un changement de
`database.url` appliqué il y a une heure reste listé à chaque rechargement
suivant, jusqu'au redémarrage qui le rend vrai. Le surveillant de fichiers le
journalise au niveau `WARN`, et la page de rechargement de la console affiche
les trois listes.

`restart_required` vaut `["*"]`, avec la raison dans `restart_reason`, quand la
configuration place un `${VAR}` là où le diff ne peut pas le garder comme
texte — hors d'une valeur chaîne sur une ligne, dans une clé, ou dans le `name`
ou le `type` de n'importe quelle entrée `[[…]]` — `[[registries]]`, `[[auth]]`
et les autres (voir
[Substitution en ligne](/fr/guide/configuration#env-inline)). `restart_reason`
nomme le côté qui n'a pas pu être comparé : la candidate, la configuration en
vigueur ou celle du démarrage. Seule cette comparaison est inconnue, et inconnu
signifie redémarrage ; les autres listes restent remplies, et le rechargement
lui-même se déroule comme avant.

## 9.1 Le surveillant de fichiers

Quand le fichier de configuration change sur le disque, BatleHub valide
automatiquement la nouvelle configuration (contrôle de schéma et sondes de
connectivité) et enregistre un **rechargement en attente**. L'administrateur le
confirme ou l'abandonne ensuite depuis la console ou l'API. Un rechargement en
attente expire au bout de 10 minutes.

Quand le processus a été démarré avec plusieurs `--config`, **toutes les couches
sont surveillées et chaque rechargement les relit toutes**. Faire tourner un
identifiant ne touche que le fichier d'identifiants, et cela seul prépare un
rechargement en attente : la déduplication des réécritures identiques octet pour
octet, qui existe pour `touch` et les enregistrements atomiques, compare toutes
les couches et pas seulement la première. Voir
[Configuration en couches](/fr/guide/configuration#layered-config-files).

Une couche qui ne peut pas être surveillée est journalisée et ignorée, plutôt que
de faire tomber le surveillant pour les autres ; le chemin de rechargement relit
de toute façon toutes les couches, donc un changement dans un fichier surveillé
prend aussi en compte ce que dit désormais le fichier non surveillé.

Le surveillant de fichiers est actif par défaut. Pour le désactiver :

```sh
BATLEHUB_DISABLE_HOT_RELOAD=1 batlehub --config config.toml
```

Utilisez-le quand `config.toml` est monté en ConfigMap Kubernetes en lecture
seule.

## 9.2 Endpoints d'API {#_9-2-api-endpoints}

| Méthode | Chemin | Description |
|--------|------|-------------|
| `POST` | `/api/v1/admin/config/reload` | Rechargement immédiat : valider et appliquer atomiquement |
| `GET` | `/api/v1/admin/config/pending` | Obtenir le diff du rechargement en attente (404 s'il n'y en a pas) |
| `POST` | `/api/v1/admin/config/pending/apply` | Appliquer le rechargement en attente |
| `DELETE` | `/api/v1/admin/config/pending` | Abandonner le rechargement en attente |
| `GET` | `/api/v1/admin/config/changes` | Historique d'audit paginé (`?page=0&per_page=50`) |
| `GET` | `/api/v1/admin/config/warnings` | Problèmes non fatals de la configuration en vigueur |

```sh
# CI/CD : appliquer une nouvelle configuration atomiquement
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/config/reload

# Flux en deux temps : laisser le surveillant charger un rechargement en attente, puis l'appliquer depuis la CI
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/config/pending/apply
```

Tous les rechargements, appliqués ou rejetés, sont écrits dans la table
`config_changes` avec le diff, la source du déclenchement et l'identité de
l'opérateur.

### Les avertissements de configuration

Certains états de configuration sont assez fautifs pour qu'on en informe un
opérateur, mais pas assez pour refuser de démarrer — un nom de registre qui ne
peut pas devenir une étiquette DNS, une clé dépréciée masquée par une autre, un
défaut de sécurité permissif laissé en place. Ils sont journalisés au démarrage
et à chaque rechargement, **et** servis par
`GET /api/v1/admin/config/warnings`, pour être vus sans faire de `grep` dans les
logs :

```json
{
  "warnings": [
    {
      "code": "proxy-trust.unconfigured",
      "path": "server.trusted_proxies",
      "message": "no trusted-proxy list is configured, so Forwarded / X-Forwarded-Host / …"
    }
  ]
}
```

`code` est un identifiant stable, sur lequel on peut filtrer sans risque ; `path`
pointe vers l'emplacement fautif tel quel dans la configuration, de sorte qu'on
peut le rechercher dans le TOML.

`POST /api/v1/admin/config/validate` et
`POST /api/v1/admin/config/from-content` renvoient la même forme, en ligne sous
`warnings`, à propos de la configuration *candidate* — un administrateur les voit
donc **avant** d'appliquer un rechargement en attente plutôt qu'après. La page
d'administration du rechargement affiche les deux.

| Code | Signification |
|---|---|
| `proxy-trust.unconfigured` | Aucune liste `trusted_proxies` nulle part ; l'hôte et le schéma transmis sont crus de n'importe quel client |
| `proxy-trust.deprecated-key-only` | La confiance envers les proxys vient du `[ip_blocking].trusted_proxies` déprécié |
| `proxy-trust.invalid-deprecated-entry` | Une entrée du `[ip_blocking].trusted_proxies` déprécié n'est ni une IP ni une plage CIDR, et a été retirée |
| `proxy-trust.shadowed-deprecated-key` | Les deux clés sont définies ; `[server]` gagne et la liste dépréciée est entièrement ignorée |
| `subdomain.invalid-dns-label` | `[subdomain_routing]` est actif mais un nom de registre ne peut pas être une étiquette DNS : aucun hôte générique n'en est dérivé |
| `vsx-signing.proxy-mode` | `[registries.vsx_signing]` sur un registre en mode `proxy` : rien n'y est publié, donc la clé ne signe rien ; la signature de l'amont est relayée quoi qu'il arrive |

## 9.3 Le bandeau d'administration global

Un administrateur peut diffuser un message à tous les visiteurs du site :

```sh
# Poser un bandeau d'avertissement
curl -s -X PUT \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message":"Maintenance window in 30 min","level":"warning"}' \
  http://localhost:8080/api/v1/admin/banner

# Le retirer
curl -s -X DELETE \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/banner
```

Le frontend interroge `GET /api/v1/banner` (sans authentification) toutes les 30
secondes. Le stockage du bandeau repose sur la même infrastructure que le cache
de métadonnées :

| `[cache] type` | Stockage du bandeau |
|----------------|---------------|
| `"memory"` | Dans le processus — non partagé entre réplicas |
| `"redis"` | Redis — partagé par tous les réplicas en haute disponibilité |
| `"postgres"` | Table `system_kv` — partagée par tous les réplicas |
