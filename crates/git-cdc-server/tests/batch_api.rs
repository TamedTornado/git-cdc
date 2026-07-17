//! Real-Postgres tests for the Git LFS Batch API.
#![allow(
    clippy::unwrap_used,
    reason = "integration environment and literal response fixtures must fail immediately"
)]

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
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
        ChunkStore::new(std::sync::Arc::new(InMemory::new())),
        Url::parse("https://cdc.example/").unwrap(),
        "integration-secret",
    );
    (pool, build_router(state))
}

fn batch_request(transfers: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/team/assets/info/lfs/objects/batch")
        .header(header::AUTHORIZATION, "Bearer integration-secret")
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(Body::from(format!(
            r#"{{"operation":"upload","transfers":{transfers},"objects":[{{"oid":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","size":0}}]}}"#
        )))
        .unwrap()
}

#[tokio::test]
#[serial_test::serial]
async fn stock_client_receives_a_basic_upload_action() {
    let (_pool, app) = setup().await;
    let response = app.oneshot(batch_request(r#"["basic"]"#)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(json["transfer"], "basic");
    assert_eq!(
        json["objects"][0]["actions"]["upload"]["href"],
        "https://cdc.example/team/assets/info/lfs/objects/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn aware_client_receives_the_cdc_action() {
    let (_pool, app) = setup().await;
    let response = app
        .oneshot(batch_request(r#"["basic","cdc"]"#))
        .await
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();

    assert_eq!(json["transfer"], "cdc");
    assert!(
        json["objects"][0]["actions"]["upload"]["href"]
            .as_str()
            .unwrap()
            .ends_with("/cdc")
    );
}

#[tokio::test]
#[serial_test::serial]
async fn missing_credentials_are_rejected_before_repository_disclosure() {
    let (_pool, app) = setup().await;
    let mut request = batch_request(r#"["basic"]"#);
    request.headers_mut().remove(header::AUTHORIZATION);

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial_test::serial]
async fn readiness_checks_postgres_and_metrics_are_prometheus_text() {
    let (_pool, app) = setup().await;
    let ready = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::NO_CONTENT);
    let metrics = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8(
        metrics
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("git_cdc_logical_upload_bytes_total 0"));
    assert!(body.contains("git_cdc_received_chunk_bytes_total 0"));
}
