---
sourcePath: registries/mise-java.md
sourceHash: 71bb82a02db87494
---

# Java (mise)

Fait proxy et cache des builds de JVM qu'installe le `core:java` de mise, comme
registre *typé*, de sorte qu'un JDK puisse être **bloqué**, soumis à un délai
d'âge et servi coupé du réseau, et pas seulement mis en cache. mise résout
chaque version de Java à travers un document de métadonnées par type de
publication, système et architecture sur `mise-java.jdx.dev`, puis télécharge
l'URL de la ligne qu'il a choisie sur l'un des onze hôtes de fournisseurs. Ce
registre sert les deux : le document, filtré, et l'archive de chaque ligne,
récupérée chez son fournisseur et vérifiée contre la somme de contrôle de la
ligne avant d'être mise en cache.

## En un coup d'œil

| Propriété | Valeur |
|---|---|
| **Type de configuration** | `mise-java` |
| **Amont par défaut** | `https://mise-java.jdx.dev` pour les documents ; chaque archive depuis l'hôte du fournisseur de sa ligne |
| **Modes** | proxy seul |
| **Adressage** | un paquet (`java`), une version par build, écrite comme mise la nomme (`temurin-21.0.5+11.0.LTS`, `temurin-jre-21.0.5+11.0.LTS`, `zulu-21.38.21.0`) |
| **Publication privée** | proxy seul |
| **Coupure réseau** | archives détenues servies par leur URL de fournisseur ; document de métadonnées composé à partir des lignes des archives détenues, par plateforme. Une installation à partir du fichier de verrouillage n'a besoin de rien d'autre |

## Mise en place du proxy

mise lit `https://mise-java.jdx.dev/jvm/{type}/{os}/{arch}.json` et télécharge
l'`url` de la ligne choisie — ou, avec un `mise.lock`, l'URL que le fichier de
verrouillage a enregistrée, sans lire le document du tout. Les URL du document
sont servies **inchangées**, parce que `mise.lock` les enregistre ; la
réécriture demande donc une règle pour l'hôte de métadonnées et une par préfixe
de fournisseur. Remplacez `<registry>` par le nom de registre que vous avez
configuré :

```toml
# ~/.config/mise/config.toml
[settings.url_replacements]
"regex:^https://mise-java\\.jdx\\.dev/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/$1"
"regex:^https://aka\\.ms/download-jdk/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/aka.ms/download-jdk/$1"
"regex:^https://cache-redirector\\.jetbrains\\.com/intellij-jbr/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/cache-redirector.jetbrains.com/intellij-jbr/$1"
"regex:^https://cdn\\.azul\\.com/zulu/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/cdn.azul.com/zulu/$1"
"regex:^https://cdn\\.azul\\.com/zulu-embedded/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/cdn.azul.com/zulu-embedded/$1"
"regex:^https://corretto\\.aws/downloads/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/corretto.aws/downloads/$1"
"regex:^https://d3pxv6yz143wms\\.cloudfront\\.net/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/d3pxv6yz143wms.cloudfront.net/$1"
"regex:^https://developers\\.redhat\\.com/content-gateway/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/developers.redhat.com/content-gateway/$1"
"regex:^https://download\\.bell-sw\\.com/vm/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.bell-sw.com/vm/$1"
"regex:^https://download\\.java\\.net/java/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.java.net/java/$1"
"regex:^https://download\\.oracle\\.com/graalvm/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.oracle.com/graalvm/$1"
"regex:^https://download\\.oracle\\.com/java/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/download.oracle.com/java/$1"
"regex:^https://gds\\.oracle\\.com/download/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/gds.oracle.com/download/$1"
"regex:^https://github\\.com/SAP/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/SAP/$1"
"regex:^https://github\\.com/Tencent/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/Tencent/$1"
"regex:^https://github\\.com/TravaOpenJDK/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/TravaOpenJDK/$1"
"regex:^https://github\\.com/adoptium/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/adoptium/$1"
"regex:^https://github\\.com/bell-sw/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/bell-sw/$1"
"regex:^https://github\\.com/dragonwell-project/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/dragonwell-project/$1"
"regex:^https://github\\.com/graalvm/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/graalvm/$1"
"regex:^https://github\\.com/ibmruntimes/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-java/dl/github.com/ibmruntimes/$1"
```

