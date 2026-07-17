//! Generic OIDC authorization contracts using signed JWTs and repository grants.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode, header},
    routing::get,
};
use git_cdc_server::{AppState, build_router, migrate};
use git_cdc_storage::ChunkStore;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use object_store::memory::InMemory;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    iss: &'a str,
    aud: &'a str,
    exp: u64,
}

fn signed_token(sub: &str, issuer: &str, audience: &str, exp: u64, secret: &[u8]) -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("test".into());
    encode(
        &header,
        &Claims {
            sub,
            iss: issuer,
            aud: audience,
            exp,
        },
        &EncodingKey::from_secret(secret),
    )
    .unwrap()
}

fn batch(token: &str, operation: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/team/assets/info/lfs/objects/batch")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(Body::from(format!(
            r#"{{"operation":"{operation}","objects":[]}}"#
        )))
        .unwrap()
}

#[tokio::test]
#[serial_test::serial]
async fn oidc_validates_signature_issuer_audience_and_database_grant() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let discovery_issuer = issuer.clone();
    let jwks_uri = format!("{issuer}/keys");
    let provider = Router::new()
        .route("/.well-known/openid-configuration", get(move || {
            let issuer = discovery_issuer.clone();
            let jwks_uri = jwks_uri.clone();
            async move { Json(json!({"issuer":issuer,"jwks_uri":jwks_uri})) }
        }))
        .route("/keys", get(|| async {
            Json(json!({"keys":[{"kty":"oct","k":"c3VwZXItc2VjcmV0","alg":"HS256","kid":"test"}]}))
        }));
    let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });

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
    sqlx::query("INSERT INTO repository_grants (repository_id, subject, can_read, can_write) VALUES ($1, 'alice', true, true)")
        .bind(Uuid::nil()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO repository_grants (repository_id, subject, can_read, can_write) VALUES ($1, 'bob', true, false)")
        .bind(Uuid::nil()).execute(&pool).await.unwrap();
    let state = AppState::new_oidc(
        pool,
        ChunkStore::new(Arc::new(InMemory::new())),
        Url::parse("http://git-cdc.example/").unwrap(),
        Url::parse(&issuer).unwrap(),
        "git-cdc",
    )
    .await
    .unwrap();
    let app = build_router(state);
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    let valid = signed_token("alice", &issuer, "git-cdc", exp, b"super-secret");
    assert_eq!(
        app.clone()
            .oneshot(batch(&valid, "upload"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let reader = signed_token("bob", &issuer, "git-cdc", exp, b"super-secret");
    assert_eq!(
        app.clone()
            .oneshot(batch(&reader, "download"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(batch(&reader, "upload"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for invalid in [
        signed_token("alice", &issuer, "git-cdc", exp - 600, b"super-secret"),
        signed_token("alice", &issuer, "wrong-audience", exp, b"super-secret"),
        signed_token(
            "alice",
            "https://wrong-issuer.invalid",
            "git-cdc",
            exp,
            b"super-secret",
        ),
        signed_token("alice", &issuer, "git-cdc", exp, b"wrong-secret"),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(batch(&invalid, "download"))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    task.abort();
}
