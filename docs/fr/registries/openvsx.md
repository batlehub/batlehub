---
sourcePath: registries/openvsx.md
sourceHash: dd06bd64d755398c
---

# OpenVSX

Fait proxy et cache des extensions VS Code depuis
[open-vsx.org](https://open-vsx.org), ou en héberge des privées. BatleHub sert
le **protocole de galerie VS Code** et l'**API REST OpenVSX**, de sorte qu'un
éditeur peut le prendre pour place de marché d'extensions et qu'`ovsx` peut
l'interroger. Les identifiants d'extension suivent la convention
`{publisher}.{name}`.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `openvsx` |
| **Amont par défaut** | `open-vsx.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ envoi de VSIX (`PUT …/vsix`) |
| **Coupure réseau** | pas de listing composé hors ligne : une galerie répond à des requêtes |
| **Signatures** | le registre signe ce qu'il héberge (`[registries.vsx_signing]`), relaie celle de l'amont pour ce dont il fait proxy, et conserve celle attachée à une version republiée |

## Mise en place du proxy

### Utiliser BatleHub comme galerie d'extensions {#use-batlehub-as-your-extension-gallery}

Pointez l'éditeur vers ce registre en ajoutant ceci à `product.json` (VSCodium et
Code - OSS lisent `~/.config/VSCodium/product.json` ; pour VS Code lui-même,
modifiez le `product.json` de l'installation) :

```jsonc
{
  "extensionsGallery": {
    "serviceUrl": "https://batlehub.example.com/proxy/<registry>/vscode/gallery",
    "itemUrl": "https://batlehub.example.com/proxy/<registry>/vscode/item",
    "resourceUrlTemplate": "https://batlehub.example.com/proxy/<registry>/vscode/unpkg/{publisher}/{name}/{version}/{path}"
  }
}
```

Redémarrez l'éditeur ; la recherche, l'installation et les mises à jour passent
alors par BatleHub, et chaque VSIX récupéré est mis en cache, audité et soumis
aux règles de politique du registre.

::: warning L'éditeur ne peut pas s'authentifier
VS Code n'envoie **aucun en-tête `Authorization`** à sa galerie, et
`product.json` n'a nulle part où mettre un token. Un registre employé comme
galerie a donc besoin de

```toml
[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

ou d'un ingress qui authentifie devant BatleHub. Un registre de galerie qui
exige un token bearer répond à toutes les requêtes par une liste vide, et
l'éditeur signale qu'aucune extension n'a été trouvée — ce qui ressemble à un
proxy cassé plutôt qu'à un choix de configuration.

**Sauf si vous compilez l'éditeur vous-même.** Une version que vous compilez peut
porter un petit correctif qui lit un identifiant et l'attache ; ce dépôt fournit
le module et les étapes d'intégration dans
[`patches/che-code/`](https://batleforc.git.batleforc.fr/batlehub/tree/main/patches/che-code).
Il lit le fichier même qu'écrit `batlehub-cli auth write-token-file` ; voir
[le fichier de contrat d'identifiants](#the-credential-contract-file) plus bas.
:::

### L'utiliser avec `ovsx`

L'API REST OpenVSX est servie elle aussi : la CLI fonctionne donc contre ce
registre.

```sh
export OVSX_REGISTRY_URL="https://batlehub.example.com/proxy/<registry>"
ovsx get acme.tool
```

### Télécharger un VSIX directement

Téléchargez un VSIX par sa coordonnée, puis installez-le. Remplacez `<registry>`
par le nom de registre que vous avez configuré :

```sh
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/ms-python.python/2024.2.1/vsix" \
  -o ms-python.python-2024.2.1.vsix

code --install-extension ms-python.python-2024.2.1.vsix
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Les deux types de registre (`openvsx` et `vscode-marketplace`) emploient le même
endpoint d'envoi. Il n'y a pas d'outil CLI dédié — les extensions se publient par
une simple requête `PUT` portant les octets bruts du VSIX.

### Configuration du serveur

```toml
[[registries]]
type = "openvsx"        # ou "vscode-marketplace"
name = "internal-ext"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

### Convention d'identifiant d'extension

Les identifiants d'extension suivent le format `{publisher}.{name}` employé par
la place de marché VS Code, par exemple `my-org.my-extension`.

### Envoyer

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-org.my-extension-1.0.0.vsix \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-extension/1.0.0/vsix"
```

Le serveur lit l'éditeur et le nom de l'extension dans le chemin de l'URL. Le
segment `{extension_id}` est l'identifiant complet `{publisher}.{name}`.

Un paquet construit avec `vsce package --pre-release` (ou publié avec
`ovsx publish --pre-release`) conserve son marqueur de pre-release : BatleHub le
lit dans l'`extension.vsixmanifest` du VSIX lui-même, le signale à la galerie
comme `Microsoft.VisualStudio.Code.PreRelease` et à l'API OpenVSX comme
`preRelease`, et un éditeur ne propose alors cette version qu'aux utilisateurs
qui ont accepté les pre-releases.

### Télécharger et installer

```sh
# Télécharger le VSIX
curl -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-extension/1.0.0/vsix" \
  -o my-org.my-extension-1.0.0.vsix

# Installer dans VS Code
code --install-extension my-org.my-extension-1.0.0.vsix
```

### Vérifier

```sh
# Contrôler les octets magiques du ZIP (PK\x03\x04) pour valider que l'envoi a été accepté
curl -s -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-extension/1.0.0/vsix" \
  | xxd | head -1
# Doit afficher : 50 4b 03 04 ...
```

### Signatures {#signatures}

La vue Extensions d'un éditeur récent n'installe que des entrées qui portent un
asset de signature. Donnez une clé au registre
([`[registries.vsx_signing]`](/fr/guide/configuration#vsx-signing)) et chaque
version qu'il héberge en reçoit un, au format d'Open VSX ; ce dont il fait proxy
depuis un amont qui signe est relayé avec la signature de cet amont. La clé
publique est sur `GET /proxy/{registry}/api/-/public-key/{key_id}` (en PEM), et
le document Open VSX de chaque version la nomme sous `files.publicKey`.

```sh
batlehub-cli vsx keygen            # une graine pour la configuration, et son identifiant de clé
batlehub-cli vsx verify my-org.my-extension-1.0.0.vsix \
  --registry https://batlehub.example.com/proxy/internal-ext \
  --id my-org.my-extension --version 1.0.0
# ok: … is signed by key 3f1e… (24503 bytes, manifest matches)
```

**Une extension qui porte déjà une signature la conserve.** Ce dont le registre
fait proxy est relayé avec l'archive de l'amont, jamais re-signé. Ce qui est
republié ici depuis la place de marché — un VSIX téléchargé sur
`marketplace.visualstudio.com` et envoyé à un registre local — ne perd rien non
plus : attachez l'archive de signature de la place de marché après le paquet, et
le registre la sert telle quelle plutôt que de signer par-dessus. C'est cette
archive-là qu'un VS Code d'origine vérifie, de sorte qu'une telle version
s'installe partout sans rien désactiver.

```sh
# le VSIX, puis l'archive servie par la place de marché comme Microsoft.VisualStudio.Services.VsixSignature
curl -X PUT -H "Authorization: Bearer <token>" -H "Content-Type: application/octet-stream" \
  --data-binary @ms-vscode.hexeditor-1.11.1.vsix \
  "https://batlehub.example.com/proxy/internal-ext/ms-vscode.hexeditor/1.11.1/vsix"
curl -X PUT -H "Authorization: Bearer <token>" -H "Content-Type: application/zip" \
  --data-binary @ms-vscode.hexeditor-1.11.1.sigzip \
  "https://batlehub.example.com/proxy/internal-ext/ms-vscode.hexeditor/1.11.1/vsix/signature"
```

Le registre confronte le manifeste de l'archive aux octets qu'il stocke (une
archive faite sur d'autres octets vaut un `400`), la garde sous la même
autorisation de publication que la version, et annonce la signature sans asset
`PublicKey` : la clé est celle du signataire, pas celle de ce registre.

Ce qui s'installe où, mesuré avec VS Code 1.136.1
([RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry) §4.5) :

| Version de l'éditeur | Registre non signé | Registre signé |
|---|---|---|
| VS Code d'origine, la vue | Installer grisé, *not signed* | Installer actif ; l'installation exige `extensions.verifySignature: false` — le vérificateur de l'éditeur n'accepte que la signature de la place de marché Microsoft |
| VS Code d'origine, `code --install-extension` | refusé, `NotSigned` (depuis 1.136) | refusé tant que le même réglage n'est pas désactivé |
| code-server, VSCodium (réglage livré désactivé) | Installer grisé | s'installe |
| che-code (livré sans vérificateur ; mesuré sur 1.128.1) | vue : Installer grisé ; `code --install-extension` installe | s'installe, rien à configurer — *Extension signature verification is not done* dans son log |
| Une extension proxifiée depuis la place de marché Microsoft | refusée : le proxy retirait autrefois la signature de l'amont | s'installe partout, rien à régler — la signature de l'amont est relayée et vérifiée par l'éditeur |
| Une extension de la place de marché republiée localement avec sa signature attachée | — | s'installe partout, vérificateur activé : `vsce-sign` répond `Success` à l'archive de la place de marché servie par ce registre (mesuré, `ms-vscode.hexeditor`) |

### Référence des endpoints

<!-- BEGIN endpoints: proxy/openvsx -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{extension_id}/{version}/vsix` | Download a VS Code extension VSIX package. |
| `PUT` | `/proxy/{registry}/{extension_id}/{version}/vsix` | Upload a VS Code extension VSIX package. |
| `PUT` | `/proxy/{registry}/{extension_id}/{version}/vsix/signature` | `PUT /proxy/{registry}/{extension_id}/{version}/vsix/signature` — attach an |
| `POST` | `/proxy/{registry}/api/-/namespace/create` | Claim an OpenVSX publisher namespace. |
| `GET` | `/proxy/{registry}/api/-/public-key/{key_id}` | `GET /proxy/{registry}/api/-/public-key/{key_id}` — the key this |
| `POST` | `/proxy/{registry}/api/-/publish` | `ovsx publish` — `POST /api/-/publish`. |
| `GET` | `/proxy/{registry}/api/-/search` | Search the registry — `GET …/api/-/search`. |
| `GET` | `/proxy/{registry}/api/{namespace}` | `GET /api/{namespace}` — what a publisher has here. |
| `GET` | `/proxy/{registry}/api/{namespace}/{extension}` | The newest version of one extension — `GET …/api/{namespace}/{extension}`. |
| `GET` | `/proxy/{registry}/api/{namespace}/{extension}/{version}` | One specific version — `GET …/api/{namespace}/{extension}/{version}`. |
| `GET` | `/proxy/{registry}/api/{namespace}/{extension}/{version}/file/{filename}` | One file out of an extension — `GET …/api/{ns}/{ext}/{version}/file/{name}`. |
| `GET` | `/proxy/{registry}/api/version` | `GET /api/version` — the registry's own version document. |
| `GET` | `/proxy/{registry}/vscode/asset/{publisher}/{name}/{version}/{asset_type}` | `GET …/vscode/asset/{publisher}/{name}/{version}/{asset_type}` |
| `POST` | `/proxy/{registry}/vscode/gallery/extensionquery` | Query the extension gallery. |
| `GET` | `/proxy/{registry}/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage` | `GET …/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage` |
| `GET` | `/proxy/{registry}/vscode/item` | `GET …/vscode/item?itemName=publisher.name` |
| `GET` | `/proxy/{registry}/vscode/unpkg/{publisher}/{name}/{version}/{path}` | `GET …/vscode/unpkg/{publisher}/{name}/{version}/{path}` |
<!-- END endpoints -->

---

## Authentification

Passez un token BatleHub dans un en-tête Bearer sur la requête de VSIX. L'accès
anonyme ne fonctionne que si le RBAC du registre accorde la lecture au rôle
`anonymous`.

### Le fichier de contrat d'identifiants {#the-credential-contract-file}

L'éditeur n'est pas la seule chose qui doive trouver un identifiant, et aucune de
celles qui le doivent ne peut partager la configuration de la CLI : un programme
démarré par une session de bureau, un modèle d'espace de travail ou un terminal
n'hérite ni d'une variable d'environnement ni d'une connexion. Il y a donc un
fichier, écrit par la CLI et lu par tout le reste
([RFC 0011](/rfc/0011-openvsx-login) §4.1) :

```sh
batlehub-cli --server https://hub.example.dev auth write-token-file
batlehub-cli auth status
```

```
REGISTRY                  KIND        TOKEN SOURCE               STATE  EXPIRES  REFRESH
https://hub.example.dev   oidc        inline (written by cli)    ok     4m12s    cli (batlehub-cli)
https://hub.k8s.dev       kubernetes  file /var/run/…/token      ok     —        reresolve
```

`$BATLEHUB_HOME/state/vsx-token.json`, en `0600`, sous `$HOME/.batlehub` par
défaut. Il est indexé par origine : un poste pointé vers trois BatleHub garde
donc trois identifiants dans un seul fichier, et se connecter à l'un n'est pas se
déconnecter des autres. La forme normative est le schéma JSON livré à côté de la
CLI, dans `cli/schema/vsx-token.schema.json`, pas cette page.

Deux propriétés méritent d'être connues avant d'écrire un consommateur :

- **Un identifiant n'a pas besoin d'y être.**
  `{"from": "file", "path": "/var/run/…"}` enregistre *où* en lire un, ce qui est
  exactement ce qu'on veut pour un token Kubernetes projeté que quelque chose
  d'autre garde frais. Passez `--from-file` à `write-token-file` pour en
  enregistrer un.
- **Rien ici n'échoue bruyamment.** Un fichier absent, un fichier illisible, une
  source qu'un consommateur n'implémente pas — tout cela signifie « pas
  d'identifiant », et l'éditeur se comporte alors exactement comme face à une
  galerie anonyme. C'est délibéré, et c'est la raison d'être d'`auth status` : il
  résout chaque entrée au moment où vous le demandez et nomme le motif quand
  l'une échoue, parce que *rien n'a été configuré* et *le fichier a disparu* sont
  indiscernables depuis l'éditeur et appellent des corrections opposées.

`batlehub-cli auth token` imprime un identifiant pour les scripts et les
courtiers, en le rafraîchissant d'abord s'il approche de son expiration. C'est la
seule commande dont le métier est d'en émettre un ; `auth status` rend un résumé
dont aucun champ ne peut contenir un secret.

### Un éditeur qui ne sait pas envoyer d'identifiant

VS Code d'origine, et toute variante qui lit sa galerie depuis `product.json`,
n'a aucun point d'accroche pour poser un en-tête `Authorization` sur les requêtes
de galerie. Pour ceux-là, lancez le proxy de galerie local et pointez l'éditeur
vers lui ([`batlehub-cli proxy serve`](/fr/use/cli#gallery-proxy), RFC 0011
§4.4) :

```sh
# Se connecter une fois, puis lancer le proxy ; l'éditeur est pointé vers ce qu'il imprime.
batlehub-cli --server https://batlehub.example.com auth login
batlehub-cli --server https://batlehub.example.com auth write-token-file
batlehub-cli proxy serve --registry https://batlehub.example.com/proxy/<registry>
```

```json
"extensionsGallery": {
  "serviceUrl": "http://127.0.0.1:<port>/<session>/vsx/vscode/gallery",
  "itemUrl": "http://127.0.0.1:<port>/<session>/vsx/vscode/item",
  "resourceUrlTemplate": "http://127.0.0.1:<port>/<session>/vsx/vscode/unpkg/{publisher}/{name}/{version}/{path}"
}
```

Le proxy attache l'identifiant du fichier de contrat ci-dessus, réécrit toutes
les URL de galerie vers lui-même de sorte que le téléchargement du `.vsix` soit
authentifié lui aussi, et — tant qu'il n'y a pas d'identifiant — répond à une
recherche par une unique entrée *Sign in to BatleHub* dont le détail donne les
étapes, au lieu de la vue vide que produit une galerie anonyme. L'éditeur ne
détient jamais le token ; il ne connaît que l'URL du proxy, propre à chaque
exécution, et qui est le secret. `product.json` est le seul endroit où une URL de
galerie peut être définie, et les mises à jour de l'éditeur l'écrasent : un
script de démarrage d'espace de travail qui lance le proxy avec
`--print-gallery-url` et réécrit le fichier est la forme qui dure.

## Notes

- La route directe du VSIX est
  `…/proxy/<registry>/{publisher}.{name}/{version}/vsix`.
- Endpoints de galerie : `POST …/vscode/gallery/extensionquery`,
  `GET …/vscode/asset/{publisher}/{name}/{version}/{assetType}`,
  `GET …/vscode/unpkg/{publisher}/{name}/{version}/{path}`,
  `GET …/vscode/item`, et
  `GET …/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage`.
- Endpoints de l'API OpenVSX : `GET …/api/{namespace}/{extension}[/{version}]`,
  `GET …/api/-/search`,
  `GET …/api/{namespace}/{extension}/{version}/file/{filename}`.
- Le manifeste, le README, le changelog, la licence et l'icône sont servis **à
  partir du VSIX en cache** : un seul artefact répond donc à toutes les requêtes
  d'asset, et une extension privée se comporte exactement comme une extension
  proxifiée. Une extension livrée sans changelog renvoie `404` pour cet asset, ce
  que l'éditeur affiche comme un onglet vide.
- L'icône d'une extension n'est jamais servie en `image/svg+xml`. Un SVG servi
  avec ce type exécute du script dans cette origine, la même où la console
  d'administration garde son token ; les icônes SVG reviennent en téléchargement
  opaque et l'éditeur n'affiche pas d'icône.
- Pour les extensions publiées uniquement sur la place de marché de Microsoft et
  non répliquées sur open-vsx.org, utilisez plutôt le type
  [place de marché VS Code](/fr/registries/vscode-marketplace).

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
