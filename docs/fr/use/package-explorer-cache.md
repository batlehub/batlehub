---
sourcePath: use/package-explorer-cache.md
sourceHash: fdc074c0b68f3811
---

# Explorateur de paquets — cache et API

## Le cache de l'explorateur {#cache}

Les résultats du catalogue sont servis depuis un **cache en mémoire**, pour ne
pas parcourir toutes les tables de paquets à chaque chargement de page. C'est
important pour les gros registres qui comptent des dizaines de milliers de
paquets.

### Comment il fonctionne {#cache-how}

| Propriété | Valeur |
| --- | --- |
| TTL | 10 minutes |
| Portée | Par requête (filtre de registre + recherche de nom + tri + page) |
| Invalidation | Expiration du TTL, purge par un admin, ou publication réussie |
| Service en cas d'échec | Oui — les entrées expirées sont conservées et servies si la base est injoignable |
| Persistance | En mémoire uniquement ; vidé au redémarrage du serveur |
| Multi-instance | Chaque instance a son propre cache ; il n'y a pas de diffusion à l'échelle du cluster — après un changement de données en masse, appelez l'endpoint de purge sur **chaque réplica** (voir [Déploiements multi-instances](#cache-ha)) |

### Servir du périmé quand la base est indisponible {#cache-stale}

Si la base devient injoignable pendant une requête, BatleHub regarde s'il existe
une entrée de cache périmée (expirée) pour cette requête exacte :

- **Une entrée périmée existe** → les résultats périmés sont renvoyés silencieusement. La réponse porte `"upstream_unavailable": false`, puisque des données sont disponibles.
- **Aucune entrée** → un résultat vide est renvoyé avec `"upstream_unavailable": true`. La console affiche alors un badge d'avertissement indiquant que les résultats peuvent être incomplets.

Autrement dit, l'explorateur reste utilisable pendant une panne de base tant que
les requêtes émises ont été mises en cache au moins une fois avant la panne.

> L'endpoint de recherche amont (`GET /api/v1/explore/upstream`) n'est **pas mis
> en cache** — il diffuse vers les registres amont en direct et renvoie toujours
> des résultats en temps réel.

### Invalidation automatique {#cache-auto-invalidate}

Le cache est invalidé automatiquement quand :

1. **Un paquet est publié** sur un registre local ou hybrid via `cargo publish`, `npm publish`, etc. Seules les entrées de ce registre-là sont vidées.
2. **Le TTL expire**, au bout de 10 minutes.

Il n'y a pas d'invalidation automatique lorsqu'un paquet passe par le proxy pour
la première fois. Ces entrées apparaissent dans l'explorateur au prochain
rafraîchissement du TTL, en général dans les 10 minutes.

### Invalidation manuelle {#cache-admin}

Les admins peuvent vider le cache depuis le panneau d'administration ou par
l'API.

#### Console d'administration

Allez dans **Admin → Cache d'exploration** (`/admin/explore-cache`). Deux actions
sont disponibles :

- **Invalider par registre** — choisissez un registre dans la liste déroulante et cliquez sur **Invalider le registre**. Seules les entrées de cache qui incluent ce registre sont vidées.
- **Tout invalider** — vide le cache entier. Tous les registres sont concernés.

Après invalidation, la requête suivante sur une entrée vidée réinterroge la base
et repeuple le cache de façon transparente.

#### API d'administration

```http
POST /api/v1/admin/explore/invalidate
Authorization: Bearer <admin-token>
Content-Type: application/json
```

**Corps de requête :**

| Champ | Type | Description |
| --- | --- | --- |
| `registry` | chaîne (facultatif) | Registre à vider. Omettez-le pour tout vider. |

**Vider un registre :**

```sh
curl -X POST https://batlehub.example.com/api/v1/admin/explore/invalidate \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm"}'
# {"ok": true}
```

**Vider tous les registres :**

```sh
curl -X POST https://batlehub.example.com/api/v1/admin/explore/invalidate \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}'
# {"ok": true}
```

**Réponses :**

| Statut | Description |
| --- | --- |
| 200 | `{"ok": true}` — cache vidé |
| 403 | Rôle admin requis |

### Déploiements multi-instances {#cache-ha}

Le cache de l'explorateur est **propre à chaque processus**. Dans un déploiement
à plusieurs réplicas (Kubernetes, Docker Swarm), chaque réplica a son cache
indépendant. Cela implique que :

- une purge par l'API n'affecte que le réplica qui a traité la requête ;
- les TTL des autres réplicas s'écoulent chacun de leur côté.

Après une opération de masse sur les données (migration de base, publication en
masse, restructuration de registre), appelez l'endpoint de purge sur **chaque
réplica**, ou attendez jusqu'à 10 minutes que l'expiration des TTL se propage
d'elle-même.

Voir [Haute disponibilité](/fr/guide/high-availability) pour les stratégies de
déploiement conscientes des réplicas.

---

## Notes de performance {#performance}

Les requêtes du catalogue exécutent deux CTE qui font l'union de
`package_statuses` et de `local_packages`, puis joignent les comptes d'événements
d'accès. Les index suivants (ajoutés par la migration 017) les gardent rapides :

