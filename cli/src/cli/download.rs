use anyhow::{Context, Result};
use clap::Args;

use std::io::IsTerminal;

use batlehub_core::entities::RegistryKind;
use batlehub_core::services::integrity::StreamingVerifier;
use batlehub_core::services::IntegrityOutcome;

use crate::api::artifact::{artifact_path, checksum_in, checksum_source};
use crate::api::BatleHubClient;
use crate::coordinate::Coordinate;
use crate::exit::{CliError, ExitCode};
use crate::render::warn;

#[derive(Args)]
pub struct DownloadArgs {
    /// What to download. One of:
    ///   - a coordinate: npm-local/lodash@4.17.21 (or lodash@4.17.21 with -r)
    ///   - a full URL: https://host/proxy/jb/jetbrains/idea/x.tar.gz
    ///   - a server path: /proxy/jb/jetbrains/idea/x.tar.gz
    ///   - registry-relative (needs -r/--registry): jetbrains/idea/x.tar.gz
    #[arg(verbatim_doc_comment)]
    pub target: String,

    /// Output file; "-" for stdout. Default: named from the coordinate, or the
    /// path's basename. An existing file is overwritten only after asking.
    #[arg(long, short = 'o')]
    pub output: Option<String>,
}

/// Resolve the CLI `target` into something `download_to` understands: a full URL
/// or a server-absolute path. A full URL or a `/…` path is used as-is; a bare
/// `"{type}/{path}"` is expanded against the default registry into
/// `/proxy/{registry}/{type}/{path}`.
fn resolve_target(target: &str, default_registry: Option<&str>) -> Result<String> {
    if target.starts_with("http://") || target.starts_with("https://") || target.starts_with('/') {
        Ok(target.to_owned())
    } else {
        let registry = default_registry.context(
            "a registry-relative path needs a registry — pass -r <registry>, or give a \
             full /proxy/… path or URL",
        )?;
        Ok(format!(
            "/proxy/{registry}/{}",
            target.trim_start_matches('/')
        ))
    }
}

/// Derive a default output filename from a resolved path/URL: the last non-empty
/// path segment (ignoring any `?`/`#` query or fragment), or `"download"`.
fn default_output_name(resolved: &str) -> String {
    resolved
        .split(['?', '#'])
        .next()
        .unwrap_or(resolved)
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("download")
        .to_string()
}

/// Download a file through the proxy cache. Because it goes through the normal
/// proxy read path, a cache miss is fetched from upstream and **cached** — so
/// this doubles as an on-demand warm for path-addressed registries.
/// What the target names: a coordinate (RFC 0040 §4.8) or a raw path.
struct Plan {
    path: String,
    /// The file name when `-o` is absent.
    file: String,
    /// The advertised checksum, when the kind's metadata carries one.
    expected: Option<String>,
}

/// `registry/name@version` (or `name@version` with `-r`) on a kind fetchable
/// by version; anything else is a path, as before.
async fn plan(
    target: &str,
    client: &BatleHubClient,
    default_registry: Option<&str>,
) -> Result<Plan> {
    let is_path = target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with('/')
        || !target.contains('@');
    if is_path {
        let path = resolve_target(target, default_registry)?;
        return Ok(Plan {
            file: default_output_name(&path),
            path,
            expected: None,
        });
    }
    let (registry, name, version) = Coordinate::resolve(client, target, default_registry)
        .await?
        .require_version()?;
    let kind: RegistryKind = client
        .registry_kind(&registry)
        .await?
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let (path, file) = artifact_path(kind, &registry, &name, &version)
        .map_err(|why| CliError::new(ExitCode::Usage, why))?;
    let expected = match checksum_source(kind, &registry, &name) {
        Some(src) => {
            let mut doc = Vec::new();
            client.download_to(&src, &mut doc).await?;
            checksum_in(kind, &String::from_utf8_lossy(&doc), &version)
        }
        None => None,
    };
    if expected.is_none() {
        warn(&format!(
            "{kind} metadata carries no checksum this CLI reads: {registry}/{name}@{version} \
             is written unverified"
        ));
    }
    Ok(Plan {
        path,
        file,
        expected,
    })
}

/// Hashes what it forwards, so the check costs no second pass (§6.5).
struct Verifying<W> {
    inner: W,
    verifier: Option<StreamingVerifier>,
}

impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Verifying<W> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let polled = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = polled {
            if let Some(v) = self.verifier.as_mut() {
                v.update(buf.get(..n).unwrap_or_default());
            }
        }
        polled
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// `Ok(())` when there was nothing to check or the bytes match.
fn check(verifier: Option<StreamingVerifier>, what: &str) -> Result<()> {
    match verifier.map(StreamingVerifier::finish) {
        None | Some(IntegrityOutcome::Verified { .. }) => Ok(()),
        Some(IntegrityOutcome::Mismatch {
            expected, actual, ..
        }) => Err(CliError::new(
            ExitCode::Failure,
            format!("{what} does not match its metadata: expected {expected}, got {actual}"),
        )
        .into()),
        Some(other) => Err(CliError::new(
            ExitCode::Failure,
            format!("{what} could not be verified: {other:?}"),
        )
        .into()),
    }
}

/// Download through the proxy cache. A cache miss is fetched from upstream
/// and **cached**, so this doubles as an on-demand warm.
pub async fn run(
    args: DownloadArgs,
    client: &BatleHubClient,
    default_registry: Option<&str>,
) -> Result<()> {
    let plan = plan(&args.target, client, default_registry).await?;
    let verifier = plan.expected.as_deref().and_then(StreamingVerifier::new);
    let out_name = args.output.clone().unwrap_or_else(|| plan.file.clone());

    if out_name == "-" {
        if std::io::stdout().is_terminal() {
            return Err(CliError::new(
                ExitCode::Usage,
                "refusing to write an artifact to a terminal",
            )
            .hint("pass -o <file>, or redirect stdout")
            .into());
        }
        // The bytes are already out by the time they are checked: a mismatch
        // is reported after the last byte, and the exit code is the signal.
        let mut out = Verifying {
            inner: tokio::io::stdout(),
            verifier,
        };
        let n = client.download_to(&plan.path, &mut out).await?;
        check(out.verifier.take(), "the download")?;
        eprintln!("Downloaded {n} bytes");
        return Ok(());
    }

    if std::path::Path::new(&out_name).exists() {
        crate::confirm::confirm("overwrite", std::slice::from_ref(&out_name), None)?;
    }
    // Written beside, renamed only on a match: an interrupted or refused
    // download never leaves a file that looks like the artifact.
    let part = format!("{out_name}.part");
    let file = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("creating '{part}'"))?;
    let mut out = Verifying {
        inner: file,
        verifier,
    };
    let fetched = tokio::select! {
        r = client.download_to(&plan.path, &mut out) => r,
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("interrupted")),
    };
    let outcome = fetched.and_then(|n| check(out.verifier.take(), &out_name).map(|()| n));
    drop(out);
    let n = match outcome {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    tokio::fs::rename(&part, &out_name)
        .await
        .with_context(|| format!("renaming '{part}' to '{out_name}'"))?;
    if crate::render::ctx().structured() {
        crate::render::data(&serde_json::json!({
            "path": out_name, "bytes": n, "verified": plan.expected.is_some()
        }))?;
    } else {
        println!("Downloaded {n} bytes → {out_name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{default_output_name, resolve_target};

    #[test]
    fn output_name_is_last_path_segment() {
        assert_eq!(
            default_output_name("/proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz"),
            "idea-2026.1.3.tar.gz"
        );
    }

    #[test]
    fn output_name_strips_query_and_fragment() {
        assert_eq!(
            default_output_name("https://h/a/b/file.zip?sig=abc#x"),
            "file.zip"
        );
    }

    #[test]
    fn output_name_ignores_trailing_slash() {
        assert_eq!(default_output_name("https://h/a/b/"), "b");
    }

    #[test]
    fn output_name_falls_back_to_download() {
        assert_eq!(default_output_name(""), "download");
    }

    #[test]
    fn full_url_is_used_verbatim() {
        let u = "https://h/proxy/jb/jetbrains/idea/x.tar.gz";
        assert_eq!(resolve_target(u, None).unwrap(), u);
    }

    #[test]
    fn absolute_path_is_used_verbatim() {
        let p = "/proxy/jb/jetbrains/idea/x.tar.gz";
        assert_eq!(resolve_target(p, Some("ignored")).unwrap(), p);
    }

    #[test]
    fn relative_path_expands_against_default_registry() {
        assert_eq!(
            resolve_target("jetbrains/idea/x.tar.gz", Some("jb")).unwrap(),
            "/proxy/jb/jetbrains/idea/x.tar.gz"
        );
    }

    #[test]
    fn relative_path_without_registry_errors() {
        assert!(resolve_target("jetbrains/idea/x.tar.gz", None).is_err());
    }
}
