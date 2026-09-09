---
sourcePath: operations/soc2-checklist.md
sourceHash: 5e26d1387374b453
---

# Critères de services de confiance SOC 2 — les contrôles de BatleHub

Ce document met chaque critère SOC 2 pertinent en regard des contrôles
implémentés dans BatleHub.

::: warning Une mise en correspondance, pas un résultat d'audit
**BatleHub n'a pas été audité, et un logiciel ne peut pas l'être.** L'entité
certifiée dans un rapport SOC 2 est l'organisation qui exploite un service, pas
le logiciel qu'elle exécute — cette page existe donc pour donner à *votre*
auditeur un point de départ, et rien de ce qui s'y trouve n'est une affirmation
sur le statut de conformité du projet.

« ✅ Implémenté » ci-dessous signifie exactement une chose : le contrôle existe
dans le code, au fichier ou à l'endpoint nommé dans la colonne Preuve. Cela ne
dit rien de son activation dans votre déploiement, de sa bonne configuration, de
sa supervision, ni de son efficacité sur une période — c'est-à-dire tout ce
qu'examine un audit de type II. Plusieurs lignes disent « Processus manuel », et
ce sont des processus que *vous* devriez exécuter.

Servez-vous-en comme d'éléments à soumettre et d'une liste de trous à combler. Ne
la citez pas comme un résultat.
:::

