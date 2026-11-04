//! A started login keeps its `return_to` (migration 064, RFC 0040 §6.5) and
//! gives it back once: the CLI's loopback login depends on the callback
//! reading the listener the login leg recorded.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_login_states

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::PgPackageRepository;
use batlehub_core::ports::{LoginState, LoginStateStore};
use uuid::Uuid;

#[tokio::test]
async fn return_to_round_trips_and_is_taken_once() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let repo = PgPackageRepository::new(
        &url,
        PoolOptions {
            max_connections: 2,
            min_connections: 1,
            acquire_timeout_secs: 10,
        },
    )
    .await
    .expect("connect to postgres");
    repo.run_migrations().await.expect("run migrations");

    let login = |return_to: Option<&str>| LoginState {
        provider: "oidc".to_owned(),
        code_verifier: "v".to_owned(),
        nonce: "n".to_owned(),
        spa_state: "s".to_owned(),
        return_to: return_to.map(str::to_owned),
    };
    let (cli, console) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    let to = "http://127.0.0.1:48123/callback";
    repo.put(&cli, login(Some(to)), 60).await.unwrap();
    repo.put(&console, login(None), 60).await.unwrap();

    assert_eq!(repo.take(&cli).await.unwrap(), Some(login(Some(to))));
    assert_eq!(repo.take(&cli).await.unwrap(), None, "one-time");
    assert_eq!(repo.take(&console).await.unwrap(), Some(login(None)));
}
