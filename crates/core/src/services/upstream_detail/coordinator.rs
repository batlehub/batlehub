//! What stops the console becoming a request amplifier (RFC 0007 §5.5, §7.7).
//!
//! Two pieces of per-process state, both deliberately *not* in `ExploreCache`:
//! that cache is keyed by query and invalidated per registry by
//! `invalidate_explore_cache`, so a per-package absence marker keyed into it
//! would be cleared by an unrelated catalogue write.
//!
//! - **Single-flight.** Ten operators opening the same new package must produce
//!   one upstream request. Without it the console amplifies requests under
//!   exactly the conditions that make a package interesting: several people
//!   looking at it at once.
//! - **A negative cache.** A `404` is a *fact* — RFC 0009's distinction between
//!   *failed* and *answered something other than success* — so it is remembered,
//!   and a bad URL, a typo or a crawler cannot turn every reload into an
//!   upstream request. A connection failure is not a fact about the package and
//!   is not cached at all.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// How many absences may be remembered at once.
///
/// The keys are `{registry}:{package}` strings a caller chooses, and
/// `/api/v1/explore/packages/{registry}/{name}` is reachable without
/// authentication — so without a cap, anything walking the namespace writes one
/// permanent entry per `404` and the map grows for the life of the process.
///
/// Sized to be a bound rather than a tuning knob: 100 000 entries is far more
/// than the working set of a real instance's genuinely-missing coordinates, and
/// small enough that the worst case is tens of megabytes rather than the whole
/// heap.
const ABSENT_CAP: usize = 100_000;

/// Per-process coordination for the discovery read.
#[derive(Default)]
pub struct UpstreamDetailCoordinator {
    /// Keys with a read in progress, and what to wake when it finishes.
    in_flight: Mutex<HashMap<String, Arc<Notify>>>,
    /// Coordinates upstream has said it does not have, and when to forget that.
    absent: Mutex<HashMap<String, Instant>>,
}

