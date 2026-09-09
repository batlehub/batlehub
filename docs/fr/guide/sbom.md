---
sourcePath: guide/sbom.md
sourceHash: c4d131494919d380
---

# SBOM {#overview}

BatleHub génère automatiquement une nomenclature logicielle (SBOM) pour chaque
artefact qu'il met en cache ou héberge dans un registre local ou hybrid. Les SBOM
sont stockés en base et exposés par une API REST — sans rien changer à votre
chaîne de build existante.

La prise en charge des SBOM répond à des exigences de conformité comme le
règlement européen sur la cyberrésilience et le décret américain 14028.

---

## Formats pris en charge {#formats}

| Format | Version de spécification | Valeur de `?format=` |
|--------|-------------|-----------------|
| **SPDX** | 2.3 | `spdx` (défaut) |
| **CycloneDX** | 1.4 | `cyclonedx` |

Les deux formats sont générés pour chaque artefact quand le SBOM est activé.
Prenez celui que préfère votre outillage :

- **SPDX 2.3** — norme ISO/IEC 5962 ; à privilégier pour la conformité des
  licences et les flux conformes à OpenChain.
- **CycloneDX 1.4** — norme OWASP ; à privilégier pour l'outillage de sécurité,
  l'intégration avec Grype, Trivy ou OSV-Scanner, et les garde-fous de politique
  fondés sur les vulnérabilités.

---

## Comment un SBOM est généré {#generation}

Pour chaque artefact, BatleHub essaie les sources suivantes dans cet ordre de
priorité et retient la première qui aboutit :

1. **L'API amont** — récupérer un SBOM déjà construit auprès du registre amont
   (API de graphe de dépendances GitHub, `bom.json` npm). C'est la meilleure
   qualité ; à activer avec `fetch_upstream = true`.
2. **L'extraction depuis l'archive** — analyser le manifeste de dépendances
   embarqué dans l'archive téléchargée :

   | Registre | Manifeste |
   |----------|---------|
   | Cargo | `Cargo.toml` |
   | npm | `package.json` |
   | Maven | `pom.xml` |
   | Go | `go.mod` |
   | PyPI | `requirements.txt`, `pyproject.toml` |

3. **La génération minimale** — si ni l'amont ni l'archive ne fournissent de
   manifeste, BatleHub produit un document valide à partir du nom du paquet, de
   sa version, de son PURL d'écosystème et de sa somme de contrôle. Pas de liste
   de dépendances, mais un document que tout l'outillage SBOM sait lire.

Le champ `source` de l'enregistrement stocké (`Upstream`, `Extracted` ou
`Generated`) indique le chemin emprunté.

---

## Configuration {#configuration}

Activez la génération de SBOM registre par registre, avec le bloc
`[registries.sbom]` :

```toml
[[registries]]
type = "cargo"
name = "crates-io"

[registries.sbom]
enabled        = true
formats        = ["spdx", "cyclonedx"]   # défaut : les deux
fetch_upstream = true                    # essayer d'abord les API amont
required       = false                   # refuser la publication si aucun manifeste n'est trouvé
```

### Référence des options

| Option | Type | Défaut | Description |
|--------|------|---------|-------------|
| `enabled` | booléen | `false` | Active la génération de SBOM. Doit valoir `true` pour toute fonctionnalité SBOM. |
| `formats` | liste | `["spdx", "cyclonedx"]` | Quels formats générer et stocker. |
| `fetch_upstream` | booléen | `true` | Tenter de récupérer un SBOM déjà construit en amont avant d'extraire ou de générer. |
| `required` | booléen | `false` | **Registres local et hybrid uniquement** — refuser la requête de publication (HTTP 422) si aucun manifeste de dépendances n'est trouvé dans l'archive envoyée. |

::: tip
`required = true` est un garde-fou fort pour la chaîne d'approvisionnement : il
empêche de publier des paquets sans dépendances déclarées. Réservez-le aux
registres internes, où vous maîtrisez ce que les équipes publient.
:::

---

## API par artefact {#per-artifact-api}

```
GET /api/v1/sbom/{registry}/{name}/{version}?format=spdx|cyclonedx
```

Exige un utilisateur authentifié (non anonyme). Renvoie le document SBOM en JSON.

**Paramètres de chemin**

| Paramètre | Description |
|-----------|-------------|
| `registry` | Le nom du registre tel que déclaré dans `config.toml` |
| `name` | Le nom du paquet |
| `version` | La chaîne de version exacte |

**Paramètres de requête**

| Paramètre | Défaut | Description |
|-----------|---------|-------------|
| `format` | `spdx` | `spdx` ou `cyclonedx` |

**Exemple**

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/crates-io/serde/1.0.0?format=spdx" \
  | jq .spdxVersion
# "SPDX-2.3"

curl -H "Authorization: Bearer $TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/npm/lodash/4.17.21?format=cyclonedx" \
  | jq .bomFormat
