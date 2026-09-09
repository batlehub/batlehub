---
sourcePath: guide/server-cli.md
sourceHash: d67738882a47cd4c
---

# Sous-commandes du binaire serveur

`batlehub` est le serveur. Cette page traite de ses options et des invocations
qui ne sont pas « démarrer le serveur ».

À ne pas confondre avec [`batlehub-cli`](/fr/use/cli), qui est le client qu'un
développeur installe — ce sont deux programmes différents, et cette page
s'intitulait autrefois « référence CLI » à l'intérieur de la référence de
configuration, ce qui faisait qu'un lecteur cherchant « CLI » tombait sur deux
pages sans pouvoir les distinguer.

```
batlehub --config config.toml          # démarrer le serveur (défaut : config.toml)
batlehub dump-spec                     # imprimer la spécification OpenAPI JSON sur stdout
batlehub hash-token <token>            # produire une empreinte Argon2id PHC pour un token statique
batlehub explain-config                # imprimer les permissions de chaque sujet, développées
```

## `--config`

Répétable. Chaque fichier supplémentaire est une couche fusionnée par-dessus les
précédentes, ce qui permet aux identifiants d'avoir leur propre cycle de vie :

```sh
batlehub --config /etc/batlehub/config.toml \
         --config /etc/batlehub/credentials/credentials.toml
```

Les couches ultérieures gagnent, toutes sont surveillées pour le rechargement à
chaud, et la **première** est la seule que l'éditeur de configuration de la
console lit et réécrit. Les règles de fusion sont dans
[Configuration en couches](/fr/guide/configuration#layered-config-files).

Là où seules des variables d'environnement sont disponibles, `BATLEHUB_CONFIG`
prend la même liste séparée par des `:`, dans le même ordre :

```sh
BATLEHUB_CONFIG=/etc/batlehub/config.toml:/etc/batlehub/credentials/credentials.toml batlehub
```

`--config` l'emporte entièrement sur `BATLEHUB_CONFIG` plutôt que de s'y
ajouter : l'option est l'instruction explicite, et concaténer les deux ferait
dépendre la configuration en cours d'un environnement que l'opérateur n'a pas
mentionné sur la ligne de commande. Sans l'un ni l'autre, c'est l'unique
`config.toml` du répertoire courant.

## `explain-config`

Imprime les permissions que chaque sujet détient sur chaque registre, après
développement des jokers. Lit la même pile de couches que le serveur au
démarrage : ce que couvre un `"*"` est donc affiché pour la configuration
fusionnée, et non pour le premier fichier.

```sh
batlehub --config config.toml --config credentials.toml explain-config
```

Un argument de chemin remplace la pile et explique ce seul fichier :

```sh
batlehub explain-config ./some-other-config.toml
```

## `dump-spec`

Redirigez la spécification vers un fichier, pour un générateur de code :

```sh
batlehub dump-spec > openapi.json
```

## `hash-token`

Produit une empreinte Argon2id PHC, stockable dans `[[auth.tokens]].value` à la
place d'un token en clair. Le token brut n'est nécessaire qu'au moment de la
génération et n'a besoin d'être stocké nulle part.

```sh
# Produire une empreinte
batlehub hash-token my-secret-token
# $argon2id$v=19$m=65536,t=3,p=4$<salt>$<hash>

# Collez la sortie directement dans la configuration :
# [[auth.tokens]]
# value = "$argon2id$v=19$m=65536,t=3,p=4$..."
# role = "admin"
```

Voir [§3.3.1 Valeurs de token hachées en Argon2id](/fr/guide/configuration#argon2id-hashed-token-values-recommended-for-production)
pour le contexte complet.
