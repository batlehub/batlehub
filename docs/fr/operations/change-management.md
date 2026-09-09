---
sourcePath: operations/change-management.md
sourceHash: 407cd7c996ed8d98
---

# Politique de gestion du changement — BatleHub

Ce document décrit comment les changements de code, de configuration et de
dépendances de BatleHub sont relus, approuvés et déployés **dans le dépôt du
projet lui-même**.

::: warning Des repères, pas un engagement
Deux publics lisent cette page et en attendent des choses différentes. Si vous
cherchez des éléments de preuve pour le contrôle SOC 2 CC8, voici ce que le
projet propose — la description de sa propre pratique, pas une attestation
qu'elle a été vérifiée (voir le [tableau SOC 2](/fr/operations/soc2-checklist)).
Si vous exploitez BatleHub, c'est un exemple travaillé à adapter : votre
processus de changement est le vôtre, et rien ici n'engage le projet sur une
cadence de publication, un délai de relecture ou une fenêtre de dépréciation.
:::

---

## Périmètre

Cette politique couvre :
- les changements de code source (crates Rust, frontend Vue, CLI) ;
- les changements de configuration (fichiers TOML, variables d'environnement) ;
- les mises à jour de dépendances (`Cargo.lock`, `pnpm-lock.yaml`) ;
- les changements d'infrastructure (images de conteneur, manifestes Kubernetes,
  pipelines de CI/CD) ;
- les changements de schéma de base (migrations dans
  `crates/adapters/migrations/`).

---

## Changements de code et d'infrastructure

### Les changements ordinaires (tout ce qui n'est pas une urgence)

1. **Brancher** — créez une branche de fonctionnalité depuis `main`.
2. **Développer** — écrivez le code en local ; lancez `cargo test --workspace` et
   `cargo clippy --workspace -- -D warnings` avant de pousser.
3. **Demande de fusion** — ouvrez une PR ; décrivez *ce qui* change et
   *pourquoi*. Liez le ticket ou l'issue.
4. **Portes automatiques** (toutes doivent passer avant la fusion) :
   - `cargo test --workspace` — tests unitaires et d'intégration
   - `cargo clippy --workspace -- -D warnings` — aucun avertissement
   - `cargo fmt --all --check` — le formatage
   - `cargo audit` — aucune alerte RUSTSEC non corrigée
   - `cargo deny check` — politique de licences, de bannissements et de sources
   - `pnpm audit --audit-level high` — aucune CVE JS haute ou critique
   - `gitleaks` — aucun secret dans le diff
   - `trivy` — aucun HIGH ou CRITICAL corrigeable dans l'image construite
5. **Relecture par un pair** — au moins une relecture approuvée est requise.
6. **Fusionner** — fusion écrasée (squash) vers `main`.
7. **Déployer** — la CI construit et pousse l'image ; le pipeline de déploiement
   l'applique d'abord en préproduction, puis en production.

### Les changements d'urgence (incident P0)

Quand le confinement exige un correctif immédiat en production :

1. Implémentez le correctif sur une branche ; au minimum, relisez le diff
   vous-même.
2. Lancez `cargo test --workspace` en local — ne sautez pas les tests.
3. Accélérez les portes automatiques (la CI tourne quand même ; ne la contournez
   pas).
4. Prévenez le responsable sécurité et l'astreinte qu'une fusion d'urgence est en
   cours.
5. Fusionnez et déployez.
6. Ouvrez une PR de suivi dans les 24 heures, avec un commentaire de retour
   d'expérience rétroactif.

---

## Les changements de configuration

### La configuration d'exécution (TOML et variables d'environnement)

