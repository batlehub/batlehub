---
# La référence de configuration : 11 955 mots de tables de champs, parce que la
# surface TOML est vaste. `docs:structure` demande cette ligne au-delà de 4 000
# mots — pas un plafond, une déclaration que quelqu'un a dû écrire
# (RFC 0005-bis §4.5).
reference: true
sourcePath: guide/configuration.md
sourceHash: a36d5858d4ed2552
---

# Référence de configuration

batlehub se configure par un unique fichier TOML. Ce document couvre chaque
option, la façon dont elles interagissent, et donne des exemples à copier pour
les scénarios de déploiement courants.

## 1. Démarrage rapide

Copiez ceci dans `config.toml`, démarrez PostgreSQL, et lancez le serveur :

```toml
[server]
port = 8080

[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
value = "my-admin-token"
role = "admin"
user_id = "admin"

[storage]
type = "filesystem"
path = "./cache"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user = ["releases:read", "source:read"]
admin = ["*"]
```

```sh
batlehub --config config.toml
```

Vérifiez que le serveur tourne :

```sh
curl http://localhost:8080/api/openapi.json
```

Une requête authentifiée emploie un token Bearer :

```sh
curl -H "Authorization: Bearer my-admin-token" http://localhost:8080/...
```

---

## 2. Comment fonctionne la configuration

### Ordre de chargement

1. Chaque fichier TOML passé à `--config` est analysé, dans l'ordre, et fusionné
   en un seul document (par défaut : le `config.toml` du répertoire courant).
   Voir [Configuration en couches](#layered-config-files) ci-dessous.
2. Les variables d'environnement de la forme `PROXY_CACHE__<SECTION>__<FIELD>`
   sont appliquées par-dessus les valeurs du fichier.
3. La configuration est validée : `config_version` (s'il est défini) ne doit pas
   dépasser ce que ce binaire prend en charge, les noms de registre ne doivent
   pas être vides, et les types de registre doivent s'analyser comme l'un de ceux
   de la ligne `type` de [la table des registres](#_3-5-registries) plus bas.

### Configuration en couches {#layered-config-files}

`--config` est répétable. Chaque fichier supplémentaire est une **couche**
fusionnée par-dessus les précédentes : un déploiement peut ainsi garder ses
identifiants dans un fichier au cycle de vie distinct du reste de sa
configuration — un Secret Kubernetes à côté d'une ConfigMap, un fichier `0600` à
côté d'un fichier lisible — sans renoncer au rechargement à chaud de l'un ou de
l'autre.

```sh
batlehub --config /etc/batlehub/config.toml --config /etc/batlehub/credentials.toml
```

Là où seules des variables d'environnement sont disponibles,
`BATLEHUB_CONFIG` accepte la même liste séparée par des `:`, dans le même
ordre :

```sh
BATLEHUB_CONFIG=/etc/batlehub/config.toml:/etc/batlehub/credentials.toml batlehub
```

#### Les règles de fusion

Les couches ultérieures gagnent. La fusion a lieu sur les documents TOML, avant
la désérialisation, de sorte qu'une couche ultérieure peut compléter une table
qu'une couche antérieure a ouverte.

| Forme | Règle |
| --- | --- |
| Table sur table | Fusionnée clé par clé. Une couche ultérieure ajoute des clés sans effacer celles qu'elle ne mentionne pas. |
| Tableau de tables, indexable des deux côtés | Fusionné entrée par entrée sur `name`, ou sur `type` en l'absence de `name`. Une entrée absente de la base est ajoutée. |
| Tout le reste | La couche ultérieure remplace purement et simplement la précédente. |

C'est cette fusion par clé qui permet à une couche d'identifiants de compléter un
registre sur vingt sans redéclarer les dix-neuf autres :

```toml
# config.toml
[[registries]]
name = "npm-priv"
type = "npm"
upstreams = ["https://npm.acme.io"]

[[registries]]
name = "crates"
type = "cargo"
```

```toml
# credentials.toml
[[registries]]
name = "npm-priv"

[registries.upstream_auth]
type = "bearer"
token = "s3cr3t"
```

Le résultat est deux registres, et `npm-priv` garde à la fois son amont et son
token.

Trois détails décident du reste :

- **L'indexation exige que la clé soit unique des deux côtés.** Deux entrées
  `[[auth]]` qui portent toutes deux `type = "token"` et aucun `name` n'ont pas
  d'unique correspondante dans l'autre couche : le tableau ultérieur remplace
  donc le précédent, plutôt que de laisser la fusion deviner quelle entrée va
  avec laquelle.
- **Les tableaux de scalaires sont remplacés, jamais complétés.** Redéclarer
  `upstreams` dans une couche ultérieure fixe la liste ; cela ne l'agrandit pas.
- **Un tableau vide efface la liste.** `registries = []` dans une couche
  ultérieure veut dire ce qu'il dit.

Les marqueurs d'environnement sont développés couche par couche, avant la fusion,
de sorte qu'un `${VAR}` est résolu dans le fichier qui l'a écrit. La validation
tourne une fois, sur le document fusionné — une couche incomplète à elle seule
est le cas normal.

#### Le rechargement à chaud entre les couches

Toutes les couches sont surveillées, et chaque rechargement les relit toutes.
Faire tourner un identifiant ne touche que le fichier d'identifiants, et cela
seul prépare un rechargement en attente ; voir
[Rechargement à chaud](/fr/guide/hot-reload).

L'éditeur de configuration de la console est l'exception, délibérément. Il lit et
réécrit **uniquement la première couche** : un fichier qui porte des identifiants
n'est donc jamais envoyé à un navigateur ni réécrit depuis un navigateur. Ce que
l'éditeur valide et compare reste le document fusionné, de sorte que son aperçu
décrit bien la configuration qui serait en vigueur.

### L'ordre d'évaluation de l'authentification

Le tableau `[[auth]]` est essayé dans l'ordre de déclaration. Le premier
fournisseur qui reconnaît un identifiant gagne, et la requête se poursuit sous
cette identité. Si aucun ne correspond, la requête est traitée comme
`anonymous`. Placer un fournisseur de tokens avant l'OIDC signifie que les tokens
statiques sont contrôlés en premier, ce qui est légèrement plus efficace.

### Le versionnage de la configuration

Un champ facultatif de premier niveau, `config_version`, fixe un fichier de
configuration à une version de schéma :

```toml
config_version = 1   # facultatif ; absent signifie « la version courante »
```

- **Son absence est toujours acceptée** et traitée comme la version de schéma
  courante du binaire : tout fichier de configuration existant continue donc de
  fonctionner à l'identique d'une mise à jour à l'autre.
- **Une valeur explicite plus récente que ce que gère le binaire en cours** fait
  échouer la validation au démarrage, avec un message de chemin de mise à jour,
  plutôt que d'ignorer silencieusement des champs qu'il ne comprend pas encore.
- **Une valeur explicite plus ancienne que la courante** est acceptée pour
  l'instant (il n'y a pas encore de moteur de migration) — ce champ existe pour
  qu'un futur changement de rupture ait où accrocher un contrôle de version, pas
  pour permettre un voyage vers l'ancien comportement aujourd'hui.

Ce qui exigera d'incrémenter `CURRENT_CONFIG_VERSION` (dans
`crates/config/src/schema/mod.rs`), le jour venu : retirer ou renommer un champ
existant, ou changer le sens du défaut d'un champ existant. Ce qui ne l'exige
**pas** : ajouter un nouveau champ facultatif (le cas courant de l'évolution de
ce code jusqu'ici — voir `CHANGELOG.md` pour les changements de chaque version).

---

## 3. Référence complète

### 3.1 `[server]`

Gouverne l'écoute HTTP et le service optionnel de la SPA.

```toml
[server]
host = "0.0.0.0"        # défaut
port = 8080             # défaut
# static_dir = "./ui/dist"  # facultatif : servir la SPA Vue compilée depuis ce chemin
# trusted_proxies = ["10.42.0.0/16"]   # voir « La confiance envers les proxys » plus bas
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `host` | chaîne | `"0.0.0.0"` | Adresse d'écoute |
| `port` | u16 | `8080` | Port TCP |
| `static_dir` | chaîne | — | Chemin de la SPA compilée ; quand il est défini, le serveur sert le frontend sur `/` |
| `cors_allowed_origins` | chaîne[] | *absent* → même origine uniquement | Origines autorisées à lire des réponses cross-origin. `["*"]` réautorise toute origine. Voir [CORS](#cors) |
| `cli_binary_path` | chaîne | — | Chemin de `batlehub-cli`, servi sur `GET /api/v1/cli/download` |
| `trusted_proxies` | chaîne[] | *absent* | Plages CIDR (ou IP nues) des reverse proxys dont les en-têtes `X-Forwarded-*` sont crus |
| `signed_urls` | table | *absent* | Matériel de signature des URL de téléchargement. Voir [`[server.signed_urls]`](#server-signed-urls) |
| `roles` | chaîne[] | `["proxy", "worker"]` | Ce que fait ce processus : `proxy` sert les requêtes et met les travaux d'analyse en file, `worker` les défile et les analyse. Le défaut est les deux (un worker embarqué). `batlehub --roles worker` le remplace pour un processus d'analyse seule. Voir [`[registries.security]`](#registries-security) et [`[worker]`](#scanners-and-worker). |

#### CORS

| `cors_allowed_origins` | Comportement |
|---|---|
| absent ou `[]` | Même origine uniquement — aucun en-tête CORS n'est émis |
| `["*"]` | Toute origine peut lire les réponses (renoncement explicite ; lève un avertissement `cors.any-origin`) |
| `["https://ui.example", …]` | Exactement ces origines |

La plupart des déploiements n'ont rien à faire ici. Le serveur héberge lui-même
la SPA quand `static_dir` est défini, et une requête de même origine ne consulte
jamais CORS — la console continue donc de fonctionner avec le champ non défini.
Ne le définissez que si la console est servie depuis une origine différente de
l'API.

> **Changé en 1.1.0 — rupture.** Une liste vide ou absente autorisait auparavant
> *toutes* les origines. N'importe quel site qu'un visiteur ouvrait pouvait alors
> émettre des requêtes cross-origin vers ce serveur et en lire les réponses. Les
> identifiants ne sont jamais envoyés en cross-origin : ce n'était donc pas une
> voie de vol de token — mais pour un proxy de registre dans un réseau privé,
> cela signifiait qu'une page publique pouvait énumérer des métadonnées de
> paquets internes en se servant du navigateur du visiteur comme position réseau.
>
> **À la mise à jour :** si votre console est servie depuis la même origine que
> l'API (le défaut, y compris pour tout déploiement par le chart Helm), il n'y a
> rien à faire. Si elle est servie depuis une autre origine, ajoutez-la
> explicitement :
>
> ```toml
> [server]
> cors_allowed_origins = ["https://ui.example.com"]
> ```
>
> Pour conserver le comportement d'avant la 1.1.0 à l'identique, mettez
> `cors_allowed_origins = ["*"]`. Le serveur démarrera et journalisera un
> avertissement `cors.any-origin`, visible sur
> `GET /api/v1/admin/config/warnings` et sur la page d'administration du
> rechargement.

#### La confiance envers les proxys

Trois en-têtes venus d'un reverse proxy façonnent le comportement de BatleHub, et
les trois sont posables par un attaquant quand le serveur est exposé
directement :

| En-tête | Décide |
|---|---|
| `Forwarded` / `X-Forwarded-Host` | l'hôte de toute URL générée — index de services NuGet, `dist.tarball` npm, pages simples PyPI, `dist` Composer, `download_url` Terraform — et, avec [`[subdomain_routing]`](#39-subdomain_routing-optional), **quel registre** sert la requête |
| `X-Forwarded-Proto` | `http` ou `https` dans ces URL |
| `X-Forwarded-For` | l'IP cliente à laquelle le middleware [`[ip_blocking]`](#36-ip_blocking-optional) impute les violations |

`trusted_proxies` énonce quels pairs peuvent les poser. Il a trois états
distincts :

| Valeur | Hôte et schéma | IP cliente |
|---|---|---|
| absent | les en-têtes transmis sont crus de **n'importe quel** pair | pair TCP (`X-Forwarded-For` ignoré) |
| `[]` | l'en-tête `Host` et la connexion seuls | pair TCP |
| `["10.42.0.0/16"]` | en-têtes transmis crus des pairs dans la plage, `Host` pour tous les autres | l'entrée `X-Forwarded-For` la plus à droite hors de la plage, depuis un pair dans la plage ; sinon le pair TCP |

**Employez des plages CIDR, pas des IP exactes.** Un ingress Kubernetes se trouve
derrière un CIDR de pods qui change à chaque déploiement : énumérer des adresses
est intenable. Une adresse nue est acceptée et traitée comme un `/32` (`/128` en
IPv6).

**Une absence est une erreur dure dès que le routage par hôte est configuré** —
router sur un en-tête à propos duquel le serveur n'a aucune position déclarée
n'est pas un état qu'un déploiement devrait atteindre. Pour tous les autres, une
absence conserve le comportement préexistant, parce que le durcir par défaut
changerait silencieusement les URL qu'annoncent les déploiements existants.
L'erreur de démarrage contient le TOML exact à coller.

> **Déprécié :** `[ip_blocking].trusted_proxies` fonctionne toujours. Quand
> `[server].trusted_proxies` est absent, c'est lui qui est employé, et il
> gouverne alors l'hôte et le schéma transmis autant que l'IP cliente — y compris
> pour satisfaire l'exigence de routage par hôte ci-dessus, de sorte qu'un
> déploiement existant peut adopter le routage par hôte sans toucher à sa
> configuration de confiance. Quand les deux sont définis, `[server]` gagne. Dans
> les deux cas, vous recevez un avertissement de configuration ; voir
> [`GET /api/v1/admin/config/warnings`](/fr/guide/hot-reload#_9-2-api-endpoints).
>
> Contrairement à `[server].trusted_proxies`, une entrée de la clé dépréciée qui
> n'est ni une IP ni une plage CIDR (un nom d'hôte, disons) est **retirée avec un
> avertissement** plutôt que refusée au démarrage — cette clé précède le
> validateur et rejetait silencieusement de telles entrées : en refuser une
> maintenant casserait une configuration qui n'a pas changé. Les entrées valides
> autour d'elle s'appliquent toujours.

#### `[server.signed_urls]` {#server-signed-urls}

Le matériel de signature des **URL de téléchargement signées** : un moyen de
garder un registre fermé aux appelants anonymes, même quand le client récupère
l'artefact sans identifiants. Terraform est le cas pour lequel cela existe — il
authentifie les deux documents JSON d'une installation de provider, puis récupère
l'archive, ses `SHA256SUMS` et la `.sig` sans en-tête `Authorization` et sans
mécanisme pour en envoyer un.

Absent, la fonctionnalité est indisponible. Elle est *globale* plutôt que
propre à chaque registre, parce que la clé est une propriété de l'instance ;
l'interrupteur qui s'en sert est
[`signed_downloads`](#registry-signed-downloads), sur chaque registre.

```toml
[server.signed_urls]
secret           = "${BATLEHUB_URL_SIGNING_SECRET}"
ttl_seconds      = 300
previous_secrets = ["${BATLEHUB_URL_SIGNING_SECRET_OLD}"]
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `secret` | chaîne | *obligatoire* | Clé de signature HMAC-SHA256, **32 octets minimum**. Interpolez-la depuis l'environnement (voir [Valeurs sensibles](#env-inline)) — une clé de signature n'a pas sa place dans un fichier versionné |
| `ttl_seconds` | u64 | `300` | Durée de vie d'une URL émise. Plafonné en dur à `3600` ; Terraform la suit en quelques millisecondes, la marge est donc pour un runner lent, pas pour un humain |
| `previous_secrets` | chaîne[] | `[]` | Vérifiés mais jamais employés pour émettre, de sorte qu'un secret puisse tourner sans jour de bascule. Une entrée qui s'interpole en chaîne vide est ignorée — mais la variable doit tout de même être **définie** (à `""`, c'est parfait) : l'expansion `${VAR}` a lieu avant l'analyse et refuse une variable non définie, donc laisser cette ligne après avoir retiré l'ancien secret fait échouer tout le chargement. Retirez la ligne, ou exportez la variable vide |

**Les erreurs de démarrage.** Chacune de celles-ci refuse de démarrer plutôt que
de se dégrader, parce que chacune produit un registre dont l'opérateur croit
qu'il est protégé :

| Condition | Pourquoi c'est fatal |
|---|---|
| Un registre met `signed_downloads = true` et ce bloc est absent | Le registre ne peut pas servir les téléchargements qu'il ferme |
| `secret` est vide | En général un `${VAR}` non défini dans cet environnement |
| `secret` fait moins de 32 octets | Trop court pour une clé HMAC-SHA256 |
| `ttl_seconds` vaut `0` | Toute URL émise naîtrait expirée |
| `ttl_seconds` dépasse `3600` | Une erreur de configuration ne doit pas émettre un identifiant valable un mois |
| Une entrée de `previous_secrets` fait moins de 32 octets | Une entrée courte est une erreur, pas une rotation en cours |

**Les avertissements** (non fatals, servis par
`GET /api/v1/admin/config/warnings`) :

| Code | Levé quand |
|---|---|
| `signed-urls.unused` | Un secret est configuré et aucun registre ne met `signed_downloads = true` : rien n'est signé |
| `signed-urls.anonymous-still-granted` | Un registre qui signe accorde encore la lecture anonyme — légal, et en général une migration arrêtée à mi-chemin, puisque la signature existe précisément pour pouvoir retirer cette autorisation |

::: warning Une URL émise est une capacité au porteur, et elle est dans vos logs
Jusqu'à son expiration, qui détient l'URL peut récupérer ce fichier-là sous
l'identité pour laquelle elle a été émise. BatleHub enregistre la cible complète
de la requête — chaîne de requête comprise — dans le champ `http.target` de son
span de requête, au niveau `INFO` : l'expédition des logs, l'export OTLP et tout
proxy terminant TLS devant BatleHub la capturent donc.

C'est borné par le TTL, par la coordonnée unique, et par le fait qu'aucune
permission que l'utilisateur signé n'avait déjà n'est accordée. Si cela ne suffit
pas pour votre parc : abaissez `ttl_seconds`, et retirez ou réécrivez
`http.target` dans votre pipeline de logs. La piste d'audit n'est pas
concernée — `access_events` enregistre la coordonnée du paquet, jamais l'URL.
:::

---

### 3.2 `[database]`

batlehub emploie PostgreSQL pour stocker les métadonnées de registre et les
tokens d'utilisateur.

```toml
[database]
type = "postgresql"
url = "postgresql://batlehub:changeme@localhost:5432/batlehub"
max_connections = 10    # défaut
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `type` | chaîne | — | Doit valoir `"postgresql"` |
| `url` | chaîne | — | DSN PostgreSQL complet, identifiants compris |
| `max_connections` | u32 | `10` | Taille du pool de connexions |

Le champ `url` se remplace à l'exécution par
`PROXY_CACHE__DATABASE__URL`, sans toucher au fichier de configuration.

---

### 3.2a `[cache]`

Choisit le backend de stockage des **entrées du cache de métadonnées** et des
**compteurs de limitation de débit**. Les deux sous-systèmes partagent ce
backend : un seul changement de configuration les affecte donc ensemble.

```toml
# Mémoire du processus (défaut — aucune infrastructure supplémentaire)
[cache]
type = "memory"

# PostgreSQL — persistant entre les redémarrages, partagé entre réplicas
[cache]
type = "postgres"

# Redis — persistant, partagé, éviction par TTL
[cache]
type = "redis"
url  = "redis://localhost:6379"
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `type` | chaîne | `"memory"` | `"memory"`, `"postgres"` ou `"redis"` |
| `url` | chaîne | — | URL de connexion Redis ; obligatoire quand `type = "redis"`. Format : `redis://[:<password>@]<host>[:<port>][/<db>]`, ou `rediss://…` pour TLS. |

#### Comparaison des backends

| Backend | Persistance | Partagé entre réplicas | Infra supplémentaire | Convient à |
|---------|:-----------:|:---------------------:|:-----------:|---------|
| `memory` | Non — remis à zéro au redémarrage | Non | Aucune | Développement local, mono-nœud |
| `postgres` | Oui | Oui | Aucune (emploie le `[database]` existant) | Production, multi-réplicas |
| `redis` | Oui | Oui | Un cluster Redis | Production à fort débit |

> **`memory` est le défaut** et ne demande aucun changement de configuration.
> Passez à `postgres` ou `redis` quand vous faites tourner plusieurs réplicas, ou
> quand vous voulez que les compteurs de limitation survivent aux redémarrages.

> **Le drapeau de fonctionnalité Redis :** le backend `redis` n'est compilé que
> si la fonctionnalité `cache-redis` est activée. L'image Docker officielle
> l'inclut. En compilant depuis les sources, passez `--features cache-redis` à
> `cargo build`.

#### Comment chaque backend est employé

**Le cache de métadonnées :** les listes de versions et les métadonnées de
release renvoyées par les registres amont sont stockées avec un TTL
(`metadata_ttl_secs`). Le backend de cache est consulté à chaque requête proxy,
avant d'atteindre l'amont.

**Les compteurs de limitation :** chaque appel d'incrément augmente atomiquement
un compteur indexé par `rl:{registry}:user:{user_id}` (ou
`rl:{registry}:group:{group}`) et renvoie le nouveau compte, plus l'horodatage de
réinitialisation de la fenêtre :
- `memory` — une HashMap protégée par un Mutex ; chaque processus a ses
  compteurs ;
- `postgres` — un `INSERT … ON CONFLICT DO UPDATE … RETURNING count`, entièrement
  sérialisable ;
- `redis` — un `INCR` atomique avec un `EXPIRE` conditionnel à la première
  écriture ; nettoyage par TTL.

---

### 3.3 `[[auth]]`

Un tableau de fournisseurs d'authentification, essayés dans l'ordre de
déclaration. Trois types sont pris en charge.

#### 3.3.1 Authentification par token (`type = "token"`)

Valide des tokens bearer statiques définis dans le fichier de configuration.
Utile pour les pipelines de CI/CD et les installations simples.

```toml
[[auth]]
type = "token"

[[auth.tokens]]
value = "my-ci-token"     # la valeur du token bearer (en clair ou empreinte Argon2id PHC)
role = "user"             # "admin", "user" ou "anonymous"
user_id = "ci-bot"        # facultatif : le nom affiché dans les logs

[[auth.tokens]]
value = "my-admin-token"
role = "admin"
user_id = "admin"
```

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `value` | chaîne | oui | La chaîne du token Bearer — en clair **ou** une empreinte Argon2id PHC (voir ci-dessous) |
| `role` | chaîne | oui | `"admin"`, `"user"` ou `"anonymous"` |
| `user_id` | chaîne | non | Employé dans les journaux d'audit |

#### Valeurs de token hachées en Argon2id (recommandé en production) {#argon2id-hashed-token-values-recommended-for-production}

Plutôt que de stocker un token brut dans le fichier de configuration, stockez une
**empreinte Argon2id au format PHC**. BatleHub fournit une commande qui la
produit depuis le token brut :

```sh
batlehub hash-token my-secret-token
# → $argon2id$v=19$m=65536,t=3,p=4$...
```

Copiez l'empreinte imprimée dans le champ `value` :

```toml
[[auth.tokens]]
value = "$argon2id$v=19$m=65536,t=3,p=4$..."
role  = "admin"
user_id = "admin"
```

BatleHub détecte automatiquement les valeurs au format PHC (celles qui commencent
par `$argon2`) et vérifie les tokens bearer entrants contre l'empreinte stockée.
Les valeurs en clair continuent de fonctionner sans changement — les deux formats
coexistent dans un même fichier.

> **Pourquoi cela compte :** si le fichier de configuration fuit (versionné par
> erreur, visible dans une ConfigMap Kubernetes), un token haché n'est pas
> directement utilisable par un attaquant. Le token brut n'a besoin d'exister que
> dans votre gestionnaire de secrets ou dans le presse-papiers du développeur.

#### 3.3.2 Authentification OIDC (`type = "oidc"`)

Valide les JWT Bearer émis par n'importe quel fournisseur OIDC conforme
(Authentik, Keycloak, Dex, etc.). Active éventuellement la connexion SSO par le
navigateur.

```toml
[[auth]]
type = "oidc"
# name = "oidc"           # défaut ; doit être unique si plusieurs fournisseurs OIDC tournent
issuer_url = "https://sso.example.com/application/o/batlehub/"
client_id = "batlehub"
# client_secret = "..."   # obligatoire pour un client confidentiel
# redirect_uri = "https://batlehub.example.com/api/v1/auth/oidc/callback"
# frontend_url = ""       # défaut : la même origine que le backend
scopes = ["openid", "profile", "email", "groups"]
user_id_claim = "preferred_username"   # défaut : "sub"
role_claim = "groups"                  # défaut : "role"

[auth.role_mappings]
"authentik Admins" = "admin"
"proxy-users"      = "user"
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `name` | chaîne | `"oidc"` | Nom du fournisseur ; devient le préfixe de groupe (par ex. `"oidc:team-a"`). Doit être unique entre fournisseurs. |
| `required` | booléen | `true` | Si un fournisseur d'identité injoignable au démarrage est fatal. Démarrer sans lui paraît sain et ne l'est pas : toute requête qui aurait porté une identité devient anonyme. Mettez `false` pour avertir et continuer, ce qui lève `batlehub_auth_provider_down`. |
| `issuer_url` | chaîne | — | URL de base du fournisseur OIDC ; `/.well-known/openid-configuration` y est ajouté pour la découverte des endpoints. Doit être en `https` (sauf sur localhost), et l'`issuer` que le document déclare doit y correspondre. |
| `client_id` | chaîne | — | Identifiant de client OAuth2 |
| `client_secret` | chaîne | — | Obligatoire pour un client confidentiel ; facultatif pour un client public |
| `redirect_uri` | chaîne | — | Quand il est défini, active le SSO navigateur sur `/api/v1/auth/oidc/callback` (fournisseur par défaut) ou `/api/v1/auth/oidc/{name}/callback` (fournisseurs nommés). Doit être enregistré auprès du fournisseur OIDC. |
| `frontend_url` | chaîne | `""` | Après un rappel SSO réussi, le navigateur est redirigé vers `{frontend_url}/#oidc_access_token=...`. Les tokens voyagent dans le **fragment** de l'URL : ils n'atteignent donc jamais le serveur qui héberge la SPA. À laisser vide en production (même origine). Mettez `http://localhost:5173` quand le serveur de développement Vite tourne à part. |
| `scopes` | chaîne[] | `["openid","profile","email"]` | Les scopes OAuth2 demandés |
| `audiences` | chaîne[] | `[client_id]` | Les valeurs pour lesquelles le claim `aud` du token est accepté. À définir explicitement quand le fournisseur émet des tokens pour une audience d'API distincte (l'`audience` d'Auth0, un serveur d'autorisation Okta). Jamais laissé sans contrôle. |
| `user_id_claim` | chaîne | `"sub"` | Le claim JWT employé comme identifiant d'utilisateur. `"preferred_username"` donne des noms lisibles avec Authentik et Keycloak. |
| `role_claim` | chaîne | `"role"` | Le claim JWT inspecté pour l'attribution de rôle. Peut être une chaîne ou un tableau de chaînes ; le rôle le plus élevé qui correspond gagne. |
| `role_mappings` | table | `{}` | Associe des valeurs de claim JWT aux rôles du proxy (`"admin"`, `"user"`, `"anonymous"`). Une valeur absente vaut `anonymous`. |

**L'espace de noms des groupes :** les valeurs de claim qui apparaissent comme
clés de `role_mappings` sont stockées telles quelles dans la liste des groupes de
l'identité. Celles qui n'y figurent pas sont préfixées par `{name}:` (par exemple
`"oidc:team-a"`). Cela permet à la table `groups` du RBAC d'employer
`"*:team-a"` comme joker inter-fournisseurs.

**Faire tourner plusieurs fournisseurs OIDC :** donnez un `name` unique à chacun.
Leurs URL de rappel seront `/api/v1/auth/oidc/{name}/callback`.

**La création de tokens est rapportée à ces fournisseurs.**
`POST /api/v1/auth/tokens` accepte une session de n'importe quel fournisseur
déclaré ici, quel que soit son `name` et qu'il ait ou non un `redirect_uri`.
Aucun autre identifiant ne peut émettre un token d'accès personnel : un token
statique, un compte de service Kubernetes, un job OIDC d'Actions ou un autre PAT
reçoivent tous un `403` — un identifiant machine ne peut donc jamais en émettre
un de plus longue durée. Sans fournisseur `type = "oidc"` configuré, personne ne
peut créer de PAT.

**Un token d'accès personnel porte un instantané des groupes de son créateur.**
Les groupes sont choisis à la création, restreints à ceux que le créateur détient
— en demander un qu'il n'a pas vaut `403` — et jamais re-résolus ensuite,
puisqu'un token n'a pas de session d'où se re-résoudre. Deux conséquences pour un
opérateur :

- **L'expiration d'un token est une durée de vie de contrôle d'accès, pas de
  l'hygiène.** L'appartenance aux groupes d'un token ne suit pas le fournisseur
  d'identité : un membre parti garde donc ce que le token porte jusqu'à son
  expiration ou sa révocation. L'expiration est obligatoire et plafonnée à 90
  jours, et **un départ doit s'accompagner d'une révocation de tokens** plutôt
  que de compter sur le changement chez le fournisseur. Pour les utilisateurs
  interactifs, une session OIDC re-résout les groupes à chaque rafraîchissement
  et reste la posture recommandée ; les tokens sont pour l'automatisation.
- **Un token nomme le groupe que le fournisseur émet réellement.** C'est la forme
  préfixée pour tout ce que `role_mappings` ci-dessus ne renomme pas —
  `k8s:system:serviceaccounts:digital` chez un fournisseur nommé `k8s`, et non
  `system:serviceaccounts:digital`. `batlehub auth whoami` les imprime tels que
  résolus, ce qui est la façon fiable d'en écrire un ; la console les propose sous
  forme de boutons pour la même raison.

Un token émis avant l'existence de ce mécanisme ne porte aucun groupe et n'en est
pas affecté : il voit `public` et `internal` et rien de ce qui est accordé à une
équipe, exactement comme avant. Un utilisateur qui a besoin d'un token atteignant
les paquets d'équipe en crée un nouveau.

#### 3.3.3 Authentification Kubernetes (`type = "kubernetes"`)

Valide les tokens de compte de service Kubernetes par l'API TokenReview de
Kubernetes. Tous les champs valent par défaut les secrets montés et les variables
d'environnement standard intra-cluster : la configuration nécessaire est donc
minimale quand on tourne dans un cluster.

```toml
[[auth]]
type = "kubernetes"
# name = "kubernetes"   # défaut

# Tous les champs suivants valent par défaut les valeurs intra-cluster :
# api_server   = "https://kubernetes.default.svc"
# ca_cert_path = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
# token_path   = "/var/run/secrets/kubernetes.io/serviceaccount/token"
# audiences    = ["batlehub"]
# issuers      = []     # n'importe quel émetteur ; voir plus bas

[auth.role_mappings]
"system:serviceaccount:prod:ci-deployer" = "admin"
"system:serviceaccounts:staging"         = "user"
"system:serviceaccounts"                 = "anonymous"
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `name` | chaîne | `"kubernetes"` | Nom du fournisseur ; devient le préfixe de groupe |
| `api_server` | chaîne | depuis les variables `KUBERNETES_SERVICE_HOST` et `KUBERNETES_SERVICE_PORT` | URL du serveur d'API Kubernetes. **Doit être en `https://`** — voir plus bas |
| `ca_cert_path` | chaîne | `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt` | Certificat d'autorité pour vérifier le TLS du serveur d'API |
| `token_path` | chaîne | `/var/run/secrets/kubernetes.io/serviceaccount/token` | Le token de compte de service de batlehub lui-même, pour les appels TokenReview ; relu à chaque requête pour gérer la rotation automatique |
| `audiences` | chaîne[] | `["batlehub"]` | Les audiences envoyées dans la requête TokenReview, **et exigées en retour dans sa réponse** — voir plus bas |
| `issuers` | chaîne[] | `[]` (tous) | Les émetteurs de token (`iss`) qui méritent un TokenReview. À définir quand ce serveur voit des tokens de plus d'un émetteur — voir plus bas |
| `role_mappings` | table | `{}` | Associe des noms d'utilisateur ou de groupe Kubernetes aux rôles du proxy |

**`api_server` doit être en `https://`, et le serveur refuse de démarrer sinon**
(le HTTP en clair n'est accepté que pour `localhost` et `127.0.0.1`, comme pour
un `issuer_url` OIDC). La règle est plus stricte qu'il n'y paraît : chaque
TokenReview porte le token de compte de service *de BatleHub lui-même*, et la
réponse décide de l'identité de l'appelant. Qui se trouve sur un chemin en clair
apprend ce token *et* peut répondre `authenticated: true` avec
`system:serviceaccount:…`, ce que `role_mappings` traduira en n'importe quel rôle
que cette clé nomme — jusqu'à `admin`. Laissez `api_server` non défini dans un
cluster, et le défaut est en `https://` par construction.

**Le `name` de chaque `[[auth]]` doit être unique, tous types confondus**, et le
serveur refuse de démarrer sur une collision. Le nom n'est pas une étiquette :
c'est ce à quoi une session, un refresh token OIDC stocké et un groupe non associé
(`"k8s-prod:team-a"`) sont attribués. Deux fournisseurs qui en partagent un sont
un seul fournisseur du point de vue de tout cela — un fournisseur
`type = "kubernetes"` nommé `"corp"` permettrait à un compte de service d'agir
sur les sessions et les tokens d'accès personnels du fournisseur OIDC nommé
`"corp"`.

**Les clés d'attribution de rôle :** Kubernetes pose
`username: "system:serviceaccount:<namespace>:<name>"` et
`groups: ["system:serviceaccounts", "system:serviceaccounts:<namespace>", ...]`.
Quand un token correspond à plusieurs clés, le rôle le plus élevé gagne.

**Le lien d'audience est appliqué dans les deux sens.** `audiences` est envoyé
comme `spec.audiences`, et la réponse du TokenReview n'est acceptée que si son
`status.audiences` en contient au moins une. Un token que le serveur d'API
authentifie mais dont il ne confirme pas le lien à l'une de ces audiences est
refusé, et le rejet est journalisé en `warn` avec les deux listes.

Cela compte parce que le token de compte de service par défaut monté dans chaque
pod du cluster est lié au serveur d'API, pas à BatleHub. Sans le contrôle côté
réponse, un authentificateur qui ignore `spec.audiences` laisserait n'importe
quel pod du cluster s'authentifier ici.

La charge de travail doit donc présenter un token **projeté**, émis pour cette
audience, et non celui monté par défaut :

```yaml
volumes:
  - name: batlehub-token
    projected:
      sources:
        - serviceAccountToken:
            path: token
            audience: batlehub        # doit correspondre à une entrée d'`audiences`
            expirationSeconds: 3600
```

Pointez le client vers `/var/run/secrets/batlehub/token`
(`batlehub-cli auth login --kubernetes-token-path`). Si l'authentification se met
à échouer avec `TokenReview authenticated a token the API server did not confirm
is bound to a requested audience` dans les logs, c'est que la charge de travail
envoie le token par défaut et a besoin du volume projeté ci-dessus.

**Seuls les identifiants qui pourraient être les nôtres sont envoyés au serveur
d'API.** Trois filtres tournent avant tout TokenReview, dans cet ordre :

- un token bearer qui n'est pas fait de trois parties séparées par des points ne
  peut pas être un token de compte de service : il est passé intact au
  fournisseur suivant — ce qui garde les tokens d'accès personnels hors des
  journaux de requêtes du plan de contrôle ;
- un JWT dont le claim `aud` ne partage rien avec `audiences` est refusé
  localement. C'est le même contrôle que subit `status.audiences` après
  l'aller-retour (ce champ est l'intersection de `spec.audiences` et du `aud` du
  token, donc un tel token ne pourrait jamais revenir confirmé), déplacé plus
  tôt. Cela compte parce qu'un token d'identité OIDC *a* la forme d'un JWT : avec
  `type = "kubernetes"` déclaré avant `type = "oidc"` — l'ordre naturel dans un
  cluster — le token d'identité de chaque requête de navigateur serait sinon posté
  tel quel au serveur d'API ;
- quand `issuers` est défini, un JWT venu de tout autre émetteur est refusé
  localement lui aussi. Laissez-le vide, sauf si ce serveur voit des tokens de
  plus d'un émetteur portant le même nom d'audience (clusters fédérés, un
  fournisseur OIDC de cloud à côté de celui du cluster). Lisez-le avec
  `kubectl get --raw /.well-known/openid-configuration | jq -r .issuer`.

Rien de tout cela n'accorde quoi que ce soit : les claims sont lus sans vérifier
la signature, et ne peuvent que faire renoncer BatleHub à *demander*. C'est le
verdict du TokenReview qui authentifie.

**Les verdicts sont mis en cache, dans les deux sens.** Un succès est réemployé
60 secondes, un rejet 10 — indexés par le SHA-256 du token, jamais par le token
lui-même. Sans le second, un client qui répète un identifiant que le cluster
refuse (un job de CI mal configuré, un token périmé dans une boucle) posait un
TokenReview par requête proxifiée sur le serveur d'API, sans plafond. Dix
secondes, c'est aussi le plus long qu'un compte de service attende après
l'arrivée de son RoleBinding.

#### 3.3.4 Authentification OIDC d'Actions (`type = "actions-oidc"`)

Valide les JWT OIDC éphémères qu'émettent GitHub Actions ou Forgejo Actions pour
les jobs de workflow (exige `id-token: write` dans les permissions du workflow).
Plutôt que d'associer une valeur de claim unique à un rôle, ce fournisseur évalue
une liste de **règles** — chacune apparie n'importe quelle combinaison de claims
JWT et accorde un nom de groupe et un rôle quand elle correspond.

```toml
[[auth]]
type = "actions-oidc"
name = "forgejo-action"                    # défaut : "actions-oidc"
issuer_url = "https://forgejo.example.com" # GitHub : "https://token.actions.githubusercontent.com"
audience = "https://batlehub.example.com"  # OBLIGATOIRE — voir plus bas
# user_id_claim = "sub"                    # défaut

  # Groupe statique : les déployeurs sur la branche principale
  [[auth.rules]]
  group = "ci-deployers"
  role  = "admin"
  match = "all"              # toutes les conditions doivent passer (défaut)
  [[auth.rules.conditions]]
  claim   = "repository_owner"
  pattern = "batleforc"
  [[auth.rules.conditions]]
  claim   = "ref"
  pattern = "refs/heads/main"

  # Groupe dynamique : chaque token reçoit un groupe automatique par dépôt et par branche
  # par ex. "forgejo-action/batleforc-batlehub/main"
  [[auth.rules]]
  group_template = "{name}/{repository}/{ref_name}"
  role           = "user"
  match          = "all"
  [[auth.rules.conditions]]
  claim   = "repository_owner"
  pattern = "batleforc"       # glob : correspondance exacte

  # Exemple avec regex : les publications par tag
  [[auth.rules]]
  group = "tag-releasers"
  role  = "user"
  match = "all"
  [[auth.rules.conditions]]
  claim      = "ref"
  pattern    = "^refs/tags/v[0-9]+"
  match_type = "regex"        # explicite ; de toute façon détecté depuis le "^"
```

**Les champs du fournisseur :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `name` | chaîne | `"actions-oidc"` | Nom du fournisseur. Apparaît dans les logs et dans `Identity.auth_provider`. Doit être unique parmi toutes les entrées `[[auth]]`. |
| `issuer_url` | chaîne | — | URL de base de l'émetteur OIDC. GitHub : `"https://token.actions.githubusercontent.com"`. Forgejo : l'URL de votre instance. Doit être en `https` (sauf sur localhost). |
| `required` | booléen | `false` | Si un émetteur injoignable au démarrage est fatal. Vaut `false` ici, contrairement à `type = "oidc"` : un fournisseur de CI en panne arrête la publication, pas la connexion. |
| `audience` | chaîne | — | **Obligatoire.** La valeur à laquelle le claim `aud` du token doit être égal. |
| `user_id_claim` | chaîne | `"sub"` | Le claim JWT employé comme `user_id` dans l'identité résolue. |
| `rules` | tableau | `[]` | Liste ordonnée de règles de groupe, évaluées contre chaque JWT. Toutes les règles qui correspondent contribuent — elles ne sont pas exclusives. |

**C'est `audience` qui rend ce fournisseur sûr, et il n'a pas de défaut.**
L'émetteur est partagé : `https://token.actions.githubusercontent.com` signe un
token pour *n'importe quel* workflow de *n'importe quel* dépôt sur GitHub, donc
valider `iss` prouve seulement que l'appelant est un job GitHub Actions, quelque
part. `aud` est le seul claim que le workflow appelant choisit : c'est donc lui
qui dit « ce token a été émis pour *ce* déploiement ». Le démarrage du serveur
échoue s'il est absent ou vide.

Choisissez quelque chose de propre au déploiement — son URL est le choix
conventionnel — et faites-le demander par les workflows :

```yaml
# GitHub Actions
- uses: actions/github-script@v7
  id: token
  with:
    script: return await core.getIDToken('https://batlehub.example.com')
```

Les `rules` ci-dessous décident toujours de ce que l'appelant a le droit de
*faire* ; `audience` décide s'il est entendu du tout. Un déploiement aux règles
lâches et sans contrôle d'audience était atteignable par n'importe quel dépôt de
la forge.

**Les champs d'une règle (`[[auth.rules]]`) :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `group` | chaîne | — | Le nom de groupe statique accordé quand la règle correspond. `group` ou `group_template` est obligatoire, au moins l'un des deux. |
| `group_template` | chaîne | — | Modèle pour un groupe au nom dynamique. Voir les variables de modèle plus bas. |
| `role` | chaîne | `"user"` | Le rôle qu'accorde cette règle (`"admin"`, `"user"`, `"anonymous"`). Le rôle final est le plus élevé parmi toutes les règles qui correspondent. |
| `match` | `"all"` \| `"any"` | `"all"` | Si toutes les conditions doivent passer (ET) ou au moins une (OU). |
| `conditions` | tableau | `[]` | Les conditions évaluées contre les claims du JWT. Une liste vide correspond toujours. |

**Les champs d'une condition (`[[auth.rules.conditions]]`) :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `claim` | chaîne | — | La clé de claim JWT à tester (par exemple `"repository"`, `"ref"`, `"environment"`, `"actor"`). |
| `pattern` | chaîne | — | Le motif auquel confronter la valeur du claim. |
| `match_type` | `"auto"` \| `"glob"` \| `"regex"` | `"auto"` | Le type de motif. `auto` traite le motif comme une regex s'il commence par `^`, se termine par `$`, ou contient `[`, `(` ou `+`. Sinon, comme un glob. |

**Les types de motif :**

- **Glob** — les jokers du shell : `myorg/*` correspond à `myorg/foo` mais pas à
  `other/foo`. `*` correspond à toute suite de caractères.
- **Regex** — toute la syntaxe de la crate `regex` : `^refs/tags/v[0-9]+`
  correspond à tout tag commençant par `v` suivi de chiffres. Une erreur de
  compilation interrompt le démarrage du fournisseur.

**Les variables de modèle de groupe :**

Un modèle est une chaîne à `{marqueurs}`, rendue à chaque requête. Dans les
valeurs substituées, les `/` sont remplacés par des `-` (pour que les noms de
groupe restent utilisables dans un chemin) ; un `/` littéral du modèle lui-même
est conservé.

| Variable | Valeur |
|----------|-------|
| `{name}` | Le champ `name` du fournisseur |
| `{ref_name}` | Le claim `ref`, préfixe `refs/heads/` ou `refs/tags/` retiré |
| `{<toute clé de claim>}` | La valeur de ce claim JWT, avec `/` → `-` |

Exemple : avec `name = "forgejo-action"`,
`repository = "batleforc/batlehub"`, `ref = "refs/heads/main"` :

```
"{name}/{repository}/{ref_name}"  →  "forgejo-action/batleforc-batlehub/main"
```

**Les claims d'un token OIDC GitHub Actions (sous-ensemble représentatif) :**

| Claim | Exemple de valeur | Description |
|-------|---------------|-------------|
| `sub` | `repo:org/repo:ref:refs/heads/main` | Le sujet (identifiant unique du token) |
| `repository` | `org/my-repo` | Le dépôt, au format `owner/name` |
| `repository_owner` | `org` | Le propriétaire du dépôt (utilisateur ou organisation) |
| `ref` | `refs/heads/main` | La ref Git complète |
| `ref_type` | `branch` ou `tag` | Le type de ref |
| `workflow` | `CI` | Le nom du workflow |
| `environment` | `production` | L'environnement de déploiement (s'il est défini) |
| `actor` | `alice` | Le nom d'utilisateur GitHub qui a déclenché l'exécution |
| `event_name` | `push` | L'événement déclencheur |
| `sha` | `abc123…` | Le SHA du commit |

