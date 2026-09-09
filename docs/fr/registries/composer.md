---
sourcePath: registries/composer.md
sourceHash: 31011f6dae4929be
---

# Composer (PHP)

Fait proxy et cache de Packagist pour Composer, ou héberge des paquets privés.
BatleHub implémente le protocole Packagist v2 (`packages.json` et les endpoints
de métadonnées `p2/`), de sorte que Composer le traite comme un dépôt Composer
natif — sous contrôle du RBAC et du garde-fou d'âge de publication.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `composer` |
| **Amont par défaut** | `repo.packagist.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `curl -X POST …/api/upload` |
| **Coupure réseau** | hors ligne, `p2` est composé à partir des dists détenues, chaque entrée depuis son `composer.json` lu à l'import ; la variante `~dev` répond vide |

## Mise en place du proxy

Ajoutez une entrée de dépôt à `composer.json`. Remplacez `<registry>` par le nom
de registre que vous avez configuré :

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/<registry>/"
    }
  ]
}
```

Installez comme d'habitude :

```sh
composer install
composer require symfony/console
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Les paquets Composer s'envoient sous forme d'archives ZIP contenant un
`composer.json`. BatleHub y lit `name` (au format `vendor/package`) et `version`
au moment de l'envoi : aucune étape de métadonnées séparée n'est nécessaire.

### Configuration du serveur

```toml
[[registries]]
type = "composer"
name = "internal-composer"
mode = "local"          # ou "hybrid" pour se rabattre sur repo.packagist.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://repo.packagist.org"]`.

### Format du paquet

Un paquet Composer est une archive ZIP avec un `composer.json` à sa racine (ou
dans un unique sous-répertoire de premier niveau — l'usage courant quand on
archive une copie de travail git). Ce `composer.json` doit comporter `name` et
`version` :

```json
{
  "name": "my-vendor/my-package",
  "version": "1.0.0",
  "description": "My private library",
  "autoload": {
    "psr-4": { "MyVendor\\MyPackage\\": "src/" }
  }
}
```

Construisez l'archive depuis le répertoire de votre projet :

```sh
# Archiver depuis le répertoire courant (les fichiers de premier niveau directement dans le ZIP)
zip -r my-vendor-my-package-1.0.0.zip . -x "*.git*" -x "vendor/*"

# Ou utiliser git archive pour un export propre
git archive --format=zip HEAD -o my-vendor-my-package-1.0.0.zip
```

Si votre `composer.json` n'a pas de champ `version` (fréquent dans un projet
versionné), passez-la en paramètre de requête au moment de l'envoi.

### Envoyer

```sh
# composer.json contient un champ "version"
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @my-vendor-my-package-1.0.0.zip \
  "https://batlehub.example.com/proxy/internal-composer/api/upload"

# Remplacer (ou fournir) la version par paramètre de requête
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @my-vendor-my-package.zip \
  "https://batlehub.example.com/proxy/internal-composer/api/upload?version=1.0.0"
```

L'envoi enregistre le **SHA-1** de l'archive à côté d'elle et le publie comme
`dist.shasum`, parce que c'est l'empreinte que Composer recalcule sur le fichier
qu'il télécharge. Un paquet envoyé avant que cette empreinte ne soit enregistrée
n'a pas de SHA-1 stocké et est servi sans `shasum` du tout — Composer l'installe
sans vérifier l'empreinte plutôt que de le refuser. Renvoyez-le (même version,
après un retrait, ou en nouvelle version) pour récupérer la somme de contrôle.

### Mise en place côté client

Composer accepte deux façons de fournir des identifiants. Préférez `auth.json`
aux en-têtes en ligne, pour que les identifiants restent hors du dépôt de code.

**`auth.json`** (à placer à la racine du projet, ou dans
`~/.composer/auth.json` pour un usage global) :

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "token",
      "password": "<your-token>"
    }
  }
}
```

Composer l'envoie sous la forme
`Authorization: Basic base64("token:<your-token>")`. BatleHub en extrait le champ
mot de passe et le compare au token que vous avez configuré.

**En-tête en ligne dans `composer.json`** (à défaut d'`auth.json`) :

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/internal-composer/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer <your-token>"]
        }
      }
    }
  ]
}
```

### Installer

