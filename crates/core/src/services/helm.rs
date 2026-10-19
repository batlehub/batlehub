//! Classic Helm chart repositories — `index.yaml` and the archives it names
//! (RFC 0029).
//!
//! No I/O here. The index travels through the proxy as **JSON**: the adapter
//! parses upstream's YAML once on fetch, the metadata cache, the blocked-set
//! filter and the URL rewrite all work on the `serde_json::Value`, and the
//! handler renders YAML on the way out. `helm` has no checksum or signature
//! over the document (§5.1), so re-emitting it loses nothing a client verifies;
//! every field is kept, because the value is the whole mapping rather than a
//! typed struct that would drop what it does not know.
//!
//! The coordinate is the file name — `charts/{name}-{version}.tgz` — and the
//! upstream index is the map back from it to the URL upstream wrote. Nothing
//! in a request can name a host (§5.2).

use serde_json::Value;

use crate::error::CoreError;
use crate::services::blocking::MultiPackageBlocks;

/// The address the registry-wide index is cached and filtered under — what
/// `fetch_version_document` receives for it. Any other name is the console's
/// discovery read asking about one chart.
pub const INDEX_ADDRESS: &str = "index.yaml";

/// The `PackageId::artifact` of a chart archive.
pub const CHART_ARTIFACT: &str = "chart";
/// The `PackageId::artifact` of its provenance file.
pub const PROV_ARTIFACT: &str = "prov";
/// The `signature_type` a locally published `.prov` is stored under: the file
/// rides the version row's signature slot, so `signing` policy sees it. A
/// `.prov` is an OpenPGP clear-signed document, so it is `pgp` — the name
/// `signing.allowed_types` already uses for OpenPGP.
pub const PROV_SIGNATURE_TYPE: &str = "pgp";

/// What an index entry whose `urls[0]` is `oci://` becomes in the served index
/// (RFC 0029 §4.4, decision 2).
///
/// In `core` rather than `config` because the handler reads it off `HotConfig`
/// on every index request, the way `GalaxyRoleMode` is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OciEntries {
    /// Omitted: an entry this instance cannot serve is one its policy cannot
    /// reach, so it is not advertised.
    #[default]
    Drop,
    /// Served as upstream wrote it — `helm` pulls it from the OCI registry,
    /// past this instance.
    Relay,
}

/// How a `hybrid` helm registry composes a chart it hosts with the upstream
/// chart of the same name. Read off `HotConfig` for `OciEntries`' reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HybridMerge {
    /// A chart name published here takes the whole name: the upstream chart's
    /// entries are dropped and its archives are never fetched. The rule every
    /// other hybrid kind applies, and the one that keeps a public `acme-auth
    /// 9.9.9` from outranking the internal `acme-auth 1.0.0`.
    #[default]
    Chart,
    /// Upstream versions are listed under the local ones, local winning per
    /// version (RFC 0029 §4.4) — what a mirror extended with a few patched
    /// releases wants, and a dependency-confusion vector for anything else.
    Version,
}

/// One file under `charts/`, parsed back into the coordinate it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartFile {
    pub name: String,
    pub version: String,
    /// `{name}-{version}.tgz.prov` rather than the archive.
    pub prov: bool,
}

impl ChartFile {
    pub fn artifact(&self) -> &'static str {
        if self.prov {
            PROV_ARTIFACT
        } else {
            CHART_ARTIFACT
        }
    }
}

/// The file name a chart version is served under, whatever upstream called it.
pub fn chart_file_name(name: &str, version: &str) -> String {
    format!("{name}-{version}.tgz")
}

/// A chart name: what `helm create` and the index keys use. No `/`, because a
/// chart repository is flat.
pub fn validate_chart_name(name: &str) -> Result<(), CoreError> {
    crate::services::validate_package_name(name)?;
    let ok = name.len() <= 250
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && name.as_bytes()[0].is_ascii_alphanumeric();
    if !ok {
        return Err(CoreError::InvalidInput(format!(
            "'{name}' is not a chart name"
        )));
    }
    Ok(())
}

