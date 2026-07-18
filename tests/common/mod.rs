#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agentplane::protocol::{ProcessGetRequest, ProcessOutputChunk, ProcessOutputStream};
pub use anyhow::Result;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::oneshot;

pub fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(repo).status()?;
    anyhow::ensure!(status.success(), "git {:?} failed", args);
    Ok(())
}

pub fn init_repo() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), &["init"])?;
    git(dir.path(), &["config", "user.email", "test@example.com"])?;
    git(dir.path(), &["config", "user.name", "Test User"])?;
    Ok(dir)
}

pub fn build_binary() -> Result<std::path::PathBuf> {
    let binary_name = if cfg!(windows) {
        "agentplane.exe"
    } else {
        "agentplane"
    };
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    let candidate = target_dir.join("debug").join(binary_name);
    anyhow::ensure!(
        candidate.exists(),
        "test binary does not exist yet: {}",
        candidate.display()
    );
    Ok(candidate)
}

pub fn find_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

pub struct CliServerHarness {
    pub process: std::process::Child,
    pub base_url: String,
    pub ca_cert_path: Option<std::path::PathBuf>,
}

impl CliServerHarness {
    pub fn start(allow_root: &Path, token: &str) -> Result<Self> {
        Self::start_with_args(allow_root, token, &[])
    }

    pub fn start_with_args(allow_root: &Path, token: &str, extra_args: &[&str]) -> Result<Self> {
        Self::start_with_args_tls(allow_root, token, extra_args, false)
    }

    pub fn start_with_args_tls(
        allow_root: &Path,
        token: &str,
        extra_args: &[&str],
        tls: bool,
    ) -> Result<Self> {
        let binary = build_binary()?;
        let mut last_error = None;
        for _ in 0..10 {
            let port = find_free_port()?;
            let mut command = Command::new(&binary);
            command.args([
                "server",
                "--listen",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--allow-root",
                &allow_root.display().to_string(),
                "--token",
                token,
            ]);
            if tls {
                command.args([
                    "--tls-mode",
                    "self-signed",
                    "--tls-state-dir",
                    &allow_root.join(".agentplane-tls").display().to_string(),
                ]);
            }
            command.args(extra_args);
            let process = command.spawn()?;
            let ca_cert_path = if tls {
                Some(allow_root.join(".agentplane-tls/server.crt.pem"))
            } else {
                None
            };
            let harness = Self {
                process,
                base_url: if tls {
                    format!("https://127.0.0.1:{port}")
                } else {
                    format!("http://127.0.0.1:{port}")
                },
                ca_cert_path,
            };
            match wait_for_health(&harness.base_url) {
                Ok(()) => return Ok(harness),
                Err(error) => {
                    last_error = Some(error.to_string());
                    drop(harness);
                }
            }
        }
        anyhow::bail!(
            "server did not become healthy after retries: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )
    }
}

impl Drop for CliServerHarness {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

pub fn run_cli(args: &[&str]) -> Result<std::process::Output> {
    let binary = build_binary()?;
    Ok(Command::new(binary).args(args).output()?)
}

pub fn run_cli_with_env(args: &[&str], env: &[(&str, &str)]) -> Result<std::process::Output> {
    let binary = build_binary()?;
    Ok(Command::new(binary)
        .args(args)
        .envs(env.iter().copied())
        .output()?)
}

pub fn wait_for_health(base_url: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let output = run_cli(&["health", "--server", base_url, "--tls-insecure-skip-verify"])?;
        if output.status.success() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("server did not become healthy: {base_url}");
}

#[derive(Clone)]
pub struct HeaderGateState {
    pub expected_name: String,
    pub expected_value: String,
}

pub async fn gated_health(
    State(state): State<HeaderGateState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    if header_matches(&headers, &state.expected_name, &state.expected_value) {
        (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "error": null})),
        )
    } else {
        (
            StatusCode::FOUND,
            Json(serde_json::json!({"ok": false, "error": "missing gateway header"})),
        )
    }
}

pub async fn gated_process_list(
    State(state): State<HeaderGateState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    if !header_matches(&headers, &state.expected_name, &state.expected_value) {
        return (
            StatusCode::FOUND,
            Json(serde_json::json!({"ok": false, "error": "missing gateway header"})),
        );
    }
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer test-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "processes": []})),
    )
}

