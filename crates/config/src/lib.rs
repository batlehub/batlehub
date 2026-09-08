pub mod schema;

pub use schema::AppConfig;

use anyhow::{bail, Context, Result};
use std::path::Path;

pub fn load(path: impl AsRef<Path>) -> Result<AppConfig> {
    let raw = std::fs::read_to_string(path.as_ref())
        .with_context(|| format!("reading config file: {}", path.as_ref().display()))?;
    load_from_str(&raw)
}

/// Parse a config from a raw TOML string.
///
/// Identical to `load` but takes the raw content directly instead of reading from disk.
/// Environment variable placeholders (`${VAR}`) are still expanded.
pub fn load_from_str(raw: &str) -> Result<AppConfig> {
    let expanded = expand_env_vars(raw)?;
    let mut config: AppConfig = toml::from_str(&expanded).with_context(|| "parsing config TOML")?;
    config.apply_env_overrides();
    config.validate()?;
    Ok(config)
}

// ── Layered configuration ─────────────────────────────────────────────────────
//
// One process, several config files: the point is to keep credentials in a file
// with a different lifecycle from the rest of the configuration — a Kubernetes
// Secret beside a ConfigMap, a 0600 file beside a world-readable one — without
// giving up hot reload on either.
//
// The layers are merged as TOML *documents*, before deserialization, for two
// reasons. Merging `AppConfig` values after the fact would need every field to
// be an `Option` to tell "absent" from "at its default", which is a change to
// every schema struct in the crate and would weaken the single-file error
// messages. And merging documents is what lets a later layer complete a table
// the earlier one opened — `upstream_auth` on a registry declared elsewhere —
// which is the case this feature exists for.
//
// Env-var placeholders are expanded per layer, before the merge, so a `${VAR}`
// is resolved in the file that wrote it.

/// The key an array-of-tables merges on, when it has one.
///
/// `name` before `type` because a `[[registries]]` entry carries both and only
/// `name` identifies it; `[[auth]]` entries of the OIDC family carry a `name`
/// too, and the static-token provider carries only its `type`.
const IDENTITY_KEYS: [&str; 2] = ["name", "type"];

/// The identity key `base` and `overlay` can be merged on, if any.
///
/// Requires the key to be present, a string, and **unique** on both sides.
/// Uniqueness is what makes the merge well defined: two entries answering to
/// the same name have no single counterpart in the other layer, and quietly
/// picking the first would be a rule nobody could predict from the file.
/// Empty arrays are excluded so that `x = []` in a later layer clears the list
/// rather than being a vacuous no-op.
fn array_identity_key(base: &[toml::Value], overlay: &[toml::Value]) -> Option<&'static str> {
    if base.is_empty() || overlay.is_empty() {
        return None;
    }
    IDENTITY_KEYS.into_iter().find(|key| {
        [base, overlay].into_iter().all(|side| {
            let mut seen = std::collections::HashSet::new();
            side.iter().all(|entry| {
                entry
                    .as_table()
                    .and_then(|t| t.get(*key))
                    .and_then(toml::Value::as_str)
                    .is_some_and(|id| seen.insert(id))
            })
        })
    })
}

