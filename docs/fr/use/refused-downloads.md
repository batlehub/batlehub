---
reference: true
sourcePath: use/refused-downloads.md
sourceHash: ffec81c460a3c5ec
---

# Pourquoi un téléchargement est refusé

Quand BatleHub refuse un téléchargement, il en donne la raison au gestionnaire
de paquets, mais la plupart des gestionnaires de paquets n'en affichent qu'une
partie. Cette page explique ce que signifie chaque refus, comment obtenir la
raison complète avec `batlehub-cli why`, et ce que vous, ou l'administrateur de
l'instance, pouvez faire dans chaque cas. La configuration de ces contrôles est
décrite sur la page de l'opérateur, [Contrôles de sécurité](/fr/guide/security-gates).

---

## Ce que vous voyez {#what-you-see}

Un refus revient sous l'une de trois formes.

**Un `403` qui contient une raison.** Le registre exécute ses contrôles pendant
la requête. Le message nomme le contrôle :

```text
npm error 403 Forbidden - GET https://hub.example.com/proxy/npm/left-pad/-/left-pad-1.3.1.tgz
npm error 403 release is too recent (age: 3600s, minimum: 259200s, 255600 seconds remaining in quarantine)
```

Les messages sont listés sous [Les refus à message simple](#plain-messages).

**Un `403` qui nomme un verdict.** Le registre a un profil de chaîne
d'approvisionnement : chaque version est analysée, et le résultat de l'analyse,
son **verdict**, décide :

```text
npm-public:left-pad@1.3.1 is quarantined (MIN_AGE_NOT_MET, available 2026-09-05T14:12:00Z). Run `batlehub-cli why npm-public:left-pad@1.3.1` for details.
```

Les mots en majuscules sont des **codes de motif**, chacun expliqué sous
[Les codes de motif](#codes). La même information figure dans les en-têtes de
réponse `X-BatleHub-Verdict`, `X-BatleHub-Reason` et `X-BatleHub-Available-At`,
que `curl -I` affiche même quand votre gestionnaire de paquets ne le fait pas.

**Un `404`, pour une version dont vous savez qu'elle existe.** La version est
retenue ou refusée par un verdict, et votre token n'a pas le droit de voir les
verdicts : il faut pour cela la permission `quarantine:read`, que les
utilisateurs connectés ont par défaut et les appelants anonymes non.
Connectez-vous, ou utilisez un token, puis réessayez : le `404` devient le
`403` ci-dessus. Les registres `goproxy` et `maven` répondent `403` au lieu de
`404` dans ce cas, parce que leurs clients traitent un `404` comme « essayer la
source suivante ».

---

## Demander la raison complète : `batlehub-cli why` {#batlehub-why}

```console
$ batlehub-cli why npm-public:left-pad@1.3.1
npm-public:left-pad@1.3.1
  state        quarantined
  reasons      MIN_AGE_NOT_MET
  docs         https://batlehub.dev/use/refused-downloads#min-age-not-met
  available    2026-09-05T14:12:00Z
  policy       npm-public/default
  evaluated    2026-09-02T14:12:03Z
  scanned      2026-09-02T14:12:01Z
  scanners     osv
  findings     none

Held until 2026-09-05T14:12:00Z. `batlehub-cli wait` waits for it.
```

- **`docs` :** un lien par code de motif, vers sa section de cette page.
- **`findings` :** le détail propre aux scanners, affiché quand votre token
  détient `findings:read` (les administrateurs, par défaut). Sinon, la ligne
  indique `withheld`.
- **Dans la console :** la page du paquet affiche le même verdict pour la
  version sélectionnée, et chaque code de motif renvoie ici.
- **`--rescan` :** met aussi une nouvelle analyse en file. Il exige
  `gates:exempt`, que personne ne détient par défaut : c'est donc en général
  votre administrateur qui le lance.

La commande est décrite en entier dans [la CLI](/fr/use/cli#commands-security).

**Attendre la levée d'une retenue** dans un script ou un pipeline :

```console
$ batlehub-cli wait npm-public:left-pad@1.3.1 --timeout 2h
```

Elle sort avec `0` dès que la version peut être servie, `1` quand attendre ne
sert à rien (la version est refusée, ou retenue sans date de fin, comme avec
`TIMESTAMP_MISSING`), et `2` quand le délai expire avant.

---

## Les quatre états {#states}

| État | Servi ? | Ce qui se passe ensuite |
| --- | --- | --- |
| `allowed` | oui | Rien. Chaque scanner requis a répondu sans rien trouver d'assez grave |
| `warned` | oui | Rien. Le téléchargement porte les motifs dans ses en-têtes, et votre gestionnaire de paquets l'installe |
| `quarantined` | non | **La retenue se lève d'elle-même** — à l'heure indiquée dans `available`, ou quand un scanner répond |
| `denied` | non | **Le refus persiste** jusqu'à ce qu'une nouvelle analyse lève le doute ou qu'un administrateur intervienne |

---

## Les codes de motif {#codes}

### Retenu pour l'instant {#held}

Ces codes retiennent une version pour un temps. Attendre est généralement la
solution.

#### `MIN_AGE_NOT_MET` {#min-age-not-met}

La version a été publiée trop récemment. Le registre attend un délai fixe après
chaque release (trois jours est courant) pour qu'une release compromise puisse
être repérée et retirée avant que quiconque ne l'installe. Le verdict porte
l'heure à laquelle elle devient disponible : attendez-la avec `batlehub-cli wait`,
ou épinglez la version précédente d'ici là.

#### `SCAN_PENDING` {#scan-pending}

Un scanner requis par le registre n'a pas encore répondu. C'est normal à la
première requête pour une version : c'est cette requête qui a mis l'analyse en
file. Réessayez dans une minute ou deux. Si cela dure, le scan worker est
peut-être arrêté : prévenez votre administrateur.

#### `SCANNER_ERROR` {#scanner-error}

Un scanner requis a échoué sur cette version, après ses nouvelles tentatives.
La retenue se lève quand une analyse réussit, et votre administrateur peut en
mettre une en file avec `batlehub-cli why --rescan`. Une version plus ancienne que
l'âge de maturité du registre est servie en `warned` malgré ce code, pour qu'un
scanner en panne ne bloque pas tout indéfiniment.

#### `SCANNER_UNSUPPORTED` {#scanner-unsupported}

Un scanner requis ne sait pas lire ce type d'artefact, par exemple un scanner
d'archives face à un paquet composé de plusieurs fichiers. Le registre choisit
si cela retient la version. Si c'est le cas, votre administrateur doit modifier
les scanners requis du registre : attendre n'y changera rien.

#### `TIMESTAMP_MISSING` {#timestamp-missing}

L'amont n'a pas indiqué quand la version a été publiée, donc le registre ne peut
pas savoir si elle est assez ancienne. La retenue n'a pas de date de fin : elle
se lève si l'amont se met à dater la version. Sinon, il faut votre
administrateur, qui peut exempter la version ou cesser de retenir les versions
non datées sur ce registre.

### Refusé à cause de ce qu'une analyse a trouvé {#found}

Un scanner a trouvé quelque chose au seuil de sévérité du registre ou au-dessus.
Sur un registre en mode `warn`, ces codes produisent `warned` à la place, et la
version est servie.

#### `VULNERABILITY` {#vulnerability}

Une vulnérabilité connue touche cette version. `batlehub-cli why` nomme l'avis de
sécurité si vous pouvez voir les constats. Passez à une version corrigée s'il
en existe une. Si la vulnérabilité ne peut pas toucher votre usage du paquet,
votre administrateur peut consigner ce jugement sous forme d'exemption, avec un
motif et une date de fin.

#### `MALWARE_SIGNAL` {#malware-signal}

Un scanner a trouvé du code qui se comporte comme un malware : exécution
obfusquée, appel réseau vers une adresse connue comme malveillante,
identifiants lus puis envoyés ailleurs, ou correspondance avec l'une des règles
propres à votre organisation. **Ne contournez pas celui-ci.** Prévenez votre
administrateur ou votre équipe sécurité, et utilisez une version qui n'en
porte pas.

#### `KNOWN_MALICIOUS` {#known-malicious}

Une base d'avis de sécurité répertorie cette version comme malveillante — un
avis OSV `MAL-` : un paquet publié pour voler des identifiants, miner, ou
ouvrir une porte dérobée. C'est toujours refusé, quels que soient le mode du
registre et son seuil de gravité. **Ne contournez pas celui-ci.** Retirez le
paquet de votre projet, et prévenez votre équipe sécurité s'il a déjà été
installé : ce qu'il a exécuté à l'installation a déjà eu lieu.

#### `INSTALL_HOOK` {#install-hook}

Le paquet exécute un script à son installation (un `preinstall` npm, un
`setup.py` Python). Beaucoup de paquets légitimes le font, et le registre
décide si cela refuse, avertit ou est ignoré. Si vous avez besoin de ce paquet,
demandez à votre administrateur de l'examiner.

#### `TYPOSQUAT_SUSPECT` {#typosquat-suspect}

Le nom ressemble de façon suspecte à celui d'un paquet populaire. Vérifiez
l'orthographe de ce que vous avez demandé : c'est la façon classique
d'installer un malware par erreur.

#### `INSTALL_HOOK_ADDED` {#install-hook-added}

Cette version ajoute un script d'installation que la version précédente
n'avait pas. C'est un signe courant de paquet détourné. Préférez la version
précédente jusqu'à ce que quelqu'un ait examiné le nouveau script.

#### `PUBLISHER_CHANGED` {#publisher-changed}

Cette version a été publiée par un autre compte que les précédentes. Cela peut
être un simple changement de mainteneur, ou une prise de contrôle. Préférez la
version précédente jusqu'à ce que le changement soit expliqué.

#### `REPOSITORY_MOVED` {#repository-moved}

Le dépôt source déclaré du paquet a changé dans cette version. Comme un nouvel
éditeur, c'est anodin aussi souvent que non. Cela mérite un coup d'œil avant
de faire confiance à la version.

#### `DORMANT_RELEASE` {#dormant-release}

Une nouvelle version est apparue après un long silence. Un paquet abandonné qui
publie soudain est un schéma connu de prise de contrôle. Vérifiez la release
avant de l'adopter.

#### `UNPUBLISHED_UPSTREAM` {#unpublished-upstream}

Le registre amont ne liste plus cette version : elle a été dépubliée ou
retirée, souvent parce qu'elle était mauvaise. Passez à une version que l'amont
publie encore.

### Signatures et provenance {#provenance}

#### `PROVENANCE_MISSING` {#provenance-missing}

La version n'a pas d'attestation de provenance, l'enregistrement signé du build
qui l'a produite. Ce code ne refuse que sur les registres qui exigent la
provenance, et avertit sinon.

#### `PROVENANCE_INVALID` {#provenance-invalid}

La version revendique une attestation de provenance que le journal de
transparence ne contient pas. C'est un signal plus fort qu'une attestation
absente. Prévenez votre administrateur.

#### `PROVENANCE_UNVERIFIABLE` {#provenance-unverifiable}

L'attestation n'a pas pu être vérifiée, par exemple parce que la forge n'a pas
répondu. Ce code ne fait jamais qu'avertir.

#### `PROVENANCE_REMOVED` {#provenance-removed}

Les versions précédentes avaient une attestation de provenance, celle-ci non.
Une chaîne de build qui a cessé de signer mérite une question avant de mettre à
jour.

#### `SIGNATURE_MISSING` {#signature-missing}

Le registre exige des releases signées, et celle-ci ne l'est pas. Utilisez une
version signée, ou demandez à l'éditeur de signer ses releases.

### La politique propre au registre {#policy}

#### `LICENSE_DENIED` {#license-denied}

La licence déclarée de la version ne fait pas partie de celles que votre
organisation autorise sur ce registre. C'est une décision de politique, pas un
défaut : adressez-vous au responsable de la politique de licences, et ne faites
pas entrer le paquet par un autre chemin.

#### `UNTRUSTED_PUBLISHER` {#untrusted-publisher}

Ce registre ne sert que les paquets d'une liste approuvée d'éditeurs, et
celui-ci n'y figure pas. Demandez à votre administrateur d'ajouter l'éditeur si
vous en avez besoin.

#### `BLOCK_LIST` {#block-list}

Un administrateur a bloqué cette version, et `batlehub-cli why` affiche son motif
si vous pouvez voir les constats. C'est toujours refusé, quel que soit le mode
du registre. Adressez-vous à l'administrateur, ou utilisez une autre version.

#### `SOC_VERDICT` {#soc-verdict}

Votre équipe sécurité, ou un flux auquel elle fait confiance, a signalé cette
version pour blocage. C'est toujours refusé. Utilisez une autre version, et
adressez-vous à l'équipe sécurité si vous pensez que le signalement est erroné.

#### `ADMIN_OVERRIDE` {#admin-override}

Ce n'est pas un refus. Un administrateur a exempté cette version d'une retenue
ou d'un refus : elle est donc servie en `warned`, avec ce code à côté des codes
d'origine. L'exemption a une date de fin.

### Forges Git {#forge}

Ces codes viennent des registres qui proxifient GitHub, GitLab ou Forgejo.

#### `MUTABLE_REF` {#mutable-ref}

Vous avez demandé une branche, et une branche bouge : la même requête peut
renvoyer un code différent demain. Demandez plutôt un tag ou un commit. Ce code
avertit par défaut.

#### `TAG_MOVED` {#tag-moved}

Le tag demandé pointe désormais vers un autre commit que lorsque cette instance
l'a vu pour la première fois. Un tag est censé être permanent : c'est donc
refusé par défaut. Cherchez pourquoi le tag a bougé avant de lui faire
confiance.

#### `ASSET_REPLACED` {#asset-replaced}

Un fichier de release a été remplacé sous le même nom : son contenu a changé
depuis que cette instance l'a mis en cache. Il est traité comme un tag déplacé.

#### `PINNED_REF_REQUIRED` {#pinned-ref-required}

Ce registre ne sert les fichiers bruts qu'à un tag ou un commit, jamais à une
branche. Utilisez une ref épinglée dans l'URL.

#### `RAW_SCRIPT` {#raw-script}

Vous avez récupéré un script sous forme de fichier brut depuis une forge.
Exécuter un script directement depuis une forge est le point de départ de
nombreuses attaques de la chaîne d'approvisionnement : le registre avertit donc
ou refuse. Vérifiez le script, et épinglez-le à un commit.

---

### Un code que ce serveur ne connaît pas {#unknown-code}

#### `UNRECOGNISED` {#unrecognised}

Le verdict de cette version a été écrit par une version du serveur plus
récente que celle qui vous a répondu, et porte une raison que celle-ci ne sait
pas lire. Elle refuse plutôt que de deviner. Cela n'arrive que pendant le
déploiement d'une mise à jour, ou après un retour arrière : réessayez dans
quelques minutes et, si cela persiste, prévenez votre administrateur — le
réplica qui a répondu est plus ancien que la base qu'il lit.

## Les refus à message simple {#plain-messages}

Ils viennent de registres sans profil de chaîne d'approvisionnement. Le message
est toute la réponse, et `batlehub-cli why` n'a rien à y ajouter.

| Le message commence par… | Signification | Que faire |
| --- | --- | --- |
| `no grant for 'releases:read'` | Votre token ne peut pas lire ce registre, ou ce paquet | Demandez un accès, ou vérifiez que vous utilisez le bon token |
| `release is too recent` | Un délai de carence après chaque release | Attendez les secondes restantes, ou épinglez la version précédente |
| `release timestamp is missing` | L'amont ne l'a pas datée, et ce registre exige une date | Adressez-vous à votre administrateur |
| `requests for the 'latest' version tag are not allowed` | Ce registre exige une version explicite | Épinglez une version dans votre manifeste ou votre lockfile |
| `version '…' is blocked by policy` | Cette version précise est connue comme mauvaise | Utilisez une autre version |
| `version '…' is not in the approved allowlist` | Seules les versions approuvées sont servies ici | Utilisez-en une de la plage approuvée, ou demandez son approbation |
| `publisher '…' is not in the trusted_publisher allowlist` | Seuls les éditeurs approuvés sont servis | Demandez l'ajout de l'éditeur |
| `cannot verify the publisher` | Le contrôle d'éditeur est actif, et ce type de registre ne sait pas indiquer d'éditeur | La configuration de votre administrateur doit être corrigée |
| `this release is not signed` | Seules les releases signées sont servies | Utilisez une version signée |
| `blocked: known … vulnerability` | Une vulnérabilité enregistrée au seuil ou au-dessus | Mettez à jour, ou demandez une exemption |
| `blocked: licence '…'` / `blocked: no declared licence` | La politique de licences | Adressez-vous au responsable de la politique de licences |
| `blocked: … flagged by …` | Votre équipe sécurité l'a signalée | Utilisez une autre version |
| tout autre texte | Très probablement un blocage par un administrateur, dans ses propres mots | Adressez-vous à l'administrateur |

Chaque refus est consigné dans le journal d'audit avec votre identité : inutile
de le reproduire. Envoyez l'heure et le paquet à votre administrateur, qui
pourra le retrouver.
