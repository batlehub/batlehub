---
sourcePath: registries/sdkman.md
sourceHash: e5c7b93519002f07
---

# SDKMAN

Fait proxy et cache de SDKMAN — le JDK, Gradle, Maven, Kotlin et tous les
autres candidats — dans un seul registre : l'API des candidats
(`api.sdkman.io/2`) et le courtier de téléchargement (`broker.sdkman.io`)
derrière un unique bloc `type = "sdkman"`. `sdk list` et `sdk install` se
résolvent par des listings dont les versions bloquées ont été retirées, une
version bloquée répond `invalid` là où `sdk install` la valide, et la redirection
du courtier vers le CDN de l'éditeur est suivie **côté serveur**, de sorte que le
JDK de 200 Mo est mis en cache ici plutôt que renvoyé ailleurs.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `sdkman` |
| **Amont par défaut** | `api.sdkman.io/2` (API des candidats) · `broker.sdkman.io` (`broker_url`) |
| **Modes** | proxy seul |
| **Adressage** | `{candidate}/{version}/{platform}` — une archive par plateforme |
| **Publication privée** | ❌ proxy seul |
| **Coupure réseau** | hors ligne, `versions/all` et `candidates/default` sont composés à partir des archives de candidats détenues pour la plateforme |

## Mise en place du proxy

`sdkman-init.sh` ne définit ses deux variables d'API que si elles sont vides :
exportez-les donc **avant** de le sourcer — dans `/etc/profile.d`, dans un
`Containerfile`, ou dans le bloc `env:` d'un job de CI. Remplacez `<registry>`
par le nom de registre que vous avez configuré :

```sh
export SDKMAN_CANDIDATES_API="https://batlehub.example.com/proxy/<registry>/sdkman"
export SDKMAN_BROKER_API="https://batlehub.example.com/proxy/<registry>/sdkman/broker"
source "$HOME/.sdkman/bin/sdkman-init.sh"

sdk list java                # la table rendue, versions bloquées retirées
sdk install java 21.0.5-tem  # valider, télécharger par la route du courtier, hook
```

Le bloc de registre côté administrateur :

```toml
[[registries]]
name       = "jvm"
type       = "sdkman"
mode       = "proxy"                        # le seul mode : pas de protocole de publication
upstreams  = ["https://api.sdkman.io/2"]    # l'API des candidats (le /2 en fait partie)
broker_url = "https://broker.sdkman.io"     # le courtier de téléchargement

[registries.rbac]
# candidates, versions, validate, hooks et healthcheck sont des listings
# (`releases:list`) ; le téléchargement est une lecture (`releases:read`).
# Une installation exige les deux.
anonymous = ["releases:read", "releases:list"]
```

`broker_url` est le second hôte de SDKMAN et de personne d'autre : il est rejeté
sur tout autre type de registre. Le `/2` d'`upstreams` est servi tel qu'il est
donné — un opérateur qui pointe vers `https://beta.sdkman.io/2` doit pouvoir le
dire — et une URL qui en est dépourvue est signalée au chargement, parce que
c'est plus probablement une faute de frappe qu'un choix.

**Ce que l'installateur n'est pas.** BatleHub fait proxy du registre, pas de
`get.sdkman.io` : amorcer SDKMAN lui-même est un `curl | bash` contre l'hôte de
son propre installateur, documenté comme prérequis de coupure réseau plutôt que
répliqué ([RFC 0010](/rfc/0010-toolchain-managers), décision 9). Les endpoints
`broker/version/sdkman/…` et `selfupdate/…` *sont*, eux, servis, parce que le
client en fonctionnement les appelle.

## Versions bloquées

Un blocage porte sur le **candidat** : bloquer `java 21.0.5-tem` couvre les huit
plateformes, pas seulement celle dont vous lisiez le listing. Il atteint tous les
endroits où `sdk` regarde :

- `versions/all` — la version est retirée de la liste séparée par des virgules.
- `candidates/default/{c}` — si le défaut nomme la version bloquée, il est
  réparé vers la plus récente autorisée, comme l'est le `dist-tags.latest` de
  npm.
- le `versions/list` rendu — la table qu'imprime `sdk list <candidate>`, dans ses
  deux mises en page. Dans la table des éditeurs Java, la ligne entière disparaît
  et le nom de l'éditeur est promu à la ligne suivante de son bloc ; dans la
  grille qu'emploie tout autre candidat, la cellule est vidée à sa propre largeur
  et rien ne bouge. Rien n'est re-rendu : un changement de mise en page en amont
  se dégrade donc en une version que nous n'avons pas su masquer, jamais en une
  table corrompue.
- `candidates/validate/{c}/{v}/{plat}` — **le point de passage obligé**. Chaque
  `sdk install` valide ici et s'arrête sur tout ce qui n'est pas `valid` ; une
  version bloquée répond `invalid` sans que l'amont soit interrogé, et SDKMAN
  imprime son propre refus (*« Stop! java 21.0.5-tem is not available … is an
  invalid version »*). Aucun téléchargement n'est tenté.

Une requête directe vers `broker/download/…` pour une version bloquée reçoit
toujours le `403` et le motif de l'opérateur : masquer gouverne la résolution,
cela ne remplace pas le diagnostic.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
et [quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered).

