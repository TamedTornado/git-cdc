//! Administrative CLI idempotency contracts against real `PostgreSQL`.
#![allow(clippy::unwrap_used, reason = "process fixtures fail immediately")]

use std::process::Command;

use git_cdc_server::migrate;
use sqlx::PgPool;

#[tokio::test]
#[serial_test::serial]
async fn repository_provisioning_is_safely_repeatable() {
    let database_url = std::env::var("GIT_CDC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc".into());
    let pool = PgPool::connect(&database_url).await.unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query("TRUNCATE repositories CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_git-cdc-admin"))
            .env("GIT_CDC_DATABASE_URL", &database_url)
            .args(["repository-add", "alice", "assets"])
            .output()
            .unwrap()
    };

    let first = invoke();
    let second = invoke();
    assert!(first.status.success());
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout);
}
