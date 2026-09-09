---
sourcePath: registries/index.md
sourceHash: 4f5aaf2aa313c487
---

# Registres

BatleHub sert par proxy, met en cache et héberge en privé **23 types de
registres** — des gestionnaires de paquets par langage aux dépôts de paquets
système, en passant par les places de marché d'extensions d'éditeur et les
miroirs de fichiers génériques.

Chaque type de registre tourne dans l'un de trois **modes**, réglé registre par
registre dans la configuration :

- **proxy** — un pur cache en lecture devant un amont. La première requête est
  récupérée en amont et stockée ; toutes les suivantes sont servies depuis le
  cache.
- **local** — un registre entièrement privé. Rien n'est récupéré en amont : vous
  publiez et servez vos propres artefacts.
- **hybrid** — les artefacts locaux l'emportent, et tout ce qui n'est pas publié
  localement se rabat sur le proxy amont.

Cinq types sont en **proxy seul** (pas de modèle de publication privée) :
**GitHub**, **Forgejo**, **GitLab** (ils hébergent les sources et les releases en
amont), ainsi que les archives d'IDE **JetBrains** et les miroirs de fichiers
**generic** (caches par chemin uniquement).

## Les registres

### Hébergement de code source

| Registre | `type` | Ce dont il fait proxy | Modes | Publication | Amont par défaut |
|----------|--------|-----------------|-------|:-------:|------------------|
| [GitHub](./github) | `github` | Releases, assets, tarballs, fichiers bruts | proxy seul | ❌ | `api.github.com` |
| [Forgejo / Gitea](./forgejo) | `forgejo` | Releases, assets, archives, brut (`/api/v1`) | proxy seul | ❌ | `codeberg.org` |
| [GitLab](./gitlab) | `gitlab` | Releases, assets de liens, archives (`/api/v4`) | proxy seul | ❌ | `gitlab.com` |

### Gestionnaires de paquets par langage

| Registre | `type` | Ce dont il fait proxy | Modes | Publication | Amont par défaut |
|----------|--------|-----------------|-------|:-------:|------------------|
| [npm](./npm) | `npm` | Packument et tarballs | proxy · local · hybrid | ✅ | `registry.npmjs.org` |
| [Cargo](./cargo) | `cargo` | Index sparse et `.crate` | proxy · local · hybrid | ✅ | `crates.io` |
| [Modules Go](./goproxy) | `goproxy` | GOPROXY (`.info`/`.mod`/`.zip`) | proxy · local · hybrid | ✅ | `proxy.golang.org` |
| [Maven](./maven) | `maven` | XML de métadonnées, JAR et POM | proxy · local · hybrid | ✅ | `repo1.maven.org` |
| [PyPI](./pypi) | `pypi` | API Simple (PEP 503/691) et wheels | proxy · local · hybrid | ✅ | `pypi.org` |
| [Conda](./conda) | `conda` | `repodata.json` et `.conda`/`.tar.bz2` | proxy · local · hybrid | ✅ | `conda.anaconda.org` |
| [Composer (PHP)](./composer) | `composer` | Packagist v2 (métadonnées p2 et dist) | proxy · local · hybrid | ✅ | `repo.packagist.org` |
| [RubyGems](./rubygems) | `rubygems` | Gems, versions et API d'information | proxy · local · hybrid | ✅ | `rubygems.org` |
| [NuGet (.NET)](./nuget) | `nuget` | Index v3, index plat et `.nupkg` | proxy · local · hybrid | ✅ | `api.nuget.org` |
| [Terraform](./terraform) | `terraform` | Providers et modules (API v1) | proxy · local · hybrid | ✅ | `registry.terraform.io` |

### Extensions d'éditeur

| Registre | `type` | Ce dont il fait proxy | Modes | Publication | Amont par défaut |
|----------|--------|-----------------|-------|:-------:|------------------|
| [OpenVSX](./openvsx) | `openvsx` | VSIX d'extensions | proxy · local · hybrid | ✅ | `open-vsx.org` |
| [Place de marché VS Code](./vscode-marketplace) | `vscode-marketplace` | VSIX d'extensions (Gallery MS) | proxy · local · hybrid | ✅ | `marketplace.visualstudio.com` |
| [Place de marché JetBrains](./jetbrains-marketplace) | `jetbrains-marketplace` | API de plugins et téléchargements | proxy · local · hybrid | ✅ | `plugins.jetbrains.com` |

