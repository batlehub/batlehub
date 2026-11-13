---
sourcePath: registries/mise-versions.md
sourceHash: 56593bb253a8be84
---

# Hôte de versions de mise

Fait proxy de l'**hôte de versions** de mise, `mise-versions.jdx.dev` : la
liste des versions que mise lit pour un outil du registre *avant* celle de
tout backend. Sans lui, `mise use jq@latest` choisit sa version dans une liste
que cette instance n'a jamais vue, et après chaque installation mise signale à
cet hôte l'outil, la version, l'OS et l'architecture. Avec lui, la liste passe
par la liste de blocage, le signalement reste sur site, et un hôte coupé du
réseau reçoit une réponse au lieu d'un délai d'attente
([RFC 0043](/rfc/0043-mise-versions-host)).

Ce type n'installe rien. Il sert des **listes** de versions ; les octets
viennent toujours du registre qui sert le backend de l'outil —
[GitHub](/fr/registries/github) pour les outils `aqua:` et `github:`.

## En un coup d'œil

| Propriété | Valeur |
|---|---|
| **Type de configuration** | `mise-versions` |
| **Amont par défaut** | `mise-versions.jdx.dev` |
| **Modes** | proxy seul |
| **Adressage** | un paquet par outil mise (`jq`), les versions telles que mise les écrit (`1.8.2`, pas le tag de la forge `jq-1.8.2`) |
| **Publication privée** | proxy seul |
| **Coupure réseau** | une liste répond `404` aussitôt — le signal pour mise de lister depuis le backend, que la coupure compose à partir de ce qu'elle détient |
| **Clients** | mise 2026.1.0 et suivants |

## Mise en place du proxy

mise atteint cet hôte par les mêmes `[settings.url_replacements]` que pour
chaque téléchargement. Une règle, placée **avant** toute règle qui pourrait
correspondre au même hôte, car mise n'applique que la première qui correspond :

```toml
[settings.url_replacements]
"regex:^https://mise-versions\\.jdx\\.dev/(.+)" = "https://batlehub.example.com/proxy/<registry>/mise-versions/$1"
```

`batlehub-cli registry suggest --mise` et `batlehub-cli setup apply --only mise`
l'écrivent, en premier, dès que l'instance a un registre `mise-versions`.

mise n'interroge cet hôte que pour un outil dont le backend est **celui du
registre par défaut** — `jq`, `node`, `terraform` — jamais pour un backend
explicite comme `github:cli/cli`, listé directement par son backend.

### Auto-wire : une règle pour tous les hôtes

Avec `[registries.auto_wire] enabled = true` sur ce registre
([RFC 0045](/rfc/0045-mise-auto-wire)), mise porte une seule règle pour tous
les hôtes que l'instance met en miroir, au lieu d'une par hôte. Le serveur
l'affiche pour votre compte :

```bash
batlehub-cli registry suggest --mise --auto-wire
batlehub-cli registry suggest --mise --auto-wire --mise-catch-all
```

