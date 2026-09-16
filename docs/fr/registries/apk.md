---
sourcePath: registries/apk.md
sourceHash: 4f37773b341e9554
---

# Alpine (apk)

Fait proxy d'un miroir Alpine et, en mode `local` ou `hybrid`, héberge votre
propre dépôt : publiez des paquets `.apk` et BatleHub régénère
l'`APKINDEX.tar.gz` de chaque architecture, en le signant avec une clé RSA que
tout apk distribué reconnaît.

C'est le seul membre de la famille des systèmes d'exploitation dont les
artefacts portent une **coordonnée**. Le nom de fichier d'un `.apk` s'écrit
`{nom}-{pkgver}-r{N}.apk` : un paquet peut donc être bloqué, soumis à un délai
de fraîcheur, compté et affiché dans la console — contrairement à un `.deb` ou
un `.rpm`, que le proxy ne sait adresser que par chemin.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `apk` |
| **Amont par défaut** | aucun — déclarez `upstreams` explicitement pour proxy et hybrid |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par chemin, avec une coordonnée sur chaque `.apk` |
| **Publication privée** | ✅ `curl -X PUT … /apk/upload` |
| **Coupure réseau** | prévu : l'index composé est le document de cette instance, donc contrairement aux autres systèmes il *peut* être resigné hors ligne |

## L'index est relayé octet pour octet, et pourquoi

`APKINDEX.tar.gz` est signé en RSA sur ses propres octets, et **tout apk
distribué vérifie cette signature avant de lire le moindre octet de l'index**.
Modifier une ligne invalide la signature, et la réaction du client est de
refuser le dépôt entier — et non d'ignorer l'entrée modifiée.

BatleHub relaie donc l'index amont tel quel, exactement comme il relaie ceux de
`deb`, `rpm` et `pacman`, et applique le blocage là où c'est possible : sur le
`.apk` lui-même, avec un `403` avant qu'un seul octet ne quitte le site.

**La limite, énoncée franchement**, parce qu'elle change ce qu'un blocage vous
apporte :

- Une branche Alpine ne contient **qu'une version par paquet**. La bloquer
  revient à bloquer le paquet jusqu'à ce que la branche monte de version : il
  n'existe pas de version antérieure vers laquelle le solveur pourrait se
  replier.
- L'index liste toujours la version bloquée, donc `apk add <paquet>` la
  *sélectionne* puis échoue au téléchargement. apk 2.14 affiche
  `ERROR: <paquet>-<version>: Permission denied` ; apk 3 affiche
  `HTTP 403: Forbidden`. Dans les deux cas la transaction échoue et rien n'est
  installé.
- En mode **local**, rien de tout cela ne s'applique : cet index est le document
  de BatleHub, une version bloquée en est simplement absente, et apk signale son
  propre « unable to select ».

## Configuration du proxy

Pointez `/etc/apk/repositories` vers le registre. apk ajoute lui-même
`{arch}/APKINDEX.tar.gz` et `{arch}/{fichier}.apk`, donc chaque ligne nomme une
branche et un dépôt :

```sh
# /etc/apk/repositories
https://batlehub.example.com/proxy/<registre>/apk/v3.22/main
https://batlehub.example.com/proxy/<registre>/apk/v3.22/community
```

apk 3 (Alpine 3.23 et ultérieur) accepte aussi la forme à composants, qui
produit exactement les mêmes requêtes :

```sh
https://batlehub.example.com/proxy/<registre>/apk/v3.24 main community
```

Migrer une image standard tient en une ligne :

```sh
sed -i 's#https://dl-cdn.alpinelinux.org/alpine#https://batlehub.example.com/proxy/<registre>/apk#' \
  /etc/apk/repositories
```

Puis `apk update` et `apk add` comme d'habitude. Les clés de signature d'Alpine
sont déjà dans l'image et l'index relayé se vérifie avec elles.

### L'entrée `upstreams` est la racine de l'arbre

```toml
[[registries]]
name      = "alpine"
type      = "apk"
mode      = "proxy"
upstreams = ["https://dl-cdn.alpinelinux.org/alpine"]   # la racine, pas une branche
```

