---
sourcePath: operations/weak-hashes.md
sourceHash: 8c2acb6cbeeb997c
---

# MD5 et SHA-1

Pour l'auditeur, ou le scanner, qui demande pourquoi un code de 2026 calcule du
MD5.

Une analyse de dépendances de BatleHub signale des usages de MD5 ou de SHA-1
(SonarCloud les lève en `rust:S4790`, CRITICAL). Cette page est le registre :
chacun d'eux, ce à quoi il sert, et — revérifié contre la spécification courante
de chaque protocole — si ce protocole accepterait quelque chose de plus fort.

Il y en avait treize. Quatre se sont révélés exigés par rien et **ont été
supprimés** ; neuf restent, et cette page dit pourquoi chacun est là.

## La ligne qui les sépare

**Aucune de ces valeurs n'est ce à quoi BatleHub fait confiance.** L'intégrité
d'un artefact, quand elle est une décision de sécurité, repose sur SHA-256,
calculé indépendamment à la première écriture des octets et revérifié à chaque
service ultérieur, ainsi que sur les signatures OpenPGP couvrant les index des
dépôts.

Les empreintes faibles sont toutes l'une de trois choses : un champ qu'un format
d'échange *nomme* MD5 ou SHA-1, un validateur de cache qu'un client compare octet
pour octet, ou le côté vérification d'une somme de contrôle qu'un registre amont
a annoncée en SHA-1. Une collision contre l'une d'elles n'apporte à un attaquant
rien qu'il n'obtiendrait en servant d'autres octets, parce que rien ne s'y
adosse.

C'est l'argument pour les neuf qui restent. Ce n'est pas la même chose que dire
que chacune est *inévitable*, ce à quoi servait la revérification — et quatre n'y
ont pas survécu.

## Le registre

| # | Où | Algorithme | Verdict |
| --- | --- | --- | --- |
| 1 | `core/services/integrity.rs` — `sha1_hex` | SHA-1 | Imposé par Composer |
| 2 | `core/services/integrity.rs` — `verify`, `StreamingVerifier` | SHA-1 | Vérification, pas émission |
| 3 | `core/…/local_registry/eco_rubygems.rs` — `/versions` | MD5 | Imposé par l'index compact |
| 4 | `web/…/proxy/rubygems/range.rs` — ETag | MD5 | **Clients anciens uniquement** |
| ~~5~~ | ~~`adapters/repo/deb.rs`~~ | ~~MD5, SHA-1~~ | **Supprimé** — facultatif chez Debian |
| ~~6~~ | ~~`adapters/repo/pacman.rs`~~ | ~~MD5~~ | **Supprimé** — retiré du format |
| 7 | `adapters/repo/openpgp.rs` — empreinte | SHA-1 | Immuable par définition |

