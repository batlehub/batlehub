//! RFC 0048-bis §6.2 — the control half of a cold fill: who fills which
//! storage key, how many of its segments readers may read, and how it ended.
//!
//! The bytes are in storage, as segments of the staging key named by the
//! fill id (`staging:<fill>/<artifact key>`, see
//! [`segment_key`](crate::ports::segment_key)). The fill id is also the
//! fencing token: every write a holder makes is conditioned on it, so a
//! holder whose lease was taken over writes nothing anyone reads.
//!
//! [`InMemoryFillLeases`] is the lease of a `memory` cache — one replica, so
//! one process holds every lease; a shared cache gets a Postgres
//! implementation of the same port.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::error::CoreError;
use crate::ports::FILL_SEGMENT_BYTES;

/// Names a fill: the fencing token and the uuid part of its staging key.
pub type FillId = uuid::Uuid;

/// §15 Q3: a lease not extended for this long is dead, and may be taken over.
pub const LEASE_TTL: Duration = Duration::from_secs(30);

/// §15 Q3: a holder extends its lease this often, on a timer of its own, so a
/// quiet upstream or a long assembly never lets a live lease expire.
pub const HEARTBEAT_AFTER: Duration = Duration::from_secs(10);

/// The heartbeat period of a lease that lives `ttl`: a third of it, so two
/// missed beats still leave it alive.
pub fn heartbeat_every(ttl: Duration) -> Duration {
    ttl / 3
}

/// How many segments a fill of `total` bytes has: an empty artifact is one
/// empty segment.
pub fn segments_for(total: u64) -> u32 {
    let seg = FILL_SEGMENT_BYTES as u64;
    // A u32 of 8 MiB segments is 32 PiB; nothing near it passes a size limit.
    u32::try_from(total.div_ceil(seg).max(1)).unwrap_or(u32::MAX)
}

/// Where a fill is. Every segment of a `Verified` fill is committed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Filling,
    /// The digest checked out; all `segments_for(total)` segments are readable.
    Verified {
        total: u64,
    },
    /// Promoted: the artifact key holds the bytes, the segments may be gone.
    Done,
    Failed(FillError),
    /// Upstream said `no-store`: nothing was written, nothing is shared.
    NoStore,
}

impl Phase {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Phase::Done | Phase::Failed(_) | Phase::NoStore)
    }
}

/// A `CoreError` every reader of a failed fill can be handed a copy of, and
/// a lease row can carry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillError {
    kind: FillErrorKind,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum FillErrorKind {
    NotFound,
    TooLarge,
    Integrity,
    Denied,
    Other,
}

impl From<&CoreError> for FillError {
    fn from(e: &CoreError) -> Self {
        let kind = match e {
            CoreError::NotFound(_) | CoreError::NotFoundWithheld(_) => FillErrorKind::NotFound,
            CoreError::PayloadTooLarge(_) => FillErrorKind::TooLarge,
            CoreError::IntegrityFailure(_) => FillErrorKind::Integrity,
            CoreError::AccessDenied(_) => FillErrorKind::Denied,
            _ => FillErrorKind::Other,
        };
        let message = match e {
            CoreError::AccessDenied(d) => d.message.clone(),
            CoreError::NotFound(m)
            | CoreError::NotFoundWithheld(m)
            | CoreError::PayloadTooLarge(m)
            | CoreError::IntegrityFailure(m)
            | CoreError::Registry(m) => m.clone(),
            other => other.to_string(),
        };
        Self { kind, message }
    }
}

impl FillError {
    /// A failure that is the fill's own, not an upstream's.
    pub fn other(message: impl Into<String>) -> Self {
        Self {
            kind: FillErrorKind::Other,
            message: message.into(),
        }
    }

