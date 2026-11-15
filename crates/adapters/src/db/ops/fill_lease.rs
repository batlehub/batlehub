//! [`FillLeases`] on the `fill_leases` table (RFC 0048-bis §6.3): the lease of
//! a cold fill shared by every replica on one database.
//!
//! - `take` is one upsert that replaces only an expired, failed or `no-store`
//!   row; every holder write is fenced on its `fill_id` (T1).
//! - `commit` and `finish` notify `batlehub_invalidate` with
//!   `fill:<uuid>:<committed>[:<phase json>]` in the same statement, so a
//!   follower on another replica wakes without a query of its own; the
//!   listener of [`read_cache`](crate::db::read_cache) routes those payloads
//!   to [`route_notification`].
//! - While that listener is not live, a follower polls its row once a second.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use sqlx::{PgPool, Row};
use tokio::sync::watch;

use batlehub_core::{
    error::CoreError,
    ports::{FillError, FillId, FillLeases, LeaseState, Phase, Taken, LEASE_TTL},
};

use crate::db::read_cache::{CACHES, CHANNEL};
use crate::db::DbResultExt;

/// The payload prefix of a fill's progress on the invalidation channel.
const PREFIX: &str = "fill:";

/// §7.3: the failure a row or a notification carries is cut to this.
const ERROR_MAX: usize = 1024;

/// How often a follower reads its row while the listener is down.
const POLL: Duration = Duration::from_secs(1);

/// What a notification said about a fill: its committed count and, on a
/// phase change, the new phase.
type Notice = (u32, Option<Phase>);

/// The fills this process follows, by fill id. A `watch` keeps only the
/// newest notice, which is all a follower needs: counts only grow.
static FOLLOWED: LazyLock<Mutex<HashMap<FillId, watch::Sender<Option<Notice>>>>> =
    LazyLock::new(Mutex::default);

/// Hand a `fill:` payload to the followers of its fill. `false` for any
/// other payload, which the caller treats as a table invalidation.
pub fn route_notification(payload: &str) -> bool {
    let Some(rest) = payload.strip_prefix(PREFIX) else {
        return false;
    };
    let mut parts = rest.splitn(3, ':');
    let parsed = (|| {
        let fill = parts.next()?.parse::<FillId>().ok()?;
        let committed = parts.next()?.parse::<u32>().ok()?;
        let phase = match parts.next() {
            Some(json) => Some(serde_json::from_str::<Phase>(json).ok()?),
            None => None,
        };
        Some((fill, (committed, phase)))
    })();
    // Anything else under the prefix is ignored (§7.3), never an invalidation.
    if let Some((fill, notice)) = parsed {
        let followed = FOLLOWED.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tx) = followed.get(&fill) {
            tx.send_replace(Some(notice));
        }
    }
    true
}

/// A follower's registration; dropping the last one forgets the fill.
struct Followed {
    fill: FillId,
    rx: watch::Receiver<Option<Notice>>,
}

impl Followed {
    fn new(fill: FillId) -> Self {
        let mut followed = FOLLOWED.lock().unwrap_or_else(|p| p.into_inner());
        let rx = followed
            .entry(fill)
            .or_insert_with(|| watch::channel(None).0)
            .subscribe();
        Self { fill, rx }
    }
}

impl Drop for Followed {
    fn drop(&mut self) {
        let mut followed = FOLLOWED.lock().unwrap_or_else(|p| p.into_inner());
        // This receiver is still alive here, so one left means it was the last.
        if followed
            .get(&self.fill)
            .is_some_and(|tx| tx.receiver_count() <= 1)
        {
            followed.remove(&self.fill);
        }
    }
}

pub struct PgFillLeases {
    pool: PgPool,
    ttl: Duration,
}

impl PgFillLeases {
    pub fn new(pool: PgPool) -> Self {
        Self::with_ttl(pool, LEASE_TTL)
    }

    /// A shorter lease, for tests that wait for one to expire.
    pub fn with_ttl(pool: PgPool, ttl: Duration) -> Self {
        Self { pool, ttl }
    }

    fn ttl_secs(&self) -> f64 {
        self.ttl.as_secs_f64()
    }
}

