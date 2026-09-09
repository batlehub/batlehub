---
sourcePath: registries/jetbrains.md
sourceHash: ac50d56f089a698d
---

# IDE JetBrains

Met en cache les archives d'installation des IDE JetBrains. Le premier
téléchargement est diffusé depuis `download.jetbrains.com` et mis en cache ; les
téléchargements suivants du même fichier sont servis localement — idéal pour les
builds de CI et de conteneurs, et pour les réseaux hors ligne. Proxy seul : il
n'y a pas de modèle de publication privée pour les archives d'IDE.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `jetbrains` |
| **Amont par défaut** | `download.jetbrains.com` |
| **Modes** | proxy seul |
| **Adressage** | par chemin |
| **Publication privée** | ❌ proxy seul |
| **Coupure réseau** | pas d'index composé hors ligne ; un fichier détenu est servi par chemin |

## Mise en place du proxy

Le chemin qui suit `/jetbrains/` correspond un pour un à
`download.jetbrains.com/<path>`. Remplacez `<registry>` par le nom de registre
que vous avez configuré :

```bash
REG="https://batlehub.example.com/proxy/<registry>/jetbrains"

# download.jetbrains.com/idea/idea-2026.1.3.tar.gz
#   → $REG/idea/idea-2026.1.3.tar.gz
curl -fL -o idea.tar.gz $REG/idea/idea-2026.1.3.tar.gz
```

Employez le chemin **canonique** `download.jetbrains.com`. Cet hôte redirige en
302 vers un CDN (`download-cdn.jetbrains.com`) ; BatleHub suit la redirection
automatiquement et met en cache les octets finaux sous le chemin que vous avez
demandé — vous ne mettez jamais l'hôte du CDN dans l'URL. Pour faire proxy de
l'hôte du CDN directement, mettez plutôt
`upstreams = ["https://download-cdn.jetbrains.com"]`.

Employez les **vrais** noms d'archive : `idea-<ver>` pour l'installateur unifié
(2025.3 et plus) ; les anciens noms `ideaIU-<ver>` (Ultimate) et `ideaIC-<ver>`
(Community) n'existent que pour les versions ≤ 2025.2. Un nom erroné renvoie le
404 de l'amont.

La CLI `batlehub download` récupère un fichier à travers le proxy (et le met en
cache au passage) :

```bash
# relatif au registre (exige -r) ; écrit ./idea-2026.1.3.tar.gz
batlehub -r <registry> download jetbrains/idea/idea-2026.1.3.tar.gz
```

## Authentification

Ajoutez `-H "Authorization: Bearer $BATLEHUB_TOKEN"` quand le registre exige une
authentification.

## Notes

- Les archives d'IDE sont volumineuses (environ 1 à 1,7 Go). Le proxy met tout
  l'artefact en mémoire tampon avant de le mettre en cache et refuse tout ce qui
  dépasse `limits.max_artifact_size_bytes` (500 Mio par défaut) : relevez donc
  cette limite — par exemple `2147483648` pour 2 Gio — sans quoi les
  téléchargements échoueront.
- Les registres adressés par chemin se préchauffent sur des **chemins** (il n'y a
  pas de modèle de version). Listez-les sous `[registries.cache] warm_paths` pour
  les récupérer au démarrage, ou préchauffez à la demande avec
  `batlehub admin cache warm <registry> --paths "idea/idea-2026.1.3.tar.gz"`.
- Pour l'écosystème des **plugins** JetBrains (plugins.jetbrains.com), utilisez
  le type dédié `jetbrains-marketplace` — ne pointez pas celui-ci vers cet hôte.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
