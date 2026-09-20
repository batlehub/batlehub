# Installation

BatleHub is a single binary backed by PostgreSQL. What differs between the
methods below is only how that process is started and where its config file
comes from — the server, the console and the registries are the same either way.

**If you have no reason to prefer otherwise, use
[Docker Compose](/guide/install/compose).** It brings up the server and its
database together, needs nothing installed but a container runtime, and is the
shortest path from nothing to a registry you can point a package manager at.
The others are for when your environment has already decided for you.

| Method | Use it when |
|---|---|
| [Docker Compose](/guide/install/compose) | evaluating, developing, or running one host — the server and PostgreSQL together |
| [Container image](/guide/install/container) | you have an orchestrator or a database already, and want only the image |
| [Pre-built binary](/guide/install/binary) | you do not run containers |
| [From source](/guide/install/source) | you are changing the code, or need a build for a platform no release covers |
| [Helm chart](/guide/install/helm) | you are deploying to Kubernetes — three complete values files, from one replica to production |

---

## Prerequisites

All installation methods require a **PostgreSQL 14+** database. The server creates its schema automatically on first start.

---

## First-time setup

Regardless of installation method, once the server is running:

**1. Verify the health endpoint:**

```sh
curl -H "Authorization: Bearer my-admin-token" \
  http://localhost:8080/api/v1/admin/health
```

**2. Open the Web UI and Setup Guide:**

Navigate to `http://localhost:8080` — the Setup Guide page (`/setup`) generates client config snippets for all registered tools.

**3. Point a client at the proxy:**

```sh
# npm
npm install --registry http://localhost:8080/proxy/npm/ some-package

# Go
GOPROXY=http://localhost:8080/proxy/go,direct go get golang.org/x/text@latest

# Cargo — add to .cargo/config.toml
# [source.crates-io]
# replace-with = "batlehub"
# [source.batlehub]
# registry = "sparse+http://localhost:8080/proxy/cargo/registry/"
```
