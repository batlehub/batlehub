---
sourcePath: registries/terraform.md
sourceHash: f45cb8ea10f0abed
---

# Terraform

Fait proxy et cache du protocole de registre Terraform pour les providers et
les modules (API v1), ou héberge des modules et des providers privés. BatleHub
sert les listes de versions de providers, les informations de téléchargement de
provider, les listes de versions de modules et le téléchargement des sources de
modules, sous contrôle du RBAC et du garde-fou d'âge de publication.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `terraform` |
| **Amont par défaut** | `registry.terraform.io` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ envoi de modules et de providers |
| **Coupure réseau** | hors ligne, les `versions` de providers et de modules sont composées à partir de l'ensemble détenu, ainsi que le document `download` d'un provider pour une plateforme dont l'archive, les `shasums` et la `shasums.sig` sont toutes détenues — le plan doit nommer les trois chemins — les clés de signature du publieur étant portées par le manifeste du lot que produit `mise export` ; `terraform init` vérifie la signature du publieur comme il le ferait connecté, et cette instance ne signe rien. Une plateforme à laquelle il manque l'un des trois donne un `503` |

## Mise en place du proxy

BatleHub parle **les deux** protocoles Terraform. Ce ne sont pas des
alternatives : choisissez selon ce dont vous avez besoin et selon la façon dont
votre instance est jointe.

### Miroir réseau de providers — fonctionne partout

Un miroir sert **uniquement des providers**, n'a pas besoin de découverte de
service, et fonctionne avec un routage par chemin ordinaire. C'est l'option la
plus simple, et la bonne pour un parc coupé du réseau qui n'a besoin que de
mettre en cache des providers publics.

```hcl
# ~/.terraformrc  (%APPDATA%/terraform.rc sous Windows)
provider_installation {
  network_mirror {
    url = "https://batlehub.example.com/proxy/<registry>/"
  }
}

credentials "batlehub.example.com" {
  token = "<your-token>"
}
```

Le segment `{hostname}` d'une URL de miroir nomme le registre *d'origine*, et
BatleHub le confronte à l'amont configuré du registre : pointer un miroir de
`registry.terraform.io` vers un registre qui réplique autre chose donne un `404`
plutôt que d'attacher silencieusement la mauvaise provenance.

::: warning Deux exigences de Terraform envers un miroir
Toutes deux mesurées avec Terraform 1.8.5.

**Le miroir doit être une URL `https:`.** Terraform refuse purement et simplement
un miroir en HTTP simple — *« the mirror must be at an https: URL »* — de sorte
qu'une instance locale sur `http://localhost:8080` ne peut pas en servir du tout.

**Terraform n'authentifie pas le téléchargement du provider.** Il envoie le token
de votre bloc `credentials` à l'`index.json` et au `{version}.json` du miroir,
puis récupère l'archive du provider **sans identifiants**. Il en va de même du
protocole de registre, ainsi que des `SHA256SUMS` et de la `.sig` qu'il récupère
à côté de l'archive : mesuré avec Terraform 1.8.5, chaque document de protocole
est authentifié et aucune récupération d'artefact ne l'est — y compris sur l'hôte
qu'il a authentifié une requête plus tôt.

Vous avez deux façons de vivre avec, et la seconde est récente :

- **Ouvrir le registre** — `anonymous = ["releases:read", "source:read"]` sous
  `[registries.rbac]`, ou un ingress qui authentifie devant lui. C'est l'option
  brutale : l'autorisation porte sur le *registre*, donc ouvrir la dernière étape
  de l'installation d'un provider ouvre toutes les listes de versions et, en mode
  hybrid, tout ce qui est publié localement.
