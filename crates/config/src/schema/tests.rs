use super::*;

#[test]
fn cache_policy_defaults() {
    let p: CachePolicy = toml::from_str("").unwrap();
    assert_eq!(p.metadata_ttl_secs, 300);
    assert!(p.serve_stale);
    assert!(p.artifact_ttl_secs.is_none());
    assert!(p.idle_days.is_none());
    assert!(p.max_size_bytes.is_none());
    assert!(p.keep_latest_n.is_none());
}

#[test]
fn cache_policy_full_config() {
    let raw = r#"
        metadata_ttl_secs = 60
        serve_stale = false
        artifact_ttl_secs = 3600
        idle_days = 30
        max_size_bytes = 10000000
        keep_latest_n = 5
    "#;
    let p: CachePolicy = toml::from_str(raw).unwrap();
    assert_eq!(p.metadata_ttl_secs, 60);
    assert!(!p.serve_stale);
    assert_eq!(p.artifact_ttl_secs, Some(3600));
    assert_eq!(p.idle_days, Some(30));
    assert_eq!(p.max_size_bytes, Some(10_000_000));
    assert_eq!(p.keep_latest_n, Some(5));
}

#[test]
fn cache_policy_partial_config_uses_defaults_for_unset_fields() {
    let raw = "artifact_ttl_secs = 7200";
    let p: CachePolicy = toml::from_str(raw).unwrap();
    assert_eq!(
        p.metadata_ttl_secs, 300,
        "metadata_ttl_secs should use default"
    );
    assert!(p.serve_stale, "serve_stale should default to true");
    assert_eq!(p.artifact_ttl_secs, Some(7200));
    assert!(p.idle_days.is_none());
    assert!(p.max_size_bytes.is_none());
    assert!(p.keep_latest_n.is_none());
}

#[test]
fn cache_policy_zero_keep_latest_n_is_valid() {
    let raw = "keep_latest_n = 1";
    let p: CachePolicy = toml::from_str(raw).unwrap();
    assert_eq!(p.keep_latest_n, Some(1));
}

#[test]
fn cache_policy_default_impl_matches_toml_defaults() {
    let from_default = CachePolicy::default();
    let from_toml: CachePolicy = toml::from_str("").unwrap();
    assert_eq!(from_default.metadata_ttl_secs, from_toml.metadata_ttl_secs);
    assert_eq!(from_default.serve_stale, from_toml.serve_stale);
    assert_eq!(from_default.artifact_ttl_secs, from_toml.artifact_ttl_secs);
    assert_eq!(from_default.idle_days, from_toml.idle_days);
    assert_eq!(from_default.max_size_bytes, from_toml.max_size_bytes);
    assert_eq!(from_default.keep_latest_n, from_toml.keep_latest_n);
}

// ── Feature flags ─────────────────────────────────────────────────────────────

#[test]
fn feature_flags_default_socket_badge_on() {
    let f: FeatureFlagsConfig = toml::from_str("").unwrap();
    assert!(f.socket_badge, "socket_badge defaults to true");
    assert!(FeatureFlagsConfig::default().socket_badge);
}

#[test]
fn feature_flags_can_disable_socket_badge() {
    let f: FeatureFlagsConfig = toml::from_str("socket_badge = false").unwrap();
    assert!(!f.socket_badge);
}

#[test]
fn integrity_defaults_verify_and_block_on_mismatch() {
    // An empty (or partial) block must fall back to verify + block-on-mismatch.
    let i: IntegrityConfig = toml::from_str("").unwrap();
    assert!(i.enabled);
    assert!(i.block_on_mismatch);
    assert!(!i.require_metadata);
    assert!(i.bypass_roles.is_empty());

    let d = IntegrityConfig::default();
    assert!(d.enabled);
    assert!(d.block_on_mismatch);
    assert!(!d.require_metadata);
}

#[test]
fn integrity_parses_full_block() {
    let raw = r#"
        type = "cargo"
        name = "crates"
        [integrity]
        enabled = true
        block_on_mismatch = false
        require_metadata = true
        bypass_roles = ["admin"]
    "#;
    let reg: RegistryConfig = toml::from_str(raw).unwrap();
    let i = reg.integrity.expect("integrity block parsed");
    assert!(i.enabled);
    assert!(!i.block_on_mismatch);
    assert!(i.require_metadata);
    assert_eq!(i.bypass_roles, vec!["admin".to_owned()]);
}

#[test]
fn registry_parses_feature_flags_block() {
    let raw = r#"
        type = "cargo"
        name = "crates"
        [feature_flags]
        socket_badge = false
    "#;
    let reg: RegistryConfig = toml::from_str(raw).unwrap();
    assert!(!reg.feature_flags.unwrap().socket_badge);
}

// ── CVE gate rule ─────────────────────────────────────────────────────────────

#[test]
fn cve_gate_rule_parses_with_defaults() {
    let raw = r#"kind = "cve_gate""#;
    let rule: RuleConfig = toml::from_str(raw).unwrap();
    match rule {
        RuleConfig::CveGate(c) => {
            assert_eq!(c.min_severity, "high");
            assert!(!c.block);
            assert!(c.bypass_roles.is_empty());
        }
        other => panic!("expected CveGate, got {other:?}"),
    }
}

#[test]
fn cve_gate_rule_parses_full() {
    let raw = r#"
        kind = "cve_gate"
        min_severity = "critical"
        block = true
        bypass_roles = ["admin"]
    "#;
    let rule: RuleConfig = toml::from_str(raw).unwrap();
    match rule {
        RuleConfig::CveGate(c) => {
            assert_eq!(c.min_severity, "critical");
            assert!(c.block);
            assert_eq!(c.bypass_roles, vec!["admin".to_owned()]);
        }
        other => panic!("expected CveGate, got {other:?}"),
    }
}

// ── Licence gate rule ─────────────────────────────────────────────────────────

/// The defaults are the whole safety story: warn-only, and an unknown licence
/// is permitted. A `license_gate` added to a config must not start refusing
/// traffic the moment it is written (RFC 0004-bis §13.1).
#[test]
fn license_gate_rule_parses_with_defaults() {
    let raw = r#"kind = "license_gate""#;
    let rule: RuleConfig = toml::from_str(raw).unwrap();
    match rule {
        RuleConfig::LicenseGate(c) => {
            assert!(c.allow.is_empty());
            assert!(c.deny.is_empty());
            assert!(
                c.allow_unknown,
                "an unknown licence must not deny by default"
            );
            assert!(!c.block, "the gate must be warn-only until asked otherwise");
            assert!(c.bypass_roles.is_empty());
        }
        other => panic!("expected LicenseGate, got {other:?}"),
    }
}

#[test]
fn license_gate_rule_parses_full() {
    let raw = r#"
        kind = "license_gate"
        allow = ["MIT", "Apache-2.0"]
        deny = ["AGPL-3.0"]
        allow_unknown = false
        block = true
        bypass_roles = ["admin"]
    "#;
    let rule: RuleConfig = toml::from_str(raw).unwrap();
    match rule {
        RuleConfig::LicenseGate(c) => {
            assert_eq!(c.allow, vec!["MIT".to_owned(), "Apache-2.0".to_owned()]);
            assert_eq!(c.deny, vec!["AGPL-3.0".to_owned()]);
            assert!(!c.allow_unknown);
            assert!(c.block);
            assert_eq!(c.bypass_roles, vec!["admin".to_owned()]);
        }
        other => panic!("expected LicenseGate, got {other:?}"),
    }
}

// ── Trusted publisher rule ────────────────────────────────────────────────────

#[test]
fn trusted_publisher_rule_parses_with_defaults() {
    let raw = r#"kind = "trusted_publisher""#;
    let rule: RuleConfig = toml::from_str(raw).unwrap();
    match rule {
        RuleConfig::TrustedPublisher(c) => {
            assert!(c.allow.is_empty());
            assert!(c.bypass_roles.is_empty());
        }
        other => panic!("expected TrustedPublisher, got {other:?}"),
    }
}

#[test]
fn trusted_publisher_rule_parses_full() {
    let raw = r#"
        kind = "trusted_publisher"
        allow = ["my-org", "trusted-user"]
        bypass_roles = ["admin"]
    "#;
    let rule: RuleConfig = toml::from_str(raw).unwrap();
    match rule {
        RuleConfig::TrustedPublisher(c) => {
            assert_eq!(
                c.allow,
                vec!["my-org".to_owned(), "trusted-user".to_owned()]
            );
            assert_eq!(c.bypass_roles, vec!["admin".to_owned()]);
        }
        other => panic!("expected TrustedPublisher, got {other:?}"),
    }
}

// ── Vulnerability scan ────────────────────────────────────────────────────────

#[test]
fn vulnerability_scan_defaults() {
    let v: VulnerabilityScanConfig = toml::from_str("enabled = true").unwrap();
    assert!(v.enabled);
    assert_eq!(v.interval_secs, 86_400);
    assert_eq!(v.batch_size, 100);
    assert!(v.osv_api_url.is_none());
}

#[test]
fn vulnerability_scan_full() {
    let raw = r#"
        enabled = true
        interval_secs = 3600
        osv_api_url = "https://osv.local"
        batch_size = 25
    "#;
    let v: VulnerabilityScanConfig = toml::from_str(raw).unwrap();
    assert_eq!(v.interval_secs, 3600);
    assert_eq!(v.osv_api_url.as_deref(), Some("https://osv.local"));
    assert_eq!(v.batch_size, 25);
}

// ── Test helper: a minimal parseable AppConfig ────────────────────────────────

/// Parse a minimal `AppConfig` with `extra` appended verbatim.
///
/// `[server]` comes last so `extra` can start with bare `key = value` lines that
/// land in it, then open further tables (`[ip_blocking]`, `[[registries]]`, …).
///
/// Deliberately calls `toml::from_str` + `validate` rather than
/// `batlehub_config::load_from_str`, so a test can assert on validation errors
/// without env-var expansion or overrides in the way.
pub(super) fn parse_config(extra: &str) -> AppConfig {
    let raw = format!(
        r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"

        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"

        [server]
        host = "127.0.0.1"
        port = 8080
{extra}
        "#
    );
    toml::from_str(&raw).expect("test config parses")
}

// ── Proxy trust ───────────────────────────────────────────────────────────────

#[test]
fn trusted_proxies_absent_empty_and_populated_are_distinguishable() {
    assert!(parse_config("").server.trusted_proxies.is_none());
    assert_eq!(
        parse_config("        trusted_proxies = []")
            .server
            .trusted_proxies,
        Some(vec![])
    );
    assert_eq!(
        parse_config(r#"        trusted_proxies = ["10.42.0.0/16"]"#)
            .server
            .trusted_proxies,
        Some(vec!["10.42.0.0/16".to_owned()])
    );
}

#[test]
fn parse_trusted_proxies_accepts_cidr_and_bare_addresses() {
    let entries = vec![
        "10.42.0.0/16".to_owned(),
        "192.168.1.10".to_owned(),
        "2001:db8::/32".to_owned(),
        "::1".to_owned(),
    ];
    let nets = parse_trusted_proxies(&entries).expect("all entries valid");
    assert_eq!(nets.len(), 4);
    // A bare address is widened to its host prefix, so it still matches exactly one IP.
    assert_eq!(nets[1].prefix_len(), 32);
    assert_eq!(nets[3].prefix_len(), 128);
}

#[test]
fn parse_trusted_proxies_rejects_garbage() {
    let err = parse_trusted_proxies(&["not-an-ip".to_owned()]).unwrap_err();
    assert!(
        err.contains("not-an-ip"),
        "error names the bad entry: {err}"
    );
}

#[test]
fn validate_rejects_a_malformed_trusted_proxy_entry() {
    let err = parse_config(r#"        trusted_proxies = ["10.0.0.0/99"]"#)
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("[server].trusted_proxies"), "{err}");
}

#[test]
fn a_malformed_deprecated_trusted_proxy_entry_warns_instead_of_failing_the_boot() {
    // The deprecated key predates any validator: entries it could not parse were
    // dropped. Rejecting one now would turn an upgrade into a CrashLoopBackOff
    // for a config file that never changed.
    let cfg = parse_config(
        r#"
        [ip_blocking]
        trusted_proxies = ["10.0.0.5", "ingress.internal"]"#,
    );
    cfg.validate()
        .expect("a stale entry must not fail the boot");

    let warning = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::PROXY_TRUST_INVALID_DEPRECATED_ENTRY)
        .expect("the dropped entry is surfaced as a warning");
    assert!(warning.message.contains("ingress.internal"), "{warning:?}");
}

#[test]
fn effective_trusted_proxies_prefers_server_over_the_deprecated_alias() {
    let cfg = parse_config(
        r#"        trusted_proxies = ["10.0.0.0/8"]

        [ip_blocking]
        trusted_proxies = ["192.0.2.1"]"#,
    );
    assert_eq!(
        cfg.effective_trusted_proxies(),
        Some(&["10.0.0.0/8".to_owned()][..])
    );
    assert!(cfg.shadows_deprecated_trusted_proxies());
    assert!(!cfg.uses_deprecated_trusted_proxies());
}

#[test]
fn effective_trusted_proxies_falls_back_to_the_deprecated_alias() {
    let cfg = parse_config(
        r#"
        [ip_blocking]
        trusted_proxies = ["192.0.2.1"]"#,
    );
    assert_eq!(
        cfg.effective_trusted_proxies(),
        Some(&["192.0.2.1".to_owned()][..])
    );
    assert!(cfg.uses_deprecated_trusted_proxies());
    assert!(!cfg.shadows_deprecated_trusted_proxies());
}

