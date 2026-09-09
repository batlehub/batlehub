---
sourcePath: registries/nuget.md
sourceHash: 42383ab70e0689df
---

# NuGet (.NET)

Fait proxy et cache de la galerie NuGet pour `dotnet`, ou héberge des paquets
privés. BatleHub synthétise l'index de services v3 (`index.json`) de sorte que
toutes les URL de ressources pointent vers le proxy, sous contrôle du RBAC et du
garde-fou d'âge de publication.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `nuget` |
| **Amont par défaut** | `api.nuget.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `dotnet nuget push` |
| **Coupure réseau** | hors ligne, l'index plat est composé à partir des paquets détenus et la page d'enregistrement depuis le `.nuspec` de chaque paquet lu à l'import ; `dotnet restore` lit l'index plat |

## Mise en place du proxy

Ajoutez la source avec la CLI. Remplacez `<registry>` par le nom de registre que
vous avez configuré :

```sh
dotnet nuget add source \
  https://batlehub.example.com/proxy/<registry>/nuget/v3/index.json \
  --name batlehub \
  --username __token__ \
  --password $BATLEHUB_TOKEN
```

Puis installez comme d'habitude :

```sh
dotnet add package Newtonsoft.Json
dotnet restore
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Les paquets NuGet sont des fichiers `.nupkg` (archives ZIP contenant un manifeste
`.nuspec`). BatleHub implémente le
[protocole NuGet v3](https://learn.microsoft.com/en-us/nuget/api/overview),
compatible avec la CLI `dotnet`, `nuget.exe` et tout client NuGet v3.

### Configuration

```toml
[[registries]]
type = "nuget"
name = "internal-nuget"
mode = "local"          # ou "hybrid" pour se rabattre sur api.nuget.org

[registries.rbac]
user  = ["releases:read"]
admin = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://api.nuget.org"]`.

### Configurer dotnet / nuget.config

**En CLI (une fois pour toutes) :**
```bash
dotnet nuget add source \
  https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json \
  --name internal-nuget \
  --username __token__ --password <api-token>
```

**`nuget.config` (au niveau du projet) :**
```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="internal-nuget"
         value="https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json" />
  </packageSources>
  <packageSourceCredentials>
    <internal-nuget>
      <add key="Username" value="__token__" />
      <add key="ClearTextPassword" value="<api-token>" />
    </internal-nuget>
  </packageSourceCredentials>
</configuration>
```

### Publier avec dotnet nuget push

Empaquetez d'abord votre projet, puis poussez :

```bash
dotnet pack MyLib.csproj -c Release

dotnet nuget push bin/Release/MyLib.1.0.0.nupkg \
  --api-key <api-token> \
  --source https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json
```

L'endpoint de publication accepte le `multipart/form-data` qu'envoie
`dotnet nuget push`. En cas de succès, il renvoie **201 Created**.

### Retirer une version

```bash
curl -X DELETE \
  -H "Authorization: Bearer <api-token>" \
  "https://batlehub.example.com/proxy/internal-nuget/nuget/v2/package/mylib/1.0.0"
```

### Consommer un paquet

```bash
# Ajouter le paquet — dotnet récupère l'index, résout la version, télécharge le .nupkg
dotnet add package MyLib --version 1.0.0 --source internal-nuget

# Restaurer toutes les dépendances du projet
dotnet restore
```

### Vérifier

```bash
# L'index de services doit renvoyer un JSON avec "version": "3.0.0"
curl -s https://batlehub.example.com/proxy/internal-nuget/nuget/v3/index.json | jq '.version'

# La liste de versions du conteneur plat après publication
curl -s https://batlehub.example.com/proxy/internal-nuget/nuget/v3/flat/mylib/index.json
# → {"versions":["1.0.0"]}
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/nuget -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `PUT` | `/proxy/{registry}/nuget/api/v2/package` | Publish a `.nupkg` to the local registry. |
| `PUT` | `/proxy/{registry}/nuget/api/v2/symbolpackage` | Publish a `.snupkg` symbol package. |
| `DELETE` | `/proxy/{registry}/nuget/v2/package/{id}/{version}` | Yank (unlist) a NuGet package version from the local registry. |
| `GET` | `/proxy/{registry}/nuget/v3/autocomplete` | `SearchAutocompleteService` — package-id completion. |
| `GET` | `/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}` | Download a NuGet package artifact (`.nupkg`, `.nuspec`, checksum, etc.). |
| `GET` | `/proxy/{registry}/nuget/v3/flat/{id}/index.json` | Return the list of available versions for a NuGet package (flat container). |
| `GET` | `/proxy/{registry}/nuget/v3/index.json` | Return a NuGet v3 service index pointing all resource URLs back to this proxy. |
| `GET` | `/proxy/{registry}/nuget/v3/query` | Search for NuGet packages. |
| `GET` | `/proxy/{registry}/nuget/v3/registration5/{id}/index.json` | Return NuGet v3 registration metadata for a package. |
| `GET` | `/proxy/{registry}/nuget/v3/vulnerabilities/index.json` | Proxy the NuGet vulnerability database index. |
| `GET` | `/proxy/{registry}/nuget/v3/vulnerabilities/page/{page}` | Proxy a single page of NuGet vulnerability records. |
<!-- END endpoints -->

---

## Versions bloquées

Les deux documents de listing de NuGet masquent une version bloquée par
l'administration.

- L'**index plat** (`/v3/flat/{id}/index.json`) — ce contre quoi
  `dotnet restore` résout un intervalle de versions — retire purement et
  simplement la version.
- Les **pages d'enregistrement** retirent la feuille et recalculent les `count`,
  `lower` et `upper` de chaque page ; une page laissée vide est supprimée. Les
  enregistrements dont les pages sont servies par URL plutôt qu'en ligne passent
  sans filtrage et sont journalisés.

Les orthographes de version sont normalisées avant comparaison : un blocage
enregistré comme `1.0.0.0` masque donc un listing qui écrit la même publication
`1.0.0`.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Passez le token BatleHub comme mot de passe de la source (nom d'utilisateur
`__token__`), ou en `--api-key` à la publication. L'en-tête `X-NuGet-ApiKey` est
normalisé en `Authorization: Bearer` en interne, donc
`--api-key $BATLEHUB_TOKEN` est accepté comme token Bearer.

## Notes

- `dotnet list package --vulnerable` fonctionne automatiquement — BatleHub
  annonce une ressource `VulnerabilitiesUrl` dans l'index de services v3 et fait
  proxy du catalogue de vulnérabilités amont. Voir
  [Utiliser BatleHub → audit de sécurité](/fr/use/#security-audit).
- Un `401` à la publication signifie en général que le token n'a pas
  `releases:publish` (ni admin) sur le registre.

## Recherche

``dotnet package search`` est traité en trois étapes : un résultat en cache pour
cette requête, puis l'amont, puis — quand l'amont est injoignable — **les paquets
que ce registre détient déjà**. Une panne dégrade la recherche vers ce dont
BatleHub peut honnêtement répondre, plutôt que vers une erreur ou une liste vide.

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

NuGet refuse purement et simplement une source de paquets en `http:`. Si BatleHub
n'est pas derrière TLS — instance locale, ou réseau interne — l'entrée de source
a besoin d'`allowInsecureConnections` :

```xml
<add key="batlehub" value="http://localhost:8080/proxy/my-nuget/nuget/v3/index.json"
     allowInsecureConnections="true" />
```

Mesuré avec dotnet 10.0.400. Sans cela, la CLI s'arrête avant d'émettre la
moindre requête, avec un message qui renvoie vers
<https://aka.ms/nuget-https-everywhere>.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
