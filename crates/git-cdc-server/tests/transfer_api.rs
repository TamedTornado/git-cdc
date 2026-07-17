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
use futures_util::TryStreamExt;
use git_cdc_core::{ChunkStream, ChunkingProfile};
use git_cdc_protocol::{BeginUploadRequest, BeginUploadResponse};
use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use http_body_util::BodyExt;
use object_store::{ObjectStore, memory::InMemory};
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

async fn setup() -> (PgPool, axum::Router) {
    let (pool, app, _) = setup_with_store().await;
    (pool, app)
}

async fn setup_with_store() -> (PgPool, axum::Router, Arc<InMemory>) {
    let database_url = std::env::var("GIT_CDC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc".into());
    let pool = PgPool::connect(&database_url).await.unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query("DROP TRIGGER IF EXISTS git_cdc_test_fail_publish ON object_chunks")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION IF EXISTS git_cdc_test_fail_publish()")
        .execute(&pool)
        .await
        .unwrap();
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
    let provider = Arc::new(InMemory::new());
    let state = AppState::new(
        pool.clone(),
        ChunkStore::new(provider.clone()),
        Url::parse("http://cdc.example/").unwrap(),
        "integration-secret",
    );
    (pool, build_router(state), provider)
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
async fn rejected_basic_uploads_do_not_leave_physical_chunks() {
    let (_pool, app, provider) = setup_with_store().await;
    let response = app
        .oneshot(authenticated(
            "PUT",
            "/team/assets/info/lfs/objects/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            Body::from(vec![0x19_u8; 600_000]),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let stored: Vec<_> = provider.list(None).try_collect().await.unwrap();
    assert!(stored.is_empty());
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
    let uri = format!("/team/assets/info/lfs/objects/{oid}/cdc");
    let first_request = app
        .clone()
        .oneshot(authenticated("POST", &uri, Body::from(body.clone())));
    let second_request = app
        .clone()
        .oneshot(authenticated("POST", &uri, Body::from(body.clone())));
    let (first_response, second_response) = tokio::join!(first_request, second_request);
    let first: BeginUploadResponse = serde_json::from_slice(
        &first_response
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    let concurrent: BeginUploadResponse = serde_json::from_slice(
        &second_response
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(first.upload_id, concurrent.upload_id);

    for _ in 0..2 {
        let response = app
            .clone()
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
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let resumed: BeginUploadResponse = serde_json::from_slice(
        &app.clone()
            .oneshot(authenticated("POST", &uri, Body::from(body)))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();

    assert_eq!(first.upload_id, resumed.upload_id);
    assert!(!resumed.missing_chunk_indexes.contains(&0));
    assert_eq!(resumed.missing_chunk_indexes.len(), chunks.len() - 1);

    for index in (1..chunks.len()).rev() {
        let response = app
            .clone()
            .oneshot(authenticated(
                "PUT",
                &format!(
                    "/team/assets/info/lfs/objects/{oid}/cdc/{}/chunks/{index}",
                    first.upload_id
                ),
                Body::from(chunks[index].data.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(
        app.oneshot(authenticated(
            "POST",
            &format!(
                "/team/assets/info/lfs/objects/{oid}/cdc/{}/finalize",
                first.upload_id
            ),
            Body::empty(),
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
#[serial_test::serial]
async fn an_expired_upload_is_quarantined_before_a_new_session_is_created() {
    let (pool, app) = setup().await;
    let manifest = ChunkStream::new(
        Cursor::new(vec![0x71_u8; 600_000]),
        ChunkingProfile::beta_v1(),
    )
    .finish()
    .unwrap();
    let oid = manifest.object_oid;
    let request = BeginUploadRequest {
        protocol_version: 1,
        manifest,
    };
    let body = serde_json::to_vec(&request).unwrap();
    let begin = |app: axum::Router, body: Vec<u8>| async move {
        let response = app
            .oneshot(authenticated(
                "POST",
                &format!("/team/assets/info/lfs/objects/{oid}/cdc"),
                Body::from(body),
            ))
            .await
            .unwrap();
        serde_json::from_slice::<BeginUploadResponse>(
            &response.into_body().collect().await.unwrap().to_bytes(),
        )
        .unwrap()
    };
    let first = begin(app.clone(), body.clone()).await;
    sqlx::query("UPDATE upload_sessions SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();

    let second = begin(app, body).await;

    assert_ne!(first.upload_id, second.upload_id);
    let state: String = sqlx::query_scalar("SELECT state FROM upload_sessions WHERE id = $1")
        .bind(first.upload_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "expired");
}

#[tokio::test]
#[serial_test::serial]
async fn database_failure_rolls_back_logical_object_publication() {
    let (pool, app) = setup().await;
    let mut stream = ChunkStream::new(
        Cursor::new(vec![0x52_u8; 600_000]),
        ChunkingProfile::beta_v1(),
    );
    let chunks: Vec<_> = stream.by_ref().map(|chunk| chunk.unwrap()).collect();
    let manifest = stream.finish().unwrap();
    let oid = manifest.object_oid;
    let response = app
        .clone()
        .oneshot(authenticated(
            "POST",
            &format!("/team/assets/info/lfs/objects/{oid}/cdc"),
            Body::from(
                serde_json::to_vec(&BeginUploadRequest {
                    protocol_version: 1,
                    manifest,
                })
                .unwrap(),
            ),
        ))
        .await
        .unwrap();
    let plan: BeginUploadResponse =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(
            app.clone()
                .oneshot(authenticated(
                    "PUT",
                    &format!(
                        "/team/assets/info/lfs/objects/{oid}/cdc/{}/chunks/{index}",
                        plan.upload_id
                    ),
                    Body::from(chunk.data.clone()),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    sqlx::query(
        "CREATE FUNCTION git_cdc_test_fail_publish() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected publish failure'; END $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER git_cdc_test_fail_publish BEFORE INSERT ON object_chunks \
         FOR EACH ROW EXECUTE FUNCTION git_cdc_test_fail_publish()",
    )
    .execute(&pool)
    .await
    .unwrap();

    let response = app
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

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let objects: i64 = sqlx::query_scalar("SELECT count(*) FROM objects")
        .fetch_one(&pool)
        .await
        .unwrap();
    let references: i64 =
        sqlx::query_scalar("SELECT count(*) FROM chunks WHERE reference_count <> 0")
            .fetch_one(&pool)
            .await
            .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM upload_sessions WHERE id = $1")
        .bind(plan.upload_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(objects, 0);
    assert_eq!(references, 0);
    assert_eq!(state, "open");
}
