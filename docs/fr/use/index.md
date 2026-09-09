---
sourcePath: use/index.md
sourceHash: b389356de281e393
---

# Utiliser BatleHub

**Pour la personne dont le gestionnaire de paquets parle à BatleHub.** Configurer
son environnement local pour passer par lui, et publier des paquets privés quand
l'administrateur a activé le mode `local` ou `hybrid`.

Si c'est vous qui *exploitez* le serveur — installation, configuration des
registres, attribution des accès — c'est le
[guide de l'opérateur](/fr/guide/installation) qu'il vous faut.

- **[La page de votre écosystème](/fr/registries/)** — l'extrait de configuration
  pour npm, Cargo, Maven, PyPI et dix-huit autres. Commencez ici si vous voulez
  simplement que ça marche.
- **[Publier](/fr/use/publishing)** — prérequis, tokens, et où vivent les
  instructions de publication de chaque écosystème.
- **[Client en ligne de commande](/fr/use/cli)** — `batlehub-cli`, TUI comprise.
- **[Dépannage](/fr/use/troubleshooting)** — quand ça ne marche pas.

---

## Obtenir un token {#getting-a-token}

La plupart des endpoints de BatleHub exigent un token Bearer. Demandez-en un à
votre administrateur ou, si la connexion OIDC est activée, générez-le vous-même :

**Depuis la console web :** connectez-vous sur `https://batlehub.example.com`,
ouvrez Paramètres → Tokens, puis cliquez sur « Nouveau token ».

**Par l'API :**

```sh
# Échanger votre token de session OIDC contre un token d'API à longue durée de vie
curl -X POST \
  -H "Authorization: Bearer <oidc-session-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-laptop", "expires_in_days": 90, "role": "user"}' \
  https://batlehub.example.com/api/v1/auth/tokens
```

La valeur du token n'est affichée **qu'une fois** — enregistrez-la dans un
gestionnaire de mots de passe ou une variable d'environnement.

```sh
export BATLEHUB_TOKEN=bh_xxxxxxxxxxxxxxxxxxxx
```

### S'authentifier depuis GitHub / Forgejo Actions {#ci-actions-oidc}

Si votre administrateur a configuré un fournisseur d'authentification
`actions-oidc`, les jobs de workflow GitHub et Forgejo peuvent s'authentifier
**sans aucun secret à longue durée de vie**. Le workflow demande un token OIDC
éphémère au runner et le présente directement comme token Bearer.

Activez l'émission de tokens OIDC dans votre workflow :

```yaml
jobs:
  publish:
    permissions:
      id-token: write   # requis — autorise le runner à émettre un token OIDC
      contents: read
```

Puis échangez le token au début de toute étape qui appelle BatleHub :

```sh
# Dans une étape "run:" de GitHub Actions :
BATLEHUB_TOKEN=$(curl -s -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=batlehub" | jq -r '.value')

# Il s'utilise exactement comme n'importe quel token Bearer
curl -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  https://batlehub.example.com/api/v1/...
```

Le token est valide pour la durée du job. Il porte des claims comme `repository`,
`ref`, `environment` et `actor`, dont le fournisseur `actions-oidc` se sert pour
vous rattacher à un ou plusieurs groupes — par exemple
`"github-actions/myorg-my-repo/main"` — de sorte que vous recevez automatiquement
les bonnes permissions RBAC, sans gestion d'utilisateurs à la main.

Demandez à votre administrateur quels groupes sont associés et quelles
permissions ils portent.

---

### Créer des tokens depuis l'API {#tokens-api}

Un utilisateur authentifié par OIDC peut créer des tokens d'API personnels à
longue durée de vie sans repasser par le SSO à chaque fois. C'est l'approche
recommandée pour les pipelines de CI/CD quand l'authentification par compte de
service Kubernetes n'est pas disponible.

```sh
# Créer un token (valide 30 jours, ne peut pas dépasser le rôle du créateur)
curl -X POST https://batlehub.example.com/api/v1/auth/tokens \
  -H "Authorization: Bearer <oidc-access-token>" \
  -H "Content-Type: application/json" \
  -d '{"name": "ci-token", "expires_in_days": 30}'

# Lister les tokens actifs
curl https://batlehub.example.com/api/v1/auth/tokens \
  -H "Authorization: Bearer <oidc-access-token>"

# Révoquer un token
curl -X DELETE https://batlehub.example.com/api/v1/auth/tokens/<token-id> \
  -H "Authorization: Bearer <oidc-access-token>"
```

Propriétés à retenir :
- La valeur d'un token n'est affichée **qu'une fois**, à sa création ; conservez-la
  en lieu sûr.
- Le rôle d'un token ne peut pas dépasser celui de l'utilisateur qui l'a créé.
- L'authentification par token (`type = "token"`) dans le fichier de configuration
  et les tokens générés par les utilisateurs sont deux mécanismes distincts ; les
  seconds sont toujours disponibles pour les utilisateurs authentifiés par OIDC,
  sans entrée `[[auth]]` supplémentaire.

---

## Le guide de mise en place de la console

Le **guide de mise en place** intégré, à l'adresse
`https://batlehub.example.com/setup`, génère des extraits de configuration prêts à
coller pour chaque outil enregistré. Ils sont préremplis avec l'adresse de votre
serveur et les registres disponibles — servez-vous-en comme point de départ des
étapes manuelles ci-dessous.

---

## Mise en place, registre par registre {#registries}

Chaque type de registre a sa page dédiée dans la
[référence des registres](/fr/registries/) : mise en place du proxy, publication
(en mode `local` / `hybrid`) et authentification. Choisissez-y votre écosystème —
hébergement de code source, registres de langage, extensions d'éditeur, paquets
système et miroirs de binaires y sont regroupés.

La liste vit là et seulement là. Une seconde copie sur cette page serait une
seconde copie à tenir à jour, et celle qui prendrait du retard serait celle que le
lecteur trouve en premier.

---

## Audit de sécurité {#security-audit}

Plusieurs écosystèmes savent faire passer leur audit de vulnérabilités par
BatleHub — le proxy transmet la requête à la base d'alertes en amont, donc aucun
accès direct à Internet n'est nécessaire.

### npm audit {#audit-npm}

`npm audit` fonctionne automatiquement dès que le registre est configuré : les
deux modes d'audit, rapide et groupé, passent par BatleHub jusqu'à la base
d'alertes amont.

```sh
npm audit
npm audit --fix
```

### Composer audit {#audit-composer}

`composer audit` fonctionne automatiquement dès que le dépôt est configuré —
BatleHub fait proxy de l'API d'alertes de sécurité de Packagist de façon
transparente.

```sh
composer audit
```

### Go — govulncheck {#audit-go}

BatleHub fait proxy de la [base de vulnérabilités Go](https://vuln.go.dev), de
sorte que `govulncheck` fonctionne sans accès direct à vuln.go.dev. Donnez à
`GOVULNDB` la même URL de base qu'à `GOPROXY` :

```sh
export GOVULNDB="https://batlehub.example.com/proxy/go"
govulncheck ./...
```

Avec authentification (mettez le token dans `~/.netrc`) :

```sh
echo "machine batlehub.example.com login user password $BATLEHUB_TOKEN" >> ~/.netrc
chmod 600 ~/.netrc
```

`machine` est comparé au nom d'hôte : ce doit donc être l'hôte présent dans
`GOVULNDB` ci-dessus. Sur un déploiement en
[routage par hôte](/rfc/0001-subdomain-routing), c'est le sous-domaine du
registre lui-même (`go.batlehub.example.com`), pas l'hôte principal — une ligne
`machine` par hôte que vous interrogez. L'onglet **.netrc** du guide de mise en
place les liste tous, déjà remplis.

L'URL de la base govulndb se change registre par registre avec `vuln_db_url` dans
la configuration du serveur (par défaut : `https://vuln.go.dev`). La valeur `""`
désactive les endpoints.

### .NET — paquets vulnérables {#audit-dotnet}

`dotnet list package --vulnerable` fonctionne automatiquement — BatleHub expose
une ressource `VulnerabilitiesUrl` dans l'index de services v3 et fait proxy du
catalogue de vulnérabilités depuis la galerie NuGet amont.

```sh
dotnet list package --vulnerable
dotnet list package --vulnerable --include-transitive
```

---

## Le tableau de bord de namespace d'équipe {#team-namespace}

Si votre administrateur a attribué des namespaces à votre groupe, la page
**Namespace d'équipe**, à l'adresse `/my-namespace`, réunit en un seul endroit ce
que vous possédez, les paquets publiés, la gestion de la visibilité et le
téléversement de nouveaux paquets, sans passer par la CLI.

### Vos groupes {#ns-groups}

La carte du haut liste tous les groupes du fournisseur d'authentification
auxquels vous appartenez. Ce sont les valeurs que votre administrateur emploie
pour créer les namespaces. Les espaces sont retirés des noms de groupe parce
qu'un préfixe de paquet ne peut pas en contenir : `"oidc:my team"` est affiché et
comparé comme `"oidc:myteam"`.

### Vos namespaces {#ns-namespaces}

La table **Mes namespaces** montre chaque préfixe attribué à vos groupes, tous
registres confondus. Chaque ligne indique :

| Colonne | Description |
|--------|-------------|
| Registre | Le registre auquel s'applique cette attribution |
| Préfixe | Le préfixe de nom de paquet que votre groupe possède |
| Groupe | L'identifiant du groupe (espaces retirés) |

Cliquez sur une ligne pour charger les paquets publiés sous ce namespace.

### Parcourir et gérer les paquets {#ns-packages}

Après un clic sur une ligne de namespace, la carte **Paquets** montre toutes les
versions publiées sous ce préfixe. Les colonnes couvrent le nom du paquet, la
version, la visibilité, le publieur et la date de publication.

**Changer la visibilité directement :**

Cliquez sur le badge de visibilité d'une ligne (ou sur le bouton « Modifier la
visibilité ») pour ouvrir une liste déroulante. Choisissez le nouveau niveau puis
cliquez sur **Enregistrer** :

| Niveau | Qui peut télécharger |
|-------|-----------------|
| `public` | Tout le monde, y compris sans authentification |
| `internal` | Tout utilisateur authentifié |
| `team` | Les membres de votre groupe uniquement |

Les résultats sont paginés (50 par page). Les boutons Précédent / Suivant
permettent de naviguer.

### Téléverser des paquets {#ns-upload}

La carte **Téléverser un paquet** permet de publier directement depuis le
navigateur, pour les types de registre qui acceptent l'envoi d'un fichier
binaire. Seuls les registres en mode `local` ou `hybrid` apparaissent dans le
sélecteur.

#### Envoi de fichier (navigateur)

| Type de registre | Fichier accepté | Champs supplémentaires |
|--------------|---------------|--------------|
| RubyGems | `.gem` | Aucun — le nom et la version sont lus dans la gem |
| Composer | `.zip` | Aucun — le nom et la version sont lus dans le `composer.json` de l'archive |
| OpenVSX / place de marché VS Code | `.vsix` | Identifiant d'extension (`publisher.name`) et version |
| Modules Go | `.zip` | Chemin du module (par ex. `github.com/org/repo`) et version (par ex. `v1.0.0`) |
| PyPI | `.whl`, `.tar.gz`, `.zip` | Aucun — le nom et la version sont extraits du nom de fichier |
| Conda | `.tar.bz2`, `.conda` | Plateforme (par ex. `linux-64`) — le nom, la version et le build sont lus dans `info/index.json` |

Sélectionnez le registre, remplissez les champs supplémentaires éventuels,
choisissez le fichier et cliquez sur **Téléverser**.

::: tip Format du zip d'un module Go
Le zip doit suivre la disposition standard des modules Go : chaque entrée doit
être préfixée par `{module}@{version}/`. `go mod zip` produit cette disposition
automatiquement.
:::

#### CLI (npm, Cargo, Maven, Terraform, NuGet)

Pour les types de registre sans format binaire adapté au navigateur, l'onglet
**Instructions CLI** affiche des commandes prêtes à coller, préremplies avec le
nom de votre registre. La [référence des registres](/fr/registries/) donne les
étapes complètes de chaque écosystème.

---

## Permissions

| Permission | Ce qu'elle accorde |
|-----------|----------------|
| `releases:read` | Lister les versions, télécharger les assets de publication et les métadonnées |
| `source:read` | Télécharger les archives de source (tarballs, `.crate`, `.zip` de module) |
| `*` | Toutes les permissions (admin) |

Héritage des rôles : `admin` ⊃ `user` ⊃ `anonymous`. Votre administrateur peut
accorder des permissions supplémentaires à des groupes OIDC ou à des namespaces
de comptes de service Kubernetes, par-dessus votre rôle.

---

## Dépannage

**`403 Forbidden` au téléchargement :** votre token est absent, ou votre rôle n'a
pas `releases:read` ni `source:read` sur ce registre. Voyez avec votre
administrateur.

**`403 Forbidden` à la publication — « registry is not in local or hybrid
mode » :** la publication est désactivée sur ce registre. Demandez à votre
administrateur d'activer `mode = "local"` ou `mode = "hybrid"`.

**`409 Conflict` à la publication :** la version existe déjà. Incrémentez-la dans
le manifeste de votre paquet.

**`cargo publish` échoue avec « invalid token » :** vérifiez que l'URL `index` de
`.cargo/config.toml` se termine bien par `/registry/` :
```
sparse+https://batlehub.example.com/proxy/internal/registry/
```

**Go : `disabled by GOPROXY=...off` :** le proxy n'arrive pas à joindre l'amont, ou
le module n'y existe pas. Retirez `,off` de `GOPROXY` pour autoriser le repli
direct, ou vérifiez que l'amont est joignable depuis le serveur BatleHub.

**`dotnet nuget push` renvoie 401 :** BatleHub accepte la valeur de `--api-key`
comme token Bearer (l'en-tête `X-NuGet-ApiKey` est normalisé de façon transparente
en `Authorization: Bearer`). Assurez-vous que le token porte `releases:publish` ou
des permissions d'admin sur le registre.
