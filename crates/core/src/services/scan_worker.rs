//! The worker role (RFC 0018 §5.4): lease a job, run every configured
//! scanner that speaks the registry's kind, judge, record, close.
//!
//! The two roles share nothing but the database. This loop never answers
//! HTTP, and the proxy never blocks a request on it: a dead or saturated
//! worker leaves versions below `mature_age_secs` refused with `SCAN_PENDING`
//! and versions above served `warned` — it degrades, it does not fail.
//!
//! Jobs are leased, not consumed. A lease that expires while a scanner is
//! still running returns the job to the queue; after `max_attempts` the job
//! is closed and the verdict carries `SCANNER_ERROR`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::entities::{
    coordinate_purl, Finding, FindingKind, PackageMetadata, ReasonCode, RegistryKind, ScanJob,
    SecurityPolicy, Severity,
};
use crate::error::CoreError;
use crate::ports::{ArtifactScanner, SbomRepository, ScanQueue, ScannerError, WorkerRegistry};
use crate::services::hot_config::HotConfigLock;
use crate::services::verdict::VerdictService;

/// How the worker is tuned (`[worker]`, RFC 0018 §4.1).
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub max_concurrent: u32,
    /// Empty means every registry.
    pub registries: Vec<String>,
    pub job_timeout: Duration,
    pub max_attempts: u32,
    /// How long to sleep when the queue is empty.
    pub idle_poll: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            worker_id: format!("worker-{}", uuid::Uuid::new_v4()),
            max_concurrent: 4,
            registries: Vec::new(),
            job_timeout: Duration::from_secs(600),
            max_attempts: 3,
            idle_poll: Duration::from_secs(2),
        }
    }
}

pub struct ScanWorker {
    pub config: WorkerConfig,
    pub queue: Arc<dyn ScanQueue>,
    pub verdicts: Arc<VerdictService>,
    pub workers: Option<Arc<dyn WorkerRegistry>>,
    pub sboms: Option<Arc<dyn SbomRepository>>,
    /// The hot config: which registries have a `[security]` profile, their
    /// kinds, and their internal scanners.
    pub hot: HotConfigLock,
    /// The scanners built from `[scanners]`, by name.
    pub scanners: HashMap<String, Arc<dyn ArtifactScanner>>,
    /// The artifact cache, for the scanners that read bytes (phase 3): a
    /// cached artifact is read from here, an uncached one is fetched from
    /// upstream through the registry's client and *not* cached — the proxy
    /// path owns that, with its integrity checks.
    pub storage: Option<Arc<dyn crate::ports::StorageBackend>>,
    /// The most bytes the worker will hold for one scan; the proxy's
    /// `max_artifact_size_bytes`, or 500 MiB.
    pub max_artifact_bytes: u64,
}

/// What one pass over the queue did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassReport {
    pub leased: usize,
    pub completed: usize,
    pub failed: usize,
    pub exhausted: usize,
}

impl ScanWorker {
    /// Lease and run one batch, then close out any job whose attempts are
    /// spent. Returns what happened so an embedded caller can pace itself.
    pub async fn run_once(&self) -> Result<PassReport, CoreError> {
        let mut report = PassReport::default();
        if let Some(w) = &self.workers {
            if let Err(e) = w
                .heartbeat(&self.config.worker_id, &self.config.registries)
                .await
            {
                tracing::warn!(error = %e, "security worker: heartbeat failed");
            }
        }
        if let Ok(counts) = self.queue.queued().await {
            for c in counts {
                metrics::gauge!(
                    "batlehub_scan_jobs_queued",
                    "registry" => c.registry,
                    "trigger" => c.trigger.as_str(),
                )
                .set(c.count as f64);
            }
        }

        let jobs = self
            .queue
            .lease(
                &self.config.worker_id,
                &self.config.registries,
                self.config.max_concurrent,
                self.config.job_timeout.as_secs(),
                self.config.max_attempts,
            )
            .await?;
        report.leased = jobs.len();
        metrics::gauge!("batlehub_scan_jobs_leased", "worker" => self.config.worker_id.clone())
            .set(jobs.len() as f64);

        for job in jobs {
            match self.run_job(&job).await {
                Ok(()) => {
                    report.completed += 1;
                    if let Err(e) = self.queue.complete(job.id).await {
                        tracing::warn!(job = %job.id, error = %e, "security worker: could not close job");
                    }
                }
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(job = %job.id, package = %job.package, error = %e, "security worker: job failed");
                    if let Err(e) = self.queue.fail(job.id, &e.to_string()).await {
                        tracing::warn!(job = %job.id, error = %e, "security worker: could not record failure");
                    }
                }
            }
        }

