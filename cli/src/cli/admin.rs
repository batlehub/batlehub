use anyhow::Result;
use clap::Subcommand;
use comfy_table::Table;

use crate::api::{
    admin::{
        AccessSimulationResponse, AuditEntry, AuditQuery, BlockedUserEntry, BulkPackageResult,
        CoherenceReportDto, EvictionReportDto, ExposureQuery, ExposureResponse, FlagsQuery,
        FlagsResponse, MissingQuery, NotificationChannelEntry, NotificationSubscriptionEntry,
        RegistryHealthEntry, SimulateAccessRequest, StatsResponse, TeamNamespaceEntry,
    },
    version::RetentionReport,
    BatleHubClient,
};

fn parse_pkg_version(s: &str) -> anyhow::Result<(String, String)> {
    let (name, version) = s
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("expected name@version, got: {s}"))?;
    Ok((name.to_string(), version.to_string()))
}

#[derive(Subcommand)]
pub enum FlagsCommand {
    /// List the flags pushed by the configured sources
    List {
        #[arg(long)]
        registry: Option<String>,
        /// Only this package name
        #[arg(long)]
        package: Option<String>,
        /// Only this `[[flag_sources]]` name
        #[arg(long)]
        source: Option<String>,
        /// inform, warn, gate or hard_block
        #[arg(long)]
        effect: Option<String>,
        /// Include revoked and expired flags
        #[arg(long)]
        include_dead: bool,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long, default_value_t = 50)]
        per_page: u64,
    },
}

