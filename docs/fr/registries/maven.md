---
sourcePath: registries/maven.md
sourceHash: c89e1694124d77fe
---

# Maven

Fait proxy d'un dépôt compatible Maven Central — POM, JAR, JAR de sources et de
Javadoc, sommes de contrôle SHA-1 et MD5, et `maven-metadata.xml` — ou héberge
des artefacts privés. Fonctionne avec Maven, Gradle et tout outil qui parle le
protocole de dépôt Maven.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `maven` |
| **Amont par défaut** | `repo1.maven.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `mvn deploy` |
| **Coupure réseau** | hors ligne, `maven-metadata.xml` est composé à partir des fichiers détenus ; Maven demande toujours un `.sha1` et un `.md5` à côté de chaque fichier, que le lot peut porter pour les fichiers réels |

## Mise en place du proxy

Ajoutez un miroir à `~/.m2/settings.xml` :

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

Pour Gradle, ajoutez le dépôt dans `settings.gradle.kts` :

```kotlin
dependencyResolutionManagement {
    repositories {
        maven { url = uri("https://batlehub.example.com/proxy/<registry>/maven2/") }
    }
}
```

## Publication (local / hybrid) {#publishing-local-hybrid}

Les artefacts Maven se publient en envoyant des fichiers un à un (`PUT`), selon
la disposition de dépôt Maven 2. Quand le fichier `.pom` est envoyé, BatleHub
l'analyse et crée un enregistrement de version — les requêtes GET suivantes
l'incluront dans `maven-metadata.xml`.

### Configuration du serveur

```toml
[[registries]]
type = "maven"
name = "internal-maven"
mode = "local"          # ou "hybrid" pour se rabattre sur repo1.maven.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://repo1.maven.org/maven2"]`.

### Mise en place côté client — Maven (`~/.m2/settings.xml`)

```xml
<settings>
  <servers>
    <server>
      <id>internal-maven</id>
      <username>token</username>
      <password>YOUR_TOKEN</password>
    </server>
  </servers>

  <!-- Facultatif : servir de miroir de téléchargement pour tous les artefacts -->
  <mirrors>
    <mirror>
      <id>internal-maven</id>
      <mirrorOf>*</mirrorOf>
      <url>https://batlehub.example.com/proxy/internal-maven/maven2</url>
    </mirror>
  </mirrors>
</settings>
```

### Mise en place côté client — Gradle (`build.gradle.kts`)

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

### Publier — Maven

Ajoutez au `pom.xml` de votre projet :

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

Puis déployez :

```sh
mvn deploy
# ou, en remplaçant l'URL du dépôt sans modifier pom.xml :
mvn deploy -DaltDeploymentRepository=internal-maven::default::https://batlehub.example.com/proxy/internal-maven/maven2
```

Maven envoie le `.jar`, le `-sources.jar`, le `.pom` et les fichiers de sommes de
contrôle un par un. BatleHub les accepte tous et enregistre la version à
l'arrivée du `.pom`.

### Publier — Gradle

Ajoutez à `build.gradle.kts` :

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

Puis publiez :

```sh
./gradlew publish
```

### Vérifier

```sh
# Télécharger maven-metadata.xml (il doit lister la version publiée)
curl -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-maven/maven2/com/example/mylib/maven-metadata.xml"

# Résoudre l'artefact (Maven)
mvn dependency:get -Dartifact=com.example:mylib:1.0.0
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/maven -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/maven2/{path}` | Proxy or serve a Maven repository request. |
| `PUT` | `/proxy/{registry}/maven2/{path}` | Upload a Maven artifact to the local registry. |
<!-- END endpoints -->

`{group}` s'écrit en segments de chemin : `com/example` correspond au groupId
`com.example`.

---

## Versions bloquées

`maven-metadata.xml` est filtré et ses deux pointeurs sont réparés : `<versions>`
perd l'entrée bloquée, `<latest>` est recalculé vers la version survivante la
plus récente (snapshots compris), et `<release>` vers la plus récente qui n'est
**pas** qualifiée — la distinction pour laquelle ces deux éléments existent. Une
résolution `LATEST` ou `RELEASE` tombe donc sur une version que l'opérateur
autorise.

Un document que BatleHub ne sait pas analyser est servi inchangé et journalisé,
plutôt que réécrit à moitié : Maven rejette purement et simplement des métadonnées
malformées, là où un document non filtré se contente de sur-lister.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

Les identifiants vivent dans un bloc `<server>` de `~/.m2/settings.xml`, indexé
par un `<id>` qui correspond à l'identifiant de dépôt de
`<distributionManagement>` :

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

Vérifiez un artefact publié en récupérant ses métadonnées :

```bash
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  "https://batlehub.example.com/proxy/<registry>/maven2/com/example/mylib/maven-metadata.xml"
```

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