- **Signer les téléchargements** — [`signed_downloads = true`](#signed-downloads),
  ce qui permet de garder `anonymous = []`. BatleHub place une signature à courte
  durée de vie, portant sur une seule coordonnée, dans le document que Terraform
  *a bien* authentifié, et l'accepte sur les récupérations qui ne portent aucun
  en-tête.

La [galerie VS Code](/fr/registries/vscode-marketplace) a la même contrainte
pour la même raison, et n'a pas encore la seconde option.
:::

### Protocole de registre — exige un routage par hôte {#registry-protocol}

Le protocole de registre sert **les modules et les providers**, et Terraform
l'atteint par nom : `source = "<host>/<namespace>/<type>"`. C'est exactement
trois segments, donc `batlehub.example.com/proxy/<registry>/myorg/mycloud` n'est
pas une adresse de source valide — elle en a cinq.

Terraform trouve par ailleurs les endpoints d'un registre en récupérant
`https://<host>/.well-known/terraform.json`, qui est ancré à la racine de l'hôte
par le protocole. Les deux faits vont dans le même sens : **le protocole de
registre exige que le registre soit lié à son propre nom d'hôte**. Configurez
cela avec `[subdomain_routing]` ou un hôte dédié (voir
[Routage par hôte](/fr/guide/host-routing)), puis :

```hcl
# ~/.terraformrc
credentials "tf.example.com" {
  token = "<your-token>"
}
```

```hcl
# main.tf
terraform {
  required_providers {
    mycloud = {
      source  = "tf.example.com/myorg/mycloud"
      version = "~> 1.0"
    }
  }
}

module "consul" {
  source  = "tf.example.com/hashicorp/consul/aws"
  version = "0.1.0"
}
```

Sur une requête routée par chemin, `/.well-known/terraform.json` répond `404`
avec le motif, plutôt que de deviner lequel des registres de cet hôte il devrait
décrire.

::: warning L'hôte doit être en HTTPS, et BatleHub doit le savoir
Terraform ne parlera pas en clair à un hôte de registre et n'offre aucune
dérogation — la même règle que pour le miroir réseau ci-dessus. Derrière un
terminateur TLS, il faut de plus indiquer à BatleHub que le schéma du client
était `https`, parce qu'il écrit des URL absolues dans le document de
téléchargement à partir de ce qu'il voit : sans `X-Forwarded-Proto` de confiance,
il annonce `http://<host>` et Terraform échoue ensuite en essayant de l'atteindre.
Déclarez l'adresse du terminateur dans `trusted_proxies` :

```toml
[server]
trusted_proxies = ["10.42.0.0/16"]   # les plages CIDR de votre ingress
```

Le routage par hôte refuse de démarrer sans position explicite ici — `[]` si
BatleHub est exposé directement — de sorte que l'échec est une erreur de
démarrage plutôt qu'une URL silencieusement fausse.
:::

::: tip Les téléchargements passent par le proxy
Quel que soit le protocole employé, l'URL d'archive que BatleHub donne à
Terraform pointe vers BatleHub — jamais vers le CDN amont. C'est ce qui fait
passer les octets des providers et des modules par le garde-fou de politique, le
cache et la piste d'audit. Les premières versions transmettaient l'URL amont, et
le téléchargement contournait donc les trois.
:::

## Publication (local / hybrid) {#publishing-local-hybrid}

BatleHub prend en charge les registres privés de **providers** comme de
**modules**. Les modules s'envoient par un simple tarball. Les providers suivent
un processus en deux étapes : envoi d'un manifeste de version (un JSON décrivant
les plateformes et les sommes de contrôle), puis envoi de chaque binaire de
plateforme.

### Configuration du serveur

```toml
[[registries]]
type = "terraform"
name = "internal-tf"
mode = "local"          # ou "hybrid" pour se rabattre sur registry.terraform.io

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://registry.terraform.io"]`.

### Fermer le registre avec des téléchargements signés {#signed-downloads}

Terraform récupère l'archive du provider, ses `SHA256SUMS` et la signature
détachée qui les couvre **sans aucun en-tête `Authorization`**, et n'a aucun
mécanisme pour en envoyer un. Sans aide, la seule façon de faire fonctionner une
installation est d'accorder la lecture anonyme sur tout le registre.

`signed_downloads` supprime ce compromis. BatleHub inscrit une signature dans le
document que Terraform *a bien* authentifié, et l'accepte sur les trois
récupérations qui ne portent aucun en-tête :

```toml
[server.signed_urls]
# 32 octets minimum. Interpolé depuis l'environnement comme tout autre
# identifiant de ce fichier — voir « Valeurs sensibles » dans le guide de
# configuration.
secret      = "${BATLEHUB_URL_SIGNING_SECRET}"
ttl_seconds = 300                # défaut ; plafonné en dur à 3600

[[registries]]
type             = "terraform"
name             = "internal-tf"
signed_downloads = true

[registries.rbac]
anonymous = []                   # désormais possible
user      = ["releases:read", "source:read"]
```

Ce qu'est précisément cette signature : une capacité de cinq minutes pour **un
registre, un paquet, une version, une plateforme, une méthode**. Elle porte
l'identité qui a récupéré le document, et sa vérification remet cette identité à
la même chaîne de règles, au même quota et au même audit que n'importe quel autre
téléchargement. Elle authentifie une requête ; elle n'autorise rien. Une version
bloquée après l'émission de l'URL reste bloquée, parce que le blocage est évalué
au moment où l'URL est présentée.

Trois conséquences à connaître avant de l'activer :

