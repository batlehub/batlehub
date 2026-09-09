---
sourcePath: guide/host-routing.md
sourceHash: da2cd38eb33cdf18
---

# Routage des registres par hôte

Tout registre est toujours joignable sous un sous-chemin :

```
https://hub.example.com/proxy/npm1/…
```

Le routage par hôte ajoute une **seconde porte d'entrée** : un ou plusieurs noms
d'hôte dont la *racine* est le registre.

```
https://npm1.hub.example.com/…      # générique, dérivé du nom du registre
https://npm.acme.io/…               # hôte dédié explicite
```

Le sous-chemin continue de fonctionner, inchangé, pour tous les registres.
L'hôte est une voie d'accès *supplémentaire* — rien ne change pour un registre
auquel vous n'en configurez pas.

## Pourquoi

- **Des écosystèmes qui se croient à la racine de l'origine.** Cargo publie sur
  `/api/v1/crates/new`, GitLab expose `/api/v4/…`, Forgejo `/api/packages/…`.
  Cela ne fonctionne derrière un préfixe de chemin que parce que BatleHub
  reproduit toute la forme sous `/proxy/{registry}`. Un hôte dédié supprime cette
  classe de problèmes.
- **Des URL absolues dans les métadonnées.** Les index de services NuGet, les
  pages simples PyPI, le `packages.json` de Composer, le `dist.tarball` de npm et
  le `download_url` de Terraform embarquent tous des URL qui se réfèrent à
  eux-mêmes. Sur un hôte de registre, elles deviennent courtes et stables.
- **L'ergonomie d'exploitation.** Un hôte dédié est quelque chose qu'on remet à
  une équipe, qu'on met dans un `settings.xml`, et qu'on garde stable pendant que
  le backend bouge. Il permet aussi d'appliquer des règles de WAF, des limites de
  débit ou des politiques TLS par hôte au niveau de l'ingress, sans lui apprendre
  le schéma de chemins de BatleHub.

## Avant / après

::: code-group

```ini [.npmrc — sous-chemin]
registry=https://hub.example.com/proxy/npm1/
//hub.example.com/proxy/npm1/:_authToken=…
```

```ini [.npmrc — hôte dédié]
registry=https://npm.acme.io/
//npm.acme.io/:_authToken=…
```

:::

::: code-group

```toml [.cargo/config.toml — sous-chemin]
[registries.internal]
index = "sparse+https://hub.example.com/proxy/cargo1/registry/"
```

```toml [.cargo/config.toml — hôte dédié]
[registries.internal]
index = "sparse+https://cargo.acme.io/registry/"
```

:::

## Configuration

```toml
# Dériver un hôte pour chaque registre depuis son nom.
[subdomain_routing]
enabled     = true
base_domain = "hub.example.com"   # npm1.hub.example.com -> registre "npm1"
scheme      = "https"             # sert uniquement à rendre les URL publiques dans l'API et la console

[[registries]]
name         = "npm1"
type         = "npm"
hosts        = ["npm.acme.io"]    # hôtes dédiés supplémentaires, facultatifs
path_routing = true               # défaut ; false => l'hôte est la seule porte d'entrée
```

- `[subdomain_routing]` est facultatif. Absent, ou avec `enabled = false`, aucun
  hôte générique n'est dérivé.
- `hosts` en est indépendant : un registre peut avoir des hôtes dédiés sans aucun
  générique configuré.
- `scheme` **n'affecte jamais le routage**. Il décide seulement si l'API annonce
  `https://npm.acme.io` ou `http://npm.acme.io`.
- Les hôtes entrants sont normalisés avant la recherche — mis en minuscules, port
  retiré, point final retiré. `NPM.Acme.io:8443.` et `npm.acme.io` sont le même
  hôte.

## L'hôte appartient au registre — entièrement

**Sur un hôte de registre, tous les chemins sont ceux du registre.** Il n'y a pas
de liste d'exceptions.

```
GET https://cargo1.hub.example.com/api/v1/crates/new
  -> /proxy/cargo1/api/v1/crates/new     ✅ cargo publish

GET https://cargo1.hub.example.com/api/v1/registries
  -> /proxy/cargo1/api/v1/registries     ❌ 404 — l'API d'administration est sur l'hôte principal
```

