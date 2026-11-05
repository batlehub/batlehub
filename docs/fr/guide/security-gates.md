---
reference: true
sourcePath: guide/security-gates.md
sourceHash: 3810efe0f860a89b
---

# Contrôles de sécurité

Un garde-fou est un contrôle qui peut refuser un téléchargement que les droits
de l'appelant ont déjà autorisé : une release trop jeune pour qu'on s'y fie,
une version porteuse d'une vulnérabilité connue, un asset non signé, un paquet
qu'un administrateur a bloqué. Cette page est la référence de l'opérateur sur
chaque garde-fou que BatleHub exécute — ce que chacun lit, ce qu'il fait quand
cette donnée manque, et comment l'ensemble change dès qu'un registre a un profil
`[registries.security]`. La personne dont l'installation a été refusée a sa
propre page : [Pourquoi un téléchargement est refusé](/fr/use/refused-downloads).

---

## Deux régimes {#regimes}

Chaque registre exécute ses garde-fous de l'une de deux façons, et une seule
section décide laquelle.

| | Sans `[registries.security]` | Avec `[registries.security]` |
| --- | --- | --- |
| **Où tournent les garde-fous** | Sur la requête : chaque garde-fou répond oui ou non pendant le téléchargement | Surtout dans le scan worker. Ses constats sont stockés sous forme de **verdict**, que la requête lit |
| **`release_age_gate`** | Une règle | Remplacé par `security.min_age_secs`. Déclarer les deux est une erreur de configuration |
| **`cve_gate`, `license_gate`, `require_signed_release`, `trusted_publisher`** | Des règles, avec leurs propres `block`, `min_severity` et `bypass_roles` | Des scanners internes. Un constat est jugé par `security.max_severity` et `security.mode`, sans rôle de contournement |
| **`deny_latest`, `version_gate`** | Des règles | Toujours des règles : elles jugent la requête, pas l'artefact |
| **Liste de blocage admin, flags du SOC** | Des règles | Des scanners internes dont les constats sont toujours `denied` |
| **Ce que voit un client refusé** | Un simple `403` qui nomme le motif | La forme d'erreur native du registre, plus des en-têtes `X-BatleHub-*` pour les appelants autorisés à les voir |
| **Une version peut-elle être retenue puis libérée ?** | Non : chaque requête est jugée à neuf | Oui : `quarantined` se lève d'elle-même une fois la condition de la retenue levée |