`batlehub-cli setup apply --only mise` écrit le bloc entier à partir des
`vendor_hosts` du registre. **Ces règles doivent précéder celles de tout
registre `github`** : mise applique la première règle qui correspond, et un JDK
de `github.com/adoptium/` envoyé au type `github` est mis en cache mais jamais
bloqué. `setup` les ordonne ainsi ; un bloc écrit à la main doit le faire aussi.

## Authentification

mise envoie l'entrée `~/.netrc` de l'hôte vers lequel une règle a réécrit
l'URL, pour le document comme pour l'archive ; une instance authentifiée
demande donc une entrée pour l'hôte du proxy :

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Le document est un listing (`releases:list`), une archive une lecture
(`releases:read`). Le bloc de registre de votre administrateur :

```toml
[[registries]]
name = "<registry>"
type = "mise-java"
mode = "proxy"                              # le seul mode : pas de protocole de publication
# upstreams = ["https://mise-java.jdx.dev"]  # la valeur par défaut
# release_types = ["ga"]                     # la valeur par défaut ; « ea » est à activer
# vendor_hosts / redirect_hosts : par défaut, l'ensemble mesuré

[registries.rbac]
anonymous = ["releases:read", "releases:list"]
```

## Hôtes de fournisseurs et redirections

Une ligne n'est servie, et son archive récupérée, que si son URL est en
`https://` et se trouve sous l'une des entrées de `vendor_hosts`. Une entrée
s'écrit `hôte/préfixe/` — un préfixe de chemin terminé par `/` — ou un hôte nu
quand l'hôte appartient à un seul fournisseur. Un `github.com` nu est refusé au
chargement : il admettrait les fichiers de publication de n'importe quel
compte, si bien que les entrées GitHub nomment le propriétaire. La valeur par
défaut est l'ensemble mesuré sur tous les documents le 2026-11-14 :

```toml
vendor_hosts = [
  "aka.ms/download-jdk/", "cache-redirector.jetbrains.com/intellij-jbr/",
  "cdn.azul.com/zulu/", "cdn.azul.com/zulu-embedded/", "corretto.aws/downloads/",
  "d3pxv6yz143wms.cloudfront.net/", "developers.redhat.com/content-gateway/",
  "download.bell-sw.com/vm/", "download.java.net/java/", "download.oracle.com/graalvm/",
  "download.oracle.com/java/", "gds.oracle.com/download/",
  "github.com/SAP/", "github.com/Tencent/", "github.com/TravaOpenJDK/", "github.com/adoptium/",
  "github.com/bell-sw/", "github.com/dragonwell-project/", "github.com/graalvm/",
  "github.com/ibmruntimes/",
]
```

Cinq d'entre eux redirigent vers un autre hôte. Chaque saut d'un
téléchargement doit être en `https://`, aboutir sur `vendor_hosts` ou
`redirect_hosts` et se résoudre vers une adresse publique ; tout le reste
donne un `502`, et rien n'est mis en cache. La valeur par défaut de
`redirect_hosts` est l'ensemble mesuré : `download.visualstudio.microsoft.com`
(depuis `aka.ms`), `d2xrhe97vsfxuc.cloudfront.net` (depuis
`cache-redirector.jetbrains.com`), `objectstorage.uk-london-1.oraclecloud.com`
(depuis `gds.oracle.com`), `access.cdn.redhat.com` (depuis
`developers.redhat.com`) et `release-assets.githubusercontent.com` (depuis
`github.com`).

Une ligne dont l'hôte ne figure dans aucune des deux listes est retirée du
document servi — mise répond alors `no metadata found for version …`, comme
pour une version qui n'a jamais existé — et elle est comptée dans
`batlehub_mise_java_rows_dropped_total{reason="host"}` et journalisée en `WARN`
avec son hôte.

