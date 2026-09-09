---
sourcePath: guide/features.md
sourceHash: 89c4a1323a41ed8e
---

# Fonctionnalités

Tout ce que fait le serveur, en une liste. La page d'accueil en montre trois
parce qu'un visiteur qui décide s'il installe a besoin d'une introduction ;
celle-ci s'adresse au lecteur qui a décidé et qui veut la spécification.

Chaque entrée dit ce qu'est la fonctionnalité. Pour savoir où elle se configure,
la référence est [Configuration](/fr/guide/configuration).

## Cache et stockage

**Mise en cache des artefacts.** Le premier téléchargement est récupéré en amont
puis stocké localement ou sur S3. Toutes les requêtes suivantes sont servies
depuis le cache — rapide, et sans consommer de bande passante. Voir
[Mise en cache](/fr/guide/caching).

**Préchauffage et éviction du cache.** Récupérez des paquets à l'avance au
démarrage pour supprimer la latence à froid. Évincez par TTL, par durée
d'inactivité, par nombre de versions ou par plafond de stockage — les critères se
combinent, registre par registre.

**Déduplication du stockage.** Des octets d'artefact identiques ne sont stockés
qu'une fois, quel que soit le nombre de registres ou de noms de paquets qui les
référencent. Compté par références, et rétrocompatible.

**Diffusion vers plusieurs amonts.** Déclarez plusieurs amonts par registre. Un
404 de l'un bascule automatiquement vers le suivant — plus de point de défaillance
unique.

## Publication

**Registres privés.** Publiez des paquets npm privés, des crates Cargo, des
modules Go, des extensions VS Code, des wheels Python, des paquets conda, des
paquets NuGet et bien d'autres directement sur BatleHub. Utilisez le mode `local`
ou `hybrid`, registre par registre. Voir [Publier](/fr/use/publishing) et la
[référence des registres](/fr/registries/).

**Canal bêta / pre-release.** Réservez les versions de pre-release (par exemple
`1.0.0-beta.1`) à des utilisateurs ou des groupes approuvés. Les autres ne voient
que les versions stables — sans étape de publication séparée. Voir
[Contrôle d'accès](/fr/guide/access-control).

## Contrôle d'accès et identité

**Contrôle d'accès par rôle.** Permissions par registre pour les rôles anonyme,
utilisateur et admin. Accès par groupe depuis les claims OIDC, les comptes de
service Kubernetes ou les tokens OIDC de GitHub / Forgejo Actions. Voir
[Contrôle d'accès](/fr/guide/access-control).

**Authentification OIDC des Actions.** Validez les JWT de workflow GitHub et
Forgejo sans secret à longue durée de vie. Associez n'importe quel claim — dépôt,
branche, environnement — à des groupes et des rôles par des règles glob ou
regex. Des noms de groupe dynamiques comme `{name}/{repository}/{ref_name}`
permettent des autorisations RBAC génériques sur tous les jobs de CI.

**Tokens statiques hachés.** Stockez des empreintes Argon2id au format PHC dans
la configuration plutôt que les tokens en clair. `batlehub hash-token <value>`
produit l'empreinte. Les tokens en clair continuent de fonctionner — les deux
formats coexistent.

## Protéger l'instance

**Garde-fou d'âge de publication.** Refuse les paquets publiés il y a moins de N
secondes. Crée un délai d'attente face aux attaques sur la chaîne
d'approvisionnement, sans bloquer les versions déjà connues comme saines.

**Limitation de débit distribuée.** Limites à fenêtre fixe, par utilisateur et par
groupe. Les compteurs vivent en mémoire, dans PostgreSQL ou dans Redis — des
limites partagées qui survivent aux redémarrages et se répartissent entre les
réplicas.

**Blocage par IP.** Blocage automatique façon Fail2ban. Une IP qui dépasse un
seuil de violations (limites de débit atteintes, échecs d'authentification) est
bloquée d'elle-même. Bannissement et débannissement manuels par l'API
d'administration.

## Exploitation

**OpenTelemetry.** Traçage distribué facultatif via OTLP/gRPC. Fonctionne
immédiatement avec Jaeger, Tempo ou tout backend compatible OTLP.

**Haute disponibilité.** Plusieurs réplicas derrière une seule adresse, avec un
cache et un état de limitation de débit partagés. Voir
[Haute disponibilité](/fr/guide/high-availability).

**SBOM et données de vulnérabilité.** Générez et servez un SBOM par artefact, et
faites proxy des bases d'alertes que vos outils interrogent déjà. Voir
[SBOM](/fr/guide/sbom) et
[Proxy de vulnérabilités](/fr/use/vulnerability-proxy).
