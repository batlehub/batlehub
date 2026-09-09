//! Where a release import's runs are remembered (RFC 0021 §6.5).

use async_trait::async_trait;

use crate::entities::ImportRun;
use crate::error::CoreError;

#[async_trait]
pub trait ImportHistory: Send + Sync {
    /// Record one finished run.
    ///
    /// **Fire-and-forget at the call site**, as `MissRecorder::record` is: an
    /// import that published its versions and then failed to write its own
    /// history row has still done the thing it was asked to do, and turning
    /// that into a failed request would be the tail wagging the dog. The caller
    /// logs and carries on.
    async fn record(&self, run: &ImportRun) -> Result<(), CoreError>;

    /// The newest run of each import configured into `registry`, one per repo,
    /// newest first. Empty when nothing has run.
    async fn latest_for(&self, registry: &str) -> Result<Vec<ImportRun>, CoreError>;
}
