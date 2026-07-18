//! Forge-neutral Git LFS and CDC service.

pub mod gc;
pub mod reconcile;

use std::{
    collections::{BTreeMap, HashMap},
    io::SeekFrom,
    path::{Path as FilePath, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use git_cdc_core::{ChunkDescriptor, ChunkStream, ObjectManifest, ObjectOid};
use git_cdc_protocol::{
    BatchObjectError, BatchObjectResponse, BatchRequest, BatchResponse, BeginUploadRequest,
    BeginUploadResponse, LfsAction, Lock, LockList, LockOwner, LockRequest, LockResponse,
    LockVerifyResponse, Operation, TransferKind, UnlockRequest, UnlockResponse, select_transfer,
};
use git_cdc_storage::ChunkStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, Semaphore},
};
use tokio_util::io::{ReaderStream, StreamReader};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use url::Url;
use uuid::Uuid;

const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

/// Shared dependencies for the Git-CDC HTTP service.
#[derive(Clone)]
pub struct AppState {
    pool: PgPool,
    chunks: ChunkStore,
    base_url: Url,
    authentication: Authentication,
    metrics: Arc<ServiceMetrics>,
    staging_root: PathBuf,
    basic_transfers: Arc<Semaphore>,
    data_requests: Arc<Semaphore>,
    storage_ready_until: Arc<Mutex<Option<Instant>>>,
}

#[derive(Default)]
struct ServiceMetrics {
    logical_uploaded: AtomicU64,
    logical_downloaded: AtomicU64,
    chunks_received: AtomicU64,
    auth_cache_hits: AtomicU64,
    auth_cache_misses: AtomicU64,
    forgejo_requests: AtomicU64,
    http_requests: AtomicU64,
    http_failures: AtomicU64,
    in_flight: AtomicU64,
}

#[derive(Clone)]
enum Authentication {
    DevelopmentToken {
        hash: [u8; 32],
    },
    Forgejo {
        base_url: Url,
        client: reqwest::Client,
        cache: ForgejoAuthorizationCache,
    },
    Oidc {
        issuer: String,
        audience: String,
        keys: HashMap<String, jsonwebtoken::jwk::Jwk>,
    },
}

#[derive(Clone)]
struct ForgejoAuthorizationCache {
    entries: Arc<Mutex<HashMap<[u8; 32], CachedForgejoIdentity>>>,
    ttl: Duration,
    capacity: usize,
}

#[derive(Clone)]
struct CachedForgejoIdentity {
    subject: String,
    administrator: bool,
    expires_at: Instant,
}

#[derive(Clone, Copy)]
enum Access {
    Read,
    Write,
}

struct Identity {
    authorization: String,
    subject: String,
    administrator: bool,
}

#[derive(Default, Deserialize)]
struct LockListQuery {
    path: Option<String>,
    id: Option<Uuid>,
    cursor: Option<Uuid>,
    limit: Option<usize>,
}

#[derive(Default, Deserialize)]
struct LockVerifyRequest {
    cursor: Option<Uuid>,
    limit: Option<usize>,
}

