---
sourcePath: registries/rubygems.md
sourceHash: d0fa2c89dde7a4a7
---

# RubyGems

Fait proxy et cache de rubygems.org pour Bundler et la CLI `gem`, ou héberge
des gems privées. BatleHub sert le téléchargement des gems, l'index de versions
et l'API REST d'information, sous contrôle du RBAC et du garde-fou d'âge de
publication.

## En un coup d'œil

| | |
|---|---|
| **Type de configuration** | `rubygems` |
| **Amont par défaut** | `rubygems.org` |
| **Modes** | proxy · local · hybrid |
| **Adressage** | par paquet |
| **Publication privée** | ✅ `gem push` |
| **Coupure réseau** | hors ligne, l'index compact (`/versions`, `/info/{gem}`, `/names`) et l'API JSON des versions sont composés à partir des gems détenues, les dépendances de chaque gem étant lues dans sa gemspec à l'import ; `bundle install` s'y résout |

## Mise en place du proxy

Installez depuis votre registre. Remplacez `<registry>` par le nom de registre
que vous avez configuré :

```sh
gem install rake --source https://batlehub.example.com/proxy/<registry>/
```

Ou dans un `Gemfile` :

```ruby
source "https://batlehub.example.com/proxy/<registry>" do
  gem "rake"
end
```

## Publication (local / hybrid) {#publishing-local-hybrid}

### Configuration du serveur

```toml
[[registries]]
type = "rubygems"
name = "internal-gems"
mode = "local"          # ou "hybrid" pour se rabattre sur rubygems.org

[registries.rbac]
anonymous = []
user      = ["source:read"]
admin     = ["*"]
```

En mode hybrid, ajoutez `upstreams = ["https://rubygems.org"]`.

### Mise en place côté client

**Option A — variable d'environnement (recommandée en CI) :**

```sh
export GEM_HOST_API_KEY="Bearer <your-token>"
```

`gem` envoie la valeur de `GEM_HOST_API_KEY` telle quelle dans l'en-tête
`Authorization` : le préfixe `Bearer ` est donc obligatoire.

**Option B — `~/.gem/credentials` (à créer s'il n'existe pas, puis
`chmod 600`) :**

```yaml
---
:batlehub: "Bearer <your-token>"
```

Le symbole (`:batlehub:`) est un nom arbitraire que vous choisissez. La valeur
doit inclure le préfixe `Bearer ` parce que `gem` l'envoie telle quelle dans
l'en-tête `Authorization`. Désignez l'entrée par son nom avec `--key` au moment
de publier.

### Publier

```sh
# Avec GEM_HOST_API_KEY (pas besoin de --key)
GEM_HOST_API_KEY="Bearer <your-token>" \
  gem push my-gem-1.0.0.gem --host https://batlehub.example.com/proxy/internal-gems/

# Avec ~/.gem/credentials et une clé nommée
gem push my-gem-1.0.0.gem \
  --host https://batlehub.example.com/proxy/internal-gems/ \
  --key batlehub
```

### Installer

```sh
# Avec GEM_HOST_API_KEY
GEM_HOST_API_KEY="Bearer <your-token>" \
  gem install my-gem --source https://batlehub.example.com/proxy/internal-gems/

# Avec une clé nommée de credentials
gem install my-gem \
  --source https://batlehub.example.com/proxy/internal-gems/ \
  --key batlehub
```

Ou dans un `Gemfile` :

```ruby
source "https://batlehub.example.com/proxy/internal-gems" do
  gem "my-gem"
end
```

### Retirer une version, ou annuler le retrait

```sh
# Retrait
curl -X DELETE \
  -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-gems/api/v1/gems/yank?gem_name=my-gem&version=1.0.0"

# Annulation du retrait
curl -X PUT \
  -H "Authorization: Bearer <your-token>" \
  "https://batlehub.example.com/proxy/internal-gems/api/v1/gems/unyank?gem_name=my-gem&version=1.0.0"
```