La seconde forme est la variante coupée du réseau.
[Faire pointer mise vers BatleHub](/fr/use/mise#auto-wire) couvre l'overlay,
le dépannage et le retour arrière.

## Authentification

mise envoie une entrée `~/.netrc` pour l'hôte vers lequel une règle a réécrit,
donc une instance authentifiée a besoin d'une strophe pour l'hôte du proxy —
la même que pour les registres de forge :

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

mise refuse d'envoyer des identifiants à travers une réécriture de HTTPS vers HTTP.

Le bloc de registre côté administrateur :

```toml
[[registries]]
name = "mise-versions"
type = "mise-versions"
mode = "proxy"                                  # le seul mode : personne ne publie ici
upstreams = ["https://mise-versions.jdx.dev"]   # le défaut

[registries.rbac]
# Chaque route de ce type est un listing (`releases:list`) ; l'index python est
# lu comme des octets (`releases:read`).
anonymous = ["releases:read", "releases:list"]

  # Facultatif. Masque de la liste les versions publiées il y a moins d'une
  # semaine, pour que `latest` tombe sur une version que le garde-fou du
  # registre backend laisse aussi passer.
  [[registries.rules]]
  kind = "release_age_gate"
  min_age_secs = 604800
```

## Versions bloquées

Une version bloquée, c'est une ligne de moins dans `data/<tool>.toml` : toutes
les autres — l'ordre, `release_url`, `rolling`, `prerelease`, les clés que
cette instance ne connaît pas — sont servies telles que l'amont les a écrites.
`mise ls-remote jq` ne l'affiche plus et `mise use jq@latest` prend la plus
récente qui reste.

**Un blocage ici porte sur la liste, pas sur les octets.** `jq` / `1.8.2` sur ce
type et `jqlang/jq` / `jq-1.8.2` sur le registre GitHub sont deux coordonnées,
et c'est la seconde qui refuse le téléchargement. Bloquez les deux — le nom de
l'outil est celui de mise (`mise registry jq` affiche son backend) et la version
est le tag sans son préfixe :

```bash
batlehub-cli package block github/jqlang/jq@jq-1.8.2 --reason "CVE-2026-0000"
batlehub-cli package block mise-versions/jq@1.8.2 --reason "CVE-2026-0000"
```

Bloquer toutes les versions sert toujours le document, avec une table vide ;
mise liste alors depuis le backend, qui refuse les mêmes versions.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
et [quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered).

## Notes

- **Le garde-fou d'âge filtre la liste.** Chaque entrée porte `created_at`, et
  ce type n'a pas d'artefact qu'un garde-fou puisse refuser, donc un
  `release_age_gate` sur le registre masque les versions plus jeunes que lui —
  le seul listing de BatleHub qu'un garde-fou d'âge filtre. Il est réévalué à
  chaque requête sur la liste en cache, donc une version apparaît le jour où
  elle atteint l'âge requis.
- **La balise d'installation ne sort jamais.** Après une installation, mise
  signale l'outil, la version, l'OS, l'architecture et l'URL du backend —
  `GET /t?…` depuis 2026.11, `POST /api/tools/{tool}` depuis 2026.5,
  `POST /api/track` avant. Ce type répond `204` et ne relaie rien ; rien n'en
  est journalisé au-dessus de `trace`. mise l'envoie sans l'attendre et se
  termine souvent avant, donc la requête peut ne pas apparaître du tout.
- **Le miroir GitHub est refusé avec `503`.** mise demande aussi à cet hôte les
  métadonnées de publication GitHub (`/api/github/…`) sauf si `api.github.com`
  est réécrit. Un `503` est la réponse que mise retente sur `api.github.com` ;
  un `404` se lirait « cette publication n'existe pas » et ferait échouer
  l'installation. Gardez les règles du registre GitHub dans le même bloc, et
  mise saute complètement le miroir.
- **L'index python-build-standalone** (`python-precompiled-<triple>.gz`, que
  mise 2026.9.5 demande sous `tools/` et les versions suivantes sous `data/`)
  est mis en cache tel que l'amont l'envoie et filtré à la sortie : chaque
  build d'une version bloquée sous le nom `python` sur ce registre est retiré,
  et, avec une règle `release_age_gate`, chaque build dont la date de
  publication (le `+AAAAMMJJ` de son nom de fichier) est plus récente que le
  seuil. Un build plus ancien de la même version reste. Sans rien à retirer,
  les octets sont ceux de l'amont.
- **Anciens mise.** mise 2026.1.0–2026.6.5 demande `tools/<tool>.toml`, servi
  depuis la même entrée de cache. Avant 2026.1, mise demandait `/<tool>` en
  texte brut ; ce chemin répond `404` ici, et ce mise liste depuis son backend.
- **Coupure réseau.** Aucun bundle ne transporte cette liste — `mise.lock`
  nomme des artefacts, pas des listings — donc une instance coupée répond `404`
  sans enregistrer de manque. mise liste alors depuis le backend, et la
  RFC 0008-bis compose cette liste à partir des artefacts que le bundle a
  apportés.

## Points d'accès

::: details Afficher tous les points d'accès

<!-- BEGIN endpoints: proxy/mise-versions -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/mise-versions/_registry/latest.tar.zst` | The registry archive mise floats, with this instance's overlay tools |
| `GET` | `/proxy/{registry}/mise-versions/_wire-answer/{outcome}` | The wire's own answer: a bounce, `not_wired`, a malformed path, or the |
| `GET` | `/proxy/{registry}/mise-versions/api/github/{tail}` | The GitHub mirror's API family — refused with the `503` that makes mise ask |
| `POST` | `/proxy/{registry}/mise-versions/api/tools/{tool}` | The install beacon of mise 2026.5–2026.10 — answered here, sent nowhere. |
| `POST` | `/proxy/{registry}/mise-versions/api/track` | The install beacon of mise 2026.1–2026.4 — answered here, sent nowhere. |
| `GET` | `/proxy/{registry}/mise-versions/data/{file}` | A tool's version list, blocked and too-young versions removed — or the |
| `GET` | `/proxy/{registry}/mise-versions/data/github/{tail}` | The GitHub mirror's whole-list family — refused like the API family. |
| `GET` | `/proxy/{registry}/mise-versions/t` | The install beacon of mise 2026.11 and later — answered here, sent nowhere. |
| `GET` | `/proxy/{registry}/mise-versions/tools/{file}` | The same documents under the path mise 2026.1.0–2026.6.5 asks for its |
<!-- END endpoints -->

:::

## Voir aussi

- [mise](/fr/use/mise) — pointer mise vers BatleHub, connecté et coupé du réseau
- [GitHub](/fr/registries/github) — d'où viennent les octets d'un outil `aqua:` ou `github:`
- [Distributions Node](/fr/registries/nodedist) — l'autre liste de chaîne d'outils que lit mise
- [Vue d'ensemble des registres](/fr/registries/) · [Cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