Les entrées 5 et 6 sont barrées parce que le code ne les calcule plus. L'entrée 4
est en gras parce que c'est un choix de compatibilité plutôt qu'une exigence, et
la seule qu'il reste à revisiter. Voir
[Revérifié](#rechecked-2026-08-31).

## 1. Le `dist.shasum` de Composer

L'objet `dist` de Composer porte un unique champ de somme de contrôle, `shasum`,
et c'est un SHA-1. Y publier un SHA-256 ne dégrade pas vers « non vérifié » —
Composer hache le zip téléchargé en SHA-1 et compare : tous les téléchargements
échouent.

Un champ `sha256` dans `dist` est
[une demande de fonctionnalité ouverte depuis 2017](https://github.com/composer/composer/issues/5940)
et n'est pas implémenté. **Rien de plus fort n'est disponible.**

Précision de portée : `sha1_hex` est appelé depuis exactement un endroit, le
gestionnaire de publication Composer. Il n'est pas employé pour npm — le proxy
npm préfère `dist.integrity` (SSRI, en général SHA-512) à `dist.shasum` et ne
s'y rabat que si l'amont l'omet, et le packument npm local laisse passer le
`dist` envoyé par le client qui publie, en n'y réécrivant que l'URL du tarball.

## 2. Vérifier ce qu'un amont a annoncé

`verify` et `StreamingVerifier` acceptent SHA-1, SHA-256 et SHA-512, et
choisissent l'algorithme d'après la somme de contrôle publiée par le *registre
amont*. Quand un registre annonce un SHA-1, hacher en SHA-1 est la seule façon de
comparer.

C'est la seule entrée où retirer l'algorithme faible aggrave strictement les
choses : l'alternative à vérifier une somme SHA-1 est de ne rien vérifier du
tout.

## 3. L'index compact de RubyGems

Le document `/versions` de l'index compact est une ligne par gem se terminant par
une empreinte, et
[le format définit cette empreinte comme un MD5](https://github.com/rubygems/guides/blob/main/rubygems-org-compact-index-api.md)
du document `/info` de la gem :

```
RUBYGEM [-]VERSION_PLATFORM[,VERSION_PLATFORM],...] MD5
```

Bundler la recalcule pour décider si sa copie en cache de `/info` est à jour.
C'est un validateur de cache, dans l'algorithme que le format nomme, et il est
toujours d'actualité. **Rien de plus fort n'est disponible pour ce champ.**

Notez que l'en-tête `Repr-Digest` que BatleHub envoie sur ces mêmes documents est
déjà en SHA-256 — c'est l'empreinte moderne, celle de la RFC 9530, et celle que
la spécification exige réellement.

## 4. L'ETag de l'index compact

`compact_response` fixe l'ETag à un MD5 du corps du document, et
`holds_our_prefix` redérive le MD5 d'un *préfixe* pour répondre aux requêtes de
plage reprises de Bundler.

La valeur doit être en MD5 parce que c'est ainsi que Bundler l'a calculée : les
versions plus anciennes exécutent
`SharedHelpers.digest(:MD5).hexdigest(File.read(path))` sur le fichier local en
cache et l'envoient en `If-None-Match`, à côté d'un `bytes=<size - 1>-`. Un ETag
serveur dans un autre algorithme ne correspond jamais, et le client retélécharge
le fichier entier.

**Mais [Bundler 2.7.0 a retiré le hachage MD5 des réponses de l'index
compact](https://bundler.io/changelog.html) (16 juillet 2025, changement de
rupture).** Le Bundler courant emploie le `Repr-Digest` SHA-256. Ce MD5 ne sert
donc plus qu'à Bundler antérieur à 2.7, et le coût de le changer est un
retéléchargement complet pour ces clients — dégradé, pas cassé. C'est une
rétention de compatibilité délibérée, ce qui est une affirmation plus faible que
« le format l'exige ».

## 5. Les `Packages` et `Release` de Debian — supprimé

`parse_deb` calculait autrefois MD5, SHA-1 et SHA-256 sur chaque `.deb` ; la
strophe `Packages` émettait les trois, et `Release` listait chaque index sous une
section `MD5Sum:` en plus de `SHA256:`.

[Le format de dépôt Debian](https://wiki.debian.org/DebianRepository/Format)
rend les faibles facultatifs et dit clairement ce qu'un client peut en faire :

> Clients may not use the MD5Sum and SHA1 fields for security purposes, and must
> require a SHA256 or a SHA512 field.

Ils n'apportaient donc rien : apt accepte un index qui ne porte que du SHA-256,
c'est le SHA-256 qu'il vérifie, et c'est la signature OpenPGP couvrant `Release`
qui rend l'index digne de confiance en premier lieu. **`DebPackage` et
`ReleaseFile` ne portent plus de champ `md5` ni `sha1`**, les lignes `MD5sum:` et
`SHA1:` ont disparu de chaque strophe, et la section `MD5Sum:` a disparu de
`Release`.

Un `.deb` envoyé dont le `control` porte lui-même un champ `MD5sum` ou `SHA1` se
le voit toujours retirer — ce sont des champs de niveau dépôt, et un envoi n'a
pas à en réintroduire un.

Le coût est la compatibilité avec un apt trop ancien pour gérer SHA-256 —
bien plus ancien que tout ce qui reçoit encore des mises à jour de sécurité.

## 6. Le `%MD5SUM%` de Pacman — supprimé

Celui-là n'était pas seulement facultatif.
[pacman 6.1.0](https://gitlab.archlinux.org/pacman/pacman/-/raw/master/NEWS) a
retiré la prise en charge de md5sum dans les bases de dépôt — `repo-add` a cessé
de l'écrire et libalpm a abandonné sa lecture comme sa validation — et
[`alpm-repo-desc(5)`](https://man.archlinux.org/man/extra/alpm-repo-db/alpm-repo-desc.5.en)
énonce :

> The section `%MD5SUM%` has been removed.

BatleHub écrivait un champ que le pacman courant ignore. **`PacmanPackage` ne
porte plus de `md5`**, et `desc_entry` n'émet plus `%MD5SUM%` ; `%SHA256SUM%`,
que pacman lit bien, est inchangé.

`PacmanPackage` est stocké en JSON, et la structure n'a pas de
`deny_unknown_fields` : les métadonnées écrites avant ce changement se
désérialisent donc toujours — la clé en trop est ignorée.

## 7. L'empreinte OpenPGP v4

L'empreinte d'une clé OpenPGP de version 4 *est définie comme* un SHA-1 sur
`0x99 || len16 || pubkey_body` (RFC 4880 §12.2), et l'identifiant de clé en est
les 64 bits de poids faible. C'est un identifiant à la construction spécifiée,
pas un choix d'empreinte : le calculer autrement produit une empreinte qu'aucun
client ne reconnaît, et le `Signed-By:` d'apt comme `rpm --import` s'y adossent
tous deux.

Les clés v6 de la RFC 9580 emploient SHA-256, mais apt et rpm ne consomment pas
de clés v6 aujourd'hui. **Immuable tant que la clé est en v4.**

## Revérifié le 31 août 2026 {#rechecked-2026-08-31}

Le tri initial enregistrait les treize comme « imposés par un format d'échange ».
Trois entrées n'ont pas survécu à la confrontation avec les spécifications, et
deux des trois ont ensuite été supprimées :

- **Le `%MD5SUM%` de Pacman (#6)** — le champ a disparu du format. Supprimé.
- **Les MD5 et SHA-1 de Debian (#5)** — facultatifs, et la spécification interdit
  aux clients de s'y fier. Supprimés.
- **L'ETag de l'index compact (#4)** — exigé seulement par Bundler antérieur à
  2.7. Conservé : il offre encore les requêtes de plage reprises à ces clients,
  et contrairement aux deux autres, ce n'est pas une sortie morte.

Cela a fait passer treize constats à neuf. Rien ici n'était une vulnérabilité —
aucune attaque n'était rendue possible par l'un d'eux, ce pourquoi c'était un
registre et non un incident — mais quatre étaient des sorties qu'aucun client
courant ne lit, et la différence entre « le format l'exige » et « nous avons
toujours émis cela » est exactement ce à quoi sert une revérification.

### Comment les suppressions ont été vérifiées

Un vrai `apt` accepte le dépôt obtenu : `apt-get update` vérifie la signature
OpenPGP Ed25519 sur `InRelease`, indexe le paquet, et `apt-get download` le
récupère. Corrompre le `.deb` fait rejeter apt avec une non-correspondance
d'empreinte signalée sur **SHA256**, ce qui est bien le point — le contrôle qui
faisait le travail le fait toujours.

`.github/workflows/repo-interop.yaml` déroule cela de bout en bout pour de vrais
`apt`, `dnf` et `pacman` en conteneurs, et se déclenche à tout changement sous
`crates/adapters/src/repo/`.

## Comment le scanner les traite

Chacun a une entrée `rust:S4790` dans `sonar-project.properties`, rapportée au
seul fichier qui parle le protocole, avec son raisonnement en ligne. Ce sont des
exclusions configurées plutôt que des résolutions par constat dans le tableau de
bord, pour que la justification soit versionnée et relisible.

La portée est délibérée : une empreinte faible *en dehors* de ces sept fichiers
est un vrai constat. N'élargissez pas une `resourceKey` à un répertoire, et
n'ajoutez pas une huitième entrée sans un argument de même nature — ce qui, comme
cette page le montre, veut dire vérifier la spécification plutôt que répéter ce
que disait le commentaire précédent.

À lire aussi : [Security scanning](/contributing/security-scanning) (en anglais)
pour la matrice complète des scanners et la position sans suppression du projet
sur les alertes de dépendances.