/// A chart version, in helm's grammar: Masterminds semver's `NewVersion`,
/// which helm calls with `CoerceNewVersion` on, so the loose expression
/// applies — `v` prefix optional, minor and patch optional, leading zeros
/// allowed. `1.0`, `v1.21.2` and `2024.01.15` are all chart versions helm
/// packages, indexes and installs.
pub fn validate_version(version: &str) -> Result<(), CoreError> {
    if version_parts(version).is_none() {
        return Err(CoreError::InvalidInput(format!(
            "'{version}' is not a chart version"
        )));
    }
    Ok(())
}

/// `looseSemVerRegex` (`Masterminds/semver/v3`, `version.go`):
/// `v?([0-9]+)(\.[0-9]+)?(\.[0-9]+)?(-ident(\.ident)*)?(\+ident(\.ident)*)?`
/// with `ident = [0-9A-Za-z-]+`, anchored. Returns the numbers, missing ones
/// zero, and the prerelease.
fn version_parts(version: &str) -> Option<([u64; 3], &str)> {
    let ident = |s: &str| {
        s.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    };
    let v = version.strip_prefix('v').unwrap_or(version);
    let (v, build) = v.split_once('+').map_or((v, None), |(a, b)| (a, Some(b)));
    let (core, pre) = v.split_once('-').map_or((v, None), |(a, b)| (a, Some(b)));
    if !build.is_none_or(ident) || !pre.is_none_or(ident) {
        return None;
    }
    let mut nums = [0u64; 3];
    let mut parts = core.split('.');
    for (i, p) in parts.by_ref().take(3).enumerate() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        nums[i] = p.parse().ok()?;
    }
    parts.next().is_none().then_some((nums, pre.unwrap_or("")))
}

/// The one spelling of a chart version that helm's comparison cannot tell
/// apart from the others: `v1.0`, `1.0.0` and `1.0.0+build.5` are all
/// `1.0.0`. `None` for what is not a chart version.
pub fn canonical_version(version: &str) -> Option<String> {
    let ([a, b, c], pre) = version_parts(version)?;
    Some(if pre.is_empty() {
        format!("{a}.{b}.{c}")
    } else {
        format!("{a}.{b}.{c}-{pre}")
    })
}

/// `{name}-{version}.tgz[.prov]` → every coordinate it can name, leftmost
/// split first.
///
/// Both halves contain dashes (`kube-prometheus-stack-65.1.0.tgz`,
/// `foo-1.0.0-rc.1.tgz`), so a file name can be read more than one way:
/// `app-1.0.0-2.0.0.tgz` is `app` at `1.0.0-2.0.0` or `app-1.0.0` at `2.0.0`,
/// and the name alone cannot say which. The caller picks the one that exists.
pub fn coordinates_of(file: &str) -> Result<Vec<ChartFile>, CoreError> {
    let (stem, prov) = match file.strip_suffix(".tgz.prov") {
        Some(stem) => (stem, true),
        None => (
            file.strip_suffix(".tgz").ok_or_else(|| {
                CoreError::InvalidInput(format!("'{file}' is not a chart archive name"))
            })?,
            false,
        ),
    };
    let all: Vec<ChartFile> = stem
        .match_indices('-')
        .map(|(i, _)| (&stem[..i], &stem[i + 1..]))
        .filter(|(name, version)| {
            validate_chart_name(name).is_ok() && validate_version(version).is_ok()
        })
        .map(|(name, version)| ChartFile {
            name: name.to_owned(),
            version: version.to_owned(),
            prov,
        })
        .collect();
    if all.is_empty() {
        return Err(CoreError::InvalidInput(format!(
            "'{file}' is not {{chart}}-{{version}}.tgz"
        )));
    }
    Ok(all)
}

