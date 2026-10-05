---
sourcePath: operations/compliance-cra.md
sourceHash: 003e0f0699098e56
---

# Cyber Resilience Act — les contrôles de BatleHub

Cette page met les parties du Cyber Resilience Act (règl. 2024/2847) qui
concernent le fabricant d'un produit logiciel en regard de ce que fait le
projet BatleHub. Contrairement aux autres pages de correspondance, la plupart
des lignes ci-dessous sont dues par le **projet**, et non par l'exploitant.

::: warning Pas encore contraignant
**Le CRA oblige un fabricant qui met un produit sur le marché de l'UE dans le
cadre d'une activité commerciale.** BatleHub est aujourd'hui un logiciel libre
non commercial ; le projet considère donc qu'il est hors du champ du
règlement. Le jour où BatleHub est proposé avec un support payant ou monétisé
d'une autre manière, le mainteneur devient fabricant et les lignes ci-dessous
s'appliquent dès la première version commerciale ; elles sont construites dès
maintenant pour que ce jour-là ne soit pas un rattrapage. Une organisation qui
intègre BatleHub dans un produit qu'elle vend est elle-même le fabricant de ce
produit.

« ✅ Implémenté » signifie que le contrôle existe dans le code ou le dépôt, à
l'endroit nommé dans la colonne Preuve. Voir [Conformité](./compliance.md).
:::

**Périmètre** : le dépôt BatleHub, sa chaîne de publication et son traitement
des vulnérabilités.

---

## Annexe I, partie II — Traitement des vulnérabilités

| Exigence | Contrôle | Statut | Preuve |
|----------|----------|--------|--------|
| II(1) – Identifier les composants ; un SBOM | Un SBOM CycloneDX pour l'espace de travail Rust et pour l'image, joint et attesté à chaque version | ✅ Implémenté | `.github/workflows/build.yaml` |
| II(2) – Corriger sans délai | Délais annoncés : accusé de réception sous 5 jours ouvrés, évaluation sous 10, correctif critique sous 30 jours, élevé sous 60 | Documenté | `SECURITY.md` § What happens next |
| II(3) – Tests et revues réguliers | Barrières de dépendances, de conteneur, d'analyse statique et de secrets sur chaque pull request et chaque jour | ✅ Implémenté | `.github/workflows/back-dep-audit.yaml`, `.github/workflows/image-scan.yaml`, `.github/workflows/codeql.yaml`, [Analyse des vulnérabilités](/contributing/security-scanning) |
| II(4) – Divulguer les vulnérabilités corrigées | Un avis de sécurité GitHub avec CVE, une entrée `### Security` au changelog et une déclaration VEX, dans la même version | Documenté | [Publier un avis de sécurité](/contributing/security-scanning#advisory), `CHANGELOG.md`, `vex/batlehub.openvex.json` |
| II(5) – Politique de divulgation coordonnée | 90 jours après le signalement, ou au correctif | Documenté | `SECURITY.md` § Disclosure |
| II(6) – Une adresse de contact pour les signalements | Signalement privé de vulnérabilités GitHub ; le modèle d'issue public a été retiré | ✅ Implémenté | `SECURITY.md` § Reporting a vulnerability, `.github/ISSUE_TEMPLATE/config.yml` |
| II(7) – Distribuer les mises à jour de façon sûre | Images de version construites en CI, avec SBOM attestés | ✅ Implémenté | `.github/workflows/build.yaml` |
| II(8) – Mises à jour gratuites, avec avis | Chaque correctif paraît dans une version publique, avec son avis | Documenté | `SECURITY.md` § Supported versions |

## Annexe I, partie I — Exigences essentielles de cybersécurité

| Exigence | Contrôle | Statut | Preuve |
|----------|----------|--------|--------|
| I(2)(a) – Aucune vulnérabilité exploitable connue à la mise sur le marché | `cargo audit`, `cargo deny` et `pnpm audit` conditionnent la construction ; `advisories.ignore = []` | ✅ Implémenté | `deny.toml`, `task security` |
| I(2)(b) – Sécurisé par défaut | Une conservation absente, une chaîne de scellement non signée, un scellement sans flux et d'autres configurations à moitié faites sont refusés ou signalés au démarrage | ✅ Implémenté | `GET /api/v1/admin/config/warnings` |
| I(2)(d) – Protection contre les accès non autorisés | Fournisseurs d'authentification, droits, blocage d'IP, limitation de débit ; chaque refus est un événement d'audit | ✅ Implémenté | [Contrôle d'accès](../guide/access-control.md), [Intégration SIEM](./siem.md) |
| I(2)(e) – Confidentialité | TLS terminé à l'ingress ; tokens stockés sous forme d'empreinte | Processus manuel | [Durcissement en production](./production-hardening.md) |
| I(2)(f) – Intégrité | Sommes de contrôle des artefacts vérifiées à l'écriture en cache ; signatures de publication ; chaîne signée sur la piste d'audit | ✅ Implémenté | `crates/core/src/services/integrity.rs`, `crates/core/src/services/signature.rs`, `batlehub-cli admin audit verify` |
| I(2)(g) – Minimisation des données | Classes de conservation de l'audit et pseudonymisation | ✅ Implémenté | [Correspondance RGPD](./compliance-gdpr.md) |
| I(2)(l) – Surveillance de sécurité | Événements d'audit pour chaque action touchant la sécurité, un flux JSON et des règles Sigma | ✅ Implémenté | `[logging] format = "json"`, `deploy/siem/sigma/` |
| I(2)(m) – Suppression sûre des données | Effacement d'une personne concernée ; expiration par conservation | ✅ Implémenté | `batlehub-cli admin gdpr erase` |

## Articles 13 et 14 — Obligations du fabricant

| Article | Contrôle | Statut | Preuve |
|---------|----------|--------|--------|
| 13(8) – Période de support | La dernière version publiée reçoit les correctifs de sécurité ; pas de branche LTS. Une offre payante y indiquerait sa période de support | Documenté | `SECURITY.md` § Supported versions |
| 13(2) – Évaluation des risques de cybersécurité | Une évaluation écrite du produit | Processus manuel | pas encore écrite ; due à la première version commerciale |
| 14 – Notifier les vulnérabilités activement exploitées (alerte précoce sous 24 h au CSIRT et à l'ENISA) | Notification via la plateforme unique de signalement | Processus manuel | due à la première version commerciale |
| Annexe II – Informations destinées aux utilisateurs | Installation, configuration, durcissement et contact pour les vulnérabilités | Documenté | [Installation](../guide/installation.md), [Durcissement en production](./production-hardening.md), `SECURITY.md` |

---

## Écarts

| Écart | Plan |
|-------|------|
| Pas d'évaluation écrite des risques de cybersécurité | À la première version commerciale |
| Pas de procédure de notification au titre de l'art. 14 | À la première version commerciale ; elle reprend la procédure d'avis ci-dessus |
| Pas de `security.txt` | Le site de documentation est servi sous un chemin, sur un hôte dont le projet ne possède pas la racine ; `SECURITY.md` fait office de politique jusqu'à ce que le projet ait son propre domaine (RFC 0036 §6.5) |
