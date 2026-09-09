---
# La référence de configuration du serveur : plus de 4 100 mots de surface TOML,
# passée au-dessus de la ligne quand les tailles de page de la console et sa
# politique de sécurité de contenu sont devenues des choses qu'un opérateur
# règle. `docs:structure` demande cette déclaration au-delà de 4 000 mots — pas
# un plafond, une phrase que quelqu'un a dû écrire (RFC 0005-bis §4.5).
reference: true
sourcePath: guide/admin-config.md
sourceHash: b5186be74a5d8ade
---

# Configuration du serveur

## Configuration {#configuration}

BatleHub lit un unique fichier TOML, `config.toml` dans le répertoire courant par
défaut. `--config /chemin/vers/config.toml` remplace ce chemin.

### Ordre de chargement

1. Le fichier TOML est lu sur le disque.
2. Les marqueurs `${VAR_NAME}` à l'intérieur des chaînes sont remplacés par la
   valeur de la variable d'environnement correspondante.
3. Le TOML obtenu est analysé.
4. Les surcharges nommées `PROXY_CACHE__*` sont appliquées par-dessus.
5. Les noms et types de registre sont validés.

### Injecter un secret avec `${VAR_NAME}` {#env-inline}

Écrivez `${VAR_NAME}` dans n'importe quelle chaîne du TOML. BatleHub remplace le
marqueur par la variable d'environnement nommée avant l'analyse. Cela fonctionne
sur **tous les champs** — secrets d'authentification, tokens amont, mots de
passe, et le reste.

::: danger Variable manquante = échec au démarrage
Si une variable référencée n'est pas définie, BatleHub s'arrête immédiatement
avec un message d'erreur clair qui la nomme. Il n'y a ni repli silencieux ni
défaut à la chaîne vide.
:::

**Le secret client OIDC :**

```toml
[[auth]]
type          = "oidc"
issuer_url    = "https://sso.example.com/application/o/batlehub/"
client_id     = "batlehub"
client_secret = "${OIDC_CLIENT_SECRET}"   # export OIDC_CLIENT_SECRET=...
redirect_uri  = "https://hub.example.com/api/v1/auth/oidc/callback"
```

**Les identifiants d'un registre amont :**

```toml
# Token bearer (PAT GitHub, token Gitea, token d'authentification npm)
[registries.upstream_auth]
type  = "bearer"
token = "${REGISTRY_TOKEN}"

# Authentification basic (Nexus, Artifactory)
[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "${REGISTRY_PASSWORD}"

# En-tête personnalisé (X-API-Key, etc.)
[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "${REGISTRY_API_KEY}"
```

**L'injection sous Kubernetes et Docker Compose :**

```yaml
# docker-compose.yml
services:
  batlehub:
    env_file: .env.secrets   # OIDC_CLIENT_SECRET=...
    volumes:
      - ./config.toml:/etc/batlehub/config.toml:ro
```

```yaml
# Deployment Kubernetes
env:
  - name: OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef:
        name: batlehub-secrets
        key: oidc-client-secret
```

Pour écrire littéralement une chaîne `${...}` (sans recherche de variable),
échappez le premier `$` :

```toml
# Stocke la chaîne littérale "${MY_VAR}" — aucune substitution :
some_field = "$${MY_VAR}"
```

### Surcharges nommées par variables d'environnement {#env-named}

Un ensemble fixe de champs de premier niveau peut aussi être remplacé par des
variables d'environnement nommées. Pratique pour ajuster des adresses
d'infrastructure (hôte, port, URL de base) dans un déploiement conteneurisé,
sans modifier le fichier de configuration.

| Variable | Champ de configuration |
|----------|-------------|
| `PROXY_CACHE__SERVER__PORT` | `server.port` |
| `PROXY_CACHE__SERVER__HOST` | `server.host` |
| `PROXY_CACHE__SERVER__STATIC_DIR` | `server.static_dir` |
| `PROXY_CACHE__DATABASE__URL` | `database.url` |
| `PROXY_CACHE__DATABASE__MAX_CONNECTIONS` | `database.max_connections` |
| `PROXY_CACHE__STORAGE__PATH` | `storage.path` (backend unique sur système de fichiers) |
| `PROXY_CACHE__STORAGE__BUCKET` | `storage.bucket` (backend S3 unique) |
| `PROXY_CACHE__STORAGE__REGION` | `storage.region` (backend S3 unique) |
| `PROXY_CACHE__STORAGE__ENDPOINT_URL` | `storage.endpoint_url` (backend S3 unique) |
| `PROXY_CACHE__OTEL__ENDPOINT` | `otel.endpoint` |
| `PROXY_CACHE__OTEL__SERVICE_NAME` | `otel.service_name` |

