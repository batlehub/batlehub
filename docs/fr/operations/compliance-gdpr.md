---
sourcePath: operations/compliance-gdpr.md
sourceHash: 333281e6c9024e19
---

# RGPD — les contrôles de BatleHub

Cette page met les articles du Règlement général sur la protection des données
(règl. 2016/679) qui concernent un proxy de paquets en regard des contrôles
qu'implémente BatleHub. L'exploitant est responsable du traitement des
données personnelles que détient une instance ; BatleHub est l'outil avec
lequel il les traite.

::: warning Une mise en correspondance, pas une déclaration de conformité
**Un logiciel ne peut pas être conforme au RGPD ; un traitement peut l'être.**
« ✅ Implémenté » signifie que le contrôle existe dans le code, à l'endroit
nommé dans la colonne Preuve. Qu'il soit activé, et que vos durées de
conservation et vos bases légales soient les bonnes, relève de votre décision
et de celle de votre DPO. Voir [Conformité](./compliance.md).
:::

**Périmètre** : le serveur BatleHub et sa base de données. Une copie du flux
d'audit expédiée vers un SIEM est une copie distincte que *vous* contrôlez : la
pseudonymisation et l'effacement dans la base ne l'atteignent pas
([Intégration SIEM](./siem.md)).

---

## Les données personnelles que stocke BatleHub

| Où | Quoi | Pourquoi |
| --- | --- | --- |
| `access_events` | `user_id`, `ip_address`, `user_agent`, `detail` | la piste d'audit : qui a lu, publié, bloqué ou s'est connecté, et d'où |
| `user_tokens` | propriétaire, nom du token, `last_used_at`, `last_used_ip` | les tokens d'accès personnels ; l'adresse alimente `token_new_source` |
| `user_blocks` | `user_id`, `blocked_by`, `reason` | la décision d'un admin de refuser un utilisateur |
| `ip_blocks`, `ip_violation_counters` | adresses IP des clients | le blocage d'IP, automatique et manuel |
| `published_by`, droits de propriété des paquets | l'identifiant de l'éditeur et du propriétaire | qui a publié une version et qui administre un paquet |

La valeur d'un token n'est jamais stockée (seulement son empreinte), et aucune
ligne d'audit ni ligne du flux ne porte un token, une empreinte, un code OIDC
ou un mot de passe.

---

## Art. 5 — Principes

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 5(1)(c) – Minimisation des données | La ligne d'audit contient un identifiant, une adresse et un user agent, jamais le corps d'une requête ni un identifiant secret | ✅ Implémenté | `crates/core/src/entities/access_log.rs` |
| 5(1)(e) – Limitation de la conservation | Deux classes de conservation : les lignes d'accès (`Download`, `ViewMetadata`) et les lignes de sécurité (toutes les autres actions, événements d'authentification compris), chacune expirée par la tâche de cycle de vie | ✅ Implémenté | `[audit] access_retention_days`, `[audit] security_retention_days` |
| 5(1)(e) – Limitation de la conservation | Passé un âge donné, les lignes de la classe d'accès perdent leur précision : IP tronquée en /24 (IPv4) ou /48 (IPv6), user agent supprimé | ✅ Implémenté | `[audit] pseudonymise_after_days` |
| 5(1)(e) – Choisir les durées | Des durées de conservation adaptées à vos finalités et à vos obligations légales | Processus manuel | votre registre des traitements |
| 5(1)(f) – Intégrité | Les fenêtres closes de la piste sont chaînées par hachage et signées ; une ligne réécrite sans la clé est détectée | ✅ Implémenté | `[audit] seal_interval_secs`, `[audit] seal_signing_key`, `batlehub-cli admin audit verify` |
| 5(2) – Responsabilité | Chaque passage du cycle de vie, chaque export et chaque effacement est lui-même un événement d'audit de la classe sécurité (`audit_lifecycle_run`, `gdpr_export`, `gdpr_erase`) | ✅ Implémenté | `GET /api/v1/admin/audit-log?action=gdpr_erase` |

## Art. 6 — Base légale

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 6(1)(f) – Intérêt légitime | Les lignes de sécurité gardent leur IP complète pendant toute leur conservation, parce que la source d'une connexion, d'un droit ou d'une purge *est* la preuve ; la conservation plus courte de la classe d'accès maintient l'ensemble proportionné | Processus manuel | documentez la base dans votre registre des traitements |
| 6(1)(c) – Obligation légale | Si une loi vous impose de conserver un enregistrement plus longtemps, réglez `security_retention_days` en conséquence | Processus manuel | `[audit] security_retention_days` |

