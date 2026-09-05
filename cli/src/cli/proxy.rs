//! `batlehub-cli proxy serve` — run the local gallery proxy (RFC 0011 §4.4).
//!
//! The server itself is `crate::gallery_proxy`; this is the command: the
//! loopback-only bind, the session secret, the state file the workspace
//! startup script reads the gallery URL from, and the lines printed once.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};

use crate::contract;
use crate::gallery_proxy;

#[derive(Subcommand)]
pub enum ProxyCommand {
    /// Run a loopback gallery proxy in front of one BatleHub VSX registry
    ///
    /// The editor's `extensionsGallery.serviceUrl` points at the URL this
    /// prints; the proxy attaches the credential from the contract file
    /// (`auth write-token-file`) so the editor never holds one, and while
    /// there is none it answers a search with a single sign-in entry.
    Serve(ServeArgs),
}

#[derive(Args)]
pub struct ServeArgs {
    /// The registry base on the BatleHub instance, e.g.
    /// `https://hub.example/proxy/vsx`.
    #[arg(long)]
    pub registry: String,
    /// Where to listen. Loopback only: any other address is refused.
    #[arg(long, default_value = "127.0.0.1:0")]
    pub bind: SocketAddr,
    /// The contract file to read the credential from. Defaults to the one
    /// `auth write-token-file` writes.
    #[arg(long)]
    pub contract: Option<PathBuf>,
    /// Where to write `gallery-proxy.json` (the URL and the session, mode
    /// 0600). Defaults to `$BATLEHUB_HOME/state`.
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
    /// Print the gallery URL and nothing else on stdout, for a startup
    /// script that captures it.
    #[arg(long)]
    pub print_gallery_url: bool,
}

pub async fn run(cmd: ProxyCommand) -> Result<()> {
    match cmd {
        ProxyCommand::Serve(args) => serve(args).await,
    }
}

/// RFC 0011 §4.4.1: loopback or nothing. The port protects nothing in a
/// pod, the path does; a bind that other hosts could reach would make the
/// session segment the only wall, and it was never meant to stand alone.
fn ensure_loopback(addr: &SocketAddr) -> Result<()> {
    let loopback = match addr.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    };
    if !loopback {
        bail!(
            "refusing to bind {addr}: the gallery proxy serves a credential and listens on \
             loopback only (RFC 0011 §4.4.1)"
        );
    }
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<()> {
    ensure_loopback(&args.bind)?;
    let contract_path = args.contract.unwrap_or_else(contract::contract_path);
    let state_dir = args
        .state_dir
        .unwrap_or_else(|| contract::batlehub_home().join("state"));
    let session = gallery_proxy::new_session();

    // Bind first, so the port is known before anything is printed or written.
    let listener = std::net::TcpListener::bind(args.bind)
        .with_context(|| format!("binding the gallery proxy on {}", args.bind))?;
    let local = listener.local_addr()?;
    let capability_base = format!("http://{local}/{session}/vsx");
    let state = Arc::new(gallery_proxy::state_for(
        &args.registry,
        &capability_base,
        session.clone(),
        contract_path.clone(),
    )?);

    write_state_file(&state_dir, &capability_base, &session, &args.registry)?;

    if args.print_gallery_url {
        println!("{capability_base}");
    } else {
        println!("gallery proxy for {} on {local}", state.registry_base);
        println!("  extensionsGallery.serviceUrl = {capability_base}/vscode/gallery");
        println!("  credential: {}", contract_path.display());
        println!(
            "  state:      {}",
            state_dir.join("gallery-proxy.json").display()
        );
        match state.credential() {
            Some(_) => println!("  signed in: requests carry the credential"),
            None => println!("  not signed in: a search shows the sign-in entry until you are"),
        }
    }

    let data = actix_web::web::Data::new(Arc::clone(&state));
    actix_web::HttpServer::new(move || {
        actix_web::App::new()
            .app_data(data.clone())
            .configure(gallery_proxy::configure)
    })
    .workers(2)
    .listen(listener)?
    .run()
    .await?;
    Ok(())
}

/// `gallery-proxy.json`, mode 0600: what a startup script or the TUI reads
/// to point the editor at this process. The session is in the URL, so the
/// file is as secret as the URL.
fn write_state_file(dir: &std::path::Path, url: &str, session: &str, registry: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("gallery-proxy.json");
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "gallery_url": url,
        "service_url": format!("{url}/vscode/gallery"),
        "session": session,
        "registry": registry,
        "pid": std::process::id(),
    }))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    std::io::Write::write_all(&mut f, &body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_binds_are_accepted() {
        assert!(ensure_loopback(&"127.0.0.1:0".parse().unwrap()).is_ok());
        assert!(ensure_loopback(&"[::1]:0".parse().unwrap()).is_ok());
        assert!(ensure_loopback(&"0.0.0.0:0".parse().unwrap()).is_err());
        assert!(ensure_loopback(&"10.0.0.5:8080".parse().unwrap()).is_err());
        assert!(ensure_loopback(&"[::]:0".parse().unwrap()).is_err());
    }

    #[test]
    fn the_state_file_is_private_and_names_the_service_url() {
        let dir = tempfile::tempdir().unwrap();
        write_state_file(
            dir.path(),
            "http://127.0.0.1:1/s/vsx",
            "s",
            "http://h/proxy/vsx",
        )
        .unwrap();
        let path = dir.path().join("gallery-proxy.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["service_url"], "http://127.0.0.1:1/s/vsx/vscode/gallery");
        assert_eq!(v["session"], "s");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{mode:o}");
        }
    }
}
