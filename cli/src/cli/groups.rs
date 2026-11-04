//! The groups `admin` dissolved into (RFC 0040 §4.4): one noun per object,
//! verbs from the closed set. Each command is a thin spelling over the
//! `AdminCommand` handler that already did the work, so the old
//! `admin …` path — a hidden alias for one minor — runs the same body.

use anyhow::Result;
use clap::Subcommand;

use super::admin::{
    self, AdminCommand, AuditTrailCommand, BannerCommand, CacheCommand, ConfigAdminCommand,
    FlagsCommand, GdprCommand, GrantsCommand, IpBlockCommand, NamespaceCommand,
    NotificationsCommand, QuotaCommand, SbomCommand, UsersCommand, VisibilityCommand,
};
use super::journeys::{self, PendingCmd};
use super::security::{self, PullsArgs};
use crate::api::BatleHubClient;
use crate::coordinate::Coordinate;
use crate::render::{ctx, deprecated};

/// A coordinate, resolved against the default registry.
async fn coord(client: &BatleHubClient, raw: &str) -> Result<Coordinate> {
    Ok(Coordinate::resolve(client, raw, ctx().registry.as_deref()).await?)
}

/// `name` or `name@version`: how the grants API spells a node.
fn target(c: &Coordinate) -> String {
    match &c.version {
        Some(v) => format!("{}@{v}", c.name),
        None => c.name.clone(),
    }
}

fn registry() -> Result<String> {
    Ok(ctx().require_registry(None)?)
}

// ── quota · ip-block · user ──────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum QuotaCmd {
    /// Quota usage, of every registry or of `-r`'s [admin]
    List,
    /// Reset a user's quota in the `-r` registry [admin]
    Reset { user: String },
}

#[derive(Subcommand)]
pub enum UserCmd {
    /// The blocked users [admin]
    List,
    /// Block a user (destructive: asks first) [admin]
    Block {
        user_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Unblock a user [admin]
    Unblock { user_id: String },
}

// ── server ───────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum ServerCmd {
    /// The server's configuration [admin]
    Config {
        #[command(subcommand)]
        cmd: ServerConfigCmd,
    },
    /// Aggregate statistics: cache hit rate, bytes served, … [admin]
    Stats,
    /// Per-registry and backend health [admin]
    Health,
}

#[derive(Subcommand)]
pub enum ServerConfigCmd {
    /// Reload the configuration now [admin]
    Reload,
    /// Recent configuration changes [admin]
    Changes,
    /// The active configuration, as TOML [admin]
    Show,
    /// Validate a local TOML file against the server [admin]
    Validate { file: String },
    /// Stage a local TOML file as the pending configuration [admin]
    FromFile { file: String },
    /// The staged configuration a reload is waiting on, and what it changes [admin]
    Pending,
    /// Apply the staged configuration (destructive: asks first) [admin]
    Apply,
    /// Drop the staged configuration (destructive: asks first) [admin]
    Discard,
    /// What the running configuration's validation warned about [admin]
    Warnings,
}

// ── cache · import · retention · banner ──────────────────────────────────────

