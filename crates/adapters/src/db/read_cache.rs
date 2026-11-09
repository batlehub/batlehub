//! In-process cache for the reads every download repeats (block, flags,
//! visibility, PAT, user block, dedup mapping), kept coherent across replicas
//! by Postgres itself.
//!
//! Migration 066 puts a row trigger on each source table that runs
//! `pg_notify('batlehub_invalidate', <table>)` when a change could alter a
//! cached answer. [`spawn_invalidation_listener`] `LISTEN`s on one dedicated
//! connection and clears that table's cache on each notification.
//!
//! The cache is only consulted while that listener is connected
//! ([`ReadCaches::live`]): before the first connection, and from the moment it
//! drops until it is back (when everything is cleared), every read goes to the
//! database as before. A block therefore reaches every replica within one
//! notification round trip, never a TTL.
//!
//! A read races a concurrent invalidation: the row is read, the change commits
//! and its notification clears the cache, then the stale row is inserted. Each
//! cache has a generation that every clear bumps; a reader takes it before the
//! query ([`ReadCache::ticket`]) and [`ReadCache::put`] drops the row if it moved.
//!
//! Process-global: one database per process, and the listener is started by
//! the server only — a test binary that never starts it never reads a cached
//! row.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use batlehub_core::entities::{PackageFlag, Visibility};
use batlehub_core::ports::UserToken;

/// The channel migration 066's triggers notify.
pub const CHANNEL: &str = "batlehub_invalidate";

/// A safety net, not the coherence mechanism: a row is re-read at least this
/// often even if a notification was somehow lost while connected.
const TTL: Duration = Duration::from_secs(300);

/// Entries per cache before it is cleared wholesale.
// shortcut: clear-all on overflow instead of LRU eviction; revisit if a cache's hit rate drops at this size.
const CAP: usize = 100_000;

pub struct ReadCache<K, V> {
    map: Mutex<HashMap<K, (Instant, V)>>,
    generation: AtomicU64,
}

/// Taken before a database read; the read's row is cached only if no
/// invalidation happened since.
#[derive(Clone, Copy)]
pub struct Ticket(Option<u64>);

impl<K: Eq + Hash, V: Clone> ReadCache<K, V> {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        if !CACHES.live() {
            return None;
        }
        let map = self.map.lock().ok()?;
        let (at, v) = map.get(key)?;
        (at.elapsed() < TTL).then(|| v.clone())
    }

    pub fn ticket(&self) -> Ticket {
        Ticket(
            CACHES
                .live()
                .then(|| self.generation.load(Ordering::Acquire)),
        )
    }

    pub fn put(&self, ticket: Ticket, key: K, value: V) {
        let Some(generation) = ticket.0 else { return };
        let Ok(mut map) = self.map.lock() else { return };
        // Checked under the lock `clear` also takes, so a clear cannot land
        // between this check and the insert.
        if !CACHES.live() || self.generation.load(Ordering::Acquire) != generation {
            return;
        }
        if map.len() >= CAP {
            map.clear();
        }
        map.insert(key, (Instant::now(), value));
    }

    /// The cached value, else `load`'s, cached when it is `Ok`.
    pub async fn get_or_try<E>(
        &self,
        key: K,
        load: impl std::future::Future<Output = Result<V, E>>,
    ) -> Result<V, E> {
        if let Some(v) = self.get(&key) {
            return Ok(v);
        }
        let ticket = self.ticket();
        let v = load.await?;
        self.put(ticket, key, v.clone());
        Ok(v)
    }

    /// Of `keys`, those with nothing cached — what a bulk prefetch reads.
    /// Empty when the cache is not live, so a prefetch then reads nothing.
    pub fn missing<'a>(&self, keys: impl IntoIterator<Item = &'a K>) -> Vec<&'a K>
    where
        K: 'a,
    {
        if !CACHES.live() {
            return Vec::new();
        }
        keys.into_iter().filter(|k| self.get(k).is_none()).collect()
    }

    /// Forget every entry `pred` matches, on this replica at once.
    pub fn forget_where(&self, pred: impl Fn(&V) -> bool) {
        if let Ok(mut map) = self.map.lock() {
            self.generation.fetch_add(1, Ordering::AcqRel);
            map.retain(|_, (_, v)| !pred(v));
        }
    }

    /// Forget one key on this replica at once, ahead of the notification —
    /// for a writer whose own next read must not see the old row.
    pub fn forget(&self, key: &K) {
        if let Ok(mut map) = self.map.lock() {
            self.generation.fetch_add(1, Ordering::AcqRel);
            map.remove(key);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut map) = self.map.lock() {
            self.generation.fetch_add(1, Ordering::AcqRel);
            map.clear();
        }
    }

    /// Clears this cache when dropped: held across a write, it clears after
    /// the commit, so this replica's next read does not wait for its own
    /// notification (a clear before the commit could be refilled with the
    /// old row).
    pub fn clear_after(&self) -> ClearAfter<'_, K, V> {
        ClearAfter(self)
    }
}

