---
sourcePath: guide/hot-reload.md
sourceHash: 932303548bcac9b9
---

# Rechargement à chaud et configuration dynamique

BatleHub sait recharger sa configuration à l'exécution, sans redémarrer le
processus. Les composants suivants se remplacent à chaud :

- la liste des registres (ajout, retrait, modification d'un registre) ;
- le RBAC par registre (`anonymous`, `user`, `admin`, accès par groupe) ;
- les règles de politique par registre (garde-fou d'âge, refus de `latest`) ;
- la configuration de versionnage, de signature et de canal bêta par registre ;
- la limite de taille des artefacts ;
- le routage par hôte (`hosts`, `path_routing`, `[subdomain_routing]`) et la
  politique `trusted_proxies` qui le gouverne — les deux basculent ensemble, de
  sorte qu'un rechargement qui active le routage par hôte ne tourne jamais sous
  l'ancienne politique de confiance.

Les composants suivants **exigent un redémarrage du processus** :
- l'hôte et le port du serveur ;
- l'URL de la base ou la taille du pool de connexions ;
- les fournisseurs d'authentification (`[[auth]]`) ;
- les backends de stockage.

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