### Référence des endpoints

<!-- BEGIN endpoints: proxy/rubygems -->
| Méthode | Chemin | Description |
|--------|------|-------------|
| `POST` | `/proxy/{registry}/api/v1/gems` | Publish a gem (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/api/v1/gems/{name}.json` | Get gem information JSON (latest version). |
| `PUT` | `/proxy/{registry}/api/v1/gems/unyank` | Unyank a gem version (local/hybrid registries only). |
| `DELETE` | `/proxy/{registry}/api/v1/gems/yank` | Yank a gem version (local/hybrid registries only). |
| `GET` | `/proxy/{registry}/api/v1/versions/{name}.json` | List all versions of a gem. |
| `GET` | `/proxy/{registry}/gems/{filename}` | Download a gem file. |
| `GET` | `/proxy/{registry}/info/{gem}` | One gem's versions and dependencies — what Bundler resolves against. |
| `GET` | `/proxy/{registry}/latest_specs.4.8.gz` | Serve the latest-versions gem index (latest_specs.4.8.gz). |
| `GET` | `/proxy/{registry}/names` | Every gem name in the registry. |
| `GET` | `/proxy/{registry}/prerelease_specs.4.8.gz` | Serve the prerelease gem index (prerelease_specs.4.8.gz). |
| `GET` | `/proxy/{registry}/quick/Marshal.4.8/{filename}` | Serve a compressed gemspec file. |
| `GET` | `/proxy/{registry}/specs.4.8.gz` | Serve the full gem index (specs.4.8.gz). |
| `GET` | `/proxy/{registry}/versions` | The whole-registry version list Bundler fetches first. |
<!-- END endpoints -->

---

## L'index compact selon le mode

**L'index compact est ce que lit `bundle install`** — `/versions` d'abord, puis
`/info/{gem}` pour chaque gem. Les API JSON qui suivent sont un repli que Bundler
n'atteint que si l'index compact est absent : ce que disent ces trois documents
est donc ce que voit la résolution.

| Mode | `/versions`, `/info/{gem}`, `/names` |
| --- | --- |
| `proxy` | les documents de l'amont, filtrés |
| `hybrid` | les documents de l'amont, avec les gems de ce registre ajoutées |
| `local` | générés depuis les gems de ce registre ; l'amont n'est pas consulté |

Les dépendances sont lues dans la gemspec de chaque gem au moment de sa
publication et écrites dans `/info/{gem}`, parce que c'est là que le résolveur
les cherche. Les dépendances d'exécution seulement — celles de `:development` ne font pas
partie de ce qu'un installateur résout.

::: tip Publier et `bundle install`
Une gem est visible de Bundler dès qu'elle est publiée ; il n'y a pas d'index à
reconstruire. Les premières versions servaient les trois documents compacts
depuis l'amont dans tous les modes, de sorte qu'une gem publiée sur un registre
`local` ne pouvait pas y être installée du tout.
:::

### Récupération incrémentale

Bundler met ces documents en cache et demande la suite de ce qu'il détient, avec
un `If-None-Match` décrivant sa copie et un `Range`. Les trois répondent :

- `304` quand la copie est à jour ;
- `206` avec la seule fin du document quand la copie est un préfixe du document
  courant — vérifié, et non supposé, en comparant le validateur du client à
  l'empreinte de notre propre préfixe ;
- `200` avec le document entier sinon, ce que reçoit aussi tout client qui
  n'envoie pas de `Range`.

Rien n'est à configurer, et un client qui ignore tout cela fonctionne quand même.
L'effet pratique porte sur `/versions`, qui décrit le registre entier : en mode
`proxy` et `hybrid`, c'est l'index de l'amont — des dizaines de mégaoctets face à
rubygems.org — et il était auparavant transféré en entier à chaque résolution.

## Versions bloquées

L'index compact est filtré. `/info/{gem}` retire la ligne de la version bloquée.
`/versions` la retire de la liste séparée par des virgules de cette gem, et
retire la ligne entière de la gem quand toutes ses versions sont bloquées.

`/versions` décrit le registre entier : son ensemble de versions bloquées vient
donc d'un instantané de 30 secondes plutôt que d'une requête à chaque appel — le
même compromis que le `repodata.json` de conda, et pour la même raison :
réinterroger la liste complète des blocages sur le chemin le plus chaud de
l'écosystème ne vaut pas les secondes gagnées. **Un nouveau blocage atteint
`/versions` dans ce délai ; le garde-fou de téléchargement refuse les octets
immédiatement dans les deux cas.** `/info/{gem}` est propre à une gem et n'a
aucun retard de ce genre.

Quand la ligne d'une gem change, la somme de contrôle de son `/info` est
recalculée. C'est ce champ qui décide, pour Bundler, s'il faut re-récupérer
`/info/{gem}` ; le laisser tel quel permettrait à un client de continuer à servir
une copie mise en cache avant le blocage, et le blocage n'atteindrait jamais le
résolveur. Les lignes qui n'ont pas changé conservent la somme de contrôle de
l'amont octet pour octet : un blocage sur une gem ne fait donc pas retélécharger
à tous les clients les métadonnées de toutes les autres.

`/names` n'est **pas** filtré. Il liste des noms de gems et aucune version : un
blocage n'y a donc rien à masquer — et retirer le nom dirait à Bundler que la gem
n'existe pas, ce qui est une plus mauvaise réponse que « certaines de ses
versions sont restreintes ».

`/api/v1/versions/{name}.json` retire l'entrée bloquée.

`/api/v1/gems/{name}.json` décrit la gem à exactement une version : il est donc
**reconstruit** autour de la version la plus récente encore autorisée. Les champs
de niveau gem survivent ; ceux qui décrivent la *publication* masquée — sa somme
de contrôle, son URL de téléchargement, ses propres dates — sont retirés, parce
que reporter une somme de contrôle sur une autre version reviendrait à donner au
client une empreinte qui ne pourra jamais correspondre.

Les index Marshal (`specs.4.8.gz`, `quick/Marshal.4.8/*`) ne sont **pas**
filtrés. Y masquer une version demanderait un encodeur Marshal Ruby — et rien ne
les lit : Bundler résout depuis l'index compact ci-dessus, et les API JSON
répondent à tous les autres clients publiés cette décennie.

Le document amont est mis en cache pour le `metadata_ttl` du registre ; les
blocages sont appliqués par-dessus la copie en cache à chaque requête, de sorte
que bloquer une version prend effet immédiatement plutôt qu'à l'expiration du
cache.

Voir [bloquer une version de paquet](/fr/guide/admin-policies#block-a-package-version)
pour les deux moitiés d'un blocage, et
[quels listings sont filtrés](/fr/guide/admin-policies#which-listings-are-filtered)
pour la table complète.

## Authentification

`gem` envoie `GEM_HOST_API_KEY` telle quelle dans l'en-tête `Authorization` : le
préfixe `Bearer ` est donc obligatoire.

```sh
export GEM_HOST_API_KEY="Bearer $BATLEHUB_TOKEN"
```

Vous pouvez aussi la stocker dans `~/.gem/credentials` (`chmod 600`) sous un nom
de clé, et la désigner avec `--key` :

```yaml
---
:batlehub: "Bearer <your-token>"
```

## Notes

- Les gems sont mises en cache après le premier téléchargement.
- Pour répliquer rubygems.org de façon transparente pour un `Gemfile` existant,
  utilisez `bundle config set mirror.https://rubygems.org/ …` plutôt que de
  modifier la `source`.

## Voir aussi

- [Utiliser BatleHub](/fr/use/) — tokens, prérequis de publication, la CLI
- [Vue d'ensemble des registres](/fr/registries/) · [Mise en cache](/fr/guide/caching) · [Contrôle d'accès](/fr/guide/access-control)
