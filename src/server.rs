use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_server::tls_rustls::RustlsConfig;
use rcgen::generate_simple_self_signed;
use tokio::sync::Mutex;

mod accelerator;
mod auth;
mod error;
mod file;
mod process;
mod util;

pub use self::accelerator::handle_accelerator_status;
pub use self::file::{
    handle_file_delete, handle_file_find, handle_file_list, handle_file_read, handle_file_stat,
    handle_file_upload_abort, handle_file_upload_chunk, handle_file_upload_chunk_bytes,
    handle_file_upload_finish, handle_file_upload_init, handle_file_upload_status,
    handle_file_write, handle_sync_run, handle_sync_session_init, handle_sync_session_release,
    handle_sync_session_status,
};
pub use self::process::{
    handle_process_cleanup, handle_process_get, handle_process_list, handle_process_read,
    handle_process_start, handle_process_terminate, handle_process_write,
};

use self::auth::{authorized, validated_execution_lease};
use self::error::{bad_request_response, unauthorized_response};
use crate::mode::ModeRegistry;
use crate::protocol::{
    AcceleratorStatusRequest, FileDeleteRequest, FileFindRequest, FileListRequest, FileReadRequest,
    FileStatRequest, FileUploadAbortRequest, FileUploadChunkRequest, FileUploadFinishRequest,
    FileUploadInitRequest, FileUploadStatusRequest, FileWriteRequest, HEADER_UPLOAD_CHUNK_SHA256,
    HEADER_UPLOAD_LOCK_TOKEN, HEADER_UPLOAD_SYNC_SESSION_ID, LeaseReleaseRequest,
    LeaseReleaseResponse, LeaseRenewRequest, LeaseRenewResponse, ModeGetRequest, ModeGetResponse,
    ModeSwitchRequest, ModeSwitchResponse, ProcessCleanupRequest, ProcessEventMessage,
    ProcessEventSubscribeRequest, ProcessGetRequest, ProcessListRequest, ProcessReadRequest,
    ProcessStartRequest, ProcessTerminateRequest, ProcessWriteRequest, SimpleResponse, SyncPayload,
    SyncSessionInitRequest, SyncSessionReleaseRequest, SyncSessionStatusRequest,
};