#[derive(Subcommand)]
pub enum AdminCommand {
    /// Quota management
    Quota {
        #[command(subcommand)]
        cmd: QuotaCommand,
    },
    /// IP block management
    IpBlock {
        #[command(subcommand)]
        cmd: IpBlockCommand,
    },
    /// Configuration management
    Config {
        #[command(subcommand)]
        cmd: ConfigAdminCommand,
    },
    /// Cache management
    Cache {
        #[command(subcommand)]
        cmd: CacheCommand,
    },
    /// Global banner management
    Banner {
        #[command(subcommand)]
        cmd: BannerCommand,
    },
    /// Run retention over a registry's locally published versions
    ///
    /// Reports by default. Reclaiming needs `dry_run = false` on the registry's
    /// `[registries.retention]` block *and* `--reclaim` here — a deletion that
    /// destroys the only copy should take two decisions, not one.
    Retention {
        /// Registry name
        registry: String,
        /// Actually reclaim. Without it the run reports and changes nothing,
        /// whatever the server is configured for.
        #[arg(long)]
        reclaim: bool,
        /// Print every kept version and the condition that kept it, not just
        /// what would be reclaimed.
        #[arg(long)]
        show_kept: bool,
    },
    /// Who pulled a flagged version (RFC 0002): the exposure report
    ///
    /// One row per consumer, coordinate and flag, newest pull first, with
    /// how many of the pulls preceded the flag. `--when before-flag` keeps
    /// the retroactive rows only. Pages by cursor: pass `--after` the value
    /// the previous page printed.
    Exposure {
        #[arg(long)]
        registry: Option<String>,
        /// Only this package name
        #[arg(long)]
        package: Option<String>,
        /// Only flags from this `[[flag_sources]]` name
        #[arg(long)]
        source: Option<String>,
        /// Only flags at this effect or stronger: inform, warn, gate, hard_block
        #[arg(long)]
        min_effect: Option<String>,
        /// any (default), before-flag, after-flag
        #[arg(long)]
        when: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        /// The cursor the previous page printed
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u64,
    },
    /// What came across the air gap: the bundles this instance imported
    /// (RFC 0008)
    Bundles,
    /// What this instance was asked for and did not hold (RFC 0008)
    AirGapMissing {
        #[arg(long)]
        registry: Option<String>,
        /// artifact, document, checksum, ref or unmirrored_host
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long, default_value_t = 100)]
        per_page: u64,
    },
    /// Pushed vulnerability flags (RFC 0002)
    Flags {
        #[command(subcommand)]
        cmd: FlagsCommand,
    },
    /// Query the access audit log
    AuditLog {
        #[arg(long)]
        registry: Option<String>,
        /// Only this package name
        #[arg(long)]
        package: Option<String>,
        /// Only these actions, comma-separated (e.g. `delete,retention_reclaim`)
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        /// Show only denied requests
        #[arg(long)]
        denied_only: bool,
        #[arg(long, default_value = "0")]
        page: u64,
        #[arg(long, default_value = "50")]
        per_page: u64,
        /// Purge entries older than this ISO-8601 datetime (e.g. 2024-01-01T00:00:00Z)
        #[arg(long, conflicts_with_all = &["registry","package","action","user","from","to","denied_only"])]
        purge_before: Option<String>,
    },
    /// Show aggregate server statistics (cache hit rate, bytes served, …)
    Stats,
    /// Show per-registry and backend health status
    Health,
    /// Manage package visibility
    Visibility {
        #[command(subcommand)]
        cmd: VisibilityCommand,
    },
    /// Manage package- and version-tier grants (RFC 0017)
    ///
    /// The two tiers a config file cannot enumerate. Registry- and
    /// namespace-tier grants stay in `config.toml`, where broad authorization
    /// belongs — this edits the deep ones, per package and per version.
    Grants {
        #[command(subcommand)]
        cmd: GrantsCommand,
    },
    /// Manage team namespace prefix claims
    Namespace {
        #[command(subcommand)]
        cmd: NamespaceCommand,
    },
    /// Manage blocked users
    Users {
        #[command(subcommand)]
        cmd: UsersCommand,
    },
    /// Show or export software bill of materials
    Sbom {
        #[command(subcommand)]
        cmd: SbomCommand,
    },
    /// Manage notification channels and subscriptions
    Notifications {
        #[command(subcommand)]
        cmd: NotificationsCommand,
    },
    /// Bulk yank / unyank / delete versions across a registry
    Bulk {
        #[command(subcommand)]
        cmd: BulkCommand,
    },
    /// Mark a package version as deprecated
    Deprecate {
        registry: String,
        name: String,
        version: String,
        #[arg(long)]
        message: Option<String>,
    },
    /// Remove deprecation from a package version
    Undeprecate {
        registry: String,
        name: String,
        version: String,
    },
    /// Hide a package version from search / listings (without deleting)
    Unlist {
        registry: String,
        name: String,
        version: String,
    },
    /// Re-list a previously unlisted package version
    Relist {
        registry: String,
        name: String,
        version: String,
    },
    /// Simulate whether an identity would be allowed to access a registry resource
    AccessCheck {
        /// Registry to evaluate the policy against
        #[arg(long, short = 'r')]
        registry: String,
        /// Package name
        #[arg(long, short = 'p')]
        package: String,
        /// Package version
        #[arg(long, short = 'v')]
        version: String,
        /// Resource type to check (e.g. "releases:read", "source:read")
        #[arg(long, default_value = "releases:read")]
        resource: String,
        /// Simulated user id
        #[arg(long)]
        user: Option<String>,
        /// Simulated role: anonymous, user, or admin (default: anonymous)
        #[arg(long)]
        role: Option<String>,
        /// Simulated OIDC groups (comma-separated or repeated flag)
        #[arg(long, value_delimiter = ',')]
        groups: Vec<String>,
    },
    /// Export audit log events for compliance review
    ExportAuditLog {
        /// Start datetime (RFC 3339)
        #[arg(long)]
        from: Option<String>,
        /// End datetime (RFC 3339)
        #[arg(long)]
        to: Option<String>,
        /// Filter by registry
        #[arg(long)]
        registry: Option<String>,
        /// Only these actions, comma-separated (e.g. `delete,retention_reclaim`)
        #[arg(long)]
        action: Option<String>,
        /// Output format: json (default) or csv
        #[arg(long, default_value = "json")]
        format: String,
        /// Write output to file instead of stdout
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum QuotaCommand {
    /// List quota usage
    List {
        /// Filter by registry
        #[arg(long, short = 'r')]
        registry: Option<String>,
    },
    /// Reset quota for a specific user in a registry
    Reset { registry: String, user: String },
}

#[derive(Subcommand)]
pub enum IpBlockCommand {
    /// List blocked IPs
    List,
    /// Block an IP address
    Add {
        ip: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Unblock an IP address
    Remove { ip: String },
}

#[derive(Subcommand)]
pub enum ConfigAdminCommand {
    /// Trigger an immediate config reload on the server
    Reload,
    /// Show recent config change history
    Changes,
    /// Print the current active server configuration (TOML)
    View,
    /// Validate a local TOML config file against the server
    Validate {
        /// Path to the TOML config file to validate
        file: String,
    },
    /// Apply a local TOML config file as the new pending configuration
    FromFile {
        /// Path to the TOML config file to apply
        file: String,
    },
}

#[derive(Subcommand)]
pub enum CacheCommand {
    /// Pre-warm the cache for a registry
    Warm {
        registry: String,
        /// Comma-separated list of package names to warm
        #[arg(long)]
        packages: Option<String>,
        /// Comma-separated upstream artifact paths to warm, for path-addressed
        /// registries (deb/rpm/jetbrains), e.g. "idea/idea-2026.1.3.tar.gz"
        #[arg(long)]
        paths: Option<String>,
    },
    /// Clear the metadata cache for a registry
    Clear { registry: String },
    /// Run the registry's configured cache-eviction strategies
    ///
    /// Evicts by default, unlike `admin retention`: what goes here is a cached
    /// copy the next request re-fetches, so there is no only-copy to protect
    /// with a two-key interlock. `--dry-run` previews instead.
    Evict {
        /// Registry name
        registry: String,
        /// Report what would be evicted, and evict nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete storage blobs nothing references any more
    ///
    /// Orphans come from a crashed cache write or a row deleted from the
    /// database by hand. **Two passes before anything goes**: a blob is deleted
    /// only if the previous sweep saw it orphaned too, so the first run on a
    /// fresh estate reports and deletes nothing — run it again to collect.
    ///
    /// `--dry-run` reports without deleting *and* without advancing anything
    /// toward deletion, so previewing twice is not the same as running twice.
    Coherence {
        /// Registry name
        registry: String,
        /// Report only; delete nothing and arm nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum BannerCommand {
    /// Set the global admin banner
    Set {
        message: String,
        #[arg(long, default_value = "info")]
        level: String,
    },
    /// Clear the global admin banner
    Clear,
}

#[derive(Subcommand)]
pub enum VisibilityCommand {
    /// Get the visibility of a package
    Get { registry: String, name: String },
    /// Set the visibility of a package (public | internal | team)
    Set {
        registry: String,
        name: String,
        visibility: String,
    },
}

#[derive(Subcommand)]
pub enum GrantsCommand {
    /// List the grants on a package and on each of its versions
    List {
        registry: String,
        /// `name` for the package tier and every version beneath it, or
        /// `name@version` for one version node.
        target: String,
    },
    /// Write a subject's grant, replacing that subject's row on the node
    Set {
        registry: String,
        /// `name` or `name@version` — the same spelling the node is stored
        /// under, so what you type and what is stored read the same.
        target: String,
        /// `user:alice`, `group:oidc1:eng`, `role:user`, or `*`
        #[arg(long)]
        subject: String,
        /// Comma-separated verbs or patterns, e.g. `releases:read,releases:list`
        /// or `releases:*`. Patterns are expanded by the server at write.
        #[arg(long, value_delimiter = ',')]
        actions: Vec<String>,
    },
    /// Remove a subject's grant from a node
    Rm {
        registry: String,
        /// `name` or `name@version`
        target: String,
        #[arg(long)]
        subject: String,
    },
}

#[derive(Subcommand)]
pub enum NamespaceCommand {
    /// List claimed namespace prefixes for a registry
    List { registry: String },
    /// Claim a namespace prefix for a team group
    Claim {
        registry: String,
        prefix: String,
        group_id: String,
    },
    /// Release a claimed namespace prefix
    Release { registry: String, prefix: String },
}

#[derive(Subcommand)]
pub enum UsersCommand {
    /// List all blocked users
    ListBlocked,
    /// Block a user
    Block {
        user_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Unblock a user
    Unblock { user_id: String },
}

#[derive(Subcommand)]
pub enum SbomCommand {
    /// Show SBOM for a specific package version
    Get {
        registry: String,
        name: String,
        version: String,
        #[arg(long, default_value = "cyclonedx")]
        format: String,
    },
    /// Export SBOMs for a registry or time range
    Export {
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "cyclonedx")]
        format: String,
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum NotificationsCommand {
    /// List configured notification channels
    Channels,
    /// List notification subscriptions
    List,
    /// Delete a notification subscription by ID
    Delete { id: String },
}

#[derive(Subcommand)]
pub enum BulkCommand {
    /// Yank multiple versions (format: name@version)
    Yank {
        registry: String,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Unyank multiple versions (format: name@version)
    Unyank {
        registry: String,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Delete multiple versions (format: name@version)
    Delete {
        registry: String,
        #[arg(required = true)]
        packages: Vec<String>,
    },
}

/// RFC 0002 §13's exposure report.
async fn handle_exposure(q: ExposureQuery, client: &BatleHubClient, json: bool) -> Result<()> {
    let resp = client.exposure(q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        print_exposure(&resp);
    }
    Ok(())
}

/// The pushed flags a source has raised.
async fn handle_flags(cmd: FlagsCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    let FlagsCommand::List {
        registry,
        package,
        source,
        effect,
        include_dead,
        page,
        per_page,
    } = cmd;
    let resp = client
        .list_flags(FlagsQuery {
            registry,
            package_name: package,
            source,
            effect,
            include_dead,
            page,
            per_page,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        print_flags(&resp);
    }
    Ok(())
}

/// Every bundle this instance has imported (RFC 0008).
async fn handle_bundles(client: &BatleHubClient, json: bool) -> Result<()> {
    let items = client.list_bundles().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header(["Bundle", "Signer", "Imported", "By", "Blobs", "Rejected"]);
    for b in &items {
        table.add_row([
            b.bundle_id.clone(),
            b.signer_key.chars().take(8).collect(),
            b.imported_at.format("%Y-%m-%d %H:%M").to_string(),
            b.imported_by.clone().unwrap_or_else(|| "-".into()),
            b.blobs.to_string(),
            b.rejected.to_string(),
        ]);
    }
    println!("{table}");
    println!("{} bundle(s)", items.len());
    if items.is_empty() {
        println!(
            "nothing has been imported. On an air-gapped instance that means it \
             holds only what it was seeded with before the gap."
        );
    }
    Ok(())
}

/// The miss log (RFC 0008 §6.3).
///
/// `Requested` and `Held` together are the next plan's diff (RFC 0008-bis
/// §4.4): not "left-pad is missing" but "1.2.0 was asked for; 1.3.0 is held".
/// Absent when the request named no version — a listing — or the instance held
/// nothing.
async fn handle_air_gap_missing(
    q: MissingQuery,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    let resp = client.air_gap_missing(q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header([
        "Registry",
        "Kind",
        "Key",
        "Requested",
        "Held",
        "Asked",
        "Last seen",
    ]);
    for m in &resp.items {
        table.add_row([
            m.registry.clone(),
            m.kind.clone(),
            m.storage_key.clone(),
            m.requested_version
                .clone()
                .unwrap_or_else(|| "—".to_owned()),
            held_versions_cell(&m.held_versions),
            m.count.to_string(),
            m.last_seen.format("%Y-%m-%d %H:%M").to_string(),
        ]);
    }
    println!("{table}");
    println!("{} of {} row(s)", resp.items.len(), resp.total);
    if !resp.air_gapped {
        println!(
            "this instance is not air-gapped, so a miss is fetched rather than \
             recorded: an empty list here is not the same as nothing missing."
        );
    }
    Ok(())
}

/// The `Held` cell: at most four versions, then a count of the rest.
fn held_versions_cell(held: &[String]) -> String {
    match held.len() {
        0 => "—".to_owned(),
        n if n > 4 => format!("{} (+{})", held[..4].join(", "), n - 4),
        _ => held.join(", "),
    }
}

async fn handle_stats(client: &BatleHubClient, json: bool) -> Result<()> {
    let resp = client.admin_stats().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        print_stats(&resp);
    }
    Ok(())
}

async fn handle_health(client: &BatleHubClient, json: bool) -> Result<()> {
    let resp = client.registry_health().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        print_health_table(&resp);
    }
    Ok(())
}

/// An export goes to the named file, or to stdout when none was named.
fn write_or_print(output: Option<&str>, text: &str) -> Result<()> {
    match output {
        Some(path) => {
            std::fs::write(path, text)?;
            println!("Exported to {path}");
        }
        None => print!("{text}"),
    }
    Ok(())
}

pub async fn run(cmd: AdminCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        AdminCommand::Quota { cmd } => handle_quota(cmd, client, json).await?,
        AdminCommand::IpBlock { cmd } => handle_ip_block(cmd, client, json).await?,
        AdminCommand::Config { cmd } => handle_config_admin(cmd, client, json).await?,
        AdminCommand::Cache { cmd } => handle_cache(cmd, client).await?,
        AdminCommand::Banner { cmd } => handle_banner(cmd, client).await?,
        AdminCommand::Retention {
            registry,
            reclaim,
            show_kept,
        } => handle_retention(&registry, reclaim, show_kept, client, json).await?,
        AdminCommand::AuditLog {
            registry,
            package,
            action,
            user,
            from,
            to,
            denied_only,
            page,
            per_page,
            purge_before,
        } => {
            handle_audit_log(
                client,
                json,
                AuditLogArgs {
                    registry,
                    package,
                    action,
                    user,
                    from,
                    to,
                    denied_only,
                    page,
                    per_page,
                    purge_before,
                },
            )
            .await?
        }
        AdminCommand::Exposure {
            registry,
            package,
            source,
            min_effect,
            when,
            from,
            to,
            after,
            limit,
        } => {
            handle_exposure(
                ExposureQuery {
                    from,
                    to,
                    registry,
                    package_name: package,
                    source,
                    min_effect,
                    when: when.map(|w| w.replace('-', "_")),
                    after,
                    limit,
                },
                client,
                json,
            )
            .await?
        }
        AdminCommand::Flags { cmd } => handle_flags(cmd, client, json).await?,
        AdminCommand::Bundles => handle_bundles(client, json).await?,
        AdminCommand::AirGapMissing {
            registry,
            kind,
            page,
            per_page,
        } => {
            handle_air_gap_missing(
                MissingQuery {
                    registry,
                    kind,
                    page,
                    per_page,
                },
                client,
                json,
            )
            .await?
        }
        AdminCommand::Stats => handle_stats(client, json).await?,
        AdminCommand::Health => handle_health(client, json).await?,
        AdminCommand::Visibility { cmd } => handle_visibility(cmd, client, json).await?,
        AdminCommand::Grants { cmd } => handle_grants(cmd, client, json).await?,
        AdminCommand::Namespace { cmd } => handle_namespace(cmd, client, json).await?,
        AdminCommand::Users { cmd } => handle_users(cmd, client, json).await?,
        AdminCommand::Sbom { cmd } => handle_sbom(cmd, client, json).await?,
        AdminCommand::Notifications { cmd } => handle_notifications(cmd, client, json).await?,
        AdminCommand::Bulk { cmd } => handle_bulk(cmd, client, json).await?,
        AdminCommand::Deprecate {
            registry,
            name,
            version,
            message,
        } => {
            client
                .deprecate_package(&registry, &name, &version, message.as_deref())
                .await?;
            println!("Deprecated {registry}/{name}@{version}");
        }
        AdminCommand::Undeprecate {
            registry,
            name,
            version,
        } => {
            client
                .undeprecate_package(&registry, &name, &version)
                .await?;
            println!("Undeprecated {registry}/{name}@{version}");
        }
        AdminCommand::Unlist {
            registry,
            name,
            version,
        } => {
            client.unlist_package(&registry, &name, &version).await?;
            println!("Unlisted {registry}/{name}@{version}");
        }
        AdminCommand::Relist {
            registry,
            name,
            version,
        } => {
            client.relist_package(&registry, &name, &version).await?;
            println!("Relisted {registry}/{name}@{version}");
        }
        AdminCommand::AccessCheck {
            registry,
            package,
            version,
            resource,
            user,
            role,
            groups,
        } => {
            let resp = client
                .simulate_access(&SimulateAccessRequest {
                    registry,
                    package_name: package,
                    version,
                    resource_type: resource,
                    user_id: user,
                    role,
                    groups,
                })
                .await?;
            print_access_check(json, &resp)?;
        }
        AdminCommand::ExportAuditLog {
            from,
            to,
            registry,
            action,
            format,
            output,
        } => {
            let text = client
                .export_audit_log(
                    registry.as_deref(),
                    from.as_deref(),
                    to.as_deref(),
                    action.as_deref(),
                    &format,
                )
                .await?;
            write_or_print(output.as_deref(), &text)?;
        }
    }
    Ok(())
}

struct AuditLogArgs {
    registry: Option<String>,
    package: Option<String>,
    action: Option<String>,
    user: Option<String>,
    from: Option<String>,
    to: Option<String>,
    denied_only: bool,
    page: u64,
    per_page: u64,
    purge_before: Option<String>,
}

async fn handle_audit_log(client: &BatleHubClient, json: bool, args: AuditLogArgs) -> Result<()> {
    if let Some(before) = args.purge_before {
        let resp = client.purge_audit_log(&before).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&resp)?);
        } else {
            println!(
                "Deleted {} audit-log row(s) older than {before}",
                resp.deleted
            );
        }
        return Ok(());
    }

    let resp = client
        .audit_log(AuditQuery {
            registry: args.registry,
            package_name: args.package,
            action: args.action,
            user_id: args.user,
            from: args.from,
            to: args.to,
            denied_only: if args.denied_only { Some(true) } else { None },
            page: args.page,
            per_page: args.per_page,
        })
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        print_audit_log_table(&resp.items);
    }
    Ok(())
}

fn print_access_check(json: bool, resp: &AccessSimulationResponse) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(resp)?);
        return Ok(());
    }
    if resp.decision == "allow" {
        println!("ALLOW");
    } else {
        let reason = resp.reason.as_deref().unwrap_or("unknown");
        let rule = resp
            .rule_matched
            .as_deref()
            .map(|r| format!("  (rule: {r})"))
            .unwrap_or_default();
        println!("DENY: {reason}{rule}");
    }
    Ok(())
}

