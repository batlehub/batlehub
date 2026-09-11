---
sourcePath: registries/pypi.md
sourceHash: e8325e5fb7087441
---

# PyPI

Fait proxy et cache de PyPI à travers BatleHub pour pip, uv, Poetry et les
autres gestionnaires de paquets Python, ou héberge des paquets privés. BatleHub
sert l'index Simple de la [PEP 503](https://peps.python.org/pep-0503/) et met en
cache les wheels et les distributions source après le premier téléchargement.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `pypi` |
| **Amont par défaut** | `pypi.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `twine upload` |
| **Coupure réseau** | hors ligne, la page simple (JSON et HTML) est composée à partir des fichiers détenus ; `pip install` s'y résout |

## Mise en place du proxy

Faites pointer pip vers l'index Simple (`~/.pip/pip.conf` sous Linux et macOS,
`%APPDATA%\pip\pip.ini` sous Windows) :

```ini
[global]
index-url = https://batlehub.example.com/proxy/<registry>/simple/
```

Pour **uv**, ajoutez l'index à `pyproject.toml` :

```toml
[[tool.uv.index]]
name = "batlehub"
url = "https://batlehub.example.com/proxy/<registry>/simple/"
default = true
```

Les trois clients (pip, uv, Poetry) lisent le même index `simple/`. BatleHub
réécrit les liens de téléchargement à l'intérieur de l'index, de sorte que les
wheels et les sdists sont récupérés — et mis en cache — par le proxy plutôt que
directement depuis `files.pythonhosted.org`.

L'amont n'a pas à être pypi.org. Un index qui ne parle que le HTML de la
[PEP 503](https://peps.python.org/pep-0503/) — un miroir statique, devpi,
Nexus — convient comme entrée `upstreams` : une page demandée par le client en
JSON [PEP 691](https://peps.python.org/pep-0691/) est servie telle que l'amont
l'a répondue, en HTML (pip liste le HTML dans son propre `Accept` pour ce cas),
et un fichier est localisé sur la page simple quand l'amont n'a pas d'API
`/pypi/{name}/{version}/json`, les liens relatifs étant résolus par rapport à
la page. Les deux chemins sont exercés par `tests/heavy/backends.sh` contre un
répertoire servi.

## Publication (local / hybrid) {#publishing-local-hybrid}

Le registre doit être en mode `local` ou `hybrid`. Construisez, puis envoyez avec
`twine` sur l'endpoint `legacy/` (le point d'envoi) — le nom de fichier, le nom
et la version sont dérivés automatiquement des métadonnées de la wheel ou de la
sdist :

```bash
python -m build

twine upload \
  --repository-url https://batlehub.example.com/proxy/<registry>/legacy/ \
  --username __token__ \
  --password $BATLEHUB_TOKEN \
  dist/*
```

Ou bien configurez `~/.pypirc` avec une entrée
`repository = https://batlehub.example.com/proxy/<registry>/legacy/` et lancez
`twine upload --repository batlehub dist/*`.

## Versions bloquées

Une version bloquée disparaît de l'index simple dans ses **deux**
représentations — le HTML de la PEP 503 et le JSON de la PEP 691 — et tous les
fichiers de cette version disparaissent avec elle, la wheel comme la sdist. Le
résumé `versions` de la PEP 700 est filtré en même temps que la liste des
fichiers, pour que les deux ne puissent pas diverger.

L'index liste des fichiers plutôt que des versions : la version est donc
retrouvée à partir du nom de chaque distribution. Un nom de fichier que BatleHub
ne reconnaît pas est **conservé** — sur-lister un fichier est la direction sûre,
là où une analyse ratée qui le retirerait masquerait tout l'ensemble de fichiers
d'un paquet. Les versions sont comparées selon la PEP 440, de sorte qu'un blocage
enregistré comme `1.0` masque une wheel listée en `1.0.0`.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Twine envoie le token comme mot de passe, avec le nom d'utilisateur littéral
`__token__`. Pour les installations, pip, uv et Poetry lisent automatiquement les
identifiants dans `~/.netrc` :

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

Gardez le token hors de l'URL de l'index : un token embarqué dans `index-url` se
retrouve dans `pip.conf`, dans les logs de build et dans la sortie de
`pip --verbose` ou de `pip config list`. Servez-vous de `~/.netrc` (ci-dessus) ou
d'un [assistant d'identifiants pip](https://pip.pypa.io/en/stable/topics/authentication/).

## Notes

Après publication, le paquet apparaît immédiatement dans l'index Simple :

```bash
curl -s "https://batlehub.example.com/proxy/<registry>/simple/my-package/" \
  -H "Authorization: Bearer $BATLEHUB_TOKEN"
```

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
