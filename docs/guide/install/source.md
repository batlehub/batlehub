# Binary from source

For changing the code, or building for a platform no release covers.

**Prerequisites:** Rust 1.87+, Node 24+, PostgreSQL

**1. Build the backend:**

```sh
cargo build --release -p batlehub-server
```

**2. Build the frontend SPA (optional — embeds the UI into the server):**

```sh
cd ui
pnpm install --frozen-lockfile
pnpm run build
cd ..
```

**3. Generate the OpenAPI spec and TypeScript client (required if building the UI):**

```sh
cargo run -p batlehub-server -- --config config.example.toml dump-spec > ui/openapi.json
cd ui && pnpm run generate && pnpm run build && cd ..
```

**4. Create a config file and run:**

```sh
cp config.example.toml config.toml
./target/release/batlehub --config config.toml
```

## Task shortcuts

If you have [Task](https://taskfile.dev) installed:

```sh
task compose:db    # start only PostgreSQL
task run           # cargo run with example config
task ui:dev        # Vite dev server, proxies /api and /proxy to :8080
task dev           # backend + frontend together
task test          # cargo test --workspace
```

---

Every method needs a **PostgreSQL 14+** database, and ends the same way:
[First-time setup](/guide/installation#first-time-setup).
