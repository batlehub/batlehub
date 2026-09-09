---
sourcePath: guide/admin-access.md
sourceHash: b4c121e82ebf3494
---

# Accès et audit

## Namespaces d'équipe et visibilité des paquets {#team-namespaces}

Les namespaces d'équipe permettent d'attribuer un préfixe de nom de paquet, à
l'intérieur d'un registre, à un groupe du fournisseur d'authentification. Seuls
les membres du groupe — et les admins — peuvent publier sous ce préfixe. La
visibilité d'un paquet gouverne séparément qui peut le télécharger.

Cette fonctionnalité n'exige aucune modification du TOML ni de redémarrage du
serveur — attributions et visibilité se gèrent entièrement par l'API
d'administration.

Pour la référence complète (niveaux de visibilité, application au téléchargement,
règle du préfixe le plus long, matrice de prise en charge par registre), voir le
[guide du contrôle d'accès](/fr/guide/access-control#team-namespaces).

### Gérer les attributions de namespace

```sh
# Lister les attributions d'un registre
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces

# Attribuer un préfixe à un groupe
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"prefix":"frontend","group_id":"oidc:frontend-team","claimed_by":"admin"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces

# Libérer une attribution (le préfixe peut contenir des barres obliques, passées telles quelles dans le chemin)
curl -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/frontend
```

### Gérer la visibilité d'un paquet

La visibilité porte sur le paquet — toutes les versions partagent le même
réglage. Valeurs acceptées : `public` (défaut), `internal`, `team`.

```sh
# Lire la visibilité courante
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# La restreindre aux membres de l'équipe
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"team"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility
```

Un nom de paquet contenant des barres obliques doit être encodé en
pourcent dans l'URL (`/` → `%2F`).

---

## Journal d'audit {#audit-log}

Toute décision de contrôle d'accès (autorisation ou refus) est enregistrée dans
PostgreSQL, et toute action d'administration également — blocages, changements de
propriété, suppressions, passes de rétention.

```sh
# Les 50 dernières décisions, tous registres confondus
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/audit-log?per_page=50"

# Filtrer par registre et par issue
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/audit-log?registry=npm&denied_only=true&per_page=100"
```

### Demander ce qui est arrivé à un paquet {#audit-actions}

Les téléchargements dépassent tout le reste de plusieurs ordres de grandeur : la
question « qu'est-ce qui a été supprimé ici » n'a donc de réponse qu'avec
`action`. Le paramètre prend une action ou un ensemble séparé par des virgules,
et un nom inconnu donne un `400` qui liste les possibilités — jamais une page
vide, qui se lirait comme « il ne s'est rien passé ».

```sh
# Toutes les suppressions d'un registre : à la main et par politique
batlehub admin audit-log --registry acme-npm --action delete,retention_reclaim

# Sortir du cache est une autre question — les octets reviennent à la requête suivante
batlehub admin audit-log --registry acme-npm --action cache_evict,cache_clear

# Toute l'histoire d'un paquet
batlehub admin audit-log --registry acme-npm --package internal-tool

# Le même ensemble, exporté pour un auditeur
batlehub admin export-audit-log --action delete,retention_reclaim --format csv
```

`--action` et `--package` sont aussi des paramètres de requête sur l'endpoint
(`?action=delete,retention_reclaim&package_name=internal-tool`), sur le listing
comme sur l'export.

| Action | Enregistrée quand |
| --- | --- |
| `delete` | une personne a supprimé une version |
| `retention_reclaim` | une politique de rétention en a repris une — [voir plus bas](/fr/guide/admin-policies#retention-trail) |
| `retention_run` / `retention_dry_run` | une passe de rétention s'est terminée, réelle ou en prévisualisation |
| `cache_evict` | un artefact du cache proxy a été retiré à la main — une copie, pas le paquet |
| `cache_clear` | le cache entier d'un registre a été vidé à la main |
| `cache_evict_run` / `cache_evict_dry_run` | une passe d'éviction s'est terminée, réelle ou en prévisualisation — [voir plus bas](/fr/guide/admin-policies#cache-eviction-trail) |
| `cache_coherence_run` / `cache_coherence_dry_run` | une passe a ramassé des blobs que rien ne référence — [voir plus bas](/fr/guide/admin-policies#cache-coherence) |
| `tombstone_compact` | le détail des pierres tombales périmées d'un registre a été retiré |
| `audit_purge` | cette piste elle-même a été purgée jusqu'à une date de coupure |
| `block` / `unblock`, `block_user` / `unblock_user`, `block_ip` / `unblock_ip` | l'action d'administration correspondante |
| `yank` / `unyank`, `deprecate` / `undeprecate`, `unlist` / `relist` | un changement de cycle de vie sur une version |
| `add_owner` / `remove_owner`, `set_visibility`, `claim_namespace` / `release_namespace` | propriété et visibilité |
| `download` / `view_metadata` | une lecture, autorisée ou refusée |

Le JSON de réponse les écrit sans les tirets bas (`retentionreclaim`) ; le filtre
accepte les deux orthographes, de sorte qu'une action collée depuis n'importe
quelle réponse fonctionne.

Exemple d'entrée :

```json
{
  "id": "01j...",
  "timestamp": "2025-05-22T10:00:00Z",
  "registry": "npm",
  "package": "lodash",
  "version": "4.17.21",
  "user_id": "ci",
  "role": "user",
  "outcome": "allow",
  "rule": null
}
```

---

## Canal bêta / de pre-release {#beta-channel}

Réservez les versions de pre-release (par exemple `1.0.0-beta.1`) à des
utilisateurs ou des groupes précis. Les autres ne voient que les versions
stables, et reçoivent un 404 au téléchargement d'un artefact de pre-release.

À activer registre par registre :

```toml
[registries.beta_channel]
enabled = true
```

La gestion des membres se fait à l'exécution :

```sh
# Ajouter un utilisateur
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"user","principal_id":"alice"}' \
  http://localhost:8080/api/v1/admin/registries/my-npm/beta-channel

# Lister les membres
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/registries/my-npm/beta-channel

# Retirer un membre
curl -X DELETE -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/registries/my-npm/beta-channel/user/alice
```

Voir le [guide du contrôle d'accès](/fr/guide/access-control#beta-channel) pour
la référence complète : appartenance par groupe, table de prise en charge par
registre, et comportement vu de l'utilisateur.

---

## Blocage par IP {#ip-blocking}

Bloque automatiquement les IP qui déclenchent trop de violations (limites de
débit atteintes, échecs d'authentification) dans une fenêtre de temps.

```toml
[ip_blocking]
enabled               = true
violation_threshold   = 10
violation_window_secs = 300      # fenêtre de 5 minutes
ban_duration_secs     = 3600     # blocage d'une heure
trigger_on_status     = [429, 401]
```

La gestion manuelle des blocages :

```sh
# Lister les IP bloquées
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/ip-blocks

# Bloquer une IP
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"ip":"1.2.3.4","reason":"bad actor","duration_secs":86400}' \
  http://localhost:8080/api/v1/admin/ip-blocks

# Débloquer
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/ip-blocks/1.2.3.4
```

Une IP bloquée reçoit `403 Forbidden` avec `X-Block-Expires`. Le contrôle a lieu
avant l'authentification. Les compteurs de violations et les blocages sont
stockés dans le même backend que les compteurs de limitation de débit
(`memory` / `postgres` / `redis`).

Voir le [guide du contrôle d'accès](/fr/guide/access-control#ip-blocking) pour la
référence complète, y compris la mise en place derrière un répartiteur de charge
et la comparaison des backends de stockage.
