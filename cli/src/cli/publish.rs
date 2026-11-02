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
use crate::render::{ctx, data, deprecated, warn};

fn usage(message: impl Into<String>) -> anyhow::Error {
    CliError::new(ExitCode::Usage, message).into()
}

/// Why a kind takes no `publish` (RFC 0040 §4.7): the cell's reason is the
/// message. `None` for a kind this command uploads.
fn refusal(kind: RegistryKind, base: &str, registry: &str) -> Option<String> {
    use RegistryKind as K;
    match kind {
        K::Nix => Some(format!(
            "a nix registry is published by `nix copy`, which signs and uploads the \
             closure: nix copy --to \"{base}/proxy/{registry}/nix\" ./result"
        )),
        K::Jetbrains | K::Github | K::Forgejo | K::Gitlab | K::Generic => Some(format!(
            "a {kind} registry is proxy-only: there is nothing to publish to"
        )),
        K::Nodedist | K::Sdkman | K::Rustup => Some(format!(
            "a {kind} registry mirrors a vendor's releases: there is nothing to publish to"
        )),
        _ => None,
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

    /// Deprecated: the registry's own kind decides. Checked against it, never
    /// obeyed.
    #[arg(long, short = 't', hide = true)]
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
    let kind: RegistryKind = client
        .registry_kind(&registry)
        .await?
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    if let Some(t) = args.r#type.as_deref() {
        if t != kind.as_str() {
            return Err(usage(format!(
                "-t {t} names a different kind from registry `{registry}`, which is {kind}"
            )));
        }
        deprecated("publish -t", "publish (the registry's kind decides)");
    }
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
                (module, std::fs::read(&args.file)?)
            };
            client.publish_goproxy(&registry, &module, &v, zip).await?;
            published = (Some(module), Some(v));
        }
        RegistryKind::Maven => {
            let (coord, files) = maven_files(&args)?;
            client.publish_maven(&registry, &files).await.map_err(|e| {
                // Some files went, some did not: a re-run resumes (§6.3).
                anyhow::Error::new(CliError::new(ExitCode::Partial, format!("{e:#}")))
            })?;
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
                    .await
                    .map_err(|e| {
                        anyhow::Error::new(CliError::new(ExitCode::Partial, format!("{e:#}")))
                    })?;
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

fn maven_files(args: &PublishArgs) -> Result<MavenUpload> {
    let input = &args.file;
    let is_pom = input.extension().is_some_and(|e| e == "pom");
    let pom_path = match (&args.pom, input.is_dir(), is_pom) {
        (Some(p), _, _) => p.clone(),
        (None, _, true) => input.clone(),
        (None, true, _) => {
            let beside = input.join("..").join("pom.xml");
            if beside.exists() {
                beside
            } else {
                input.join("pom.xml")
            }
        }
        (None, false, false) => return Err(usage("a jar publishes with --pom <pom.xml>")),
    };
    let pom = std::fs::read_to_string(&pom_path)
        .with_context(|| format!("reading {}", pom_path.display()))?;
    let coord = pom_coordinate(&pom).ok_or_else(|| {
        usage(format!(
            "{} names no literal groupId/artifactId/version (a `${{…}}` property is the \
             build's to resolve)",
            pom_path.display()
        ))
    })?;
    let base = format!("{}/{}-{}", coord.dir(), coord.artifact_id, coord.version);
    let jar = if input.is_dir() {
        let j = input.join(format!("{}-{}.jar", coord.artifact_id, coord.version));
        j.exists().then_some(j)
    } else {
        (!is_pom).then(|| input.clone())
    };
    let mut files = Vec::new();
    if let Some(jar) = jar {
        files.extend(with_maven_checksums(
            &format!("{base}.jar"),
            std::fs::read(&jar)?,
        ));
    }
    let mut pom_files = with_maven_checksums(&format!("{base}.pom"), pom.into_bytes());
    // Sidecars first, the pom itself last.
    pom_files.rotate_left(1);
    files.extend(pom_files);
    if args.name.is_some() || args.version.is_some() {
        warn("--name/--version are ignored for maven: the pom is the coordinate");
    }
    Ok((coord, files))
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