Ni `…/alpine/v3.22`, ni `…/alpine/v3.22/main` : le client ajoute lui-même la
branche et le dépôt, donc une racine qui en contient déjà une place l'index à
`…/v3.22/v3.22/main/…`. BatleHub le refuse au démarrage plutôt que de laisser
l'erreur survenir au premier `apk update`.

Bornez l'arbre avec `path_allow` si vous n'utilisez que certaines branches :

```toml
path_allow = ["v3.22/**", "v3.24/**", "latest-stable/**"]
```

## Publication (local / hybrid)

Envoyez un paquet avec un `PUT`. Le nom, la version et l'architecture sont lus
dans le `.PKGINFO` embarqué — jamais dans le nom de fichier que vous envoyez —
et BatleHub stocke le fichier, régénère `{arch}/APKINDEX.tar.gz` et le signe :

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello-1.0-r0.apk \
  https://batlehub.example.com/proxy/<registre>/apk/upload
```

Le paquet n'a **pas** besoin d'être signé lui-même. Lorsqu'apk installe depuis
un dépôt, il vérifie la somme de contrôle du segment de contrôle du paquet
contre le champ `C:` de l'index et ne consulte jamais la signature du paquet :
un paquet non signé s'installe donc parfaitement depuis un index signé.

### Ce que le paquet doit être

**Un `.apk` v2, et `apk mkpkg` n'en produit pas.** Le constructeur d'apk-tools 3
écrit le conteneur v3 (ADB), qu'un `APKINDEX` v2 n'a aucun moyen de décrire et
pour lequel aucune branche Alpine ne publie d'index. Un envoi dans ce format est
refusé par un `400` qui le nomme. Utilisez `abuild`, qui écrit toujours du v2.

Deux autres exigences viennent d'apk 3 et non de BatleHub, et apk 2.14 accepte
des paquets qui enfreignent l'une ou l'autre : un paquet qui s'installe sur
Alpine 3.22 et échoue sur 3.24 relève presque toujours de l'une des deux.

| Exigence | Ce que dit apk 3 si elle manque |
| --- | --- |
| `.PKGINFO` porte `datahash` (sha256 du segment de données compressé) | `v2 package format error` |
| Le segment de données nomme `usr/…` directement, sans entrée racine `.` | `file format is invalid or inconsistent` |

`abuild` satisfait les deux. Un paquet assemblé à la main doit les satisfaire
lui-même.

### Signer l'index

```toml
[registries.apk_signing]
key_name        = "internal-apk@example.com-5f3a1c2e.rsa.pub"
private_key_pem = "${APK_SIGNING_KEY_PEM}"   # RSA, PEM, 2048 bits ou plus
```

Générez la paire une fois :

```bash
openssl genrsa -out apk-signing.pem 4096
openssl rsa -in apk-signing.pem -pubout -out internal-apk@example.com-5f3a1c2e.rsa.pub
```

**`key_name` doit correspondre exactement au nom de fichier côté client.** apk
ouvre la clé publique *par le nom que porte l'entrée de signature*, dans
`/etc/apk/keys/`. Une divergence ne produit pas de message d'erreur : elle
produit un index non fiable. La renommer plus tard oblige à la réinstaller sur
chaque client, alors choisissez-la une bonne fois.

Les consommateurs l'installent avant leur premier `apk update` :

```sh
KEY=internal-apk@example.com-5f3a1c2e.rsa.pub
curl -fsSL -o /etc/apk/keys/$KEY \
  https://batlehub.example.com/proxy/<registre>/apk/keys/$KEY
