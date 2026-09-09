---
sourcePath: registries/jetbrains-marketplace.md
sourceHash: 2ca5c0ff596e6156
---

# Place de marché JetBrains

Fait proxy et cache de l'écosystème de plugins JetBrains
([plugins.jetbrains.com](https://plugins.jetbrains.com)) — recherche depuis
l'IDE, mises à jour compatibles, blobs `meta.json` et téléchargements de
plugins — ou héberge des plugins privés. Tout est mis en cache avec repli sur
une réponse périmée : un plugin vu une fois continue donc de se résoudre même si
l'amont est injoignable. À distinguer du type
[`jetbrains`](/fr/registries/jetbrains), adressé par chemin, qui sert les
archives d'IDE.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `jetbrains-marketplace` |
| **Amont par défaut** | `plugins.jetbrains.com` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ envoi compatible place de marché |
| **Coupure réseau** | pas de listing composé hors ligne : une galerie répond à des requêtes |

## Mise en place du proxy

Pointez un IDE vers le proxy. Remplacez `<registry>` par le nom de registre que
vous avez configuré.

**En complément** — garder la place de marché publique et ajouter les plugins
locaux de ce registre. Paramètres → Plugins → ⚙ → **Manage Plugin
Repositories…** → ajoutez :

```text
https://batlehub.example.com/proxy/<registry>/updatePlugins.xml
```

(`updatePlugins.xml` liste les plugins publiés localement ; il renvoie 404 sur un
registre en mode proxy pur.)

**En remplacement complet** — l'IDE ne parle qu'à BatleHub (recherche, mises à
jour, téléchargements). Aide → **Edit Custom Properties…**, puis ajoutez :

```properties
idea.plugins.host=https://batlehub.example.com/proxy/<registry>
```

Téléchargez un plugin directement :

```bash
curl -fL -o rust-plugin.zip \
  "https://batlehub.example.com/proxy/<registry>/plugin/download?pluginId=org.rust.lang&version=241.25026.107"

# Ou laissez le proxy choisir la version la plus récente compatible avec votre build d'IDE :
curl -fL -o plugin.zip \
  "https://batlehub.example.com/proxy/<registry>/pluginManager?action=download&id=org.rust.lang&build=IU-241.14494"
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Un registre `jetbrains-marketplace` en mode `local` ou `hybrid` accepte le même
envoi multipart que plugins.jetbrains.com : un simple `curl` et l'outillage de
publication de JetBrains fonctionnent donc tous les deux.

### Configuration du serveur

```toml
[[registries]]
type = "jetbrains-marketplace"
name = "internal-plugins"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["releases:read"]
admin     = ["*"]
```

### Envoi (curl)

L'identifiant et la version du plugin sont lus dans le `META-INF/plugin.xml` de
l'archive (`.jar`, ou distribution `.zip` avec `lib/*.jar`). Un champ de
formulaire `xmlId`, quand il est présent, doit correspondre au descripteur.

```sh
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -F "xmlId=com.example.myplugin" \
  -F "channel=" \
  -F "file=@my-plugin-1.0.0.zip" \
  "https://batlehub.example.com/proxy/internal-plugins/api/updates/upload"
# → 201 {"id":"com.example.myplugin","pluginId":"com.example.myplugin","version":"1.0.0","channel":""}
```

Passez `-F "isHidden=true"` pour publier une version masquée des listings (elle
reste téléchargeable par coordonnée exacte).

### Envoi (Gradle / plugin-repository-rest-client)

Faites pointer l'hôte de l'outillage vers le proxy et utilisez votre token
BatleHub :

```kotlin
// build.gradle.kts (plugin org.jetbrains.intellij / intellij-platform)
tasks.publishPlugin {
    host.set("https://batlehub.example.com/proxy/internal-plugins")
    token.set(providers.environmentVariable("BATLEHUB_TOKEN"))
}
```

### Installer depuis l'IDE

Paramètres → Plugins → ⚙ → **Manage Plugin Repositories…** → ajoutez
`https://batlehub.example.com/proxy/internal-plugins/updatePlugins.xml`. Pour un
remplacement complet de la place de marché, mettez plutôt
`idea.plugins.host=https://batlehub.example.com/proxy/internal-plugins` dans
Aide → Edit Custom Properties….

### Vérifier

```sh
# Le XML du dépôt personnalisé liste le plugin publié
curl -s -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-plugins/updatePlugins.xml"

# Le retélécharger
curl -s -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-plugins/plugin/download?pluginId=com.example.myplugin&version=1.0.0" \
  -o roundtrip.zip
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/jetbrains-marketplace -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/api/plugins/{id}` | `/api/plugins/{id}` — plugin object (id = xmlId). |
| `GET` | `/proxy/{registry}/api/plugins/{id}/updates` | `/api/plugins/{id}/updates` — every version, newest first. |
| `GET` | `/proxy/{registry}/api/products/intellij/plugins/{id}/comments` | `/api/products/intellij/plugins/{id}/comments` — plugin comments. |
| `GET` | `/proxy/{registry}/api/search/aggregation/{field}` | `/api/search/aggregation/{field}` — facet values for the marketplace UI. |
| `GET` | `/proxy/{registry}/api/search/plugins` | `/api/search/plugins?search=&build=` — array shape used by the IDE's |
| `POST` | `/proxy/{registry}/api/search/updates/compatible` | `POST /api/search/updates/compatible` — newest compatible update per |
| `GET` | `/proxy/{registry}/api/searchPlugins` | `/api/searchPlugins?search=&max=` — `{plugins, total}` shape. |
| `POST` | `/proxy/{registry}/api/updates/upload` |  |
| `GET` | `/proxy/{registry}/feature/getImplementations` | `/feature/getImplementations` — feature-implementation lookup. |
| `GET` | `/proxy/{registry}/files/{plugin}/{update}/{file_name}` | `files/{plugin}/{update}/{fileName}` — the IDE `/files/` artifact |
| `GET` | `/proxy/{registry}/files/{plugin}/{update}/meta.json` | `files/{plugin}/{update}/meta.json` — update-level metadata blob. |
| `GET` | `/proxy/{registry}/files/{plugin}/meta.json` | `files/{plugin}/meta.json` — plugin-level metadata blob. |
| `GET` | `/proxy/{registry}/files/brokenPlugins.json` | `files/brokenPlugins.json` — the IDE's known-broken plugin list. |
| `GET` | `/proxy/{registry}/files/IDE/extensions.json` | `files/IDE/extensions.json` — IDE extension descriptors. |
| `GET` | `/proxy/{registry}/files/jbPluginsXMLIds.json` | `files/jbPluginsXMLIds.json` — JetBrains-authored plugin ids. |
| `GET` | `/proxy/{registry}/files/pluginsXMLIds.json` | `files/pluginsXMLIds.json` — every known plugin xmlId. |
| `GET` | `/proxy/{registry}/plugin/download` | `plugin/download?pluginId=&version=[&channel=]` — the canonical download |
| `GET` | `/proxy/{registry}/pluginManager` | `pluginManager?action=download&id=&build=` — resolve the newest |
| `GET` | `/proxy/{registry}/plugins/list` | `/plugins/list?pluginId=` — classic plugin-repository XML: every version of |
| `GET` | `/proxy/{registry}/updatePlugins.xml` | `updatePlugins.xml` — the custom-plugin-repository document the IDE polls |
<!-- END endpoints -->

---

## Versions bloquées

Les trois listings de plugins masquent un build bloqué : le document de dépôt
personnalisé `updatePlugins.xml` qu'un IDE interroge, le classique
`/plugins/list`, et `/api/plugins/{id}/updates`. Ils sont rendus à partir d'une
seule liste de versions, et le filtre se pose sur cette liste — de sorte qu'un
IDE ne propose jamais un build bloqué comme mise à jour disponible pour échouer
ensuite à l'installer.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Passez un token BatleHub dans un en-tête Bearer sur les requêtes d'envoi.
L'accès en lecture est gouverné par le RBAC du registre — l'accès anonyme ne
fonctionne que si le rôle `anonymous` reçoit la lecture.

## Notes

- Les métadonnées, les artefacts de plugins et les blobs JSON fixes que l'IDE
  récupère au démarrage sont tous mis en cache avec repli sur une réponse
  périmée.
- Ne pointez **pas** le type [`jetbrains`](/fr/registries/jetbrains), adressé par
  chemin et destiné aux archives d'IDE, vers plugins.jetbrains.com — c'est ce
  type-ci qui sert l'écosystème de plugins.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
