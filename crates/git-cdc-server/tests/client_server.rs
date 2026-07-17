//! Real HTTP client-to-server integration against `PostgreSQL`.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::{collections::BTreeMap, fs, io::Cursor, sync::Arc};

use git_cdc::{DownloadRequest, HttpBackend, TransferAction, TransferBackend, UploadRequest};
use git_cdc_core::{ChunkStream, ChunkingProfile};
use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use object_store::memory::InMemory;
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

#[tokio::test]
#[serial_test::serial]
async fn production_client_uploads_and_downloads_over_real_http() {
    let database_url = std::env::var("GIT_CDC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc".into());
    let pool = PgPool::connect(&database_url).await.unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query("TRUNCATE repositories CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories (id, owner, name) VALUES ($1, 'team', 'assets')")
        .bind(Uuid::nil())
        .execute(&pool)
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base_url = Url::parse(&format!("http://{address}/")).unwrap();
    let state = AppState::new(
        pool,
        ChunkStore::new(Arc::new(InMemory::new())),
        base_url.clone(),
        "integration-secret",
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });

    let source: Vec<u8> = (0_usize..11 * 1024 * 1024 + 91)
        .map(|index| index.wrapping_mul(17).to_le_bytes()[0])
        .collect();
    let manifest = ChunkStream::new(Cursor::new(&source), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    let source_file = tempfile::NamedTempFile::new().unwrap();
    fs::write(source_file.path(), &source).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut header = BTreeMap::new();
    header.insert("Authorization".into(), "Bearer integration-secret".into());
    let action = TransferAction {
        href: base_url
            .join(&format!(
                "team/assets/info/lfs/objects/{}/cdc",
                manifest.object_oid
            ))
            .unwrap()
            .to_string(),
        header,
    };
    let upload = UploadRequest {
        oid: manifest.object_oid,
        size: manifest.object_size,
        path: source_file.path().to_path_buf(),
        action: Some(action.clone()),
    };
    let download = DownloadRequest {
        oid: manifest.object_oid,
        size: manifest.object_size,
        action: Some(action),
    };
    let cache_path = cache.path().to_path_buf();
    let downloaded = tokio::task::spawn_blocking(move || {
        let backend = HttpBackend::new(cache_path).unwrap();
        assert_eq!(backend.upload(&upload).unwrap(), upload.size);
        backend.download(&download).unwrap()
    })
    .await
    .unwrap();

    assert_eq!(fs::read(downloaded.path).unwrap(), source);
    server.abort();
}
