---
title: Procédure de coupure réseau
sourcePath: operations/air-gap.md
sourceHash: a2cdaaba7ad0ce5a
---

# Procédure de coupure réseau

Pour le parc dont les postes de travail n'ont aucune route vers l'extérieur.
Lisez d'abord [Faire pointer mise vers BatleHub](/fr/use/mise) — cette page est
la boucle d'exploitation, pas la conception.

::: warning Des repères, pas un engagement
C'est un modèle pour la procédure que *vous* écrivez. La cadence, les
approbations et les supports ci-dessous sont des exemples ; remplacez-les par
ceux de votre organisation.
:::

---

## La boucle

```text
côté connecté                           côté coupé du réseau
──────────────                          ─────────────────
1. planifier ← le verrou du projet
2. amorcer   ← récupérer + vérifier
3. exporter  ← lot signé
                        ══ transport ══→   4. importer
                                        4b. listings composés depuis le détenu
                                        5. lire ce qui a été demandé, et ce qui était détenu
                                        └──────── alimente l'étape 1 ────────┘
```

La boucle se referme sur elle-même. Ce que le côté coupé n'a pas su servir est ce
que le plan suivant emporte, et personne n'a à deviner. L'étape 4b ne demande
aucune action : une instance coupée du réseau répond à un listing qu'elle ne
détient pas — le packument que lit `npm install`, la page simple que lit `pip`,
la release qu'un `mise install` figé demande, l'index sparse de cargo, le
`@v/list` de Go, `maven-metadata.xml`, l'index plat de NuGet — à partir des
versions qu'elle *détient*, marqué `X-BatleHub-Listing: synthesised`
([RFC 0008-bis](/rfc/0008-bis-listings-across-the-gap)). Une chaîne de version se
résout donc hors ligne, et une version que l'instance ne détient pas est une
version que le listing ne nomme jamais : le client s'arrête donc de lui-même
plutôt que de réessayer sur un `503`. `synthesise_listings = false` dans
`[air_gap]` rétablit le refus.

---

## 1. Construire le plan

Sur une machine capable d'atteindre à la fois la source du projet et un BatleHub
**connecté** :

```bash
batlehub mise plan --lock mise.lock --platform linux-x64,darwin-arm64 \
  --include-mise -o mise-plan.json
```

`--platform` vaut par défaut celle de cette machine ; nommez celles que le parc
fait réellement tourner, ou passez `all`. `--include-mise` ajoute la release de
mise elle-même, de sorte que le parc puisse mettre à jour l'outil qui lira le
prochain plan sans un second passage de la coupure.

Avant d'aller plus loin, lisez les deux listes qu'elle imprime :

| Ligne | Quoi faire |
| --- | --- |
| `no mirror: <host>` | Ajoutez un registre pour cet hôte, ou acceptez que l'outil qui en a besoin ne s'installera pas. |
| `unsupported: <tool>` | Un backend récupéré par git. Il n'y a pas de chemin HTTP ; l'outil doit être remplacé ou installé autrement. |

Un plan qui n'a ni l'une ni l'autre est un plan que le lot peut satisfaire
entièrement.

Un plan est une liste de chemins de proxy, et quelques types en demandent
plusieurs par version. Un provider Terraform en demande **trois** : l'archive
(`/v1/providers/{ns}/{type}/{v}/artifact/{os}/{arch}`), la liste de sommes de
contrôle (`…/{v}/shasums`) et sa signature (`…/{v}/shasums.sig`) —
`terraform init` vérifie l'archive contre elles et refuse sans elles. L'export
lit le document de téléchargement du provider sur l'instance connectée et porte
ses clés de signature sur le manifeste, de sorte que l'instance coupée puisse
composer ce document sans rien signer ; il imprime aussi une note pour toute
archive de provider dont le plan ne nomme pas les deux fichiers annexes.

## 2. Amorcer et vérifier

```bash
batlehub mise seed --plan mise-plan.json --verify
```