#[test]
fn an_empty_deprecated_list_is_not_a_policy_but_an_empty_server_list_is() {
    // `[ip_blocking].trusted_proxies` defaults to `[]`, so an empty value there
    // carries no operator intent — unlike `[server].trusted_proxies = []`, which
    // is an explicit "trust nobody".
    let cfg = parse_config(
        r#"
        [ip_blocking]
        enabled = true"#,
    );
    assert!(cfg.effective_trusted_proxies().is_none());

    let cfg = parse_config("        trusted_proxies = []");
    assert_eq!(cfg.effective_trusted_proxies(), Some(&[][..]));
}

// ── Config warnings ───────────────────────────────────────────────────────────

fn warning_codes(cfg: &AppConfig) -> Vec<String> {
    cfg.warnings().into_iter().map(|w| w.code).collect()
}

#[test]
fn a_config_with_no_proxy_trust_policy_warns_about_it() {
    let cfg = parse_config("");
    assert_eq!(
        warning_codes(&cfg),
        vec![warnings::PROXY_TRUST_UNCONFIGURED]
    );
    assert_eq!(cfg.warnings()[0].path, "server.trusted_proxies");
}

#[test]
fn an_explicit_empty_server_list_is_a_policy_and_does_not_warn() {
    assert!(parse_config("        trusted_proxies = []")
        .warnings()
        .is_empty());
}

#[test]
fn a_configured_server_list_does_not_warn() {
    assert!(parse_config(r#"        trusted_proxies = ["10.0.0.0/8"]"#)
        .warnings()
        .is_empty());
}

#[test]
fn the_deprecated_key_alone_warns_but_is_honoured() {
    let cfg = parse_config(
        r#"
        [ip_blocking]
        trusted_proxies = ["10.0.0.1"]"#,
    );
    assert_eq!(
        warning_codes(&cfg),
        vec![warnings::PROXY_TRUST_DEPRECATED_KEY_ONLY]
    );
    assert_eq!(cfg.warnings()[0].path, "ip_blocking.trusted_proxies");
    assert!(cfg.effective_trusted_proxies().is_some());
}

#[test]
fn a_shadowed_deprecated_key_warns_and_names_itself() {
    let cfg = parse_config(
        r#"        trusted_proxies = ["10.0.0.0/8"]

        [ip_blocking]
        trusted_proxies = ["10.0.0.1"]"#,
    );
    assert_eq!(
        warning_codes(&cfg),
        vec![warnings::PROXY_TRUST_SHADOWED_DEPRECATED_KEY]
    );
    assert_eq!(cfg.warnings()[0].path, "ip_blocking.trusted_proxies");
}

// ── Host-based routing (RFC 0001 §4.3) ────────────────────────────────────────

/// A config with proxy trust already declared, so host-routing tests assert on
/// the condition they are actually about.
fn host_routing_config(extra: &str) -> AppConfig {
    parse_config(&format!(
        "        trusted_proxies = [\"10.0.0.0/8\"]\n{extra}"
    ))
}

fn npm_registry(name: &str, extra: &str) -> String {
    format!(
        "
        [[registries]]
        type = \"npm\"
        name = \"{name}\"
{extra}"
    )
}

#[test]
fn registry_host_fields_default_to_no_hosts_and_path_routing_on() {
    let cfg = parse_config(&npm_registry("npm1", ""));
    assert!(cfg.registries[0].hosts.is_empty());
    assert!(cfg.registries[0].path_routing);
    assert!(!cfg.host_routing_configured());
    assert!(cfg.registry_host_bindings().is_empty());
}

#[test]
fn wildcard_and_explicit_hosts_both_bind_to_the_registry() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
{}"#,
        npm_registry("npm1", "        hosts = [\"NPM.Acme.io\"]")
    ));
    cfg.validate().expect("valid");
    let bindings = cfg.registry_host_bindings();
    assert_eq!(bindings.len(), 2);
    // Explicit entries come first and are normalised.
    assert_eq!(bindings[0].host, "npm.acme.io");
    assert!(bindings[0].explicit);
    assert_eq!(bindings[1].host, "npm1.hub.example.com");
    assert!(!bindings[1].explicit);
    // The explicit host is the advertised one.
    assert_eq!(
        cfg.registry_public_urls(),
        vec![("npm1".to_owned(), "https://npm.acme.io".to_owned())]
    );
}

#[test]
fn public_url_falls_back_to_the_wildcard_host_and_honours_scheme() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
        scheme = "http"
{}"#,
        npm_registry("npm1", "")
    ));
    assert_eq!(
        cfg.registry_public_urls(),
        vec![("npm1".to_owned(), "http://npm1.hub.example.com".to_owned())]
    );
}

#[test]
fn enabled_without_base_domain_is_rejected() {
    let err = host_routing_config(
        r#"
        [subdomain_routing]
        enabled = true"#,
    )
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("base_domain"), "{err}");
}

#[test]
fn a_url_shaped_base_domain_is_rejected() {
    let err = host_routing_config(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "https://hub.example.com""#,
    )
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("base_domain"), "{err}");
    assert!(err.contains("scheme prefix"), "{err}");
}

#[test]
fn a_base_domain_with_a_path_is_rejected() {
    let err = host_routing_config(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com/registry""#,
    )
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("base_domain"), "{err}");
}

#[test]
fn two_registries_claiming_the_same_host_is_rejected() {
    let cfg = host_routing_config(&format!(
        "{}{}",
        npm_registry("npm1", "        hosts = [\"npm.acme.io\"]"),
        npm_registry("npm2", "        hosts = [\"NPM.acme.io.\"]")
    ));
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("npm.acme.io"), "{err}");
    assert!(err.contains("npm1") && err.contains("npm2"), "{err}");
}

#[test]
fn an_explicit_host_colliding_with_another_registrys_wildcard_is_rejected() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
{}{}"#,
        npm_registry("npm1", ""),
        npm_registry("npm2", "        hosts = [\"npm1.hub.example.com\"]")
    ));
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("npm1.hub.example.com"), "{err}");
}

#[test]
fn a_registry_may_restate_its_own_wildcard_host_explicitly() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
{}"#,
        npm_registry("npm1", "        hosts = [\"npm1.hub.example.com\"]")
    ));
    cfg.validate()
        .expect("same-registry duplicate is not a conflict");
}

#[test]
fn a_host_equal_to_the_base_domain_is_rejected() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
{}"#,
        npm_registry("npm1", "        hosts = [\"hub.example.com\"]")
    ));
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("admin API"), "{err}");
}

#[test]
fn malformed_host_entries_are_rejected() {
    for (entry, needle) in [
        ("https://npm.acme.io", "scheme prefix"),
        ("npm.acme.io/proxy", "contains '/'"),
        ("   ", "empty"),
    ] {
        let cfg = host_routing_config(&npm_registry(
            "npm1",
            &format!("        hosts = [\"{entry}\"]"),
        ));
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains(needle), "entry {entry:?} → {err}");
    }
}

#[test]
fn path_routing_false_without_a_reachable_host_is_rejected() {
    let cfg = host_routing_config(&format!(
        "{}{}",
        npm_registry("npm1", "        hosts = [\"npm.acme.io\"]"),
        npm_registry("npm2", "        path_routing = false")
    ));
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("npm2") && err.contains("no ingress"), "{err}");
}

#[test]
fn path_routing_false_with_a_host_is_accepted() {
    let cfg = host_routing_config(&npm_registry(
        "npm1",
        "        hosts = [\"npm.acme.io\"]\n        path_routing = false",
    ));
    cfg.validate().expect("valid");
    assert_eq!(cfg.host_only_registries(), vec!["npm1".to_owned()]);
}

#[test]
fn path_routing_false_satisfied_by_a_wildcard_host_is_accepted() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
{}"#,
        npm_registry("npm1", "        path_routing = false")
    ));
    cfg.validate().expect("wildcard host is a reachable host");
}

#[test]
fn host_routing_without_any_trusted_proxy_policy_is_rejected() {
    let err = parse_config(&npm_registry("npm1", "        hosts = [\"npm.acme.io\"]"))
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("trusted_proxies"), "{err}");
    // The message contains the exact TOML to paste.
    assert!(err.contains("[server]"), "{err}");
}

#[test]
fn host_routing_is_accepted_when_only_the_deprecated_key_is_set_and_it_warns() {
    let cfg = parse_config(&format!(
        r#"{}
        [ip_blocking]
        trusted_proxies = ["10.0.0.1"]"#,
        npm_registry("npm1", "        hosts = [\"npm.acme.io\"]")
    ));
    cfg.validate()
        .expect("the deprecated alias satisfies the requirement");
    assert_eq!(
        warning_codes(&cfg),
        vec![warnings::PROXY_TRUST_DEPRECATED_KEY_ONLY]
    );
}

#[test]
fn an_empty_server_list_satisfies_the_host_routing_requirement() {
    // "trust nobody" is a stated policy: BatleHub is exposed directly and only
    // the `Host` header routes.
    parse_config(&format!(
        "        trusted_proxies = []\n{}",
        npm_registry("npm1", "        hosts = [\"npm.acme.io\"]")
    ))
    .validate()
    .expect("an explicit empty list is a policy");
}

#[test]
fn a_non_dns_label_registry_name_warns_and_derives_no_wildcard() {
    let cfg = host_routing_config(&format!(
        r#"
        [subdomain_routing]
        enabled = true
        base_domain = "hub.example.com"
{}{}"#,
        npm_registry("my_registry", ""),
        npm_registry("npm1", "")
    ));
    cfg.validate()
        .expect("valid — this degrades, it does not fail");

    let w = cfg.warnings();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].code, warnings::SUBDOMAIN_INVALID_DNS_LABEL);
    assert_eq!(w[0].path, "registries[0].name");

    // Only the well-named registry gets a wildcard host.
    let hosts: Vec<String> = cfg
        .registry_host_bindings()
        .into_iter()
        .map(|b| b.host)
        .collect();
    assert_eq!(hosts, vec!["npm1.hub.example.com".to_owned()]);
}

#[test]
fn no_dns_label_warning_when_wildcard_derivation_is_off() {
    let cfg = parse_config(&npm_registry("my_registry", ""));
    assert!(!warning_codes(&cfg).contains(&warnings::SUBDOMAIN_INVALID_DNS_LABEL.to_owned()));
}

// ── Licence gate warnings ─────────────────────────────────────────────────────

/// Base config plus one registry carrying a `license_gate` rule.
fn config_with_license_gate(registry_type: &str, rule_body: &str) -> AppConfig {
    parse_config(&format!(
        r#"
        [[registries]]
        type = "{registry_type}"
        name = "reg"

        [registries.sbom]
        enabled = true

        [[registries.rules]]
        kind = "license_gate"
{rule_body}"#
    ))
}

/// A registry type with a manifest parser is the case the rule was written for,
/// and it must stay quiet — a warning that fires on correct config is noise
/// that teaches operators to ignore the channel.
#[test]
fn license_gate_on_a_supported_type_does_not_warn() {
    let cfg = config_with_license_gate("npm", r#"        allow = ["MIT"]"#);
    assert!(!warning_codes(&cfg)
        .iter()
        .any(|c| c.starts_with("license-gate")));
}

/// Sixteen of the twenty-one registry types have no manifest parser, so the
/// licence is permanently unknown and — with the default `allow_unknown = true`
/// — the rule never denies anything. Nothing errors at runtime, which is
/// exactly why it has to be said here (RFC 0004-bis §13.1).
#[test]
fn license_gate_on_a_type_with_no_parser_warns_that_it_never_fires() {
    let cfg = config_with_license_gate("goproxy", r#"        allow = ["MIT"]"#);
    assert!(warning_codes(&cfg).contains(&warnings::LICENSE_GATE_NO_EXTRACTOR.to_owned()));

    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::LICENSE_GATE_NO_EXTRACTOR)
        .expect("warning emitted");
    assert_eq!(w.path, "registries[0].rules[0]");
    assert!(w.message.contains("never denies"), "{}", w.message);
    // The message names what *is* covered, so the operator can act on it.
    assert!(w.message.contains("npm"), "{}", w.message);
}

/// The opposite consequence from the same missing parser, and the one an
/// operator will be triaging under pressure: every download refused. It gets
/// its own code so it can be found by name.
#[test]
fn license_gate_that_refuses_everything_gets_its_own_code() {
    let cfg = config_with_license_gate(
        "goproxy",
        "        block = true\n        allow_unknown = false",
    );
    let codes = warning_codes(&cfg);
    assert!(codes.contains(&warnings::LICENSE_GATE_DENIES_EVERYTHING.to_owned()));
    // Not both: the two states are mutually exclusive, and emitting each would
    // make the dangerous one harder to see.
    assert!(!codes.contains(&warnings::LICENSE_GATE_NO_EXTRACTOR.to_owned()));
}

/// `allow_unknown = false` without `block` is still warn-only, so nothing is
/// refused — it is the inert case, not the dangerous one.
#[test]
fn allow_unknown_false_without_block_is_still_only_inert() {
    let cfg = config_with_license_gate("goproxy", "        allow_unknown = false");
    let codes = warning_codes(&cfg);
    assert!(codes.contains(&warnings::LICENSE_GATE_NO_EXTRACTOR.to_owned()));
    assert!(!codes.contains(&warnings::LICENSE_GATE_DENIES_EVERYTHING.to_owned()));
}

/// A registry with no `license_gate` has nothing to say, whatever its type.
#[test]
fn a_registry_without_a_license_gate_never_warns_about_one() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "goproxy"
        name = "reg""#,
    );
    assert!(!warning_codes(&cfg)
        .iter()
        .any(|c| c.starts_with("license-gate")));
}

