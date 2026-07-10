use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

use super::file::{resolve_cwd, resolve_remote_root, safe_join};
use super::util::{parse_i32_field, unix_now_ms};
use super::{ServerLimits, ServerState};
use crate::protocol::{
    CleanupProcess, ProcessCleanupRequest, ProcessCleanupResponse, ProcessGetRequest,
    ProcessGetResponse, ProcessInfo, ProcessListResponse, ProcessOutputChunk, ProcessOutputStream,
    ProcessReadRequest, ProcessReadResponse, ProcessStartConfig, ProcessStartRequest,
    ProcessStartResponse, ProcessTerminateRequest, ProcessWriteRequest, ResourceClaim,
    SimpleResponse, infer_gpu_resource_claims_from_process_env, merge_resource_claims,
};
use crate::server::auth::ExecutionLease;

const MAX_PROCESS_READ_WAIT_MS: u64 = 30_000;
const MAINTENANCE_INTERVAL_MS: u64 = 1000;
#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(unix)]
const SIGKILL: i32 = 9;
#[cfg(not(unix))]
const SIGTERM: i32 = 15;
#[cfg(not(unix))]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

#[derive(Debug, Clone)]
struct OutputChunk {
    seq: u64,
    stream: ProcessOutputStream,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
struct ProcessOutputState {
    chunks: Vec<OutputChunk>,
    next_seq: u64,
    total_bytes: usize,
    truncated: bool,
    open_streams: usize,
    last_output_at_unix_ms: Option<u128>,
}

#[derive(Debug)]
pub(super) struct ManagedProcess {
    process_id: String,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<ProcessOutputState>>,
    remote_root: String,
    cwd: String,
    command: Vec<String>,
    pipe_stdin: bool,
    kill_tree_on_terminate: bool,
    process_group_id: Option<i32>,
    started_at: Instant,
    started_at_unix_ms: u128,
    timeout_seconds: Option<u64>,
    output_bytes_limit: usize,
    save_output_path: Option<String>,
    claimed_resources: Vec<ResourceClaim>,
    exit_code: Option<i32>,
    failure: Option<String>,
    finished_at: Option<Instant>,
    finished_at_unix_ms: Option<u128>,
    pid: Option<i32>,
}

impl ManagedProcess {
    fn start_config(&self) -> ProcessStartConfig<'_> {
        ProcessStartConfig::new(
            &self.remote_root,
            &self.cwd,
            &self.command,
            &self.claimed_resources,
            self.pipe_stdin,
            self.kill_tree_on_terminate,
            self.save_output_path.as_deref(),
            self.timeout_seconds,
            self.output_bytes_limit,
        )
    }
}