impl AppState {
    /// Creates service state with explicit development-token authentication.
    ///
    /// Production deployments will construct state through a Forgejo or OIDC
    /// authorizer; this constructor deliberately requires a non-empty token.
    #[must_use]
    pub fn new(pool: PgPool, chunks: ChunkStore, base_url: Url, dev_token: &str) -> Self {
        Self {
            pool,
            chunks,
            base_url,
            authentication: Authentication::DevelopmentToken {
                hash: Sha256::digest(dev_token.as_bytes()).into(),
            },
            metrics: Arc::default(),
            staging_root: std::env::temp_dir(),
            basic_transfers: Arc::new(Semaphore::new(2)),
            data_requests: Arc::new(Semaphore::new(64)),
            storage_ready_until: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates service state that delegates identity and repository access to Forgejo.
    ///
    /// Tokens are checked with Forgejo on every request so revocation takes
    /// effect immediately and administrator credentials never reach clients.
    ///
    /// # Errors
    ///
    /// Returns an error if a hardened HTTP client cannot be constructed.
    pub fn new_forgejo(
        pool: PgPool,
        chunks: ChunkStore,
        base_url: Url,
        forgejo_url: Url,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            pool,
            chunks,
            base_url,
            authentication: Authentication::Forgejo {
                base_url: forgejo_url,
                client: reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(5))
                    .timeout(Duration::from_secs(10))
                    .build()?,
                cache: ForgejoAuthorizationCache {
                    entries: Arc::new(Mutex::new(HashMap::new())),
                    ttl: Duration::from_secs(30),
                    capacity: 10_000,
                },
            },
            metrics: Arc::default(),
            staging_root: std::env::temp_dir(),
            basic_transfers: Arc::new(Semaphore::new(2)),
            data_requests: Arc::new(Semaphore::new(64)),
            storage_ready_until: Arc::new(Mutex::new(None)),
        })
    }

    /// Creates service state using OIDC signature validation and database grants.
    ///
    /// # Errors
    ///
    /// Returns [`OidcSetupError`] if discovery or the initial JWKS load fails.
    pub async fn new_oidc(
        pool: PgPool,
        chunks: ChunkStore,
        base_url: Url,
        issuer_url: Url,
        audience: &str,
    ) -> Result<Self, OidcSetupError> {
        let client = reqwest::Client::builder().build()?;
        let discovery_url = issuer_url.join(".well-known/openid-configuration")?;
        let discovery: OidcDiscovery = client
            .get(discovery_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if discovery.issuer.trim_end_matches('/') != issuer_url.as_str().trim_end_matches('/') {
            return Err(OidcSetupError::IssuerMismatch);
        }
        let set: jsonwebtoken::jwk::JwkSet = client
            .get(discovery.jwks_uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let keys = set
            .keys
            .into_iter()
            .filter_map(|key| key.common.key_id.clone().map(|id| (id, key)))
            .collect();
        Ok(Self {
            pool,
            chunks,
            base_url,
            authentication: Authentication::Oidc {
                issuer: discovery.issuer,
                audience: audience.into(),
                keys,
            },
            metrics: Arc::default(),
            staging_root: std::env::temp_dir(),
            basic_transfers: Arc::new(Semaphore::new(2)),
            data_requests: Arc::new(Semaphore::new(64)),
            storage_ready_until: Arc::new(Mutex::new(None)),
        })
    }

    /// Configures the bounded staging directory used by stock basic transfers.
    #[must_use]
    pub fn with_staging(mut self, root: impl AsRef<FilePath>, maximum_transfers: usize) -> Self {
        self.staging_root = root.as_ref().to_path_buf();
        self.basic_transfers = Arc::new(Semaphore::new(maximum_transfers.max(1)));
        self
    }

    /// Configures the maximum concurrent chunk/finalization data-plane work.
    #[must_use]
    pub fn with_data_limit(mut self, maximum_requests: usize) -> Self {
        self.data_requests = Arc::new(Semaphore::new(maximum_requests.max(1)));
        self
    }

    /// Overrides Forgejo authorization-cache policy for deployment or tests.
    #[must_use]
    pub fn with_forgejo_cache(mut self, ttl: Duration, capacity: usize) -> Self {
        if let Authentication::Forgejo { cache, .. } = &mut self.authentication {
            cache.ttl = ttl;
            cache.capacity = capacity.max(1);
        }
        self
    }
}

/// OIDC discovery or key-loading failure during fail-closed startup.
#[derive(Debug, thiserror::Error)]
pub enum OidcSetupError {
    /// Provider HTTP or JSON operation failed.
    #[error("OIDC provider request failed: {0}")]
    Provider(#[from] reqwest::Error),
    /// A configured or discovered URL was invalid.
    #[error("OIDC URL is invalid: {0}")]
    Url(#[from] url::ParseError),
    /// Discovery did not describe the configured issuer.
    #[error("OIDC discovery issuer does not match the configured issuer")]
    IssuerMismatch,
}

#[derive(serde::Deserialize)]
struct OidcDiscovery {
    issuer: String,
    jwks_uri: Url,
}

#[derive(serde::Deserialize)]
struct OidcClaims {
    sub: String,
}

/// Runs all embedded `PostgreSQL` schema migrations.
///
/// # Errors
///
/// Returns the migration failure without starting or mutating HTTP state.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!().run(pool).await
}

/// Builds the public HTTP router for one configured service instance.
pub fn build_router(state: AppState) -> Router {
    let request_id = header::HeaderName::from_static("x-request-id");
    Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/readyz", get(readiness))
        .route("/metrics", get(metrics))
        .route("/{owner}/{repository}/info/lfs/objects/batch", post(batch))
        .route(
            "/{owner}/{repository}/info/lfs/objects/{oid}",
            put(upload_basic).get(download_basic),
        )
        .route(
            "/{owner}/{repository}/info/lfs/objects/{oid}/cdc",
            post(begin_cdc).get(download_cdc_manifest),
        )
        .route(
            "/{owner}/{repository}/info/lfs/objects/{oid}/cdc/chunks/{index}",
            get(download_cdc_chunk),
        )
        .route(
            "/{owner}/{repository}/info/lfs/objects/{oid}/cdc/{upload_id}/chunks/{index}",
            put(upload_cdc_chunk),
        )
        .route(
            "/{owner}/{repository}/info/lfs/objects/{oid}/cdc/{upload_id}/finalize",
            post(finalize_cdc),
        )
        .route(
            "/{owner}/{repository}/info/lfs/locks",
            post(create_lock).get(list_locks),
        )
        .route(
            "/{owner}/{repository}/info/lfs/locks/verify",
            post(verify_locks),
        )
        .route(
            "/{owner}/{repository}/info/lfs/locks/{lock_id}/unlock",
            post(unlock),
        )
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            record_http_metrics,
        ))
        .with_state(state)
}

async fn record_http_metrics(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    state.metrics.in_flight.fetch_add(1, Ordering::Relaxed);
    let response = next.run(request).await;
    state.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    state.metrics.http_requests.fetch_add(1, Ordering::Relaxed);
    if response.status().is_server_error() {
        state.metrics.http_failures.fetch_add(1, Ordering::Relaxed);
    }
    response
}

async fn readiness(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .map_err(|error| ApiError::database(&error))?;
    let storage_is_fresh = state
        .storage_ready_until
        .lock()
        .await
        .is_some_and(|until| until > Instant::now());
    if !storage_is_fresh {
        state
            .chunks
            .healthcheck()
            .await
            .map_err(ApiError::storage)?;
        *state.storage_ready_until.lock().await = Some(Instant::now() + Duration::from_secs(10));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn metrics(State(state): State<AppState>) -> String {
    format!(
        "# TYPE git_cdc_logical_upload_bytes_total counter\n\
git_cdc_logical_upload_bytes_total {}\n\
# TYPE git_cdc_logical_download_bytes_total counter\n\
git_cdc_logical_download_bytes_total {}\n\
# TYPE git_cdc_received_chunk_bytes_total counter\n\
git_cdc_received_chunk_bytes_total {}\n\
# TYPE git_cdc_forgejo_auth_cache_hits_total counter\n\
git_cdc_forgejo_auth_cache_hits_total {}\n\
# TYPE git_cdc_forgejo_auth_cache_misses_total counter\n\
git_cdc_forgejo_auth_cache_misses_total {}\n\
# TYPE git_cdc_forgejo_requests_total counter\n\
git_cdc_forgejo_requests_total {}\n\
# TYPE git_cdc_http_requests_total counter\n\
git_cdc_http_requests_total {}\n\
# TYPE git_cdc_http_failures_total counter\n\
git_cdc_http_failures_total {}\n\
# TYPE git_cdc_http_in_flight gauge\n\
git_cdc_http_in_flight {}\n",
        state.metrics.logical_uploaded.load(Ordering::Relaxed),
        state.metrics.logical_downloaded.load(Ordering::Relaxed),
        state.metrics.chunks_received.load(Ordering::Relaxed),
        state.metrics.auth_cache_hits.load(Ordering::Relaxed),
        state.metrics.auth_cache_misses.load(Ordering::Relaxed),
        state.metrics.forgejo_requests.load(Ordering::Relaxed),
        state.metrics.http_requests.load(Ordering::Relaxed),
        state.metrics.http_failures.load(Ordering::Relaxed),
        state.metrics.in_flight.load(Ordering::Relaxed),
    )
}

async fn create_lock(
    State(state): State<AppState>,
    Path((owner, repository)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<LockRequest>,
) -> Result<(StatusCode, Json<LockResponse>), ApiError> {
    let identity = authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    validate_lock_path(&request.path)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let id = Uuid::new_v4();
    let result = sqlx::query_as::<_, (String,)>(
        "INSERT INTO lfs_locks (id, repository_id, path, owner_subject) \
         VALUES ($1, $2, $3, $4) \
         RETURNING to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .bind(id)
    .bind(repository_id)
    .bind(&request.path)
    .bind(&identity.subject)
    .fetch_one(&state.pool)
    .await;
    let (locked_at,) = match result {
        Ok(row) => row,
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|db| db.code().as_deref() == Some("23505")) =>
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "path is already locked",
            ));
        }
        Err(error) => return Err(ApiError::database(&error)),
    };
    Ok((
        StatusCode::CREATED,
        Json(LockResponse {
            lock: Lock {
                id: id.to_string(),
                path: request.path,
                locked_at,
                owner: LockOwner {
                    name: identity.subject,
                },
            },
        }),
    ))
}