Un code de sortie non nul signifie que le lot serait incomplet ou faux. N'en
construisez pas un depuis un amorçage échoué : l'échec est soit un miroir
manquant (corrigez l'étape 1), soit un désaccord d'empreinte, ce qui signifie que
le verrou et l'amont ne s'accordent plus et mérite d'être compris avant d'être
transporté à travers une coupure.

## 3. Exporter

```bash
batlehub mise export --plan mise-plan.json --sign-key ./estate.key -o estate.bhub
```

```text
estate.bhub · 41 entries · 38 blob(s) · signed
  signed by 3b1fa9c2…
  the disconnected instance must list that key in [air_gap].bundle_trusted_keys
```

La clé de signature est un fichier, pas une option : une clé sur une ligne de
commande est une clé dans l'historique du shell et dans toutes les listes de
processus de la machine. L'export imprime la moitié **publique**, qui est la
valeur dont l'instance coupée a besoin — copiez cette ligne plutôt que de la
dériver de la clé privée, ce qui est la façon dont une clé privée finit sur une
ligne de commande.

**Gardez la moitié publique dans la configuration de l'instance coupée** et la
moitié privée là où votre organisation garde son matériel de signature. Le côté
coupé n'accepte rien d'autre.

Moins de blobs que d'entrées est normal, et c'est le but : le lot est adressé par
le contenu, donc un artefact atteignable à deux adresses ne part qu'une fois.

## 4. Transporter et importer

```bash
batlehub mise import estate.bhub
```

```text
signature ok (3b1fa9c2) · 41 blob(s) · 0 rejected
```

La signature est vérifiée avant qu'un seul blob ne soit lu. Une ligne de rejet
nomme ce qui a été refusé et pourquoi — un blob dont les octets ne hachent pas
vers son nom, une clé qui n'est pas une clé de stockage, un registre que cette
instance n'a pas. Les rejets ne font pas échouer tout l'import : le reste
atterrit, et c'est le compte que vous vérifiez.

Importer deux fois le même lot n'écrit qu'une fois, et le dit.

## 5. Réconcilier

```bash
batlehub admin air-gap-missing --kind artifact
batlehub admin air-gap-missing --kind document
batlehub admin air-gap-missing --kind unmirrored_host
```

ou la page **Coupure réseau** de la console. La première liste est le contenu que
le prochain lot devrait porter. La deuxième, les listings pour lesquels rien n'a
pu être composé — un paquet dont aucune version n'est détenue, ou un type dont
l'index n'est pas composé (voir la page du registre) — et, pour une release de
forge par tag ou un `.info` Go, la version que voulait le client. La troisième,
les hôtes que rien ne réplique : chacun est une règle de réécriture qui vous
manque, et aucun lot n'y remédiera jamais.

Chaque ligne porte deux colonnes de plus, **Demandée** et **Détenue** : la
version que la requête nommait, quand elle en nommait une (un artefact issu d'un
verrou, une release par tag), et ce que l'instance détenait de ce paquet à ce
moment-là — ce que le listing composé avait proposé. Lues ensemble, elles sont le
diff du prochain plan : non pas « left-pad manque » mais « 1.2.0 a été demandée ;
1.3.0 est détenue ». `Demandée` reste vide pour un client qui a résolu une chaîne
de version contre le listing composé et s'est arrêté (le `ETARGET` de npm, le
« no matching distribution » de pip) : il n'a jamais demandé une version que
cette instance pouvait enregistrer, et seul un verrou nommant cette version le
fera.

Alimentez le prochain plan avec les deux, et purgez ce que vous avez satisfait :

```bash
batlehub admin air-gap-missing            # confirmez d'abord la liste
curl -X DELETE "$BATLEHUB/api/v1/admin/air-gap/missing?before=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  -H "Authorization: Bearer $TOKEN"
```

---

## Ce qu'un opérateur doit s'attendre à voir