/// The licence is a side effect of SBOM generation, so a `license_gate` on a
/// registry with SBOM off never sees anything — even on a registry type whose
/// parser works perfectly. This was found by running the server, not by reading
/// it: extraction was correct, the rule was loaded, and no licence was ever
/// stored because `maybe_trigger_sbom` returned early.
#[test]
fn license_gate_without_sbom_enabled_warns_that_nothing_is_extracted() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"

        [[registries.rules]]
        kind = "license_gate"
        deny = ["AGPL-3.0"]"#,
    );
    let codes = warning_codes(&cfg);
    assert!(codes.contains(&warnings::LICENSE_GATE_SBOM_DISABLED.to_owned()));

    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::LICENSE_GATE_SBOM_DISABLED)
        .expect("warning emitted");
    assert!(w.message.contains("never deny anything"), "{}", w.message);
    assert!(w.message.contains("[registries.sbom]"), "{}", w.message);
}

// ── require_signed_release vs. local publishing ──────────────────────────────

/// A hybrid registry enables the rule to gate its *proxied* half; the side
/// effect is that every local publish without `X-Artifact-Signature` is recorded
/// unsigned and refused at download. `deny_missing_signature` does not save it —
/// that flag governs `is_signed: None`, and a local row reports `Some(false)`.
#[test]
fn require_signed_release_without_publish_side_signing_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"
        mode = "hybrid"

        [[registries.rules]]
        kind = "require_signed_release""#,
    );
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES)
        .expect("warning emitted");
    assert!(
        w.message.contains("signing.required = true"),
        "{}",
        w.message
    );
    assert!(w.path.contains("rules[0]"), "{}", w.path);
}

/// Local mode publishes too, so it carries the same trap.
#[test]
fn require_signed_release_warns_in_local_mode_as_well() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"
        mode = "local"

        [[registries.rules]]
        kind = "require_signed_release""#,
    );
    assert!(warning_codes(&cfg)
        .contains(&warnings::REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES.to_owned()));
}

/// Pairing the two is the fix, and must silence it.
#[test]
fn require_signed_release_with_signing_required_is_coherent() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"
        mode = "hybrid"

        [registries.signing]
        required = true

        [[registries.rules]]
        kind = "require_signed_release""#,
    );
    assert!(!warning_codes(&cfg)
        .contains(&warnings::REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES.to_owned()));
}

/// A proxy-mode registry accepts no publishes, so there is no second half to
/// disagree with and no warning to raise.
#[test]
fn require_signed_release_on_a_proxy_registry_is_not_flagged() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"

        [[registries.rules]]
        kind = "require_signed_release""#,
    );
    assert!(!warning_codes(&cfg)
        .contains(&warnings::REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES.to_owned()));
}

/// `signing` present but `required = false` is the easier state to overlook,
/// because the block is *there* — same shape as the SBOM case above.
#[test]
fn a_signing_block_without_required_still_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"
        mode = "hybrid"

        [registries.signing]
        verify_on_download = true

        [[registries.rules]]
        kind = "require_signed_release""#,
    );
    assert!(warning_codes(&cfg)
        .contains(&warnings::REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES.to_owned()));
}

/// `enabled = false` is the same state as an absent block, and is the easier
/// one to overlook because the block is *there*.
#[test]
fn license_gate_with_sbom_explicitly_disabled_warns_too() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"

        [registries.sbom]
        enabled = false

        [[registries.rules]]
        kind = "license_gate"
        deny = ["AGPL-3.0"]"#,
    );
    assert!(warning_codes(&cfg).contains(&warnings::LICENSE_GATE_SBOM_DISABLED.to_owned()));
}

/// SBOM off *and* block + allow_unknown = false is the dangerous combination:
/// nothing is ever extracted, so every download is refused. The message has to
/// say that rather than the generic "never denies".
#[test]
fn sbom_disabled_with_strict_unknown_says_it_refuses_everything() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"

        [[registries.rules]]
        kind = "license_gate"
        block = true
        allow_unknown = false"#,
    );
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::LICENSE_GATE_SBOM_DISABLED)
        .expect("warning emitted");
    assert!(w.message.contains("refuse every download"), "{}", w.message);
}

/// One warning per rule, not two: SBOM being off already makes the licence
/// unknown, so also reporting the missing parser would be the same fact twice
/// and would bury whichever one the operator can act on.
#[test]
fn sbom_disabled_on_an_unsupported_type_reports_only_the_sbom_problem() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "goproxy"
        name = "reg"

        [[registries.rules]]
        kind = "license_gate"
        deny = ["AGPL-3.0"]"#,
    );
    let codes = warning_codes(&cfg);
    assert!(codes.contains(&warnings::LICENSE_GATE_SBOM_DISABLED.to_owned()));
    assert!(!codes.contains(&warnings::LICENSE_GATE_NO_EXTRACTOR.to_owned()));
}

// ── README capture (RFC 0007 §4.5) ────────────────────────────────────────────

fn config_with_readme(registry_type: &str, extra: &str, block: &str) -> AppConfig {
    parse_config(&format!(
        r#"
        [[registries]]
        type = "{registry_type}"
        name = "reg"
        upstreams = ["https://example.invalid"]
{extra}
        [registries.readme]
{block}"#
    ))
}

/// An unrecognised `remote_images` must not silently become the default: the
/// two behaviours differ in what leaves the network, so an operator who wrote
/// `"allow"` expecting images believes the opposite of what they would get.
#[test]
fn an_unknown_remote_images_value_refuses_to_start() {
    let cfg = config_with_readme("npm", "", r#"        remote_images = "allow""#);
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("remote_images"), "{err}");
    assert!(err.contains("there is no \"allow\""), "{err}");
}

#[test]
fn the_two_documented_remote_images_values_are_accepted() {
    for value in ["strip", "proxy"] {
        let cfg = config_with_readme("npm", "", &format!("        remote_images = \"{value}\""));
        cfg.validate().expect("documented value accepted");
    }
}

/// Stores nothing while claiming to be on. A configuration that cannot do its
/// job should not start.
#[test]
fn max_bytes_zero_while_enabled_refuses_to_start() {
    let cfg = config_with_readme("npm", "", "        max_bytes = 0");
    assert!(cfg
        .validate()
        .unwrap_err()
        .to_string()
        .contains("max_bytes"));
}

/// With the feature off, a zero cap says nothing and is not worth refusing over.
#[test]
fn max_bytes_zero_with_the_feature_off_is_not_an_error() {
    let cfg = config_with_readme("npm", "", "        enabled = false\n        max_bytes = 0");
    cfg.validate()
        .expect("disabled registry stores nothing anyway");
}

#[test]
fn max_bytes_above_the_ceiling_refuses_to_start() {
    let cfg = config_with_readme("npm", "", "        max_bytes = 8388608");
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("ceiling"), "{err}");
}

/// The block written down on a type that has no README is accepted and inert,
/// and says so by name — with the reason `readme_support()` carries, so the
/// warning cannot drift from the behaviour.
#[test]
fn a_readme_block_on_a_type_with_none_warns_that_it_is_inert() {
    let cfg = config_with_readme("maven", "", "        enabled = true");
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::README_UNSUPPORTED_TYPE)
        .expect("warning emitted");
    assert!(w.message.contains("maven"), "{}", w.message);
    assert!(w.message.contains("sentence"), "{}", w.message);
    assert_eq!(w.path, "registries[0].readme");
}

/// The feature is on by default, so warning about every absent block would put
/// a notice on the admin panel for every `maven` registry in every deployment.
/// The operator expressed no belief there; there is nothing to correct.
#[test]
fn an_absent_readme_block_never_warns_even_on_an_unsupported_type() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "maven"
        name = "reg""#,
    );
    assert!(!warning_codes(&cfg).iter().any(|c| c.starts_with("readme")));
}

/// `enabled = false` is a decision, not a mistake — nothing to warn about.
#[test]
fn a_disabled_readme_block_warns_about_nothing() {
    let cfg = config_with_readme("maven", "", "        enabled = false");
    assert!(!warning_codes(&cfg).iter().any(|c| c.starts_with("readme")));
}

/// `from_archive` on a metadata-borne-only kind is accepted and does nothing.
/// Distinct from the unsupported-type warning: READMEs *are* stored here.
#[test]
fn from_archive_on_a_metadata_borne_type_warns_that_it_is_inert() {
    let cfg = config_with_readme("jetbrains-marketplace", "", "        from_archive = true");
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::README_FROM_ARCHIVE_INERT)
        .expect("warning emitted");
    assert!(w.message.contains("stored either way"), "{}", w.message);
}

/// npm reads both, so nothing about `from_archive` is inert there.
#[test]
fn from_archive_on_a_type_that_reads_the_archive_is_quiet() {
    let cfg = config_with_readme("npm", "", "        from_archive = true");
    assert!(!warning_codes(&cfg).iter().any(|c| c.starts_with("readme")));
}

/// `firewall_only` streams without buffering, so nothing is ever cached to
/// extract from. On an archive-only kind that means no README at all, and the
/// warning says which of the two it is.
#[test]
fn from_archive_on_a_firewall_only_registry_warns_that_nothing_is_cached() {
    let cfg = config_with_readme(
        "cargo",
        "        firewall_only = true\n",
        "        from_archive = true",
    );
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::README_FROM_ARCHIVE_FIREWALL_ONLY)
        .expect("warning emitted");
    assert!(
        w.message.contains("no README will ever be stored"),
        "{}",
        w.message
    );

    // npm still has its metadata-borne half, and the message says so rather
    // than telling the operator the feature is dead.
    let npm = config_with_readme(
        "npm",
        "        firewall_only = true\n",
        "        from_archive = true",
    );
    let w = npm
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::README_FROM_ARCHIVE_FIREWALL_ONLY)
        .expect("warning emitted");
    assert!(w.message.contains("still work"), "{}", w.message);
}

// ── The console's discovery read (RFC 0007 §4.5) ──────────────────────────────

fn config_with_upstream_detail(registry_type: &str, extra: &str, block: &str) -> AppConfig {
    parse_config(&format!(
        r#"
        [[registries]]
        type = "{registry_type}"
        name = "reg"
        upstreams = ["https://example.invalid"]
{extra}
        [registries.upstream_detail]
{block}"#
    ))
}

/// Attempts the fetch and discards every result: the egress happens and nothing
/// is shown.
#[test]
fn max_versions_zero_while_enabled_refuses_to_start() {
    let cfg = config_with_upstream_detail("npm", "", "        max_versions = 0");
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("max_versions"), "{err}");
    assert!(err.contains("nothing is shown"), "{err}");
}

#[test]
fn max_versions_zero_with_the_read_disabled_is_not_an_error() {
    let cfg = config_with_upstream_detail(
        "npm",
        "",
        "        enabled = false\n        max_versions = 0",
    );
    cfg.validate()
        .expect("a disabled read fetches nothing anyway");
}

#[test]
fn max_versions_above_the_ceiling_refuses_to_start() {
    let cfg = config_with_upstream_detail("npm", "", "        max_versions = 10000");
    assert!(cfg.validate().unwrap_err().to_string().contains("ceiling"));
}

/// There is no upstream to ask, and the page is already complete from local
/// rows.
#[test]
fn the_discovery_read_on_a_local_registry_warns_that_it_is_inert() {
    let cfg = config_with_upstream_detail(
        "npm",
        "        mode = \"local\"\n",
        "        enabled = true",
    );
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::UPSTREAM_DETAIL_LOCAL_MODE)
        .expect("warning emitted");
    assert!(w.message.contains("no upstream to ask"), "{}", w.message);
    assert_eq!(w.path, "registries[0].upstream_detail");
}

/// A path-addressed kind has no package identity to ask about, and the warning
/// quotes the reason `upstream_detail()` carries — so the code and the notice
/// cannot disagree.
#[test]
fn the_discovery_read_on_an_unaskable_kind_warns_with_its_reason() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "generic"
        name = "reg"
        upstreams = ["https://example.invalid"]
        path_allow = ["**"]

        [registries.upstream_detail]
        enabled = true"#,
    );
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::UPSTREAM_DETAIL_UNSUPPORTED_KIND)
        .expect("warning emitted");
    assert!(w.message.contains("path-addressed"), "{}", w.message);
}

/// The read is on by default, so warning about the implicit default would put a
/// notice on the admin panel for every `local`-mode registry in every
/// deployment. The operator expressed no belief there.
#[test]
fn an_absent_upstream_detail_block_never_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "reg"
        mode = "local""#,
    );
    assert!(!warning_codes(&cfg)
        .iter()
        .any(|c| c.starts_with("upstream-detail")));
}

#[test]
fn a_disabled_discovery_read_warns_about_nothing() {
    let cfg = config_with_upstream_detail(
        "npm",
        "        mode = \"local\"\n",
        "        enabled = false",
    );
    assert!(!warning_codes(&cfg)
        .iter()
        .any(|c| c.starts_with("upstream-detail")));
}

