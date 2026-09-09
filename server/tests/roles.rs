//! The process roles, end to end (RFC 0018 §10, phase 5): the built binary
//! started with `--roles worker` alone drains a queue another process
//! seeded — at `Backfill` priority, the one the anti-starvation slot exists
//! for — and records the verdict. Nothing in process can prove this: the
//! role split is the binary's own start-up, and the queue is the database's.
//!
//! Needs a Postgres, like the adapters' `pg_*` suites, and reports itself
//! skipped without one:
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-server --test roles
//!
//! The OSV the worker asks is a `mockito` server in this test, so the run
//! costs no network and the verdict is the one the fake decided.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use batlehub_adapters::db::{PgScanQueue, PgVerdictRepository};
use batlehub_core::{
    entities::{PackageId, ScanTrigger, VerdictState},
    ports::{ScanQueue, VerdictRepository},
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

/// A guard that kills the child on drop, so a failing assertion never
/// leaves a server bound to the port.
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn a_worker_only_process_drains_a_queue_seeded_at_backfill_priority() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let mut osv = mockito::Server::new_async().await;
    let clean = osv
        .mock("POST", "/v1/query")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{}")
        .expect_at_least(1)
        .create_async()
        .await;

    let registry = format!("npm-roles-{}", std::process::id());
    let work = tempfile::tempdir().unwrap();
    let storage = work.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    // A registry behind `[security]`, the fake OSV, and a port nothing else
    // in the tree uses. `interval`-driven work is left off: the queue is
    // seeded by hand below.
    let config = format!(
        r#"
[server]
host = "127.0.0.1"
port = 8199

[database]
type = "postgresql"
url = "{url}"
max_connections = 3

[[auth]]
type = "token"

[[auth.tokens]]
value = "roles-token"
role = "admin"
user_id = "ci"

[storage]
type = "filesystem"
path = "{storage}"

[scanners.osv]
type = "osv"
api_url = "{osv}"

[[registries]]
type = "npm"
name = "{registry}"
mode = "proxy"
upstreams = ["https://registry.npmjs.org"]

[registries.security]
mode = "block"
min_age_secs = 3600
mature_age_secs = 0
scanners = ["osv"]
required_scanners = ["osv"]
"#,
        storage = storage.display(),
        osv = osv.url(),
    );
    let config_path = work.path().join("config.toml");
    std::fs::write(&config_path, config).unwrap();

    // Seed, as a proxy process would: one FirstSeen and one Backfill, so the
    // worker has both tiers to choose from and drains both.
    let pool = sqlx::PgPool::connect(&url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    let queue = PgScanQueue::new(pool.clone());
    let verdicts = PgVerdictRepository::new(pool.clone());
    let hot = PackageId::new(&registry, "left-pad", "1.3.1");
    let cold = PackageId::new(&registry, "left-pad", "1.3.0");
    let dated = Some(chrono::Utc::now() - chrono::Duration::days(30));
    assert!(queue
        .enqueue(&hot, dated, ScanTrigger::FirstSeen)
        .await
        .unwrap());
    assert!(queue
        .enqueue(&cold, dated, ScanTrigger::Backfill)
        .await
        .unwrap());

    let child = Command::new(env!("CARGO_BIN_EXE_batlehub"))
        .arg("--config")
        .arg(&config_path)
        .arg("--roles")
        .arg("worker")
        .env("BATLEHUB_DISABLE_HOT_RELOAD", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the server binary");
    let _guard = Child(child);

    // The worker polls every two seconds when idle; a minute is generous.
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut done = (None, None);
    while Instant::now() < deadline {
        done = (
            verdicts.get(&hot).await.unwrap(),
            verdicts.get(&cold).await.unwrap(),
        );
        if done.0.is_some() && done.1.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let (hot_v, cold_v) = done;
    let hot_v = hot_v.expect("the FirstSeen job was judged by the worker-only process");
    let cold_v = cold_v.expect("the Backfill job was judged by the worker-only process");
    assert_eq!(hot_v.state, VerdictState::Allowed, "{hot_v:?}");
    assert_eq!(cold_v.state, VerdictState::Allowed, "{cold_v:?}");
    // The internal scanners (`block_list`, `flags`) answer beside it.
    assert!(
        cold_v.scanners_done.iter().any(|s| s == "osv"),
        "{cold_v:?}"
    );
    clean.assert_async().await;

    // Both rows closed: nothing left for anyone to lease.
    let leftover = queue
        .lease("probe", std::slice::from_ref(&registry), 4, 5, 3)
        .await
        .unwrap();
    assert!(leftover.is_empty(), "{leftover:?}");
}
