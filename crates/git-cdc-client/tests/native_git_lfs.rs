//! Native stock-Git-LFS end to end through the compiled transfer agent.
#![allow(
    clippy::unwrap_used,
    reason = "native acceptance fixtures fail immediately"
)]

use std::{collections::HashMap, fs, process::Command, sync::Arc};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::{get, post, put},
};
use git_cdc_core::ObjectManifest;
use git_cdc_protocol::{BatchRequest, BeginUploadRequest, BeginUploadResponse, Operation};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone, Default)]
struct FakeState {
    base: String,
    manifests: Arc<Mutex<HashMap<String, ObjectManifest>>>,
    chunks: Arc<Mutex<HashMap<(String, usize), Bytes>>>,
    basic_objects: Arc<Mutex<HashMap<String, Bytes>>>,
}

async fn batch(State(state): State<FakeState>, Json(request): Json<BatchRequest>) -> Json<Value> {
    let cdc = request.transfers.iter().any(|transfer| transfer == "cdc");
    let objects = request
        .objects
        .into_iter()
        .map(|object| {
            let href = if cdc {
                format!("{}/objects/{}/cdc", state.base, object.oid)
            } else {
                format!("{}/basic/{}", state.base, object.oid)
            };
            let action = json!({"href": href});
            let actions = match request.operation {
                Operation::Upload => json!({"upload": action}),
                Operation::Download => json!({"download": action}),
            };
            json!({"oid": object.oid, "size": object.size, "actions": actions})
        })
        .collect::<Vec<_>>();
    Json(json!({"transfer": if cdc { "cdc" } else { "basic" },"objects":objects}))
}

async fn begin(
    State(state): State<FakeState>,
    Path(oid): Path<String>,
    Json(request): Json<BeginUploadRequest>,
) -> Json<BeginUploadResponse> {
    let missing_chunk_indexes = (0..request.manifest.chunks.len())
        .map(|index| u32::try_from(index).unwrap())
        .collect();
    state.manifests.lock().await.insert(oid, request.manifest);
    Json(BeginUploadResponse {
        protocol_version: 1,
        upload_id: Uuid::nil(),
        missing_chunk_indexes,
        expires_at: "2099-01-01T00:00:00Z".into(),
    })
}

async fn upload_chunk(
    State(state): State<FakeState>,
    Path((oid, _upload, index)): Path<(String, Uuid, usize)>,
    bytes: Bytes,
) -> StatusCode {
    state.chunks.lock().await.insert((oid, index), bytes);
    StatusCode::NO_CONTENT
}

async fn manifest(State(state): State<FakeState>, Path(oid): Path<String>) -> Json<ObjectManifest> {
    Json(state.manifests.lock().await.get(&oid).unwrap().clone())
}

async fn chunk(State(state): State<FakeState>, Path((oid, index)): Path<(String, usize)>) -> Bytes {
    state
        .chunks
        .lock()
        .await
        .get(&(oid, index))
        .unwrap()
        .clone()
}

async fn basic_upload(
    State(state): State<FakeState>,
    Path(oid): Path<String>,
    bytes: Bytes,
) -> StatusCode {
    state.basic_objects.lock().await.insert(oid, bytes);
    StatusCode::NO_CONTENT
}

async fn basic_download(State(state): State<FakeState>, Path(oid): Path<String>) -> Bytes {
    if let Some(bytes) = state.basic_objects.lock().await.get(&oid).cloned() {
        return bytes;
    }
    let manifests = state.manifests.lock().await;
    let manifest = manifests.get(&oid).unwrap();
    let chunks = state.chunks.lock().await;
    let mut object = Vec::with_capacity(usize::try_from(manifest.object_size).unwrap());
    for index in 0..manifest.chunks.len() {
        object.extend_from_slice(chunks.get(&(oid.clone(), index)).unwrap());
    }
    Bytes::from(object)
}

fn run(repository: &std::path::Path, binary: &str, cache: &std::path::Path, arguments: &[&str]) {
    let status = Command::new(binary)
        .current_dir(repository)
        .env("XDG_CACHE_HOME", cache)
        .env("LOCALAPPDATA", cache)
        .args(arguments)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "{binary} {arguments:?} failed with {status}"
    );
}

