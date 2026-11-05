use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Args;

use batlehub_core::entities::RegistryKind;

use crate::api::{
    publish::{detect_meta, devfile_bundle, with_maven_checksums},
    publish_input::{go_module_zip, pom_coordinate, provider_platforms},
    BatleHubClient,
};
use crate::exit::{CliError, ExitCode};
use crate::render::{ctx, data, warn};

fn usage(message: impl Into<String>) -> anyhow::Error {
    CliError::new(ExitCode::Usage, message).into()
}

/// Why a kind takes no `publish`: its §4.7 cell's reason is the message
/// (§4.11). `None` for a kind this command uploads.
fn refusal(kind: RegistryKind, base: &str, registry: &str) -> Option<String> {
    use crate::coverage::{cell, Capability, Cell};
    match cell(kind, Capability::Publish) {
        Cell::Done(_) => None,
        Cell::Print(why) if kind == RegistryKind::Nix => Some(format!(
            "{why}: nix copy --to \"{base}/proxy/{registry}/nix\" ./result"
        )),
        Cell::Print(why) | Cell::NotApplicable(why) => {
            Some(format!("nothing to publish to a {kind} registry: {why}"))
        }
    }
}

#[derive(Args)]
pub struct PublishArgs {
    /// What to publish: an artifact file; a devfile stack's directory or
    /// devfile.yaml; a Go module directory; a Maven `target/` directory; a
    /// Terraform provider directory of `_<os>_<arch>.zip` archives
    pub file: PathBuf,

    /// Package name (overrides auto-detection)
    #[arg(long, short = 'n')]
    pub name: Option<String>,

    /// Package version (overrides auto-detection)
    #[arg(long, short = 'v')]
    pub version: Option<String>,

    /// The registry's kind, for a token that may publish to it but not list
    /// it. Otherwise the registry's own kind decides, and this is checked
    /// against it.
    #[arg(long = "type", short = 't')]
    pub r#type: Option<String>,

    /// Maven: the pom to publish with the jar (default: `pom.xml` beside a
    /// `target/` directory)
    #[arg(long)]
    pub pom: Option<PathBuf>,

    /// Debian: target suite/distribution (e.g. "stable"). Required when publishing a `.deb`.
    #[arg(long)]
    pub distribution: Option<String>,

    /// Debian: target component (e.g. "main"). Required when publishing a `.deb`.
    #[arg(long)]
    pub component: Option<String>,

    /// Conda: target platform/subdir (e.g. "noarch", "linux-64"). Only used as a
    /// fallback when the package itself carries no `subdir` metadata.
    #[arg(long, default_value = "noarch")]
    pub platform: String,
}