C'est contraint, pas préféré : cargo (`/api/v1/…`), GitLab (`/api/v4/…`) et
Forgejo (`/api/packages/…`) servent tous légitimement des chemins sous `/api`,
donc tout préfixe réservé masquerait une vraie route de registre. Le même
argument vaut pour `/healthz` et `/metrics` — un registre `generic` ou `deb` peut
légitimement répliquer ces chemins.

::: warning Pointez les sondes et les collectes vers l'hôte principal
`/healthz`, `/metrics`, l'API d'administration et la SPA ne sont servis que sur
le `base_domain` nu.
:::

::: tip Choisissez une porte d'entrée par client
`https://npm1.hub.example.com/proxy/npm1/lodash` devient
`/proxy/npm1/proxy/npm1/lodash` et donne un 404.
:::

## Les URL générées suivent la porte d'entrée

Toute URL auto-référente que BatleHub génère reflète la porte d'entrée réellement
employée par le client :

```jsonc
// GET https://npm.acme.io/lodash
{ "dist": { "tarball": "https://npm.acme.io/lodash/-/lodash-4.17.21.tgz" } }

// GET https://hub.example.com/proxy/npm1/lodash
{ "dist": { "tarball": "https://hub.example.com/proxy/npm1/lodash/-/lodash-4.17.21.tgz" } }
```

Il en va de même pour l'index de services NuGet et les `@id` d'enregistrement,
l'index simple de PyPI, les `metadata-url` et `dist` de Composer, le
`download_url` d'un provider Terraform, et les `dl` et `api` de l'index cargo.

