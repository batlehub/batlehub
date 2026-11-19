//! Shim over memmap2 0.9, substituted for the crates.io `memmap2` 0.5 line by
//! the `[patch.crates-io]` block in the workspace `Cargo.toml`.
//!
//! memmap2 < 0.9.11 carries RUSTSEC-2026-0186 (`advise_range` / `flush_range`
//! pass an unchecked offset to `pointer::add`). Its one holder here is
//! `pprof` 0.15.0 — the latest release, still on `memmap2 = "0.5"`, and only
//! behind the server's `profiling` feature — so the advisory cannot be
//! upgraded away, and `deny.toml` / `.cargo/audit.toml` ignore nothing.
//!
//! pprof's whole use is `memmap2::Mmap::map(&file)` and `Deref<[u8]>` in
//! `backtrace/framehop_unwinder/shlib.rs`, whose 0.9 signature is the 0.5 one.
//! A mismatch would be a compile error under `--features profiling`, which is
//! how this shim is checked.

pub use memmap2_09::*;
