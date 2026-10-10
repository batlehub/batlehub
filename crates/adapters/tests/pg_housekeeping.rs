//! The housekeeping sweep deletes past-cutoff rows and nothing else.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_housekeeping

use sqlx::PgPool;
use uuid::Uuid;

async fn exists(pool: &PgPool, sql: &'static str, key: &str) -> bool {
    sqlx::query_scalar::<_, bool>(sql)
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn sweep_deletes_only_past_cutoff_rows() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .unwrap();
    let tag = Uuid::new_v4().to_string();
    let (old, live) = (format!("{tag}-old"), format!("{tag}-live"));

    for (key, expires) in [
        (&old, "NOW() - INTERVAL '1 minute'"),
        (&live, "NOW() + INTERVAL '1 hour'"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO metadata_cache (cache_key, metadata, cached_at, expires_at) \
             VALUES ($1, '{{}}', NOW(), {expires})"
        )))
        .bind(key)
        .execute(&pool)
        .await
        .unwrap();
    }
    // Old and finished, recently finished, and still open.
    for (name, completed) in [
        (&old, "NOW() - INTERVAL '8 days'"),
        (&live, "NOW() - INTERVAL '1 day'"),
        (&tag, "NULL"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO scan_jobs (id, registry, package_name, version, trigger, priority, completed_at) \
             VALUES ($1, 'hk', $2, '1', 'rescan', 2, {completed})"
        )))
        .bind(Uuid::new_v4())
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (ip, unblock) in [(&old, -60), (&live, 3600)] {
        sqlx::query(
            "INSERT INTO ip_blocks (ip, blocked_at, unblock_at) \
             VALUES ($1, 0, EXTRACT(EPOCH FROM NOW())::BIGINT + $2)",
        )
        .bind(ip)
        .bind(unblock as i64)
        .execute(&pool)
        .await
        .unwrap();
    }

    batlehub_adapters::db::housekeeping::sweep(&pool)
        .await
        .unwrap();

    let cache = "SELECT EXISTS (SELECT 1 FROM metadata_cache WHERE cache_key = $1)";
    let job = "SELECT EXISTS (SELECT 1 FROM scan_jobs WHERE package_name = $1)";
    let block = "SELECT EXISTS (SELECT 1 FROM ip_blocks WHERE ip = $1)";
    assert!(!exists(&pool, cache, &old).await);
    assert!(exists(&pool, cache, &live).await);
    assert!(!exists(&pool, job, &old).await);
    assert!(exists(&pool, job, &live).await);
    assert!(exists(&pool, job, &tag).await);
    assert!(!exists(&pool, block, &old).await);
    assert!(exists(&pool, block, &live).await);
}