async fn list_locks(
    State(state): State<AppState>,
    Path((owner, repository)): Path<(String, String)>,
    Query(query): Query<LockListQuery>,
    headers: HeaderMap,
) -> Result<Json<LockList>, ApiError> {
    authenticate(&state, &headers, &owner, &repository, Access::Read).await?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let (locks, next_cursor) = load_locks_page(
        &state,
        repository_id,
        query.path.as_deref(),
        query.id,
        query.cursor,
        query.limit,
    )
    .await?;
    Ok(Json(LockList { locks, next_cursor }))
}

async fn load_locks_page(
    state: &AppState,
    repository_id: Uuid,
    path: Option<&str>,
    id: Option<Uuid>,
    cursor: Option<Uuid>,
    requested_limit: Option<usize>,
) -> Result<(Vec<Lock>, Option<String>), ApiError> {
    let limit = requested_limit.unwrap_or(100).clamp(1, 100);
    let rows =
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            "SELECT id, path, owner_subject, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
         FROM lfs_locks WHERE repository_id = $1 \
           AND ($2::text IS NULL OR path = $2) \
           AND ($3::uuid IS NULL OR id = $3) \
           AND ($4::uuid IS NULL OR id > $4) \
         ORDER BY id LIMIT $5",
        )
        .bind(repository_id)
        .bind(path)
        .bind(id)
        .bind(cursor)
        .bind(i64::try_from(limit + 1).map_err(|_| {
            ApiError::new(StatusCode::BAD_REQUEST, "lock page limit is out of range")
        })?)
        .fetch_all(&state.pool)
        .await
        .map_err(|error| ApiError::database(&error))?;
    let mut locks: Vec<_> = rows
        .into_iter()
        .map(|(id, path, subject, locked_at)| Lock {
            id: id.to_string(),
            path,
            locked_at,
            owner: LockOwner { name: subject },
        })
        .collect();
    let has_more = locks.len() > limit;
    locks.truncate(limit);
    let next_cursor = has_more
        .then(|| locks.last().map(|lock| lock.id.clone()))
        .flatten();
    Ok((locks, next_cursor))
}

async fn verify_locks(
    State(state): State<AppState>,
    Path((owner, repository)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<LockVerifyRequest>,
) -> Result<Json<LockVerifyResponse>, ApiError> {
    let identity = authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let (locks, next_cursor) = load_locks_page(
        &state,
        repository_id,
        None,
        None,
        request.cursor,
        request.limit,
    )
    .await?;
    let (ours, theirs) = locks
        .into_iter()
        .partition(|lock| lock.owner.name == identity.subject);
    Ok(Json(LockVerifyResponse {
        ours,
        theirs,
        next_cursor,
    }))
}

async fn unlock(
    State(state): State<AppState>,
    Path((owner, repository, lock_id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
    Json(request): Json<UnlockRequest>,
) -> Result<Json<UnlockResponse>, ApiError> {
    let identity = authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let lock_owner: String = sqlx::query_scalar(
        "SELECT owner_subject FROM lfs_locks WHERE id = $1 AND repository_id = $2",
    )
    .bind(lock_id)
    .bind(repository_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| ApiError::database(&error))?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "lock not found"))?;
    if lock_owner != identity.subject && (!request.force || !identity.administrator) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only the lock owner or an administrator using force may unlock",
        ));
    }
    let row = sqlx::query_as::<_, (String, String, String)>(
        "DELETE FROM lfs_locks WHERE id = $1 AND repository_id = $2 \
         RETURNING path, owner_subject, \
         to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
    )
    .bind(lock_id)
    .bind(repository_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| ApiError::database(&error))?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "lock not found"))?;
    Ok(Json(UnlockResponse {
        lock: Lock {
            id: lock_id.to_string(),
            path: row.0,
            owner: LockOwner { name: row.1 },
            locked_at: row.2,
        },
    }))
}

fn validate_lock_path(path: &str) -> Result<(), ApiError> {
    if path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|component| component == ".." || component.is_empty())
    {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "lock path must be a normalized repository-relative path",
        ))
    } else {
        Ok(())
    }
}