async fn scan_cleanup_processes(process_match: &str) -> Result<Vec<CleanupProcess>> {
    let needles = process_match
        .split('|')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if needles.is_empty() {
        bail!("process_match must include at least one non-empty term");
    }

    let mut output = cleanup_ps_output("sid").await?;
    if !output.status.success() {
        let sid_stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let fallback = cleanup_ps_output("sess").await?;
        if fallback.status.success() {
            output = fallback;
        } else {
            bail!(
                "ps failed for process cleanup: sid stderr: {}; sess stderr: {}",
                sid_stderr,
                String::from_utf8_lossy(&fallback.stderr).trim()
            );
        }
    }
    if !output.status.success() {
        bail!(
            "ps failed for process cleanup: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let rows = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    for row in rows.lines() {
        let Some(process) = parse_cleanup_ps_row(row) else {
            continue;
        };
        let command = process
            .command
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if needles.iter().any(|needle| command.contains(needle)) {
            processes.push(process);
        }
    }
    processes.sort_by_key(|process| process.pid);
    Ok(processes)
}

async fn cleanup_ps_output(session_field: &str) -> Result<std::process::Output> {
    Command::new("ps")
        .args([
            "-axo",
            "pid=",
            "-o",
            "ppid=",
            "-o",
            "pgid=",
            "-o",
            &format!("{session_field}="),
            "-o",
            "etime=",
            "-o",
            "stat=",
            "-o",
            "user=",
            "-o",
            "command=",
        ])
        .output()
        .await
        .context("failed to run ps for process cleanup")
}

fn parse_cleanup_ps_row(row: &str) -> Option<CleanupProcess> {
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 7 {
        return None;
    }
    let pid = parse_i32_field(fields[0])?;
    let command = row.split_whitespace().skip(7).collect::<Vec<_>>().join(" ");
    Some(CleanupProcess {
        pid,
        ppid: parse_i32_field(fields[1]),
        process_group_id: parse_i32_field(fields[2]),
        session_id: parse_i32_field(fields[3]),
        elapsed: Some(fields[4].to_string()),
        stat: Some(fields[5].to_string()),
        user: Some(fields[6].to_string()),
        command: if command.is_empty() {
            None
        } else {
            Some(command)
        },
        skip_reason: None,
    })
}

fn parse_cleanup_signal(signal: &str) -> Result<i32> {
    match signal
        .trim()
        .trim_start_matches("SIG")
        .to_ascii_uppercase()
        .as_str()
    {
        "TERM" => Ok(SIGTERM),
        "KILL" => Ok(SIGKILL),
        other => bail!("unsupported cleanup signal: {other}; supported signals: TERM, KILL"),
    }
}

fn send_cleanup_signal(pid: i32, signal: i32) -> Result<()> {
    if pid <= 0 {
        bail!("invalid pid for cleanup: {pid}");
    }
    #[cfg(unix)]
    {
        unsafe {
            if kill(pid, signal) == -1 {
                return Err(std::io::Error::last_os_error()).context("failed to signal process");
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
        bail!("process cleanup signaling is only supported on Unix platforms")
    }
}

pub async fn handle_process_start(
    state: &ServerState,
    payload: ProcessStartRequest,
) -> Result<ProcessStartResponse> {
    handle_process_start_with_lease(state, payload, None).await
}

pub(super) async fn handle_process_start_with_lease(
    state: &ServerState,
    payload: ProcessStartRequest,
    execution_lease: Option<&ExecutionLease>,
) -> Result<ProcessStartResponse> {
    if payload.command.is_empty() {
        bail!("command must not be empty");
    }

    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let cwd = match payload.cwd.as_deref() {
        Some(cwd) => resolve_cwd(&remote_root, cwd)?,
        None => remote_root.clone(),
    };
    tokio::fs::create_dir_all(&cwd).await?;

    let mut processes = state.processes.lock().await;
    prune_finished_processes(&mut processes, &state.limits);
    if let Some(timeout_seconds) = payload.timeout_seconds
        && timeout_seconds > state.limits.max_process_timeout_seconds
    {
        bail!(
            "timeout_seconds exceeds server limit: {} > {}",
            timeout_seconds,
            state.limits.max_process_timeout_seconds
        );
    }
    let requested_cwd = cwd.display().to_string();
    let requested_remote_root = remote_root.display().to_string();
    let requested_output_limit = payload
        .output_bytes_limit
        .unwrap_or(state.limits.default_process_output_limit_bytes)
        .min(state.limits.max_process_output_limit_bytes);
    let requested_kill_tree_on_terminate =
        payload.kill_tree_on_terminate || state.limits.default_kill_tree_on_terminate;
    let requested_save_output_path = payload.save_output_path.clone();
    let requested_claims = if execution_lease.is_some() {
        merge_resource_claims(
            &payload.claims,
            &infer_gpu_resource_claims_from_process_env(payload.env.as_ref()),
        )?
    } else {
        Vec::new()
    };
    if let Some(existing) = processes.get_mut(&payload.process_id) {
        refresh_process_state(state, existing).await?;
        if payload.matches_existing_normalized_config(
            &existing.start_config(),
            &requested_remote_root,
            &requested_cwd,
            &requested_claims,
            requested_output_limit,
            requested_kill_tree_on_terminate,
            requested_save_output_path.as_deref(),
        ) {
            if let Some(execution_lease) = execution_lease
                && !requested_claims.is_empty()
                && process_still_holds_runtime(existing)
            {
                let mut modes = state.modes.lock().await;
                modes.claim_resources(
                    &execution_lease.task_id,
                    &execution_lease.lease_id,
                    &payload.process_id,
                    &requested_claims,
                )?;
            }
            return Ok(ProcessStartResponse {
                ok: true,
                process_id: payload.process_id,
                created: false,
                already_exists: true,
            });
        }
        bail!(
            "process_id already exists with different process configuration: {}",
            payload.process_id
        );
    }
    enforce_process_capacity(&processes, &state.limits)?;
    let save_output_file =
        prepare_save_output_file(&remote_root, requested_save_output_path.as_deref()).await?;
    let reserved_resource_claims = if let Some(execution_lease) = execution_lease {
        let mut modes = state.modes.lock().await;
        modes.claim_resources(
            &execution_lease.task_id,
            &execution_lease.lease_id,
            &payload.process_id,
            &requested_claims,
        )?;
        !requested_claims.is_empty()
    } else {
        false
    };

    let mut command = Command::new(&payload.command[0]);
    if payload.command.len() > 1 {
        command.args(&payload.command[1..]);
    }
    command.current_dir(cwd);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    if payload.pipe_stdin {
        command.stdin(std::process::Stdio::piped());
    }
    configure_process_group(&mut command, requested_kill_tree_on_terminate)?;

    if let Some(env) = payload.env {
        for (key, value) in env {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
    }

    let mut child = match command.spawn().context("failed to spawn process") {
        Ok(child) => child,
        Err(error) => {
            if reserved_resource_claims {
                let mut modes = state.modes.lock().await;
                modes.release_process_resource_claims(&payload.process_id);
            }
            return Err(error);
        }
    };
    let process_group_id = process_group_id(&child, requested_kill_tree_on_terminate);
    let pid = process_group_id.or_else(|| child.id().and_then(|p| i32::try_from(p).ok()));
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let output = Arc::new(Mutex::new(ProcessOutputState::default()));
    let output_limit = requested_output_limit;
    if let Some(stdout) = stdout {
        {
            let mut state = output.lock().await;
            state.open_streams += 1;
        }
        spawn_reader(
            stdout,
            Arc::clone(&output),
            ProcessOutputStream::Stdout,
            output_limit,
            save_output_file.clone(),
        );
    }
    if let Some(stderr) = stderr {
        {
            let mut state = output.lock().await;
            state.open_streams += 1;
        }
        spawn_reader(
            stderr,
            Arc::clone(&output),
            ProcessOutputStream::Stderr,
            output_limit,
            save_output_file.clone(),
        );
    }
    processes.insert(
        payload.process_id.clone(),
        ManagedProcess {
            process_id: payload.process_id.clone(),
            child: Some(child),
            stdin,
            output,
            remote_root: requested_remote_root,
            cwd: requested_cwd,
            command: payload.command.clone(),
            pipe_stdin: payload.pipe_stdin,
            kill_tree_on_terminate: requested_kill_tree_on_terminate,
            process_group_id,
            started_at: Instant::now(),
            started_at_unix_ms: unix_now_ms(),
            timeout_seconds: payload.timeout_seconds,
            output_bytes_limit: output_limit,
            save_output_path: requested_save_output_path,
            claimed_resources: requested_claims,
            exit_code: None,
            failure: None,
            finished_at: None,
            finished_at_unix_ms: None,
            pid,
        },
    );

    Ok(ProcessStartResponse {
        ok: true,
        process_id: payload.process_id,
        created: true,
        already_exists: false,
    })
}

pub async fn handle_process_read(
    state: &ServerState,
    payload: ProcessReadRequest,
) -> Result<ProcessReadResponse> {
    let after_seq = payload.after_seq.unwrap_or(0);
    let max_bytes = payload
        .max_bytes
        .unwrap_or(state.limits.default_process_read_max_bytes)
        .clamp(1, state.limits.max_process_read_max_bytes);
    let wait_ms = payload.wait_ms.unwrap_or(0).min(MAX_PROCESS_READ_WAIT_MS);
    let deadline = Instant::now() + Duration::from_millis(wait_ms);

    loop {
        let snapshot =
            read_process_snapshot(state, &payload.process_id, after_seq, max_bytes).await?;
        if !snapshot.chunks.is_empty() || snapshot.exited || wait_ms == 0 {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Ok(snapshot);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn handle_process_get(
    state: &ServerState,
    payload: ProcessGetRequest,
) -> Result<ProcessGetResponse> {
    let process = snapshot_process_info(state, &payload.process_id).await?;
    Ok(ProcessGetResponse { ok: true, process })
}

pub async fn handle_process_list(state: &ServerState) -> Result<ProcessListResponse> {
    let process_ids = {
        let mut processes = state.processes.lock().await;
        prune_finished_processes(&mut processes, &state.limits);
        processes.keys().cloned().collect::<Vec<_>>()
    };
    let mut processes = Vec::with_capacity(process_ids.len());
    for process_id in process_ids {
        processes.push(snapshot_process_info(state, &process_id).await?);
    }
    processes.sort_by(|a, b| a.process_id.cmp(&b.process_id));
    Ok(ProcessListResponse {
        ok: true,
        processes,
    })
}

pub async fn handle_process_write(
    state: &ServerState,
    payload: ProcessWriteRequest,
) -> Result<SimpleResponse> {
    let mut processes = state.processes.lock().await;
    let process = processes
        .get_mut(&payload.process_id)
        .ok_or_else(|| anyhow!("unknown process_id: {}", payload.process_id))?;
    refresh_process_state(state, process).await?;
    if process.exit_code.is_some() {
        bail!(
            "process already exited for process_id: {}",
            payload.process_id
        );
    }

    let stdin = process.stdin.as_mut().ok_or_else(|| {
        anyhow!(
            "stdin is not available for process_id: {}",
            payload.process_id
        )
    })?;
    let bytes = BASE64
        .decode(payload.data_b64.as_bytes())
        .context("failed to decode write payload")?;
    if bytes.len() > state.limits.max_stdin_write_bytes {
        bail!(
            "stdin write exceeds server limit: {} > {}",
            bytes.len(),
            state.limits.max_stdin_write_bytes
        );
    }
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    if payload.close_stdin {
        let _ = process.stdin.take();
    }
    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

pub async fn handle_process_terminate(
    state: &ServerState,
    payload: ProcessTerminateRequest,
) -> Result<SimpleResponse> {
    let mut processes = state.processes.lock().await;
    let process = processes
        .get_mut(&payload.process_id)
        .ok_or_else(|| anyhow!("unknown process_id: {}", payload.process_id))?;
    refresh_process_state(state, process).await?;
    if process.exit_code.is_some() {
        if payload.tree || process.kill_tree_on_terminate {
            terminate_process(process, true).await;
        }
        return Ok(SimpleResponse {
            ok: true,
            error: None,
        });
    }
    terminate_process(process, payload.tree).await;
    finish_process(
        state,
        process,
        1,
        Some("process terminated by client".to_string()),
    )
    .await;
    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

pub async fn handle_process_cleanup(
    state: &ServerState,
    payload: ProcessCleanupRequest,
) -> Result<ProcessCleanupResponse> {
    let process_match = payload.process_match.trim();
    if process_match.is_empty() {
        bail!("process_match must not be empty");
    }
    if payload.kill && payload.dry_run {
        bail!("--kill and --dry-run are mutually exclusive");
    }
    if payload.kill && payload.signal.is_none() {
        bail!("process cleanup kill requires an explicit signal");
    }
    let signal = match payload.signal.as_deref() {
        Some(value) => Some(parse_cleanup_signal(value)?),
        None => None,
    };
    if !payload.kill && signal.is_some() {
        bail!("--signal is only valid with --kill");
    }

    let current_pid = std::process::id() as i32;
    let mut matched = scan_cleanup_processes(process_match).await?;
    let mut skipped = Vec::new();
    matched.retain(|process| {
        if let Some(skip_reason) = cleanup_skip_reason(process, current_pid) {
            let mut skipped_process = process.clone();
            skipped_process.skip_reason = Some(skip_reason.to_string());
            skipped.push(skipped_process);
            false
        } else {
            true
        }
    });

    let dry_run = !payload.kill;
    let mut signaled = Vec::new();
    if payload.kill {
        let signal = signal.expect("validated signal");
        for process in &matched {
            send_cleanup_signal(process.pid, signal)?;
            signaled.push(process.clone());
        }
    }

    // Optional accelerator occupancy summary on dry-run (feedback §6). Built
    // only when requested and only for dry-run, so a --kill never pays the smi
    // cost and never blocks the signal path.
    let accelerator_summary = if dry_run {
        if let Some(kind) = payload.accelerator_summary {
            let pids: Vec<i32> = matched.iter().map(|process| process.pid).collect();
            Some(super::accelerator::accelerator_process_occupancy(state, kind, &pids).await)
        } else {
            None
        }
    } else {
        None
    };

    Ok(ProcessCleanupResponse {
        ok: true,
        dry_run,
        signal: payload.signal.map(|signal| signal.to_ascii_uppercase()),
        matched,
        signaled,
        skipped,
        agent_hint: if dry_run {
            "Dry run only. No process was signaled. To clean up, rerun with --kill --signal TERM after reviewing matched processes.".to_string()
        } else {
            "Explicit cleanup signal was sent only to matched processes; review process status before sending stronger signals.".to_string()
        },
        accelerator_summary,
    })
}

fn cleanup_skip_reason(process: &CleanupProcess, current_pid: i32) -> Option<&'static str> {
    if process.pid == current_pid {
        return Some("AgentPlane server process");
    }
    let command = process.command.as_deref()?.to_ascii_lowercase();
    if command.contains("agentplane") && command.contains("process-cleanup") {
        return Some("AgentPlane cleanup client process");
    }
    None
}

async fn read_process_snapshot(
    state: &ServerState,
    process_id: &str,
    after_seq: u64,
    max_bytes: usize,
) -> Result<ProcessReadResponse> {
    let (output, exit_code, failure) = {
        let mut processes = state.processes.lock().await;
        prune_finished_processes(&mut processes, &state.limits);
        let process = processes
            .get_mut(process_id)
            .ok_or_else(|| anyhow!("unknown process_id: {process_id}"))?;
        refresh_process_state(state, process).await?;
        (
            Arc::clone(&process.output),
            process.exit_code,
            process.failure.clone(),
        )
    };

    let output = output.lock().await;
    let available_from_seq = output
        .chunks
        .first()
        .map(|chunk| chunk.seq)
        .unwrap_or(output.next_seq);
    let cursor_expired = after_seq < available_from_seq;
    let effective_after = if cursor_expired {
        available_from_seq
    } else {
        after_seq
    };
    let mut chunks = Vec::new();
    let mut used_bytes = 0usize;
    for chunk in output
        .chunks
        .iter()
        .filter(|chunk| chunk.seq >= effective_after)
    {
        let chunk_len = chunk.data.len();
        if !chunks.is_empty() && used_bytes + chunk_len > max_bytes {
            break;
        }
        chunks.push(ProcessOutputChunk {
            seq: chunk.seq,
            stream: chunk.stream.clone(),
            data_b64: BASE64.encode(&chunk.data),
        });
        used_bytes += chunk_len;
        if used_bytes >= max_bytes {
            break;
        }
    }
    let next_seq = chunks
        .last()
        .map(|chunk| chunk.seq + 1)
        .unwrap_or(effective_after);
    let output_drained = output.open_streams == 0;
    let delivered_all_retained_output = next_seq >= output.next_seq;

    Ok(ProcessReadResponse {
        ok: true,
        process_id: process_id.to_string(),
        chunks,
        next_seq,
        available_from_seq,
        cursor_expired,
        exited: exit_code.is_some() && output_drained && delivered_all_retained_output,
        exit_code,
        truncated: output.truncated,
        failure,
    })
}

async fn snapshot_process_info(state: &ServerState, process_id: &str) -> Result<ProcessInfo> {
    let (
        remote_root,
        cwd,
        command,
        pipe_stdin,
        kill_tree_on_terminate,
        process_group_id,
        timeout_seconds,
        output_bytes_limit,
        started_at_unix_ms,
        finished_at_unix_ms,
        exit_code,
        failure,
        children_running,
        pid,
        save_output_path,
        output,
    ) = {
        let mut processes = state.processes.lock().await;
        prune_finished_processes(&mut processes, &state.limits);
        let process = processes
            .get_mut(process_id)
            .ok_or_else(|| anyhow!("unknown process_id: {process_id}"))?;
        refresh_process_state(state, process).await?;
        (
            process.remote_root.clone(),
            process.cwd.clone(),
            process.command.clone(),
            process.pipe_stdin,
            process.kill_tree_on_terminate,
            process.process_group_id,
            process.timeout_seconds,
            process.output_bytes_limit,
            process.started_at_unix_ms,
            process.finished_at_unix_ms,
            process.exit_code,
            process.failure.clone(),
            process_children_running(process),
            process.pid,
            process.save_output_path.clone(),
            Arc::clone(&process.output),
        )
    };
    let now = unix_now_ms();
    let elapsed_ms = finished_at_unix_ms
        .unwrap_or(now)
        .saturating_sub(started_at_unix_ms);
    let status = compute_process_status(exit_code, failure.as_deref());
    let output = output.lock().await;
    let available_from_seq = output
        .chunks
        .first()
        .map(|chunk| chunk.seq)
        .unwrap_or(output.next_seq);
    let last_output_at_unix_ms = output.last_output_at_unix_ms;
    Ok(ProcessInfo {
        process_id: process_id.to_string(),
        remote_root,
        cwd,
        command,
        pipe_stdin,
        kill_tree_on_terminate,
        process_group_id,
        children_running,
        timeout_seconds,
        output_bytes_limit,
        started_at_unix_ms,
        finished_at_unix_ms,
        exited: exit_code.is_some(),
        exit_code,
        failure,
        next_seq: output.next_seq,
        available_from_seq,
        truncated: output.truncated,
        output_retained: true,
        status,
        pid,
        elapsed_ms,
        last_output_at_unix_ms,
        save_output_path,
    })
}

fn compute_process_status(exit_code: Option<i32>, failure: Option<&str>) -> String {
    if exit_code.is_some() {
        if exit_code == Some(0) && failure.is_none() {
            "exited".to_string()
        } else {
            "failed".to_string()
        }
    } else {
        "running".to_string()
    }
}

pub(super) fn spawn_maintenance_task(state: Arc<ServerState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(MAINTENANCE_INTERVAL_MS)).await;
            let mut processes = state.processes.lock().await;
            prune_finished_processes(&mut processes, &state.limits);
            for process in processes.values_mut() {
                let _ = refresh_process_state(&state, process).await;
            }
        }
    });
}

async fn refresh_process_state(state: &ServerState, process: &mut ManagedProcess) -> Result<()> {
    if process.exit_code.is_some() {
        maybe_release_process_resource_claims(state, process).await;
        process.child = None;
        process.stdin = None;
        return Ok(());
    }

    if let Some(timeout_seconds) = process.timeout_seconds
        && process.started_at.elapsed() >= Duration::from_secs(timeout_seconds)
    {
        terminate_process(process, process.kill_tree_on_terminate).await;
        finish_process(
            state,
            process,
            124,
            Some(format!("process timed out after {timeout_seconds} seconds")),
        )
        .await;
        return Ok(());
    }

    let Some(child) = process.child.as_mut() else {
        finish_process(
            state,
            process,
            1,
            Some("process state lost child handle unexpectedly".to_string()),
        )
        .await;
        return Ok(());
    };

    if let Some(status) = child.try_wait().context("failed to poll child status")? {
        finish_process(state, process, status.code().unwrap_or(1), None).await;
    }
    Ok(())
}

fn configure_process_group(command: &mut Command, enabled: bool) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    #[cfg(unix)]
    {
        unsafe {
            command.pre_exec(|| {
                if setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        bail!("--kill-tree-on-terminate is only supported on Unix platforms")
    }
}

fn process_group_id(child: &Child, enabled: bool) -> Option<i32> {
    if !enabled {
        return None;
    }
    #[cfg(unix)]
    {
        child.id().and_then(|pid| i32::try_from(pid).ok())
    }
    #[cfg(not(unix))]
    {
        let _ = child;
        None
    }
}

async fn terminate_process(process: &mut ManagedProcess, tree: bool) {
    let use_tree = tree || process.kill_tree_on_terminate;
    if use_tree && let Some(process_group_id) = process.process_group_id {
        terminate_process_group(process_group_id, SIGTERM);
        if let Some(child) = process.child.as_mut() {
            if matches!(
                timeout(Duration::from_millis(500), child.wait()).await,
                Ok(Ok(_))
            ) && !process_group_alive(process_group_id)
            {
                return;
            }
        } else {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !process_group_alive(process_group_id) {
                return;
            }
        }
        terminate_process_group(process_group_id, SIGKILL);
    }
    if let Some(child) = process.child.as_mut() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[cfg(unix)]
fn terminate_process_group(process_group_id: i32, signal: i32) {
    if process_group_id <= 0 {
        return;
    }
    unsafe {
        let _ = kill(-process_group_id, signal);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_process_group_id: i32, _signal: i32) {}

fn process_children_running(process: &ManagedProcess) -> bool {
    if process.exit_code.is_none() {
        return false;
    }
    process.process_group_id.is_some_and(process_group_alive)
}

fn process_still_holds_runtime(process: &ManagedProcess) -> bool {
    process.exit_code.is_none() || process_children_running(process)
}

#[cfg(unix)]
fn process_group_alive(process_group_id: i32) -> bool {
    if process_group_id <= 0 {
        return false;
    }
    unsafe { kill(-process_group_id, 0) == 0 }
}

#[cfg(not(unix))]
fn process_group_alive(_process_group_id: i32) -> bool {
    false
}

fn mark_process_finished(process: &mut ManagedProcess, exit_code: i32, failure: Option<String>) {
    if process.exit_code.is_some() {
        process.child = None;
        process.stdin = None;
        return;
    }
    process.exit_code = Some(exit_code);
    process.failure = failure;
    process.finished_at = Some(Instant::now());
    process.finished_at_unix_ms = Some(unix_now_ms());
    process.child = None;
    process.stdin = None;
}

async fn finish_process(
    state: &ServerState,
    process: &mut ManagedProcess,
    exit_code: i32,
    failure: Option<String>,
) {
    mark_process_finished(process, exit_code, failure);
    maybe_release_process_resource_claims(state, process).await;
}

async fn release_process_resource_claims(state: &ServerState, process: &mut ManagedProcess) {
    if process.claimed_resources.is_empty() {
        return;
    }
    let mut modes = state.modes.lock().await;
    modes.release_process_resource_claims(&process.process_id);
    process.claimed_resources.clear();
}

async fn maybe_release_process_resource_claims(state: &ServerState, process: &mut ManagedProcess) {
    if process.claimed_resources.is_empty() {
        return;
    }
    if process.exit_code.is_some() && process_children_running(process) {
        return;
    }
    release_process_resource_claims(state, process).await;
}

fn prune_finished_processes(
    processes: &mut BTreeMap<String, ManagedProcess>,
    limits: &ServerLimits,
) {
    let now = Instant::now();
    processes.retain(|_, process| {
        process
            .finished_at
            .map(|finished_at| {
                now.duration_since(finished_at).as_secs() < limits.zombie_ttl_seconds
            })
            .unwrap_or(true)
    });

    let finished_count = processes
        .values()
        .filter(|process| process.finished_at.is_some())
        .count();
    if finished_count <= limits.max_zombie_processes {
        return;
    }

    let mut finished = processes
        .iter()
        .filter_map(|(process_id, process)| {
            process
                .finished_at
                .map(|finished_at| (process_id.clone(), finished_at))
        })
        .collect::<Vec<_>>();
    finished.sort_by_key(|(_, finished_at)| *finished_at);
    let remove_count = finished_count - limits.max_zombie_processes;
    for (process_id, _) in finished.into_iter().take(remove_count) {
        let _ = processes.remove(&process_id);
    }
}

fn enforce_process_capacity(
    processes: &BTreeMap<String, ManagedProcess>,
    limits: &ServerLimits,
) -> Result<()> {
    let running = processes
        .values()
        .filter(|process| process.finished_at.is_none())
        .count();
    if running >= limits.max_processes {
        bail!(
            "running process count exceeds server limit: {} >= {}",
            running,
            limits.max_processes
        );
    }
    Ok(())
}

async fn prepare_save_output_file(
    remote_root: &Path,
    save_output_path: Option<&str>,
) -> Result<Option<PathBuf>> {
    let Some(save_output_path) = save_output_path else {
        return Ok(None);
    };
    let path = safe_join(remote_root, save_output_path)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .await
        .with_context(|| format!("failed to create save output file: {}", path.display()))?;
    Ok(Some(path))
}

async fn append_saved_output(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(data).await?;
    file.flush().await?;
    Ok(())
}

fn spawn_reader<R>(
    mut reader: R,
    output: Arc<Mutex<ProcessOutputState>>,
    stream: ProcessOutputStream,
    output_limit: usize,
    save_output_path: Option<PathBuf>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let _reader_id = Uuid::new_v4();
    tokio::spawn(async move {
        let mut local = [0u8; 4096];
        loop {
            match reader.read(&mut local).await {
                Ok(0) => break,
                Ok(n) => {
                    let data = local[..n].to_vec();
                    if let Some(path) = save_output_path.as_deref()
                        && let Err(error) = append_saved_output(path, &data).await
                    {
                        eprintln!(
                            "[agentplane] failed to append process output to {}: {error:#}",
                            path.display()
                        );
                    }
                    let mut target = output.lock().await;
                    target.total_bytes += data.len();
                    target.last_output_at_unix_ms = Some(unix_now_ms());
                    let seq = target.next_seq;
                    target.next_seq += 1;
                    target.chunks.push(OutputChunk {
                        seq,
                        stream: stream.clone(),
                        data,
                    });
                    if target.total_bytes > output_limit {
                        target.truncated = true;
                    }
                    while target.total_bytes > output_limit && target.chunks.len() > 1 {
                        if let Some(removed) = target.chunks.first().cloned() {
                            target.total_bytes =
                                target.total_bytes.saturating_sub(removed.data.len());
                            target.chunks.remove(0);
                            target.truncated = true;
                        } else {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let mut target = output.lock().await;
        target.open_streams = target.open_streams.saturating_sub(1);
    });
}
