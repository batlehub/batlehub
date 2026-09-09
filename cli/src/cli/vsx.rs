//! `batlehub-cli vsx` — the client side of a registry's VSIX signature
//! (RFC 0020 §6.6): `keygen` prints a seed for `[registries.vsx_signing]`,
//! `verify` checks a downloaded `.vsix` against the archive the registry
//! serves as its `VsixSignature` asset and the key its `PublicKey` asset
//! names.
//!
//! What `verify` proves: the `.signature.sig` is the key's Ed25519 signature
//! over the file's bytes, and the `.signature.manifest` describes those bytes
//! — the same two checks a registry makes before serving a stored archive.
//! It does not, and cannot, run the editor's own verifier: that one accepts
//! the Microsoft marketplace's signature and no other (RFC 0020 §4.5).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use batlehub_core::services::{
    signature::{public_key_from_pem_or_hex, VsxSigningKey},
    vsx_signature::{manifest_matches, read_signature_archive},
};

#[derive(Subcommand)]
pub enum VsxCommand {
    /// Print a fresh Ed25519 seed for `[registries.vsx_signing]`, and the key
    /// id it derives. Writes nothing.
    Keygen,
    /// Verify a `.vsix` against a registry's signature archive and public key.
    Verify(VerifyArgs),
}

#[derive(Args)]
pub struct VerifyArgs {
    /// The `.vsix` file, as downloaded.
    pub vsix: PathBuf,
    /// The signature archive (`Microsoft.VisualStudio.Services.VsixSignature`).
    /// Fetched from `--registry` when absent.
    #[arg(long)]
    pub signature: Option<PathBuf>,
    /// The public key, PEM or 64 hex characters, or a file holding either.
    /// Fetched from the registry's `PublicKey` asset when absent.
    #[arg(long)]
    pub public_key: Option<String>,
    /// The registry base (`https://hub/proxy/vsx`) to fetch the archive and
    /// the key from, for the extension `--id` at `--version`.
    #[arg(long)]
    pub registry: Option<String>,
    /// `publisher.name`, with `--registry`.
    #[arg(long)]
    pub id: Option<String>,
    /// The version, with `--registry`.
    #[arg(long)]
    pub version: Option<String>,
}

/// `token` is the CLI's global `--token`/`BATLEHUB_TOKEN`: a registry whose
/// `anonymous` holds no verb answers the asset routes with a credential only.
pub async fn run(cmd: VsxCommand, token: Option<&str>) -> Result<()> {
    match cmd {
        VsxCommand::Keygen => keygen(),
        VsxCommand::Verify(args) => verify(args, token).await,
    }
}

fn keygen() -> Result<()> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|e| anyhow::anyhow!("reading the system's random source: {e}"))?;
    let key = VsxSigningKey::from_seed(seed, None);
    println!("# [registries.vsx_signing] — keep the seed out of the file: seed_hex = \"${{VSX_SIGNING_SEED}}\"");
    println!("seed_hex   = \"{}\"", hex::encode(seed));
    println!("# derived:");
    println!("key_id     = \"{}\"", key.key_id());
    println!(
        "public_key = \"{}\"   # the trusted_keys form",
        key.public_key_hex()
    );
    Ok(())
}

async fn verify(args: VerifyArgs, token: Option<&str>) -> Result<()> {
    let vsix =
        std::fs::read(&args.vsix).with_context(|| format!("reading {}", args.vsix.display()))?;

    let (archive_bytes, key_text) = match (&args.signature, &args.registry) {
        (Some(path), _) => {
            let archive =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let key = args
                .public_key
                .clone()
                .context("--public-key is required with --signature (or use --registry)")?;
            (archive, read_key_text(&key)?)
        }
        (None, Some(base)) => {
            let (id, version) = (
                args.id
                    .as_deref()
                    .context("--id is required with --registry")?,
                args.version
                    .as_deref()
                    .context("--version is required with --registry")?,
            );
            let (publisher, name) = id.split_once('.').context("--id is `publisher.name`")?;
            let asset = |t: &str| {
                format!(
                    "{}/vscode/asset/{publisher}/{name}/{version}/{t}",
                    base.trim_end_matches('/')
                )
            };
            let http = reqwest::Client::new();
            let archive = fetch(
                &http,
                asset("Microsoft.VisualStudio.Services.VsixSignature"),
                token,
            )
            .await?;
            let key = match &args.public_key {
                Some(k) => read_key_text(k)?,
                None => String::from_utf8(
                    fetch(
                        &http,
                        asset("Microsoft.VisualStudio.Services.PublicKey"),
                        token,
                    )
                    .await?,
                )
                .context("the public key is not text")?,
            };
            (archive, key)
        }
        (None, None) => {
            bail!("give --signature and --public-key, or --registry with --id and --version")
        }
    };

    let archive = read_signature_archive(&archive_bytes)?;
    let raw = public_key_from_pem_or_hex(&key_text)
        .context("the public key is neither PEM (SubjectPublicKeyInfo) nor 64 hex characters")?;
    let key_hex = hex::encode(raw);
    if !batlehub_core::services::signature::verify_ed25519(
        std::slice::from_ref(&key_hex),
        &archive.signature,
        &vsix,
    ) {
        bail!(
            "signature does not verify: the archive was not made over these bytes by the key {}",
            &key_hex[..16]
        );
    }
    if !manifest_matches(&archive.manifest, &vsix)? {
        bail!("the signature manifest does not describe this file's entries");
    }
    println!(
        "ok: {} is signed by key {}… ({} bytes, manifest matches)",
        args.vsix.display(),
        &key_hex[..16],
        vsix.len()
    );
    Ok(())
}

async fn fetch(http: &reqwest::Client, url: String, token: Option<&str>) -> Result<Vec<u8>> {
    let mut req = http.get(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !resp.status().is_success() {
        bail!("{url} answered {}", resp.status());
    }
    Ok(resp.bytes().await?.to_vec())
}

/// A key given inline (PEM or hex) or as a path to a file holding either.
fn read_key_text(given: &str) -> Result<String> {
    let path = std::path::Path::new(given);
    if path.is_file() {
        return std::fs::read_to_string(path).with_context(|| format!("reading {given}"));
    }
    Ok(given.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inline_key_is_returned_as_is_and_a_file_is_read() {
        assert_eq!(read_key_text("abcd").unwrap(), "abcd");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("key.pem");
        std::fs::write(&p, "-----BEGIN PUBLIC KEY-----\n").unwrap();
        assert!(read_key_text(p.to_str().unwrap())
            .unwrap()
            .starts_with("-----BEGIN"));
    }
}
