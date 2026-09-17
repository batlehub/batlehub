---
sourcePath: registries/nix.md
sourceHash: 15595b7f3b802650
---

# Cache binaire Nix

Le protocole *substituter* de Nix est relayé et mis en cache comme un registre typé, ce qui permet de **bloquer** un chemin du store plutôt que de simplement le mettre en cache. Les documents `{hash}.narinfo` sont le point d'application : Nix en demande un pour chaque chemin de la clôture, toujours, avant que le moindre octet ne circule.

**À lire d'abord : bloquer un binaire ne bloque pas le logiciel.** Une machine qui possède la dérivation — c'est le cas de toute machine NixOS — compile `hello-1.0.0.2` depuis les sources dès qu'aucun cache ne le sert, et rien dans ce protocole ne peut l'en empêcher. Le levier dont dispose un parc est `max-jobs = 0` sur les machines qui ne doivent pas compiler : c'est une configuration de Nix, pas de ce proxy.

## En bref

| | |
|---|---|
| **Type de configuration** | `nix` |
| **Amont par défaut** | `cache.nixos.org` |
| **Modes** | proxy · local · hybride |
| **Adressage** | le nom du store est la coordonnée — `hello-1.0.0.2-doc` donne `hello` en `1.0.0.2-doc` — et le hash de 32 caractères est l'artefact |
| **Publication privée** | ✅ `nix copy --to` |
| **Réglage client** | `substituters` dans `nix.conf` |

## Configuration du proxy

`substituters` est le seul réglage nécessaire. Remplacez `<registry>` par le nom du registre configuré :

```ini
# /etc/nix/nix.conf, ou nixConfig dans un flake pour un utilisateur de confiance.
#
# ?priority=30 place ce cache avant cache.nixos.org (priorité 40 ; la valeur la
# plus basse l'emporte) lorsqu'une machine liste les deux. Seul, ce paramètre
# n'a aucun effet.
substituters        = https://batlehub.example.com/proxy/<registry>/nix?priority=30
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=
```

La clé est celle de l'**amont**, inchangée : cette instance relaie chaque `Sig:` octet pour octet et ne signe jamais ce qu'elle a relayé, de sorte que vos machines continuent de vérifier avec la clé qu'elles font déjà confiance. Rien n'est à ajouter à `trusted-public-keys` pour lire à travers le proxy.

Les utilisateurs non privilégiés ne peuvent utiliser que les substituters listés dans `trusted-substituters` ou dans la liste du démon : c'est le levier qui permet de faire de cette instance le seul cache qu'une machine consulte.

Le bloc de registre à confier à votre administrateur :

```toml
[[registries]]
name      = "<registry>"
type      = "nix"
mode      = "proxy"
upstreams = ["https://cache.nixos.org"]   # la valeur par défaut

# Refuser de relayer un narinfo qui ne porte aucune ligne Sig:. Désactivé par
# défaut : un chemin adressé par contenu (CA:) n'en porte légitimement aucune,
# et c'est le require-sigs du client qui protège son store. Activé, ce réglage
# couvre le seul cas que le client ne peut pas voir : un miroir qui aurait
# silencieusement supprimé les signatures.
require_upstream_sigs = false

[registries.rbac]
# narinfo et nix-cache-info sont des listings ; les NAR, les .ls et les
# realisations sont des lectures.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list"]
admin     = ["*"]
```

## Authentification

**Nix n'envoie aucune identification tant qu'on ne lui a pas dit où la trouver.** Le téléchargeur est libcurl, avec `CURLOPT_NETRC_FILE` renseigné depuis le réglage `netrc-file` et `CURL_NETRC_OPTIONAL` ; le chemin par défaut est factice. Il n'existe aucun réglage d'en-tête pour les substituters : un registre authentifié exige donc cette ligne, et le chemin doit être absolu.

```ini
netrc-file = /etc/nix/netrc
```

```text
machine batlehub.example.com
  login token
  password <votre-jeton>
```

## Ce qui est bloqué, et ce que voit un client

Le paquet et la version sont ceux de Nix. `DrvName` découpe un nom de store au *premier tiret non suivi d'une lettre*, ce qui est exactement l'analyseur qu'utilisent `nix-env -u` et `lib.getVersion` — la version lue dans `nix-env -q` est donc bien celle que prend un blocage :

| nom du store | paquet | version |
|---|---|---|
| `curl-8.21.0` | `curl` | `8.21.0` |
| `hello-1.0.0.2-doc` | `hello` | `1.0.0.2-doc` |
| `php-curl-8.4.25` | `php-curl` | `8.4.25` |
| `gcc-wrapper-14+` | `gcc-wrapper` | `14+` |
| `source` | `source` | `-` (aucune partie version) |

Deux compilations d'une même version ne diffèrent que par le hash de 32 caractères, et un blocage les couvre **toutes** : le hash est l'artefact, pas quelque chose qu'un administrateur nomme.