async fn handle_quota(cmd: QuotaCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        QuotaCommand::List { registry } => {
            let entries = client.list_quota(registry.as_deref()).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                let mut table = Table::new();
                table.set_header(["Registry", "User", "Storage (bytes)", "Packages"]);
                for e in &entries {
                    table.add_row([
                        &e.registry,
                        &e.user_id,
                        &e.storage_bytes.to_string(),
                        &e.package_count.to_string(),
                    ]);
                }
                println!("{table}");
            }
        }
        QuotaCommand::Reset { registry, user } => {
            client.reset_quota(&registry, &user).await?;
            println!("Reset quota for {user} in {registry}");
        }
    }
    Ok(())
}

async fn handle_ip_block(cmd: IpBlockCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        IpBlockCommand::List => {
            let blocks = client.list_ip_blocks().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&blocks)?);
            } else {
                let mut table = Table::new();
                table.set_header(["IP", "Reason", "Blocked At"]);
                for b in &blocks {
                    table.add_row([b.ip.as_str(), b.reason.as_str(), &b.blocked_at.to_string()]);
                }
                println!("{table}");
                println!("{} block(s)", blocks.len());
            }
        }
        IpBlockCommand::Add { ip, reason } => {
            client.add_ip_block(&ip, reason.as_deref()).await?;
            println!("Blocked {ip}");
        }
        IpBlockCommand::Remove { ip } => {
            client.remove_ip_block(&ip).await?;
            println!("Unblocked {ip}");
        }
    }
    Ok(())
}

