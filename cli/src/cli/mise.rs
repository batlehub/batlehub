//! `batlehub-cli mise …` — the air gap's connected-side commands (RFC 0008).
//!
//! `plan` turns a `mise.lock` into a bill of materials: every download the
//! locked tools imply, resolved onto the storage key it will occupy, plus
//! the two lists an operator cannot get today — the tools that will not work
//! through a proxy at all, and the hosts nothing mirrors.
//!
//! Planning is offline. It reads the lock and the server's registry list;
//! it resolves nothing over the network, so it works from a laptop with the
//! lock in hand.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::api::mise_plan::{plan_from_path, KnownRegistry, MisePlan};
use crate::api::BatleHubClient;

#[derive(Subcommand)]
pub enum MiseCommand {
    /// Turn a mise.lock into a plan BatleHub can be seeded from and audited
    /// against
    Plan(PlanArgs),
    /// Fetch every planned entry through BatleHub and prove it matches the
    /// lock
    ///
    /// Exits non-zero if any entry is missing or any digest disagrees, so it
    /// is usable as a CI gate on the connected side.
    Seed(SeedArgs),
    /// Carry a bundle in: verify its signature, then write its blobs
    Import(ImportArgs),
    /// Build a signed, content-addressed bundle from a plan
    ///
    /// Each planned entry is fetched through this (connected) BatleHub and
    /// written under its digest, so identical bytes ship once. The manifest
    /// is signed with an ed25519 key; the disconnected side verifies it
    /// before reading a single blob.
    Export(ExportArgs),
}

#[derive(Args)]
pub struct ImportArgs {
    /// The bundle to import.
    pub bundle: PathBuf,
}

#[derive(Args)]
pub struct ExportArgs {
    /// The plan, as `mise plan -o` wrote it.
    #[arg(long, default_value = "mise-plan.json")]
    pub plan: PathBuf,
    /// A file holding the 32-byte ed25519 signing key, hex-encoded.
    #[arg(long)]
    pub sign_key: PathBuf,
    /// Where to write the bundle.
    #[arg(long, short = 'o', default_value = "estate.bhub")]
    pub out: PathBuf,
    /// A name for this bundle. Defaults to the plan's digest, which makes
    /// the same plan produce the same id and an import idempotent.
    #[arg(long)]
    pub bundle_id: Option<String>,
}

#[derive(Args)]
pub struct SeedArgs {
    /// The plan, as `mise plan -o` wrote it.
    #[arg(long, default_value = "mise-plan.json")]
    pub plan: PathBuf,
    /// Also report what the supply-chain layer said about each entry, and
    /// fail on a denied verdict (RFC 0018).
    #[arg(long)]
    pub verify: bool,
}

#[derive(Args)]
pub struct PlanArgs {
    /// The lock file. Defaults to `mise.lock` in the current directory.
    #[arg(long, default_value = "mise.lock")]
    pub lock: PathBuf,
    /// Platforms to plan for, comma-separated (`linux-x64,darwin-arm64`),
    /// or `all` for every platform the lock records.
    ///
    /// The default is the platform of the machine running the command. A
    /// bundle for one estate is usually a bundle for one platform, and
    /// planning every platform a lock happens to record makes it several
    /// times larger than the thing anyone asked for (RFC 0008 §13
    /// decision 1, the same shape as RFC 0010's `warm_platforms`).
    #[arg(long, value_delimiter = ',')]
    pub platform: Vec<String>,
    /// Also plan the mise binary itself, so a bundle carries the tool that
    /// reads the next plan (RFC 0008 §13 decision 2).
    #[arg(long)]
    pub include_mise: bool,
    /// Which mise version `--include-mise` should carry. Defaults to the
    /// version of the `mise` on this PATH.
    #[arg(long, requires = "include_mise")]
    pub mise_version: Option<String>,
    /// Write the plan here instead of standard output.
    #[arg(long, short = 'o')]
    pub out: Option<PathBuf>,
}

pub async fn run(cmd: MiseCommand, client: &BatleHubClient, json: bool) -> Result<()> {
    match cmd {
        MiseCommand::Plan(args) => run_plan(args, client, json).await,
        MiseCommand::Seed(args) => run_seed(args, client, json).await,
        MiseCommand::Export(args) => run_export(args, client, json).await,
        MiseCommand::Import(args) => run_import(args, client, json).await,
    }
}

