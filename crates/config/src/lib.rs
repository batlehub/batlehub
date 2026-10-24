pub mod reload_keys;
mod render;
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
    load_layers_with(&[raw], &process_env)
}

/// An environment as the loaders read it. A `Result` rather than the
/// `Option` [`validate_layers`] takes so the process environment keeps
/// `VarError`'s "not valid unicode" apart from "not set".
type Env<'a> = &'a dyn Fn(&str) -> Result<String, std::env::VarError>;

/// The environment of the process this code runs in — what every loader
/// reads unless a caller hands [`validate_layers`] another one.
fn process_env(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}

/// Deserialize-time readers of `env` (the overrides, the sandbox opt-out)
/// only ask "set or not".
fn env_or_none(env: Env<'_>) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| env(name).ok()
}

/// The single-layer tail: parsed straight into `AppConfig` so a syntax error
/// keeps its line and column, which the `toml::Value` merge path loses.
fn config_from_str(raw: &str, env: Env<'_>) -> Result<AppConfig> {
    let expanded = expand_env_vars_with(raw, env)?;
    let mut config: AppConfig = toml::from_str(&expanded).with_context(|| "parsing config TOML")?;
    config.apply_env_overrides_from(&env_or_none(env));
    config.validate_with(&env_or_none(env))?;
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
    identity_key(base, overlay)
}

/// [`array_identity_key`] without its empty-side rule, which is the merge's
/// own: the reload diff walks an array one side of which is empty.
fn identity_key(base: &[toml::Value], overlay: &[toml::Value]) -> Option<&'static str> {
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

/// Read one layer: the file's bytes, placeholders still in place.
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
    load_layered_text(paths).map(|(config, _)| config)
}