/// A proxy-mode registry of an askable kind is the case the feature was written
/// for, and it must stay quiet: a warning that fires on correct config teaches
/// operators to ignore the channel.
#[test]
fn a_correctly_configured_discovery_read_is_quiet() {
    let cfg = config_with_upstream_detail("npm", "", "        enabled = true");
    assert!(!warning_codes(&cfg)
        .iter()
        .any(|c| c.starts_with("upstream-detail")));
}

/// `remote_images = "proxy"` renders images now, so it is accepted in silence.
///
/// It used to raise `readme.image-proxy-unimplemented`, because the endpoint it
/// rewrote to did not exist. The endpoint exists (RFC 0007-bis §4.2), and a
/// warning that outlives its subject is worse than no warning — it teaches
/// operators the channel is noise.
#[test]
fn remote_images_proxy_is_accepted_without_a_warning() {
    for value in ["proxy", "strip"] {
        let cfg = config_with_readme("npm", "", &format!("        remote_images = \"{value}\""));
        cfg.validate().expect("accepted");
        assert!(
            !warning_codes(&cfg)
                .iter()
                .any(|c| c.contains("image-proxy")),
            "{value} warned about an unimplemented endpoint"
        );
    }
}

/// The image cap gets the same two guards `max_bytes` has, for the same
/// reasons: a zero serves nothing while claiming to render, and a ceiling makes
/// "held in memory while its type is checked" a bound rather than a hope
/// (RFC 0007-bis §4.5).
#[test]
fn the_image_cap_refuses_zero_under_proxy_and_refuses_an_unbounded_ceiling() {
    let zero = config_with_readme(
        "npm",
        "",
        "        remote_images = \"proxy\"\n        image_max_bytes = 0",
    );
    let err = zero.validate().expect_err("must be refused").to_string();
    assert!(err.contains("image_max_bytes"), "{err}");
    assert!(err.contains("serves no image"), "{err}");

    // Zero is fine under `strip`, where nothing is ever fetched.
    let stripped = config_with_readme(
        "npm",
        "",
        "        remote_images = \"strip\"\n        image_max_bytes = 0",
    );
    stripped.validate().expect("inert under strip");

    let huge = config_with_readme("npm", "", "        image_max_bytes = 33554432");
    let err = huge.validate().expect_err("must be refused").to_string();
    assert!(err.contains("ceiling"), "{err}");
}

// ── Prose search ──────────────────────────────────────────────────────────────

/// Off by default, and it stays off unless an operator writes it down. Unlike
/// README *capture*, which defaults on because it costs one already-parsed
/// field, this builds an index over prose (RFC 0007-bis §4.1).
#[test]
fn prose_search_is_off_unless_asked_for() {
    let cfg = parse_config("");
    assert!(!cfg.search.readmes);
    assert_eq!(cfg.search.text_config, "english");
    cfg.validate().expect("the default config is valid");
}

/// `english`, against the recommendation this RFC was drafted with. The draft
/// said stemming mangles identifiers; it does, symmetrically, and `simple` fails
/// `retry` against a README that says `retrying` (RFC 0007-bis §13.3).
#[test]
fn the_text_configuration_defaults_to_english_and_is_settable() {
    let cfg = parse_config(
        r#"
        [search]
        readmes = true
        text_config = "french""#,
    );
    cfg.validate().expect("accepted");
    assert!(cfg.search.readmes);
    assert_eq!(cfg.search.text_config, "french");
}

#[test]
fn an_empty_text_configuration_is_refused() {
    let cfg = parse_config(
        r#"
        [search]
        text_config = """#,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("text_config"), "{err}");
}

/// The index is a Postgres generated column with a GIN index. There is nowhere
/// else to put it, and failing at startup beats a search that quietly matches
/// nothing (RFC 0007-bis §4.5).
#[test]
fn prose_search_without_postgres_refuses_to_start() {
    let raw = r#"
        [database]
        type = "sqlite"
        url = "sqlite://test.db"

        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"

        [server]
        host = "127.0.0.1"
        port = 8080

        [search]
        readmes = true
        "#;
    let cfg: AppConfig = toml::from_str(raw).expect("parses");
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("Postgres"), "{err}");

    // Both spellings of the one backend that does work are accepted.
    for spelling in ["postgres", "postgresql", "PostgreSQL"] {
        let raw = raw.replace(r#"type = "sqlite""#, &format!(r#"type = "{spelling}""#));
        let cfg: AppConfig = toml::from_str(&raw).expect("parses");
        cfg.validate().unwrap_or_else(|e| panic!("{spelling}: {e}"));
    }
}

/// Accepted, and said out loud: the index will exist and stay empty, because
/// nothing is ever stored to put in it.
#[test]
fn prose_search_over_registries_that_store_nothing_is_warned_about() {
    let cfg = parse_config(
        r#"
        [search]
        readmes = true

        [[registries]]
        type = "npm"
        name = "reg"
        upstreams = ["https://example.invalid"]
        [registries.readme]
        enabled = false"#,
    );
    cfg.validate().expect("accepted");
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::SEARCH_READMES_NOTHING_STORED)
        .expect("warning emitted");
    assert!(w.message.contains("stay empty"), "{}", w.message);

    // One registry that does capture is enough to make the setting useful, so
    // nothing is said.
    let quiet = parse_config(
        r#"
        [search]
        readmes = true

        [[registries]]
        type = "npm"
        name = "reg"
        upstreams = ["https://example.invalid"]
        [registries.readme]
        enabled = false

        [[registries]]
        type = "cargo"
        name = "reg2"
        upstreams = ["https://example.invalid"]"#,
    );
    assert!(!warning_codes(&quiet)
        .iter()
        .any(|c| c == warnings::SEARCH_READMES_NOTHING_STORED));
}

// ── `[limits].versions_per_page` ──────────────────────────────────────────────

/// The key answers one question — how much of a version list this server is
/// willing to build for one request — so it is both the default for a caller
/// that asks for nothing and the ceiling on what one may ask for. Absent means
/// the same number either way, which is what this pins: a `[limits]` block that
/// mentions something else must not read as zero.
#[test]
fn versions_per_page_defaults_to_a_hundred_with_or_without_a_limits_block() {
    let none = parse_config("");
    assert_eq!(none.limits.versions_per_page, DEFAULT_VERSIONS_PER_PAGE);

    let other_key = parse_config(
        r#"
        [limits]
        max_artifact_size_bytes = 1024"#,
    );
    assert_eq!(
        other_key.limits.versions_per_page,
        DEFAULT_VERSIONS_PER_PAGE
    );
}

/// Every caller would get an empty version list, and the failure would land on a
/// package page rather than at startup.
#[test]
fn versions_per_page_zero_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [limits]
        versions_per_page = 0"#,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("versions_per_page"), "{err}");
    assert!(err.contains("empty version list"), "{err}");
}

/// Every row costs a vulnerability read and a licence read before it is
/// serialised — the same argument `upstream_detail.max_versions` makes one level
/// up.
#[test]
fn versions_per_page_above_the_ceiling_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [limits]
        versions_per_page = 5000"#,
    );
    assert!(cfg.validate().unwrap_err().to_string().contains("ceiling"));
}

#[test]
fn versions_per_page_within_the_ceiling_loads() {
    let cfg = parse_config(
        r#"
        [limits]
        versions_per_page = 250"#,
    );
    cfg.validate().expect("250 is under the ceiling");
    assert_eq!(cfg.limits.versions_per_page, 250);
}

// ── `[limits].packages_per_page` ──────────────────────────────────────────────

/// The catalog's half of the same idea, with its own number because a screenful
/// of names and a query's worth of enriched version rows are not the same
/// question.
#[test]
fn packages_per_page_defaults_to_twenty() {
    assert_eq!(
        parse_config("").limits.packages_per_page,
        DEFAULT_PACKAGES_PER_PAGE
    );
    let other_key = parse_config(
        r#"
        [limits]
        versions_per_page = 50"#,
    );
    assert_eq!(
        other_key.limits.packages_per_page,
        DEFAULT_PACKAGES_PER_PAGE
    );
}

#[test]
fn packages_per_page_zero_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [limits]
        packages_per_page = 0"#,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("packages_per_page"), "{err}");
    assert!(err.contains("empty package catalog"), "{err}");
}

#[test]
fn packages_per_page_above_the_ceiling_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [limits]
        packages_per_page = 5000"#,
    );
    assert!(cfg.validate().unwrap_err().to_string().contains("ceiling"));
}

/// The two keys are independent: setting one must not move the other.
#[test]
fn the_two_page_sizes_do_not_move_each_other() {
    let cfg = parse_config(
        r#"
        [limits]
        packages_per_page = 40"#,
    );
    cfg.validate().expect("40 is a fine catalog page");
    assert_eq!(cfg.limits.packages_per_page, 40);
    assert_eq!(cfg.limits.versions_per_page, DEFAULT_VERSIONS_PER_PAGE);
}

// ── Auth: transport and identity ──────────────────────────────────────────────

/// The Kubernetes API server is held to the same rule as an OIDC issuer, and for
/// a stronger reason: the TokenReview carries this server's own service account
/// token, and whoever answers it decides the caller's role.
#[test]
fn a_plain_http_kubernetes_api_server_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [[auth]]
        type = "kubernetes"
        name = "k8s"
        api_server = "http://kube.internal:6443""#,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("api_server"), "{err}");
    assert!(err.contains("must use https"), "{err}");
    assert!(
        err.contains("service account token"),
        "the message should say what is at stake; {err}"
    );
}

/// The same userinfo trick `is_secure_issuer_url` refuses for an issuer: the
/// authority ends at the last `@`, so this dials `evil.example` in cleartext.
#[test]
fn a_kubernetes_api_server_hiding_behind_userinfo_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [[auth]]
        type = "kubernetes"
        name = "k8s"
        api_server = "http://localhost:6443@evil.example""#,
    );
    assert!(cfg
        .validate()
        .unwrap_err()
        .to_string()
        .contains("must use https"));
}

/// Loopback over plain HTTP stays allowed — that is how the test suites and a
/// developer's local cluster run — and an absent `api_server` has nothing to
/// check: it means the in-cluster default, which is https by construction.
#[test]
fn a_loopback_or_absent_kubernetes_api_server_starts() {
    parse_config(
        r#"
        [[auth]]
        type = "kubernetes"
        name = "k8s"
        api_server = "http://127.0.0.1:6443""#,
    )
    .validate()
    .expect("loopback is exempt");

    parse_config(
        r#"
        [[auth]]
        type = "kubernetes"
        name = "k8s""#,
    )
    .validate()
    .expect("an absent api_server means the in-cluster https default");
}

/// A blank `audience` is a configuration error, not an unreachable provider.
///
/// `ActionsOidcAuthProvider::new` refuses it, but that error is routed through
/// `provider_unavailable(…, cfg.required, …)` and `required` defaults to `false`
/// for this kind — so the server used to log one warning and come up *without*
/// the provider, silently downgrading every CI caller to anonymous.
#[test]
fn a_blank_actions_oidc_audience_is_refused_at_load() {
    for audience in ["\"\"", "\"   \""] {
        let cfg = parse_config(&format!(
            r#"
        [[auth]]
        type = "actions-oidc"
        name = "gha"
        issuer_url = "https://token.actions.githubusercontent.com"
        audience = {audience}"#
        ));
        let err = cfg
            .validate()
            .expect_err("a blank audience must not start")
            .to_string();
        assert!(err.contains("audience"), "{err}");
        assert!(err.contains("gha"), "{err}");
    }
}

/// And a real one still starts — the check is about blankness, not about the
/// provider kind being unwelcome.
#[test]
fn an_actions_oidc_provider_with_an_audience_starts() {
    parse_config(
        r#"
        [[auth]]
        type = "actions-oidc"
        name = "gha"
        issuer_url = "https://token.actions.githubusercontent.com"
        audience = "https://batlehub.example.com""#,
    )
    .validate()
    .expect("a named audience is the supported configuration");
}

/// Two providers of different kinds sharing one name are one provider as far as
/// `oidc_session_owner` and the group prefix are concerned — which is how a
/// service account comes to mint personal access tokens as an OIDC user.
#[test]
fn two_auth_providers_may_not_share_a_name() {
    let cfg = parse_config(
        r#"
        [[auth]]
        type = "oidc"
        name = "corp"
        issuer_url = "https://idp.example.com"
        client_id = "batlehub"

        [[auth]]
        type = "kubernetes"
        name = "corp""#,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("\"corp\""), "{err}");
    assert!(err.contains("oidc"), "{err}");
    assert!(err.contains("kubernetes"), "{err}");
}

/// Including two of the same kind: the collision is about the name, not about
/// the kinds differing.
#[test]
fn two_oidc_providers_may_not_share_a_name_either() {
    let cfg = parse_config(
        r#"
        [[auth]]
        type = "oidc"
        name = "corp"
        issuer_url = "https://idp.example.com"
        client_id = "batlehub"

        [[auth]]
        type = "oidc"
        name = "corp"
        issuer_url = "https://other.example.com"
        client_id = "batlehub""#,
    );
    assert!(cfg.validate().is_err());
}

/// Distinct names are the normal case and must still start, static tokens
/// included — that provider has no name of its own to collide with.
#[test]
fn distinct_auth_names_start() {
    parse_config(
        r#"
        [[auth]]
        type = "token"

        [[auth]]
        type = "oidc"
        name = "corp"
        issuer_url = "https://idp.example.com"
        client_id = "batlehub"

        [[auth]]
        type = "kubernetes"
        name = "k8s-prod""#,
    )
    .validate()
    .expect("distinct names are fine");
}

// ── Signed download URLs (RFC 0012 §4.1) ──────────────────────────────────────

/// 32 ASCII bytes — the minimum a signing secret may be.
const GOOD_SECRET: &str = "0123456789abcdef0123456789abcdef";

fn signed_urls_config(extra_server: &str, registry_signed: bool) -> AppConfig {
    parse_config(&format!(
        r#"
        [[registries]]
        type = "terraform"
        name = "tf"
        signed_downloads = {registry_signed}

        [server.signed_urls]
{extra_server}
        "#
    ))
}

#[test]
fn signed_downloads_defaults_to_false() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "terraform"
        name = "tf""#,
    );
    assert!(!cfg.registries[0].signed_downloads);
    cfg.validate().expect("a registry with no signing is fine");
}

#[test]
fn a_valid_signed_urls_block_is_accepted() {
    let cfg = signed_urls_config(&format!(r#"        secret = "{GOOD_SECRET}""#), true);
    cfg.validate().expect("accepted");
    let block = cfg.server.signed_urls.as_ref().expect("present");
    assert_eq!(block.ttl_seconds, 300, "the documented default");
    assert!(block.previous_secrets.is_empty());
}

/// The failure this whole feature exists to prevent: a registry that believes
/// it is closed and is not.
#[test]
fn signed_downloads_without_a_secret_refuses_to_start() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "terraform"
        name = "tf"
        signed_downloads = true"#,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("signed_downloads"), "{err}");
    assert!(err.contains("tf"), "{err}");
}

#[test]
fn a_short_signing_secret_refuses_to_start() {
    let cfg = signed_urls_config(r#"        secret = "too-short""#, true);
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("32"), "{err}");
}

/// `${VAR}` that interpolates to nothing is the shape this catches: the file
/// looks configured and the instance has no key.
#[test]
fn an_empty_signing_secret_refuses_to_start() {
    let cfg = signed_urls_config(r#"        secret = """#, true);
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("empty"), "{err}");
}