fn phase_columns(phase: &Phase) -> (&'static str, Option<i64>, Option<serde_json::Value>) {
    match phase {
        Phase::Filling => ("filling", None, None),
        Phase::Verified { total } => ("verified", i64::try_from(*total).ok(), None),
        Phase::Done => ("done", None, None),
        Phase::Failed(e) => ("failed", None, serde_json::to_value(e).ok()),
        Phase::NoStore => ("no_store", None, None),
    }
}

fn phase_of(name: &str, total: Option<i64>, error: Option<serde_json::Value>) -> Phase {
    match name {
        "filling" => Phase::Filling,
        "verified" => Phase::Verified {
            total: total.and_then(|t| u64::try_from(t).ok()).unwrap_or(0),
        },
        "done" => Phase::Done,
        "no_store" => Phase::NoStore,
        _ => Phase::Failed(
            error
                .and_then(|e| serde_json::from_value(e).ok())
                .unwrap_or_else(|| FillError::other("the cold fill failed on another replica")),
        ),
    }
}

fn count(row: &sqlx::postgres::PgRow, col: &str) -> u32 {
    row.try_get::<i32, _>(col)
        .ok()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// A row as a reader sees it, and whether its lease has expired.
fn state_of(row: &sqlx::postgres::PgRow) -> Result<(LeaseState, bool), CoreError> {
    let phase: String = row.try_get("phase").db_err()?;
    let state = LeaseState {
        fill: row.try_get("fill_id").db_err()?,
        holder: row.try_get("holder").db_err()?,
        committed: count(row, "committed"),
        phase: phase_of(
            &phase,
            row.try_get("total_bytes").db_err()?,
            row.try_get("error").db_err()?,
        ),
    };
    Ok((state, row.try_get("expired").db_err()?))
}

const SELECT_ROW: &str = "SELECT fill_id, holder, committed, phase, total_bytes, error, \
     expires_at < NOW() AS expired FROM fill_leases WHERE storage_key = $1";

async fn read_row(pool: &PgPool, key: &str) -> Result<Option<(LeaseState, bool)>, CoreError> {
    let row = sqlx::query(SELECT_ROW)
        .bind(key)
        .fetch_optional(pool)
        .await
        .db_err()?;
    row.as_ref().map(state_of).transpose()
}

fn dead(state: &LeaseState, expired: bool) -> bool {
    expired || matches!(state.phase, Phase::Failed(_) | Phase::NoStore)
}

#[async_trait]
impl FillLeases for PgFillLeases {
    fn shared(&self) -> bool {
        true
    }

    async fn take(&self, key: &str, holder: &str) -> Result<Taken, CoreError> {
        // Two attempts cover a concurrent take racing this one: the loser's
        // upsert updates nothing, and it reads the winner's row.
        for _ in 0..3 {
            let fill = FillId::new_v4();
            let row = sqlx::query(
                r#"
                WITH prev AS (
                    SELECT fill_id, holder, committed, written, phase, total_bytes, error,
                           expires_at < NOW() AS expired
                    FROM fill_leases WHERE storage_key = $1
                ), ins AS (
                    INSERT INTO fill_leases
                        (storage_key, fill_id, holder, expires_at, phase, committed, written)
                    VALUES ($1, $2, $3, NOW() + make_interval(secs => $4), 'filling', 0, 1)
                    ON CONFLICT (storage_key) DO UPDATE SET
                        fill_id = EXCLUDED.fill_id, holder = EXCLUDED.holder,
                        expires_at = EXCLUDED.expires_at, phase = 'filling',
                        committed = 0, written = 1, total_bytes = NULL, error = NULL,
                        started_at = NOW()
                    WHERE fill_leases.expires_at < NOW()
                       OR fill_leases.phase IN ('failed', 'no_store')
                    RETURNING fill_id
                )
                SELECT (SELECT fill_id FROM ins) AS taken, prev.*
                FROM (SELECT 1) AS one LEFT JOIN prev ON TRUE
                "#,
            )
            .bind(key)
            .bind(fill)
            .bind(holder)
            .bind(self.ttl_secs())
            .fetch_one(&self.pool)
            .await
            .db_err()?;
            let prev: Option<FillId> = row.try_get("fill_id").db_err()?;
            if row
                .try_get::<Option<FillId>, _>("taken")
                .db_err()?
                .is_some()
            {
                // `prev` is the statement's snapshot: a takeover racing this
                // one between it and the upsert leaves segments the staging
                // sweep removes a day later.
                let previous = prev.map(|p| (p, count(&row, "written")));
                return Ok(Taken::Started { fill, previous });
            }
            if prev.is_some() {
                let (state, expired) = state_of(&row)?;
                if !dead(&state, expired) {
                    return Ok(Taken::Running(state));
                }
            }
            // Raced: read what the winner wrote.
            match read_row(&self.pool, key).await? {
                Some((state, expired)) if !dead(&state, expired) => {
                    return Ok(Taken::Running(state))
                }
                _ => continue,
            }
        }
        Err(CoreError::Registry(format!(
            "fill lease on {key}: three concurrent takes in a row"
        )))
    }

    async fn commit(&self, key: &str, fill: FillId, committed: u32) -> Result<bool, CoreError> {
        let n = i32::try_from(committed).unwrap_or(i32::MAX);
        let row = sqlx::query(
            r#"
            WITH u AS (
                UPDATE fill_leases SET committed = $3, written = $3 + 1,
                       expires_at = NOW() + make_interval(secs => $4)
                WHERE storage_key = $1 AND fill_id = $2 AND phase = 'filling'
                RETURNING committed
            )
            SELECT pg_notify($5, 'fill:' || $2::text || ':' || committed) FROM u
            "#,
        )
        .bind(key)
        .bind(fill)
        .bind(n)
        .bind(self.ttl_secs())
        .bind(CHANNEL)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        Ok(row.is_some())
    }

    async fn heartbeat(&self, key: &str, fill: FillId) -> Result<bool, CoreError> {
        let done = sqlx::query(
            "UPDATE fill_leases SET expires_at = NOW() + make_interval(secs => $3) \
             WHERE storage_key = $1 AND fill_id = $2 \
               AND phase NOT IN ('done', 'failed', 'no_store')",
        )
        .bind(key)
        .bind(fill)
        .bind(self.ttl_secs())
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(done.rows_affected() > 0)
    }

    async fn finish(&self, key: &str, fill: FillId, phase: Phase) -> Result<bool, CoreError> {
        let phase = match phase {
            Phase::Failed(e) => Phase::Failed(e.truncated(ERROR_MAX)),
            other => other,
        };
        let (name, total, error) = phase_columns(&phase);
        let all = match phase {
            Phase::Verified { total } => {
                Some(i32::try_from(batlehub_core::ports::segments_for(total)).unwrap_or(i32::MAX))
            }
            _ => None,
        };
        let json = serde_json::to_string(&phase)
            .map_err(|e| CoreError::Registry(format!("fill lease phase: {e}")))?;
        let row = sqlx::query(
            r#"
            WITH u AS (
                UPDATE fill_leases SET phase = $3, total_bytes = $4, error = $5,
                       committed = COALESCE($6, committed),
                       written = GREATEST(written, COALESCE($6, written)),
                       expires_at = NOW() + make_interval(secs => $7)
                WHERE storage_key = $1 AND fill_id = $2
                  AND phase NOT IN ('done', 'failed', 'no_store')
                RETURNING committed
            )
            SELECT pg_notify($8, 'fill:' || $2::text || ':' || committed || ':' || $9) FROM u
            "#,
        )
        .bind(key)
        .bind(fill)
        .bind(name)
        .bind(total)
        .bind(error)
        .bind(all)
        .bind(self.ttl_secs())
        .bind(CHANNEL)
        .bind(json)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        Ok(row.is_some())
    }

    async fn release(&self, key: &str, fill: FillId) -> Result<(), CoreError> {
        sqlx::query("DELETE FROM fill_leases WHERE storage_key = $1 AND fill_id = $2")
            .bind(key)
            .bind(fill)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    fn follow(&self, key: &str, fill: FillId) -> BoxStream<'static, Result<LeaseState, CoreError>> {
        let follower = Follower {
            pool: self.pool.clone(),
            key: key.to_owned(),
            ttl: self.ttl,
            followed: Followed::new(fill),
            last: None,
            ended: false,
        };
        Box::pin(futures::stream::unfold(follower, |mut f| async move {
            if f.ended {
                return None;
            }
            match f.next().await {
                Some(Ok(state)) => {
                    f.ended = state.phase.is_terminal();
                    Some((Ok(state), f))
                }
                Some(Err(e)) => {
                    f.ended = true;
                    Some((Err(e), f))
                }
                None => None,
            }
        }))
    }

    async fn expired(&self, older_than: Duration) -> Result<Vec<(String, FillId, u32)>, CoreError> {
        // `expires_at` is the last extension plus the TTL.
        let slack = self.ttl.as_secs_f64() - older_than.as_secs_f64();
        let rows = sqlx::query(
            "SELECT storage_key, fill_id, written FROM fill_leases \
             WHERE expires_at < NOW() + make_interval(secs => $1)",
        )
        .bind(slack)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("storage_key").db_err()?,
                    r.try_get("fill_id").db_err()?,
                    count(r, "written"),
                ))
            })
            .collect()
    }
}