async fn handle_config_admin(
    cmd: ConfigAdminCommand,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    match cmd {
        ConfigAdminCommand::Reload => {
            client.config_reload().await?;
            println!("Config reload triggered.");
        }
        ConfigAdminCommand::Changes => {
            let changes = client.config_changes().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&changes)?);
            } else {
                print_config_changes_table(&changes);
            }
        }
        ConfigAdminCommand::View => {
            let resp = client.config_content().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("{}", resp.content);
            }
        }
        ConfigAdminCommand::Validate { file } => {
            let content = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("could not read {file}: {e}"))?;
            let resp = client.config_validate(&content).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("{file}: valid");
            }
        }
        ConfigAdminCommand::FromFile { file } => {
            let content = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("could not read {file}: {e}"))?;
            let resp = client.config_from_content(&content).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("Config applied from {file}");
            }
        }
    }
    Ok(())
}

fn print_config_changes_table(changes: &[crate::api::admin::ConfigChangeEntry]) {
    let mut table = Table::new();
    table.set_header(["Applied At", "Status", "Triggered By", "Summary"]);
    for c in changes {
        table.add_row([
            c.applied_at.as_deref().unwrap_or("-"),
            c.status.as_deref().unwrap_or("-"),
            c.triggered_by.as_deref().unwrap_or("-"),
            c.summary.as_deref().unwrap_or("-"),
        ]);
    }
    println!("{table}");
}

