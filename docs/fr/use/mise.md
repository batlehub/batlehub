---
title: mise
sourcePath: use/mise.md
sourceHash: b17491d0e1e62915
---

# Faire pointer mise vers BatleHub

[mise](https://mise.jdx.dev) installe des outils depuis l'endroit où chaque
backend les publie : releases GitHub, npm, crates.io, `nodejs.org/dist`, l'hôte
d'un éditeur. Chacun de ces cas est un téléchargement direct, et aucun ne passe
par un gestionnaire de paquets qu'un seul réglage suffirait à faire pointer vers
un proxy. `[settings.url_replacements]` est la façon de dire à mise de router
tout cela ici.

Cette page est le seul endroit qui en parle, en réseau ouvert comme en réseau
coupé.

## 1. Le cas connecté

Demandez à la CLI ce dont votre projet a besoin, elle imprime le bloc :

```bash
batlehub registry suggest --mise
```

Elle lit d'abord `mise.lock` — le verrou enregistre l'URL exacte de chaque outil,
plateforme par plateforme, de sorte que la réponse est dérivée de ce que le projet
télécharge réellement plutôt que devinée à partir de noms d'outils. Ajoutez
`--mise-commented` pour obtenir un bloc que vous pouvez versionner dans un
`mise.toml` partagé sans l'activer pour tout le monde d'un coup.

L'ordre compte, et le générateur s'en occupe : mise applique les règles dans
l'ordre de déclaration, la première qui correspond gagne, et la comparaison
**s'arrête au premier succès**. Il n'y a pas de repli — la règle qui a
correspondu est la seule URL essayée.

::: tip Identifiants
Pour un registre qui exige une authentification, ajoutez une entrée `~/.netrc`
pour l'hôte du proxy plutôt que d'embarquer un token dans l'URL. mise la lit, et
le token reste hors de l'historique du shell, des logs de CI et de la sortie de
`mise doctor`.
:::

## 2. Ce qu'exige une coupure réseau

Un hôte sans route vers l'extérieur ne peut installer que ce dont mise a déjà été
informé, et l'échec qu'il vous donne aujourd'hui est un délai de connexion
dépassé, sans nom attaché. Trois choses changent cela
([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate)) :

1. **Le verrou devient un plan.** `mise.lock` nomme déjà chaque téléchargement ;
   `mise plan` résout chacun d'eux sur le chemin qu'il emprunte à travers ce
   serveur, et signale les deux choses que vous ne pouvez pas connaître
   aujourd'hui — les outils qui ne fonctionneront pas du tout, et les hôtes que
   rien ne réplique.
2. **Le serveur refuse de composer vers l'extérieur.** Avec `[air_gap]` actif, un
   défaut est un `503` immédiat qui nomme la coordonnée, pas un délai dépassé, et
   il est enregistré — de sorte que la liste de ce dont le prochain lot a besoin
   est produite par le parc plutôt que devinée.
3. **La vérification passe du côté connecté.** Les contrôles cosign et
   d'attestation de mise ne peuvent pas tourner hors ligne ; ils tournent donc
   une fois là où ils le peuvent, et ce qu'ils ont trouvé voyage avec le contenu.

### 2.1 Planifier

```bash
batlehub mise plan --lock mise.lock --platform linux-x64 -o mise-plan.json
```

```text
28 tool(s) · 41 download(s) · 6 registries · 3 host(s) with no mirror configured
  no mirror: binaries.sonarsource.com
  unsupported: asdf:mise-plugins/mise-postgres: a git-fetched plugin backend; mise clones it, and there is no HTTP path through BatleHub
```

La planification est hors ligne : elle lit le verrou et la liste des registres du
serveur, et ne résout rien par le réseau.

`--platform` vaut par défaut **la plateforme de la machine sur laquelle vous
lancez la commande**, ce qui est le cas courant et garde un lot à la taille du
parc qui l'a demandé. Nommez-en plusieurs, séparées par des virgules, pour un
parc hétérogène, ou `--platform all` pour toutes les plateformes que le verrou
enregistre. Si le verrou n'a rien pour cet hôte, la commande le dit et planifie
tout, plutôt que d'écrire un lot vide.

Un verrou enregistre deux adresses pour l'asset d'une release de forge — l'URL de
téléchargement et celle de l'API — et les deux sont planifiées. C'est un même
artefact atteint de deux manières, elles partagent une empreinte, et le lot n'en
porte qu'une copie : la seconde adresse coûte une ligne de manifeste et rien
d'autre. Ce n'est pas optionnel, parce que les backends `aqua:` et `github:` vont
d'ordinaire chercher celle de l'API.

Ajoutez `--include-mise` pour embarquer mise lui-même : le lot contient alors
toujours le binaire qui lira le prochain plan. La version est celle du mise
présent dans votre PATH, sauf indication contraire par `--mise-version`.

**Lisez `no mirror` avant tout le reste.** C'est la réponse à « ma table de
réécriture est-elle complète ? », et il est moins coûteux d'y répondre maintenant
qu'au moment de l'installation, sur une machine sans réseau.

### 2.2 Amorcer

```bash
batlehub mise seed --plan mise-plan.json --verify
```

Chaque entrée planifiée est récupérée *à travers* BatleHub — récupérer, c'est
préchauffer — et l'empreinte de ce que le serveur a servi est comparée à celle du
verrou. Le code de sortie est non nul si une entrée manque ou si une empreinte
diverge : c'est donc utilisable comme porte de CI du côté connecté.

`--verify` signale en plus ce que la couche de chaîne d'approvisionnement
([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)) a dit de chaque
entrée, et fait échouer l'exécution sur un verdict de refus. Le niveau
d'exigence, c'est le `[registries.security]` du registre, pas un drapeau ici : un
registre qui exige la provenance refuse un artefact non signé, quel que soit le
demandeur.

### 2.3 Exporter, transporter, importer

```bash
# côté connecté
batlehub mise export --plan mise-plan.json --sign-key ./estate.key -o estate.bhub

# côté coupé du réseau
batlehub mise import estate.bhub
```

Un lot est une archive tar de trois choses :

```text
manifest.json     le plan, plus ce qui a été vérifié de chaque entrée
blobs/<sha256>    adressé par le contenu, donc des octets identiques ne partent qu'une fois
manifest.sig      une signature ed25519 détachée sur manifest.json
```

Les blobs sont nommés par leur propre empreinte : un blob dont le hachage ne
correspond pas à son nom est donc rejeté à l'entrée sans référence à aucune
signature — et la signature est vérifiée **avant qu'un seul blob ne soit lu**.
Une importation est idempotente par identifiant de lot : transporter deux fois le
même lot n'écrit qu'une fois.

Ed25519 parce que c'est la seule signature que BatleHub vérifie dans son propre
processus. Les signatures cosign sont de l'ECDSA sur une identité x509, que ce
serveur ne sait pas contrôler : il enregistre donc le **verdict** plutôt que la
signature, et l'étiquette comme une preuve d'un contrôle passé — jamais comme un
contrôle en direct.

## 3. Activer la coupure réseau

```toml
[air_gap]
enabled             = true
bundle_trusted_keys = ["3b1f…"]   # clés publiques ed25519 en hexadécimal acceptées à l'import
record_misses       = true
miss_retention_days = 90
```

Voir [configuration](/fr/guide/configuration#air-gap) pour la référence
complète. Deux refus au chargement méritent d'être connus avant d'essayer : un
proxy sortant à côté de `enabled = true` est une contradiction, et
`enabled = true` sans clé de confiance accepterait n'importe quel lot.

### 3.1 La règle attrape-tout

```bash
batlehub registry suggest --mise --mise-catch-all
```

ajoute une dernière règle qui envoie tout ce qu'aucune autre règle n'a attrapé
vers `/_air-gap/unmirrored/{host}/…`. Cette route **ne récupère rien** : elle
répond `501`, nomme l'hôte et l'enregistre. Un hôte non répliqué devient une
ligne dans la console au lieu d'un délai de connexion dépassé sur un poste de
travail.

Elle doit être en dernier, ce pourquoi le générateur l'ajoute lui-même plutôt que
de vous le demander — et elle est précédée d'une règle d'identité pour l'hôte de
ce serveur, parce que l'attrape-tout correspond à *toutes* les URL https et
enverrait sinon dans le puits `501` un backend que vous aviez déjà fait pointer
vers BatleHub.

## 4. Lire ce qui manque

```bash
batlehub admin air-gap-missing
batlehub admin bundles
```

ou la page **Coupure réseau** de la console, sous Exploitation. Le journal des
défauts a une ligne par `(registre, clé)` avec un compteur — mise réessaie, et le
journal ne doit pas grossir avec les tentatives — trié du plus demandé au moins
demandé, ce qui est l'ordre dans lequel construire le prochain plan. Deux
colonnes disent ce que la ligne signifie pour ce plan : **Demandée**, la version
demandée par le client quand sa requête en nommait une (c'est le cas de la
release par tag de mise, donc `mise install gh@2.61.0` contre une instance qui
détient 2.60.0 se lit *demandée v2.61.0, détenue v2.60.0*), et **Détenue**, ce
que l'instance avait de cet outil.

Une coordonnée qu'un administrateur a **bloquée** n'y apparaît jamais. Un paquet
bloqué n'est pas un trou dans le miroir, et le proposer pour le prochain lot
reviendrait à défaire le blocage par accident.

## 5. Ce qui ne fonctionnera pas

- **Les backends de plugins `asdf:` et `vfox:`.** mise les récupère par git, et
  il n'y a pas de chemin HTTP à travers un proxy. `mise plan` les liste sous
  `unsupported`, en nommant le backend.
- **Un hôte pour lequel vous n'avez pas de registre.** Le plan le dit ; ajoutez
  un registre ou renoncez à l'outil.
- **La vérification de signature en direct du côté coupé.** Elle ne peut pas y
  tourner : `github_attestations`, `github.slsa`, `aqua.cosign`, `aqua.slsa` et
  `aqua.minisign` atteignent chacun Sigstore ou la forge avant d'installer, et un
  journal de transparence n'est pas cachable. Désactivez les cinq ; la
  [procédure](/fr/operations/air-gap) donne le bloc. La somme de contrôle de
  `mise.lock` est toujours vérifiée localement — elle n'a besoin que des octets —
  et ce qui remplace la signature, c'est le verdict de BatleHub, enregistré quand
  il pouvait l'être et transporté avec eux.
- **Une version que le lot ne portait pas.** Un lot porte des artefacts et
  l'entrée qui les trouve, pas les documents propres à la forge. Depuis la
  [RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap), l'instance coupée
  *compose* le document de release à partir des assets qu'elle détient : ainsi
  `mise install some-tool@1.2.3` fonctionne sans verrou quand l'asset de 1.2.3
  était dans le lot — mise lit la release par tag, trouve l'asset, installe. Pour
  une version absente du lot, la release est un `503`, enregistré sous le type
  `document` avec le tag en **Demandée**, et le repli de mise sur la liste des
  releases ne trouve que les versions détenues. Le verrou reste la nomenclature :
  une installation **depuis `mise.lock`** tente exactement une URL, l'asset que
  le verrou nomme, et `mise plan` lit le verrou plutôt que la configuration
  parce que c'est cette liste-là que le lot doit porter. Gardez
  `lockfile = true` et versionnez le verrou.

## Voir aussi

- [Vue d'ensemble des registres](/fr/registries/) · [Configuration](/fr/guide/configuration#air-gap)
- [La procédure de coupure réseau](/fr/operations/air-gap) — construire, transporter, importer, réconcilier
- [RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate) — pourquoi c'est fait ainsi