/// [`load_layered`], plus the text of each layer as it was read.
///
/// Each file is opened once, so the text is exactly what the config was
/// built from — a FIFO or a `<(…)` process substitution has nothing left to
/// give a second read, and a second open of a FIFO blocks forever.
pub fn load_layered_text(paths: &[impl AsRef<Path>]) -> Result<(AppConfig, Vec<String>)> {
    if paths.is_empty() {
        bail!("no config file given");
    }
    let layers = paths
        .iter()
        .map(|p| read_layer(p.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    let config = match layers.as_slice() {
        [only] => load_from_str(only)?,
        _ => {
            let mut values = paths.iter().zip(&layers).map(|(p, raw)| {
                merged_layer_value(raw, &process_env)
                    .with_context(|| format!("parsing config file: {}", p.as_ref().display()))
            });
            // `paths` has at least two entries here, so the first is present.
            let mut merged = values.next().expect("at least two layers")?;
            for value in values {
                merge_value(&mut merged, value?);
            }
            config_from_value(merged, &process_env)?
        }
    };
    Ok((config, layers))
}

/// One layer as a TOML document, with its env placeholders already expanded.
fn merged_layer_value(raw: &str, env: Env<'_>) -> Result<toml::Value> {
    let expanded = expand_env_vars_with(raw, env)?;
    let value: toml::Value = toml::from_str(&expanded)?;
    Ok(value)
}

/// Deserialize, apply env overrides and validate — the tail of [`load_from_str`],
/// shared so a layered config goes through exactly the same checks as a single
/// file rather than a parallel set that could drift.
fn config_from_value(value: toml::Value, env: Env<'_>) -> Result<AppConfig> {
    let mut config: AppConfig = value
        .try_into()
        .with_context(|| "parsing merged config TOML")?;
    config.apply_env_overrides_from(&env_or_none(env));
    config.validate_with(&env_or_none(env))?;
    Ok(config)
}

/// Merge already-read layer contents, in order. The in-memory twin of
/// [`load_layered`], for the hot-reload path, which holds the primary layer's
/// text (from the file watcher or the config editor) and re-reads the rest.
pub fn load_layered_from_str(layers: &[impl AsRef<str>]) -> Result<AppConfig> {
    load_layers_with(layers, &process_env)
}

/// Load already-read layers against an environment the caller supplies.
///
/// Exactly [`load_layered_from_str`], with `env` in place of the process
/// environment at both places it is read: `${VAR}` expansion and the
/// `PROXY_CACHE__*` overrides. For a tool that prepares a configuration for
/// *another* BatleHub process (RFC 0037 §4.2): validating against its own
/// environment would refuse a `${VAR}` set only in the target and accept an
/// override set only in the tool. Read [`AppConfig::warnings`] on the result
/// for the advisories the server would log at startup.
///
/// `env` returning `None` for a `${VAR}` is the "is not set" error; for an
/// override, or `BATLEHUB_UNSAFE_NO_SANDBOX`, it means "not set". Never
/// reads `std::env`.
pub fn validate_layers(
    layers: &[impl AsRef<str>],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<AppConfig> {
    load_layers_with(layers, &|name| {
        env(name).ok_or(std::env::VarError::NotPresent)
    })
}

fn load_layers_with(layers: &[impl AsRef<str>], env: Env<'_>) -> Result<AppConfig> {
    match layers {
        [] => bail!("no config content given"),
        [only] => config_from_str(only.as_ref(), env),
        _ => {
            let mut values = layers.iter().map(|l| merged_layer_value(l.as_ref(), env));
            let mut merged = values.next().expect("at least two layers")?;
            for value in values {
                merge_value(&mut merged, value?);
            }
            config_from_value(merged, env)
        }
    }
}

// ── Merging for another process ───────────────────────────────────────────────
//
// RFC 0037 §5.2. Each layer is expanded with a *marking* environment that
// answers `${NAME}` with `MARK_OPEN NAME MARK_CLOSE`, parsed, and merged with
// `merge_value`; `render` turns each marker back into `${NAME}` and writes
// every other `$` as `\u0024`. So a placeholder stays a placeholder, a literal
// stays a literal, and no value is ever read.

/// Merge layers with the server's own rules and render one TOML document with
/// every `${NAME}` placeholder kept (RFC 0037 §4.1).
///
/// The output is canonical: sorted keys, every string a one-line basic string,
/// every literal `$` written `\u0024`. Two inputs that mean the same thing give
/// the same bytes, so a hash of it moves only when the configuration does.
///
/// Expanding the output under an environment gives the same value as expanding
/// each layer under it and merging — whenever the latter succeeds, and for
/// values that carry no `"`, `\` or control character, which would change the
/// TOML they are pasted into on either path — nor a `'` when the layer wrote
/// the placeholder in a single-quoted string, where the server's own read
/// fails. To hold that, it refuses what the
/// server's own loading accepts and the two paths would read differently: a
/// placeholder outside a string, inside a multi-line string, in a key, or in
/// the `name`/`type` of an array-of-tables entry; a variable name outside
/// `[A-Za-z_][A-Za-z0-9_]*`; and a layer carrying a marker character. Never
/// reads `std::env`.
pub fn merge_layers(layers: &[impl AsRef<str>]) -> Result<String> {
    let mut merged: Option<toml::Value> = None;
    for (i, layer) in layers.iter().enumerate() {
        let value = marked_layer_value(layer.as_ref())
            .with_context(|| format!("config layer {}", i + 1))?;
        match merged.as_mut() {
            Some(base) => merge_value(base, value),
            None => merged = Some(value),
        }
    }
    let Some(merged) = merged else {
        bail!("no config content given")
    };
    Ok(render::render(&merged))
}

/// One layer as a TOML document, each placeholder replaced by its marker.
fn marked_layer_value(raw: &str) -> Result<toml::Value> {
    check_placeholder_positions(raw)?;

    // A marker the layer carries itself would render back as a placeholder.
    // Checked on the parsed value, so `"\uE000"` is seen as well as the raw
    // character; the empty answer keeps the layer's own text apart from ours.
    let plain = expand_env_vars_with(raw, &|_| Ok(String::new()))?;
    let plain: toml::Value = toml::from_str(&plain).context("parsing config TOML")?;
    let mut carries_marker = false;
    for_each_text(&plain, &mut |text, _| {
        carries_marker |= text.contains([render::MARK_OPEN, render::MARK_CLOSE]);
    });
    if carries_marker {
        bail!("the layer contains U+E000 or U+E001, which merging reserves for placeholders");
    }

    let bad_name = std::cell::RefCell::new(None::<String>);
    let marking = |name: &str| {
        if !is_placeholder_name(name) {
            bad_name.borrow_mut().get_or_insert_with(|| name.to_owned());
        }
        Ok(format!("{}{name}{}", render::MARK_OPEN, render::MARK_CLOSE))
    };
    let marked = expand_env_vars_with(raw, &marking)?;
    if let Some(name) = bad_name.into_inner() {
        bail!(
            "placeholder '{}': a variable name to merge must match \
             [A-Za-z_][A-Za-z0-9_]*",
            shown_placeholder(&format!("{name}}}"))
        );
    }
    let value: toml::Value = toml::from_str(&marked).context("parsing config TOML")?;
    check_marker_positions(&value)?;
    Ok(value)
}

/// `${NAME}` for an error message, `after_open` being the text after the
/// `${`. The name is cut at the first character a variable name cannot hold:
/// the server's scanner reads a placeholder through the next `}`, however far
/// down the layer, and an error message carrying that text ends up in logs,
/// the console and the audit table — with whatever credential sat in between.
fn shown_placeholder(after_open: &str) -> String {
    let name: String = after_open
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    let end = if after_open[name.len()..].starts_with('}') {
        "}"
    } else {
        "…"
    };
    format!("${{{name}{end}")
}

/// Whether `expand_env_vars` expands a `${` that follows `line`, the text
/// since the last newline. Its scanner resets on every newline, so a `#` on a
/// line inside a multi-line string reads to it as a comment, and a `$$` is
/// its escape wherever it stands.
fn server_expands(line: &[u8]) -> bool {
    let (mut dquote, mut squote) = (false, false);
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'"' if !squote => dquote = !dquote,
            b'\\' if dquote => i += 1,
            b'\'' if !dquote => squote = !squote,
            b'#' if !dquote && !squote => return false,
            b'$' if line.get(i + 1) == Some(&b'$') => i += 1,
            b'$' if line.get(i + 1) == Some(&b'{') => {
                match line[i..].iter().position(|&b| b == b'}') {
                    Some(end) => i += end,
                    None => return true,
                }
            }
            _ => {}
        }
        i += 1;
    }
    true
}

fn is_placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every key (`true`) and string (`false`) in `value`, depth first.
fn for_each_text(value: &toml::Value, f: &mut dyn FnMut(&str, bool)) {
    match value {
        toml::Value::String(s) => f(s, false),
        toml::Value::Array(items) => items.iter().for_each(|v| for_each_text(v, f)),
        toml::Value::Table(table) => {
            for (k, v) in table {
                f(k, true);
                for_each_text(v, f);
            }
        }
        _ => {}
    }
}

/// `MARK_OPEN NAME MARK_CLOSE` back to `${NAME}`, for an error message.
fn unmark(text: &str) -> String {
    text.replace(render::MARK_OPEN, "${")
        .replace(render::MARK_CLOSE, "}")
}

/// Refuse a marker where the merge compares text: a key, and the `name` or
/// `type` an array-of-tables entry is merged on. `name = "${REG}"` beside
/// `name = "npm"` is two entries as markers and one on a server with
/// `REG=npm` (RFC 0037 §4.1).
fn check_marker_positions(value: &toml::Value) -> Result<()> {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                if k.contains(render::MARK_OPEN) {
                    bail!(
                        "placeholder in the key '{}': merging compares keys as text",
                        unmark(k)
                    );
                }
                if let toml::Value::Array(items) = v {
                    check_identity_markers(k, items)?;
                }
                check_marker_positions(v)?;
            }
        }
        toml::Value::Array(items) => items.iter().try_for_each(check_marker_positions)?,
        _ => {}
    }
    Ok(())
}

