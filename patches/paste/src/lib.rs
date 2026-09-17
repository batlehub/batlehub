//! Shim over [`pastey`], substituted for the crates.io `paste` crate by the
//! `[patch.crates-io]` block in the workspace `Cargo.toml`.
//!
//! `paste` was archived by its author and carries RUSTSEC-2024-0436
//! (unmaintained, "no safe upgrade is available"). Its one holder here is
//! `tikv-jemalloc-ctl`, which is at its latest release (0.7.0) and still
//! depends on `paste = "1"`, so the advisory cannot be upgraded away — and the
//! stance in `deny.toml` / `.cargo/audit.toml` is that nothing is ignored.
//!
//! `pastey` is the fork RustSec names as the drop-in replacement, and the whole
//! of `tikv-jemalloc-ctl`'s use is `[<$id _mib>]` identifier concatenation
//! (`src/macros.rs`) — no `$byte_string` mallctl key passes through it, so the
//! keys the stats reader looks up are unchanged. A mismatch would be a name
//! resolution error at compile time, not a wrong key at run time.

pub use pastey::{expr, item, paste};
