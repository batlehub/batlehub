# Glossaire français de la documentation

Non publié : c'est un outil de traduction, pas une page. Il arbitre le
vocabulaire de `docs/fr/` et il a une source, une seule —
`ui/src/locales/fr.json`, la traduction de la console, 1 074 chaînes déjà
arbitrées et déjà lues par des utilisateurs.

La raison de cette source unique tient en une phrase : **un lecteur qui lit la
page doit reconnaître le bouton.** Si la console dit « Amont » et que la page
dit « en amont du flux », personne ne relie les deux, et la documentation
devient une seconde interface avec son propre vocabulaire.

---

## 1. Ce qui ne se traduit pas

Cette liste est plus importante que la suivante. La faute la plus coûteuse en
documentation technique n'est pas la maladresse : c'est le terme traduit que le
lecteur doit re-traduire pour le retrouver dans son terminal.

| Catégorie | Exemples | Pourquoi |
| --- | --- | --- |
| Noms de registres et d'outils | npm, Cargo, Maven, PyPI, NuGet, Composer, RubyGems, Terraform, SDKMAN, mise, OpenVSX | Ce sont des noms propres. |
| Clés de configuration | `registry_type`, `cache_ttl_seconds`, `[[registries]]`, `deny_latest` | Ce que le lecteur écrit dans `config.toml`. Traduire une clé, c'est publier une configuration qui ne démarre pas. |
| Valeurs d'énumération | `proxy`, `local`, `hybrid`, `audit`, `block`, `allow`, `warn` | Idem : ce sont des littéraux. On écrit « le mode `hybrid` », jamais « le mode hybride ». |
| Verbes RBAC | `read`, `write`, `publish`, `yank`, `releases:read`, `releases:*` | Ce sont des chaînes que l'on colle dans une politique. |
| Sorties de commande et messages d'erreur | `403 Forbidden`, `error: package is blocked`, les blocs de log | Le serveur et la CLI parlent anglais — décision de périmètre, cf. §7 de `todo.md`. Une sortie traduite dans la doc est une sortie que `grep` ne retrouve pas. |
| En-têtes HTTP et chemins | `X-NuGet-ApiKey`, `Authorization: Bearer`, `/proxy/{registry}/npm/{name}` | Format de fil. |
| Noms de fichiers et de commandes | `config.toml`, `task docs:build`, `batlehub-cli`, `cargo test` | Ce que le lecteur tape. |
| Titres de RFC | « RFC 0015 — Grants on the resource hierarchy » | Les RFC ne sont pas traduites ; les citer sous un titre français rendrait la référence introuvable. |

Corollaire pratique : dans une phrase française, un identifiant reste dans son
code span et la phrase se construit autour. « Le champ `deny_latest` refuse
`latest` », pas « le champ Refuser-la-dernière ».

---

## 2. Le vocabulaire produit

Colonne de droite : la clé de `ui/src/locales/fr.json` où le choix a déjà été
fait. Quand une nuance manque, c'est la console qui tranche, pas la page.

