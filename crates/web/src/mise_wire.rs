//! RFC 0045 — the mise wire, as the server holds it.
//!
//! One `url_replacements` rule sends every host mise reaches to
//! `/proxy/{reg}/mise-versions/_wire/{host}/{rest}`. The rewrite itself runs in
//! [`crate::middleware::host_routing`] (it has to precede routing and auth);
//! this module is the state it reads and the wire's own answers.

use std::sync::Arc;

use anyhow::{bail, Result};
use batlehub_adapters::registry::mise_registry_overlay::{Collision, OverlayBackend, OverlayEntry};
use batlehub_config::schema::mise_auto_wire::{AutoWireConfig, MiseBackendConfig, OnCollision};
use batlehub_config::schema::AppConfig;
use batlehub_core::entities::RegistryKind;
use batlehub_core::services::mise_wire::{source_backend, WireRegistry, WireTable, WireWarning};

use crate::UpstreamMap;

/// The enabled wire of an instance: at most one `mise-versions` registry has
/// one (`AppConfig::validate`).
#[derive(Debug)]
pub struct MiseWire {
    /// The `mise-versions` registry whose `_wire` path is rewritten.
    pub registry: String,
    pub table: WireTable,
    pub air_gap: bool,
    pub auto_wire: AutoWireConfig,
    /// The overlay tools with their backends written, in config order.
    pub overlay: Vec<OverlayEntry>,
    /// Overlay names, aliases and `retired` names, answered locally (§4.2).
    pub local_names: std::collections::BTreeSet<String>,
    /// Index of the `mise-versions` registry in `[[registries]]`.
    pub index: usize,
    /// Every registry's index in `[[registries]]`, for warning paths.
    pub indices: std::collections::HashMap<String, usize>,
    /// What the table left out, for the config page.
    pub warnings: Vec<WireWarning>,
}

/// Request extension: the host mise asked for, on a request the wire
/// rewrote. Read by the request span (§6.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WiredFrom(pub String);

/// Request extension for `_wire-answer/{outcome}`: what the wire decided.
/// Without it the route answers `404` and nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireAnswer {
    /// `307` to this URL.
    Bounce(String),
    /// `404 not_wired`, naming `{host}/{prefix of rest}`.
    NotWired(String),
    /// `400`.
    Malformed,
    /// The air-gap sink, `{host}/{rest}` (§15 Q15: behind read on `{reg}`).
    Sink(String),
}

impl WireAnswer {
    pub fn outcome(&self) -> &'static str {
        match self {
            Self::Bounce(_) => "bounced",
            Self::NotWired(_) => "not_wired",
            Self::Malformed => "malformed",
            Self::Sink(_) => "sink",
        }
    }
}