Un chemin bloqué répond `404` sur son `.narinfo`, sur son `.ls` et sur toute requête `nar/{hash}/…`. Pour Nix, un `404` n'est pas une erreur : c'est *« ce cache ne l'a pas »*. Il mémorise ce résultat négatif pendant `narinfo-cache-negative-ttl` (3600 s) puis passe au substituter suivant, ou compile, en le disant avec ses propres mots. Il n'y a aucun refus en cours de transfert à prévoir : le narinfo est demandé avant le NAR, systématiquement.

Comme la route du NAR redérive la coordonnée depuis le hash présent dans son propre chemin, un client qui détient un narinfo récupéré *avant* le blocage — Nix les garde 30 jours — se voit également refuser les octets, plutôt que de les recevoir.

## La seule ligne que modifie cette instance

Un narinfo est relayé avec exactement un champ réécrit et aucun supprimé :

```text
StorePath: /nix/store/0001npbf…-hslua-aeson-2.3.2-doc
URL: nar/0001npbf…/075lhsj….nar.zst        ← réécrite
Compression: zstd
FileHash: sha256:10k72lz1…
FileSize: 46064
NarHash: sha256:075lhsj…
NarSize: 226848
References: ghpayap4…-aeson-2.2.4.1-doc …
Deriver: y1h1bh5g…-hslua-aeson-2.3.2.drv
Sig: cache.nixos.org-1:21qiHy652KfJ…      ← intacte
```

`URL:` est réécrite pour que la requête du NAR porte le hash dont sa coordonnée est dérivée : sans cela, une requête de NAR ne nomme aucun paquet et rien ne peut être bloqué, mis en cache ni comptabilisé. Cette réécriture est sans danger parce que **la signature ne couvre pas ce champ** : l'empreinte signée est `1;{StorePath};{NarHash};{NarSize};{References}`, et rien d'autre. Votre client vérifie donc exactement ce qu'il aurait vérifié face à l'amont, avec la clé de l'amont.

Si une machine détient encore un narinfo antérieur à ce registre, elle demandera le `nar/{filehash}.nar.zst` de l'amont. Cette instance tient un index inverse de ces noms, écrit à chaque narinfo servi ; une correspondance est servie sous sa coordonnée, une absence est un `404`, ce qui conduit Nix à redemander le narinfo, à y lire l'URL réécrite et à aboutir. Un aller-retour supplémentaire, une fois par chemin.

## `nix-cache-info`

Relayé tel que l'amont l'envoie. `StoreDir` n'est pas un réglage — un client dont le répertoire de store diffère refuse le cache entier avec *« binary cache '…' is for Nix stores with prefix '…', not '…' »* — et `Priority` est la seule valeur qu'un opérateur voudrait changer, ce qu'il fait sur sa propre ligne `substituters` avec `?priority=`.

Un registre sans amont à relayer en compose un : `StoreDir: /nix/store`, `WantMassQuery: 1`, `Priority: 30`.

## La barrière d'ancienneté

**Le protocole substituter ne transporte aucune date.** Un narinfo porte des empreintes, une clôture et un `Deriver`, rien d'autre : chaque chemin du store atteint donc une règle `release_age_gate` *sans date*, et le champ `deny_missing_timestamp` n'est pas un départage, c'est toute la règle :

- `true` refuse toute substitution sur le registre ;
- `false` rend la barrière inopérante.

Il n'y a pas de valeur par défaut. Une règle `release_age_gate` sur un registre `nix` qui ne renseigne pas ce champ est refusée au démarrage plutôt que d'hériter silencieusement d'une valeur.

## Publication

Un registre `local` ou `hybrid` accepte `nix copy --to` :

```sh
nix copy --to "https://batlehub.example.com/proxy/<registry>/nix" ./result
```

`nix copy` envoie le NAR **avant** le narinfo — les octets arrivent sans nommer
le moindre paquet — cette instance les met donc en attente jusqu'à ce que le
narinfo les réclame. C'est le narinfo qui porte la coordonnée, et cette
réclamation est aussi le moment où les octets sont vérifiés : `FileHash`,
`FileSize`, `NarHash` et `NarSize` sont tous recalculés ici, à partir des octets
que ce serveur détient, avant la moindre signature. Un document qui contredit
ses octets est un `400` nommant le champ fautif, et rien n'est stocké.

Un NAR ne peut être réclamé que par le narinfo du **même publieur** que celui
qui l'a téléversé.

### Signer ce qu'il héberge

```toml
[registries.nix_signing]
seed_hex = "${NIX_SIGNING_SEED}"   # openssl rand -hex 32
key_name = "batlehub-nix-1"        # facultatif ; défaut : batlehub-{registry}-1
```

Le registre signe l'empreinte qu'il vient de vérifier — jamais les condensats
annoncés par le document. `key_name` est la moitié d'une entrée
`trusted-public-keys` située avant le deux-points : elle doit donc rester stable
entre les redémarrages et être unique parmi les caches auxquels un client fait
confiance. Une rotation consiste à changer de suffixe, ce qui est le modèle de
Nix lui-même.

