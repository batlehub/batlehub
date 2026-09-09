---
sourcePath: use/troubleshooting.md
sourceHash: d5bd3db5f78c1e3b
---

# Dépannage

Les pannes courantes, leurs symptômes et leur correction.

## Épuisement du pool de connexions à la base

**Symptôme :** les requêtes renvoient `503 Service Unavailable` ; les logs
montrent `connection pool timed out` ou `PoolTimedOut`.

**Cause :** `max_connections` dans `[database]` est trop bas pour le parallélisme
des requêtes, ou une requête lente monopolise des connexions.

**Correction :**
1. Regardez l'usage courant du pool : `SELECT count(*) FROM pg_stat_activity WHERE datname = 'batlehub';`
2. Repérez les requêtes lentes : `SELECT query, wait_event, state FROM pg_stat_activity WHERE state != 'idle' ORDER BY duration DESC LIMIT 20;`
3. Augmentez `[database] max_connections` dans la configuration et déclenchez un rechargement : `batlehub-cli admin config reload`.
4. Si les requêtes sont lentes, cherchez des index manquants sur `local_packages(registry, name)` et `access_events(created_at)`.

## Expiration des identifiants S3

**Symptôme :** `502 Bad Gateway` au téléchargement d'artefacts censés être en
cache ; les logs montrent `InvalidAccessKeyId` ou `ExpiredTokenException`.

**Cause :** les identifiants AWS / MinIO de `[storage]` ont expiré (un token STS
temporaire, ou une clé qui a tourné).

**Correction :**
1. Faites tourner les identifiants dans votre gestionnaire de secrets.
2. Mettez à jour `[storage] access_key_id` et `secret_access_key` dans le fichier de configuration.
3. Déclenchez un rechargement à chaud : `batlehub-cli admin config reload`.
   À défaut, redémarrez le serveur ; les nouveaux identifiants sont pris au démarrage.

Pour des tokens STS temporaires, envisagez plutôt un rôle IAM d'instance ou
IRSA : plus aucun identifiant statique.

## Publications en attente orphelines

**Symptôme :** republier un paquet dont l'envoi précédent s'est interrompu en
chemin renvoie une erreur de contrainte d'unicité ; le paquet apparaît en base avec `status = 'pending'`.

**Cause :** une tentative de publication antérieure s'est arrêtée après avoir
créé la ligne dans `local_packages`, mais avant de la valider.

**Correction (automatique) :** la tâche de fond
`spawn_pending_publish_cleanup` supprime les lignes `pending` de plus de deux
heures, une fois par heure. Attendez le prochain passage.

**Correction (immédiate) :**
```sql
DELETE FROM local_packages WHERE status = 'pending' AND created_at < now() - interval '2 hours';
```
Ou supprimez la ligne précise :
```sql
DELETE FROM local_packages WHERE registry = 'my-reg' AND name = 'my-pkg' AND version = '1.0.0' AND status = 'pending';
```

## Le cache d'artefacts n'évince plus (boucle d'éviction arrêtée)

**Symptôme :** l'usage du disque ou de S3 croît sans limite ; les vieux artefacts
ne sont pas supprimés.

**À vérifier :**
1. Comptez les lignes d'`artifact_cache_meta` : `SELECT count(*) FROM artifact_cache_meta;`
2. Vérifiez que l'éviction est configurée : `[cache] max_cache_bytes` doit être défini.
3. Cherchez des entrées `eviction` dans les logs — leur absence peut signifier que l'éviction n'est pas branchée.

**Correction :** assurez-vous que `[cache] max_cache_bytes` a une valeur non
nulle. Déclenchez une éviction manuelle en abaissant temporairement la limite et
en rechargeant, puis remettez-la.

## Cache en mémoire avec plusieurs réplicas (limites et quotas non globaux)

**Symptôme :** un utilisateur dépasse son quota ou sa limite de débit sur un
réplica mais pas sur un autre ; la commande `batlehub-cli admin quota` affiche
des valeurs différentes selon l'instance.

**Cause :** `[cache] type = "memory"` garde les compteurs de limitation de débit
et de quota dans la mémoire du processus : chaque réplica a donc sa propre vue.

