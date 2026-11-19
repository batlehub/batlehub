pub mod in_memory;
pub mod postgres;

pub use in_memory::InMemoryLocalRegistry;
pub use postgres::PostgresLocalRegistry;

use std::time::Duration;

use batlehub_core::error::CoreError;

/// A pending row older than this is a crashed publish and may be replaced; a
/// younger one is a publish in flight. Bytes are in memory by then, so the store
/// step takes seconds; this only has to outlast a slow object store.
pub(crate) const STALE_PENDING: Duration = Duration::from_secs(15 * 60);

/// `commit_publish` found no pending row of ours: it went stale and was replaced
/// or removed, so promoting whatever is there would publish another caller's row.
pub(crate) fn lost_reservation(registry: &str, name: &str, version: &str) -> CoreError {
    CoreError::Conflict(format!(
        "{name}@{version} in registry '{registry}': the publish lost its reservation \
         to a concurrent publish of the same version"
    ))
}