pub async fn gated_file_list_gzip(
    State(state): State<HeaderGateState>,
    headers: HeaderMap,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    if !header_matches(&headers, &state.expected_name, &state.expected_value) {
        let mut plain_headers = HeaderMap::new();
        plain_headers.insert(
            "content-type",
            "application/json".parse().expect("content-type"),
        );
        return (
            StatusCode::FOUND,
            plain_headers,
            br#"{"ok":false,"error":"missing gateway header"}"#.to_vec(),
        );
    }

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(
        &mut gz,
        br#"{"ok":true,"entries":[{"path":"README.md","is_dir":false}]}"#,
    )
    .expect("write gzip body");
    let body = gz.finish().expect("finish gzip body");

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "content-type",
        "application/json".parse().expect("content-type"),
    );
    response_headers.insert(
        "content-encoding",
        "gzip".parse().expect("content-encoding"),
    );
    (StatusCode::OK, response_headers, body)
}

pub fn header_matches(headers: &HeaderMap, expected_name: &str, expected_value: &str) -> bool {
    headers
        .get(expected_name)
        .and_then(|value| value.to_str().ok())
        == Some(expected_value)
}

pub struct HeaderGateHarness {
    pub base_url: String,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl HeaderGateHarness {
    pub fn start(expected_name: &str, expected_value: &str) -> Result<Self> {
        let port = find_free_port()?;
        let base_url = format!("http://127.0.0.1:{port}");
        let state = HeaderGateState {
            expected_name: expected_name.to_string(),
            expected_value: expected_value.to_string(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .expect("bind header gate");
                let app = Router::new()
                    .route("/healthz", get(gated_health))
                    .route("/v1/process/list", post(gated_process_list))
                    .route("/v1/file/list", post(gated_file_list_gzip))
                    .with_state(state);
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("serve header gate");
            });
        });
        Ok(Self {
            base_url,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }
}

impl Drop for HeaderGateHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub struct DelayedHealthHarness {
    pub base_url: String,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl DelayedHealthHarness {
    pub fn start(delay: Duration) -> Result<Self> {
        let port = find_free_port()?;
        let base_url = format!("http://127.0.0.1:{port}");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = std::thread::spawn(move || {
            std::thread::sleep(delay);
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .expect("bind delayed health");
                let app = Router::new().route(
                    "/healthz",
                    get(|| async {
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({"ok": true, "error": null})),
                        )
                    }),
                );
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("serve delayed health");
            });
        });
        Ok(Self {
            base_url,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }
}

impl Drop for DelayedHealthHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Clone)]
pub struct FlakyProcessStartState {
    pub remaining_failures: Arc<AtomicUsize>,
    pub request_count: Arc<AtomicUsize>,
}

pub async fn flaky_healthz() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "error": null})),
    )
}

pub async fn flaky_process_start(
    State(state): State<FlakyProcessStartState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    state.request_count.fetch_add(1, Ordering::SeqCst);
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer test-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
        );
    }

    let should_fail = loop {
        let current = state.remaining_failures.load(Ordering::SeqCst);
        if current == 0 {
            break false;
        }
        if state
            .remaining_failures
            .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            break true;
        }
    };

    if should_fail {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"ok": false, "error": "temporary gateway failure"})),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "process_id": "retry-job",
            "created": true,
            "already_exists": false
        })),
    )
}

pub async fn flaky_process_get(
    headers: HeaderMap,
    Json(payload): Json<ProcessGetRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer test-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
        );
    }
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "ok": false,
            "error": format!("unknown process_id: {}", payload.process_id)
        })),
    )
}

pub struct FlakyProcessStartHarness {
    pub base_url: String,
    pub request_count: Arc<AtomicUsize>,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl FlakyProcessStartHarness {
    pub fn start(failures_before_success: usize) -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener
            .set_nonblocking(true)
            .expect("set nonblocking flaky listener");
        let port = listener.local_addr()?.port();
        let base_url = format!("http://127.0.0.1:{port}");
        let state = FlakyProcessStartState {
            remaining_failures: Arc::new(AtomicUsize::new(failures_before_success)),
            request_count: Arc::new(AtomicUsize::new(0)),
        };
        let request_count = Arc::clone(&state.request_count);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("convert flaky process-start listener");
                let app = Router::new()
                    .route("/healthz", get(flaky_healthz))
                    .route("/v1/process/start", post(flaky_process_start))
                    .route("/v1/process/get", post(flaky_process_get))
                    .with_state(state);
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("serve flaky process-start");
            });
        });
        Ok(Self {
            base_url,
            request_count,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }

    pub fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

impl Drop for FlakyProcessStartHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Clone)]
pub struct RecoverableProcessStartState {
    pub start_calls: Arc<AtomicUsize>,
}

pub async fn recoverable_process_start(
    State(state): State<RecoverableProcessStartState>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    state.start_calls.fetch_add(1, Ordering::SeqCst);
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer test-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "process_id": "recover-after-drop",
            "created": false,
            "already_exists": true
        })),
    )
}

