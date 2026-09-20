# Pre-built binary

No container runtime involved: one file, a config file, and a PostgreSQL it can
reach.

A statically linked `batlehub` binary for Linux is attached to each [GitHub Release](https://github.com/batlehub/batlehub/releases). Download it, make it executable, and run:

```sh
curl -L -o batlehub https://github.com/batlehub/batlehub/releases/download/<version>/batlehub
chmod +x batlehub
./batlehub --config config.toml
```

---

Every method needs a **PostgreSQL 14+** database, and ends the same way:
[First-time setup](/guide/installation#first-time-setup).