#[derive(Subcommand)]
pub enum CacheCmd {
    /// Pre-warm the `-r` registry's cache [admin]
    Warm {
        /// Comma-separated package names
        #[arg(long)]
        packages: Option<String>,
        /// Comma-separated upstream paths, for path-addressed registries
        #[arg(long)]
        paths: Option<String>,
    },
    /// Delete the `-r` registry's metadata cache (destructive: asks first) [admin]
    Delete,
    /// Run the `-r` registry's eviction strategies (destructive unless --dry-run) [admin]
    Evict {
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete blobs nothing references, in two passes (destructive unless --dry-run) [admin]
    Coherence {
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum RetentionCmd {
    /// What retention would reclaim in a registry; changes nothing [admin]
    Show {
        registry: String,
        /// Also print every kept version and why
        #[arg(long)]
        show_kept: bool,
    },
    /// Reclaim what the policy selects (destructive: asks first; the
    /// registry also needs `dry_run = false`) [admin]
    Apply {
        registry: String,
        #[arg(long)]
        show_kept: bool,
        /// Report as `show` does, and reclaim nothing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum BannerCmd {
    /// Set the global banner [admin]
    Set {
        message: String,
        #[arg(long, default_value = "info")]
        level: String,
    },
    /// Delete the global banner [admin]
    Delete,
}

// ── exposure · bundle · air-gap · flag ───────────────────────────────────────

#[derive(Subcommand)]
pub enum ExposureCmd {
    /// Who pulled a flagged version, one row per consumer (RFC 0002) [admin]
    List {
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
}

#[derive(Subcommand)]
pub enum BundleCmd {
    /// The bundles this instance imported (RFC 0008) [admin]
    List,
}

#[derive(Subcommand)]
pub enum AirGapCmd {
    /// What this instance was asked for and did not hold (RFC 0008) [admin]
    Missing {
        /// artifact, document, checksum, ref or unmirrored_host
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 0)]
        page: u64,
        #[arg(long, default_value_t = 100)]
        per_page: u64,
    },
}

// ── audit · gdpr · sbom · notification ───────────────────────────────────────

#[derive(Subcommand)]
pub enum AuditCmd {
    /// Query the access audit log [admin]
    Log {
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
        /// Only denied requests
        #[arg(long)]
        denied_only: bool,
        #[arg(long, default_value = "0")]
        page: u64,
        #[arg(long, default_value = "50")]
        per_page: u64,
    },
    /// Delete audit-log entries older than an RFC 3339 instant (destructive: asks first) [admin]
    Purge {
        #[arg(long)]
        before: String,
    },
    /// Export a window of the audit log for compliance; `--format csv` for CSV [admin]
    Export {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        /// Only these actions, comma-separated
        #[arg(long)]
        action: Option<String>,
        /// Write to this file instead of stdout
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
    /// Replay the seal chain and every sealed window (RFC 0036) [admin]
    Verify {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        /// The digest of the newest `audit_seal` line your SIEM holds
        #[arg(long)]
        head: Option<String>,
    },
    /// What one identity pulled inside a window
    Pulls(PullsArgs),
}

#[derive(Subcommand)]
pub enum GdprCmd {
    /// Pseudonymise a subject's records (destructive: asks first) [admin]
    Erase {
        #[arg(long)]
        user: String,
        /// Erase even while an open decision names the subject
        #[arg(long)]
        ignore_open_decisions: bool,
        #[arg(long, hide = true)]
        force: bool,
    },
    /// Everything held about a subject, as JSON [admin]
    Export {
        #[arg(long)]
        user: String,
    },
}

#[derive(Subcommand)]
pub enum SbomCmd {
    /// The SBOM of one version: `registry/name@version` [admin]
    Show {
        coordinate: String,
        /// The SBOM's document format
        #[arg(long, default_value = "cyclonedx")]
        sbom_format: String,
    },
    /// Export SBOMs of the `-r` registry or a time range [admin]
    Export {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "cyclonedx")]
        sbom_format: String,
        #[arg(long, short = 'o')]
        output: Option<String>,
    },
}

// ── visibility · grant · namespace ───────────────────────────────────────────

#[derive(Subcommand)]
pub enum VisibilityCmd {
    /// A package's visibility: `registry/name`
    Show { coordinate: String },
    /// Set a package's visibility: public, internal or team [admin]
    Set {
        coordinate: String,
        visibility: String,
    },
}

#[derive(Subcommand)]
pub enum GrantCmd {
    /// The grants on a package (`registry/name`) and its versions, or on one
    /// version (`registry/name@version`) [admin]
    List { coordinate: String },
    /// Write a subject's grant on a node, replacing its row [admin]
    Set {
        coordinate: String,
        /// `user:alice`, `group:oidc1:eng`, `role:user`, or `*`
        #[arg(long)]
        subject: String,
        /// Comma-separated verbs or patterns, e.g. `releases:read,releases:list`
        #[arg(long, value_delimiter = ',')]
        actions: Vec<String>,
    },
    /// Remove a subject's grant from a node [admin]
    Remove {
        coordinate: String,
        #[arg(long)]
        subject: String,
    },
}

#[derive(Subcommand)]
pub enum NamespaceCmd {
    /// The claimed namespace prefixes of the `-r` registry
    List,
    /// Claim a prefix for a team group in the `-r` registry [admin]
    Claim { prefix: String, group_id: String },
    /// Remove a claim from the `-r` registry [admin]
    Remove { prefix: String },
    /// The keys a namespace's releases are signed with [admin]
    SigningKey {
        #[command(subcommand)]
        cmd: journeys::SigningKeyCmd,
    },
}

/// Every group above, as `main` dispatches it.
pub enum Group {
    Quota(QuotaCmd),
    IpBlock(IpBlockCommand),
    User(UserCmd),
    Server(ServerCmd),
    Cache(CacheCmd),
    Import {
        registry: String,
        tag: Option<String>,
        repo: Option<String>,
    },
    Retention(RetentionCmd),
    Banner(BannerCmd),
    Exposure(ExposureCmd),
    Bundle(BundleCmd),
    AirGap(AirGapCmd),
    Flag(journeys::FlagCmd),
    /// `flag list`, once `run` has taken the verbs `admin flags` lacks.
    FlagList(FlagsCommand),
    Audit(AuditCmd),
    Gdpr(GdprCmd),
    Sbom(SbomCmd),
    Notification(NotificationsCommand),
    Visibility(VisibilityCmd),
    Grant(GrantCmd),
    Namespace(NamespaceCmd),
    AccessCheck(AccessCheckArgs),
}

#[derive(clap::Args)]
pub struct AccessCheckArgs {
    /// `registry/name@version`
    pub coordinate: String,
    /// The verb to check (e.g. `releases:read`, `source:read`)
    #[arg(long, default_value = "releases:read")]
    pub resource: String,
    /// Simulated user id
    #[arg(long)]
    pub user: Option<String>,
    /// Simulated role: anonymous, user or admin (default: anonymous)
    #[arg(long)]
    pub role: Option<String>,
    /// Simulated OIDC groups (comma-separated or repeated)
    #[arg(long, value_delimiter = ',')]
    pub groups: Vec<String>,
}

pub async fn run(group: Group, client: &BatleHubClient, json: bool) -> Result<()> {
    let group = match group {
        Group::Server(ServerCmd::Config { cmd }) => {
            let pending = match cmd {
                ServerConfigCmd::Pending => PendingCmd::Pending,
                ServerConfigCmd::Apply => PendingCmd::Apply,
                ServerConfigCmd::Discard => PendingCmd::Discard,
                ServerConfigCmd::Warnings => PendingCmd::Warnings,
                cmd => return run_config(cmd, client, json).await,
            };
            return journeys::run_pending(pending, client).await;
        }
        Group::Namespace(NamespaceCmd::SigningKey { cmd }) => {
            return journeys::run_signing_key(cmd, client).await
        }
        Group::Flag(cmd) => match cmd {
            journeys::FlagCmd::Listing(list) => Group::FlagList(list),
            cmd => return journeys::run_flag(cmd, client, json).await,
        },
        other => other,
    };
    let cmd = match group {
        Group::Quota(QuotaCmd::List) => AdminCommand::Quota {
            cmd: QuotaCommand::List,
        },
        Group::Quota(QuotaCmd::Reset { user }) => AdminCommand::Quota {
            cmd: QuotaCommand::Reset {
                registry: registry()?,
                user,
            },
        },
        Group::IpBlock(cmd) => AdminCommand::IpBlock { cmd },
        Group::User(c) => AdminCommand::Users {
            cmd: match c {
                UserCmd::List => UsersCommand::ListBlocked,
                UserCmd::Block { user_id, reason } => UsersCommand::Block { user_id, reason },
                UserCmd::Unblock { user_id } => UsersCommand::Unblock { user_id },
            },
        },
        Group::Server(ServerCmd::Stats) => AdminCommand::Stats,
        Group::Server(ServerCmd::Health) => AdminCommand::Health,
        Group::Server(ServerCmd::Config { cmd }) => return run_config(cmd, client, json).await,
        Group::Cache(c) => AdminCommand::Cache {
            cmd: match c {
                CacheCmd::Warm { packages, paths } => CacheCommand::Warm {
                    registry: registry()?,
                    packages,
                    paths,
                },
                CacheCmd::Delete => CacheCommand::Clear {
                    registry: registry()?,
                },
                CacheCmd::Evict { dry_run } => CacheCommand::Evict {
                    registry: registry()?,
                    dry_run,
                },
                CacheCmd::Coherence { dry_run } => CacheCommand::Coherence {
                    registry: registry()?,
                    dry_run,
                },
            },
        },
        Group::Import {
            registry,
            tag,
            repo,
        } => AdminCommand::Import {
            registry,
            tag,
            repo,
        },
        Group::Retention(RetentionCmd::Show {
            registry,
            show_kept,
        }) => AdminCommand::Retention {
            registry,
            reclaim: false,
            show_kept,
        },
        Group::Retention(RetentionCmd::Apply {
            registry,
            show_kept,
            dry_run,
        }) => AdminCommand::Retention {
            registry,
            reclaim: !dry_run,
            show_kept,
        },
        Group::Banner(BannerCmd::Set { message, level }) => AdminCommand::Banner {
            cmd: BannerCommand::Set { message, level },
        },
        Group::Banner(BannerCmd::Delete) => AdminCommand::Banner {
            cmd: BannerCommand::Clear,
        },
        Group::Exposure(ExposureCmd::List {
            package,
            source,
            min_effect,
            when,
            from,
            to,
            after,
            limit,
        }) => AdminCommand::Exposure {
            package,
            source,
            min_effect,
            when,
            from,
            to,
            after,
            limit,
        },
        Group::Bundle(BundleCmd::List) => AdminCommand::Bundles,
        Group::AirGap(AirGapCmd::Missing {
            kind,
            page,
            per_page,
        }) => AdminCommand::AirGapMissing {
            kind,
            page,
            per_page,
        },
        Group::FlagList(cmd) => AdminCommand::Flags { cmd },
        Group::Flag(_) | Group::Namespace(NamespaceCmd::SigningKey { .. }) => {
            unreachable!("dispatched above")
        }
        Group::Audit(AuditCmd::Pulls(args)) => {
            return security::run_audit(security::AuditCommand::Pulls(args), client, json).await
        }
        Group::Audit(AuditCmd::Log {
            package,
            action,
            user,
            from,
            to,
            denied_only,
            page,
            per_page,
        }) => AdminCommand::AuditLog {
            package,
            action,
            user,
            from,
            to,
            denied_only,
            page,
            per_page,
            purge_before: None,
        },
        Group::Audit(AuditCmd::Purge { before }) => AdminCommand::AuditLog {
            package: None,
            action: None,
            user: None,
            from: None,
            to: None,
            denied_only: false,
            page: 0,
            per_page: 50,
            purge_before: Some(before),
        },
        Group::Audit(AuditCmd::Export {
            from,
            to,
            action,
            output,
        }) => AdminCommand::ExportAuditLog {
            from,
            to,
            action,
            output,
        },
        Group::Audit(AuditCmd::Verify { from, to, head }) => AdminCommand::Audit {
            cmd: AuditTrailCommand::Verify { from, to, head },
        },
        Group::Gdpr(GdprCmd::Erase {
            user,
            ignore_open_decisions,
            force,
        }) => {
            if force {
                deprecated("gdpr erase --force", "gdpr erase --ignore-open-decisions");
            }
            AdminCommand::Gdpr {
                cmd: GdprCommand::Erase {
                    user,
                    force: ignore_open_decisions || force,
                },
            }
        }
        Group::Gdpr(GdprCmd::Export { user }) => AdminCommand::Gdpr {
            cmd: GdprCommand::Export { user },
        },
        Group::Sbom(SbomCmd::Show {
            coordinate,
            sbom_format,
        }) => {
            let (registry, name, version) = coord(client, &coordinate).await?.require_version()?;
            AdminCommand::Sbom {
                cmd: SbomCommand::Get {
                    registry,
                    name,
                    version,
                    sbom_format,
                },
            }
        }
        Group::Sbom(SbomCmd::Export {
            from,
            to,
            sbom_format,
            output,
        }) => AdminCommand::Sbom {
            cmd: SbomCommand::Export {
                from,
                to,
                sbom_format,
                output,
            },
        },
        Group::Notification(cmd) => AdminCommand::Notifications { cmd },
        Group::Visibility(VisibilityCmd::Show { coordinate }) => {
            let c = coord(client, &coordinate).await?;
            AdminCommand::Visibility {
                cmd: VisibilityCommand::Get {
                    registry: c.registry,
                    name: c.name,
                },
            }
        }
        Group::Visibility(VisibilityCmd::Set {
            coordinate,
            visibility,
        }) => {
            let c = coord(client, &coordinate).await?;
            AdminCommand::Visibility {
                cmd: VisibilityCommand::Set {
                    registry: c.registry,
                    name: c.name,
                    visibility,
                },
            }
        }
        Group::Grant(g) => {
            let raw = match &g {
                GrantCmd::List { coordinate }
                | GrantCmd::Set { coordinate, .. }
                | GrantCmd::Remove { coordinate, .. } => coordinate.clone(),
            };
            let c = coord(client, &raw).await?;
            let (registry, target) = (c.registry.clone(), target(&c));
            AdminCommand::Grants {
                cmd: match g {
                    GrantCmd::List { .. } => GrantsCommand::List { registry, target },
                    GrantCmd::Set {
                        subject, actions, ..
                    } => GrantsCommand::Set {
                        registry,
                        target,
                        subject,
                        actions,
                    },
                    GrantCmd::Remove { subject, .. } => GrantsCommand::Rm {
                        registry,
                        target,
                        subject,
                    },
                },
            }
        }
        Group::Namespace(n) => AdminCommand::Namespace {
            cmd: match n {
                NamespaceCmd::List => NamespaceCommand::List {
                    registry: registry()?,
                },
                NamespaceCmd::Claim { prefix, group_id } => NamespaceCommand::Claim {
                    registry: registry()?,
                    prefix,
                    group_id,
                },
                NamespaceCmd::Remove { prefix } => NamespaceCommand::Release {
                    registry: registry()?,
                    prefix,
                },
                NamespaceCmd::SigningKey { .. } => unreachable!("dispatched above"),
            },
        },
        Group::AccessCheck(a) => {
            let (registry, package, version) =
                coord(client, &a.coordinate).await?.require_version()?;
            AdminCommand::AccessCheck {
                package,
                version,
                resource: a.resource,
                user: a.user,
                role: a.role,
                groups: a.groups,
                registry: Some(registry),
            }
        }
    };
    admin::run(cmd, client, json).await
}

/// `server config <verb>` for the verbs `admin config` already had.
async fn run_config(cmd: ServerConfigCmd, client: &BatleHubClient, json: bool) -> Result<()> {
    let cmd = match cmd {
        ServerConfigCmd::Reload => ConfigAdminCommand::Reload,
        ServerConfigCmd::Changes => ConfigAdminCommand::Changes,
        ServerConfigCmd::Show => ConfigAdminCommand::View,
        ServerConfigCmd::Validate { file } => ConfigAdminCommand::Validate { file },
        ServerConfigCmd::FromFile { file } => ConfigAdminCommand::FromFile { file },
        ServerConfigCmd::Pending
        | ServerConfigCmd::Apply
        | ServerConfigCmd::Discard
        | ServerConfigCmd::Warnings => unreachable!("dispatched by `run`"),
    };
    admin::run(AdminCommand::Config { cmd }, client, json).await
}

/// The new spelling of an `admin …` command, for its deprecation warning.
pub fn admin_replacement(cmd: &AdminCommand) -> (&'static str, &'static str) {
    use AdminCommand as A;
    match cmd {
        A::Quota { cmd } => match cmd {
            QuotaCommand::List => ("admin quota list", "quota list"),
            QuotaCommand::Reset { .. } => ("admin quota reset", "quota reset -r <registry>"),
        },
        A::IpBlock { .. } => ("admin ip-block", "ip-block"),
        A::Config { cmd } => match cmd {
            ConfigAdminCommand::View => ("admin config view", "server config show"),
            _ => ("admin config", "server config"),
        },
        A::Cache { cmd } => match cmd {
            CacheCommand::Clear { .. } => ("admin cache clear", "cache delete -r <registry>"),
            _ => (
                "admin cache <verb> <registry>",
                "cache <verb> -r <registry>",
            ),
        },
        A::Import { .. } => ("admin import", "import"),
        A::Banner { cmd } => match cmd {
            BannerCommand::Clear => ("admin banner clear", "banner delete"),
            BannerCommand::Set { .. } => ("admin banner set", "banner set"),
        },
        A::Retention { reclaim: true, .. } => ("admin retention --reclaim", "retention apply"),
        A::Retention { .. } => ("admin retention", "retention show"),
        A::Exposure { .. } => ("admin exposure", "exposure list"),
        A::Bundles => ("admin bundles", "bundle list"),
        A::AirGapMissing { .. } => ("admin air-gap-missing", "air-gap missing"),
        A::Flags { .. } => ("admin flags list", "flag list"),
        A::AuditLog {
            purge_before: Some(_),
            ..
        } => ("admin audit-log --purge-before", "audit purge --before"),
        A::AuditLog { .. } => ("admin audit-log", "audit log"),
        A::Gdpr { .. } => ("admin gdpr", "gdpr"),
        A::Audit { .. } => ("admin audit verify", "audit verify"),
        A::Stats => ("admin stats", "server stats"),
        A::Health => ("admin health", "server health"),
        A::Visibility { cmd } => match cmd {
            VisibilityCommand::Get { .. } => {
                ("admin visibility get", "visibility show <registry>/<name>")
            }
            VisibilityCommand::Set { .. } => {
                ("admin visibility set", "visibility set <registry>/<name>")
            }
        },
        A::Grants { cmd } => match cmd {
            GrantsCommand::Rm { .. } => ("admin grants rm", "grant remove <registry>/<name>"),
            _ => ("admin grants", "grant <verb> <registry>/<name>[@<version>]"),
        },
        A::Namespace { cmd } => match cmd {
            NamespaceCommand::Release { .. } => ("admin namespace release", "namespace remove"),
            _ => ("admin namespace", "namespace <verb> -r <registry>"),
        },
        A::Users { cmd } => match cmd {
            UsersCommand::ListBlocked => ("admin users list-blocked", "user list"),
            _ => ("admin users", "user"),
        },
        A::Sbom { cmd } => match cmd {
            SbomCommand::Get { .. } => ("admin sbom get", "sbom show <registry>/<name>@<version>"),
            SbomCommand::Export { .. } => ("admin sbom export", "sbom export"),
        },
        A::Notifications { .. } => ("admin notifications", "notification"),
        A::Bulk { .. } => (
            "admin bulk <verb> <registry> <name@version>…",
            "package <verb> <registry>/<name>@<version>…",
        ),
        A::Deprecate { .. } => ("admin deprecate", "package deprecate"),
        A::Undeprecate { .. } => ("admin undeprecate", "package undeprecate"),
        A::Unlist { .. } => ("admin unlist", "package unlist"),
        A::Relist { .. } => ("admin relist", "package relist"),
        A::AccessCheck { .. } => ("admin access-check", "authz check"),
        A::ExportAuditLog { .. } => ("admin export-audit-log", "audit export"),
    }
}
