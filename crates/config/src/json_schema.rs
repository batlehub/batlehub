//! The JSON Schema of [`AppConfig`] (RFC 0049): what an editor validates a
//! `config.toml` against through its `#:schema` line, and what
//! `batlehub dump-config-schema` prints.
//!
//! Derived by schemars from the same structs serde deserializes, then two
//! passes the derive cannot know about:
//!
//! - **TOML has no null.** schemars writes `Option<T>` as `type: [T, "null"]`
//!   or `anyOf: [T, {type: null}]`. A null cannot be written in TOML, and the
//!   extra arm turns a typo's diagnostic into "not valid under any of the
//!   schemas listed in the 'anyOf' keyword", so the arm is removed.
//! - **`$ref` beside validation keywords becomes `allOf`.** schemars writes a
//!   tagged enum's newtype variant as `$ref` plus the tag's `properties`;
//!   draft-07 validators, taplo among them, ignore every keyword beside a
//!   `$ref`, so the tag vanished and each `oneOf` matched several variants
//!   (found by `taplo check` on `config.example.toml`, RFC 0049 §6.P).
//! - **The layer variant** drops every `required`: a credentials layer or any
//!   overlay is partial by design, and `required` only holds once the layers
//!   are merged (`load_layered_text` deserializes once, after the merge).

use serde_json::{Map, Value};

use crate::AppConfig;

/// The schema of a complete config (`layer = false`), or of one layer of a
/// layered config (`layer = true`).
pub fn json_schema(layer: bool) -> Value {
    let mut schema = schemars::schema_for!(AppConfig).to_value();
    strip_null(&mut schema);
    ref_siblings_to_all_of(&mut schema);
    if layer {
        strip_required(&mut schema);
    }
    schema
}

/// [`json_schema`] as the committed files hold it: pretty-printed, with a
/// trailing newline, so `dump-config-schema > file` is byte-identical.
pub fn json_schema_text(layer: bool) -> String {
    let mut text = serde_json::to_string_pretty(&json_schema(layer))
        .unwrap_or_else(|e| unreachable!("a serde_json::Value always serializes: {e}"));
    text.push('\n');
    text
}

fn is_null_schema(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("null")
}

fn strip_null(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get_mut("type") {
                types.retain(|t| t.as_str() != Some("null"));
                if let [only] = types.as_slice() {
                    let only = only.clone();
                    map.insert("type".into(), only);
                }
            }
            if map.get("default").is_some_and(Value::is_null) {
                map.remove("default");
            }
            for key in ["anyOf", "oneOf"] {
                let Some(Value::Array(arms)) = map.get_mut(key) else {
                    continue;
                };
                let had_null = arms.iter().any(is_null_schema);
                arms.retain(|arm| !is_null_schema(arm));
                // `Option<Struct>`: the one arm left is the schema itself.
                if had_null && arms.len() == 1 {
                    if let Some(Value::Object(arm)) = arms.pop() {
                        map.remove(key);
                        merge_into(map, arm);
                    }
                }
            }
            map.values_mut().for_each(strip_null);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_null),
        _ => {}
    }
}

/// The arm's keywords onto the node holding it; the node's own keywords
/// (`description`, `default`) win, since they describe this field.
fn merge_into(node: &mut Map<String, Value>, arm: Map<String, Value>) {
    for (k, v) in arm {
        node.entry(k).or_insert(v);
    }
}

/// Keywords that only describe a node; beside a `$ref` they lose nothing when
/// a validator ignores them.
const ANNOTATIONS: [&str; 5] = ["description", "default", "title", "examples", "deprecated"];

fn ref_siblings_to_all_of(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.values_mut().for_each(ref_siblings_to_all_of);
            let validates_beside_ref = map.contains_key("$ref")
                && map
                    .keys()
                    .any(|k| k != "$ref" && !ANNOTATIONS.contains(&k.as_str()));
            if validates_beside_ref {
                let reference = map.remove("$ref").unwrap_or(Value::Null);
                let mut rest = Map::new();
                for k in map.keys().cloned().collect::<Vec<_>>() {
                    if !ANNOTATIONS.contains(&k.as_str()) {
                        if let Some(val) = map.remove(&k) {
                            rest.insert(k, val);
                        }
                    }
                }
                let mut arms = vec![serde_json::json!({ "$ref": reference })];
                if let Some(Value::Array(existing)) = rest.remove("allOf") {
                    arms.extend(existing);
                }
                arms.push(Value::Object(rest));
                map.insert("allOf".into(), Value::Array(arms));
            }
        }
        Value::Array(items) => items.iter_mut().for_each(ref_siblings_to_all_of),
        _ => {}
    }
}

