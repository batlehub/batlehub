# Docker Compose

The fastest way to a running instance, for local development or evaluation:
the server and its database come up together.

**1. Clone the repository:**

```sh
git clone https://github.com/batlehub/batlehub
cd batlehub
```

**2. Copy and edit the example config:**

```sh
cp config.example.toml config.toml
# Edit config.toml: set database URL, admin token, and at least one registry
```

**3. Start PostgreSQL and the server:**

```sh
podman compose up -d   # or docker compose up -d
```

The server listens on `http://localhost:8080`. The Swagger UI is at `http://localhost:8080/swagger-ui/`.

**4. Verify:**

```sh
curl http://localhost:8080/api/openapi.json
```

## With S3 storage (RustFS)

A separate Compose file adds a RustFS (S3-compatible) storage backend and Authentik OIDC:

```sh
podman compose -f docker-compose.s3.yml up -d postgres rustfs
# Then run the server with the S3 config:
task run:s3
```

---

Every method needs a **PostgreSQL 14+** database, and ends the same way:
[First-time setup](/guide/installation#first-time-setup).