**Périmètre** : le serveur BatleHub (proxy de paquets, registre local, API
d'administration).

---

## CC6 — contrôles d'accès logiques et physiques

| Critère | Contrôle | Statut | Preuve |
|-----------|---------|--------|---------|
| CC6.1 – Protéger les identifiants d'accès logique | Les tokens d'API sont hachés en SHA-256 avant stockage en base ; le texte clair n'est jamais persisté | ✅ Implémenté | `crates/adapters/src/db/postgres/user_tokens.rs` |
| CC6.1 – Expiration des tokens | `expires_at` est appliqué à chaque appel d'API | ✅ Implémenté | `crates/adapters/src/auth/user_token.rs` |
| CC6.1 – Révocation des tokens | Suppression logique par `revoked_at` ; un token révoqué est rejeté immédiatement | ✅ Implémenté | `DELETE /api/v1/auth/tokens/{id}` |
| CC6.2 – Accès par rôle | Rôles anonyme, utilisateur et admin, avec des règles RBAC par registre | ✅ Implémenté | `crates/core/src/rules/rbac.rs` |
| CC6.2 – Accès par groupe | Les claims de groupe OIDC sont associés à des autorisations de ressources par registre | ✅ Implémenté | `RbacRule::with_groups()` |
| CC6.3 – Retirer un accès | API de révocation de token ; API de blocage d'utilisateur qui désactive toutes ses requêtes | ✅ Implémenté | `POST /api/v1/admin/users/{id}/block` |
| CC6.6 – Restriction d'accès réseau | Listes d'autorisation et de blocage d'IP appliquées dans le middleware de requête | ✅ Implémenté | `crates/web/src/middleware/ip_block.rs` |
| CC6.7 – Chiffrement des transmissions | TLS terminé au répartiteur de charge ; les requêtes internes emploient des clients HTTPS | Processus manuel | Déployer avec une terminaison TLS |
| CC6.8 – Empêcher les logiciels non autorisés | Liste d'autorisation des types de registre dans la configuration ; génération de SBOM et analyse de vulnérabilités | ✅ Implémenté | `docs/contributing/security-scanning.md` |

---

## CC7 — exploitation du système

| Critère | Contrôle | Statut | Preuve |
|-----------|---------|--------|---------|
| CC7.1 – Détecter les changements de configuration | Le journal des changements est stocké dans la table `config_changes` | ✅ Implémenté | `GET /api/v1/admin/config/changes` |
| CC7.2 – Surveiller les anomalies | Limitation de débit par IP et par utilisateur ; compteurs d'anomalies dans Prometheus | ✅ Implémenté | [configuration `[otel]`](/fr/guide/configuration#_3-7-otel-optional), `deploy/prometheus-alerts.yaml` |
| CC7.3 – Évaluer les événements de sécurité | Le journal d'audit capture chaque téléchargement, blocage, déblocage et suppression (à la main comme par politique de rétention) avec l'utilisateur, l'horodatage et l'IP ; filtrable par action | ✅ Implémenté | `GET /api/v1/admin/audit-log?action=…` |
| CC7.3 – IP et user-agent dans le journal d'audit | Colonnes `ip_address` et `user_agent` d'`access_events` (migration 029) | ✅ Implémenté | `crates/adapters/migrations/029_audit_ip_ua.sql` |
| CC7.4 – Répondre aux incidents de sécurité | Voir `docs/operations/incident-response.md` | Processus manuel | Documenté |
| CC7.5 – Communiquer sur les incidents | La procédure de réponse à incident comporte des étapes de notification | Processus manuel | `docs/operations/incident-response.md` |

---

## CC8 — gestion du changement

| Critère | Contrôle | Statut | Preuve |
|-----------|---------|--------|---------|
| CC8.1 – Autoriser les changements | Relecture de demande de fusion requise (protection de branche GitHub / Forgejo) | Processus manuel | `docs/operations/change-management.md` |
| CC8.1 – Changements de configuration tracés | Tout changement de configuration par un admin est stocké dans `config_changes` avec l'identité | ✅ Implémenté | `GET /api/v1/admin/config/changes` |
| CC8.1 – Mises à jour de dépendances | Portes `cargo audit`, `cargo deny` et `pnpm audit` dans la CI | ✅ Implémenté | `.github/workflows/back-dep-audit.yaml` |

---

## CC9 — atténuation des risques

| Critère | Contrôle | Statut | Preuve |
|-----------|---------|--------|---------|
| CC9.1 – Identifier les risques | Analyse de CVE par `cargo audit`, Trivy et OSV | ✅ Implémenté | `docs/contributing/security-scanning.md` |
| CC9.2 – Risque fournisseur | SBOM généré à chaque publication (CycloneDX) ; analyse de chaîne d'approvisionnement par le badge socket.dev | ✅ Implémenté | `GET /api/v1/sbom/export` |

---

## A1 — disponibilité

| Critère | Contrôle | Statut | Preuve |
|-----------|---------|--------|---------|
| A1.1 – Capacité de traitement actuelle | Métriques Prometheus et tableau de bord Grafana ; dimensionnement documenté | ✅ Implémenté | `deploy/grafana/batlehub-production.json`, `docs/guide/configuration.md` |
| A1.2 – Protections environnementales | Endpoint de santé ; alerte Prometheus `BatleHubDown` | ✅ Implémenté | `GET /healthz`, `GET /api/v1/admin/health`, `deploy/prometheus-alerts.yaml` |
| A1.3 – Sauvegarde et reprise | `pg_dump` Postgres et `rclone sync` S3 ; procédures de restauration | Documenté | `docs/operations/disaster-recovery.md` |

---

## Export pour la conformité

Le journal d'audit s'exporte pour un auditeur par :

```bash
# Export JSON (30 derniers jours)
batlehub admin export-audit-log --from 2026-06-01T00:00:00Z --format json --output audit.json

# Export CSV (pour une revue en tableur)
batlehub admin export-audit-log --from 2026-06-01T00:00:00Z --format csv --output audit.csv
```

Ou depuis la console → **Journal d'audit** → bouton **Exporter**.

---

## Trous et état des remédiations

| Trou | Priorité | Plan |
|-----|----------|------|
| TLS non appliqué par BatleHub lui-même | Faible | Documenter l'exigence de terminaison TLS dans le guide de déploiement |
| Extraction de l'IP et du user-agent pas encore branchée dans les appelants de `proxy_stream` | Moyenne | Faire passer `HttpRequest` dans les gestionnaires de proxy (prévu au prochain cycle) |
| Procédure de réponse à incident jamais testée | Moyenne | Planifier un exercice sur table |