/// The `name`/`type` of each entry in the array of tables `key`, which the
/// merge matches entries on as text.
fn check_identity_markers(key: &str, items: &[toml::Value]) -> Result<()> {
    for entry in items.iter().filter_map(toml::Value::as_table) {
        for id in IDENTITY_KEYS {
            let Some(s) = entry.get(id).and_then(toml::Value::as_str) else {
                continue;
            };
            if s.contains(render::MARK_OPEN) {
                bail!(
                    "placeholder in `{key}` entry's `{id} = \"{}\"`: merging \
                     matches entries on it as text",
                    unmark(s)
                );
            }
        }
    }
    Ok(())
}

/// Refuse a `${` the merge cannot keep as a placeholder: outside any string,
/// where TOML would refuse its marker, and inside a multi-line string, where
/// the server expands it in a different string context from the one-line
/// basic string it is rendered into (RFC 0037 §4.1). `expand_env_vars`'s own
/// scanner resets on every newline and so cannot tell; this one tracks the
/// multi-line delimiters, and is used here only.
fn check_placeholder_positions(raw: &str) -> Result<()> {
    #[derive(Clone, Copy, PartialEq)]
    enum At {
        Code,
        Basic,
        Literal,
        MultiBasic,
        MultiLiteral,
        Comment,
    }
    let bytes = raw.as_bytes();
    let name_at = |i: usize| shown_placeholder(&raw[i + 2..]);
    let run_of = |i: usize, quote: u8| bytes[i..].iter().take_while(|&&b| b == quote).count();
    let mut at = At::Code;
    let mut i = 0;
    while i < bytes.len() {
        // `$$` is the escape for a literal `$` in every context the server
        // expands in, so its second `$` never opens a placeholder.
        if at != At::Comment && bytes[i..].starts_with(b"$$") {
            i += 2;
            continue;
        }
        let placeholder = bytes[i..].starts_with(b"${") && {
            let line_start = raw[..i].rfind('\n').map_or(0, |n| n + 1);
            server_expands(&bytes[line_start..i])
        };
        match (at, bytes[i]) {
            (At::Code, b'"' | b'\'') => {
                let quote = bytes[i];
                let multi = bytes[i..].starts_with(&[quote; 3]);
                at = match (quote, multi) {
                    (b'"', true) => At::MultiBasic,
                    (b'"', false) => At::Basic,
                    (_, true) => At::MultiLiteral,
                    (_, false) => At::Literal,
                };
                i += if multi { 3 } else { 1 };
                continue;
            }
            (At::Code, b'#') => at = At::Comment,
            (At::Code, _) if placeholder => bail!(
                "placeholder '{}' outside a string value: merging can keep a placeholder \
                 only inside a string",
                name_at(i)
            ),
            (At::Basic | At::MultiBasic, b'\\') => {
                i += 2;
                continue;
            }
            (At::Basic, b'"') | (At::Literal, b'\'') => at = At::Code,
            (At::MultiBasic, b'"') | (At::MultiLiteral, b'\'') => {
                let run = run_of(i, bytes[i]);
                if run >= 3 {
                    at = At::Code;
                }
                i += run;
                continue;
            }
            (At::MultiBasic | At::MultiLiteral, _) if placeholder => bail!(
                "placeholder '{}' inside a multi-line string: write the value as a \
                 one-line string to merge it",
                name_at(i)
            ),
            // The server's scanner reads a placeholder through its `}`, quote
            // or not; follow it so `${A"B}` reaches the name check.
            (At::Basic, _) if placeholder => {
                match bytes[i..].iter().position(|&b| b == b'}') {
                    Some(end) => i += end + 1,
                    None => return Ok(()), // the expansion reports it unclosed
                }
                continue;
            }
            (At::Basic | At::Literal | At::Comment, b'\n') => at = At::Code,
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// Expand `${VAR_NAME}` placeholders in a raw config string with their
/// environment variable values.
///
/// Rules:
/// - `${VAR_NAME}` is replaced with `env("VAR_NAME")`.
///   Returns an error if the variable is not set.
/// - `$${VAR_NAME}` is an escape sequence that produces the literal string
///   `${VAR_NAME}` without any variable lookup.
/// - Any other `$` character is left unchanged.
/// - Placeholders inside a TOML `#` comment (i.e. outside of a quoted
///   string) are left untouched — commented-out example lines never
///   require the referenced variable to be set.
///
/// Read `${VAR_NAME}` from `chars` (the `$` and `{` have already been consumed),
/// look up the variable in `env`, and return its value.
fn expand_braced_var(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    env: Env<'_>,
) -> Result<String> {
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
    env(&var_name)
        .with_context(|| format!("config references env var '${{{var_name}}}' but it is not set"))
}

/// Tracks TOML string/comment context while scanning, so a `#` inside a string
/// is not taken for a comment and a placeholder in a `#` comment is left alone.
/// A placeholder in a `'single'`-quoted string *is* expanded, as the
/// configuration guide promises for any string value; `in_squote` only stops
/// a `"` or `#` inside one from changing the state.
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

/// [`expand_env_vars_with`] against the process environment.
#[cfg(test)]
fn expand_env_vars(raw: &str) -> Result<String> {
    expand_env_vars_with(raw, &process_env)
}

fn expand_env_vars_with(raw: &str, env: Env<'_>) -> Result<String> {
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
                out.push_str(&expand_braced_var(&mut chars, env)?);
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
    ///
    /// That includes every test that only *loads* a config: the loaders apply
    /// `PROXY_CACHE__*` overrides from the process env, so a test setting one
    /// leaks into any load running beside it (a `port = 9090` once read 1111).
    /// Testing an override goes through `apply_env_overrides_from(&env_of(..))`
    /// instead, which needs neither the lock nor the process env.
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = crate::load_layered_from_str(&[two_registries(), "registries = []\n".to_owned()])
            .expect("layered load failed");

        assert!(cfg.registries.is_empty());
    }

    /// With no unique identity on both sides the merge would have to guess
    /// which entry pairs with which, so the later layer replaces the array
    /// outright. Two `type = "token"` blocks are the realistic way to get here.
    #[test]
    fn an_array_with_a_repeated_identity_is_replaced_not_merged() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let base = layer_file(dir.path(), "config.toml", minimal());
        let creds = layer_file(
            dir.path(),
            "credentials.toml",
            "[database]\nurl = \"postgresql://real:secret@db/batlehub\"\n",
        );

        let cfg = crate::load_layered(&[base, creds]).expect("layered load failed");

        assert_eq!(cfg.database.url, "postgresql://real:secret@db/batlehub");
        // From the base: the later layer completed the table rather than
        // replacing it.
        assert_eq!(cfg.server.port, 8080);
    }

    /// A missing layer names the file. The whole point of the feature is that
    /// the two files have different lifecycles, so "one of them is not there"
    /// is a normal deployment mistake and the message has to say which.
    #[test]
    fn load_layered_names_the_file_it_could_not_read() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[(
            "PROXY_CACHE__DATABASE__URL",
            "postgresql://env-host/env-db",
        )]));
        assert_eq!(cfg.database.url, "postgresql://env-host/env-db");
    }

    #[test]
    fn env_override_replaces_server_port() {
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[("PROXY_CACHE__SERVER__PORT", "9090")]));
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

    // ── RFC 0037: merge_layers ───────────────────────────────────────────────

    fn merge_err(layers: &[&str]) -> String {
        format!("{:#}", crate::merge_layers(layers).unwrap_err())
    }

    #[test]
    fn merge_layers_keeps_placeholders_and_literals_apart() {
        let merged = crate::merge_layers(&[
            "[database]\nurl = \"postgresql://${DB_USER}@h/d\"\n",
            "[x]\nliteral = \"$${KEEP} costs $5\"\nsingle = '${TOKEN}'\nescaped = \"\\u0024{X}\"\n",
        ])
        .unwrap();
        assert_eq!(
            merged,
            "[database]\nurl = \"postgresql://${DB_USER}@h/d\"\n\n\
             [x]\nescaped = \"\\u0024{X}\"\n\
             literal = \"\\u0024{KEEP} costs \\u00245\"\n\
             single = \"${TOKEN}\"\n"
        );
        // In the pod: the two placeholders resolve, nothing else does. A
        // single-quoted string is expanded too, as on the server.
        let expanded =
            crate::expand_env_vars_with(&merged, &var_env_of(&[("DB_USER", "u"), ("TOKEN", "t")]))
                .unwrap();
        let value: toml::Value = toml::from_str(&expanded).unwrap();
        assert_eq!(
            value["database"]["url"].as_str(),
            Some("postgresql://u@h/d")
        );
        assert_eq!(value["x"]["literal"].as_str(), Some("${KEEP} costs $5"));
        assert_eq!(value["x"]["single"].as_str(), Some("t"));
        assert_eq!(value["x"]["escaped"].as_str(), Some("${X}"));
    }

    #[test]
    fn merge_layers_uses_the_server_merge_rules() {
        let merged = crate::merge_layers(&[
            two_registries().as_str(),
            "[[registries]]\nname = \"crates\"\nupstream_auth = { token = \"${PYPI_TOKEN}\" }\n",
        ])
        .unwrap();
        let value: toml::Value = toml::from_str(
            &crate::expand_env_vars_with(&merged, &var_env_of(&[("PYPI_TOKEN", "t")])).unwrap(),
        )
        .unwrap();
        let regs = value["registries"].as_array().unwrap();
        assert_eq!(regs.len(), 2, "completed in place, not appended");
        assert_eq!(regs[1]["upstream_auth"]["token"].as_str(), Some("t"));
    }

    #[test]
    fn merge_layers_output_is_canonical() {
        let a = crate::merge_layers(&["b = 1 # comment\na = \"x\"\n"]).unwrap();
        let b = crate::merge_layers(&["a = 'x'\n\n\nb = 1\n"]).unwrap();
        assert_eq!(a, b);
        assert_eq!(crate::merge_layers(&[&a]).unwrap(), a, "a fixed point");
    }

    #[test]
    fn merge_layers_with_no_layers_is_an_error() {
        let none: [&str; 0] = [];
        assert!(crate::merge_layers(&none).is_err());
    }

    #[test]
    fn merge_layers_refuses_what_the_two_paths_would_read_differently() {
        let cases: &[(&str, &str)] = &[
            ("a = \"unclosed\n", "parsing config TOML"),
            ("port = ${PORT}\n", "'${PORT}' outside a string value"),
            ("x = \"${}\"\n", "empty variable name"),
            ("x = \"${OPEN\n", "unclosed"),
            ("\"${K}\" = 1\n", "in the key '${K}'"),
            ("[[registries]]\nname = \"${REG}\"\n", "`name = \"${REG}\"`"),
            ("[[auth]]\ntype = \"${T}\"\n", "`type = \"${T}\"`"),
            ("x = \"${A\"B}\"\n", "'${A…'"),
            // A `$$` escape does not hide the placeholder after it.
            (
                "p = \"$${\"\nx = \"\"\"${PEM}\"\"\"\n",
                "'${PEM}' inside a multi-line string",
            ),
            ("x = \"${1X}\"\n", "'${1X}'"),
            ("x = '''\nline ${X}'''\n", "inside a multi-line string"),
            (
                "x = \"\"\"\nline ${X}\"\"\"\n",
                "inside a multi-line string",
            ),
            ("x = '''${X}'''\n", "inside a multi-line string"),
            ("x = \"\\uE000X\\uE001\"\n", "U+E000 or U+E001"),
            ("x = \"\u{E000}X\u{E001}\"\n", "U+E000 or U+E001"),
            ("\"\\uE000\" = 1\n", "U+E000 or U+E001"),
        ];
        for (layer, expected) in cases {
            let err = merge_err(&["a = 1\n", layer]);
            assert!(err.contains("config layer 2"), "{layer:?}: {err}");
            assert!(err.contains(expected), "{layer:?}: {err}");
        }
    }

    /// The server reads a placeholder through the next `}`, however far; a
    /// refusal quoting that text would carry a password into the logs, the
    /// console and the audit table.
    #[test]
    fn a_refusal_never_quotes_past_the_placeholder_name() {
        let layer = "x = '''\nopen ${ here\n'''\n\
                     [database]\nurl = \"postgresql://u:hunter2@db/x\"\n# }\n";
        let err = merge_err(&[layer]);
        assert!(err.contains("'${…'"), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        let layer = "x = \"${A\nurl = \"postgresql://u:hunter2@db/x\"}\"\n";
        let err = merge_err(&[layer]);
        assert!(!err.contains("hunter2"), "{err}");
    }

    #[test]
    fn merge_layers_accepts_what_only_looks_like_a_placeholder() {
        for layer in [
            "x = \"$${X}\"\n",
            "x = '${X}'\n",
            "# ${X} in a comment\nx = 1\n",
            "x = \"a\" # ${X}\n",
            "x = '''\nno placeholder, a \" quote and a $\n'''\n",
            "x = \"\"\"\nescaped \\\"\"\" quote\"\"\"\n",
            "[[registries]]\nname = \"npm\"\nupstreams = [\"${NPM_URL}\"]\n",
            // A line the server's scanner reads as a comment, and an escape.
            "x = '''\n# Use ${ to open a template\n'''\n",
            "x = \"\"\"\n$${X}\"\"\"\n",
        ] {
            crate::merge_layers(&[layer]).unwrap_or_else(|e| panic!("{layer:?}: {e:#}"));
        }
    }

    /// A seeded xorshift, so the property test needs no dependency and a
    /// failure names the seed that reproduces it.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[self.below(items.len())]
        }
    }

    fn gen_string(rng: &mut Rng) -> String {
        // Multi-line strings carry no placeholder (merging refuses one), but
        // their quotes and `#` must not throw the position check off.
        if rng.below(8) == 0 {
            let parts = ["text", "\"", "'", "#", "$", "\n", "$5"];
            let body: String = (0..rng.below(5)).map(|_| rng.pick(&parts)).collect();
            return match rng.below(2) {
                0 => format!("'''\n{body}'''"),
                _ => format!("\"\"\"\n{}\"\"\"", body.replace('"', "\\\"")),
            };
        }
        if rng.below(4) == 0 {
            let parts = ["text", "${V1}", "$", "\"", "#", "$${V2}"];
            let body: String = (0..rng.below(4)).map(|_| rng.pick(&parts)).collect();
            return format!("'{body}'");
        }
        let parts = [
            "text",
            "${V1}",
            "${V2}",
            "${V3}",
            "$",
            "$5",
            "$${V1}",
            "$$",
            "\\\"",
            "\\\\",
            "\\u0024",
            "\\u0024{V1}",
            "#",
            "'",
            "\\n",
            "é",
        ];
        let body: String = (0..rng.below(5)).map(|_| rng.pick(&parts)).collect();
        format!("\"{body}\"")
    }

    fn gen_value(rng: &mut Rng) -> String {
        match rng.below(5) {
            0 => rng.below(100).to_string(),
            1 => "true".to_owned(),
            2 => {
                let items: Vec<String> = (0..rng.below(3)).map(|_| gen_string(rng)).collect();
                format!("[{}]", items.join(", "))
            }
            _ => gen_string(rng),
        }
    }

    fn gen_pairs(rng: &mut Rng, out: &mut String) {
        for key in ["a", "b", "\"q$k\"", "c"] {
            if rng.below(2) == 0 {
                out.push_str(&format!("{key} = {}\n", gen_value(rng)));
            }
        }
    }

    fn gen_layer(rng: &mut Rng) -> String {
        let mut out = String::new();
        gen_pairs(rng, &mut out);
        if rng.below(2) == 0 {
            out.push_str("[t]\n");
            gen_pairs(rng, &mut out);
        }
        for _ in 0..rng.below(4) {
            out.push_str("[[r]]\n");
            match rng.below(3) {
                0 => out.push_str(&format!("type = \"{}\"\n", rng.pick(&["x", "y"]))),
                _ => out.push_str(&format!("name = \"{}\"\n", rng.pick(&["x", "y", "z"]))),
            }
            gen_pairs(rng, &mut out);
            if rng.below(3) == 0 {
                out.push_str("[r.s]\n");
                gen_pairs(rng, &mut out);
            }
        }
        out
    }

    /// RFC 0037 §4.1: expanding `merge_layers(L)` under `E` gives what the
    /// server gets by expanding each layer under `E` and merging, whenever the
    /// latter succeeds — for values with no `"`, `\` or control character.
    #[test]
    fn merging_before_expanding_matches_the_server() {
        let values = [
            "", "plain", "${V2}", "$", "$$", "{}", "#hash", "it's", "é/ü:1",
        ];
        let mut skipped = 0;
        for seed in 1..=3000u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let layers: Vec<String> = (0..1 + rng.below(3)).map(|_| gen_layer(&mut rng)).collect();
            let pairs: Vec<(&str, &str)> = ["V1", "V2", "V3"]
                .into_iter()
                .map(|name| (name, rng.pick(&values)))
                .collect();
            let env = var_env_of(&pairs);

            // The server path can fail where the merged one does not — a `'`
            // pasted into a single-quoted string — and the invariant is
            // claimed only where it succeeds.
            let mut server = None::<toml::Value>;
            let mut failed = false;
            for layer in &layers {
                let Ok(value) = crate::merged_layer_value(layer, &env) else {
                    failed = true;
                    break;
                };
                match server.as_mut() {
                    Some(base) => crate::merge_value(base, value),
                    None => server = Some(value),
                }
            }
            if failed {
                skipped += 1;
                continue;
            }

            let merged = crate::merge_layers(&layers)
                .unwrap_or_else(|e| panic!("seed {seed}: {e:#}\n{layers:#?}"));
            let pod: toml::Value =
                toml::from_str(&crate::expand_env_vars_with(&merged, &env).unwrap())
                    .unwrap_or_else(|e| panic!("seed {seed}: {e}\n{merged}"));
            assert_eq!(Some(pod), server, "seed {seed}\n{layers:#?}\n{merged}");
            assert_eq!(
                crate::merge_layers(&[&merged]).unwrap(),
                merged,
                "seed {seed}"
            );
        }
        assert!(
            skipped < 1000,
            "{skipped} of 3000 seeds never reached the comparison"
        );
    }

    // ── RFC 0037: an environment supplied by the caller ─────────────────────

    /// One environment with the given pairs and nothing else.
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    /// [`env_of`] as the internal loaders read an environment.
    fn var_env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Result<String, std::env::VarError> {
        let env = env_of(pairs);
        move |name| env(name).ok_or(std::env::VarError::NotPresent)
    }

    #[test]
    fn validate_layers_expands_placeholders_from_the_given_env_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        let raw = minimal().replace(
            "postgresql://user:pass@localhost/db",
            "postgresql://${BATLEHUB_TEST_0037_DB}/db",
        );
        // Set in this process, absent from the injected env: refused.
        unsafe { std::env::set_var("BATLEHUB_TEST_0037_DB", "from-the-process") };
        let refused = crate::validate_layers(&[&raw], &env_of(&[]));
        let given = crate::validate_layers(
            &[&raw],
            &env_of(&[("BATLEHUB_TEST_0037_DB", "from-the-target")]),
        );
        unsafe { std::env::remove_var("BATLEHUB_TEST_0037_DB") };

        let err = format!("{:#}", refused.unwrap_err());
        assert!(
            err.contains("BATLEHUB_TEST_0037_DB") && err.contains("not set"),
            "{err}"
        );
        assert_eq!(
            given.unwrap().database.url,
            "postgresql://from-the-target/db"
        );
    }

    #[test]
    fn validate_layers_applies_overrides_from_the_given_env_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("PROXY_CACHE__SERVER__PORT", "1111") };
        let ignored = crate::validate_layers(&[minimal()], &env_of(&[]));
        let applied = crate::validate_layers(
            &[minimal(), "[server]\nhost = \"0.0.0.0\"\n"],
            &env_of(&[("PROXY_CACHE__SERVER__PORT", "2222")]),
        );
        unsafe { std::env::remove_var("PROXY_CACHE__SERVER__PORT") };

        assert_eq!(ignored.unwrap().server.port, 8080);
        let applied = applied.unwrap();
        assert_eq!(applied.server.port, 2222);
        assert_eq!(
            applied.server.host, "0.0.0.0",
            "the second layer was merged"
        );
    }

    #[test]
    fn validate_layers_with_no_layers_is_an_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let none: [&str; 0] = [];
        assert!(crate::validate_layers(&none, &env_of(&[])).is_err());
    }

    /// Every `PROXY_CACHE__*` override reads the injected env, one at a time,
    /// so a helper that went back to `std::env` fails here by name.
    #[test]
    fn every_env_override_reads_the_given_env() {
        let s3 = r#"
        [server]
        host = "127.0.0.1"
        [database]
        type = "postgresql"
        url = "postgresql://u:p@h/d"
        [storage]
        type = "s3"
        bucket = "b"
        region = "r"
        "#;
        let fs_path = |c: &AppConfig| match &c.storage {
            StoragesConfig::Single(StorageBackendConfig::Filesystem(fs)) => fs.path.clone(),
            _ => panic!("expected filesystem storage"),
        };
        let s3_field =
            |c: &AppConfig, f: fn(&crate::schema::S3StorageConfig) -> String| match &c.storage {
                StoragesConfig::Single(StorageBackendConfig::S3(s3)) => f(s3),
                _ => panic!("expected s3 storage"),
            };
        fn proxy(c: &AppConfig) -> &crate::schema::UpstreamProxyConfig {
            c.proxy.as_ref().expect("proxy section")
        }
        type Read = Box<dyn Fn(&AppConfig) -> String>;
        /// The variable, its value, the base document, the other variables it
        /// needs set beside it, and where its value lands.
        type Case<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)], Read);
        let with_proxy = [("PROXY_CACHE__PROXY__URL", "http://p:3128")];
        let cases: Vec<Case> = vec![
            (
                "PROXY_CACHE__SERVER__HOST",
                "h",
                minimal(),
                &[],
                Box::new(|c| c.server.host.clone()),
            ),
            (
                "PROXY_CACHE__SERVER__PORT",
                "9",
                minimal(),
                &[],
                Box::new(|c| c.server.port.to_string()),
            ),
            (
                "PROXY_CACHE__SERVER__STATIC_DIR",
                "/s",
                minimal(),
                &[],
                Box::new(|c| c.server.static_dir.clone().unwrap()),
            ),
            (
                "PROXY_CACHE__DATABASE__URL",
                "postgresql://x/y",
                minimal(),
                &[],
                Box::new(|c| c.database.url.clone()),
            ),
            (
                "PROXY_CACHE__DATABASE__MAX_CONNECTIONS",
                "7",
                minimal(),
                &[],
                Box::new(|c| c.database.max_connections.to_string()),
            ),
            (
                "PROXY_CACHE__DATABASE__MIN_CONNECTIONS",
                "3",
                minimal(),
                &[],
                Box::new(|c| c.database.min_connections.to_string()),
            ),
            (
                "PROXY_CACHE__DATABASE__ACQUIRE_TIMEOUT_SECS",
                "11",
                minimal(),
                &[],
                Box::new(|c| c.database.acquire_timeout_secs.to_string()),
            ),
            (
                "PROXY_CACHE__STORAGE__PATH",
                "/p",
                minimal(),
                &[],
                Box::new(fs_path),
            ),
            (
                "PROXY_CACHE__STORAGE__BUCKET",
                "nb",
                s3,
                &[],
                Box::new(move |c| s3_field(c, |s| s.bucket.clone())),
            ),
            (
                "PROXY_CACHE__STORAGE__REGION",
                "nr",
                s3,
                &[],
                Box::new(move |c| s3_field(c, |s| s.region.clone())),
            ),
            (
                "PROXY_CACHE__STORAGE__ENDPOINT_URL",
                "http://e",
                s3,
                &[],
                Box::new(move |c| s3_field(c, |s| s.endpoint_url.clone().unwrap())),
            ),
            (
                "PROXY_CACHE__OTEL__ENDPOINT",
                "http://o",
                minimal(),
                &[],
                Box::new(|c| c.otel.as_ref().unwrap().endpoint.clone()),
            ),
            (
                "PROXY_CACHE__OTEL__SERVICE_NAME",
                "n",
                minimal(),
                &[("PROXY_CACHE__OTEL__ENDPOINT", "http://o")],
                Box::new(|c| c.otel.as_ref().unwrap().service_name.clone()),
            ),
            (
                "PROXY_CACHE__PROXY__URL",
                "http://p:3128",
                minimal(),
                &[],
                Box::new(move |c| proxy(c).url.clone()),
            ),
            (
                "PROXY_CACHE__PROXY__USERNAME",
                "u",
                minimal(),
                &with_proxy,
                Box::new(move |c| proxy(c).username.clone().unwrap()),
            ),
            (
                "PROXY_CACHE__PROXY__PASSWORD",
                "pw",
                minimal(),
                &with_proxy,
                Box::new(move |c| proxy(c).password.clone().unwrap()),
            ),
            (
                "PROXY_CACHE__PROXY__NO_PROXY",
                "localhost",
                minimal(),
                &with_proxy,
                Box::new(move |c| proxy(c).no_proxy.clone().unwrap()),
            ),
        ];
        assert_eq!(cases.len(), 17, "one case per override (RFC 0037 §10)");
        for (var, value, base, extra, read) in cases {
            let mut pairs = extra.to_vec();
            pairs.push((var, value));
            let mut cfg: AppConfig = toml::from_str(base).unwrap();
            cfg.apply_env_overrides_from(&env_of(&pairs));
            assert_eq!(read(&cfg), value, "{var} did not reach its field");
        }
    }

    #[test]
    fn env_override_filesystem_storage_path() {
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[("PROXY_CACHE__STORAGE__PATH", "/new/path")]));
        if let StoragesConfig::Single(StorageBackendConfig::Filesystem(fs)) = &cfg.storage {
            assert_eq!(fs.path, "/new/path");
        } else {
            panic!("expected filesystem storage");
        }
    }

    #[test]
    fn env_override_s3_storage_fields() {
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
        cfg.apply_env_overrides_from(&env_of(&[
            ("PROXY_CACHE__STORAGE__BUCKET", "new-bucket"),
            ("PROXY_CACHE__STORAGE__REGION", "us-east-1"),
            ("PROXY_CACHE__STORAGE__ENDPOINT_URL", "http://rustfs:9000"),
        ]));
        if let StoragesConfig::Single(StorageBackendConfig::S3(s3)) = &cfg.storage {
            assert_eq!(s3.bucket, "new-bucket");
            assert_eq!(s3.region, "us-east-1");
            assert_eq!(s3.endpoint_url.as_deref(), Some("http://rustfs:9000"));
        } else {
            panic!("expected s3 storage");
        }
    }

    #[test]
    fn env_override_otel_creates_section_when_absent() {
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        assert!(cfg.otel.is_none());
        cfg.apply_env_overrides_from(&env_of(&[(
            "PROXY_CACHE__OTEL__ENDPOINT",
            "http://otel:4317",
        )]));
        assert_eq!(cfg.otel.as_ref().unwrap().endpoint, "http://otel:4317");
        assert_eq!(cfg.otel.as_ref().unwrap().service_name, "batlehub");
    }

    #[test]
    fn env_override_otel_service_name_when_section_present() {
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
        cfg.apply_env_overrides_from(&env_of(&[("PROXY_CACHE__OTEL__SERVICE_NAME", "new-name")]));
        assert_eq!(cfg.otel.as_ref().unwrap().service_name, "new-name");
    }

    #[test]
    fn env_override_static_dir() {
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[("PROXY_CACHE__SERVER__STATIC_DIR", "/var/www")]));
        assert_eq!(cfg.server.static_dir.as_deref(), Some("/var/www"));
    }

    #[test]
    fn env_override_database_max_connections() {
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[("PROXY_CACHE__DATABASE__MAX_CONNECTIONS", "25")]));
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
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[("PROXY_CACHE__DATABASE__MIN_CONNECTIONS", "3")]));
        assert_eq!(cfg.database.min_connections, 3);
    }

    #[test]
    fn env_override_database_acquire_timeout_secs() {
        let mut cfg: AppConfig = toml::from_str(minimal()).unwrap();
        cfg.apply_env_overrides_from(&env_of(&[(
            "PROXY_CACHE__DATABASE__ACQUIRE_TIMEOUT_SECS",
            "5",
        )]));
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