La suite de cette page décrit les garde-fous un par un, et dit pour chacun ce
que le second régime y change. Le profil lui-même, ses champs et le scan worker
sont décrits dans [`[registries.security]`](/fr/guide/configuration#registries-security)
et [Le scan worker](/fr/operations/scan-worker).

::: warning Les réglages que le verdict ne lit pas
Sur un registre `[security]`, `bypass_roles` sur les quatre garde-fous
encapsulés, `block = false` sur `cve_gate` et `license_gate`,
`cve_gate.min_severity`, et toute surcharge de namespace de ces quatre
garde-fous sont sans effet. Un rechargement signale chacun d'eux comme
`security.rule-settings-ignored`, plutôt que de les laisser dans le fichier
comme s'ils servaient à quelque chose.
:::

---

## L'ordre dans lequel un téléchargement les rencontre {#order}

Un téléchargement proxifié franchit ces étapes dans l'ordre et s'arrête au
premier refus :

1. **Les blocages d'IP et de compte.** Ce sont des middlewares : ils refusent
   avant que rien de ce qui suit ne tourne. Voir
   [Blocage par IP](/fr/guide/access-control#ip-blocking).
2. **Les droits.** L'appelant doit détenir `releases:read` sur la coordonnée.
   C'est vérifié avant toute requête vers l'amont, de sorte qu'un appelant sans
   ce droit ne coûte aucun trafic sortant. Voir
   [Le modèle d'autorisation](/fr/guide/access-control#authorization).
3. **Les métadonnées.** Elles sont résolues d'abord depuis le cache. À la toute
   première requête pour une version non cachée, l'amont a donc déjà répondu
   avant qu'aucun garde-fou ne tourne : une date de publication ou un indicateur
   de signature est une vraie donnée, pas une supposition.
4. **La chaîne de garde-fous du registre**, dans cet ordre :
   - la [liste de blocage](#block-list) admin et le garde-fou des
     [flags](#flags), ou le [garde-fou du verdict](#verdict-gate) sur un
     registre `[security]`, qui porte les deux ;
   - sur `github`, `gitlab` et `forgejo` seulement, les garde-fous des
     [refs de forge](#forge-refs) et du [contenu brut](#forge-refs) ;
   - puis chaque entrée `[[registries.rules]]`, dans l'ordre du fichier.

La chaîne s'arrête au premier garde-fou qui refuse. La chaîne applicable est
choisie par paquet : quand un bloc `[[registries.namespaces]]` surcharge un
garde-fou, c'est le namespace correspondant le plus profond qui fournit la
chaîne (voir [Garde-fous par namespace](#namespaces)).

**Les lectures de paquets publiés localement** (registres `local` et `hybrid`)
exécutent la même chaîne, jugée sur la version stockée. Une lecture hybride
d'une version absente du stockage local saute les trois garde-fous qui ont
besoin de vraies métadonnées (`release_age_gate`, `require_signed_release` et
le garde-fou du verdict), puis bascule sur le chemin proxy, qui exécute la
chaîne complète sur les métadonnées de l'amont.

**Les listings et les index n'exécutent aucun garde-fou.** Un packument, une
liste de versions ou un `index.json` vérifie le droit `releases:list` et rien
d'autre. Deux choses sont tout de même retirées d'un listing : les versions de
la liste de blocage admin et, sur un registre `[security]`, les versions dont
le verdict n'est pas servi. `latest` est recalculé sans elles. Aucun autre
garde-fou ne masque une version dans un listing : une version que `cve_gate`
refuse reste listée, et refusée au téléchargement.

---

## Les garde-fous que vous configurez {#rules}

Chacun est une entrée `[[registries.rules]]`, choisie par `kind`. Tous les
champs sont listés dans [la référence de configuration](/fr/guide/configuration).

### `release_age_gate` {#release-age-gate}

Refuse une release publiée il y a moins de `min_age_secs`. Le but est de
laisser à l'écosystème le temps de repérer une release compromise avant que
vous ne l'installiez.

```toml
[[registries.rules]]
kind                   = "release_age_gate"
min_age_secs           = 259200   # 3 days; the default is 3600
bypass_roles           = ["admin"]
deny_missing_timestamp = false
```

- **Lit :** la date de publication que rapporte l'amont, ou la date stockée
  pour une version publiée localement.
- **Sans date :** la version est autorisée, sauf si
  `deny_missing_timestamp = true`. Sur `nodedist`, `sdkman`, `apk`, `galaxy`,
  `nix` et `devfile`, le champ n'a pas de défaut et doit être écrit. Sur `nix`,
  `sdkman` et `devfile`, aucune version n'est jamais datée : `true` refuse tout
  et `false` rend le garde-fou inopérant.
- **Refus :** `release is too recent (age: …s, minimum: …s, … seconds remaining in quarantine)`.
- **Sous `[security]` :** le déclarer est une erreur de configuration. Utilisez
  plutôt `security.min_age_secs` (au moins 3600). Le verdict retient la version
  en `MIN_AGE_NOT_MET`, avec la date à laquelle elle devient disponible, et
  retient une version non datée en `TIMESTAMP_MISSING` tant que
  `hold_missing_timestamp` est activé.

### `deny_latest` {#deny-latest}

Refuse une requête qui demande le tag `latest` au lieu d'une version, pour que
les builds soient reproductibles.

```toml
[[registries.rules]]
kind         = "deny_latest"
bypass_roles = ["admin"]
```

- **Lit :** la chaîne de version envoyée par le client. Seule la chaîne exacte
  `latest`, en minuscules, correspond.
- **Refus :** `requests for the 'latest' version tag are not allowed; pin an explicit version`.
- **Sous `[security]` :** inchangé. Il tourne toujours sur la requête.

### `version_gate` {#version-gate}

Une liste d'autorisation de versions approuvées, une liste de blocage de
versions connues comme mauvaises, ou les deux.

```toml
[[registries.rules]]
kind  = "version_gate"
allow = [">=1.2.0, <2.0.0"]   # when set, only matching versions are served
block = ["1.4.7", "1.5.0"]    # checked first: a block always wins
bypass_roles = ["admin"]
```

- **Correspondance :** une entrée qui contient `<`, `>`, `=`, `^`, `~`, `*` ou
  `,` et s'analyse comme une plage semver est une plage. Tout le reste est une
  chaîne exacte. Une version qui n'est pas semver ne correspond jamais à une
  plage.
- **Refus :** `version '…' is blocked by policy (known issue)` ou
  `version '…' is not in the approved allowlist`.
- **Sous `[security]` :** inchangé. Il tourne toujours sur la requête.

### `trusted_publisher` {#trusted-publisher}

Ne sert que les paquets dont l'éditeur figure sur une liste d'autorisation.
L'éditeur est lu dans les métadonnées déjà résolues : le contrôle ne fait aucun
appel supplémentaire à l'amont.

```toml
[[registries.rules]]
kind         = "trusted_publisher"
allow        = ["my-org", "trusted-user"]
bypass_roles = ["admin"]
```

| Type de registre | L'éditeur est… |
| --- | --- |
| `github`, `gitlab`, `forgejo` | le premier segment du chemin (`owner/repo` → `owner`) |
| `npm` | le scope (`@scope/name` → `scope`), sinon l'utilisateur qui a publié |
| `openvsx`, `vscode-marketplace` | le premier segment de l'identifiant d'extension (`publisher.extension` → `publisher`) |
| tout autre type, `cargo` compris | inconnu, donc **chaque requête est refusée** |

La correspondance ignore la casse. Une liste `allow` vide autorise tout. Un
éditeur impossible à déterminer est refusé : ce garde-fou échoue fermé.

- **Sous `[security]` :** un scanner interne dont le constat est
  `UNTRUSTED_PUBLISHER`, jugé par `max_severity`, sans contournement.

### `require_signed_release` {#require-signed-release}

Refuse une release que l'amont déclare non signée. Il s'appuie sur le signal de
l'amont lui-même, pas sur une vérification cryptographique.

```toml
[[registries.rules]]
kind                   = "require_signed_release"
enabled                = true
deny_missing_signature = false
bypass_roles           = ["admin"]
```

- **Lit :** si la release est signée. `github`, `gitlab` et `forgejo`
  signalent un asset de release `.asc`/`.sig`. `openvsx` et
  `vscode-marketplace` signalent un blob de signature. `nix` signale une ligne
  `Sig`. Une version publiée localement est signée si une signature a été
  téléversée avec elle. Tous les autres types ne signalent rien.
- **Sans signal :** la release est autorisée, sauf si
  `deny_missing_signature = true`. Une release déclarée non signée est
  toujours refusée.
- **Refus :** `this release is not signed; require_signed_release is enabled for this registry`.
- **Sous `[security]` :** un scanner interne dont le constat est
  `SIGNATURE_MISSING`, au niveau `high`, sans contournement.

### `cve_gate` {#cve-gate}

Refuse les versions qui portent une vulnérabilité enregistrée d'une sévérité
supérieure ou égale à un seuil.

```toml
[[registries.rules]]
kind         = "cve_gate"
min_severity = "high"   # unknown | low | medium | high | critical
block        = true     # the default is false: report only, never refuse
bypass_roles = ["admin"]
```

- **Lit :** les vulnérabilités que la réanalyse SBOM a enregistrées pour la
  version (`[vulnerability_scan]`, voir [SBOM](/fr/guide/sbom)). La réanalyse
  tourne hors bande : **rien n'est enregistré au premier téléchargement**
  d'une version, et ce téléchargement est autorisé.
- **Par défaut `block = false` :** les constats apparaissent dans la console et
  le garde-fou ne refuse rien.
- **Erreur du store :** échoue ouvert, avec une ligne de log `WARN`.
- **Refus :** `blocked: known <severity> vulnerability <id> (minimum gated severity: …)`.
- **Il fixe aussi le seuil du garde-fou des [flags](#flags).** Avec
  `block = false`, les flags `gate` cessent eux aussi de refuser.
- **Sous `[security]` :** les vulnérabilités enregistrées deviennent des
  constats `VULNERABILITY`. `min_severity`, `block` et `bypass_roles` ne sont
  pas lus. Dans ce régime, le scanner `osv` répond d'ailleurs généralement le
  premier.

### `license_gate` {#license-gate}

Refuse des versions selon la licence que déclare leur propre manifeste.

```toml
[[registries.rules]]
kind          = "license_gate"
allow         = ["MIT", "Apache-2.0", "BSD-3-Clause"]
deny          = ["AGPL-3.0", "SSPL-1.0"]   # checked first: a deny always wins
allow_unknown = true
block         = true                       # the default is false: report only
```

- **Lit :** la licence que l'extracteur SBOM a lue dans l'archive lors de sa
  mise en cache ou de sa publication. L'extraction existe pour `cargo`,
  `maven`, `npm`, `nuget` et `pypi`. Sur tous les autres types, la licence est
  toujours inconnue.
- **Le premier téléchargement est tranché par `allow_unknown`.** La licence
  n'est connue qu'une fois l'archive récupérée : avant cela, la version est
  jugée comme inconnue.
- **La correspondance** ignore la casse et reste sinon littérale :
  `allow = ["MIT"]` ne correspond pas à `MIT OR Apache-2.0`.
- **Refus :** `blocked: licence '…' is on the deny list`,
  `… is not on the allow list`, ou `no declared licence recorded … and license_gate.allow_unknown is false`.
- **Avertissements de configuration :** `license-gate.sbom-disabled`,
  `license-gate.no-extractor` et `license-gate.denies-everything` (avec
  `block = true` et `allow_unknown = false` sur un type sans extracteur).
- **Sous `[security]` :** un scanner interne dont le constat est
  `LICENSE_DENIED`, au niveau `high`. Il refuse quoi que dise `block`, sans
  contournement.

---

## Les garde-fous toujours présents {#built-in}

Ceux-ci ne se déclarent pas dans `[[registries.rules]]`. Ils font partie de
chaque chaîne à laquelle ils s'appliquent.

### La liste de blocage admin {#block-list}

Les versions qu'un administrateur a bloquées, avec le motif qu'il a donné (voir
[Bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)).
Un blocage sur une version nue couvre tous ses artefacts. Un blocage par
artefact ne couvre que ce fichier. L'[audit de l'amont](/fr/guide/configuration)
avec `on_confirmed = "block"` écrit lui aussi ses blocages ici.

Le refus reprend le motif de l'administrateur. Il n'y a pas de contournement.
Une erreur du store échoue ouvert. Sur un registre `[security]`, le blocage
devient `BLOCK_LIST`, toujours `denied`, même en `mode = "warn"`.

### Les flags poussés par un SOC {#flags}

Les flags poussés par une entrée `[[flag_sources]]` (voir
[Réponse aux incidents](/fr/operations/incident-response)) :

| Effet | Ce qu'il fait |
| --- | --- |
| `hard_block` | Toujours refusé. Sur un registre `[security]`, il devient `SOC_VERDICT`, toujours `denied` |
| `gate` | Refusé au seuil de `cve_gate` ou au-dessus (`high` si aucun `cve_gate` n'est déclaré), et seulement tant que ce `cve_gate` a `block = true` |
| `warn`, `inform` | Ne refusent jamais |

Il n'y a pas de contournement. Une erreur du store échoue ouvert sur le chemin
de la requête. Le scanner utilisé sur un registre `[security]`, lui, échoue
fermé.

### Les refs de forge et le contenu brut {#forge-refs}

Ils tournent sur les registres `github`, `gitlab` et `forgejo`, et se
configurent par `[registries.refs]` et `[registries.raw]` (voir
[la référence de configuration](/fr/guide/configuration)) :

- Une branche est `MUTABLE_REF` (par défaut : avertit).
- Un tag dont le commit a changé est `TAG_MOVED` (par défaut : refuse).
- Un asset de release dont le condensé a changé depuis sa mise en cache est
  `ASSET_REPLACED`.
- Les fichiers bruts sont refusés sauf si `[registries.raw]` les active pour le
  dépôt. `PINNED_REF_REQUIRED` refuse une branche quand `require_pinned` est
  activé, et `RAW_SCRIPT` signale un chemin de script.

Sans `[security]`, un refus est un simple `403` qui nomme le code, et un
avertissement n'apparaît que dans les en-têtes de ref. Avec `[security]`, les
codes rejoignent le verdict de la requête.

### Le garde-fou du verdict {#verdict-gate}

Celui-ci n'existe que sur un registre `[security]`, où il remplace les
garde-fous de la liste de blocage et des flags. Il lit le verdict stocké de la
version :

- **Première rencontre :** une version jamais vue met une analyse en file et
  est retenue en `SCAN_PENDING`.
- **Réévalué à la lecture :** la retenue d'âge et les scanners en attente sont
  recalculés à chaque lecture, de sorte qu'une retenue se lève d'elle-même.
- **États servis :** seuls `allowed` et `warned` sont servis. `quarantined` et
  `denied` sont refusés.
- **Contournement par maturité :** une version plus ancienne que
  `mature_age_secs` dont les seuls motifs sont `SCAN_PENDING` ou
  `SCANNER_ERROR` est servie en `warned`.
- **Erreur du store :** contrairement à tous les autres garde-fous, il
  **échoue fermé** : `… cannot be served: the verdict store is unavailable`.

Chaque code de motif qu'un verdict peut porter est listé, avec ce qu'il
signifie et ce qu'il faut faire, dans
[Pourquoi un téléchargement est refusé](/fr/use/refused-downloads#codes).
L'audit de présence amont en ajoute un sur ces registres,
`UNPUBLISHED_UPSTREAM`, pour une version que son amont ne liste plus.

---

## Les rôles de contournement {#bypass}

`bypass_roles` accepte `anonymous`, `user` ou `admin`. Toute autre valeur fait
échouer le chargement de la configuration. Le rôle le moins privilégié de la
liste sert de seuil, et chaque rôle égal ou supérieur passe : `["user"]` laisse
passer les utilisateurs et les administrateurs. Une liste vide ne laisse passer
personne. Seul le rôle de l'appelant compte, pas ses groupes.

Les garde-fous de la liste de blocage, des flags, des refs de forge et du
verdict n'ont pas de contournement. Sur un registre `[security]`, les quatre
garde-fous qu'il encapsule non plus (voir [Deux régimes](#regimes)). Pour
excuser une seule version, il y a l'[exemption](#namespaces).

---

## Garde-fous par namespace et exemptions {#namespaces}

Deux façons de juger différemment une partie d'un registre, toutes deux
décrites dans [Contrôle d'accès](/fr/guide/access-control#policies) :

- **Une surcharge de namespace** (`[[registries.namespaces.rules]]`) remplace
  les réglages du registre pour un garde-fou, pour les paquets sous un préfixe,
  et conserve les autres garde-fous. Le namespace correspondant le plus profond
  l'emporte, pour les paquets proxifiés comme pour les paquets publiés
  localement. En 1.8.2 et avant, une surcharge n'atteignait que les paquets
  publiés localement.
- **Une exemption** fait taire un garde-fou pour une version jusqu'à une date,
  avec un motif, et exige le droit `gates:exempt`, que personne ne détient par
  défaut (voir [Les exemptions de garde-fou](/fr/guide/access-control#exemptions)).
  Seuls `cve_gate`, `license_gate`, `flags` et `security_verdict` peuvent être
  exemptés. En 1.8.2 et avant, une exemption ne s'appliquait qu'aux versions
  publiées localement, pas aux téléchargements proxifiés.

Sur un registre `[security]`, les quatre garde-fous encapsulés sont des
scanners : aucun des deux mécanismes ne les atteint. L'exemption à écrire y est
`security_verdict` : elle sert la version retenue en `warned`, avec
`ADMIN_OVERRIDE` parmi ses motifs, jamais en `allowed`. Une surcharge de
namespace de `release_age_gate` n'est pas non plus refusée au chargement sur un
tel registre, et tourne comme une règle à côté de `security.min_age_secs`.
Préférez les champs du profil lui-même.

---

## Ce que reçoit un client refusé {#responses}

| Régime | Statut | Corps | En-têtes |
| --- | --- | --- | --- |
| Sans `[security]` | `403` | `{"error":"Forbidden","message":"<reason>"}` | aucun |
| Avec `[security]`, l'appelant n'a pas `quarantine:read` | `404` ; `403` sur `goproxy` et `maven` | le « not found » natif du registre | aucun |
| Avec `[security]`, l'appelant détient `quarantine:read` | `403` | la forme d'erreur native du registre, portant le message d'une ligne du verdict | `X-BatleHub-Verdict`, `X-BatleHub-Reason`, `X-BatleHub-Available-At`, `X-BatleHub-Details`, `Retry-After` |

- **Pourquoi un `404` :** sans `quarantine:read`, l'appelant ne peut pas
  distinguer une version retenue d'une version qui n'existe pas.
- **Défauts :** `quarantine:read` est accordé à `user` et `admin`.
  `findings:read`, qui ajoute au message les constats des scanners, n'est
  accordé qu'à `admin`.
- **`Retry-After` :** seulement sur une retenue bornée dans le temps.
- **Téléchargements avertis :** un téléchargement `warned` est un `200`
  normal, avec les mêmes en-têtes.

Le côté utilisateur, avec ce qu'affiche chaque gestionnaire de paquets, est sur
[Pourquoi un téléchargement est refusé](/fr/use/refused-downloads).

---

## Les surveiller {#observability}

- **Chaque refus** écrit une ligne d'audit `denied_download` avec le motif,
  l'appelant, l'IP et le user agent, et compte dans
  `batlehub_requests_total{outcome="denied"}`. Le garde-fou qui a refusé
  n'apparaît que dans le texte du motif : il n'y a pas de label de garde-fou.
- **Sur les registres `[security]` :**
  - `batlehub_verdict_denials_served_total{registry,code,status}` compte chaque
    téléchargement refusé une fois par code de motif, et une fois de plus avec
    `code="response"` et le statut envoyé ;
  - `batlehub_verdicts_total`, `batlehub_verdict_transitions_total` et
    `batlehub_findings_total` suivent les verdicts eux-mêmes.
- **Les garde-fous qui échouent ouverts écrivent une ligne `WARN`** quand leur
  store est illisible : `BlockListRule`, `CveGateRule`, `LicenseGateRule` et
  `FlagsRule`. Le garde-fou du verdict écrit en `ERROR` et refuse.
- **Pour répondre à « pourquoi est-ce refusé » sans le reproduire :**
  - `batlehub-cli why <registry>:<name>@<version>` (voir
    [la CLI](/fr/use/cli#commands-security)) ;
  - le panneau de verdict sur la page du paquet ;
  - la page admin *Access Check*, qui exécute la même chaîne qu'un vrai
    téléchargement, surcharges de namespace et exemptions comprises.

---

## Les avertissements de configuration {#warnings}

Un rechargement signale ceux-ci sans refuser le fichier :

| Code | Signification |
| --- | --- |
| `security.rule-settings-ignored` | Un réglage que le verdict ne lit pas : voir [Deux régimes](#regimes) |
| `security.unprotected` | `mode = "warn"` sans `required_scanners` : rien ne peut retenir une version |
| `security.timestamp-hold-unavailable` | `hold_missing_timestamp = true` sur un type sans date de publication : chaque version est retenue |
| `security.enrichment-required` | Une entrée de `required_scanners` qui ne produit jamais de constats propres (`mlab`) |
| `license-gate.sbom-disabled`, `license-gate.no-extractor`, `license-gate.denies-everything` | `license_gate` sans rien à lire |
| `require-signed-release.unsigned-publishes` | Le garde-fou sur un registre qui accepte des publications non signées |
| `flag-source.can-hard-block` | Une source de flags qui peut refuser des téléchargements sans seuil |

---

## Recettes {#recipes}

**Un délai de carence, sans le scan worker.**

```toml
[[registries.rules]]
kind         = "release_age_gate"
min_age_secs = 259200          # 3 days
bypass_roles = ["admin"]

[[registries.rules]]
kind         = "cve_gate"
min_severity = "high"
block        = true
```

**La même chose, avec le scan worker.** Ici, la retenue se lève d'elle-même
une fois que les scanners ont répondu.

```toml
[registries.security]
mode              = "block"
min_age_secs      = 259200
mature_age_secs   = 2592000    # 30 days: serve `warned` past this if a scanner is stuck
scanners          = ["osv", "postmortem"]
required_scanners = ["osv"]
max_severity      = "high"
```

**Les paquets de votre propre CI échappent au délai de carence.** Une
surcharge de namespace remplace un garde-fou et conserve les autres :

```toml
[[registries.namespaces]]
match = "@acme"

[[registries.namespaces.rules]]
kind         = "release_age_gate"
min_age_secs = 0
```

**Épingler une plage approuvée, et refuser une release défectueuse.**

```toml
[[registries.rules]]
kind  = "version_gate"
allow = [">=4.17.0, <5.0.0"]
block = ["4.17.20"]
```
