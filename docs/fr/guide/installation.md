---
sourcePath: guide/installation.md
sourceHash: ca5acd90b7473453
---

# Installation

BatleHub est un binaire unique adossé à PostgreSQL. Ce qui distingue les
méthodes ci-dessous, c'est seulement la façon dont ce processus démarre et d'où
vient son fichier de configuration — le serveur, la console et les registres
sont les mêmes dans tous les cas.

**Sans raison particulière de préférer autre chose, prenez
[Docker Compose](/fr/guide/install/compose).** Il démarre le serveur et sa base
ensemble, n'exige rien d'installé sinon un moteur de conteneurs, et c'est le
chemin le plus court entre rien et un registre vers lequel pointer un
gestionnaire de paquets. Les autres valent quand votre environnement a déjà
tranché pour vous.

| Méthode | Quand la prendre |
|---|---|
| [Docker Compose](/fr/guide/install/compose) | évaluation, développement, une seule machine — le serveur et PostgreSQL ensemble |
| [Image de conteneur](/fr/guide/install/container) | vous avez déjà un orchestrateur et une base, et ne voulez que l'image |
| [Binaire précompilé](/fr/guide/install/binary) | vous ne faites pas tourner de conteneurs |
| [Depuis les sources](/fr/guide/install/source) | vous modifiez le code, ou visez une plateforme qu'aucune release ne couvre |
| [Chart Helm](/fr/guide/install/helm) | vous déployez sur Kubernetes — trois fichiers de valeurs complets, d'un réplica à la production |

---

## Prérequis

Toutes les méthodes d'installation exigent une base **PostgreSQL 14+**. Le
serveur crée son schéma automatiquement au premier démarrage.

---

## Première mise en route

Quelle que soit la méthode d'installation, une fois le serveur démarré :

**1. Vérifiez l'endpoint de santé :**

```sh
curl -H "Authorization: Bearer my-admin-token" \
  http://localhost:8080/api/v1/admin/health
```

**2. Ouvrez la console et le guide de mise en place :**

Rendez-vous sur `http://localhost:8080` — la page de mise en place (`/setup`)
génère les extraits de configuration client pour tous les outils enregistrés.

**3. Faites pointer un client vers le proxy :**

```sh
# npm
npm install --registry http://localhost:8080/proxy/npm/ some-package

# Go
GOPROXY=http://localhost:8080/proxy/go,direct go get golang.org/x/text@latest

# Cargo — à ajouter dans .cargo/config.toml
# [source.crates-io]
# replace-with = "batlehub"
# [source.batlehub]
# registry = "sparse+http://localhost:8080/proxy/cargo/registry/"
```
