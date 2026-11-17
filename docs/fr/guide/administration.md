---
sourcePath: guide/administration.md
sourceHash: f04f14fdb5956e33
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

## Où agir, selon ce qui arrive

Un *chemin rouge* mène de ce qui vous amène à la commande qui le résout
([RFC 0046](/rfc/0046-admin-red-paths)). Chaque ligne est aussi à un
`Ctrl+K` (`⌘K` sur macOS) : saisissez le mot de la dernière colonne dans la
palette *Aller à…* de la console.

| Ce qui arrive | Où | Palette |
| --- | --- | --- |
| « Je ne peux pas télécharger X » | *Accès → Autorisations*, panneau *Simuler* (`/admin/access/authorization#simulate`) | `refusé` |
| Un amont est dégradé | la page du registre, puis *Registres → Amont* (`/admin/registries/upstream`) | `amont` |
| La configuration d'un registre doit changer | la page du registre → *Modifier la configuration*, qui ouvre *Système → Configuration* sur son bloc | `config` |
| Une CVE : quelles versions la portent ? | *Sécurité → Constats* (`/security/findings`) | `cve` |
| Une analyse a échoué | *Sécurité → Analyseurs et worker*, onglet des échecs (`/security/scanners?tab=failures`) | `réanalyser` |
| Bloquer une version | la page du paquet, onglet *Admin* | `bloquer` |
| Bloquer plusieurs versions | *Paquets → Blocage de masse* (`/admin/packages/bulk`) | `masse` |
| Un jeton divulgué ou une adresse abusive | *Sécurité de l'instance → Comptes et adresses bloqués* (`/admin/instance-security/blocks`) | `jeton` |
| Un paquet semble périmé | *Système → Santé et cache* → *Rafraîchir l'explorateur* (`/admin/system/health`) | `périmé` |
| Qui a fait ceci ? | *Système → Journal d'audit* (`/admin/system/audit-log`) | `audit` |
| Annoncer une maintenance | *Système → Bandeau global* (`/admin/system/banner`) | `bannière` |
| Une équipe a besoin d'un namespace | *Accès → Namespaces d'équipe* (`/admin/access/team-namespaces`) | `namespace` |

Deux déclencheurs n'ont pas encore de commande dans la console et relèvent de
leur propre RFC : **révoquer tous les jetons d'un utilisateur** (seul le
blocage du compte existe, ci-dessus) et **ce qui a changé dans la
configuration la semaine dernière** au-delà de l'historique affiché sur
*Système → Configuration*.

Les favoris vers les anciennes adresses des pages, d'avant les sections
ci-dessus, fonctionnent toujours : chacun redirige vers la nouvelle page, avec
sa chaîne de requête.

## Les deux espaces de la console

La console a deux barres latérales ([RFC 0050](/rfc/0050-security-console-completion-and-admin-navigation)) :

- **`/admin`** — l'instance : registres, paquets, accès, **Sécurité de
  l'instance** et système. *Sécurité de l'instance* (`/admin/instance-security`,
  `system:read`) dit ce qui concerne ce serveur lui-même sur les dernières
  24 heures ou 7 jours : identifiants refusés et leurs adresses, adresses
  bannies, jetons utilisés depuis une nouvelle adresse, jetons statiques encore
  conservés en clair, exemptions qui prennent fin dans la semaine, et les
  avertissements de sécurité de la configuration. Adresses et identifiants
  d'utilisateur n'apparaissent qu'avec `audit:read`. Sa page *Comptes et
  adresses bloqués* est l'ancienne *Accès → Blocages*.
- **`/security`** — les paquets : une **vue d'ensemble** qui dit, domaine par
  domaine, si quelque chose demande de l'attention (retenues, signalements,
  analyseurs et worker, couverture), puis *Tri* (retenues, constats,
  signalements, exposition) et *Suivi* (tendances, couverture, analyseurs et
  worker). Elle s'ouvre sur la première page que vos verbes lisent ; voir
  [Contrôles de sécurité](/fr/guide/security-gates#console).

Le tableau de bord d'administration porte les mêmes lignes de sécurité, une par
domaine que vos verbes lisent. Chaque groupe de la barre latérale s'ouvre et se
ferme, et le navigateur retient ceux que vous avez ouverts ; le groupe de la
page courante est toujours ouvert.
