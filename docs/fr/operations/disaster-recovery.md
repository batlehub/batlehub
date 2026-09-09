---
sourcePath: operations/disaster-recovery.md
sourceHash: f884ba5d7c0f48c5
---

# Reprise après sinistre

Ce document décrit où vivent les données, comment les sauvegarder, et comment
restaurer une instance BatleHub après une panne.

::: warning Des repères, pas un engagement
L'inventaire des données est un fait à propos du logiciel ; les procédures sont
un point de départ pour les vôtres. Les sauvegardes ne sont pas prises pour vous,
aucun objectif de point de reprise ou de temps de reprise énoncé ici n'est
garanti, et le chemin de restauration ci-dessous ne vaut que ce que vaut la
dernière sauvegarde que vous avez réellement vérifiée.
:::

## Inventaire des données

| Magasin | Ce qui y vit | Impact d'une perte |
|-------|-----------------|-------------|
| PostgreSQL | Métadonnées de paquets, événements d'accès, quotas, compteurs de limitation, enregistrements SBOM, historique des changements de configuration, blocages d'utilisateurs et d'IP, abonnements aux notifications | Tous les listings de paquets, le journal d'audit et l'état des politiques sont perdus |
| Stockage des artefacts (S3 ou système de fichiers) | Artefacts du cache proxy et artefacts publiés en mode local | Les artefacts du cache proxy sont re-récupérés à la demande ; **les artefacts du mode local sont perdus définitivement**, sauf sauvegarde séparée |
| Redis (facultatif) | Cache de métadonnées, compteurs de limitation (éphémères) | Sans conséquence : le serveur les reconstitue au redémarrage |

## Stratégie de sauvegarde

### PostgreSQL

Planifiez un `pg_dump` vers un emplacement durable (un bucket S3 avec versionnage
activé, par exemple) :

```bash
pg_dump -Fc "$DATABASE_URL" > batlehub-$(date +%Y%m%d%H%M%S).pgdump
```

Rétention recommandée : 7 quotidiennes, 4 hebdomadaires et 12 mensuelles.

Pour une sauvegarde continue fondée sur les WAL, employez `pgBackRest` ou
`Barman`. La reprise à un instant donné permet de restaurer à la seconde près, et
pas seulement aux frontières d'instantané.

### Le stockage des artefacts

**S3 :** activez le versionnage sur le bucket et configurez des règles de cycle
de vie pour conserver les objets supprimés 30 jours. Employez la réplication
inter-régions pour un site de secours tiède.

**Système de fichiers :** planifiez un `rclone sync` vers une destination
distante, par exemple :

```bash
rclone sync /var/lib/batlehub/artifacts s3:my-backup-bucket/batlehub-artifacts \
  --transfers 16 --checksum
```

## Procédure de restauration

### Scénario 1 — corruption de la base ou table supprimée par accident

1. Arrêtez toutes les instances BatleHub pour empêcher d'autres écritures.
2. Créez une base neuve : `createdb batlehub`.
3. Restaurez : `pg_restore -d batlehub batlehub-YYYYMMDDHHMMSS.pgdump`.
4. Redémarrez BatleHub — il exécutera les migrations automatiquement au
   démarrage.
5. Vérifiez avec `batlehub-cli registry list`.

### Scénario 2 — bucket S3 perdu (artefacts en cache seulement, pas de mode local)

Cela se surmonte sans perte de données pour les registres en mode proxy. Les
artefacts en cache sont re-récupérés en amont à la requête suivante.

1. Créez un nouveau bucket, mettez à jour `[storage] bucket` dans la
   configuration.
2. Déclenchez un rechargement : `batlehub-cli admin config reload`.
3. Éventuellement, préchauffez les paquets critiques :
   `batlehub-cli admin cache warm <registry> --packages <name>`.

### Scénario 3 — bucket S3 perdu (avec des artefacts en mode local)

Les artefacts du mode local ne sont pas re-récupérables. Sans sauvegarde du
bucket, ils sont perdus.

1. S'il existe une sauvegarde du bucket : restaurez avec
   `rclone sync s3:my-backup-bucket/batlehub-artifacts /restore/`.
2. Faites pointer le nouveau bucket vers les données restaurées, ou configurez un
   stockage sur système de fichiers pointant vers le chemin restauré.
3. Redémarrez BatleHub.

Sans sauvegarde, republiez les artefacts concernés avec `batlehub-cli publish` ou
le client propre au registre.

### Scénario 4 — sinistre total (base et stockage perdus)

1. Provisionnez une nouvelle instance Postgres et restaurez depuis le dernier
   `pg_dump`.
2. Provisionnez un nouveau backend de stockage et restaurez depuis la sauvegarde
   des artefacts.
3. Déployez BatleHub avec l'URL de base et la configuration de stockage
   restaurées.
4. Lancez `batlehub-cli admin cache warm` pour chaque registre, afin d'éviter une
   tempête de défauts de cache au démarrage à froid.

## Bascule inter-régions

BatleHub gère une bascule actif/passif avec un réplica en lecture et un bucket de
stockage de secours :

1. **Promouvez la base réplique** : `pg_ctl promote` (Postgres 12+) ou la
   console de bascule de votre base managée.
2. **Basculez le bucket de stockage** : mettez `[storage] bucket` sur le bucket
   de la région de secours (alimenté par la réplication inter-régions).
3. **Mettez à jour le DNS** pour faire pointer `batlehub.example.com` vers la
   région de secours.
4. **Redémarrez** les instances BatleHub de la région de secours.

Temps de reprise attendu avec un site de secours préchauffé : moins de 5 minutes.
Le point de reprise dépend du retard de réplication (typiquement quelques
secondes en flux WAL, quelques minutes avec un `pg_dump` planifié).

## Publications en attente orphelines

Si un processus BatleHub meurt pendant une publication, la table
`local_packages` peut contenir des lignes en `status = 'pending'`. Elles sont
nettoyées automatiquement par la tâche de fond
`spawn_pending_publish_cleanup` (toutes les heures par défaut). Pour nettoyer
immédiatement :

```sql
DELETE FROM local_packages WHERE status = 'pending' AND created_at < now() - interval '2 hours';
```

Ou déclenchez-le par l'API : redémarrez le serveur (la tâche s'exécute au
premier battement après l'intervalle initial).

## Liste de contrôle de la procédure

- [ ] La sauvegarde Postgres tourne quotidiennement, avec une rétention d'au moins 7 jours
- [ ] Le versionnage S3 est activé sur le bucket d'artefacts
- [ ] Le `rclone sync` des artefacts tourne au moins toutes les heures pour les registres en mode local
- [ ] La procédure de reprise est testée chaque trimestre (restauration en préproduction)
- [ ] Une alerte est levée sur l'échec du job de sauvegarde (CloudWatch, Alertmanager Prometheus…)
- [ ] `deploy/prometheus-alerts.yaml` est chargé — l'alerte `BatleHubDown` est active
