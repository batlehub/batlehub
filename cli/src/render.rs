//! Output (RFC 0040 §4.3): `--format`, data on stdout, messages on stderr.
//!
//! The process-wide [`Context`] is resolved once in `main` from the global
//! options and the profile, then only read. A command asks it for the format
//! and the default registry instead of redefining `--json` or `-r`.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

use crate::exit::{CliError, ExitCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Format {
    #[default]
    Table,
    Json,
    Csv,
    Raw,
}

#[derive(Debug, Default)]
pub struct Context {
    pub format: Format,
    /// `-r` / `BATLEHUB_REGISTRY` / the profile's `registry`, in that order.
    pub registry: Option<String>,
    /// `-y/--yes`: answer yes to a confirmation (§4.5).
    pub yes: bool,
}

static CONTEXT: OnceLock<Context> = OnceLock::new();

pub fn init(ctx: Context) {
    let _ = CONTEXT.set(ctx);
}

/// The context `main` resolved, or the defaults before it did (unit tests).
pub fn ctx() -> &'static Context {
    CONTEXT.get_or_init(Context::default)
}

impl Context {
    /// Whether a command should print its data rather than its table:
    /// `json` and `csv` both start from the API's own JSON.
    pub fn structured(&self) -> bool {
        matches!(self.format, Format::Json | Format::Csv)
    }

    /// A command's registry filter: its own value if it still has one, else
    /// the global default.
    pub fn registry(&self, local: Option<String>) -> Option<String> {
        local.or_else(|| self.registry.clone())
    }

    /// The same, for a command that cannot run without one.
    pub fn require_registry(&self, local: Option<String>) -> Result<String, CliError> {
        self.registry(local).ok_or_else(|| {
            CliError::new(ExitCode::Usage, "no registry given").hint(
                "pass -r <registry>, set BATLEHUB_REGISTRY, or set `registry` in your profile",
            )
        })
    }
}

/// Print a command's data in the chosen format: JSON as the API sent it, or
/// CSV of a listing (a bare array, or an object's `items`).
pub fn data<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    let value = serde_json::to_value(value)?;
    match ctx().format {
        Format::Csv => {
            print!("{}", to_csv(&value)?);
            Ok(())
        }
        _ => {
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
    }
}

fn to_csv(value: &Value) -> Result<String, CliError> {
    let rows = match value {
        Value::Array(rows) => rows,
        Value::Object(o) if matches!(o.get("items"), Some(Value::Array(_))) => match &o["items"] {
            Value::Array(rows) => rows,
            _ => unreachable!("matched above"),
        },
        _ => {
            return Err(CliError::new(
                ExitCode::Usage,
                "--format csv is for listings; this command prints one object",
            )
            .hint("use --format json"))
        }
    };
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(o) = row {
            for k in o.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    let mut out = String::new();
    if columns.is_empty() {
        // A list of scalars: one column, named for what it is.
        out.push_str("value\n");
        for row in rows {
            out.push_str(&csv_cell(row));
            out.push('\n');
        }
        return Ok(out);
    }
    out.push_str(
        &columns
            .iter()
            .map(|c| csv_escape(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = columns
            .iter()
            .map(|c| row.get(c).map(csv_cell).unwrap_or_default())
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Ok(out)
}

fn csv_cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => csv_escape(s),
        other => csv_escape(&other.to_string()),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// `warning: …` on stderr, once per message per invocation.
pub fn warn(message: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Ok(mut seen) = seen.lock() {
        if !seen.insert(message.to_owned()) {
            return;
        }
    }
    eprintln!("warning: {message}");
}

/// The minor that removes the old spellings (RFC 0040 §13).
pub const ALIASES_REMOVED_IN: &str = "1.10";

/// The warning a deprecated command or flag prints, once.
pub fn deprecated(old: &str, new: &str) {
    warn(&format!(
        "`{old}` is deprecated and goes away in {ALIASES_REMOVED_IN}; use `{new}`"
    ));
}

/// Both ends a prompt needs: someone to type, and somewhere they can read it.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn csv_of_a_bare_array_uses_the_json_field_names() {
        let v = json!([{"name": "a,b", "n": 1, "x": null}, {"name": "q\"", "extra": [1]}]);
        assert_eq!(
            to_csv(&v).unwrap(),
            "n,name,x,extra\n1,\"a,b\",,\n,\"q\"\"\",,[1]\n"
        );
    }

    #[test]
    fn csv_of_a_page_reads_its_items() {
        let v = json!({"items": [{"a": 1}], "total": 1});
        assert_eq!(to_csv(&v).unwrap(), "a\n1\n");
        assert_eq!(to_csv(&json!(["x", "y"])).unwrap(), "value\nx\ny\n");
    }

    #[test]
    fn csv_of_one_object_is_a_usage_error() {
        let err = to_csv(&json!({"name": "x"})).unwrap_err();
        assert_eq!(err.exit, ExitCode::Usage);
    }

    #[test]
    fn a_local_registry_wins_over_the_default() {
        let ctx = Context {
            registry: Some("d".into()),
            ..Default::default()
        };
        assert_eq!(ctx.registry(Some("l".into())).as_deref(), Some("l"));
        assert_eq!(ctx.registry(None).as_deref(), Some("d"));
        assert_eq!(
            Context::default().require_registry(None).unwrap_err().exit,
            ExitCode::Usage
        );
    }
}