echo https://batlehub.example.com/proxy/<registre>/apk >> /etc/apk/repositories
apk update
```

La clé est servie en direct, avant toute publication, pour qu'un client puisse
être configuré d'abord.

Un dépôt local non signé est ininstallable par tout apk, sauf si le client passe
`--allow-untrusted` — ce qui **désactive aussi la vérification d'identité du
paquet**. Cette option n'est donc pas documentée ici comme une possibilité. Si
vous en voulez un malgré tout, déclarez-le explicitement avec
`apk_unsigned = true` : BatleHub refuse de le déduire du silence.

### Faire tourner la clé

Déployer une clé sur un parc prend le temps de la machine la plus lente, et un
index signé de la seule nouvelle clé est ininstallable partout tant que toutes
ne l'ont pas. La rotation est donc additive : déclarez la clé sortante sous
`previous_keys` et BatleHub signe chaque index avec **les deux**.

```toml
[registries.apk_signing]
key_name        = "internal-apk@example.com-9e21ff40.rsa.pub"   # la nouvelle
private_key_pem = "${APK_SIGNING_KEY_PEM}"

[[registries.apk_signing.previous_keys]]
key_name        = "internal-apk@example.com-5f3a1c2e.rsa.pub"   # la sortante
private_key_pem = "${APK_SIGNING_KEY_PEM_OLD}"
```

apk installe depuis la première entrée `.SIGN.*` dont il détient le fichier de
clé : une machine qui a la nouvelle clé prend la nouvelle signature, une machine
qui ne l'a pas retombe sur l'ancienne, et ni l'une ni l'autre ne s'en aperçoit.
Les deux noms sont servis sur la route des clés, de sorte qu'un retardataire
peut encore récupérer l'ancien fichier. Retirez l'entrée `previous_keys` une fois
que tous les clients ont la nouvelle clé ; une liste vide est une rotation
terminée.

La fenêtre est une liste plate : une clé retirée ne peut pas porter ses propres
clés retirées, et un nom ne peut pas apparaître deux fois.

## Authentification

Le téléchargeur d'apk est sa propre copie de libfetch. Il n'accepte aucune
configuration d'en-tête ni de netrc : le seul mécanisme de justificatif est
l'authentification HTTP Basic intégrée à l'URL du dépôt.

```sh
https://<utilisateur>:<jeton>@batlehub.example.com/proxy/<registre>/apk/v3.22/main
```

Cela fonctionne, et cela fait fuiter le jeton dans la ligne `fetch https://…`
qu'`apk update` affiche lui-même. Préférez des verbes de lecture `anonymous` sur
le registre plus une entrée réseau authentifiante, et gardez la forme URL comme
dernier recours — c'est le conseil que la page du
[registre generic](/fr/registries/generic) donne pour la même raison.

## Notes

- `keys/` est un **préfixe réservé** sous `…/apk/` : il sert la clé de
  signature, il ne peut donc pas servir à relayer un chemin amont de ce nom.
  L'arbre d'Alpine n'en contient aucun.
- Le délai de fraîcheur dispose ici d'une vraie date : chaque paquet listé par un
  `APKINDEX` porte son horodatage de construction dans `t:`. Une règle
  `release_age_gate` sur un registre `apk` doit déclarer `deny_missing_timestamp`
  explicitement, car le seul cas sans date — une version que l'index en cache ne
  liste plus — admet deux réponses opposées et BatleHub n'en choisira pas une
  pour vous.
- `latest-stable` est un lien symbolique du miroir vers la branche stable la plus
  récente. BatleHub le traite comme un chemin ordinaire : le même fichier atteint
  par `v3.24/` et par `latest-stable/` représente deux entrées de cache et une
  seule copie stockée.
- La publication exige que le registre soit en mode `local` ou `hybrid` —
  adressez-vous à votre administrateur.
- **Une instance coupée du réseau répond à `apk update`.** Sans amont à relayer,
  un BatleHub déconnecté compose `APKINDEX.tar.gz` à partir des paquets qu'il
  détient réellement et le signe avec la clé du registre : le client résout donc
  à travers un listing vrai par construction — chaque version qu'il nomme est
  servie à la requête suivante, et un paquet que le parc n'a jamais reçu en est
  simplement absent. Cela exige `[registries.apk_signing]` sur le registre même
  en mode `proxy` : sans clé, l'index ne peut pas être signé et la requête reste
  le `503` d'un défaut hors ligne. `apk` est le seul type de paquets système
  capable de cela : les index `deb`, `rpm` et `pacman` sont signés par des clés
  que le parc ne détient pas.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — jetons, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
