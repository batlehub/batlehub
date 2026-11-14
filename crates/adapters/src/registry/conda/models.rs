use super::Deserialize;

// ── Serde types for repodata.json ─────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub(super) struct CondaRepodata {
    #[serde(default)]
    pub(super) packages: std::collections::HashMap<String, CondaPackageEntry>,
    #[serde(default, rename = "packages.conda")]
    pub(super) packages_conda: std::collections::HashMap<String, CondaPackageEntry>,
}

/// A package entry, as `repodata.json` spells it and as a CEP-16 shard does.
#[derive(Debug, Deserialize)]
pub(super) struct CondaPackageEntry {
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) version: Option<String>,
    #[serde(default)]
    pub(super) build: Option<String>,
    /// Hex. A shard carries the digest as msgpack `bin`, repodata as a string.
    #[serde(default, deserialize_with = "digest")]
    pub(super) sha256: Option<String>,
    /// Build timestamp in milliseconds since the Unix epoch.
    /// Present in most but not all repodata.json entries.
    #[serde(default)]
    pub(super) timestamp: Option<i64>,
}

/// CEP-16 `repodata_shards.msgpack.zst`: package name → shard digest.
#[derive(Debug, Deserialize)]
pub(super) struct CondaShardIndexDoc {
    #[serde(default)]
    pub(super) info: CondaShardIndexInfo,
    pub(super) shards: std::collections::HashMap<String, Digest>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CondaShardIndexInfo {
    #[serde(default)]
    pub(super) shards_base_url: Option<String>,
}

/// A sha256 as hex, whichever of `bin` or string it arrived as.
#[derive(Debug)]
pub(super) struct Digest(pub(super) String);

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        digest(d)?
            .map(Digest)
            .ok_or_else(|| serde::de::Error::custom("missing digest"))
    }
}

fn digest<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a digest as bytes or hex")
        }
        fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E> {
            Ok(Some(hex::encode(v)))
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    d.deserialize_any(V)
}

/// Metadata extracted from a conda package archive.
#[derive(Debug, Clone)]
pub struct CondaPackageInfo {
    pub name: String,
    pub version: String,
    pub build: String,
    pub build_number: u64,
    pub depends: Vec<String>,
    pub subdir: Option<String>,
    pub license: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CondaIndexJson {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) build: String,
    #[serde(default)]
    pub(super) build_number: u64,
    #[serde(default)]
    pub(super) depends: Vec<String>,
    #[serde(default)]
    pub(super) subdir: Option<String>,
    #[serde(default)]
    pub(super) license: Option<String>,
}