impl MiseWire {
    /// The wire of `cfg`, or `None` when no `mise-versions` registry enables
    /// one. `upstreams` gives each registry's upstream with its kind's default
    /// resolved, as the registry list reports it to the CLI — so the server's
    /// table and the CLI's classic block select from the same view.
    ///
    /// Fails on §4.3's one check that needs the table: an overlay `source`
    /// whose registry the wire does not route.
    pub fn build(cfg: &AppConfig, upstreams: &UpstreamMap) -> Result<Option<Self>> {
        let Some((index, registry, aw)) = cfg.registries.iter().enumerate().find_map(|(i, r)| {
            r.auto_wire
                .as_ref()
                .filter(|aw| aw.enabled)
                .map(|aw| (i, r.name.clone(), aw.clone()))
        }) else {
            return Ok(None);
        };
        // (name, kind, first upstream, every upstream, path_routing)
        type Owned = (String, RegistryKind, Option<String>, Vec<String>, bool);
        let owned: Vec<Owned> = cfg
            .registries
            .iter()
            .filter_map(|r| {
                let kind = r.registry_type.parse::<RegistryKind>().ok()?;
                let mut all = upstreams.upstreams_for(&r.name);
                if all.is_empty() {
                    all = r.upstreams.clone();
                }
                Some((
                    r.name.clone(),
                    kind,
                    upstreams.upstream_for(&r.name),
                    all,
                    r.path_routing,
                ))
            })
            .collect();
        let regs: Vec<WireRegistry<'_>> = owned
            .iter()
            .map(|(name, kind, up, all, path_routing)| WireRegistry {
                name,
                kind: *kind,
                upstream: up.as_deref(),
                upstreams: all,
                path_routing: *path_routing,
            })
            .collect();
        let (table, warnings) = WireTable::build(&regs, None);
        let table = table.with_registry_archive(&registry);
        let mut overlay = Vec::with_capacity(aw.tools.len());
        for (i, t) in aw.tools.iter().enumerate() {
            let backends = match &t.source {
                None => t
                    .backends
                    .iter()
                    .map(|b| match b {
                        MiseBackendConfig::Plain(s) => OverlayBackend::Plain(s.clone()),
                        MiseBackendConfig::Full { full, options } => OverlayBackend::Full {
                            full: full.clone(),
                            options: options.clone(),
                        },
                    })
                    .collect(),
                Some(src) => {
                    let kind = cfg
                        .registries
                        .iter()
                        .find(|r| r.name == src.registry)
                        .and_then(|r| r.registry_type.parse::<RegistryKind>().ok())
                        .unwrap_or(RegistryKind::Generic);
                    let upstream = upstreams.upstream_for(&src.registry).unwrap_or_default();
                    let b = source_backend(kind, &upstream, &src.repo).map_err(|e| {
                        anyhow::anyhow!("registries[{index}].auto_wire.tools[{i}].source: {e}")
                    })?;
                    vec![match b.api_url {
                        None => OverlayBackend::Plain(b.backend),
                        Some(api_url) => {
                            let mut options = toml::Table::new();
                            options.insert("api_url".to_owned(), toml::Value::String(api_url));
                            OverlayBackend::Full {
                                full: b.backend,
                                options,
                            }
                        }
                    }]
                }
            };
            overlay.push(OverlayEntry {
                name: t.name.clone(),
                aliases: t.aliases.clone(),
                backends,
                bins: t.bins.clone(),
                description: t.description.clone(),
                version_order: t.version_order.clone(),
                on_collision: match t.on_collision {
                    OnCollision::Drop => Collision::Drop,
                    OnCollision::Overlay => Collision::Overlay,
                    OnCollision::Upstream => Collision::Upstream,
                },
            });
        }
        let local_names = aw
            .tools
            .iter()
            .flat_map(|t| std::iter::once(t.name.clone()).chain(t.aliases.iter().cloned()))
            .chain(aw.retired.iter().cloned())
            .collect();
        for (i, t) in aw.tools.iter().enumerate() {
            if let Some(src) = &t.source {
                if !table.wires_registry(&src.registry) {
                    bail!(
                        "registries[{index}].auto_wire.tools[{i}].source.registry: {:?} is not wired \
                         (its prefix is claimed by another registry, or path_routing = false)",
                        src.registry
                    );
                }
            }
        }
        for w in &warnings {
            if let WireWarning::PrefixClaimed {
                registry,
                claimed_by,
                host,
                prefix,
            } = w
            {
                tracing::warn!(
                    %registry, %claimed_by, %host, %prefix,
                    "mise wire: prefix already claimed by an earlier registry; the first declared wins"
                );
            }
        }
        let air_gap = cfg.air_gap.as_ref().is_some_and(|a| a.enabled);
        Ok(Some(Self {
            registry,
            table,
            air_gap,
            auto_wire: aw,
            overlay,
            local_names,
            index,
            indices: cfg
                .registries
                .iter()
                .enumerate()
                .map(|(i, r)| (r.name.clone(), i))
                .collect(),
            warnings,
        }))
    }

    /// The `_wire` path prefix this wire rewrites.
    pub fn wire_prefix(&self) -> String {
        format!("/proxy/{}/mise-versions/_wire/", self.registry)
    }

    /// The `_wire-answer` path for `answer`.
    pub fn answer_path(&self, answer: &WireAnswer) -> String {
        let seg = match answer {
            WireAnswer::Bounce(_) => "bounce",
            WireAnswer::NotWired(_) => "not-wired",
            WireAnswer::Malformed => "malformed",
            WireAnswer::Sink(_) => "sink",
        };
        format!("/proxy/{}/mise-versions/_wire-answer/{seg}", self.registry)
    }
}

/// Shared handle the host-routing middleware reads; swapped by reload.
pub type MiseWireSlot = Arc<std::sync::RwLock<Option<Arc<MiseWire>>>>;