async fn upload_basic(
    State(state): State<AppState>,
    Path((owner, repository, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    let oid = parse_oid(&oid)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > git_cdc_core::MAX_OBJECT_SIZE)
    {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "object exceeds the 100 GiB production limit",
        ));
    }
    let _permit =
        state.basic_transfers.acquire().await.map_err(|_| {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down")
        })?;
    tokio::fs::create_dir_all(&state.staging_root)
        .await
        .map_err(ApiError::io)?;
    let temporary = tempfile::NamedTempFile::new_in(&state.staging_root).map_err(ApiError::io)?;
    let mut file = tokio::fs::File::from_std(temporary.reopen().map_err(ApiError::io)?);
    let reader = StreamReader::new(
        body.into_data_stream()
            .map_err(|error| std::io::Error::other(error.to_string())),
    );
    let mut limited = reader.take(git_cdc_core::MAX_OBJECT_SIZE + 1);
    let copied = tokio::io::copy(&mut limited, &mut file)
        .await
        .map_err(ApiError::io)?;
    if copied > git_cdc_core::MAX_OBJECT_SIZE {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "object exceeds the 100 GiB production limit",
        ));
    }
    file.flush().await.map_err(ApiError::io)?;

    let source = temporary.reopen().map_err(ApiError::io)?;
    let manifest = tokio::task::spawn_blocking(move || {
        ChunkStream::new(source, git_cdc_core::ChunkingProfile::beta_v1())
            .finish()
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(ApiError::join)?
    .map_err(|message| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message))?;
    if manifest.object_oid != oid {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "uploaded bytes do not match the requested object OID",
        ));
    }
    let upload_id = ensure_upload_session(&state, repository_id, &manifest).await?;

    let source = temporary.reopen().map_err(ApiError::io)?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let producer = tokio::task::spawn_blocking(move || {
        let mut stream = ChunkStream::new(source, git_cdc_core::ChunkingProfile::beta_v1());
        for chunk in stream.by_ref() {
            let chunk = chunk.map_err(|error| error.to_string())?;
            if sender.blocking_send(chunk).is_err() {
                return Err("upload consumer stopped".to_owned());
            }
        }
        stream
            .finish()
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    while let Some(chunk) = receiver.recv().await {
        register_and_store_chunk(
            &state,
            repository_id,
            upload_id,
            &chunk.descriptor,
            Bytes::from(chunk.data),
        )
        .await?;
    }
    producer
        .await
        .map_err(ApiError::join)?
        .map_err(|message| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message))?;
    let published = publish_manifest(&state, repository_id, upload_id, &manifest).await?;
    if published {
        state
            .metrics
            .logical_uploaded
            .fetch_add(manifest.object_size, Ordering::Relaxed);
    }
    state
        .metrics
        .chunks_received
        .fetch_add(manifest.object_size, Ordering::Relaxed);
    Ok(StatusCode::NO_CONTENT)
}

async fn download_basic(
    State(state): State<AppState>,
    Path((owner, repository, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers, &owner, &repository, Access::Read).await?;
    let oid = parse_oid(&oid)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let permit = state
        .basic_transfers
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down"))?;
    let manifest = load_manifest(&state, repository_id, oid).await?;
    manifest
        .validate()
        .map_err(|error| ApiError::integrity(error.to_string()))?;
    tokio::fs::create_dir_all(&state.staging_root)
        .await
        .map_err(ApiError::io)?;
    let temporary = tempfile::NamedTempFile::new_in(&state.staging_root).map_err(ApiError::io)?;
    let (standard_file, temporary_path) = temporary.into_parts();
    let mut file = tokio::fs::File::from_std(standard_file);
    let mut hasher = Sha256::new();
    for descriptor in &manifest.chunks {
        let bytes = state
            .chunks
            .get_verified(repository_id, descriptor.id)
            .await
            .map_err(ApiError::storage)?;
        if bytes.len() != descriptor.length as usize {
            return Err(ApiError::integrity(
                "stored chunk length does not match manifest",
            ));
        }
        hasher.update(&bytes);
        file.write_all(&bytes).await.map_err(ApiError::io)?;
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if actual.as_slice() != oid.as_bytes() {
        return Err(ApiError::integrity("reconstructed object SHA-256 mismatch"));
    }
    file.seek(SeekFrom::Start(0)).await.map_err(ApiError::io)?;
    let stream = ReaderStream::new(file).map(move |result| {
        let _keep_until_stream_ends = &temporary_path;
        let _hold_transfer_slot = &permit;
        result
    });
    state
        .metrics
        .logical_downloaded
        .fetch_add(manifest.object_size, Ordering::Relaxed);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, manifest.object_size)
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn begin_cdc(
    State(state): State<AppState>,
    Path((owner, repository, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<BeginUploadRequest>,
) -> Result<Json<BeginUploadResponse>, ApiError> {
    authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    let oid = parse_oid(&oid)?;
    if request.protocol_version != 1 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unsupported CDC protocol version",
        ));
    }
    request
        .manifest
        .validate()
        .map_err(|error| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    if request.manifest.object_oid != oid {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "manifest OID does not match the requested object OID",
        ));
    }
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let manifest_json = serde_json::to_value(&request.manifest)
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|error| ApiError::database(&error))?;
    sqlx::query(
        "UPDATE upload_sessions SET state = 'expired' \
         WHERE repository_id = $1 AND object_oid = $2 \
           AND state = 'open' AND expires_at <= now()",
    )
    .bind(repository_id)
    .bind(oid.as_bytes().as_slice())
    .execute(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?;
    let (upload_id, expires_at, stored_manifest) =
        sqlx::query_as::<_, (Uuid, String, serde_json::Value)>(
        "INSERT INTO upload_sessions (id, repository_id, object_oid, object_size, manifest, state, expires_at) \
         VALUES ($1, $2, $3, $4, $5, 'open', now() + interval '24 hours') \
         ON CONFLICT (repository_id, object_oid) WHERE state = 'open' \
         DO UPDATE SET expires_at = now() + interval '24 hours' \
         RETURNING id, to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), manifest",
    )
    .bind(Uuid::new_v4())
    .bind(repository_id)
    .bind(oid.as_bytes().as_slice())
    .bind(i64::try_from(request.manifest.object_size).map_err(|_| {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "object is too large")
    })?)
    .bind(manifest_json)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?;
    let stored_manifest: ObjectManifest = serde_json::from_value(stored_manifest)
        .map_err(|error| ApiError::integrity(error.to_string()))?;
    if stored_manifest != request.manifest {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "an open upload for this OID uses a different manifest",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| ApiError::database(&error))?;
    let mut missing_chunk_indexes = Vec::new();
    for (index, descriptor) in stored_manifest.chunks.iter().enumerate() {
        if !state
            .chunks
            .exists(repository_id, descriptor.id)
            .await
            .map_err(ApiError::storage)?
        {
            missing_chunk_indexes.push(u32::try_from(index).map_err(|_| {
                ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "manifest has too many chunks",
                )
            })?);
        }
    }
    Ok(Json(BeginUploadResponse {
        protocol_version: 1,
        upload_id,
        missing_chunk_indexes,
        expires_at,
    }))
}