## Versions bloquées

Un blocage s'écrit sur la coordonnée par laquelle mise désigne le build :
`java@temurin-21.0.5+11.0.LTS`. Le JRE de la même publication a sa propre
coordonnée (`temurin-jre-21.0.5+11.0.LTS`) ; les builds glibc et musl d'une
publication en partagent une seule. Un build bloqué quitte tous les documents
de métadonnées que sert ce registre, si bien que
`mise install java@temurin-21.0.5` échoue avec le message de mise lui-même,
`no metadata found for version temurin-21.0.5 on linux-x64`, sans jamais
demander l'archive. Un fichier de verrouillage qui épingle l'URL de l'archive
saute le document, et la route de téléchargement refuse le même build par un
`403` portant le motif du blocage : l'URL est résolue vers sa ligne, et c'est
la coordonnée de la ligne que le blocage contrôle.

Un `release_age_gate` lit le `created_at` de chaque ligne : un build plus
récent que le délai quitte le document et son téléchargement est refusé. Toutes
les lignes sont datées, aussi le délai doit-il dire ce qu'est une coordonnée
sans date (`deny_missing_timestamp`) ; elle ne l'est que lorsqu'aucun document
ne liste plus sa ligne.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
et [quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered).

## Notes

- **L'URL du client est une clé de recherche, jamais une cible.**
  `dl/{host}/{path}` est résolu vers la ligne d'un document que ce registre a
  récupéré sur son propre amont ; une URL qu'aucun document ne liste reçoit un
  `404` sans aucune requête. L'index est propre au processus et parcouru en cas
  d'absence — une installation à partir du fichier de verrouillage demande
  l'archive sans lire de document, si bien que le premier téléchargement après
  un démarrage récupère le document qu'indique le nom du fichier. La coordonnée
  résolue est conservée dans le cache partagé, et une archive déjà détenue est
  servie par sa clé même après le retrait de sa ligne.
- **Les sommes de contrôle faibles sont conservées.** 30 % des lignes portent
  une somme `sha1:` (tous les builds Liberica), une somme `md5:` ou aucune. Les
  lignes SHA-256 et SHA-512 sont vérifiées à l'écriture en cache ; les lignes
  SHA-1 le sont avec SHA-1 ; les lignes `md5:` et sans somme sont mises en cache
  sans vérification. Chacun de ces remplissages est compté dans
  `batlehub_mise_java_weak_downloads_total{checksum}` et journalisé en `INFO`
  avec la coordonnée. mise vérifie l'archive contre la même ligne, et un
  `mise.lock` versionné épingle la somme qu'il a vue en premier.
- **Un catalogue qui rétrécit de façon invraisemblable est refusé.** Un
  document illisible, ou qui compte plus d'un quart de lignes de moins que le
  dernier bon, n'est pas servi : c'est le dernier bon qui l'est, avec un `WARN`
  et `batlehub_mise_java_rows_dropped_total{reason="implausible"}`.
- **Licences.** Le proxy met en cache les JDK des fournisseurs sous leurs
  propres licences ; savoir si les conditions d'Oracle permettent à un parc de
  redistribuer un build en interne relève de l'opérateur, comme pour
  [SDKMAN](/fr/registries/sdkman).

## Endpoints

::: details Afficher tous les endpoints

<!-- BEGIN endpoints: proxy/mise-java -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/mise-java/dl/{path}` | One archive, by its row's URL. |
| `GET` | `/proxy/{registry}/mise-java/jvm/{release_type}/{os}/{arch}.json` | One metadata document, filtered. |
<!-- END endpoints -->

:::

## Voir aussi

- [SDKMAN](/fr/registries/sdkman) — la JVM pour `sdk`, par sa propre API de candidats
- [Chaîne d'outils Go](/fr/registries/godist) et [Distributions Node](/fr/registries/nodedist) — la même conception pour les autres chaînes d'outils de mise
- [mise](/fr/use/mise) — chaque backend de mise et la route qu'il emprunte
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
