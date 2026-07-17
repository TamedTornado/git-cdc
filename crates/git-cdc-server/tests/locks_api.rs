//! Git LFS locking contracts against real `PostgreSQL`.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use git_cdc_protocol::{LockList, LockResponse, LockVerifyResponse, UnlockResponse};
use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use http_body_util::BodyExt;
use object_store::memory::InMemory;
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

async fn setup() -> axum::Router {
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
    build_router(AppState::new(
        pool,
        ChunkStore::new(Arc::new(InMemory::new())),
        Url::parse("http://cdc.example/").unwrap(),
        "integration-secret",
    ))
}

fn request(method: &str, uri: &str, json: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer integration-secret")
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(Body::from(json.to_owned()))
        .unwrap()
}

#[tokio::test]
#[serial_test::serial]
async fn lock_list_conflict_and_unlock_follow_lfs_contract() {
    let app = setup().await;
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/team/assets/info/lfs/locks",
            r#"{"path":"art/hero.glb"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let lock: LockResponse =
        serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let lock = lock.lock;
    assert_eq!(lock.path, "art/hero.glb");

    let conflict = app
        .clone()
        .oneshot(request(
            "POST",
            "/team/assets/info/lfs/locks",
            r#"{"path":"art/hero.glb"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let listed = app
        .clone()
        .oneshot(request("GET", "/team/assets/info/lfs/locks", ""))
        .await
        .unwrap();
    let locks: LockList =
        serde_json::from_slice(&listed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(locks.locks, vec![lock.clone()]);

    let verified = app
        .clone()
        .oneshot(request("POST", "/team/assets/info/lfs/locks/verify", "{}"))
        .await
        .unwrap();
    assert_eq!(verified.status(), StatusCode::OK);
    let verified: LockVerifyResponse =
        serde_json::from_slice(&verified.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(verified.ours, vec![lock.clone()]);
    assert!(verified.theirs.is_empty());

    let unlocked = app
        .oneshot(request(
            "POST",
            &format!("/team/assets/info/lfs/locks/{}/unlock", lock.id),
            r#"{"force":false}"#,
        ))
        .await
        .unwrap();
    assert_eq!(unlocked.status(), StatusCode::OK);
    let body: UnlockResponse =
        serde_json::from_slice(&unlocked.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body.lock, lock);
}

#[tokio::test]
#[serial_test::serial]
async fn lock_listing_and_verification_paginate_with_stable_cursors() {
    let app = setup().await;
    for path in ["art/a.glb", "art/b.glb", "art/c.glb"] {
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/team/assets/info/lfs/locks",
                    &format!(r#"{{"path":"{path}"}}"#),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
    }

    let first: LockList = serde_json::from_slice(
        &app.clone()
            .oneshot(request("GET", "/team/assets/info/lfs/locks?limit=2", ""))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(first.locks.len(), 2);
    let cursor = first.next_cursor.unwrap();
    let second: LockList = serde_json::from_slice(
        &app.clone()
            .oneshot(request(
                "GET",
                &format!("/team/assets/info/lfs/locks?limit=2&cursor={cursor}"),
                "",
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
    assert_eq!(second.locks.len(), 1);
    assert!(second.next_cursor.is_none());

    let filtered: LockList = serde_json::from_slice(
        &app.clone()
            .oneshot(request(
                "GET",
                "/team/assets/info/lfs/locks?path=art%2Fb.glb",
                "",
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
    assert_eq!(filtered.locks.len(), 1);
    assert_eq!(filtered.locks[0].path, "art/b.glb");

    let verified: LockVerifyResponse = serde_json::from_slice(
        &app.oneshot(request(
            "POST",
            "/team/assets/info/lfs/locks/verify",
            r#"{"limit":1}"#,
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
    assert_eq!(verified.ours.len(), 1);
    assert!(verified.theirs.is_empty());
    assert!(verified.next_cursor.is_some());
}
