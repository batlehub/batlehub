---
sourcePath: registries/pacman.md
sourceHash: 192301240c88789b
---

# Pacman (Arch Linux)

Fait proxy d'un miroir Arch Linux et, en mode `local` ou `hybrid`, héberge le
vôtre : publiez des paquets `.pkg.tar.{zst,xz,gz}` et BatleHub régénère les bases
`<repo>.db` et `<repo>.files` par architecture, en les signant avec une clé
OpenPGP Ed25519 quand `[registries.repo_signing]` est configuré.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `pacman` |
| **Amont par défaut** | aucun — déclarez `upstreams` explicitement pour proxy et hybrid |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par chemin |
| **Publication privée** | ✅ `curl -X PUT … /pacman/upload` |
| **Coupure réseau** | pas d'index composé hors ligne : une base signée ne peut pas être re-signée ici ; un fichier détenu est servi par chemin |

## Mise en place du proxy

Ajoutez une strophe de dépôt à `/etc/pacman.conf`. Le nom de section doit
correspondre au nom de la base, qui est le nom du registre ; `$arch` est
substitué par pacman :

```ini
# /etc/pacman.conf
[<registry>]
SigLevel = Required
Server = https://batlehub.example.com/proxy/<registry>/pacman/$arch
```

Importez la clé de signature (dépôts local/hybrid signés uniquement) :

```bash
curl -fsSL https://batlehub.example.com/proxy/<registry>/pacman/key.gpg \
  | sudo pacman-key --add -
sudo pacman-key --lsign-key <key-id>
```

Puis :

```bash
sudo pacman -Sy
sudo pacman -S hello
```

La base est servie sous `$arch/<registry>.db`. Pour un dépôt **local** non signé
(pas de clé `[registries.repo_signing]`), mettez
`SigLevel = Optional TrustAll` (ou `Never`) et sautez l'import de la clé.

Le **mode proxy** n'a pas de clé BatleHub — `pacman/key.gpg` n'est servi que pour
les registres `local` et `hybrid` dotés d'une clé `repo_signing`. En mode proxy,
les paquets sont signés (ou non) par le miroir **amont** : réglez donc `SigLevel`
en fonction de la signature de l'amont.

## Publication (local / hybrid) {#publishing-local-hybrid}

Envoyez un paquet par un `PUT`. Le nom, la version et l'architecture sont lus
dans le `.PKGINFO` embarqué ; BatleHub range le fichier sous `{arch}/` et
régénère la base `<repo>.db` (en la re-signant quand une clé de signature est
configurée) :

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello-1.0-1-x86_64.pkg.tar.zst \
  https://batlehub.example.com/proxy/<registry>/pacman/upload
```

La signature exige une clé OpenPGP Ed25519 sous `[registries.repo_signing]` ; les
consommateurs l'importent depuis `…/pacman/key.gpg` avec `pacman-key --add` puis
la signent localement (`pacman-key --lsign-key`).

## Authentification

Pacman n'a pas de fichier d'identifiants dédié — embarquez le token comme
identifiants HTTP Basic dans l'URL `Server` :

```ini
Server = https://<user>:<token>@batlehub.example.com/proxy/<registry>/pacman/$arch
```

## Notes

- Le nom de section `[<section>]` de `pacman.conf` **doit** être égal au nom du
  registre, parce que la base est servie sous `$arch/<registry>.db` et que pacman
  déduit le nom du fichier de base du nom de section.
- Publier exige que le registre soit en mode `local` ou `hybrid` — demandez à
  votre administrateur.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