# "CycloneDX"
```

---

## Export au niveau de l'organisation {#org-export}

```
GET /api/v1/sbom/export?registry=…&from=…&to=…&format=spdx|cyclonedx
```

Exige le rôle `admin`. Renvoie un unique document SBOM fusionné couvrant tous les
artefacts dont l'enregistrement tombe dans la fenêtre demandée. Les paquets sont
**dédupliqués** par `name@version`, tous registres confondus.

La réponse porte `Content-Disposition: attachment` — les navigateurs et
`curl -O -J` enregistrent donc le fichier automatiquement.

**Paramètres de requête**

| Paramètre | Obligatoire | Description |
|-----------|----------|-------------|
| `format` | Non (défaut : `spdx`) | `spdx` ou `cyclonedx` |
| `registry` | Non | Se limiter à un registre. À omettre pour tout exporter. |
| `from` | Non | Horodatage de création d'artefact le plus ancien (ISO 8601) |
| `to` | Non | Horodatage de création d'artefact le plus récent (ISO 8601) |

**Exemple**

```sh
# Exporter tous les SBOM des 30 derniers jours
FROM=$(date -u -d '30 days ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
     || date -u -v-30d +%Y-%m-%dT%H:%M:%SZ)  # macOS

curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/export?from=${FROM}&format=spdx" \
  -O -J   # enregistré sous sbom-export-all-<timestamp>.spdx.json
```

---

## La console d'administration {#admin-ui}

Le panneau d'administration, sur **`/admin/sbom`**, offre une interface
graphique pour l'export au niveau de l'organisation :

- **Registre** — filtre textuel facultatif (laisser vide pour tout) ;
- **De / À** — sélecteurs de plage de dates ;
- **Format** — SPDX 2.3 ou CycloneDX 1.4 ;
- le bouton **Télécharger** — déclenche l'export et enregistre le fichier
  directement dans votre navigateur.

Les SBOM par artefact sont accessibles depuis l'
**[explorateur de paquets](/fr/use/package-explorer)** (`/explore`). Ouvrez la
page de détail d'un paquet, trouvez la ligne d'une version, et cliquez sur le
bouton **SPDX** ou **CDX**. Si aucun SBOM n'a été généré pour cette version, le
bouton est remplacé par la mention « No SBOM ».

---

## Correspondance des PURL {#purl}

Chaque paquet du SBOM généré porte un
[Package URL](https://github.com/package-url/purl-spec), pour l'interopérabilité
avec les scanners de vulnérabilités (Grype, Trivy, OSV-Scanner) :

| Type de registre | Exemple de PURL |
|---------------|-------------|
| `cargo` | `pkg:cargo/serde@1.0.0` |
| `npm` | `pkg:npm/lodash@4.17.21` |
| `maven` | `pkg:maven/org.springframework/spring-core@6.0.0` |
| `pypi` | `pkg:pypi/requests@2.31.0` |
| `rubygems` | `pkg:gem/rails@7.1.0` |
| `goproxy` | `pkg:golang/github.com/gin-gonic/gin@v1.9.1` |
| `terraform` | `pkg:terraform/hashicorp/aws@5.0.0` |
| `composer` | `pkg:composer/symfony/console@7.0.0` |
| `conda` | `pkg:conda/numpy@1.26.0` |
| tout le reste | `pkg:generic/{name}@{version}` |

---

## Exemples commentés {#examples}

### Audit de conformité — export SPDX trimestriel

```sh
curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/export?from=2025-01-01T00:00:00Z&to=2025-03-31T23:59:59Z&format=spdx" \
  -O -J
# → sbom-export-all-20250401120000.spdx.json
```

### Analyse de vulnérabilités avec Grype

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/npm/express/4.18.0?format=cyclonedx" \
  -o express-4.18.0.cyclonedx.json

grype sbom:express-4.18.0.cyclonedx.json
```

### Analyse de vulnérabilités avec Trivy

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/crates-io/tokio/1.36.0?format=cyclonedx" \
  -o tokio.cyclonedx.json

trivy sbom tokio.cyclonedx.json
```

### Registre privé — exiger un SBOM à la publication

```toml
[[registries]]
type = "npm"
name = "internal-npm"
mode = "local"

[registries.sbom]
enabled  = true
required = true   # refuse la publication si package.json est absent
```

Publier un tarball sans `package.json` renvoie :

```
HTTP 422 Unprocessable Entity
{"error": "no dependency manifest found in archive"}
```

### Pipeline de CI — attacher un SBOM à chaque release

```yaml
# .github/workflows/release.yml
- name: Download release SBOM
  run: |
    curl -fsSL \
      -H "Authorization: Bearer ${{ secrets.BATLEHUB_TOKEN }}" \
      "${{ vars.BATLEHUB_URL }}/api/v1/sbom/export?format=cyclonedx" \
      -o release-sbom.cyclonedx.json

- name: Upload SBOM
  uses: actions/upload-artifact@v4
  with:
    name: sbom-cyclonedx
    path: release-sbom.cyclonedx.json
```
