# Docker Compose

Three ready-made stacks live under
[`deploy/compose/`](https://github.com/batlehub/batlehub/tree/main/deploy/compose),
each a `docker-compose.yaml` with the `config.toml` it mounts beside it. They
run the released image (`ghcr.io/batlehub/batlehub:1.7.0`) and differ only in
what surrounds it:

| Stack | Services | Sign-in | Cache | Artifacts |
| --- | --- | --- | --- | --- |
| `min` | PostgreSQL, BatleHub | admin token | in process | local volume |
| `base` | + Dex | OIDC (Dex) and admin token | in process | local volume |
| `max` | + Valkey, RustFS | OIDC (Dex) and admin token | Valkey | S3 (RustFS) |

Each config proxies one `npm` registry to `registry.npmjs.org`; add the
registries you need from the [registry pages](/registries/) and
[Configuration](/guide/configuration). Secrets are never written in the files:
every one is an environment variable, and `docker compose` refuses to start
while one is missing.

```sh
git clone https://github.com/batlehub/batlehub
cd batlehub/deploy/compose/min      # or base, or max
```

## `min` — the server and its database

```sh
BATLEHUB_ADMIN_TOKEN=$(openssl rand -hex 32) docker compose up -d
```

The console is on `http://localhost:8080`, npm on
`http://localhost:8080/proxy/npm/`. The admin token is the only credential,
for the API and the [CLI](/use/cli).

## `base` — with an OIDC login

```sh
export BATLEHUB_ADMIN_TOKEN=$(openssl rand -hex 32)
export OIDC_CLIENT_SECRET=$(openssl rand -hex 32)
export DEX_ADMIN_PASSWORD_HASH=$(docker run --rm httpd:2.4-alpine htpasswd -nbBC 10 '' 'your-password' | tr -d ':\n')
docker compose up -d
```

**Sign in** on the console redirects to Dex, where `admin@example.com` logs in
with the password hashed above and lands as an administrator. `dex.yaml` holds
the user and the client; the user's hash and the client secret are read from
Dex's environment, so no bcrypt hash is committed. For more than one person,
replace the static user with a real [Dex connector](https://dexidp.io/docs/connectors/)
(LDAP, GitHub, an upstream OIDC) and map its claims in `[auth.role_mappings]`.

BatleHub runs in **Dex's network namespace** (`network_mode: service:dex`),
which is why port `8080` is published on the `dex` service. The OIDC issuer has
to be one URL that both the browser and the server reach, and BatleHub accepts
a plain-HTTP issuer only on loopback: sharing the namespace makes
`http://localhost:5556/dex` true for both. Behind a TLS reverse proxy, give Dex
an `https://` issuer instead and the two services can be separated.

## `max` — shared cache and S3 storage

```sh
export BATLEHUB_ADMIN_TOKEN=$(openssl rand -hex 32)
export OIDC_CLIENT_SECRET=$(openssl rand -hex 32)
export S3_SECRET_KEY=$(openssl rand -hex 32)
export DEX_ADMIN_PASSWORD_HASH=$(docker run --rm httpd:2.4-alpine htpasswd -nbBC 10 '' 'your-password' | tr -d ':\n')
docker compose up -d
```

`base`, plus [Valkey](https://valkey.io) as the `[cache]` backend (it speaks
the Redis protocol) and [RustFS](https://rustfs.com) as S3 `[storage]`. A
one-shot `s3-init` container creates the `batlehub` bucket before the server
starts, since BatleHub does not create it. These are the two pieces a second
replica needs — see [High availability](/guide/high-availability) — and
RustFS's console is on `http://localhost:9001`.

## Checking it

```sh
curl http://localhost:8080/healthz
docker compose logs -f batlehub
```

The image is distroless, with no shell to run a container healthcheck in;
`/healthz` answers once the server is up.

## The compose files at the repository root

`docker-compose.yml` and `docker-compose.s3.yml` are the **development**
stacks: a database, Authentik, RustFS and Jaeger for running a server built
from source (`task run:s3`). They do not run the released image; see
[Contributing](/contributing/contributing).

---

Every method needs a **PostgreSQL 14+** database, and ends the same way:
[First-time setup](/guide/installation#first-time-setup).
