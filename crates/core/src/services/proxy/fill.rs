//! RFC 0048 §15 Q9 — the detached single-flight cold fill — with its bytes
//! in storage (RFC 0048-bis).
//!
//! A cold miss used to store the whole upstream body before serving a byte,
//! so a 475 MB archive behind a slow vendor link outlived the client's read
//! timeout, and each retry started another upstream fetch. Now the fill is a
//! task keyed on the storage key, spawned detached from the request:
//!
//! - the first miss starts it; every concurrent or retried request for the
//!   same key **joins** it and is served the bytes already received, then the
//!   rest as they arrive;
//! - a client that disconnects drops its subscription, never the fill;
//! - on a mismatch, an oversize body or an upstream error the stream ends in
//!   an error instead (the response is reset) and nothing is promoted.
//!
//! The bytes go to storage as [`FILL_SEGMENT_BYTES`] segments of the fill's
//! staging key, then are promoted onto the artifact key once the digest is
//! verified. Readers read committed segments back from storage and the open
//! one from the filler's memory, so a 1 GB fill holds one segment in RAM and
//! writes nothing to local disk. What a reader may see (RFC 0048-bis R6, T3):
//!
//! - in the open segment, every chunk but the **newest**;
//! - segment n only once it is in storage and a byte of segment n+1 arrived
//!   (it is then *committed*); so the last segment — even a full one — is
//!   withheld until the digest is verified, and a client that sees a complete
//!   body saw one whose digest was checked.
//!
//! Who fills a key is a lease ([`FillLeases`]); this process's map of running
//! fills makes every request of one replica share one [`Fill`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use bytes::Bytes;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::error::CoreError;
use crate::ports::segment_key;
use crate::ports::storage::skip_bytes;
use crate::ports::{
    ByteStream, FillError, FillId, FillLeases, InMemoryFillLeases, LeaseState, Phase,
    StorageBackend, StoreOutcome, Taken, FILL_SEGMENT_BYTES, HEARTBEAT_AFTER, STAGING_PREFIX,
};

#[derive(Debug)]
pub(super) struct Progress {
    /// Segments `0..committed` are in storage and may be read.
    committed: u32,
    /// The chunks of segment `committed`, still being filled; every one but
    /// the newest may be read.
    open: Vec<Bytes>,
    pub(super) phase: Phase,
    /// Set once the fill was seen `Verified`: every segment is committed. A
    /// follower can see `Done` without it, and then reads the rest from the
    /// artifact rather than trusting a count it may have missed.
    verified: bool,
}

/// One running fill, shared by every request of this process for its key.
pub(super) struct Fill {
    /// The artifact key the fill promotes onto.
    key: String,
    /// `staging:<fill id>/<key>`, set once the lease is taken and before any
    /// segment is committed.
    staged: OnceLock<String>,
    storage: Arc<dyn StorageBackend>,
    progress: watch::Sender<Progress>,
    /// Fed by a follower of another replica's fill, for the metrics' `scope`.
    remote: AtomicBool,
}

/// The running fills of one process, keyed on the artifact's storage key,
/// and the lease that says which process fills a key.
pub struct FillCoordinator {
    inflight: Mutex<HashMap<String, Arc<Fill>>>,
    leases: Arc<dyn FillLeases>,
    /// This replica's name in a lease, for logs only.
    holder: String,
}

impl Default for FillCoordinator {
    /// The in-process lease of a single replica (RFC 0048-bis R1).
    fn default() -> Self {
        Self::with_leases(Arc::new(InMemoryFillLeases::default()))
    }
}

pub(super) enum Joined {
    /// This request starts the fill and must drive the writer to its end.
    Started(Box<FillWriter>),
    /// A fill for the key is already running.
    Joined(Arc<Fill>),
}

impl FillCoordinator {
    /// `leases`, held under this replica's name (`HOSTNAME`, a pod's name).
    pub fn with_leases(leases: Arc<dyn FillLeases>) -> Self {
        let holder = std::env::var("HOSTNAME").unwrap_or_else(|_| "batlehub".to_owned());
        Self::new(leases, holder)
    }

    pub fn new(leases: Arc<dyn FillLeases>, holder: String) -> Self {
        Self {
            inflight: Mutex::default(),
            leases,
            holder,
        }
    }