`GET /api/v1/registries` publie l'URL préférée de chaque registre sous
`public_url` (le premier hôte explicite, à défaut l'hôte générique), et le guide
de mise en place s'en sert dans tous ses extraits.

## `path_routing = false` — l'hôte comme seule porte d'entrée

```toml
[[registries]]
name         = "npm1"
hosts        = ["npm.acme.io"]
path_routing = false        # /proxy/npm1/… -> 404
```

La motivation est l'isolement : une fois `npm.acme.io` remis à une équipe, vous
ne voulez peut-être pas que le même contenu réponde sur l'hôte principal
partagé, où il hérite de la politique CORS, des règles de WAF et des clés de
cache de cet hôte, et où une URL fuitée d'une porte d'entrée continue
silencieusement de fonctionner sur l'autre.

- Le défaut est `true` ; les configurations existantes ne bougent pas.
- Un registre avec `path_routing = false` et aucun hôte joignable est une
  **erreur de configuration** — ce serait un registre auquel rien ne peut parler.
- Le sous-chemin renvoie **404**, pas 403 : une porte d'entrée désactivée doit
  paraître absente, pas interdite. Elle est indiscernable d'un registre inconnu.
- C'est de l'isolement, pas de l'autorisation. Cela ferme une porte d'entrée ;
  cela n'accorde ni ne retire d'accès, et un utilisateur qui atteint le registre
  par l'hôte atteint exactement ce qu'il atteignait avant.

## La confiance envers les proxys — obligatoire

Le routage dépend désormais d'un en-tête. Derrière un reverse proxy, l'hôte
arrive dans `Forwarded` ou `X-Forwarded-Host` ; exposé directement, c'est `Host`.
Lequel des deux BatleHub croit est une décision de routage : elle doit donc être
énoncée.

```toml
[server]
# Plages CIDR (ou IP nues) des reverse proxys devant BatleHub.
trusted_proxies = ["10.42.0.0/16", "192.168.1.10"]
```

| `trusted_proxies` | Pair | Hôte employé pour le routage et les URL | IP client |
| --- | --- | --- | --- |
| absent | n'importe lequel | l'hôte transmis, à défaut `Host` | pair TCP |
| `[]` | n'importe lequel | l'en-tête `Host` seul | pair TCP |
| `["10.42.0.0/16"]` | dans la plage | l'hôte transmis, à défaut `Host` | premier `X-Forwarded-For` |
| `["10.42.0.0/16"]` | hors plage | l'en-tête `Host` seul | pair TCP |

::: danger Configurer le routage par hôte sans politique de confiance est une erreur de démarrage
Router sur un en-tête à propos duquel le serveur n'a aucune position déclarée
n'est pas un état qu'un déploiement devrait atteindre. Le message d'erreur
contient le TOML exact à coller.

Pour les déploiements *sans* routage par hôte, une liste absente conserve le
comportement préexistant — le durcir par défaut changerait silencieusement les
URL qu'ils annoncent déjà.
:::

**Employez des plages CIDR, pas des IP exactes.** Un ingress Kubernetes se trouve
derrière un CIDR de pods qui change à chaque déploiement. Une adresse nue est
acceptée et traitée comme un `/32` (`/128` en IPv6), de sorte que toute valeur
valide pour l'ancien `[ip_blocking].trusted_proxies` le reste.

::: info La clé dépréciée fonctionne toujours
Quand `[server].trusted_proxies` est absent, `[ip_blocking].trusted_proxies` est
employé — et gouverne alors l'hôte et le schéma transmis autant que l'IP client,
y compris pour satisfaire l'exigence ci-dessus. Un déploiement qui le déclare
déjà peut donc adopter le routage par hôte sans toucher à sa configuration de
confiance ; il reçoit simplement un avertissement de configuration l'invitant au
déplacement d'une ligne. Quand les deux sont définis, `[server]` gagne.
:::

**Usurper un hôte n'apporte rien.** Forger un hôte pour atteindre le registre *B*
équivaut exactement à demander `/proxy/B/…`, ce que tout client peut déjà faire.
L'autorisation est évaluée sur le registre, par les mêmes règles RBAC, après la
réécriture. Il n'existe aucune route joignable par hôte qui ne le soit pas par
chemin.

## Prérequis pour l'opérateur

1. Un enregistrement DNS par hôte, ou un générique `*.hub.example.com`.
2. Un certificat qui le couvre — un certificat générique dans le cas du
   `base_domain`. Avec `cert-manager`, le générique exige un résolveur DNS-01.
3. Un reverse proxy qui transmet l'en-tête `Host` d'origine.
4. Un `[server].trusted_proxies` listant les plages CIDR de ce proxy.

### Helm

```yaml
ingress:
  enabled: true
  host: batlehub.example.com
  extraHosts:
    - "*.batlehub.example.com"
    - "npm.acme.io"
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com
        - "*.batlehub.example.com"   # le SAN doit couvrir le générique

config:
  server:
    trusted_proxies: ["10.42.0.0/16"]   # le CIDR de pods de votre contrôleur d'ingress
  subdomain_routing:
    enabled: true
    base_domain: "batlehub.example.com"
```

Pour trouver le CIDR de pods :

```sh
kubectl cluster-info dump | grep -m1 cluster-cidr
```

## Validation

Rejeté au démarrage et à chaque rechargement :

| Condition | Pourquoi |
| --- | --- |
| `enabled = true` sans `base_domain` | la section ne routerait rien |
| le même hôte revendiqué par deux registres | ambigu ; un « le dernier écrit gagne » serait invisible |
| une entrée de `hosts` en collision avec l'hôte générique d'un autre registre | même ambiguïté, plus difficile à repérer |
| une entrée de `hosts` égale au `base_domain` | masquerait l'hôte principal et cacherait l'API d'administration |
| une entrée de `hosts` contenant `/`, un préfixe de schéma, ou vide après nettoyage | ce n'est pas un nom d'hôte |
| `path_routing = false` sur un registre sans hôte joignable | le registre serait entièrement injoignable |
| le routage par hôte sans politique de confiance envers les proxys | le routage dépendrait d'un en-tête non gouverné |

Signalé mais accepté, et exposé sur
`GET /api/v1/admin/config/warnings` ainsi que sur la page d'administration du
rechargement :

| Condition | Comportement |
| --- | --- |
| un nom de registre qui n'est pas une étiquette DNS valide (`my_registry`, `Foo.Bar`) | aucun hôte générique n'en est dérivé ; il reste joignable par chemin et par toute entrée `hosts` explicite |
| un routage par hôte satisfait uniquement par `[ip_blocking].trusted_proxies` | accepté et honoré ; déplacez la liste dans `[server]` |

## Retour en arrière

Une modification de configuration et un rechargement à chaud — la table des hôtes
se recharge à chaud comme toute autre table rapportée aux registres, et rien
n'est persisté. Sans `[subdomain_routing]` ni `hosts`, la table est vide, le
middleware ne fait rien, et toute URL générée est identique octet pour octet à
celle d'un déploiement qui n'a jamais eu la fonctionnalité.
