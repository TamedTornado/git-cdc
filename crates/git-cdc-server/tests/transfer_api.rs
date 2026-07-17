//! End-to-end HTTP transfer contracts against real `PostgreSQL`.
#![allow(
    clippy::unwrap_used,
    reason = "integration environment and literal response fixtures must fail immediately"
)]

use std::{io::Cursor, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use bytes::Bytes;
use git_cdc_core::{ChunkStream, ChunkingProfile};
use git_cdc_protocol::{BeginUploadRequest, BeginUploadResponse};
use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use http_body_util::BodyExt;
use object_store::memory::InMemory;
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

async fn setup() -> (PgPool, axum::Router) {
    let database_url = std::env::var("GIT_CDC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc".into());
    let pool = PgPool::connect(&database_url).await.unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query("TRUNCATE repositories CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories (id, owner, name) VALUES ($1, $2, $3)")
        .bind(Uuid::nil())
        .bind("team")
        .bind("assets")
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::new(
        pool.clone(),
        ChunkStore::new(Arc::new(InMemory::new())),
        Url::parse("http://cdc.example/").unwrap(),
        "integration-secret",
    );
    (pool, build_router(state))
}

fn authenticated(method: &str, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer integration-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap()
}

#[tokio::test]
#[serial_test::serial]
async fn cdc_upload_resumes_finalizes_and_downloads_through_basic_lfs() {
    let (_pool, app) = setup().await;
    let source = vec![0x5a_u8; 2 * 1024 * 1024 + 17];
    let mut chunks = ChunkStream::new(Cursor::new(source.clone()), ChunkingProfile::beta_v1());
    let chunk_data: Vec<_> = chunks.by_ref().map(|chunk| chunk.unwrap()).collect();
    let manifest = chunks.finish().unwrap();
    let oid = manifest.object_oid;
    let begin = BeginUploadRequest {
        protocol_version: 1,
        manifest: manifest.clone(),
    };

    let response = app
        .clone()
        .oneshot(authenticated(
            "POST",
            &format!("/team/assets/info/lfs/objects/{oid}/cdc"),
            Body::from(serde_json::to_vec(&begin).unwrap()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let plan: BeginUploadResponse =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(plan.missing_chunk_indexes.len(), chunk_data.len());

    for (index, chunk) in chunk_data.iter().enumerate() {
        let response = app
            .clone()
            .oneshot(authenticated(
                "PUT",
                &format!(
                    "/team/assets/info/lfs/objects/{oid}/cdc/{}/chunks/{index}",
                    plan.upload_id
                ),
                Body::from(chunk.data.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let response = app
        .clone()
        .oneshot(authenticated(
            "POST",
            &format!(
                "/team/assets/info/lfs/objects/{oid}/cdc/{}/finalize",
                plan.upload_id
            ),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(authenticated(
            "POST",
            &format!(
                "/team/assets/info/lfs/objects/{oid}/cdc/{}/finalize",
                plan.upload_id
            ),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "retrying a successful finalize must be idempotent"
    );

    let response = app
        .oneshot(authenticated(
            "GET",
            &format!("/team/assets/info/lfs/objects/{oid}"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from(source)
    );
}

#[tokio::test]
#[serial_test::serial]
async fn cdc_rejects_corrupt_chunk_bytes_as_a_client_integrity_error() {
    let (_pool, app) = setup().await;
    let mut stream = ChunkStream::new(
        Cursor::new(vec![0x3c_u8; 600_000]),
        ChunkingProfile::beta_v1(),
    );
    let chunks: Vec<_> = stream.by_ref().map(|chunk| chunk.unwrap()).collect();
    let manifest = stream.finish().unwrap();
    let oid = manifest.object_oid;
    let request = BeginUploadRequest {
        protocol_version: 1,
        manifest,
    };
    let plan: BeginUploadResponse = serde_json::from_slice(
        &app.clone()
            .oneshot(authenticated(
                "POST",
                &format!("/team/assets/info/lfs/objects/{oid}/cdc"),
                Body::from(serde_json::to_vec(&request).unwrap()),
            ))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    let corrupt = Bytes::from(vec![0x4d_u8; chunks[0].data.len()]);

    let response = app
        .oneshot(authenticated(
            "PUT",
            &format!(
                "/team/assets/info/lfs/objects/{oid}/cdc/{}/chunks/0",
                plan.upload_id
            ),
            Body::from(corrupt),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[serial_test::serial]
async fn cdc_rejects_a_manifest_whose_oid_does_not_match_the_route() {
    let (_pool, app) = setup().await;
    let manifest = ChunkStream::new(Cursor::new(vec![1_u8; 600_000]), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    let request = BeginUploadRequest {
        protocol_version: 1,
        manifest,
    };

    let response = app
        .oneshot(authenticated(
            "POST",
            "/team/assets/info/lfs/objects/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855/cdc",
            Body::from(serde_json::to_vec(&request).unwrap()),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[serial_test::serial]
async fn stock_lfs_basic_upload_streams_and_round_trips() {
    let (_pool, app) = setup().await;
    let source = vec![0xa7_u8; 9 * 1024 * 1024 + 31];
    let manifest = ChunkStream::new(Cursor::new(&source), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    let oid = manifest.object_oid;

    let response = app
        .clone()
        .oneshot(authenticated(
            "PUT",
            &format!("/team/assets/info/lfs/objects/{oid}"),
            Body::from(source.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .oneshot(authenticated(
            "GET",
            &format!("/team/assets/info/lfs/objects/{oid}"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        source
    );
}

#[tokio::test]
#[serial_test::serial]
async fn resumed_cdc_session_only_requests_still_missing_chunks() {
    let (_pool, app) = setup().await;
    let source: Vec<u8> = (0_usize..10 * 1024 * 1024)
        .map(|index| (index % 251).to_le_bytes()[0])
        .collect();
    let mut stream = ChunkStream::new(Cursor::new(source), ChunkingProfile::beta_v1());
    let chunks: Vec<_> = stream.by_ref().map(|chunk| chunk.unwrap()).collect();
    let manifest = stream.finish().unwrap();
    assert!(chunks.len() > 1);
    let oid = manifest.object_oid;
    let request = BeginUploadRequest {
        protocol_version: 1,
        manifest,
    };
    let body = serde_json::to_vec(&request).unwrap();
    let first: BeginUploadResponse = serde_json::from_slice(
        &app.clone()
            .oneshot(authenticated(
                "POST",
                &format!("/team/assets/info/lfs/objects/{oid}/cdc"),
                Body::from(body.clone()),
            ))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    app.clone()
        .oneshot(authenticated(
            "PUT",
            &format!(
                "/team/assets/info/lfs/objects/{oid}/cdc/{}/chunks/0",
                first.upload_id
            ),
            Body::from(chunks[0].data.clone()),
        ))
        .await
        .unwrap();

    let second: BeginUploadResponse = serde_json::from_slice(
        &app.oneshot(authenticated(
            "POST",
            &format!("/team/assets/info/lfs/objects/{oid}/cdc"),
            Body::from(body),
        ))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes(),
    )
    .unwrap();

    assert_eq!(first.upload_id, second.upload_id);
    assert!(!second.missing_chunk_indexes.contains(&0));
    assert_eq!(second.missing_chunk_indexes.len(), chunks.len() - 1);
}