async fn upload_cdc_chunk(
    State(state): State<AppState>,
    Path((owner, repository, oid, upload_id, index)): Path<(String, String, String, Uuid, usize)>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<StatusCode, ApiError> {
    let _permit =
        state.data_requests.acquire().await.map_err(|_| {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down")
        })?;
    authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    let oid = parse_oid(&oid)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let manifest = load_open_upload(&state, repository_id, oid, upload_id).await?;
    let descriptor = manifest.chunks.get(index).ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "chunk index is outside the manifest")
    })?;
    if bytes.len() != descriptor.length as usize {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chunk length mismatch",
        ));
    }
    let received = bytes.len() as u64;
    register_and_store_chunk(&state, repository_id, upload_id, descriptor, bytes).await?;
    state
        .metrics
        .chunks_received
        .fetch_add(received, Ordering::Relaxed);
    Ok(StatusCode::NO_CONTENT)
}

async fn finalize_cdc(
    State(state): State<AppState>,
    Path((owner, repository, oid, upload_id)): Path<(String, String, String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let _permit =
        state.data_requests.acquire().await.map_err(|_| {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down")
        })?;
    authenticate(&state, &headers, &owner, &repository, Access::Write).await?;
    let oid = parse_oid(&oid)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let (manifest, already_finalized) =
        load_finalizable_upload(&state, repository_id, oid, upload_id).await?;
    if already_finalized {
        return Ok(StatusCode::NO_CONTENT);
    }
    let mut hasher = Sha256::new();
    for descriptor in &manifest.chunks {
        let bytes = state
            .chunks
            .get_verified(repository_id, descriptor.id)
            .await
            .map_err(|error| match error {
                git_cdc_storage::StorageError::Provider(object_store::Error::NotFound {
                    ..
                }) => ApiError::new(StatusCode::CONFLICT, "upload is missing chunks"),
                other => ApiError::storage(other),
            })?;
        if bytes.len() != descriptor.length as usize {
            return Err(ApiError::integrity(
                "stored chunk length does not match manifest",
            ));
        }
        hasher.update(bytes);
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if actual.as_slice() != oid.as_bytes() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "object SHA-256 mismatch",
        ));
    }
    let published = publish_manifest(&state, repository_id, upload_id, &manifest).await?;
    if published {
        state
            .metrics
            .logical_uploaded
            .fetch_add(manifest.object_size, Ordering::Relaxed);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn download_cdc_manifest(
    State(state): State<AppState>,
    Path((owner, repository, oid)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Json<ObjectManifest>, ApiError> {
    authenticate(&state, &headers, &owner, &repository, Access::Read).await?;
    let oid = parse_oid(&oid)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    Ok(Json(load_manifest(&state, repository_id, oid).await?))
}

async fn download_cdc_chunk(
    State(state): State<AppState>,
    Path((owner, repository, oid, index)): Path<(String, String, String, usize)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _permit =
        state.data_requests.acquire().await.map_err(|_| {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "server is shutting down")
        })?;
    authenticate(&state, &headers, &owner, &repository, Access::Read).await?;
    let oid = parse_oid(&oid)?;
    let repository_id = repository_id(&state, &owner, &repository).await?;
    let manifest = load_manifest(&state, repository_id, oid).await?;
    let descriptor = manifest.chunks.get(index).ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "chunk index is outside the manifest")
    })?;
    let bytes = state
        .chunks
        .get_verified(repository_id, descriptor.id)
        .await
        .map_err(ApiError::storage)?;
    if bytes.len() != descriptor.length as usize {
        return Err(ApiError::integrity(
            "stored chunk length does not match manifest",
        ));
    }
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response())
}