/// One replica's follow of a fill another replica holds.
struct Follower {
    pool: PgPool,
    key: String,
    ttl: Duration,
    followed: Followed,
    last: Option<LeaseState>,
    ended: bool,
}

impl Follower {
    /// The next state that differs from the last one yielded; `None` once the
    /// fill no longer holds the row or its lease expired.
    async fn next(&mut self) -> Option<Result<LeaseState, CoreError>> {
        loop {
            let state = match &self.last {
                // The first read is a query: a commit between the caller's
                // `take` and this registration sent a notice nobody heard.
                None => self.poll().await,
                Some(last) if CACHES.live() => {
                    match tokio::time::timeout(self.ttl, self.followed.rx.changed()).await {
                        Ok(Ok(())) => match self.followed.rx.borrow_and_update().clone() {
                            Some((committed, phase)) => Some(Ok(LeaseState {
                                committed: committed.max(last.committed),
                                phase: phase.unwrap_or_else(|| last.phase.clone()),
                                ..last.clone()
                            })),
                            None => continue,
                        },
                        // Nothing for a whole lease: is the holder still there?
                        Ok(Err(_)) | Err(_) => self.poll().await,
                    }
                }
                Some(_) => {
                    metrics::counter!("batlehub_proxy_fill_follow_polls_total").increment(1);
                    tokio::time::sleep(POLL).await;
                    self.poll().await
                }
            };
            match state {
                Some(Ok(s)) if self.last.as_ref() != Some(&s) => {
                    self.last = Some(s.clone());
                    return Some(Ok(s));
                }
                Some(Ok(_)) => {}
                other => return other,
            }
        }
    }