/// Every `required` goes, except an enum's tag (a property with a `const`):
/// an entry a layer adds must still say which variant it is, or serde cannot
/// read it, and without the tag every variant of a `oneOf` would match.
fn strip_required(v: &mut Value) {
    match v {
        Value::Object(map) => {
            let tags: Vec<Value> = map
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| {
                    props
                        .iter()
                        .filter(|(_, p)| p.get("const").is_some())
                        .map(|(k, _)| Value::String(k.clone()))
                        .collect()
                })
                .unwrap_or_default();
            match map.get_mut("required") {
                Some(Value::Array(req)) if !tags.is_empty() => req.retain(|r| tags.contains(r)),
                _ => {
                    map.remove("required");
                }
            }
            map.values_mut().for_each(strip_required);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_required),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn walk<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        out.push(v);
        match v {
            Value::Object(m) => m.values().for_each(|c| walk(c, out)),
            Value::Array(a) => a.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }

    #[test]
    fn no_null_survives() {
        let schema = json_schema(false);
        let mut nodes = Vec::new();
        walk(&schema, &mut nodes);
        for n in nodes {
            assert!(!is_null_schema(n), "a null arm survived: {n}");
            if let Some(Value::Array(types)) = n.get("type") {
                assert!(
                    !types.iter().any(|t| t == "null"),
                    "a null type survived: {n}"
                );
            }
        }
    }

    #[test]
    fn full_requires_what_serde_requires_and_layer_only_tags() {
        let full = json_schema(false);
        let required = full["required"]
            .as_array()
            .expect("AppConfig has required fields");
        for f in ["server", "database", "storage"] {
            assert!(required.iter().any(|r| r == f), "{f} must be required");
        }
        let layer = json_schema(true);
        assert!(layer.get("required").is_none());
        let mut nodes = Vec::new();
        walk(&layer, &mut nodes);
        // What survives is a tag, and only a tag.
        for n in nodes {
            for r in n
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = r.as_str().unwrap_or_default();
                assert!(
                    n["properties"][name].get("const").is_some(),
                    "{name} kept in {n}"
                );
            }
        }
    }

    #[test]
    fn no_id_so_refs_stay_local() {
        assert!(json_schema(false).get("$id").is_none());
    }

    #[test]
    fn option_of_struct_becomes_the_struct() {
        let mut v = json!({"description": "d", "anyOf": [{"$ref": "#/$defs/A"}, {"type": "null"}]});
        strip_null(&mut v);
        assert_eq!(v, json!({"description": "d", "$ref": "#/$defs/A"}));
    }

    #[test]
    fn option_of_integer_becomes_integer_and_loses_null_default() {
        let mut v = json!({"type": ["integer", "null"], "format": "uint32", "default": null});
        strip_null(&mut v);
        assert_eq!(v, json!({"type": "integer", "format": "uint32"}));
    }

    #[test]
    fn a_real_union_keeps_its_arms() {
        let arms = json!({"anyOf": [{"$ref": "#/$defs/A"}, {"$ref": "#/$defs/B"}]});
        let mut v = arms.clone();
        strip_null(&mut v);
        assert_eq!(v, arms);
    }

    #[test]
    fn ref_with_a_tag_becomes_all_of_and_ref_with_a_description_does_not() {
        let mut v = json!({
            "$ref": "#/$defs/Bearer",
            "properties": {"type": {"const": "bearer"}},
            "required": ["type"],
            "description": "d"
        });
        ref_siblings_to_all_of(&mut v);
        assert_eq!(
            v,
            json!({
                "description": "d",
                "allOf": [
                    {"$ref": "#/$defs/Bearer"},
                    {"properties": {"type": {"const": "bearer"}}, "required": ["type"]}
                ]
            })
        );
        let plain = json!({"$ref": "#/$defs/A", "description": "d"});
        let mut w = plain.clone();
        ref_siblings_to_all_of(&mut w);
        assert_eq!(w, plain);
    }

    #[test]
    fn no_ref_has_validating_siblings() {
        let schema = json_schema(false);
        let mut nodes = Vec::new();
        walk(&schema, &mut nodes);
        for n in nodes {
            if let Some(m) = n.as_object().filter(|m| m.contains_key("$ref")) {
                assert!(
                    m.keys()
                        .all(|k| k == "$ref" || ANNOTATIONS.contains(&k.as_str())),
                    "a $ref with validating siblings: {n}"
                );
            }
        }
    }

    #[test]
    fn integer_widths_are_kept() {
        let s = json_schema(false).to_string();
        assert!(s.contains("\"uint32\"") && s.contains("\"uint64\"") && s.contains("\"double\""));
    }
}
