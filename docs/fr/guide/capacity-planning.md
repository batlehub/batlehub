---
sourcePath: guide/capacity-planning.md
sourceHash: 0898b9866ff89b27
---

# Dimensionnement

Cette page donne des repères pour dimensionner le disque, la mémoire, le
processeur et la base de données selon le type de registre et l'usage attendu.

## Dimensionner le stockage des artefacts

La taille d'un artefact varie énormément d'un écosystème à l'autre. Servez-vous
de ces ordres de grandeur pour estimer `max_artifact_size_bytes` et le stockage
total nécessaire :

| Type de registre | Taille d'artefact courante | `max_artifact_size_bytes` recommandé |
|---------------|----------------------|--------------------------------------|
| npm | 50 Ko – 5 Mo | 50 Mio |
| Cargo | 100 Ko – 10 Mo | 50 Mio |
| PyPI (wheel) | 1 Mo – 100 Mo | 200 Mio |
| Maven (JAR) | 1 Mo – 200 Mo | 256 Mio |
| NuGet | 100 Ko – 50 Mo | 100 Mio |
| Conda | 50 Mo – 500 Mo | 512 Mio |
| Docker (couche) | 1 Mo – 2 Go | 4 Gio |
| IDE JetBrains | 500 Mo – 2 Go | 4 Gio |
| Debian / RPM | 100 Ko – 500 Mo | 512 Mio |
| Provider Terraform | 5 Mo – 100 Mo | 256 Mio |

Pour un déploiement **en proxy seul** : dimensionnez le stockage à
`(paquets uniques en cache) × (taille moyenne d'artefact) × (versions par
paquet)`. Un point de départ raisonnable pour une petite équipe est 100 Go.

Pour un déploiement **en mode local** : ajoutez la taille totale de tous les
artefacts que vous comptez publier, et gardez une sauvegarde à part (voir
[reprise après sinistre](/fr/operations/disaster-recovery)).

## Mémoire

La consommation mémoire propre à BatleHub est faible (moins de 200 Mo à vide).
L'essentiel de la mémoire est consommé par :

- le cache de métadonnées (TTL configurable ; `[cache] type = "redis"` le
  déporte) ;
- la mise en tampon par requête, pour diffuser les gros artefacts.

Minimum recommandé : **512 Mo de RAM** pour un petit déploiement. Pour un fort
parallélisme (plus de 100 téléchargements simultanés), prévoyez **2 Go** afin
d'absorber les tampons en cours.

## Processeur

BatleHub est limité par les entrées-sorties, pas par le processeur. Même une
instance à 2 cœurs encaisse plusieurs centaines de requêtes par seconde sur des
artefacts en cache. Les pics de CPU surviennent lors de :

- l'extraction de SBOM (décompression ZIP et sérialisation JSON) ;
- la terminaison TLS (à déporter sur un reverse proxy ou un répartiteur de charge
  en cas de très fort trafic) ;
- l'analyse de vulnérabilités (périodique, hors du chemin chaud).

Recommandé : **2 à 4 vCPU** pour la plupart des déploiements.

## Ce que coûtent les gates de sécurité par requête {#gate-cost}

Mesuré avec `perf/scripts/gate_levels.sh` (2026-11-09, build release, upstream simulé, 50 req/s par forme de requête) : la latence médiane de huit requêtes de téléchargement et de métadonnées, à cinq niveaux de contrôle. Chaque niveau ajoute des règles à tous les registres, toutes réglées pour *laisser passer* le trafic : le chiffre est le coût de l'évaluation des gates sur une requête acceptée.

| Niveau | Règles | Latence médiane ajoutée | Premier téléchargement d'une nouvelle version |
| --- | --- | --- | --- |
| aucun | — | — (≈ 5 ms par téléchargement, 1,3 ms par packument) | servi tout de suite |
| min | `release_age_gate` | +0,0 à +0,2 ms | servi tout de suite (s'il est assez ancien) |
| moyen | + `cve_gate` | +0,0 à +0,4 ms | servi tout de suite |
| élevé | `[registries.security]` (osv) + `cve_gate` | +0,3 à +0,65 ms | **retenu jusqu'à la réponse du scan : ~9 s** |
| max | élevé + `license_gate`, `version_gate`, `require_signed_release`, SBOM | +0,2 à +0,75 ms | **retenu jusqu'à la réponse du scan : ~10 s** |

D'une mesure à l'autre, un même niveau varie de ±0,15 ms au plus. Le coût par requête de chaque niveau reste sous la milliseconde. Ce qu'un niveau coûte vraiment, c'est la **retenue** : un profil `[registries.security]` ne sert une version qu'une fois que ses scanners requis ont répondu (ici, OSV par le réseau plus le tour de 2 secondes du worker), et une `release_age_gate` ou `security.min_age_secs` retient une version plus jeune que son âge, ce qui se compte en heures ou en jours, par construction. Prévoyez la retenue, pas le processeur.

## Base de données (PostgreSQL)

- **Connexions :** `max_connections = 20` par instance par défaut ; voir
  [dimensionner `max_connections`](./configuration#dimensionner-max-connections)
  avant de monter plus haut. La base
  elle-même doit autoriser au moins
  `max_connections × (nombre d'instances) + 10` de marge.
- **Disque :** la table `access_events` grossit d'environ 1 Ko par événement. À
  1 000 événements par jour, cela fait environ 365 Mo par an. Planifiez des
  purges régulières :
  `batlehub-cli audit purge --before 2024-01-01T00:00:00Z`.
- **IOPS :** les lectures de métadonnées et les écritures de `record_access` sont
  les chemins les plus chauds. Un SSD à 3 000 IOPS suffit à la plupart des
  équipes.

## `max_artifact_size_bytes` recommandé selon le déploiement

Réglez-le par registre, et non globalement, pour éviter qu'un gros
téléchargement d'IDE JetBrains ne bloque des installations npm plus petites qui
partagent un pool de connexions :

```toml
[[registries]]
name = "npm-proxy"
type = "npm"
[registries.local_registry]
max_artifact_size_bytes = 52428800   # 50 Mio

[[registries]]
name = "jetbrains-proxy"
type = "jetbrains"
[registries.local_registry]
max_artifact_size_bytes = 4294967296  # 4 Gio
```