### Paquets système <Badge type="tip" text="adressé par chemin" />

| Registre | `type` | Ce dont il fait proxy | Modes | Publication | Amont par défaut |
|----------|--------|-----------------|-------|:-------:|------------------|
| [Debian / APT](./deb) | `deb` | `Packages`/`Release` et `.deb` | proxy · local · hybrid | ✅ | aucun — déclarez `upstreams` |
| [RPM / YUM / DNF](./rpm) | `rpm` | `repodata/` et `.rpm` | proxy · local · hybrid | ✅ | aucun — déclarez `upstreams` |
| [Pacman / Arch](./pacman) | `pacman` | `<repo>.db` et `.pkg.tar.zst` | proxy · local · hybrid | ✅ | aucun — déclarez `upstreams` |

### Binaires et miroirs <Badge type="tip" text="adressé par chemin" />

| Registre | `type` | Ce dont il fait proxy | Modes | Publication | Amont par défaut |
|----------|--------|-----------------|-------|:-------:|------------------|
| [IDE JetBrains](./jetbrains) | `jetbrains` | Archives d'installation d'IDE | proxy seul | ❌ | `download.jetbrains.com` |
| [Miroir générique](./generic) | `generic` | N'importe quelle arborescence HTTP | proxy seul | ❌ | aucun — déclarez `upstreams` et `path_allow` |

### Chaînes d'outils <Badge type="tip" text="RFC 0010" />

Typés, de sorte qu'une publication puisse être *bloquée* et pas seulement mise en
cache — l'identité que le miroir générique ne sait pas donner aux mêmes octets.

| Registre | `type` | Ce dont il fait proxy | Modes | Publication | Amont par défaut |
|----------|--------|-----------------|-------|:-------:|------------------|
| [Distributions Node](./nodedist) | `nodedist` | `index.tab`/`index.json`, archives de publication, `SHASUMS256.txt` octet pour octet (nvm, fnm, n, mise) | proxy seul | ❌ | `nodejs.org/dist` |
| [SDKMAN](./sdkman) | `sdkman` | API des candidats et courtier de téléchargement (le JDK, Gradle, Maven, Kotlin, …) ; le 302 du courtier est suivi côté serveur | proxy seul | ❌ | `api.sdkman.io/2` et `broker.sdkman.io` |

## Matrice des fonctionnalités

Chaque registre, et la façon dont ses capacités se projettent sur les
fonctionnalités de BatleHub. Les types **adressés par chemin** (Deb, RPM, Pacman,
IDE JetBrains, Generic) n'ont pas de modèle de version par paquet : les axes
structurels (liste de versions, archive de sources, binaire, garde-fou d'âge,
préchauffage, recherche) affichent donc `—`. Ils bénéficient tout de même du RBAC
au niveau du registre et de la diffusion vers plusieurs amonts, et Deb, RPM et
Pacman gèrent l'hébergement privé signé. **Forgejo** et **GitLab** reproduisent
le comportement de **GitHub**.

Légende : **Ver.** liste de versions · **Src** archive de sources · **Bin** asset
binaire ou d'extension · **Pub** publication privée · **Fan** diffusion vers
plusieurs amonts · **Âge** garde-fou d'âge de publication · **Préch.**
préchauffage du cache (énumération des versions) · **Rech.** recherche amont de
l'explorateur de paquets. ✓ pris en charge · `—` sans objet · ⚠ partiel.

| Registre | Ver. | Src | Bin | Pub | Fan | Âge | RBAC | Préch. | Rech. |
|----------|:----:|:---:|:---:|:---:|:---:|:---:|:----:|:----:|:------:|
| GitHub | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — |
| Forgejo / Gitea | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — |
| GitLab | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — |
| npm | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Cargo | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Modules Go | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Maven | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| PyPI | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Conda | ✓ ¹ | ✓ | ✓ | ✓ | ✓ | ⚠ ² | ✓ | ✓ ¹ | — |
| Composer | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| RubyGems | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| NuGet | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Terraform | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| OpenVSX | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Place de marché VS Code | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — |
| Place de marché JetBrains | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| Debian / APT ³ | — | — | — | ✓ | ✓ | — | ✓ | — | — |
| RPM / YUM / DNF ³ | — | — | — | ✓ | ✓ | — | ✓ | — | — |
| Pacman / Arch ³ | — | — | — | ✓ | ✓ | — | ✓ | — | — |
| IDE JetBrains ³ | — | — | — | — | ✓ | — | ✓ | — | — |
| Generic ³ | — | — | — | — | ✓ | — | ✓ | — | — |
| Distributions Node | ✓ | ✓ | ✓ | — | ✓ | ✓ ⁴ | ✓ | ✓ ⁵ | — |
| SDKMAN | ✓ | — | ✓ | — | ✓ | ⚠ ⁴ | ✓ | ✓ ⁵ | — |

