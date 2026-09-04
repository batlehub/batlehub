use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use batlehub_adapters::db::PgQuotaRepository;
use batlehub_adapters::registry::{
    CargoRegistryClient, ComposerRegistryClient, CondaRegistryClient, FanoutRegistryClient,
    ForgejoRegistryClient, GithubRegistryClient, GitlabRegistryClient, GoProxyRegistryClient,
    JetbrainsMarketplaceRegistryClient, MavenRegistryClient, NodeDistRegistryClient,
    NpmRegistryClient, NugetRegistryClient, OpenVsxRegistryClient, PathProxyRegistryClient,
    PypiRegistryClient, RubyGemsRegistryClient, TerraformRegistryClient, UpstreamHttpOptions,
    VsCodeMarketplaceRegistryClient,
};
use batlehub_config::schema::{
    QuotaEnforcement as ConfigQuotaEnforcement, RegistryConfig, RuleConfig, UpstreamAuthConfig,
};
use batlehub_core::{
    entities::{
        RegistryKind, ReleaseAgeGateParams, ResolutionPolicy, Role, SecurityPolicy, Severity,
    },
    ports::{ArtifactScanner, PolicyRepository, SbomRepository, VulnerabilityRepository},
    rules::{
        BlockListRule, CveGateRule, DenyLatestRule, LicenseGateRule, RbacRule, ReleaseAgeGateRule,
        RequireSignedReleaseRule, TrustedPublisherRule, VerdictGateRule, VersionGateRule,
    },
    services::{
        BlockListScanner, QuotaEnforcement, QuotaService, RecordedVulnerabilityScanner,
        RegistryPolicy, RegistryQuotaConfig, RuleAsScanner, VerdictService,
    },
};

/// What a `[security]` registry's chain needs that a plain one does not
/// (RFC 0018 §6.5): the verdict service, its profile, and the policy store
/// the `security_verdict` exemption is read from.
#[derive(Clone)]
pub(super) struct SecurityWiring {
    pub service: Arc<VerdictService>,
    pub policy: SecurityPolicy,
    pub policy_repo: Option<Arc<dyn PolicyRepository>>,
}
use batlehub_web::CargoIndexProxy;

/// Parse a config-file role string. Fails on anything but the 3 recognized
/// values instead of silently treating a typo as `anonymous` — a config that
/// meant to grant a bypass/permission to a real role must not silently end up
/// granting (or denying) it to nobody.
pub(super) fn parse_role(s: &str) -> anyhow::Result<Role> {
    match s {
        "admin" => Ok(Role::Admin),
        "user" => Ok(Role::User),
        "anonymous" => Ok(Role::Anonymous),
        other => anyhow::bail!("unknown role '{other}' (expected admin, user, or anonymous)"),
    }
}

pub(super) fn upstream_options(
    reg: &RegistryConfig,
    global_proxy: Option<&batlehub_config::schema::UpstreamProxyConfig>,
) -> UpstreamHttpOptions {
    let (bearer_token, basic_auth, custom_header) = match &reg.upstream_auth {
        Some(UpstreamAuthConfig::Bearer(b)) => (Some(b.token.clone()), None, None),
        Some(UpstreamAuthConfig::Basic(b)) => {
            (None, Some((b.username.clone(), b.password.clone())), None)
        }
        Some(UpstreamAuthConfig::Header(h)) => {
            (None, None, Some((h.name.clone(), h.value.clone())))
        }
        None => (None, None, None),
    };
    let proxy = reg.proxy.as_ref().or(global_proxy);
    UpstreamHttpOptions {
        bearer_token,
        basic_auth,
        custom_header,
        ca_cert_path: reg.tls.as_ref().and_then(|t| t.ca_cert_path.clone()),
        search_url: reg.search_url.clone(),
        proxy_url: proxy.map(|p| p.url.clone()),
        proxy_username: proxy.and_then(|p| p.username.clone()),
        proxy_password: proxy.and_then(|p| p.password.clone()),
        no_proxy: proxy.and_then(|p| p.no_proxy.clone()),
    }
}

/// The sparse-index base for a cargo registry: `index_url` when configured,
/// otherwise derived from the API upstream.
///
/// Shared by [`build_cargo_index`] and [`build_registry_client`] so the index
/// the handler's presence check sees and the index the client actually fetches
/// cannot drift apart.
pub(super) fn cargo_index_url(reg: &RegistryConfig) -> String {
    if let Some(ref url) = reg.index_url {
        return url.clone();
    }
    let upstream = reg
        .upstreams
        .first()
        .map(|s| s.as_str())
        .unwrap_or("https://crates.io");
    if upstream.contains("crates.io") {
        "https://index.crates.io".to_owned()
    } else {
        upstream.to_owned()
    }
}

pub(super) fn build_cargo_index(
    reg: &RegistryConfig,
    global_proxy: Option<&batlehub_config::schema::UpstreamProxyConfig>,
) -> anyhow::Result<CargoIndexProxy> {
    let index_url = cargo_index_url(reg);
    let opts = upstream_options(reg, global_proxy);
    let http = batlehub_adapters::registry::apply_upstream_options(
        reqwest::Client::builder().user_agent("batlehub/0.1"),
        &opts,
    )?;
    tracing::info!(index_url = %index_url, "cargo sparse index proxy configured");
    Ok(CargoIndexProxy { http, index_url })
}

/// Build the per-registry repository-metadata signing keys for `deb`/`rpm`/`pacman`
/// registries that configured `[registries.repo_signing]`. Registries without a
/// key host unsigned repositories.
pub(super) fn build_repo_signer_map(
    cfg: &batlehub_config::schema::AppConfig,
) -> anyhow::Result<batlehub_web::RepoSignerMap> {
    use batlehub_adapters::repo::OpenPgpSigner;
    let mut map = HashMap::new();
    for reg in &cfg.registries {
        if let Some(sign) = &reg.repo_signing {
            let signer = OpenPgpSigner::from_seed_hex(
                &sign.seed_hex,
                sign.created.unwrap_or(0),
                sign.user_id.as_deref().unwrap_or("BatleHub"),
            )
            .map_err(|e| anyhow::anyhow!("building repo signing key for '{}': {e}", reg.name))?;
            map.insert(reg.name.clone(), Arc::new(signer));
        }
    }
    Ok(batlehub_web::RepoSignerMap::from(map))
}