const DEFAULT_PROCESS_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const DEFAULT_PROCESS_READ_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_PROCESSES: usize = 8;
const DEFAULT_MAX_ZOMBIE_PROCESSES: usize = 32;
const DEFAULT_MAX_STDIN_WRITE_BYTES: usize = 64 * 1024;
const MAX_PROCESS_OUTPUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROCESS_READ_MAX_BYTES: usize = 1024 * 1024;
const MAX_PROCESS_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
const DEFAULT_ZOMBIE_TTL_SECONDS: u64 = 600;
const PROCESS_EVENT_WAIT_MS: u64 = 15_000;
const PROCESS_EVENT_SUBSCRIBE_TIMEOUT_SECONDS: u64 = 10;
const MAX_RAW_UPLOAD_CHUNK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum TlsMode {
    Off,
    SelfSigned {
        state_dir: PathBuf,
    },
    Files {
        cert_path: PathBuf,
        key_path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub struct TlsConfig {
    pub mode: TlsMode,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self { mode: TlsMode::Off }
    }
}

#[derive(Debug, Clone)]
pub struct ServerLimits {
    pub max_processes: usize,
    pub max_zombie_processes: usize,
    pub default_process_output_limit_bytes: usize,
    pub max_process_output_limit_bytes: usize,
    pub default_process_read_max_bytes: usize,
    pub max_process_read_max_bytes: usize,
    pub max_stdin_write_bytes: usize,
    pub max_process_timeout_seconds: u64,
    pub zombie_ttl_seconds: u64,
    pub default_kill_tree_on_terminate: bool,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_processes: DEFAULT_MAX_PROCESSES,
            max_zombie_processes: DEFAULT_MAX_ZOMBIE_PROCESSES,
            default_process_output_limit_bytes: DEFAULT_PROCESS_OUTPUT_LIMIT_BYTES,
            max_process_output_limit_bytes: MAX_PROCESS_OUTPUT_LIMIT_BYTES,
            default_process_read_max_bytes: DEFAULT_PROCESS_READ_MAX_BYTES,
            max_process_read_max_bytes: MAX_PROCESS_READ_MAX_BYTES,
            max_stdin_write_bytes: DEFAULT_MAX_STDIN_WRITE_BYTES,
            max_process_timeout_seconds: MAX_PROCESS_TIMEOUT_SECONDS,
            zombie_ttl_seconds: DEFAULT_ZOMBIE_TTL_SECONDS,
            default_kill_tree_on_terminate: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerState {
    pub token: String,
    pub allow_roots: Vec<PathBuf>,
    pub limits: ServerLimits,
    pub nvidia_smi_path: Option<PathBuf>,
    pub npu_smi_path: Option<PathBuf>,
    processes: Arc<Mutex<BTreeMap<String, process::ManagedProcess>>>,
    modes: Arc<Mutex<ModeRegistry>>,
    uploads: Arc<Mutex<BTreeMap<String, file::UploadSession>>>,
    sync_sessions: Arc<Mutex<BTreeMap<String, file::SyncSession>>>,
}

impl ServerState {
    pub fn new(token: String, allow_roots: Vec<PathBuf>) -> Self {
        Self::with_limits(token, allow_roots, ServerLimits::default())
    }

    pub fn with_limits(token: String, allow_roots: Vec<PathBuf>, limits: ServerLimits) -> Self {
        Self {
            token,
            allow_roots: allow_roots
                .into_iter()
                .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
                .collect(),
            limits,
            nvidia_smi_path: None,
            npu_smi_path: None,
            processes: Arc::new(Mutex::new(BTreeMap::new())),
            modes: Arc::new(Mutex::new(ModeRegistry::default())),
            uploads: Arc::new(Mutex::new(BTreeMap::new())),
            sync_sessions: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

pub async fn serve(
    listen: String,
    port: u16,
    allow_roots: Vec<PathBuf>,
    token: String,
) -> Result<ExitCode> {
    serve_with_config(
        listen,
        port,
        allow_roots,
        token,
        ServerLimits::default(),
        TlsConfig::default(),
    )
    .await
}

pub async fn serve_with_limits(
    listen: String,
    port: u16,
    allow_roots: Vec<PathBuf>,
    token: String,
    limits: ServerLimits,
) -> Result<ExitCode> {
    serve_with_config(
        listen,
        port,
        allow_roots,
        token,
        limits,
        TlsConfig::default(),
    )
    .await
}

pub async fn serve_with_config(
    listen: String,
    port: u16,
    allow_roots: Vec<PathBuf>,
    token: String,
    limits: ServerLimits,
    tls: TlsConfig,
) -> Result<ExitCode> {
    serve_with_config_and_accelerators(listen, port, allow_roots, token, limits, tls, None, None)
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn serve_with_config_and_accelerators(
    listen: String,
    port: u16,
    allow_roots: Vec<PathBuf>,
    token: String,
    limits: ServerLimits,
    tls: TlsConfig,
    nvidia_smi_path: Option<PathBuf>,
    npu_smi_path: Option<PathBuf>,
) -> Result<ExitCode> {
    let mut state = ServerState::with_limits(token, allow_roots, limits);
    state.nvidia_smi_path = nvidia_smi_path;
    state.npu_smi_path = npu_smi_path;
    let state = Arc::new(state);
    process::spawn_maintenance_task(Arc::clone(&state));

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/sync-run", post(sync_run))
        .route("/v1/sync/session/init", post(sync_session_init))
        .route("/v1/sync/session/status", post(sync_session_status))
        .route("/v1/sync/session/release", post(sync_session_release))
        .route("/v1/mode/get", post(mode_get))
        .route("/v1/mode/switch", post(mode_switch))
        .route("/v1/lease/renew", post(lease_renew))
        .route("/v1/lease/release", post(lease_release))
        .route("/v1/accelerator/status", post(accelerator_status))
        .route("/v1/process/start", post(process_start))
        .route("/v1/process/get", post(process_get))
        .route("/v1/process/list", post(process_list))
        .route("/v1/process/read", post(process_read))
        .route("/v1/events", get(process_events))
        .route("/v1/process/write", post(process_write))
        .route("/v1/process/terminate", post(process_terminate))
        .route("/v1/process/cleanup", post(process_cleanup))
        .route("/v1/file/read", post(file_read))
        .route("/v1/file/stat", post(file_stat))
        .route("/v1/file/write", post(file_write))
        .route("/v1/file/upload/init", post(file_upload_init))
        .route("/v1/file/upload/chunk", post(file_upload_chunk))
        .route(
            "/v1/file/upload/chunk/raw/{upload_id}/{offset}",
            post(file_upload_chunk_raw).layer(DefaultBodyLimit::max(MAX_RAW_UPLOAD_CHUNK_BYTES)),
        )
        .route("/v1/file/upload/status", post(file_upload_status))
        .route("/v1/file/upload/finish", post(file_upload_finish))
        .route("/v1/file/upload/abort", post(file_upload_abort))
        .route("/v1/file/delete", post(file_delete))
        .route("/v1/file/find", post(file_find))
        .route("/v1/file/list", post(file_list))
        .with_state(state);

    let addr: SocketAddr = format!("{listen}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address {listen}:{port}"))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    match tls.mode {
        TlsMode::Off => {
            println!("agentplane server listening on http://{addr}");
            axum::serve(listener, app).await?;
        }
        TlsMode::SelfSigned { state_dir } => {
            let (cert_path, key_path) = ensure_self_signed_tls_assets(&state_dir, &listen)?;
            let config = RustlsConfig::from_pem_file(cert_path, key_path).await?;
            drop(listener);
            println!("agentplane server listening on https://{addr}");
            axum_server::bind_rustls(addr, config)
                .serve(app.into_make_service())
                .await?;
        }
        TlsMode::Files {
            cert_path,
            key_path,
        } => {
            let config = RustlsConfig::from_pem_file(cert_path, key_path).await?;
            drop(listener);
            println!("agentplane server listening on https://{addr}");
            axum_server::bind_rustls(addr, config)
                .serve(app.into_make_service())
                .await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn ensure_self_signed_tls_assets(state_dir: &Path, _listen: &str) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create tls state dir {}", state_dir.display()))?;
    let cert_path = state_dir.join("server.crt.pem");
    let key_path = state_dir.join("server.key.pem");
    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();
    std::fs::write(&cert_path, cert_pem)
        .with_context(|| format!("failed to write tls cert {}", cert_path.display()))?;
    std::fs::write(&key_path, key_pem)
        .with_context(|| format!("failed to write tls key {}", key_path.display()))?;
    Ok((cert_path, key_path))
}

async fn healthz() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(SimpleResponse {
            ok: true,
            error: None,
        }),
    )
}

async fn sync_run(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<SyncPayload>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    let execution_lease = if payload.command.is_some() {
        match validated_execution_lease(&state, &headers).await {
            Ok(execution_lease) => execution_lease,
            Err(error) => return bad_request_response(error).into_response(),
        }
    } else {
        None
    };

    match file::handle_sync_run_with_lease(&state, payload, execution_lease.as_ref()).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn sync_session_init(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<SyncSessionInitRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_sync_session_init(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn sync_session_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<SyncSessionStatusRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_sync_session_status(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn sync_session_release(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<SyncSessionReleaseRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_sync_session_release(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn mode_get(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(_payload): Json<ModeGetRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match handle_mode_get(&state).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn mode_switch(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ModeSwitchRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match handle_mode_switch(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn lease_renew(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<LeaseRenewRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match handle_lease_renew(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn lease_release(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<LeaseReleaseRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match handle_lease_release(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn accelerator_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<AcceleratorStatusRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match handle_accelerator_status(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_start(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessStartRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    let execution_lease = match validated_execution_lease(&state, &headers).await {
        Ok(execution_lease) => execution_lease,
        Err(error) => return bad_request_response(error).into_response(),
    };
    match process::handle_process_start_with_lease(&state, payload, execution_lease.as_ref()).await
    {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_read(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessReadRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match process::handle_process_read(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_events(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    websocket
        .on_upgrade(move |socket| process_event_stream(state, socket))
        .into_response()
}

async fn process_event_stream(state: Arc<ServerState>, mut socket: WebSocket) {
    let first_message = tokio::time::timeout(
        std::time::Duration::from_secs(PROCESS_EVENT_SUBSCRIBE_TIMEOUT_SECONDS),
        socket.recv(),
    )
    .await;
    let subscription = match first_message {
        Ok(Some(Ok(Message::Text(text)))) => {
            serde_json::from_str::<ProcessEventSubscribeRequest>(&text)
        }
        _ => return,
    };
    let subscription = match subscription {
        Ok(subscription) => subscription,
        Err(error) => {
            let _ = send_process_event(
                &mut socket,
                &ProcessEventMessage::Error {
                    error: format!("invalid process event subscription: {error}"),
                },
            )
            .await;
            return;
        }
    };
    let mut after_seq = subscription.after_seq;

    loop {
        let response = handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: subscription.process_id.clone(),
                after_seq,
                max_bytes: subscription.max_bytes,
                wait_ms: Some(PROCESS_EVENT_WAIT_MS),
            },
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let _ = send_process_event(
                    &mut socket,
                    &ProcessEventMessage::Error {
                        error: error.to_string(),
                    },
                )
                .await;
                return;
            }
        };
        after_seq = Some(response.next_seq);
        let exited = response.exited;
        if send_process_event(&mut socket, &ProcessEventMessage::Read { response })
            .await
            .is_err()
        {
            return;
        }
        if exited {
            return;
        }
    }
}

async fn send_process_event(socket: &mut WebSocket, event: &ProcessEventMessage) -> Result<()> {
    let text = serde_json::to_string(event)?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("failed to send process event")
}

async fn process_get(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessGetRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match process::handle_process_get(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessListRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match process::handle_process_list(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_write(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessWriteRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match process::handle_process_write(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_terminate(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessTerminateRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match process::handle_process_terminate(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn process_cleanup(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<ProcessCleanupRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match process::handle_process_cleanup(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_read(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileReadRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_read(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_stat(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileStatRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_stat(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_write(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileWriteRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_write(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_upload_init(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileUploadInitRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_upload_init(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_upload_chunk(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileUploadChunkRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_upload_chunk(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_upload_chunk_raw(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath((upload_id, offset)): AxumPath<(String, u64)>,
    body: Bytes,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
    };
    match file::handle_file_upload_chunk_bytes(
        &state,
        &upload_id,
        offset,
        &body,
        header(HEADER_UPLOAD_CHUNK_SHA256),
        header(HEADER_UPLOAD_SYNC_SESSION_ID),
        header(HEADER_UPLOAD_LOCK_TOKEN),
    )
    .await
    {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_upload_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileUploadStatusRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_upload_status(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_upload_finish(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileUploadFinishRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_upload_finish(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_upload_abort(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileUploadAbortRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_upload_abort(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_delete(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileDeleteRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_delete(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_find(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileFindRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_find(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

async fn file_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<FileListRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return unauthorized_response().into_response();
    }
    match file::handle_file_list(&state, payload).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => bad_request_response(error).into_response(),
    }
}

pub async fn handle_mode_get(state: &ServerState) -> Result<ModeGetResponse> {
    let mut registry = state.modes.lock().await;
    registry.expire_stale_leases();
    Ok(ModeGetResponse {
        ok: true,
        current_mode: registry.current_mode(),
        leases: registry.leases(),
    })
}

pub async fn handle_mode_switch(
    state: &ServerState,
    payload: ModeSwitchRequest,
) -> Result<ModeSwitchResponse> {
    let mut registry = state.modes.lock().await;
    let lease = registry.switch_mode(
        payload.mode,
        payload.task_id,
        payload.lease_id,
        payload.ttl_seconds,
        payload.heartbeat_seconds,
        payload.max_renewals,
    )?;
    Ok(ModeSwitchResponse {
        ok: true,
        current_mode: registry.current_mode(),
        lease,
        leases: registry.leases(),
    })
}

pub async fn handle_lease_renew(
    state: &ServerState,
    payload: LeaseRenewRequest,
) -> Result<LeaseRenewResponse> {
    let mut registry = state.modes.lock().await;
    let lease = registry.renew(&payload.task_id, &payload.lease_id)?;
    Ok(LeaseRenewResponse { ok: true, lease })
}

pub async fn handle_lease_release(
    state: &ServerState,
    payload: LeaseReleaseRequest,
) -> Result<LeaseReleaseResponse> {
    let mut registry = state.modes.lock().await;
    let lease = registry.release(&payload.task_id, &payload.lease_id)?;
    Ok(LeaseReleaseResponse { ok: true, lease })
}