    pub(super) fn join_or_start(
        self: &Arc<Self>,
        key: &str,
        storage: &Arc<dyn StorageBackend>,
    ) -> Joined {
        let mut inflight = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(fill) = inflight.get(key) {
            return Joined::Joined(Arc::clone(fill));
        }
        let (progress, _) = watch::channel(Progress {
            committed: 0,
            open: Vec::new(),
            phase: Phase::Filling,
            verified: false,
        });
        let fill = Arc::new(Fill {
            key: key.to_owned(),
            staged: OnceLock::new(),
            storage: Arc::clone(storage),
            progress,
            remote: AtomicBool::new(false),
        });
        inflight.insert(key.to_owned(), Arc::clone(&fill));
        Joined::Started(Box::new(FillWriter {
            fill,
            coordinator: Arc::clone(self),
            lease: None,
            registry: Arc::from(""),
            active: false,
            staged: String::new(),
            written: 0,
            open_len: 0,
            hasher: Sha256::new(),
            size: 0,
            extended: Instant::now(),
            ended: false,
        }))
    }

    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        self.inflight.lock().map(|m| m.is_empty()).unwrap_or(false)
    }

    fn release(&self, key: &str, fill: &Arc<Fill>) {
        let mut inflight = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if inflight.get(key).is_some_and(|f| Arc::ptr_eq(f, fill)) {
            inflight.remove(key);
        }
    }
}

/// The advisory lock that elects the one replica running [`sweep_fills`].
pub const FILL_SWEEP_LEADER_KEY: i64 = 0x0048_b15e_e000;

