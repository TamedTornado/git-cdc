//! Git LFS Delta server entrypoint.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use git_lfs_delta_server::{AppState, build_router, migrate};
use git_lfs_delta_storage::ChunkStore;
use object_store::prefix::PrefixStore;
use sqlx::postgres::PgPoolOptions;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "git_lfs_delta_server=info,tower_http=info".into()),
        )
        .json()
        .init();

    let database_url = required("GIT_LFS_DELTA_DATABASE_URL")?;
    let base_url = Url::parse(&required("GIT_LFS_DELTA_BASE_URL")?)?;
    let storage_url = Url::parse(&required("GIT_LFS_DELTA_STORAGE_URL")?)?;
    let auth_mode = required("GIT_LFS_DELTA_AUTH_MODE")?;
    let bind: SocketAddr = std::env::var("GIT_LFS_DELTA_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    let maximum_connections = optional_usize("GIT_LFS_DELTA_DATABASE_MAX_CONNECTIONS", 20)?;
    let maximum_basic_transfers = optional_usize("GIT_LFS_DELTA_MAX_BASIC_TRANSFERS", 2)?;
    let maximum_data_requests = optional_usize("GIT_LFS_DELTA_MAX_DATA_REQUESTS", 64)?;
    let staging_root = PathBuf::from(
        std::env::var("GIT_LFS_DELTA_STAGING_DIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
    );
    if auth_mode == "development"
        && !bind.ip().is_loopback()
        && std::env::var_os("GIT_LFS_DELTA_ALLOW_REMOTE_DEVELOPMENT_AUTH").is_none()
    {
        return Err("development authentication may not bind remotely without GIT_LFS_DELTA_ALLOW_REMOTE_DEVELOPMENT_AUTH".into());
    }

    let pool = PgPoolOptions::new()
        .max_connections(u32::try_from(maximum_connections)?)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await?;
    migrate(&pool).await?;
    let (store, prefix) = object_store::parse_url_opts(&storage_url, std::env::vars())?;
    let store = PrefixStore::new(store, prefix);
    let chunks = ChunkStore::new(Arc::new(store));
    let state = match auth_mode.as_str() {
        "forgejo" => AppState::new_forgejo(
            pool,
            chunks,
            base_url,
            Url::parse(&required("GIT_LFS_DELTA_FORGEJO_URL")?)?,
        )?,
        "oidc" => {
            tracing::warn!("OIDC authentication remains preview in beta.2");
            AppState::new_oidc(
                pool,
                chunks,
                base_url,
                Url::parse(&required("GIT_LFS_DELTA_OIDC_ISSUER")?)?,
                &required("GIT_LFS_DELTA_OIDC_AUDIENCE")?,
            )
            .await?
        }
        "development" => AppState::new(
            pool,
            chunks,
            base_url,
            &required("GIT_LFS_DELTA_DEV_TOKEN")?,
        ),
        other => return Err(format!("unsupported GIT_LFS_DELTA_AUTH_MODE: {other}").into()),
    }
    .with_staging(staging_root, maximum_basic_transfers)
    .with_data_limit(maximum_data_requests);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(address = %bind, "Git LFS Delta server listening");
    let (shutdown_sender, _) = tokio::sync::watch::channel(false);
    let signal_sender = shutdown_sender.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = signal_sender.send(true);
    });
    let mut graceful = shutdown_sender.subscribe();
    let mut deadline = shutdown_sender.subscribe();
    let server = axum::serve(listener, build_router(state)).with_graceful_shutdown(async move {
        if !*graceful.borrow() {
            let _ = graceful.changed().await;
        }
    });
    tokio::select! {
        result = server => result?,
        () = async move {
            if !*deadline.borrow() {
                let _ = deadline.changed().await;
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        } => tracing::warn!("shutdown grace expired; terminating remaining requests"),
    }
    Ok(())
}

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("required environment variable {name} is missing or empty").into()),
    }
}

fn optional_usize(name: &str, default: usize) -> Result<usize, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .map_err(|error| format!("invalid {name}: {error}").into()),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            tracing::error!(%error, "could not install shutdown signal handler");
                        }
                    }
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => tracing::error!(%error, "could not install SIGTERM handler"),
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "could not install shutdown signal handler");
    }
    tracing::info!("shutdown requested");
}