        // Jobs nobody could finish: the verdict says so, and the row closes.
        for job in self
            .queue
            .exhausted(self.config.max_attempts, self.config.max_concurrent)
            .await?
        {
            report.exhausted += 1;
            self.close_exhausted(&job).await;
        }
        Ok(report)
    }

    /// Run forever, pacing on the queue.
    pub async fn run(self: Arc<Self>) {
        loop {
            match self.run_once().await {
                Ok(r) if r.leased == 0 && r.exhausted == 0 => {
                    tokio::time::sleep(self.config.idle_poll).await
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "security worker: pass failed; retrying");
                    tokio::time::sleep(self.config.idle_poll).await;
                }
            }
        }
    }

    /// The registry's profile and kind, and the scanners to run for it.
    async fn plan(
        &self,
        registry: &str,
    ) -> Option<(SecurityPolicy, RegistryKind, Vec<Arc<dyn ArtifactScanner>>)> {
        let hot = self.hot.read().await;
        let policy = hot.security.get(registry)?.clone();
        let kind: RegistryKind = hot.registries.get(registry)?.registry_type().parse().ok()?;
        let mut scanners: Vec<Arc<dyn ArtifactScanner>> = hot
            .internal_scanners
            .get(registry)
            .cloned()
            .unwrap_or_default();
        for name in &policy.scanners {
            if let Some(s) = self.scanners.get(name) {
                scanners.push(Arc::clone(s));
            }
        }
        Some((policy, kind, scanners))
    }

    async fn run_job(&self, job: &ScanJob) -> Result<(), CoreError> {
        let Some((policy, kind, scanners)) = self.plan(&job.package.registry).await else {
            // The registry left `[security]` (or the config) since the job
            // was queued: nothing to judge, and nothing to hold.
            tracing::info!(package = %job.package, "security worker: registry has no security profile any more; dropping job");
            return Ok(());
        };
        let mut package = PackageMetadata::minimal(job.package.clone(), serde_json::Value::Null);
        package.published_at = job.published_at;
        let purl = coordinate_purl(kind, &job.package.name, &job.package.version);
        let sbom = match &self.sboms {
            Some(repo) => repo
                .get_sbom_by_coordinates(
                    &job.package.registry,
                    &job.package.name,
                    &job.package.version,
                    &crate::entities::SbomFormat::CycloneDx,
                )
                .await
                .ok()
                .flatten()
                .map(|s| s.document),
            None => None,
        };
        let applicable: Vec<Arc<dyn ArtifactScanner>> =
            scanners.into_iter().filter(|s| s.supports(kind)).collect();
        // Bytes and the listing only when a scanner will read them: a
        // metadata-only profile is one row read and no egress.
        let mut findings: Vec<Finding> = Vec::new();
        let artifact = if applicable.iter().any(|s| s.needs_artifact()) {
            match self.artifact_bytes(&job.package, kind).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(package = %job.package, error = %e, "security worker: could not fetch the artifact for scanning");
                    None
                }
            }
        } else {
            None
        };
        let artifact_unavailable = artifact.is_none();
        let listing = if applicable.iter().any(|s| s.needs_listing()) {
            self.listing_document(&job.package).await
        } else {
            None
        };
        let input = crate::ports::ScanInput {
            package: package.clone(),
            kind,
            purl,
            artifact,
            sbom,
            listing,
        };

        let mut done: Vec<String> = Vec::new();
        for scanner in applicable {
            // A scanner that reads bytes it could not be given did not run:
            // `SCANNER_UNSUPPORTED` names the reason without pretending the
            // scan happened (RFC 0018 §11 q1).
            if scanner.needs_artifact() && artifact_unavailable {
                findings.push(Finding::new(
                    scanner.name(),
                    FindingKind::ScannerError,
                    ReasonCode::ScannerUnsupported,
                    Severity::High,
                    "the artifact bytes could not be obtained for scanning",
                ));
                continue;
            }
            let started = Instant::now();
            let outcome = tokio::time::timeout(self.config.job_timeout, scanner.scan(&input)).await;
            let outcome = match outcome {
                Ok(r) => r,
                Err(_) => Err(ScannerError::Timeout),
            };
            let label = match &outcome {
                Ok(_) => "ok",
                Err(_) => "error",
            };
            metrics::histogram!(
                "batlehub_scan_job_duration_seconds",
                "registry" => job.package.registry.clone(),
                "scanner" => scanner.name().to_owned(),
                "outcome" => label,
            )
            .record(started.elapsed().as_secs_f64());
            match outcome {
                Ok(found) => {
                    findings.extend(found);
                    done.push(scanner.name().to_owned());
                }
                Err(e) => {
                    metrics::counter!(
                        "batlehub_scanner_errors_total",
                        "scanner" => scanner.name().to_owned(),
                        "class" => e.class(),
                    )
                    .increment(1);
                    tracing::warn!(package = %job.package, scanner = scanner.name(), error = %e, "security worker: scanner did not answer");
                    let code = match e {
                        ScannerError::Unsupported(_) => ReasonCode::ScannerUnsupported,
                        _ => ReasonCode::ScannerError,
                    };
                    findings.push(Finding::new(
                        scanner.name(),
                        FindingKind::ScannerError,
                        code,
                        Severity::High,
                        e.to_string(),
                    ));
                }
            }
            if let Err(e) = self
                .queue
                .heartbeat(job.id, self.config.job_timeout.as_secs())
                .await
            {
                tracing::debug!(job = %job.id, error = %e, "security worker: heartbeat failed");
            }
        }

        let (from, verdict) = self
            .verdicts
            .record_scan(&package, &policy, findings, done, Utc::now())
            .await?;
        metrics::counter!(
            "batlehub_verdicts_total",
            "registry" => job.package.registry.clone(),
            "state" => verdict.state.as_str(),
            "trigger" => job.trigger.as_str(),
        )
        .increment(1);
        tracing::info!(
            package = %job.package,
            from = ?from,
            to = %verdict.state,
            codes = ?verdict.reason_codes,
            "security worker: verdict recorded"
        );
        Ok(())
    }

    /// The bytes of the version's primary artifact — the one file the kind
    /// names for a version (`RegistryKind::warm_artifact`) — from the cache
    /// when it is there, else from upstream. `None` for a kind that names a
    /// set of files (PyPI, Maven, conda, Terraform), which the archive
    /// scanners answer `SCANNER_UNSUPPORTED` for.
    async fn artifact_bytes(
        &self,
        package: &crate::entities::PackageId,
        kind: RegistryKind,
    ) -> Result<Option<bytes::Bytes>, CoreError> {
        use futures::StreamExt;
        let Some(coordinate) =
            kind.fetch_coordinate(&package.registry, &package.name, &package.version)
        else {
            return Ok(None);
        };
        let limit = self.max_artifact_bytes;
        let key = format!("artifact:{}", coordinate.cache_key());
        let mut stream = match &self.storage {
            Some(storage) => match storage.retrieve(&key).await? {
                Some(stored) => Some(stored.stream),
                None => None,
            },
            None => None,
        };
        if stream.is_none() {
            let client = {
                let hot = self.hot.read().await;
                hot.registries.get(&package.registry).cloned()
            };
            let Some(client) = client else {
                return Ok(None);
            };
            stream = Some(client.fetch_artifact(&coordinate).await?.stream);
        }
        let mut stream = stream.expect("set above");
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if buf.len() as u64 + chunk.len() as u64 > limit {
                return Err(CoreError::PayloadTooLarge(format!(
                    "artifact exceeds the {limit}-byte scan limit"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(Some(bytes::Bytes::from(buf)))
    }

    /// The registry's listing document for the package, or nothing — a
    /// scanner that needs it says so in its own finding.
    async fn listing_document(
        &self,
        package: &crate::entities::PackageId,
    ) -> Option<crate::ports::VersionDocument> {
        let client = {
            let hot = self.hot.read().await;
            hot.registries.get(&package.registry).cloned()
        }?;
        client
            .fetch_version_document(&package.name, crate::ports::DocumentKind::Versions)
            .await
            .ok()
    }

    /// A job whose attempts are spent: every required scanner that never
    /// answered becomes `SCANNER_ERROR`, and the row closes.
    async fn close_exhausted(&self, job: &ScanJob) {
        metrics::counter!(
            "batlehub_scan_jobs_expired_total",
            "registry" => job.package.registry.clone(),
            "scanner" => "job",
        )
        .increment(1);
        if let Some((policy, _, _)) = self.plan(&job.package.registry).await {
            let mut package =
                PackageMetadata::minimal(job.package.clone(), serde_json::Value::Null);
            package.published_at = job.published_at;
            let findings = policy
                .required_scanners
                .iter()
                .map(|s| {
                    Finding::new(
                        s.clone(),
                        FindingKind::ScannerError,
                        ReasonCode::ScannerError,
                        Severity::High,
                        format!(
                            "no attempt out of {} completed within {}s",
                            self.config.max_attempts,
                            self.config.job_timeout.as_secs()
                        ),
                    )
                })
                .collect();
            let done = policy.required_scanners.clone();
            if let Err(e) = self
                .verdicts
                .record_scan(&package, &policy, findings, done, Utc::now())
                .await
            {
                tracing::warn!(package = %job.package, error = %e, "security worker: could not record SCANNER_ERROR");
            }
        }
        if let Err(e) = self.queue.complete(job.id).await {
            tracing::warn!(job = %job.id, error = %e, "security worker: could not close exhausted job");
        }
    }
}