async fn run_import(args: ImportArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let bytes = std::fs::read(&args.bundle)
        .with_context(|| format!("reading {}", args.bundle.display()))?;
    let resp = client.import_bundle(bytes).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    if resp.already_imported {
        println!(
            "bundle {} was already imported; nothing was written again",
            resp.bundle_id
        );
        return Ok(());
    }
    println!(
        "signature ok ({}) · {} blob(s) · {} rejected",
        &resp.signer_key[..resp.signer_key.len().min(8)],
        resp.imported,
        resp.rejected
    );
    for line in &resp.rejections {
        println!("  rejected: {line}");
    }
    if resp.rejected > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Read the signing key: 32 bytes, hex, one line.
///
/// A file rather than a flag, because a key on a command line is a key in
/// the shell history and in every process listing on the machine.
fn read_signing_key(path: &std::path::Path) -> Result<ed25519_dalek::SigningKey> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading the signing key from {}", path.display()))?;
    let bytes =
        hex::decode(raw.trim()).with_context(|| format!("{} is not hex", path.display()))?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "{} is {} bytes; an ed25519 signing key is 32",
            path.display(),
            bytes.len()
        )
    })?;
    Ok(ed25519_dalek::SigningKey::from_bytes(&arr))
}

async fn run_export(args: ExportArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    use batlehub_core::services::bundle::{BundleEntry, BundleManifest, BUNDLE_VERSION};
    use ed25519_dalek::Signer;

    let text = std::fs::read_to_string(&args.plan)
        .with_context(|| format!("reading {}", args.plan.display()))?;
    let plan: MisePlan = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a plan", args.plan.display()))?;
    let signing = read_signing_key(&args.sign_key)?;

    // Fetch every entry the plan can address, keeping the bytes by digest —
    // that is the dedup: two keys naming the same bytes carry one blob.
    let mut blobs: std::collections::BTreeMap<String, Vec<u8>> = std::collections::BTreeMap::new();
    let mut entries: Vec<BundleEntry> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for entry in &plan.entries {
        let (Some(path), Some(key), Some(registry)) =
            (&entry.proxy_path, &entry.key, &entry.registry)
        else {
            skipped.push(format!("{} [{}]: no mirror", entry.tool, entry.platform));
            continue;
        };
        let fetched = match client.fetch_for_bundle(path).await {
            Ok(b) => b,
            Err(e) => {
                skipped.push(format!("{} [{}]: {e}", entry.tool, entry.platform));
                continue;
            }
        };
        let bytes = fetched.bytes;
        let digest = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&bytes);
            hex::encode(h.finalize())
        };
        if let Some(expected) = &entry.sha256 {
            if !expected.eq_ignore_ascii_case(&digest) {
                // A bundle built from bytes that disagree with the lock
                // would be a bundle nobody can verify on the other side.
                skipped.push(format!(
                    "{} [{}]: the lock says {expected}, the server served {digest}",
                    entry.tool, entry.platform
                ));
                continue;
            }
        }
        // The key the *server* keeps these bytes under, taken off the
        // response rather than derived here. A storage key is a function of
        // the route — the same GitHub asset is `…/{tag}/filename/{file}` by
        // name and `…/unknown/{id}` by id, a generic mirror is `…/repo/_/…`,
        // a forge archive is keyed by its commit — and a bundle whose keys
        // were guessed plants bytes where the disconnected instance's read
        // path never looks. The plan's own key is the fallback, and the line
        // below says when it was used.
        let stored_key = match fetched.storage_key.as_deref() {
            Some(k) => k.strip_prefix("artifact:").unwrap_or(k).to_owned(),
            None => {
                skipped.push(format!(
                    "{} [{}]: the server did not report a storage key; falling back to the \
                     plan's derived key '{key}', which older routes may not read back",
                    entry.tool, entry.platform
                ));
                key.clone()
            }
        };
        // The judgement this instance made about these bytes, if it made one.
        // The headers the fetch already carried settle the `warned` and held
        // cases; this settles `allowed`, which says nothing on the wire.
        let carried = match (&fetched.package_name, &fetched.version) {
            (Some(name), Some(version)) => client
                .get_verdict(&registry.name, name, version)
                .await
                .unwrap_or(None),
            _ => None,
        };
        entries.push(BundleEntry {
            registry: registry.name.clone(),
            key: stored_key,
            size: bytes.len() as u64,
            digest: digest.clone(),
            package_name: fetched.package_name.clone(),
            version: fetched
                .version
                .clone()
                .or_else(|| Some(entry.version.clone())),
            // RFC 0008 §13 decision 4: the verdict crosses the gap with the
            // bytes. Without it every imported artifact is `SCAN_PENDING` on
            // an instance with `[security]`, and fail-closed means the
            // bundle it just accepted serves nothing.
            //
            // Asked for rather than read off the response, because the
            // response only carries the RFC 0018 headers when there is
            // something to *say* — a hold or a warning. An `allowed` verdict
            // is silent on the wire, and silence is the case that has to
            // cross: it is the one that lets the disconnected instance serve.
            verdict: carried.as_ref().map(|v| v.state.clone()),
            reason_codes: carried
                .as_ref()
                .map(|v| v.reason_codes.clone())
                .unwrap_or_default(),
            verified_at: carried.as_ref().map(|_| chrono::Utc::now()),
            // RFC 0008 §13.3: the ref → commit pair travels with the bytes.
            // A disconnected instance resolves a ref before it fetches
            // anything and has no forge to ask, so without this row a forge
            // registry answers `503` to every coordinate in the bundle it
            // just imported.
            git_ref: match (
                &fetched.package_name,
                &fetched.requested_ref,
                &fetched.ref_kind,
                &fetched.resolved_commit,
            ) {
                (Some(owner_repo), Some(git_ref), Some(kind), Some(sha)) => {
                    Some(batlehub_core::services::bundle::BundleRef {
                        owner_repo: owner_repo.clone(),
                        git_ref: git_ref.clone(),
                        kind: kind.clone(),
                        sha: sha.clone(),
                    })
                }
                _ => None,
            },
        });
        blobs.entry(digest).or_insert(bytes);
    }

    let manifest = BundleManifest {
        bundle_version: BUNDLE_VERSION,
        bundle_id: args
            .bundle_id
            .clone()
            .unwrap_or_else(|| plan.generated_from.sha256.clone()),
        created_at: chrono::Utc::now(),
        source_plan: Some(plan.generated_from.file.clone()),
        entries,
    };
    let manifest_bytes = manifest.to_signed_bytes()?;
    let signature = signing.sign(&manifest_bytes).to_bytes();
    // The public half, reported rather than left to be derived: it is what
    // the disconnected instance must carry in `air_gap.bundle_trusted_keys`,
    // and an operator who has to compute it from the private key on the
    // command line is an operator who will paste the private one.
    let public_key = hex::encode(signing.verifying_key().to_bytes());

    let file = std::fs::File::create(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;
    let written = batlehub_core::services::bundle::write_bundle(
        std::io::BufWriter::new(file),
        &manifest,
        &signature,
        |digest| blobs.get(digest).cloned(),
    )?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "bundle_id": manifest.bundle_id,
                "path": args.out.display().to_string(),
                "entries": manifest.entries.len(),
                "blobs": written,
                "signer_key": public_key,
                "skipped": skipped,
            }))?
        );
    } else {
        println!(
            "{} · {} entr{} · {written} blob(s) · signed",
            args.out.display(),
            manifest.entries.len(),
            if manifest.entries.len() == 1 {
                "y"
            } else {
                "ies"
            },
        );
        for line in &skipped {
            println!("  skipped: {line}");
        }
        println!("  signed by {public_key}");
        println!(
            "  the disconnected instance must list that key in \
             [air_gap].bundle_trusted_keys"
        );
    }
    Ok(())
}

