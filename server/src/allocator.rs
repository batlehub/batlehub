//! The allocator's own accounting, as gauges.
//!
//! RSS answers "how much memory does this process hold" and cannot answer "who
//! is holding it". Those are different questions whenever the allocator keeps
//! pages the program has already freed — which jemalloc does by design, and
//! which is exactly what a leak looks like from outside:
//!
//! | | grows | means |
//! | --- | --- | --- |
//! | `allocated` | the program is holding more objects | a leak |
//! | `resident` alone | the allocator is holding more pages | fragmentation or decay |
//!
//! Measured here once, at cost: a soak reported +16.6 % idle RSS and 4 MiB/min
//! against a server whose live heap never moved, and settling that took four
//! ten-minute runs and two seventeen-minute builds because the process could
//! not be asked. It can now.
//!
//! The five series are jemalloc's, renamed only by prefix:
//!
//! - `allocated` — bytes in live allocations. **This is the leak signal.**
//! - `active` — bytes in pages containing at least one live allocation; the
//!   difference from `allocated` is what fragmentation costs.
//! - `resident` — bytes in physically resident pages, the allocator's own view
//!   of what `/proc` calls RSS.
//! - `mapped` — bytes in mappings, resident or not.
//! - `retained` — address space that was mapped, is no longer used, and has not
//!   been returned to the OS. Virtual, not resident: a large `retained` is
//!   jemalloc keeping its options open, not memory being wasted.
//!
//! Without the `jemalloc` feature the sampler is a no-op rather than an error —
//! a build on the system allocator has no equivalent to report, and a metric
//! that exists but is always zero is worse than one that is absent.

/// How often the allocator gauges are re-sampled.
///
/// Five seconds rather than the pool sampler's fifteen, because this is what a
/// leak gate reads: `perf/scripts/soak.sh` samples once a second over ten
/// minutes, and a gauge refreshed every fifteen would hand it a staircase to
/// fit a trend through. Each tick advances jemalloc's stats epoch, which
/// aggregates per-arena counters — microseconds, and the reason this is not
/// done per scrape.
#[cfg(feature = "jemalloc")]
const ALLOCATOR_GAUGE_INTERVAL_SECS: u64 = 5;

/// Periodically publish jemalloc's statistics as gauges.
#[cfg(feature = "jemalloc")]
pub(super) fn spawn_allocator_gauge_sampler() {
    use tikv_jemalloc_ctl::{epoch, stats};

    // Every handle is resolved once here rather than per tick: a `mib` is the
    // numeric path to a mallctl, and looking it up by name on every sample
    // would be a string parse to read a counter. A failure is a build whose
    // jemalloc was compiled without `--enable-stats`, which the `stats` feature
    // in `Cargo.toml` is there to prevent — so it is worth a warning and not
    // worth taking the process down for.
    let handles = (|| -> Result<_, tikv_jemalloc_ctl::Error> {
        Ok((
            epoch::mib()?,
            stats::allocated::mib()?,
            stats::active::mib()?,
            stats::resident::mib()?,
            stats::mapped::mib()?,
            stats::retained::mib()?,
        ))
    })();
    let (epoch_mib, allocated, active, resident, mapped, retained) = match handles {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "allocator metrics unavailable — jemalloc built without stats");
            return;
        }
    };

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
            ALLOCATOR_GAUGE_INTERVAL_SECS,
        ));
        loop {
            ticker.tick().await;
            // jemalloc's counters are cached and only refreshed when the epoch
            // advances. Without this every gauge reports the values from
            // process start, for the life of the process — which looks like a
            // server that allocates nothing.
            if let Err(e) = epoch_mib.advance() {
                tracing::warn!(error = %e, "allocator metrics: epoch advance failed");
                continue;
            }
            for (name, value) in [
                ("batlehub_memory_allocated_bytes", allocated.read()),
                ("batlehub_memory_active_bytes", active.read()),
                ("batlehub_memory_resident_bytes", resident.read()),
                ("batlehub_memory_mapped_bytes", mapped.read()),
                ("batlehub_memory_retained_bytes", retained.read()),
            ] {
                match value {
                    Ok(bytes) => metrics::gauge!(name).set(bytes as f64),
                    Err(e) => {
                        tracing::warn!(metric = name, error = %e, "allocator metrics: read failed")
                    }
                }
            }
        }
    });
}

/// No allocator statistics on a build that uses the system allocator.
#[cfg(not(feature = "jemalloc"))]
pub(super) fn spawn_allocator_gauge_sampler() {}
