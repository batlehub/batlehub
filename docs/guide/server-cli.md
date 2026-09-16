# Server binary subcommands

`batlehub` is the server. This page is about its flags and about the
invocations that are not "start the server".

Not to be confused with [`batlehub-cli`](/use/cli), which is the client a
developer installs — they are different programs, and this page used to be
titled "CLI Reference" inside the configuration reference, which is how a
reader searching for "CLI" found two pages and could not tell them apart.

```
batlehub --config config.toml          # start the server (default: config.toml)
batlehub --version                     # version, and the backends this build carries
batlehub dump-spec                     # print the OpenAPI JSON spec to stdout
batlehub hash-token <token>            # generate an Argon2id PHC hash for a static token
batlehub explain-config                # print the permissions each subject holds, expanded
```

## `--version`

Prints the version and the cargo features the binary was compiled with:

```sh
batlehub --version
# batlehub 1.2.0 (features: storage-s3, cache-redis, sbom, jemalloc)
```

The features in that list are backends that either exist in the binary or do
not — `storage-s3` for `[storage] type = "s3"`, `cache-redis` for
`[cache] type = "redis"` — and a config naming one the build lacks is refused at
startup rather than degraded. All of them are default features, so a published
image carries the lot and this is mostly a question to ask of a build from
source, or of an image whose provenance is unclear:

```sh
docker run --rm ghcr.io/batleforc/batlehub:<version> --version
```

## `--config`

Repeatable. Each further file is a layer merged over the ones before it, which
is how credentials keep a lifecycle of their own:

```sh
batlehub --config /etc/batlehub/config.toml \
         --config /etc/batlehub/credentials/credentials.toml
```

Later layers win, every layer is watched for hot reload, and the **first** layer
is the only one the console's config editor reads or rewrites. The merge rules
are in [Layered config files](/guide/configuration#layered-config-files).

Where only environment variables are available, `BATLEHUB_CONFIG` takes the same
list separated by `:`, in the same order:

```sh
BATLEHUB_CONFIG=/etc/batlehub/config.toml:/etc/batlehub/credentials/credentials.toml batlehub
```

`--config` wins over `BATLEHUB_CONFIG` outright rather than adding to it: the
flag is the explicit instruction, and concatenating the two would make the
running config depend on an environment the operator did not mention on the
command line. With neither, the single `config.toml` in the working directory.

## `explain-config`

Prints the permissions each subject holds on each registry, after wildcard
expansion. Reads the same layer stack the server would start with, so what a
`"*"` covers is shown for the merged config rather than for the first file:

```sh
batlehub --config config.toml --config credentials.toml explain-config
```

A path argument overrides the stack and explains that one file alone:

```sh
batlehub explain-config ./some-other-config.toml
```

## `dump-spec`

Redirect the spec to a file for use with code generators:

```sh
batlehub dump-spec > openapi.json
```

## `hash-token`

Generates an Argon2id PHC hash that can be stored in `[[auth.tokens]].value` instead of a raw token string. The raw token is only required at generation time and does not need to be stored anywhere.

```sh
# Generate a hash
batlehub hash-token my-secret-token
# $argon2id$v=19$m=65536,t=3,p=4$<salt>$<hash>

# Paste the output directly into the config:
# [[auth.tokens]]
# value = "$argon2id$v=19$m=65536,t=3,p=4$..."
# role = "admin"
```

See [§3.3.1 Argon2id hashed token values](/guide/configuration#argon2id-hashed-token-values-recommended-for-production) for full context.

