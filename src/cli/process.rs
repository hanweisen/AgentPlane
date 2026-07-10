use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::StatusCode;

use crate::cli_client::{
    post_json, post_process_start_with_recovery, print_error_response, process_error_response,
};
use crate::config::{ClientProfile, ResolvedClientAuth, resolve_remote_root};
use crate::protocol::{
    CleanupProcess, ProcessCleanupRequest, ProcessCleanupResponse, ProcessGetRequest,
    ProcessGetResponse, ProcessListResponse, ProcessReadRequest, ProcessReadResponse,
    ProcessStartRequest, ProcessStartResponse, ProcessTerminateRequest, ProcessWriteRequest,
    SimpleResponse, parse_resource_claim_specs,
};

use super::{
    ProcessCleanupArgs, ProcessGetArgs, ProcessListArgs, ProcessReadArgs, ProcessRunArgs,
    ProcessStartArgs, ProcessStatusArgs, ProcessTerminateArgs, ProcessWriteArgs,
};

pub(super) async fn process_start(
    args: ProcessStartArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let env = parse_process_env_pairs(&args.env)?;
    let claims = parse_resource_claim_specs(&args.claims)?;
    let payload = ProcessStartRequest {
        remote_root: remote_root.display().to_string(),
        process_id: args.process_id,
        command: args.command,
        cwd: args.cwd,
        env: Some(env),
        claims,
        timeout_seconds: args.timeout_seconds,
        output_bytes_limit: args.output_bytes_limit,
        save_output_path: args.save_output_path,
        pipe_stdin: args.pipe_stdin,
        kill_tree_on_terminate: args.kill_tree_on_terminate,
    };
    let body = post_process_start_with_recovery(&auth, &payload).await?;
    let body = enrich_start_response(body, &auth);
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(ExitCode::SUCCESS)
}

#[derive(serde::Serialize)]
struct StartResponseWithNextCommands {
    #[serde(flatten)]
    base: ProcessStartResponse,
    next_commands: NextCommands,
}

#[derive(serde::Serialize)]
struct NextCommands {
    status: String,
    read: String,
    terminate: String,
}