Distribuez la clé à chaque client :

```sh
curl -s https://batlehub.example.com/proxy/<registry>/nix/public-key
# batlehub-nix-1:<base64>
```

```ini
trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= batlehub-nix-1:<base64>
```

**Sans clé `nix_signing`, un registre local fonctionne toujours, et tout client
standard refusera ce qu'il sert** : le `require-sigs` de Nix est actif par
défaut, et un chemin non signé échoue avec *« cannot add path '…' because it
lacks a signature by a trusted key »*. Le serveur émet un avertissement au
démarrage plutôt que de refuser de démarrer, car un parc configuré en
`require-sigs = false` reste un laboratoire légitime.

Les signatures propres au publieur sont conservées : `nix copy` signe côté
client lorsque le store dispose de `secret-key-files`, et cela constitue une
provenance. La seule exception est une ligne `Sig:` portant le nom de clé de
**ce registre**, qui est supprimée — un publieur ne peut pas fabriquer la
signature qui atteste que ce serveur a vérifié quelque chose.

### Compression

`nix copy --to` compresse en **xz** par défaut. Ce registre sait vérifier `xz`,
`zstd` et `none` ; les dix autres algorithmes de Nix sont refusés à la
publication, car un NAR que ce serveur ne sait pas décompresser est un NAR dont
il ne peut pas contrôler le `NarHash` — et signer un condensat non vérifié est
précisément ce que cette vérification existe pour empêcher. Choisissez-en un via
l'URL du store :

```sh
nix copy --to "https://batlehub.example.com/proxy/<registry>/nix?compression=zstd" ./result
```

## Parcs isolés du réseau

Un chemin du store voyage dans le [paquet d'isolation réseau](../operations/air-gap) accompagné des faits que ses octets ne portent pas : sa clôture (`References:`), son `Deriver` et chacune de ses signatures. L'instance déconnectée compose le narinfo à partir de ces faits et sert la signature du **publieur** sans y toucher : un client de l'autre côté vérifie donc avec la clé qu'il utilise en mode connecté, et le parc ne signe rien.

Un chemin que le paquet ne transporte pas répond `503` et non `404` : il existe, il n'est simplement pas ici.

### Référence des endpoints

<!-- BEGIN endpoints: proxy/nix -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/nix/{hash}.ls` | `{hash}.ls` — a JSON listing of a NAR's contents, for `nix store ls` alone. |
| `GET` | `/proxy/{registry}/nix/{hash}.narinfo` | One store path's metadata — the chokepoint every substitution goes through. |
| `PUT` | `/proxy/{registry}/nix/{hash}.narinfo` | `PUT {hash}.narinfo` — the document that names the coordinate, and where this registry signs. |
| `GET` | `/proxy/{registry}/nix/log/{drv}` | `log/{drv}` — a build log, read by `nix log`. |
| `GET` | `/proxy/{registry}/nix/nar/{file}` | A NAR asked for in the **upstream's** own shape, resolved through the reverse index. |
| `PUT` | `/proxy/{registry}/nix/nar/{file}` | `PUT nar/{file}` — the NAR, arriving before anything knows what it is. |
| `GET` | `/proxy/{registry}/nix/nar/{hash}/{file}` | The NAR, under the coordinate the rewritten `URL:` gave it. |
| `GET` | `/proxy/{registry}/nix/nix-cache-info` | `nix-cache-info` — the three lines a client reads once per substituter. |
| `GET` | `/proxy/{registry}/nix/public-key` | `GET public-key` — the line an operator pastes into `trusted-public-keys`. |
| `GET` | `/proxy/{registry}/nix/realisations/{id}.doi` | `realisations/{id}.doi` — the derivation-to-output mapping of a content-addressed derivation. |
<!-- END endpoints -->

## Non pris en charge

- **Les stores `s3://` et `file://`.** Nix parle la même disposition sur les deux ; ce registre est le store `https://`. Un bucket S3 relève du backend de stockage, derrière la surface HTTP.
- **Le comportement dynamique de `nix-serve`** — calculer un narinfo depuis un `/nix/store` local à la demande. Cette instance n'a pas de store ; elle a ce qu'elle a relayé.
- **Re-signer ce que l'amont a signé.** Un narinfo relayé conserve ses signatures à l'identique.
- **Vérifier les signatures amont côté serveur.** Votre client s'en charge, avec les clés que vous avez choisies ; un proxy qui vérifierait aussi devrait maintenir la même liste de clés à un second endroit.
- **Bloquer un seul chemin du store.** Une version recompilée avec un autre hash reste le même logiciel, et un administrateur qui bloque une CVE les vise tous.
- **`channels.nixos.org` et les archives de canaux** — un autre hôte et un autre protocole. C'est le registre [`generic`](./generic) qui les met en miroir.
- **Les entrées de flake issues des forges.** Une entrée `github:` relève du registre [`github`](./github).