async fn handle_cache(cmd: CacheCommand, client: &BatleHubClient) -> Result<()> {
    match cmd {
        CacheCommand::Warm {
            registry,
            packages,
            paths,
        } => {
            let split = |s: Option<String>| -> Vec<String> {
                s.unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            };
            let pkgs = split(packages);
            let pths = split(paths);
            if pkgs.is_empty() && pths.is_empty() {
                anyhow::bail!("specify --packages and/or --paths");
            }
            client.cache_warm(&registry, pkgs, pths).await?;
            println!("Cache warming started for {registry}");
        }
        CacheCommand::Clear { registry } => {
            client.cache_clear(&registry).await?;
            println!("Cache cleared for {registry}");
        }
        CacheCommand::Evict { registry, dry_run } => {
            let report = client.evict_registry(&registry, dry_run).await?;
            print_eviction_report(&registry, &report);
        }
        CacheCommand::Coherence { registry, dry_run } => {
            let report = client.coherence_sweep(&registry, dry_run).await?;
            print_coherence_report(&registry, &report);
        }
    }
    Ok(())
}

/// The coherence report: what the two passes found, and what each list means.
///
/// The two lists are printed separately because they answer different
/// questions — `deleted` is what went, `first seen` is what a *second* run
/// would take — and collapsing them would make the two-pass grace invisible,
/// which is how an operator concludes the sweep does nothing.
fn print_coherence_report(registry: &str, report: &CoherenceReportDto) {
    let mode = if report.dry_run {
        "dry run — nothing was deleted, nothing was armed"
    } else {
        "swept"
    };
    println!("Cache coherence on {registry}: {mode}");
    println!(
        "  {} blobs in storage, {} meta rows",
        report.storage_keys, report.meta_rows
    );

    let verb = if report.dry_run {
        "would delete"
    } else {
        "deleted"
    };
    println!("  {verb} {}", report.orphaned_deleted);
    for key in &report.deleted_keys {
        println!("    {key}");
    }

    if report.first_seen_orphaned > 0 {
        println!(
            "  {} newly orphaned, deferred to the next sweep:",
            report.first_seen_orphaned
        );
        for key in &report.first_seen_keys {
            println!("    {key}");
        }
        println!();
        println!(
            "  Nothing is deleted the first time it looks orphaned — a cache write \n  \
             in flight looks exactly like one. Re-run to collect them."
        );
    }
    if report.keys_truncated > 0 {
        println!();
        println!(
            "  … and {} more not listed — the lists above are a sample.",
            report.keys_truncated
        );
    }
}

/// The eviction report: what went, per strategy, and the keys.
///
/// The key list is what an operator reads before running a new size cap live —
/// a count alone cannot be checked against the policy that produced it.
fn print_eviction_report(registry: &str, report: &EvictionReportDto) {
    let mode = if report.dry_run {
        "dry run — nothing was evicted"
    } else {
        "evicted"
    };
    println!("Cache eviction on {registry}: {mode}");
    println!(
        "  total {}   ttl {}   idle {}   keep_latest_n {}   lru {}",
        report.total,
        report.evicted_ttl,
        report.evicted_idle,
        report.evicted_old_versions,
        report.evicted_lru
    );
    if !report.evicted_keys.is_empty() {
        println!();
        println!(
            "{}:",
            if report.dry_run {
                "would evict"
            } else {
                "evicted"
            }
        );
        for key in &report.evicted_keys {
            println!("  {key}");
        }
    }
    if report.keys_truncated > 0 {
        println!();
        println!(
            "  … and {} more not listed — the list above is a sample, not the answer.",
            report.keys_truncated
        );
    }
    if let Some(reason) = &report.incomplete_because {
        println!();
        println!("  {reason}");
    }
}