#[test]
fn a_zero_ttl_refuses_to_start() {
    let cfg = signed_urls_config(
        &format!("        secret = \"{GOOD_SECRET}\"\n        ttl_seconds = 0"),
        true,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("born expired"), "{err}");
}

#[test]
fn a_ttl_over_the_ceiling_refuses_to_start() {
    let cfg = signed_urls_config(
        &format!("        secret = \"{GOOD_SECRET}\"\n        ttl_seconds = 2592000"),
        true,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("3600"), "{err}");
}

#[test]
fn a_short_previous_secret_refuses_to_start() {
    let cfg = signed_urls_config(
        &format!("        secret = \"{GOOD_SECRET}\"\n        previous_secrets = [\"nope\"]"),
        true,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("previous_secrets[0]"), "{err}");
}

/// The index has to name the entry in the operator's file, not its position in
/// the filtered list. An unset `${VAR}` ahead of the short one used to shift
/// every later index down by one, so the error pointed at the entry that was
/// fine — during a startup failure, which is the worst moment to be misdirected.
#[test]
fn the_reported_index_is_the_one_in_the_config_file() {
    let cfg = signed_urls_config(
        &format!("        secret = \"{GOOD_SECRET}\"\n        previous_secrets = [\"\", \"nope\"]"),
        true,
    );
    let err = cfg.validate().unwrap_err().to_string();
    assert!(
        err.contains("previous_secrets[1]"),
        "the short entry is index 1 in the file: {err}"
    );
}

/// `previous_secrets = ["${VAR_OLD}"]` with no old secret set is the normal
/// steady state between rotations; failing on it would make rotation a two-step
/// config edit.
#[test]
fn an_empty_previous_secret_is_ignored() {
    let cfg = signed_urls_config(
        &format!("        secret = \"{GOOD_SECRET}\"\n        previous_secrets = [\"\", \"  \"]"),
        true,
    );
    cfg.validate().expect("accepted");
    assert!(cfg
        .server
        .signed_urls
        .as_ref()
        .unwrap()
        .active_previous_secrets()
        .is_empty());
}

#[test]
fn an_unknown_key_in_the_signed_urls_block_is_refused() {
    let raw = format!(
        r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"

        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"

        [server]
        host = "127.0.0.1"
        port = 8080

        [server.signed_urls]
        secret = "{GOOD_SECRET}"
        ttl_second = 300
        "#
    );
    // A typo in a security key must fail the load, not silently take a default.
    assert!(toml::from_str::<AppConfig>(&raw).is_err());
}

// ── Signed-URL warnings (RFC 0012 §7) ─────────────────────────────────────────

#[test]
fn signing_alongside_an_anonymous_grant_warns() {
    let cfg = parse_config(&format!(
        r#"
        [[registries]]
        type = "terraform"
        name = "tf"
        signed_downloads = true

        [registries.rbac]
        anonymous = ["releases:read"]

        [server.signed_urls]
        secret = "{GOOD_SECRET}""#
    ));
    cfg.validate().expect("legal, just probably unintended");
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        codes
            .iter()
            .any(|c| c == warnings::SIGNED_URLS_ANONYMOUS_STILL_GRANTED),
        "{codes:?}"
    );
}

#[test]
fn signing_with_an_empty_anonymous_grant_does_not_warn() {
    let cfg = signed_urls_config(&format!(r#"        secret = "{GOOD_SECRET}""#), true);
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        !codes
            .iter()
            .any(|c| c == warnings::SIGNED_URLS_ANONYMOUS_STILL_GRANTED),
        "{codes:?}"
    );
}

#[test]
fn a_signing_secret_no_registry_uses_warns() {
    let cfg = signed_urls_config(&format!(r#"        secret = "{GOOD_SECRET}""#), false);
    cfg.validate().expect("legal");
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        codes.iter().any(|c| c == warnings::SIGNED_URLS_UNUSED),
        "{codes:?}"
    );
}

// ── Local retention (RFC 0016 §4.6) ───────────────────────────────────────────

/// A `[registries.retention]` block on the given registry `mode`, with whatever
/// keys the caller wants inside it.
fn retention_config(mode: &str, block: &str) -> AppConfig {
    parse_config(&format!(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "{mode}"
        upstreams = ["https://registry.npmjs.org"]

        [registries.retention]
{block}"#
    ))
}

#[test]
fn retention_is_absent_by_default_and_valid() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local""#,
    );
    cfg.validate().expect("no block is the default");
    assert!(cfg.registries[0].retention.is_none());
}

#[test]
fn retention_dry_run_defaults_to_on() {
    let cfg = retention_config("local", "        tombstone_detail_for_days = 730");
    cfg.validate().expect("valid");
    let ret = cfg.registries[0].retention.as_ref().unwrap();
    assert_eq!(ret.tombstone_detail_for_days, Some(730));
    assert!(
        ret.dry_run,
        "a configured window must do nothing until an operator turns dry_run off"
    );
}

/// The derived `Default` would say `dry_run: false`, which is the destructive
/// direction and the opposite of the `serde` default. The two must agree.
#[test]
fn retention_struct_default_matches_the_serde_default() {
    let from_toml: RetentionConfig = toml::from_str("").expect("empty table parses");
    let from_default = RetentionConfig::default();
    assert_eq!(from_default.dry_run, from_toml.dry_run);
    assert!(from_default.dry_run);
    assert_eq!(
        from_default.tombstone_detail_for_days,
        from_toml.tombstone_detail_for_days
    );
}

#[test]
fn retention_on_a_proxy_registry_is_rejected() {
    let err = retention_config("proxy", "        tombstone_detail_for_days = 730")
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("locally published versions"), "{err}");
    assert!(
        err.contains("[registries.cache]"),
        "the error must point at the block the operator actually meant: {err}"
    );
}

#[test]
fn an_empty_retention_block_is_rejected() {
    let err = retention_config("local", "        # nothing at all")
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("no setting that does anything"), "{err}");
}

#[test]
fn a_zero_or_short_detail_window_is_rejected() {
    let zero = retention_config("local", "        tombstone_detail_for_days = 0")
        .validate()
        .unwrap_err()
        .to_string();
    assert!(zero.contains("never be investigated"), "{zero}");

    let short = retention_config("local", "        tombstone_detail_for_days = 7")
        .validate()
        .unwrap_err()
        .to_string();
    assert!(short.contains("30-day floor"), "{short}");
}

/// The reclamation keys are phase 3 and depend on RFC 0015's `policy` table.
/// They must fail to load rather than parse and sit inert — an operator who
/// wrote one believes versions are being reclaimed.
#[test]
fn a_phase_three_retention_key_is_refused_rather_than_ignored() {
    // Not through `parse_config`, which `expect`s the parse: the failure is the
    // assertion here, not the setup.
    let raw = r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"

        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"

        [server]
        host = "127.0.0.1"
        port = 8080

        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [registries.retention]
        keep_if_pulled = "90d""#;
    let err = toml::from_str::<AppConfig>(raw)
        .expect_err("an unimplemented key must not load silently")
        .to_string();
    assert!(err.contains("keep_if_pulled"), "{err}");
}

#[test]
fn live_compaction_warns_on_every_reload() {
    let cfg = retention_config(
        "local",
        "        tombstone_detail_for_days = 730\n        dry_run = false",
    );
    cfg.validate().expect("legal — it is the only way it works");
    let warning = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::RETENTION_COMPACTION_LIVE)
        .expect("an armed compaction must be said out loud");
    assert!(
        warning.message.contains("coordinate claim is kept"),
        "the warning must say what survives, not only what is lost: {}",
        warning.message
    );
    assert_eq!(warning.path, "registries[0].retention");
}

#[test]
fn a_dry_run_compaction_does_not_warn() {
    let cfg = retention_config("local", "        tombstone_detail_for_days = 730");
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        !codes
            .iter()
            .any(|c| c == warnings::RETENTION_COMPACTION_LIVE),
        "{codes:?}"
    );
}

// ── Retention keep conditions (RFC 0016 §4.2, §4.6) ───────────────────────────

#[test]
fn retention_keep_conditions_parse_with_safe_defaults() {
    let cfg = retention_config(
        "local",
        "        keep_versions = 10\n        keep_if_pulled_days = 90",
    );
    cfg.validate().expect("valid");
    let ret = cfg.registries[0].retention.as_ref().unwrap();
    assert_eq!(ret.keep_versions, Some(10));
    assert_eq!(ret.keep_if_pulled_days, Some(90));
    assert!(
        ret.dry_run,
        "a configured policy reclaims nothing until asked"
    );
    assert!(
        ret.keep_yanked,
        "a yank is not a reason to destroy the only copy"
    );
    assert_eq!(ret.reclaim_delay_ms, 0);
    assert!(ret.download_signal_floor_days.is_none());
}

/// The block that would reclaim *everything* on its first live run: no keep
/// condition, so the union of vetoes is empty and nothing vetoes.
#[test]
fn a_retention_block_with_no_keep_condition_is_rejected() {
    let err = retention_config("local", "        keep_yanked = true")
        .validate()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("reclaim every version on its first live run"),
        "the error must say what the empty block would do: {err}"
    );
}

/// `keep_yanked` alone must not make an otherwise-empty block look configured —
/// it defaults to true and only ever vetoes, so a block containing nothing else
/// still destroys every unyanked version.
#[test]
fn keep_yanked_alone_does_not_count_as_a_keep_condition() {
    let cfg = retention_config("local", "        keep_yanked = false");
    assert!(cfg.validate().is_err());
    assert!(!cfg.registries[0]
        .retention
        .as_ref()
        .unwrap()
        .reclaims_anything());
}

#[test]
fn a_zero_keep_condition_is_rejected() {
    for key in ["keep_versions", "keep_for_days", "keep_if_pulled_days"] {
        let err = retention_config("local", &format!("        {key} = 0"))
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("keeps nothing"), "{key}: {err}");
    }
}

/// **The mistake this feature exists to make hard** — reclaiming without
/// consulting the download signal.
#[test]
fn live_reclamation_without_a_pull_veto_warns_loudly() {
    let cfg = retention_config(
        "local",
        "        keep_versions = 10\n        dry_run = false",
    );
    cfg.validate()
        .expect("legal, and exactly the dangerous shape");
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        codes.iter().any(|c| c == warnings::RETENTION_NO_PULL_VETO),
        "{codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|c| c == warnings::RETENTION_RECLAMATION_LIVE),
        "{codes:?}"
    );

    let warning = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::RETENTION_NO_PULL_VETO)
        .unwrap();
    assert!(
        warning.message.contains("pinned to"),
        "the warning must describe the consequence, not the setting: {}",
        warning.message
    );
}

#[test]
fn live_reclamation_with_a_pull_veto_warns_once_not_twice() {
    let cfg = retention_config(
        "local",
        "        keep_versions = 10\n        keep_if_pulled_days = 90\n        dry_run = false",
    );
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        !codes.iter().any(|c| c == warnings::RETENTION_NO_PULL_VETO),
        "a policy that consults the signal must not be warned about it: {codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|c| c == warnings::RETENTION_RECLAMATION_LIVE),
        "it is still destroying the only copy: {codes:?}"
    );
}