/// Any YAML document as a JSON value — `index.yaml`, or a `Chart.yaml` read out
/// of an archive. The one place the YAML crate is named, so swapping it is one
/// function (RFC 0029 §11 decision 1).
pub fn parse_yaml(text: &str) -> Result<Value, String> {
    serde_saphyr::from_str(text).map_err(|e| e.to_string())
}

/// `parse_yaml` with the node, event and scalar budgets scaled to the input.
/// The crate's defaults (250 000 nodes) are sized for a config file: the
/// prometheus-community index (6.5 MB) is 316 644 nodes and Bitnami's about
/// four times that, so the defaults refuse the repositories RFC 0029 is for.
/// The input is already capped by the caller's byte limit; the alias and depth
/// limits that stop an expansion bomb stay at their defaults.
fn parse_large_yaml(text: &str) -> Result<Value, String> {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_nodes = budget.max_nodes.max(text.len() / 4);
    budget.max_events = budget.max_events.max(text.len() / 4);
    budget.max_total_scalar_bytes = budget.max_total_scalar_bytes.max(text.len());
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    serde_saphyr::from_str_with_options(text, options).map_err(|e| e.to_string())
}

/// Upstream's `index.yaml`, as the value every later step reads.
///
/// Entries with no string `version` are dropped, as helm's `loadIndex` drops
/// them; a document with no `entries` map is an empty repository.
pub fn parse_index(text: &str) -> Result<Value, CoreError> {
    let mut doc = parse_large_yaml(text)
        .map_err(|e| CoreError::Registry(format!("index.yaml is not YAML: {e}")))?;
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| CoreError::Registry("index.yaml is not a mapping".to_owned()))?;
    let entries = obj
        .entry("entries")
        .or_insert_with(|| Value::Object(Default::default()));
    if !entries.is_object() {
        *entries = Value::Object(Default::default());
    }
    if let Some(charts) = entries.as_object_mut() {
        for versions in charts.values_mut() {
            match versions.as_array_mut() {
                Some(list) => list.retain(|e| e.get("version").is_some_and(Value::is_string)),
                None => *versions = Value::Array(Vec::new()),
            }
        }
        charts.retain(|_, v| v.as_array().is_some_and(|l| !l.is_empty()));
    }
    Ok(doc)
}

/// The served document, as YAML.
pub fn render_index(index: &Value) -> Result<String, CoreError> {
    // Plain or quoted scalars only: the default folds any string over 80
    // characters into a `>-` block, which every YAML reader accepts and no
    // person or line-oriented tool reading `urls:` expects.
    let opts = serde_saphyr::ser_options! { prefer_block_scalars: false };
    serde_saphyr::to_string_with_options(index, opts)
        .map_err(|e| CoreError::Other(anyhow::anyhow!("could not render index.yaml: {e}")))
}

fn charts_mut(index: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    index.get_mut("entries")?.as_object_mut()
}

/// The entries of one chart.
pub fn chart_entries<'a>(index: &'a Value, name: &str) -> &'a [Value] {
    index
        .pointer("/entries")
        .and_then(|e| e.get(name))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// One version's entry.
pub fn find_entry<'a>(index: &'a Value, name: &str, version: &str) -> Option<&'a Value> {
    chart_entries(index, name)
        .iter()
        .find(|e| e.get("version").and_then(Value::as_str) == Some(version))
}

/// Every version of a chart, in index order (newest first, as `helm repo
/// index` writes them).
pub fn versions_of(index: &Value, name: &str) -> Vec<String> {
    chart_entries(index, name)
        .iter()
        .filter_map(|e| e.get("version")?.as_str().map(str::to_owned))
        .collect()
}

/// An entry's `urls[0]`, the only one `helm` reads.
pub fn first_url(entry: &Value) -> Option<&str> {
    entry.get("urls")?.as_array()?.first()?.as_str()
}

/// Whether an entry is a pointer into an OCI registry.
pub fn is_oci(entry: &Value) -> bool {
    first_url(entry).is_some_and(|u| {
        u.as_bytes()
            .get(..6)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"oci://"))
    })
}