    /// The same failure with its message cut to `max` bytes, for a lease row
    /// or a notification (RFC 0048-bis §7.3).
    pub fn truncated(&self, max: usize) -> Self {
        let mut message = self.message.clone();
        if message.len() > max {
            let mut end = max;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self {
            kind: self.kind,
            message,
        }
    }

    pub fn to_core(&self) -> CoreError {
        let m = self.message.clone();
        match self.kind {
            FillErrorKind::NotFound => CoreError::NotFound(m),
            FillErrorKind::TooLarge => CoreError::PayloadTooLarge(m),
            FillErrorKind::Integrity => CoreError::IntegrityFailure(m),
            FillErrorKind::Denied => CoreError::access_denied(m),
            FillErrorKind::Other => CoreError::Registry(m),
        }
    }
}

/// A lease as a reader sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseState {
    pub fill: FillId,
    /// The replica filling, for logs only.
    pub holder: String,
    /// Segments `0..committed` are in storage and may be read.
    pub committed: u32,
    pub phase: Phase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Taken {
    /// The caller holds the lease and must fill as `fill`. `previous` is a
    /// dead fill it took over and how many segments that one may have
    /// written: the caller deletes them before starting.
    Started {
        fill: FillId,
        previous: Option<(FillId, u32)>,
    },
    /// Another holder is filling; follow it.
    Running(LeaseState),
}

#[async_trait]
pub trait FillLeases: Send + Sync {
    /// Whether other replicas share this lease. Only then can a fill have
    /// ended elsewhere between a request's storage miss and its lease (R5).
    fn shared(&self) -> bool {
        false
    }
    /// How long a lease lives without an extension.
    fn ttl(&self) -> Duration {
        LEASE_TTL
    }
    /// Take the lease on `key`, or learn who holds it. A lease that expired
    /// or ended (done, failed, `no-store`) is taken over; only an expired
    /// running one is reported as `previous`, its segments to delete.
    async fn take(&self, key: &str, holder: &str) -> Result<Taken, CoreError>;
    /// Make segments `0..committed` readable and extend the lease. Fenced on
    /// `fill`: `Ok(false)` means the lease is lost and the caller must stop.
    async fn commit(&self, key: &str, fill: FillId, committed: u32) -> Result<bool, CoreError>;
    /// Extend the lease without progress. Fenced as `commit`.
    async fn heartbeat(&self, key: &str, fill: FillId) -> Result<bool, CoreError>;
    /// Move the fill to `phase`; `Verified { total }` also commits every
    /// segment. Fenced as `commit`.
    async fn finish(&self, key: &str, fill: FillId, phase: Phase) -> Result<bool, CoreError>;
    /// Drop the lease, if `fill` still holds it.
    async fn release(&self, key: &str, fill: FillId) -> Result<(), CoreError>;
    /// Drop `fill`'s lease on `key` only if it is still expired, returning the
    /// segments it may have written: the sweep claims a dead fill before it
    /// deletes anything, so a holder that came back in between keeps both.
    async fn reap(&self, key: &str, fill: FillId) -> Result<Option<u32>, CoreError>;
    /// Progress of `fill`, until it ends or its lease goes; empty when `fill`
    /// does not hold `key`.
    fn follow(&self, key: &str, fill: FillId) -> BoxStream<'static, Result<LeaseState, CoreError>>;
    /// Leases not extended for `older_than`: their key, fill and the segments
    /// they may have written, for the sweep to delete.
    async fn expired(&self, older_than: Duration) -> Result<Vec<(String, FillId, u32)>, CoreError>;
}

/// What a lease's followers watch. `extended` moves without waking them.
struct Watched {
    state: LeaseState,
    extended: Instant,
}

struct Lease {
    state: watch::Sender<Watched>,
    /// Segments that may exist in storage: one past the committed ones.
    written: u32,
}

impl Lease {
    fn fill(&self) -> FillId {
        self.state.borrow().state.fill
    }
    fn phase(&self) -> Phase {
        self.state.borrow().state.phase.clone()
    }
    fn extended(&self) -> Instant {
        self.state.borrow().extended
    }
}

/// The in-process lease of a `memory` cache (§4.2 R1).
pub struct InMemoryFillLeases {
    ttl: Duration,
    leases: Mutex<HashMap<String, Lease>>,
    /// Tests stand two replicas on one instance; this makes it say so (R5).
    shared: bool,
}

impl Default for InMemoryFillLeases {
    fn default() -> Self {
        Self::with_ttl(LEASE_TTL)
    }
}

impl InMemoryFillLeases {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            leases: Mutex::new(HashMap::new()),
            shared: false,
        }
    }

    /// Report the lease as shared, as a Postgres one is: for tests that stand
    /// two replicas on one instance.
    pub fn as_shared(mut self) -> Self {
        self.shared = true;
        self
    }

    /// Apply `f` to `fill`'s lease on `key` if it holds it; `false` otherwise.
    fn fenced(&self, key: &str, fill: FillId, f: impl FnOnce(&mut Lease) -> bool) -> bool {
        let mut leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
        match leases.get_mut(key) {
            Some(lease) if lease.fill() == fill => {
                let moved = f(lease);
                if moved {
                    lease.state.send_if_modified(|w| {
                        w.extended = Instant::now();
                        false
                    });
                }
                moved
            }
            _ => false,
        }
    }
}

