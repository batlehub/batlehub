---
sourcePath: guide/install/binary.md
sourceHash: 78f4cd7c2040d318
---

# Binaire précompilé

Aucun moteur de conteneurs : un fichier, un fichier de configuration, et un
PostgreSQL qu'il peut joindre.

Un binaire `batlehub` lié statiquement pour Linux est joint à chaque
[release GitHub](https://github.com/batlehub/batlehub/releases).
Téléchargez-le, rendez-le exécutable et lancez-le :

```sh
curl -L -o batlehub https://github.com/batlehub/batlehub/releases/download/<version>/batlehub
chmod +x batlehub
./batlehub --config config.toml
```

---

Toutes les méthodes exigent une base **PostgreSQL 14+**, et se terminent de la
même façon : [Première mise en route](/fr/guide/installation#premiere-mise-en-route).