/// The registry-wide blocked set applied: blocked versions removed, a chart
/// with none left removed with its key, so `helm` reports its own *"no chart
/// version found"* or *"no chart name found"*. Returns the removed pairs as
/// `name@version`.
pub fn strip(index: &mut Value, blocked: &MultiPackageBlocks) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(charts) = charts_mut(index) else {
        return removed;
    };
    for (name, versions) in charts.iter_mut() {
        if let Some(list) = versions.as_array_mut() {
            list.retain(|e| {
                let v = e.get("version").and_then(Value::as_str).unwrap_or("");
                let keep = !blocked.contains(name, v);
                if !keep {
                    removed.push(format!("{name}@{v}"));
                }
                keep
            });
        }
    }
    charts.retain(|_, v| v.as_array().is_some_and(|l| !l.is_empty()));
    removed
}

/// What [`rewrite`] did that an operator should hear about.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rewritten {
    /// `oci://` entries omitted under [`OciEntries::Drop`].
    pub oci_dropped: usize,
    /// Entries upstream gave more than one URL, served with ours alone.
    pub multi_url: usize,
}

/// The URL edit of §4.4: every non-OCI entry's `urls` becomes one URL on this
/// instance, `{chart_base}/{name}-{version}.tgz`, and `oci://` entries follow
/// `oci`. A chart with nothing left loses its key, so `helm search repo` does
/// not advertise what `helm pull` cannot get.
pub fn rewrite(index: &mut Value, chart_base: &str, oci: OciEntries) -> Rewritten {
    let mut out = Rewritten::default();
    let Some(charts) = charts_mut(index) else {
        return out;
    };
    for (name, versions) in charts.iter_mut() {
        let Some(list) = versions.as_array_mut() else {
            continue;
        };
        list.retain_mut(|entry| {
            if is_oci(entry) {
                if oci == OciEntries::Drop {
                    out.oci_dropped += 1;
                    return false;
                }
                return true;
            }
            let version = entry
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(obj) = entry.as_object_mut() {
                if obj
                    .get("urls")
                    .and_then(Value::as_array)
                    .is_some_and(|u| u.len() > 1)
                {
                    out.multi_url += 1;
                }
                obj.insert(
                    "urls".to_owned(),
                    serde_json::json!([format!(
                        "{chart_base}/{}",
                        chart_file_name(name, &version)
                    )]),
                );
            }
            true
        });
    }
    charts.retain(|_, v| v.as_array().is_some_and(|l| !l.is_empty()));
    out
}

/// The shape a field must have to survive into the index.
#[derive(Clone, Copy)]
enum Shape {
    Str,
    Bool,
    Strs,
    StrMap,
    Any,
    /// A list of mappings, each keeping only these keys.
    Records(&'static [(&'static str, Shape)]),
}

/// `Maintainer` in helm's `pkg/chart/metadata.go`.
const MAINTAINER: &[(&str, Shape)] = &[
    ("name", Shape::Str),
    ("email", Shape::Str),
    ("url", Shape::Str),
];

/// `Dependency` in helm's `pkg/chart/dependency.go`.
const DEPENDENCY: &[(&str, Shape)] = &[
    ("name", Shape::Str),
    ("version", Shape::Str),
    ("repository", Shape::Str),
    ("condition", Shape::Str),
    ("tags", Shape::Strs),
    ("enabled", Shape::Bool),
    ("import-values", Shape::Any),
    ("alias", Shape::Str),
];

