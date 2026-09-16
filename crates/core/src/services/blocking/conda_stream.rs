//! conda's `repodata.json`, filtered **as it is copied**.
//!
//! [`super::conda::strip_repodata`] does the same job on a parsed document and
//! is the right tool for every other document in this proxy. It is the wrong
//! tool for this one: `conda-forge/linux-64/repodata.json` is 424 MiB of about
//! 1.4 million small objects, and a `serde_json::Value` of that is several
//! gigabytes — measured at ~11.5 GB resident for one request, which is past
//! what a client will wait for and past what a 16 GB runner has.
//!
//! So this module reads the document and writes the filtered one at the same
//! time, holding **one package entry** at a time. The output is the input with
//! blocked entries missing: same keys, same order, same values, which is what
//! makes it safe to serve to a solver that has to agree with the artifacts
//! behind it.
//!
//! JSON is written by hand rather than through a `serde_json::Serializer`
//! because the work here is *copying*, not serialising: every value except the
//! two package maps is re-emitted exactly as it arrived, and the two package
//! maps are copied entry by entry with a predicate in between. Keys and values
//! still go out through `serde_json`, so escaping and number formatting are not
//! this module's business.

use std::io::{Read, Write};

use serde::de::{DeserializeSeed, Deserializer, Error as _, MapAccess, Visitor};
use serde_json::Value;

use super::MultiPackageBlocks;

/// The two members of a repodata document that list packages.
///
/// A channel serves both for the same release — `packages` is the `.tar.bz2`
/// generation and `packages.conda` the current one — so leaving either would
/// keep a blocked version installable.
const PACKAGE_MAPS: [&str; 2] = ["packages", "packages.conda"];

/// Copy `reader` to `writer`, dropping the blocked entries of both package maps.
///
/// Returns the `name-version` pairs removed, as the document spelled them —
/// the same value [`super::conda::strip_repodata`] returns, so the two paths
/// report identically.
///
/// Errors are `serde_json::Error`, including write failures (wrapped with
/// [`serde::de::Error::custom`]): a half-written document is not a document,
/// and the caller has to fail rather than serve one.
pub fn filter_repodata<R: Read, W: Write>(
    reader: R,
    writer: W,
    blocked: &MultiPackageBlocks,
) -> Result<Vec<String>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let mut writer = writer;
    let seed = Document {
        writer: &mut writer,
        blocked,
    };
    seed.deserialize(&mut deserializer)
}

/// Map a write error into the deserializer's error type.
fn io<E: serde::de::Error>(e: std::io::Error) -> E {
    E::custom(format!("writing the filtered repodata: {e}"))
}

/// The top-level object: copied key by key, with the package maps intercepted.
struct Document<'a, W: Write> {
    writer: &'a mut W,
    blocked: &'a MultiPackageBlocks,
}

impl<'de, W: Write> DeserializeSeed<'de> for Document<'_, W> {
    type Value = Vec<String>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de, W: Write> Visitor<'de> for Document<'_, W> {
    type Value = Vec<String>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a repodata.json object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut removed = Vec::new();
        self.writer.write_all(b"{").map_err(io)?;
        let mut first = true;

        while let Some(key) = map.next_key::<String>()? {
            if !first {
                self.writer.write_all(b",").map_err(io)?;
            }
            first = false;
            serde_json::to_writer(&mut *self.writer, &key).map_err(A::Error::custom)?;
            self.writer.write_all(b":").map_err(io)?;

            if PACKAGE_MAPS.contains(&key.as_str()) {
                map.next_value_seed(Packages {
                    writer: self.writer,
                    blocked: self.blocked,
                    removed: &mut removed,
                })?;
            } else {
                // Everything else — `info`, `repodata_version`, `removed` — is
                // re-emitted as it arrived. These are small: `info` is a handful
                // of fields and `removed` a list of filenames.
                let value: Value = map.next_value()?;
                serde_json::to_writer(&mut *self.writer, &value).map_err(A::Error::custom)?;
            }
        }

        self.writer.write_all(b"}").map_err(io)?;
        self.writer.flush().map_err(io)?;
        Ok(removed)
    }
}

/// One package map: `{ "<filename>": { "name": …, "version": …, … } }`.
struct Packages<'a, W: Write> {
    writer: &'a mut W,
    blocked: &'a MultiPackageBlocks,
    removed: &'a mut Vec<String>,
}