async fn batch(
    State(state): State<AppState>,
    Path((owner, repository)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<BatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if request.objects.len() > 1_000 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "batch contains more than 1,000 objects",
        ));
    }
    let access = match request.operation {
        Operation::Upload => Access::Write,
        Operation::Download => Access::Read,
    };
    let identity = authenticate(&state, &headers, &owner, &repository, access).await?;
    let repository_id =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM repositories WHERE owner = $1 AND name = $2")
            .bind(&owner)
            .bind(&repository)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| ApiError::database(&error))?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "repository not found"))?;
    let transfer = select_transfer(&request.transfers)
        .map_err(|error| ApiError::new(StatusCode::NOT_ACCEPTABLE, error.to_string()))?;
    let mut objects = Vec::with_capacity(request.objects.len());

    for object in request.objects {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE repository_id = $1 AND oid = $2)",
        )
        .bind(repository_id)
        .bind(object.oid.as_bytes().as_slice())
        .fetch_one(&state.pool)
        .await
        .map_err(|error| ApiError::database(&error))?;
        let mut actions = BTreeMap::new();
        let mut object_error = None;

        match (request.operation, exists) {
            (Operation::Upload, false) => {
                let mut action =
                    action_url(&state.base_url, &owner, &repository, object.oid, transfer)?;
                action
                    .header
                    .insert("Authorization".into(), identity.authorization.clone());
                actions.insert("upload".into(), action);
            }
            (Operation::Download, true) => {
                let mut action =
                    action_url(&state.base_url, &owner, &repository, object.oid, transfer)?;
                action
                    .header
                    .insert("Authorization".into(), identity.authorization.clone());
                actions.insert("download".into(), action);
            }
            (Operation::Download, false) => {
                object_error = Some(BatchObjectError {
                    code: StatusCode::NOT_FOUND.as_u16(),
                    message: "object not found".into(),
                });
            }
            (Operation::Upload, true) => {}
        }
        objects.push(BatchObjectResponse {
            oid: object.oid,
            size: object.size,
            actions,
            error: object_error,
        });
    }

    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(LFS_MEDIA_TYPE),
        )],
        Json(BatchResponse { transfer, objects }),
    ))
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repository: &str,
    access: Access,
) -> Result<Identity, ApiError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::unauthorized)?;
    match &state.authentication {
        Authentication::DevelopmentToken { hash } => {
            let token = value
                .strip_prefix("Bearer ")
                .ok_or_else(ApiError::unauthorized)?;
            let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if bool::from(candidate.ct_eq(hash)) {
                Ok(Identity {
                    authorization: value.to_owned(),
                    subject: "development-user".into(),
                    administrator: true,
                })
            } else {
                Err(ApiError::unauthorized())
            }
        }
        Authentication::Forgejo {
            base_url,
            client,
            cache,
        } => {
            authenticate_forgejo(
                state, base_url, client, cache, value, owner, repository, access,
            )
            .await
        }
        Authentication::Oidc {
            issuer,
            audience,
            keys,
        } => {
            authenticate_oidc(
                state,
                OidcTrust {
                    issuer,
                    audience,
                    keys,
                },
                value,
                owner,
                repository,
                access,
            )
            .await
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the adapter keeps cache lookup and the two fail-closed Forgejo checks together"
)]
async fn authenticate_forgejo(
    state: &AppState,
    base_url: &Url,
    client: &reqwest::Client,
    cache: &ForgejoAuthorizationCache,
    authorization: &str,
    owner: &str,
    repository_name: &str,
    access: Access,
) -> Result<Identity, ApiError> {
    let mut key_hasher = Sha256::new();
    key_hasher.update(authorization.as_bytes());
    key_hasher.update([0]);
    key_hasher.update(owner.as_bytes());
    key_hasher.update([0]);
    key_hasher.update(repository_name.as_bytes());
    key_hasher.update([match access {
        Access::Read => 0,
        Access::Write => 1,
    }]);
    let cache_key: [u8; 32] = key_hasher.finalize().into();
    {
        let mut entries = cache.entries.lock().await;
        if let Some(cached) = entries.get(&cache_key) {
            if cached.expires_at > Instant::now() {
                state
                    .metrics
                    .auth_cache_hits
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(Identity {
                    authorization: authorization.to_owned(),
                    subject: cached.subject.clone(),
                    administrator: cached.administrator,
                });
            }
            entries.remove(&cache_key);
        }
    }
    state
        .metrics
        .auth_cache_misses
        .fetch_add(1, Ordering::Relaxed);
    let user_url = base_url
        .join("api/v1/user")
        .map_err(|error| ApiError::authentication(error.to_string()))?;
    let mut repo_url = base_url.clone();
    repo_url
        .path_segments_mut()
        .map_err(|()| ApiError::authentication("invalid Forgejo URL"))?
        .extend(["api", "v1", "repos", owner, repository_name]);
    state
        .metrics
        .forgejo_requests
        .fetch_add(1, Ordering::Relaxed);
    let user_response = client
        .get(user_url)
        .header(header::AUTHORIZATION, authorization)
        .send()
        .await
        .map_err(|error| ApiError::authentication(error.to_string()))?;
    if !user_response.status().is_success() {
        return Err(ApiError::unauthorized());
    }
    let user: ForgejoUser = user_response
        .json()
        .await
        .map_err(|error| ApiError::authentication(error.to_string()))?;
    state
        .metrics
        .forgejo_requests
        .fetch_add(1, Ordering::Relaxed);
    let repo_response = client
        .get(repo_url)
        .header(header::AUTHORIZATION, authorization)
        .send()
        .await
        .map_err(|error| ApiError::authentication(error.to_string()))?;
    if repo_response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ApiError::unauthorized());
    }
    if repo_response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "repository not found"));
    }
    let repository: ForgejoRepository = repo_response
        .error_for_status()
        .map_err(|error| ApiError::authentication(error.to_string()))?
        .json()
        .await
        .map_err(|error| ApiError::authentication(error.to_string()))?;
    let allowed = match access {
        Access::Read => {
            repository.permissions.pull
                || repository.permissions.push
                || repository.permissions.admin
        }
        Access::Write => repository.permissions.push || repository.permissions.admin,
    };
    if !allowed {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "repository access denied",
        ));
    }
    let identity = Identity {
        authorization: authorization.to_owned(),
        subject: user.login,
        administrator: repository.permissions.admin,
    };
    let mut entries = cache.entries.lock().await;
    entries.retain(|_, value| value.expires_at > Instant::now());
    if entries.len() >= cache.capacity {
        if let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, value)| value.expires_at)
            .map(|(key, _)| *key)
        {
            entries.remove(&oldest);
        }
    }
    entries.insert(
        cache_key,
        CachedForgejoIdentity {
            subject: identity.subject.clone(),
            administrator: identity.administrator,
            expires_at: Instant::now() + cache.ttl,
        },
    );
    Ok(identity)
}

struct OidcTrust<'a> {
    issuer: &'a str,
    audience: &'a str,
    keys: &'a HashMap<String, jsonwebtoken::jwk::Jwk>,
}

async fn authenticate_oidc(
    state: &AppState,
    trust: OidcTrust<'_>,
    authorization: &str,
    owner: &str,
    repository: &str,
    access: Access,
) -> Result<Identity, ApiError> {
    let token = authorization
        .strip_prefix("Bearer ")
        .ok_or_else(ApiError::unauthorized)?;
    let token_header = jsonwebtoken::decode_header(token).map_err(|_| ApiError::unauthorized())?;
    let key_id = token_header.kid.ok_or_else(ApiError::unauthorized)?;
    let jwk = trust.keys.get(&key_id).ok_or_else(ApiError::unauthorized)?;
    let key = jsonwebtoken::DecodingKey::from_jwk(jwk).map_err(|_| ApiError::unauthorized())?;
    let mut validation = jsonwebtoken::Validation::new(token_header.alg);
    validation.set_issuer(&[trust.issuer]);
    validation.set_audience(&[trust.audience]);
    let claims = jsonwebtoken::decode::<OidcClaims>(token, &key, &validation)
        .map_err(|_| ApiError::unauthorized())?
        .claims;
    let grant = sqlx::query_as::<_, (bool, bool, bool)>(
        "SELECT g.can_read, g.can_write, g.can_admin \
                 FROM repository_grants g JOIN repositories r ON r.id = g.repository_id \
                 WHERE r.owner = $1 AND r.name = $2 AND g.subject = $3",
    )
    .bind(owner)
    .bind(repository)
    .bind(&claims.sub)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| ApiError::database(&error))?
    .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "repository access denied"))?;
    let allowed = match access {
        Access::Read => grant.0,
        Access::Write => grant.1,
    };
    if !allowed {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "repository access denied",
        ));
    }
    Ok(Identity {
        authorization: authorization.to_owned(),
        subject: claims.sub,
        administrator: grant.2,
    })
}

