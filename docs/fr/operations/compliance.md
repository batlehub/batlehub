---
sourcePath: operations/compliance.md
sourceHash: 925c16ca6637d6a2
---

# Conformité

BatleHub se trouve sur la chaîne d'approvisionnement logicielle de qui
l'exploite, et cet exploitant est souvent audité. Cette page indique les cadres
réglementaires au regard desquels le projet est construit, qui chacun d'eux
oblige, et ce que le logiciel apporte pour y répondre. La position elle-même,
et ses raisons, se trouvent dans la
[RFC 0036](/rfc/0036-regulatory-alignment).

::: warning Un logiciel n'est jamais certifié
**Un certificat appartient à l'organisation qui exploite un service, jamais au
logiciel qu'elle exécute.** Aucun certificat ISO 27001, aucun rapport SOC 2 et
aucune attestation RGPD ne peut nommer BatleHub, et rien dans ces pages
n'affirme que le projet est conforme à quoi que ce soit.

Ce que le logiciel peut devoir, ce sont les *éléments de preuve* et les
*contrôles* qu'un auditeur demande à l'exploitant : une piste d'audit complète
et impossible à modifier en silence, des données personnelles dotées d'un
cycle de vie, un moyen de répondre à la demande d'une personne concernée, un
traitement des vulnérabilités aux délais annoncés. Les pages ci-dessous
rattachent chacun d'eux à la clause qu'il sert, au fichier, au réglage, à la
route ou à la commande qui l'implémente. Montrer qu'un contrôle est activé,
configuré et opérant dans votre déploiement vous revient.

Qui une réglementation oblige est dit tel que le projet le lit. Faites-le
confirmer par votre conseil juridique avant de vous y fier.
:::

## Qui chaque cadre oblige

| Cadre | Oblige | Position de BatleHub | Ce que le projet doit |
| --- | --- | --- | --- |
| RGPD (règl. 2016/679) | l'exploitant, responsable du traitement des données de ses utilisateurs | socle | minimisation et limitation de la conservation par défaut (art. 5, 25) ; outillage d'accès et d'effacement (art. 15, 17) ; la sécurité du traitement qu'il offre (art. 32) |
| ISO/IEC 27001:2022 | le SMSI de l'exploitant | socle | des preuves pour l'annexe A : journalisation (8.15), surveillance (8.16), contrôle d'accès (5.15–5.18, 8.2, 8.5), vulnérabilités (8.8), cryptographie (8.24), développement sécurisé (8.25–8.28), chaîne d'approvisionnement TIC (5.21) |
| Cyber Resilience Act (règl. 2024/2847) | un fabricant qui met un produit sur le marché de l'UE dans le cadre d'une activité commerciale | socle ; **pas encore contraignant** — le projet est non commercial | le traitement des vulnérabilités de l'annexe I, partie II, un SBOM, une période de support ; l'obligation de notification de l'art. 14 (en vigueur depuis le 2026-09-11) dès qu'il devient commercial |
| NIS2 (dir. 2022/2555) | les entités essentielles et importantes | objectif futur | les contrôles que nomment les mesures de son art. 21(2) : sécurité de la chaîne d'approvisionnement (d), traitement des vulnérabilités (e), contrôle d'accès et MFA (i, j) ; une détection qui rend possibles les notifications à 24 h / 72 h / un mois de l'art. 23 |
| DORA (règl. 2022/2554) | les entités financières et, par contrat, leurs prestataires TIC | objectif futur | journalisation et détection (art. 9–10), sauvegarde et restauration (art. 12), classification des incidents (art. 18) ; pour une instance hébergée, les clauses contractuelles de l'art. 30 |

Une **instance publique** est un rôle à part entière : son exploitant est
responsable du traitement, au sens du RGPD, de l'adresse IP de chaque
visiteur, et — s'il sert un client régulé — un prestataire tiers de services
TIC au sens de DORA.

## Les pages de correspondance

| Page | À lire quand |
| --- | --- |
| [RGPD](./compliance-gdpr.md) | données personnelles dans la piste d'audit, durées de conservation, demande d'une personne concernée |
| [ISO/IEC 27001:2022](./compliance-iso27001.md) | l'auditeur de votre SMSI demande quel contrôle de l'annexe A une fonctionnalité sert |
| [Cyber Resilience Act](./compliance-cra.md) | un questionnaire fournisseur demande comment le projet traite ses vulnérabilités |
| [NIS2 et DORA](./compliance-nis2-dora.md) | vous êtes une entité régulée, ou vous en servez une |
| [Checklist SOC 2](./soc2-checklist.md) | votre auditeur travaille à partir des Trust Service Criteria |
| [Intégration SIEM](./siem.md) | il vous faut le flux d'audit et ses règles de détection |

Chaque page emploie les mêmes statuts que la checklist SOC 2 :
**✅ Implémenté** signifie que le contrôle existe dans le code, à l'endroit
nommé dans la colonne Preuve ; **Documenté** signifie qu'une procédure écrite
existe ; **Processus manuel** désigne un processus que *vous* exécutez, et que
BatleHub ne peut pas exécuter à votre place. La page NIS2 et DORA ajoute
**Non construit** pour ce qui est prévu et n'existe pas encore.

## Activer les contrôles

La plupart des preuves ci-dessus restent inactives tant qu'elles ne sont pas
configurées, parce qu'une mise à jour ne doit jamais supprimer des données
que l'exploitant n'a pas demandé de supprimer :

```toml
[logging]
format = "json"                   # le flux d'audit, voir siem.md

[audit]
access_retention_days   = 365     # téléchargements et lectures de métadonnées
security_retention_days = 1095    # connexions, droits, blocages, purges, config
pseudonymise_after_days = 30      # lignes d'accès : IP tronquée, user agent supprimé
seal_interval_secs      = 300     # fenêtres chaînées par hachage et signées
seal_signing_key        = "${BATLEHUB_AUDIT_SEAL_KEY}"
erasure_key             = "${BATLEHUB_AUDIT_ERASURE_KEY}"
```

Sans table `[audit]`, rien n'expire, rien n'est pseudonymisé et rien n'est
scellé, et le serveur l'annonce par un avertissement au démarrage. Les valeurs
ci-dessus sont l'exemple documenté, pas une valeur par défaut ; choisissez les
vôtres avec la personne responsable de votre politique de conservation.