/// Staging older than this is an orphan no live fill can still read.
const STALE_STAGING: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// RFC 0048-bis T6: delete what dead fills left — the segments of every lease
/// that expired, and its row — then any staging a day old, which no lease
/// names (a process killed between a segment's `PUT` and its commit, a
/// crashed single replica, the router's own staging). Returns how many
/// segments and staging entries went; one replica runs it at a time.
/// `lease_ttl` is the leases' ([`LEASE_TTL`](crate::ports::LEASE_TTL) but in tests).
pub async fn sweep_fills(
    leases: &dyn FillLeases,
    storage: &dyn StorageBackend,
    lease_ttl: std::time::Duration,
) -> usize {
    let mut deleted = 0usize;
    match leases.expired(lease_ttl).await {
        Ok(expired) => {
            for (key, fill, written) in expired {
                let staged = format!("{STAGING_PREFIX}{fill}/{key}");
                match storage.delete_segments(&staged, written).await {
                    Ok(()) => {
                        deleted += written as usize;
                        // Fenced: a takeover since `expired` keeps its row.
                        if let Err(e) = leases.release(&key, fill).await {
                            tracing::warn!(%key, error = %e, "fill sweep: release failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(%key, %fill, error = %e, "fill sweep: deleting segments failed")
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "fill sweep: listing expired leases failed"),
    }
    match storage.delete_stale_staging(STALE_STAGING).await {
        Ok(n) => deleted += n,
        Err(e) => tracing::warn!(error = %e, "fill sweep: deleting stale staging failed"),
    }
    if deleted > 0 {
        metrics::counter!("batlehub_proxy_fill_orphans_deleted_total").increment(deleted as u64);
        tracing::info!(
            deleted,
            "fill sweep: deleted the leftovers of dead cold fills"
        );
    }
    deleted
}

/// The starting request's handle on its fill: cuts what upstream sends into
/// segments, commits them, then says how the fill ended. Dropped without an
/// end, the fill fails rather than leaving its readers waiting forever.
pub(super) struct FillWriter {
    fill: Arc<Fill>,
    coordinator: Arc<FillCoordinator>,
    /// The lease this fill holds; `None` when it fills without one (the lease
    /// store failed, R10, or another holder has it and nothing follows it yet).
    lease: Option<FillId>,
    /// The registry label of the metrics this fill moves.
    registry: Arc<str>,
    /// Counted in `batlehub_proxy_fill_active`.
    active: bool,
    staged: String,
    /// Segments that may exist in storage, the ones to delete on failure.
    written: u32,
    /// Bytes in the open segment.
    open_len: usize,
    hasher: Sha256,
    size: u64,
    /// When the lease was last extended.
    extended: Instant,
    ended: bool,
}

impl FillWriter {
    pub(super) fn fill(&self) -> Arc<Fill> {
        Arc::clone(&self.fill)
    }

    /// The staging key the segments are written under.
    pub(super) fn staged(&self) -> &str {
        &self.staged
    }

    /// Take the lease and name the fill. Called once, before any byte.
    pub(super) async fn start(&mut self, registry: Arc<str>) -> Start {
        self.registry = registry;
        let key = self.fill.key.clone();
        let leases = Arc::clone(&self.coordinator.leases);
        let id = match leases.take(&key, &self.coordinator.holder).await {
            Ok(Taken::Started { fill, previous }) => {
                self.lease = Some(fill);
                self.active = true;
                metrics::gauge!("batlehub_proxy_fill_active").increment(1.0);
                if let Some((old, written)) = previous {
                    // R4: restart from zero; nothing of the dead fill is read again.
                    let old_staged = format!("{STAGING_PREFIX}{old}/{key}");
                    let deleted = self
                        .fill
                        .storage
                        .delete_segments(&old_staged, written)
                        .await;
                    metrics::counter!("batlehub_proxy_fill_takeovers_total", "registry" => Arc::clone(&self.registry)).increment(1);
                    tracing::warn!(%key, previous_fill = %old, fill = %fill, deleted_segments = written, ok = deleted.is_ok(), "cold fill taken over");
                }
                // R5: a fill that ended on another replica between this
                // request's storage miss and its lease left the artifact.
                if leases.shared() {
                    match self.fill.storage.exists(&key).await {
                        Ok(true) => return Start::Cached,
                        Ok(false) => {}
                        Err(e) => {
                            tracing::warn!(%key, error = %e, "storage check under the fill lease failed; filling")
                        }
                    }
                }
                fill
            }
            Ok(Taken::Running(state)) => return Start::Follow(state),
            Err(e) => {
                metrics::counter!("batlehub_proxy_fill_lease_errors_total").increment(1);
                tracing::warn!(%key, error = %e, "fill lease unavailable; filling without it");
                FillId::new_v4()
            }
        };
        self.staged = format!("{STAGING_PREFIX}{id}/{key}");
        let _ = self.fill.staged.set(self.staged.clone());
        Start::Fill
    }

    /// Feed this replica's [`Fill`] from the fill another replica holds (R3):
    /// its readers read the committed segments from storage, and nothing is
    /// fetched here. Ends as that fill ends; a holder that stopped without an
    /// end resets the readers (R7), and the next request takes the lease over
    /// (R4) and fills from zero.
    pub(super) async fn follow(mut self, state: LeaseState) {
        let key = self.fill.key.clone();
        self.staged = format!("{STAGING_PREFIX}{}/{key}", state.fill);
        let _ = self.fill.staged.set(self.staged.clone());
        self.fill.remote.store(true, Ordering::Relaxed);
        metrics::counter!("batlehub_proxy_fill_joined_total", "registry" => Arc::clone(&self.registry), "scope" => "remote").increment(1);
        tracing::debug!(%key, holder = %state.holder, fill = %state.fill, "following the cold fill of another replica");
        let mut updates = self.coordinator.leases.follow(&key, state.fill);
        let mut last = state;
        let end = loop {
            if last.phase.is_terminal() {
                break last.phase;
            }
            let verified = matches!(last.phase, Phase::Verified { .. });
            self.fill.progress.send_modify(|p| {
                p.committed = p.committed.max(last.committed);
                if verified {
                    p.phase = last.phase.clone();
                    p.verified = true;
                }
            });
            match updates.next().await {
                Some(Ok(s)) if s.fill == last.fill => last = s,
                Some(Err(e)) => {
                    tracing::warn!(%key, error = %e, "following a cold fill failed");
                    break self.vanished().await;
                }
                Some(Ok(_)) | None => break self.vanished().await,
            }
        };
        self.finish(end).await;
    }

    /// How a followed fill whose lease went without a word ended: promoted
    /// and released between two wake-ups, or dead.
    async fn vanished(&self) -> Phase {
        match self.fill.storage.exists(&self.fill.key).await {
            Ok(true) => Phase::Done,
            _ => Phase::Failed(FillError::other(
                "the cold fill stopped on the replica filling it",
            )),
        }
    }

    /// Add `chunk` to the fill: hashed, counted, appended to the open segment,
    /// which is put and committed first if it is full (R6: a full segment is
    /// committed only once the next one has a byte).
    pub(super) async fn push(&mut self, mut chunk: Bytes) -> Result<(), CoreError> {
        self.hasher.update(&chunk);
        self.size += chunk.len() as u64;
        if self.lease.is_some() && self.extended.elapsed() >= HEARTBEAT_AFTER {
            self.lease_call(LeaseCall::Heartbeat).await?;
        }
        while !chunk.is_empty() {
            if self.open_len == FILL_SEGMENT_BYTES {
                self.commit_open().await?;
            }
            let part = chunk.split_to((FILL_SEGMENT_BYTES - self.open_len).min(chunk.len()));
            self.open_len += part.len();
            self.fill.progress.send_modify(|p| p.open.push(part));
        }
        Ok(())
    }

    /// Put the open segment as segment `committed`; returns its number.
    /// The chunks are handed over as they are — reference counts, never a
    /// joined copy — so a fill holds one segment of RAM, not two.
    async fn put_open(&mut self) -> Result<u32, CoreError> {
        let (n, parts) = {
            let p = self.fill.progress.borrow();
            (p.committed, p.open.clone())
        };
        self.written = n + 1;
        self.fill
            .storage
            .put_segment_parts(&self.staged, n, parts)
            .await?;
        metrics::counter!("batlehub_proxy_fill_segments_total", "registry" => Arc::clone(&self.registry)).increment(1);
        Ok(n)
    }

    async fn commit_open(&mut self) -> Result<(), CoreError> {
        let n = self.put_open().await?;
        self.lease_call(LeaseCall::Commit(n + 1)).await?;
        self.fill.progress.send_modify(|p| {
            p.committed = n + 1;
            p.open.clear();
        });
        self.open_len = 0;
        Ok(())
    }

    /// The digest was verified: put the last segment and make every segment
    /// readable. Returns the segment count and what was written.
    pub(super) async fn seal(&mut self) -> Result<(u32, StoreOutcome), CoreError> {
        let count = self.put_open().await? + 1;
        let total = self.size;
        self.lease_call(LeaseCall::Finish(Phase::Verified { total }))
            .await?;
        self.fill.progress.send_modify(|p| {
            p.committed = count;
            p.open.clear();
            p.phase = Phase::Verified { total };
            p.verified = true;
        });
        let content_hash = hex::encode(std::mem::take(&mut self.hasher).finalize());
        Ok((
            count,
            StoreOutcome {
                content_hash,
                size: total,
            },
        ))
    }

    /// Say how the fill ended, release the lease, and delete the segments
    /// unless they were promoted.
    pub(super) async fn finish(mut self, phase: Phase) {
        self.ended = true;
        self.deactivate();
        let failed = !matches!(phase, Phase::Done);
        if let Some(id) = self.lease {
            let key = &self.fill.key;
            let leases = &self.coordinator.leases;
            if let Err(e) = leases.finish(key, id, phase.clone()).await {
                tracing::warn!(%key, error = %e, "fill lease: finish failed");
            }
            if let Err(e) = leases.release(key, id).await {
                tracing::warn!(%key, error = %e, "fill lease: release failed");
            }
        }
        self.fill.progress.send_modify(|p| p.phase = phase);
        self.coordinator.release(&self.fill.key, &self.fill);
        if failed && self.written > 0 {
            // Readers were told first; a reader still on a segment is reset.
            if let Err(e) = self
                .fill
                .storage
                .delete_segments(&self.staged, self.written)
                .await
            {
                tracing::warn!(key = %self.staged, error = %e, "failed to delete the segments of a failed fill");
            }
        }
    }

    async fn lease_call(&mut self, call: LeaseCall) -> Result<(), CoreError> {
        let Some(id) = self.lease else {
            return Ok(());
        };
        let (key, leases) = (&self.fill.key, &self.coordinator.leases);
        let r = match call {
            LeaseCall::Commit(n) => leases.commit(key, id, n).await,
            LeaseCall::Heartbeat => leases.heartbeat(key, id).await,
            LeaseCall::Finish(phase) => leases.finish(key, id, phase).await,
        };
        match r {
            Ok(true) => {
                self.extended = Instant::now();
                Ok(())
            }
            Ok(false) => {
                metrics::counter!("batlehub_proxy_fill_lease_lost_total", "registry" => Arc::clone(&self.registry)).increment(1);
                tracing::warn!(%key, fill = %id, "cold fill lost its lease, stopping");
                self.lease = None;
                Err(CoreError::Registry(
                    "the cold fill lost its lease".to_owned(),
                ))
            }
            // RFC 0048-bis §10.2: without the row the holder cannot be fenced;
            // it keeps filling and serves its own readers.
            Err(e) => {
                metrics::counter!("batlehub_proxy_fill_lease_errors_total").increment(1);
                tracing::warn!(%key, error = %e, "fill lease unavailable mid-fill; continuing");
                Ok(())
            }
        }
    }
}

impl FillWriter {
    fn deactivate(&mut self) {
        if std::mem::take(&mut self.active) {
            metrics::gauge!("batlehub_proxy_fill_active").decrement(1.0);
        }
    }
}

/// What the lease said a starting request does.
pub(super) enum Start {
    /// This replica fills: it holds the lease, or the lease store failed (R10).
    Fill,
    /// Another replica holds a live lease: follow it (R3).
    Follow(LeaseState),
    /// The artifact is already in storage (R5): end the fill `Done`.
    Cached,
}

enum LeaseCall {
    Commit(u32),
    Heartbeat,
    Finish(Phase),
}

impl Drop for FillWriter {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        self.deactivate();
        let failed = Phase::Failed(FillError::other("the cold fill stopped before it finished"));
        self.fill.progress.send_modify(|p| p.phase = failed);
        self.coordinator.release(&self.fill.key, &self.fill);
        let (lease, written) = (self.lease, self.written);
        if lease.is_none() && written == 0 {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (fill, leases) = (Arc::clone(&self.fill), Arc::clone(&self.coordinator.leases));
        let staged = std::mem::take(&mut self.staged);
        rt.spawn(async move {
            if let Some(id) = lease {
                let _ = leases.release(&fill.key, id).await;
            }
            if written > 0 {
                let _ = fill.storage.delete_segments(&staged, written).await;
            }
        });
    }
}

/// What a request does with a fill once it has one.
pub(super) enum Served {
    /// The bytes, streamed from the segments as they become readable.
    Stream(ByteStream),
    /// The fill ended and promoted the artifact; read it from storage.
    FromStorage,
    /// Upstream forbade caching; fetch for this request alone.
    NoStore,
}

impl Fill {
    /// Whether this fill is another replica's, followed here.
    pub(super) fn is_remote(&self) -> bool {
        self.remote.load(Ordering::Relaxed)
    }

    /// Wait until there is something to serve, then say how to serve it.
    ///
    /// `wait_for_end` is the quarantine case: a registry that holds artifacts
    /// before serving them never streams a byte the policy has not seen.
    pub(super) async fn serve(self: Arc<Self>, wait_for_end: bool) -> Result<Served, CoreError> {
        let mut rx = self.progress.subscribe();
        loop {
            let stream = {
                let p = rx.borrow_and_update();
                match &p.phase {
                    Phase::Failed(e) => return Err(e.to_core()),
                    Phase::NoStore => return Ok(Served::NoStore),
                    Phase::Done => return Ok(Served::FromStorage),
                    _ if wait_for_end => false,
                    Phase::Verified { .. } => true,
                    Phase::Filling => p.committed > 0 || p.open.len() > 1,
                }
            };
            if stream {
                return Ok(Served::Stream(self.stream(rx)));
            }
            if rx.changed().await.is_err() {
                return Err(CoreError::Registry("the cold fill vanished".to_owned()));
            }
        }
    }

    fn stream(self: Arc<Self>, rx: watch::Receiver<Progress>) -> ByteStream {
        let reader = Reader {
            fill: self,
            rx,
            seg: 0,
            off: 0,
            chunk: 0,
            source: None,
            tail: false,
            done: false,
        };
        Box::pin(futures::stream::unfold(reader, |mut r| async move {
            if r.done {
                return None;
            }
            match r.next().await {
                Some(Ok(b)) => Some((Ok(b), r)),
                Some(Err(e)) => {
                    r.done = true;
                    Some((Err(e), r))
                }
                None => None,
            }
        }))
    }
}

/// One request reading a fill: segment `seg`, `off` bytes into it.
struct Reader {
    fill: Arc<Fill>,
    rx: watch::Receiver<Progress>,
    seg: u32,
    off: usize,
    /// The next chunk of the open segment, while `seg` is the open one.
    chunk: usize,
    /// A segment read from storage, or (`tail`) the promoted artifact.
    source: Option<ByteStream>,
    tail: bool,
    done: bool,
}

enum Step {
    Chunk(Bytes),
    Segment,
    /// A followed fill ended `Done` without this replica seeing its count:
    /// the rest is read from the artifact.
    Tail,
    Wait,
    End,
    Fail(CoreError),
}

impl Reader {
    async fn next(&mut self) -> Option<Result<Bytes, CoreError>> {
        loop {
            if let Some(source) = self.source.as_mut() {
                match source.next().await {
                    Some(Ok(b)) => {
                        self.off += b.len();
                        return Some(Ok(b));
                    }
                    Some(Err(e)) => return Some(Err(e)),
                    None if self.tail => return None,
                    None => {
                        self.source = None;
                        self.seg += 1;
                        (self.off, self.chunk) = (0, 0);
                        continue;
                    }
                }
            }
            let step = {
                let p = self.rx.borrow_and_update();
                match &p.phase {
                    Phase::Failed(e) => Step::Fail(e.to_core()),
                    _ if self.seg < p.committed => Step::Segment,
                    Phase::Verified { .. } => Step::End,
                    Phase::Done if p.verified => Step::End,
                    Phase::Done => Step::Tail,
                    Phase::NoStore => Step::Fail(CoreError::Registry(
                        "the cold fill was not stored".to_owned(),
                    )),
                    // Every chunk of the open segment but the newest (R6).
                    Phase::Filling => match p.open.get(self.chunk) {
                        Some(b) if self.chunk + 1 < p.open.len() => Step::Chunk(b.clone()),
                        _ => Step::Wait,
                    },
                }
            };
            match step {
                Step::Chunk(b) => {
                    self.chunk += 1;
                    self.off += b.len();
                    return Some(Ok(b));
                }
                Step::Segment => {
                    if let Err(e) = self.open_segment().await {
                        return Some(Err(e));
                    }
                }
                Step::Tail => {
                    if let Err(e) = self.open_artifact().await {
                        return Some(Err(e));
                    }
                }
                Step::Wait => {
                    if self.rx.changed().await.is_err() {
                        return Some(Err(CoreError::Registry(
                            "the cold fill vanished".to_owned(),
                        )));
                    }
                }
                Step::End => return None,
                Step::Fail(e) => return Some(Err(e)),
            }
        }
    }

    /// Read committed segment `seg` from storage, from `off` on. Promotion
    /// deletes the segments: a reader that finds its segment gone waits for
    /// the fill to end and, promoted, continues from the artifact.
    async fn open_segment(&mut self) -> Result<(), CoreError> {
        let storage = &self.fill.storage;
        let staged =
            self.fill.staged.get().ok_or_else(|| {
                CoreError::Registry("cold fill segment without a fill".to_owned())
            })?;
        let off = self.off as u64;
        // A reader that was on the open segment when it was committed resumes
        // mid-segment: a ranged read where the segment is its own object (S3
        // answers it with `Range`), else read it and skip (the filesystem's
        // one staging file is not addressable by segment key).
        if off > 0 {
            if let Some(key) = segment_key(staged, self.seg) {
                if let Some(a) = storage.retrieve_from(&key, off).await? {
                    self.source = Some(a.stream);
                    return Ok(());
                }
            }
        }
        if let Some(s) = storage.get_segment(staged, self.seg).await? {
            self.source = Some(skip_bytes(s, off));
            return Ok(());
        }
        loop {
            match &self.rx.borrow_and_update().phase {
                Phase::Done => break,
                Phase::Failed(e) => return Err(e.to_core()),
                _ => {}
            }
            if self.rx.changed().await.is_err() {
                return Err(CoreError::Registry("the cold fill vanished".to_owned()));
            }
        }
        self.open_artifact().await
    }

    /// Continue from the promoted artifact at this reader's offset.
    async fn open_artifact(&mut self) -> Result<(), CoreError> {
        let storage = &self.fill.storage;
        let offset = u64::from(self.seg) * FILL_SEGMENT_BYTES as u64 + self.off as u64;
        let artifact = storage
            .retrieve_from(&self.fill.key, offset)
            .await?
            .ok_or_else(|| {
                CoreError::Registry(format!(
                    "artifact '{}' vanished during its cold fill",
                    self.fill.key
                ))
            })?;
        self.source = Some(artifact.stream);
        self.tail = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{StorageMeta, StoredArtifact};
    use async_trait::async_trait;

    /// Storage by trait defaults, plus a way to see which keys exist and how
    /// many chunks each segment was handed over in.
    #[derive(Default)]
    struct Mem(Mutex<HashMap<String, Bytes>>, Mutex<Vec<usize>>);

    #[async_trait]
    impl StorageBackend for Mem {
        async fn store(&self, key: &str, data: Bytes, _: StorageMeta) -> Result<(), CoreError> {
            self.0.lock().unwrap().insert(key.to_owned(), data);
            Ok(())
        }
        async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .map(|b| StoredArtifact {
                    stream: Box::pin(futures::stream::once(async move { Ok(b) })),
                    meta: StorageMeta::default(),
                }))
        }
        async fn exists(&self, key: &str) -> Result<bool, CoreError> {
            Ok(self.0.lock().unwrap().contains_key(key))
        }
        async fn delete(&self, key: &str) -> Result<bool, CoreError> {
            Ok(self.0.lock().unwrap().remove(key).is_some())
        }
        async fn delete_by_prefix(&self, _: &str) -> Result<usize, CoreError> {
            Ok(0)
        }
        async fn stat_by_prefix(&self, _: &str) -> Result<(u64, u64), CoreError> {
            Ok((0, 0))
        }
        async fn put_segment_parts(
            &self,
            staged: &str,
            n: u32,
            parts: Vec<Bytes>,
        ) -> Result<(), CoreError> {
            self.1.lock().unwrap().push(parts.len());
            let joined: Vec<u8> = parts.concat();
            self.put_segment(staged, n, Bytes::from(joined)).await
        }
        async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
            let map = self.0.lock().unwrap();
            Ok(map
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect())
        }
    }

    fn setup() -> (Arc<FillCoordinator>, Arc<Mem>, Arc<dyn StorageBackend>) {
        let mem = Arc::new(Mem::default());
        let storage: Arc<dyn StorageBackend> = mem.clone();
        (Arc::new(FillCoordinator::default()), mem, storage)
    }

    async fn started(
        coordinator: &Arc<FillCoordinator>,
        storage: &Arc<dyn StorageBackend>,
    ) -> FillWriter {
        let Joined::Started(mut writer) = coordinator.join_or_start("k", storage) else {
            panic!("the first request starts the fill")
        };
        assert!(matches!(writer.start(Arc::from("r")).await, Start::Fill));
        *writer
    }

    async fn read_all(s: ByteStream) -> Result<Vec<u8>, CoreError> {
        let mut s = s;
        let mut out = Vec::new();
        while let Some(chunk) = s.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }

    /// RFC 0048 §6.6: a registry that holds artifacts before serving them
    /// never streams; it is served from storage once the fill has ended.
    #[tokio::test]
    async fn a_quarantined_kind_still_stores_before_serving() {
        let (coordinator, _, storage) = setup();
        let mut writer = started(&coordinator, &storage).await;
        writer.push(Bytes::from_static(b"one-")).await.unwrap();
        writer.push(Bytes::from_static(b"two-")).await.unwrap();
        let fill = writer.fill();
        let held = tokio::spawn(Arc::clone(&fill).serve(true));
        let streamed = Arc::clone(&fill).serve(false).await.unwrap();
        assert!(
            matches!(streamed, Served::Stream(_)),
            "an ordinary registry streams"
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!held.is_finished(), "a holding registry waits for the end");
        writer.seal().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !held.is_finished(),
            "verified is not done: nothing promoted yet"
        );
        writer.finish(Phase::Done).await;
        assert!(matches!(held.await.unwrap().unwrap(), Served::FromStorage));
        assert!(coordinator.is_idle());
    }

    /// The newest chunk is withheld until the fill ends, and a writer dropped
    /// without an end fails its readers instead of stranding them.
    #[tokio::test]
    async fn the_last_chunk_waits_for_the_end_and_a_dropped_writer_fails() {
        let (coordinator, mem, storage) = setup();
        let mut writer = started(&coordinator, &storage).await;
        let Joined::Joined(fill) = coordinator.join_or_start("k", &storage) else {
            panic!("the second request joins")
        };
        writer.push(Bytes::from_static(b"one-")).await.unwrap();
        writer.push(Bytes::from_static(b"two")).await.unwrap();
        let Served::Stream(mut s) = fill.serve(false).await.unwrap() else {
            panic!("readable bytes stream")
        };
        assert_eq!(
            s.next().await.unwrap().unwrap(),
            Bytes::from_static(b"one-")
        );
        let pending = tokio::time::timeout(std::time::Duration::from_millis(30), s.next()).await;
        assert!(pending.is_err(), "the newest chunk is withheld");
        drop(writer); // no end: the reader must not hang
        let err = s.next().await.unwrap().expect_err("the fill failed");
        assert!(
            err.to_string().contains("stopped before it finished"),
            "{err}"
        );
        assert!(coordinator.is_idle());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(mem.0.lock().unwrap().is_empty(), "nothing left in storage");
    }

    /// T3: a body that is an exact multiple of the segment size keeps its
    /// last — full — segment withheld until the digest is verified, for a
    /// local reader and in storage alike.
    #[tokio::test]
    async fn an_exact_multiple_of_the_segment_size_withholds_its_last_segment() {
        let (coordinator, mem, storage) = setup();
        let mut writer = started(&coordinator, &storage).await;
        let seg = |b: u8| Bytes::from(vec![b; FILL_SEGMENT_BYTES]);
        writer.push(seg(b'a')).await.unwrap();
        writer.push(seg(b'b')).await.unwrap();
        let fill = writer.fill();
        let Served::Stream(mut s) = Arc::clone(&fill).serve(false).await.unwrap() else {
            panic!("segment 0 is committed")
        };
        let mut got = 0;
        while got < FILL_SEGMENT_BYTES {
            got += s.next().await.unwrap().unwrap().len();
        }
        let pending = tokio::time::timeout(std::time::Duration::from_millis(30), s.next()).await;
        assert!(pending.is_err(), "the full last segment is withheld");
        let staged = writer.staged().to_owned();
        assert!(mem
            .0
            .lock()
            .unwrap()
            .contains_key(&segment_key(&staged, 0).unwrap()));
        assert!(
            !mem.0
                .lock()
                .unwrap()
                .contains_key(&segment_key(&staged, 1).unwrap()),
            "the last segment is not even in storage before verification"
        );
        let (count, outcome) = writer.seal().await.unwrap();
        assert_eq!((count, outcome.size), (2, 2 * FILL_SEGMENT_BYTES as u64));
        let rest = read_all(s).await.unwrap();
        assert_eq!(rest.len(), FILL_SEGMENT_BYTES);
        assert!(rest.iter().all(|b| *b == b'b'));
    }

    /// A reader part-way through the open segment when it is committed
    /// resumes from storage at its offset, then reads the next one from memory.
    #[tokio::test]
    async fn a_reader_on_the_open_segment_resumes_from_storage_when_it_commits() {
        let (coordinator, _, storage) = setup();
        let mut writer = started(&coordinator, &storage).await;
        let half = FILL_SEGMENT_BYTES / 2;
        writer.push(Bytes::from(vec![b'a'; half])).await.unwrap();
        writer
            .push(Bytes::from(vec![b'b'; half - 1]))
            .await
            .unwrap();
        let Served::Stream(mut s) = writer.fill().serve(false).await.unwrap() else {
            panic!("one chunk is readable")
        };
        assert_eq!(s.next().await.unwrap().unwrap().len(), half);
        // Fills segment 0 and starts segment 1: segment 0 is committed.
        writer.push(Bytes::from_static(b"bcd")).await.unwrap();
        writer.push(Bytes::from_static(b"e")).await.unwrap();
        let mut got = vec![b'a'; half];
        for _ in 0..2 {
            got.extend_from_slice(&s.next().await.unwrap().unwrap());
        }
        assert_eq!(
            got.len(),
            FILL_SEGMENT_BYTES + 2,
            "segment 0 whole, then \"cd\""
        );
        writer.seal().await.unwrap();
        got.extend(read_all(s).await.unwrap());
        let mut want = vec![b'a'; half];
        want.extend(vec![b'b'; half]);
        want.extend_from_slice(b"cde");
        assert!(got == want);
    }

    /// Promotion deletes the segments under a reader still on them: it
    /// continues from the promoted artifact at its offset.
    #[tokio::test]
    async fn a_reader_whose_segment_vanished_continues_from_the_artifact() {
        let (coordinator, _, storage) = setup();
        let mut writer = started(&coordinator, &storage).await;
        let mut body = vec![b'x'; FILL_SEGMENT_BYTES];
        body.extend_from_slice(b"tail");
        writer.push(Bytes::from(body.clone())).await.unwrap();
        let fill = writer.fill();
        let (count, outcome) = writer.seal().await.unwrap();
        let Served::Stream(mut s) = Arc::clone(&fill).serve(false).await.unwrap() else {
            panic!("a verified fill streams")
        };
        // Segment 0 read from storage; segment 1 will be gone.
        let mut got = s.next().await.unwrap().unwrap().to_vec();
        assert_eq!(got.len(), FILL_SEGMENT_BYTES);
        let staged = writer.staged().to_owned();
        storage
            .promote_segments(&staged, count, "k", &outcome)
            .await
            .unwrap();
        assert!(storage.get_segment(&staged, 1).await.unwrap().is_none());
        writer.finish(Phase::Done).await;
        got.extend(read_all(s).await.unwrap());
        assert_eq!(got, body);
        assert_eq!(
            outcome.content_hash,
            crate::services::integrity::sha256_hex(&body)
        );
    }

    /// The memory fix: a segment received in many chunks reaches storage as
    /// those chunks, not joined into a second segment-sized buffer.
    #[tokio::test]
    async fn a_segment_is_put_from_its_chunks_without_joining_them() {
        let (coordinator, mem, storage) = setup();
        let mut writer = started(&coordinator, &storage).await;
        let quarter = FILL_SEGMENT_BYTES / 4;
        for _ in 0..5 {
            writer.push(Bytes::from(vec![b'q'; quarter])).await.unwrap();
        }
        writer.seal().await.unwrap();
        assert_eq!(
            *mem.1.lock().unwrap(),
            vec![4, 1],
            "a full segment of four chunks, then the last of one"
        );
    }

    /// T6: the sweep deletes an expired fill's segments and its row, and
    /// leaves a live one alone.
    #[tokio::test]
    async fn the_sweep_deletes_an_expired_fills_segments() {
        let ttl = std::time::Duration::from_millis(30);
        let leases = InMemoryFillLeases::with_ttl(ttl);
        let (_, mem, storage) = setup();
        let Taken::Started { fill, .. } = leases.take("k", "dead").await.unwrap() else {
            panic!("the first take starts")
        };
        let staged = format!("{STAGING_PREFIX}{fill}/k");
        storage
            .put_segment(&staged, 0, Bytes::from_static(b"orphan"))
            .await
            .unwrap();
        assert!(leases.commit("k", fill, 1).await.unwrap());
        assert_eq!(
            sweep_fills(&leases, storage.as_ref(), ttl).await,
            0,
            "still live"
        );
        assert!(!mem.0.lock().unwrap().is_empty());
        tokio::time::sleep(ttl * 2).await;
        assert_eq!(
            sweep_fills(&leases, storage.as_ref(), ttl).await,
            2,
            "segments 0..written"
        );
        assert!(mem.0.lock().unwrap().is_empty(), "the segments are gone");
        assert!(
            leases
                .expired(std::time::Duration::ZERO)
                .await
                .unwrap()
                .is_empty(),
            "and the row"
        );
    }
}