#[test]
fn a_dry_run_policy_does_not_warn_about_reclamation() {
    let cfg = retention_config("local", "        keep_versions = 10");
    let codes: Vec<String> = cfg.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        !codes
            .iter()
            .any(|c| c == warnings::RETENTION_RECLAMATION_LIVE
                || c == warnings::RETENTION_NO_PULL_VETO),
        "{codes:?}"
    );
}

// ── RFC 0015 §4.9 — tiered-policy warnings ───────────────────────────────────
//
// Every case here is a **legal config that does nothing**, which is the whole
// category §4.9 reserves warnings for. Each test asserts the config still
// validates before asserting the warning, because a warning that fires on a
// config the loader would have rejected anyway is not doing any work.

fn codes(cfg: &AppConfig) -> Vec<String> {
    cfg.warnings().into_iter().map(|w| w.code).collect()
}

/// `prerelease_visibility` on a registry that publishes nothing.
///
/// The one warning §4.9 argues for at length: it is a warning rather than a
/// rejection because `[registries.beta_channel]` carries no mode restriction
/// today and translates into this setting, so refusing it would stop an existing
/// instance from booting on upgrade — the one thing §10 forbids.
#[test]
fn prerelease_visibility_on_a_proxy_registry_warns_rather_than_failing() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "proxy"
        prerelease_visibility = "team""#,
    );
    cfg.validate()
        .expect("must not fail: an upgraded beta_channel config lands exactly here");
    let c = codes(&cfg);
    assert!(
        c.iter()
            .any(|x| x == warnings::PRERELEASE_VISIBILITY_PROXY_MODE),
        "{c:?}"
    );
}

/// Pre-releases visible to a wider audience than releases is legal and is
/// almost always a typo, since the setting exists to do the opposite.
#[test]
fn a_wider_prerelease_audience_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"
        visibility = "team"
        prerelease_visibility = "public""#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    assert!(
        c.iter().any(|x| x == warnings::PRERELEASE_VISIBILITY_WIDER),
        "{c:?}"
    );
}

/// …and the ordinary direction — pre-releases narrower than releases — is
/// silent. Without this the test above would pass on a warning that fires on
/// every configuration.
#[test]
fn a_narrower_prerelease_audience_is_silent() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"
        visibility = "public"
        prerelease_visibility = "team""#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    assert!(
        !c.iter().any(|x| x == warnings::PRERELEASE_VISIBILITY_WIDER),
        "the intended direction must not warn: {c:?}"
    );
}

/// Grants decided *who*; nothing decided *how wide*. §4.5's two directions, met
/// one at a time.
#[test]
fn a_namespace_with_grants_and_no_visibility_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [[registries.namespaces]]
        match = "@acme/billing"

        [registries.namespaces.grants]
        "group:*:platform" = ["releases:read"]"#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    assert!(
        c.iter()
            .any(|x| x == warnings::NAMESPACE_GRANTS_WITHOUT_VISIBILITY),
        "{c:?}"
    );
}

/// The same namespace, with visibility set, is silent — so the warning is about
/// the missing half rather than about having grants at all.
#[test]
fn a_namespace_with_grants_and_visibility_is_silent() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [[registries.namespaces]]
        match = "@acme/billing"
        visibility = "team"

        [registries.namespaces.grants]
        "group:*:platform" = ["releases:read"]"#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    assert!(
        !c.iter()
            .any(|x| x == warnings::NAMESPACE_GRANTS_WITHOUT_VISIBILITY),
        "{c:?}"
    );
}

/// `immutable = "always"` makes `releases:overwrite` inert. Not a
/// contradiction — a replace needs the verb *and* a mutable resource — but the
/// operator who wrote both believes one of them is doing something.
#[test]
fn immutable_always_beside_an_overwrite_grant_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [registries.grants]
        "role:user" = ["releases:overwrite"]

        [registries.versioning]
        immutable = "always""#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    assert!(
        c.iter()
            .any(|x| x == warnings::IMMUTABLE_ALWAYS_WITH_OVERWRITE_GRANT),
        "{c:?}"
    );
}

/// `released` on a node that publishes no pre-releases can never take its second
/// branch: it is `always`, written in two settings.
#[test]
fn immutable_released_without_prereleases_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [registries.versioning]
        immutable = "released"
        allow_prerelease = false"#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    assert!(
        c.iter()
            .any(|x| x == warnings::IMMUTABLE_RELEASED_WITHOUT_PRERELEASES),
        "{c:?}"
    );
}

/// An ordinary tiered-policy config warns about nothing, which is what makes
/// every assertion above meaningful.
#[test]
fn an_ordinary_namespace_policy_is_silent() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"
        visibility = "public"

        [[registries.namespaces]]
        match = "@acme/billing"
        visibility = "team"
        prerelease_visibility = "team"

        [registries.namespaces.grants]
        "group:*:platform" = ["releases:read", "releases:publish"]

        [registries.namespaces.versioning]
        enforce_semver = true
        immutable = "released"
        monotonic = true"#,
    );
    cfg.validate().expect("legal");
    let c = codes(&cfg);
    for code in [
        warnings::PRERELEASE_VISIBILITY_PROXY_MODE,
        warnings::PRERELEASE_VISIBILITY_WIDER,
        warnings::NAMESPACE_GRANTS_WITHOUT_VISIBILITY,
        warnings::IMMUTABLE_ALWAYS_WITH_OVERWRITE_GRANT,
        warnings::IMMUTABLE_RELEASED_WITHOUT_PRERELEASES,
    ] {
        assert!(!c.iter().any(|x| x == code), "{code} fired: {c:?}");
    }
}

// ── RFC 0015 §4.7 — shadow mode ──────────────────────────────────────────────

/// The warning §4.7 asks for on **every** reload, not once at the edit.
///
/// This is the most dangerous setting in RFC 0015: a request that would be
/// refused is served. The warning names the expiry because the countdown is the
/// point — a shadow that cannot be forgotten is what the required `until` buys.
#[test]
fn a_registry_in_shadow_warns_on_every_reload() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [registries.grants_shadow]
        until = "2099-12-01""#,
    );
    cfg.validate().expect("legal — that is the whole problem");
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::GRANTS_IN_SHADOW)
        .expect("must warn");
    assert!(
        w.message.contains("2099-12-01"),
        "the expiry is the point: {}",
        w.message
    );
    assert!(
        w.message.contains("bypass"),
        "and the message must name the consequence, not the setting: {}",
        w.message
    );
}

/// A namespace shadow warns too, and names the namespace.
#[test]
fn a_namespace_in_shadow_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [[registries.namespaces]]
        match = "@acme"

        [registries.namespaces.grants_shadow]
        until = "2099-12-01""#,
    );
    cfg.validate().expect("legal");
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::GRANTS_IN_SHADOW)
        .expect("must warn");
    assert!(w.message.contains("@acme"), "{}", w.message);
}

/// A config with no shadow is silent, which is what makes the two above
/// meaningful.
#[test]
fn no_shadow_is_silent() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local""#,
    );
    cfg.validate().expect("legal");
    assert!(!cfg
        .warnings()
        .iter()
        .any(|w| w.code == warnings::GRANTS_IN_SHADOW));
}

/// `until` is **required by the type**, so a shadow with no expiry cannot be
/// written at all.
///
/// §4.7 asks config load to reject the flag without a companion date. A
/// rejection the type performs is stronger than one a validator remembers to,
/// and this is the assertion that it is the type doing it.
#[test]
fn a_shadow_without_an_expiry_does_not_parse() {
    let err = toml::from_str::<AppConfig>(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [registries.grants_shadow]"#,
    )
    .expect_err("a shadow with no expiry must not parse");
    assert!(
        err.to_string().contains("until"),
        "the error must name the missing field: {err}"
    );
}

/// `versioning.dry_run` warns, more quietly.
///
/// §4.7's table calls this direction **mixed** rather than fail-open: bad data
/// lands, nothing leaks. It still warns, because the operator who sets it during
/// an import is the operator who forgets to unset it afterwards.
#[test]
fn versioning_in_dry_run_warns() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [registries.versioning]
        enforce_semver = true
        dry_run = true"#,
    );
    cfg.validate().expect("legal");
    assert!(cfg
        .warnings()
        .iter()
        .any(|w| w.code == warnings::VERSIONING_IN_DRY_RUN));
}

/// …and `dry_run` defaults to `false` on `versioning`, as §4.7 requires.
///
/// Only `retention.dry_run` defaults to `true`, and RFC 0016 argues that from
/// the fact that it is the only one of the three whose dry-run direction is
/// unambiguously safe.
#[test]
fn versioning_dry_run_defaults_to_false() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        mode = "local"

        [registries.versioning]
        enforce_semver = true"#,
    );
    assert!(!cfg.registries[0].versioning.as_ref().unwrap().dry_run);
    assert!(!cfg
        .warnings()
        .iter()
        .any(|w| w.code == warnings::VERSIONING_IN_DRY_RUN));
}

// ── [cache_coherence] ─────────────────────────────────────────────────────────

/// Absent means no sweep. The orphan collector deletes data on a timer with
/// nobody watching, so it is opt-in like every other destructive policy here.
#[test]
fn cache_coherence_is_absent_by_default() {
    let cfg = parse_config("");
    assert!(cfg.cache_coherence.is_none());
}

/// Present but `enabled = false` is still no sweep — an operator who turned it
/// off must not have it run because they left the interval behind.
#[test]
fn cache_coherence_disabled_keeps_its_interval_inert() {
    let cfg = parse_config(
        r#"
        [cache_coherence]
        enabled       = false
        interval_secs = 60"#,
    );
    let coh = cfg.cache_coherence.as_ref().unwrap();
    assert!(!coh.enabled);
    assert_eq!(coh.interval_secs, 60);
    assert!(
        !cfg.warnings()
            .iter()
            .any(|w| w.code == warnings::COHERENCE_INTERVAL_TOO_SHORT),
        "a sweep that never runs has no grace window to narrow"
    );
}

#[test]
fn cache_coherence_interval_defaults_to_daily() {
    let cfg = parse_config(
        r#"
        [cache_coherence]
        enabled = true"#,
    );
    let coh = cfg.cache_coherence.as_ref().unwrap();
    assert!(coh.enabled);
    assert_eq!(coh.interval_secs, 86_400);
    cfg.validate().expect("legal");
    assert!(!cfg
        .warnings()
        .iter()
        .any(|w| w.code == warnings::COHERENCE_INTERVAL_TOO_SHORT));
}

/// The interval is the grace window a cache write in flight gets. Legal, and
/// worth saying out loud.
#[test]
fn a_short_coherence_interval_warns() {
    let cfg = parse_config(
        r#"
        [cache_coherence]
        enabled       = true
        interval_secs = 30"#,
    );
    cfg.validate().expect("legal, not an error");
    let w = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::COHERENCE_INTERVAL_TOO_SHORT)
        .expect("a 30s sweep must warn");
    assert_eq!(w.path, "cache_coherence.interval_secs");
    assert!(w.message.contains("30s"), "{}", w.message);
}

// ── RFC 0010 §4.5: the age gate on a toolchain kind must state the field ─────

/// The validation error for `extra`, or a failure naming what was accepted.
fn validation_error(extra: &str, what: &str) -> String {
    match parse_config(extra).validate() {
        Ok(()) => panic!("{what}"),
        Err(e) => e.to_string(),
    }
}

/// On `nodedist` the field *is* the gate for every release `index.tab` no
/// longer lists, so inheriting npm's default silently is refused.
#[test]
fn an_age_gate_on_nodedist_must_state_deny_missing_timestamp() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "nodedist"
        name = "node"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 86400
        "#,
        "an age gate on nodedist without deny_missing_timestamp must not load",
    );
    assert!(err.contains("deny_missing_timestamp"), "{err}");
    assert!(err.contains("nodedist"), "the error names the kind: {err}");
    assert!(
        err.contains("'node'"),
        "the error names the registry: {err}"
    );
}

/// Either value is a legitimate posture; what is refused is not choosing.
#[test]
fn an_age_gate_on_nodedist_loads_with_either_value() {
    for value in ["true", "false"] {
        let cfg = parse_config(&format!(
            r#"
        [[registries]]
        type = "nodedist"
        name = "node"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 86400
        deny_missing_timestamp = {value}
        "#
        ));
        cfg.validate()
            .unwrap_or_else(|e| panic!("deny_missing_timestamp = {value} must load: {e}"));
    }
}

/// A namespace that re-tunes the gate re-inherits the same silent default, so
/// it is held to the same rule.
#[test]
fn a_namespace_age_gate_override_on_nodedist_must_state_the_field_too() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "nodedist"
        name = "node"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 86400
        deny_missing_timestamp = false

        [[registries.namespaces]]
        match = "node"

        [[registries.namespaces.rules]]
        kind = "release_age_gate"
        min_age_secs = 0
        "#,
        "a namespace override without deny_missing_timestamp must not load",
    );
    assert!(err.contains("deny_missing_timestamp"), "{err}");
}

/// Everywhere else the field stays optional and the default is unchanged.
#[test]
fn an_age_gate_elsewhere_keeps_its_optional_default() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 3600
        "#,
    );
    cfg.validate().expect("npm needs no explicit value");
    let RuleConfig::ReleaseAgeGate(gate) = &cfg.registries[0].rules[0] else {
        panic!("expected the age gate");
    };
    assert_eq!(gate.deny_missing_timestamp, None);
    assert!(!gate.deny_missing_timestamp());
}

