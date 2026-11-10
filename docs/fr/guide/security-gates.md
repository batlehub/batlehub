---
reference: true
sourcePath: guide/security-gates.md
sourceHash: cff271fe09b853d8
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
`cve_gate.min_severity`, `cve_gate.unknown_severity`, et toute surcharge de
namespace de ces quatre
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
min_severity     = "high"   # unknown | low | medium | high | critical
unknown_severity = "high"   # how an unrated advisory is judged; the default is "high"
block            = true     # the default is false: report only, never refuse
bypass_roles     = ["admin"]
```

- **Lit :** les vulnérabilités que la réanalyse SBOM a enregistrées pour la
  version (`[vulnerability_scan]`, voir [SBOM](/fr/guide/sbom)). La réanalyse
  tourne hors bande : **rien n'est enregistré au premier téléchargement**
  d'une version, et ce téléchargement est autorisé.
- **Par défaut `block = false` :** les constats apparaissent dans la console et
  le garde-fou ne refuse rien.
- **Erreur du store :** échoue ouvert, avec une ligne de log `WARN`.
- **Les avis non notés** sont jugés à `unknown_severity`, `high` par défaut :
  avec `block = true`, un avis que personne n'a noté est donc refusé comme un
  avis `high`. `"unknown"` rétablit l'ancien comportement, où un avis non noté
  ne refusait jamais. Voir [Les constats non notés](#unknown-severity).
- **Les avis `MAL-`** sont refusés quoi que dise `min_severity`, tant que
  `block = true`. `bypass_roles` les laisse quand même passer : la garantie
  qu'un paquet connu comme malveillant n'est jamais servi appartient au profil
  `[security]`, pas à cette règle.
- **Refus :** `blocked: known <severity> vulnerability <id> (minimum gated severity: …)`,
  `blocked: unrated vulnerability GO-… (judged as high by unknown_severity; minimum gated severity: high)`,
  ou `blocked: known malicious package MAL-…`.
- **Il fixe aussi le seuil du garde-fou des [flags](#flags).** Avec
  `block = false`, les flags `gate` cessent eux aussi de refuser.
- **Sous `[security]` :** les vulnérabilités enregistrées deviennent des
  constats `VULNERABILITY`. `min_severity`, `unknown_severity`, `block` et
  `bypass_roles` ne sont pas lus : ce sont le `max_severity` et le
  `unknown_severity` du profil qui décident. Dans ce régime, le scanner `osv` répond d'ailleurs généralement le
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

Un flag `gate` poussé avec la gravité `unknown` est jugé au `unknown_severity`
du `cve_gate`, `high` si aucun `cve_gate` n'est déclaré. Il n'y a pas de
contournement. Une erreur du store échoue ouvert sur le chemin
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

## Comment un constat est noté {#severity}

Sur un registre `[security]`, chaque constat porte une gravité, et
`max_severity` décide de son effet : à ce niveau ou au-dessus, `denied` en mode
`block` et `warned` en mode `warn`. Cette section dit d'où vient cette gravité,
et ce qui se passe quand personne n'en a donné.

### Les constats non notés : `unknown_severity` {#unknown-severity}

Un constat dont la source ne déclare aucune gravité est stocké en `unknown`.
`unknown_severity` dit comment il est jugé face à `max_severity` :

```toml
[registries.security]
mode             = "block"
max_severity     = "high"
unknown_severity = "high"   # unknown | low | medium | high | critical; the default is "high"
```

- **Absent, il vaut `"high"`.** Sous le `max_severity = "high"` par défaut,
  une version dont le seul avis n'est pas noté est refusée : un avis que
  personne n'a encore noté n'est pas une raison de servir.
- **`"unknown"` rétablit l'ancien comportement**, où un constat non noté
  n'agissait jamais. Écrivez-le explicitement, pour que le choix se voie dans
  le fichier.
- **La gravité stockée n'est pas réécrite.** Le constat garde `unknown`, et le
  verdict enregistre le substitut à côté, sous `judged_as`. `batlehub-cli why`
  et le panneau de verdict de la page du paquet montrent les deux : gravité
  inconnue, jugée high par `unknown_severity`. Comme tout constat, c'est
  visible des appelants qui détiennent `findings:read`.
- **Il est recalculé à chaque lecture.** Modifier `unknown_severity` prend
  effet au téléchargement suivant, sans réanalyse. L'exception est une
  [escalade](#escalation) qu'un constat non noté a déjà déclenchée : celle-ci a
  écrit sa nouvelle gravité dans le constat stocké, et elle reste.
- **Il est appliqué partout où une gravité rencontre un seuil de
  l'opérateur :** `max_severity`, le `from` d'une escalade, et un
  [flag](#flags) poussé.
- Réglé sous `max_severity` sur un registre `block`, il lève
  `security.unknown-served` au chargement : les avis non notés sont alors
  servis. Le `"high"` par défaut sous `max_severity = "critical"` ne lève rien,
  puisque personne ne l'a choisi.

Les registres sans profil règlent la même clé sur leur
[`cve_gate`](#cve-gate), avec le même défaut.

### Les paquets connus comme malveillants : `MAL-` {#known-malicious}

Un avis OSV dont l'identifiant ou l'un des alias commence par `MAL-` n'est pas
une vulnérabilité dotée d'une gravité. C'est la déclaration d'une base que le
paquet est un malware, et le constat vaut `KNOWN_MALICIOUS` :

- **Toujours refusé**, sous `mode = "warn"` aussi, quoi que disent
  `max_severity` et `unknown_severity`, comme `BLOCK_LIST` et `SOC_VERDICT`.
- **Rien ne l'abaisse.** Il est décidé en premier, à partir de l'identifiant
  et de chaque alias, avant toutes les règles de notation ci-dessous.
- **Les versions en cache sont couvertes sans réanalyse.** Un constat stocké
  avant la mise à jour et qui nomme un avis `MAL-` est reconnu à la lecture :
  la version est refusée à son prochain téléchargement.
- **La seule issue** est une exemption par version, posée par un
  administrateur sur le garde-fou `security_verdict`
  (`batlehub-cli exemption add … --gate security_verdict`). Elle sert la
  version en `warned` avec `ADMIN_OVERRIDE`, visible dans les en-têtes de
  réponse et le journal d'audit. Elle existe pour un faux signalement `MAL-`.

Sur un registre sans profil, `cve_gate` ne refuse un avis `MAL-` que tant que
`block = true`, et un appelant de `bypass_roles` passe quand même.

### Ce que rapporte chaque scanner {#scanner-severity}

Un scanner qui déclare une gravité n'est jamais contredit. Quand sa sortie
n'en déclare aucune, le constat prend le défaut de son type :

| Type de constat | Quand le scanner ne déclare aucune gravité |
| --- | --- |
| `MALWARE_SIGNAL`, `INSTALL_HOOK` | `high` |
| `TYPOSQUAT_SUSPECT`, les codes de transition | `medium` |
| `VULNERABILITY`, tous les autres types | `unknown`, puis jugé par `unknown_severity` |

Scanner par scanner :

| Scanner | D'où vient la gravité |
| --- | --- |
| `osv` | Un identifiant ou un alias `MAL-` : `KNOWN_MALICIOUS`, `critical`. Sinon, dans l'ordre : le `database_specific.severity` de l'avis lui-même (le `MODERATE` de GitHub vaut `medium`) ; à défaut, la plus haute de ces notes parmi ses *jumeaux*, les avis qu'il nomme comme alias ou qui le nomment ; à défaut, le plus haut score de base CVSS calculé à partir des vecteurs `severity[]` (v2, v3, v4) ou des nombres bruts de l'avis et de ses jumeaux, un score de `0.0` valant `low`. Un avis `RUSTSEC-` marqué informatif (`unsound`, `unmaintained`, `notice`) vaut `low`. Un avis `withdrawn` est ignoré. Un avis sans rien de tout cela vaut `unknown` |
| `trivy` | Le `Severity` de Trivy. Son `UNKNOWN` vaut `unknown`, jugé par `unknown_severity` |
| `postmortem` | La `severity` du constat. Sans elle : signaux de malware et hooks d'installation `high`, typosquats `medium`. Les transitions de l'historique valent `medium` ; une version absente de l'historique des releases vaut `high` |
| `socket` | La `severity` de l'alerte. Sans elle, le défaut du type de l'alerte |
| `guarddog` | Par famille de règles : règles d'installation et d'exécution `high`, exfiltration et stéganographie `high`, typosquat `medium`, toute autre règle `medium` |
| `yara` | La méta `severity` de la règle, `high` si elle n'en a pas |
| `sigstore` | `PROVENANCE_MISSING` `high` ; `PROVENANCE_INVALID` `high`, `critical` quand une entrée de journal citée n'existe pas |
| `mlab` | Ne crée rien. Élève en `critical` une vulnérabilité listée au CISA KEV |

Avant cette version, postmortem et socket notaient `medium` un signal de
malware sans gravité déclarée, et aucun réglage ne le rétablit : un signal de
malware non noté est précisément ce à quoi sert le seuil.

### Les registres Go {#go}

Un registre `goproxy` est celui où les avis non notés sont les plus fréquents,
et donc celui où `unknown_severity` est le plus souvent abaissé. La base de
vulnérabilités Go ne note rien : un avis `GO-` ne prend une note que d'un
jumeau, généralement un avis GitHub, et beaucoup n'en ont pas.
`golang.org/x/net@0.22.0` porte 17 avis `GO-` sans jumeau noté : sous les
défauts, il est donc refusé.

Le choix, registre par registre :

```toml
# Keep the default, refuse what nobody rated, and excuse a version at a time
# with a security_verdict exemption.