/// What one seeded entry became.
#[derive(Debug, serde::Serialize)]
struct SeedOutcome {
    tool: String,
    platform: String,
    key: String,
    status: u16,
    /// What the server actually served, which is what the next bundle will
    /// carry — a plan whose `size` disagreed with reality is the first sign
    /// the lock and the mirror have drifted.
    bytes: u64,
    /// `ok`, `digest_mismatch`, `denied`, `failed` or `unmirrored`.
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

async fn run_seed(args: SeedArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    let text = std::fs::read_to_string(&args.plan)
        .with_context(|| format!("reading {}", args.plan.display()))?;
    let plan: MisePlan = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a plan", args.plan.display()))?;
    if plan.plan_version > crate::api::mise_plan::PLAN_VERSION {
        anyhow::bail!(
            "{} is plan_version {}, and this CLI understands {}",
            args.plan.display(),
            plan.plan_version,
            crate::api::mise_plan::PLAN_VERSION
        );
    }

    let mut outcomes: Vec<SeedOutcome> = Vec::new();
    for entry in &plan.entries {
        let (Some(path), Some(key)) = (&entry.proxy_path, &entry.key) else {
            // No registry mirrors this host: `plan` already said so, and
            // seeding cannot invent one. Reported rather than skipped, so
            // the count at the end is the whole lock.
            outcomes.push(SeedOutcome {
                tool: entry.tool.clone(),
                platform: entry.platform.clone(),
                key: entry.url.clone(),
                status: 0,
                bytes: 0,
                result: "unmirrored",
                detail: Some("no registry on this server mirrors this host".into()),
            });
            continue;
        };
        let fetched = client.seed_fetch(path).await;
        let mut outcome = SeedOutcome {
            tool: entry.tool.clone(),
            platform: entry.platform.clone(),
            key: key.clone(),
            status: 0,
            bytes: 0,
            result: "failed",
            detail: None,
        };
        match fetched {
            Err(e) => outcome.detail = Some(e.to_string()),
            Ok(f) => {
                outcome.status = f.status;
                outcome.bytes = f.size;
                if f.error.is_some() || f.status >= 400 {
                    outcome.detail = f.error.or(Some(format!("HTTP {}", f.status)));
                    // A refusal with a verdict is a *judgement*, not a
                    // fetch failure, and the two lead an operator to
                    // different places.
                    if f.verdict.as_deref() == Some("denied") {
                        outcome.result = "denied";
                        outcome.detail = Some(f.reasons.clone());
                    }
                } else if let Some(expected) = &entry.sha256 {
                    // The lock's digest against what BatleHub actually
                    // stored. This is the check that makes a bundle's
                    // contents provable on the disconnected side without
                    // reference to anything the bundle itself claims.
                    if expected.eq_ignore_ascii_case(&f.sha256) {
                        outcome.result = "ok";
                    } else {
                        outcome.result = "digest_mismatch";
                        outcome.detail =
                            Some(format!("lock says {expected}, server served {}", f.sha256));
                    }
                } else {
                    // No digest in the lock: fetched and cached, and this
                    // says so rather than claiming a match nobody made.
                    outcome.result = "ok";
                    outcome.detail = Some("the lock records no checksum for this entry".into());
                }
                if args.verify {
                    if let Some(v) = &f.verdict {
                        let note = format!("verdict {v} ({})", f.reasons);
                        outcome.detail = Some(match outcome.detail.take() {
                            Some(d) => format!("{d}; {note}"),
                            None => note,
                        });
                    }
                }
            }
        }
        outcomes.push(outcome);
    }

    let ok = outcomes.iter().filter(|o| o.result == "ok").count();
    let failed = outcomes.len() - ok;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "seeded": ok,
                "total": outcomes.len(),
                "entries": outcomes,
            }))?
        );
    } else {
        println!("{ok}/{} fetched and verified", outcomes.len());
        for o in outcomes.iter().filter(|o| o.result != "ok") {
            println!(
                "  {} [{}] {}: {}{}",
                o.result,
                o.platform,
                o.tool,
                o.key,
                o.detail
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default()
            );
        }
        if args.verify {
            let denied = outcomes.iter().filter(|o| o.result == "denied").count();
            let provenance = outcomes
                .iter()
                .filter(|o| {
                    o.detail
                        .as_deref()
                        .is_some_and(|d| d.contains("PROVENANCE_MISSING"))
                })
                .count();
            println!("  {denied} denied · {provenance} with no provenance");
        }
    }
    if failed > 0 {
        // A CI gate on the connected side: the run failed, and the lines
        // above say which entries and why.
        std::process::exit(1);
    }
    Ok(())
}