- **`GET /api/v1/admin/audit-log` nomme l'utilisateur** pour les téléchargements
  de providers, là où il n'enregistrait auparavant aucun acteur — avec
  `anonymous` accordé, la chaîne de règles évaluait *anonymous*, donc les
  autorisations de groupe ne s'appliquaient jamais et le quota n'était imputé à
  personne.
- **Le token peut atteindre vos logs — mais pas ceux de BatleHub.** Le span de
  requête de BatleHub renseigne `http.target` à partir du *seul* chemin de la
  requête, délibérément (voir la note ci-dessous). Tout ce qui se trouve ailleurs
  sur le trajet et journalise une URL complète voit encore la signature, le temps
  de sa durée de vie.
- **`signed_downloads = true` sans `[server.signed_urls].secret` est une erreur
  de démarrage**, pas un avertissement. Un registre qui se croit fermé et ne
  l'est pas est exactement la panne que cette fonctionnalité existe pour éviter.

::: warning La signature peut atteindre des logs qui ne sont pas ceux de BatleHub
Une URL émise est une capacité au porteur jusqu'à son expiration : tout ce qui
enregistre une URL de requête complète enregistre donc le token. Le constructeur
de span de `tracing-actix-web` renseigne `http.target` à partir du chemin *et de
la chaîne de requête*, ce pourquoi BatleHub ne l'emploie pas :
`BatleHubSpanBuilder` (`server/src/server_factory.rs`) en est une
réimplémentation champ pour champ, dont l'unique écart est
`http.target = uri.path()`, et un test vérifie que la cible du span ne porte
jamais de chaîne de requête.

Cela couvre ce serveur et rien d'autre. Un reverse proxy qui termine TLS devant
BatleHub, un CDN, ou la sortie `TF_LOG=DEBUG` de Terraform lui-même captureront
chacun l'URL entière. Ce que cela vaut pour qui lit ces logs reste borné : cinq
minutes par défaut, un fichier, et aucune permission que l'utilisateur signé
n'avait déjà. Si c'est encore plus que vous ne le voulez, les leviers sont :
abaisser `ttl_seconds`, et vérifier la configuration de journalisation de ce qui
se trouve devant. La piste d'audit, elle, est propre : `access_events`
enregistre la coordonnée du paquet, jamais l'URL.
:::

**Faire tourner le secret** n'exige ni redémarrage ni bascule générale. Mettez le
nouveau secret dans `secret`, déplacez l'ancien dans `previous_secrets`, et
rechargez : les URL émises sous l'un ou l'autre se vérifient, et seul le secret
courant en émet. Retirez l'ancienne entrée une fois passé le plus long
`ttl_seconds`.

```toml
[server.signed_urls]
secret           = "${BATLEHUB_URL_SIGNING_SECRET}"
previous_secrets = ["${BATLEHUB_URL_SIGNING_SECRET_OLD}"]
```

Une entrée qui s'interpole en chaîne vide est ignorée : la ligne
`previous_secrets` peut donc rester dans le fichier entre deux rotations — mais
la variable doit tout de même être *définie*. L'expansion `${VAR}` a lieu avant
l'analyse de la configuration et refuse une variable non définie, donc une fois
l'ancien secret retiré, ou bien supprimez la ligne, ou bien gardez
`BATLEHUB_URL_SIGNING_SECRET_OLD=""` exporté.

### Publier des modules

Un module Terraform est une archive `.tar.gz` du répertoire du module.

```sh
# Construire l'archive
tar -czf consul-aws-0.1.0.tar.gz -C /path/to/module .

# Envoyer
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/gzip" \
  --data-binary @consul-aws-0.1.0.tar.gz \
  "https://batlehub.example.com/proxy/internal-tf/v1/modules/hashicorp/consul/aws/0.1.0"
```

### Utiliser un module privé

Ajoutez les identifiants à `~/.terraformrc` :

```hcl
credentials "batlehub.example.com" {
  token = "<your-token>"
}
```

Référencez le module dans Terraform. Une source de module s'écrit
`<host>/<namespace>/<name>/<provider>` : cela exige donc que le registre soit lié
à son propre nom d'hôte (voir [Protocole de registre](#registry-protocol)
ci-dessus) :

```hcl
module "consul" {
  source  = "tf.example.com/hashicorp/consul/aws"
  version = "0.1.0"
}
```

### Publier des providers

**Étape 1 — envoyer le manifeste de version** (un JSON décrivant la version et
ses plateformes) :

