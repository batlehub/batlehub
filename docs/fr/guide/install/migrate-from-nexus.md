---
sourcePath: guide/install/migrate-from-nexus.md
sourceHash: c09a0617b98c6129
---

# Migrer depuis Nexus Repository

Cette page fait passer une installation Sonatype Nexus Repository 3 sur
BatleHub, un dépôt à la fois, sans jour où tous les builds cassent en même temps.

BatleHub ne sait pas encore importer le *contenu* d'une instance Nexus en une
commande — c'est à la [feuille de route](/guide/roadmap) (« Seed a registry from
an incumbent »). La migration ci-dessous s'en passe : les dépôts proxy sont
recréés sur leurs amonts d'origine, les dépôts hébergés sont recopiés avec le
client de publication de chaque écosystème, et Nexus continue de servir jusqu'à
ce que le dernier client soit passé.

## Les concepts en regard

| Nexus | BatleHub | Notes |
|-------|----------|-------|
| Dépôt `/repository/<nom>/` | Registre `/proxy/<nom>/…` | Un bloc `[[registries]]` par dépôt. Le chemin après `<nom>` dépend du type — voir chaque [page de registre](/fr/registries/). |
| Dépôt *proxy* | `mode = "proxy"` | Cache en lecture traversante ; le mode par défaut. |
| Dépôt *hosted* | `mode = "local"` | BatleHub fait autorité ; les clients y publient. |
| Dépôt *group* | `mode = "hybrid"`, ou plusieurs `upstreams` | Voir [Groupes](#groups). |
| Blob store | Backend `[storage]` | Système de fichiers ou S3 ; un registre choisit un backend nommé avec `storage = "…"`. Voir [Configuration § `[storage]`](/fr/guide/configuration#_3-4-storage). |
| Cleanup policy | Éviction `[registries.cache]` | `artifact_ttl_secs`, `idle_days`, `max_size_bytes`, `keep_latest_n` — voir [Mise en cache](/fr/guide/caching). |
| Authentification distante | `[registries.upstream_auth]` | `basic`, `bearer` ou `header` — voir [Amonts privés](/fr/guide/private-upstreams). |
| Utilisateurs, rôles, realms LDAP/SAML | Fournisseurs `[[auth]]` (token, OIDC, Kubernetes, Actions OIDC) | Les groupes viennent du fournisseur d'identité. |
| Privilèges, content selectors | Grants sur l'instance, le registre, l'espace de noms, le paquet ou la version | Voir [Contrôle d'accès](/fr/guide/access-control). |
| User token | Token d'accès personnel | Voir [Obtenir un token](/fr/use/#getting-a-token). |

## Formats

| Format Nexus | `type` BatleHub | Équivalent hébergé |
|--------------|-----------------|:------------------:|
| `maven2` | [`maven`](/fr/registries/maven) | ✅ |
| `npm` | [`npm`](/fr/registries/npm) | ✅ |
| `pypi` | [`pypi`](/fr/registries/pypi) | ✅ |
| `nuget` | [`nuget`](/fr/registries/nuget) (protocole v3) | ✅ |
| `rubygems` | [`rubygems`](/fr/registries/rubygems) | ✅ |
| `go` | [`goproxy`](/fr/registries/goproxy) | ✅ |
| `apt` | [`deb`](/fr/registries/deb) | ✅ |
| `yum` | [`rpm`](/fr/registries/rpm) | ✅ |
| `conda` | [`conda`](/fr/registries/conda) | ✅ |
| `cargo` | [`cargo`](/fr/registries/cargo) | ✅ |
| `composer` | [`composer`](/fr/registries/composer) | ✅ |
| `raw` | [`generic`](/fr/registries/generic) | ❌ proxy seulement |
| `docker`, `helm`, `r`, `conan`, `cocoapods`, `p2`, `bower`, `gitlfs` | — | — |

La dernière ligne n'est **pas encore prise en charge**. Certains de ces
formats figurent à la [feuille de route](/guide/roadmap), et la liste des types
pris en charge s'allonge à chaque version :

- `helm` — un dépôt de charts (`index.yaml` + `.tgz`) est prévu par la
  [RFC 0029](/rfc/0029-helm-charts) (Draft). Les charts OCI restent hors
  périmètre.
- `docker` — non prévu : la feuille de route oriente vers un registre OCI
  dédié comme [Harbor](https://goharbor.io).
- `r`, `conan`, `cocoapods`, `p2`, `bower`, `gitlfs` — absents de la feuille de
  route à ce jour ; ouvrez une issue si vous en avez besoin.

En attendant, gardez ces dépôts dans Nexus, ou déplacez-les vers un outil
dédié, avant de prévoir l'arrêt de Nexus.

Un dépôt `raw` *hébergé* n'a pas de place non plus : `generic` ne fait que
refléter une arborescence de fichiers amont.

## 1. Faire l'inventaire

L'API REST de Nexus liste chaque dépôt avec son format, son type et son URL
distante :

```bash
curl -su admin "https://nexus.example.com/service/rest/v1/repositories" \
  | jq -r '.[] | [.name, .format, .type, (.attributes.proxy.remoteUrl // "")] | @tsv'
```

Répartissez le résultat en trois piles — proxy, hosted, group — et rayez les
formats de la dernière ligne du tableau ci-dessus. Chaque pile a son étape
ci-dessous.

## 2. Dépôts proxy

Faites pointer le registre BatleHub vers l'amont **d'origine**, pas vers Nexus :

```toml
[[registries]]
type      = "maven"
name      = "maven-central"
upstreams = ["https://repo1.maven.org/maven2"]
```

Le cache démarre vide et se remplit à la première demande de chaque artefact.
Pour éviter un premier build lent, préchauffez les paquets dont vous savez avoir
besoin :

```toml
[registries.cache]
warm_packages = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n = 3
```

Quand l'amont demande des identifiants que Nexus détenait (le flux privé d'un
éditeur), déplacez-les dans `[registries.upstream_auth]` — voir
[Amonts privés](/fr/guide/private-upstreams).

## 3. Dépôts hébergés

Un dépôt hébergé détient la seule copie de ce que vos équipes ont publié. Son
historique est recopié dans un registre `local` **avant** que quiconque publie
sur BatleHub.

::: warning Pas de pont en `hybrid` avec Nexus comme amont
Un registre `hybrid` répond à la liste des versions d'un paquet à partir de ses
seules versions locales dès qu'il en a une, et ne consulte plus l'amont pour ce
nom. Publiez `foo@1.0.1` dans un registre hybride dont l'amont est le dépôt
Nexus qui détient `foo@1.0.0`, et les clients npm, Maven, pip, NuGet, cargo et
Go ne voient plus `1.0.0`. Ce n'est pas le comportement d'un groupe Nexus, qui
fusionne les versions d'un paquet entre ses membres.
:::

### Créer le registre local

```toml
[[registries]]
type = "npm"
name = "npm-internal"
mode = "local"

[registries.grants]
"role:user"        = ["releases:read", "releases:list"]
"group:*:engineer" = ["releases:publish"]
```

### Recopier l'historique

Listez les assets du dépôt hébergé avec l'API components de Nexus (elle pagine
par `continuationToken`) :

```bash
repo=npm-hosted token=""
while :; do
  page=$(curl -su batlehub-reader \
    "https://nexus.example.com/service/rest/v1/components?repository=$repo${token:+&continuationToken=$token}")
  jq -r '.items[].assets[].downloadUrl' <<<"$page"
  token=$(jq -r '.continuationToken // empty' <<<"$page")
  [ -z "$token" ] && break
done > assets.txt
```

Téléchargez-les (`wget --user batlehub-reader --ask-password -i assets.txt`),
puis publiez chaque fichier avec le client de l'écosystème, comme n'importe
quelle nouvelle version :

| Type | Republier avec |
|------|----------------|
| `maven` | `mvn deploy:deploy-file -Dfile=<jar> -DpomFile=<pom> -Durl=https://batlehub.example.com/proxy/<registry>/maven2/ -DrepositoryId=<server-id>` |
| `npm` | `npm publish <package>-<version>.tgz` |
| `pypi` | `twine upload --repository-url https://batlehub.example.com/proxy/<registry>/legacy/ <files>` |
| `nuget` | `dotnet nuget push <file>.nupkg --source <source>` |
| `rubygems` | `gem push <file>.gem --host https://batlehub.example.com/proxy/<registry>` |

Chaque page de registre donne la configuration exacte du client sous
*Publication*. Chaque republication est une publication ordinaire : quotas,
propriété et analyses s'appliquent.

::: warning Désactivez `monotonic` pendant la copie
Un historique se publie du plus ancien au plus récent ; un espace de noms avec
`versioning.monotonic = true` le refuse. Réactivez-le ensuite — voir
[Immuabilité et ordre](/fr/guide/access-control#versioning).
:::

### Basculer

1. Passez le dépôt Nexus en lecture seule, pour que plus rien n'y soit publié.
2. Recopiez les versions publiées depuis la première copie.
3. Déplacez les publieurs (jobs de CI, `distributionManagement`,
   `publishConfig`) et les consommateurs vers BatleHub — voir
   [Migrer les clients](#_4-migrer-les-clients).

## Groupes {#groups}

Un groupe Nexus n'a pas d'équivalent terme à terme. Choisissez la forme qui
correspond à son contenu :

- **Un dépôt hébergé et un dépôt proxy** (le classique `maven-public`) : un seul
  registre `hybrid`, dans lequel l'historique hébergé est recopié comme à
  l'étape 3, avec l'amont public dans `upstreams`. Un nom publié localement est
  servi à partir de ses seules versions locales ; tout autre nom retombe sur
  l'amont.
- **Plusieurs dépôts proxy** : un seul registre `proxy` avec plusieurs
  `upstreams`. Ils sont essayés dans l'ordre, et le suivant prend le relais sur
  un `404`.
- **Un amont authentifié et un amont public** : `upstream_auth` est envoyé à
  *chaque* amont d'un registre, séparez-les donc — voir
  [Amonts privés](/fr/guide/private-upstreams), section « Mêler un amont privé
  et un repli public ».

## 4. Migrer les clients

Remplacez l'URL et l'identifiant Nexus dans la configuration de chaque client.
La forme de l'URL par type est sur les [pages de registre](/fr/registries/) ;
quelques exemples :

| Type | Nexus | BatleHub |
|------|-------|----------|
| `maven` | `https://nexus.example.com/repository/maven-public/` | `https://batlehub.example.com/proxy/<registry>/maven2/` |
| `npm` | `https://nexus.example.com/repository/npm-group/` | `https://batlehub.example.com/proxy/<registry>/` |
| `pypi` | `https://nexus.example.com/repository/pypi-group/simple/` | `https://batlehub.example.com/proxy/<registry>/simple/` |
| `nuget` | `https://nexus.example.com/repository/nuget-group/index.json` | `https://batlehub.example.com/proxy/<registry>/nuget/v3/index.json` |

L'identifiant devient un token d'accès personnel BatleHub. Les clients qui
envoient un nom d'utilisateur et un mot de passe (Maven, pip, NuGet) continuent
de le faire, avec le token comme mot de passe — le nom d'utilisateur attendu
par chacun est sur sa page de registre.

`batlehub-cli registry suggest --client-env` lit les manifestes d'un projet et
affiche les registres et les réglages client correspondants — voir la
[référence de la CLI](/fr/use/cli#registry-suggest).

## 5. Arrêter Nexus

Nexus peut partir une fois que :

- aucun registre BatleHub ne cite d'URL Nexus dans `upstreams` ;
- le journal des requêtes de Nexus ne montre aucun client autre que BatleHub ;
- chaque dépôt hébergé a été recopié, ou délibérément laissé de côté.

Gardez un Nexus en lecture seule, ou une sauvegarde de ses blob stores, aussi
longtemps que votre politique de rétention le demande.