pub struct ClearAfter<'a, K: Eq + Hash, V: Clone>(&'a ReadCache<K, V>);

impl<K: Eq + Hash, V: Clone> Drop for ClearAfter<'_, K, V> {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// `(registry, name, version, artifact)`, as `covering_block` reads it.
pub type BlockKey = (String, String, String, Option<String>);

pub struct ReadCaches {
    live: AtomicBool,
    /// `package_statuses`: the covering block reason, `None` included.
    pub blocks: ReadCache<BlockKey, Option<String>>,
    /// `package_flags`: every unrevoked flag of `(registry, name)`; expiry is
    /// re-checked on read.
    pub flags: ReadCache<(String, String), Vec<PackageFlag>>,
    /// `local_packages`: a package's visibility.
    pub visibility: ReadCache<(String, String), Visibility>,
    /// `user_tokens`: a live token by hash; an unknown hash is never cached.
    pub tokens: ReadCache<String, UserToken>,
    /// `user_blocks`: whether a user is blocked.
    pub user_blocks: ReadCache<String, bool>,
    /// `artifact_dedup_refs` ⋈ `artifact_dedup_index` ⋈ `artifact_storage`:
    /// logical key → (content key, backend name); a miss is never cached.
    pub dedup: ReadCache<String, (String, String)>,
}

impl ReadCaches {
    pub fn live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    fn set_live(&self, live: bool) {
        // Cleared on both edges: going down, nothing cached may outlive the
        // listener; coming up, whatever changed while it was down is unknown.
        self.clear_all();
        self.live.store(live, Ordering::Release);
        metrics::gauge!("batlehub_read_cache_live").set(if live { 1.0 } else { 0.0 });
    }

    pub fn clear_all(&self) {
        self.blocks.clear();
        self.flags.clear();
        self.visibility.clear();
        self.tokens.clear();
        self.user_blocks.clear();
        self.dedup.clear();
    }

    /// Clear what a notification from `table` may have made stale.
    pub fn invalidate(&self, table: &str) {
        match table {
            "package_statuses" => self.blocks.clear(),
            "package_flags" => self.flags.clear(),
            "local_packages" => self.visibility.clear(),
            "user_tokens" => self.tokens.clear(),
            "user_blocks" => self.user_blocks.clear(),
            "artifact_dedup_refs" | "artifact_dedup_index" | "artifact_storage" => {
                self.dedup.clear()
            }
            // A payload this binary does not know (a newer migration): clear
            // everything rather than guess.
            _ => self.clear_all(),
        }
        metrics::counter!("batlehub_read_cache_invalidations_total").increment(1);
    }
}

pub static CACHES: LazyLock<ReadCaches> = LazyLock::new(|| ReadCaches {
    live: AtomicBool::new(false),
    blocks: ReadCache::new(),
    flags: ReadCache::new(),
    visibility: ReadCache::new(),
    tokens: ReadCache::new(),
    user_blocks: ReadCache::new(),
    dedup: ReadCache::new(),
});

/// Start the `LISTEN` loop that makes [`CACHES`] usable. Until it connects
/// and has seen its own heartbeat delivered, and whenever it is disconnected,
/// every read goes to the database.
///
/// `listen_url` (`[database] listen_url`) gives the listener its own session
/// connection; absent, it holds one connection of `pool`.
#[cfg(feature = "db-postgres")]
pub fn spawn_invalidation_listener(pool: sqlx::PgPool, listen_url: Option<String>) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        let mut quiet = false;
        loop {
            let was_live = CACHES.live();
            match listen(&pool, listen_url.as_deref()).await {
                Ok(()) => backoff = Duration::from_secs(1),
                // Once per outage at warn, then quietly until it recovers: a
                // pooler that never delivers would otherwise log forever.
                Err(e) if was_live || !quiet => {
                    tracing::warn!(error = %e, "read cache off: its invalidation listener is down, every read goes to the database");
                    quiet = true;
                }
                Err(e) => tracing::debug!(error = %e, "read cache listener still down"),
            }
            if CACHES.live() {
                quiet = false;
            }
            CACHES.set_live(false);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
}

/// How often the listener proves notifications still reach it. A listener
/// whose connection dies silently (no RST: a partition, a pooler restart)
/// keeps serving for at most two of these, so this is the bound on a stale
/// answer: 20 s, for one `pg_notify` per replica every 10 s.
const HEARTBEAT: Duration = Duration::from_secs(10);