```sh
curl -X POST \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/json" \
  -d '{
    "version": "1.0.0",
    "protocols": ["5.0"],
    "platforms": [
      {
        "os": "linux", "arch": "amd64",
        "filename": "terraform-provider-mycloud_1.0.0_linux_amd64.zip",
        "shasum": "<sha256-hex>"
      }
    ]
  }' \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/myorg/mycloud/versions"
```

**Étape 2 — envoyer les binaires de plateforme** :

```sh
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/zip" \
  --data-binary @terraform-provider-mycloud_1.0.0_linux_amd64.zip \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/myorg/mycloud/1.0.0/artifact/linux/amd64"
```

Répétez l'envoi du binaire pour chaque plateforme prise en charge.

### Utiliser un provider privé

```hcl
# ~/.terraformrc
credentials "tf.example.com" {
  token = "<your-token>"
}
```

```hcl
# main.tf
terraform {
  required_providers {
    mycloud = {
      source  = "tf.example.com/myorg/mycloud"
      version = "~> 1.0"
    }
  }
}
```

### Retirer une version (admin)

Servez-vous de l'API d'opérations en masse (voir le
[guide d'administration](/fr/guide/administration)) :

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages": [{"name": "modules/hashicorp/consul/aws", "versions": ["0.1.0"]}]}' \
  "https://batlehub.example.com/api/v1/admin/registries/internal-tf/bulk-yank"
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/terraform -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/.well-known/terraform.json` | `GET /.well-known/terraform.json` — the document Terraform reads first. |
| `GET` | `/proxy/{registry}/.well-known/terraform.json` | The same document at the path the host-routing middleware actually produces. |
| `GET` | `/proxy/{registry}/{hostname}/{namespace}/{ptype}/{version}.json` | `GET {mirror}/{hostname}/{namespace}/{type}/{version}.json` — where one |
| `GET` | `/proxy/{registry}/{hostname}/{namespace}/{ptype}/index.json` | `GET {mirror}/{hostname}/{namespace}/{type}/index.json` — the versions a |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}` | `GET /v1/modules/{ns}/{name}/{provider}/{version}` — one module version's |
| `POST` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}` | Upload a Terraform module tarball to the local registry. |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/artifact` | Download the tarball for a locally-published Terraform module. |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/download` | Get the download URL for a specific Terraform module version. |
| `GET` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions` | List available versions for a Terraform module. |
| `DELETE` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}` | Yank a Terraform module version (local/hybrid registries only). |
| `POST` | `/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}/unyank` | Unyank a Terraform module version (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}` | Download a Terraform provider platform binary from local storage. |
| `PUT` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}` | Upload a platform binary for a locally-published Terraform provider. |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/download/{os}/{arch}` | Get download information for a specific Terraform provider version and platform. |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums` | The provider's checksum manifest (`SHA256SUMS`) and its detached signature. |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig` | The detached signature over the checksum manifest. See |
| `GET` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions` | List available versions for a Terraform provider. |
| `POST` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions` | Upload a Terraform provider version manifest (JSON describing version + platforms). |
| `DELETE` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}` | Yank a Terraform provider version (local/hybrid registries only). |
| `POST` | `/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}/unyank` | Unyank a Terraform provider version (local/hybrid registries only). |
<!-- END endpoints -->

---

## Versions bloquées

Les deux facettes de `/v1/{namespace}/versions` sont filtrées — les versions de
modules (imbriquées sous `modules[].versions`) et celles de providers (un tableau
`versions` à plat) — de sorte que `terraform init` ne sélectionne jamais une
version qui lui sera ensuite refusée en plein plan. Aucun des deux documents ne
nomme de version préférée : il n'y a donc rien à réparer au-delà du retrait de
l'entrée.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Terraform lit les identifiants par hôte dans le bloc
`credentials "batlehub.example.com"` de `~/.terraformrc` (montré plus haut) et
envoie le token dans un en-tête Bearer.

## Notes

- Les providers sont mis en cache après le premier téléchargement en mode proxy
  ou hybrid, ou servis entièrement depuis le stockage local en mode local.
- La réponse à l'envoi d'un module porte un en-tête `X-Terraform-Get` qui pointe
  vers l'URL de téléchargement de l'artefact.
- Les réponses de téléchargement de provider portent toujours un objet
  `signing_keys`. Terraform refuse un provider dont le document de téléchargement
  l'omet : le champ est donc présent (vide quand le registre ne publie aucune
  clé) plutôt qu'absent.
- `shasums_url` et `shasums_signature_url` nomment encore l'amont en mode proxy.
  L'*archive* du provider est proxifiée et filtrée ; son manifeste de sommes de
  contrôle ne l'est pas encore, de sorte qu'une installation de provider
  entièrement hors ligne n'est pas complète.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