#[async_trait]
impl FillLeases for InMemoryFillLeases {
    fn shared(&self) -> bool {
        self.shared
    }

    fn ttl(&self) -> Duration {
        self.ttl
    }

    async fn take(&self, key: &str, holder: &str) -> Result<Taken, CoreError> {
        let mut leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
        let mut previous = None;
        if let Some(lease) = leases.get(key) {
            let state = lease.state.borrow().state.clone();
            let ended = state.phase.is_terminal();
            if !ended && lease.extended().elapsed() < self.ttl {
                return Ok(Taken::Running(state));
            }
            // An ended fill cleaned up after itself; an expired one did not.
            if !ended {
                previous = Some((state.fill, lease.written));
            }
        }
        let fill = FillId::new_v4();
        let (state, _) = watch::channel(Watched {
            state: LeaseState {
                fill,
                holder: holder.to_owned(),
                committed: 0,
                phase: Phase::Filling,
            },
            extended: Instant::now(),
        });
        // Replacing the entry drops the old sender, which ends its followers.
        leases.insert(key.to_owned(), Lease { state, written: 1 });
        Ok(Taken::Started { fill, previous })
    }

    async fn commit(&self, key: &str, fill: FillId, committed: u32) -> Result<bool, CoreError> {
        Ok(self.fenced(key, fill, |lease| {
            if lease.phase() != Phase::Filling {
                return false;
            }
            lease.written = committed.saturating_add(1);
            lease.state.send_modify(|w| w.state.committed = committed);
            true
        }))
    }

    async fn heartbeat(&self, key: &str, fill: FillId) -> Result<bool, CoreError> {
        Ok(self.fenced(key, fill, |lease| !lease.phase().is_terminal()))
    }

    async fn finish(&self, key: &str, fill: FillId, phase: Phase) -> Result<bool, CoreError> {
        Ok(self.fenced(key, fill, |lease| {
            if lease.phase().is_terminal() {
                return false;
            }
            if let Phase::Verified { total } = phase {
                lease.written = segments_for(total);
            }
            lease.state.send_modify(|w| {
                let s = &mut w.state;
                if let Phase::Verified { total } = phase {
                    s.committed = segments_for(total);
                }
                s.phase = phase;
            });
            true
        }))
    }

    async fn release(&self, key: &str, fill: FillId) -> Result<(), CoreError> {
        let mut leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
        if leases.get(key).is_some_and(|l| l.fill() == fill) {
            leases.remove(key);
        }
        Ok(())
    }

    async fn reap(&self, key: &str, fill: FillId) -> Result<Option<u32>, CoreError> {
        let mut leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
        let reaped = leases
            .get(key)
            .filter(|l| l.fill() == fill && l.extended().elapsed() >= self.ttl)
            .map(|l| l.written);
        if reaped.is_some() {
            leases.remove(key);
        }
        Ok(reaped)
    }

