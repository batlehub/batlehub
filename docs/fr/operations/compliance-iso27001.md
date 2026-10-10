---
sourcePath: operations/compliance-iso27001.md
sourceHash: 78d1b5e92e73863c
---

# ISO/IEC 27001:2022 — les contrôles de BatleHub

Cette page met les contrôles de l'annexe A d'ISO/IEC 27001:2022 qu'un proxy de
paquets sert en regard des contrôles qu'implémente BatleHub. Un exploitant dont
le SMSI couvre une instance BatleHub peut la remettre à son auditeur comme point
de départ de la déclaration d'applicabilité.

::: warning Une mise en correspondance, pas un certificat
**Le certificat appartient à votre SMSI, pas au logiciel.** « ✅ Implémenté »
signifie que le contrôle existe dans le code, à l'endroit nommé dans la colonne
Preuve ; cela ne dit rien de son activation dans votre déploiement ni de son
fonctionnement sur la période auditée. Voir [Conformité](./compliance.md).
:::

**Périmètre** : le serveur BatleHub, sa CLI, son scan worker, ainsi que le
développement et le traitement des vulnérabilités du projet lui-même. Les
contrôles organisationnels (5.1–5.14), les contrôles liés aux personnes (6.x)
et les contrôles physiques (7.x) vous appartiennent et ne sont pas listés.

---

## 5 — Contrôles organisationnels

