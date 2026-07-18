//! Forgejo authorization contracts using a real loopback HTTP adapter.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
    routing::get,
};
use git_cdc_protocol::LockResponse;
use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use http_body_util::BodyExt;
use object_store::memory::InMemory;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

async fn database() -> PgPool {
    let url = std::env::var("GIT_CDC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc".into());
    let pool = PgPool::connect(&url).await.unwrap();
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
    pool
}

fn batch() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/team/assets/info/lfs/objects/batch")
        .header(header::AUTHORIZATION, "Bearer forgejo-token")
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(Body::from(r#"{"operation":"upload","objects":[]}"#))
        .unwrap()
}

#[tokio::test]
#[serial_test::serial]
async fn forgejo_checks_repository_permission_on_every_request_and_observes_revocation() {
    let valid = Arc::new(AtomicBool::new(true));
    let user_valid = valid.clone();
    let repo_valid = valid.clone();
    let forgejo = Router::new()
        .route(
            "/api/v1/user",
            get(move || {
                let valid = user_valid.clone();
                async move {
                    if valid.load(Ordering::SeqCst) {
                        (StatusCode::OK, Json(json!({"login":"alice"})))
                    } else {
                        (StatusCode::UNAUTHORIZED, Json(json!({"message":"revoked"})))
                    }
                }
            }),
        )
        .route(
            "/api/v1/repos/{owner}/{repo}",
            get(move || {
                let valid = repo_valid.clone();
                async move {
                    if valid.load(Ordering::SeqCst) {
                        (
                            StatusCode::OK,
                            Json(json!({"permissions":{"pull":true,"push":true,"admin":false}})),
                        )
                    } else {
                        (StatusCode::UNAUTHORIZED, Json(json!({"message":"revoked"})))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let forgejo_url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, forgejo).await.unwrap() });
    let state = AppState::new_forgejo(
        database().await,
        ChunkStore::new(Arc::new(InMemory::new())),
        Url::parse("http://git-cdc.example/").unwrap(),
        forgejo_url,
    )
    .unwrap()
    .with_forgejo_cache(Duration::from_millis(250), 100);
    let app = build_router(state);

    assert_eq!(
        app.clone().oneshot(batch()).await.unwrap().status(),
        StatusCode::OK
    );
    valid.store(false, Ordering::SeqCst);
    assert_eq!(
        app.clone().oneshot(batch()).await.unwrap().status(),
        StatusCode::OK,
        "a successful decision remains valid for the bounded cache TTL"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        app.oneshot(batch()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

fn forgejo_request(token: &str, method: &str, uri: &str, body: &'static str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
#[serial_test::serial]
async fn forgejo_enforces_read_write_lock_ownership_and_administrative_force() {
    let forgejo = Router::new()
        .route(
            "/api/v1/user",
            get(|headers: HeaderMap| async move {
                let token = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap();
                let login = token.strip_prefix("Bearer ").unwrap();
                (StatusCode::OK, Json(json!({"login":login})))
            }),
        )
        .route(
            "/api/v1/repos/{owner}/{repo}",
            get(|headers: HeaderMap| async move {
                let token = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap();
                let reader = token == "Bearer reader";
                let admin = token == "Bearer admin";
                (
                    StatusCode::OK,
                    Json(json!({"permissions":{"pull":true,"push":!reader,"admin":admin}})),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let forgejo_url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, forgejo).await.unwrap() });
    let app = build_router(
        AppState::new_forgejo(
            database().await,
            ChunkStore::new(Arc::new(InMemory::new())),
            Url::parse("http://git-cdc.example/").unwrap(),
            forgejo_url,
        )
        .unwrap(),
    );

    assert_eq!(
        app.clone()
            .oneshot(forgejo_request(
                "reader",
                "POST",
                "/team/assets/info/lfs/objects/batch",
                r#"{"operation":"download","objects":[]}"#,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(forgejo_request(
                "reader",
                "POST",
                "/team/assets/info/lfs/objects/batch",
                r#"{"operation":"upload","objects":[]}"#,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let created = app
        .clone()
        .oneshot(forgejo_request(
            "alice",
            "POST",
            "/team/assets/info/lfs/locks",
            r#"{"path":"art/hero.glb"}"#,
        ))
        .await
        .unwrap();
    let lock: LockResponse =
        serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let unlock_uri = format!("/team/assets/info/lfs/locks/{}/unlock", lock.lock.id);
    for body in [r#"{"force":false}"#, r#"{"force":true}"#] {
        assert_eq!(
            app.clone()
                .oneshot(forgejo_request("bob", "POST", &unlock_uri, body))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        app.oneshot(forgejo_request(
            "admin",
            "POST",
            &unlock_uri,
            r#"{"force":true}"#,
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    task.abort();
}