/// Run retention and print the report.
///
/// `--reclaim` is the client's half of a two-key interlock: the server also has
/// to be configured with `dry_run = false`. Without the flag this always sends
/// `?dry_run=true`, and because the server treats that query as "only ever more
/// conservative", a live registry previews rather than reclaims. Retention
/// destroys the only copy of a locally published artifact — an operator should
/// have to say so twice, in two places, once of them a config file someone
/// reviewed.
async fn handle_retention(
    registry: &str,
    reclaim: bool,
    show_kept: bool,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    let report = client.run_retention(registry, !reclaim).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let mode = if report.dry_run {
        "dry run — nothing was changed"
    } else {
        "LIVE — versions were reclaimed"
    };
    println!("Retention on {registry}: {mode}");
    println!(
        "  examined {}   kept {}   reclaimed {}",
        report.examined, report.kept, report.reclaimed
    );

    print_retention_lists(&report, show_kept);
    print_retention_notes(&report);
    Ok(())
}

/// The per-version detail of a retention report: what was (or would be)
/// reclaimed, and optionally what survived and why.
fn print_retention_lists(report: &RetentionReport, show_kept: bool) {
    if !report.reclaimed_coordinates.is_empty() {
        let verb = if report.dry_run {
            "would reclaim"
        } else {
            "reclaimed"
        };
        println!("\n{verb}:");
        for coord in &report.reclaimed_coordinates {
            println!("  {coord}");
        }
    }

    if show_kept {
        println!("\nkept:");
        for d in report.decisions.iter().filter(|d| d.kept_because.is_some()) {
            let reason = d.kept_because.as_deref().unwrap_or("");
            println!("  {}@{}  ({reason})", d.name, d.version);
        }
    }
}

/// The three footnotes that qualify the counts above them — each one exists
/// because reading the report without it leads an operator to the wrong action.
fn print_retention_notes(report: &RetentionReport) {
    // Never silent about a bounded list: a truncated report read as complete is
    // how an operator approves a reclamation they have not actually seen.
    if report.decisions_truncated > 0 {
        println!(
            "\n({} more per-version decisions not shown; the reclaim list above is complete)",
            report.decisions_truncated
        );
    }
    if report.dry_run && report.reclaimed > 0 {
        println!(
            "\nNothing was deleted. To reclaim: set dry_run = false on this registry's \
             [registries.retention] block, then re-run with --reclaim."
        );
    }
    // Last, and unmissable: the counts above describe a run that stopped, and
    // reading them as a completed sweep is how the rest of the estate goes
    // unreclaimed without anyone noticing.
    if let Some(reason) = &report.incomplete_because {
        println!("\nRUN INCOMPLETE: {reason}");
    }
}

async fn handle_banner(cmd: BannerCommand, client: &BatleHubClient) -> Result<()> {
    match cmd {
        BannerCommand::Set { message, level } => {
            client.set_banner(&message, &level).await?;
            println!("Banner set ({level})");
        }
        BannerCommand::Clear => {
            client.clear_banner().await?;
            println!("Banner cleared");
        }
    }
    Ok(())
}

async fn handle_visibility(
    cmd: VisibilityCommand,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    match cmd {
        VisibilityCommand::Get { registry, name } => {
            let resp = client.get_visibility(&registry, &name).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("{registry}/{name}: {}", resp.visibility);
            }
        }
        VisibilityCommand::Set {
            registry,
            name,
            visibility,
        } => {
            client.set_visibility(&registry, &name, &visibility).await?;
            println!("Set {registry}/{name} visibility to {visibility}");
        }
    }
    Ok(())
}

/// Split `name@version` into its two halves, or `name` alone.
///
/// The last `@` wins, matching `version_node_key`'s own construction: a scoped
/// npm name (`@acme/billing`) starts with one, so splitting on the first would
/// make every scoped package look like a version node named `acme/billing`.
fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.rfind('@') {
        // Position 0 is a scope marker, not a separator.
        Some(i) if i > 0 => (&target[..i], Some(&target[i + 1..])),
        _ => (target, None),
    }
}

async fn handle_grants(cmd: GrantsCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        GrantsCommand::List { registry, target } => {
            let (package, version) = split_target(&target);
            let resp = client.list_grants(&registry, package, version).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else if resp.grants.is_empty() {
                println!("No grants written on {registry}/{target}");
            } else {
                let mut table = Table::new();
                table.set_header(["Node", "Subject", "Actions", "Source"]);
                for g in &resp.grants {
                    table.add_row([
                        &format!("{}:{}", g.node_kind, g.node_key),
                        &g.subject,
                        &g.actions.join(", "),
                        &if g.from_ownership {
                            "ownership".to_owned()
                        } else {
                            g.granted_by.clone().unwrap_or_else(|| "-".to_owned())
                        },
                    ]);
                }
                println!("{table}");
            }
        }
        GrantsCommand::Set {
            registry,
            target,
            subject,
            actions,
        } => {
            let (package, version) = split_target(&target);
            let resp = client
                .put_grant(&registry, package, version, &subject, &actions)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                // What was *stored*, not what was asked for: `releases:*` names
                // one verb and stores several, and an operator who cannot see
                // the difference cannot review it.
                println!(
                    "Granted {} on {registry}/{target} to {subject}",
                    resp.actions.join(", ")
                );
                for w in &resp.warnings {
                    println!("  warning: {w}");
                }
            }
        }
        GrantsCommand::Rm {
            registry,
            target,
            subject,
        } => {
            let (package, version) = split_target(&target);
            let resp = client
                .delete_grant(&registry, package, version, &subject)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else if resp.removed {
                println!("Removed {subject}'s grant on {registry}/{target}");
            } else {
                println!("{subject} had no grant on {registry}/{target}");
            }
        }
    }
    Ok(())
}

async fn handle_namespace(
    cmd: NamespaceCommand,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    match cmd {
        NamespaceCommand::List { registry } => {
            let resp = client.list_namespaces(&registry).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_namespaces_table(&resp);
            }
        }
        NamespaceCommand::Claim {
            registry,
            prefix,
            group_id,
        } => {
            client
                .claim_namespace(&registry, &prefix, &group_id)
                .await?;
            println!("Claimed namespace prefix '{prefix}' for group '{group_id}' in {registry}");
        }
        NamespaceCommand::Release { registry, prefix } => {
            client.release_namespace(&registry, &prefix).await?;
            println!("Released namespace prefix '{prefix}' from {registry}");
        }
    }
    Ok(())
}