## Art. 15–21 — Droits des personnes concernées

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 15 – Droit d'accès | Exporte toutes les lignes concernant une personne, y compris les lignes de publication et de propriété que l'effacement conserve ; enregistré comme `gdpr_export` | ✅ Implémenté | `batlehub-cli admin gdpr export --user <id>`, `GET /api/v1/admin/gdpr/export?user_id=<id>` (verbe `audit:read`) |
| 17 – Droit à l'effacement | Remplace l'identifiant de la personne par `erased:<hmac>` dans `access_events` — comme auteur, et là où une ligne de grant ou d'export écrite par un administrateur la désigne comme sujet —, les lignes de tokens et les lignes de blocage ; les lignes d'une même personne restent liées entre elles et à personne d'autre | ✅ Implémenté | `batlehub-cli admin gdpr erase --user <id>`, `POST /api/v1/admin/gdpr/erase` (verbe `gdpr:erase`), `[audit] erasure_key` |
| 17(3) – Exceptions | Les lignes de publication et de propriété sont conservées au titre de l'intérêt légitime : « qui a publié ceci » doit rester une question à laquelle chaque consommateur du paquet obtient une réponse | Documenté | [RFC 0036 §4.2](/rfc/0036-regulatory-alignment#_4-2-behaviour-rules) |
| 17 – Effacement face à une décision ouverte | Refusé pour une personne visée par une quarantaine ou un blocage ouvert, sauf forçage | ✅ Implémenté | `batlehub-cli admin gdpr erase --user <id> --force` |
| 17 – Qui peut effacer | `gdpr:erase` est un verbe à part ; `audit:purge` ne l'implique pas | ✅ Implémenté | [Contrôle d'accès § verbes](../guide/access-control.md#verbs) |
| 12 – Répondre sous un mois | Recevoir, vérifier et traiter la demande | Processus manuel | [Réponse à incident § données personnelles](./incident-response.md#pii-handling) |
| 21 – Droit d'opposition | Mettre une opposition en balance avec votre intérêt légitime | Processus manuel | — |

L'effacement exige `erasure_key`, un secret HMAC. Gardez-le **hors de la base
de données** — une variable d'environnement ou un coffre à secrets — faute de
quoi le pseudonyme est réversible par quiconque lit la base.

## Art. 25 — Protection des données dès la conception et par défaut

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 25(1) – Dès la conception | La pseudonymisation et l'expiration ne portent que sur des fenêtres closes et scellées, et chaque passage est chaîné dans l'enregistrement de scellement | ✅ Implémenté | `[audit]`, table `audit_seals` |
| 25(2) – Par défaut | Sans table `[audit]`, le comportement antérieur est conservé (rien n'expire) et un avertissement est journalisé au démarrage ; la console le liste | ✅ Implémenté | `GET /api/v1/admin/config/warnings` |
| 25(2) – Par défaut | Renseigner la table `[audit]`, pour que l'avertissement par défaut ne reste pas en production | Processus manuel | votre configuration |

## Art. 28, 30, 32–34 — Sous-traitants, registre, sécurité, violations

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 28 – Sous-traitants | Un SIEM, un hébergeur ou une chaîne de journaux qui reçoit le flux est votre sous-traitant | Processus manuel | [Intégration SIEM](./siem.md) |
| 30 – Registre des traitements | L'inventaire ci-dessus en est l'entrée ; le registre vous appartient | Processus manuel | cette page |
| 32 – Contrôle d'accès | Droits et verbes par registre, connexion OIDC, tokens hachés et à expiration | ✅ Implémenté | [Contrôle d'accès](../guide/access-control.md), `crates/adapters/src/db/auth/user_tokens.rs` |
| 32 – Chiffrement en transit | TLS terminé à l'ingress | Processus manuel | [Durcissement en production](./production-hardening.md) |
| 32 – Chiffrement au repos | Chiffrement de Postgres et du stockage objet | Processus manuel | les réglages de votre base et de votre bucket |
| 32 – Résilience et restauration | Procédures de sauvegarde et de restauration | Documenté | [Reprise après sinistre](./disaster-recovery.md) |
| 33 – Notification d'une violation (72 h) | Détecter la violation et la notifier à l'autorité de contrôle | Processus manuel | [Réponse à incident](./incident-response.md), le flux d'audit |
| 33(5) – Documenter une violation | L'export du journal d'audit et une chaîne vérifiée pour la fenêtre de l'incident | ✅ Implémenté | `batlehub-cli admin export-audit-log`, `batlehub-cli admin audit verify --from <start> --to <end>` |

## Art. 44 — Transferts

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 44 – Transferts vers des pays tiers | Chaque requête sortante du serveur, et ce qu'elle transporte | Documenté | [Ce qui quitte cette instance](./egress.md) |
| 44 – Où tournent l'instance et son SIEM | Choisir la région | Processus manuel | — |

---

## Écarts

| Écart | Plan |
|-------|------|
| Une copie déjà expédiée vers un SIEM n'est ni pseudonymisée ni effacée | La conservation propre au collecteur la régit ; alignez-la sur `[audit]` |