async fn run_plan(args: PlanArgs, client: &BatleHubClient, json: bool) -> Result<()> {
    // An unreachable server costs the registry annotation, not the plan: the
    // lock is the bill of materials, and a plan built against no registries
    // reports every host as unmirrored, which is true of a server with none.
    let known: Vec<KnownRegistry> = client
        .list_registries()
        .await
        .map(|regs| {
            regs.into_iter()
                .map(|r| KnownRegistry {
                    name: r.name,
                    registry_type: r.registry_type,
                })
                .collect()
        })
        .unwrap_or_default();

    // RFC 0008 §13 decision 1: explicit wins, `all` opts into every platform
    // the lock records, and the default is this host's.
    let wanted: Vec<String> = if args.platform.iter().any(|p| p == "all") {
        Vec::new()
    } else if args.platform.is_empty() {
        vec![crate::api::mise_plan::host_platform()]
    } else {
        args.platform.clone()
    };

    let mut plan = plan_from_path(&args.lock, &wanted, &known)?;
    // A host platform that the lock does not record would otherwise plan an
    // empty bundle for a lock full of tools, and the operator would find out
    // on the disconnected side. Say it, and plan what there is.
    if plan.entries.is_empty() && args.platform.is_empty() {
        let every = plan_from_path(&args.lock, &[], &known)?;
        if !every.entries.is_empty() {
            eprintln!(
                "warning: {} records no entry for this host ({}); planning every platform it \
                 does record. Pass --platform to choose.",
                args.lock.display(),
                crate::api::mise_plan::host_platform()
            );
            plan = every;
        }
    }
    if args.include_mise {
        let version = resolve_mise_version(args.mise_version.as_deref())?;
        crate::api::mise_plan::add_mise_itself(&mut plan, &version, &known);
    }
    let rendered = serde_json::to_string_pretty(&plan)?;

    if let Some(path) = &args.out {
        std::fs::write(path, format!("{rendered}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    if json {
        if args.out.is_none() {
            println!("{rendered}");
        }
        return Ok(());
    }
    if args.out.is_none() {
        println!("{rendered}");
        return Ok(());
    }
    print_summary(&plan, args.out.as_deref());
    Ok(())
}

/// Which mise to carry across the gap.
///
/// RFC 0008 §13 decision 2: `mise self-update` reads the GitHub releases API,
/// which the github rewrite rule already covers — so a bundle that carries
/// `github:jdx/mise@<current>` always contains the binary that will read the
/// next plan, and the estate never has to cross the gap by hand to upgrade
/// the tool that crosses the gap.
///
/// "current" is the mise on this PATH, because that is the one whose lock
/// this plan was built from. When there is none, the version has to be said
/// out loud rather than guessed.
fn resolve_mise_version(explicit: Option<&str>) -> Result<String> {
    if let Some(v) = explicit {
        return Ok(v.trim().trim_start_matches('v').to_owned());
    }
    let out = std::process::Command::new("mise")
        .arg("--version")
        .output()
        .context("running `mise --version` to find the version to carry; pass --mise-version")?;
    if !out.status.success() {
        anyhow::bail!("`mise --version` failed; pass --mise-version");
    }
    // `mise 2026.8.6 linux-x64 (…)` — the first token that looks like one.
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|v| v.trim_start_matches('v').to_owned())
        .context("could not read a version out of `mise --version`; pass --mise-version")
}

/// The one line §1 shows: tools, downloads, registries, and the number that
/// matters — how many hosts have no mirror.
fn print_summary(plan: &MisePlan, out: Option<&std::path::Path>) {
    use std::collections::BTreeSet;
    let tools: BTreeSet<&str> = plan.entries.iter().map(|e| e.tool.as_str()).collect();
    let registries: BTreeSet<&str> = plan
        .entries
        .iter()
        .filter_map(|e| e.registry.as_ref().map(|r| r.name.as_str()))
        .collect();
    println!(
        "{} tool(s) · {} download(s) · {} registr{} · {} host(s) with no mirror configured",
        tools.len(),
        plan.entries.len(),
        registries.len(),
        if registries.len() == 1 { "y" } else { "ies" },
        plan.unmirrored_hosts.len()
    );
    for host in &plan.unmirrored_hosts {
        println!("  no mirror: {host}");
    }
    for tool in &plan.unsupported {
        println!("  unsupported: {}", tool.reason);
    }
    if let Some(path) = out {
        println!("plan written to {}", path.display());
    }
}