/// The `Chart.yaml` fields a composed index entry carries (§4.4), with the
/// type `Metadata` gives each.
///
/// The shapes are not decoration. `helm` reads `Chart.yaml` leniently but
/// `index.yaml` with `yaml.UnmarshalStrict` (`pkg/repo/index.go`), so one
/// unknown key or wrong type in one entry fails the whole document — every
/// client's `helm repo update` on the repository, for one chart's
/// `maintainers: [{name: a, github: b}]`.
const CHART_YAML_FIELDS: &[(&str, Shape)] = &[
    ("apiVersion", Shape::Str),
    ("name", Shape::Str),
    ("version", Shape::Str),
    ("appVersion", Shape::Str),
    ("description", Shape::Str),
    ("type", Shape::Str),
    ("kubeVersion", Shape::Str),
    ("keywords", Shape::Strs),
    ("maintainers", Shape::Records(MAINTAINER)),
    ("dependencies", Shape::Records(DEPENDENCY)),
    ("annotations", Shape::StrMap),
    ("home", Shape::Str),
    ("sources", Shape::Strs),
    ("icon", Shape::Str),
    ("deprecated", Shape::Bool),
];

/// `v` cut down to `shape`, or `None` when nothing of it fits.
fn conform(v: &Value, shape: Shape) -> Option<Value> {
    match shape {
        Shape::Str => v.is_string().then(|| v.clone()),
        Shape::Bool => v.is_boolean().then(|| v.clone()),
        Shape::Any => (!v.is_null()).then(|| v.clone()),
        Shape::Strs => Some(Value::Array(
            v.as_array()?
                .iter()
                .filter(|s| s.is_string())
                .cloned()
                .collect(),
        )),
        Shape::StrMap => Some(Value::Object(
            v.as_object()?
                .iter()
                .filter(|(_, s)| s.is_string())
                .map(|(k, s)| (k.clone(), s.clone()))
                .collect(),
        )),
        Shape::Records(fields) => Some(Value::Array(
            v.as_array()?
                .iter()
                .filter(|r| r.is_object())
                .map(|r| conform_fields(r, fields))
                .collect(),
        )),
    }
}

fn conform_fields(v: &Value, fields: &[(&str, Shape)]) -> Value {
    let mut out = serde_json::Map::new();
    for (k, shape) in fields {
        if let Some(c) = v.get(*k).and_then(|f| conform(f, *shape)) {
            out.insert((*k).to_owned(), c);
        }
    }
    Value::Object(out)
}

/// The subset of a parsed `Chart.yaml` stored as a version's `index_metadata`,
/// each field cut to the shape helm's strict index reader accepts.
pub fn index_metadata_from_chart_yaml(chart: &Value) -> Value {
    conform_fields(chart, CHART_YAML_FIELDS)
}

/// An index entry composed from a stored version: its `Chart.yaml` fields plus
/// `digest` and `created`. `urls` is left for [`rewrite`], like upstream's.
pub fn composed_entry(
    meta: &Value,
    name: &str,
    version: &str,
    digest: &str,
    created: &str,
) -> Value {
    // Conformed again on the way out: rows stored before the shapes were
    // enforced would otherwise still break the index.
    let mut entry = match index_metadata_from_chart_yaml(meta) {
        Value::Object(m) => m,
        _ => Default::default(),
    };
    entry.insert("name".to_owned(), Value::String(name.to_owned()));
    entry.insert("version".to_owned(), Value::String(version.to_owned()));
    entry.insert("digest".to_owned(), Value::String(digest.to_owned()));
    entry.insert("created".to_owned(), Value::String(created.to_owned()));
    entry.insert("urls".to_owned(), serde_json::json!([]));
    Value::Object(entry)
}

/// A whole index around composed entries, in the shape `helm repo index`
/// writes.
pub fn compose_index(entries: serde_json::Map<String, Value>, generated: &str) -> Value {
    serde_json::json!({
        "apiVersion": "v1",
        "entries": entries,
        "generated": generated,
    })
}