pub async fn run(
    args: PublishArgs,
    client: &BatleHubClient,
    default_registry: Option<&str>,
) -> Result<()> {
    if !args.file.exists() {
        bail!("file not found: {}", args.file.display());
    }
    let registry = ctx().require_registry(default_registry.map(str::to_string))?;
    // The registry decides the kind, never the file: a Helm chart `.tgz` is
    // not an npm tarball because they share an extension (§6.3, T8).
    let kind: RegistryKind = match (
        client.registry_kind(&registry).await,
        args.r#type.as_deref(),
    ) {
        (Ok(listed), Some(t)) if t != listed => {
            return Err(usage(format!(
                "-t {t} names a different kind from registry `{registry}`, which is {listed}"
            )));
        }
        (Ok(listed), _) => listed,
        // A least-privilege CI token may publish to a registry it cannot
        // list: the server checks only `releases:publish`, so neither does
        // this, once the caller names the kind.
        (Err(e), Some(t)) if is_not_found(&e) => t.to_owned(),
        (Err(e), None) if is_not_found(&e) => {
            return Err(CliError::new(ExitCode::NotFound, format!("{e:#}"))
                .hint(
                    "a token that may publish but not list the registry names its kind: -t <kind>",
                )
                .into());
        }
        (Err(e), _) => return Err(e),
    }
    .parse()
    .map_err(|e: String| usage(e))?;
    if let Some(reason) = refusal(kind, &client.base_url, &registry) {
        return Err(usage(reason));
    }
    if kind == RegistryKind::Devfile {
        return publish_devfile(args, client, &registry).await;
    }

    // The file name is read only for a name and version, and only when it
    // reads as this kind's own artifact.
    let meta = detect_meta(&args.file).filter(|m| {
        m.registry_type == kind.as_str()
            || (kind == RegistryKind::VscodeMarketplace && m.registry_type == "openvsx")
    });
    let name = args
        .name
        .clone()
        .or_else(|| meta.as_ref().map(|m| m.name.clone()));
    let version = args
        .version
        .clone()
        .or_else(|| meta.as_ref().map(|m| m.version.clone()));
    let need = |what: &str, v: &Option<String>| -> Result<String> {
        v.clone().ok_or_else(|| {
            usage(format!(
                "could not read the package {what} from {}; pass --{what}",
                args.file.display()
            ))
        })
    };

    eprintln!(
        "Publishing {} to {kind} registry '{registry}' ...",
        args.file.display()
    );
    let mut published = (name.clone(), version.clone());
    match kind {
        RegistryKind::Nuget => client.publish_nuget(&registry, &args.file).await?,
        RegistryKind::Pacman => client.publish_pacman(&registry, &args.file).await?,
        RegistryKind::Rubygems => client.publish_rubygems(&registry, &args.file).await?,
        RegistryKind::Rpm => client.publish_rpm(&registry, &args.file).await?,
        RegistryKind::Galaxy => client.publish_galaxy(&registry, &args.file).await?,
        RegistryKind::Helm => client.publish_helm(&registry, &args.file).await?,
        RegistryKind::Apk => client.publish_apk(&registry, &args.file).await?,
        RegistryKind::JetbrainsMarketplace => {
            client
                .publish_jetbrains_plugin(&registry, &args.file)
                .await?
        }
        RegistryKind::Composer => {
            client
                .publish_composer(&registry, &args.file, args.version.as_deref())
                .await?
        }
        RegistryKind::Conda => {
            client
                .publish_conda(&registry, &args.platform, &args.file)
                .await?
        }
        RegistryKind::Deb => {
            let distribution = args.distribution.as_deref().ok_or_else(|| {
                usage("publishing a .deb requires --distribution (e.g. 'stable')")
            })?;
            let component = args
                .component
                .as_deref()
                .ok_or_else(|| usage("publishing a .deb requires --component (e.g. 'main')"))?;
            client
                .publish_deb(&registry, distribution, component, &args.file)
                .await?
        }
        RegistryKind::Pypi => {
            let (n, v) = (need("name", &name)?, need("version", &version)?);
            client.publish_pypi(&registry, &n, &v, &args.file).await?
        }
        RegistryKind::Npm => {
            let (n, v) = (need("name", &name)?, need("version", &version)?);
            client.publish_npm(&registry, &n, &v, &args.file).await?
        }
        RegistryKind::Cargo => {
            let (n, v) = (need("name", &name)?, need("version", &version)?);
            client.publish_cargo(&registry, &n, &v, &args.file).await?
        }
        // The server's `PUT …/vsix` serves both gallery kinds.
        RegistryKind::Openvsx | RegistryKind::VscodeMarketplace => {
            let (n, v) = (need("name", &name)?, need("version", &version)?);
            client
                .publish_openvsx(&registry, &n, &v, &args.file)
                .await?
        }
        RegistryKind::Goproxy => {
            let v = need("version", &args.version)?;
            let (module, zip) = if args.file.is_dir() {
                go_module_zip(&args.file, &v)?
            } else {
                // A module zip built elsewhere: sent as it is.
                let module = need("name", &args.name)?;
                (module, tokio::fs::read(&args.file).await?)
            };
            client.publish_goproxy(&registry, &module, &v, zip).await?;
            published = (Some(module), Some(v));
        }
        RegistryKind::Maven => {
            let (coord, files) = maven_files(&args)?;
            client.publish_maven(&registry, &files).await?;
            published = (Some(coord.name()), Some(coord.version));
        }
        RegistryKind::Terraform => {
            if args.file.is_dir() {
                let ns_type = need("name", &args.name)?;
                if ns_type.split('/').count() != 2 {
                    return Err(usage(
                        "a provider publishes as --name <namespace>/<type>, e.g. acme/null",
                    ));
                }
                let (v, platforms) = provider_platforms(&args.file)?;
                client
                    .publish_terraform_provider(&registry, &ns_type, &v, &platforms)
                    .await?;
                published = (Some(format!("providers/{ns_type}")), Some(v));
            } else {
                let coordinate = need("name", &args.name)?;
                if coordinate.split('/').count() != 3 {
                    return Err(usage(
                        "a module publishes as --name <namespace>/<name>/<provider>, e.g. \
                         acme/vpc/aws",
                    ));
                }
                let v = need("version", &args.version)?;
                client
                    .publish_terraform_module(&registry, &coordinate, &v, &args.file)
                    .await?;
                published = (Some(format!("modules/{coordinate}")), Some(v));
            }
        }
        RegistryKind::Devfile
        | RegistryKind::Nix
        | RegistryKind::Jetbrains
        | RegistryKind::Github
        | RegistryKind::Forgejo
        | RegistryKind::Gitlab
        | RegistryKind::Generic
        | RegistryKind::Nodedist
        | RegistryKind::Sdkman
        | RegistryKind::Rustup => {
            // Answered by `refusal` and the devfile branch above.
            return Err(usage(format!("nothing to publish to a {kind} registry")));
        }
    }

    if ctx().structured() {
        data(&serde_json::json!({
            "registry": registry, "kind": kind.as_str(),
            "name": published.0, "version": published.1,
        }))?;
    } else {
        println!("Published successfully.");
    }
    Ok(())
}

