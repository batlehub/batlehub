//! SDKMAN — the candidates API and the download broker as one registry
//! (RFC 0010 phases 5–7).
//!
//! Two hosts, one protocol. `api.sdkman.io/2` answers every question —
//! which candidates exist, which versions, whether one is valid on a
//! platform, the hook scripts an install runs — and `broker.sdkman.io`
//! answers the one download with a `302` to whichever CDN the vendor
//! publishes on. The client follows that redirect server-side, through the
//! SSRF guard, so the bytes are cached here rather than referred elsewhere
//! (decision 3).
//!
//! Split per the layout rule in `CLAUDE.md`: the request logic in
//! `client.rs`, the protocol's small vocabulary (the validate answer, the
//! relayed-path allow-list, the URL shapes) in `models.rs`, and a `tests.rs`
//! spanning both.

mod client;
mod models;
#[cfg(test)]
mod tests;

pub use client::SdkmanRegistryClient;
pub use models::{DEFAULT_API_BASE, DEFAULT_BROKER_BASE};