| Contrôle | Contrôle BatleHub | Statut | Preuve |
|----------|-------------------|--------|--------|
| 5.15 – Contrôle d'accès | Des droits sur des verbes, le long de la hiérarchie registre → paquet → version ; une décision est un refus sauf si un droit l'autorise | ✅ Implémenté | [Contrôle d'accès](../guide/access-control.md), `POST /api/v1/admin/access-check` |
| 5.16 – Gestion des identités | Les identités viennent de votre IdP (OIDC), de comptes de service Kubernetes, de l'OIDC d'une CI ou de tokens statiques ; les noms de fournisseurs sont uniques | ✅ Implémenté | `[[auth]]` dans la [configuration](../guide/configuration.md), `crates/adapters/src/auth/` |
| 5.17 – Informations d'authentification | Tokens d'accès personnels stockés sous forme d'empreinte SHA-256, avec expiration ; la valeur n'est montrée qu'une fois | ✅ Implémenté | `crates/adapters/src/db/auth/user_tokens.rs`, `crates/adapters/src/auth/user_token.rs` |
| 5.18 – Droits d'accès | Révoquer un token, bloquer un utilisateur, lister et retirer des droits | ✅ Implémenté | `DELETE /api/v1/auth/tokens/{id}`, `POST /api/v1/admin/users/{user_id}/block`, `batlehub-cli admin grants` |
| 5.18 – Revue périodique des accès | Revoir droits et tokens selon un calendrier | Processus manuel | `batlehub-cli admin grants`, `GET /api/v1/admin/audit-log?action=sign_in` |
| 5.21 – Chaîne d'approvisionnement TIC | Chaque artefact relayé peut être analysé (SBOM, CVE, verdicts), signalé par votre SOC, retenu par une barrière d'âge de publication ou bloqué | ✅ Implémenté | [SBOM](../guide/sbom.md), [Scan worker](./scan-worker.md), `crates/core/src/rules/` |
| 5.21 – La chaîne d'approvisionnement de BatleHub | `cargo audit`, `cargo deny`, `pnpm audit`, postmortem, Trivy ; aucune exception | ✅ Implémenté | `.github/workflows/back-dep-audit.yaml`, `.github/workflows/image-scan.yaml`, `deny.toml` |
| 5.24–5.27 – Gestion des incidents | Procédure : gravité, détection, confinement, éradication, reprise, retour d'expérience | Documenté | [Réponse à incident](./incident-response.md) |
| 5.28 – Collecte de preuves | Export du journal d'audit, et une chaîne de scellement signée dont `verify` prouve que la fenêtre exportée n'a pas été modifiée | ✅ Implémenté | `batlehub-cli admin export-audit-log`, `batlehub-cli admin audit verify` |
| 5.33 – Protection des enregistrements | Les lignes de sécurité ne disparaissent que par leur conservation — une purge manuelle n'atteint que les lignes d'accès, si bien que l'enregistrement d'une purge survit aux purges suivantes | ✅ Implémenté | `DELETE /api/v1/admin/audit-log?before=`, `[audit] security_retention_days` |
| 5.34 – Vie privée et données personnelles | Classes de conservation, pseudonymisation, effacement et export pour le droit d'accès | ✅ Implémenté | [Correspondance RGPD](./compliance-gdpr.md) |

## 8 — Contrôles technologiques

| Contrôle | Contrôle BatleHub | Statut | Preuve |
|----------|-------------------|--------|--------|
| 8.2 – Droits d'accès privilégiés | Les actions d'administration exigent des verbes d'admin ; `gdpr:erase` est un verbe à part, que `audit:purge` n'implique pas | ✅ Implémenté | [Contrôle d'accès § verbes](../guide/access-control.md#verbs) |
| 8.5 – Authentification sécurisée | Connexion OIDC ; chaque connexion, échec, création et révocation de token et identifiant refusé est un événement d'audit | ✅ Implémenté | `sign_in`, `sign_in_failed`, `token_create`, `token_revoke`, `credential_rejected` dans [Intégration SIEM](./siem.md) |
| 8.5 – MFA | Imposée par votre IdP ; BatleHub ne vérifie pas encore les claims `acr`/`amr` | Processus manuel | [NIS2 et DORA](./compliance-nis2-dora.md) |
| 8.7 – Protection contre les logiciels malveillants | Scan worker en bac à sable (GuardDog, Trivy, règles YARA fournies par l'exploitant) ; une correspondance est un constat sur lequel la politique peut bloquer | ✅ Implémenté | [Scan worker](./scan-worker.md), `[scanners.yara] rules_dir` dans la [configuration](../guide/configuration.md#scanners-and-worker) |
| 8.8 – Vulnérabilités techniques (les vôtres) | Analyse CVE des artefacts relayés ; rapport d'exposition de qui a récupéré une version signalée | ✅ Implémenté | [Réponse à incident § qui a récupéré une version signalée](./incident-response.md#who-pulled-a-flagged-version) |
| 8.8 – Vulnérabilités techniques (celles du projet) | Signalement privé, délais annoncés, avis de sécurité avec CVE, VEX | Documenté | `SECURITY.md`, [Correspondance CRA](./compliance-cra.md) |
| 8.9 – Gestion de la configuration | Chaque rechargement appliqué ou refusé est enregistré avec son auteur et son diff ; les avertissements de validation sont listés | ✅ Implémenté | `GET /api/v1/admin/config/changes`, `GET /api/v1/admin/config/warnings` |
| 8.10 – Suppression des informations | Expiration par classe de conservation ; effacement d'une personne | ✅ Implémenté | `[audit]`, `batlehub-cli admin gdpr erase` |
| 8.11 – Masquage des données | Lignes de la classe d'accès pseudonymisées passé un âge donné | ✅ Implémenté | `[audit] pseudonymise_after_days` |
| 8.13 – Sauvegarde des informations | Procédures de sauvegarde et de restauration de Postgres et du stockage objet | Documenté | [Reprise après sinistre](./disaster-recovery.md) |
| 8.15 – Journalisation | Chaque téléchargement, publication, blocage, droit, purge, changement de configuration et événement d'authentification dans `access_events`, avec utilisateur, heure, IP et user agent | ✅ Implémenté | `GET /api/v1/admin/audit-log` |
| 8.15 – Journaux protégés contre l'altération | Chaîne de hachage signée sur les fenêtres closes ; troncature détectée contre la dernière ligne `audit_seal` du SIEM | ✅ Implémenté | `[audit] seal_interval_secs`, `batlehub-cli admin audit verify --head <digest>`, `deploy/siem/sigma/audit_chain_gap.yml` |
| 8.15 – Une copie hors de l'hôte | Le flux d'audit JSON sur la sortie standard, pour un collecteur | ✅ Implémenté | `[logging] format = "json"`, [Intégration SIEM](./siem.md) |
| 8.15 – Stockage non réinscriptible | Stockage immuable pour la copie expédiée (S3 Object Lock, bucket WORM) | Processus manuel | la destination de votre collecteur |
| 8.16 – Activités de surveillance | Règles Sigma sur le flux ; alertes Prometheus | ✅ Implémenté | `deploy/siem/sigma/`, `deploy/prometheus-alerts.yaml` |
| 8.16 – Suivre les alertes | Quelqu'un pour les lire | Processus manuel | — |
| 8.20 – Sécurité des réseaux | Blocage d'IP, limitation de débit, règles de confiance des proxys | ✅ Implémenté | `crates/web/src/middleware/ip_block.rs`, [Durcissement en production](./production-hardening.md) |
| 8.24 – Utilisation de la cryptographie | Signatures Ed25519 des publications et des scellements ; poussées de signalements signées par HMAC ; TLS à l'ingress | ✅ Implémenté | `crates/core/src/services/signature.rs`, `[audit] seal_signing_key` |
| 8.24 – Gestion des clés | Conserver les clés de signature dans un KMS et les faire tourner | Processus manuel | [NIS2 et DORA](./compliance-nis2-dora.md) |
| 8.25–8.28 – Développement sécurisé | Revue obligatoire, clippy en `-D warnings`, CodeQL, Semgrep, gitleaks, cibles de fuzzing | ✅ Implémenté | `.github/workflows/codeql.yaml`, `.github/workflows/semgrep.yaml`, `.github/workflows/secret-scan.yaml`, [Gestion du changement](./change-management.md) |
| 8.32 – Gestion des changements | Revue des pull requests, migrations, journal des changements de configuration | Documenté | [Gestion du changement](./change-management.md) |

---

## Écarts

| Écart | Plan |
|-------|------|
| La MFA n'est pas vérifiée par BatleHub lui-même | RFC 0036, phase 7 — [NIS2 et DORA](./compliance-nis2-dora.md) |
| Les clés de signature sont des fichiers ou des variables d'environnement, pas des clés détenues par un KMS | RFC 0036, phase 7 |
