//! One module per registry protocol the soak drives.
//!
//! The split is by *protocol*, not by registry kind: `files` serves the five
//! path-shaped kinds at once because the proxy serves them with one client, and
//! `forge` serves three because the three forges differ only in spelling. A
//! module is added when a kind needs a document shape none of the others emit.
//!
//! **Every route answers `HEAD` as well as `GET`**, which is why they are
//! `#[route(..., method = "GET", method = "HEAD")]` rather than `#[get]`.
//! actix does not derive one from the other, and two of the proxy's clients ask
//! `HEAD` before streaming — rustup and nodedist both check whether an artifact
//! exists rather than reading a 200 MB tarball to answer a yes/no question. A
//! mock that answers `GET` only makes every one of those reads a 404 that looks
//! like a missing release.

mod cargo;
mod composer;
pub mod conda;
mod files;
mod forge;
mod galaxy;
mod goproxy;
mod marketplaces;
mod maven;
mod nodedist;
mod npm;
mod nuget;
mod openvsx;
mod pypi;
mod rubygems;
mod rustup;
mod sdkman;
mod terraform;

/// Mount every protocol. One call site, so a module that is written and not
/// wired is a compile error rather than a route that 404s under load.
pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cargo::configure(cfg);
    composer::configure(cfg);
    conda::configure(cfg);
    files::configure(cfg);
    forge::configure(cfg);
    galaxy::configure(cfg);
    goproxy::configure(cfg);
    marketplaces::configure(cfg);
    maven::configure(cfg);
    nodedist::configure(cfg);
    npm::configure(cfg);
    nuget::configure(cfg);
    openvsx::configure(cfg);
    pypi::configure(cfg);
    rubygems::configure(cfg);
    rustup::configure(cfg);
    sdkman::configure(cfg);
    terraform::configure(cfg);
}