    async fn poll(&self) -> Option<Result<LeaseState, CoreError>> {
        match read_row(&self.pool, &self.key).await {
            Ok(Some((state, expired))) if state.fill == self.followed.fill && !expired => {
                Some(Ok(state))
            }
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_reach_the_followers_of_their_fill_only() {
        let fill = FillId::new_v4();
        let mut f = Followed::new(fill);
        assert!(!route_notification("artifact_storage"));
        assert!(
            route_notification("fill:not-a-uuid:1"),
            "ignored, not an invalidation"
        );
        assert!(route_notification(&format!("fill:{}:3", FillId::new_v4())));
        assert!(!f.rx.has_changed().unwrap(), "another fill's notice");
        assert!(route_notification(&format!("fill:{fill}:3")));
        assert_eq!(*f.rx.borrow_and_update(), Some((3, None)));
        let failed = Phase::Failed(FillError::other("a: b"));
        let json = serde_json::to_string(&failed).unwrap();
        assert!(route_notification(&format!("fill:{fill}:4:{json}")));
        assert_eq!(*f.rx.borrow_and_update(), Some((4, Some(failed))));
        drop(f);
        assert!(
            !FOLLOWED.lock().unwrap().contains_key(&fill),
            "the last follower's drop forgets the fill"
        );
    }

    #[test]
    fn phases_round_trip_through_their_columns() {
        for phase in [
            Phase::Filling,
            Phase::Verified { total: 42 },
            Phase::Done,
            Phase::Failed(FillError::other("boom")),
            Phase::NoStore,
        ] {
            let (name, total, error) = phase_columns(&phase);
            assert_eq!(phase_of(name, total, error), phase);
        }
    }
}