#[derive(serde::Deserialize)]
struct ForgejoUser {
    login: String,
}

#[derive(serde::Deserialize)]
struct ForgejoRepository {
    permissions: ForgejoPermissions,
}

#[derive(serde::Deserialize)]
struct ForgejoPermissions {
    #[serde(default)]
    pull: bool,
    #[serde(default)]
    push: bool,
    #[serde(default)]
    admin: bool,
}

fn action_url(
    base_url: &Url,
    owner: &str,
    repository: &str,
    oid: git_cdc_core::ObjectOid,
    transfer: TransferKind,
) -> Result<LfsAction, ApiError> {
    let mut url = base_url.clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid base URL"))?;
        segments.pop_if_empty();
        segments.extend([owner, repository, "info", "lfs", "objects"]);
        segments.push(&oid.to_string());
        if transfer == TransferKind::Cdc {
            segments.push("cdc");
        }
    }
    Ok(LfsAction {
        href: url,
        header: BTreeMap::new(),
        expires_at: None,
    })
}

fn parse_oid(value: &str) -> Result<ObjectOid, ApiError> {
    ObjectOid::from_str(value)
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))
}

async fn repository_id(state: &AppState, owner: &str, repository: &str) -> Result<Uuid, ApiError> {
    sqlx::query_scalar("SELECT id FROM repositories WHERE owner = $1 AND name = $2")
        .bind(owner)
        .bind(repository)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| ApiError::database(&error))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "repository not found"))
}

async fn load_manifest(
    state: &AppState,
    repository_id: Uuid,
    oid: ObjectOid,
) -> Result<ObjectManifest, ApiError> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT manifest FROM objects WHERE repository_id = $1 AND oid = $2")
            .bind(repository_id)
            .bind(oid.as_bytes().as_slice())
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| ApiError::database(&error))?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "object not found"))?;
    serde_json::from_value(value).map_err(|error| ApiError::integrity(error.to_string()))
}

async fn load_open_upload(
    state: &AppState,
    repository_id: Uuid,
    oid: ObjectOid,
    upload_id: Uuid,
) -> Result<ObjectManifest, ApiError> {
    let value: serde_json::Value = sqlx::query_scalar(
        "SELECT manifest FROM upload_sessions \
         WHERE id = $1 AND repository_id = $2 AND object_oid = $3 \
           AND state = 'open' AND expires_at > now()",
    )
    .bind(upload_id)
    .bind(repository_id)
    .bind(oid.as_bytes().as_slice())
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| ApiError::database(&error))?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "open upload session not found"))?;
    serde_json::from_value(value).map_err(|error| ApiError::integrity(error.to_string()))
}

async fn ensure_upload_session(
    state: &AppState,
    repository_id: Uuid,
    manifest: &ObjectManifest,
) -> Result<Uuid, ApiError> {
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|error| ApiError::database(&error))?;
    sqlx::query(
        "UPDATE upload_sessions SET state = 'expired' \
         WHERE repository_id = $1 AND object_oid = $2 \
           AND state = 'open' AND expires_at <= now()",
    )
    .bind(repository_id)
    .bind(manifest.object_oid.as_bytes().as_slice())
    .execute(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?;
    let (id, stored): (Uuid, serde_json::Value) = sqlx::query_as(
        "INSERT INTO upload_sessions \
         (id, repository_id, object_oid, object_size, manifest, state, expires_at) \
         VALUES ($1, $2, $3, $4, $5, 'open', now() + interval '24 hours') \
         ON CONFLICT (repository_id, object_oid) WHERE state = 'open' \
         DO UPDATE SET expires_at = now() + interval '24 hours' \
         RETURNING id, manifest",
    )
    .bind(Uuid::new_v4())
    .bind(repository_id)
    .bind(manifest.object_oid.as_bytes().as_slice())
    .bind(i64::try_from(manifest.object_size).map_err(|_| ApiError::integrity("object too large"))?)
    .bind(serde_json::to_value(manifest).map_err(|error| ApiError::integrity(error.to_string()))?)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?;
    let stored: ObjectManifest =
        serde_json::from_value(stored).map_err(|error| ApiError::integrity(error.to_string()))?;
    if stored != *manifest {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "an open upload for this OID uses a different manifest",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| ApiError::database(&error))?;
    Ok(id)
}