impl<'de, W: Write> DeserializeSeed<'de> for Packages<'_, W> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de, W: Write> Visitor<'de> for Packages<'_, W> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a map of filename to package entry")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        self.writer.write_all(b"{").map_err(io)?;
        let mut first = true;

        while let Some(filename) = map.next_key::<String>()? {
            // One entry in memory at a time, and it is small — the whole point
            // of this module. `Value` rather than a typed struct because the
            // entry is re-emitted verbatim and a channel may carry fields this
            // proxy has never heard of.
            let entry: Value = map.next_value()?;

            let name = entry.get("name").and_then(Value::as_str);
            let version = entry.get("version").and_then(Value::as_str);
            let drop = match (name, version) {
                (Some(n), Some(v)) => self.blocked.contains(n, v),
                // An entry whose coordinate cannot be read is kept, exactly as
                // the parsed filter keeps it: over-listing one package beats
                // emptying a channel.
                _ => false,
            };

            if drop {
                self.removed.push(format!(
                    "{}-{}",
                    name.unwrap_or_default(),
                    version.unwrap_or_default()
                ));
                continue;
            }

            if !first {
                self.writer.write_all(b",").map_err(io)?;
            }
            first = false;
            serde_json::to_writer(&mut *self.writer, &filename).map_err(A::Error::custom)?;
            self.writer.write_all(b":").map_err(io)?;
            serde_json::to_writer(&mut *self.writer, &entry).map_err(A::Error::custom)?;
        }

        self.writer.write_all(b"}").map_err(io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    const DOC: &str = r#"{
        "info": {"subdir": "linux-64"},
        "packages": {
            "numpy-1.24.0-py311_0.tar.bz2": {"name": "numpy", "version": "1.24.0", "build": "py311_0"},
            "six-1.17.0-py_0.tar.bz2": {"name": "six", "version": "1.17.0"}
        },
        "packages.conda": {
            "numpy-1.24.0-py311_0.conda": {"name": "numpy", "version": "1.24.0"},
            "numpy-1.25.0-py311_0.conda": {"name": "numpy", "version": "1.25.0"}
        },
        "removed": ["old-1.0.0.tar.bz2"],
        "repodata_version": 1
    }"#;

    fn blocks(pairs: &[(&str, &str)]) -> MultiPackageBlocks {
        MultiPackageBlocks::new(
            RegistryKind::Conda,
            pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    fn filter(doc: &str, blocked: &MultiPackageBlocks) -> (Value, Vec<String>) {
        let mut out = Vec::new();
        let removed = filter_repodata(doc.as_bytes(), &mut out, blocked).unwrap();
        // The emitted bytes are shown on failure: a filter that writes a
        // stray comma or an unescaped character produces a document that is
        // wrong in a way no assertion on the *parsed* value can describe.
        let parsed = serde_json::from_slice(&out).unwrap_or_else(|e| {
            panic!(
                "the filtered document is not valid JSON: {e}\n--- emitted ---\n{}\n---",
                String::from_utf8_lossy(&out)
            )
        });
        (parsed, removed)
    }

    /// Nothing blocked is a copy — and that is the property the whole fast path
    /// rests on, so it is asserted rather than assumed.
    #[test]
    fn an_empty_block_set_copies_the_document() {
        let (out, removed) = filter(DOC, &blocks(&[]));
        assert!(removed.is_empty());
        assert_eq!(out, serde_json::from_str::<Value>(DOC).unwrap());
    }

    /// The blocked version goes from **both** package maps, and nothing else
    /// moves.
    #[test]
    fn a_blocked_version_leaves_both_package_maps() {
        let (out, removed) = filter(DOC, &blocks(&[("numpy", "1.24.0")]));

        assert_eq!(removed, vec!["numpy-1.24.0", "numpy-1.24.0"]);
        let packages = out["packages"].as_object().unwrap();
        assert!(!packages.contains_key("numpy-1.24.0-py311_0.tar.bz2"));
        assert!(packages.contains_key("six-1.17.0-py_0.tar.bz2"));

        let conda = out["packages.conda"].as_object().unwrap();
        assert!(!conda.contains_key("numpy-1.24.0-py311_0.conda"));
        assert!(
            conda.contains_key("numpy-1.25.0-py311_0.conda"),
            "a different version of the same package stays"
        );

        // The members that are not package maps are untouched.
        assert_eq!(out["info"]["subdir"], "linux-64");
        assert_eq!(out["removed"][0], "old-1.0.0.tar.bz2");
        assert_eq!(out["repodata_version"], 1);
    }

    /// The same answer as the parsed filter, on the same input — the two paths
    /// serve the same clients and must not disagree about what a channel holds.
    #[test]
    fn it_agrees_with_the_parsed_filter() {
        let blocked = blocks(&[("numpy", "1.24.0"), ("six", "1.17.0")]);

        let (streamed, mut streamed_removed) = filter(DOC, &blocked);

        let mut parsed: Value = serde_json::from_str(DOC).unwrap();
        let mut parsed_removed = super::super::conda::strip_repodata(&mut parsed, &blocked);

        assert_eq!(streamed, parsed);
        streamed_removed.sort();
        parsed_removed.sort();
        assert_eq!(streamed_removed, parsed_removed);
    }

    /// An entry with no readable coordinate is kept, like the parsed filter.
    #[test]
    fn an_unreadable_entry_is_kept() {
        let doc = r#"{"packages": {"mystery.tar.bz2": {"build": "0"}}}"#;
        let (out, removed) = filter(doc, &blocks(&[("numpy", "1.24.0")]));
        assert!(removed.is_empty());
        assert!(out["packages"]
            .as_object()
            .unwrap()
            .contains_key("mystery.tar.bz2"));
    }

    /// An empty package map still has to come out as `{}` rather than as a
    /// dangling comma or a missing member.
    #[test]
    fn emptying_a_package_map_leaves_valid_json() {
        let doc = r#"{"packages": {"six-1.17.0-py_0.tar.bz2": {"name": "six", "version": "1.17.0"}}, "repodata_version": 1}"#;
        let (out, removed) = filter(doc, &blocks(&[("six", "1.17.0")]));
        assert_eq!(removed, vec!["six-1.17.0"]);
        assert_eq!(out["packages"], serde_json::json!({}));
        assert_eq!(out["repodata_version"], 1);
    }

    /// Keys and values that need escaping survive the copy, because they go out
    /// through `serde_json` rather than through this module's own quoting.
    #[test]
    fn escaping_survives_the_copy() {
        let doc = r#"{"packages": {"od\"d\n.tar.bz2": {"name": "od\"d", "version": "1.0", "note": "é\u0001"}}}"#;
        let (out, _) = filter(doc, &blocks(&[]));
        assert_eq!(out, serde_json::from_str::<Value>(doc).unwrap());
    }
}