fn enrich_start_response(
    body: ProcessStartResponse,
    auth: &ResolvedClientAuth,
) -> StartResponseWithNextCommands {
    let pid = &body.process_id;
    let base = format!(
        "agentplane process-status --server {} --token <token> --process-id {}",
        shell_quote(&auth.server),
        shell_quote(pid),
    );
    let read_cmd = format!(
        "agentplane process-read --server {} --token <token> --process-id {}",
        shell_quote(&auth.server),
        shell_quote(pid),
    );
    let terminate_cmd = format!(
        "agentplane process-terminate --server {} --token <token> --process-id {}",
        shell_quote(&auth.server),
        shell_quote(pid),
    );
    let next_commands = NextCommands {
        status: base,
        read: format!("{read_cmd} --text --follow"),
        terminate: terminate_cmd,
    };
    StartResponseWithNextCommands {
        base: body,
        next_commands,
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '.' || c == ':' || c == '/')
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(super) async fn process_status(
    args: ProcessStatusArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    match args.process_id {
        Some(process_id) => {
            let payload = ProcessGetRequest { process_id };
            let response = post_json(&auth, "/v1/process/get", &payload, true).await?;
            if response.status() == StatusCode::OK {
                let body: ProcessGetResponse = response.json().await?;
                if args.text {
                    print_process_info_text(&body.process);
                } else {
                    println!("{}", serde_json::to_string_pretty(&body)?);
                }
                return Ok(ExitCode::SUCCESS);
            }
            print_error_response(response).await
        }
        None => {
            let response =
                post_json(&auth, "/v1/process/list", &serde_json::json!({}), true).await?;
            if response.status() == StatusCode::OK {
                let body: ProcessListResponse = response.json().await?;
                let mut processes = body.processes;
                processes.sort_by(|a, b| {
                    let a_key = a.last_output_at_unix_ms.unwrap_or(a.started_at_unix_ms);
                    let b_key = b.last_output_at_unix_ms.unwrap_or(b.started_at_unix_ms);
                    b_key.cmp(&a_key)
                });
                processes.truncate(args.limit);
                let output = serde_json::json!({
                    "ok": true,
                    "processes": processes,
                });
                if args.text {
                    for process in &processes {
                        print_process_info_text(process);
                    }
                } else {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                return Ok(ExitCode::SUCCESS);
            }
            print_error_response(response).await
        }
    }
}

fn print_process_info_text(info: &crate::protocol::ProcessInfo) {
    let exit_info = match info.exit_code {
        Some(code) => format!("exit_code={code}"),
        None => "exit_code=-".to_string(),
    };
    let pid = info
        .pid
        .map(|p| p.to_string())
        .unwrap_or_else(|| "-".to_string());
    let last_output = info
        .last_output_at_unix_ms
        .map(|t| t.to_string())
        .unwrap_or_else(|| "-".to_string());
    println!(
        "{:<24} status={:<8} pid={:<7} {} elapsed_ms={} last_output_at_unix_ms={}",
        info.process_id, info.status, pid, exit_info, info.elapsed_ms, last_output
    );
    println!("  cwd={} command={}", info.cwd, info.command.join(" "));
}

pub(super) async fn process_run(args: ProcessRunArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let env = parse_process_env_pairs(&args.env)?;
    let claims = parse_resource_claim_specs(&args.claims)?;
    let save_output_path = args.save_output_path.clone();
    let save_output_hint = save_output_path
        .as_ref()
        .map(|path| remote_root.join(path).display().to_string());
    let start_payload = ProcessStartRequest {
        remote_root: remote_root.display().to_string(),
        process_id: args.process_id.clone(),
        command: args.command,
        cwd: args.cwd,
        env: Some(env),
        claims,
        timeout_seconds: args.timeout_seconds,
        output_bytes_limit: args.output_bytes_limit,
        save_output_path: save_output_path.clone(),
        pipe_stdin: false,
        kill_tree_on_terminate: false,
    };
    let start = post_process_start_with_recovery(&auth, &start_payload).await?;
    if !start.ok {
        println!("{}", serde_json::to_string_pretty(&start)?);
        return Ok(ExitCode::from(1));
    }

    let mut after_seq = None;
    loop {
        let body = read_process_once(
            &auth,
            &args.process_id,
            after_seq,
            args.max_bytes,
            Some(args.wait_ms),
        )
        .await?;
        warn_if_cursor_expired(&body, save_output_hint.as_deref());
        if !args.json {
            dump_process_chunks(&body)?;
        }
        after_seq = Some(body.next_seq);
        let exited = body.exited;
        let exit_code = body.exit_code;
        if exited {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&body)?);
            }
            if exit_code.is_some_and(|code| code != 0)
                && let Some(bytes) = args.tail_on_error
            {
                print_process_tail_on_error(
                    &auth,
                    &args.process_id,
                    body.available_from_seq,
                    bytes,
                )
                .await?;
            }
            return Ok(remote_exit_code_to_local(exit_code));
        }
    }
}

pub(super) async fn process_read(
    args: ProcessReadArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let mut after_seq = args.after_seq;
    if args.tail && after_seq.is_none() {
        after_seq = Some(fetch_process_next_seq(&auth, &args.process_id).await?);
    }

    if args.follow || args.text || args.tail {
        process_read_loop(
            auth,
            args.process_id,
            after_seq,
            args.max_bytes,
            args.wait_ms,
            args.text,
            args.follow,
        )
        .await
    } else {
        let payload = ProcessReadRequest {
            process_id: args.process_id,
            after_seq,
            max_bytes: args.max_bytes,
            wait_ms: args.wait_ms,
        };
        let response = post_json(&auth, "/v1/process/read", &payload, true).await?;
        if response.status() == StatusCode::OK {
            let body: serde_json::Value = response.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
            return Ok(ExitCode::SUCCESS);
        }
        print_error_response(response).await
    }
}

