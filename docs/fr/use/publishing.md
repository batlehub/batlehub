---
sourcePath: use/publishing.md
sourceHash: 66fdabd55153a8cf
---

# Publier des paquets sur BatleHub

Ce guide déroule la publication de paquets sur un registre privé BatleHub, pour
chaque type de registre pris en charge. Publier suppose un registre en mode
`local` ou `hybrid` et un token doté des permissions suffisantes.

## 1. Prérequis

La publication n'est possible que si le registre est configuré avec
`mode = "local"` ou `mode = "hybrid"`. En mode `proxy` (le défaut), toutes les
requêtes d'écriture sont refusées.

| Mode | Comportement |
|------|-----------|
| `local` | BatleHub est la seule source. Aucun amont nécessaire. |
| `hybrid` | Les paquets locaux priment ; les paquets inconnus sont cherchés en amont. |

La référence complète est
[Configuration § Modes de registre](/fr/guide/configuration#registry-modes).

---

## 2. Obtenir un token d'API

Toute requête de publication exige un token `Bearer` dans l'en-tête
`Authorization`.

### Tokens statiques (config.toml)

L'option la plus simple pour un pipeline de CI ou une installation à un seul
utilisateur :

```toml
[[auth]]
type = "token"

[[auth.tokens]]
value   = "my-publish-token"
role    = "admin"
user_id = "ci"
```

### Tokens d'API générés par l'utilisateur (sessions OIDC)

Si vous utilisez la connexion OIDC, vous pouvez générer des tokens à durée de vie
courte depuis la console (Paramètres → Tokens) ou par l'API :

```sh
curl -s -X POST \
  -H "Authorization: Bearer <oidc-session-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "ci-publish", "expires_in_days": 30, "role": "user"}' \
  https://batlehub.example.com/api/v1/auth/tokens
```

La réponse contient la valeur brute du token — enregistrez-la, elle n'est
affichée qu'une fois.

```json
{
  "id": "...",
  "name": "ci-publish",
  "token": "bh_xxxxxxxxxxxxxxxxxxxx",
  "expires_at": "2026-06-21T00:00:00Z"
}
```

---

## 3. Les instructions de votre écosystème

La publication se fait écosystème par écosystème, et chaque écosystème a une
page. C'est là que vivent sa configuration serveur, sa mise en place côté client,
la commande de publication, comment vérifier que la publication a réussi et sa
référence d'endpoints — cette page-ci ne porte que ce qui est commun à tous.

| Catégorie | Registre | Instructions de publication |
| --- | --- | --- |
| Langage | npm | [/fr/registries/npm](/fr/registries/npm#publishing-local-hybrid) |
| Langage | Cargo | [/fr/registries/cargo](/fr/registries/cargo#publishing-local-hybrid) |
| Langage | Modules Go | [/fr/registries/goproxy](/fr/registries/goproxy#publishing-local-hybrid) |
| Langage | Maven | [/fr/registries/maven](/fr/registries/maven#publishing-local-hybrid) |
| Langage | PyPI | [/fr/registries/pypi](/fr/registries/pypi#publishing-local-hybrid) |
| Langage | Conda | [/fr/registries/conda](/fr/registries/conda#publishing-local-hybrid) |
| Langage | Composer | [/fr/registries/composer](/fr/registries/composer#publishing-local-hybrid) |
| Langage | RubyGems | [/fr/registries/rubygems](/fr/registries/rubygems#publishing-local-hybrid) |
| Langage | NuGet | [/fr/registries/nuget](/fr/registries/nuget#publishing-local-hybrid) |
| Langage | Terraform | [/fr/registries/terraform](/fr/registries/terraform#publishing-local-hybrid) |
| Extensions d'éditeur | OpenVSX | [/fr/registries/openvsx](/fr/registries/openvsx#publishing-local-hybrid) |
| Extensions d'éditeur | Place de marché VS Code | [/fr/registries/vscode-marketplace](/fr/registries/vscode-marketplace#publishing-local-hybrid) |
| Extensions d'éditeur | Place de marché JetBrains | [/fr/registries/jetbrains-marketplace](/fr/registries/jetbrains-marketplace#publishing-local-hybrid) |
| Paquets système | Debian / APT | [/fr/registries/deb](/fr/registries/deb#publishing-local-hybrid) |
| Paquets système | RPM / YUM / DNF | [/fr/registries/rpm](/fr/registries/rpm#publishing-local-hybrid) |
| Paquets système | Pacman / Arch | [/fr/registries/pacman](/fr/registries/pacman#publishing-local-hybrid) |

Les forges de code source (GitHub, GitLab, Forgejo), le miroir des IDE JetBrains
et le miroir générique sont en proxy seul — il n'y a rien à y publier.

---

## 4. Dépannage

### `403 Forbidden` à la publication

- Le token est absent, expiré, ou ne porte pas le rôle requis. La publication est
  réservée au rôle `admin` par défaut. Regardez le bloc `[registries.rbac]` — le
  rôle censé publier a besoin de `"*"` (ou au minimum d'un accès en écriture).
- Passez le token explicitement : `-H "Authorization: Bearer <token>"`.

### `403 Forbidden` — « registry is not in local or hybrid mode »

Le `mode` du registre vaut `proxy` (le défaut). Passez-le à `"local"` ou
`"hybrid"` dans `config.toml` et redémarrez le serveur.

### `409 Conflict`

La version existe déjà dans le registre. Incrémentez-la dans le manifeste de
votre paquet et republiez.

### `400 Bad Request` (Go)

La structure du zip de module est invalide. Chaque entrée du zip doit être
préfixée par `{module}@{version}/`. Reconstruisez-le avec `go mod zip` pour
obtenir la bonne disposition.

### `400 Bad Request` (Cargo)

Cargo emploie un format binaire propre à son protocole (JSON de métadonnées
préfixé par sa longueur, suivi des octets du `.crate`). Seul `cargo publish` produit ce format —
n'essayez pas de fabriquer la requête à la main.

### Le token est accepté mais `cargo publish` échoue avec « invalid token »

Cargo attend que le `config.json` de l'index sparse corresponde à l'endpoint du
token. Vérifiez que l'URL `index` de `.cargo/config.toml` se termine par
`/registry/` :

```
sparse+https://batlehub.example.com/proxy/internal/registry/
```

### `400 Bad Request` (Maven) — « POM missing groupId »

Le fichier `.pom` envoyé n'a pas de `<groupId>` ou pas d'`<artifactId>`. Ces
champs sont obligatoires. Vérifiez que votre `pom.xml` ou votre
`build.gradle.kts` définit bien `group` et `archivesName` /
`rootProject.name` avant de publier.

### `mvn deploy` réussit mais `maven-metadata.xml` n'est pas à jour

BatleHub génère `maven-metadata.xml` dynamiquement depuis la base. Un envoi de
`.pom` réussi (HTTP 201) signifie que la version a été enregistrée. Si le GET
renvoie 404, c'est peut-être l'envoi du `.pom` qui a échoué — vérifiez le statut
de réponse de chaque fichier envoyé en mode verbeux (`mvn deploy -X`).

### `terraform init` échoue — « registry does not have a provider »

Vérifiez que l'adresse `source` de `required_providers` correspond exactement au
nom d'hôte et au chemin du registre :
```
batlehub.example.com/proxy/{registry}/namespace/type
```
Assurez-vous que les identifiants pour `batlehub.example.com` sont bien dans
`~/.terraformrc`.

### Le téléchargement d'un provider Terraform échoue — « no matching binary »

Le manifeste du provider a été envoyé sans binaire pour la plateforme demandée.
Envoyez le binaire par :
```
PUT /proxy/{registry}/v1/providers/{ns}/{type}/{version}/artifact/{os}/{arch}
```