# Or serve unrated Go advisories, and accept the load-time warning
# security.unknown-served that says so.
[registries.security]
max_severity     = "high"
unknown_severity = "medium"
```

Les avis `MAL-` sont refusés dans les deux cas.

### Élever une combinaison : l'escalade {#escalation}

`[scanners.<name>.escalation]` élève les constats d'un scanner quand assez
d'entre eux tombent sur une même version. Rien n'est réglé par défaut. Un
usage documenté est l'historique de postmortem sur npm : un changement
d'éditeur seul est courant, mais un changement d'éditeur dans la release même
qui ajoute un hook d'installation est la façon habituelle dont un compte
détourné livre sa charge.

```toml
[scanners.postmortem.escalation]
kinds = ["transition"]   # publisher_changed, install_hook_added, repository_moved, …
count = 2                # two transitions at the same version…
from  = "medium"
to    = "high"           # …are both raised to high, which max_severity = "high" refuses
```

L'escalade compte des types de constats, pas des codes : deux transitions
quelconques sur une même version suffisent, `repository_moved` avec
`dormant_release` compris. Mesurez la fréquence sur votre propre cache avant
de l'activer. Un constat escaladé est stocké à sa nouvelle gravité.

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

## Lire le statut d'analyse {#scan-status}

Sur un registre `[security]`, le catalogue, la liste d'administration des
paquets et chaque version d'une page de paquet portent un **statut d'analyse**
à côté du badge d'état existant. Une seule fonction, côté serveur, le dérive ;
la première ligne qui s'applique l'emporte :

| Statut | Quand |
| --- | --- |
| *(aucun)* | le registre n'a pas de `[registries.security]` |
| `analyse en cours` | une tâche ouverte est tenue par un worker dont le bail court encore |
| `en file` | une tâche ouverte que personne ne tient : jamais louée, bail expiré, ou différée pendant une panne de la base d'avis |
| `non analysé` | aucun verdict n'a été enregistré pour la version |
| `analyse échouée` | la dernière tâche close a épuisé ses tentatives, ou le verdict porte `SCANNER_ERROR` |
| `en file` | le verdict porte `SCAN_PENDING` |
| `analysé · <verdict>` | sinon, avec `autorisé`, `averti`, `en quarantaine` ou `refusé` |

**Qui le voit** suit les verbes qui décident déjà qui peut apprendre qu'une
version est retenue :

- sans `quarantine:read`, rien : la ligne se lit exactement comme sur un
  registre qui n'analyse pas, si bien qu'un visiteur anonyme ne peut ni
  distinguer les deux ni apprendre qu'une version est retenue ;
- avec `quarantine:read` (les rôles `user` et `admin` par défaut), le statut et
  le verdict ;
- avec `findings:read` en plus (`admin` par défaut), l'heure de la dernière
  analyse, les tentatives et la dernière erreur du worker, dont toute URL est
  privée de ses identifiants et de sa requête, coupée à 200 caractères.

Une liste en décide par registre, d'après les permissions configurées, et
**ignore un `dry_run` actif** : pendant un essai à blanc, une liste peut en
montrer moins que le point d'accès du verdict d'une version, jamais plus.

**Une liste montre le verdict tel qu'il a été enregistré.**
`GET /api/v1/verdicts/…` le rejuge selon la politique et l'horloge du moment ;
les listes lisent la ligne stockée et ne déclenchent jamais d'analyse. Les deux
peuvent différer pendant les quelques minutes qui suivent la levée d'une
fenêtre d'âge ou un changement de politique.

Quand le statut n'a pas pu être lu en 500 ms, la liste est servie sans lui et
porte `scan_status_unavailable: true` ; la console affiche alors « — » à la
place de chaque badge.

**L'endpoint de verdict porte le même état**, dans `scan_status`, et pour un
détenteur de `system:read` un bloc `next_rescan` : quand le verdict sera
réanalysé (`last_scanned_at` plus `[registries.security.rescan]
interval_secs` ; absent sans intervalle, et une date passée signifie « dû ») et
quand la revérification SBOM du registre aura lieu (sa dernière exécution plus
`[vulnerability_scan] interval_secs` ; absente quand la revérification est
désactivée ou n'a jamais couvert le registre). L'onglet Sécurité de la page du
paquet affiche les deux sous *Prochaine réanalyse*.

### La page Scans {#scans-page}

*Admin → Sécurité et accès → Scans* suit la file d'analyse
(`GET /api/v1/admin/scans`) :

- **File d'attente** : chaque tâche ouverte, celles qu'un worker tient en
  premier, avec le déclencheur, l'heure de mise en file, le worker et les
  tentatives. Une tâche différée indique quand son report a commencé.
- **Échecs** : chaque version dont la dernière tâche d'analyse a épuisé ses
  tentatives et qui n'a aucune tâche ouverte, avec la dernière erreur du
  worker. [Relancer l'analyse] crée une nouvelle tâche
  (`POST /api/v1/verdicts/…/rescan`) et la ligne passe dans la file.
- **Verdicts par état** : les verdicts d'un registre, depuis
  `GET /api/v1/admin/verdicts`.

Ouvrir la page demande `system:read`. Les lignes ne viennent ensuite que des
registres où vous détenez aussi `quarantine:read`, et la dernière erreur
seulement là où vous détenez `findings:read` : `system:read` seul ne nomme
aucune version retenue. [Relancer l'analyse] apparaît là où les droits
configurés vous donnent `gates:exempt` sur le registre — personne ne le détient
par défaut — et l'endpoint de relance le vérifie à nouveau. `per_page` vaut au
plus 200.

Une tâche close avant cette version ne porte pas de marqueur d'échec : un échec
antérieur à la mise à jour n'apparaît donc pas sous Échecs ; son verdict
affiche toujours `SCANNER_ERROR` dans *Verdicts par état*.

### La page Constats {#findings-page}

*Admin → Sécurité et accès → Constats* liste ce que les scanners ont trouvé,
tous registres confondus (`GET /api/v1/admin/findings`). Chaque ligne nomme la
version, la sévérité, le scanner, l'identifiant de l'avis et l'état du verdict
que le constat a produit. Les filtres portent sur le registre, une sévérité
minimale, le scanner, l'état du verdict et un préfixe d'identifiant d'avis (pris
littéralement, 128 caractères au plus). *Par référence* regroupe les versions
de chaque avis, pour répondre à « quelles versions cette CVE touche-t-elle ? ».
[Exempter…] lève le verdict d'une version retenue : il demande un motif et une
échéance, et écrit une exemption `security_verdict`
([Garde-fous par namespace et exemptions](#namespaces)).

Les constats nomment des avis qui peuvent être sous embargo : la liste demande
donc `findings:read`, vérifié registre par registre. Vous voyez les registres où
vous le détenez, et la page est refusée si vous ne le détenez nulle part.
Nommer un autre registre dans le filtre renvoie une page vide, pas un refus. Un
namespace scellé qui vous exclut retire ses lignes, comme il le fait pour le
verdict de la version. [Exempter…] apparaît là où les droits configurés vous
donnent `gates:exempt` et où le verdict retient la version ; l'endpoint
d'exemption le vérifie à nouveau. `per_page` vaut au plus 200 ; à cause des
namespaces scellés, une page peut contenir moins de lignes que demandé.

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
| `security.unknown-served` | `mode = "block"` avec `unknown_severity` réglé sous `max_severity` (`"unknown"` compris) : les avis non notés sont servis. Les avis `MAL-` restent refusés. Voir [Les constats non notés](#unknown-severity) |
| `security.scanner-error-open` | `mode = "block"` avec `scanner_error = "warn"` ou `"ignore"` : un scanner qui ne répond pas — base d'avis indisponible ou qui limite le débit comprise — laisse servir la version |
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
