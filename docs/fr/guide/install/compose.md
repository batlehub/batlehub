---
sourcePath: guide/install/compose.md
sourceHash: 928a1bccbc7d4e87
---

# Docker Compose

Trois piles prêtes à l'emploi se trouvent sous
[`deploy/compose/`](https://github.com/batlehub/batlehub/tree/main/deploy/compose),
chacune un `docker-compose.yaml` accompagné du `config.toml` qu'il monte. Elles
lancent l'image publiée (`ghcr.io/batlehub/batlehub:1.7.0`) et ne diffèrent que
par ce qui l'entoure :

| Pile | Services | Connexion | Cache | Artefacts |
| --- | --- | --- | --- | --- |
| `min` | PostgreSQL, BatleHub | token d'admin | dans le processus | volume local |
| `base` | + Dex | OIDC (Dex) et token d'admin | dans le processus | volume local |
| `max` | + Valkey, RustFS | OIDC (Dex) et token d'admin | Valkey | S3 (RustFS) |

Chaque configuration proxifie un registre `npm` vers `registry.npmjs.org` ;
ajoutez les registres dont vous avez besoin à partir des
[pages des registres](/fr/registries/) et de la
[Configuration](/fr/guide/configuration). Aucun secret n'est écrit dans les
fichiers : chacun est une variable d'environnement, et `docker compose` refuse
de démarrer tant qu'il en manque une.

```sh
git clone https://github.com/batlehub/batlehub
cd batlehub/deploy/compose/min      # ou base, ou max
```

## `min` — le serveur et sa base

```sh
BATLEHUB_ADMIN_TOKEN=$(openssl rand -hex 32) docker compose up -d
```

La console est sur `http://localhost:8080`, npm sur
`http://localhost:8080/proxy/npm/`. Le token d'admin est le seul identifiant,
pour l'API et la [CLI](/fr/use/cli).

## `base` — avec une connexion OIDC

```sh
export BATLEHUB_ADMIN_TOKEN=$(openssl rand -hex 32)
export OIDC_CLIENT_SECRET=$(openssl rand -hex 32)
export DEX_ADMIN_PASSWORD_HASH=$(docker run --rm httpd:2.4-alpine htpasswd -nbBC 10 '' 'votre-mot-de-passe' | tr -d ':\n')
docker compose up -d
```

**Se connecter**, dans la console, redirige vers Dex, où `admin@example.com` se
connecte avec le mot de passe haché ci-dessus et arrive en administrateur.
`dex.yaml` contient l'utilisateur et le client ; le hash de l'utilisateur et le
secret du client sont lus dans l'environnement de Dex, si bien qu'aucun hash
bcrypt n'est commité. Pour plus d'une personne, remplacez l'utilisateur
statique par un vrai [connecteur Dex](https://dexidp.io/docs/connectors/)
(LDAP, GitHub, un OIDC amont) et associez ses claims dans
`[auth.role_mappings]`.

BatleHub tourne dans **l'espace réseau de Dex** (`network_mode: service:dex`),
c'est pourquoi le port `8080` est publié sur le service `dex`. L'issuer OIDC
doit être une seule URL que le navigateur et le serveur atteignent tous les
deux, et BatleHub n'accepte un issuer en HTTP simple que sur la boucle locale :
partager l'espace réseau rend `http://localhost:5556/dex` valable pour les deux.
Derrière un reverse proxy TLS, donnez plutôt à Dex un issuer en `https://`, et
les deux services peuvent être séparés.

## `max` — cache partagé et stockage S3

```sh
export BATLEHUB_ADMIN_TOKEN=$(openssl rand -hex 32)
export OIDC_CLIENT_SECRET=$(openssl rand -hex 32)
export S3_SECRET_KEY=$(openssl rand -hex 32)
export DEX_ADMIN_PASSWORD_HASH=$(docker run --rm httpd:2.4-alpine htpasswd -nbBC 10 '' 'votre-mot-de-passe' | tr -d ':\n')
docker compose up -d
```

`base`, plus [Valkey](https://valkey.io) comme backend de `[cache]` (il parle
le protocole Redis) et [RustFS](https://rustfs.com) comme `[storage]` S3. Un
conteneur ponctuel `s3-init` crée le bucket `batlehub` avant le démarrage du
serveur, puisque BatleHub ne le crée pas. Ce sont les deux briques dont une
deuxième réplique a besoin — voir la [haute disponibilité](/fr/guide/high-availability) —
et la console de RustFS est sur `http://localhost:9001`.

## Vérifier

```sh
curl http://localhost:8080/healthz
docker compose logs -f batlehub
```

L'image est distroless, sans shell pour exécuter un healthcheck de conteneur ;
`/healthz` répond dès que le serveur est démarré.

## Les fichiers Compose à la racine du dépôt

`docker-compose.yml` et `docker-compose.s3.yml` sont les piles de
**développement** : une base, Authentik, RustFS et Jaeger, pour faire tourner
un serveur compilé depuis les sources (`task run:s3`). Elles ne lancent pas
l'image publiée ; voir [Contributing](/contributing/contributing) (en anglais).

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