/// The kind itself: proxy-only, and its default upstream is the one the
/// client uses, so `upstreams` may be omitted.
#[test]
fn nodedist_is_proxy_only_and_needs_no_explicit_upstream() {
    parse_config(
        r#"
        [[registries]]
        type = "nodedist"
        name = "node"
        "#,
    )
    .validate()
    .expect("a bare nodedist registry loads");

    let err = validation_error(
        r#"
        [[registries]]
        type = "nodedist"
        name = "node"
        mode = "local"
        "#,
        "nodedist has no publish protocol, so local mode must be refused",
    );
    assert!(err.contains("not supported for nodedist"), "{err}");
}

// ── RFC 0010 phase 6: sdkman ─────────────────────────────────────────────────

/// On `sdkman` the field *is* the rule: no artifact ever carries a date.
#[test]
fn an_age_gate_on_sdkman_must_state_deny_missing_timestamp() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 86400
        "#,
        "an age gate on sdkman without deny_missing_timestamp must not load",
    );
    assert!(err.contains("deny_missing_timestamp"), "{err}");
    assert!(err.contains("sdkman"), "the error names the kind: {err}");
    assert!(
        err.contains("no dates at all"),
        "the error says what the field decides on this kind: {err}"
    );
    for value in ["true", "false"] {
        parse_config(&format!(
            r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 86400
        deny_missing_timestamp = {value}
        "#
        ))
        .validate()
        .unwrap_or_else(|e| panic!("deny_missing_timestamp = {value} must load: {e}"));
    }
}

/// Proxy-only, default upstream, and `broker_url` optional.
#[test]
fn sdkman_is_proxy_only_and_needs_no_explicit_upstream() {
    parse_config(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        "#,
    )
    .validate()
    .expect("a bare sdkman registry loads");

    parse_config(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        upstreams = ["https://api.sdkman.io/2"]
        broker_url = "https://broker.sdkman.io"
        "#,
    )
    .validate()
    .expect("both hosts stated loads");

    let err = validation_error(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        mode = "hybrid"
        "#,
        "sdkman has no publish protocol, so hybrid mode must be refused",
    );
    assert!(err.contains("not supported for sdkman"), "{err}");
}

/// `broker_url` is SDKMAN's second host and nobody else's; silently ignored
/// elsewhere it would read as a working option that does nothing.
#[test]
fn broker_url_is_refused_off_sdkman_and_must_be_absolute() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        broker_url = "https://broker.sdkman.io"
        "#,
        "broker_url on npm must not load",
    );
    assert!(err.contains("broker_url"), "{err}");
    assert!(err.contains("sdkman"), "{err}");

    let err = validation_error(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        broker_url = "broker.sdkman.io"
        "#,
        "a relative broker_url must not load",
    );
    assert!(err.contains("absolute http(s) URL"), "{err}");
}

/// `path_allow` means nothing on a typed kind; the existing rule refuses it
/// on `sdkman` exactly as on `nodedist` (RFC 0010 §13.1).
#[test]
fn path_allow_on_sdkman_is_refused() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        path_allow = ["**"]
        "#,
        "path_allow on sdkman must not load",
    );
    assert!(err.contains("path_allow"), "{err}");
}

/// `warm_platforms` names the files warming fetches on the two
/// platform-addressed kinds; on `sdkman` the set is closed (RFC 0010 §6.9).
#[test]
fn warm_platforms_is_checked_on_sdkman_and_refused_elsewhere() {
    parse_config(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"

        [registries.cache]
        warm_packages = ["java@21.0.5-tem"]
        warm_platforms = ["linuxx64", "darwinarm64"]
        "#,
    )
    .validate()
    .expect("two SDKMAN platforms load");
    parse_config(
        r#"
        [[registries]]
        type = "nodedist"
        name = "node"

        [registries.cache]
        warm_platforms = ["linux-x64", "darwin-arm64"]
        "#,
    )
    .validate()
    .expect("Node's platform tags load");

    let err = validation_error(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"

        [registries.cache]
        warm_platforms = ["linux-x64"]
        "#,
        "a Node spelling is not an SDKMAN platform",
    );
    assert!(err.contains("not an SDKMAN platform"), "{err}");

    let err = validation_error(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [registries.cache]
        warm_platforms = ["linuxx64"]
        "#,
        "warm_platforms means nothing on npm",
    );
    assert!(err.contains("warm_platforms"), "{err}");
}

/// An upstream without the `/2` is served as given and warned about.
#[test]
fn an_sdkman_upstream_without_the_api_version_is_warned_not_refused() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        upstreams = ["https://sdkman.internal.example"]
        "#,
    );
    cfg.validate().expect("served as given");
    let warning = cfg
        .warnings()
        .into_iter()
        .find(|w| w.code == warnings::SDKMAN_UPSTREAM_WITHOUT_API_VERSION)
        .expect("a warning names the upstream");
    assert_eq!(warning.path, "registries[0].upstreams[0]");

    let cfg = parse_config(
        r#"
        [[registries]]
        type = "sdkman"
        name = "jvm"
        upstreams = ["https://api.sdkman.io/2/"]
        "#,
    );
    assert!(
        !cfg.warnings()
            .iter()
            .any(|w| w.code == warnings::SDKMAN_UPSTREAM_WITHOUT_API_VERSION),
        "a trailing slash after the version is not a missing version"
    );
}

// ── RFC 0019 §4.3: [registries.refs] and the anonymous-forge warning ─────────

#[test]
fn refs_is_refused_on_a_registry_that_is_not_a_forge() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [registries.refs]
        branch_ttl_secs = 60
        "#,
        "[registries.refs] on npm must not load",
    );
    assert!(err.contains("refs"), "{err}");
    assert!(err.contains("npm"), "{err}");
}

#[test]
fn a_branch_ttl_below_the_floor_is_refused_and_the_defaults_load() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "github"
        name = "gh"

        [registries.refs]
        branch_ttl_secs = 5
        "#,
        "a 5-second branch TTL must not load",
    );
    assert!(err.contains("branch_ttl_secs"), "{err}");

    let cfg = parse_config(
        r#"
        [[registries]]
        type = "forgejo"
        name = "fj"
        upstreams = ["https://codeberg.org"]

        [registries.refs]
        "#,
    );
    cfg.validate()
        .expect("an empty refs block takes the defaults");
    let refs = cfg.registries[0].refs.as_ref().unwrap();
    assert_eq!((refs.branch_ttl_secs, refs.tag_ttl_secs), (60, 3600));
}

#[test]
fn a_forge_registry_without_a_token_warns_and_one_with_a_token_does_not() {
    let anonymous = parse_config(
        r#"
        [[registries]]
        type = "github"
        name = "gh"
        "#,
    );
    assert!(
        warning_codes(&anonymous).contains(&warnings::FORGE_ANONYMOUS_UPSTREAM.to_owned()),
        "{:?}",
        warning_codes(&anonymous)
    );

    let authenticated = parse_config(
        r#"
        [[registries]]
        type = "github"
        name = "gh"

        [registries.upstream_auth]
        type = "bearer"
        token = "ghp_x"
        "#,
    );
    assert!(!warning_codes(&authenticated).contains(&warnings::FORGE_ANONYMOUS_UPSTREAM.to_owned()));

    let npm = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        "#,
    );
    assert!(
        !warning_codes(&npm).contains(&warnings::FORGE_ANONYMOUS_UPSTREAM.to_owned()),
        "a package registry is not metered like a forge"
    );
}

// ── RFC 0018 §4.3: [registries.security] ─────────────────────────────────────

fn security_config(body: &str, extra: &str) -> String {
    format!(
        r#"
{extra}
        [[registries]]
        type = "npm"
        name = "npm"

        [registries.security]
{body}
        "#
    )
}

