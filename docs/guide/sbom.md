# SBOM {#overview}

BatleHub automatically generates Software Bills of Materials (SBOMs) for every artifact it caches or hosts in a local / hybrid registry. SBOMs are stored in the database and exposed through a REST API — no changes to your existing build pipeline needed.

SBOM support is driven by compliance requirements such as the EU Cyber Resilience Act and US Executive Order 14028.

---

## Supported formats {#formats}

| Format | Spec version | `?format=` value |
|--------|-------------|-----------------|
| **SPDX** | 2.3 | `spdx` (default) |
| **CycloneDX** | 1.4 | `cyclonedx` |

Both formats are generated for each artifact when SBOM is enabled. Use whichever your toolchain prefers:

- **SPDX 2.3** — ISO/IEC 5962 standard; preferred for license compliance and OpenChain-conformant workflows.
- **CycloneDX 1.4** — OWASP standard; preferred for security tooling, Grype / Trivy / OSV-Scanner integration, and vulnerability-driven policy gates.

---

## How SBOMs are generated {#generation}

For each artifact, BatleHub tries the following sources in priority order and uses the first one that succeeds:

1. **Upstream API** — fetch a pre-built SBOM from the upstream registry (GitHub dependency graph API, npm `bom.json`). Highest quality; enable with `fetch_upstream = true`.
2. **Archive extraction** — parse the dependency manifest embedded in the downloaded archive:

   | Registry | Manifest |
   |----------|---------|
   | Cargo | `Cargo.toml` |
   | npm | `package.json` |
   | Maven | `pom.xml` |
   | Go | `go.mod` |
   | PyPI | `requirements.txt`, `pyproject.toml` |

3. **Minimal generation** — if neither upstream nor archive yields a manifest, BatleHub produces a valid document from the package name, version, ecosystem PURL, and checksum. No dependency list, but parseable by all SBOM tooling.

The `source` field in the stored record (`Upstream`, `Extracted`, or `Generated`) indicates which path was taken.

---

## Configuration {#configuration}

Enable SBOM generation per registry with the `[registries.sbom]` block:

```toml
[[registries]]
type = "cargo"
name = "crates-io"

[registries.sbom]
enabled        = true
formats        = ["spdx", "cyclonedx"]   # default: both
fetch_upstream = true                    # try upstream APIs first
required       = false                   # deny publish if no manifest found
```

### Option reference

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enabled` | bool | `false` | Enable SBOM generation. Must be `true` for any SBOM functionality. |
| `formats` | list | `["spdx", "cyclonedx"]` | Which formats to generate and store. |
| `fetch_upstream` | bool | `true` | Attempt to retrieve a pre-built SBOM from the upstream before extracting or generating. |
| `required` | bool | `false` | **Local/hybrid registries only** — deny the publish request (HTTP 422) if no dependency manifest is found in the uploaded archive. |

::: tip
`required = true` is a strong supply-chain gate: it prevents publishing packages that have no declared dependencies. Use it for internal registries where you control what teams publish.
:::

---

## Per-artifact API {#per-artifact-api}

```
GET /api/v1/sbom/{registry}/{name}/{version}?format=spdx|cyclonedx
```

Requires an authenticated (non-anonymous) user. Returns the SBOM document as JSON.

**Path parameters**

| Parameter | Description |
|-----------|-------------|
| `registry` | Registry name as defined in `config.toml` |
| `name` | Package name |
| `version` | Exact version string |

**Query parameters**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `format` | `spdx` | `spdx` or `cyclonedx` |

**Example**

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

## Org-level export {#org-export}

```
GET /api/v1/sbom/export?registry=…&from=…&to=…&format=spdx|cyclonedx
```

Requires the `admin` role. Returns a single merged SBOM document covering all artifacts whose records fall within the requested time window. Packages are **deduplicated** by `name@version` across registries.

The response includes `Content-Disposition: attachment` — browsers and `curl -O -J` save the file automatically.

**Query parameters**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `format` | No (default: `spdx`) | `spdx` or `cyclonedx` |
| `registry` | No | Filter to one registry. Omit to export all. |
| `from` | No | Earliest artifact creation timestamp (ISO 8601) |
| `to` | No | Latest artifact creation timestamp (ISO 8601) |

**Example**

```sh
# Export all SBOMs from the last 30 days
FROM=$(date -u -d '30 days ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
     || date -u -v-30d +%Y-%m-%dT%H:%M:%SZ)  # macOS

curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/export?from=${FROM}&format=spdx" \
  -O -J   # saves as sbom-export-all-<timestamp>.spdx.json
```

---

## Admin UI {#admin-ui}

The admin panel at **`/admin/packages/sbom`** provides a point-and-click interface for the org-level export:

- **Registry** — optional text filter (leave empty for all)
- **From / To** — date range pickers
- **Format** — SPDX 2.3 or CycloneDX 1.4
- **Download** button — triggers the export and saves the file directly to your browser

Per-artifact SBOMs are accessible from the **[Package Explorer](/use/package-explorer)** (`/explore`). Open any package's detail page, find a version row, and click the **SPDX** or **CDX** button. If no SBOM was generated for that version the button is replaced with a "No SBOM" label.

---

## PURL mapping {#purl}

Each package in the generated SBOM includes a [Package URL](https://github.com/package-url/purl-spec) for interoperability with vulnerability scanners (Grype, Trivy, OSV-Scanner):

| Registry type | PURL example |
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
| everything else | `pkg:generic/{name}@{version}` |

---

## Worked examples {#examples}

### Compliance audit — quarterly SPDX export

```sh
curl -H "Authorization: Bearer $ADMIN_TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/export?from=2025-01-01T00:00:00Z&to=2025-03-31T23:59:59Z&format=spdx" \
  -O -J
# → sbom-export-all-20250401120000.spdx.json
```

### Vulnerability scan with Grype

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/npm/express/4.18.0?format=cyclonedx" \
  -o express-4.18.0.cyclonedx.json

grype sbom:express-4.18.0.cyclonedx.json
```

### Vulnerability scan with Trivy

```sh
curl -H "Authorization: Bearer $TOKEN" \
  "https://batlehub.example.com/api/v1/sbom/crates-io/tokio/1.36.0?format=cyclonedx" \
  -o tokio.cyclonedx.json

trivy sbom tokio.cyclonedx.json
```

### Private registry — enforce SBOM at publish time

```toml
[[registries]]
type = "npm"
name = "internal-npm"
mode = "local"

[registries.sbom]
enabled  = true
required = true   # deny publish if package.json missing
```

Publishing a tarball without a `package.json` returns:

```
HTTP 422 Unprocessable Entity
{"error": "no dependency manifest found in archive"}
```

### CI pipeline — attach SBOM to every release

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
