---
sourcePath: registries/npm.md
sourceHash: 5c56255ed77c3c15
---

# npm

Fait proxy et cache du registre npm, ou héberge des paquets npm privés.
BatleHub sert le packument complet et le téléchargement des tarballs, sous
contrôle du RBAC et du garde-fou d'âge de publication. `npm audit` fonctionne à
travers le proxy.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `npm` |
| **Amont par défaut** | `registry.npmjs.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `npm publish` |
| **Coupure réseau** | hors ligne, le packument est composé à partir des versions détenues ([RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap)) ; un `npm install` neuf s'y résout |

## Mise en place du proxy

Faites pointer npm vers votre registre. Remplacez `<registry>` par le nom de
registre que vous avez configuré, et définissez `BATLEHUB_TOKEN` si le registre
exige une authentification :

```ini
# .npmrc (racine du projet ou ~/.npmrc)
registry=https://batlehub.example.com/proxy/<registry>/
//batlehub.example.com/proxy/<registry>/:_authToken=${BATLEHUB_TOKEN}
```

Pour ne router qu'un scope précis par le proxy, écrivez plutôt
`@myorg:registry=https://batlehub.example.com/proxy/<registry>/`. **pnpm** lit
les mêmes clés `.npmrc`, sans changement.

**Yarn Berry** (Yarn 2+) ne lit pas `.npmrc` — configurez-le dans `.yarnrc.yml`,
avec ses propres noms de clés :

```yaml
# .yarnrc.yml
npmRegistryServer: "https://batlehub.example.com/proxy/<registry>/"
npmAuthToken: "${BATLEHUB_TOKEN}"

# Ou, pour ne router qu'un seul scope :
npmScopes:
  myorg:
    npmRegistryServer: "https://batlehub.example.com/proxy/<registry>/"
    npmAuthToken: "${BATLEHUB_TOKEN}"
```

## Publication (local / hybrid) {#publishing-local-hybrid}

### Configuration du serveur

```toml
[[registries]]
type = "npm"
name = "internal-npm"
mode = "local"          # ou "hybrid" pour se rabattre sur registry.npmjs.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://registry.npmjs.org"]` dans le bloc
du registre.

### Mise en place côté client

Créez ou modifiez `.npmrc` (par projet ou `~/.npmrc`) :

```ini
# Rattacher tous les paquets @myorg au registre privé
@myorg:registry=https://batlehub.example.com/proxy/internal-npm/

# Token d'authentification pour cet hôte de registre
//batlehub.example.com/proxy/internal-npm/:_authToken=<your-token>
```

Pour utiliser le registre pour tous les paquets (sans scope), définissez le
registre global :

```ini
registry=https://batlehub.example.com/proxy/internal-npm/
//batlehub.example.com/proxy/internal-npm/:_authToken=<your-token>
```

### Publier

```sh
npm publish --registry https://batlehub.example.com/proxy/internal-npm/
# ou, avec un .npmrc configuré :
npm publish
```

### Vérifier

```sh
npm view @myorg/my-package --registry https://batlehub.example.com/proxy/internal-npm/
npm install @myorg/my-package
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/npm -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `POST` | `/proxy/{registry}/-/npm/v1/audit/bulk` | Deprecated alias of the bulk audit endpoint — npm sends `/-/npm/v1/security/advisories/bulk`. |
| `POST` | `/proxy/{registry}/-/npm/v1/audit/quick` | Deprecated alias of the quick audit endpoint — npm sends `/-/npm/v1/security/audits/quick`. |
| `POST` | `/proxy/{registry}/-/npm/v1/security/advisories/bulk` | `npm audit`, bulk mode — the default since npm 7, on the path npm sends. |
| `POST` | `/proxy/{registry}/-/npm/v1/security/audits/quick` | `npm audit`, quick mode — on the path npm sends. |
| `GET` | `/proxy/{registry}/-/package/{package}/dist-tags` | `npm dist-tag ls`. |
| `PUT` | `/proxy/{registry}/-/package/{package}/dist-tags/{tag}` | `npm dist-tag add` — declined, with a reason the client prints. |
| `DELETE` | `/proxy/{registry}/-/package/{package}/dist-tags/{tag}` | `npm dist-tag rm`. Declined for the same reason as `add`. |
| `GET` | `/proxy/{registry}/-/ping` | `npm ping`. |
| `GET` | `/proxy/{registry}/-/v1/search` | `npm search` / `npm search --json`. |
| `GET` | `/proxy/{registry}/-/whoami` | `npm whoami`. |
| `PUT` | `/proxy/{registry}/{name}` | Publish a new npm package version (`npm publish`). |
| `GET` | `/proxy/{registry}/{package}` | Fetch package metadata (all versions / packument). |
| `GET` | `/proxy/{registry}/{package}/{version}` | Fetch package version metadata. |
| `GET` | `/proxy/{registry}/{package}/{version}/tarball` | Download npm package tarball for a specific version. |
<!-- END endpoints -->

Le packument est la réponse propre de BatleHub, pas une copie de celle de
l'amont. Deux choses sont réécrites avant qu'il n'atteigne le client :

- **`dist.tarball` pointe vers BatleHub**, de sorte que les téléchargements
  passent par le proxy — son cache, sa piste d'audit et ses garde-fous de
  politique — plutôt que directement vers le CDN amont.
- **Les versions bloquées sont retirées**, et `dist-tags.latest` est recalculé
  vers la version la plus récente encore autorisée. Voir
  [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version).

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

---

## Authentification

Passez un token BatleHub comme token d'authentification npm (`_authToken`).
L'accès anonyme ne fonctionne que si le RBAC du registre accorde la lecture au
rôle `anonymous`.

## Notes

`npm audit` fonctionne dès que le registre est configuré — les deux modes,
rapide et groupé, sont relayés vers la base d'alertes amont, sur les chemins que
la CLI npm envoie déjà :

```bash
npm audit
npm audit --fix
```

Les réponses sont mises en cache, et une base d'alertes injoignable est servie
depuis le cache plutôt qu'échouée : une panne en amont n'arrête donc pas un
pipeline qui lance `npm audit`. Voir
[le proxy de vulnérabilités](/fr/use/vulnerability-proxy#_2-npm-—-npm-audit)
pour les en-têtes de cache et les deux alias dépréciés.

## Recherche

``npm search`` est traité en trois étapes : un résultat en cache pour cette
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

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