/// Held for the duration of a read; releasing it wakes everyone waiting on the
/// same key.
///
/// A guard rather than a `finish(key)` call, so an early return or a `?` cannot
/// leave a key marked in-flight forever — which would wedge that package's page
/// for the life of the process.
pub struct FlightGuard {
    coordinator: Arc<UpstreamDetailCoordinator>,
    key: String,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        let notify = self.coordinator.in_flight.lock().unwrap().remove(&self.key);
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

impl UpstreamDetailCoordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Claim `key`, or wait for whoever already holds it.
    ///
    /// `Some(guard)` means this caller does the work. `None` means someone else
    /// just did it and the caller should re-read the cache — which is where the
    /// answer now is, because the holder writes it before releasing.
    ///
    /// The wait is bounded: a holder that is wedged (a hung upstream against a
    /// client with no timeout) must not hold every other reader with it. On
    /// timeout the caller proceeds on its own, which costs a duplicate request
    /// in a situation that is already broken.
    pub async fn claim(self: &Arc<Self>, key: &str, wait: Duration) -> Option<FlightGuard> {
        // One deadline for the whole call, not one per turn of the loop.
        //
        // The loop is re-entered whenever the key is re-claimed under us, and
        // again whenever a wake finds the key still held — so a `wait` measured
        // from the top of each iteration is not a bound at all: a popular
        // coordinate under continuous contention could hold a reader for a
        // multiple of it, which is exactly the "a wedged holder must not hold
        // every other reader with it" this promises.
        let deadline = Instant::now() + wait;
        loop {
            let waiter = {
                let mut in_flight = self.in_flight.lock().unwrap();
                match in_flight.get(key) {
                    Some(notify) => Arc::clone(notify),
                    None => {
                        in_flight.insert(key.to_owned(), Arc::new(Notify::new()));
                        return Some(FlightGuard {
                            coordinator: Arc::clone(self),
                            key: key.to_owned(),
                        });
                    }
                }
            };
            // Register *before* re-checking the map, then re-check, then wait.
            //
            // The order is the whole of the fix. `notify_waiters()` wakes only
            // waiters already registered, and `FlightGuard::drop` removes the
            // key and notifies in one step — so a holder that finished between
            // the lock being dropped above and this future being polled woke
            // nobody, and a plain `timeout(wait, waiter.notified())` then sat
            // out the entire `wait` (ten seconds on the detail path) before the
            // re-check below could tell it the answer had been in the cache the
            // whole time. The caller does not "proceed on its own" after that:
            // it reads the cache, finds the entry, and the page it renders spent
            // ten seconds waiting for something already done.
            //
            // `enable()` registers the waiter without awaiting, so any
            // `notify_waiters()` from here on is guaranteed to reach it; the
            // re-check then covers the window that closed before it.
            //
            // The re-check is on the `Notify`'s *identity*, not on the key being
            // present. The key alone is not enough: the holder may have finished
            // **and** a fresh caller claimed the key again, both inside the same
            // window — in which case the map says "in flight" while the waiter
            // that was just registered belongs to a `Notify` nobody holds any
            // more and nothing will ever signal. `contains_key` reads that as
            // "still running" and parks for the whole `wait`, which is the ten
            // seconds this fix exists to avoid. Same `Arc`: wait on it. Different
            // `Arc`: start over and register on the one the new holder will
            // actually signal.
            let notified = waiter.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let still_ours = {
                let in_flight = self.in_flight.lock().unwrap();
                in_flight
                    .get(key)
                    .map(|current| Arc::ptr_eq(current, &waiter))
            };
            match still_ours {
                // Gone: the holder finished and the answer is in the cache.
                None => return None,
                // Re-claimed under us; the waiter above is registered on a dead
                // `Notify`.
                Some(false) => continue,
                Some(true) => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if tokio::time::timeout(left, notified).await.is_err() {
                tracing::debug!(key, "upstream detail: waited out the in-flight read");
                return None;
            }
            if !self.in_flight.lock().unwrap().contains_key(key) {
                return None;
            }
        }
    }

    /// Remember that upstream does not have this coordinate.
    pub fn record_absent(&self, key: &str, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        let now = Instant::now();
        let mut absent = self.absent.lock().unwrap();
        // Sweep before growing past the cap, and only then.
        //
        // Dropping expired entries "as they are found" is not enough on its own:
        // a key is written once and, if nobody ever asks for that coordinate
        // again, is never looked at again either. The keys are package names an
        // unauthenticated caller chooses, so anything walking
        // `/api/v1/explore/packages/{registry}/{random}` writes a permanent
        // entry per 404 and the map grows for the life of the process.
        //
        // The sweep is O(n) and runs at most once per `ABSENT_CAP` inserts, so
        // the amortised cost is constant. If every entry is still live the cap
        // is enforced by clearing outright: a negative cache is an optimisation,
        // and losing it costs one upstream request per forgotten key, which is
        // the correct thing to trade for a bound.
        if absent.len() >= ABSENT_CAP {
            absent.retain(|_, until| *until > now);
            if absent.len() >= ABSENT_CAP {
                absent.clear();
            }
        }
        // `checked_add`, not `+`: `ttl` is `negative_ttl_secs` straight out of
        // the config and nothing bounds it, so an operator who writes a very
        // large number would otherwise panic the task handling the first
        // upstream `404` — a config typo taking out a page request. A deadline
        // that cannot be represented is one that never expires, which is what
        // the operator asked for anyway.
        let Some(until) = now.checked_add(ttl) else {
            tracing::warn!(
                ttl_secs = ttl.as_secs(),
                "upstream detail: negative_ttl is too large to represent; not remembering this \
                 absence"
            );
            return;
        };
        absent.insert(key.to_owned(), until);
    }

    /// Whether this coordinate is remembered as absent.
    ///
    /// Expired entries are dropped as they are found; `record_absent` sweeps
    /// the ones nobody comes back for.
    pub fn is_absent(&self, key: &str) -> bool {
        let mut absent = self.absent.lock().unwrap();
        match absent.get(key) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                absent.remove(key);
                false
            }
            None => false,
        }
    }

    /// Forget every remembered absence.
    ///
    /// Called by every applied config reload: a reload that adds an upstream,
    /// fixes its base URL or repairs its credentials changes what "absent"
    /// means, and the coordinates that failed under the old config must be
    /// asked again.
    pub fn clear_absent(&self) {
        self.absent.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_first_caller_claims_and_the_second_waits_for_it() {
        let coordinator = UpstreamDetailCoordinator::new();
        let guard = coordinator
            .claim("npm1:express", Duration::from_secs(5))
            .await
            .expect("first caller does the work");

        let second = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                coordinator
                    .claim("npm1:express", Duration::from_secs(5))
                    .await
                    .is_none()
            })
        };

        // Let the second caller reach the wait before the guard is released,
        // so this exercises the wake rather than the fast path.
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(guard);

        assert!(
            second.await.unwrap(),
            "the second caller should have waited and then re-read the cache"
        );
    }

    /// A different package is a different key, and must not be serialised
    /// behind an unrelated read.
    #[tokio::test]
    async fn a_different_key_is_not_blocked() {
        let coordinator = UpstreamDetailCoordinator::new();
        let _held = coordinator
            .claim("npm1:express", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(coordinator
            .claim("npm1:lodash", Duration::from_secs(5))
            .await
            .is_some());
    }

    /// A wedged holder must not hold every other reader with it.
    #[tokio::test]
    async fn a_waiter_gives_up_rather_than_hanging_forever() {
        let coordinator = UpstreamDetailCoordinator::new();
        let _held = coordinator
            .claim("npm1:express", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(coordinator
            .claim("npm1:express", Duration::from_millis(10))
            .await
            .is_none());
    }

    /// An early return must not leave a key claimed for the life of the
    /// process, which would wedge that package's page permanently.
    #[tokio::test]
    async fn dropping_the_guard_releases_the_key() {
        let coordinator = UpstreamDetailCoordinator::new();
        drop(
            coordinator
                .claim("npm1:express", Duration::from_secs(5))
                .await
                .unwrap(),
        );
        assert!(coordinator
            .claim("npm1:express", Duration::from_secs(5))
            .await
            .is_some());
    }

    #[tokio::test]
    async fn an_absence_is_remembered_until_it_expires() {
        let coordinator = UpstreamDetailCoordinator::new();
        assert!(!coordinator.is_absent("npm1:nope"));

        coordinator.record_absent("npm1:nope", Duration::from_secs(60));
        assert!(coordinator.is_absent("npm1:nope"));
        assert!(!coordinator.is_absent("npm1:other"));

        coordinator.clear_absent();
        assert!(!coordinator.is_absent("npm1:nope"));
    }

    #[tokio::test]
    async fn an_expired_absence_is_forgotten() {
        let coordinator = UpstreamDetailCoordinator::new();
        coordinator.record_absent("npm1:nope", Duration::from_millis(5));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!coordinator.is_absent("npm1:nope"));
    }

    /// A zero TTL is "do not remember", not "remember forever".
    #[tokio::test]
    async fn a_zero_negative_ttl_remembers_nothing() {
        let coordinator = UpstreamDetailCoordinator::new();
        coordinator.record_absent("npm1:nope", Duration::ZERO);
        assert!(!coordinator.is_absent("npm1:nope"));
    }
}
