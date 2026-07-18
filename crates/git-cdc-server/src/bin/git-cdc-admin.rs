//! Explicit administrative control surface for provisioning and safe GC.

use std::{sync::Arc, time::Duration};

use clap::{Parser, Subcommand};
use git_cdc_server::{gc, migrate, reconcile};
use git_cdc_storage::ChunkStore;
use object_store::prefix::PrefixStore;
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Git-CDC administrative operations")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Exits successfully when a running server reports readiness.
    Healthcheck {
        #[arg(long, default_value = "http://127.0.0.1:8080/readyz")]
        url: String,
    },
    /// Creates an explicit repository mapping.
    RepositoryAdd { owner: String, name: String },
    /// Creates or replaces one OIDC subject grant.
    Grant {
        repository_id: Uuid,
        subject: String,
        #[arg(long)]
        write: bool,
        #[arg(long)]
        admin: bool,
    },
    /// Mirrors ordinary Git refs and atomically submits a complete reachability epoch.
    Reconcile {
        repository_id: Uuid,
        git_url: String,
    },
    /// Prints every object currently proven eligible for staging.
    GcDryRun { repository_id: Uuid },
    /// Stages proven candidates behind a grace period.
    GcStage {
        repository_id: Uuid,
        #[arg(long, default_value_t = 604_800)]
        grace_seconds: u64,
    },
    /// Deletes due tombstones and drains crash-retryable chunk cleanup.
    GcCollect { repository_id: Uuid },
    /// Reclaims expired partial uploads after a quarantine grace period.
    UploadsReclaim {
        repository_id: Uuid,
        #[arg(long, default_value_t = 86_400)]
        grace_seconds: u64,
    },
}

#[tokio::main]
#[allow(
    clippy::too_many_lines,
    reason = "the administrative binary keeps explicit one-shot command dispatch in one place"
)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if let Commands::Healthcheck { url } = &cli.command {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()?
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        return Ok(());
    }
    let pool = PgPool::connect(&required("GIT_CDC_DATABASE_URL")?).await?;
    migrate(&pool).await?;
    match cli.command {
        Commands::Healthcheck { .. } => return Err("healthcheck dispatch failed".into()),
        Commands::RepositoryAdd { owner, name } => {
            let id = Uuid::new_v4();
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO repositories (id, owner, name) VALUES ($1, $2, $3) \
                 ON CONFLICT (owner, name) DO UPDATE SET owner = EXCLUDED.owner \
                 RETURNING id",
            )
            .bind(id)
            .bind(owner)
            .bind(name)
            .fetch_one(&pool)
            .await?;
            println!("{id}");
        }
        Commands::Grant {
            repository_id,
            subject,
            write,
            admin,
        } => {
            sqlx::query(
                "INSERT INTO repository_grants \
                 (repository_id, subject, can_read, can_write, can_admin) \
                 VALUES ($1, $2, true, $3, $4) \
                 ON CONFLICT (repository_id, subject) DO UPDATE SET \
                 can_read = true, can_write = EXCLUDED.can_write, can_admin = EXCLUDED.can_admin",
            )
            .bind(repository_id)
            .bind(subject)
            .bind(write || admin)
            .bind(admin)
            .execute(&pool)
            .await?;
        }
        Commands::Reconcile {
            repository_id,
            git_url,
        } => {
            let scan = tokio::task::spawn_blocking(move || reconcile::scan(&git_url)).await??;
            let epoch = gc::submit_snapshot(
                &pool,
                gc::ReachabilitySnapshot {
                    repository_id,
                    ref_fingerprint: &scan.ref_fingerprint,
                    reachable_objects: &scan.reachable_objects,
                },
            )
            .await?;
            println!(
                "epoch {epoch}: {} reachable objects",
                scan.reachable_objects.len()
            );
        }
        Commands::GcDryRun { repository_id } => {
            for candidate in gc::dry_run(&pool, repository_id).await? {
                println!("{}\t{}", candidate.oid, candidate.size);
            }
        }
        Commands::GcStage {
            repository_id,
            grace_seconds,
        } => {
            let candidates =
                gc::stage(&pool, repository_id, Duration::from_secs(grace_seconds)).await?;
            println!("staged {} objects", candidates.len());
        }
        Commands::GcCollect { repository_id } => {
            let storage_url = Url::parse(&required("GIT_CDC_STORAGE_URL")?)?;
            let (store, prefix) = object_store::parse_url_opts(&storage_url, std::env::vars())?;
            let chunks = ChunkStore::new(Arc::new(PrefixStore::new(store, prefix)));
            println!(
                "deleted {} objects",
                gc::collect_due(&pool, &chunks, repository_id).await?
            );
        }
        Commands::UploadsReclaim {
            repository_id,
            grace_seconds,
        } => {
            let storage_url = Url::parse(&required("GIT_CDC_STORAGE_URL")?)?;
            let (store, prefix) = object_store::parse_url_opts(&storage_url, std::env::vars())?;
            let chunks = ChunkStore::new(Arc::new(PrefixStore::new(store, prefix)));
            println!(
                "reclaimed {} expired upload sessions",
                gc::reclaim_expired_uploads(
                    &pool,
                    &chunks,
                    repository_id,
                    Duration::from_secs(grace_seconds),
                )
                .await?
            );
        }
    }
    Ok(())
}

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("required environment variable {name} is missing or empty").into())
}
