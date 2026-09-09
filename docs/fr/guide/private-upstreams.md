---
sourcePath: guide/private-upstreams.md
sourceHash: a3b4e2dba51f3881
---

# Amonts privés et auto-hébergés

N'importe quel registre peut faire proxy d'un amont privé ou auto-hébergé, en
combinant les champs `upstream_auth` et `tls`. Les deux sont facultatifs et
indépendants l'un de l'autre.

## Authentification amont

Trois schémas sont disponibles par `[registries.upstream_auth]` :

| `type` | Cas d'usage | Champs requis |
|--------|----------|-----------------|
| `bearer` | Gitea, Forgejo, GitHub Enterprise, tokens d'API Artifactory | `token` |
| `basic` | Nexus, Artifactory (mot de passe), la plupart des flux authentifiés en HTTP | `username`, `password` |
| `header` | Tout registre employant un en-tête personnalisé (par ex. `X-API-Key`) | `name`, `value` |

Les tokens bearer sont envoyés en `Authorization: Bearer <token>`. Les
identifiants basic sont attachés à chaque requête en HTTP Basic. Les en-têtes
personnalisés sont injectés comme en-têtes par défaut sur toutes les requêtes
amont.

## Certificats d'autorité personnalisés

Quand l'amont présente un certificat signé par une autorité privée, ajoutez le
certificat de cette autorité au magasin de confiance du système, **ou** faites
pointer `tls.ca_cert_path` vers un fichier PEM :

```toml
[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"
```

Ce réglage est propre à chaque registre : vous pouvez donc mélanger des registres
publics (sans configuration TLS) et des registres privés qui emploient une
autorité d'entreprise, dans un seul `config.toml`.

## Employer `upstream_auth` et `tls` ensemble

Les deux champs peuvent figurer dans le même bloc de registre :

```toml
[[registries]]
type      = "npm"
name      = "npm-private"
upstreams = ["https://nexus.corp.example.com/repository/npm-proxy/"]

[registries.upstream_auth]
type  = "header"
name  = "X-API-Key"
value = "my-api-key"

[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"
```

## Types de registre pris en charge

Tous les types de registre acceptent `upstream_auth` et `tls`. Ils sont lus une
fois, avant la construction du client, de sorte qu'un type ajouté plus tard en
bénéficie sans rien changer ici. Pour `cargo`, le proxy de l'index sparse
(l'endpoint `index_url`) emploie les mêmes identifiants et les mêmes réglages
TLS.

## Mêler un amont privé et un repli public

`upstream_auth` porte sur le bloc de registre, pas sur une URL. Quand
`upstreams` liste plusieurs URL, les identifiants configurés sont envoyés à
**toutes** les entrées de la liste. Cela pose problème quand on veut un amont
privé en source principale et un registre public en repli non authentifié : les
identifiants transmis au registre public peuvent produire un `401 Unauthorized`
plutôt qu'un `404 Not Found`, et la diffusion ne passe à l'amont suivant que sur
un `404` — un `401` arrête donc la chaîne immédiatement.

Le motif recommandé est :

1. Un **bloc de registre privé** pointant vers l'amont authentifié, avec
   `upstream_auth` configuré et la lecture anonyme activée, pour que BatleHub
   puisse l'atteindre sans token client.
2. Un **bloc de registre de diffusion** que les clients configurent réellement,
   dont la liste `upstreams` pointe d'abord vers l'URL de proxy de BatleHub pour
   le registre privé, puis vers le registre public.

BatleHub gère les identifiants en interne quand il se récupère lui-même : le bloc
de diffusion n'a donc jamais besoin de son propre `upstream_auth`.

```toml
# Étape 1 — registre Gitea privé avec identifiants.
# Le source:read anonyme est nécessaire pour que le bloc de diffusion ci-dessous
# puisse l'atteindre sans transmettre un token client.
[[registries]]
type      = "cargo"
name      = "internal-cargo"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/cargo"]
index_url = "https://gitea.corp.example.com/api/packages/myorg/cargo/index"

[registries.upstream_auth]
type  = "bearer"
token = "npat-xxxx"

[registries.rbac]
anonymous = ["source:read"]
user      = ["source:read"]
admin     = ["*"]

# Étape 2 — registre de diffusion : le privé d'abord (via l'auto-proxy BatleHub),
# le public en repli. Les clients ne configurent que celui-ci.
[[registries]]
type      = "cargo"
name      = "cargo"
upstreams = [
  "http://localhost:8080/proxy/internal-cargo",  # BatleHub fait proxy avec les identifiants stockés
  "https://static.crates.io/crates",             # repli public — aucune authentification
]
index_url = "https://index.crates.io"

[registries.rbac]
anonymous = ["source:read"]
user      = ["source:read"]
admin     = ["*"]
```

Les clients ne configurent que le registre de diffusion :

```toml
# ~/.cargo/config.toml
[registries.cargo]
index = "sparse+https://batlehub.example.com/proxy/cargo/registry/"
```

Quand BatleHub résout une crate par le registre `cargo`, il récupère d'abord
`http://localhost:8080/proxy/internal-cargo/…` ; cette requête vers lui-même est
servie par le registre `internal-cargo`, qui injecte le token bearer Gitea au
départ. Si la crate est introuvable (404), BatleHub se rabat sur crates.io sans
aucun identifiant. Le client ignore jusqu'à l'existence du registre privé.

## Gestion des secrets

Les valeurs d'identifiants (`token`, `password`, `value`) sont stockées dans le
fichier de configuration TOML. En production :
- employez un gestionnaire de secrets (Vault, AWS Secrets Manager, Secrets
  Kubernetes) pour injecter les valeurs à l'exécution ;
- beaucoup d'outils de déploiement (Helm, Kustomize, `EnvironmentFile` de
  systemd) savent substituer des références de variables d'environnement dans un
  fichier de configuration avant le démarrage du processus.

Voir l'[exemple commenté 6.5](/fr/guide/configuration-examples#_6-5-self-hosted-private-registries)
pour une configuration multi-registres complète.
