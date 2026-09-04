/// How many evicted keys one report carries.
///
/// An LRU sweep on a large estate evicts by the thousand; a report that
/// serialised every one of them would be the largest response this server ever
/// sends. What is dropped is *counted* and reported, because a bounded list
/// that does not say it is bounded reads as "this is everything".
pub const MAX_REPORTED_KEYS: usize = 2_000;

/// Summary of a completed eviction run, or — under `dry_run` — of what one
/// would have done.
///
/// The same structure either way, deliberately: an operator reads a preview,
/// turns `dry_run` off, and has to be able to compare the two without
/// translating between shapes. That is the shape `RetentionReport` already has,
/// for the same reason.
#[derive(Debug, Default, Clone)]
pub struct EvictionReport {
    pub total: usize,
    pub evicted_ttl: usize,
    pub evicted_idle: usize,
    pub evicted_old_versions: usize,
    pub evicted_lru: usize,
    /// Candidates the upstream-disappearance hold kept (RFC 0014 §5.3):
    /// artifacts upstream no longer has, which no strategy that assumes a
    /// re-fetch may take. The LRU size cap still can, and counts them here
    /// only when it skipped them in favour of a present candidate.
    pub held: usize,
    /// True when nothing was written.
    pub dry_run: bool,
    /// The storage keys evicted, or that would be under `dry_run`.
    ///
    /// The list an operator actually reads before running a new size cap live —
    /// a count alone cannot be checked against the policy that produced it.
    pub evicted_keys: Vec<String>,
    /// How many keys were dropped from [`Self::evicted_keys`]. Non-zero means
    /// the list is a sample, not the answer.
    pub keys_truncated: u64,
    /// Set when the run could not finish the question it was asked — today,
    /// only a `dry_run` size-cap preview that ran out of page before it ran out
    /// of excess. Partial results that say so, rather than a preview that looks
    /// like the whole answer. (`RetentionReport` carries the same field for the
    /// same reason.)
    pub incomplete_because: Option<String>,
}

impl EvictionReport {
    /// A run that may write.
    pub fn live() -> Self {
        Self::default()
    }

    /// A run that must not write, and reports what it would have taken.
    pub fn dry() -> Self {
        Self {
            dry_run: true,
            ..Self::default()
        }
    }

    /// Note one evicted key, keeping the list bounded.
    pub(super) fn record(&mut self, key: &str) {
        if self.evicted_keys.len() < MAX_REPORTED_KEYS {
            self.evicted_keys.push(key.to_owned());
        } else {
            self.keys_truncated += 1;
        }
    }
}

/// Summary of a coherence check run, or — under `dry_run` — of what one would
/// have done.
#[derive(Debug, Clone, Default)]
pub struct CoherenceReport {
    pub storage_keys: usize,
    pub meta_rows: usize,
    /// Blobs deleted, or that would be under `dry_run`.
    pub orphaned_deleted: usize,
    /// True when nothing was deleted **and** no blob was advanced toward
    /// deletion.
    pub dry_run: bool,
    /// The keys deleted, or that would be. Bounded by [`MAX_REPORTED_KEYS`].
    ///
    /// The list matters more here than anywhere else in this module: everything
    /// on it is a blob the sweep believes nothing references, and an operator
    /// who disagrees has no second copy to fall back on — the meta row that
    /// would have pointed at it is exactly what is missing.
    pub deleted_keys: Vec<String>,
    /// Blobs seen orphaned for the **first** time: not deleted, carried forward,
    /// and deletable by the next run if they are still orphaned then.
    ///
    /// Reported separately because "what would go if I run this again" is a
    /// different question from "what went", and the two-pass grace makes them
    /// different answers.
    pub first_seen_orphaned: usize,
    /// The first-strike keys, bounded the same way.
    pub first_seen_keys: Vec<String>,
    /// How many keys were dropped from the two lists above.
    pub keys_truncated: u64,
}

impl CoherenceReport {
    pub(super) fn record_deleted(&mut self, key: &str) {
        self.orphaned_deleted += 1;
        if self.deleted_keys.len() < MAX_REPORTED_KEYS {
            self.deleted_keys.push(key.to_owned());
        } else {
            self.keys_truncated += 1;
        }
    }

    pub(super) fn record_first_seen(&mut self, key: &str) {
        self.first_seen_orphaned += 1;
        if self.first_seen_keys.len() < MAX_REPORTED_KEYS {
            self.first_seen_keys.push(key.to_owned());
        } else {
            self.keys_truncated += 1;
        }
    }
}
