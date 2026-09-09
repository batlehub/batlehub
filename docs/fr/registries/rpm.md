---
sourcePath: registries/rpm.md
sourceHash: d1472016b8479eb6
---

# RPM / YUM (DNF)

Fait proxy d'un dépôt YUM ou DNF et, en mode `local` ou `hybrid`, héberge le
vôtre : publiez des paquets `.rpm` et BatleHub régénère `repodata/`, en signant
`repomd.xml.asc` avec une clé OpenPGP Ed25519 quand
`[registries.repo_signing]` est configuré.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `rpm` |
| **Amont par défaut** | aucun — déclarez `upstreams` explicitement pour proxy et hybrid |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par chemin |
| **Publication privée** | ✅ `curl -X PUT … /rpm/upload` |
| **Coupure réseau** | pas d'index composé hors ligne : un `repomd.xml` signé ne peut pas être re-signé ici ; un fichier détenu est servi par chemin |

## Mise en place du proxy

Ajoutez un fichier `.repo` sous `/etc/yum.repos.d/`. Remplacez `<registry>` par
le nom de registre que vous avez configuré :

```ini
# /etc/yum.repos.d/<registry>.repo
[<registry>]
name=<registry>
baseurl=https://batlehub.example.com/proxy/<registry>/rpm
enabled=1
repo_gpgcheck=1
gpgcheck=0
gpgkey=https://batlehub.example.com/proxy/<registry>/rpm/repodata/repomd.xml.key
```

```bash
sudo dnf makecache && sudo dnf install hello
```

Pour un dépôt **local** non signé (pas de clé `[registries.repo_signing]`),
mettez `repo_gpgcheck=0` et omettez `gpgkey`.

Le **mode proxy** n'a pas de clé BatleHub — `repodata/repomd.xml.key` n'est servi
que pour les registres `local` et `hybrid` dotés d'une clé `repo_signing`. En
mode proxy, BatleHub relaie le `repodata` **amont** (y compris un éventuel
`repomd.xml.asc`) : faites donc pointer `gpgkey` vers la clé du **projet amont**
avec `repo_gpgcheck=1`, ou mettez `repo_gpgcheck=0` si vous faites confiance au
canal.

## Publication (local / hybrid) {#publishing-local-hybrid}

Envoyez un `.rpm` par un `PUT` ; BatleHub régénère `repodata/` et re-signe
`repomd.xml.asc` quand une clé de signature est configurée :

```bash
curl -X PUT \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  --data-binary @hello-1.0-1.x86_64.rpm \
  https://batlehub.example.com/proxy/<registry>/rpm/upload
```

La signature exige une clé OpenPGP Ed25519 sous `[registries.repo_signing]`. Les
consommateurs la vérifient contre `…/rpm/repodata/repomd.xml.key` avec
`repo_gpgcheck=1`.

## Authentification

DNF et YUM lisent `username` et `password` directement dans le fichier `.repo` :

```ini
[<registry>]
name=<registry>
baseurl=https://batlehub.example.com/proxy/<registry>/rpm
enabled=1
repo_gpgcheck=0
gpgcheck=0
username=<your-username>
password=<your-token>
```

Vous pouvez aussi employer `~/.netrc` (DNF et libcurl l'honorent pour
l'authentification HTTP Basic) :

```text
machine batlehub.example.com
login <your-username>
password <your-token>
```

## Notes

- `repo_gpgcheck` contrôle la vérification de la signature des **métadonnées du
  dépôt** (`repomd.xml.asc`) ; `gpgcheck` contrôle les signatures RPM de chaque
  paquet — BatleHub signe les métadonnées, pas les paquets individuels, donc
  `gpgcheck=0` est attendu.
- Publier exige que le registre soit en mode `local` ou `hybrid` — demandez à
  votre administrateur.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
