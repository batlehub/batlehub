---
title: config
description: TOML configuration schema, validation, layered loading, environment overrides and hot-reload key classification.
---
<!-- techdoc:begin components/config#notice -->
> Sections between `techdoc` markers are regenerated; write your own notes outside them.
<!-- techdoc:end -->

<!-- techdoc:begin components/config#role -->
## Role

`crates/config` (crate `batlehub-config`) defines `AppConfig` and its sub-schemas (`crates/config/src/schema/`: `server.rs`, `auth.rs`, `registry.rs`, `storage.rs`, `rules.rs`, `security.rs`, `audit.rs`, `air_gap.rs`, `network.rs`, `routing.rs`, `notifications.rs`, `flag_sources.rs`, `forge.rs`, `release_imports.rs`, `warnings.rs`). Read by [server](server) and [web](web).
<!-- techdoc:end -->

<!-- techdoc:begin components/config#interfaces -->
## Interfaces

| Symbol | Path | Purpose |
| --- | --- | --- |
| `fn load`, `fn load_from_str` | `crates/config/src/lib.rs` | Single-file load (keeps TOML error spans) |
| `fn load_layered`, `fn load_layered_text`, `fn load_layered_from_str` | `crates/config/src/lib.rs` | Merge TOML documents (tables by key, arrays of tables on `name` then `type`, rest replaced) then deserialize once |
| `AppConfig::apply_env_overrides`, `apply_env_overrides_from` | `crates/config/src/schema/mod.rs` | `PROXY_CACHE__<SECTION>__<FIELD>` overrides |
| `validate()` | `crates/config/src/schema/mod.rs` | Parses `registry_type` into `RegistryKind`, other checks |
| reload key classification | `crates/config/src/reload_keys.rs` | Which keys apply hot and which need a restart |
| rendering | `crates/config/src/render.rs` | Writing config back (config editor) |
<!-- techdoc:end -->

<!-- techdoc:begin components/config#resources -->
## Resources

Filesystem only (config files); no network.
<!-- techdoc:end -->

<!-- techdoc:begin components/config#config -->
## Configuration

See [Getting started § Configuration](../getting-started) for the top-level sections and env overrides. The first layer is the only one the config editor reads or rewrites.
<!-- techdoc:end -->