Tout changement de configuration qui affecte le comportement à l'exécution
(nouveaux registres, changements de règles RBAC, surcharges de limitation de
débit, ajouts de blocages d'IP) doit être :

1. **Relu** — au moins une autre personne doit voir le diff.
2. **Appliqué par rechargement de configuration** quand c'est possible — employez
   `POST /api/v1/admin/config/reload` pour permuter la configuration à chaud sans
   redémarrage. BatleHub journalise et enregistre chaque rechargement dans la
   table `config_changes`.
3. **Auditable** — l'historique des changements de configuration s'interroge :

   ```bash
   batlehub admin config changes
   # ou depuis la console → Rechargement de la configuration → Historique
   ```

4. **Réversible** — gardez la version précédente de la configuration sous
   gestion de version. Revenez en arrière en déployant le commit antérieur.

### Les migrations de base

Les migrations SQL vivent dans `crates/adapters/migrations/`, avec des numéros
séquentiels. Les règles :

- ne modifiez jamais une migration existante (elle peut déjà être appliquée en
  production) ;
- une nouvelle migration doit être additive quand c'est possible (ajouter des
  colonnes, pas en supprimer) ;
- toutes les migrations s'exécutent automatiquement au démarrage du serveur, par
  le migrateur embarqué ;
- testez les migrations avec `task test:pg-cache` avant de fusionner.

---

## Les mises à jour de dépendances

### Les mises à jour de routine

- Dependabot ou Renovate ouvre automatiquement des PR pour les montées de version
  correctives et mineures.
- Relisez le changelog à la recherche de ruptures ou d'implications de sécurité.
- Vérifiez que `cargo audit` et `cargo deny check` passent toujours après la mise
  à jour.

### Les correctifs de sécurité

Quand une alerte RUSTSEC est publiée pour une dépendance que nous employons :

1. Le job de CI `back-dep-audit` de BatleHub échouera dans les 24 heures suivant
   la publication de l'alerte.
2. Vérifiez si l'alerte concerne l'arbre de dépendances direct ou transitif :
   `cargo tree -i <crate>`.
3. Montez la version de la dépendance (ou appliquez un stub
   `[patch.crates-io]`, comme cela a été fait pour `sqlx-macros` et
   `sqlx-mysql`).
4. Vérifiez que `cargo audit` et `cargo deny check` passent en local.
5. Ouvrez la PR et accélérez-la (une relecture reste requise).

### Les opérations interdites

| Action | Raison |
|--------|--------|
| `features = ["macros"]` sur `sqlx` | Fait entrer la crate `rsa` (RUSTSEC-2023-0071) |
| Fonctionnalités par défaut sur `aws-sdk-s3` ou `aws-config` | Fait entrer l'ancien `rustls` (RUSTSEC-2026-0098) |
| Suppressions par `advisories.ignore` ou `.cargo/audit.toml` | Politique sans suppression — corriger ou patcher plutôt |
| `--no-verify` sur un commit | Contourne les hooks de pré-commit |

Ces interdictions sont appliquées par les règles `cargo deny` de `deny.toml` et
font échouer la CI.

---

## Les changements de politique RBAC

Un changement de règles RBAC par registre affecte les paquets que les
utilisateurs peuvent ou ne peuvent pas télécharger. Avant de l'appliquer :

1. Servez-vous du simulateur RBAC pour valider l'effet voulu :

   ```bash
   batlehub admin access-check --registry npm --package lodash \
     --version 4.17.21 --resource releases:read --role user
   ```

2. Testez au moins un cas « refusé » et un cas « autorisé ».
3. Appliquez par rechargement de configuration (les changements sont journalisés
   automatiquement).
4. Surveillez le journal d'audit à la recherche d'événements `denied` inattendus
   après application.

---

## La piste d'audit

Les opérations suivantes sont automatiquement journalisées dans la table
`access_events` (avec l'identité de l'utilisateur, l'adresse IP et le
user-agent) :

- les téléchargements de paquets (autorisés et refusés) ;
- les publications, retraits, suppressions, blocages et déblocages de paquets ;
- les révocations de tokens par un administrateur.

Les changements de configuration sont journalisés dans la table
`config_changes`.

Pour exporter tous les événements d'audit d'une période :

```bash
batlehub admin export-audit-log \
  --from 2026-01-01T00:00:00Z --to 2026-03-31T23:59:59Z \
  --format csv --output q1-2026-audit.csv
```

---

## Revue annuelle

Cette politique devrait être revue au moins une fois par an par le responsable
sécurité, et mise à jour pour refléter les changements d'outillage, de taille
d'équipe ou d'exigences de conformité.

Dernière revue : 2026-06-28