| Anglais | Français | Genre / forme | Attesté dans la console |
| --- | --- | --- | --- |
| registry | registre | m. | `adminAccessCheck.versionNeedsPackage` |
| upstream (n.) | l'amont | m. | `adminHealth.upstreamTitle` = « Audit amont » |
| upstream (adv.) | en amont | — | `adminHealth.deleteArtifactHelp` |
| proxy | proxy | m. — « via proxy », « faire proxy vers » | `common.proxied` |
| cache | cache | m. — « le cache », « en cache » | `adminHealth.artifactNotCached` |
| to cache | mettre en cache | — | `adminDashboard.nothingIsCachedOrServed` |
| cache warming | préchauffage (du cache) | m. — « préchauffer » | `adminNav.warming` |
| cache hit / hit rate | hit / taux de hit | m. | `adminDashboard.hitRate` |
| miss | manque | m. | `adminUpstream.col.misses` |
| artifact | artefact | m. | `accessCheck.artifactOptional` |
| package | paquet | m. | `adminBulk.packageNoun` |
| version | version | f. | partout |
| coordinate | coordonnée | f. | `adminPackages.deleteConsequence` |
| storage | stockage | m. | `common.storage` |
| token | token | m. (invariable, pas « jeton ») | `account.tokens`, `appHeader.myTokens` |
| to revoke | révoquer | — | `tokensPage.revoke` |
| namespace | namespace | m. | `account.namespace` |
| group | groupe | m. | `common.group` |
| role | rôle | m. — anonyme, utilisateur, admin | `common.role`, `adminHealth.roleAnonymous` |
| grant | autorisation | f. | `packageGrants.addGrant` |
| granted by | accordé par | — | `adminAuthorization.grantedBy` |
| gate | garde-fou | m. | `adminAuthorization.gate` |
| rule | règle | f. | `adminAccessCheck.ruleMatched` |
| policy | politique | f. (les valeurs `audit`/`block` restent) | `adminUpstream.policyHelp.audit` |
| allowed / denied | autorisé / refusé | — | `adminAccessCheck.allow`, `.deny` |
| to block / a block | bloquer / un blocage | m. — « liste de blocage » | `adminAccessCheck.layerAccountBlocks` |
| to publish / publishing | publier / la publication | f. | `homePage.acceptingPublishes` |
| to yank / yanked | retirer / retiré | — (`yank` reste comme nom de commande) | `resolution.yanked` |
| pull (n.) | pull | m. — « pulls / jour » | `adminHealth.pullsDay` |
| to pull | récupérer | — | `adminHealth.deleteArtifactHelp` |
| stale | périmé | — | `resolution.stale`, `upstreamNotice.stale` |
| quota | quota | m. | `quotaWidget.title` |
| retention | rétention | f. | `adminAuthorization.retentionTitle` |
| audit log | journal d'audit | m. | `adminNav.auditLog` |
| health | santé, état de santé | f. | `adminDashboard.openHealth` |
| advisory | alerte (de sécurité) | f. | `advisoriesWidget.clearTitle` |
| vulnerability | vulnérabilité | f. | `flagsPanel.title` |
| scan (n.) | analyse | f. — « analyse CVE » | `exposurePanel.coverageScan` |
| verdict | verdict | m. | `verdictPanel.title` |
| quarantined | en quarantaine | — | `verdictPanel.state.quarantined` |
| signature / unsigned | signature / non signé | f. | `common.signature` |
| licence | licence | f. | `packageDetailPage.licenseUnknown` |
| beta channel | canal bêta | m. | `adminBetaChannel.betaChannel` |
| prefix | préfixe | m. | `common.prefix` |
| to expire | expirer | — | `common.expires` |
| air gap | coupure réseau | f. — « instance coupée du réseau » | page `docs/operations/air-gap.md` |
| SBOM | SBOM | m. (invariable) | `adminNav.sbomExport` |

---

## 3. Règles de rédaction

1. **Un libellé se nomme d'après ce qu'il fait**, jamais d'après la chaîne
   anglaise. La console a déjà payé cette leçon : « Bulk Import » y est devenu
   « Blocage de masse », parce que la page bloque des paquets et n'importe rien.
   Le même arbitrage vaut pour un titre de section.

2. **Vouvoiement, présent de l'indicatif, voix active.** « Le serveur refuse la
   requête », pas « la requête sera refusée par le serveur ».

3. **Les nombres et les unités ne bougent pas.** `4 GiB`, `30s`, `8080`. Les
   espaces insécables dans les nombres français (`4 000 mots`) sont bienvenus
   dans la prose, jamais dans une valeur de configuration.

4. **Guillemets français** (« … ») dans la prose, guillemets droits dans le
   code. L'apostrophe typographique (') est celle du texte ; l'apostrophe droite
   reste dans les identifiants et les commandes.

5. **Les titres gardent leur numérotation.** `## 6. Worked Examples` devient
   `## 6. Exemples commentés` : le numéro est lu par `docs:structure`, et une
   RFC ou une page anglaise qui cite « §6 » doit tomber sur la même section.

6. **Les ancres suivent le titre traduit.** Un lien français pointe vers
   l'ancre française. Quand un lien anglais existant doit continuer de
   fonctionner, on écrit une ancre explicite `{#…}` identique à l'anglaise.

7. **Un bloc de code ne se traduit pas**, à une exception près : les commentaires
   à l'intérieur, quand ils s'adressent au lecteur (`# remplacez par votre
   domaine`). Ce qui est exécuté reste intact.

8. **Les diagrammes Mermaid se traduisent** — leurs libellés sont du texte lu.
   Après traduction, `task docs:mermaid` doit toujours passer : une virgule
   déplacée dans un libellé casse le parseur, et le rendu se fait chez le
   lecteur.

9. **Les blocs générés ne se traduisent pas à la main.** Trois familles :
   les tables d'endpoints (`<!-- BEGIN endpoints: … -->`, régénérées par
   `task docs:endpoints`, dont seul l'en-tête est en français), la table de
   couverture des listings et celle des README (`task docs:listing-coverage`,
   `task docs:readme-coverage`, dont les cellules sont de la prose issue du code
   Rust et restent en anglais). Écrire dedans, c'est se faire écraser au premier
   `task docs:design`.

---

## 4. Ce que la page déclare

Chaque page de `docs/fr/` porte deux clés de frontmatter :

```yaml
---
sourcePath: guide/caching.md
sourceHash: 3f1c9a2b7d4e5061
---
```

`sourceHash` est le préfixe de 16 caractères du SHA-256 de la page anglaise au
moment de la traduction. `task docs:i18n:check` échoue quand la source a bougé,
`task docs:i18n:status` dit où on en est, et `task docs:i18n:stamp` re-tamponne
— après avoir lu le diff et mis le texte à jour, jamais à la place.