/// The jar (when there is one) and the pom, each with its checksums, the pom
/// last: the version row the server creates at the pom then names files
/// that are all there.
/// A Maven coordinate and the `(repository path, bytes)` files to upload.
type MavenUpload = (
    crate::api::publish_input::PomCoordinate,
    Vec<(String, Vec<u8>)>,
);

/// The main artifact's extension for a `<packaging>`: Maven's own mapping
/// for the packagings that do not name their extension.
fn maven_extension(packaging: &str) -> &str {
    match packaging {
        "maven-plugin" | "ejb" | "bundle" | "pom" => "jar",
        other => other,
    }
}

fn maven_files(args: &PublishArgs) -> Result<MavenUpload> {
    let input = &args.file;
    let is_pom = input.extension().is_some_and(|e| e == "pom");
    let pom_path = match (&args.pom, input.is_dir(), is_pom) {
        (Some(p), _, _) => p.clone(),
        (None, _, true) => input.clone(),
        // The directory's own pom first (a module), then the one above it
        // (`target/`): the other way round, a module read its aggregator's.
        (None, true, _) => {
            let own = input.join("pom.xml");
            if own.exists() {
                own
            } else {
                input.join("..").join("pom.xml")
            }
        }
        (None, false, false) => return Err(usage("a jar publishes with --pom <pom.xml>")),
    };
    let pom = std::fs::read_to_string(&pom_path)
        .with_context(|| format!("reading {}", pom_path.display()))?;
    let coord = pom_coordinate(&pom).ok_or_else(|| {
        usage(format!(
            "{} names no literal groupId/artifactId/version of its own (a `${{…}}` property is \
             the build's to resolve, and one inherited from <parent> is not read by the registry)",
            pom_path.display()
        ))
    })?;
    let stem = format!("{}-{}", coord.artifact_id, coord.version);
    // An artifact keeps its own file name, so a `-sources.jar` or a `.war`
    // lands beside the main jar rather than in its place.
    let artifact = if input.is_dir() {
        let main = format!("{stem}.{}", maven_extension(&coord.packaging));
        let found = [input.join(&main), input.join("target").join(&main)]
            .into_iter()
            .find(|p| p.exists());
        if found.is_none() && coord.packaging != "pom" {
            return Err(usage(format!(
                "no {main} in {} or its target/: build it first, or name the file",
                input.display()
            )));
        }
        found
    } else {
        (!is_pom).then(|| input.clone())
    };
    let mut files = Vec::new();
    if let Some(path) = artifact {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let named_for_the_pom = file_name
            .strip_prefix(&stem)
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('-'));
        if !named_for_the_pom {
            return Err(usage(format!(
                "{file_name} is not named {stem}[-classifier].<ext>, as `mvn package` writes it \
                 for this pom"
            )));
        }
        files.extend(with_maven_checksums(
            &format!("{}/{file_name}", coord.dir()),
            std::fs::read(&path)?,
        ));
    }
    let base = format!("{}/{stem}", coord.dir());
    let mut pom_files = with_maven_checksums(&format!("{base}.pom"), pom.into_bytes());
    // Sidecars first, the pom itself last.
    pom_files.rotate_left(1);
    files.extend(pom_files);
    if args.name.is_some() || args.version.is_some() {
        warn("--name/--version are ignored for maven: the pom is the coordinate");
    }
    Ok((coord, files))
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<CliError>())
        .any(|c| c.exit == ExitCode::NotFound)
}

/// RFC 0039 §4.2: pack (or read) the bundle, read its name and version the
/// way the server will, and upload it. `--name`/`--version` are refused: the
/// server reads both from the devfile and nowhere else, so an override could
/// only ever produce a `400`.
async fn publish_devfile(args: PublishArgs, client: &BatleHubClient, registry: &str) -> Result<()> {
    if args.name.is_some() || args.version.is_some() {
        bail!(
            "--name and --version are not accepted for a devfile stack: the registry reads both \
             from metadata.name and metadata.version in devfile.yaml"
        );
    }
    let bundle = devfile_bundle(&args.file)?;
    let read = batlehub_core::services::devfile::read_bundle(&bundle)
        .map_err(|e| anyhow::anyhow!("{}: {e}", args.file.display()))?;
    eprintln!(
        "Publishing devfile {}@{} to registry '{}' ...",
        read.name, read.version, registry
    );
    let published = client.publish_devfile(registry, bundle).await?;
    if crate::render::ctx().structured() {
        crate::render::data(&published)?;
    } else {
        println!(
            "Published {}@{} ({}).",
            published.stack, published.version, published.digest
        );
    }
    Ok(())
}