/// Merge `overlay` into `base`, in place. Later layers win.
///
/// - Table over table: recurse, so a later layer adds keys without erasing the
///   ones it does not mention.
/// - Array-of-tables over array-of-tables, when both sides key cleanly on
///   `name` (else `type`): merge entry by entry on that key, appending entries
///   the base does not have. This is the only rule that lets a credentials
///   layer complete one registry out of twenty without restating the other
///   nineteen.
/// - Everything else: the overlay replaces the base outright. That covers
///   scalars, scalar arrays such as `upstreams`, and arrays of tables with no
///   usable identity — where entry-by-entry merging would have to guess.
fn merge_value(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (toml::Value::Array(base), toml::Value::Array(overlay)) => {
            let Some(key) = array_identity_key(base, &overlay) else {
                *base = overlay;
                return;
            };
            let id_of = |entry: &toml::Value| {
                entry
                    .as_table()
                    .and_then(|t| t.get(key))
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            };
            for entry in overlay {
                match id_of(&entry)
                    .and_then(|id| base.iter_mut().find(|e| id_of(e).as_deref() == Some(&id)))
                {
                    Some(existing) => merge_value(existing, entry),
                    None => base.push(entry),
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

/// Read one layer: the file's bytes, with `${VAR}` placeholders expanded.
fn read_layer(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading config file: {}", path.display()))
}

/// Load a config from one or more layered TOML files, later layers winning.
///
/// With a single path this is exactly [`load`], including its error messages:
/// the merge path parses through `toml::Value` and so loses the line and column
/// a direct `toml::from_str::<AppConfig>` reports, and the single-file case is
/// the one almost every deployment is in.
pub fn load_layered(paths: &[impl AsRef<Path>]) -> Result<AppConfig> {
    match paths {
        [] => bail!("no config file given"),
        [only] => load(only),
        _ => {
            let mut layers = paths.iter().map(|p| {
                let path = p.as_ref();
                read_layer(path).and_then(|raw| {
                    merged_layer_value(&raw)
                        .with_context(|| format!("parsing config file: {}", path.display()))
                })
            });
            // `paths` has at least two entries here, so the first is present.
            let mut merged = layers.next().expect("at least two layers")?;
            for layer in layers {
                merge_value(&mut merged, layer?);
            }
            config_from_value(merged)
        }
    }
}

/// One layer as a TOML document, with its env placeholders already expanded.
fn merged_layer_value(raw: &str) -> Result<toml::Value> {
    let expanded = expand_env_vars(raw)?;
    let value: toml::Value = toml::from_str(&expanded)?;
    Ok(value)
}

/// Deserialize, apply env overrides and validate — the tail of [`load_from_str`],
/// shared so a layered config goes through exactly the same checks as a single
/// file rather than a parallel set that could drift.
fn config_from_value(value: toml::Value) -> Result<AppConfig> {
    let mut config: AppConfig = value
        .try_into()
        .with_context(|| "parsing merged config TOML")?;
    config.apply_env_overrides();
    config.validate()?;
    Ok(config)
}

/// Merge already-read layer contents, in order. The in-memory twin of
/// [`load_layered`], for the hot-reload path, which holds the primary layer's
/// text (from the file watcher or the config editor) and re-reads the rest.
pub fn load_layered_from_str(layers: &[impl AsRef<str>]) -> Result<AppConfig> {
    match layers {
        [] => bail!("no config content given"),
        [only] => load_from_str(only.as_ref()),
        _ => {
            let mut values = layers.iter().map(|l| merged_layer_value(l.as_ref()));
            let mut merged = values.next().expect("at least two layers")?;
            for value in values {
                merge_value(&mut merged, value?);
            }
            config_from_value(merged)
        }
    }
}

/// Expand `${VAR_NAME}` placeholders in a raw config string with their
/// environment variable values.
///
/// Rules:
/// - `${VAR_NAME}` is replaced with `std::env::var("VAR_NAME")`.
///   Returns an error if the variable is not set.
/// - `$${VAR_NAME}` is an escape sequence that produces the literal string
///   `${VAR_NAME}` without any variable lookup.
/// - Any other `$` character is left unchanged.
/// - Placeholders inside a TOML `#` comment (i.e. outside of a quoted
///   string) are left untouched — commented-out example lines never
///   require the referenced variable to be set.
///
/// Read `${VAR_NAME}` from `chars` (the `$` and `{` have already been consumed),
/// look up the variable in the environment, and return its value.
fn expand_braced_var(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String> {
    let mut var_name = String::new();
    loop {
        match chars.next() {
            Some('}') => break,
            Some(c) => var_name.push(c),
            None => bail!("unclosed '${{...}}' placeholder in config file"),
        }
    }
    if var_name.is_empty() {
        bail!("empty variable name in '${{}}' placeholder in config file");
    }
    std::env::var(&var_name)
        .with_context(|| format!("config references env var '${{{var_name}}}' but it is not set"))
}

/// Tracks TOML string/comment context while scanning so `$` expansion only fires
/// in code positions (not inside `'single'` strings or `#` comments).
#[derive(Default)]
struct QuoteScan {
    in_dquote: bool,
    in_squote: bool,
    in_comment: bool,
}

impl QuoteScan {
    /// Update state for `ch`, pushing it to `out` when it is structural.
    /// Returns `true` when `ch` was consumed here (the caller should move on),
    /// `false` when it is an ordinary character still eligible for `$` handling.
    fn consume(
        &mut self,
        ch: char,
        out: &mut String,
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    ) -> bool {
        if ch == '\n' {
            *self = QuoteScan::default();
            out.push(ch);
            return true;
        }
        if self.in_comment {
            out.push(ch);
            return true;
        }
        if ch == '"' && !self.in_squote {
            self.in_dquote = !self.in_dquote;
            out.push(ch);
            return true;
        }
        if ch == '\\' && self.in_dquote {
            // Don't let an escaped quote (\") toggle string state.
            out.push(ch);
            if let Some(next) = chars.next() {
                out.push(next);
            }
            return true;
        }
        if ch == '\'' && !self.in_dquote {
            self.in_squote = !self.in_squote;
            out.push(ch);
            return true;
        }
        if ch == '#' && !self.in_dquote && !self.in_squote {
            self.in_comment = true;
            out.push(ch);
            return true;
        }
        false
    }
}

fn expand_env_vars(raw: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut scan = QuoteScan::default();

    while let Some(ch) = chars.next() {
        if scan.consume(ch, &mut out, &mut chars) {
            continue;
        }
        if ch != '$' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                // $${ ... } → literal ${ ... }
                chars.next();
                if chars.peek() == Some(&'{') {
                    out.push('$');
                } else {
                    out.push('$');
                    out.push('$');
                }
            }
            Some('{') => {
                chars.next(); // consume '{'
                out.push_str(&expand_braced_var(&mut chars)?);
            }
            _ => out.push('$'),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::schema::{
        AppConfig, AuthConfig, StorageBackendConfig, StoragesConfig, CURRENT_CONFIG_VERSION,
    };
    use crate::{expand_env_vars, load};

    /// `cargo test` runs this crate's unit tests multi-threaded by default, but
    /// `std::env::set_var`/`remove_var` mutate the single process-wide
    /// environment table. Every test below that touches env vars acquires this
    /// lock first so their set/read/remove sequences never interleave.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn parse(toml: &str) -> AppConfig {
        let config: AppConfig = toml::from_str(toml).expect("parse failed");
        config.validate().expect("validate failed");
        config
    }

    fn minimal() -> &'static str {
        r#"
        [server]
        host = "127.0.0.1"
        port = 8080

        [database]
        type = "postgresql"
        url = "postgresql://user:pass@localhost/db"

        [storage]
        type = "filesystem"
        path = "./tmp"
        "#
    }

    // ── Layered config ────────────────────────────────────────────────────────
    //
    // The rules these pin down are the ones an operator has to be able to
    // predict from reading two files, so each test is named for the rule rather
    // than for the function under test.

    /// The base layer every layering test starts from: two registries, so a test
    /// can show that completing one leaves the other alone.
    fn two_registries() -> String {
        format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        name = "npm-priv"
        type = "npm"
        upstreams = ["https://npm.acme.io"]

        [[registries]]
        name = "crates"
        type = "cargo"
        "#
        )
    }

    #[test]
    fn a_later_layer_completes_a_registry_without_restating_the_others() {
        let cfg = crate::load_layered_from_str(&[
            two_registries(),
            r#"
            [[registries]]
            name = "npm-priv"
            [registries.upstream_auth]
            type = "bearer"
            token = "s3cr3t"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        // The registry the overlay named kept the fields only the base had.
        let npm = cfg
            .registries
            .iter()
            .find(|r| r.name == "npm-priv")
            .expect("npm-priv missing");
        assert_eq!(npm.upstreams, vec!["https://npm.acme.io".to_owned()]);
        assert!(
            npm.upstream_auth.is_some(),
            "credentials layer not merged in"
        );

        // And the one it did not name survived, which is the whole difference
        // from replacing the array.
        assert_eq!(cfg.registries.len(), 2);
        assert!(cfg.registries.iter().any(|r| r.name == "crates"));
    }

    #[test]
    fn a_later_layer_appends_a_registry_the_base_does_not_have() {
        let cfg = crate::load_layered_from_str(&[
            two_registries(),
            r#"
            [[registries]]
            name = "pypi-priv"
            type = "pypi"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        assert_eq!(cfg.registries.len(), 3);
        assert!(cfg.registries.iter().any(|r| r.name == "pypi-priv"));
    }

    #[test]
    fn a_later_layer_wins_on_a_scalar() {
        let cfg = crate::load_layered_from_str(&[
            minimal().to_owned(),
            r#"
            [database]
            url = "postgresql://real:secret@db/batlehub"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        assert_eq!(cfg.database.url, "postgresql://real:secret@db/batlehub");
        // `type` came from the base: a table merges rather than replaces.
        assert_eq!(cfg.server.port, 8080);
    }

    /// A scalar array has no identity to merge on, so the later layer replaces
    /// it. Appending would make a list of upstreams grow every time someone
    /// restated it, which is not what writing a list means.
    #[test]
    fn a_scalar_array_is_replaced_not_appended() {
        let cfg = crate::load_layered_from_str(&[
            two_registries(),
            r#"
            [[registries]]
            name = "npm-priv"
            upstreams = ["https://mirror.internal"]
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        let npm = cfg
            .registries
            .iter()
            .find(|r| r.name == "npm-priv")
            .expect("npm-priv missing");
        assert_eq!(npm.upstreams, vec!["https://mirror.internal".to_owned()]);
    }

    /// The case the whole feature is for: the credentials layer carries the
    /// `[[auth]]` block and the base carries none.
    #[test]
    fn the_credentials_layer_can_carry_the_whole_auth_block() {
        let cfg = crate::load_layered_from_str(&[
            minimal().to_owned(),
            r#"
            [[auth]]
            type = "token"
            [[auth.tokens]]
            value = "real-admin-token"
            role = "admin"
            user_id = "admin"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        assert_eq!(cfg.auth.len(), 1);
        assert!(matches!(cfg.auth[0], AuthConfig::Token(_)));
    }

    /// `[[auth]]` entries of the OIDC family carry a `name`, so two different
    /// providers stay two entries rather than collapsing on their shared
    /// `type`.
    #[test]
    fn two_named_auth_providers_do_not_collapse_onto_their_type() {
        let cfg = crate::load_layered_from_str(&[
            format!(
                "{}\n{}",
                minimal(),
                r#"
            [[auth]]
            type = "oidc"
            name = "corp"
            issuer_url = "https://sso.example.com/"
            client_id = "batlehub"
            client_secret = "placeholder"
            redirect_uri = "https://batlehub.example.com/api/v1/auth/oidc/callback"
            "#
            ),
            r#"
            [[auth]]
            type = "oidc"
            name = "partners"
            issuer_url = "https://partners.example.com/"
            client_id = "batlehub"
            client_secret = "placeholder"
            redirect_uri = "https://batlehub.example.com/api/v1/auth/oidc/callback"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        assert_eq!(cfg.auth.len(), 2);
    }

    /// An empty array in a later layer clears the list. Treating it as "nothing
    /// to merge" would make a deliberate `registries = []` unwritable.
    #[test]
    fn an_empty_array_in_a_later_layer_clears_the_list() {
        let cfg = crate::load_layered_from_str(&[two_registries(), "registries = []\n".to_owned()])
            .expect("layered load failed");

        assert!(cfg.registries.is_empty());
    }

    /// With no unique identity on both sides the merge would have to guess
    /// which entry pairs with which, so the later layer replaces the array
    /// outright. Two `type = "token"` blocks are the realistic way to get here.
    #[test]
    fn an_array_with_a_repeated_identity_is_replaced_not_merged() {
        let cfg = crate::load_layered_from_str(&[
            format!(
                "{}\n{}",
                minimal(),
                r#"
            [[auth]]
            type = "token"
            [[auth.tokens]]
            value = "first"
            role = "admin"

            [[auth]]
            type = "token"
            [[auth.tokens]]
            value = "second"
            role = "user"
            "#
            ),
            r#"
            [[auth]]
            type = "token"
            [[auth.tokens]]
            value = "only-this-one"
            role = "admin"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        assert_eq!(cfg.auth.len(), 1);
    }

    /// Placeholders are expanded per layer, before the merge, so a `${VAR}` is
    /// resolved in the file that wrote it rather than wherever it lands.
    #[test]
    fn env_placeholders_are_expanded_per_layer() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: single-threaded within this test thanks to ENV_LOCK.
        unsafe { std::env::set_var("BATLEHUB_TEST_LAYER_SECRET", "from-the-env") };

        let cfg = crate::load_layered_from_str(&[
            minimal().to_owned(),
            r#"
            [database]
            url = "postgresql://u:${BATLEHUB_TEST_LAYER_SECRET}@db/batlehub"
            "#
            .to_owned(),
        ])
        .expect("layered load failed");

        unsafe { std::env::remove_var("BATLEHUB_TEST_LAYER_SECRET") };
        assert_eq!(cfg.database.url, "postgresql://u:from-the-env@db/batlehub");
    }

    /// Validation runs on the merged document, not per layer: a layer that is
    /// incomplete on its own is the normal case, and only the result has to
    /// hold together.
    #[test]
    fn validation_runs_on_the_merged_document() {
        // The overlay alone has no [server]/[database]/[storage] and would never
        // deserialize; merged, it is a valid config.
        let cfg = crate::load_layered_from_str(&[
            minimal().to_owned(),
            "[server]\nport = 9090\n".to_owned(),
        ])
        .expect("layered load failed");
        assert_eq!(cfg.server.port, 9090);

        // And a merge that produces an invalid config is still refused.
        let err = crate::load_layered_from_str(&[
            minimal().to_owned(),
            r#"
            [[registries]]
            name = "bad"
            type = "not-a-registry-type"
            "#
            .to_owned(),
        ])
        .expect_err("an invalid merged config was accepted");
        assert!(
            format!("{err:#}").contains("not-a-registry-type"),
            "unexpected error: {err:#}"
        );
    }

    /// One layer behaves exactly like `load_from_str`, error messages included:
    /// the merge path parses through `toml::Value` and loses the span, so the
    /// single-file case must not take it.
    #[test]
    fn a_single_layer_takes_the_unlayered_path() {
        let one = crate::load_layered_from_str(&[minimal()]).expect("single layer failed");
        let direct = crate::load_from_str(minimal()).expect("direct load failed");
        assert_eq!(one.server.port, direct.server.port);
    }

    // ── Layering from disk ────────────────────────────────────────────────────
    //
    // The tests above drive `load_layered_from_str`, which is the shape the hot
    // reload path uses. These drive `load_layered`, which is what the process
    // actually starts with: the file reads, and the error messages that have to
    // name the file a reader must go and fix.

    /// Write `contents` into `dir` as `name` and return the path.
    fn layer_file(dir: &std::path::Path, name: &str, contents: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write layer");
        path.to_str().expect("utf-8 path").to_owned()
    }

    #[test]
    fn load_layered_merges_two_files_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = layer_file(dir.path(), "config.toml", minimal());
        let creds = layer_file(
            dir.path(),
            "credentials.toml",
            "[database]\nurl = \"postgresql://real:s3cr3t@db/batlehub\"\n",
        );

        let cfg = crate::load_layered(&[base, creds]).expect("layered load failed");

        assert_eq!(cfg.database.url, "postgresql://real:s3cr3t@db/batlehub");
        // From the base: the later layer completed the table rather than
        // replacing it.
        assert_eq!(cfg.server.port, 8080);
    }

    /// A missing layer names the file. The whole point of the feature is that
    /// the two files have different lifecycles, so "one of them is not there"
    /// is a normal deployment mistake and the message has to say which.
    #[test]
    fn load_layered_names_the_file_it_could_not_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = layer_file(dir.path(), "config.toml", minimal());
        let missing = dir.path().join("credentials.toml");

        let err = crate::load_layered(&[base, missing.to_str().unwrap().to_owned()])
            .expect_err("a missing layer was accepted");

        assert!(
            format!("{err:#}").contains("credentials.toml"),
            "the error does not name the missing layer: {err:#}"
        );
    }

    /// And a layer that is present but malformed names itself too, rather than
    /// reporting a parse error against a merged document no file contains.
    #[test]
    fn load_layered_names_the_file_that_would_not_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = layer_file(dir.path(), "config.toml", minimal());
        let broken = layer_file(dir.path(), "credentials.toml", "this is not = = toml\n");

        let err = crate::load_layered(&[base, broken]).expect_err("a broken layer was accepted");

        assert!(
            format!("{err:#}").contains("credentials.toml"),
            "the error does not name the unparseable layer: {err:#}"
        );
    }

    /// One path takes the unlayered code path, which is what keeps the line and
    /// column in the parse error for the case almost every deployment is in.
    #[test]
    fn load_layered_with_one_path_reports_the_position_of_a_syntax_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let broken = layer_file(dir.path(), "config.toml", "[server]\nport = = 8080\n");

        let err = crate::load_layered(&[broken]).expect_err("broken TOML was accepted");
        let msg = format!("{err:#}");

        assert!(
            msg.contains("line 2") || msg.contains("2:"),
            "the single-file path lost its error position: {msg}"
        );
    }

    #[test]
    fn load_layered_with_no_paths_is_an_error_rather_than_a_default() {
        // `impl Trait` cannot be turbofished, so the empty slice needs a type
        // from somewhere: an empty `Vec<String>` gives it one.
        let none: Vec<String> = Vec::new();
        let err = crate::load_layered(&none).expect_err("no paths was accepted");
        assert!(format!("{err:#}").contains("no config file"), "{err:#}");
    }

    #[test]
    fn parse_minimal_valid_config() {
        let cfg = parse(minimal());
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.database.url, "postgresql://user:pass@localhost/db");
        assert!(cfg.registries.is_empty());
        assert!(cfg.auth.is_empty());
        assert!(matches!(
            cfg.storage,
            StoragesConfig::Single(StorageBackendConfig::Filesystem(_))
        ));
    }

    #[test]
    fn parse_config_with_static_token_auth() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[auth]]
        type = "token"
        [[auth.tokens]]
        value = "secret"
        role = "admin"
        user_id = "alice"
        "#
        );
        let cfg = parse(&toml);
        assert_eq!(cfg.auth.len(), 1);
        assert!(matches!(cfg.auth[0], AuthConfig::Token(_)));
    }

    #[test]
    fn parse_config_with_oidc_auth() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[auth]]
        type = "oidc"
        issuer_url = "https://idp.example.com"
        client_id = "my-client"
        "#
        );
        let cfg = parse(&toml);
        assert!(matches!(cfg.auth[0], AuthConfig::Oidc(_)));
    }

    #[test]
    fn parse_config_with_registry() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "github"
        name = "gh"
        "#
        );
        let cfg = parse(&toml);
        assert_eq!(cfg.registries.len(), 1);
        assert_eq!(cfg.registries[0].name, "gh");
        assert_eq!(cfg.registries[0].registry_type, "github");
    }

    #[test]
    fn unknown_registry_type_returns_validation_error() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "bogus-registry"
        name = "my-bogus"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        let err = config
            .validate()
            .expect_err("unknown registry type should fail validation");
        assert!(err.to_string().contains("bogus-registry"));
    }

    #[test]
    fn registry_missing_name_returns_validation_error() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "github"
        name = ""
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "empty registry name should fail validation"
        );
    }

    #[test]
    fn duplicate_registry_names_return_validation_error() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "dup"

        [[registries]]
        type = "cargo"
        name = "dup"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        let err = config
            .validate()
            .expect_err("duplicate registry names should fail validation");
        assert!(err.to_string().contains("duplicate registry name"));
    }

    #[test]
    fn invalid_version_pattern_returns_validation_error() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npm-local"
        mode = "local"

        [registries.versioning]
        version_pattern = "[unterminated"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        let err = config
            .validate()
            .expect_err("invalid version_pattern regex should fail validation");
        assert!(err.to_string().contains("version_pattern"));
    }

    #[test]
    fn registry_storage_referencing_unknown_backend_returns_validation_error() {
        // Single-backend storage has no named backends, so a per-registry
        // `storage` reference must be rejected rather than silently ignored.
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npm-local"
        storage = "does-not-exist"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        let err = config
            .validate()
            .expect_err("unknown storage backend reference should fail validation");
        assert!(err.to_string().contains("storage"));
    }

    #[test]
    fn composer_local_mode_passes_validation() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "composer"
        name = "my-composer"
        mode = "local"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("composer + local mode must be accepted");
    }

    #[test]
    fn composer_hybrid_mode_passes_validation() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "composer"
        name = "my-composer"
        mode = "hybrid"
        upstreams = ["https://repo.packagist.org"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("composer + hybrid mode must be accepted");
    }

    #[test]
    fn jetbrains_proxy_mode_passes_without_upstream() {
        // jetbrains is proxy-only and has a real default upstream, so no explicit
        // `upstreams` is required (unlike deb/rpm).
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "jetbrains"
        name = "jb"
        mode = "proxy"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("jetbrains + proxy mode (no upstream) must be accepted");
    }

    #[test]
    fn jetbrains_local_mode_is_rejected() {
        // jetbrains is proxy-only — local/hybrid hosting is not supported.
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "jetbrains"
        name = "jb"
        mode = "local"
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "jetbrains + local mode should fail validation"
        );
    }

    #[test]
    fn generic_proxy_mode_with_upstream_and_path_allow_passes() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "generic"
        name = "node-dist"
        mode = "proxy"
        upstreams = ["https://nodejs.org/dist"]
        path_allow = ["v*/node-v*-linux-x64.tar.*", "v*/SHASUMS256.txt*"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("generic + upstream + path_allow must be accepted");
    }

    #[test]
    fn generic_without_upstream_is_rejected() {
        // A generic mirror has no default file tree to fall back to.
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "generic"
        name = "files"
        mode = "proxy"
        path_allow = ["**"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "generic without upstreams should fail validation"
        );
    }

    #[test]
    fn generic_without_path_allow_is_rejected() {
        // The allowlist is mandatory so a mirror of a shared host can't relay
        // every unrelated path on it.
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "generic"
        name = "files"
        mode = "proxy"
        upstreams = ["https://storage.googleapis.com"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "generic without path_allow should fail validation"
        );
    }

    #[test]
    fn generic_path_allow_double_star_is_the_explicit_opt_out() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "generic"
        name = "files"
        mode = "proxy"
        upstreams = ["https://get.helm.sh"]
        path_allow = ["**"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("path_allow = [\"**\"] must be accepted as the deliberate opt-out");
    }

    #[test]
    fn generic_local_mode_is_rejected() {
        // generic is proxy-only for now — hosting arbitrary files is a separate
        // roadmap item.
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "generic"
        name = "files"
        mode = "local"
        path_allow = ["**"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "generic + local mode should fail validation"
        );
    }

    #[test]
    fn path_allow_on_non_path_addressed_registry_is_rejected() {
        // Accepting it silently would read as a working restriction while gating
        // nothing, since npm requests never carry a raw upstream path.
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npm1"
        path_allow = ["lodash/*"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "path_allow on an npm registry should fail validation"
        );
    }

    #[test]
    fn path_allow_on_jetbrains_registry_is_accepted() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "jetbrains"
        name = "jb"
        path_allow = ["idea/*"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("path_allow must be accepted on path-addressed kinds");
    }

    #[test]
    fn invalid_path_allow_glob_is_rejected() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "generic"
        name = "files"
        upstreams = ["https://get.helm.sh"]
        path_allow = ["[unclosed"]
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            config.validate().is_err(),
            "an uncompilable path_allow glob should fail validation"
        );
    }

    #[test]
    fn config_version_absent_defaults_to_none_and_passes_validation() {
        let cfg = parse(minimal());
        assert_eq!(cfg.config_version, None);
    }

    #[test]
    fn config_version_at_current_passes_validation() {
        let toml = format!("config_version = {}\n{}", CURRENT_CONFIG_VERSION, minimal());
        let cfg = parse(&toml);
        assert_eq!(cfg.config_version, Some(CURRENT_CONFIG_VERSION));
    }

    #[test]
    fn config_version_newer_than_supported_is_rejected() {
        let toml = format!(
            "config_version = {}\n{}",
            CURRENT_CONFIG_VERSION + 1,
            minimal()
        );
        let cfg: AppConfig = toml::from_str(&toml).expect("parse failed");
        let err = cfg
            .validate()
            .expect_err("a config_version newer than supported should fail validation");
        assert!(err.to_string().contains("config_version"));
    }

    #[test]
    fn require_signed_release_enabled_passes_validation() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "github"
        name = "my-gh"

        [[registries.rules]]
        kind = "require_signed_release"
        enabled = true
        bypass_roles = ["admin"]
        deny_missing_signature = true
        "#
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        config
            .validate()
            .expect("require_signed_release rule must be accepted");
    }

    #[test]
    fn server_defaults_applied_when_fields_absent() {
        let toml = r#"
        [server]
        # no host or port

        [database]
        type = "postgresql"
        url = "postgresql://u:p@h/d"

        [storage]
        type = "filesystem"
        path = "./tmp"
        "#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.port, 8080);
    }

    #[test]
    fn cors_allowed_origins_parses_correctly() {
        let toml_full = r#"
        [server]
        host = "0.0.0.0"
        port = 8080
        cors_allowed_origins = ["https://app.example.com", "https://staging.example.com"]

        [database]
        type = "postgresql"
        url = "postgresql://u:p@h/d"

        [storage]
        type = "filesystem"
        path = "./tmp"
        "#;
        let cfg: AppConfig = toml::from_str(toml_full).unwrap();
        let origins = cfg.server.cors_allowed_origins.unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], "https://app.example.com");
    }

    #[test]
    fn env_override_replaces_database_url() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__DATABASE__URL", "postgresql://env-host/env-db");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__DATABASE__URL");
        assert_eq!(cfg.database.url, "postgresql://env-host/env-db");
    }

    #[test]
    fn env_override_replaces_server_port() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__SERVER__PORT", "9090");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__SERVER__PORT");
        assert_eq!(cfg.server.port, 9090);
    }

    // ── Offline / stale-metadata config ──────────────────────────────────────

    #[test]
    fn cache_config_defaults_to_memory() {
        let cfg: AppConfig = toml::from_str(minimal()).unwrap();
        assert_eq!(cfg.cache.cache_type, "memory");
    }

    #[test]
    fn cache_config_explicit_postgres() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [cache]
        type = "postgres"
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert_eq!(cfg.cache.cache_type, "postgres");
    }

    #[test]
    fn cache_policy_serve_stale_defaults_to_true() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npmjs"
        [registries.cache]
        metadata_ttl_secs = 300
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(
            cfg.registries[0].cache.serve_stale,
            "serve_stale should default to true"
        );
    }

    #[test]
    fn cache_policy_serve_stale_can_be_disabled() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npmjs"
        [registries.cache]
        metadata_ttl_secs = 300
        serve_stale = false
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(!cfg.registries[0].cache.serve_stale);
    }

    #[test]
    fn parse_config_with_kubernetes_auth() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[auth]]
        type = "kubernetes"
        api_server = "https://k8s.example.com"
        [auth.role_mappings]
        "system:masters" = "admin"
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(matches!(cfg.auth[0], AuthConfig::Kubernetes(_)));
        if let AuthConfig::Kubernetes(k8s) = &cfg.auth[0] {
            assert_eq!(k8s.api_server.as_deref(), Some("https://k8s.example.com"));
            assert_eq!(k8s.name, "kubernetes");
        }
    }

    #[test]
    fn parse_config_with_s3_storage() {
        let toml = r#"
        [server]
        host = "0.0.0.0"
        port = 8080

        [database]
        type = "postgresql"
        url = "postgresql://u:p@h/d"

        [storage]
        type = "s3"
        bucket = "my-bucket"
        region = "us-east-1"
        endpoint_url = "http://localhost:9000"
        force_path_style = true
        "#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        assert!(matches!(
            cfg.storage,
            StoragesConfig::Single(StorageBackendConfig::S3(_))
        ));
    }

    #[test]
    fn parse_config_with_multi_storage() {
        let toml = r#"
        [server]
        host = "0.0.0.0"
        port = 8080

        [database]
        type = "postgresql"
        url = "postgresql://u:p@h/d"

        [storage]
        default = "primary"

        [[storage.backends]]
        name = "primary"
        type = "filesystem"
        path = "./tmp"

        [[storage.backends]]
        name = "secondary"
        type = "s3"
        bucket = "artifacts"
        region = "us-east-1"
        "#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        assert!(matches!(cfg.storage, StoragesConfig::Multi(_)));
        if let StoragesConfig::Multi(m) = &cfg.storage {
            assert_eq!(m.default, "primary");
            assert_eq!(m.backends.len(), 2);
            assert_eq!(m.backends[0].name, "primary");
            assert_eq!(m.backends[1].name, "secondary");
        }
    }

    #[test]
    fn env_override_filesystem_storage_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__STORAGE__PATH", "/new/path");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__STORAGE__PATH");
        if let StoragesConfig::Single(StorageBackendConfig::Filesystem(fs)) = &cfg.storage {
            assert_eq!(fs.path, "/new/path");
        } else {
            panic!("expected filesystem storage");
        }
    }

    #[test]
    fn env_override_s3_storage_fields() {
        let _guard = ENV_LOCK.lock().unwrap();
        let toml = r#"
        [server]
        host = "0.0.0.0"
        port = 8080

        [database]
        type = "postgresql"
        url = "postgresql://u:p@h/d"

        [storage]
        type = "s3"
        bucket = "old-bucket"
        region = "eu-west-1"
        "#;
        let mut cfg: AppConfig = toml::from_str(toml).unwrap();
        std::env::set_var("PROXY_CACHE__STORAGE__BUCKET", "new-bucket");
        std::env::set_var("PROXY_CACHE__STORAGE__REGION", "us-east-1");
        std::env::set_var("PROXY_CACHE__STORAGE__ENDPOINT_URL", "http://minio:9000");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__STORAGE__BUCKET");
        std::env::remove_var("PROXY_CACHE__STORAGE__REGION");
        std::env::remove_var("PROXY_CACHE__STORAGE__ENDPOINT_URL");
        if let StoragesConfig::Single(StorageBackendConfig::S3(s3)) = &cfg.storage {
            assert_eq!(s3.bucket, "new-bucket");
            assert_eq!(s3.region, "us-east-1");
            assert_eq!(s3.endpoint_url.as_deref(), Some("http://minio:9000"));
        } else {
            panic!("expected s3 storage");
        }
    }

    #[test]
    fn env_override_otel_creates_section_when_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        assert!(cfg.otel.is_none());
        std::env::set_var("PROXY_CACHE__OTEL__ENDPOINT", "http://otel:4317");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__OTEL__ENDPOINT");
        assert_eq!(cfg.otel.as_ref().unwrap().endpoint, "http://otel:4317");
        assert_eq!(cfg.otel.as_ref().unwrap().service_name, "batlehub");
    }

    #[test]
    fn env_override_otel_service_name_when_section_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [otel]
        endpoint = "http://otel:4317"
        service_name = "old-name"
        "#
        );
        let mut cfg: AppConfig = toml::from_str(&toml).unwrap();
        std::env::set_var("PROXY_CACHE__OTEL__SERVICE_NAME", "new-name");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__OTEL__SERVICE_NAME");
        assert_eq!(cfg.otel.as_ref().unwrap().service_name, "new-name");
    }

    #[test]
    fn env_override_static_dir() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__SERVER__STATIC_DIR", "/var/www");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__SERVER__STATIC_DIR");
        assert_eq!(cfg.server.static_dir.as_deref(), Some("/var/www"));
    }

    #[test]
    fn env_override_database_max_connections() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__DATABASE__MAX_CONNECTIONS", "25");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__DATABASE__MAX_CONNECTIONS");
        assert_eq!(cfg.database.max_connections, 25);
    }

    #[test]
    fn database_pool_fields_default() {
        let cfg: AppConfig = toml::from_str(minimal()).unwrap();
        assert_eq!(cfg.database.min_connections, 1);
        assert_eq!(cfg.database.acquire_timeout_secs, 30);
    }

    #[test]
    fn env_override_database_min_connections() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__DATABASE__MIN_CONNECTIONS", "3");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__DATABASE__MIN_CONNECTIONS");
        assert_eq!(cfg.database.min_connections, 3);
    }

    #[test]
    fn env_override_database_acquire_timeout_secs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        std::env::set_var("PROXY_CACHE__DATABASE__ACQUIRE_TIMEOUT_SECS", "5");
        cfg.apply_env_overrides();
        std::env::remove_var("PROXY_CACHE__DATABASE__ACQUIRE_TIMEOUT_SECS");
        assert_eq!(cfg.database.acquire_timeout_secs, 5);
    }

    // ── env var interpolation ──────────────────────────────────────────────────

    #[test]
    fn env_interpolation_basic() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_EXPAND_BASIC", "hello");
        let result = expand_env_vars("value = \"${_TEST_EXPAND_BASIC}\"").unwrap();
        std::env::remove_var("_TEST_EXPAND_BASIC");
        assert_eq!(result, "value = \"hello\"");
    }

    #[test]
    fn env_interpolation_missing_var_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("_TEST_EXPAND_MISSING");
        let err = expand_env_vars("x = \"${_TEST_EXPAND_MISSING}\"").unwrap_err();
        assert!(err.to_string().contains("_TEST_EXPAND_MISSING"));
    }

    #[test]
    fn env_interpolation_escape_produces_literal() {
        let result = expand_env_vars("x = \"$${LITERAL}\"").unwrap();
        assert_eq!(result, "x = \"${LITERAL}\"");
    }

    #[test]
    fn env_interpolation_bare_dollar_unchanged() {
        let result = expand_env_vars("x = \"price is $5\"").unwrap();
        assert_eq!(result, "x = \"price is $5\"");
    }

    #[test]
    fn env_interpolation_oidc_client_secret() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_OIDC_SECRET", "super-secret");
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[auth]]
        type = "oidc"
        issuer_url = "https://idp.example.com"
        client_id = "my-client"
        client_secret = "${_TEST_OIDC_SECRET}"
        "#
        );
        let expanded = expand_env_vars(&toml).unwrap();
        std::env::remove_var("_TEST_OIDC_SECRET");
        let cfg: AppConfig = toml::from_str(&expanded).unwrap();
        if let AuthConfig::Oidc(oidc) = &cfg.auth[0] {
            assert_eq!(oidc.client_secret.as_deref(), Some("super-secret"));
        } else {
            panic!("expected OIDC auth");
        }
    }

    #[test]
    fn env_interpolation_upstream_bearer_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_BEARER_TOKEN", "tok-abcdef");
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "private-npm"
        [registries.upstream_auth]
        type = "bearer"
        token = "${_TEST_BEARER_TOKEN}"
        "#
        );
        let expanded = expand_env_vars(&toml).unwrap();
        std::env::remove_var("_TEST_BEARER_TOKEN");
        let cfg: AppConfig = toml::from_str(&expanded).unwrap();
        assert!(matches!(
            cfg.registries[0].upstream_auth,
            Some(crate::schema::UpstreamAuthConfig::Bearer(ref b)) if b.token == "tok-abcdef"
        ));
    }

    #[test]
    fn env_interpolation_upstream_basic_password() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_BASIC_PASS", "s3cr3t");
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "cargo"
        name = "private-cargo"
        [registries.upstream_auth]
        type = "basic"
        username = "deploy"
        password = "${_TEST_BASIC_PASS}"
        "#
        );
        let expanded = expand_env_vars(&toml).unwrap();
        std::env::remove_var("_TEST_BASIC_PASS");
        let cfg: AppConfig = toml::from_str(&expanded).unwrap();
        assert!(matches!(
            cfg.registries[0].upstream_auth,
            Some(crate::schema::UpstreamAuthConfig::Basic(ref b)) if b.password == "s3cr3t"
        ));
    }

    #[test]
    fn env_interpolation_multiple_vars_in_one_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_MULTI_A", "val-a");
        std::env::set_var("_TEST_MULTI_B", "val-b");
        let result = expand_env_vars("a = \"${_TEST_MULTI_A}\"\nb = \"${_TEST_MULTI_B}\"").unwrap();
        std::env::remove_var("_TEST_MULTI_A");
        std::env::remove_var("_TEST_MULTI_B");
        assert_eq!(result, "a = \"val-a\"\nb = \"val-b\"");
    }

    #[test]
    fn env_interpolation_skips_commented_placeholder() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("_TEST_COMMENTED_UNSET");
        let result = expand_env_vars(
            "# token = \"${_TEST_COMMENTED_UNSET}\"   # export _TEST_COMMENTED_UNSET=tok\n",
        )
        .unwrap();
        assert_eq!(
            result,
            "# token = \"${_TEST_COMMENTED_UNSET}\"   # export _TEST_COMMENTED_UNSET=tok\n"
        );
    }

    #[test]
    fn env_interpolation_trailing_comment_after_value() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_TRAILING_COMMENT", "real-value");
        let result =
            expand_env_vars("x = \"${_TEST_TRAILING_COMMENT}\" # uses ${_TEST_TRAILING_COMMENT}\n")
                .unwrap();
        std::env::remove_var("_TEST_TRAILING_COMMENT");
        assert_eq!(
            result,
            "x = \"real-value\" # uses ${_TEST_TRAILING_COMMENT}\n"
        );
    }

    #[test]
    fn env_interpolation_two_vars_in_same_value() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_CONCAT_USER", "admin");
        std::env::set_var("_TEST_CONCAT_PASS", "s3cr3t");
        let result =
            expand_env_vars("url = \"${_TEST_CONCAT_USER}:${_TEST_CONCAT_PASS}@host\"").unwrap();
        std::env::remove_var("_TEST_CONCAT_USER");
        std::env::remove_var("_TEST_CONCAT_PASS");
        assert_eq!(result, "url = \"admin:s3cr3t@host\"");
    }

    #[test]
    fn env_interpolation_value_with_special_chars() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Real-world passwords contain @, /, =, :, !, +
        std::env::set_var("_TEST_SPECIAL_CHARS", "P@ss/w=rd:1!+x");
        let result = expand_env_vars("password = \"${_TEST_SPECIAL_CHARS}\"").unwrap();
        std::env::remove_var("_TEST_SPECIAL_CHARS");
        assert_eq!(result, "password = \"P@ss/w=rd:1!+x\"");
    }

    #[test]
    fn env_interpolation_double_dollar_not_brace_is_literal() {
        // $$ not followed by { → passes through as $$
        let result = expand_env_vars("x = \"$$VAR\"").unwrap();
        assert_eq!(result, "x = \"$$VAR\"");
    }

    #[test]
    fn env_interpolation_empty_input_is_ok() {
        let result = expand_env_vars("").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn env_interpolation_no_placeholders_is_unchanged() {
        let input = "[server]\nhost = \"0.0.0.0\"\nport = 8080\n";
        let result = expand_env_vars(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn env_interpolation_substituted_value_is_not_re_expanded() {
        let _guard = ENV_LOCK.lock().unwrap();
        // The substituted value itself may contain ${...} — it must NOT be re-expanded.
        std::env::set_var("_TEST_NO_REEXPAND", "${_TEST_EXPAND_BASIC}");
        let result = expand_env_vars("x = \"${_TEST_NO_REEXPAND}\"").unwrap();
        std::env::remove_var("_TEST_NO_REEXPAND");
        assert_eq!(result, "x = \"${_TEST_EXPAND_BASIC}\"");
    }

    #[test]
    fn env_interpolation_upstream_header_value() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("_TEST_API_KEY", "my-api-key-xyz");
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "api-keyed-npm"
        [registries.upstream_auth]
        type = "header"
        name = "X-API-Key"
        value = "${_TEST_API_KEY}"
        "#
        );
        let expanded = expand_env_vars(&toml).unwrap();
        std::env::remove_var("_TEST_API_KEY");
        let cfg: AppConfig = toml::from_str(&expanded).unwrap();
        assert!(matches!(
            cfg.registries[0].upstream_auth,
            Some(crate::schema::UpstreamAuthConfig::Header(ref h)) if h.value == "my-api-key-xyz"
        ));
    }

    #[test]
    fn env_interpolation_database_url_via_load() {
        let _guard = ENV_LOCK.lock().unwrap();
        let path = std::env::temp_dir().join("_batlehub_test_load_expand.toml");
        std::fs::write(
            &path,
            r#"
[server]
host = "127.0.0.1"
port = 8080

[database]
type = "postgresql"
url  = "${_TEST_LOAD_DB_URL}"

[storage]
type = "filesystem"
path = "./tmp"
"#,
        )
        .unwrap();
        std::env::set_var(
            "_TEST_LOAD_DB_URL",
            "postgresql://env-user:env-pass@db/mydb",
        );
        let cfg = load(&path).expect("load failed");
        std::env::remove_var("_TEST_LOAD_DB_URL");
        let _ = std::fs::remove_file(&path);
        assert_eq!(cfg.database.url, "postgresql://env-user:env-pass@db/mydb");
    }

    #[test]
    fn env_interpolation_unclosed_placeholder_errors() {
        let err = expand_env_vars("x = \"${UNCLOSED\"").unwrap_err();
        assert!(err.to_string().contains("unclosed"));
    }

    #[test]
    fn env_interpolation_empty_var_name_errors() {
        let err = expand_env_vars("x = \"${}\"").unwrap_err();
        assert!(err.to_string().contains("empty variable name"));
    }

    #[test]
    fn parse_config_with_upstream_bearer_auth() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "private-npm"
        [registries.upstream_auth]
        type = "bearer"
        token = "secret-token"
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(cfg.registries[0].upstream_auth.is_some());
        assert!(matches!(
            cfg.registries[0].upstream_auth,
            Some(crate::schema::UpstreamAuthConfig::Bearer(_))
        ));
    }

    #[test]
    fn parse_config_with_upstream_basic_auth() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "cargo"
        name = "private-cargo"
        [registries.upstream_auth]
        type = "basic"
        username = "user"
        password = "pass"
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(matches!(
            cfg.registries[0].upstream_auth,
            Some(crate::schema::UpstreamAuthConfig::Basic(_))
        ));
    }

    #[test]
    fn parse_config_with_upstream_header_auth() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "api-keyed-npm"
        [registries.upstream_auth]
        type = "header"
        name = "X-API-Key"
        value = "my-key"
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(matches!(
            cfg.registries[0].upstream_auth,
            Some(crate::schema::UpstreamAuthConfig::Header(_))
        ));
    }

    #[test]
    fn parse_config_with_release_age_gate_rule() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npmjs"
        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 7200
        bypass_roles = ["admin"]
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert_eq!(cfg.registries[0].rules.len(), 1);
        assert!(matches!(
            cfg.registries[0].rules[0],
            crate::schema::RuleConfig::ReleaseAgeGate(_)
        ));
    }

    #[test]
    fn parse_config_firewall_only() {
        let toml = format!(
            "{}\n{}",
            minimal(),
            r#"
        [[registries]]
        type = "npm"
        name = "npmjs"
        firewall_only = true
        "#
        );
        let cfg: AppConfig = toml::from_str(&toml).unwrap();
        assert!(cfg.registries[0].firewall_only);
    }

    /// The S3 example config showcases every registry type and variant. Loading it
    /// through the real `load()` path (parse + env expansion + validate) guards it
    /// from drift and proves the new registry types/blocks are accepted. It must be
    /// self-contained (no `${VAR}` env placeholders) so it loads with no setup.
    #[test]
    fn example_s3_config_loads_and_validates() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.example-s3.toml");
        let cfg = load(path).expect("config.example-s3.toml must load and validate");
        let types: std::collections::HashSet<&str> = cfg
            .registries
            .iter()
            .map(|r| r.registry_type.as_str())
            .collect();
        for expected in [
            "github",
            "forgejo",
            "gitlab",
            "npm",
            "cargo",
            "goproxy",
            "openvsx",
            "vscode-marketplace",
            "maven",
            "rubygems",
            "terraform",
            "composer",
            "pypi",
            "conda",
            "nuget",
            "deb",
            "rpm",
        ] {
            assert!(
                types.contains(expected),
                "s3 example is missing a '{expected}' registry"
            );
        }
        // The signed deb/rpm hosting variants must carry a repo_signing key.
        assert!(cfg
            .registries
            .iter()
            .any(|r| r.registry_type == "deb" && r.repo_signing.is_some()));
        assert!(cfg
            .registries
            .iter()
            .any(|r| r.registry_type == "rpm" && r.repo_signing.is_some()));
    }

    /// The workspace config (`task run:space`), which unlike the others is *not*
    /// self-contained: it names no workspace, and reads the three URLs a Che
    /// workspace decides out of the environment. The task exports them; this
    /// test does the same so the file is still exercised by `cargo test`.
    ///
    /// The assertions are the two things that were silently wrong before, and
    /// that nothing else would catch — a config only has to parse to be broken
    /// here:
    ///
    ///   * both providers share ONE `redirect_uri`. There is a single callback
    ///     route and the provider comes from the stored login state, so a
    ///     per-provider path (`/auth/oidc2/callback`) is a 404 the browser
    ///     reaches after the password has already been typed.
    ///   * roles map off `email`. Dex's password DB emits no groups, so
    ///     `role_claim = "groups"` would resolve every login to anonymous.
    #[test]
    fn example_space_config_loads_and_validates() {
        let _guard = ENV_LOCK.lock().unwrap();
        for (k, v) in [
            ("BATLEHUB_FRONT_URL", "https://front.example.invalid"),
            ("BATLEHUB_BACK_URL", "https://back.example.invalid"),
            ("BATLEHUB_DEX_ISSUER", "https://dex.example.invalid"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("OIDC2_CLIENT_SECRET", "s"),
        ] {
            std::env::set_var(k, v);
        }

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config.example-space.toml"
        );
        let cfg = load(path).expect("config.example-space.toml must load and validate");

        let oidc: Vec<_> = cfg
            .auth
            .iter()
            .filter_map(|a| match a {
                AuthConfig::Oidc(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(oidc.len(), 2, "the workspace config declares oidc + oidc2");

        for c in &oidc {
            assert_eq!(
                c.redirect_uri.as_deref(),
                Some("https://back.example.invalid/api/v1/auth/oidc/callback"),
                "provider '{}' must come back to the one callback route",
                c.name
            );
            assert_eq!(c.issuer_url, "https://dex.example.invalid");
            assert_eq!(c.frontend_url, "https://front.example.invalid");
            assert_eq!(c.role_claim, "email", "dex's password DB emits no groups");
            assert_eq!(c.user_id_claim, "email");
            assert_eq!(
                c.role_mappings.get("admin@example.com").map(String::as_str),
                Some("admin")
            );
        }

        // The SPA is served from a different origin than the API in a workspace;
        // without its origin here every call fails on the preflight.
        let cors = cfg
            .server
            .cors_allowed_origins
            .as_ref()
            .expect("the workspace config must allow the front's origin");
        assert!(cors.iter().any(|o| o == "https://front.example.invalid"));

        for k in [
            "BATLEHUB_FRONT_URL",
            "BATLEHUB_BACK_URL",
            "BATLEHUB_DEX_ISSUER",
            "OIDC_CLIENT_SECRET",
            "OIDC2_CLIENT_SECRET",
        ] {
            std::env::remove_var(k);
        }
    }

    /// The example configs load and validate — including the grants hierarchy.
    ///
    /// `config.example-space.toml` had a test and `config.example.toml` did not,
    /// which is backwards: the latter is what `task run`, `task dump-spec` and
    /// the quickstart use, so a mistake in it breaks the first thing anybody
    /// does. It also shipped a `# actions:read — (future)` line for a verb that
    /// has never existed, and an operator uncommenting it got a server that
    /// would not start — the same failure the published guide had, in the file
    /// people copy from rather than the page they read.
    ///
    ///
    /// Load *and* validate, because they fail differently: TOML that parses can
    /// still name a verb outside the closed set, and expansion happens at load
    /// precisely so that is a startup error rather than a silent no-op.
    #[test]
    fn example_configs_load_and_validate() {
        let _guard = ENV_LOCK.lock().unwrap();
        for (k, v) in [
            ("BATLEHUB_FRONT_URL", "https://front.example.invalid"),
            ("BATLEHUB_BACK_URL", "https://back.example.invalid"),
            ("BATLEHUB_DEX_ISSUER", "https://dex.example.invalid"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("OIDC2_CLIENT_SECRET", "s"),
        ] {
            std::env::set_var(k, v);
        }

        // Every tracked config except `config.example-space.toml`, which has its
        // own test below because it needs three more env vars.
        //
        // `config.s3.toml` is here on purpose. It did not validate — its OIDC
        // issuer was `http://authentik-server:9000/…`, and plain HTTP is
        // accepted for loopback only, so the server refused to start on it. The
        // hostname turned out to be the smaller half of the bug: nothing in
        // `docker-compose.s3.yml` runs the server, so `postgres`, `rustfs`,
        // `authentik-server` and `jaeger` were all unreachable from the host
        // that actually runs it. A file no test loaded, describing a topology
        // that did not exist.
        for name in [
            "config.example.toml",
            "config.example-s3.toml",
            "config.s3.toml",
        ] {
            let path = format!("{}/../../{}", env!("CARGO_MANIFEST_DIR"), name);
            let cfg = load(&path).unwrap_or_else(|e| panic!("{name} must load and validate: {e}"));
            assert!(
                !cfg.registries.is_empty(),
                "{name} defines no registries, so loading it proves nothing"
            );
        }
    }

    /// The grants hierarchy in `config.example.toml` is the shape it documents.
    ///
    /// A commented-out example teaches without being checked. These blocks are
    /// live, so the test above already proves the verbs are real — this one
    /// proves the *structure* is still there, because deleting it would leave
    /// that test just as green.
    #[test]
    fn the_example_config_demonstrates_the_grant_tiers() {
        let _guard = ENV_LOCK.lock().unwrap();
        for (k, v) in [
            ("BATLEHUB_FRONT_URL", "https://front.example.invalid"),
            ("BATLEHUB_BACK_URL", "https://back.example.invalid"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("OIDC2_CLIENT_SECRET", "s"),
        ] {
            std::env::set_var(k, v);
        }

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.example.toml");
        let cfg = load(path).expect("config.example.toml must load");

        assert!(
            cfg.grants.as_ref().is_some_and(|g| !g.is_empty()),
            "the instance tier is the one an operator cannot discover from a \
             registry block, so the example has to show it"
        );

        let with_ns = cfg
            .registries
            .iter()
            .find(|r| !r.namespaces.is_empty())
            .expect("at least one registry must demonstrate `[[registries.namespaces]]`");
        assert!(
            with_ns.grants.as_ref().is_some_and(|g| !g.is_empty()),
            "and the registry tier beside it, or the example shows a namespace \
             with nothing to inherit from"
        );
        assert!(
            with_ns
                .namespaces
                .iter()
                .any(|n| n.grants.as_ref().is_some_and(|g| !g.is_empty())),
            "a namespace carrying grants is the whole point of the tier"
        );
    }
}