/// Build the upstream client for `reg`, with the rate-limit budget the forge
/// clients draw on (RFC 0019 §5.2). `None` builds them unbudgeted, which is
/// what a deployment with no database gets.
pub(super) fn build_registry_client(
    reg: &RegistryConfig,
    global_proxy: Option<&batlehub_config::schema::UpstreamProxyConfig>,
    budget: Option<&Arc<dyn batlehub_core::ports::RateLimitBudget>>,
) -> anyhow::Result<Arc<dyn batlehub_core::ports::RegistryClient>> {
    fn resolve_urls(configured: &[String], default: &str) -> Vec<String> {
        if configured.is_empty() {
            vec![default.to_owned()]
        } else {
            configured.to_vec()
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn make_one(
        kind: RegistryKind,
        url: &str,
        opts: &UpstreamHttpOptions,
        path_allow: &[String],
        cargo_index: &str,
        registry_name: &str,
        budget: Option<&Arc<dyn batlehub_core::ports::RateLimitBudget>>,
    ) -> anyhow::Result<Arc<dyn batlehub_core::ports::RegistryClient>> {
        // The path-addressed kinds all share one client, so the `path_allow`
        // allowlist is applied uniformly to them. Config validation has already
        // rejected `path_allow` on any other kind.
        let path_proxy =
            |ty: &str| -> anyhow::Result<Arc<dyn batlehub_core::ports::RegistryClient>> {
                Ok(Arc::new(
                    PathProxyRegistryClient::new(ty, url, opts)?.with_path_allow(path_allow)?,
                ))
            };
        // Exhaustive match over `RegistryKind`: adding a new variant is a compile
        // error here until an adapter arm is added, instead of silently falling
        // through to a runtime "no adapter compiled in" bail.
        let client: Arc<dyn batlehub_core::ports::RegistryClient> = match kind {
            // The two forge clients of RFC 0019 phase 1 draw every API call on
            // the shared budget; GitLab joins them in phase 4.
            RegistryKind::Github => {
                let c = GithubRegistryClient::new(url, opts)?;
                Arc::new(match budget {
                    Some(b) => c.with_budget(registry_name, Arc::clone(b)),
                    None => c,
                })
            }
            RegistryKind::Forgejo => {
                let c = ForgejoRegistryClient::new(url, opts)?;
                Arc::new(match budget {
                    Some(b) => c.with_budget(registry_name, Arc::clone(b)),
                    None => c,
                })
            }
            RegistryKind::Gitlab => Arc::new(GitlabRegistryClient::new(url, opts)?),
            RegistryKind::Npm => Arc::new(NpmRegistryClient::new(url, opts)?),
            RegistryKind::Cargo => {
                Arc::new(CargoRegistryClient::new(url, opts)?.with_index_url(cargo_index))
            }
            RegistryKind::Nuget => Arc::new(NugetRegistryClient::new(url, opts)?),
            RegistryKind::Openvsx => Arc::new(OpenVsxRegistryClient::new(url, opts)?),
            RegistryKind::Goproxy => Arc::new(GoProxyRegistryClient::new(url, opts)?),
            RegistryKind::VscodeMarketplace => {
                Arc::new(VsCodeMarketplaceRegistryClient::new(url, opts)?)
            }
            RegistryKind::Maven => Arc::new(MavenRegistryClient::new(url, opts)?),
            RegistryKind::Terraform => Arc::new(TerraformRegistryClient::new(url, opts)?),
            RegistryKind::Rubygems => Arc::new(RubyGemsRegistryClient::new(url, opts)?),
            RegistryKind::Composer => Arc::new(ComposerRegistryClient::new(url, opts)?),
            RegistryKind::Pypi => Arc::new(PypiRegistryClient::new(url, opts)?),
            RegistryKind::Conda => Arc::new(CondaRegistryClient::new(url, opts)?),
            RegistryKind::JetbrainsMarketplace => {
                Arc::new(JetbrainsMarketplaceRegistryClient::new(url, opts)?)
            }
            RegistryKind::Deb => path_proxy("deb")?,
            RegistryKind::Rpm => path_proxy("rpm")?,
            RegistryKind::Pacman => path_proxy("pacman")?,
            RegistryKind::Jetbrains => path_proxy("jetbrains")?,
            RegistryKind::Generic => path_proxy("generic")?,
            RegistryKind::Nodedist => Arc::new(NodeDistRegistryClient::new(url, opts)?),
        };
        Ok(client)
    }

    let opts = upstream_options(reg, global_proxy);
    let kind: RegistryKind = reg.registry_type.parse().map_err(anyhow::Error::msg)?;
    let urls = match kind {
        RegistryKind::Github => resolve_urls(&reg.upstreams, "https://api.github.com"),
        RegistryKind::Forgejo => resolve_urls(&reg.upstreams, "https://codeberg.org"),
        RegistryKind::Gitlab => resolve_urls(&reg.upstreams, "https://gitlab.com"),
        RegistryKind::Npm => resolve_urls(&reg.upstreams, "https://registry.npmjs.org"),
        RegistryKind::Cargo => resolve_urls(&reg.upstreams, "https://crates.io"),
        RegistryKind::Nuget => resolve_urls(&reg.upstreams, "https://api.nuget.org"),
        RegistryKind::Openvsx => resolve_urls(&reg.upstreams, "https://open-vsx.org"),
        RegistryKind::Goproxy => resolve_urls(&reg.upstreams, "https://proxy.golang.org"),
        RegistryKind::VscodeMarketplace => {
            resolve_urls(&reg.upstreams, "https://marketplace.visualstudio.com")
        }
        RegistryKind::Maven => resolve_urls(&reg.upstreams, "https://repo1.maven.org/maven2"),
        RegistryKind::Terraform => resolve_urls(&reg.upstreams, "https://registry.terraform.io"),
        RegistryKind::Rubygems => resolve_urls(&reg.upstreams, "https://rubygems.org"),
        RegistryKind::Composer => resolve_urls(&reg.upstreams, "https://repo.packagist.org"),
        RegistryKind::Pypi => resolve_urls(&reg.upstreams, "https://pypi.org"),
        RegistryKind::Conda => resolve_urls(&reg.upstreams, "https://conda.anaconda.org"),
        // Deb/RPM have no universal default upstream; proxy/hybrid mode requires an
        // explicit `upstreams` entry. The placeholder keeps a client constructible
        // for local-only mode, where the upstream is never contacted.
        RegistryKind::Deb => resolve_urls(&reg.upstreams, "https://deb.debian.org"),
        RegistryKind::Rpm => resolve_urls(&reg.upstreams, "https://example.invalid/rpm"),
        // Arch mirrors share a common layout (`$repo/os/$arch/…`); the geo CDN is a
        // sensible default, overridable via `upstreams`.
        RegistryKind::Pacman => resolve_urls(&reg.upstreams, "https://geo.mirror.pkgbuild.com"),
        // JetBrains IDE archives are served from a stable CDN, so it's a sensible
        // default; the marketplace (plugin ecosystem) is its own kind below.
        RegistryKind::Jetbrains => resolve_urls(&reg.upstreams, "https://download.jetbrains.com"),
        RegistryKind::JetbrainsMarketplace => {
            resolve_urls(&reg.upstreams, "https://plugins.jetbrains.com")
        }
        // A generic mirror has no meaningful default upstream — it mirrors whatever
        // file tree the operator points it at. Config validation already requires an
        // explicit `upstreams` entry, so the placeholder is unreachable in practice.
        RegistryKind::Generic => resolve_urls(&reg.upstreams, "https://example.invalid/generic"),
        // The tree nvm itself defaults to; io.js takes a second registry
        // pointed at `https://iojs.org/dist` (RFC 0010 §4.1).
        RegistryKind::Nodedist => resolve_urls(&reg.upstreams, "https://nodejs.org/dist"),
    };
    let cargo_index = cargo_index_url(reg);
    if urls.len() == 1 {
        make_one(
            kind,
            &urls[0],
            &opts,
            &reg.path_allow,
            &cargo_index,
            &reg.name,
            budget,
        )
    } else {
        let clients = urls
            .iter()
            .map(|u| {
                make_one(
                    kind,
                    u,
                    &opts,
                    &reg.path_allow,
                    &cargo_index,
                    &reg.name,
                    budget,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Arc::new(FanoutRegistryClient::new(
            &reg.registry_type,
            clients,
        )))
    }
}

/// Parse a rule's `bypass_roles` list, failing loudly on the first unrecognized
/// entry rather than silently dropping it to `anonymous` (see `parse_role`).
fn parse_bypass_roles(roles: &[String]) -> anyhow::Result<Vec<Role>> {
    roles.iter().map(|r| parse_role(r)).collect()
}

/// RFC 0015 §4.2 rule 2: an ecosystem verb is rejected on a registry of another
/// type, not silently inert.
///
/// The registry type is known here, so this is checkable — and "I granted it and
/// nothing happened" is the failure mode the closed enum exists to remove. A
/// `npm:dist-tags:write` on a Maven registry is a mistake with an obvious fix,
/// and the only bad outcome is not telling anyone.
fn check_action_scoping(
    rbac: &RbacRule,
    reg: &RegistryConfig,
    kind: Option<RegistryKind>,
) -> anyhow::Result<()> {
    // An unparseable `registry_type` is already a config-validation failure; if
    // one somehow reaches here there is no kind to check against, and inventing
    // one would reject a verb for the wrong reason.
    let Some(kind) = kind else {
        return Ok(());
    };
    let offenders: Vec<String> = rbac
        .permissions
        .values()
        .chain(rbac.group_permissions.values())
        .flatten()
        .filter(|a| !a.applies_to(kind))
        .map(|a| a.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "registry '{}': [registries.rbac] grants {} on a '{}' registry, which does not \
         define {}. An ecosystem permission is only grantable on the registry types that \
         implement it.",
        reg.name,
        offenders.join(", "),
        reg.registry_type,
        if offenders.len() == 1 { "it" } else { "them" },
    )
}

pub(super) fn build_policy(
    reg: &RegistryConfig,
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
    vuln_repo: Arc<dyn VulnerabilityRepository>,
    sbom_repo: Arc<dyn SbomRepository>,
) -> anyhow::Result<RegistryPolicy> {
    build_policy_with_rules(reg, &reg.rules, repo, vuln_repo, sbom_repo, None)
}

/// [`build_policy`] for a registry that opted into `[security]` (RFC 0018
/// §6.5): `VerdictGateRule` first, where `BlockListRule` sits otherwise, and
/// the five gates the verdict covers left out of the chain — they run as
/// scanners through [`build_internal_scanners`] instead.
pub(super) fn build_security_policy(
    reg: &RegistryConfig,
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
    vuln_repo: Arc<dyn VulnerabilityRepository>,
    sbom_repo: Arc<dyn SbomRepository>,
    wiring: SecurityWiring,
) -> anyhow::Result<RegistryPolicy> {
    build_policy_with_rules(reg, &reg.rules, repo, vuln_repo, sbom_repo, Some(wiring))
}

/// The gates a `[security]` registry runs as scanners (RFC 0018 §6.1):
/// `block_list` always, `cve_gate` and the three metadata gates when the
/// registry configured them. Every one of them is local and required.
pub(super) fn build_internal_scanners(
    reg: &RegistryConfig,
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
    vuln_repo: Arc<dyn VulnerabilityRepository>,
    sbom_repo: Arc<dyn SbomRepository>,
    // RFC 0014's rows and whether `on_confirmed = "block"`, when the audit
    // is enabled: the `upstream-presence` scanner (0018 decision 29).
    upstream_presence: Option<(Arc<dyn batlehub_core::ports::UpstreamStatusPort>, bool)>,
) -> anyhow::Result<Vec<Arc<dyn ArtifactScanner>>> {
    let registry_kind: Option<RegistryKind> = reg.registry_type.parse().ok();
    let mut out: Vec<Arc<dyn ArtifactScanner>> = vec![Arc::new(BlockListScanner { repo })];
    if let Some((status, deny)) = upstream_presence {
        out.push(Arc::new(batlehub_core::services::UpstreamPresenceScanner {
            status,
            deny,
        }));
    }
    for rule_cfg in &reg.rules {
        let rule: Option<Box<dyn batlehub_core::rules::Rule>> = match rule_cfg {
            RuleConfig::CveGate(_) => {
                out.push(Arc::new(RecordedVulnerabilityScanner {
                    repo: Arc::clone(&vuln_repo),
                }));
                None
            }
            RuleConfig::LicenseGate(cfg) => Some(Box::new(LicenseGateRule::new(
                Arc::clone(&sbom_repo),
                cfg.allow.clone(),
                cfg.deny.clone(),
                cfg.allow_unknown,
                Vec::new(),
                true,
            ))),
            RuleConfig::RequireSignedRelease(cfg) if cfg.enabled => Some(Box::new(
                RequireSignedReleaseRule::new(Vec::new())
                    .with_deny_missing_signature(cfg.deny_missing_signature),
            )),
            RuleConfig::TrustedPublisher(cfg) => Some(Box::new(TrustedPublisherRule::new(
                &cfg.allow,
                Vec::new(),
                registry_kind,
            ))),
            _ => None,
        };
        if let Some(rule) = rule {
            if let Some(scanner) = RuleAsScanner::for_rule(rule) {
                out.push(Arc::new(scanner));
            }
        }
    }
    Ok(out)
}

/// RFC 0015 §4.1 — one rule chain per `[[registries.namespaces]]` block that
/// overrides a gate.
///
/// Returns `(match_prefix, chain)` in config order, and **only for namespaces
/// that declared `rules`**: a namespace with none runs the registry's chain, and
/// building an identical second copy would double the per-request `Box<dyn Rule>`
/// footprint of every registry for no behaviour change.
///
/// Built at config load rather than per request because a rule is a trait object
/// with owned state — a `Regex`, a repository handle, a parsed role set — and
/// composing one per request would put that construction on the download path.
/// Namespaces are few and declared, so there is a small fixed number of chains.
///
/// The merge is **per gate** (§4.1), which is the composition rule that differs
/// from `versioning`'s and for a stated reason: a wholesale override would force
/// an operator to redeclare `cve_gate` and `license_gate` in order to change
/// `release_age`, and a forgotten one is a gate silently switched off.
pub(super) fn build_namespace_policies(
    reg: &RegistryConfig,
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
    vuln_repo: Arc<dyn VulnerabilityRepository>,
    sbom_repo: Arc<dyn SbomRepository>,
    security: Option<SecurityWiring>,
) -> anyhow::Result<Vec<(String, Arc<RegistryPolicy>)>> {
    let mut out = Vec::new();
    for ns in &reg.namespaces {
        let Some(overrides) = ns.rules.as_deref() else {
            continue;
        };
        let owned = effective_rules(&reg.rules, overrides)?;
        out.push((
            ns.match_prefix.clone(),
            Arc::new(build_policy_with_rules(
                reg,
                &owned,
                Arc::clone(&repo),
                Arc::clone(&vuln_repo),
                Arc::clone(&sbom_repo),
                security.clone(),
            )?),
        ));
    }
    Ok(out)
}

/// RFC 0015 §4.1's **per-gate** merge: `base`, with each entry replaced by the
/// override of the same gate, and anything the override adds appended.
///
/// Order is the registry's, so a gate the namespace only re-tunes stays where it
/// was in the chain. The chain is evaluated in order and the gates are
/// independent, so this is presentation rather than semantics — but a chain that
/// reordered itself when a namespace touched one gate would make a `403`'s
/// provenance harder to read for no gain.
fn effective_rules(
    base: &[RuleConfig],
    overrides: &[RuleConfig],
) -> anyhow::Result<Vec<RuleConfig>> {
    let mut effective: Vec<&RuleConfig> = Vec::new();
    for b in base {
        match overrides.iter().find(|o| same_gate(o, b)) {
            Some(over) => effective.push(over),
            None => effective.push(b),
        }
    }
    for over in overrides {
        if !base.iter().any(|b| same_gate(over, b)) {
            effective.push(over);
        }
    }
    effective.into_iter().map(clone_rule_config).collect()
}

/// Whether two rule blocks configure the same gate.
///
/// By discriminant rather than by a name string: the `kind` tag and the enum
/// variant are the same fact, and comparing the tag would mean a second spelling
/// of it that could drift.
fn same_gate(a: &RuleConfig, b: &RuleConfig) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// `RuleConfig` is not `Clone` — it holds only plain data, but deriving `Clone`
/// on eight structs to move a reference into an owned list is a wider change
/// than round-tripping through the serialisation it already has. The round trip
/// cannot fail for a value that was deserialised from TOML a moment ago; the
/// `Result` is there so a future field that does not serialise is a load error
/// rather than a panic.
fn clone_rule_config(cfg: &RuleConfig) -> anyhow::Result<RuleConfig> {
    let value = serde_json::to_value(cfg)?;
    Ok(serde_json::from_value(value)?)
}

fn build_policy_with_rules(
    reg: &RegistryConfig,
    rule_configs: &[RuleConfig],
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
    vuln_repo: Arc<dyn VulnerabilityRepository>,
    sbom_repo: Arc<dyn SbomRepository>,
    security: Option<SecurityWiring>,
) -> anyhow::Result<RegistryPolicy> {
    // Best-effort: an unrecognized `registry_type` is already rejected by config
    // validation before this runs, but rules that need the kind (e.g.
    // TrustedPublisherRule) degrade to their fail-closed default rather than
    // panicking if this ever sees one anyway.
    let registry_kind: Option<RegistryKind> = reg.registry_type.parse().ok();
    let mut rules: Vec<Box<dyn batlehub_core::rules::Rule>> = Vec::new();
    let rbac_perms = HashMap::from([
        (Role::Anonymous, reg.rbac.anonymous.clone()),
        (Role::User, reg.rbac.user.clone()),
        (Role::Admin, reg.rbac.admin.clone()),
    ]);
    // `RbacRule` is **not** pushed. RFC 0015 §5.1: it becomes grant resolution,
    // and `Authorizer::check_grants` is what answers the question it used to.
    // The chain keeps the gates that judge the *artifact* — blocks, CVEs,
    // licence, age, signature — because those are a different question (§5.2).
    //
    // It is still constructed, because constructing it is what validates the
    // config: an unknown verb, an unknown prefix or an ecosystem verb on the
    // wrong registry type are all refused here, at load, exactly as they were
    // before the rule stopped being evaluated. `build_registry_grants` re-reads
    // the same block and would reject the same things; doing it here too keeps
    // the diagnostic attached to `[registries.rbac]` rather than to a hierarchy
    // the operator did not write.
    let rbac = RbacRule::from_patterns(rbac_perms)
        .and_then(|r| r.with_group_patterns(reg.rbac.groups.clone()))
        .map_err(|e| anyhow::anyhow!("registry '{}': [registries.rbac]: {e}", reg.name))?;
    check_action_scoping(&rbac, reg, registry_kind)?;
    // RFC 0018 §5.1: on a `[security]` registry the verdict gate is first and
    // the five gates it covers are not in the chain at all — one answer, and
    // no second path where a gate can fail open beside the verdict.
    let quarantined = security.is_some();
    match security {
        Some(w) => rules.push(Box::new(VerdictGateRule::new(
            w.service,
            w.policy,
            w.policy_repo,
        ))),
        None => rules.push(Box::new(BlockListRule::new(repo))),
    }
    for rule_cfg in rule_configs {
        if quarantined
            && matches!(
                rule_cfg,
                RuleConfig::CveGate(_)
                    | RuleConfig::LicenseGate(_)
                    | RuleConfig::RequireSignedRelease(_)
                    | RuleConfig::TrustedPublisher(_)
            )
        {
            continue;
        }
        match rule_cfg {
            RuleConfig::ReleaseAgeGate(cfg) => {
                let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                rules.push(Box::new(
                    ReleaseAgeGateRule::new(Duration::from_secs(cfg.min_age_secs), bypass)
                        .with_deny_missing_timestamp(cfg.deny_missing_timestamp()),
                ));
            }
            RuleConfig::RequireSignedRelease(cfg) => {
                if cfg.enabled {
                    let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                    rules.push(Box::new(
                        RequireSignedReleaseRule::new(bypass)
                            .with_deny_missing_signature(cfg.deny_missing_signature),
                    ));
                }
            }
            RuleConfig::DenyLatest(cfg) => {
                let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                rules.push(Box::new(DenyLatestRule::new(bypass)));
            }
            RuleConfig::CveGate(cfg) => {
                let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                let min_severity = Severity::parse(&cfg.min_severity).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown min_severity '{}' (expected unknown, low, medium, high, or critical)",
                        cfg.min_severity
                    )
                })?;
                rules.push(Box::new(CveGateRule::new(
                    Arc::clone(&vuln_repo),
                    min_severity,
                    bypass,
                    cfg.block,
                )));
            }
            RuleConfig::LicenseGate(cfg) => {
                let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                rules.push(Box::new(LicenseGateRule::new(
                    Arc::clone(&sbom_repo),
                    cfg.allow.clone(),
                    cfg.deny.clone(),
                    cfg.allow_unknown,
                    bypass,
                    cfg.block,
                )));
            }
            RuleConfig::VersionGate(cfg) => {
                let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                rules.push(Box::new(VersionGateRule::new(
                    &cfg.allow, &cfg.block, bypass,
                )));
            }
            RuleConfig::TrustedPublisher(cfg) => {
                let bypass = parse_bypass_roles(&cfg.bypass_roles)?;
                rules.push(Box::new(TrustedPublisherRule::new(
                    &cfg.allow,
                    bypass,
                    registry_kind,
                )));
            }
        }
    }
    Ok(RegistryPolicy {
        metadata_ttl: Some(Duration::from_secs(reg.cache.metadata_ttl_secs)),
        firewall_only: reg.firewall_only,
        serve_stale_metadata: reg.cache.serve_stale,
        artifact_ttl: reg.cache.artifact_ttl_secs.map(Duration::from_secs),
        rules,
    })
}

/// The same two settings `build_registry_policy` just consumed, kept as plain
/// data so the catalog can read them back.
///
/// `RegistryPolicy` puts `artifact_ttl` behind a field the web layer does not
/// hold, and folds the release-age gate into a `Box<dyn Rule>` that exposes
/// nothing. Both are right for the download path — a rule should be opaque to
/// its caller — and both make it impossible to answer "would this be
/// quarantined, is this past its TTL" for a *listing*, which never runs a rule.
///
/// Deliberately built here, in the same function body's neighbourhood as the
/// rule itself and off the identical `RegistryConfig`: the failure mode this
/// shape invites is the two drifting, and the defence is that changing
/// `min_age_secs` or `bypass_roles` means editing one config block that both
/// read. If a third caller ever needs these, it takes them from here rather
/// than re-deriving them.
pub(super) fn build_resolution_policy(reg: &RegistryConfig) -> anyhow::Result<ResolutionPolicy> {
    let mut release_age = None;
    for rule_cfg in &reg.rules {
        if let RuleConfig::ReleaseAgeGate(cfg) = rule_cfg {
            release_age = Some(ReleaseAgeGateParams {
                min_age: Duration::from_secs(cfg.min_age_secs),
                bypass_roles: parse_bypass_roles(&cfg.bypass_roles)?,
                deny_missing_timestamp: cfg.deny_missing_timestamp(),
            });
        }
    }
    // RFC 0018 §4.1: `security.min_age_secs` replaces the rule, so the
    // catalog's "would be held" reads the same number the gate holds on.
    if let Some(sec) = &reg.security {
        release_age = Some(ReleaseAgeGateParams {
            min_age: Duration::from_secs(sec.min_age_secs),
            bypass_roles: Vec::new(),
            deny_missing_timestamp: sec.hold_missing_timestamp,
        });
    }
    Ok(ResolutionPolicy {
        artifact_ttl: reg.cache.artifact_ttl_secs.map(Duration::from_secs),
        release_age,
    })
}

pub(super) fn build_quota_service(
    pool: sqlx::PgPool,
    registries: &[RegistryConfig],
) -> QuotaService {
    let repo = Arc::new(PgQuotaRepository::new(pool));
    let configs = registries
        .iter()
        .filter_map(|reg| {
            reg.quota.as_ref().map(|q| {
                let enforcement = match q.enforcement {
                    ConfigQuotaEnforcement::Block => QuotaEnforcement::Block,
                    ConfigQuotaEnforcement::Warn => QuotaEnforcement::Warn,
                };
                (
                    reg.name.clone(),
                    RegistryQuotaConfig {
                        max_storage_bytes_per_user: q.max_storage_bytes_per_user,
                        max_packages_per_user: q.max_packages_per_user,
                        warn_threshold: q.warn_threshold_pct.clamp(1, 100) as f64 / 100.0,
                        enforcement,
                    },
                )
            })
        })
        .collect();
    QuotaService::new(repo, configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_adapters::in_memory::{
        InMemoryPackageRepository, InMemoryVulnerabilityRepository, NoopSbomRepository,
    };
    use batlehub_config::schema::UpstreamProxyConfig;

    fn make_registry(reg_type: &str, name: &str, extra: &str) -> RegistryConfig {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            registries: Vec<RegistryConfig>,
        }
        let toml_str = format!(
            r#"
            [[registries]]
            type = "{reg_type}"
            name = "{name}"
            {extra}
            "#
        );
        let w: Wrapper = toml::from_str(&toml_str).expect("valid registry toml");
        w.registries.into_iter().next().unwrap()
    }

    #[test]
    fn parse_role_variants() {
        assert_eq!(parse_role("admin").unwrap(), Role::Admin);
        assert_eq!(parse_role("user").unwrap(), Role::User);
        assert_eq!(parse_role("anonymous").unwrap(), Role::Anonymous);
    }

    #[test]
    fn parse_role_unknown_value_errors() {
        assert!(parse_role("anything-else").is_err());
    }

    #[test]
    fn upstream_options_bearer_auth() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            [registries.upstream_auth]
            type = "bearer"
            token = "tok123"
            "#,
        );
        let opts = upstream_options(&r, None);
        assert_eq!(opts.bearer_token.as_deref(), Some("tok123"));
        assert!(opts.basic_auth.is_none());
        assert!(opts.custom_header.is_none());
    }

    #[test]
    fn upstream_options_basic_auth() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            [registries.upstream_auth]
            type = "basic"
            username = "u"
            password = "p"
            "#,
        );
        let opts = upstream_options(&r, None);
        assert_eq!(opts.basic_auth, Some(("u".to_owned(), "p".to_owned())));
        assert!(opts.bearer_token.is_none());
    }

    #[test]
    fn upstream_options_header_auth() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            [registries.upstream_auth]
            type = "header"
            name = "X-Api-Key"
            value = "secret"
            "#,
        );
        let opts = upstream_options(&r, None);
        assert_eq!(
            opts.custom_header,
            Some(("X-Api-Key".to_owned(), "secret".to_owned()))
        );
    }

    #[test]
    fn upstream_options_proxy_from_registry_overrides_global() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            [registries.proxy]
            url = "http://reg-proxy:3128"
            "#,
        );
        let global = UpstreamProxyConfig {
            url: "http://global-proxy:3128".into(),
            username: None,
            password: None,
            no_proxy: None,
        };
        let opts = upstream_options(&r, Some(&global));
        assert_eq!(opts.proxy_url.as_deref(), Some("http://reg-proxy:3128"));
    }

    #[test]
    fn upstream_options_proxy_falls_back_to_global() {
        let r = make_registry("npm", "reg", "");
        let global = UpstreamProxyConfig {
            url: "http://global-proxy:3128".into(),
            username: Some("u".into()),
            password: Some("p".into()),
            no_proxy: Some("localhost".into()),
        };
        let opts = upstream_options(&r, Some(&global));
        assert_eq!(opts.proxy_url.as_deref(), Some("http://global-proxy:3128"));
        assert_eq!(opts.proxy_username.as_deref(), Some("u"));
        assert_eq!(opts.proxy_password.as_deref(), Some("p"));
        assert_eq!(opts.no_proxy.as_deref(), Some("localhost"));
    }

    #[test]
    fn upstream_options_search_url_and_ca_cert() {
        let r = make_registry(
            "maven",
            "reg",
            r#"
            search_url = "https://search.example.com"
            [registries.tls]
            ca_cert_path = "/etc/ssl/ca.pem"
            "#,
        );
        let opts = upstream_options(&r, None);
        assert_eq!(
            opts.search_url.as_deref(),
            Some("https://search.example.com")
        );
        assert_eq!(opts.ca_cert_path.as_deref(), Some("/etc/ssl/ca.pem"));
    }

    #[test]
    fn build_cargo_index_uses_explicit_index_url() {
        let r = make_registry(
            "cargo",
            "reg",
            r#"index_url = "https://my-index.example.com""#,
        );
        let proxy = build_cargo_index(&r, None).unwrap();
        assert_eq!(proxy.index_url, "https://my-index.example.com");
    }

    #[test]
    fn build_cargo_index_defaults_crates_io_to_index_crates_io() {
        let r = make_registry("cargo", "reg", "");
        let proxy = build_cargo_index(&r, None).unwrap();
        assert_eq!(proxy.index_url, "https://index.crates.io");
    }

    #[test]
    fn build_cargo_index_non_crates_upstream_used_directly() {
        let r = make_registry(
            "cargo",
            "reg",
            r#"upstreams = ["https://my-mirror.example.com"]"#,
        );
        let proxy = build_cargo_index(&r, None).unwrap();
        assert_eq!(proxy.index_url, "https://my-mirror.example.com");
    }

    #[test]
    fn build_registry_client_unknown_type_errors() {
        let r = make_registry("not-a-real-type", "reg", "");
        assert!(build_registry_client(&r, None, None).is_err());
    }

    #[test]
    fn build_registry_client_single_upstream() {
        let r = make_registry("npm", "reg", "");
        let client = build_registry_client(&r, None, None).unwrap();
        assert_eq!(client.registry_type(), "npm");
    }

    #[test]
    fn build_registry_client_multi_upstream_uses_fanout() {
        let r = make_registry(
            "npm",
            "reg",
            r#"upstreams = ["https://a.example.com", "https://b.example.com"]"#,
        );
        let client = build_registry_client(&r, None, None).unwrap();
        assert_eq!(client.registry_type(), "npm");
    }

    /// Build a policy, or return why it was refused.
    fn policy_for(r: &RegistryConfig) -> anyhow::Result<batlehub_core::services::RegistryPolicy> {
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        build_policy(
            r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
        )
    }

    /// The refusal message, or a failure naming what was wrongly accepted.
    ///
    /// `RegistryPolicy` holds boxed `dyn Rule`s and so cannot be `Debug`, which
    /// rules out `expect_err`.
    fn refusal(r: &RegistryConfig, what: &str) -> String {
        match policy_for(r) {
            Ok(_) => panic!("{what}"),
            Err(e) => e.to_string(),
        }
    }

    /// RFC 0015 §4.9: a verb this build does not know is a config-load error.
    ///
    /// Before phase 1 this config started, and the permission it named was
    /// granted to nobody — a typo with no error, no log line and no effect,
    /// which is the failure the closed enum exists to end.
    #[test]
    fn an_unknown_verb_refuses_to_load() {
        let r = make_registry(
            "npm",
            "reg",
            r#"[registries.rbac]
               anonymous = []
               user = ["releases:raed"]
               admin = ["*"]"#,
        );
        let err = refusal(&r, "a typo'd verb must not load");
        assert!(err.contains("releases:raed"), "{err}");
        assert!(
            err.contains("reg"),
            "the error must name the registry: {err}"
        );
    }

    /// The same for a prefix nobody carries, which is the mistake that would
    /// otherwise expand to the empty set and grant nothing.
    #[test]
    fn an_unknown_prefix_refuses_to_load() {
        let r = make_registry(
            "npm",
            "reg",
            r#"[registries.rbac]
               anonymous = []
               user = ["release:*"]
               admin = ["*"]"#,
        );
        let err = refusal(&r, "an unknown prefix must not load");
        assert!(err.contains("release"), "{err}");
    }

    /// RFC 0015 §4.2 rule 2: an ecosystem verb is rejected on a registry of
    /// another type, not silently inert.
    #[test]
    fn an_ecosystem_verb_is_refused_on_the_wrong_registry_type() {
        let r = make_registry(
            "maven",
            "mvn1",
            r#"[registries.rbac]
               anonymous = []
               user = ["npm:dist-tags:write"]
               admin = ["*"]"#,
        );
        let err = refusal(&r, "npm:dist-tags:write is not a Maven permission");
        assert!(err.contains("npm:dist-tags:write"), "{err}");
        assert!(err.contains("maven"), "the error must name the type: {err}");
    }

    /// …and accepted on one that does define it, so the rule is a scope rather
    /// than a ban. Without this the test above would pass against a build that
    /// rejected the verb everywhere.
    #[test]
    fn an_ecosystem_verb_is_accepted_on_its_own_registry_type() {
        let r = make_registry(
            "npm",
            "npm1",
            r#"[registries.rbac]
               anonymous = []
               user = ["npm:dist-tags:write"]
               admin = ["*"]"#,
        );
        assert!(policy_for(&r).is_ok());
    }

    /// A group grant is scoped exactly like a role grant.
    ///
    /// Separate map, separate code path, and the one an operator is more likely
    /// to reach for — `[registries.rbac.groups]` is where per-team permissions
    /// go, so a check that covered only the three role fields would miss the
    /// common case.
    #[test]
    fn a_group_grant_is_scoped_too() {
        let r = make_registry(
            "maven",
            "mvn1",
            r#"[registries.rbac]
               anonymous = []
               user = []
               admin = ["*"]
               [registries.rbac.groups]
               "team-a" = ["npm:dist-tags:write"]"#,
        );
        let err = refusal(&r, "group grants are scoped too");
        assert!(err.contains("npm:dist-tags:write"), "{err}");
    }

    /// Every permission this repository ships in a config file is a real one.
    ///
    /// Phase 1 turned an unknown verb from a silent no-op into a startup
    /// failure, and the first thing that change found was three of them in this
    /// repository: `releases:write` in `docs/guide/configuration.md` and
    /// `packages:publish` in both perf configs. All three had been granting
    /// nothing to nobody for as long as they had existed, which is precisely why
    /// nobody noticed.
    ///
    /// So the shipped configs are parsed here rather than trusted. A doc example
    /// that cannot start the server is worse than no example — the reader has no
    /// reason to doubt it.
    #[test]
    fn every_shipped_config_grants_only_real_permissions() {
        for path in [
            "../config.example.toml",
            "../perf/config.perf.toml",
            "../perf/config.perf-s3.toml",
            "../perf/config.perf-authz.toml",
        ] {
            let raw = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("{path} must be readable: {e}"));

            #[derive(serde::Deserialize)]
            struct Wrapper {
                #[serde(default)]
                registries: Vec<RegistryConfig>,
            }
            // Parsed directly rather than through `batlehub_config::load`, which
            // also interpolates `${ENV}` and would make this a test about the
            // environment it happens to run in.
            let w: Wrapper =
                toml::from_str(&raw).unwrap_or_else(|e| panic!("{path} must be valid TOML: {e}"));

            for reg in &w.registries {
                let patterns = reg
                    .rbac
                    .anonymous
                    .iter()
                    .chain(&reg.rbac.user)
                    .chain(&reg.rbac.admin)
                    .chain(reg.rbac.groups.values().flatten())
                    .cloned()
                    .collect::<Vec<_>>();
                batlehub_core::entities::expand_patterns(
                    &patterns,
                    batlehub_core::entities::WildcardScope::Legacy,
                )
                .unwrap_or_else(|e| panic!("{path}: registry '{}': {e}", reg.name));
            }
        }
    }

    #[test]
    fn build_policy_default_has_rbac_and_block_list_rules() {
        let r = make_registry("npm", "reg", "");
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        let policy = build_policy(
            &r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
        )
        .unwrap();
        let names: Vec<&str> = policy.rules.iter().map(|rule| rule.name()).collect();
        // `rbac` is absent by design since RFC 0015 phase 3: grant resolution
        // answers that question now (§5.1), and the chain keeps only the gates
        // that judge the artifact.
        assert_eq!(names, vec!["block_list"]);
        assert!(!policy.firewall_only);
        assert!(policy.serve_stale_metadata);
        assert_eq!(policy.metadata_ttl, Some(Duration::from_secs(300)));
        assert!(policy.artifact_ttl.is_none());
    }

    /// RFC 0018 §6.5: a `[security]` registry's chain is the verdict gate
    /// first, the two selection gates after it, and none of the five the
    /// verdict covers.
    #[test]
    fn a_security_registry_chain_is_the_verdict_gate_and_the_selection_gates() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            [registries.security]
            mode = "block"

            [[registries.rules]]
            kind = "cve_gate"
            min_severity = "high"

            [[registries.rules]]
            kind = "deny_latest"

            [[registries.rules]]
            kind = "license_gate"
            deny = ["AGPL-3.0"]
            "#,
        );
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        let verdicts = batlehub_adapters::in_memory::InMemoryVerdictRepository::new();
        let queue = batlehub_adapters::in_memory::InMemoryScanQueue::new();
        let wiring = SecurityWiring {
            service: Arc::new(VerdictService::new(verdicts, queue)),
            policy: r
                .security
                .as_ref()
                .unwrap()
                .to_policy("reg", &Default::default()),
            policy_repo: None,
        };
        let policy = build_security_policy(
            &r,
            Arc::clone(&repo),
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
            wiring,
        )
        .unwrap();
        let names: Vec<&str> = policy.rules.iter().map(|rule| rule.name()).collect();
        assert_eq!(names, vec!["verdict_gate", "deny_latest"]);

        let scanners = build_internal_scanners(
            &r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
            None,
        )
        .unwrap();
        let names: Vec<&str> = scanners.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["block_list", "cve_gate", "license_gate"]);
    }

    #[test]
    fn build_policy_with_release_age_gate_and_deny_latest_rules() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            firewall_only = true

            [registries.cache]
            metadata_ttl_secs = 60
            serve_stale = false
            artifact_ttl_secs = 3600

            [[registries.rules]]
            kind = "release_age_gate"
            min_age_secs = 7200
            bypass_roles = ["admin"]

            [[registries.rules]]
            kind = "deny_latest"
            bypass_roles = ["user"]

            [[registries.rules]]
            kind = "require_signed_release"
            enabled = true
            "#,
        );
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        let policy = build_policy(
            &r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
        )
        .unwrap();
        let names: Vec<&str> = policy.rules.iter().map(|rule| rule.name()).collect();
        assert_eq!(
            names,
            vec![
                "block_list",
                "release_age_gate",
                "deny_latest",
                "require_signed_release"
            ]
        );
        assert!(policy.firewall_only);
        assert!(!policy.serve_stale_metadata);
        assert_eq!(policy.metadata_ttl, Some(Duration::from_secs(60)));
        assert_eq!(policy.artifact_ttl, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn build_policy_appends_cve_gate_rule() {
        let r = make_registry(
            "cargo",
            "reg",
            r#"
            [[registries.rules]]
            kind = "cve_gate"
            min_severity = "critical"
            block = true
            bypass_roles = ["admin"]
            "#,
        );
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        let policy = build_policy(
            &r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
        )
        .unwrap();
        let names: Vec<&str> = policy.rules.iter().map(|rule| rule.name()).collect();
        assert_eq!(names, vec!["block_list", "cve_gate"]);
    }

    #[test]
    fn build_policy_appends_license_gate_rule() {
        let r = make_registry(
            "npm",
            "reg",
            r#"
            [[registries.rules]]
            kind = "license_gate"
            allow = ["MIT", "Apache-2.0"]
            deny = ["AGPL-3.0"]
            allow_unknown = false
            block = true
            bypass_roles = ["admin"]
            "#,
        );
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        let policy = build_policy(
            &r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
        )
        .unwrap();
        let names: Vec<&str> = policy.rules.iter().map(|rule| rule.name()).collect();
        assert_eq!(names, vec!["block_list", "license_gate"]);
    }

    #[test]
    fn build_policy_appends_trusted_publisher_rule() {
        let r = make_registry(
            "github",
            "reg",
            r#"
            [[registries.rules]]
            kind = "trusted_publisher"
            allow = ["my-org"]
            bypass_roles = ["admin"]
            "#,
        );
        let repo: Arc<dyn batlehub_core::ports::PackageRepository> =
            InMemoryPackageRepository::new();
        let policy = build_policy(
            &r,
            repo,
            InMemoryVulnerabilityRepository::arc(),
            NoopSbomRepository::arc(),
        )
        .unwrap();
        let names: Vec<&str> = policy.rules.iter().map(|rule| rule.name()).collect();
        assert_eq!(names, vec!["block_list", "trusted_publisher"]);
    }

    // ── RFC 0015 §4.1: per-gate rule composition ─────────────────────────────

    fn rule(toml_body: &str) -> RuleConfig {
        toml::from_str(toml_body).expect("valid rule toml")
    }

    fn gate_names(rules: &[RuleConfig]) -> Vec<String> {
        rules
            .iter()
            .map(|r| {
                serde_json::to_value(r).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    /// The composition rule that differs from `versioning`'s, and the reason it
    /// does: a wholesale override would force redeclaring `cve_gate` to change
    /// `release_age`, and a forgotten one is a gate silently switched off.
    #[test]
    fn a_namespace_override_replaces_one_gate_and_leaves_the_others() {
        let base = vec![
            rule("kind = \"release_age_gate\"\nmin_age_secs = 3600"),
            rule("kind = \"deny_latest\"\nenabled = true"),
        ];
        let overrides = vec![rule("kind = \"release_age_gate\"\nmin_age_secs = 0")];

        let effective = effective_rules(&base, &overrides).expect("merges");
        assert_eq!(
            gate_names(&effective),
            vec!["release_age_gate", "deny_latest"],
            "the untouched gate survives, in its original position"
        );
        let age = serde_json::to_value(&effective[0]).unwrap();
        assert_eq!(
            age["min_age_secs"], 0,
            "and the overridden one carries the namespace's value"
        );
    }

    /// The standing `release_age` finding §4.5 names: first-party CI publishes
    /// into a namespace that sets `min_age_secs = 0`, instead of the operator
    /// choosing between quarantining their own builds and turning the gate off
    /// everywhere.
    #[test]
    fn a_namespace_may_lift_the_quarantine_for_its_own_builds() {
        let base = vec![rule("kind = \"release_age_gate\"\nmin_age_secs = 86400")];
        let effective = effective_rules(
            &base,
            &[rule("kind = \"release_age_gate\"\nmin_age_secs = 0")],
        )
        .expect("merges");
        assert_eq!(effective.len(), 1);
        assert_eq!(
            serde_json::to_value(&effective[0]).unwrap()["min_age_secs"],
            0
        );
    }

    /// A gate the registry does not configure at all can be *added* by a
    /// namespace.
    #[test]
    fn a_namespace_may_add_a_gate_the_registry_does_not_have() {
        let base = vec![rule("kind = \"deny_latest\"\nenabled = true")];
        let effective = effective_rules(
            &base,
            &[rule("kind = \"release_age_gate\"\nmin_age_secs = 60")],
        )
        .expect("merges");
        assert_eq!(
            gate_names(&effective),
            vec!["deny_latest", "release_age_gate"]
        );
    }

    /// No overrides is the registry's chain unchanged, which is what every
    /// namespace without a `rules` block gets — and is why such namespaces get
    /// no chain of their own at all.
    #[test]
    fn no_overrides_is_the_registrys_chain() {
        let base = vec![
            rule("kind = \"release_age_gate\"\nmin_age_secs = 3600"),
            rule("kind = \"deny_latest\"\nenabled = true"),
        ];
        let effective = effective_rules(&base, &[]).expect("merges");
        assert_eq!(gate_names(&effective), gate_names(&base));
    }
}