#[cfg(feature = "db-postgres")]
async fn listen(pool: &sqlx::PgPool, listen_url: Option<&str>) -> Result<(), sqlx::Error> {
    use sqlx::postgres::PgListener;
    let mut listener = match listen_url {
        Some(url) => PgListener::connect(url).await?,
        None => PgListener::connect_with(pool).await?,
    };
    listener.listen(CHANNEL).await?;
    // A connected LISTEN is not a delivered NOTIFY: behind a transaction-mode
    // pooler (PgBouncer) the notifications go to whichever server connection
    // ran the LISTEN, not to this client. So the cache goes live only once a
    // heartbeat sent through `pool` — the way every write is sent — has come
    // back, and goes down when one does not.
    let mut token = heartbeat(pool).await?;
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match listener.try_recv().await? {
                Some(n) if n.payload() == token => return Ok(()),
                Some(_) => {}
                None => return Err(sqlx::Error::PoolClosed),
            }
        }
    })
    .await;
    match first {
        Ok(r) => r?,
        Err(_) => {
            return Err(sqlx::Error::Protocol(
                "a NOTIFY sent through the pool never reached the listener (a transaction-mode \
                 pooler? set [database] listen_url to a direct connection)"
                    .into(),
            ))
        }
    }
    CACHES.set_live(true);
    tracing::info!("read cache on: invalidation listener connected");
    let mut tick = tokio::time::interval(HEARTBEAT);
    tick.tick().await;
    let mut seen = true;
    loop {
        tokio::select! {
            n = listener.try_recv() => match n? {
                Some(n) if n.payload() == token => seen = true,
                // Another replica's heartbeat changes nothing here.
                Some(n) if n.payload().starts_with(HEARTBEAT_PREFIX) => {}
                Some(n) => CACHES.invalidate(n.payload()),
                // The connection was lost (sqlx reconnects quietly): whatever
                // was notified meanwhile is gone, so the caller drops `live`.
                None => return Err(sqlx::Error::PoolClosed),
            },
            _ = tick.tick() => {
                if !seen {
                    return Err(sqlx::Error::Protocol("the last heartbeat was not delivered".into()));
                }
                seen = false;
                token = heartbeat(pool).await?;
            }
        }
    }
}

const HEARTBEAT_PREFIX: &str = "heartbeat:";

/// Send a heartbeat no other process can have sent, and return it.
#[cfg(feature = "db-postgres")]
async fn heartbeat(pool: &sqlx::PgPool) -> Result<String, sqlx::Error> {
    let token = format!("{HEARTBEAT_PREFIX}{}", uuid::Uuid::new_v4());
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(&token)
        .execute(pool)
        .await?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: CACHES is process-global and these flip `live`.
    #[test]
    fn a_read_is_cached_only_while_live_and_not_across_an_invalidation() {
        let c = &CACHES.user_blocks;
        // Not live: nothing is stored, nothing is served.
        let t = c.ticket();
        c.put(t, "u".into(), true);
        assert_eq!(c.get(&"u".to_string()), None);

        CACHES.set_live(true);
        let t = c.ticket();
        c.put(t, "u".into(), true);
        assert_eq!(c.get(&"u".to_string()), Some(true));

        // The race: ticket, invalidation, then the stale put.
        let t = c.ticket();
        CACHES.invalidate("user_blocks");
        assert_eq!(c.get(&"u".to_string()), None);
        c.put(t, "u".into(), false);
        assert_eq!(
            c.get(&"u".to_string()),
            None,
            "a put across a clear is dropped"
        );

        // Only the notified table's cache is cleared.
        let tb = CACHES.blocks.ticket();
        let key: BlockKey = ("r".into(), "n".into(), "1".into(), None);
        CACHES.blocks.put(tb, key.clone(), None);
        CACHES.invalidate("user_tokens");
        assert_eq!(CACHES.blocks.get(&key), Some(None));
        CACHES.invalidate("artifact_storage");
        assert_eq!(CACHES.blocks.get(&key), Some(None));
        CACHES.invalidate("some_future_table");
        assert_eq!(
            CACHES.blocks.get(&key),
            None,
            "an unknown payload clears all"
        );

        // Going down clears everything and stops serving.
        let t = c.ticket();
        c.put(t, "u".into(), true);
        CACHES.set_live(false);
        CACHES.live.store(true, Ordering::Release);
        assert_eq!(c.get(&"u".to_string()), None, "set_live(false) cleared it");
        CACHES.set_live(false);
    }
}