async fn register_and_store_chunk(
    state: &AppState,
    repository_id: Uuid,
    upload_id: Uuid,
    descriptor: &ChunkDescriptor,
    bytes: Bytes,
) -> Result<(), ApiError> {
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|error| ApiError::database(&error))?;
    let registered = sqlx::query_scalar::<_, i64>(
        "INSERT INTO chunks (repository_id, chunk_id, size) VALUES ($1, $2, $3) \
         ON CONFLICT (repository_id, chunk_id) DO UPDATE SET size = chunks.size \
         WHERE chunks.size = EXCLUDED.size RETURNING size",
    )
    .bind(repository_id)
    .bind(descriptor.id.as_bytes().as_slice())
    .bind(i64::from(descriptor.length))
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?;
    if registered.is_none() {
        return Err(ApiError::integrity(
            "chunk metadata disagrees with the manifest length",
        ));
    }
    sqlx::query(
        "INSERT INTO upload_session_chunks (session_id, repository_id, chunk_id) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(upload_id)
    .bind(repository_id)
    .bind(descriptor.id.as_bytes().as_slice())
    .execute(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?;
    transaction
        .commit()
        .await
        .map_err(|error| ApiError::database(&error))?;
    state
        .chunks
        .put_verified(repository_id, descriptor.id, bytes)
        .await
        .map_err(ApiError::storage)
}

async fn load_finalizable_upload(
    state: &AppState,
    repository_id: Uuid,
    oid: ObjectOid,
    upload_id: Uuid,
) -> Result<(ObjectManifest, bool), ApiError> {
    let row: (serde_json::Value, String) = sqlx::query_as(
        "SELECT manifest, state FROM upload_sessions \
         WHERE id = $1 AND repository_id = $2 AND object_oid = $3 \
           AND ((state = 'open' AND expires_at > now()) OR state = 'finalized')",
    )
    .bind(upload_id)
    .bind(repository_id)
    .bind(oid.as_bytes().as_slice())
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| ApiError::database(&error))?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "upload session not found"))?;
    let manifest =
        serde_json::from_value(row.0).map_err(|error| ApiError::integrity(error.to_string()))?;
    Ok((manifest, row.1 == "finalized"))
}

#[allow(
    clippy::too_many_lines,
    reason = "publication is one auditable PostgreSQL transaction with ordered integrity updates"
)]
async fn publish_manifest(
    state: &AppState,
    repository_id: Uuid,
    upload_id: Uuid,
    manifest: &ObjectManifest,
) -> Result<bool, ApiError> {
    manifest
        .validate()
        .map_err(|error| ApiError::integrity(error.to_string()))?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|error| ApiError::database(&error))?;
    let session_state: String = sqlx::query_scalar(
        "SELECT state FROM upload_sessions \
         WHERE id = $1 AND repository_id = $2 AND object_oid = $3 \
           AND ((state = 'open' AND expires_at > now()) OR state = 'finalized') FOR UPDATE",
    )
    .bind(upload_id)
    .bind(repository_id)
    .bind(manifest.object_oid.as_bytes().as_slice())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "upload session not found"))?;
    if session_state == "finalized" {
        transaction
            .commit()
            .await
            .map_err(|error| ApiError::database(&error))?;
        return Ok(false);
    }
    if session_state != "open" {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "open upload session not found",
        ));
    }
    for descriptor in &manifest.chunks {
        sqlx::query(
            "INSERT INTO chunks (repository_id, chunk_id, size) VALUES ($1, $2, $3) \
             ON CONFLICT (repository_id, chunk_id) DO NOTHING",
        )
        .bind(repository_id)
        .bind(descriptor.id.as_bytes().as_slice())
        .bind(i64::from(descriptor.length))
        .execute(&mut *transaction)
        .await
        .map_err(|error| ApiError::database(&error))?;
    }
    let inserted = sqlx::query(
        "INSERT INTO objects (repository_id, oid, size, manifest) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (repository_id, oid) DO NOTHING",
    )
    .bind(repository_id)
    .bind(manifest.object_oid.as_bytes().as_slice())
    .bind(i64::try_from(manifest.object_size).map_err(|_| ApiError::integrity("object too large"))?)
    .bind(serde_json::to_value(manifest).map_err(|error| ApiError::integrity(error.to_string()))?)
    .execute(&mut *transaction)
    .await
    .map_err(|error| ApiError::database(&error))?
    .rows_affected();
    if inserted == 1 {
        for (ordinal, descriptor) in manifest.chunks.iter().enumerate() {
            sqlx::query(
                "INSERT INTO object_chunks \
                 (repository_id, object_oid, ordinal, chunk_id, byte_offset, byte_length) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(repository_id)
            .bind(manifest.object_oid.as_bytes().as_slice())
            .bind(i32::try_from(ordinal).map_err(|_| ApiError::integrity("too many chunks"))?)
            .bind(descriptor.id.as_bytes().as_slice())
            .bind(
                i64::try_from(descriptor.offset)
                    .map_err(|_| ApiError::integrity("offset too large"))?,
            )
            .bind(
                i32::try_from(descriptor.length)
                    .map_err(|_| ApiError::integrity("chunk too large"))?,
            )
            .execute(&mut *transaction)
            .await
            .map_err(|error| ApiError::database(&error))?;
            sqlx::query(
                "UPDATE chunks SET reference_count = reference_count + 1 \
                 WHERE repository_id = $1 AND chunk_id = $2",
            )
            .bind(repository_id)
            .bind(descriptor.id.as_bytes().as_slice())
            .execute(&mut *transaction)
            .await
            .map_err(|error| ApiError::database(&error))?;
        }
    }
    sqlx::query("UPDATE upload_sessions SET state = 'finalized' WHERE id = $1")
        .bind(upload_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| ApiError::database(&error))?;
    transaction
        .commit()
        .await
        .map_err(|error| ApiError::database(&error))?;
    Ok(inserted == 1)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "authentication required")
    }

    fn authentication(message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::error!(message, "authentication provider request failed");
        Self::new(
            StatusCode::BAD_GATEWAY,
            "authentication provider unavailable",
        )
    }

    fn database(error: &sqlx::Error) -> Self {
        tracing::error!(error = %error, "database request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal service error")
    }

    fn io(error: std::io::Error) -> Self {
        tracing::error!(error = %error, "I/O request failed");
        drop(error);
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal I/O error")
    }

    fn storage(error: git_cdc_storage::StorageError) -> Self {
        if matches!(error, git_cdc_storage::StorageError::DigestMismatch { .. }) {
            return Self::new(StatusCode::UNPROCESSABLE_ENTITY, error.to_string());
        }
        tracing::error!(error = %error, "object storage request failed");
        drop(error);
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "object storage error")
    }

    fn join(error: tokio::task::JoinError) -> Self {
        tracing::error!(error = %error, "blocking chunk producer failed");
        drop(error);
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "chunk producer failed")
    }

    fn integrity(message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::error!(message, "stored object failed integrity validation");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored object failed integrity validation",
        )
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(LFS_MEDIA_TYPE),
            )],
            Json(ErrorBody {
                message: &self.message,
            }),
        )
            .into_response()
    }
}
