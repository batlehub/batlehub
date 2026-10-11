---
sourcePath: operations/compliance-nis2-dora.md
sourceHash: c6525abe691a6ca9
---

# NIS2 et DORA — ce qui existe, ce qui est prévu

NIS2 (dir. 2022/2555) oblige les entités essentielles et importantes ; DORA
(règl. 2022/2554) oblige les entités financières et, par leurs contrats, les
prestataires TIC dont elles dépendent. Ni l'un ni l'autre n'oblige un
logiciel, et aucun exploitant régulé ne fait encore tourner BatleHub : le
projet les traite donc comme un **objectif futur**. Cette page liste ce qui
sert déjà leurs mesures et ce que la dernière phase de la RFC 0036 ajouterait
lorsque le premier exploitant régulé en aura besoin.

::: warning Pas encore un objectif que le projet atteint
Les lignes de la section *Ce qu'ajoute la phase 7* ne sont **pas construites**.
Elles sont listées pour qu'un exploitant régulé voie l'écart avant d'adopter
BatleHub, et pour que le socle (RGPD, ISO 27001, CRA) n'ait pas à être refait
pour les atteindre. « ✅ Implémenté » signifie que le contrôle existe dans le
code, à l'endroit nommé dans la colonne Preuve. Voir [Conformité](./compliance.md).
:::

---

## NIS2 — mesures de gestion des risques de l'art. 21(2)

| Mesure | Contrôle BatleHub | Statut | Preuve |
|--------|-------------------|--------|--------|
| (b) – Gestion des incidents | Procédure de réponse à incident ; flux d'audit et règles Sigma pour la détection | Documenté | [Réponse à incident](./incident-response.md), [Intégration SIEM](./siem.md) |
| (c) – Continuité d'activité, sauvegarde | Procédures de sauvegarde et de restauration ; réplicas sans état | Documenté | [Reprise après sinistre](./disaster-recovery.md), [Haute disponibilité](../guide/high-availability.md) |
| (d) – Sécurité de la chaîne d'approvisionnement | Analyse, signalements du SOC, verdicts et barrières d'âge de publication sur chaque artefact relayé ; un rapport d'exposition de qui a récupéré une version signalée | ✅ Implémenté | [Scan worker](./scan-worker.md), `batlehub-cli admin exposure` |
| (e) – Traitement et divulgation des vulnérabilités | Signalement privé, délais et avis de sécurité du projet | Documenté | `SECURITY.md`, [Correspondance CRA](./compliance-cra.md) |
| (h) – Cryptographie | Signatures Ed25519 sur les publications et sur la chaîne de scellement de l'audit | ✅ Implémenté | `crates/core/src/services/signature.rs`, `[audit] seal_signing_key` |
| (i) – Contrôle d'accès | Droits et verbes ; chaque événement d'authentification est audité | ✅ Implémenté | [Contrôle d'accès](../guide/access-control.md), [Correspondance ISO 27001](./compliance-iso27001.md) |
| (j) – Authentification multifacteur | Imposée par votre IdP ; BatleHub ne la vérifie pas | Processus manuel | phase 7 ci-dessous |

## NIS2 — notifications de l'art. 23

L'art. 23 demande à une entité une alerte précoce dans les 24 heures après
avoir eu connaissance d'un incident important, une notification dans les
72 heures et un rapport final dans le mois. Les notifications reviennent à
l'exploitant ; BatleHub fournit les éléments de preuve à partir desquels elles
sont rédigées.

| Obligation | Contrôle BatleHub | Statut | Preuve |
|------------|-------------------|--------|--------|
| Avoir connaissance de l'incident | Règles Sigma sur le flux d'audit ; alertes Prometheus | ✅ Implémenté | `deploy/siem/sigma/`, `deploy/prometheus-alerts.yaml` |
| Les faits de l'incident | Le journal d'audit de la fenêtre, exporté | ✅ Implémenté | `batlehub-cli admin export-audit-log --from <start> --to <end>` |
| Les faits n'ont pas été modifiés | La chaîne de scellement se vérifie sur la fenêtre, et sa tête correspond à la copie du SIEM | ✅ Implémenté | `batlehub-cli admin audit verify --from <start> --to <end> --head <digest>` |
| Les notifications elles-mêmes | 24 h / 72 h / un mois | Processus manuel | [Réponse à incident § NIS2 art. 23](./incident-response.md#nis2-reporting) |

## DORA — art. 9–12, 18, 30

| Article | Contrôle BatleHub | Statut | Preuve |
|---------|-------------------|--------|--------|
| 9 – Protection et prévention | Contrôle d'accès, blocage d'IP, limitation de débit, tokens hachés | ✅ Implémenté | [Durcissement en production](./production-hardening.md) |
| 10 – Détection | Flux d'audit, règles Sigma, alertes Prometheus | ✅ Implémenté | [Intégration SIEM](./siem.md) |
| 11 – Réponse et rétablissement | Procédure de réponse à incident | Documenté | [Réponse à incident](./incident-response.md) |
| 12 – Sauvegarde et restauration | Procédures de sauvegarde et de restauration | Documenté | [Reprise après sinistre](./disaster-recovery.md) |
| 12 – Restauration testée, avec RPO/RTO | — | Non construit | phase 7 ci-dessous |
| 18 – Classification des incidents | — | Non construit | phase 7 ci-dessous |
| 30 – Clauses contractuelles avec un prestataire TIC | — | Non construit | phase 7 ci-dessous |

---

## Ce qu'ajoute la phase 7 — pas encore construit

La [RFC 0036 §12](/rfc/0036-regulatory-alignment#_12-implementation-phases)
les nomme, à construire **lorsqu'un exploitant régulé en aura besoin**. Aucun
n'existe aujourd'hui.

| Ajout | Sert | Ce que ce serait |
|-------|------|------------------|
| MFA imposée | NIS2 21(2)(j), DORA 9 | Un réglage de fournisseur qui refuse une connexion dont les claims OIDC `acr` / `amr` ne montrent pas de second facteur |
| Clés de signature détenues par un KMS | NIS2 21(2)(h), DORA 9 | Les clés de signature des publications, d'APK, de VS Code et du scellement conservées dans un KMS, avec une procédure de rotation écrite |
| Test de restauration en CI | DORA 12 | Une restauration planifiée d'une sauvegarde dans une instance neuve, mesurée contre un RPO et un RTO annoncés |
| Classification des incidents | DORA 18, NIS2 23 | Des champs de classification (gravité, services touchés, impact sur les données) sur les notifications |
| Annexe contractuelle art. 30 | DORA 30 | Un modèle d'annexe pour une offre hébergée : niveaux de service, droits d'audit et d'accès, sortie, sous-traitance |

D'ici là, un exploitant régulé couvre ces points par ses propres contrôles : la
MFA à l'IdP, les clés dans son propre coffre à secrets, ses propres exercices de
restauration et sa propre classification dans le ticket.
