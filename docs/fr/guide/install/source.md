---
sourcePath: guide/install/source.md
sourceHash: 8d9852aceeee33d2
---

# Binaire depuis les sources

Pour modifier le code, ou construire pour une plateforme qu'aucune release ne
couvre.

**Prérequis :** Rust 1.87+, Node 24+, PostgreSQL

**1. Compilez le backend :**

```sh
cargo build --release -p batlehub-server
```

**2. Compilez la SPA du frontend (facultatif — embarque la console dans le
serveur) :**

```sh
cd ui
pnpm install --frozen-lockfile
pnpm run build
cd ..
```

**3. Générez la spécification OpenAPI et le client TypeScript (nécessaire si vous
compilez la console) :**

```sh
cargo run -p batlehub-server -- --config config.example.toml dump-spec > ui/openapi.json
cd ui && pnpm run generate && pnpm run build && cd ..
```

**4. Créez un fichier de configuration et lancez :**

```sh
cp config.example.toml config.toml
./target/release/batlehub --config config.toml
```

## Raccourcis Task

Si [Task](https://taskfile.dev) est installé :

```sh
task compose:db    # démarrer uniquement PostgreSQL
task run           # cargo run avec la configuration d'exemple
task ui:dev        # serveur de développement Vite, proxy de /api et /proxy vers :8080
task dev           # backend et frontend ensemble
task test          # cargo test --workspace
```

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
