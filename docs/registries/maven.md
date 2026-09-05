# Maven

Proxy a Maven Central-compatible repository — POMs, JARs, source/Javadoc JARs, SHA-1/MD5 checksums, and `maven-metadata.xml` — or host private artifacts. Works with Maven, Gradle, and any tool that speaks the Maven repository protocol.

## At a glance

| | |
|---|---|
| **Config type** | `maven` |
| **Default upstream** | `repo1.maven.org` |
| **Modes** | proxy · local · hybrid |
| **Addressing** | per-package |
| **Private publish** | ✅ `mvn deploy` |
| **Air gap** | offline,`maven-metadata.xml` is composed from the held files; Maven still asks for `.sha1`/`.md5` beside every file, which the bundle can carry for real files |

## Proxy setup

Add a mirror to `~/.m2/settings.xml`:

```xml
<settings>
  <mirrors>
    <mirror>
      <id>batlehub</id>
      <mirrorOf>central</mirrorOf>
      <url>https://batlehub.example.com/proxy/<registry>/maven2/</url>
    </mirror>
  </mirrors>
</settings>
```

For Gradle, add the repository in `settings.gradle.kts`:

```kotlin
dependencyResolutionManagement {
    repositories {
        maven { url = uri("https://batlehub.example.com/proxy/<registry>/maven2/") }
    }
}
```

## Publishing (local / hybrid)

Maven artifacts are published by uploading individual files (`PUT`) using the Maven 2 repository layout. When the `.pom` file is uploaded, BatleHub parses it and creates a version record — subsequent GET requests will include it in `maven-metadata.xml`.

### Server configuration

```toml
[[registries]]
type = "maven"
name = "internal-maven"
mode = "local"          # or "hybrid" to fall back to repo1.maven.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

For hybrid mode add `upstreams = ["https://repo1.maven.org/maven2"]`.

### Client setup — Maven (`~/.m2/settings.xml`)

```xml
<settings>
  <servers>
    <server>
      <id>internal-maven</id>
      <username>token</username>
      <password>YOUR_TOKEN</password>
    </server>
  </servers>

  <!-- Optional: use as a download mirror for all artifacts -->
  <mirrors>
    <mirror>
      <id>internal-maven</id>
      <mirrorOf>*</mirrorOf>
      <url>https://batlehub.example.com/proxy/internal-maven/maven2</url>
    </mirror>
  </mirrors>
</settings>
```

### Client setup — Gradle (`build.gradle.kts`)

```kotlin
repositories {
    maven {
        name = "internalMaven"
        url  = uri("https://batlehub.example.com/proxy/internal-maven/maven2")
        credentials {
            username = "token"
            password = System.getenv("BATLEHUB_TOKEN") ?: ""
        }
    }
}
```

### Publish — Maven

Add to your project's `pom.xml`:

```xml
<distributionManagement>
  <repository>
    <id>internal-maven</id>
    <url>https://batlehub.example.com/proxy/internal-maven/maven2</url>
  </repository>
  <snapshotRepository>
    <id>internal-maven</id>
    <url>https://batlehub.example.com/proxy/internal-maven/maven2</url>
  </snapshotRepository>
</distributionManagement>
```

Then deploy:

```sh
mvn deploy
# or, overriding the repository URL without editing pom.xml:
mvn deploy -DaltDeploymentRepository=internal-maven::default::https://batlehub.example.com/proxy/internal-maven/maven2
```

Maven uploads the `.jar`, `-sources.jar`, `.pom`, and checksum files individually. BatleHub accepts all of them and records the version when the `.pom` arrives.

### Publish — Gradle

Add to `build.gradle.kts`:

```kotlin
publishing {
    repositories {
        maven {
            name = "internalMaven"
            url  = uri("https://batlehub.example.com/proxy/internal-maven/maven2")
            credentials {
                username = "token"
                password = System.getenv("BATLEHUB_TOKEN") ?: ""
            }
        }
    }
}
```

Then publish:

```sh
./gradlew publish
```

### Verify

```sh
# Download maven-metadata.xml (should list the published version)
curl -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-maven/maven2/com/example/mylib/maven-metadata.xml"

# Resolve the artifact (Maven)
mvn dependency:get -Dartifact=com.example:mylib:1.0.0
```

### Endpoint reference

<!-- BEGIN endpoints: proxy/maven -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/maven2/{path}` | Proxy or serve a Maven repository request. |
| `PUT` | `/proxy/{registry}/maven2/{path}` | Upload a Maven artifact to the local registry. |
<!-- END endpoints -->

`{group}` uses path segments: `com/example` maps to groupId `com.example`.

---

## Blocked versions

`maven-metadata.xml` is filtered and both of its pointers are repaired:
`<versions>` loses the blocked entry, `<latest>` is recomputed to the newest
surviving version (snapshots included), and `<release>` to the newest surviving
version that is **not** qualified — the distinction the two elements exist to
draw. A `LATEST` or `RELEASE` resolution therefore lands on a version the
operator allows.

A document BatleHub cannot parse is served unchanged and logged, rather than
half-rewritten: Maven rejects malformed metadata outright, where an unfiltered
document merely over-lists.

The upstream document is cached for the registry's `metadata_ttl`; blocks are
applied on top of the cached copy on every request, so blocking a version takes
effect immediately rather than when the cache expires.

See [blocking a package version](/guide/admin-policies#block-a-package-version) for the two halves of a block, and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered) for the full table.

## Authentication

Credentials live in a `<server>` block in `~/.m2/settings.xml`, keyed by an `<id>` that matches the repository id in `<distributionManagement>`:

```xml
<settings>
  <servers>
    <server>
      <id>internal-maven</id>
      <username>token</username>
      <password>${env.BATLEHUB_TOKEN}</password>
    </server>
  </servers>
</settings>
```

## Notes

Verify a published artifact by fetching its metadata:

```bash
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/maven2/com/example/mylib/maven-metadata.xml"
```

## See also

- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