**Correction :** passez à un backend de cache partagé :
```toml
[cache]
type = "postgres"   # ou "redis"
```
Puis déclenchez un rechargement de configuration. Un simple cache Postgres coûte
très peu pour des compteurs de limitation ; Redis est préférable pour les
déploiements à fort débit.

BatleHub émet un avertissement au démarrage quand le cache en mémoire est
utilisé :
> `metadata cache: in-memory — rate-limit, quota, and session state are NOT shared between replicas`

## Tempête de préchauffage au redémarrage

**Symptôme :** tous les réplicas redémarrent en même temps (après un déploiement,
typiquement) et bombardent les registres amont de requêtes en défaut de cache
pendant des secondes ou des minutes.

**Correction :**
1. **Étalez les redémarrages** dans votre outil de déploiement (une mise à jour progressive avec `maxSurge=1`, par exemple).
2. **Réduisez le parallélisme du préchauffage** : `[cache] warm_concurrency = 1` (si l'option existe) pour ralentir le remplissage.
3. **Préchauffez** avant de basculer le trafic :
   ```bash
   batlehub-cli admin cache warm my-registry --packages "react,lodash,typescript"
   ```
4. Pour les registres adressés par chemin (JetBrains, Debian, RPM), utilisez `--paths` au lieu de `--packages`.

## Le cache de métadonnées renvoie des résultats périmés

**Symptôme :** un paquet a été mis à jour en amont mais BatleHub sert encore les
anciennes métadonnées.

**Cause :** le TTL de `[cache] ttl_secs` n'est pas écoulé.

**Correction :** videz le cache de métadonnées du registre concerné :
```bash
batlehub-cli admin cache clear <registry>
```
Ou attendez l'expiration du TTL (300 secondes par défaut).

**Vérifiez aussi la santé de l'amont :** si le registre amont est en erreur ou
lent, `serve_stale_metadata` sert peut-être délibérément le dernier cache valide,
plutôt qu'un TTL périmé. `GET /api/v1/admin/stats` publie `upstream_degraded`,
`upstream_error_rate` et `upstream_latency_ms` par registre, et une jauge
`batlehub_upstream_health_degraded{registry}` (avec un log d'avertissement à la
transition) est disponible sur `/metrics` pour l'alerting.

## L'envoi échoue avec `413 Payload Too Large`

**Symptôme :** `cargo publish`, `pip upload` ou un client équivalent reçoit une
erreur 413.

**Cause :** l'artefact envoyé dépasse
`[local_registry] max_artifact_size_bytes`.

**Correction :** relevez la limite du registre concerné :
```toml
[[registries]]
name = "my-cargo"
type = "cargo"
max_artifact_size_bytes = 524288000  # 500 MiB
```

Déclenchez un rechargement : `batlehub-cli admin config reload`.

## La clé d'API n'est pas reconnue

**Symptôme :** les clients reçoivent `401 Unauthorized` alors qu'ils passent un
token.

**À vérifier :**
1. Confirmez que le token existe : `batlehub-cli auth token list`.
2. Vérifiez qu'il n'est pas expiré : `batlehub-cli auth whoami`.
3. Vérifiez que l'en-tête `Authorization: Bearer <token>` est bien envoyé (ou `X-NuGet-ApiKey` pour les clients NuGet — BatleHub le normalise en interne).
4. En OIDC, regardez les logs du fournisseur pour l'échange de token.

## L'analyse de vulnérabilités (Trivy / grype) bloque le démarrage

**Symptôme :** la tâche `watcher::spawn_periodic_vuln_scan` échoue et journalise
des erreurs au démarrage, sur un environnement où le binaire du scanner n'est pas
installé.

**Correction :** installez le scanner (`mise install`), ou désactivez l'analyse :
```toml
[vulnerability_scan]
enabled = false
```

Supprimer entièrement la section `[vulnerability_scan]` revient au même —
`enabled` vaut `false` quand la section est absente, de sorte que l'analyse
périodique ne tourne que là où un opérateur l'a demandée.

## Obtenir plus d'informations de diagnostic

```bash
# Augmenter la verbosité des logs
RUST_LOG=batlehub=debug cargo run -p batlehub-server -- --config config.toml

# Consulter le journal d'audit pour les erreurs récentes
batlehub-cli admin audit-log --denied-only --per-page 50

# Statistiques courantes
batlehub-cli admin stats

# Santé par registre
batlehub-cli admin health
```
