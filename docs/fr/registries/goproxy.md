---
sourcePath: registries/goproxy.md
sourceHash: 7021356d815f56fe
---

# Modules Go

Fait proxy et cache des modules Go par le
[protocole GOPROXY](https://go.dev/ref/mod#goproxy-protocol), ou héberge des
modules privés. BatleHub met en cache les zips de module définitivement après le
premier téléchargement, et fait aussi proxy de la base de vulnérabilités Go, de
sorte que `govulncheck` fonctionne sans accès direct à Internet.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `goproxy` |
| **Amont par défaut** | `proxy.golang.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ (envoi d'un zip de module) |
| **Coupure réseau** | hors ligne, `@v/list`, `@latest` et `.info` sont composés à partir des zips détenus ; le `.mod` doit être dans le lot à côté du zip, et c'est au client de désactiver `GOSUMDB` |

## Mise en place du proxy

Faites pointer la chaîne d'outils go vers votre registre :

```bash
export GOPROXY="https://batlehub.example.com/proxy/<registry>"

go get golang.org/x/text@v0.3.7
```

Il n'y a délibérément pas de repli `,direct` ici : tous les modules se résolvent
alors par BatleHub, le proxy reste le point d'entrée unique, et un défaut échoue
bruyamment plutôt que d'atteindre Internet en silence — ce qui est en général la
raison même de faire tourner un proxy. Activez-le explicitement quand vos hôtes
de build *ont* le droit de récupérer directement et que vous voulez ce repli sur
un 404 :

```bash
export GOPROXY="https://batlehub.example.com/proxy/<registry>,direct"
```

Pour les **modules privés**, listez leurs préfixes de chemin afin que l'outil go
cesse de les confronter à la base de sommes de contrôle publique
(`sum.golang.org`), qui ne les a jamais vus :

```bash
# Privés et servis par BatleHub : toujours par le proxy, mais sans contrôle de somme.
export GONOSUMDB="example.com/internal/*"

# Privés et récupérés directement depuis le VCS : GOPRIVATE implique à la fois
# GONOPROXY et GONOSUMDB, donc ceux-là contournent entièrement BatleHub.
export GOPRIVATE="example.com/internal/*"
```

Pour les **modules publics**, aucune variable de ce genre n'est nécessaire :
BatleHub fait aussi proxy de la base de sommes de contrôle, sur `/sumdb/{path}`.
C'est l'autre moitié du protocole GOPROXY, et sans elle l'outil go ouvrirait
encore une connexion directe vers `sum.golang.org` pour chaque module inconnu —
le proxy aurait déplacé la sortie réseau plutôt que de la supprimer, et un build
coupé du réseau échouerait sur une consultation impossible.

Les réponses de sommes de contrôle sont mises en cache, et c'est ce qui fait
fonctionner le cas hors ligne : le deuxième build n'a besoin d'aucune route vers
l'extérieur. Ce cache est sain parce que le journal est signé — la signature
voyage avec les octets, de sorte qu'un enregistrement en cache est exactement
aussi digne de confiance qu'un enregistrement en direct, et BatleHub ne
l'analyse ni ne le réécrit.

Faites pointer `GOSUMDB` vers le proxy, ou laissez sa valeur par défaut et
laissez `GOPROXY` porter les consultations :

```bash
export GOSUMDB="sum.golang.org https://batlehub.example.com/proxy/<registry>/sumdb/sum.golang.org"
```

Mettez `sumdb_url = ""` sur un registre qui ne sert **que** des modules privés :
une consultation y publierait des chemins de modules privés dans un journal de
transparence public, et `GONOSUMDB` ci-dessus est la bonne réponse pour eux.

Pour rendre l'un de ces réglages permanent, utilisez `go env -w`, par exemple
`go env -w GOPROXY="https://batlehub.example.com/proxy/<registry>"`.

## Publication (local / hybrid) {#publishing-local-hybrid}

Les modules Go se publient en envoyant une archive zip de module. BatleHub en
extrait le `go.mod` et génère automatiquement les métadonnées de version — il n'y
a pas d'étape d'envoi de métadonnées séparée.

### Configuration du serveur

```toml
[[registries]]
type = "goproxy"
name = "internal-go"
mode = "local"

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://proxy.golang.org"]`.

### Construire le zip du module

Utilisez la commande standard `go mod zip` depuis le répertoire source du
module :

```sh
# Depuis la racine de votre module (là où vit go.mod)
go mod zip example.com/mymod@v1.0.0 . --mod-zip /tmp/mymod-v1.0.0.zip
```

Le zip doit contenir tous les fichiers sous un unique répertoire de premier
niveau nommé `{module}@{version}/` (par exemple `example.com/mymod@v1.0.0/`).
`go mod zip` produit cette disposition automatiquement. Si vous construisez le
zip à la main, tous les chemins d'entrée doivent porter ce préfixe.

### Envoyer

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @/tmp/mymod-v1.0.0.zip \
  "https://batlehub.example.com/proxy/internal-go/example.com/mymod/@v/v1.0.0.zip"
```

Un chemin de module peut contenir des barres obliques — le motif d'URL capture
tout ce qui précède `/@v/` comme chemin de module.

### Configurer la chaîne d'outils go

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="https://batlehub.example.com/proxy/internal-go,direct"
```

Ou enregistrez-le durablement avec `go env -w` :

```sh
go env -w GONOSUMCHECK="*"
go env -w GONOSUMDB="*"
go env -w GOPROXY="https://batlehub.example.com/proxy/internal-go,direct"
```

`GONOSUMCHECK` et `GONOSUMDB` désactivent la base de sommes de contrôle pour les
modules privés. Le repli `,direct` dit à l'outil go d'atteindre Internet
directement si le proxy renvoie un 404 — retirez-le si BatleHub doit être la
seule source.

### Vérifier

```sh
go get example.com/mymod@v1.0.0
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/goproxy -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/{module}/@latest` | Fetch the latest version info for a Go module. |
| `GET` | `/proxy/{registry}/{module}/@v/{filename}` | Fetch a versioned Go module file: `.info`, `.mod`, or `.zip`. |
| `PUT` | `/proxy/{registry}/{module}/@v/{filename}` | Publish a Go module version by uploading its zip archive. |
| `GET` | `/proxy/{registry}/{module}/@v/list` | List known versions for a Go module. |
| `GET` | `/proxy/{registry}/sumdb/{path}` | Proxy the Go checksum database. |
| `GET` | `/proxy/{registry}/v1/ID/{id}.json` | Proxy a single Go vulnerability record by its ID (e.g. `GO-2023-1234`). |
| `GET` | `/proxy/{registry}/v1/index.json` | Proxy the Go Vulnerability Database index. |
| `POST` | `/proxy/{registry}/v1/query` | Proxy a Go vulnerability database query. |
<!-- END endpoints -->

---

## Versions bloquées

`@v/list` retire la ligne de la version bloquée, et `@latest` est **re-résolu**
sur ce qui survit plutôt que filtré — il nomme une version et ne porte pas de
liste. Le `@latest` reconstruit porte `Version` et omet `Time`, parce que
l'horodatage appartenait à la publication qu'on masque. S'il ne reste aucune
version à nommer, `@latest` répond `404`, ce que le client Go gère déjà pour un
module sans publication.

Un `v` initial et un suffixe `+incompatible` nomment la même publication de toute
façon : un blocage enregistré dans l'une de ces orthographes masque celle qui est
listée.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Les envois passent un token BatleHub dans un en-tête Bearer. Pour l'accès en
lecture, mettez le token dans `~/.netrc` afin que l'outil go et `govulncheck` le
reprennent automatiquement :

```bash
cat >> ~/.netrc <<EOF
machine batlehub.example.com login user password $BATLEHUB_TOKEN
EOF
chmod 600 ~/.netrc
```

## Notes

BatleHub fait proxy de la [base de vulnérabilités Go](https://vuln.go.dev), de
sorte que `govulncheck` fonctionne sans atteindre vuln.go.dev. Donnez à
`GOVULNDB` la même URL de base qu'à `GOPROXY` :

```bash
export GOVULNDB="https://batlehub.example.com/proxy/<registry>"
govulncheck ./...
```

L'URL de la base de vulnérabilités amont vaut `https://vuln.go.dev` par défaut et
se remplace registre par registre avec `vuln_db_url` dans la configuration du
serveur ; la valeur `""` désactive les endpoints. Les réponses `@latest` et
`@v/list` sont mises en cache — videz le stockage du proxy pour prendre en compte
immédiatement une version nouvellement publiée.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