Une fois les identifiants configurés, ajoutez le dépôt à `composer.json` et
exigez le paquet :

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/internal-composer/"
    }
  ],
  "require": {
    "my-vendor/my-package": "^1.0"
  }
}
```

```sh
composer install
# ou
composer require my-vendor/my-package
```

### Retirer une version

```sh
curl -X DELETE \
  -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-composer/api/packages/my-vendor/my-package/versions/1.0.0"
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/composer -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `DELETE` | `/proxy/{registry}/api/packages/{vendor}/{package}/versions/{version}` | Yank a Composer package version (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/api/security-advisories/` | Proxy Composer security advisory queries to the upstream Packagist server. |
| `POST` | `/proxy/{registry}/api/upload` | Upload a Composer package ZIP (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/dist/{vendor}/{package}/{version}` | Download a Composer package ZIP artifact. |
| `GET` | `/proxy/{registry}/list.json` | `composer` bulk package enumeration — `list.json`. |
| `GET` | `/proxy/{registry}/p2/{path}` | Packagist v2 package metadata (all versions). |
| `GET` | `/proxy/{registry}/packages.json` | Composer registry root index. |
| `GET` | `/proxy/{registry}/search.json` | `composer search`. |
<!-- END endpoints -->

---

## Versions bloquées

Les métadonnées p2 retirent la version bloquée, et `dist.url` est redirigée vers
BatleHub de sorte que les téléchargements passent par le proxy plutôt que
directement par le CDN amont.

Packagist sert `"minified": "composer/2.0"`, où chaque entrée omet toute clé
identique à celle de l'entrée précédente. Supprimer une entrée au milieu d'une
telle liste change silencieusement ce dont héritent les entrées *suivantes* — un
document bien formé qui décrit les mauvais paquets. BatleHub développe la liste,
retire la version, puis re-minifie : une entrée située après celle qui a disparu
signifie donc toujours ce qu'elle signifiait.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Stockez les identifiants HTTP Basic dans `auth.json` (à la racine du projet ou
dans `~/.config/composer/` — ne le versionnez jamais) :

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "user",
      "password": "<your-token>"
    }
  }
}
```

Quand `auth.json` est présent, aucun en-tête `Authorization` n'est nécessaire
dans `composer.json`.

## Notes

- `composer audit` fonctionne automatiquement — BatleHub fait proxy de l'API
  d'alertes de sécurité de Packagist (`/api/security-advisories/`) de façon
  transparente. Voir
  [Utiliser BatleHub → audit de sécurité](/fr/use/#security-audit).
- Les versions retirées sont masquées des listes de versions et renvoient `404`
  au téléchargement.

## Recherche

``composer search`` est traité en trois étapes : un résultat en cache pour cette
requête, puis l'amont, puis — quand l'amont est injoignable — **les paquets que
ce registre détient déjà**. Une panne dégrade la recherche vers ce dont BatleHub
peut honnêtement répondre, plutôt que vers une erreur ou une liste vide.

Chaque réponse porte `X-BatleHub-Cache: hit | miss | stale`. `stale` signifie que
l'amont n'a pas pu être joint et que la réponse vient du cache ou de l'ensemble
détenu : une liste courte n'est donc jamais présentée silencieusement comme
complète.

::: warning Les requêtes de recherche atteignent l'amont
La deuxième étape transmet la chaîne de requête à l'amont configuré. Les termes
de recherche sont un relevé de ce que votre organisation cherche. Mettez
`serve_stale = false` et laissez le registre sans amont si vous voulez la réponse
des paquets détenus et aucune sortie réseau.
:::

Les versions bloquées sont retirées des résultats, et le total annoncé est ajusté
en conséquence — les clients paginent par décalage, donc une page silencieusement
raccourcie ferait sauter un résultat à la suivante.

## Une instance en HTTP simple exige un accord explicite

Composer refuse par défaut un dépôt en `http:`. Si BatleHub n'est pas derrière
TLS, le projet a besoin de :

```json
{ "config": { "secure-http": false } }
```

Mesuré avec Composer 2.10.2. Sans cela, Composer s'arrête avant d'émettre la
moindre requête, en renvoyant vers
<https://getcomposer.org/doc/06-config.md#secure-http>.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