#[test]
fn a_security_section_with_only_mode_loads_with_the_documented_defaults() {
    let cfg = parse_config(&security_config(r#"        mode = "block""#, ""));
    cfg.validate().expect("mode alone is a complete profile");
    let sec = cfg.registries[0].security.as_ref().unwrap();
    assert_eq!((sec.min_age_secs, sec.mature_age_secs), (86_400, 86_400));
    assert_eq!(sec.scanners, vec!["osv"]);
    assert_eq!(sec.required_scanners, vec!["osv"]);
    assert!(sec.hold_missing_timestamp);
    let policy = sec.to_policy("npm", &cfg.scanners);
    assert_eq!(policy.policy_ref, "npm/default");
    assert_eq!(policy.max_severity, batlehub_core::entities::Severity::High);
}

#[test]
fn min_age_below_one_hour_is_refused() {
    let err = validation_error(
        &security_config(r#"        min_age_secs = 600"#, ""),
        "a 10-minute quarantine must not load",
    );
    assert!(err.contains("min_age_secs"), "{err}");
    assert!(err.contains("3600"), "{err}");
}

#[test]
fn security_and_a_release_age_rule_on_one_registry_are_refused() {
    let err = validation_error(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [registries.security]
        mode = "block"

        [[registries.rules]]
        kind = "release_age_gate"
        min_age_secs = 7200
        "#,
        "two owners of the age gate must not load",
    );
    assert!(err.contains("release_age_gate"), "{err}");
}

#[test]
fn an_undeclared_scanner_and_a_required_scanner_outside_scanners_are_refused() {
    let err = validation_error(
        &security_config(r#"        scanners = ["osv", "trivy"]"#, ""),
        "an undeclared scanner must not load",
    );
    assert!(err.contains("trivy") && err.contains("[scanners]"), "{err}");

    let err = validation_error(
        &security_config(
            r#"        scanners = ["osv"]
        required_scanners = ["osv", "postmortem"]"#,
            "",
        ),
        "a required scanner that never runs must not load",
    );
    assert!(err.contains("required_scanners"), "{err}");
}

/// RFC 0018 phase 5 (§13.7): the external scanners ship, so declaring one
/// is accepted — the refusal that used to stand here was "not before it
/// ships", and it has.
#[test]
fn the_phase_5_scanners_load_when_declared() {
    parse_config(&security_config(
        r#"        scanners = ["osv", "socket", "mlab"]"#,
        r#"
        [scanners.socket]
        type = "socket"
        api_key = "k"

        [scanners.mlab]
        type = "mlab"
        "#,
    ))
    .validate()
    .expect("the phase-5 scanners are runnable now");
}

/// RFC 0018 phase 3: the archive and provenance scanners are runnable now.
#[test]
fn the_phase_3_scanners_load_when_declared() {
    parse_config(&security_config(
        r#"        scanners = ["osv", "trivy", "sigstore"]"#,
        r#"
        [scanners.trivy]
        type = "trivy"
        endpoint = "http://trivy:4954"

        [scanners.sigstore]
        type = "sigstore"
        require_for = ["npm"]
        "#,
    ))
    .validate()
    .expect("phase-3 scanners load");
}

#[test]
fn a_socket_scanner_without_a_key_and_a_bad_command_are_refused() {
    let err = validation_error(
        r#"
        [scanners.socket]
        type = "socket"
        "#,
        "socket without api_key must not load",
    );
    assert!(err.contains("api_key"), "{err}");

    let err = validation_error(
        r#"
        [scanners.pm]
        type = "postmortem"
        command = "/nonexistent/postmortem"
        "#,
        "a missing command must not load",
    );
    assert!(err.contains("executable"), "{err}");

    parse_config(
        r#"
        [scanners.pm]
        type = "postmortem"
        command = "/bin/sh"
        "#,
    )
    .validate()
    .expect("an executable command loads");
}

#[test]
fn mature_age_below_min_age_and_an_unknown_severity_are_refused() {
    let err = validation_error(
        &security_config(
            r#"        min_age_secs = 7200
        mature_age_secs = 3600"#,
            "",
        ),
        "mature before servable is nonsense",
    );
    assert!(err.contains("mature_age_secs"), "{err}");
    let err = validation_error(
        &security_config(r#"        max_severity = "scary""#, ""),
        "an unknown severity must not load",
    );
    assert!(err.contains("max_severity"), "{err}");
}

#[test]
fn an_unsigned_webhook_is_refused_once_any_registry_is_quarantined() {
    let err = validation_error(
        &security_config(
            r#"        mode = "block""#,
            r#"
        [notifications]
        enabled = true

        [[notifications.inbound]]
        name = "soc"
        "#,
        ),
        "an unsigned webhook beside a quarantine must not load",
    );
    assert!(err.contains("secret"), "{err}");
}

#[test]
fn roles_and_worker_scoping_are_checked() {
    let err = validation_error(
        r#"
        roles = []
        "#,
        "an empty roles list must not load",
    );
    assert!(err.contains("roles"), "{err}");

    let cfg = parse_config("");
    assert_eq!(cfg.server.roles, default_roles(), "absent means both");

    let err = validation_error(
        r#"
        [worker]
        registries = ["nope"]
        "#,
        "an unknown worker registry must not load",
    );
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn warn_mode_with_nothing_required_and_a_dateless_kind_warn() {
    let cfg = parse_config(&security_config(
        r#"        mode = "warn"
        required_scanners = []"#,
        "",
    ));
    assert!(warning_codes(&cfg).contains(&warnings::SECURITY_UNPROTECTED.to_owned()));

    let cfg = parse_config(
        r#"
        [[registries]]
        type = "deb"
        name = "deb"
        upstreams = ["https://deb.debian.org/debian"]

        [registries.security]
        mode = "block"
        "#,
    );
    assert!(warning_codes(&cfg).contains(&warnings::SECURITY_TIMESTAMP_HOLD_UNAVAILABLE.to_owned()));

    let quiet = parse_config(&security_config(r#"        mode = "block""#, ""));
    assert!(
        !warning_codes(&quiet).contains(&warnings::SECURITY_TIMESTAMP_HOLD_UNAVAILABLE.to_owned()),
        "npm dates its versions"
    );
}

// ── RFC 0014 §4.4: [upstream_audit] ──────────────────────────────────────────

fn audit_config(body: &str) -> String {
    format!(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [[registries]]
        type = "npm"
        name = "mine"
        mode = "local"

        [upstream_audit]
{body}
        "#
    )
}

#[test]
fn an_absent_upstream_audit_block_is_off_with_the_documented_defaults() {
    let cfg = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm"
        "#,
    );
    cfg.validate().unwrap();
    let a = &cfg.upstream_audit;
    assert!(!a.enabled);
    assert_eq!(
        a.on_confirmed, "audit",
        "the single most important default in RFC 0014"
    );
    assert_eq!((a.confirm_after, a.confirm_min_age_secs), (3, 86_400));
    assert_eq!(a.interval_secs, 21_600);
    assert!(a.retain_disappeared && a.skip_recently_seen);
    assert_eq!(cfg.upstream_audit_registries(), vec!["npm"]);
}

#[test]
fn the_audited_set_is_every_proxy_or_hybrid_registry_unless_named() {
    let cfg = parse_config(&audit_config("        enabled = true"));
    cfg.validate().unwrap();
    assert_eq!(
        cfg.upstream_audit_registries(),
        vec!["npm"],
        "the local one is never in"
    );
    let cfg = parse_config(&audit_config(
        r#"        enabled = true
        registries = ["npm"]"#,
    ));
    cfg.validate().unwrap();
    assert_eq!(cfg.upstream_audit_registries(), vec!["npm"]);
}

#[test]
fn upstream_audit_rejections() {
    for (body, needle) in [
        ("        confirm_after = 0", "confirm_after"),
        ("        outage_ratio = 0.0", "outage_ratio"),
        ("        outage_ratio = 1.5", "outage_ratio"),
        ("        interval_secs = 60", "interval_secs"),
        (
            r#"        registries = ["nope"]"#,
            "not a configured registry",
        ),
        (r#"        registries = ["mine"]"#, "local registry"),
        (
            r#"        on_confirmed = "blocked""#,
            "not \"audit\" or \"block\"",
        ),
        (r#"        on_confirmed = "block""#, "enabled = false"),
    ] {
        let err = validation_error(&audit_config(body), body);
        assert!(err.contains(needle), "{body}: {err}");
    }
}

/// RFC 0014 phase 6: `"block"` is accepted with the audit enabled, and the
/// one combination §5.4 shows is quietly destructive warns.
#[test]
fn on_confirmed_block_is_accepted_and_warns_without_the_hold() {
    let ok = audit_config(
        r#"        enabled = true
        on_confirmed = "block""#,
    );
    let cfg = parse_config(&ok);
    cfg.validate()
        .expect("block is RFC 0014 phase 6, and it is built");
    assert_eq!(cfg.upstream_audit.on_confirmed, "block");
    let codes: Vec<String> = cfg.warnings().iter().map(|w| w.code.clone()).collect();
    assert!(
        !codes
            .iter()
            .any(|c| c == warnings::UPSTREAM_AUDIT_BLOCK_WITHOUT_HOLD),
        "the hold is on by default: {codes:?}"
    );

    let cfg = parse_config(&audit_config(
        r#"        enabled = true
        on_confirmed = "block"
        retain_disappeared = false"#,
    ));
    cfg.validate().unwrap();
    let codes: Vec<String> = cfg.warnings().iter().map(|w| w.code.clone()).collect();
    assert!(
        codes
            .iter()
            .any(|c| c == warnings::UPSTREAM_AUDIT_BLOCK_WITHOUT_HOLD),
        "{codes:?}"
    );
}

#[test]
fn a_misspelled_upstream_audit_key_fails_the_load() {
    let raw = audit_config("        confirm_afer = 3");
    assert!(
        crate::load_from_str(&raw).is_err(),
        "a typo must not silently take the default"
    );
}

#[test]
fn upstream_audit_warnings_name_the_missing_registry_and_the_missing_role() {
    let cfg = crate::load_from_str(
        r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"

        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"

        [server]
        roles = ["proxy"]

        [[registries]]
        type = "npm"
        name = "mine"
        mode = "local"

        [upstream_audit]
        enabled = true
        "#,
    )
    .expect("loads");
    let ws = cfg.warnings();
    let codes: Vec<&str> = ws.iter().map(|w| w.code.as_str()).collect();
    assert!(
        codes.contains(&warnings::UPSTREAM_AUDIT_NOTHING_TO_AUDIT),
        "{codes:?}"
    );
    assert!(
        codes.contains(&warnings::UPSTREAM_AUDIT_NO_WORKER),
        "{codes:?}"
    );
    let quiet = parse_config(&audit_config("        enabled = true"));
    let codes: Vec<String> = quiet.warnings().into_iter().map(|w| w.code).collect();
    assert!(
        !codes.iter().any(|c| c.starts_with("upstream-audit")),
        "{codes:?}"
    );
}

// ── RFC 0008 §4.5: the air gap is a promise about the whole instance ──────────
//
// Everything that contradicts it is refused at load rather than discovered
// from a log, and the two things that *narrow* behaviour without breaking it
// are warnings rather than refusals.

/// A proxy registry plus whatever `extra` adds, with `[air_gap]` on and a
/// usable key — the shape every rejection below deviates from by one line.
fn air_gapped_config(extra: &str) -> AppConfig {
    parse_config(&format!(
        r#"
        [air_gap]
        enabled = true
        bundle_trusted_keys = ["{key}"]

        [[registries]]
        type = "npm"
        name = "npm-mirror"
        mode = "proxy"
        upstreams = ["https://registry.npmjs.org"]
{extra}
        "#,
        key = "a".repeat(64),
    ))
}

#[test]
fn an_air_gapped_instance_with_a_usable_key_and_no_egress_is_valid() {
    air_gapped_config("")
        .validate()
        .expect("the ordinary shape");
}

#[test]
fn the_section_is_absent_by_default_and_changes_nothing() {
    let cfg = parse_config("");
    assert!(cfg.air_gap.is_none());
    cfg.validate().expect("today's config still validates");
}

#[test]
fn an_air_gap_with_no_trusted_key_is_refused() {
    let err = parse_config(
        r#"
        [air_gap]
        enabled = true
        "#,
    )
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("bundle_trusted_keys"), "{err}");
    assert!(
        err.contains("any bundle"),
        "the message says what an empty list means: {err}"
    );
}

#[test]
fn a_trusted_key_that_is_not_thirty_two_hex_bytes_is_refused_whether_or_not_the_mode_is_on() {
    for enabled in ["true", "false"] {
        let err = parse_config(&format!(
            r#"
            [air_gap]
            enabled = {enabled}
            bundle_trusted_keys = ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5"]
            "#
        ))
        .validate()
        .unwrap_err()
        .to_string();
        assert!(err.contains("hex-encoded 32-byte"), "{enabled}: {err}");
    }
}

/// The check `signing.trusted_keys` never had. It was parsed at verify time,
/// so a typo read as "signing is configured" and surfaced as a `502` on the
/// first download, naming nothing.
#[test]
fn a_malformed_signing_key_is_refused_at_load_rather_than_at_the_first_download() {
    let err = parse_config(
        r#"
        [[registries]]
        type = "npm"
        name = "npm-mirror"
        mode = "proxy"
        upstreams = ["https://registry.npmjs.org"]

        [registries.signing]
        trusted_keys = ["not-a-key"]
        "#,
    )
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("signing.trusted_keys"), "{err}");
    assert!(err.contains("npm-mirror"), "the message names it: {err}");
}

#[test]
fn an_egress_proxy_and_an_air_gap_together_are_refused_at_both_levels() {
    let err = parse_config(&format!(
        r#"
        [air_gap]
        enabled = true
        bundle_trusted_keys = ["{key}"]

        [proxy]
        url = "http://egress.corp:3128"
        "#,
        key = "a".repeat(64),
    ))
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("[proxy]"), "{err}");

    let err = air_gapped_config(
        r#"
        [registries.proxy]
        url = "http://egress.corp:3128"
        "#,
    )
    .validate()
    .unwrap_err()
    .to_string();
    assert!(err.contains("route off the site"), "{err}");
}

/// Warming fetches from an upstream this mode guarantees will never be
/// dialled. Failing at boot beats a startup task logging a connect error per
/// path, forever.
#[test]
fn warming_configured_on_an_air_gapped_instance_is_refused() {
    for line in [
        r#"warm_packages = ["lodash"]"#,
        r#"warm_paths = ["/lodash/-/lodash-4.17.21.tgz"]"#,
    ] {
        let err = air_gapped_config(&format!(
            r#"
            [registries.cache]
            {line}
            "#
        ))
        .validate()
        .unwrap_err()
        .to_string();
        assert!(err.contains("warming"), "{line}: {err}");
        assert!(err.contains("bundle"), "it says what to do instead: {err}");
    }
}

/// A hybrid registry keeps working — publishing to a disconnected instance is
/// legitimate — but its fall-through can never happen, so it behaves as a
/// local registry and an operator should not have to deduce that.
#[test]
fn a_hybrid_registry_under_an_air_gap_warns_rather_than_failing() {
    let cfg = parse_config(&format!(
        r#"
        [air_gap]
        enabled = true
        bundle_trusted_keys = ["{key}"]

        [[registries]]
        type = "npm"
        name = "npm-mirror"
        mode = "hybrid"
        upstreams = ["https://registry.npmjs.org"]
        "#,
        key = "a".repeat(64),
    ));
    cfg.validate().expect("allowed, and warned about");
    assert!(
        warning_codes(&cfg).contains(&warnings::AIR_GAP_HYBRID_REGISTRY.to_owned()),
        "{:?}",
        warning_codes(&cfg)
    );
}

/// Staging a bundle on a *connected* instance is how one is built, so keys
/// with the mode off are kept — and the warning says they authorise imports
/// and nothing else.
#[test]
fn trusted_keys_without_the_mode_are_kept_and_explained() {
    let cfg = parse_config(&format!(
        r#"
        [air_gap]
        enabled = false
        bundle_trusted_keys = ["{key}"]
        "#,
        key = "b".repeat(64),
    ));
    cfg.validate().expect("valid: this is the staging instance");
    assert!(warning_codes(&cfg).contains(&warnings::AIR_GAP_KEYS_UNUSED.to_owned()));
}

/// RFC 0014 §13 O6: the registry-tier `on_confirmed` row, held to the
/// estate key's own rules.
#[test]
fn a_registry_tier_on_confirmed_is_validated_like_the_estate_key() {
    let with = |registry_line: &str, audit: &str| {
        format!(
            r#"
        [[registries]]
        type = "npm"
        name = "npm"
{registry_line}

        [[registries]]
        type = "npm"
        name = "mine"
        mode = "local"

        [upstream_audit]
{audit}
        "#
        )
    };
    // A proxy registry may say block once the audit sweeps it.
    let cfg = parse_config(&with(
        r#"        on_confirmed = "block""#,
        "        enabled = true",
    ));
    cfg.validate()
        .expect("a registry-tier block under an enabled audit");
    assert_eq!(cfg.registries[0].on_confirmed.as_deref(), Some("block"));
    // …and warns without the hold, exactly as the estate key does.
    let cfg = parse_config(&with(
        r#"        on_confirmed = "block""#,
        "        enabled = true\n        retain_disappeared = false",
    ));
    cfg.validate().unwrap();
    let codes: Vec<String> = cfg.warnings().iter().map(|w| w.code.clone()).collect();
    assert!(
        codes
            .iter()
            .any(|c| c == warnings::UPSTREAM_AUDIT_BLOCK_WITHOUT_HOLD),
        "{codes:?}"
    );
    // Audit is always accepted, sweeping or not.
    parse_config(&with(
        r#"        on_confirmed = "audit""#,
        "        enabled = false",
    ))
    .validate()
    .expect("audit is the default and never a lie");

    // Block with nothing sweeping reads as if blocking were active.
    let err = parse_config(&with(
        r#"        on_confirmed = "block""#,
        "        enabled = false",
    ))
    .validate()
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("'npm'") && err.contains("not enabled"),
        "{err}"
    );
    // Block on a registry the audit does not sweep.
    let err = parse_config(&with(
        r#"        on_confirmed = "block""#,
        r#"        enabled = true
        registries = ["mine-too"]"#,
    ))
    .validate()
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("mine-too") || err.contains("not audited"),
        "{err}"
    );
    // A typo never falls back.
    let err = parse_config(&with(
        r#"        on_confirmed = "blocked""#,
        "        enabled = true",
    ))
    .validate()
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("\"blocked\"") && err.contains("'npm'"),
        "{err}"
    );
}

#[test]
fn a_local_registry_cannot_say_block() {
    let text = r#"
        [[registries]]
        type = "npm"
        name = "npm"

        [[registries]]
        type = "npm"
        name = "mine"
        mode = "local"
        on_confirmed = "block"

        [upstream_audit]
        enabled = true
        "#;
    let err = parse_config(text).validate().unwrap_err().to_string();
    assert!(
        err.contains("'mine'") && err.contains("not audited"),
        "{err}"
    );
}