Forgejo émet des tokens à la même structure de claims ; seule l'URL de l'émetteur
diffère.

**Accorder l'accès par le RBAC :**

Les groupes dynamiques permettent des autorisations avec jokers. Pour laisser
tous les tokens de CI des dépôts de `batleforc` lire les publications :

```toml
[registries.rbac.groups]
"forgejo-action/*" = ["releases:read"]

# Accorder à la CI d'un dépôt précis le droit de publier
"forgejo-action/batleforc-batlehub/*" = ["releases:read", "releases:publish"]
```

**Un extrait de workflow GitHub Actions :**

```yaml
jobs:
  publish:
    permissions:
      id-token: write   # nécessaire pour demander un token OIDC
      contents: read
    steps:
      - name: Push artifact
        env:
          BATLEHUB_TOKEN: ${{ secrets.BATLEHUB_TOKEN }}
        run: |
          # BatleHub valide le token OIDC ; aucun secret à longue durée de vie
          # n'est nécessaire avec actions-oidc — passez les variables
          # ACTIONS_ID_TOKEN_REQUEST_URL et ACTIONS_ID_TOKEN_REQUEST_TOKEN
          # à votre outil de publication
          cargo publish --registry batlehub
```

---

### 3.4 `[storage]`

Deux formats sont pris en charge : le backend unique (plus simple, accepte les
surcharges par variables d'environnement) et le multi-backend (qui permet un
routage par registre).

#### Backend unique

```toml
# Système de fichiers
[storage]
type = "filesystem"
path = "./cache"

# S3 (ou compatible S3 : MinIO, RustFS, etc.)
[storage]
type = "s3"
bucket = "my-artifacts"
region = "us-east-1"
prefix = "batlehub/"         # facultatif, aucun par défaut
endpoint_url = "http://minio:9000"  # facultatif : à omettre pour le vrai AWS
force_path_style = true         # facultatif : nécessaire pour MinIO et RustFS
```

**Les champs du système de fichiers :**

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `path` | chaîne | oui | Le répertoire des fichiers en cache ; créé s'il n'existe pas |

**Les champs S3 :**

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `bucket` | chaîne | oui | Le nom du bucket S3 |
| `region` | chaîne | oui | La région AWS (par exemple `"us-east-1"`) |
| `prefix` | chaîne | non | Le préfixe de clé de tous les objets stockés |
| `endpoint_url` | chaîne | non | Un endpoint personnalisé, pour un stockage compatible S3 |
| `force_path_style` | booléen | non | Nécessaire pour MinIO, RustFS et les autres stockages compatibles S3 qui emploient des URL par chemin |

Les identifiants S3 viennent de la chaîne d'identifiants standard du SDK AWS :
les variables `AWS_ACCESS_KEY_ID` et `AWS_SECRET_ACCESS_KEY`,
`~/.aws/credentials`, les métadonnées d'instance EC2 ou ECS, et ainsi de suite.

#### Multi-backend

Employez ceci quand des registres différents doivent stocker leurs artefacts dans
des backends différents.

```toml
[storage]
default = "primary"           # obligatoire : le nom du backend de repli

[[storage.backends]]
name = "primary"
type = "filesystem"
path = "./cache"

[[storage.backends]]
name = "s3-artifacts"
type = "s3"
bucket = "release-artifacts"
region = "eu-west-1"
```

Assignez ensuite un registre à un backend précis, par le champ `storage` :

```toml
[[registries]]
type = "github"
name = "github"
storage = "s3-artifacts"    # ce registre emploie s3-artifacts ; les autres, "primary"
```

> **Note :** les surcharges par variables d'environnement des champs de stockage
> (`PROXY_CACHE__STORAGE__PATH`, etc.) ne fonctionnent qu'avec la forme à backend
> unique. Une configuration multi-backend se modifie dans le fichier.

---

### 3.5 `[[registries]]` {#_3-5-registries}

Un tableau de proxys de registres de paquets. Chaque entrée configure un endpoint
de registre.

```toml
[[registries]]
type = "cargo"
name = "cargo"
# upstreams = ["https://crates.io"]   # défaut pour cargo
# index_url = "https://index.crates.io"  # défaut ; à définir pour un registre auto-hébergé
# storage = "backend-name"            # facultatif : employer un backend de stockage nommé

[registries.cache]
metadata_ttl_secs = 300     # défaut : 300 (5 minutes)
# artifact_ttl_secs = 2592000  # facultatif : re-récupérer les artefacts de plus de 30 jours

[registries.rbac]
anonymous = []
user = ["releases:read", "source:read"]
admin = ["*"]

[registries.rbac.groups]
"team-a" = ["releases:read", "source:read"]
"*:ops"  = ["*"]   # joker : le groupe "ops" de n'importe quel fournisseur

[[registries.rules]]
kind = "release_age_gate"
min_age_secs = 3600              # défaut : 3600 (1 heure)
bypass_roles = ["admin"]
deny_missing_timestamp = false   # true pour refuser les paquets sans horodatage

# [[registries.rules]]
# kind = "require_signed_release"
# enabled = true
```