## Authentification

`sdk` construit sa propre commande `curl` et n'a nulle part où placer un en-tête.
libcurl lit `~/.netrc` sans qu'on le lui demande : une instance authentifiée a
donc besoin d'une entrée pour l'hôte du proxy.

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

## Notes

- **Les scripts de hook sont relayés octet pour octet.** `hooks/pre` et
  `hooks/post` renvoient du bash que `sdk` source et exécute sous l'utilisateur
  appelant — le hook du JDK Linux réempaquette l'archive avec `tar` et `zip`.
  C'est la conception de SDKMAN et ce que le client fait aujourd'hui directement
  depuis `api.sdkman.io` ; BatleHub n'ajoute ni ne retire de confiance en
  transportant ces octets, et ne les modifie pas, parce que réécrire une URL à
  l'intérieur d'un hook ferait de lui un coauteur du shell exécuté.
- **La chaîne de redirections est suivie à travers le garde-fou SSRF.** Le
  courtier nomme l'hôte de téléchargement, et cet hôte n'est pas SDKMAN :
  `github.com`, `repo.maven.apache.org`, `services.gradle.org`, et tout ce sur
  quoi un éditeur publie. Chaque saut est vérifié contre le garde-fou, et les
  identifiants amont de l'opérateur s'arrêtent aux deux origines configurées. La
  sortie réseau vers ces hôtes de CDN est un prérequis ; la liste est observée,
  pas exhaustive — voir
  [Ce qui sort de cette instance](/fr/operations/egress).
- **Pas de dates : un garde-fou d'âge doit donc afficher sa position.** SDKMAN ne
  publie aucune date de publication, de sorte que tout artefact atteint un
  `release_age_gate` avec `published_at = None`. Sur ce type,
  `deny_missing_timestamp` est **obligatoire** : `true` refuse tout
  téléchargement sur le registre, `false` rend le garde-fou inerte, et ce serveur
  ne choisit ni l'un ni l'autre à votre place. Un bloc `[registries.security]`
  ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)) retient plutôt par
  défaut sur un horodatage manquant.
- **Les en-têtes de réponse `X-Sdkman-*` ne sont pas transmis.** Le client lit
  `X-Sdkman-Checksum-<ALG>` pour vérifier un téléchargement ; aucun candidat
  échantillonné n'en émet, et le lecteur du client (`grep '^X-Sdkman'`) ne
  correspond à rien sur les noms d'en-têtes en minuscules que l'API envoie
  aujourd'hui : le contrôle est donc inerte, avec ou sans proxy. Consigné dans la
  RFC 0010 §13.2 plutôt que laissé à découvrir.
- **Le préchauffage a besoin d'une plateforme.**
  `warm_packages = ["java@21.0.5-tem"]` préchauffe une archive par entrée de
  `cache.warm_platforms`, avec pour défaut la plateforme sur laquelle tourne ce
  serveur ; deviner les huit récupérerait 1,6 Go de JDK pour un `.sdkmanrc` d'une
  ligne. `batlehub-cli registry suggest` lit `.sdkmanrc` et écrit les deux.
- **La liste rendue est mise en cache par client.** Sa clé de cache inclut la
  requête `?current=&installed=` qu'envoie `sdk list`, de sorte que deux machines
  aux ensembles installés différents ne partagent jamais une entrée.

## Endpoints

<!-- BEGIN endpoints: proxy/sdkman -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/sdkman/broker/download/{candidate}/{version}/{platform}` | The archive for one version on one platform, streamed through the |
| `GET` | `/proxy/{registry}/sdkman/broker/version/sdkman/{component}/{channel}` | The current SDKMAN script or native version on a channel — what `sdk |
| `GET` | `/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/all` | Every version of a candidate on a platform, comma-separated, blocked |
| `GET` | `/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/list` | The rendered table `sdk list <candidate>` prints, in either of its two |
| `GET` | `/proxy/{registry}/sdkman/candidates/all` | Every candidate name, comma-separated — what `sdk update` caches and |
| `GET` | `/proxy/{registry}/sdkman/candidates/default/{candidate}` | The version `sdk install <candidate>` resolves to with no version given, |
| `GET` | `/proxy/{registry}/sdkman/candidates/list` | The rendered candidate table `sdk list` prints with no argument. |
| `GET` | `/proxy/{registry}/sdkman/candidates/validate/{candidate}/{version}/{platform}` | The chokepoint: `valid` or `invalid` for one version on one platform. A |
| `GET` | `/proxy/{registry}/sdkman/healthcheck` | The API's health token, read by every `sdk` invocation. Relayed as-is: |
| `GET` | `/proxy/{registry}/sdkman/hooks/{phase}/{candidate}/{version}/{platform}` | A pre- or post-install hook: bash the client sources and runs, relayed |
| `GET` | `/proxy/{registry}/sdkman/selfupdate/{channel}/{platform}` | The self-update script for a channel and platform — bash the client |
<!-- END endpoints -->

## Voir aussi

- [Distributions Node](/fr/registries/nodedist) — l'autre type de chaîne d'outils de la RFC 0010
- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
