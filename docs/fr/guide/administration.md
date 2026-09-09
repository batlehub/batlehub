---
sourcePath: guide/administration.md
sourceHash: 2fc8547853dafcce
---

# Administration

Cette page couvre tout ce qu'un administrateur doit savoir pour exploiter
BatleHub : configuration, stockage, fournisseurs d'authentification, gestion des
registres, suivi de santé, nettoyage du cache, rechargement à chaud et bandeau
global.

Pour la référence TOML complète, voir
[`docs/guide/configuration.md`](https://github.com/batlehub/batlehub/blob/main/docs/guide/configuration.md)
(la version française est [Configuration](/fr/guide/configuration)).

## Sommaire des pages

Le guide d'administration est réparti sur quatre pages :

- **[Configuration](/fr/guide/admin-config)** — le fichier de configuration TOML
  et son ordre de chargement, l'injection de secrets `${VAR_NAME}`, les
  surcharges nommées `PROXY_CACHE__*`, les modes de registre, les fournisseurs
  d'authentification (tokens statiques, OIDC, Kubernetes, tokens utilisateur), le
  **rechargement à chaud** de la configuration et le **bandeau global**.
- **[Stockage et santé](/fr/guide/admin-storage-health)** — le **stockage** sur
  système de fichiers, compatible S3 et multi-backend, ainsi que la **santé et
  l'observabilité** : l'endpoint de santé, le vidage du cache et le traçage
  OpenTelemetry.
- **[Politiques et paquets](/fr/guide/admin-policies)** — la **politique de
  cache** (éviction, préchauffage, déduplication), la **gestion des paquets**
  (lister, bloquer, bloquer en masse, invalider) et les **règles** par registre
  (garde-fou d'âge de publication, refus de `latest`, publieur de confiance).
- **[Accès et audit](/fr/guide/admin-access)** — les **namespaces d'équipe et la
  visibilité des paquets**, le **journal d'audit**, le **canal bêta / de
  pre-release** et le **blocage par IP**.