    fn follow(&self, key: &str, fill: FillId) -> BoxStream<'static, Result<LeaseState, CoreError>> {
        let rx = {
            let leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
            leases
                .get(key)
                .filter(|l| l.fill() == fill)
                .map(|l| l.state.subscribe())
        };
        let Some(mut rx) = rx else {
            return Box::pin(futures::stream::empty());
        };
        rx.mark_changed();
        let ttl = self.ttl;
        // `ended` stops the stream after the terminal state was yielded once;
        // a lease not extended for `ttl` ends it too: its holder is dead.
        Box::pin(futures::stream::unfold(
            (rx, false),
            move |(mut rx, ended)| async move {
                if ended {
                    return None;
                }
                loop {
                    match tokio::time::timeout(ttl, rx.changed()).await {
                        Ok(Ok(())) => break,
                        Ok(Err(_)) => return None,
                        Err(_) if rx.borrow().extended.elapsed() >= ttl => return None,
                        Err(_) => {}
                    }
                }
                let state = rx.borrow_and_update().state.clone();
                let ended = state.phase.is_terminal();
                Some((Ok(state), (rx, ended)))
            },
        ))
    }

    async fn expired(&self, older_than: Duration) -> Result<Vec<(String, FillId, u32)>, CoreError> {
        let leases = self.leases.lock().unwrap_or_else(|p| p.into_inner());
        Ok(leases
            .iter()
            .filter(|(_, l)| l.extended().elapsed() >= older_than)
            .map(|(k, l)| (k.clone(), l.fill(), l.written))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn started(t: Taken) -> (FillId, Option<(FillId, u32)>) {
        match t {
            Taken::Started { fill, previous } => (fill, previous),
            Taken::Running(s) => panic!("expected to take the lease, found {s:?}"),
        }
    }

    #[test]
    fn segment_counts() {
        let seg = FILL_SEGMENT_BYTES as u64;
        assert_eq!(segments_for(0), 1, "an empty artifact is one empty segment");
        assert_eq!(segments_for(1), 1);
        assert_eq!(segments_for(seg), 1);
        assert_eq!(segments_for(seg + 1), 2);
        assert_eq!(segments_for(3 * seg), 3);
    }

    #[tokio::test]
    async fn a_live_lease_is_followed_not_taken() {
        let leases = InMemoryFillLeases::default();
        let (fill, previous) = started(leases.take("k", "a").await.unwrap());
        assert_eq!(previous, None);
        assert!(leases.commit("k", fill, 2).await.unwrap());
        match leases.take("k", "b").await.unwrap() {
            Taken::Running(s) => {
                assert_eq!((s.fill, s.holder.as_str(), s.committed), (fill, "a", 2));
            }
            other => panic!("a live lease is not taken over: {other:?}"),
        }
    }

    /// T1 for the in-process lease: a holder whose lease expired and was
    /// taken over writes nothing into the new fill.
    #[tokio::test]
    async fn a_stale_holder_cannot_commit_into_a_taken_over_fill() {
        let leases = InMemoryFillLeases::with_ttl(Duration::from_millis(20));
        let (old, _) = started(leases.take("k", "a").await.unwrap());
        assert!(leases.commit("k", old, 3).await.unwrap());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            leases
                .expired(Duration::from_millis(20))
                .await
                .unwrap()
                .len(),
            1
        );
        let (new, previous) = started(leases.take("k", "b").await.unwrap());
        assert_eq!(
            previous,
            Some((old, 4)),
            "the takeover learns what to delete"
        );
        assert!(!leases.commit("k", old, 4).await.unwrap());
        assert!(!leases.heartbeat("k", old).await.unwrap());
        assert!(!leases.finish("k", old, Phase::Done).await.unwrap());
        leases.release("k", old).await.unwrap();
        assert!(
            leases.commit("k", new, 1).await.unwrap(),
            "the stale release left the new lease"
        );
    }

    #[tokio::test]
    async fn a_failed_fill_is_taken_over_at_once_and_verified_commits_everything() {
        let leases = InMemoryFillLeases::default();
        let (fill, _) = started(leases.take("k", "a").await.unwrap());
        let mut follow = leases.follow("k", fill);
        let seg = FILL_SEGMENT_BYTES as u64;
        assert!(leases
            .finish("k", fill, Phase::Verified { total: 2 * seg + 1 })
            .await
            .unwrap());
        let s = follow.next().await.unwrap().unwrap();
        assert_eq!(s.committed, 3);
        assert!(
            !leases.commit("k", fill, 1).await.unwrap(),
            "no commit after verified"
        );
        let failed = Phase::Failed(FillError::other("boom"));
        assert!(leases.finish("k", fill, failed.clone()).await.unwrap());
        let s = follow.next().await.unwrap().unwrap();
        assert_eq!(s.phase, failed);
        assert!(
            follow.next().await.is_none(),
            "a terminal state ends the follow"
        );
        let (_, previous) = started(leases.take("k", "b").await.unwrap());
        assert_eq!(previous, None, "a failed fill deleted its own segments");
    }
}