pub(super) async fn process_get(args: ProcessGetArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = ProcessGetRequest {
        process_id: args.process_id,
    };
    let response = post_json(&auth, "/v1/process/get", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn process_list(
    args: ProcessListArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let response = post_json(&auth, "/v1/process/list", &serde_json::json!({}), true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn process_write(
    args: ProcessWriteArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = ProcessWriteRequest {
        process_id: args.process_id,
        data_b64: BASE64.encode(args.data.as_bytes()),
        close_stdin: args.close_stdin,
    };
    let response = post_json(&auth, "/v1/process/write", &payload, false).await?;
    if response.status() == StatusCode::OK {
        let body: SimpleResponse = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn process_terminate(
    args: ProcessTerminateArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let payload = ProcessTerminateRequest {
        process_id: args.process_id,
        tree: args.tree,
    };
    let response = post_json(&auth, "/v1/process/terminate", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: SimpleResponse = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn process_cleanup(
    args: ProcessCleanupArgs,
    profile: &ClientProfile,
) -> Result<ExitCode> {
    if args.kill && args.dry_run {
        bail!("--kill and --dry-run are mutually exclusive");
    }
    if args.kill && args.signal.is_none() {
        bail!("process-cleanup --kill requires --signal TERM or --signal KILL");
    }
    if !args.kill && args.signal.is_some() {
        bail!("--signal is only valid with --kill");
    }
    let auth = args.auth.resolve(profile)?;
    let payload = ProcessCleanupRequest {
        process_match: args.process_match,
        dry_run: args.dry_run || !args.kill,
        kill: args.kill,
        signal: args.signal,
    };
    let response = post_json(&auth, "/v1/process/cleanup", &payload, false).await?;
    if response.status() == StatusCode::OK {
        let body: ProcessCleanupResponse = response.json().await?;
        if args.text && !args.json {
            print_process_cleanup_text(&body)?;
        } else {
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

fn print_process_cleanup_text(body: &ProcessCleanupResponse) -> Result<()> {
    if body.dry_run {
        println!("Dry run: no processes were signaled.");
        println!("Matched processes: {}", body.matched.len());
        for process in &body.matched {
            print_cleanup_process(process);
        }
        if !body.skipped.is_empty() {
            println!("Skipped processes: {}", body.skipped.len());
            for process in &body.skipped {
                print_cleanup_process(process);
            }
        }
        println!("{}", body.agent_hint);
        return Ok(());
    }
    println!(
        "Sent {} to {} process(es).",
        body.signal.as_deref().unwrap_or("signal"),
        body.signaled.len()
    );
    for process in &body.signaled {
        print_cleanup_process(process);
    }
    if !body.skipped.is_empty() {
        println!("Skipped processes: {}", body.skipped.len());
        for process in &body.skipped {
            print_cleanup_process(process);
        }
    }
    println!("{}", body.agent_hint);
    Ok(())
}

fn print_cleanup_process(process: &CleanupProcess) {
    println!(
        "  pid={} ppid={} pgid={} sid={} user={} stat={} cmd={}{}",
        process.pid,
        render_optional_i32(process.ppid),
        render_optional_i32(process.process_group_id),
        render_optional_i32(process.session_id),
        process.user.as_deref().unwrap_or("?"),
        process.stat.as_deref().unwrap_or("?"),
        process.command.as_deref().unwrap_or("?"),
        process
            .skip_reason
            .as_deref()
            .map(|reason| format!(" skip_reason={reason}"))
            .unwrap_or_default()
    );
}

async fn fetch_process_next_seq(auth: &ResolvedClientAuth, process_id: &str) -> Result<u64> {
    let payload = ProcessGetRequest {
        process_id: process_id.to_string(),
    };
    let response = post_json(auth, "/v1/process/get", &payload, true).await?;
    if response.status() != StatusCode::OK {
        return Err(anyhow!("failed to fetch process cursor for {}", process_id));
    }
    let body: serde_json::Value = response.json().await?;
    Ok(body["process"]["next_seq"].as_u64().unwrap_or(0))
}

async fn process_read_loop(
    auth: ResolvedClientAuth,
    process_id: String,
    mut after_seq: Option<u64>,
    max_bytes: Option<usize>,
    wait_ms: Option<u64>,
    text: bool,
    follow: bool,
) -> Result<ExitCode> {
    loop {
        let payload = ProcessReadRequest {
            process_id: process_id.clone(),
            after_seq,
            max_bytes,
            wait_ms,
        };
        let response = post_json(&auth, "/v1/process/read", &payload, true).await?;
        if response.status() != StatusCode::OK {
            return print_error_response(response).await;
        }
        let body: ProcessReadResponse = response.json().await?;
        warn_if_cursor_expired(&body, None);
        if text {
            dump_process_chunks(&body)?;
        } else {
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        after_seq = Some(body.next_seq);
        if !follow || body.exited {
            return Ok(ExitCode::SUCCESS);
        }
    }
}

async fn read_process_once(
    auth: &ResolvedClientAuth,
    process_id: &str,
    after_seq: Option<u64>,
    max_bytes: Option<usize>,
    wait_ms: Option<u64>,
) -> Result<ProcessReadResponse> {
    let payload = ProcessReadRequest {
        process_id: process_id.to_string(),
        after_seq,
        max_bytes,
        wait_ms,
    };
    let response = post_json(auth, "/v1/process/read", &payload, true).await?;
    if response.status() != StatusCode::OK {
        return Err(process_error_response(response).await);
    }
    Ok(response.json().await?)
}

async fn print_process_tail_on_error(
    auth: &ResolvedClientAuth,
    process_id: &str,
    available_from_seq: u64,
    tail_bytes: usize,
) -> Result<()> {
    if tail_bytes == 0 {
        return Ok(());
    }
    let mut after_seq = Some(available_from_seq);
    let max_bytes = Some(tail_bytes.clamp(1, 1024 * 1024));
    let mut retained = Vec::new();
    for _ in 0..10_000 {
        let body = read_process_once(auth, process_id, after_seq, max_bytes, None).await?;
        warn_if_cursor_expired(&body, None);
        append_process_chunk_bytes(&body, &mut retained)?;
        if retained.len() > tail_bytes {
            let remove = retained.len() - tail_bytes;
            retained.drain(..remove);
        }
        let previous = after_seq;
        after_seq = Some(body.next_seq);
        if body.exited || after_seq == previous {
            break;
        }
    }
    if !retained.is_empty() {
        let mut err = io::stderr();
        writeln!(
            err,
            "[agentplane] tail-on-error: last {} retained output bytes follow:",
            retained.len()
        )?;
        err.write_all(&retained)?;
        if !retained.ends_with(b"\n") {
            writeln!(err)?;
        }
        err.flush()?;
    }
    Ok(())
}

fn append_process_chunk_bytes(body: &ProcessReadResponse, target: &mut Vec<u8>) -> Result<()> {
    for chunk in &body.chunks {
        let bytes = BASE64
            .decode(chunk.data_b64.as_bytes())
            .context("failed to decode process output chunk")?;
        target.extend(bytes);
    }
    Ok(())
}

fn dump_process_chunks(body: &ProcessReadResponse) -> Result<()> {
    let mut out = io::stdout();
    let mut err = io::stderr();
    for chunk in &body.chunks {
        let bytes = BASE64
            .decode(chunk.data_b64.as_bytes())
            .context("failed to decode process output chunk")?;
        match chunk.stream {
            crate::protocol::ProcessOutputStream::Stderr => {
                err.write_all(&bytes)?;
                err.flush()?;
            }
            crate::protocol::ProcessOutputStream::Stdout => {
                out.write_all(&bytes)?;
                out.flush()?;
            }
        }
    }
    Ok(())
}

fn warn_if_cursor_expired(body: &ProcessReadResponse, save_output_path: Option<&str>) {
    if body.cursor_expired {
        if let Some(save_output_path) = save_output_path {
            eprintln!(
                "[agentplane] process output cursor expired for {}; resumed at seq {}. Full output was saved to remote path {}.",
                body.process_id, body.available_from_seq, save_output_path
            );
        } else {
            eprintln!(
                "[agentplane] process output cursor expired for {}; resumed at seq {}. Increase --output-bytes-limit or the server process output retention limits to avoid losing earlier logs.",
                body.process_id, body.available_from_seq
            );
        }
    }
}

fn remote_exit_code_to_local(exit_code: Option<i32>) -> ExitCode {
    match exit_code {
        Some(0) => ExitCode::SUCCESS,
        Some(code @ 1..=255) => ExitCode::from(code as u8),
        _ => ExitCode::from(1),
    }
}

fn parse_process_env_pairs(
    values: &[String],
) -> Result<std::collections::BTreeMap<String, Option<String>>> {
    let mut env = std::collections::BTreeMap::new();
    for value in values {
        let Some((key, item)) = value.split_once('=') else {
            return Err(anyhow!("expected KEY=VALUE or KEY=, got: {value}"));
        };
        if key.is_empty() {
            return Err(anyhow!("empty key in env item: {value}"));
        }
        if item.is_empty() {
            env.insert(key.to_string(), None);
        } else {
            env.insert(key.to_string(), Some(item.to_string()));
        }
    }
    Ok(env)
}

fn render_optional_i32(value: Option<i32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string())
}