| Index | Rôle |
| --- | --- |
| `idx_access_events_pkg` sur `(registry, package_name, package_version)` | Condition de JOIN dans la liste des paquets |
| `idx_access_events_pkg_allowed_recent` sur `(registry, package_name, package_version, outcome, created_at DESC)` | Sous-requête corrélée `last_accessed_by` |
| `idx_access_events_registry_name` sur `(registry, package_name)` | Comptage LATERAL des événements d'accès dans le catalogue |
| `idx_package_statuses_registry_name` sur `(registry, package_name)` | Agrégation GROUP BY de l'explorateur |

Ces index sont créés automatiquement au démarrage de BatleHub, avec les
migrations. Aucune action manuelle n'est nécessaire.

Pour les gros registres (plus de 50 000 paquets), le cache en mémoire de 10
minutes ramène la charge des requêtes répétées de l'explorateur à presque rien.
Si vous avez besoin d'un TTL plus court pour refléter les publications plus vite,
appelez l'endpoint de purge depuis votre pipeline de CI/CD (voir
[Invalidation automatique](#cache-auto-invalidate)).

---

## API REST {#api}

Les registres dont le RBAC accorde l'accès en lecture au rôle `anonymous` sont
interrogeables sans identifiants ; tous les autres exigent un token Bearer dont
l'identité résout au moins au rôle `user`. Dans les deux cas, seuls les registres
que l'appelant peut explorer figurent dans les réponses.

### Lister les paquets {#api-list}

```http
GET /api/v1/explore/packages
```

**Paramètres de requête :**

| Paramètre | Type | Défaut | Description |
| --- | --- | --- | --- |
| `registry` | chaîne | — | Filtrer sur un seul registre. |
| `name` | chaîne | — | Filtre par sous-chaîne sur le nom du paquet (insensible à la casse). |
| `sort` | `downloads` \| `name` \| `recent` | `downloads` | Ordre de tri. |
| `page` | entier | `0` | Numéro de page, à partir de zéro. |
| `per_page` | entier | `20` | Résultats par page. |

**Réponse :**

```json
{
  "items": [
    {
      "registry": "cargo",
      "name": "tokio",
      "version_count": 50,
      "total_downloads": 12500,
      "last_accessed": "2026-05-31T10:00:00Z",
      "source": "proxied",
      "has_blocked": false
    }
  ],
  "total": 150,
  "page": 0,
  "per_page": 20,
  "upstream_unavailable": false
}
```

`source` vaut `"proxied"`, `"local"` ou `"both"`.

`upstream_unavailable` ne vaut `true` que si la base était injoignable **et**
qu'aucune donnée en cache n'existait pour cette requête. Les résultats sont alors
vides. Voir [Le cache de l'explorateur](#cache).

### Statistiques par registre {#api-stats}

```http
GET /api/v1/explore/registries
```

Renvoie, pour chaque registre qui a déjà des paquets en cache, le nombre de
paquets et le total des événements de téléchargement. La console appelle cet
endpoint en même temps que `GET /api/v1/registries` (qui renvoie tous les
registres configurés) et fusionne les deux listes, de sorte que les registres
vides affichent un compte de `0`.

**Réponse :**

```json
{
  "registries": [
    { "registry": "cargo", "package_count": 120, "total_downloads": 45000 },
    { "registry": "npm",   "package_count":  30, "total_downloads":  8200 }
  ],
  "upstream_unavailable": false
}
```

### Détail d'un paquet {#api-detail}

```http
GET /api/v1/explore/packages/{registry}/{name}
```

Renvoie toutes les versions connues d'un paquet, l'état des garde-fous pour
l'appelant, et l'état du pare-feu version par version.

**Réponse :**

```json
{
  "registry": "cargo",
  "name": "tokio",
  "gate": {
    "beta_member": false
  },
  "versions": [
    {
      "version": "1.38.0",
      "source": "proxied",
      "firewall": { "status": "clear" },
      "download_count": 500,
      "last_accessed": "2026-05-31T10:00:00Z",
      "published_at": null,
      "is_prerelease": false
    },
    {
      "version": "0.9.0",
      "source": "proxied",
      "firewall": {
        "status": "blocked",
        "reason": "CVE-2021-12345",
        "blocked_by": "admin",
        "blocked_at": "2026-01-10T12:00:00Z"
      },
      "download_count": 80,
      "last_accessed": "2026-01-09T09:00:00Z",
      "published_at": null,
      "is_prerelease": false
    }
  ],
  "upstream_unavailable": false
}
```

`firewall.status` vaut `"clear"`, `"blocked"` ou `"yanked"`. Les entrées bloquées
portent en plus `reason`, `blocked_by` et `blocked_at`.

### Recherche en amont {#api-upstream}

```http
GET /api/v1/explore/upstream?name=<query>&registry=<optional>&limit=<n>
```

Interroge les API de recherche des registres amont pour les paquets qui
correspondent à `name`. Seuls les registres que l'appelant peut explorer sont
interrogés.

| Paramètre | Type | Défaut | Description |
| --- | --- | --- | --- |
| `name` | chaîne | (obligatoire) | La requête de recherche. |
| `registry` | chaîne | — | Se limiter à un seul registre. |
| `limit` | entier | `10` | Nombre maximum de résultats par registre. |

**Réponse :**

```json
{
  "items": [
    {
      "registry": "npm",
      "name": "lodash",
      "latest_version": "4.17.21",
      "description": "Lodash modular utilities.",
      "already_cached": false
    }
  ]
}
```

`already_cached: true` signifie que le paquet figure déjà dans le catalogue
principal (la console le retire alors de la section « Pas encore passé par le
proxy »).