pub async fn recoverable_process_get(
    headers: HeaderMap,
    Json(payload): Json<ProcessGetRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer test-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"ok": false, "error": "unauthorized"})),
        );
    }
    if payload.process_id != "recover-after-drop" {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"ok": false, "error": format!("unknown process_id: {}", payload.process_id)}),
            ),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "process": {
                "process_id": "recover-after-drop",
                "remote_root": "/workspace/project",
                "cwd": "/workspace/project",
                "command": ["bash", "-lc", "echo ok"],
                "pipe_stdin": false,
                "kill_tree_on_terminate": false,
                "process_group_id": null,
                "children_running": false,
                "timeout_seconds": null,
                "output_bytes_limit": 1048576,
                "started_at_unix_ms": 1,
                "finished_at_unix_ms": null,
                "exited": false,
                "exit_code": null,
                "failure": null,
                "next_seq": 0,
                "available_from_seq": 0,
                "truncated": false,
                "output_retained": true
            }
        })),
    )
}

pub struct RecoverableProcessStartHarness {
    pub base_url: String,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl RecoverableProcessStartHarness {
    pub fn start() -> Result<Self> {
        let port = find_free_port()?;
        let base_url = format!("http://127.0.0.1:{port}");
        let state = RecoverableProcessStartState {
            start_calls: Arc::new(AtomicUsize::new(0)),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .expect("bind recoverable process-start");
                let app = Router::new()
                    .route("/healthz", get(flaky_healthz))
                    .route("/v1/process/start", post(recoverable_process_start))
                    .route("/v1/process/get", post(recoverable_process_get))
                    .with_state(state);
                axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("serve recoverable process-start");
            });
        });
        Ok(Self {
            base_url,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }
}

impl Drop for RecoverableProcessStartHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub struct DroppedConnectionRecoveryHarness {
    pub base_url: String,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl DroppedConnectionRecoveryHarness {
    pub fn start() -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let base_url = format!("http://127.0.0.1:{port}");
        let recoverable = RecoverableProcessStartHarness::start()?;
        let recover_base_url = recoverable.base_url.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let join = std::thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("set nonblocking listener");
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0u8; 4096];
                        let _ = stream.read(&mut buffer);
                        let request = String::from_utf8_lossy(&buffer);
                        if request.starts_with("GET /healthz ") {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"ok\":true,\"error\":null}",
                            );
                        } else if request.starts_with("POST /v1/process/start ") {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
                            );
                            let _ = stream.flush();
                            drop(stream);
                        } else if request.starts_with("POST /v1/process/get ") {
                            let mut upstream = std::net::TcpStream::connect(
                                recover_base_url.trim_start_matches("http://"),
                            )
                            .expect("connect upstream recoverable harness");
                            upstream
                                .write_all(&buffer)
                                .expect("forward process-get request");
                            let mut response = Vec::new();
                            upstream
                                .read_to_end(&mut response)
                                .expect("read upstream response");
                            let _ = stream.write_all(&response);
                        } else {
                            let _ = stream.write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
            drop(recoverable);
        });
        Ok(Self {
            base_url,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }
}

impl Drop for DroppedConnectionRecoveryHarness {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn decode_b64(value: &str) -> Result<String> {
    Ok(String::from_utf8(BASE64.decode(value.as_bytes())?)?)
}

pub fn decode_process_chunks(
    chunks: &[ProcessOutputChunk],
    stream: ProcessOutputStream,
) -> Result<String> {
    let mut combined = Vec::new();
    for chunk in chunks {
        if chunk.stream == stream {
            combined.extend(BASE64.decode(chunk.data_b64.as_bytes())?);
        }
    }
    Ok(String::from_utf8_lossy(&combined).into_owned())
}

pub fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    anyhow::ensure!(output.status.success(), "git {:?} failed", args);
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

pub fn test_sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

pub fn wait_for_child_exit(child: &mut std::process::Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    anyhow::bail!("child process did not exit")
}

#[cfg(unix)]
pub fn write_mock_executable(path: &Path, body: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
pub fn write_mock_nvidia_smi(path: &Path, body: &str) -> Result<()> {
    write_mock_executable(path, body)
}

#[cfg(unix)]
pub fn wait_for_pid_file(path: &Path) -> Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return Ok(pid);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("pid file did not appear: {}", path.display())
}

#[cfg(unix)]
pub fn assert_process_exits(pid: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let status = Command::new("ps")
            .args(["-p", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    anyhow::bail!("process still alive after tree termination: {pid}")
}

#[cfg(unix)]
pub fn assert_process_running(pid: u32) -> Result<()> {
    let status = Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "process is not running: {pid}");
    Ok(())
}