async fn serve_fixture() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = FakeState {
        base: base.clone(),
        ..FakeState::default()
    };
    let app = Router::new()
        .route("/team/assets/info/lfs/objects/batch", post(batch))
        .route("/objects/{oid}/cdc", post(begin).get(manifest))
        .route(
            "/objects/{oid}/cdc/{upload}/chunks/{index}",
            put(upload_chunk),
        )
        .route(
            "/objects/{oid}/cdc/{upload}/finalize",
            post(|| async { StatusCode::NO_CONTENT }),
        )
        .route("/objects/{oid}/cdc/chunks/{index}", get(chunk))
        .route("/basic/{oid}", put(basic_upload).get(basic_download))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, server)
}

fn initialize_repository(repository: &std::path::Path, cache: &std::path::Path, base: &str) {
    run(repository, "git", cache, &["init", "-b", "master"]);
    run(
        repository,
        "git",
        cache,
        &["config", "user.name", "Git CDC Test"],
    );
    run(
        repository,
        "git",
        cache,
        &["config", "user.email", "test@git-cdc.invalid"],
    );
    run(repository, "git", cache, &["lfs", "install", "--local"]);
    run(
        repository,
        env!("CARGO_BIN_EXE_git-cdc"),
        cache,
        &["install", "--scope", "local"],
    );
    run(
        repository,
        env!("CARGO_BIN_EXE_git-cdc"),
        cache,
        &[
            "configure",
            "--scope",
            "local",
            "--url",
            &format!("{base}/team/assets/info/lfs"),
        ],
    );
    run(
        repository,
        "git",
        cache,
        &[
            "remote",
            "add",
            "origin",
            "https://invalid.example/repository.git",
        ],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stock_git_lfs_pushes_and_fetches_through_native_agent() {
    let (base, server) = serve_fixture().await;

    let repository = tempfile::tempdir().unwrap();
    let cache = repository.path().join("cache");
    initialize_repository(repository.path(), &cache, &base);
    run(repository.path(), "git", &cache, &["lfs", "track", "*.bin"]);
    let source: Vec<u8> = (0_usize..3 * 1024 * 1024 + 19)
        .map(|index| index.wrapping_mul(29).to_le_bytes()[0])
        .collect();
    fs::write(repository.path().join("asset.bin"), &source).unwrap();
    let second_source: Vec<u8> = (0_usize..2 * 1024 * 1024 + 71)
        .map(|index| index.wrapping_mul(47).to_le_bytes()[0])
        .collect();
    fs::write(repository.path().join("spaced ünicode.bin"), second_source).unwrap();
    run(
        repository.path(),
        "git",
        &cache,
        &["add", ".gitattributes", "asset.bin", "spaced ünicode.bin"],
    );
    run(
        repository.path(),
        "git",
        &cache,
        &["commit", "-m", "native LFS fixture"],
    );
    run(
        repository.path(),
        "git",
        &cache,
        &["lfs", "push", "--all", "origin"],
    );

    let objects = repository.path().join(".git").join("lfs").join("objects");
    fs::rename(&objects, repository.path().join("lfs-objects-before-fetch")).unwrap();
    run(
        repository.path(),
        "git",
        &cache,
        &["lfs", "fetch", "origin", "master"],
    );
    run(
        repository.path(),
        "git",
        &cache,
        &["lfs", "fsck", "--objects"],
    );

    run(
        repository.path(),
        env!("CARGO_BIN_EXE_git-cdc"),
        &cache,
        &["uninstall", "--scope", "local"],
    );
    fs::rename(
        &objects,
        repository.path().join("lfs-objects-before-basic-fetch"),
    )
    .unwrap();
    run(
        repository.path(),
        "git",
        &cache,
        &["lfs", "fetch", "origin", "master"],
    );
    run(
        repository.path(),
        "git",
        &cache,
        &["lfs", "fsck", "--objects"],
    );
    server.abort();
}