::: tip Lequel employer, et quand
Employez les **marqueurs `${VAR_NAME}`** pour les secrets (tokens
d'authentification, mots de passe, secrets client) — ils fonctionnent sur tous
les champs et gardent les identifiants entièrement hors du fichier TOML.

Employez les **variables `PROXY_CACHE__*`** pour les adresses d'infrastructure
(URL de base, chemin de stockage, hôte et port), dont la valeur n'est pas secrète
mais varie d'un environnement à l'autre.
:::

### Configuration de production minimale

```toml
[server]
host = "0.0.0.0"
port = 8080
static_dir = "/app/ui/dist"
cors_allowed_origins = ["https://batlehub.example.com"]

[database]
type = "postgresql"
url  = "${DATABASE_URL}"

[[auth]]
type = "token"

[[auth.tokens]]
value   = "${ADMIN_TOKEN}"
role    = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "/var/cache/batlehub"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

---

### La confiance envers les proxys {#trusted-proxies}

BatleHub se trouve derrière un reverse proxy dans la plupart des déploiements, et
trois en-têtes de ce proxy façonnent son comportement : `Forwarded` et
`X-Forwarded-Host` décident de l'hôte dans toute URL générée (et, avec le
[routage par hôte](/fr/guide/host-routing), de *quel registre* sert la requête),
`X-Forwarded-Proto` décide entre `http` et `https`, et `X-Forwarded-For` décide
de l'IP cliente à laquelle le middleware fail2ban impute les violations.

`[server].trusted_proxies` énonce quels pairs ont le droit de les poser :

```toml
[server]
# Plages CIDR (ou IP nues) des reverse proxys devant BatleHub.
trusted_proxies = ["10.42.0.0/16", "192.168.1.10"]
```

| Valeur | Comportement |
| --- | --- |
| absente | l'hôte et le schéma transmis sont crus de n'importe quel client, `X-Forwarded-For` est ignoré (le défaut préexistant) |
| `[]` | les en-têtes transmis sont entièrement ignorés — l'en-tête `Host` et la connexion décident |
| `[réseaux]` | honorés uniquement depuis les pairs situés dans ces préfixes |

Employez des plages CIDR plutôt que des IP exactes : un ingress Kubernetes se
trouve derrière un CIDR de pods qui change à chaque déploiement. Une adresse nue
est traitée comme un `/32` (`/128` en IPv6).

::: warning Obligatoire avec le routage par hôte
Dès que `[subdomain_routing]` est activé ou qu'un registre déclare `hosts`, une
liste absente est une erreur de démarrage — sans quoi le routage dépendrait d'un
en-tête à propos duquel le serveur n'a aucune position déclarée. Le message
d'erreur contient le TOML à coller.
:::

::: info `[ip_blocking].trusted_proxies` est déprécié
Il fonctionne toujours : quand `[server].trusted_proxies` est absent, c'est lui
qui est employé, et il gouverne alors l'hôte et le schéma transmis autant que
l'IP cliente — y compris pour satisfaire l'exigence ci-dessus. Quand les deux
sont définis, `[server]` gagne. Dans les deux cas, vous obtenez un
[avertissement de configuration](#config-warnings).
:::

### Les modes de registre {#registry-modes}

Tout registre tourne dans l'un de trois modes :

| Mode | Comportement |
|------|-----------|
| `proxy` | Le défaut. Transmet toutes les requêtes à l'amont ; la publication est refusée. |
| `local` | BatleHub est la seule source. Aucun amont nécessaire. Les équipes publient directement. |
| `hybrid` | Le local d'abord. Sert les paquets publiés localement ; se rabat sur l'amont pour tout le reste. |

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"         # ou "hybrid"

[registries.rbac]
user  = ["source:read"]
admin = ["*"]
```

---

### La capture des README {#readme-capture}

Tout registre qui a une notion de *paquet comme chose à propos de laquelle on
lit* porte un README, et la plupart en portent un différent par version.
BatleHub le stocke indexé par `(registre, nom, version)` et le rend — nettoyé,
côté serveur — sur la page du paquet et par `batlehub package readme`.

**Absent veut dire actif.** Pour les types de registre dont le README voyage dans
les métadonnées, le texte est un champ d'un document que le proxy récupère et
analyse déjà : le défaut coûte donc un champ désérialisé. Quels types, et d'où
vient le README de chacun, est dans la
[table de couverture des README](/fr/registries/#readmes).

```toml
[registries.readme]
enabled         = true      # stocker et servir les README de ce registre
from_archive    = true      # extraire de l'artefact en cache quand les métadonnées n'en portent pas
max_bytes       = 262144    # plafond sur la source stockée (256 Kio) ; au-delà, tronqué et signalé
remote_images   = "strip"   # "strip" | "proxy"
remote_image_hosts = []     # sous "proxy" : hôtes autorisés ; [] signifie tous
image_max_bytes = 2097152   # plafond d'une image proxifiée (2 Mio) ; au-delà, non servie
```

- **`from_archive`** est la seule partie du défaut qui ne soit pas gratuite. Elle
  s'appuie sur la lecture d'artefact que le SBOM effectue déjà quand il est
  actif, et ajoute une lecture de stockage par version nouvellement mise en cache
  quand il ne l'est pas. Elle est inerte sur un registre dont le README ne
  voyage que dans les métadonnées, et sur un registre `firewall_only` — qui
  diffuse sans mise en tampon, de sorte qu'aucun artefact n'est mis en cache d'où
  extraire. Les deux cas sont signalés plutôt que rejetés.
- **`max_bytes`** plafonne la *source stockée*, après décompression. Une
  troncature est enregistrée et montrée au lecteur, jamais silencieuse. `0` avec
  `enabled = true` est refusé : cela ne stockerait rien tout en se déclarant
  actif.
- **`remote_images`** décide du sort d'une `<img>` pointant vers un hôte tiers.
  `"strip"` (le défaut) la remplace par une pastille en ligne portant le texte
  alternatif et l'hôte, de sorte que le lecteur voie qu'il y avait une image et
  vers où elle pointait. La rendre signifierait qu'à chaque consultation de page
  de la console, une requête part — avec un `Referer` — vers un hôte choisi par
  l'auteur du paquet, annonçant que quelqu'un de votre réseau est en train de
  lire à propos de ce paquet. Il n'y a délibérément **pas de `"allow"`** : la
  CSP de la console est inscrite dans le document et le serveur ne peut jamais
  que la *restreindre* (voir [la politique de la console](#csp)), donc ce réglage
  ne pourrait produire que des images cassées sans erreur nulle part.

  `"proxy"` rend les images, récupérées par **ce serveur** et servies depuis
  cette origine : le navigateur du lecteur ne parle donc toujours pas à un hôte
  choisi par l'auteur du paquet. Ce que le panneau reçoit est une `<img>` dont le
  `src` revient ici, portant l'*index* de l'image dans le README de cette
  version plutôt que son URL :

  ```
  GET /api/v1/explore/packages/{registry}/{name}/{version}/readme-image/{n}
  ```

  **Aucun appelant ne fournit jamais d'URL.** Le serveur résout l'index contre le
  README stocké, et c'est ce qui empêche ce mécanisme d'être un proxy d'images
  ouvert vers tout ce qu'un auteur de paquet écrit — il n'y a pas de clé de
  signature à faire tourner ni de liste de CDN à maintenir. La récupération passe
  par le même garde-fou SSRF que les téléchargements d'artefacts, en validant
  chaque saut de redirection, et le type de la réponse doit figurer sur une
  courte liste d'autorisation. Une image impossible à obtenir — URL morte,
  mauvais type, au-delà du plafond — se rabat sur la pastille qu'aurait affichée
  `"strip"`, ce qui est une meilleure réponse qu'une icône d'image cassée.

  Le SVG est servi, et c'est le cas qui mérite d'être énoncé : les deux tiers des
  images des README réels sont en SVG, donc les refuser ferait rendre à ce
  réglage un tiers de rangée de badges. Chacune passe par une liste
  d'autorisation XML qui retire `<script>`, `<foreignObject>`, tous les
  gestionnaires `on*` et toute référence externe, **et** la réponse porte
  `Content-Security-Policy: default-src 'none'; …; sandbox`, ce qui arrête le
  script même pour un lecteur qui ouvre l'image dans un nouvel onglet. Chacun de
  ces deux contrôles suffirait seul.
- **`remote_image_hosts`** restreint `"proxy"` à un ensemble d'hôtes nommés.
  C'est le réglage intermédiaire entre une balise de traçage et un blanc : un
  README qui affiche des badges depuis `shields.io` et des captures d'écran
  depuis le domaine personnel de quelqu'un obtient les badges proxifiés et une
  pastille pour la capture, plutôt qu'un choix tout ou rien.

  ```toml
  [registries.readme]
  remote_images      = "proxy"
  remote_image_hosts = ["img.shields.io", "badgen.net", "codecov.io"]
  ```

  Une entrée correspond à l'hôte lui-même ou à n'importe quel sous-domaine, donc
  `shields.io` couvre `img.shields.io` — et ne couvre **pas** `notshields.io`,
  parce que le point est obligatoire. Le port et les informations d'utilisateur
  ne font pas partie de la comparaison. Tout ce qui ne correspond pas devient la
  même pastille que produit `"strip"`, de sorte que le lecteur voie tout de même
  qu'il y avait une image et vers où elle pointait.

  **Une liste vide ou absente signifie tous les hôtes**, ce que faisait `"proxy"`
  avant l'existence de ce réglage : ajouter une clé à votre configuration ne doit
  pas changer ce qu'une instance en fonctionnement sert déjà. Restreindre tient
  en une ligne, et c'est vérifié à deux endroits — au rendu de la page, puis à
  nouveau avant que ce serveur ne compose vers l'hôte — de sorte que retirer une
  entrée prend effet à la requête suivante plutôt qu'au prochain défaut de cache
  de rendu.

  Inerte sous `"strip"`, où rien n'est récupéré du tout.
- **`image_max_bytes`** plafonne **une image proxifiée**, séparément de
  `max_bytes`, qui plafonne le *texte* stocké. Ce ne sont pas le même nombre pour
  la même raison, et n'en partager qu'un ferait de la hausse de l'un une décision
  à propos de l'autre. La plus grosse image d'un relevé de 150 URL d'images de
  README réels pesait 1,6 Mo, contre ce défaut de 2 Mio : il est donc généreux
  plutôt que restrictif. `0` avec `remote_images = "proxy"` est refusé — cela ne
  servirait rien tout en prétendant rendre des images — et tout ce qui dépasse
  16 Mio est refusé aussi, parce que les octets sont tenus en mémoire pendant que
  leur type et leur taille sont vérifiés.

Le rendu est côté serveur, sur liste d'autorisation, et soumis au fuzzing. Une
version bloquée ne sert aucun README (`403`, avec le même motif que donne le
chemin de téléchargement) ; les versions retirées, dépréciées ou sorties des
listes servent le leur normalement, parce que retirer une recommandation n'est
pas retirer la documentation.

---

### La lecture de découverte de la console {#the-console-s-discovery-read}

Décide si la page d'un paquet a le droit d'interroger l'amont à propos d'un
paquet dont cette instance ne détient rien.

Sans elle, la recherche propre à la console — qui trouve des paquets et les
marque « pas encore passés par le proxy » — mène à une page qui dit *aucune
version pour l'instant*. Avec elle, la page liste les versions que l'amont
connaît, marque chacune **non détenue ici**, et montre le README là où le
protocole en porte un.

**Absent veut dire actif.** C'est inerte sur un registre en mode `local` (il n'y
a pas d'amont à interroger) et sur les types de registre auxquels on ne peut pas
poser de question sur un paquet ; les deux cas sont signalés plutôt que rejetés.

```toml
[registries.upstream_detail]
enabled           = true    # la console peut interroger l'amont sur un paquet dont on ne détient rien
max_versions      = 300     # plafond de versions purement amont renvoyées pour un paquet
negative_ttl_secs = 300     # combien de temps un « paquet inconnu » de l'amont est mémorisé
```

- **Il n'y a pas de TTL propre.** Le document atterrit dans le cache de
  métadonnées, sous la clé qu'emploie déjà le chemin proxy : il obéit donc aux
  `cache.metadata_ttl_secs` et `cache.serve_stale` de ce registre. Une seconde
  expiration, cadencée indépendamment pour les mêmes octets, est la façon dont
  deux caches finissent par diverger.
- **`max_versions`** borne la *réponse*, pas la récupération — le document est un
  document, quelle que soit sa taille. La page dit quand le plafond s'est
  appliqué.
- **`negative_ttl_secs`** mémorise un `404` de l'amont, de sorte qu'une mauvaise
  URL, une faute de frappe ou un robot ne transforme pas chaque rechargement en
  requête amont. Un *échec de connexion* n'est pas un fait à propos du paquet et
  n'est jamais mémorisé.

**Regarder un paquet n'est pas le télécharger.** La lecture récupère un document
de métadonnées et rien d'autre : pas d'artefact, pas de ligne
`package_statuses`, pas de compteur de téléchargement, pas de `last_accessed`,
pas de quota, pas d'entrée de stockage — et le paquet n'apparaît pas dans le
catalogue parce que quelqu'un l'a regardé. Ce qui sort de l'instance, et comment
le désactiver, est dans
[ce qui sort de cette instance](/fr/operations/egress#the-console-s-discovery-read).

---

### La longueur des listes que ce serveur remet {#per-page}

Les deux endpoints de navigation répondent par une **page**, et ces deux clés
disent la longueur d'une page.

```toml
[limits]
versions_per_page = 100     # les versions d'un paquet ; le défaut, de 1 à 1000
packages_per_page = 20      # le catalogue ; le défaut, de 1 à 1000
```

Chaque clé a deux lectures, délibérément : c'est ce qu'obtient un appelant qui ne
demande pas de `per_page`, **et** le maximum que tout appelant peut demander. Un
plafond et un défaut séparés seraient deux nombres capables de se contredire, et
la question qu'un opérateur se pose est une seule question : quelle quantité de
liste ce serveur va construire, tenir en mémoire et sérialiser pour une requête.

Ce sont **deux clés et non une** parce que les deux listes ne posent pas la même
question. Une ligne de catalogue est un nom et quelques compteurs, et 20 lignes
font un écran. Une ligne de version coûte une lecture de vulnérabilités et une
lecture de licence avant d'être sérialisée, et
`@babel/plugin-transform-runtime` a 169 versions. Un opérateur qui dimensionne
un écran ne devrait pas dimensionner une requête en même temps.

La console les traite différemment pour la même raison, et il vaut la peine de
savoir laquelle est laquelle :

| Liste | Ce que la console envoie | Pourquoi |
| --- | --- | --- |
| `GET …/explore/packages` (catalogue) | rien | Le catalogue *est* la liste : le nombre de l'opérateur est donc le bon. Une console qui demanderait le sien rendrait `packages_per_page` inerte sur le seul écran pour lequel il existe. |
| `GET …/explore/packages/{registry}/{name}` (versions) | `per_page=25` | La table des versions se trouve au-dessus d'un README sur une page de paquet ; 25, c'est le nombre de lignes qui y tiennent. `versions_per_page` est le plafond au-dessus. |

Dans les deux cas, la console dimensionne sa pagination d'après le `per_page` qui
revient, et non d'après celui qu'elle a demandé.

Une requête peut demander moins, et peut demander plus, auquel cas elle obtient
le nombre configuré plutôt qu'une erreur — la demande n'est pas illégitime, elle
dépasse simplement ce que ce serveur remet d'un coup. Ce qui a été appliqué
revient toujours dans la réponse, de sorte qu'un appelant pagine au lieu de rater
des lignes en silence :

```json
"versions_page": { "page": 0, "per_page": 100, "total": 169,
                   "unfiltered_total": 169, "prerelease_total": 12,
                   "hidden_prereleases": 0 }
```

(l'enveloppe du catalogue est plus plate — `total`, `page`, `per_page` à côté des
`items` — parce qu'il a une liste et aucun filtre à comptabiliser.)

Sur l'endpoint des versions, les autres paramètres restreignent la liste avant sa
pagination : `q=` filtre sur la chaîne de version, `prereleases=hide` retire les
pre-releases, `version=` en nomme une qui doit survivre à ce filtre et, quand
aucune `page` n'est demandée, choisit la page qui la contient.

`0` est refusé au démarrage pour l'une comme pour l'autre : cela répondrait à
tout appelant par une liste vide, et l'échec atterrirait sur une page plutôt que
chez l'opérateur.

::: warning Une réponse plus étroite qu'avant
Avant l'existence de `versions_per_page`, l'endpoint des versions renvoyait
**toutes** les versions qu'il pouvait assembler. Un client qui lit `versions` en
supposant avoir la liste entière n'en voit désormais au plus 100 que s'il ne
pagine pas ; ce sont les compteurs de `versions_page` qui lui disent qu'il y en a
davantage. Le catalogue n'est pas concerné — il a toujours paginé, et
`packages_per_page` ne fait que transformer son 20 en nombre d'opérateur plutôt
qu'en littéral.
:::

---

### Servir la console {#serving-the-console}

`[server].static_dir` pointe vers la SPA compilée, et le serveur la sert de trois
façons :

- **le document** (`/` et `/index.html`), avec sa politique restreinte à votre
  configuration — voir [plus bas](#csp) ;
- **les fichiers à côté**, directement depuis le disque ;
- **toutes les autres URL de la console** — `/packages/npm/chalk?version=4.0.2`,
  `/setup`, `/me/tokens` — avec ce même document, parce qu'une application à page
  unique a un document et beaucoup d'URL. Sans cela, un lien collé, un
  rechargement ou un signet répondaient `404`.

Ce repli est délibérément étroit, parce que la façon de se tromper est de remettre
la console à quelque chose qui n'est pas un navigateur. Il ne répond qu'aux
`GET`, et jamais pour :

| Pas la console | Pourquoi |
| --- | --- |
| `/api`, `/proxy`, `/scalar`, `/metrics`, `/healthz`, `/livez` | les chemins propres à ce serveur — tout protocole de registre vit sous `/proxy/{registry}/…`, et les chemins d'un hôte de registre sont réécrits dans cette forme avant le routage : la requête d'un gestionnaire de paquets ne peut donc pas atteindre le repli |
| `/assets/…`, `/fonts/…` | les répertoires propres au build : un asset au nom haché devenu périmé doit échouer en tant qu'asset, pas arriver sous forme de HTML qu'un navigateur essaierait ensuite d'exécuter comme du JavaScript |
| un nom pointé à la racine — `/favicon.ico`, `/logo.svg` | demandé par son nom ; s'il n'est pas là, il n'est pas là. La règle s'arrête à la racine, donc `/packages/npm/lodash.merge` reste un lien |

Si votre ingress réécrit déjà les chemins inconnus vers `index.html`, rien ne
change — ces requêtes n'atteignent jamais le repli.

---

### La politique de sécurité de contenu de la console {#csp}

Le document de la console porte sa propre `Content-Security-Policy`, dans un
`<meta http-equiv>` plutôt que dans un en-tête de réponse — le service de
fichiers statiques derrière lui ne peut pas porter d'en-tête propre, et les trois
choses que sert cette origine demandent trois politiques différentes. Les deux
autres sont envoyées en en-têtes :

| Chemin | Politique | Pourquoi |
| --- | --- | --- |
| `/proxy/**` | `default-src 'none'; sandbox` | les documents de protocole peuvent porter des chaînes contrôlées par un éditeur, et un document en bac à sable n'a accès ni à l'origine de la console ni aux tokens qu'elle y stocke |
| `/scalar` | `default-src 'none'`, `script-src 'self'`, `connect-src 'self'` | la référence d'API ne charge rien d'ailleurs que de ce serveur — voir ci-dessous |

#### La référence d'API n'émet aucune requête sortante {#scalar-self-hosted}

`/scalar` chargeait autrefois son bundle depuis un CDN public, sans version. Il
est désormais servi depuis cette origine, à partir de la sortie de build de la
console (`assets/scalar/standalone.js`). Trois conséquences, dont la dernière est
un changement de comportement :

- **Elle fonctionne sans sortie réseau.** La référence était une page blanche sur
  tout déploiement incapable d'atteindre Internet — c'est-à-dire la plupart des
  registres privés. La charger n'envoie plus les adresses IP de vos opérateurs
  nulle part, et ne dépend plus de la disponibilité d'un CDN.
- **Le bundle est dans `ui/pnpm-lock.yaml`**, donc `pnpm audit`, postmortem et le
  SBOM le couvrent tous. Ce code était de toute façon exécuté par votre
  navigateur ; il n'était simplement déclaré nulle part où un scanner pouvait le
  voir.
- **Elle exige `static_dir`.** Un serveur configuré sans les assets de la console
  n'a pas de bundle à servir : `/scalar` répond alors par une courte page qui le
  dit et explique comment corriger. Elle ne se rabat délibérément **pas** sur le
  CDN : cela réinstallerait discrètement le script tiers précisément sur les
  déploiements coupés du réseau, les moins capables de l'atteindre. Le document
  OpenAPI reste embarqué dans cette page dans les deux cas, donc un `curl` sur
  l'URL le donne toujours, tout comme `batlehub dump-spec`.

`connect-src 'self'` est délibéré, pas accidentel. Le bundle appelle
`api.scalar.com` au chargement ; ces URL y sont compilées et aucun réglage ne les
désactive : c'est la politique qui les arrête. Rien n'est perdu — la
spécification générée ne déclare aucun bloc `servers`, donc « Test Request » vise
ce serveur, celui que la page documente.

La politique est construite avec la console, et le serveur **la restreint à votre
configuration** quand il sert le document. Restreindre ne fait jamais que
*retirer* des sources ; rien dans un fichier de configuration ne peut en ajouter.
Aujourd'hui, une source est décidée ainsi :

| Source | Conservée quand |
| --- | --- |
| `https://badge.socket.dev` | au moins un registre a `[registries.feature_flags] socket_badge` actif — ce qui est le défaut, elle n'est donc retirée que si vous avez désactivé le badge partout |

C'est la différence entre ce que la page *peut* charger et ce qu'elle *charge* :
une instance ayant désactivé le badge partout livrait auparavant un document
annonçant une origine tierce qu'elle n'appellerait jamais. Désactiver le drapeau
la retire désormais aussi de la politique, au prochain chargement du document —
sans recompilation, et cela suit un rechargement à chaud.

Le document est servi avec `Cache-Control: no-cache` pour cette raison : il décrit
une instance dont la configuration peut changer sous lui. Les assets à côté ne
sont pas touchés et restent servis par le service de fichiers.

::: info Pourquoi le serveur ne peut pas l'élargir
Une politique qui pourrait croître depuis la configuration permettrait à un
fichier fautif d'ouvrir la console à une origine que le build n'a jamais
autorisée. La politique construite est le maximum et le serveur ne possède que la
soustraction — ce qui explique aussi qu'un déploiement dont l'`index.html`
précède ce comportement serve sa politique inchangée plutôt que d'échouer.
:::

---

### Récupérer une version depuis la console {#console-fetch}

Décide si un lecteur peut demander à cette instance de récupérer une version
depuis la page qui lui a appris son existence.

La lecture de découverte ci-dessus rend la page d'un paquet honnête sur ce
qu'elle détient : elle liste toutes les versions que l'amont connaît et marque
chacune **non détenue ici**. C'est un mur. Ceci est la porte — un bouton
**Récupérer cette version** sur ces lignes.

Le catalogue a la même porte. Une recherche qui trouve un paquet en amont le
liste en ligne `upstream`, et cette ligne propose un bouton nommant la version
renvoyée par la recherche : **Récupérer 4.17.21**. C'est le même endpoint, les
mêmes garde-fous et la même ligne d'audit que le bouton de la page du paquet ; un
seul interrupteur désactive les deux. Là où un registre ne le propose pas, le
listing n'affiche rien et la page du paquet en donne la raison.

```toml
[[registries]]
console_fetch = true   # défaut
```

**Cela n'admet rien de plus.** Le bouton exécute le même téléchargement que
lancerait un gestionnaire de paquets, sous l'identité de l'appelant, à travers
tous les garde-fous que ce téléchargement traverserait :

- les règles s'exécutent — RBAC, liste de blocage, garde-fou d'âge de
  publication, garde-fou de licence, `require_signed_release`, garde-fou de
  version. Un refus affiche le motif propre à la règle, la même chaîne qu'aurait
  donnée le téléchargement, de sorte que le simulateur RBAC de la console
  (`POST /api/v1/admin/access-check`) explique le même verdict ;
- la vérification d'intégrité s'exécute, `block_on_mismatch` compris. Des octets
  qui échouent à leur somme de contrôle annoncée ne sont pas stockés ;
- le quota est consommé là où un quota s'applique ;
- **l'événement d'accès est enregistré, avec l'appelant comme acteur.** C'est la
  différence avec une consultation de page, en une ligne : une consultation n'a
  pas d'acteur parce que personne n'a rien décidé. Une récupération en a un, et
  le journal d'audit le nomme ;
- l'extraction du SBOM et du README s'exécute, parce que l'artefact atterrit dans
  le stockage par le chemin ordinaire. Une version récupérée depuis la page gagne
  donc sa licence, son manifeste de dépendances et le README porté par son
  archive — et `non analysée` devient une vraie réponse dès le prochain passage
  du scanner.

L'interrupteur existe pour l'opérateur qui veut une console strictement en
lecture, ce qui est une posture légitime et pas une chose que le logiciel devrait
avoir à deviner. Il est inerte sur un registre en mode `local` : il n'y a pas
d'amont d'où récupérer.

Le bouton n'est **pas affiché** là où « récupérer cette version » n'a pas de sens
unique — l'artefact Maven est un ensemble de fichiers, un provider Terraform
demande un système et une architecture, une version PyPI est une sdist plus une
wheel par interpréteur et par plateforme, un artefact conda demande une
plateforme de canal et une chaîne de build — et la page dit pourquoi, plutôt que
d'afficher un bouton désactivé sans explication. Quels types sont concernés est
la colonne *Récupérable* de la
[table de couverture des README](/fr/registries/#readmes).

Voir [ce qui sort de cette instance](/fr/operations/egress#someone-presses-fetch).

---

### Chercher dans la prose des README {#search-readmes}

Décide si la recherche du catalogue peut correspondre à ce qu'un paquet **dit**
autant qu'à la façon dont il s'appelle.

Le champ de recherche correspond à des noms. Cela répond à *« avons-nous quelque
chose qui s'appelle `retry` »* et ne peut pas répondre à *« laquelle de nos
bibliothèques internes gère la temporisation exponentielle »* — qui est la
question avec laquelle un développeur arrive réellement, et celle à laquelle la
page d'un paquet interne est le seul endroit au monde à pouvoir répondre.

```toml
[search]
readmes     = false     # défaut : les noms seulement
text_config = "english" # la configuration de recherche plein texte de Postgres
```

- **Désactivé par défaut.** Contrairement à la *capture* des README, active par
  défaut parce qu'elle coûte un champ déjà analysé, ceci construit un index sur
  de la prose : une colonne `tsvector` générée et un index GIN sur
  `package_readmes`. Le coût est du stockage plus une amplification d'écriture à
  chaque capture. C'est à vous de le choisir.
- **`text_config`** est la configuration de recherche plein texte de Postgres
  avec laquelle l'index est construit. `english` est le défaut parce qu'il répond
  mesurablement mieux à la question pour laquelle cette fonctionnalité existe :
  un lecteur qui tape `retry` trouve un README qui dit `retrying`, et un lecteur
  qui tape `cache` trouve `caching`. `simple` ne trouve ni l'un ni l'autre. La
  racinisation abîme bien les identifiants — `axios` est stocké en `axio` — mais
  elle le fait *symétriquement*, la requête étant racinisée elle aussi : la
  correspondance tient donc.
- **Le changer reconstruit la colonne.** `to_tsvector` dans une colonne générée
  doit être immuable : la configuration est donc un littéral, et en changer
  supprime puis recrée la colonne, réécrivant l'index de chaque ligne. Le serveur
  le fait au démarrage et le dit dans les logs. Prenez la décision à
  l'installation plutôt que de l'ajuster ensuite.
- **`readmes = true` exige Postgres**, et le serveur refuse de démarrer sans lui.
  Échouer au démarrage vaut mieux qu'une recherche qui ne correspond
  silencieusement à rien.

Une fois active, l'endpoint de listing accepte `?q=…&in=name|readme|both` :

- `in` vaut `name` par défaut, ce qui est le comportement actuel octet pour
  octet ;
- **une correspondance de nom prime toujours sur une correspondance de prose.**
  Un paquet littéralement nommé `retry` passe devant un paquet qui parle de
  réessayer, si dense soit-il. C'est ce que veut dire un lecteur qui saisit un
  nom, et ce n'est pas un paramètre de réglage ;
- chaque résultat dit son `matched_in` — `name`, `readme` ou `both` — parce
  qu'une ligne dont le nom n'a rien à voir avec la requête et dont le README la
  mentionne en passant est un résultat *correct*, et un résultat inexplicable
  sans cette étiquette ;
- le `snippet` est du **texte brut** et est rendu comme du texte. Il n'atteint
  jamais le chemin de balisage qu'emploie le panneau README.

Désactivée, `in=readme` est acceptée et répond exactement comme `in=name`, et la
réponse dit `readme_search_enabled: false`, de sorte qu'un client peut distinguer
« aucun paquet ici ne dit cela » de « cette instance ne cherche pas dans la
prose ».

**Seuls les README stockés sont interrogeables**, c'est-à-dire les seules
versions que cette instance détient ou héberge. Un README dérivé à la volée pour
une version purement amont n'a pas de ligne, et en écrire une est ce que la
lecture de découverte refuse de faire.

---

### Les fournisseurs d'authentification {#auth}

Les fournisseurs d'authentification sont évalués dans l'ordre de déclaration. Le
premier qui reconnaît un identifiant gagne. Une requête sans identifiant
correspondant est traitée comme `anonymous`.

#### Les tokens statiques

```toml
[[auth]]
type = "token"

[[auth.tokens]]
value   = "ci-pipeline-token"
role    = "user"
user_id = "ci"
```

#### OIDC (Authentik, Keycloak, Dex, …)

```toml
[[auth]]
type          = "oidc"
issuer_url    = "https://sso.example.com/application/o/batlehub/"
client_id     = "batlehub"
client_secret = "${OIDC_CLIENT_SECRET}"   # injecté depuis l'environnement — ne versionnez jamais un secret
redirect_uri  = "https://batlehub.example.com/api/v1/auth/oidc/callback"
scopes        = ["openid", "profile", "email", "groups"]

user_id_claim = "preferred_username"
role_claim    = "groups"

[auth.role_mappings]
"authentik Admins" = "admin"
"proxy-users"      = "user"
```

#### Les comptes de service Kubernetes

```toml
[[auth]]
type = "kubernetes"
# api_server, ca_cert_path et token_path valent par défaut les valeurs intra-cluster

[auth.role_mappings]
"system:serviceaccount:prod:ci-deployer" = "admin"
"system:serviceaccounts:staging"         = "user"
```

#### Les tokens d'API générés par les utilisateurs

Un utilisateur authentifié (session OIDC) peut générer des tokens à courte durée
de vie depuis la console ou par l'API :

```sh
curl -X POST \
  -H "Authorization: Bearer <oidc-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-token", "expires_in_days": 30, "role": "user"}' \
  https://batlehub.example.com/api/v1/auth/tokens
```

La valeur brute du token est renvoyée **une seule fois** — enregistrez-la
immédiatement.

---

## Le rechargement à chaud {#hot-reload}

BatleHub sait recharger sa configuration à l'exécution — ajouter ou retirer des
registres, modifier des règles RBAC, changer des réglages de politique — sans
redémarrer le processus. Les requêtes en cours se terminent avec l'ancienne
configuration avant que la nouvelle ne prenne effet.

### Comment ça marche

1. Quand `config.toml` change sur le disque, le surveillant de fichiers intégré
   valide la nouvelle configuration, lance des sondes de connectivité vers les
   URL amont, et enregistre un **rechargement en attente** en mémoire.
2. Un administrateur examine le diff en attente sur la page d'administration
   **Rechargement de la configuration** (`/admin/config-reload`) et clique sur
   **Appliquer** — ou l'abandonne.
3. À défaut, l'endpoint `POST /api/v1/admin/config/reload` applique un
   rechargement immédiatement (charger, valider et appliquer atomiquement), ce
   qui est utile dans un pipeline de CI/CD.

```sh
# Rechargement immédiat (sans étape de confirmation)
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/reload

# Voir s'il existe un rechargement en attente, chargé par le surveillant
curl -s -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/pending

# Appliquer le rechargement en attente
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/pending/apply

# L'abandonner sans l'appliquer
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/config/pending
```

Un rechargement en attente expire au bout de **10 minutes** s'il n'est ni
appliqué ni abandonné.

### Ce qui se recharge à chaud

| Composant | Rechargeable à chaud |
|-----------|---------------|
| Liste des registres (ajout, retrait, modification) | ✅ |
| RBAC par registre (`anonymous`, `user`, `admin`, groupes) | ✅ |
| Règles par registre (garde-fou d'âge, refus de `latest`) | ✅ |
| Versionnage, signature et canal bêta par registre | ✅ |
| Limite de taille des artefacts | ✅ |
| Hôte et port du serveur | ❌ exige un redémarrage |
| URL de la base | ❌ exige un redémarrage |
| Fournisseurs d'authentification | ❌ exige un redémarrage |
| Backends de stockage | ❌ exige un redémarrage |

### Les avertissements de configuration {#config-warnings}

Certains états de configuration méritent d'être signalés à un opérateur sans
mériter un refus de démarrer — un nom de registre qui ne peut pas devenir une
étiquette DNS, une clé dépréciée masquée par une autre, un défaut de sécurité
permissif laissé en place. Ils sont journalisés au démarrage et à chaque
rechargement, **et** servis par un endpoint, pour être réellement vus :

```sh
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/config/warnings
```

```json
{
  "warnings": [
    {
      "code": "proxy-trust.unconfigured",
      "path": "server.trusted_proxies",
      "message": "no trusted-proxy list is configured, so Forwarded / X-Forwarded-Host / …"
    }
  ]
}
```

`code` est un identifiant stable, sur lequel une automatisation peut filtrer sans
risque ; `path` pointe vers l'emplacement fautif tel quel, pour que vous puissiez
le rechercher dans le TOML.

`POST /api/v1/admin/config/validate` et `/config/from-content` renvoient la même
forme, en ligne sous `warnings`, à propos de la configuration **candidate** — vous
les voyez donc avant d'appliquer un rechargement en attente plutôt qu'après. La
page d'administration du rechargement affiche les deux, les avertissements actifs
sous forme de liste que l'on peut écarter.

### La piste d'audit

Chaque rechargement, appliqué ou rejeté, est écrit dans la table
`config_changes` et visible dans l'historique des changements de la page
d'administration :

```sh
curl -s -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/config/changes?per_page=20"
```

### Désactiver le rechargement à chaud

Mettez `BATLEHUB_DISABLE_HOT_RELOAD=1` dans l'environnement du serveur pour
désactiver le surveillant de fichiers et faire répondre `503 Service
Unavailable` à tous les endpoints de rechargement. C'est recommandé quand
`config.toml` est monté en ConfigMap Kubernetes en lecture seule, où le fichier
ne changera pas à l'exécution.

```yaml
# env du Deployment Kubernetes
- name: BATLEHUB_DISABLE_HOT_RELOAD
  value: "1"
```

---

## Le bandeau global {#global-banner}

Un administrateur peut diffuser un court message à tous les visiteurs du site,
authentifiés ou non. Usages courants : fenêtres de maintenance, annonces de
rechargement en cours, communications de politique interne.

Le bandeau est automatiquement mis à « Configuration reload in progress… » quand
un rechargement à chaud démarre, et vidé à sa fin.

### Poser le bandeau

Depuis la page d'administration **Rechargement de la configuration**,
remplissez le message, choisissez un niveau (info, warning ou error), puis
cliquez sur **Set Banner**.

```sh
# Par l'API
curl -s -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"message":"Scheduled maintenance in 30 min","level":"warning"}' \
  http://localhost:8080/api/v1/admin/banner

# Le retirer
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/banner
```

Le frontend interroge `GET /api/v1/banner` toutes les 30 secondes (sans
authentification) et affiche le bandeau en barre que l'on peut écarter, en haut
de chaque page.

### La propagation du bandeau en haute disponibilité

Le backend du bandeau est choisi dans le même pool que le cache de métadonnées :

| `[cache] type` | Stockage du bandeau |
|----------------|---------------|
| `"memory"` (défaut) | Dans le processus — non partagé entre réplicas |
| `"redis"` | Clé Redis `batlehub:system:banner` — partagée par tous les réplicas |
| `"postgres"` | Table `system_kv` — partagée par tous les réplicas |

Dans un déploiement en haute disponibilité, employez `"redis"` ou `"postgres"`
pour que tous les réplicas affichent le même bandeau, quelle que soit l'instance
que le client atteint.