**Les champs de premier niveau :**

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `type` | chaîne | oui | `"github"`, `"forgejo"`, `"gitlab"`, `"npm"`, `"cargo"`, `"nuget"`, `"openvsx"`, `"vscode-marketplace"`, `"goproxy"`, `"maven"`, `"terraform"`, `"rubygems"`, `"composer"`, `"pypi"`, `"conda"`, `"deb"`, `"rpm"`, `"pacman"`, `"jetbrains"`, `"jetbrains-marketplace"`, `"generic"`, `"nodedist"`, `"sdkman"` |
| `name` | chaîne | oui | Identifiant unique ; employé dans les chemins d'URL du proxy |
| `mode` | chaîne | non | `"proxy"` (défaut), `"local"` ou `"hybrid"`. Pris en charge par `cargo`, `npm`, `nuget`, `openvsx`, `vscode-marketplace`, `jetbrains-marketplace`, `goproxy`, `maven`, `terraform`, `rubygems`, `composer`, `pypi`, `conda`, `deb`, `rpm` et `pacman`. Voir [les modes de registre](#registry-modes). |
| `upstreams` | chaîne[] | non | Les URL amont essayées dans l'ordre en cas de défaut de cache ; un 404 de l'une passe à la suivante. Vaut par défaut l'URL intégrée du registre. Obligatoire en mode `hybrid`. |
| `index_url` | chaîne | non | Cargo uniquement : l'URL de l'index sparse. Vaut `https://index.crates.io` par défaut. Obligatoire en mode `hybrid` et pour les registres Gitea ou Forgejo auto-hébergés. |
| `broker_url` | chaîne | non | **sdkman uniquement.** Le courtier de téléchargement, second hôte de l'unique protocole. Vaut `https://broker.sdkman.io` par défaut ; `upstreams` est l'API des candidats (`https://api.sdkman.io/2`). Une URL http(s) absolue ; rejetée sur tout autre type ([RFC 0010](/rfc/0010-toolchain-managers) §4.5). |
| `storage` | chaîne | non | Le nom du backend de stockage. Doit correspondre à un `name` de `[[storage.backends]]`. À omettre pour employer le backend par défaut. |
| `path_allow` | chaîne[] | non | Liste d'autorisation, en motifs glob, des chemins amont que ce registre peut servir. Valide uniquement pour les types adressés par chemin (`deb`, `rpm`, `pacman`, `jetbrains`, `generic`) — l'employer ailleurs est une erreur de configuration. **Obligatoire et non vide pour `generic`.** Employez `["**"]` pour tout autoriser délibérément. |
| `on_confirmed` | chaîne | non | RFC 0014 §13 O6 — ce que fait l'audit amont d'une disparition confirmée sur *ce* registre : `"audit"` ou `"block"`. Remplace `[upstream_audit] on_confirmed` pour ce seul registre ; absent, la clé du parc s'applique. `"block"` exige que l'audit soit actif et que ce registre soit audité, sans quoi la configuration est refusée. |
| `vuln_db_url` | chaîne | non | **goproxy uniquement.** L'URL amont de la base de vulnérabilités Go. Défaut : `https://vuln.go.dev`. Mettre `""` désactive les endpoints `/v1/`. Voir [Proxy de vulnérabilités](/fr/use/vulnerability-proxy#_1-go-govulncheck). |
| `sumdb_url` | chaîne | non | **goproxy uniquement.** L'URL amont de la base de sommes de contrôle Go. Défaut : `https://sum.golang.org`. Mettre `""` désactive `/sumdb/{path}` — faites-le pour un registre qui ne sert que des modules privés, où une consultation divulguerait des chemins de modules privés à un journal public. |
| `upstream_auth` | table | non | Les identifiants envoyés à chaque requête amont. Voir [l'authentification amont](#upstream_auth). |
| `signed_downloads` | booléen | non | `false`. Émettre et accepter des URL de téléchargement signées pour ce registre, afin qu'il puisse garder `anonymous = []` même quand le client récupère les artefacts sans identifiants. Exige [`[server.signed_urls]`](#server-signed-urls) — le définir sans est une erreur de démarrage. Voir [les téléchargements signés](#registry-signed-downloads). |
| `tls` | table | non | Les réglages TLS des connexions amont. Voir [le TLS amont](#upstream_tls). |
| `proxy` | table | non | Un proxy HTTP ou SOCKS pour les connexions amont. Voir [le proxy amont](#upstream_proxy). |

#### Les modes de registre {#registry-modes}

Les registres `cargo`, `npm`, `nuget`, `openvsx`, `vscode-marketplace`,
`jetbrains-marketplace`, `goproxy`, `maven`, `terraform`, `rubygems`,
`composer`, `pypi`, `conda`, `deb`, `rpm` et `pacman` gèrent trois modes de
fonctionnement, réglés par le champ `mode`. Les autres — les forges git,
`jetbrains`, `generic`, `nodedist` et `sdkman` — sont en proxy seul, parce
qu'ils n'ont pas de protocole de publication à héberger :

| Mode | Description |
|------|-------------|
| `proxy` | Le défaut. BatleHub se contente de transmettre les requêtes aux registres amont. La publication est refusée. |
| `local` | BatleHub est le registre qui fait autorité. Aucun amont nécessaire. Les clients publient directement sur BatleHub. |
| `hybrid` | Le local d'abord. Sert directement les paquets publiés localement ; se rabat sur l'amont configuré pour tout ce qui ne l'est pas. Exige `upstreams` (et `index_url` pour Cargo). |

Publier exige au moins le rôle `user`. Le champ `published_by` est renseigné
depuis le `user_id` de l'utilisateur authentifié.

**Cargo** — les modes `local` et `hybrid` exposent toute l'API de publication
(`PUT /api/v1/crates/new`, yank, unyank, owners) et annoncent l'URL `api` dans
`config.json`, de sorte que Cargo la découvre automatiquement.

**npm** — les modes `local` et `hybrid` acceptent les charges de `npm publish`
(`PUT /proxy/{registry}/{name}`) et servent packuments et tarballs depuis le
stockage local.

**openvsx / vscode-marketplace** — les modes `local` et `hybrid` acceptent des
envois de VSIX bruts (`PUT /proxy/{registry}/{extension_id}/{version}/vsix`) et
les servent au téléchargement.

**goproxy** — les modes `local` et `hybrid` acceptent des envois de zips de
modules Go (`PUT /proxy/{registry}/{module}/@v/{version}.zip`). Le `go.mod` est
extrait automatiquement du zip ; le `.info` est généré depuis la version et
l'horodatage de l'envoi. Sert `@latest`, `@v/list`, `.info`, `.mod` et `.zip`
depuis le stockage local.

**maven** — les modes `local` et `hybrid` acceptent les envois d'artefacts de
`mvn deploy` (`PUT /proxy/{registry}/maven2/{path}`). Les fichiers autres que le
POM (JAR, sommes de contrôle) sont stockés immédiatement ; la publication en
trois phases est déclenchée à l'arrivée du `.pom`. `maven-metadata.xml` est
généré dynamiquement depuis la base et jamais mis en cache côté client. Voir
l'[exemple commenté 6.12](/fr/guide/configuration-examples#612-private-maven-registry-local--hybrid-mode).

**terraform** — les modes `local` et `hybrid` acceptent les envois de modules
(`POST /proxy/{registry}/v1/modules/{ns}/{name}/{provider}/{version}`), les
manifestes de version de provider (`POST .../v1/providers/{ns}/{type}/versions`)
et les envois de binaires de provider (`PUT .../artifact/{os}/{arch}`).
L'endpoint `tf_module_download` renvoie un `204` avec un en-tête
`X-Terraform-Get` qui pointe vers le tarball stocké localement. Voir
l'[exemple commenté 6.13](/fr/guide/configuration-examples#613-private-terraform-registry-local--hybrid-mode).

**rubygems** — les modes `local` et `hybrid` acceptent les envois de
`gem push` (`POST /proxy/{registry}/api/v1/gems`). Servent les fichiers de gem,
l'index de versions et l'information REST depuis le stockage local.

**composer** — les modes `local` et `hybrid` acceptent des envois de ZIP
(`POST /proxy/{registry}/api/upload`). Le `composer.json` (avec ses champs `name`
et `version`) est extrait automatiquement. Servent `packages.json`, les
métadonnées `p2/` et les artefacts `dist/` depuis le stockage local.

**pypi** — les modes `local` et `hybrid` acceptent les envois multipart
compatibles twine (`POST /proxy/{registry}/legacy/`). Le nom et la version sont
extraits du nom de fichier envoyé et des champs multipart. En mode `local`,
l'index de l'API Simple (`GET /proxy/{registry}/simple/{package}/`) est généré
depuis la base. En mode `hybrid`, les entrées amont et locales sont servies
ensemble.

**conda** — les modes `local` et `hybrid` acceptent des envois de paquets conda
bruts (`POST /proxy/{registry}/{platform}/`). Les métadonnées (`name`,
`version`, `build`, `depends`) sont extraites de l'`info/index.json` de
l'archive `.tar.bz2` ou `.conda`. En mode `local`, `repodata.json` est généré
depuis la base. En mode `hybrid`, les entrées locales sont fusionnées dans le
`repodata.json` de l'amont.

#### Notes par type de registre

**`github`** — fait proxy de l'API REST de GitHub (releases, assets, tarballs de
sources, fichiers bruts). Exige que `upstreams` pointe vers
`https://api.github.com` (le défaut).

**`npm`** — fait proxy de tout le protocole du registre npm : packuments,
métadonnées de version et tarballs `.tgz`. Fonctionne avec npm, yarn, pnpm et
tout outil qui parle le protocole du registre npm. Mettez `mode = "local"` ou
`mode = "hybrid"` pour activer la publication. Voir
[les modes de registre](#registry-modes) et
l'[exemple commenté 6.7](/fr/guide/configuration-examples#67-private-npm-registry-local--hybrid-mode).
Les deux modes de `npm audit` (`quick` et `bulk`) sont relayés automatiquement —
voir [Proxy de vulnérabilités](/fr/use/vulnerability-proxy#_2-npm-—-npm-audit).

**`cargo`** — fait proxy de l'index sparse Cargo et des téléchargements de
`.crate`. Définissez `index_url` pour un registre Gitea ou Forgejo
auto-hébergé. Mettez `mode = "local"` ou `mode = "hybrid"` pour activer la
publication. Voir [les modes de registre](#registry-modes) et
l'[exemple commenté 6.6](/fr/guide/configuration-examples#66-private-cargo-registry-local--hybrid-mode).

**`openvsx`** — fait proxy des téléchargements de VSIX d'extensions VS Code
depuis [open-vsx.org](https://open-vsx.org) ou un hôte compatible. Les
identifiants d'extension suivent la convention `{publisher}.{name}`. Mettez
`mode = "local"` ou `mode = "hybrid"` pour activer la publication. Voir
l'[exemple commenté 6.8](/fr/guide/configuration-examples#68-private-vs-code-extension-registry-local--hybrid-mode).

**`vscode-marketplace`** — fait proxy des téléchargements de VSIX d'extensions
VS Code depuis
[marketplace.visualstudio.com](https://marketplace.visualstudio.com), par l'API
Gallery de Microsoft. Les identifiants d'extension suivent la même convention
`{publisher}.{name}` qu'OpenVSX. Les métadonnées sont résolues par un appel
`POST /_apis/public/gallery/extensionquery` ; les artefacts sont récupérés
directement sur
`/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage`.
Employez ce type quand vous devez mettre en cache des extensions disponibles
uniquement sur la place de marché Microsoft et non répliquées sur open-vsx.org.
Gère `mode = "local"` et `mode = "hybrid"` pour héberger des extensions privées —
voir l'[exemple commenté 6.8](/fr/guide/configuration-examples#68-private-vs-code-extension-registry-local--hybrid-mode).

```toml
[[registries]]
type = "vscode-marketplace"
name = "vscode"
# upstreams = ["https://marketplace.visualstudio.com"]  # défaut

[registries.rbac]
user = ["releases:read", "source:read"]
admin = ["*"]
```

Télécharger un VSIX par le proxy :

```sh
# Dernière version
curl -H "Authorization: Bearer <token>" \
  http://localhost:8080/proxy/vscode/ms-python.python/latest/vsix \
  -o ms-python.python.vsix

# Version figée
curl -H "Authorization: Bearer <token>" \
  http://localhost:8080/proxy/vscode/ms-python.python/2024.2.1/vsix \
  -o ms-python.python-2024.2.1.vsix
```

**`jetbrains-marketplace`** — une émulation complète de la
[place de marché JetBrains](https://plugins.jetbrains.com) pour l'écosystème de
plugins des IDE (recherche, mises à jour compatibles, blobs `meta.json`,
téléchargements de plugins), à distinguer du type `jetbrains`, adressé par chemin
et destiné aux archives d'IDE. Pointez un IDE vers le proxy soit
**entièrement** (Aide → Edit Custom Properties… →
`idea.plugins.host=https://your-host/proxy/{registry}`), soit **en
complément** (Paramètres → Plugins → Manage Plugin Repositories… →
`https://your-host/proxy/{registry}/updatePlugins.xml`). Gère les modes `local`
et `hybrid`, avec une publication multipart compatible place de marché
(`POST /proxy/{registry}/api/updates/upload`), de sorte que le
`plugin-repository-rest-client` de JetBrains et la tâche Gradle `publishPlugin`
fonctionnent contre lui. Les métadonnées par plugin, les artefacts et les blobs de
requête transmis sont mis en cache avec repli sur une réponse périmée : tout ce
qui a été vu une fois continue de se résoudre si plugins.jetbrains.com devient
injoignable.

```toml
[[registries]]
type = "jetbrains-marketplace"
name = "jbm"
mode = "hybrid"                                # ou "proxy" / "local"
upstreams = ["https://plugins.jetbrains.com"]  # défaut

[registries.rbac]
user = ["releases:read"]
admin = ["*"]
```

Télécharger et publier par le proxy :

```sh
# Télécharger une version de plugin
curl -H "Authorization: Bearer <token>" \
  "http://localhost:8080/proxy/jbm/plugin/download?pluginId=org.rust.lang&version=241.25026.107" \
  -o rust-plugin.zip

# Publier un plugin (mode local ou hybrid)
curl -X POST -H "Authorization: Bearer <token>" \
  -F "xmlId=com.example.myplugin" \
  -F "file=@my-plugin.zip" \
  http://localhost:8080/proxy/jbm/api/updates/upload
```

**`goproxy`** — implémente le
[protocole GOPROXY](https://go.dev/ref/mod#goproxy-protocol) pour le proxy de
modules Go. Mettez `mode = "local"` ou `mode = "hybrid"` pour héberger des
modules privés — voir [les modes de registre](#registry-modes) et
l'[exemple commenté 6.9](/fr/guide/configuration-examples#69-private-go-module-proxy-local--hybrid-mode).
Gère les cinq endpoints du proxy de modules, plus le protocole de la base de
vulnérabilités Go (`govulndb`) — voir
[Proxy de vulnérabilités](/fr/use/vulnerability-proxy#_1-go-govulncheck).

| Endpoint | Description |
|----------|-------------|
| `/{module}/@latest` | JSON de métadonnées de la dernière version |
| `/{module}/@v/list` | Liste des versions connues, une par ligne |
| `/{module}/@v/{version}.info` | JSON de métadonnées d'une version |
| `/{module}/@v/{version}.mod` | Le fichier `go.mod` brut |
| `/{module}/@v/{version}.zip` | L'archive zip des sources du module |
| `/v1/index.json` | govulndb — tous les identifiants de vulnérabilité connus |
| `/v1/ID/{id}.json` | govulndb — l'enregistrement OSV complet d'une vulnérabilité |
| `/v1/query` | govulndb — requête groupée par module et version |

Un chemin de module peut contenir des barres obliques (par exemple
`golang.org/x/text`). Les chemins encodés en majuscules (la convention
`!{minuscule}`) sont transmis inchangés à l'amont.

> **Note sur le cache :** les réponses `@latest` et `@v/list` sont mises en cache
> définitivement après la première requête, comme les autres artefacts. Elles
> peuvent devenir périmées si de nouvelles versions sont publiées. Videz le
> stockage du proxy (ou configurez un `metadata_ttl_secs` plus court) pour
> prendre en compte les nouvelles versions immédiatement.

Le champ facultatif `vuln_db_url` décide de l'amont govulndb employé (défaut :
`https://vuln.go.dev`). Mettez-le à `""` pour désactiver entièrement les
endpoints `/v1/`.

Le champ facultatif `sumdb_url` gouverne le proxy de la **base de sommes de
contrôle** (défaut : `https://sum.golang.org`). C'est l'autre moitié du protocole
GOPROXY : sans lui, l'outil go ouvre encore une connexion directe vers
`sum.golang.org` pour chaque module inconnu — le proxy a alors déplacé la sortie
réseau plutôt que de la supprimer, et un parc coupé du réseau échoue en se
fermant. Les réponses sont mises en cache, et c'est ce qui fait fonctionner le
cas hors ligne ; ce cache est sain parce que le journal est signé, donc un
enregistrement en cache est exactement aussi digne de confiance qu'un
enregistrement en direct. Mettez-le à `""` pour un registre qui ne sert que des
modules privés, où une consultation publierait des chemins de modules privés dans
un journal de transparence public.

Configurez la chaîne d'outils go pour employer le proxy :

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="http://batlehub.example.com/proxy/go,direct"
export GOVULNDB="http://batlehub.example.com/proxy/go"
```

---

**`maven`** — fait proxy des dépôts d'artefacts Maven. Gère les requêtes `GET`
pour les fichiers POM, les JAR, les JAR de sources et de Javadoc, les sommes de
contrôle SHA-1 et MD5, et le XML de métadonnées Maven. Compatible avec Maven,
Gradle et tout outil qui parle le protocole de dépôt Maven. Amont par défaut :
`https://repo1.maven.org/maven2`. Mettez `mode = "local"` ou `mode = "hybrid"`
pour activer la publication privée — voir
[les modes de registre](#registry-modes) et
l'[exemple commenté 6.12](/fr/guide/configuration-examples#612-private-maven-registry-local--hybrid-mode).

Configurez Maven pour employer le proxy :

```xml
<!-- ~/.m2/settings.xml -->
<settings>
  <mirrors>
    <mirror>
      <id>batlehub</id>
      <mirrorOf>central</mirrorOf>
      <url>http://batlehub.example.com/proxy/maven/maven2/</url>
    </mirror>
  </mirrors>
</settings>
```

Configurez Gradle pour employer le proxy :

```kotlin
// settings.gradle.kts
dependencyResolutionManagement {
    repositories {
        maven { url = uri("http://batlehub.example.com/proxy/maven/maven2/") }
    }
}
```

---

**`terraform`** — fait proxy du protocole de registre Terraform pour les
providers et les modules. Gère la liste des versions de provider, les
informations de téléchargement d'un provider (URL du binaire et sommes de
contrôle), la liste des versions de module et le téléchargement des sources d'un
module. Amont par défaut : `https://registry.terraform.io`. Mettez
`mode = "local"` ou `mode = "hybrid"` pour activer la publication privée de
modules et de providers — voir [les modes de registre](#registry-modes) et
l'[exemple commenté 6.13](/fr/guide/configuration-examples#613-private-terraform-registry-local--hybrid-mode).

| Endpoint | Méthode | Description |
|---|---|---|
| `/v1/providers/{namespace}/{type}/versions` | GET | Liste des versions de provider (JSON, mis en cache) |
| `/v1/providers/{namespace}/{type}/{version}/download/{os}/{arch}` | GET | JSON d'informations de téléchargement (mis en cache ; en local : réécrit vers l'URL `/artifact`) |
| `/v1/providers/{namespace}/{type}/versions` | POST | **Local/hybrid :** publier le manifeste de version d'un provider |
| `/v1/providers/{namespace}/{type}/{version}/artifact/{os}/{arch}` | PUT | **Local/hybrid :** envoyer le zip du binaire de provider |
| `/v1/providers/{namespace}/{type}/{version}/artifact/{os}/{arch}` | GET | **Local/hybrid :** servir le zip du binaire de provider |
| `/v1/modules/{namespace}/{name}/{provider}/versions` | GET | Liste des versions de module (JSON, mis en cache) |
| `/v1/modules/{namespace}/{name}/{provider}/{version}/download` | GET | Redirection vers les sources du module (`204` + `X-Terraform-Get` ; en local : pointe vers `/artifact`) |
| `/v1/modules/{namespace}/{name}/{provider}/{version}` | POST | **Local/hybrid :** envoyer le tar.gz du module |
| `/v1/modules/{namespace}/{name}/{provider}/{version}/artifact` | GET | **Local/hybrid :** servir le tar.gz du module |

> **Le téléchargement de module en mode proxy :** transmet l'en-tête
> `204 + X-Terraform-Get` de l'amont sans le mettre en cache. En mode local ou
> hybrid, l'en-tête est réécrit pour pointer vers l'endpoint `/artifact` local.

Configurez la CLI Terraform pour employer le proxy pour les providers :

```hcl
# ~/.terraformrc  (ou %APPDATA%/terraform.rc sous Windows)
provider_installation {
  network_mirror {
    url = "http://batlehub.example.com/proxy/terraform/"
  }
}
```

---

**`nuget`** — implémente l'[API NuGet v3](https://learn.microsoft.com/en-us/nuget/api/overview)
pour la gestion de paquets .NET. L'index de services v3 (`index.json`) est
synthétisé par BatleHub et fait pointer toutes les URL de ressources vers le
proxy. Amont par défaut : `https://api.nuget.org`. Les données de vulnérabilité
pour `dotnet list package --vulnerable` sont relayées automatiquement — voir
[Proxy de vulnérabilités](/fr/use/vulnerability-proxy#_3-nuget-vulnerable).

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/nuget/v3/index.json` | GET | L'index de services NuGet v3 |
| `/proxy/{registry}/nuget/v3/registration5/{id}/index.json` | GET | L'enregistrement d'un paquet (toutes versions et métadonnées) |
| `/proxy/{registry}/nuget/v3/flat/{id}/index.json` | GET | La liste de versions du conteneur plat |
| `/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}` | GET | Le téléchargement du contenu (`.nupkg`, `.nuspec`) |
| `/proxy/{registry}/nuget/v3/query` | GET | La recherche de paquets |
| `/proxy/{registry}/nuget/v3/vulnerabilities/index.json` | GET | L'index du catalogue de vulnérabilités |
| `/proxy/{registry}/nuget/v3/vulnerabilities/page/{page}` | GET | Une page du catalogue de vulnérabilités |
| `/proxy/{registry}/nuget/api/v2/package` | PUT | Publier un `.nupkg` |
| `/proxy/{registry}/nuget/v2/package/{id}/{version}` | DELETE | Retirer une version |

Configurez NuGet dans `nuget.config` :

```xml
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="batlehub" value="https://batlehub.example.com/proxy/nuget/nuget/v3/index.json" />
  </packageSources>
  <packageSourceCredentials>
    <batlehub>
      <add key="Username" value="user" />
      <add key="ClearTextPassword" value="<token>" />
    </batlehub>
  </packageSourceCredentials>
</configuration>
```

---

**`composer`** — implémente le
[protocole Packagist v2](https://packagist.org/apidoc) pour Composer. Sert
`packages.json` (l'index racine du dépôt), `p2/{vendor}/{package}.json` (les
métadonnées) et `dist/{vendor}/{package}/{version}` (le téléchargement de
l'artefact ZIP). Amont par défaut : `https://packagist.org`. `composer audit`
est relayé automatiquement — voir
[Proxy de vulnérabilités](/fr/use/vulnerability-proxy#_4-composer-—-composer-audit).
Mettez `mode = "local"` ou `mode = "hybrid"` pour activer la publication de
paquets privés — voir [les modes de registre](#registry-modes) et
l'[exemple commenté 6.15](/fr/guide/configuration-examples#615-private-composer-registry-local--hybrid-mode).

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/packages.json` | GET | L'index racine du dépôt (liste tous les noms de paquets connus) |
| `/proxy/{registry}/p2/{vendor}/{package}.json` | GET | Les métadonnées du paquet (toutes versions, URL de dist) |
| `/proxy/{registry}/p2/{vendor}/{package}~dev.json` | GET | La variante de métadonnées en stabilité dev |
| `/proxy/{registry}/dist/{vendor}/{package}/{version}` | GET | Télécharger l'artefact ZIP |
| `/proxy/{registry}/api/security-advisories/` | GET | La requête d'alertes de sécurité (`composer audit`) |
| `/proxy/{registry}/api/upload` | POST | **Local/hybrid :** publier un paquet (multipart ou corps ZIP brut) |
| `/proxy/{registry}/api/packages/{vendor}/{package}/versions/{version}` | DELETE | **Local/hybrid :** retirer une version |

---

**`pypi`** — implémente l'[API de dépôt simple de Python (PEP 503 / PEP 691)](https://peps.python.org/pep-0503/)
et l'[API JSON de PyPI](https://docs.pypi.org/api/json/). Les URL de
téléchargement des pages d'index simple sont réécrites pour passer par le cache
du proxy. Amont par défaut : `https://pypi.org`. Mettez `mode = "local"` ou
`mode = "hybrid"` pour activer la publication privée par `twine upload` — voir
[les modes de registre](#registry-modes).

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/simple/` | GET | L'index racine (tous les noms de projet) |
| `/proxy/{registry}/simple/{package}/` | GET | La liste de fichiers d'un paquet (HTML ou JSON selon l'en-tête `Accept`) |
| `/proxy/{registry}/packages/{filename}` | GET | Télécharger une wheel ou une sdist (mise en cache) |
| `/proxy/{registry}/legacy/` | POST | **Local/hybrid :** publication multipart compatible twine |

Configurez pip :

```ini
# ~/.pip/pip.conf
[global]
index-url = http://batlehub.example.com/proxy/my-pypi/simple/
```

---

**`conda`** — fait proxy d'un unique canal conda (par exemple `conda-forge`),
toutes plateformes confondues. Met en cache `repodata.json` et les fichiers de
paquet par plateforme. En mode hybrid, les paquets publiés localement sont
fusionnés dans le `repodata.json` de l'amont. Amont par défaut :
`https://conda.anaconda.org`. Mettez `mode = "local"` ou `mode = "hybrid"` pour
activer la publication privée — voir [les modes de registre](#registry-modes).

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/{platform}/repodata.json` | GET | L'index de canal d'une plateforme (par ex. `linux-64`, `noarch`) |
| `/proxy/{registry}/{platform}/current_repodata.json` | GET | L'index réduit (mode proxy uniquement) |
| `/proxy/{registry}/{platform}/{filename}` | GET | Télécharger un paquet `.conda` ou `.tar.bz2` |
| `/proxy/{registry}/{platform}/` | POST | **Local/hybrid :** publier un paquet conda |

Configurez conda :

```yaml
# ~/.condarc
channels:
  - http://batlehub.example.com/proxy/my-conda
  - nodefaults
```

Configurez Composer pour employer le proxy, en ajoutant une entrée de dépôt à
`composer.json` :

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "http://batlehub.example.com/proxy/packagist/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer <your-token>"]
        }
      }
    }
  ]
}
```

Ou stockez les identifiants dans `auth.json` (ne versionnez jamais ce fichier) :

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "user",
      "password": "<your-token>"
    }
  }
}
```

---

**`deb`** — fait proxy et héberge des dépôts APT Debian et Ubuntu. En mode proxy,
le fichier `Release` ou `InRelease` de l'amont et sa signature existante sont
relayés inchangés ; les clients vérifient contre la clé d'archive **de l'amont**.
En mode local ou hybrid, BatleHub génère et (éventuellement) signe `Packages` et
`Release` avec une clé OpenPGP Ed25519 (`repo_signing`). Amont par défaut :
`https://deb.debian.org`.

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/deb/dists/{dist}/{component}/binary-{arch}/Packages` | GET | L'index des paquets (texte brut) |
| `/proxy/{registry}/deb/dists/{dist}/{component}/binary-{arch}/Packages.gz` | GET | L'index des paquets (gzip) |
| `/proxy/{registry}/deb/dists/{dist}/Release` | GET | Les métadonnées de publication |
| `/proxy/{registry}/deb/dists/{dist}/InRelease` | GET | Les métadonnées de publication signées en ligne |
| `/proxy/{registry}/deb/pool/{dist}/{component}/{filename}` | GET | Télécharger un paquet `.deb` |
| `/proxy/{registry}/deb/pool/{dist}/{component}/upload` | PUT | **Local/hybrid :** publier un `.deb` |
| `/proxy/{registry}/deb/key.gpg` | GET | **Local/hybrid :** la clé publique de signature (en ASCII armé) |

**Mise en place côté client — mode proxy** (la signature de l'amont est relayée ;
faites confiance à la clé d'archive de l'amont) :

```sh
# Miroirs Debian et Ubuntu officiels — la clé est déjà dans le paquet de trousseau
KEYRING=/usr/share/keyrings/debian-archive-keyring.gpg  # ubuntu : ubuntu-archive-keyring.gpg
echo "deb [signed-by=$KEYRING] http://batlehub.example.com/proxy/my-deb stable main" \
  | sudo tee /etc/apt/sources.list.d/my-deb.list

# Amont tiers — importez d'abord sa clé :
# curl -fsSL <upstream-key-url> | gpg --dearmor \
#   | sudo tee /usr/share/keyrings/my-deb.gpg >/dev/null

sudo apt update
```

**Mise en place côté client — mode local ou hybrid** (BatleHub signe `Release` ;
importez la clé de BatleHub) :

```sh
# Importer la clé de signature de BatleHub
curl -fsSL http://batlehub.example.com/proxy/my-deb/deb/key.gpg \
  | sudo tee /usr/share/keyrings/my-deb.asc >/dev/null

# Ajouter la source (ajustez la suite et le composant à votre dépôt)
echo "deb [signed-by=/usr/share/keyrings/my-deb.asc] \
  http://batlehub.example.com/proxy/my-deb/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/my-deb.list

sudo apt update
```

Pour un dépôt local non signé (aucune clé `repo_signing` configurée), remplacez
`[signed-by=…]` par `[trusted=yes]`.

**L'authentification d'un registre privé :**

APT lit ses identifiants dans `/etc/apt/auth.conf.d/` (Debian 9+ / Ubuntu
19.04+). L'entrée de `sources.list` reste inchangée — les identifiants vivent
dans un fichier à part.

```sh
sudo tee /etc/apt/auth.conf.d/batlehub.conf > /dev/null <<'EOF'
machine batlehub.example.com
login <your-username>
password <your-token>
EOF
sudo chmod 0600 /etc/apt/auth.conf.d/batlehub.conf

sudo apt update
```

Sur un système plus ancien, sans prise en charge d'`auth.conf.d`, employez
`/etc/apt/auth.conf` avec la même strophe `machine / login / password`.

Vous pouvez aussi embarquer les identifiants directement dans l'URL (moins sûr —
visible dans la sortie d'`apt-cache policy`) :

```sh
echo "deb [signed-by=…] https://<user>:<token>@batlehub.example.com/proxy/my-deb/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/my-deb.list
```

**Publier un `.deb` (mode local ou hybrid) :**

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  --data-binary @hello_1.0_amd64.deb \
  http://batlehub.example.com/proxy/my-deb/deb/pool/stable/main/upload
```

---

**`rpm`** — fait proxy et héberge des dépôts RPM pour DNF et YUM. En mode proxy,
le `repomd.xml` de l'amont (et son éventuelle signature `repomd.xml.asc`) est
relayé ; les clients vérifient contre la clé GPG **de l'amont**. En mode local ou
hybrid, BatleHub régénère `repodata/` et signe éventuellement `repomd.xml` avec
une clé OpenPGP Ed25519 (`repo_signing`). Amont par défaut :
`https://dl.fedoraproject.org/pub/fedora/linux/releases`.

| Endpoint | Méthode | Description |
|---|---|---|
| `/proxy/{registry}/rpm/repodata/repomd.xml` | GET | L'index des métadonnées du dépôt |
| `/proxy/{registry}/rpm/repodata/repomd.xml.asc` | GET | **Local/hybrid (signé) :** la signature OpenPGP détachée |
| `/proxy/{registry}/rpm/repodata/repomd.xml.key` | GET | **Local/hybrid (signé) :** la clé publique de signature (en ASCII armé) |
| `/proxy/{registry}/rpm/repodata/{filename}` | GET | Les autres fichiers de repodata (primary.xml.gz, filelists.xml.gz, …) |
| `/proxy/{registry}/rpm/{path}` | GET | Télécharger un paquet `.rpm` |
| `/proxy/{registry}/rpm/upload` | PUT | **Local/hybrid :** publier un `.rpm` |

**Mise en place côté client — le fichier `.repo`**
(`/etc/yum.repos.d/<name>.repo`) :

```ini
[my-rpm]
name=My RPM Registry
baseurl=http://batlehub.example.com/proxy/my-rpm/rpm
enabled=1
repo_gpgcheck=0   # mettre 1 et ajouter gpgkey= pour un dépôt signé
gpgcheck=0
```

Pour un dépôt local ou hybrid signé (avec une clé `repo_signing` de BatleHub
configurée) :

```ini
[my-rpm]
name=My RPM Registry
baseurl=http://batlehub.example.com/proxy/my-rpm/rpm
enabled=1
repo_gpgcheck=1
gpgcheck=0
gpgkey=http://batlehub.example.com/proxy/my-rpm/rpm/repodata/repomd.xml.key
```

Pour un dépôt en proxy dont l'amont signe ses métadonnées, faites pointer
`gpgkey` vers la clé du projet **amont**.

**L'authentification d'un registre privé :**

DNF et YUM lisent `username` et `password` directement dans le fichier `.repo` :

```ini
[my-rpm]
name=My RPM Registry
baseurl=http://batlehub.example.com/proxy/my-rpm/rpm
enabled=1
repo_gpgcheck=0
gpgcheck=0
username=<your-username>
password=<your-token>
```

Vous pouvez aussi employer `~/.netrc` (DNF et libcurl l'honorent pour
l'authentification HTTP Basic) :

```
machine batlehub.example.com
login <your-username>
password <your-token>
```

**Publier un `.rpm` (mode local ou hybrid) :**

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  --data-binary @hello-1.0-1.x86_64.rpm \
  http://batlehub.example.com/proxy/my-rpm/rpm/upload
```

---

**`generic`** — un miroir adressé par chemin de n'importe quelle arborescence de
fichiers en HTTP, pour les amonts sans aucun protocole de paquets : archives de
chaînes d'outils (`nodejs.org/dist`, `static.rust-lang.org`,
`dl.google.com/go`) et CDN d'éditeurs à binaire unique (`get.helm.sh`,
`dl.min.io`, `binaries.sonarsource.com`). Proxy seul — il n'y a ni publication,
ni index, ni modèle de signature. Une requête vers
`/proxy/{registry}/generic/{path}` diffuse `{upstream}/{path}` et le met en cache
au premier défaut.

Deux champs sont **obligatoires** pour ce type :

- `upstreams` — il n'y a pas d'arborescence par défaut sur laquelle se rabattre ;
- `path_allow` — le chemin de la requête est transmis tel quel à l'amont, donc
  sans liste d'autorisation, un registre pointé vers un hôte qui sert de nombreux
  locataires sans rapport (un bucket partagé, un CDN multi-éditeurs) relaierait
  *tous* les chemins de cet hôte. Les motifs suivent la sémantique de
  [`glob`](https://docs.rs/glob), où `*` traverse aussi les `/`. Un chemin hors de
  la liste est rejeté par un `403` avant toute requête amont — ce qui signifie
  aussi que le préchauffage (`warm_paths`) ne peut pas la contourner.

```toml
[[registries]]
type = "generic"
name = "node-dist"
mode = "proxy"
upstreams = ["https://nodejs.org/dist"]
# Vérifié contre `mise install node` : mise récupère à la fois l'archive de la
# plateforme et l'archive des sources `node-v<ver>.tar.gz`, donc un motif limité
# à une plateforme donne un 403 en pleine installation.
path_allow = ["v*/**"]

[registries.rbac]
anonymous = ["releases:read"]

# Préchauffer des chemins précis (les registres adressés par chemin emploient
# `warm_paths`, pas `warm_packages`).
[registries.cache]
warm_paths = ["v24.18.0/node-v24.18.0-linux-x64.tar.gz"]
```

Pointez le client dessus par la variable de miroir propre à la chaîne d'outils :

```sh
export NODEJS_ORG_MIRROR=https://batlehub.example.com/proxy/node-dist/generic
export RUSTUP_DIST_SERVER=https://batlehub.example.com/proxy/rust-dist/generic
```

`batlehub-cli registry suggest` analyse un projet (`mise.toml` et `mise.lock`
compris) et imprime à la fois les blocs `[[registries]]` et les variables
d'environnement client correspondantes — voir les
[sous-commandes du binaire serveur](/fr/guide/server-cli).

**La taille des artefacts :** les archives répliquées sont souvent volumineuses
et le proxy met un artefact en tampon avant de le mettre en cache : relevez donc
`limits.max_artifact_size_bytes` (500 Mio par défaut) quand vous répliquez des
chaînes d'outils ou des téléchargements de la taille d'un IDE.

---

**Les champs de `[registries.cache]` :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `metadata_ttl_secs` | u64 | `300` | Durée de mise en cache des métadonnées de publication (listes de versions, informations de release), en secondes |
| `serve_stale` | booléen | `true` | À `true`, sert des métadonnées périmées si l'amont renvoie une erreur transitoire (5xx). Garde le registre utilisable pendant une panne amont. |
| `artifact_ttl_secs` | u64 ? | — | Évincer les artefacts plus vieux que N secondes. À omettre pour ne jamais expirer par âge. |
| `idle_days` | u64 ? | — | Évincer les artefacts non accédés depuis N jours. À omettre pour désactiver l'éviction par inactivité. |
| `max_size_bytes` | u64 ? | — | Plafond de stockage, en octets. Au-delà, les artefacts les moins récemment utilisés sont retirés jusqu'à repasser sous la limite. À omettre pour ne pas plafonner. |
| `keep_latest_n` | usize ? | — | Ne garder que les N versions les plus récemment mises en cache par paquet. Les plus anciennes sont évincées à l'arrivée d'une nouvelle. À omettre pour tout garder. |
| `warm_packages` | chaîne[] | `[]` | Les paquets à récupérer d'avance au démarrage et par l'endpoint de préchauffage. Chaque entrée est un nom nu (`"lodash"`) ou une version figée (`"lodash@4.17.21"`). |
| `warm_latest_n` | usize | `1` | Nombre de versions les plus récentes à préchauffer par nom nu. Une entrée à version figée en préchauffe toujours exactement une. |
| `warm_concurrency` | usize | `2` | Nombre maximum de téléchargements d'artefacts simultanés pendant une passe de préchauffage. |

**Exemple d'éviction :**

```toml
[registries.cache]
metadata_ttl_secs = 600
artifact_ttl_secs = 2592000   # 30 jours
idle_days         = 14
max_size_bytes    = 10737418240  # 10 Gio
keep_latest_n     = 5
```

**Exemple de préchauffage :**

```toml
[registries.cache]
warm_packages    = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n    = 3      # préchauffer les 3 versions les plus récentes des noms nus
warm_concurrency = 4      # jusqu'à 4 téléchargements en parallèle
```

Au démarrage, BatleHub récupère d'avance les paquets listés, pour qu'ils soient
disponibles sans latence à la première requête. Les mêmes paquets se
repréchauffent à tout moment par l'API d'administration :

```sh
# Préchauffer toutes les versions configurées de lodash
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash"}'

# Remplacer le nombre de versions pour cet appel seulement
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash", "versions": 10}'
```

> **Ce que chaque registre gère :** l'énumération des versions (employée par le
> préchauffage par nom nu) est implémentée pour **npm**, **Cargo**, **OpenVSX**
> et les modules **Go**. Pour GitHub et la place de marché VS Code, passez une
> chaîne de version figée (par exemple `"owner/repo@v1.2.3"`) pour préchauffer
> une version précise.

**La place de marché JetBrains :** les plugins se préchauffent comme n'importe
quel paquet — une entrée est l'`xmlId` du plugin, nu ou figé :

```toml
[[registries]]
name = "jetbrains-plugins"
type = "jetbrains-marketplace"

[registries.cache]
warm_packages = ["org.rust.lang", "com.intellij.ml.llm@2026.1.1"]
warm_latest_n = 2
```

Un nom nu énumère les versions par `/plugins/list`, qui ne liste que le canal
**Stable** — les plugins d'un canal EAP ou nocturne ne sont pas récupérés
d'avance (leur chemin de téléchargement porte un paramètre `channel`). L'archive
préchauffée est celle que l'IDE télécharge depuis
`plugin/download?pluginId=…&version=…` : elle est donc servie depuis le cache dès
la première requête, y compris pendant que plugins.jetbrains.com est
injoignable.

**La coordination du préchauffage entre réplicas (Redis) :**

Quand `[cache] type = "redis"` est configuré et que la fonctionnalité
`cache-redis` est compilée, BatleHub coordonne automatiquement le préchauffage
entre les réplicas. Avant de télécharger un artefact, chaque réplica tente
d'acquérir un verrou Redis éphémère
(`SET batlehub:warm:{key} 1 NX PX 600000`). Seul le premier réplica à l'obtenir
effectue le téléchargement amont ; les autres sautent cet artefact. Cela évite la
ruée de téléchargements quand plusieurs réplicas redémarrent en même temps et
découvrent les mêmes défauts de cache. Aucune configuration supplémentaire n'est
nécessaire — la coordination s'active dès que le backend de cache Redis est
choisi. Avec les autres backends (`memory` ou `postgres`), chaque réplica
préchauffe de son côté (sans danger, mais redondant).

**La déduplication par le contenu :**

BatleHub stocke les octets physiques d'un artefact sous une clé adressée par le
contenu (`blob/{sha256}`) et fait correspondre les clés d'artefact logiques à ce
blob par un compteur de références. Quand les mêmes octets sont référencés par
plusieurs clés logiques (le même paquet répliqué sur deux registres, ou une
version retirée puis republiée), une seule copie des données est stockée sur
disque ou sur S3. Les tables de déduplication (`artifact_dedup_index`,
`artifact_dedup_refs`) sont créées automatiquement par la migration de base et ne
demandent aucune configuration.

**Les champs de `[registries.rbac]` :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `anonymous` | chaîne[] | `[]` | Permissions accordées aux requêtes non authentifiées |
| `user` | chaîne[] | `[]` | Permissions accordées aux utilisateurs authentifiés (hérite de celles des anonymes) |
| `admin` | chaîne[] | `[]` | Permissions accordées aux admins (hérite de celles des utilisateurs et des anonymes) |
| `groups` | table | `{}` | Permissions dynamiques par groupe (voir la [section 4](#_4-permissions-reference)) |

**`[registries.refs]` — la résolution de ref d'une forge git (`github`, `gitlab`
et `forgejo` uniquement ; RFC 0019) :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `branch_ttl_secs` | u64 | `60` | Durée de confiance d'une résolution branche → commit avant de réinterroger la forge. En dessous de `10`, c'est une erreur de configuration : re-résoudre à chaque requête est un déni de service contre soi-même par limitation de débit. |
| `tag_ttl_secs` | u64 | `3600` | Durée de confiance d'une résolution de tag. C'est aussi la latence avec laquelle un tag déplacé est remarqué. |
| `mutable_refs` | chaîne | `"warn"` | Ce que fait le fait de suivre une branche. `warn` la sert et le signale ; `deny` refuse toute coordonnée mutable, ce que veut un registre qui doit être reproductible. |
| `tag_moved` | chaîne | `"deny"` | Ce que fait un tag qui résout désormais vers un autre commit — et un asset de release dont l'empreinte a changé. Refusé par défaut : ce sont les deux façons, propres aux forges, de substituer des octets sous une coordonnée stable. `warn` sert et signale. |

> Toute archive (`tarball/{ref}`, `zipball/{ref}`) et tout fichier brut est résolu
> en un commit avant d'être récupéré, et mis en cache sous ce commit — `main`
> aujourd'hui et `main` demain sont deux entrées. Toute réponse de forge porte
> `X-BatleHub-Ref-Kind` (`commit`, `tag` ou `branch`),
> `X-BatleHub-Resolved-Commit`, et `X-BatleHub-Ref-Previous-Commit` quand la ref a
> bougé. Un registre de forge sans `[registries.upstream_auth]` lève
> l'avertissement `forge.anonymous-upstream` : GitHub en anonyme autorise 60
> requêtes d'API par heure, et une résolution de ref en dépense une ou deux par
> nouvelle ref.
>
> Avec `[registries.security]`, ces trois faits voyagent avec le verdict de la
> version : une branche avertie répond donc `X-BatleHub-Verdict: warned`, et
> `batlehub why` l'explique. Sans lui, il n'y a pas de verdict pour les porter :
> un `deny` est un simple `403` qui nomme le code, et un `warn` se réduit aux
> en-têtes ci-dessus.

**`[registries.raw]` — le service de fichiers bruts (types de forge uniquement ;
RFC 0019) :** {#registries-raw}

**Désactivé tant qu'il n'est pas écrit.** Le contenu brut était autrefois servi
implicitement sur les trois forges ; un registre sans bloc `[registries.raw]` le
refuse désormais, et le refus nomme cette section. Tout registre de forge qui en
est dépourvu lève `forge.raw-disabled-but-linked`, parce que l'extrait de mise en
place que le registre génère réécrit l'hôte de contenu brut de la forge vers un
chemin qui refuse.

```toml
[registries.raw]
enabled        = true
max_size_bytes = 10485760
repos          = ["cli/*"]
require_pinned = false
scripts        = "warn"
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Servir des fichiers bruts du tout. |
| `max_size_bytes` | u64 | `10485760` | Plafond d'un fichier brut ; le flux s'arrête là, donc le fichier est refusé plutôt que tronqué. `0` avec `enabled` est une erreur de configuration, et une valeur supérieure à `[limits].max_artifact_size_bytes` est refusée parce que le plafond global gagnerait silencieusement. |
| `repos` | chaîne[] | `[]` | Des motifs glob `owner/repo` (`cli/*`). Vide autorise tout dépôt ; la liste restreint et n'élargit jamais. Une entrée malformée est une erreur de configuration. |
| `require_pinned` | booléen | `false` | Refuser une ref de branche (`PINNED_REF_REQUIRED`) : un contenu brut qui change sous la même URL est ce dont un parc figé ne veut pas. |
| `scripts` | chaîne | *voir plus bas* | `warn`, `deny` ou `ignore` pour les charges shell, PowerShell, Python et batch (`RAW_SCRIPT`). **Absent signifie `deny` quand le registre a un `[registries.security]`**, et `warn` sinon. |

> `scripts` regarde toujours l'extension du fichier, et sous `deny` ses premiers
> octets également — une charge sans extension commençant par un shebang est donc
> refusée aussi. Le défaut à `deny` sous un profil de sécurité est la RFC 0019
> §11 q2 :
> choisir une quarantaine, c'est choisir « rien de non analysé n'est servi », et
> un simple fichier de script est le seul artefact qu'aucun scanner ne lit.

**`[registries.api_reads]` — les routes JSON typées en lecture seule (types de
forge uniquement ; RFC 0019) :** {#registries-api-reads}

```toml
[registries.api_reads]
families = ["tags", "commits", "branches"]
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `families` | chaîne[] | `[]` | Parmi `tags`, `commits`, `branches`. Toute autre valeur est une erreur de configuration — `contents` et `git/blobs` sont du contenu brut par une autre porte, et cette décision vit dans `[registries.raw]`. |

> Chaque famille ajoute un `GET` sous
> `/proxy/<registry>/<owner>/<repo>/`, qui répond dans la forme propre de
> BatleHub plutôt que dans celle de la forge : aucune URL amont à suivre, aucun
> champ dont le sens change d'une forge à l'autre. Une famille que le registre
> n'a pas demandée répond `404`. Séparément et toujours, les **documents de
> release** — la liste et la release par tag, sur les trois forges — voient leurs
> `tarball_url`, `zipball_url` et URL de téléchargement d'assets redirigés vers ce
> proxy : un client qui lit le document plutôt que de construire un chemin reste
> donc derrière la politique, le cache et la piste d'audit.

**`[registries.security]` — quarantaine et verdicts (facultatif) :** {#registries-security}

Fait entrer le registre dans la couche de chaîne d'approvisionnement de la
RFC 0018. Toute version que ce registre sert porte alors un **verdict** —
`allowed`, `warned`, `quarantined` ou `denied` — calculé depuis son âge, les
constats des scanners, les blocages de l'opérateur et un éventuel verdict de SOC.
Une version dont le verdict n'est pas servi est refusée sur le chemin de
téléchargement, avec ses codes de motif, jusqu'à ce que le verdict change. Un
registre sans cette section n'est pas touché.

```toml
[registries.security]
mode                   = "block"     # "block" | "warn"
min_age_secs           = 259200      # jamais servi en dessous de cet âge ; plancher 3600
mature_age_secs        = 2592000     # servi `warned` au-dessus de cet âge tant qu'une analyse est en attente
hold_missing_timestamp = true        # retenir une version que l'amont n'a pas datée
scanners               = ["osv"]     # des noms de [scanners] ; "osv" n'a pas besoin d'être déclaré
required_scanners      = ["osv"]     # tous doivent avoir répondu avant que la version soit servie
max_severity           = "high"      # les constats à ce niveau ou au-dessus refusent (block) ou avertissent
require_provenance     = false
deny_install_hooks     = "warn"      # "deny" | "warn" | "ignore"
scanner_error          = "quarantine" # "quarantine" | "warn" | "ignore"

pullers_window_days    = 30          # jusqu'où une alerte de bascule remonte pour nommer qui a récupéré la version

[registries.security.rescan]         # le minuteur de réanalyse (RFC 0018 phase 4)
interval_secs = 0                    # > 0 : tout verdict plus ancien est réanalysé
on_webhook    = true
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `mode` | chaîne | `"block"` | `block` refuse une version dont le verdict est `quarantined` ou `denied` ; `warn` la sert avec le verdict visible. `BLOCK_LIST` et `SOC_VERDICT` refusent dans les deux modes. |
| `min_age_secs` | u64 | `86400` | En dessous de cet âge, une version est retenue (`MIN_AGE_NOT_MET`), quoi que disent les scanners. En dessous de `3600`, c'est une erreur de configuration : une heure est tout l'intérêt de la quarantaine. |
| `mature_age_secs` | u64 | `86400` | Au-dessus de cet âge, une version dont l'analyse n'est pas revenue est servie `warned` (`SCAN_PENDING`) et analysée derrière la requête. `0` ne sert jamais rien de non analysé. Doit valoir au moins `min_age_secs`. Avec les deux à leur défaut, la fenêtre de retenue par analyse est vide — le profil de production recommandé est 3 jours / 30 jours. |
| `hold_missing_timestamp` | booléen | `true` | Retient une version que l'amont n'a pas datée (`TIMESTAMP_MISSING`, sans terme : pas d'`available_at`, et le contournement par maturité ne l'atteint pas). `false` lui fait sauter le garde-fou d'âge, comme le fait `release_age_gate` par défaut. Sur les types proxifiés par chemin (`deb`, `rpm`, `pacman`, `generic`, `jetbrains`), aucune version n'est datée : `true` retient donc tout et lève `security.timestamp-hold-unavailable`. |
| `scanners` | chaîne[] | `["osv"]` | Les scanners que le worker exécute sur ce registre. Chaque nom est une entrée `[scanners.<name>]` ; `osv` est implicite. Tous les types que la RFC nomme sont construits : `osv`, `postmortem`, `guarddog`, `trivy`, `sigstore`, et les deux services externes `socket` (Socket.dev, un appel par coordonnée, exige une `api_key`) et `mlab` (l'API CVE de mlab.sh, un *enrichissement* : elle attache CVSS, EPSS et CISA KEV aux constats de vulnérabilité produits par les autres et élève une CVE listée au KEV en `critical` ; elle ne crée jamais de constat, donc la lister sous `required_scanners` déclenche un avertissement). Un scanner répond sous sa clé de configuration : un second `osv` pointé vers une autre `api_url` est donc son propre nom. |
| `required_scanners` | chaîne[] | `["osv"]` | Doivent tous avoir répondu avant que la version soit servie. Doit être un sous-ensemble de `scanners`. Vide avec `mode = "warn"` lève `security.unprotected` : rien ne peut plus jamais retenir une version. |
| `max_severity` | chaîne | `"high"` | `low`, `medium`, `high` ou `critical`. Un constat à ce niveau ou au-dessus produit `denied` en mode `block` et `warned` en mode `warn`. |
| `require_provenance` | booléen | `false` | Une version sans attestation de provenance vaut `PROVENANCE_MISSING` (un constat en `high`). N'a de sens qu'avec un scanner qui contrôle la provenance (`sigstore`, phase 3). |
| `deny_install_hooks` | chaîne | `"warn"` | Ce qu'est un hook d'installation (un `preinstall` npm, un `setup.py` Python) : `deny`, `warn` ou `ignore`. Lu par les scanners d'archive de la phase 3. |
| `scanner_error` | chaîne | `"quarantine"` | Un scanner qui ne peut pas répondre après `[worker].max_attempts` : `quarantine` retient la version (`SCANNER_ERROR`, borné dans le temps), `warn` la sert avertie, `ignore` abandonne le constat. |
| `pullers_window_days` | u32 | `30` | Quand une réanalyse fait passer une version *déjà servie* en `denied`, la notification `verdict_changed` nomme toutes les identités qui l'ont récupérée dans cette fenêtre, lues dans le journal d'accès (RFC 0018, décision 23) ; c'est aussi la fenêtre par défaut de `GET /api/v1/verdicts/{registry}/{name}/{version}/pullers` et de `batlehub verdicts pullers`. Les récupérations anonymes sont conservées sous `ip:<addr>`. |
| `rescan.interval_secs` | u64 | `0` | `> 0` : l'ordonnanceur de réanalyse — un par parc, élu par un verrou consultatif PostgreSQL — met en file une `Rescan` (sous `FirstSeen` et `Webhook`, au-dessus de `Backfill`) pour tout verdict de ce registre dont la dernière analyse est plus ancienne que l'intervalle. Un verdict qui bascule de servi à `denied` lève l'alerte ci-dessus ; une retenue qui se lève lève `artifact_released` vers les identités à qui la version avait été refusée. `0` ne réanalyse jamais sur horloge ; `POST …/rescan` et le webhook `security.rescan` le font toujours. |

> **Où vont les règles.** `min_age_secs` *remplace* une règle
> `release_age_gate` sur ce registre — déclarer les deux est une erreur de
> configuration. Les règles `cve_gate`, `license_gate`,
> `require_signed_release` et `trusted_publisher`, ainsi que la liste de blocage
> de l'administrateur, ne sont plus exécutées comme des règles sur ce registre :
> elles tournent comme des scanners internes dont le refus devient un constat
> (`VULNERABILITY`, `LICENSE_DENIED`, `SIGNATURE_MISSING`,
> `UNTRUSTED_PUBLISHER`, `BLOCK_LIST`), de sorte qu'il y ait une décision par
> version et aucune règle capable de laisser passer à côté. `deny_latest` et
> `version_gate` restent dans la chaîne ; elles jugent la requête, pas
> l'artefact.
>
> **Qui voit pourquoi.** Un téléchargement refusé est un simple `403` pour tout
> le monde ; les codes de motif et l'indication `batlehub why` du corps exigent
> `quarantine:read` (accordé à `user` et `admin` par défaut), et les constats
> derrière eux exigent `findings:read` (`admin`). Une dérogation d'opérateur est
> une `GateExemption` sur le garde-fou `security_verdict` (`gates:exempt`) : elle
> transforme une retenue en `warned`, jamais en `allowed`, de sorte qu'elle reste
> visible.
>
> **Ce qui doit aussi être vrai.** Tout webhook
> `[[notifications.inbound]]` doit porter un `secret` dès qu'un registre a cette
> section — un événement `security.*` sur un webhook non signé permettrait à
> quiconque sur le réseau de refuser des paquets. Et un processus ayant `worker`
> parmi ses rôles doit exister quelque part : un proxy qui met en file des
> travaux que personne ne défile retient toute nouvelle version jusqu'à
> `mature_age_secs`, et journalise un avertissement au démarrage quand il est
> seul.

**`[[registries.rules]]` — le garde-fou d'âge de publication :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"release_age_gate"` |
| `min_age_secs` | u64 | `3600` | Les publications plus jeunes sont refusées. |
| `bypass_roles` | chaîne[] | `[]` | Les rôles qui sautent entièrement le garde-fou, contrôle d'horodatage manquant compris (par exemple `["admin"]`). |
| `deny_missing_timestamp` | booléen | `false` | À `true`, refuse le téléchargement des paquets dont l'amont ne fournit pas d'horodatage de publication, plutôt que de sauter le contrôle et d'autoriser. Utile pour des registres comme conda, où le champ d'horodatage est facultatif — le mettre à `true` garantit que chaque paquet porte un âge vérifiable. |

> **La prise en charge des horodatages par type de registre :** le garde-fou ne
> s'applique que si l'amont fournit un horodatage de publication.
> - **npm**, **Cargo**, **OpenVSX**, **place de marché VS Code**, **Go**,
>   **PyPI** — horodatage toujours renseigné ; le garde-fou s'applique
>   entièrement.
> - **GitHub** — horodatage renseigné uniquement pour les requêtes de release par
>   tag précis (téléchargements d'assets). Les fichiers bruts, les tarballs de
>   sources et les listes de releases n'en renvoient pas ; le garde-fou est sauté
>   pour ces requêtes.
> - **Conda** — l'horodatage est le champ `timestamp` (en millisecondes depuis
>   l'époque) de `repodata.json`. La plupart des paquets le portent, mais les
>   paquets anciens ou tiers peuvent l'omettre. Employez
>   `deny_missing_timestamp = true` pour rejeter les paquets sans date de build
>   vérifiable.
> - **Providers Terraform** — horodatage renseigné par
>   `registry.terraform.io`, mais non imposé par la spécification officielle ;
>   d'autres registres Terraform peuvent l'omettre.
> - **Distributions Node (`nodedist`)** — la date de publication est lue dans
>   `index.tab` : les publications courantes portent donc un horodatage ; une
>   publication que l'index ne liste plus atteint le garde-fou sans date. Sur ce
>   type, `deny_missing_timestamp` est **obligatoire** : une règle
>   `release_age_gate` qui en est dépourvue est une erreur de configuration,
>   parce que ce champ décide du garde-fou pour toute publication retirée du
>   listing et qu'aucune des deux réponses n'est un défaut que ce serveur choisit
>   à votre place (RFC 0010 §6.7). `true` refuse les publications retirées,
>   `false` les sert.
> - **SDKMAN (`sdkman`)** — le protocole ne publie aucune date : tout artefact
>   atteint donc le garde-fou sans horodatage, et `deny_missing_timestamp` *est*
>   la règle : `true` refuse tout téléchargement sur le registre, `false` rend le
>   garde-fou inerte. Obligatoire ici pour la même raison, et un bloc
>   `[registries.security]` retient plutôt par défaut sur un horodatage manquant.

**`[[registries.rules]]` — exiger une publication signée :**

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"require_signed_release"` |
| `enabled` | booléen | `false` | À `true`, refuse les publications sans signal de signature (sous réserve de `deny_missing_signature` ci-dessous) |
| `bypass_roles` | chaîne[] | `[]` | Les rôles qui sautent entièrement le garde-fou (par exemple `["admin"]`). |
| `deny_missing_signature` | booléen | `false` | À `true`, refuse les publications des registres qui ne rapportent aucun signal de signature, plutôt que de sauter le contrôle et d'autoriser. |

> Cette règle contrôle `PackageMetadata.is_signed`, un signal au mieux, renseigné
> par chaque adaptateur de registre — ce n'est pas une vérification
> cryptographique complète. GitHub, Forgejo, GitLab, OpenVSX et la place de
> marché VS Code le renseignent (présence d'un asset `.asc` ou `.sig`, ou d'un
> blob de signature d'extension) ; les registres dont l'écosystème n'a pas de
> notion de signature (npm, PyPI, crates.io, Maven, RubyGems, Conda, Composer,
> Go, Terraform, NuGet, deb, rpm, pacman) renvoient `None` et sont laissés passer,
> sauf si `deny_missing_signature = true`.
>
> **Sur un registre `local` ou `hybrid`, associez-la à
> `[registries.signing] required = true`.** Le paragraphe ci-dessus décrit les
> artefacts *proxifiés*. Une version **publiée localement** est différente : elle
> est enregistrée comme non signée — et non comme inconnue — dès que la requête
> de publication ne portait pas d'en-tête `X-Artifact-Signature`, et cette règle
> refuse le non signé purement et simplement. `deny_missing_signature` n'adoucit
> pas cela ; il ne gouverne que le cas inconnu. Activer cette règle pour filtrer
> la moitié proxifiée d'un registre hybrid fait donc aussi échouer toute
> publication locale non signée **au téléchargement**, avec un `403` qui atteint
> le consommateur plutôt que le publieur qui aurait pu corriger.
> `signing.required = true` refuse le même artefact dès la requête de publication.
> Configurer l'un sans l'autre lève `require-signed-release.unsigned-publishes`
> sur la page d'administration **Rechargement de la configuration** et sur
> `GET /api/v1/admin/config/warnings`.

**`[[registries.rules]]` — refuser `latest` :**

Rejette toute requête qui emploie `"latest"` comme tag de version, ce qui force
les consommateurs à épingler des versions explicites (hygiène de chaîne
d'approvisionnement).

```toml
[[registries.rules]]
kind = "deny_latest"
bypass_roles = ["admin"]   # à omettre ou laisser vide pour un blocage strict
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"deny_latest"` |
| `bypass_roles` | chaîne[] | `[]` | Les rôles qui peuvent encore demander `"latest"` (par exemple `["admin"]`). Quand plusieurs rôles sont listés, le moins privilégié fixe le plancher d'accès. Vide, le blocage s'applique à tous les rôles. |

> Cette règle vaut pour tous les types de registre. `"latest"` est la chaîne de
> version littérale envoyée par le client — pour npm elle correspond au dist-tag
> `latest`, pour Cargo et Go elle déclenche une résolution `@latest` en amont, et
> pour OpenVSX et la place de marché VS Code elle récupère la version publiée du
> moment.

**`[[registries.rules]]` — le garde-fou de version :**

Filtre les téléchargements par version, au moyen d'une liste facultative de
versions approuvées et d'une liste de versions bloquées aux problèmes connus. La
version résolue est confrontée aux deux listes : une correspondance dans `block`
est toujours rejetée, et quand `allow` n'est pas vide, une version qui ne
correspond à **aucune** de ses entrées est rejetée elle aussi. `block` prime sur
`allow`.

```toml
[[registries.rules]]
kind = "version_gate"
allow = [">=1.2.0, <2.0.0"]   # facultatif : quand il est défini, seules les versions correspondantes sont servies
block = ["1.4.7", "1.5.0"]    # versions précises aux problèmes connus
bypass_roles = ["admin"]
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"version_gate"` |
| `allow` | chaîne[] | `[]` | La liste des versions approuvées. Non vide, une version qui ne correspond à aucune entrée est rejetée. Vide, toutes les versions sont autorisées (sous réserve de `block`). |
| `block` | chaîne[] | `[]` | La liste des versions (ou plages) aux problèmes connus. Une correspondance est toujours rejetée. |
| `bypass_roles` | chaîne[] | `[]` | Les rôles qui peuvent contourner le garde-fou (par exemple `["admin"]`). Quand plusieurs sont listés, le moins privilégié fixe le plancher d'accès. Vide, le garde-fou s'applique à tous les rôles. |

> **La correspondance :** chaque entrée est traitée comme une plage semver quand
> elle contient un opérateur de plage (`<`, `>`, `=`, `^`, `~`, `*`, `,`) et
> s'analyse comme un [`VersionReq`](https://docs.rs/semver/) valide (par exemple
> `">=1.2.0, <2.0.0"`) ; sinon, elle est comparée par **égalité de chaîne
> exacte**. Cela garde un `"1.2.3"` nu exact (plutôt que la sémantique de curseur
> `^1.2.3` que semver déduirait) et permet de lister verbatim des chaînes de
> version non semver (empreintes git, dates).

**`[[registries.rules]]` — le garde-fou de CVE :**

Refuse le téléchargement des versions portant un constat de vulnérabilité
enregistré, au niveau de gravité seuil ou au-dessus. Exige un scanner de
vulnérabilités configuré — voir
[Vulnerability scanning](/contributing/security-scanning) (en anglais) pour la
mise en place complète.

```toml
[[registries.rules]]
kind         = "cve_gate"
min_severity = "high"        # parmi : low, medium, high, critical (défaut : high)
bypass_roles = ["admin"]
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"cve_gate"` |
| `min_severity` | chaîne | `"high"` | La gravité minimale qui déclenche un blocage : `"low"`, `"medium"`, `"high"` ou `"critical"`. |
| `bypass_roles` | chaîne[] | `[]` | Les rôles exemptés du garde-fou. |

**`[[registries.rules]]` — le garde-fou de licence :**

Refuse un téléchargement d'après la licence que déclare le **manifeste du paquet
lui-même**. La licence est lue dans l'archive par l'extracteur de SBOM au moment
où l'artefact est mis en cache ou publié : cette règle n'a donc besoin d'aucun
flux externe ni d'appel amont supplémentaire.

```toml
[[registries.rules]]
kind          = "license_gate"
allow         = ["MIT", "Apache-2.0", "BSD-3-Clause"]  # liste d'autorisation facultative
deny          = ["AGPL-3.0", "SSPL-1.0"]               # toujours refusées
allow_unknown = true                                    # défaut
block         = true                                    # défaut false = simple avertissement
bypass_roles  = ["admin"]
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"license_gate"` |
| `allow` | chaîne[] | `[]` | Les licences approuvées. Non vide, une licence déclarée qui n'y figure pas est refusée. Vide signifie « pas de liste d'autorisation », pas « rien n'est autorisé ». |
| `deny` | chaîne[] | `[]` | Les licences refusées. Contrôlée avant `allow` : une licence présente dans les deux est donc refusée. |
| `allow_unknown` | booléen | `true` | Le traitement d'une version dont la licence est inconnue. `true` la laisse passer ; `false` la refuse. |
| `block` | booléen | `false` | `false` = simple avertissement : la licence est affichée dans la console et rien n'est jamais refusé. |
| `bypass_roles` | chaîne[] | `[]` | Les rôles exemptés du garde-fou. |

> **Cette règle exige `[registries.sbom]` avec `enabled = true` sur le même
> registre.** La licence est lue dans l'archive au cours de la génération du
> SBOM : avec le SBOM désactivé, rien n'est jamais extrait et le garde-fou voit
> une licence inconnue pour chaque version — quelle que soit la qualité de
> l'analyseur pour ce type de registre. La configurer sans SBOM lève
> `license-gate.sbom-disabled`.
>
> **La première requête sur un paquet non mis en cache ne peut pas être
> filtrée.** La licence vit à l'intérieur de l'archive : elle est donc
> enregistrée *au passage* dans le proxy — après la première récupération, pas
> avant. Avec le défaut `allow_unknown = true`, le premier téléchargement passe
> et tous les suivants sont filtrés ; avec `allow_unknown = false`, rien
> d'inconnu n'est servi, ce qui coûte une requête refusée par nouveau paquet.
> C'est le même compromis que fait `integrity.require_metadata` pour les sommes
> de contrôle.
>
> L'extraction de licence couvre aujourd'hui **cargo, npm, maven, pypi et
> nuget**. Tout autre type de registre signale une licence inconnue en
> permanence, donc `allow_unknown = false` sur l'un d'eux refuse tout.
>
> Configurer `license_gate` sur un type de registre sans analyseur lève un
> avertissement de configuration — `license-gate.no-extractor` quand la règle est
> simplement inerte, ou `license-gate.denies-everything` quand `block = true` et
> `allow_unknown = false` se combinent pour refuser tout téléchargement. Les deux
> apparaissent sur la page d'administration **Rechargement de la configuration**
> et sur `GET /api/v1/admin/config/warnings`, parce qu'aucun de ces états ne
> produit d'erreur à l'exécution : la configuration est valide, la règle est
> chargée, et elle ne peut simplement pas voir ce qu'elle prétend gouverner.

La comparaison est insensible à la casse et ignore les espaces autour. Elle est
littérale par ailleurs : `allow = ["MIT"]` ne correspond **pas** à un paquet qui
déclare `MIT OR Apache-2.0`, parce qu'une expression composée est une autre
déclaration — l'accepter permettrait à n'importe quel paquet de sortir du
garde-fou en ajoutant une alternative.

La règle filtre la licence du paquet lui-même, pas celle de ses dépendances :
BatleHub ne résout pas de graphe de dépendances, il ne peut donc pas répondre à
une question sur les licences transitives et ne prétend pas le faire.

**`[[registries.rules]]` — le publieur de confiance :**

Restreint les téléchargements aux paquets publiés par une organisation, un
utilisateur ou un scope autorisés. Le publieur est dérivé de métadonnées déjà
résolues pendant la récupération par le proxy — aucun appel amont
supplémentaire.

```toml
[[registries.rules]]
kind = "trusted_publisher"
allow = ["my-org", "trusted-user"]
bypass_roles = ["admin"]
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `kind` | chaîne | — | Doit valoir `"trusted_publisher"` |
| `allow` | chaîne[] | `[]` | Les identifiants de publieur autorisés. Non vide, un paquet dont le publieur dérivé n'y figure pas est rejeté. Vide, la règle autorise tout. |
| `bypass_roles` | chaîne[] | `[]` | Les rôles qui peuvent contourner le garde-fou (par exemple `["admin"]`). |

> **La prise en charge du publieur par type de registre :** la comparaison est
> insensible à la casse.
> - **GitHub**, **GitLab**, **Forgejo** — le segment propriétaire ou groupe de
>   premier niveau du chemin du paquet (`"owner/repo"` ou
>   `"group/subgroup/project"` → `"owner"` / `"group"`).
> - **npm** — le scope pour les paquets à scope (`"@scope/name"` → `"scope"`) ;
>   sinon l'utilisateur qui a publié cette version.
> - **OpenVSX**, **place de marché VS Code** — le segment d'éditeur de
>   l'identifiant d'extension (`"publisher.extension"` → `"publisher"`).
> - **Pas encore pris en charge : Cargo** (la propriété d'une crate n'est pas
>   dans l'index sparse et demanderait un appel séparé à l'API de crates.io) ni
>   aucun autre type. Configurer cette règle sur un registre non pris en charge
>   **refuse toutes les requêtes** — c'est un garde-fou de chaîne
>   d'approvisionnement qui échoue en se fermant, pas en s'ouvrant.

#### `signed_downloads` {#registry-signed-downloads}

Certains clients authentifient les *documents* d'une installation puis récupèrent
l'*artefact* que l'un d'eux nomme sans aucun identifiant, parce que leur
protocole n'a aucun moyen d'en envoyer. Terraform est le cas mesuré : contre la
1.8.5, chaque document de protocole est authentifié et aucune récupération
d'artefact ne l'est — l'archive du provider, ses `SHA256SUMS` et la signature
détachée qui les couvre, sur n'importe quel hôte, y compris celui qu'il a
authentifié une requête plus tôt.

Sans aide, le seul levier est
`anonymous = ["releases:read", "source:read"]`, qui porte sur le *registre* :
ouvrir la dernière étape de l'installation d'un provider ouvre toutes ses listes
de versions, et en mode hybrid tout ce qui est publié localement.

`signed_downloads = true` inscrit une signature à courte durée de vie, portant
sur une seule coordonnée, dans le document que le client *a bien* authentifié, et
l'accepte sur les récupérations qui ne portent aucun en-tête. Le registre peut
alors mettre `anonymous = []`.

```toml
[server.signed_urls]
secret = "${BATLEHUB_URL_SIGNING_SECRET}"

[[registries]]
type             = "terraform"
name             = "internal-tf"
signed_downloads = true

[registries.rbac]
anonymous = []
user      = ["releases:read", "source:read"]
```

La signature **authentifie une requête et n'autorise rien**. Elle tient lieu de
l'en-tête `Authorization` et ne change rien d'autre : la même chaîne de règles,
la même liste de blocage, le même garde-fou d'âge, le même garde-fou de licence,
le même contrôle de visibilité, le même quota et le même audit s'exécutent contre
l'identité qu'elle porte. Une version bloquée après l'émission d'une URL reste
bloquée, parce que le blocage est évalué au moment où l'URL est présentée, pas
au moment où elle est émise.

Implémenté aujourd'hui pour `terraform`. Le définir sur un autre type de registre
est accepté et ne fait rien, puisqu'aucun autre adaptateur n'a de route qui
émette.

Voir [`[server.signed_urls]`](#server-signed-urls) pour la clé, sa rotation et la
note sur l'apparition du token dans les logs.

#### `[registries.integrity]` {#integrity}

La vérification d'intégrité des artefacts, registre par registre. Sur le chemin
« récupérer puis mettre en cache » du proxy, les octets amont mis en tampon sont
hachés et comparés à la somme de contrôle annoncée par les métadonnées du
registre (SHA-256 pour Cargo, SRI ou `shasum` pour npm, SHA-256 pour PyPI). Les
registres qui n'annoncent aucune somme (NuGet, Maven, GitHub, Go, …) tombent dans
le chemin « manquante ». Ne s'applique **pas** aux registres `firewall_only`, qui
diffusent sans mise en tampon.

```toml
[registries.integrity]
enabled = true            # vérifier quand une somme de contrôle est annoncée
block_on_mismatch = true  # faire échouer le téléchargement sur une non-correspondance (jamais contournable)
require_metadata = false  # refuser les téléchargements sans somme annoncée
bypass_roles = ["admin"]  # les rôles exemptés du garde-fou require_metadata
verify_on_serve = false   # re-hacher les octets stockés à chaque service, pas seulement à la première récupération
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `true` | L'interrupteur principal. À `false`, aucune vérification n'est faite. |
| `block_on_mismatch` | booléen | `true` | Faire échouer le téléchargement (et sauter la mise en cache) quand l'empreinte calculée ne correspond pas à celle annoncée. Une non-correspondance n'est jamais contournable. |
| `require_metadata` | booléen | `false` | Refuser les téléchargements pour lesquels l'amont n'annonce aucune somme utilisable, sauf si l'appelant détient l'un des `bypass_roles`. Simple avertissement par défaut. |
| `bypass_roles` | chaîne[] | `[]` | Les rôles autorisés à contourner le garde-fou `require_metadata`. |
| `verify_on_serve` | booléen | `false` | Revérifier les octets en cache ou stockés contre un SHA-256 **calculé par nous** (enregistré à la première mise en cache) à chaque service — les hits de cache du chemin proxy et les lectures du registre local — et pas seulement à la première récupération. Attrape une corruption de stockage ou l'altération d'artefacts déjà en cache. Une non-correspondance fait échouer le téléchargement (`502`) et évince la mauvaise entrée, de sorte qu'une requête ultérieure récupère des octets sains. Désactivé par défaut parce qu'il lit et hache les octets à chaque service (le chemin proxy les diffuse à travers le hachage, la mémoire reste donc bornée, puis rouvre l'entrée pour la servir). Les lignes de cache préexistantes n'ont pas de somme stockée et sont traitées comme « à ne pas revérifier » jusqu'à leur prochain rafraîchissement. |

#### `[registries.signing]` {#signing}

La signature d'artefacts, registre par registre. À la publication, un client
fournit une signature détachée par les en-têtes `X-Artifact-Signature` et
`X-Signature-Type`, stockée à côté de l'artefact. **Les deux en-têtes vont
ensemble** : l'un sans l'autre est rejeté par un `400`, parce qu'une signature
qui ne nomme aucun algorithme ne pourra jamais être vérifiée et qu'un type qui ne
nomme aucune signature ne décrit rien. Les champs `required` et `allowed_types`
filtrent la **présence et le type** de signature à la publication ;
`verify_on_download` et `trusted_keys` revérifient une signature `ed25519`
stockée au **téléchargement**.

```toml
[registries.signing]
required = false                 # rejeter les publications sans en-tête X-Artifact-Signature
allowed_types = ["ed25519"]      # types de signature acceptés ; vide = n'importe lequel (ou aucun)
verify_on_download = false       # revérifier une signature ed25519 stockée à chaque téléchargement
trusted_keys = ["<hex pubkey>"]  # clés publiques Ed25519 de 32 octets, en hexadécimal, à qui l'on fait confiance
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `required` | booléen | `false` | Rejeter les requêtes de publication sans en-tête `X-Artifact-Signature`. Une signature est toujours une paire : une publication qui n'en porte qu'un des deux en-têtes est refusée quelle que soit cette valeur. |
| `allowed_types` | chaîne[] | `[]` | Les types de signature acceptés (par exemple `["pgp", "ed25519"]`). Vide, tout type est accepté. Une publication *non signée* est gouvernée par `required` seul — cette liste ne rend jamais une signature obligatoire. |
| `verify_on_download` | booléen | `false` | Vérifier une signature détachée `ed25519` stockée contre `trusted_keys` à chaque téléchargement (lectures du registre local). Une signature stockée qui échoue à la vérification — ou signée par une clé non fiable, ou d'un type que l'on ne sait pas vérifier, ou qui ne nomme **aucun** type — fait échouer le téléchargement par un `502` : avec ce réglage actif, un artefact qui porte une signature non vérifiable est refusé plutôt que servi. Un artefact sans signature stockée n'est pas vérifié ici ; sa présence est gouvernée par `required` à la publication. |
| `trusted_keys` | chaîne[] | `[]` | Les clés publiques Ed25519 de 32 octets, en hexadécimal, à qui l'on fait confiance pour signer les artefacts de ce registre. Un téléchargement vérifie contre chacune à son tour ; toute correspondance passe. |

> **Pourquoi Ed25519 seulement ?** La cryptographie fondée sur RSA (la crate
> `rsa`, et donc PGP, x509 et les chemins Sigstore par défaut) est bannie en dur
> de l'arbre de dépendances par `deny.toml` (RUSTSEC-2023-0071). La vérification
> de signature détachée Ed25519 garde l'arbre sans RSA ; la vérification Sigstore
> et de provenance npm est laissée à plus tard pour cette raison.

#### `[registries.vsx_signing]` {#vsx-signing}

La signature propre au registre sur chaque VSIX qu'il publie
([RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry)), pour les
registres `vscode-marketplace` et `openvsx`. La vue Extensions d'un VS Code
récent grise le bouton Installer sur toute entrée de galerie sans asset de
signature — *This extension is not signed by the Extension Marketplace* — et un
registre qui détient une clé en sert une pour tout ce qu'il héberge, dans la
forme d'archive qu'emploie Open VSX : une signature Ed25519 sur tout le `.vsix`,
un manifeste de ses entrées, un `.signature.p7s` vide. Ce dont il fait proxy
depuis un amont qui signe (la place de marché Microsoft, une instance Open VSX)
est relayé avec la signature de cet amont, qu'une clé soit configurée ou non, et
n'est jamais re-signé.

```toml
[registries.vsx_signing]
seed_hex = "${VSX_SIGNING_SEED}"   # graine Ed25519 de 32 octets, en hexa — `batlehub-cli vsx keygen` en imprime une
key_id   = "2026-09"               # facultatif ; défaut : les 16 premiers caractères hexa de SHA-256(clé publique)
```

| Champ | Type | Défaut | Description |
|-------|------|---------|-------------|
| `seed_hex` | chaîne | — | La graine, 64 caractères hexadécimaux. Un secret de la même classe que `repo_signing.seed_hex` : gardez-la hors du fichier avec `${VAR}`. Rejetée au chargement si ce ne sont pas 32 octets hexadécimaux. |
| `key_id` | chaîne | dérivé | L'identifiant sous lequel la clé publique est servie, `GET /proxy/{registry}/api/-/public-key/{key_id}` (en PEM, anonyme, mis en cache une journée). C'est un segment de chemin d'URL : `[A-Za-z0-9._-]`. Doit changer quand la clé change ; le défaut le dérive de la clé, donc c'est le cas. |

**Ce que cela fait.** À la publication, le registre écrit l'archive de signature
à côté de l'artefact, et la galerie annonce
`Microsoft.VisualStudio.Services.VsixSignature` et `…PublicKey` pour la version ;
le document Open VSX porte `files.signature` et `files.publicKey`. Une version
publiée avant l'existence de la clé est signée à la première requête sur son
archive ; une clé qui tourne re-signe de la même façon, et la clé servie vérifie
toujours l'archive servie. Sur un registre en mode `proxy`, la clé ne signe rien
(rien n'y est publié) et un avertissement le dit. Une version dont l'archive de
signature a été **fournie** — celle d'un amont, attachée après la publication par
`PUT …/{extension_id}/{version}/vsix/signature` (voir la
[page Open VSX](/fr/registries/openvsx#signatures)) — n'est jamais re-signée : le
registre sert cette archive telle quelle et n'annonce aucune clé pour elle.

**Ce que cela ne fait pas.** Faire passer le vérificateur propre à un VS Code
d'origine : celui-là accepte la signature de la place de marché et aucune autre,
donc sur une version d'origine le bouton Installer de la vue s'allume et
l'installation exige `extensions.verifySignature: false` — le réglage que
code-server, VSCodium et che-code livrent désactivé. La
[page de la CLI](/fr/use/cli#gallery-proxy) dit où il se place ;
`batlehub-cli vsx verify` est le contrôle qui le remplace.

#### `[registries.upstream_auth]` {#upstream_auth}

Les identifiants à envoyer à chaque requête amont de ce registre. Trois schémas
sont pris en charge ; choisissez-en un.

**Token bearer** — ajoute `Authorization: Bearer <token>`. Accepté par Gitea,
Forgejo, Nexus (token npm), JFrog Artifactory et GitHub Enterprise.

```toml
[registries.upstream_auth]
type  = "bearer"
token = "npat-xxxx"
```

**Authentification basic** — l'authentification HTTP Basic standard.

```toml
[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "s3cr3t"
```

**En-tête personnalisé** — envoie un en-tête arbitraire à chaque requête. Utile
pour les registres qui emploient `X-API-Key` ou un schéma similaire.

```toml
[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "my-api-key"
```

| Champ | Type | Schémas | Notes |
|---|---|---|---|
| `type` | chaîne | tous | `"bearer"`, `"basic"` ou `"header"` |
| `token` | chaîne | bearer | La valeur du token bearer |
| `username` | chaîne | basic | Le nom d'utilisateur HTTP Basic |
| `password` | chaîne | basic | Le mot de passe HTTP Basic |
| `name` | chaîne | header | Le nom de l'en-tête HTTP (par exemple `"X-API-Key"`) |
| `value` | chaîne | header | La valeur de l'en-tête HTTP |

> **Sécurité :** ne versionnez jamais d'identifiants.
> Employez des marqueurs `${VAR_NAME}` dans le fichier de configuration pour
> tirer les secrets de variables d'environnement au démarrage — voir
> [§5 Surcharges par variables d'environnement](#_5-environment-variable-overrides).

#### `[registries.tls]` {#upstream_tls}

Les réglages TLS des connexions amont. Employez-les quand le registre amont
présente un certificat signé par une autorité privée ou auto-hébergée, absente du
magasin de confiance du système.

```toml
[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"
```

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `ca_cert_path` | chaîne | non | Le chemin d'un certificat d'autorité au format PEM, à ajouter comme racine de confiance pour les connexions amont de ce registre |

> Le certificat est chargé une fois au démarrage. Pour faire tourner un
> certificat d'autorité, redémarrez le serveur.

---

#### `[registries.proxy]` {#upstream_proxy}

Router toutes les requêtes sortantes vers les registres amont par un proxy HTTP,
HTTPS ou SOCKS5. Employez-le dans un environnement d'entreprise ou coupé du
réseau, où l'accès direct à Internet est restreint.

```toml
[registries.proxy]
url = "http://proxy.corp.example.com:3128"

# Facultatif : identifiants du proxy (alternative à leur inclusion dans l'URL)
# username = "proxyuser"
# password = "${PROXY_PASSWORD}"

# Facultatif : contourner le proxy pour certains hôtes ou domaines (séparés par des virgules).
# Équivalent à la variable d'environnement NO_PROXY.
# no_proxy = "localhost,10.0.0.0/8,internal.example.com"
```

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `url` | chaîne | oui | L'URL du proxy. Gère les schémas `http://`, `https://` et `socks5://`. Les identifiants peuvent y être inclus directement : `http://user:pass@proxy:3128`. |
| `username` | chaîne | non | Le nom d'utilisateur en authentification Basic du proxy. Remplace tout identifiant inclus dans `url`. Employez `${VAR}` pour l'injecter depuis une variable d'environnement. |
| `password` | chaîne | non | Le mot de passe en authentification Basic du proxy. Remplace tout identifiant inclus dans `url`. Employez `${VAR}` pour l'injecter depuis une variable d'environnement. |
| `no_proxy` | chaîne | non | Une liste, séparée par des virgules, d'hôtes, de domaines ou de plages CIDR pour lesquels contourner le proxy (par exemple `"localhost,10.0.0.0/8,corp.example.com"`). Équivalent à la variable d'environnement standard `NO_PROXY`. |

> **Portée :** le proxy ne s'applique qu'aux requêtes vers le registre amont du
> registre sur lequel il est configuré. En son absence, la section globale
> `[proxy]` (si elle est définie) sert de repli — vous pouvez donc définir un
> proxy global unique et le remplacer registre par registre là où c'est
> nécessaire.

> **Sécurité :** évitez de versionner les identifiants de proxy. Employez des
> marqueurs `${VAR_NAME}` — voir
> [§5 Surcharges par variables d'environnement](#_5-environment-variable-overrides).

> **Les variables `HTTP_PROXY` et `HTTPS_PROXY` :** quand aucun
> `[registries.proxy]` (et aucun `[proxy]` global) n'est configuré pour un
> registre, le client HTTP sous-jacent lit automatiquement les variables standard
> `HTTP_PROXY`, `HTTPS_PROXY` et `NO_PROXY`. Dès qu'un proxy est configuré par le
> fichier, la lecture de ces variables est désactivée pour le client de ce
> registre — la valeur de configuration remplace entièrement la variable.

#### Faire passer `HTTP_PROXY` dans la configuration

Si vous voulez continuer d'employer la variable standard `HTTP_PROXY` tout en
pouvant définir `no_proxy` ou des identifiants dans le fichier, faites passer la
variable par le mécanisme de substitution `${VAR}` :

```toml
# Shell : export HTTP_PROXY=http://proxy.corp.example.com:3128

[registries.proxy]
url      = "${HTTP_PROXY}"
no_proxy = "localhost,10.0.0.0/8"
```

Le même motif fonctionne pour la section globale :

```toml
[proxy]
url      = "${HTTP_PROXY}"
no_proxy = "${NO_PROXY}"   # faire passer aussi la liste NO_PROXY standard
```

---

#### `[registries.rate_limit]` {#rate_limit}

La limitation de débit par registre, par un algorithme de **compteur à fenêtre
fixe**. Les limites sont suivies par utilisateur authentifié (via `user_id`) ou
par IP cliente pour les requêtes anonymes.

Les compteurs sont stockés dans le **backend de cache** choisi par `[cache]` :
- `type = "memory"` (défaut) — les compteurs sont propres au processus ; ils sont
  remis à zéro au redémarrage et **ne sont pas** partagés entre plusieurs
  réplicas ;
- `type = "postgres"` ou `type = "redis"` — les compteurs survivent aux
  redémarrages et sont partagés par tous les réplicas, ce qui rend la limite
  cohérente sur un cluster réparti.

```toml
[registries.rate_limit]
requests_per_window = 100
window_secs         = 60
enforcement         = "block"   # "block" (défaut) ou "warn"
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `requests_per_window` | u32 | — | Le nombre maximum de requêtes autorisées dans `window_secs` |
| `window_secs` | u32 | — | La longueur de la fenêtre, en secondes |
| `enforcement` | chaîne | `"block"` | `"block"` renvoie un HTTP 429 ; `"warn"` laisse passer la requête et ajoute `X-RateLimit-Warning` |

**Les en-têtes de réponse :**

| En-tête | Quand il est ajouté | Description |
|---|---|---|
| `X-RateLimit-Limit` | Toute réponse proxifiée (quand la limitation est configurée) | La limite effective qui a borné cette requête |
| `Retry-After` | Réponses 429 (mode block) | Les secondes avant que le seau ne se remplisse |
| `X-RateLimit-Reset` | Réponses 429 (mode block) | L'horodatage Unix du remplissage du seau |
| `X-RateLimit-Warning: rate-limit-exceeded` | Réponses au-delà de la limite (mode warn) | Signale que la limite a été dépassée mais que la requête a été autorisée |

#### Les limites de débit par groupe {#per_group_rate_limits}

Tous les membres d'un groupe nommé partagent un unique réservoir de requêtes. Les
noms de groupe sont comparés aux chaînes de la liste `groups` de l'identité
authentifiée, qui sont préfixées par le fournisseur d'authentification :
`"oidc:<group>"`, `"kubernetes:<group>"`, etc.

```toml
[registries.rate_limit]
requests_per_window = 100
window_secs         = 60
enforcement         = "block"

# Les bots de CI partagent un unique réservoir de 5000 req/min entre tous leurs membres :
[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 5000
window_secs         = 60
# enforcement = "block"   # facultatif ; hérite de l'application parente si omis

# Les utilisateurs de l'offre gratuite partagent un réservoir plus restrictif de 200 req/min :
[[registries.rate_limit.groups]]
name                = "oidc:free-tier"
requests_per_window = 200
window_secs         = 60
```

Les champs de `[[registries.rate_limit.groups]]` :

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `name` | chaîne | oui | Correspondance exacte avec une entrée d'`Identity.groups` (par exemple `"oidc:ci-bots"`) |
| `requests_per_window` | u32 | oui | La taille du réservoir partagé par **tous** les membres de ce groupe |
| `window_secs` | u32 | oui | La longueur de la fenêtre, en secondes |
| `enforcement` | chaîne | non | Remplace l'`enforcement` parent pour ce seul groupe ; vaut la valeur parente si omis |

**La sémantique multi-limiteur :** le seau de l'utilisateur et chaque seau de
groupe applicable doivent tous avoir des jetons pour que la requête passe. Si un
seau est épuisé :
- en mode `block`, la requête est rejetée par un HTTP 429. Les en-têtes
  `Retry-After` et `X-RateLimit-Reset` reflètent la plus longue attente parmi
  tous les seaux épuisés ;
- en mode `warn`, la requête est autorisée et `X-RateLimit-Warning` est ajouté à
  la réponse ;
- si des seaux ont des modes d'application différents, `block` prime sur `warn`.

> **Déploiements multi-instances :** mettez `[cache] type = "postgres"` ou
> `type = "redis"` pour partager les compteurs entre tous les réplicas. Avec le
> défaut `type = "memory"`, chaque réplica tient ses propres compteurs et la
> limite effective par utilisateur vaut
> `requests_per_window × nombre de réplicas`.

> **Comportement en cas de panne : laisser passer.** Si le backend de cache est
> injoignable au moment d'incrémenter un compteur, la requête est **autorisée**
> plutôt que rejetée. Un log `WARN` (`rate-limit store unavailable … failing
> open`) est émis pour chaque seau concerné. Surveillez ces avertissements pour
> détecter une panne du backend.

---

#### `[registries.beta_channel]`

Restreint les versions de pre-release (les versions semver dont la composante de
pre-release n'est pas vide, par exemple `1.0.0-beta.1`), de sorte que seuls les
membres du canal bêta du registre puissent les voir et les télécharger. Les
autres ne reçoivent que les versions stables et obtiennent un HTTP 404 sur une
requête directe d'artefact de pre-release.

S'applique aux registres en mode `local` ou `hybrid`. Les membres se gèrent par
l'API d'administration.

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.beta_channel]
enabled = true
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Active le filtrage par canal bêta sur ce registre |

**L'API de gestion des membres (admin uniquement) :**
- `GET    /api/v1/admin/registries/{registry}/beta-channel` — lister les membres
- `POST   /api/v1/admin/registries/{registry}/beta-channel` — corps :
  `{ "principal_type": "user"|"group", "principal_id": "...", "granted_by": "..." }`
- `DELETE /api/v1/admin/registries/{registry}/beta-channel/{principal_type}/{principal_id}` —
  retirer un membre

#### `[registries.retention]`

La rétention de ce que ce registre détient **localement**. Absente — le défaut —
garde tout pour toujours, ce que fait toute instance sans ce bloc.

À ne pas confondre avec les clés d'éviction de
[`[registries.cache]`](/fr/guide/caching), qui gouvernent le *cache proxy* : une
entrée de cache évincée est re-récupérable en amont, une version locale est
souvent la seule copie au monde. Un bloc `[registries.retention]` sur un registre
en mode `proxy` est une **erreur de démarrage**, parce qu'il gouvernerait un
ensemble vide.

Le bloc gouverne deux objets différents : les **versions publiées**, que la
rétention reprend, et les **pierres tombales** que leur suppression laisse, dont
le compactage retire le détail. Supprimer une version laisse déjà une pierre
tombale permanente, sans aucune configuration ; rien ici ne change cela, et la
revendication de coordonnée n'est retirée par aucun réglage. Voir
[Supprimer une version publiée](/fr/guide/admin-policies#deleting-versions).

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.retention]
keep_versions       = 10
keep_if_pulled_days = 90    # le veto qui rend l'activation sûre
keep_for_days       = 365
dry_run             = false
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `keep_versions` | entier | *(non défini)* | Garder les N versions les plus récentes de chaque paquet, par date de publication |
| `keep_for_days` | entier | *(non défini)* | Garder tout ce qui a été **publié** dans cette fenêtre |
| `keep_if_pulled_days` | entier | *(non défini)* | Garder tout ce qui a été **téléchargé** dans cette fenêtre |
| `keep_yanked` | booléen | `true` | Garder les versions retirées |
| `download_signal_floor_days` | entier | *(intégré)* | Avant ce point, « aucun enregistrement de téléchargement » ne prouve rien. Vaut le 27 août 2026 par défaut |
| `reclaim_delay_ms` | entier | `0` | La pause entre deux reprises, pour borner une première passe réelle |
| `tombstone_detail_for_days` | entier | *(non défini)* | Retirer le détail d'une pierre tombale ce nombre de jours après la suppression. Non défini, il est gardé pour toujours. Minimum 30 |
| `dry_run` | booléen | `true` | Rapporter et ne rien changer |

### Les conditions de conservation sont une union de vetos

**Une version survit si *n'importe quelle* condition configurée s'applique.** Il
n'y a pas d'expression à écrire ni d'ordre à ne pas se tromper : la seule façon
de reprendre une version est que toutes les conditions configurées refusent de la
garder. Une mauvaise configuration échoue donc du côté de la conservation, qui
est la direction récupérable.

Un bloc **sans** condition de conservation est rejeté au démarrage, parce que
c'est celui qui reprend toutes les versions dès sa première passe réelle.
`keep_yanked` ne compte pas : il vaut `true` par défaut et ne fait jamais que
mettre un veto, donc un bloc qui ne contiendrait que lui détruirait quand même
toute version non retirée. `0` est rejeté pour toutes les fenêtres — une
condition de conservation à zéro ne garde rien, ce qui n'est pas ce qu'elle a
l'air de vouloir dire.

### `keep_if_pulled_days` est celle qui compte

`keep_versions = 10` seul jette la version sur laquelle la moitié du parc est
épinglée, parce qu'elle se trouve être la onzième par date.
`keep_if_pulled_days` est la règle qui rend la rétention sûre à activer : *ce que
quelqu'un utilise réellement reste, quel que soit son âge ou son rang.*

Configurer une reprise sans elle lève `retention.no-pull-veto` à chaque
rechargement. Une reprise en mode réel lève en plus
`retention.reclamation-live`, et un compactage en mode réel
`retention.compaction-live` — les trois à chaque rechargement, parce que,
contrairement à l'éviction du cache, elles détruisent la seule copie.

Lire le signal de téléchargement, c'est aussi lire ses trous. Les chemins
d'artefact locaux de Maven et de NuGet n'enregistraient aucun événement de
téléchargement avant le 26 août 2026 : `download_signal_floor_days` marque donc
le point avant lequel un enregistrement *absent* ne prouve rien, et une version
dont la seule preuve le précède est conservée. Définissez-le explicitement si
l'historique d'audit de cette instance commence plus tard — après une
restauration, ou après un `audit_purge`.

Une politique `keep_if_pulled_days` sur un déploiement sans dépôt de paquets
**refuse de s'exécuter** plutôt que de traiter « aucun signal » comme « aucun
téléchargement ».

### Ce que ceci n'a pas

La rétention est ici de **niveau registre**. Les niveaux namespace et paquet que
la RFC 0016
§4.1 décrit exigent les blocs de namespace de la RFC 0015 et sa table `policy`,
dont aucun n'existe encore. L'épinglage de niveau version, lui, ne les exige pas
et est disponible : `POST …/retention-pin` pose `retention_keep` sur une version,
et une version épinglée n'est jamais reprise, quoi que dise la politique.

**L'API (admin uniquement) :**
- `POST /api/v1/admin/registries/{registry}/retention` — lancer la rétention ;
  `?dry_run=true` facultatif pour prévisualiser. `409` quand aucune condition de
  conservation n'est configurée
- `POST /api/v1/admin/registries/{registry}/retention-pin` — corps
  `{name, version, keep}` ; épingler une version ou la relâcher
- `GET  /api/v1/admin/registries/{registry}/tombstones` — les coordonnées
  supprimées, la plus récente en premier ; `?name=` facultatif
- `POST /api/v1/admin/registries/{registry}/tombstones/compact` — lancer le
  compactage ; `?dry_run=true` facultatif pour prévisualiser. `409` quand aucune
  fenêtre n'est configurée

`?dry_run=` ne peut jamais que rendre une passe *plus prudente* : passer `false`
ne remplace pas un `dry_run = true` configuré.

---

### 3.6 `[ip_blocking]` (facultatif) {#36-ip_blocking-optional}

Bloque automatiquement les adresses IP qui déclenchent trop d'événements de
violation dans une fenêtre glissante — à la manière de fail2ban. Une IP bloquée
reçoit un HTTP 403 avec un en-tête `X-Block-Expires`, jusqu'à l'expiration du
bannissement.

```toml
[ip_blocking]
enabled               = true
violation_threshold   = 10      # violations avant blocage automatique
violation_window_secs = 300     # fenêtre de comptage, en secondes (5 min)
ban_duration_secs     = 3600    # durée du blocage de l'IP (1 heure)
trigger_on_status     = [429, 401]   # codes de réponse HTTP comptés comme violations
trusted_proxies       = ["10.0.0.1"] # IP dont l'en-tête X-Forwarded-For est cru
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Activer ou désactiver le middleware |
| `violation_threshold` | entier | `10` | Nombre de violations avant blocage automatique |
| `violation_window_secs` | entier | `300` | Longueur de la fenêtre de comptage |
| `ban_duration_secs` | entier | `3600` | Durée d'un blocage automatique |
| `trigger_on_status` | entier[] | `[429, 401]` | Les codes de statut comptés comme violations |
| `trusted_proxies` | chaîne[] | `[]` | Les IP de proxys amont autorisées à poser `X-Forwarded-For` |

**Les backends :** l'état des blocages est stocké dans le même backend que le
cache (`memory`, `postgres` ou `redis`). Employez `postgres` ou `redis` pour un
déploiement multi-instances.

**La gestion manuelle :** un admin gère les blocages par l'API :
- `GET    /api/v1/admin/ip-blocks` — lister les IP actuellement bloquées
- `POST   /api/v1/admin/ip-blocks` — corps :
  `{ "ip": "1.2.3.4", "reason": "...", "duration_secs": 3600 }`
- `DELETE /api/v1/admin/ip-blocks/{ip}` — débloquer une IP

**Les proxys de confiance :** quand une requête arrive par un reverse proxy
connu, batlehub lit la vraie IP cliente dans `X-Forwarded-For` seulement si
l'adresse du pair TCP figure dans `trusted_proxies`. Sans cette configuration,
`X-Forwarded-For` est ignoré, pour empêcher l'usurpation par en-tête.

---

### 3.7 `[otel]` (facultatif) {#_3-7-otel-optional}

Active le traçage distribué OpenTelemetry par OTLP en gRPC.

```toml
[otel]
endpoint = "http://localhost:4317"
service_name = "batlehub"   # défaut
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `endpoint` | chaîne | — | L'endpoint OTLP gRPC |
| `service_name` | chaîne | `"batlehub"` | Le nom de service rapporté dans les traces |

Toute la section s'active sans modifier le fichier de configuration, en
définissant `PROXY_CACHE__OTEL__ENDPOINT` — la section est créée automatiquement
si la variable est présente.

---

### 3.8 `[proxy]` (facultatif) {#_3-8-proxy-optional}

Un proxy HTTP ou SOCKS **global**, appliqué à toutes les requêtes vers les
registres amont. Un registre qui définit sa propre section
`[registries.proxy]` remplace ce réglage global, pour lui seul.

```toml
[proxy]
url      = "http://proxy.corp.example.com:3128"
# username = "proxyuser"   # facultatif
# password = "${PROXY_PASSWORD}"   # facultatif
# no_proxy = "localhost,10.0.0.0/8,internal.example.com"  # facultatif
```

| Champ | Type | Obligatoire | Notes |
|---|---|---|---|
| `url` | chaîne | oui | L'URL du proxy (`http://`, `https://` ou `socks5://`). Les identifiants peuvent y être inclus : `http://user:pass@proxy:3128`. |
| `username` | chaîne | non | Le nom d'utilisateur en authentification Basic du proxy. |
| `password` | chaîne | non | Le mot de passe en authentification Basic du proxy. Employez `${VAR}` pour garder les secrets hors du fichier. |
| `no_proxy` | chaîne | non | Les hôtes, domaines ou CIDR à contourner, séparés par des virgules. |

Toute la section se définit sans toucher au fichier, par variables
d'environnement :

```sh
export PROXY_CACHE__PROXY__URL="http://proxy.corp.example.com:3128"
export PROXY_CACHE__PROXY__USERNAME="proxyuser"
export PROXY_CACHE__PROXY__PASSWORD="s3cr3t"
export PROXY_CACHE__PROXY__NO_PROXY="localhost,10.0.0.0/8"
```

`PROXY_CACHE__PROXY__URL` crée la section `[proxy]` automatiquement si elle est
absente du fichier TOML : un déploiement minimal n'a donc besoin que de cette
seule variable.

> **La priorité :** le `[registries.proxy]` d'un registre l'emporte sur le
> `[proxy]` global. Quand aucun des deux n'est défini, le client HTTP
> sous-jacent lit automatiquement les variables standard `HTTP_PROXY`,
> `HTTPS_PROXY` et `NO_PROXY`. Configurer un proxy par le fichier désactive la
> lecture de ces variables pour le client de ce registre — pour les y faire
> passer, voir
> [Faire passer `HTTP_PROXY` dans la configuration](#faire-passer-http-proxy-dans-la-configuration)
> ci-dessus.

---

### 3.8a `[stats]` (facultatif)

Quels nombres cette instance conserve, et lesquels elle publie. Deux drapeaux, un
bloc, parce que « est-ce que je veux que cette instance garde des nombres » est
une seule question d'opérateur — même si les deux moitiés diffèrent :
`metrics_enabled` porte sur l'**exposition**, `history_*` sur le **stockage**.

```toml
[stats]
history_enabled        = true   # défaut
history_retention_days = 30     # défaut ; 0 désactive l'élagage, pas l'historique
metrics_enabled        = true   # défaut : le comportement d'avant la RFC 0004
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `history_enabled` | booléen | `true` | Enregistrer le cumul horaire du cache derrière la tendance du tableau de bord. `false` rétablit le tableau de bord d'avant la RFC 0004, qui n'affiche que les compteurs depuis le démarrage du processus courant |
| `history_retention_days` | u32 | `30` | Supprimer les lignes de cumul plus anciennes. `0` garde toutes les lignes — cela désactive l'*élagage*, pas l'historique |
| `metrics_enabled` | booléen | `true` | Installer le collecteur Prometheus et servir `/metrics`. `false` fait répondre `503 metrics not configured` à `/metrics` |

**`metrics_enabled` est un contrôle de sécurité, pas une préférence.**
`/metrics` n'est pas authentifié et, avant l'existence de ce bloc, était
inconditionnel : il publie les taux de hit du cache, les volumes de pulls par
registre et les latences amont à quiconque atteint le port. C'est un défaut
défendable derrière un ingress qui ne le route pas, et indéfendable pour un
auto-hébergeur qui n'avait aucun moyen de le fermer. Il vaut `true` par défaut,
pour qu'aucune collecte existante ne casse à la mise à jour.

**Pourquoi le cumul plutôt que le journal d'accès.** Le journal d'accès contient
déjà chaque téléchargement : un taux de hit sur 30 jours pourrait en principe en
être dérivé. Il ne l'est pas, délibérément : cette table est une piste *d'audit*,
avec ses propres sémantiques de rétention et de purge, et en dériver un graphe
opérationnel permettrait à une purge d'audit de réécrire silencieusement un
tableau de bord. Un rapport hit/miss est par ailleurs une question de compteurs,
et parcourir une table d'audit à chaque chargement du tableau de bord va bien à
dix mille lignes et devient un problème à dix millions.

L'intervalle est fixé à une heure et n'est pas configurable : c'est la résolution
à laquelle les données sont *conservées*, une agrégation quotidienne se recalcule
toujours à la lecture mais ne se récupère jamais, et deux instances aux
intervalles différents auraient des historiques incomparables. Une ligne par
registre et par heure fait moins de 9 000 lignes par an.

La table ne contient ni principal ni coordonnée — registre, fenêtre, compteurs —
sa rétention est donc un choix opérationnel plutôt qu'un choix de confidentialité.

---

### 3.8b `[cache_coherence]` (facultatif) {#38b-cache-coherence-optional}

Le ramassage périodique des blobs de stockage que rien ne référence.

Un artefact est mis en cache en deux temps — les octets sont stockés, puis la
ligne qui pointe vers eux est enregistrée. Un processus tué entre les deux laisse
un blob qu'aucune requête ne peut atteindre et qu'**aucune stratégie d'éviction
ne considérera jamais**, puisque chacune lit la table dont la ligne est absente.
Supprimer une ligne à la main laisse la même chose. C'est la passe qui les
ramasse.

```toml
[cache_coherence]
enabled       = true
interval_secs = 86400   # défaut : quotidien
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Exécuter la passe sur un minuteur. Un bloc absent équivaut à `false` |
| `interval_secs` | u64 | `86400` | Secondes entre deux passes, tous registres confondus |

**Désactivé par défaut**, comme toute politique d'ici qui supprime : elle tourne
sans personne pour regarder. Un déploiement qui préfère regarder d'abord dispose
de la même passe à la demande, et d'une prévisualisation —
[voir le guide d'administration](/fr/guide/admin-policies#cache-coherence).

**L'intervalle est aussi la fenêtre de grâce.** Un blob n'est supprimé que si la
passe *précédente* l'a vu orphelin lui aussi, parce qu'une écriture de cache en
cours — octets stockés, ligne pas encore enregistrée — ressemble exactement à un
orphelin. L'écart entre deux passes est toute la marge dont dispose cette
écriture : un intervalle court échange donc l'unique propriété de sûreté de cette
passe contre un ramassage plus rapide d'octets que personne ne peut atteindre. En
dessous de 300 s, le serveur lève un avertissement de configuration
(`cache-coherence.interval-too-short`) plutôt qu'un refus : un petit parc sur un
stockage rapide peut raisonnablement vouloir une boucle plus serrée.

Chaque passe liste par ailleurs **tous les objets en cache de tous les
registres** — un `ListObjectsV2` paginé contre S3, ou un parcours complet de
répertoire sur un backend de fichiers. C'est une seconde raison pour laquelle le
défaut est quotidien plutôt qu'horaire.

La première passe d'un processus a lieu un intervalle après le démarrage, et non
à l'instant zéro : un redémarrage est précisément le moment où existent des
écritures de cache à moitié faites, et un processus neuf a un ensemble de grâce
vide.

Une passe planifiée est auditée en `cache_coherence_run` avec
`user_id = "system"` — le champ qui la sépare d'un opérateur qui en a lancé une à
la main.

---

### 3.8c `[upstream_audit]` (facultatif) {#upstream-audit}

Une passe périodique qui demande à chaque amont en mode proxy ou hybrid si les
artefacts qu'on lui a récupérés existent toujours, confirme une disparition sur
plusieurs passes avant d'y croire, et **retient un artefact confirmé hors de
l'éviction**, de sorte que la dernière copie du parc ne soit pas ramassée
précisément parce que l'amont a cessé de la rafraîchir (RFC 0014). Désactivée
tant qu'on ne la demande pas : elle envoie des requêtes planifiées à des
registres tiers.

```toml
[upstream_audit]
enabled              = true
interval_secs        = 21600    # 6 h entre les passes ; plancher 300
confirm_after        = 3        # passes consécutives qu'un défaut doit traverser
confirm_min_age_secs = 86400    # …et au moins ce délai depuis le premier défaut
outage_ratio         = 0.25     # au-delà de cette proportion de manquants, la passe est nulle
on_confirmed         = "audit"  # ou "block" : refuser sur le fil une disparition confirmée
retain_disappeared   = true     # retenir les artefacts confirmés hors de l'éviction
skip_recently_seen   = true     # le trafic réel compte comme une sonde réussie
registries           = []       # vide = tous les registres proxy et hybrid
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Rien ne balaie tant qu'on ne le demande pas. |
| `interval_secs` | u64 | `21600` | Secondes entre deux passes. En dessous de `300`, c'est une erreur de configuration : une boucle plus rapide est un déni de service contre le registre de quelqu'un d'autre, à partir d'une faute de frappe. |
| `confirm_after` | u32 | `3` | Défauts consécutifs, chacun dans une passe valide, avant qu'une disparition ne soit crue. `0` est refusé. |
| `confirm_min_age_secs` | u64 | `86400` | L'autre plancher : au moins ce délai depuis le premier défaut. Les deux doivent être franchis : avec les défauts, la confirmation la plus rapide prend 24 h. N'abaisser qu'`interval_secs` achète plus de sondes et la même réponse. |
| `outage_ratio` | f64 | `0.25` | Une passe dans laquelle plus que cette proportion des paquets sondés d'un registre revient manquante est **nulle** : rien d'enregistré, rien de confirmé. Une panne affecte presque tout ; un retrait de publication affecte une chose. Doit être dans `(0.0, 1.0]`. En dessous de dix paquets sondés, le ratio est ignoré et les deux planchers portent seuls la décision. |
| `on_confirmed` | chaîne | `"audit"` | Ce que fait une confirmation au-delà d'enregistrer, retenir et notifier, pour tout registre audité qui ne dit pas autre chose. `"block"` bloque en plus toutes les versions détenues de ce nom par la liste de blocage d'administration (`blocked_by = system:upstream-audit`), et lève *son propre* blocage quand le paquet réapparaît — jamais celui d'un admin. Toute autre valeur est une erreur de configuration, pas un repli. Un registre le remplace pour lui-même par son propre `on_confirmed` (RFC 0014 §13 O6 ; le plus profond gagne) — bloquer automatiquement un amont public, se contenter d'auditer un miroir interne. Lisez le paragraphe ci-dessous avant de choisir `"block"`, et la [page d'exploitation](/fr/operations/upstream-disappearance) pour ce que cela donne depuis la console. |
| `retain_disappeared` | booléen | `true` | Retenir un artefact confirmé hors des passes d'éviction par TTL, par inactivité et par nombre de versions, et ré-épingler ses métadonnées en cache à chaque passe. **Pas** hors du plafond LRU : celui-ci existe pour empêcher le disque de se remplir, donc les artefacts retenus y passent en dernier plutôt que d'en être exemptés. |
| `skip_recently_seen` | booléen | `true` | Un paquet remis en cache depuis l'amont après le début de la dernière passe était démontrablement présent ; sa sonde est sautée. |
| `registries` | chaîne[] | `[]` | Uniquement ces registres. Vide signifie tous les registres en mode `proxy` ou `hybrid`. Nommer un registre inconnu ou en mode `local` est une erreur de configuration. |

**Ce que coûte `"block"`.** Sous `"audit"`, la pire conséquence d'une
confirmation erronée est un admin qui lit une mauvaise alerte. Sous `"block"`,
c'est un déni de service ciblé : un attaquant capable de servir des `404`
sélectifs à cette instance — la maîtrise du chemin vers l'amont, tenue pendant
toute la fenêtre de confirmation, assez étroitement pour ne pas déclencher
`outage_ratio` — choisit un paquet dont le parc dépend, et le parc le bloque
contre lui-même. Cette position lui permet déjà de servir des métadonnées
fabriquées en cas de défaut de cache : la *capacité* n'est donc pas nouvelle ;
c'est le coût qui l'est, et il n'est pas entièrement atténuable. Choisissez
`"block"` pour un parc dont la menace est un paquet retiré ou détourné qui
atteindrait un build ; gardez une fenêtre de confirmation longue, gardez
`retain_disappeared = true` (sans lui, un paquet bloqué n'est jamais lu et
l'éviction par inactivité supprime la copie que le blocage préservait —
`upstream-audit.block-without-hold` avertit), et sachez que désactiver la
politique ne débloque rien : les blocages qu'elle a écrits sont un état
administratif, listés sous `system:upstream-audit` dans la table des blocages de
la console, et restent jusqu'à ce qu'un admin les lève ou que le paquet
réapparaisse.

**Comment une passe décide.** Registre par registre : chaque paquet en cache est
sondé — une requête de listing par paquet sur les types qui ont un document de
listing, une requête par version (25 au plus par paquet et par passe) sur les
types qui n'en ont pas, et un `HEAD` par fichier détenu sur les types adressés
par chemin (`deb`, `rpm`, `pacman`, `jetbrains`, `generic`), dont les lignes et
les blocages nomment alors le chemin du fichier ; un amont qui ne répond pas est
*non concluant* et ne compte d'aucun côté du ratio. Un défaut insère ou
incrémente une ligne ; une sonde réussie la supprime purement et simplement,
jamais ne la décrémente. Une ligne confirmée est journalisée en `WARN` avec la
coordonnée et le nombre de défauts, apparaît dans les jauges
`batlehub_upstream_*` et dans la table *Exploitation → Amont* de la console, et
est envoyée à tous les abonnements sur `package_disappeared_upstream` ; une
réapparition efface la ligne, la journalise et envoie
`package_reappeared_upstream` ; une passe nulle envoie `upstream_unreachable`
pour le registre. `POST /api/v1/admin/upstream/recheck` sonde un paquet
maintenant, par la même échelle et la même machine à états. **La première passe
après activation ne trouve rien**, à dessein — chaque défaut démarre à un — et
les premières confirmations arrivent après `confirm_min_age_secs`.

**Où cela tourne.** Sur le rôle `worker` (voir
[`[server].roles`](#31-server)) : la sonde est un scanner du worker de la
RFC 0018, et `[worker].max_concurrent` borne les requêtes amont simultanées. Un
processus en proxy seul avec cette section active journalise un avertissement et
lève `upstream-audit.no-worker-role`. Sur un registre doté de
[`[registries.security]`](#registries-security), une disparition confirmée est
aussi un constat `UNPUBLISHED_UPSTREAM` sur le verdict de la version —
enregistré et visible dans `batlehub why`, jamais une retenue sous `"audit"`.

**Les métriques.** `batlehub_upstream_missing_total` et
`batlehub_upstream_disappeared_total` (jauges, par registre),
`batlehub_upstream_audit_sweeps_total` (compteur, `outcome` = `ok` / `void`),
`batlehub_upstream_audit_duration_seconds`. Un taux de `void` qui monte est
l'alerte qui dit que la fonctionnalité a cessé de marcher ; une jauge de
disparitions seule ne le montrerait jamais. Le compte `held` du rapport
d'éviction dit ce que la retenue a préservé à chaque passe.

---

### 3.8d `[notifications]` (facultatif) {#_3-8d-notifications-optional}

Où va un événement quand quelque chose se produit : une version mise en
quarantaine, un artefact disparu en amont, un rechargement de configuration.
Absente, rien n'est envoyé.

```toml
[notifications]
enabled = true                 # défaut

[[notifications.channels]]
name = "ops-slack"
type = "slack"
url  = "https://hooks.slack.com/services/..."

[[notifications.channels]]
name    = "ci-webhook"
type    = "webhook"
url     = "https://ci.example.com/hooks/batlehub"
secret  = "${WEBHOOK_SIGNING_SECRET}"   # signe chaque POST
timeout_secs = 10                       # défaut

[[notifications.inbound]]
name   = "ci-scanner"
secret = "${INBOUND_SECRET}"
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `true` | Présent mais désactivé, c'est la façon de garder les canaux déclarés et de cesser d'envoyer |
| `channels` | tableau | `[]` | Le sortant. Un bloc `[[notifications.channels]]` par canal |
| `inbound` | tableau | `[]` | Les webhooks que ce serveur *accepte*. Un bloc `[[notifications.inbound]]` par webhook |

**Les types de canal.** `type` choisit la forme, et les champs diffèrent :

| `type` | Obligatoires | Facultatifs |
|---|---|---|
| `slack` | `name`, `url` | `timeout_secs` (10) |
| `teams` | `name`, `url` | `timeout_secs` (10) |
| `webhook` | `name`, `url` | `secret`, `timeout_secs` (10) |
| `email` | `name`, `smtp_host`, `from`, `to` | `smtp_port` (587), `smtp_user`, `smtp_password`, `tls` (`true`), `timeout_secs` (10) |

**Seul le `webhook` générique signe ce qu'il envoie.** Avec un `secret` défini,
chaque POST porte `X-BatleHub-Signature-256: sha256=<hex>`, un HMAC-SHA256 sur le
corps. Slack et Teams n'ont pas de tel champ parce que leur protocole n'en a
pas : le secret de l'URL est toute leur authentification. Traitez donc ces URL
comme des identifiants et injectez-les par `env` plutôt que de les écrire dans le
fichier.

**Les webhooks entrants.** Chaque bloc `[[notifications.inbound]]` accepte des
événements venus d'ailleurs, vérifiés contre `X-Hub-Signature-256` quand un
`secret` est défini. Sans secret, **toute charge est acceptée**, ce qui ne
convient qu'à un réseau où rien de non fiable n'atteint le port. Un registre doté
de [`[registries.security]`](#registries-security) rend le secret obligatoire :
un événement `security.*` sur un webhook non signé permettrait à quiconque sur le
réseau de refuser des paquets.

Un nom entrant doit par ailleurs être distinct de tout nom de
[`[[flag_sources]]`](#flag-sources) — les deux partagent un espace de noms parce
qu'ils partagent le schéma de vérification.

**La gestion manuelle par l'API :**

- `GET /api/v1/admin/notifications/channels` — les canaux sortants configurés. Ne
  renvoie jamais d'URL ni de secret.
- `GET /api/v1/admin/notifications/inbound` — les webhooks entrants.
- `GET` / `POST /api/v1/admin/notifications/subscriptions` — lister et créer.
- `GET` / `PUT` / `DELETE /api/v1/admin/notifications/subscriptions/{id}` — un
  abonnement.
- `POST /api/v1/admin/notifications/subscriptions/{id}/test` — y envoyer un
  événement de test, ce qui est la seule façon de prouver l'URL et le secret d'un
  canal avant qu'un incident ne s'en charge.

Les trois mêmes sont
`batlehub-cli admin notifications channels|list|delete` ; voir
[la référence de la CLI](/fr/use/cli#notifications). Les canaux eux-mêmes sont de
la configuration : en ajouter un est un changement de configuration et un
rechargement, pas un appel d'API.

---

### 3.9 `[subdomain_routing]` (facultatif) {#39-subdomain_routing-optional}

Tout registre est toujours joignable sur `/proxy/{name}/…`. Cette section ajoute
une seconde porte d'entrée : un nom d'hôte dont la **racine** est le registre.

```toml
[subdomain_routing]
enabled     = true                # dériver "<name>.<base_domain>" par registre
base_domain = "hub.example.com"   # npm1.hub.example.com -> registre "npm1"
scheme      = "https"             # sert uniquement à annoncer les URL publiques

[[registries]]
name         = "npm1"
type         = "npm"
hosts        = ["npm.acme.io"]    # hôtes dédiés supplémentaires, facultatifs
path_routing = true               # défaut ; false => l'hôte est la seule porte d'entrée
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Dériver un hôte générique par registre |
| `base_domain` | chaîne | — | Obligatoire quand `enabled = true` |
| `scheme` | chaîne | `"https"` | Décide seulement si l'API annonce `https://…` ou `http://…` ; **n'affecte jamais** le routage |
| `registries[].hosts` | chaîne[] | `[]` | Noms d'hôte supplémentaires enracinés sur ce registre. Indépendant de `[subdomain_routing]` |
| `registries[].path_routing` | booléen | `true` | `false` fait renvoyer 404 à `/proxy/{name}/…` |

```ini
# .npmrc — sous-chemin
registry=https://hub.example.com/proxy/npm1/

# .npmrc — avec un hôte dédié
registry=https://npm.acme.io/
```

**Sur un hôte de registre, tous les chemins sont ceux du registre.** Il n'y a pas
de liste d'exceptions, parce que cargo (`/api/v1/…`), GitLab (`/api/v4/…`) et
Forgejo (`/api/packages/…`) servent tous légitimement des chemins sous `/api`, et
qu'un registre `generic` ou `deb` peut légitimement répliquer `/healthz` ou
`/metrics`. L'API d'administration, la SPA, `/healthz` et `/metrics` vivent donc
sur l'**hôte principal uniquement** — pointez-y vos sondes et vos collectes.

Un corollaire utile : `https://npm1.hub.example.com/proxy/npm1/lodash` devient
`/proxy/npm1/proxy/npm1/lodash` et donne un 404. Choisissez une porte d'entrée
par client.

Toute URL que le serveur génère reflète la porte d'entrée réellement employée par
le client : un packument récupéré depuis `npm.acme.io` annonce donc
`https://npm.acme.io/lodash/-/lodash-4.17.21.tgz`, tandis que le même packument
sur le sous-chemin continue d'annoncer `https://hub.example.com/proxy/npm1/…`.

#### `path_routing = false`

Rend un registre joignable **uniquement** par son ou ses hôtes. La motivation est
l'isolement : une fois `npm.acme.io` remis à une équipe, vous ne voulez peut-être
pas que le même contenu réponde sur l'hôte principal partagé, où il hérite de la
politique CORS, des règles de WAF et des clés de cache de cet hôte, et où une URL
fuitée d'une porte d'entrée continue silencieusement de fonctionner sur l'autre.
Le sous-chemin renvoie **404**, pas 403 — une porte d'entrée désactivée doit
paraître absente, pas interdite.

#### Prérequis pour l'opérateur

- Un enregistrement DNS par hôte (ou un générique `*.hub.example.com`).
- Un certificat qui le couvre — un certificat générique dans le cas du
  `base_domain`.
- Un reverse proxy qui transmet l'en-tête `Host` d'origine.
- Un [`[server].trusted_proxies`](#31-server) listant les plages CIDR de ce
  proxy. **C'est obligatoire :** configurer le routage par hôte sans politique de
  confiance est une erreur de démarrage, parce que le routage dépendrait alors
  d'un en-tête non gouverné.

Avec le chart Helm, ajoutez les hôtes à `ingress.extraHosts` et le CIDR à
`config.server.trusted_proxies`.

#### Validation

Rejeté au démarrage et à chaque rechargement :

| Condition | Pourquoi |
|---|---|
| `enabled = true` sans `base_domain` | la section ne routerait rien |
| le même hôte revendiqué par deux registres | ambigu ; un « le dernier écrit gagne » serait invisible |
| une entrée de `hosts` en collision avec l'hôte générique d'un autre registre | même ambiguïté, plus difficile à repérer |
| une entrée de `hosts` égale au `base_domain` | masquerait l'hôte principal et cacherait l'API d'administration |
| une entrée de `hosts` contenant `/`, un préfixe de schéma, ou vide après nettoyage | ce n'est pas un nom d'hôte |
| `path_routing = false` sur un registre sans hôte joignable | le registre serait entièrement injoignable |
| le routage par hôte sans `[server].trusted_proxies` ni `[ip_blocking].trusted_proxies` | le routage dépendrait d'un en-tête non gouverné |

Signalé, mais accepté — voir `GET /api/v1/admin/config/warnings` et la page
d'administration du rechargement :

| Condition | Comportement |
|---|---|
| un nom de registre qui n'est pas une étiquette DNS valide (`my_registry`, `Foo.Bar`) | pas d'hôte générique pour lui ; il reste joignable par chemin et par toute entrée `hosts` explicite |
| un routage par hôte satisfait uniquement par le `[ip_blocking].trusted_proxies` déprécié | accepté et honoré ; déplacez la liste dans `[server]` |

#### Retour en arrière

Une modification de configuration et un rechargement à chaud. Rien n'est
persisté, et sans `[subdomain_routing]` ni `hosts`, la table de routage est vide,
le middleware ne fait rien, et toute URL générée est identique octet pour octet à
celle d'un déploiement qui n'a jamais eu la fonctionnalité.

---

### 3.10 `[scanners]` et `[worker]` (facultatif) {#scanners-and-worker}

Les scanners sont déclarés une fois, globalement, et les registres y souscrivent
par nom dans [`[registries.security]`](#registries-security). Le worker est le
rôle de processus qui les exécute.

```toml
[scanners.osv]                       # implicite — à déclarer seulement pour changer quelque chose
type = "osv"
# api_url = "https://api.osv.dev"

[scanners.socket]                    # Socket.dev : facturé, souscription par registre, clé obligatoire
type    = "socket"
api_key = "${SOCKET_API_KEY}"
# api_url = "https://api.socket.dev"

[scanners.mlab]                      # API CVE de mlab.sh : CVSS/EPSS/KEV sur les constats CVE (enrichissement)
type    = "mlab"
# api_key = "${MLAB_API_KEY}"        # facultatif : l'endpoint répond sans authentification
# api_url = "https://vuln.mlab.sh"

[scanners.osv.escalation]            # facultatif, par scanner
kinds = ["vulnerability"]            # les FindingKind qui se combinent
count = 3                            # autant de constats à ce niveau ou au-dessus de `from`…
from  = "medium"
to    = "high"                       # …sont élevés à cette gravité

[server]
roles = ["proxy", "worker"]

[worker]
max_concurrent   = 4                 # travaux d'analyse en vol dans ce processus
registries       = []                # vide = tous les registres ; sinon, seulement ces noms
job_timeout_secs = 600
max_attempts     = 3                 # ensuite, le verdict porte SCANNER_ERROR

[worker.sandbox]                     # ce sous quoi tourne chaque scanner binaire
runtime          = "bwrap"           # "none" est refusé sauf avec BATLEHUB_UNSAFE_NO_SANDBOX=1
memory_limit_mb  = 2048              # RLIMIT_AS sur le processus du scanner
cpu_seconds      = 300               # RLIMIT_CPU
max_extracted_mb = 512               # le plafond de la politique d'extraction, refusé et non tronqué
max_entries      = 50000

# Les scanners d'archive de la phase 3 de la RFC 0018. `command` doit être un
# fichier exécutable (ou sur le PATH) dans le processus qui porte le rôle worker —
# l'image du worker (Containerfile.worker) porte les trois ; celle du proxy aucun.
[scanners.postmortem]
type     = "postmortem"
command  = "/usr/local/bin/postmortem"
timeline = true                      # npm uniquement : les signaux de transition (changement de publieur, …)
online   = false                     # `--enrich` ; conserve le namespace réseau du bac à sable à true

[scanners.trivy]
type         = "trivy"
endpoint     = "http://batlehub-trivy:4954"   # un serveur Trivy ; vide = la base propre du client
timeout_secs = 120

[scanners.guarddog]                  # second avis facultatif sur npm, PyPI et Go
type       = "guarddog"
command    = "/usr/local/bin/guarddog"
ecosystems = ["npm", "pypi"]

[scanners.sigstore]                  # attestations de provenance npm, vérifiées contre Rekor
type        = "sigstore"
rekor_url   = "https://rekor.sigstore.dev"
require_for = ["npm"]                # PROVENANCE_MISSING sur ces types ; ailleurs l'absence est muette
```

| `type` de scanner | Disponible | Clés | Notes |
|---|---|---|---|
| `osv` | maintenant | `api_url` | La requête OSV.dev déjà derrière `cve_gate`, sous forme de scanner : une vulnérabilité au `max_severity` du registre ou au-dessus est un constat. Tourne sur tous les types ayant une URL de paquet ; les types proxifiés par chemin, `nodedist`, les places de marché et Terraform n'en ont pas. |
| `postmortem` | maintenant | `command`, `online`, `timeline` | L'archive est extraite, sous la politique du bac à sable, dans la disposition où son écosystème garde une dépendance (`node_modules/<name>`, `site-packages/…`, `vendor/…`), un fichier de verrouillage est écrit depuis la coordonnée — jamais en exécutant l'outil de l'écosystème — et `postmortem scan --json --no-config` tourne dans `bwrap`. Constats : `INSTALL_HOOK`, `MALWARE_SIGNAL` (IOC, obfuscation, API sensible), `TYPOSQUAT_SUSPECT` ; avec `timeline`, les codes de transition à la version analysée (npm). Couvre npm, PyPI, Cargo, RubyGems, Composer, Go et Maven. |
| `trivy` | maintenant | `endpoint`, `timeout_secs` | Le **client** Trivy, contre le serveur d'`endpoint` (le `trivy.enabled` du chart en déploie un) ou contre sa propre base quand il est vide. Analyse le SBOM CycloneDX que cette instance a déjà enregistré pour l'artefact, à défaut l'archive extraite. Constats : `VULNERABILITY`, avec la CVE en référence. |
| `guarddog` | maintenant | `command`, `ecosystems` | GuardDog de DataDog sur les archives npm, PyPI et Go, sous le même bac à sable. Second avis facultatif ; absent du profil par défaut, et le seul scanner qui ne soit pas sur l'image du worker — il est livré sur la variante `-worker-guarddog`, qu'un déploiement emploie à la place. La correspondance règle → constat se fait par famille de règles et est *lue, non observée* tant que cette image ne l'a pas exécutée. |
| `sigstore` | maintenant | `rekor_url`, `require_for` | La provenance npm : les attestations que le packument annonce pour la version sont récupérées, et chaque entrée de journal de transparence qu'elles citent est recherchée dans Rekor. `PROVENANCE_MISSING` sur les types de `require_for`, `PROVENANCE_INVALID` quand une entrée citée n'est pas dans le journal. Un contrôle d'existence et d'inclusion, pas une vérification Sigstore complète. |
| `socket`, `mlab` | phase 5 de la RFC 0018 | `api_key` pour `socket` | `socket` est refusé au chargement sans elle (un `401` que personne ne lirait autrement) ; l'API CVE de `mlab` répond sans authentification, sa clé est donc une politesse de limitation de débit plutôt qu'une exigence. `mlab` ne fait qu'enrichir les constats des autres et est refusé dans `required_scanners` (`security.enrichment-required`). |

| Champ de `[worker]` | Type | Défaut | Notes |
|---|---|---|---|
| `max_concurrent` | u32 | `4` | Travaux réservés simultanément par ce processus. |
| `registries` | chaîne[] | `[]` | Restreindre le worker à ces noms de registre ; chacun doit exister. Vide, c'est tous les registres. |
| `job_timeout_secs` | u64 | `600` | Une réservation qui n'est ni terminée ni renouvelée dans ce délai retourne dans la file. |
| `max_attempts` | u32 | `3` | Tentatives avant que le verdict de la coordonnée n'enregistre `SCANNER_ERROR` et que le travail ne se ferme. |
| `sandbox.runtime` | chaîne | `"bwrap"` | Ce sous quoi tourne chaque scanner binaire : de nouveaux namespaces user, pid, ipc et uts, aucun réseau sauf si le scanner le déclare, la racine en lecture seule, le répertoire du travail comme unique montage inscriptible, un environnement vide, et l'argv passé sans shell. `"none"` exécute la commande nue et est refusé sauf avec `BATLEHUB_UNSAFE_NO_SANDBOX=1`. |
| `sandbox.memory_limit_mb`, `sandbox.cpu_seconds` | u64 | `2048`, `300` | `RLIMIT_AS` et `RLIMIT_CPU` sur le processus du scanner. |
| `sandbox.max_extracted_mb`, `sandbox.max_entries` | u64 | `512`, `50000` | La politique d'extraction : une archive au-delà de l'un ou l'autre est **refusée**, jamais tronquée, tout comme une archive dont le taux de décompression dépasse 100:1, une entrée qui s'échappe de la racine, un lien symbolique, un lien physique, un périphérique. Les archives imbriquées sont écrites et non parcourues ; les bits d'exécution sont retirés. |

**Ce qu'exige une analyse.** Un scanner qui lit des octets (`postmortem`,
`guarddog`, `trivy` sans SBOM) fait récupérer par le worker l'artefact principal
de la version — depuis le cache quand il y est, sinon depuis l'amont, sans mise
en cache — de sorte qu'un profil de scanners purement métadonnées ne coûte aucune
sortie réseau. Un type dont une version est un *ensemble* de fichiers (PyPI,
Maven, conda, Terraform) n'a pas d'artefact unique à remettre, et ces scanners y
répondent `SCANNER_UNSUPPORTED` plutôt que de prétendre avoir regardé.

**Où vivent les chaînes d'outils.** Seul le rôle worker ouvre des artefacts :
seule l'image du worker (`Containerfile.worker` : bubblewrap, postmortem, le
client Trivy) porte donc les outils ; l'image du proxy reste distroless. Dans le
chart, `worker.enabled` la déploie dans son propre Deployment depuis cette image,
et `config.server.roles = ["proxy"]` arrête l'analyse dans le pod du proxy.

GuardDog est l'exception. C'est le seul scanner qui ne soit pas un binaire
statique — il apporte un interpréteur Python et son propre venv — et il est
facultatif : il a donc son image à lui,
`Containerfile.worker-guarddog`, publiée sous `…-worker-guarddog`, qui est
l'image du worker avec GuardDog en plus. Activer `[scanners.guarddog]` suppose
donc de pointer `worker.image.repository` vers cette variante ; le processus
refuse de démarrer si la `command` n'est pas sur l'image qu'il exécute. Rien
d'autre ne change — GuardDog reste un sous-processus du rôle worker, sous le même
bac à sable.

**Comment se comporte la file.** Les travaux portent un déclencheur —
`FirstSeen` (un utilisateur attend) est défilé avant `Webhook`, `Rescan` et
`Backfill` ; à niveau égal, le plus ancien d'abord. Un travail est réservé avec un
battement de cœur : un worker qui meurt en pleine analyse remet donc son travail
au suivant après `job_timeout_secs`. Plusieurs processus worker partagent une
file par la base ; les vivants sont comptés dans `batlehub_workers_live`, et un
processus en proxy seul avertit au démarrage quand ce compte est nul.

---

### 3.11 `[[flag_sources]]` (facultatif) {#flag-sources}

Les tiers autorisés à pousser des signalements de vulnérabilités
([RFC 0002](/rfc/0002-vulnerability-flags-and-exposure), refondue par son §13) :
un SOC, une plateforme de vulnérabilités d'entreprise, un flux d'alertes. Un
signalement dit *ce que* la source affirme d'une version de paquet (`cve`,
`malware`, `license`, `policy`, ou un type à elle) et *avec quelle force* elle
veut que cette instance réagisse (`inform`, `warn`, `gate`, `hard_block`).

```toml
[[flag_sources]]
name = "soc"                        # le segment de chemin de l'endpoint de poussée
secret = "hmac-key-from-your-vault" # obligatoire, non vide
max_effect = "hard_block"           # inform | warn | gate | hard_block ; défaut gate
registries = ["npm", "pypi"]        # vide (défaut) : n'importe quel registre
max_flags_per_minute = 600          # 0 désactive la limite
```

| Clé | Défaut | Signification |
| --- | --- | --- |
| `name` | — | `[a-z0-9][a-z0-9_-]*`, 64 caractères au plus, unique — et distinct de tout nom de `[[notifications.inbound]]`, parce qu'un événement `security.verdict` stocke son `hard_block` sous le nom du webhook et que c'est le secret de cette source qui révoque un signalement sous ce nom. La source pousse sur `POST /api/v1/flags/{name}` et révoque par `DELETE /api/v1/flags/{name}/{external_id}`. |
| `secret` | — | La clé HMAC-SHA256. La poussée porte `X-Hub-Signature-256: sha256=<hex>` sur le corps brut (sur un `DELETE`, qui n'en a pas, sur la chaîne canonique `DELETE\n/api/v1/flags/{source}/{external_id}`, de sorte qu'une signature de révocation capturée ne lève que le signalement qu'elle nomme), le même schéma que vérifie `[[notifications.inbound]]`. Un nom inconnu et une mauvaise signature répondent le même `404`. |
| `max_effect` | `gate` | L'effet le plus fort que cette source peut poser. Une poussée qui en demande plus est stockée au plafond et en est informée (`effect_capped: true`). |
| `registries` | `[]` | Les registres que la source peut signaler. Un élément qui en nomme un autre est rejeté, élément par élément. |
| `max_flags_per_minute` | `600` | Éléments par minute, toutes poussées confondues ; au-delà, la poussée entière vaut `429`, avec un `Retry-After`. |

**Ce que fait un signalement.** Sur un registre doté d'un profil
[`[registries.security]`](#registries-security), le signalement est un constat du
verdict de la version : `hard_block` vaut `SOC_VERDICT` et refuse sous toute
politique — une version que cette instance a déjà jugée est refusée dès que la
poussée est acceptée, et la réanalyse qui suit redérive la même réponse ; `gate`
est jugé à la `severity` poussée, contre `max_severity` ; `warn` et `inform` sont
enregistrés pour le rapport. Sur un registre qui n'en a pas, `FlagsRule` lit les
signalements à chaque requête : `hard_block` refuse purement et simplement,
`gate` emprunte le seuil `cve_gate` du registre (`high` s'il n'est pas
configuré), et les deux autres ne refusent jamais. Une dérogation d'opérateur est
une exemption sur le garde-fou `flags` (`gates:exempt`, bornée dans le temps,
avec un motif), sur l'un comme sur l'autre type de registre.

**Ce qu'un signalement peut nommer.** Une `version` exacte, ou
`version_range = "*"` pour toutes les versions du paquet. Toute autre plage est
refusée élément par élément : une plage exige l'ordre des versions propre au type
de registre, ce qui est une RFC à part entière.

::: warning Une source à `hard_block` peut refuser tout téléchargement de ce qu'elle nomme
Le serveur avertit au démarrage (`flag-source.can-hard-block`) pour chaque source
dont le plafond est `hard_block`. Il n'y a sur ce chemin ni seuil ni contournement
par rôle ; le seul recours est une exemption de garde-fou sur la version.
:::

L'administrateur lit ce qui a été poussé avec `GET /api/v1/admin/flags`
(`flags:read`) et demande *qui a récupéré une version signalée* avec
`GET /api/v1/admin/exposure` (`audit:read`) — voir
[Réponse à incident](/fr/operations/incident-response#who-pulled-a-flagged-version).

### 3.12 `[air_gap]` (facultatif) {#air-gap}

Un serveur qui **ne composera pas vers l'extérieur**
([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate)). Absente, ou avec
`enabled = false`, c'est exactement le comportement d'aujourd'hui ; cette section
est additive et `config_version` ne bouge pas.

```toml
[air_gap]
enabled             = true
bundle_trusted_keys = ["3b1f…"]   # clés publiques ed25519 en hexa acceptées à l'import
synthesise_listings = true        # répondre un listing depuis ce que l'instance détient
record_misses       = true
miss_retention_days = 90
```

| Champ | Type | Défaut | Notes |
|---|---|---|---|
| `enabled` | booléen | `false` | Aucun registre en mode proxy ne tente de connexion amont. Un hit de cache est servi exactement comme aujourd'hui ; un défaut est un `503` immédiat qui nomme le registre et la coordonnée, et non une erreur de connexion quelques secondes plus tard. |
| `bundle_trusted_keys` | chaîne[] | `[]` | Les clés publiques ed25519 de 32 octets, en hexadécimal, dont un lot importé doit porter la signature. **Obligatoire** quand `enabled` : une instance dont l'unique chemin d'entrée de contenu n'est pas authentifié est pire qu'une instance sans chemin d'entrée. |
| `synthesise_listings` | booléen | `true` | Un listing dont cette instance ne détient aucun document — le packument que lit `npm install`, la page simple que lit `pip`, la release qu'un `mise install` figé demande — est composé à partir des versions qu'elle *détient* et répondu en `200`, avec `X-BatleHub-Listing: synthesised` (RFC 0008-bis). Toute version qu'un tel listing nomme est servie à la requête suivante ; une version non détenue n'est pas nommée, de sorte que le client s'arrête de lui-même (`ETARGET`, « no matching distribution ») plutôt que de réessayer sur un `503`. Composé pour le packument npm, la page simple PyPI (JSON de la PEP 691 et HTML de la PEP 503), l'index sparse de cargo (depuis le manifeste de la crate, lu à l'import), les `@v/list`, `@latest` et `.info` de Go, `maven-metadata.xml`, l'index plat de NuGet, les releases GitHub, Forgejo et GitLab (liste et par tag), les `index.tab` et `index.json` de nodedist, le `versions/all` de SDKMAN, l'index compact de RubyGems, le `repodata.json` de conda, la page d'enregistrement NuGet et le `p2` de Composer (les quatre derniers depuis des faits que l'import lit dans le paquet). Terraform n'est pas composé et reste un `503`. `false` est le comportement de la RFC 0008 : tout listing non détenu est un `503` et un défaut enregistré. Lu uniquement sous `enabled`. |
| `record_misses` | booléen | `true` | Enregistrer ce qui a été demandé et n'était pas détenu, une ligne par `(registre, clé)` avec un compteur. Cet enregistrement est l'entrée du prochain lot. |
| `miss_retention_days` | u32 | `90` | Combien de temps un défaut enregistré est conservé. `0` le garde jusqu'à une purge manuelle. |

**Refusé au chargement**, parce que chacun est une contradiction qu'un opérateur
doit voir au démarrage plutôt que découvrir dans un log :

| Condition | Pourquoi |
|---|---|
| `enabled = true` avec `[proxy]` ou le `[registries.proxy]` d'un registre | Un proxy sortant est une route vers l'extérieur. |
| `enabled = true` avec `warm_packages` ou `warm_paths` sur un registre | Le préchauffage récupère depuis un amont que ce mode garantit ne jamais composer. Amorcez avec un lot à la place. |
| `enabled = true` avec `bundle_trusted_keys = []` | L'import accepterait n'importe quel lot. |
| Une entrée de `bundle_trusted_keys` qui ne fait pas 64 caractères hexadécimaux | Une clé inutilisable ne doit pas se lire comme « la signature est configurée ». |
| `synthesise_listings = true` avec `enabled = false` | La clé n'a aucun effet sur une instance connectée et laisserait croire que celle-ci répond des listings hors ligne. |

Le même contrôle hexadécimal s'applique désormais au `trusted_keys` de
`[registries.signing]` de chaque registre, qui n'en avait aucun : une faute de
frappe y apparaissait auparavant sous la forme d'un `502` au premier
téléchargement, et ne nommait rien.

**Signalé**, parce que chacun est légitime et qu'aucun ne signifie ce qu'il a
l'air de signifier : un registre hybrid sous `enabled = true` se comporte comme
un local (son repli ne peut jamais atteindre l'amont —
`air-gap.hybrid-registry`) ; des `bundle_trusted_keys` sur une instance connectée
n'autorisent que des imports, ce qui est la façon de préparer un lot
(`air-gap.keys-unused`) ; et un registre `deb`, `rpm`, `pacman`, `jetbrains` ou
`generic` sous `enabled = true` n'obtient aucun index synthétisé — un fichier
`Packages` signé ne peut pas être re-signé ici — de sorte que son listing reste
un `503` tandis qu'un fichier détenu est servi par chemin
(`air-gap.listing-not-synthesised`).

**Ce que voit un opérateur.** Un défaut est un `503` avec
`{"code": "content_unavailable", "registry", "coordinate", "bundle_hint"}` ; un
hôte que rien ne réplique, atteint par la règle de réécriture attrape-tout, est
un `501` sur `/_air-gap/unmirrored/{host}/…` et ne récupère rien. Une coordonnée
qu'un administrateur a **bloquée** répond `403` et n'est jamais enregistrée comme
manquante — un paquet bloqué n'est pas un trou dans le miroir. Voir
[la procédure de coupure réseau](/fr/operations/air-gap) et
[Faire pointer mise vers BatleHub](/fr/use/mise).

### 3.13 `[[release_imports]]` (facultatif) {#release-imports}

Une release de forge dans le registre qui la sert
([RFC 0021](/rfc/0021-forge-releases-into-registries)). La CI construit un
artefact et l'attache à une release ; un registre `github`, `gitlab` ou `forgejo`
rend cet asset *téléchargeable*, et ceci le rend **installable** — l'extension
apparaît dans la vue Extensions d'un éditeur, le paquet dans l'index de `pip`,
parce que l'import le publie dans le registre dont le client parle le protocole.

```toml
[[release_imports]]
into          = "vsx-local"              # un registre local ou hybrid
from          = "gh"                     # un registre github/gitlab/forgejo configuré
repo          = "batleforc/batlehub-vsx" # owner/repo sur cette forge
assets        = ["*.vsix"]               # motifs glob ; jamais vide
releases      = "latest"                 # latest | all | un tag
interval_secs = 3600                     # absent : ne tourne que sur demande

[release_imports.as]
user_id = "svc-release-import"
groups  = ["config:extension-publishers"]
```

| Clé | Défaut | Signification |
| --- | --- | --- |
| `into` | — | Le registre dans lequel on publie. Local ou hybrid : un import est une publication, et une publication dans un registre en mode proxy est un `404`. |
| `from` | — | Le registre par lequel on récupère — un registre de forge configuré, jamais une URL, de sorte que la récupération garde l'identifiant, la liste d'autorisation et le garde-fou SSRF de ce registre. |
| `repo` | — | `owner/repo` sur la forge source. |
| `assets` | — | Des motifs glob de noms d'asset, où `*` correspond à toute suite de caractères. Obligatoire : une release porte des sommes de contrôle et des signatures à côté de l'artefact, et les publier comme paquets est ce que ferait une liste vide. |
| `releases` | `latest` | `latest` est la release la plus récente qui n'est ni un brouillon ni une pre-release. `all` est toute release publiée. Toute autre valeur est lue comme un tag — le seul moyen d'importer une pre-release. Un **brouillon n'est jamais importé**, quel que soit le réglage. |
| `interval_secs` | absent | À quelle fréquence cet import s'exécute de lui-même. Le plancher est de 300 s et s'applique au débit **combiné** de tous les imports qui partagent un même `from` : la limite de débit d'une forge est dépensée par l'identifiant, pas par un import en particulier. |
| `as.user_id` | — | Qui est la publication. Ce que les autorisations nomment (`user:<id>`), ce à quoi le quota est imputé, et ce que la ligne d'audit enregistre. |
| `as.groups` | `[]` | Les groupes, chacun écrit `config:<name>`. Le préfixe est réservé, pour qu'un fichier de configuration ne puisse pas fabriquer une chaîne de groupe qui appartient à un fournisseur d'identité. |

**Sous quelle identité cela publie.** Le bloc `as` déclare un *principal*, pas
un identifiant : aucun token n'est émis, stocké ni envoyé, parce que le serveur ne
s'authentifie pas auprès de lui-même. C'est toujours un **utilisateur**, jamais
un admin — un admin saute le contrôle d'appartenance au namespace, donc un import
configuré comme tel pourrait publier dans n'importe quel namespace de la cible.
Vérifiez ce qu'il a le droit de faire avant la première exécution :

```bash
batlehub authz explain vsx-local \
  --subject user:svc-release-import \
  --action releases:publish \
  --package batlehub.batlehub-vsx
```

**Lancez-en un maintenant**, quel que soit l'intervalle, et importez une
pre-release par son nom :

```bash
batlehub-cli admin import vsx-local
batlehub-cli admin import vsx-local --tag v1.1.0-rc.1
```

La même chose en HTTP, pour un pipeline sans CLI :

```bash
curl -fX POST -H "Authorization: Bearer $TOKEN" \
  "$HUB/api/v1/admin/registries/vsx-local/import"

curl -fX POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"tag": "v1.1.0-rc.1"}' \
  "$HUB/api/v1/admin/registries/vsx-local/import"
```

**Lire ce qui est configuré et quand cela a tourné pour la dernière fois.**
`GET /api/v1/admin/imports` liste chaque import configuré avec sa dernière
exécution ; la forme par registre est
`GET /api/v1/admin/registries/{registry}/imports`. C'est ce que lit la page
[Imports de releases](/fr/guide/administration) de la console.

Un `last_run` à `null` signifie que l'import **n'a pas tourné** ; une exécution
avec `imported: 0` signifie qu'il a tourné et n'a rien trouvé de neuf. Ce sont
deux états différents et l'intervalle rend les deux normaux, ce pourquoi ils sont
distinguables plutôt que réduits tous deux à une cellule vide. Une exécution
porte `triggered_by` quand un opérateur l'a demandée, et l'omet quand c'est la
planification qui a déclenché.

Demander une exécution exige `cache:warm` ; la publication elle-même s'exécute
sous le principal et exige le `releases:publish` de ce principal. La réponse
compte ce qui a été importé, ce qui a été **sauté** parce que le registre le
détient déjà — ce qui rend un intervalle gratuit à régler — et nomme chaque asset
en échec.

**Sur un registre de galerie, configurez
[`[registries.vsx_signing]`](#vsx-signing).** Une extension importée est signée à
la publication exactement comme une extension envoyée, et un VS Code récent grise
le bouton Installer sur une entrée qu'il ne peut pas vérifier. La configuration
avertit au chargement quand la cible n'a pas de clé.

---

## 4. Référence des permissions {#_4-permissions-reference}

### Les rôles

Trois rôles intégrés sont évalués avec héritage : `admin` hérite de toutes les
permissions de `user`, et `user` de toutes celles d'`anonymous`. Autrement dit,
si `anonymous` peut faire `releases:read`, les admins le peuvent aussi, sans
répéter la permission.

| Rôle | Description |
|---|---|
| `anonymous` | Requête non authentifiée, ou aucun fournisseur d'authentification ne correspond |
| `user` | Authentifié avec succès par n'importe quel fournisseur |
| `admin` | Accès complet |

### Les chaînes de permission

| Permission | Signification |
|---|---|
| `releases:read` | Lister les publications et télécharger leurs assets |
| `source:read` | Télécharger les tarballs de sources |
| `quarantine:read` | Voir qu'une version est retenue ou refusée, ses codes de motif et sa date de disponibilité (défaut : `user`, `admin`) |
| `findings:read` | Voir les constats derrière ces codes — identifiants CVE, sortie de scanner, texte SOC (défaut : `admin`) |
| `*` | Toutes les permissions (joker) |

### Les permissions par groupe

Les groupes complètent les permissions de rôle — une requête passe si elle
satisfait le contrôle de rôle *ou* n'importe quel contrôle de groupe. Les
permissions des rôles et des groupes s'additionnent (union).

Les noms de groupe de `[registries.rbac.groups]` sont comparés aux chaînes de
groupe préfixées que produisent les fournisseurs d'authentification :

- **Correspondance exacte :** `"oidc:team-a"` — ne correspond qu'à `team-a` chez
  le fournisseur nommé `"oidc"` ;
- **Préfixe joker :** `"*:team-a"` — correspond à `team-a` chez n'importe quel
  fournisseur (`oidc:team-a`, `kubernetes:team-a`, etc.).

Exemple :

```toml
[registries.rbac.groups]
"oidc:developers" = ["releases:read", "source:read"]
"*:ops"           = ["*"]
```

---

## 5. Surcharges par variables d'environnement {#_5-environment-variable-overrides}

BatleHub offre deux mécanismes complémentaires pour injecter des valeurs de
variables d'environnement dans le fichier de configuration.

### 5.1 Substitution en ligne — `${VAR_NAME}` {#env-inline}

Écrivez `${VAR_NAME}` n'importe où dans une **chaîne** TOML. BatleHub remplace
chaque marqueur par la valeur de la variable d'environnement correspondante avant
l'analyse du TOML. C'est la façon recommandée d'injecter des secrets : secrets
client OIDC, tokens d'authentification amont, mots de passe.

**Les règles :**

| Syntaxe | Signification |
|---|---|
| `${VAR_NAME}` | Remplacé par `$VAR_NAME` au démarrage. Erreur si la variable n'est pas définie. |
| `$${VAR_NAME}` | Produit la chaîne littérale `${VAR_NAME}` — aucune recherche. |
| Tout autre `$` | Laissé inchangé. |

> Si une variable référencée n'est pas définie, BatleHub s'arrête immédiatement
> avec un message d'erreur clair qui la nomme. Il n'y a ni repli silencieux ni
> défaut à la chaîne vide — c'est délibéré, pour empêcher un déploiement mal
> configuré de démarrer.

**Le secret client OIDC :**

```toml
[[auth]]
type = "oidc"
issuer_url = "https://sso.example.com/application/o/batlehub/"
client_id   = "batlehub"
client_secret = "${OIDC_CLIENT_SECRET}"   # export OIDC_CLIENT_SECRET=<value>
redirect_uri  = "https://hub.example.com/api/v1/auth/oidc/callback"
```

**Registre amont — token bearer :**

```toml
[[registries]]
type = "npm"
name = "internal-npm"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/npm"]

[registries.upstream_auth]
type  = "bearer"
token = "${INTERNAL_NPM_TOKEN}"   # export INTERNAL_NPM_TOKEN=npat-xxxx
```

**Registre amont — authentification basic :**

```toml
[[registries]]
type     = "cargo"
name     = "internal-cargo"
upstreams = ["https://nexus.corp.example.com/repository/cargo-proxy/"]

[registries.upstream_auth]
type     = "basic"
username = "deploy"
password = "${INTERNAL_CARGO_PASSWORD}"   # export INTERNAL_CARGO_PASSWORD=s3cr3t
```

**Registre amont — en-tête personnalisé :**

```toml
[[registries]]
type     = "npm"
name     = "api-keyed-npm"
upstreams = ["https://nexus.corp.example.com/repository/npm-proxy/"]

[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "${INTERNAL_NPM_API_KEY}"   # export INTERNAL_NPM_API_KEY=my-api-key
```

**Kubernetes et Docker Compose :** montez un Secret en variable
d'environnement et référencez-le depuis le fichier de configuration.

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

**L'échappement :** si une valeur de configuration a légitimement besoin de la
chaîne `${...}` (un modèle d'URL, par exemple), écrivez `$${...}` :

```toml
# Ceci stocke la chaîne littérale "${MY_VAR}" — aucune recherche de variable :
some_template = "$${MY_VAR}/suffix"
```

---

### 5.2 Surcharges nommées — `PROXY_CACHE__*` {#env-named}

Un ensemble fixe de champs de premier niveau se remplace aussi par des variables
d'environnement nommées. C'est utile pour un déploiement en conteneur, où le
fichier de configuration est cuit dans l'image et où il faut ajuster des adresses
d'infrastructure (hôte, port, URL de base) sans reconstruire.

| Variable | Champ de configuration | Notes |
|---|---|---|
| `PROXY_CACHE__SERVER__HOST` | `server.host` | |
| `PROXY_CACHE__SERVER__PORT` | `server.port` | Analysé en u16 |
| `PROXY_CACHE__SERVER__STATIC_DIR` | `server.static_dir` | |
| `PROXY_CACHE__DATABASE__URL` | `database.url` | |
| `PROXY_CACHE__DATABASE__MAX_CONNECTIONS` | `database.max_connections` | Analysé en u32 |
| `PROXY_CACHE__STORAGE__PATH` | `storage.path` | Backend unique sur système de fichiers uniquement |
| `PROXY_CACHE__STORAGE__BUCKET` | `storage.bucket` | Backend S3 unique uniquement |
| `PROXY_CACHE__STORAGE__REGION` | `storage.region` | Backend S3 unique uniquement |
| `PROXY_CACHE__STORAGE__ENDPOINT_URL` | `storage.endpoint_url` | Backend S3 unique uniquement |
| `PROXY_CACHE__OTEL__ENDPOINT` | `otel.endpoint` | Crée la section `[otel]` si elle est absente |
| `PROXY_CACHE__OTEL__SERVICE_NAME` | `otel.service_name` | |
| `PROXY_CACHE__PROXY__URL` | `proxy.url` | Crée la section `[proxy]` si elle est absente ; s'applique à tous les registres |
| `PROXY_CACHE__PROXY__USERNAME` | `proxy.username` | |
| `PROXY_CACHE__PROXY__PASSWORD` | `proxy.password` | |
| `PROXY_CACHE__PROXY__NO_PROXY` | `proxy.no_proxy` | |

> Les surcharges de stockage par variables d'environnement ne fonctionnent
> qu'avec la forme `[storage]` à **backend unique**. Une configuration
> multi-backend (`[[storage.backends]]`) se modifie dans le fichier.

> **Choisir entre les deux mécanismes :** employez les marqueurs `${VAR_NAME}`
> pour les **secrets** (tokens d'authentification, mots de passe, secrets
> client) — ils fonctionnent sur tous les champs et gardent les identifiants hors
> du fichier TOML. Employez les variables `PROXY_CACHE__*` pour les **adresses
> d'infrastructure** (URL de base, chemin de stockage, hôte et port), dont la
> valeur n'est pas secrète mais varie d'un environnement à l'autre.

---

---

## 6. Ce qui se trouvait ici

Cette page est la référence de configuration, et elle a longtemps été six autres
documents en plus : des exemples commentés, les sous-commandes du binaire
serveur, les tokens d'API personnels, le rechargement à chaud, les amonts privés,
et une seconde copie de la documentation SBOM. À 15 706 mots, elle représentait un
quart de tout ce qui est publié, et une sous-section numérotée 6.16 siégeait dans
la section 11 depuis assez longtemps pour que personne ne puisse dire laquelle
des deux était fausse (RFC 0005-bis).

Ce sont désormais des pages, trouvables par leur nom :

| Quoi | Où |
| --- | --- |
| Exemples commentés — des `config.toml` complets par scénario | [Exemples commentés](/fr/guide/configuration-examples) |
| `batlehub dump-spec`, `batlehub hash-token` | [Sous-commandes du binaire serveur](/fr/guide/server-cli) |
| Créer et révoquer son propre token d'API | [Utiliser BatleHub → tokens](/fr/use/#tokens-api) |
| Recharger la configuration sans redémarrer | [Rechargement à chaud](/fr/guide/hot-reload) |
| Faire proxy d'un amont privé ou auto-hébergé | [Amonts privés](/fr/guide/private-upstreams) |
| La génération de SBOM, ses endpoints et la correspondance des PURL | [SBOM](/fr/guide/sbom) |
| Dimensionner une instance | [Dimensionnement](/fr/guide/capacity-planning) |
