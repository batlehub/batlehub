---
sourcePath: registries/vscode-marketplace.md
sourceHash: 44ab88c2014f12c2
---

# Place de marché VS Code

Fait proxy et cache des téléchargements de VSIX d'extensions VS Code depuis la
[Visual Studio Marketplace](https://marketplace.visualstudio.com) de Microsoft,
par son API Gallery. Servez-vous-en pour les extensions présentes uniquement sur
la place de marché Microsoft et non répliquées sur open-vsx.org ; ce type peut
aussi héberger des extensions privées.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `vscode-marketplace` |
| **Amont par défaut** | `marketplace.visualstudio.com` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ envoi de VSIX (`PUT …/vsix`) |
| **Coupure réseau** | pas de listing composé hors ligne : une galerie répond à des requêtes |
| **Signatures** | le registre signe ce qu'il héberge (`[registries.vsx_signing]`), relaie celle de l'amont pour ce dont il fait proxy, et conserve celle attachée à une version republiée |

## Mise en place du proxy

### Utiliser BatleHub comme galerie d'extensions

Même protocole, mêmes routes que sur la
[page OpenVSX](/fr/registries/openvsx#use-batlehub-as-your-extension-gallery) —
la galerie est le côté *client*, elle ne dépend donc pas de l'amont qui se
trouve derrière le registre :

```jsonc
{
  "extensionsGallery": {
    "serviceUrl": "https://batlehub.example.com/proxy/<registry>/vscode/gallery",
    "itemUrl": "https://batlehub.example.com/proxy/<registry>/vscode/item",
    "resourceUrlTemplate": "https://batlehub.example.com/proxy/<registry>/vscode/unpkg/{publisher}/{name}/{version}/{path}"
  }
}
```

L'éditeur n'envoie aucun identifiant à sa galerie : ce registre a donc besoin
d'`anonymous = ["releases:read", "source:read"]` sous `[registries.rbac]`, ou
d'un ingress qui authentifie. Voir l'avertissement sur la
[page OpenVSX](/fr/registries/openvsx#use-batlehub-as-your-extension-gallery).

### Un éditeur qui ne sait pas envoyer d'identifiant

`product.json` n'a nulle part où mettre un token : un registre qui refuse les
lectures anonymes s'atteint donc par le proxy de galerie local
([`batlehub-cli proxy serve`](/fr/use/cli#gallery-proxy), RFC 0011 §4.4) plutôt
qu'en ouvrant le registre :

```sh
# Se connecter une fois, puis lancer le proxy ; l'éditeur est pointé vers ce qu'il imprime.
batlehub-cli --server https://batlehub.example.com auth login
batlehub-cli --server https://batlehub.example.com auth write-token-file
batlehub-cli proxy serve --registry https://batlehub.example.com/proxy/<registry>
```

Le proxy attache l'identifiant, réécrit toutes les URL de galerie vers lui-même
de sorte que le téléchargement du `.vsix` soit authentifié lui aussi, et — tant
qu'aucun identifiant ne se résout — répond à une recherche par une unique entrée
*Sign in to BatleHub*, au lieu de la vue vide que produit une galerie anonyme.
Pointez l'éditeur vers l'URL de boucle locale qu'il imprime, à la place du
`serviceUrl` ci-dessus.

### Télécharger un VSIX directement

Téléchargez un VSIX par sa coordonnée, puis installez-le. Remplacez `<registry>`
par le nom de registre que vous avez configuré ; utilisez `latest` comme version
pour obtenir la plus récente :

```sh
# Version figée
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/ms-python.python/2024.2.1/vsix" \
  -o ms-python.python-2024.2.1.vsix

# Ou la dernière version
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/ms-python.python/latest/vsix" \
  -o ms-python.python.vsix

code --install-extension ms-python.python-2024.2.1.vsix
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Le registre doit être en mode `local` ou `hybrid`. Un registre
`vscode-marketplace` partage l'endpoint d'envoi et de téléchargement de VSIX avec
[OpenVSX](/fr/registries/openvsx) ; pointez l'envoi
`PUT …/{publisher}.{name}/{version}/vsix` vers le nom de votre registre
`vscode-marketplace` :

```sh
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-org.my-extension-1.0.0.vsix \
  "https://batlehub.example.com/proxy/<registry>/my-org.my-extension/1.0.0/vsix"
```

### Signatures

La vue Extensions d'un éditeur récent n'installe que des entrées qui portent un
asset de signature. Ce registre en obtient un de trois façons, et la
[page OpenVSX](/fr/registries/openvsx#signatures) explique chacune : la signature
propre à la place de marché est relayée pour ce dont on fait proxy (un VS Code
d'origine la vérifie, rien à régler) ; une clé sous
[`[registries.vsx_signing]`](/fr/guide/configuration#vsx-signing) signe ce qui est
publié ici ; et une extension de la place de marché republiée ici conserve sa
signature quand l'archive est attachée après l'envoi :

```sh
curl -X PUT -H "Authorization: Bearer $BATLEHUB_TOKEN" -H "Content-Type: application/zip" \
  --data-binary @ms-vscode.hexeditor-1.11.1.sigzip \
  "https://batlehub.example.com/proxy/<registry>/ms-vscode.hexeditor/1.11.1/vsix/signature"
```

## Authentification

Passez un token BatleHub dans un en-tête Bearer sur la requête de VSIX. L'accès
anonyme ne fonctionne que si le RBAC du registre accorde la lecture au rôle
`anonymous`.

## Notes

- La route directe du VSIX est
  `…/proxy/<registry>/{publisher}.{name}/{version}/vsix`, identique à celle
  d'OpenVSX — les deux types partagent le même gestionnaire.
- Préférez [OpenVSX](/fr/registries/openvsx) pour les extensions répliquées sur
  open-vsx.org ; n'employez ce type que pour les extensions exclusives à la place
  de marché Microsoft.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