| Symptôme | Signification |
| --- | --- |
| `503` avec `"code": "content_unavailable"` | Normal. L'instance ne le détient pas et n'ira pas le chercher. Le corps nomme la coordonnée ; elle est déjà enregistrée. |
| `501` avec `"code": "unmirrored_host"` | La règle attrape-tout a capté un hôte que rien ne réplique. Rien n'a été récupéré. |
| `403` nommant un blocage | Pas un trou. Un administrateur a bloqué cette coordonnée, et elle ne sera pas proposée pour le prochain lot. |
| Un registre hybrid qui se comporte comme un local | Attendu, et signalé au chargement : le repli vers l'amont ne peut pas avoir lieu ici. |
| `200` avec `X-BatleHub-Listing: synthesised` | Un listing composé depuis ce que l'instance détient ; `X-BatleHub-Listing-Held` compte les versions. Toute version qu'il nomme est servie à la requête suivante. |
| Un `503` sous `--kind document` | Un listing pour lequel rien n'a pu être composé : aucune version du paquet n'est détenue, l'index de ce type n'est pas composé (la page du registre le dit), ou — avec `Demandée` renseignée — une release de forge par tag ou un `.info` Go pour une version non détenue. Une installation depuis un verrou n'en a pas besoin. |
| `pip (simple-json)` dans le journal des défauts, une fois par exécution | L'autocontrôle de version de pip, `/simple/pip/`, que pip ignore. `PIP_DISABLE_PIP_VERSION_CHECK=1` côté client le supprime. |
| `github.com (versions)`, `github.com/google (versions)` à côté d'un module Go | `go get` sonde les chemins parents pour trouver la frontière du module avant de se fixer dessus. Rien à emporter. |
| Des défauts `.sha1` et `.md5` à côté d'un artefact Maven | Maven demande un fichier de somme de contrôle à côté de chaque fichier. Emportez-les pour les vrais fichiers (le plan peut les nommer) ; le `maven-metadata.xml` composé répond pour les siens. |
| Un `503` sur le `download/{os}/{arch}` d'un provider Terraform alors que ses `versions` listent la version | L'archive est détenue mais pas ses `shasums` ou sa `shasums.sig` : le document que Terraform vérifierait ne peut donc pas être composé. Ajoutez les deux chemins au plan. |

Les deux listes de la page nomment les registres qui ne détiennent **rien du
tout**, ce qu'il vaut mieux lire en premier : un tel registre refuse toutes les
requêtes, et un journal de défauts vide à côté signifie « personne n'a encore
demandé », pas « complet ».

## Avant la coupure

Deux choses valent la peine d'être faites tant que l'instance est encore
joignable :

- **L'amorcer.** Un registre coupé du réseau et sans contenu répond `503` à tout,
  ce qui est correct et inutile. Le serveur le dit une fois au démarrage, et la
  page Coupure réseau continue de le dire jusqu'à ce qu'un lot arrive.
- **Vérifier les clés de confiance.** `enabled = true` avec un
  `bundle_trusted_keys` vide est refusé au chargement, et une clé malformée
  aussi — mais une clé *valide et fausse* ne se découvre qu'au premier import, du
  mauvais côté de la coupure.
- **S'assurer que chaque projet versionne son `mise.lock`,** avec
  `lockfile = true`. C'est la différence entre une installation qui marche et une
  qui ne marche pas : depuis un verrou, mise tente exactement l'URL d'asset que
  le lot porte ; sans lui, il demande à la forge de résoudre la version contre la
  liste des releases, qui est un document, et obtient un `503`.
- **Désactiver la vérification de signature propre à mise du côté coupé.** Les
  cinq :

  ```toml
  [settings]
  github_attestations = false
  [settings.github]
  slsa = false
  [settings.aqua]
  cosign = false
  slsa = false
  minisign = false
  ```

  Chacune atteint Sigstore ou la forge avant d'installer quoi que ce soit, et un
  journal de transparence n'est pas quelque chose qu'un proxy peut mettre en
  cache — une preuve d'inclusion hors ligne ne prouve rien. Laissées actives, une
  installation passe le téléchargement et la somme de contrôle, puis échoue à la
  récupération de l'attestation ; si vous n'en désactivez qu'une, SLSA l'arrête
  plus tôt, au document de release.

  La somme de contrôle de `mise.lock` est toujours vérifiée localement — elle n'a
  besoin que des octets — et croisée avec le verdict propre de BatleHub. Voir
  [RFC 0008 §14.8](/rfc/0008-mise-in-an-air-gapped-estate) pour savoir
  exactement quelle part de la vérification côté connecté est construite.

## Voir aussi

- [Faire pointer mise vers BatleHub](/fr/use/mise) · [Configuration](/fr/guide/configuration#air-gap)
- [Reprise après sinistre](/fr/operations/disaster-recovery) · [Réponse à incident](/fr/operations/incident-response)
