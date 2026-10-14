//! The canonical TOML writer behind [`crate::merge_layers`] (RFC 0037 §6.2).
//!
//! `toml::to_string` cannot be used: it picks `'…'` or `"""…"""` for a string
//! on its own, and inside either `$` is not an escape, so a literal `$`
//! could not be kept literal. This writer emits every string as a one-line
//! basic string, every literal `$` as `\u0024`, and every placeholder marker
//! back as `${NAME}` — the only `${` the output ever contains.

use std::fmt::Write as _;

use toml::Value;

/// Opens a placeholder in a merged value: `MARK_OPEN NAME MARK_CLOSE` stands
/// for `${NAME}`. Private Use Area, refused in any layer that carries it.
pub(crate) const MARK_OPEN: char = '\u{E000}';
pub(crate) const MARK_CLOSE: char = '\u{E001}';

/// Render `value` (a document root) as canonical TOML.
///
/// Keys are sorted here rather than left to `toml::Table`: it is a `BTreeMap`
/// in this workspace, but a consumer whose graph enables `toml/preserve_order`
/// would get insertion order, and the hash of the output would move on a
/// reordering. Arrays keep their order — the order of `[[auth]]` means
/// something.
pub(crate) fn render(value: &Value) -> String {
    let mut out = String::new();
    if let Value::Table(table) = value {
        write_table(&mut out, &mut Vec::new(), table);
    }
    out
}

fn sorted(table: &toml::Table) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = table.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

fn is_array_of_tables(value: &Value) -> bool {
    matches!(value, Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_table))
}

/// Plain keys first (TOML requires them before any header), then each
/// sub-table and array of tables under its own header.
fn write_table(out: &mut String, path: &mut Vec<String>, table: &toml::Table) {
    let entries = sorted(table);
    for (k, v) in &entries {
        if !v.is_table() && !is_array_of_tables(v) {
            let _ = writeln!(out, "{} = {}", key(k), inline(v));
        }
    }
    for (k, v) in &entries {
        path.push(key(k));
        match v {
            Value::Table(sub) => {
                header(out, "[", path, "]");
                write_table(out, path, sub);
            }
            Value::Array(entries) if is_array_of_tables(v) => {
                for entry in entries.iter().filter_map(Value::as_table) {
                    header(out, "[[", path, "]]");
                    write_table(out, path, entry);
                }
            }
            _ => {}
        }
        path.pop();
    }
}

fn header(out: &mut String, open: &str, path: &[String], close: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(out, "{open}{}{close}", path.join("."));
}

fn inline(value: &Value) -> String {
    match value {
        Value::String(s) => quoted(s),
        Value::Integer(i) => i.to_string(),
        // `{:?}` keeps a fractional part or an exponent, so a float never
        // reads back as an integer; TOML spells NaN in lower case.
        Value::Float(f) if f.is_nan() => "nan".to_owned(),
        Value::Float(f) => format!("{f:?}"),
        Value::Boolean(b) => b.to_string(),
        Value::Datetime(d) => d.to_string(),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(inline).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Table(table) if table.is_empty() => "{}".to_owned(),
        Value::Table(table) => {
            let pairs: Vec<String> = sorted(table)
                .into_iter()
                .map(|(k, v)| format!("{} = {}", key(k), inline(v)))
                .collect();
            format!("{{ {} }}", pairs.join(", "))
        }
    }
}

fn key(k: &str) -> String {
    let bare = !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        k.to_owned()
    } else {
        quoted(k)
    }
}

/// A one-line basic string: markers back to `${NAME}`, every other `$` as
/// `\u0024`, and `"`, `\` and every control character escaped.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            MARK_OPEN => {
                out.push_str("${");
                // The name was checked against `[A-Za-z_][A-Za-z0-9_]*`.
                out.extend(chars.by_ref().take_while(|&c| c != MARK_CLOSE));
                out.push('}');
            }
            '$' => out.push_str("\\u0024"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Value {
        toml::from_str(raw).unwrap_or_else(|e| panic!("{e}\n---\n{raw}"))
    }

    fn round_trips(raw: &str) {
        let value = parse(raw);
        let rendered = render(&value);
        assert_eq!(parse(&rendered), value, "re-parse differs:\n{rendered}");
        assert_eq!(render(&parse(&rendered)), rendered, "not a fixed point");
    }

    #[test]
    fn every_value_kind_round_trips() {
        round_trips(
            r#"
            s = "text"
            i = -42
            f = 1.0
            big = 1e100
            small = 0.1
            neg_zero = -0.0
            inf = inf
            ninf = -inf
            b = true
            odt = 1979-05-27T07:32:00Z
            ldt = 1979-05-27T07:32:00
            ld = 1979-05-27
            lt = 07:32:00
            empty = []
            nested = [[1, 2], ["a"]]
            mixed = [1, { a = 1, b = { c = "d" } }]
            empty_inline = [{}]
            "weird key" = 1
            "" = 2
            [t]
            k = "v"
            [t.u.v]
            w = 1
            [[aot]]
            name = "x"
            [aot.sub]
            k = 1
            [[aot.inner]]
            z = 1
            [[aot]]
            name = "y"
            "#,
        );
    }

    #[test]
    fn nan_renders_lower_case() {
        let rendered = render(&parse("x = nan"));
        assert_eq!(rendered, "x = nan\n");
    }

    #[test]
    fn hard_strings_round_trip_on_one_line() {
        round_trips(
            "a = \"line\\nbreak\\ttab\\r\\b\\f\\u0001\\u007F\"\nb = \"quote \\\" back \\\\ slash\"\n",
        );
        let rendered = render(&parse("a = \"x\\ny\"\nm = \"\"\"\nmulti\nline\"\"\"\n"));
        assert_eq!(rendered.lines().count(), 2, "{rendered}");
    }

    #[test]
    fn a_literal_dollar_is_written_as_an_escape() {
        let value = parse(r#"a = "price $5 and ${X} and $${Y}""#);
        let rendered = render(&value);
        assert!(!rendered.contains('$'), "{rendered}");
        assert_eq!(parse(&rendered), value);
        round_trips(r#""k$y" = 1"#);
    }

    #[test]
    fn a_marker_is_written_back_as_a_placeholder() {
        let mut table = toml::Table::new();
        table.insert(
            "url".into(),
            Value::String(format!("https://{MARK_OPEN}HOST{MARK_CLOSE}/$x")),
        );
        assert_eq!(
            render(&Value::Table(table)),
            "url = \"https://${HOST}/\\u0024x\"\n"
        );
    }

    #[test]
    fn keys_render_sorted_whatever_the_insertion_order() {
        let a = render(&parse("b = 1\na = 2\n[z]\nk = 1\n[y]\nk = 2\n"));
        let b = render(&parse("[y]\nk = 2\n[z]\nk = 1\n"));
        assert_eq!(a, "a = 2\nb = 1\n\n[y]\nk = 2\n\n[z]\nk = 1\n");
        assert!(a.ends_with(&b));
    }

    #[test]
    fn an_empty_document_renders_empty() {
        assert_eq!(render(&parse("")), "");
    }
}