async fn handle_users(cmd: UsersCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        UsersCommand::ListBlocked => {
            let resp = client.list_blocked_users().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_blocked_users_table(&resp);
            }
        }
        UsersCommand::Block { user_id, reason } => {
            client.block_user(&user_id, reason.as_deref()).await?;
            println!("Blocked user {user_id}");
        }
        UsersCommand::Unblock { user_id } => {
            client.unblock_user(&user_id).await?;
            println!("Unblocked user {user_id}");
        }
    }
    Ok(())
}

async fn handle_sbom(cmd: SbomCommand, client: &BatleHubClient, _json: bool) -> Result<()> {
    match cmd {
        SbomCommand::Get {
            registry,
            name,
            version,
            format,
        } => {
            let resp = client.get_sbom(&registry, &name, &version, &format).await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        SbomCommand::Export {
            registry,
            from,
            to,
            format,
            output,
        } => {
            let resp = client
                .export_sbom(registry.as_deref(), from.as_deref(), to.as_deref(), &format)
                .await?;
            let content = serde_json::to_string_pretty(&resp)?;
            if let Some(path) = output {
                std::fs::write(&path, &content)?;
                println!("SBOM exported to {path}");
            } else {
                println!("{content}");
            }
        }
    }
    Ok(())
}

async fn handle_notifications(
    cmd: NotificationsCommand,
    client: &BatleHubClient,
    json: bool,
) -> Result<()> {
    match cmd {
        NotificationsCommand::Channels => {
            let resp = client.list_notification_channels().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_notification_channels_table(&resp);
            }
        }
        NotificationsCommand::List => {
            let resp = client.list_notification_subscriptions().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_notification_subscriptions_table(&resp);
            }
        }
        NotificationsCommand::Delete { id } => {
            client.delete_notification_subscription(&id).await?;
            println!("Deleted subscription {id}");
        }
    }
    Ok(())
}

async fn handle_bulk(cmd: BulkCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        BulkCommand::Yank { registry, packages } => {
            let pkgs = packages
                .iter()
                .map(|s| parse_pkg_version(s))
                .collect::<Result<Vec<_>>>()?;
            let resp = client.bulk_yank(&registry, pkgs).await?;
            print_bulk_result(json, &resp)?;
        }
        BulkCommand::Unyank { registry, packages } => {
            let pkgs = packages
                .iter()
                .map(|s| parse_pkg_version(s))
                .collect::<Result<Vec<_>>>()?;
            let resp = client.bulk_unyank(&registry, pkgs).await?;
            print_bulk_result(json, &resp)?;
        }
        BulkCommand::Delete { registry, packages } => {
            let pkgs = packages
                .iter()
                .map(|s| parse_pkg_version(s))
                .collect::<Result<Vec<_>>>()?;
            let resp = client.bulk_delete(&registry, pkgs).await?;
            print_bulk_result(json, &resp)?;
        }
    }
    Ok(())
}

fn print_bulk_result(json: bool, resp: &BulkPackageResult) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(resp)?);
    } else {
        println!(
            "processed={} succeeded={} failed={}",
            resp.processed,
            resp.succeeded,
            resp.failed.len()
        );
        for f in &resp.failed {
            println!("  FAILED {}@{}: {}", f.name, f.version, f.error);
        }
    }
    Ok(())
}

fn fmt_hit_rate(rate: Option<f64>) -> String {
    match rate {
        Some(r) => format!("{:.1}%", r * 100.0),
        None => "-".to_string(),
    }
}

fn fmt_opt_bytes(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) => b.to_string(),
        None => "-".to_string(),
    }
}

fn print_stats(resp: &StatsResponse) {
    println!("Since startup: {}", resp.since_startup);
    println!(
        "Aggregate: hits={} misses={} hit_rate={} cached_bytes={}",
        resp.aggregate.artifact_hits,
        resp.aggregate.artifact_misses,
        fmt_hit_rate(resp.aggregate.hit_rate),
        resp.aggregate.cached_bytes
    );
    let mut table = Table::new();
    table.set_header(["Registry", "Hits", "Misses", "Hit Rate", "Cached Bytes"]);
    for r in &resp.per_registry {
        table.add_row([
            r.registry.clone(),
            r.artifact_hits.to_string(),
            r.artifact_misses.to_string(),
            fmt_hit_rate(r.hit_rate),
            fmt_opt_bytes(r.cached_bytes),
        ]);
    }
    println!("{table}");
}

fn print_health_table(entries: &[RegistryHealthEntry]) {
    if entries.is_empty() {
        println!("(no registries)");
        return;
    }
    let mut table = Table::new();
    table.set_header([
        "Registry",
        "Type",
        "Packages",
        "Cached Artifacts",
        "Pulls (1h)",
        "Pulls (24h)",
        "Recent Errors",
    ]);
    for e in entries {
        table.add_row([
            e.registry.clone(),
            e.registry_type.clone(),
            e.package_count.to_string(),
            e.cached_artifact_count.to_string(),
            e.pulls_last_hour.to_string(),
            e.pulls_last_day.to_string(),
            e.recent_errors.len().to_string(),
        ]);
    }
    println!("{table}");
}

fn print_namespaces_table(entries: &[TeamNamespaceEntry]) {
    if entries.is_empty() {
        println!("(no namespaces)");
        return;
    }
    let mut table = Table::new();
    table.set_header(["Registry", "Prefix", "Group", "Claimed By"]);
    for e in entries {
        table.add_row([
            e.registry.as_str(),
            e.prefix.as_str(),
            e.group_id.as_str(),
            e.claimed_by.as_deref().unwrap_or("-"),
        ]);
    }
    println!("{table}");
}

fn print_blocked_users_table(entries: &[BlockedUserEntry]) {
    if entries.is_empty() {
        println!("(no blocked users)");
        return;
    }
    let mut table = Table::new();
    table.set_header(["User", "Blocked At", "Blocked By", "Reason"]);
    for e in entries {
        table.add_row([
            e.user_id.as_str(),
            &e.blocked_at.to_rfc3339(),
            e.blocked_by.as_str(),
            e.reason.as_deref().unwrap_or("-"),
        ]);
    }
    println!("{table}");
}

