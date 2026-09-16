//! The Rust toolchain tree as a typed registry (RFC 0024).

mod client;
mod models;
#[cfg(test)]
mod tests;

pub use client::RustupRegistryClient;
