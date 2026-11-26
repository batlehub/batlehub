//! Shim over cookie 0.18, substituted for the crates.io `cookie` 0.16 line by
//! the `[patch.crates-io]` block in the workspace `Cargo.toml`.
//!
//! cookie < 0.18.3 carries RUSTSEC-2026-0594 (`PrivateJar` AES-GCM nonces can
//! repeat across `fork()`ed processes). Its one holder here is `actix-web`
//! 4.20.0 — the latest release, still on `cookie = "^0.16"` — so the advisory
//! cannot be upgraded away, and `deny.toml` / `.cargo/audit.toml` ignore
//! nothing. actix-web enables `percent-encode` only, so the vulnerable
//! `PrivateJar` was never compiled; the shim removes the advisory, not a live
//! exposure.
//!
//! actix-web's use (request-cookie parsing, `HttpResponse` cookie setters) is
//! source-compatible with 0.18: a mismatch would be a compile error in
//! actix-web, which is how this shim is checked. Drop it when actix-web moves
//! to cookie ≥ 0.18.3.

pub use cookie_018::*;