> ¹ Conda n'a pas d'API dédiée de liste de versions par paquet. BatleHub en
> synthétise une en parcourant `repodata.json` sur `noarch`, `linux-64`,
> `osx-64`, `osx-arm64` et `win-64` ; le résultat est l'union des versions
> trouvées sur toutes les plateformes disponibles.
>
> ² Les horodatages conda viennent du champ `timestamp` de `repodata.json` (en
> millisecondes depuis l'époque). La plupart des paquets le portent ; ceux qui ne
> l'ont pas sautent le garde-fou, sauf si vous mettez
> `deny_missing_timestamp = true` sur la règle.
>
> ³ Type **adressé par chemin** : les artefacts sont récupérés par chemin de
> fichier, sans modèle de version par paquet, d'où les `—` sur les axes
> structurels. Ces types n'énumèrent pas de versions mais savent préchauffer des
> fichiers précis par `cache.warm_paths`, et sont encadrés par une liste
> d'autorisation `path_allow` obligatoire. Deb, RPM et Pacman gèrent en plus
> l'hébergement privé signé (`local` / `hybrid`) ; les archives d'IDE JetBrains
> et Generic sont en proxy seul.
>
> ⁴ **Garde-fous d'âge des chaînes d'outils** (RFC 0010 §6.7) : `nodedist` lit la
> date de publication dans `index.tab`, donc les publications courantes sont
> filtrées et une publication retirée du listing atteint le garde-fou sans date ;
> `sdkman` ne publie aucune date, donc le garde-fou est entièrement décidé par
> `deny_missing_timestamp`. Sur ces deux types, ce champ est **obligatoire** sur
> une règle `release_age_gate`.
>
> ⁵ **Préchauffage par plateforme** : une publication Node et une version SDKMAN
> sont une archive *par plateforme*, donc `warm_packages` préchauffe les
> plateformes de `cache.warm_platforms`, avec pour défaut celle du serveur. Le
> bouton de récupération par version de la console est refusé pour la même
> raison.
>
> Recherche amont de l'explorateur de paquets (« Pas encore passé par le
> proxy ») : Go passe par pkg.go.dev ; PyPI est une recherche par nom exact ;
> Terraform combine la recherche de modules avec une recherche de provider par
> namespace ou par paire exacte. Les proxys de releases (GitHub, Forgejo,
> GitLab), la place de marché VS Code, Conda et les types adressés par chemin
> n'ont pas d'API de recherche amont — voir le
> [guide de l'explorateur de paquets](/fr/use/package-explorer-search#upstream-search).

## READMEs

Ce qu'un paquet dit de lui-même, version par version. Qu'un registre en ait un du
tout est une propriété de son *protocole*, pas une préférence : le texte voyage
soit dans un document que le proxy récupère déjà pour résoudre une version, soit
à l'intérieur de l'artefact — et c'est ce qui décide si la page du paquet peut
l'afficher pour une version dont cette instance ne détient aucun octet.

**Détenu nulle part ici** est la colonne à lire avant de chercher un manque :
**versions + README** signifie que la page du paquet répond entièrement pour un
paquet que rien ici n'a jamais récupéré ; **versions seulement** signifie qu'elle
liste les versions et annonce que le README arrivera au premier téléchargement ;
**ni l'un ni l'autre** signifie que la page répond à partir de ce que cette
instance détient, et de rien d'autre.

**Récupérable** dit si la page propose un bouton *Récupérer cette version* sur
ces lignes purement amont, et si le catalogue en propose un sur un résultat de
recherche amont. `no` n'est pas une limite du bouton mais de la coordonnée : une
version Maven est un ensemble de fichiers, un provider Terraform s'adresse par
système et architecture en plus de la version, une version PyPI est une sdist
plus une wheel par interpréteur et par plateforme, et un artefact conda porte une
plateforme de canal et une chaîne de build — « récupérer cette version » n'a donc
de sens unique pour aucun d'eux. La page dit lequel, plutôt que d'afficher un
bouton désactivé — voir
[récupérer depuis la console](/fr/guide/admin-config#console-fetch).

<!-- BEGIN readme-coverage: generated by `task docs:readme-coverage`. Do not edit by hand. -->
| Registry | README source | Per version | Held nowhere here | Fetchable |
| --- | --- | --- | --- | --- |
| github | the README is one of the repository files this proxy already serves by path, under `raw/{ref}/`, so a second URL for it would be a second answer to a solved question | — | neither | no |
| forgejo | the README is one of the repository files this proxy already serves by path, under `raw/{ref}/`, so a second URL for it would be a second answer to a solved question | — | neither | no |
| gitlab | the README is one of the repository files this proxy already serves by path, under `raw/{ref}/`, so a second URL for it would be a second answer to a solved question | — | neither | no |
| cargo | a file inside the artifact | yes | versions only | yes |
| npm | the metadata document, else the artifact | yes | versions + README | yes |
| openvsx | a URL in the metadata, read separately | yes | versions + README | yes |
| goproxy | a file inside the artifact | yes | versions only | yes |
| pypi | the metadata document, else the artifact | yes | versions + README | no |
| conda | a file inside the artifact | yes | versions only | no |
| composer | a file inside the artifact | yes | versions only | yes |
| vscode-marketplace | a URL in the metadata, read separately | yes | versions + README | yes |
| maven | the POM carries `<description>`, which is a sentence rather than a document; putting one where a reader expects the other makes every package look thinly documented | — | versions only | no |
| terraform | a file inside the artifact | yes | versions only | no |
| rubygems | a file inside the artifact | yes | versions only | yes |
| nuget | a file inside the artifact | yes | versions only | yes |
| deb | path-addressed: there is no package identity to hang a README on | — | neither | no |
| rpm | path-addressed: there is no package identity to hang a README on | — | neither | no |
| pacman | path-addressed: there is no package identity to hang a README on | — | neither | no |
| jetbrains | path-addressed: there is no package identity to hang a README on | — | neither | no |
| jetbrains-marketplace | the metadata document, already fetched | yes | versions + README | yes |
| generic | path-addressed: there is no package identity to hang a README on | — | neither | no |
| nodedist | a Node release is a set of tarballs and a checksum file; the dist tree carries no prose | — | versions only | no |
| sdkman | SDKMAN describes a distribution, not a package: no document in the protocol carries prose about a candidate | — | versions only | no |
<!-- END readme-coverage -->

La table ci-dessus est générée depuis le code Rust et reste en anglais : ses
cellules sont de la prose que `RegistryKind::readme_support()` possède, et la
traduire créerait une seconde source de vérité pour une phrase que le code écrit.

Cela se configure registre par registre avec
[`[registries.readme]`](/fr/guide/admin-config#readme-capture) et
[`[registries.upstream_detail]`](/fr/guide/admin-config#the-console-s-discovery-read).
Les deux sont **actifs par défaut**, et le second émet une requête sortante la
première fois que quelqu'un ouvre la page d'un paquet dont cette instance ne
détient rien — voir
[ce qui sort de cette instance](/fr/operations/egress#the-console-s-discovery-read).

Deux réglages façonnent ce que la page sait faire de plus :
`remote_images = "proxy"` rend les images d'un README à travers ce serveur
plutôt que de les laisser partir ailleurs, et `console_fetch` (actif par défaut)
est le bouton *Récupérer cette version*, sur la page du paquet comme sur les
lignes amont du catalogue. La recherche dans la prose des README stockés vaut
pour toute l'instance et est désactivée par défaut —
[`[search] readmes`](/fr/guide/admin-config#search-readmes).

## Voir aussi

- [Guide utilisateur](/fr/use/) — les parcours pas à pas des registres les plus courants.
- [Administration → Configuration](/fr/guide/admin-config#configuration) — comment déclarer des registres dans `config.toml`.
- [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control) · [Feuille de route (en)](/guide/roadmap#new-registry-types).
