//! Git-CDC server entrypoint.

use std::{net::SocketAddr, sync::Arc};

use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use object_store::prefix::PrefixStore;
use sqlx::postgres::PgPoolOptions;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "git_cdc_server=info,tower_http=info".into()),
        )
        .json()
        .init();

    let database_url = required("GIT_CDC_DATABASE_URL")?;
    let base_url = Url::parse(&required("GIT_CDC_BASE_URL")?)?;
    let storage_url = Url::parse(&required("GIT_CDC_STORAGE_URL")?)?;
    let bind: SocketAddr = std::env::var("GIT_CDC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await?;
    migrate(&pool).await?;
    let (store, prefix) = object_store::parse_url_opts(&storage_url, std::env::vars())?;
    let store = PrefixStore::new(store, prefix);
    let chunks = ChunkStore::new(Arc::new(store));
    let auth_mode = match std::env::var("GIT_CDC_AUTH_MODE") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "development".into(),
        Err(error) => return Err(error.into()),
    };
    let state = match auth_mode.as_str() {
        "forgejo" => AppState::new_forgejo(
            pool,
            chunks,
            base_url,
            Url::parse(&required("GIT_CDC_FORGEJO_URL")?)?,
        )?,
        "oidc" => {
            AppState::new_oidc(
                pool,
                chunks,
                base_url,
                Url::parse(&required("GIT_CDC_OIDC_ISSUER")?)?,
                &required("GIT_CDC_OIDC_AUDIENCE")?,
            )
            .await?
        }
        "development" => AppState::new(pool, chunks, base_url, &required("GIT_CDC_DEV_TOKEN")?),
        other => return Err(format!("unsupported GIT_CDC_AUTH_MODE: {other}").into()),
    };
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(address = %bind, "Git-CDC server listening");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!("required environment variable {name} is missing or empty").into()),
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "could not install shutdown signal handler");
    }
}
