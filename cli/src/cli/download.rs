use anyhow::{Context, Result};
use clap::Args;

use crate::api::BatleHubClient;

#[derive(Args)]
pub struct DownloadArgs {
    /// What to download. One of:
    ///   - a full URL: https://host/proxy/jb/jetbrains/idea/x.tar.gz
    ///   - a server path: /proxy/jb/jetbrains/idea/x.tar.gz
    ///   - registry-relative (needs -r/--registry): jetbrains/idea/x.tar.gz
    #[arg(verbatim_doc_comment)]
    pub target: String,

    /// Output file. Defaults to the path's basename; use "-" for stdout.
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
pub async fn run(
    args: DownloadArgs,
    client: &BatleHubClient,
    default_registry: Option<&str>,
) -> Result<()> {
    let resolved = resolve_target(&args.target, default_registry)?;

    let out_name = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_name(&resolved));

    if out_name == "-" {
        let mut stdout = tokio::io::stdout();
        let n = client.download_to(&resolved, &mut stdout).await?;
        eprintln!("Downloaded {n} bytes");
    } else {
        let mut file = tokio::fs::File::create(&out_name)
            .await
            .with_context(|| format!("creating output file '{out_name}'"))?;
        let n = client.download_to(&resolved, &mut file).await?;
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