/// Hybrid: `local`'s entries over `upstream`'s.
///
/// `hosted` is every chart name this registry holds rows for, **including the
/// ones this caller may not see**. Under [`HybridMerge::Chart`] each of them
/// drops the upstream chart of that name before the local entries go in, so a
/// chart hidden from this caller reads as absent rather than as the public
/// chart of the same name. Under [`HybridMerge::Version`] local wins on a
/// `{name, version}` collision only.
pub fn merge(upstream: &mut Value, local: &Value, hosted: &[String], how: HybridMerge) {
    let Some(charts) = charts_mut(upstream) else {
        return;
    };
    if how == HybridMerge::Chart {
        for name in hosted {
            charts.remove(name);
        }
    }
    let Some(local_charts) = local.get("entries").and_then(Value::as_object) else {
        return;
    };
    for (name, local_versions) in local_charts {
        let local_list = local_versions.as_array().cloned().unwrap_or_default();
        let slot = charts
            .entry(name.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(list) = slot.as_array_mut() else {
            continue;
        };
        list.retain(|e| {
            let v = e.get("version");
            !local_list.iter().any(|l| l.get("version") == v)
        });
        let mut merged = local_list;
        merged.append(list);
        *list = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    const INDEX: &str = r#"apiVersion: v1
entries:
  alertmanager:
  - apiVersion: v2
    appVersion: v0.34.0
    created: "2026-09-11T12:39:49.209345931Z"
    digest: d370858f873b904a966e64d530874e645114a86a3095f7afc43ed65b217afab8
    name: alertmanager
    annotations:
      artifacthub.io/license: Apache-2.0
    urls:
    - https://github.com/prometheus-community/helm-charts/releases/download/alertmanager-1.43.1/alertmanager-1.43.1.tgz
    - https://mirror.example/alertmanager-1.43.1.tgz
    version: 1.43.1
  - name: alertmanager
    urls:
    - alertmanager-1.43.0.tgz
    version: 1.43.0
  - name: alertmanager
    urls: [x.tgz]
  redis:
  - name: redis
    urls:
    - oci://registry-1.docker.io/bitnamicharts/redis
    version: 20.0.0
generated: "2026-09-11T12:40:00Z"
"#;

    fn only(file: &str) -> (String, String, bool) {
        let mut all = coordinates_of(file).unwrap();
        assert_eq!(all.len(), 1, "{file}: {all:?}");
        let c = all.remove(0);
        (c.name, c.version, c.prov)
    }

    #[test]
    fn coordinates_of_splits_at_the_version() {
        let s = |a: &str, b: &str, p| (a.to_owned(), b.to_owned(), p);
        assert_eq!(
            only("kube-prometheus-stack-65.1.0.tgz"),
            s("kube-prometheus-stack", "65.1.0", false)
        );
        assert_eq!(
            only("cert-manager-v1.21.2.tgz.prov"),
            s("cert-manager", "v1.21.2", true)
        );
        assert_eq!(only("foo-1.0.0-rc.1.tgz"), s("foo", "1.0.0-rc.1", false));
        // Versions helm accepts and strict SemVer does not.
        assert_eq!(only("foo-1.0.tgz"), s("foo", "1.0", false));
        assert_eq!(only("cal-2024.01.15.tgz"), s("cal", "2024.01.15", false));
        for bad in [
            "foo.tgz",
            "-1.0.0.tgz",
            "../x-1.0.0.tgz",
            "a%2F..-1.0.0.tgz",
            "x-1.0.0.zip",
            "x/y-1.0.0.tgz",
            "x-1.0.0.0.tgz",
            "x-V1.0.0.tgz",
        ] {
            assert!(coordinates_of(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn an_ambiguous_file_name_yields_every_reading() {
        let all: Vec<_> = coordinates_of("app-1.0.0-2.0.0.tgz")
            .unwrap()
            .into_iter()
            .map(|c| (c.name, c.version))
            .collect();
        assert_eq!(
            all,
            [
                ("app".to_owned(), "1.0.0-2.0.0".to_owned()),
                ("app-1.0.0".to_owned(), "2.0.0".to_owned())
            ]
        );
    }

    #[test]
    fn canonical_version_is_what_helm_compares() {
        for (a, b) in [
            ("v1.21.2", "1.21.2"),
            ("1.0", "1.0.0"),
            ("1.0.0+b.5", "1"),
            ("01.2.3", "1.2.3"),
        ] {
            assert_eq!(canonical_version(a), canonical_version(b), "{a} {b}");
        }
        assert_ne!(canonical_version("1.0.0-rc.1"), canonical_version("1.0.0"));
        assert_eq!(canonical_version("../x"), None);
    }

    #[test]
    fn parse_drops_versionless_entries_and_round_trips() {
        let index = parse_index(INDEX).unwrap();
        assert_eq!(versions_of(&index, "alertmanager"), ["1.43.1", "1.43.0"]);
        let again = parse_index(&render_index(&index).unwrap()).unwrap();
        assert_eq!(
            again, index,
            "render must round-trip, unknown fields included"
        );
        assert_eq!(
            again.pointer("/entries/alertmanager/0/annotations/artifacthub.io~1license"),
            Some(&Value::String("Apache-2.0".into()))
        );
        // A numeric-looking version stays a string on the way out.
        let mut v = index.clone();
        v["entries"]["alertmanager"][0]["version"] = "1.10".into();
        assert!(render_index(&v).unwrap().contains("version: \"1.10\""));
        // A long URL is one line, not a folded block.
        let long = format!("https://hub.example/proxy/{}/x-1.0.0.tgz", "r".repeat(90));
        v["entries"]["alertmanager"][0]["urls"] = serde_json::json!([long]);
        assert!(render_index(&v).unwrap().contains(&format!("- {long}\n")));
    }

    #[test]
    fn rewrite_points_every_url_here_and_follows_oci_entries() {
        let mut index = parse_index(INDEX).unwrap();
        let mut relayed = index.clone();
        let r = rewrite(
            &mut index,
            "https://hub/proxy/charts/helm/charts",
            OciEntries::Drop,
        );
        assert_eq!(
            r,
            Rewritten {
                oci_dropped: 1,
                multi_url: 1
            }
        );
        assert_eq!(
            index.pointer("/entries/alertmanager/0/urls"),
            Some(&serde_json::json!([
                "https://hub/proxy/charts/helm/charts/alertmanager-1.43.1.tgz"
            ]))
        );
        assert!(
            index.pointer("/entries/redis").is_none(),
            "a chart with only OCI entries loses its key"
        );

        let r = rewrite(&mut relayed, "https://hub/c", OciEntries::Relay);
        assert_eq!(r.oci_dropped, 0);
        assert_eq!(
            first_url(&relayed["entries"]["redis"][0]),
            Some("oci://registry-1.docker.io/bitnamicharts/redis")
        );
    }

    #[test]
    fn strip_removes_blocked_versions_and_emptied_charts() {
        let mut index = parse_index(INDEX).unwrap();
        let blocked = MultiPackageBlocks::new(
            RegistryKind::Helm,
            vec![
                ("alertmanager".into(), "1.43.0".into()),
                ("redis".into(), "20.0.0".into()),
            ],
        );
        let mut removed = strip(&mut index, &blocked);
        removed.sort();
        assert_eq!(removed, ["alertmanager@1.43.0", "redis@20.0.0"]);
        assert_eq!(versions_of(&index, "alertmanager"), ["1.43.1"]);
        assert!(index.pointer("/entries/redis").is_none());
    }

    #[test]
    fn merge_lets_local_win() {
        let mut upstream = parse_index(INDEX).unwrap();
        let mut entries = serde_json::Map::new();
        entries.insert(
            "alertmanager".into(),
            serde_json::json!([
                composed_entry(
                    &serde_json::json!({"description": "ours"}),
                    "alertmanager",
                    "1.43.0",
                    "ab",
                    "t"
                ),
                composed_entry(&Value::Null, "alertmanager", "9.0.0", "cd", "t")
            ]),
        );
        entries.insert(
            "mine".into(),
            serde_json::json!([composed_entry(&Value::Null, "mine", "0.1.0", "ef", "t")]),
        );
        let local = compose_index(entries, "t");
        let hosted = ["alertmanager".to_owned(), "mine".to_owned()];
        let mut by_chart = upstream.clone();

        merge(&mut upstream, &local, &hosted, HybridMerge::Version);
        assert_eq!(
            versions_of(&upstream, "alertmanager"),
            ["1.43.0", "9.0.0", "1.43.1"]
        );
        assert_eq!(
            find_entry(&upstream, "alertmanager", "1.43.0").unwrap()["description"],
            "ours"
        );
        assert_eq!(versions_of(&upstream, "mine"), ["0.1.0"]);

        // The upstream 1.43.1 is gone: the name is ours.
        merge(&mut by_chart, &local, &hosted, HybridMerge::Chart);
        assert_eq!(versions_of(&by_chart, "alertmanager"), ["1.43.0", "9.0.0"]);
        assert_eq!(versions_of(&by_chart, "redis"), ["20.0.0"]);
    }

    #[test]
    fn a_chart_hosted_here_but_hidden_from_the_caller_is_absent_not_public() {
        let mut upstream = parse_index(INDEX).unwrap();
        let local = compose_index(serde_json::Map::new(), "t");
        merge(
            &mut upstream,
            &local,
            &["redis".to_owned()],
            HybridMerge::Chart,
        );
        assert!(upstream.pointer("/entries/redis").is_none());
        assert!(!versions_of(&upstream, "alertmanager").is_empty());
    }

    #[test]
    fn index_metadata_keeps_the_chart_yaml_fields_only() {
        let chart = serde_json::json!({"apiVersion": "v2", "name": "x", "version": "1.0.0", "appVersion": "2", "secret": "no", "home": null});
        assert_eq!(
            index_metadata_from_chart_yaml(&chart),
            serde_json::json!({"apiVersion": "v2", "name": "x", "version": "1.0.0", "appVersion": "2"})
        );
    }

    #[test]
    fn index_metadata_has_the_shape_helms_strict_index_reader_accepts() {
        let chart = serde_json::json!({
            "name": "x",
            "version": "1.0.0",
            "appVersion": 1.1,
            "maintainers": [{"name": "a", "github": "b"}, null, "c"],
            "dependencies": [{"name": "d", "repository": "r", "import-values": ["x"], "extra": 1}],
            "annotations": {"k": "v", "n": 3},
            "keywords": ["a", 1],
            "deprecated": "yes",
        });
        assert_eq!(
            index_metadata_from_chart_yaml(&chart),
            serde_json::json!({
                "name": "x",
                "version": "1.0.0",
                "maintainers": [{"name": "a"}],
                "dependencies": [{"name": "d", "repository": "r", "import-values": ["x"]}],
                "annotations": {"k": "v"},
                "keywords": ["a"],
            })
        );
    }

    #[test]
    fn an_index_past_the_yaml_crates_default_node_budget_parses() {
        // 30 000 versions × 10 nodes is past serde-saphyr's 250 000-node
        // default — the size of the prometheus-community index.
        let mut text = String::from("apiVersion: v1\nentries:\n  big:\n");
        for i in 0..30_000 {
            text.push_str(&format!(
                "  - name: big\n    version: 0.0.{i}\n    digest: d{i}\n    urls:\n    - big-0.0.{i}.tgz\n"
            ));
        }
        assert_eq!(
            versions_of(&parse_index(&text).unwrap(), "big").len(),
            30_000
        );
    }

    #[test]
    fn is_oci_does_not_slice_inside_a_character() {
        for url in ["ñandú-1.0.0.tgz", "oci:/ñ", "oci", ""] {
            assert!(!is_oci(&serde_json::json!({ "urls": [url] })), "{url}");
        }
        assert!(is_oci(&serde_json::json!({ "urls": ["OCI://r/c"] })));
    }
}