fn print_notification_channels_table(entries: &[NotificationChannelEntry]) {
    if entries.is_empty() {
        println!("(no notification channels configured)");
        return;
    }
    let mut table = Table::new();
    table.set_header(["Channel"]);
    for e in entries {
        table.add_row([e.name.as_str()]);
    }
    println!("{table}");
}

fn print_notification_subscriptions_table(entries: &[NotificationSubscriptionEntry]) {
    if entries.is_empty() {
        println!("(no subscriptions)");
        return;
    }
    let mut table = Table::new();
    table.set_header(["ID", "Registry", "Package", "Events", "Channel", "Enabled"]);
    for e in entries {
        let events = e
            .event_types
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(",");
        table.add_row([
            e.id.to_string(),
            e.registry.clone().unwrap_or_else(|| "*".to_string()),
            e.package_name.clone().unwrap_or_else(|| "*".to_string()),
            events,
            e.channel_name.clone(),
            e.enabled.to_string(),
        ]);
    }
    println!("{table}");
}

fn print_flags(resp: &FlagsResponse) {
    let mut table = Table::new();
    table.set_header([
        "Source", "Id", "Registry", "Package", "Version", "Effect", "Kind", "State", "Summary",
    ]);
    for f in &resp.items {
        let state = if f.revoked_at.is_some() {
            "revoked".to_owned()
        } else if let Some(exp) = f.expires_at {
            format!("expires {}", exp.format("%Y-%m-%d"))
        } else {
            "live".to_owned()
        };
        table.add_row([
            f.source.as_str(),
            f.external_id.as_str(),
            f.registry.as_str(),
            f.package_name.as_str(),
            f.version.as_str(),
            f.effect.as_str(),
            f.kind.as_str(),
            state.as_str(),
            f.summary.as_str(),
        ]);
    }
    println!("{table}");
    println!(
        "{} of {} flag(s), page {}",
        resp.items.len(),
        resp.total,
        resp.page
    );
}

fn print_exposure(resp: &ExposureResponse) {
    let mut table = Table::new();
    table.set_header([
        "Consumer",
        "Registry",
        "Package",
        "Version",
        "Flag",
        "Effect",
        "Pulls",
        "Before flag",
        "Last pull",
    ]);
    for r in &resp.rows {
        table.add_row([
            r.consumer.clone(),
            r.registry.clone(),
            r.package_name.clone(),
            r.version.clone(),
            format!("{}:{}", r.source, r.external_id),
            r.effect.clone(),
            r.pulls.to_string(),
            r.pulls_before_flag.to_string(),
            r.last_pull.format("%Y-%m-%d %H:%M").to_string(),
        ]);
    }
    println!("{table}");
    println!("{} row(s)", resp.rows.len());
    if let Some(next) = &resp.next {
        println!("more follow: --after {next}");
    }
    let c = &resp.coverage;
    println!(
        "coverage: {} registr{}, {} with an SBOM extractor, {} with a security profile",
        c.registries_total,
        if c.registries_total == 1 { "y" } else { "ies" },
        c.sbom_configured,
        c.security_profiles
    );
    if c.last_scan.is_empty() {
        println!("  CVE scan: never recorded a pass — the report knows only what was pushed");
    }
    for s in &c.last_scan {
        println!(
            "  {}: scanned {} at {} ({} finding(s), {} error(s))",
            s.registry,
            s.artifacts_scanned,
            s.last_scan_at.format("%Y-%m-%d %H:%M"),
            s.findings,
            s.errors
        );
    }
    for s in &c.flag_sources {
        println!(
            "  source {}: {} live flag(s), last push {}",
            s.source,
            s.live_flags,
            s.last_push_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "never".into())
        );
    }
}

fn print_audit_log_table(entries: &[AuditEntry]) {
    let mut table = Table::new();
    table.set_header(["Time", "Registry", "User", "Action", "Package", "Denied"]);
    for e in entries {
        let registry = e
            .package_id
            .as_ref()
            .map(|p| p.registry.as_str())
            .unwrap_or("-");
        let package = e
            .package_id
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("-");
        let denied = e.result.as_ref().is_some_and(|r| r.is_denied());
        table.add_row([
            e.timestamp.as_deref().unwrap_or("-"),
            registry,
            e.user_id.as_deref().unwrap_or("(anon)"),
            e.action.as_deref().unwrap_or("-"),
            package,
            if denied { "yes" } else { "no" },
        ]);
    }
    println!("{table}");
    println!("{} entry/entries", entries.len());
}

#[cfg(test)]
mod grants_target_tests {
    use super::split_target;

    /// A scoped npm name starts with `@`. Splitting on the *first* one would
    /// read `@acme/billing` as a version node keyed `acme/billing`, which is a
    /// grant written on a coordinate nobody can reach.
    #[test]
    fn a_scoped_name_is_not_a_version_node() {
        assert_eq!(split_target("@acme/billing"), ("@acme/billing", None));
    }

    #[test]
    fn a_version_suffix_splits_on_the_last_at() {
        assert_eq!(
            split_target("@acme/billing@2.4.0-rc.1"),
            ("@acme/billing", Some("2.4.0-rc.1"))
        );
        assert_eq!(split_target("rails@7.1.0"), ("rails", Some("7.1.0")));
    }

    #[test]
    fn a_bare_name_is_the_package_node() {
        assert_eq!(split_target("rails"), ("rails", None));
    }

    /// Round-trips with `version_node_key`, which is the point of taking the
    /// coordinate in this spelling at all.
    #[test]
    fn the_spelling_matches_the_storage_key() {
        let (p, v) = split_target("@acme/billing@2.4.0-rc.1");
        assert_eq!(
            batlehub_core::ports::version_node_key(p, v.unwrap()),
            "@acme/billing@2.4.0-rc.1"
        );
    }
}
