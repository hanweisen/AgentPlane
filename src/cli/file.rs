use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::cli_client::{
    build_http_client, normalize_server_url, post_json, post_json_with_client,
    print_error_response, wrap_request_error,
};
use crate::config::{
    ClientProfile, ResolvedClientAuth, load_client_profile, resolve_profile_auth,
    resolve_remote_root,
};
use crate::protocol::{
    FileDeleteRequest, FileFindRequest, FileListRequest, FileReadRequest, FileReadResponse,
    FileStatRequest, FileStatResponse, FileUploadChunkRequest, FileUploadChunkResponse,
    FileUploadFinishRequest, FileUploadInitRequest, FileUploadInitResponse,
    FileUploadStatusRequest, FileUploadStatusResponse, FileWriteRequest,
    HEADER_UPLOAD_CHUNK_SHA256, HEADER_UPLOAD_LOCK_TOKEN, HEADER_UPLOAD_SYNC_SESSION_ID,
    ProcessGetRequest, SimpleResponse,
};

use super::sync_session::{acquire_sync_session, release_sync_session};
use super::{
    FileCopyArgs, FileDeleteArgs, FileFindArgs, FileListArgs, FileReadArgs, FileStatArgs,
    FileUploadArgs, FileWaitArgs, FileWriteArgs, UploadTransportArg, upload_transport_from_env,
};

const FILE_WAIT_POLL_MS: u64 = 100;

pub(super) async fn file_read(args: FileReadArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let payload = FileReadRequest {
        remote_root: remote_root.display().to_string(),
        path: args.path,
    };
    let response = post_json(&auth, "/v1/file/read", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: FileReadResponse = response.json().await?;
        if args.text {
            let bytes = BASE64
                .decode(body.content_b64.as_bytes())
                .context("failed to decode file content")?;
            io::stdout().write_all(&bytes)?;
            io::stdout().flush()?;
        } else {
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn file_stat(args: FileStatArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let payload = FileStatRequest {
        remote_root: remote_root.display().to_string(),
        path: args.path,
    };
    match read_file_stat(&auth, &payload).await? {
        Ok(body) => {
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(ExitCode::SUCCESS)
        }
        Err(response) => print_error_response(response).await,
    }
}

pub(super) async fn file_wait(args: FileWaitArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let payload = FileStatRequest {
        remote_root: remote_root.display().to_string(),
        path: args.path,
    };
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    let mut stable_size = None;
    let mut stable_since = None;

    loop {
        let stat = match read_file_stat(&auth, &payload).await? {
            Ok(body) => body,
            Err(response) => return print_error_response(response).await,
        };
        if stat_ready(&stat, args.min_bytes) {
            if let Some(stable_ms) = args.stable_ms {
                if stable_size != stat.size {
                    stable_size = stat.size;
                    stable_since = Some(Instant::now());
                }
                if stable_since
                    .is_some_and(|since| since.elapsed() >= Duration::from_millis(stable_ms))
                {
                    println!("{}", serde_json::to_string_pretty(&stat)?);
                    return Ok(ExitCode::SUCCESS);
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&stat)?);
                return Ok(ExitCode::SUCCESS);
            }
        } else {
            stable_size = None;
            stable_since = None;
        }

        let now = Instant::now();
        if now >= deadline {
            let producer = file_wait_producer_hint(&auth, args.process_id.as_deref()).await;
            eprintln!(
                "file-wait timed out after {} seconds; last observed state:",
                args.timeout_seconds
            );
            eprintln!("{}", serde_json::to_string_pretty(&stat)?);
            eprintln!(
                "hint: path {} exists={} size={} modified_unix_ms={}; {producer}",
                payload.path,
                stat.exists,
                stat.size.unwrap_or(0),
                stat.modified_unix_ms.unwrap_or(0),
            );
            return Ok(ExitCode::from(1));
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(FILE_WAIT_POLL_MS))).await;
    }
}

/// One-line summary of the producer process for the file-wait timeout hint.
/// When no `--process-id` is supplied, nudge the caller to provide one; when one
/// is supplied, report whether it is still alive so the agent can tell a dead
/// producer from a slow one.
async fn file_wait_producer_hint(auth: &ResolvedClientAuth, process_id: Option<&str>) -> String {
    let Some(process_id) = process_id else {
        return "pass --process-id to report whether the producer is still alive".to_string();
    };
    let payload = ProcessGetRequest {
        process_id: process_id.to_string(),
    };
    let response = match post_json(auth, "/v1/process/get", &payload, true).await {
        Ok(response) => response,
        Err(error) => {
            return format!("producer '{process_id}' status=unknown (failed to probe: {error:#})");
        }
    };
    if response.status() != StatusCode::OK {
        return format!("producer '{process_id}' not found (no longer running)");
    }
    match response.json::<serde_json::Value>().await {
        Ok(body) => {
            let status = body["process"]["status"].as_str().unwrap_or("unknown");
            let exited = body["process"]["exited"].as_bool().unwrap_or(false);
            match body["process"]["exit_code"].as_i64() {
                Some(code) => {
                    format!(
                        "producer '{process_id}' status={status} exited={exited} exit_code={code}"
                    )
                }
                None => format!("producer '{process_id}' status={status} exited={exited}"),
            }
        }
        Err(_) => format!("producer '{process_id}' status=unknown (could not decode response)"),
    }
}

pub(super) async fn file_write(args: FileWriteArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let (content, local_executable, local_mode) = if let Some(path) = &args.from_local {
        (
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?,
            is_local_executable(path)?,
            local_file_mode(path)?,
        )
    } else if let Some(content) = &args.content {
        (content.as_bytes().to_vec(), false, None)
    } else {
        bail!("either --content or --from-local is required");
    };
    let mode = args.mode.or_else(|| {
        if args.from_local.is_some() && args.preserve_mode {
            local_mode
        } else {
            None
        }
    });
    let payload = FileWriteRequest {
        remote_root: remote_root.display().to_string(),
        path: args.path,
        content_b64: BASE64.encode(&content),
        executable: args.executable || local_executable,
        create_parents: args.create_parents,
        atomic: args.atomic,
        mode,
        preserve_mode: args.preserve_mode && mode.is_none(),
        checksum_sha256: args.checksum_sha256,
    };
    let response = post_json(&auth, "/v1/file/write", &payload, false).await?;
    if response.status() == StatusCode::OK {
        let body: SimpleResponse = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn file_upload(args: FileUploadArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let mut transport = upload_transport_from_env()?;
    let auth = args.auth.resolve(profile)?;
    let client = build_http_client(&auth)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    if args.chunk_size == 0 {
        bail!("--chunk-size must be greater than zero");
    }

    let local_metadata = std::fs::metadata(&args.from_local)
        .with_context(|| format!("failed to stat {}", args.from_local.display()))?;
    if !local_metadata.is_file() {
        bail!("--from-local must point to a regular file");
    }
    let local_executable = is_local_executable(&args.from_local)?;
    let local_mode = local_file_mode(&args.from_local)?;
    let local_checksum = hash_local_file_sha256(&args.from_local)?;
    if let Some(expected_checksum) = args.checksum_sha256.as_deref() {
        let expected_checksum = normalize_sha256(expected_checksum);
        validate_sha256(&expected_checksum)?;
        if !local_checksum.eq_ignore_ascii_case(&expected_checksum) {
            bail!(
                "local file checksum mismatch for {}",
                args.from_local.display()
            );
        }
    }

    let mode = args
        .mode
        .or(if args.preserve_mode { local_mode } else { None });
    let remote_root_string = remote_root.display().to_string();
    let sync_session = if let Some(lock_key) = args.lock_key.as_deref() {
        Some(acquire_sync_session(&auth, &remote_root_string, Some(lock_key)).await?)
    } else {
        None
    };

    let upload_result = async {
        let init_payload = FileUploadInitRequest {
            remote_root: remote_root_string.clone(),
            path: args.path,
            total_size: local_metadata.len(),
            chunk_size: u64::try_from(args.chunk_size).context("chunk size is too large")?,
            executable: args.executable || local_executable,
            create_parents: args.create_parents,
            atomic: args.atomic,
            mode,
            preserve_mode: args.preserve_mode && mode.is_none(),
            checksum_sha256: local_checksum.clone(),
            resume: args.resume,
            sync_session_id: sync_session
                .as_ref()
                .map(|session| session.sync_session_id.clone()),
            lock_token: sync_session
                .as_ref()
                .map(|session| session.lock_token.clone()),
        };
        let init = init_upload(&client, &auth, &init_payload).await?;
        let mut file = tokio::fs::File::open(&args.from_local)
            .await
            .with_context(|| format!("failed to open {}", args.from_local.display()))?;
        file.seek(std::io::SeekFrom::Start(init.received_bytes))
            .await?;

        let mut offset = init.received_bytes;
        let mut buffer = vec![0_u8; args.chunk_size];
        while offset < init.total_size {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                bail!(
                    "local file ended early for {} at offset {}",
                    args.from_local.display(),
                    offset
                );
            }
            let chunk = &buffer[..read];
            let next_offset = offset
                .checked_add(u64::try_from(read).context("chunk read overflow")?)
                .ok_or_else(|| anyhow::anyhow!("upload offset overflow"))?;
            let response = upload_chunk_with_recovery(
                &client,
                &auth,
                &init.upload_id,
                offset,
                chunk,
                sync_session
                    .as_ref()
                    .map(|session| session.sync_session_id.as_str()),
                sync_session
                    .as_ref()
                    .map(|session| session.lock_token.as_str()),
                &mut transport,
                next_offset,
            )
            .await?;
            offset = response.received_bytes;
        }

        let response = post_json_with_client(
            &client,
            &auth,
            "/v1/file/upload/finish",
            &FileUploadFinishRequest {
                upload_id: init.upload_id,
                sync_session_id: sync_session
                    .as_ref()
                    .map(|session| session.sync_session_id.clone()),
                lock_token: sync_session
                    .as_ref()
                    .map(|session| session.lock_token.clone()),
            },
            false,
        )
        .await?;
        if response.status() == StatusCode::OK {
            let body: SimpleResponse = response.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
            return Ok(ExitCode::SUCCESS);
        }
        print_error_response(response).await
    }
    .await;

    if let Some(session) = sync_session
        && let Err(error) = release_sync_session(&auth, &session).await
    {
        eprintln!("warning: failed to release file upload lock: {error:#}");
    }
    upload_result
}

pub(super) async fn file_copy(args: FileCopyArgs, _profile: &ClientProfile) -> Result<ExitCode> {
    let transport = upload_transport_from_env()?;
    if args.chunk_size == 0 {
        bail!("--chunk-size must be greater than zero");
    }
    let from_profile = load_client_profile(Some(&args.from_profile))?;
    let to_profile = load_client_profile(Some(&args.to_profile))?;
    let from_auth = resolve_profile_auth(&from_profile)?;
    let to_auth = resolve_profile_auth(&to_profile)?;
    let from_root = resolve_remote_root(args.from_remote_root.as_ref(), &from_profile)?;
    let to_root = resolve_remote_root(args.to_remote_root.as_ref(), &to_profile)?;
    let from_root_string = from_root.display().to_string();
    let to_root_string = to_root.display().to_string();

    // Pull the source file. file-read returns the whole file base64-encoded in
    // one response, so very large sources are bounded by that transport; the
    // destination side still uploads in chunks to stay friendly to gateways.
    let from_client = build_http_client(&from_auth)?;
    let read_payload = FileReadRequest {
        remote_root: from_root_string,
        path: args.from_path.clone(),
    };
    let read_response = post_json_with_client(
        &from_client,
        &from_auth,
        "/v1/file/read",
        &read_payload,
        true,
    )
    .await?;
    if read_response.status() != StatusCode::OK {
        return print_error_response(read_response).await;
    }
    let read_body: FileReadResponse = read_response.json().await?;
    let bytes = BASE64
        .decode(read_body.content_b64.as_bytes())
        .context("failed to decode source file content")?;
    let source_checksum = sha256_hex(&bytes);

    // Push to the destination through the existing chunked upload transport.
    // upload_bytes verifies the content checksum before the first chunk, so the
    // transfer itself is integrity-checked; create_parents lets the to-path
    // land in a directory that does not exist yet.
    let to_client = build_http_client(&to_auth)?;
    let options = UploadBytesOptions {
        chunk_size: args.chunk_size,
        transport,
        resume: false,
        executable: false,
        create_parents: true,
        atomic: args.atomic,
        mode: None,
        preserve_mode: false,
        checksum_sha256: source_checksum.clone(),
        sync_session_id: None,
        lock_token: None,
    };
    upload_bytes(
        &to_client,
        &to_auth,
        &to_root_string,
        &args.to_path,
        &bytes,
        &options,
    )
    .await?;

    let mut summary = serde_json::json!({
        "ok": true,
        "from_path": args.from_path,
        "to_path": args.to_path,
        "bytes": bytes.len(),
        "sha256": source_checksum,
    });

    if args.checksum {
        let stat_payload = FileStatRequest {
            remote_root: to_root_string,
            path: args.to_path.clone(),
        };
        match read_file_stat(&to_auth, &stat_payload).await? {
            Ok(stat) => {
                let verified = stat
                    .sha256
                    .as_deref()
                    .map(|sha| sha.eq_ignore_ascii_case(&source_checksum))
                    .unwrap_or(false);
                summary["checksum_verified"] = serde_json::json!(verified);
                if !verified {
                    summary["ok"] = serde_json::json!(false);
                    eprintln!("{}", serde_json::to_string_pretty(&summary)?);
                    return Ok(ExitCode::from(1));
                }
            }
            Err(response) => return print_error_response(response).await,
        }
    }

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(ExitCode::SUCCESS)
}

pub(super) struct UploadBytesOptions {
    pub chunk_size: usize,
    pub transport: UploadTransportArg,
    pub resume: bool,
    pub executable: bool,
    pub create_parents: bool,
    pub atomic: bool,
    pub mode: Option<u32>,
    pub preserve_mode: bool,
    pub checksum_sha256: String,
    pub sync_session_id: Option<String>,
    pub lock_token: Option<String>,
}

pub(super) async fn upload_bytes(
    client: &reqwest::Client,
    auth: &ResolvedClientAuth,
    remote_root: &str,
    path: &str,
    content: &[u8],
    options: &UploadBytesOptions,
) -> Result<()> {
    if options.chunk_size == 0 {
        bail!("upload chunk size must be greater than zero");
    }
    let checksum = normalize_sha256(&options.checksum_sha256);
    validate_sha256(&checksum)?;
    let actual_checksum = sha256_hex(content);
    if !actual_checksum.eq_ignore_ascii_case(&checksum) {
        bail!("content checksum mismatch for sync upload {}", path);
    }

    let init_payload = FileUploadInitRequest {
        remote_root: remote_root.to_string(),
        path: path.to_string(),
        total_size: u64::try_from(content.len()).context("upload content is too large")?,
        chunk_size: u64::try_from(options.chunk_size).context("chunk size is too large")?,
        executable: options.executable,
        create_parents: options.create_parents,
        atomic: options.atomic,
        mode: options.mode,
        preserve_mode: options.preserve_mode,
        checksum_sha256: checksum,
        resume: options.resume,
        sync_session_id: options.sync_session_id.clone(),
        lock_token: options.lock_token.clone(),
    };
    let init = init_upload(client, auth, &init_payload).await?;
    let mut transport = options.transport;

    let mut offset = usize::try_from(init.received_bytes).context("upload offset is too large")?;
    while offset < content.len() {
        let next_offset = offset.saturating_add(options.chunk_size).min(content.len());
        let chunk = &content[offset..next_offset];
        let response = upload_chunk_with_recovery(
            client,
            auth,
            &init.upload_id,
            u64::try_from(offset).context("upload offset is too large")?,
            chunk,
            options.sync_session_id.as_deref(),
            options.lock_token.as_deref(),
            &mut transport,
            u64::try_from(next_offset).context("upload offset is too large")?,
        )
        .await?;
        offset = usize::try_from(response.received_bytes).context("upload offset is too large")?;
    }

    let response = post_json_with_client(
        client,
        auth,
        "/v1/file/upload/finish",
        &FileUploadFinishRequest {
            upload_id: init.upload_id,
            sync_session_id: options.sync_session_id.clone(),
            lock_token: options.lock_token.clone(),
        },
        false,
    )
    .await?;
    if response.status() == StatusCode::OK {
        let _body: SimpleResponse = response.json().await?;
        return Ok(());
    }
    Err(anyhow::anyhow!(response.text().await?))
}

pub(super) async fn file_delete(args: FileDeleteArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let payload = FileDeleteRequest {
        remote_root: remote_root.display().to_string(),
        path: args.path,
    };
    let response = post_json(&auth, "/v1/file/delete", &payload, false).await?;
    if response.status() == StatusCode::OK {
        let body: SimpleResponse = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn file_find(args: FileFindArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let payload = FileFindRequest {
        remote_root: remote_root.display().to_string(),
        pattern: args.pattern,
    };
    let response = post_json(&auth, "/v1/file/find", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

pub(super) async fn file_list(args: FileListArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let payload = FileListRequest {
        remote_root: remote_root.display().to_string(),
        path: args.path,
    };
    let response = post_json(&auth, "/v1/file/list", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_error_response(response).await
}

async fn read_file_stat(
    auth: &ResolvedClientAuth,
    payload: &FileStatRequest,
) -> Result<std::result::Result<FileStatResponse, reqwest::Response>> {
    let response = post_json(auth, "/v1/file/stat", payload, true).await?;
    if response.status() == StatusCode::OK {
        let body = response.json().await?;
        Ok(Ok(body))
    } else {
        Ok(Err(response))
    }
}

async fn init_upload(
    client: &reqwest::Client,
    auth: &ResolvedClientAuth,
    payload: &FileUploadInitRequest,
) -> Result<FileUploadInitResponse> {
    let response =
        post_json_with_client(client, auth, "/v1/file/upload/init", payload, false).await?;
    if response.status() == StatusCode::OK {
        return Ok(response.json().await?);
    }
    Err(anyhow::anyhow!(response.text().await?))
}

#[allow(clippy::too_many_arguments)]
async fn upload_chunk_with_recovery(
    client: &reqwest::Client,
    auth: &ResolvedClientAuth,
    upload_id: &str,
    offset: u64,
    data: &[u8],
    sync_session_id: Option<&str>,
    lock_token: Option<&str>,
    transport: &mut UploadTransportArg,
    expected_next_offset: u64,
) -> Result<FileUploadChunkResponse> {
    let max_attempts = auth.connect_retries.saturating_add(1);
    let mut last_error = None;
    let checksum = sha256_hex(data);
    let mut attempt = 0;
    while attempt < max_attempts {
        let used_auto_binary = *transport == UploadTransportArg::Auto;
        let response = match *transport {
            UploadTransportArg::Auto | UploadTransportArg::Binary => {
                post_raw_upload_chunk(
                    client,
                    auth,
                    upload_id,
                    offset,
                    data,
                    &checksum,
                    sync_session_id,
                    lock_token,
                )
                .await
            }
            UploadTransportArg::Json => {
                post_json_with_client(
                    client,
                    auth,
                    "/v1/file/upload/chunk",
                    &FileUploadChunkRequest {
                        upload_id: upload_id.to_string(),
                        offset,
                        data_b64: BASE64.encode(data),
                        chunk_checksum_sha256: Some(checksum.clone()),
                        sync_session_id: sync_session_id.map(ToOwned::to_owned),
                        lock_token: lock_token.map(ToOwned::to_owned),
                    },
                    false,
                )
                .await
            }
        };
        match response {
            Ok(response) => {
                if response.status() == StatusCode::OK {
                    let body: FileUploadChunkResponse = response.json().await?;
                    if body.received_bytes != expected_next_offset {
                        bail!(
                            "upload chunk advanced to unexpected offset {}",
                            body.received_bytes
                        );
                    }
                    if used_auto_binary {
                        *transport = UploadTransportArg::Binary;
                    }
                    return Ok(body);
                }
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                last_error = Some(anyhow::anyhow!(
                    "upload chunk failed with status {status}: {text}"
                ));
                if used_auto_binary && status == StatusCode::PAYLOAD_TOO_LARGE {
                    return Err(last_error.take().expect("raw upload error was recorded"));
                }
            }
            Err(error) => {
                last_error = Some(error);
            }
        }

        let status = read_upload_status(
            client,
            auth,
            &FileUploadStatusRequest {
                upload_id: upload_id.to_string(),
                sync_session_id: sync_session_id.map(ToOwned::to_owned),
                lock_token: lock_token.map(ToOwned::to_owned),
            },
        )
        .await;
        if let Ok(status) = status {
            if status.received_bytes == expected_next_offset {
                return Ok(FileUploadChunkResponse {
                    ok: true,
                    upload_id: status.upload_id,
                    received_bytes: status.received_bytes,
                });
            }
            if status.received_bytes > offset && status.received_bytes < expected_next_offset {
                bail!(
                    "upload resumed at unexpected intermediate offset {}",
                    status.received_bytes
                );
            }
        }

        if used_auto_binary {
            *transport = UploadTransportArg::Json;
            continue;
        }

        attempt += 1;
        if attempt < max_attempts {
            tokio::time::sleep(Duration::from_millis(auth.connect_retry_delay_ms)).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("upload chunk failed")))
}

#[allow(clippy::too_many_arguments)]
async fn post_raw_upload_chunk(
    client: &reqwest::Client,
    auth: &ResolvedClientAuth,
    upload_id: &str,
    offset: u64,
    data: &[u8],
    checksum: &str,
    sync_session_id: Option<&str>,
    lock_token: Option<&str>,
) -> Result<reqwest::Response> {
    let url = format!(
        "{}/v1/file/upload/chunk/raw/{upload_id}/{offset}",
        normalize_server_url(&auth.server)
    );
    let mut request = client
        .post(url)
        .header(AUTHORIZATION, format!("Bearer {}", auth.token))
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(HEADER_UPLOAD_CHUNK_SHA256, checksum)
        .body(data.to_vec());
    if let Some(sync_session_id) = sync_session_id {
        request = request.header(HEADER_UPLOAD_SYNC_SESSION_ID, sync_session_id);
    }
    if let Some(lock_token) = lock_token {
        request = request.header(HEADER_UPLOAD_LOCK_TOKEN, lock_token);
    }
    request
        .send()
        .await
        .map_err(|error| wrap_request_error(error, &auth.server, auth.socks5_hostname.as_deref()))
}

async fn read_upload_status(
    client: &reqwest::Client,
    auth: &ResolvedClientAuth,
    payload: &FileUploadStatusRequest,
) -> Result<FileUploadStatusResponse> {
    let response =
        post_json_with_client(client, auth, "/v1/file/upload/status", payload, true).await?;
    if response.status() == StatusCode::OK {
        return Ok(response.json().await?);
    }
    Err(anyhow::anyhow!(response.text().await?))
}

fn stat_ready(stat: &FileStatResponse, min_bytes: Option<u64>) -> bool {
    if !stat.exists {
        return false;
    }
    if let Some(min_bytes) = min_bytes {
        return stat.size.is_some_and(|size| size >= min_bytes);
    }
    true
}

fn hash_local_file_sha256(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("checksum must be a sha256 hex digest or sha256:<hex>");
    }
    Ok(())
}

fn normalize_sha256(value: &str) -> String {
    value.strip_prefix("sha256:").unwrap_or(value).to_string()
}

fn sha256_hex(content: &[u8]) -> String {
    hex_digest(Sha256::digest(content).as_slice())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn is_local_executable(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        Ok(metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}

fn local_file_mode(path: &Path) -> Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        Ok(Some(metadata.permissions().mode() & 0o7777))
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;

    #[derive(Clone, Default)]
    struct LegacyUploadState {
        raw_calls: Arc<AtomicUsize>,
        json_calls: Arc<AtomicUsize>,
    }

    #[tokio::test]
    async fn auto_upload_falls_back_to_json_when_raw_route_is_missing() -> Result<()> {
        let state = LegacyUploadState::default();
        let observed = state.clone();
        let app = Router::new()
            .route(
                "/v1/file/upload/chunk/raw/{upload_id}/{offset}",
                post(|State(state): State<LegacyUploadState>| async move {
                    state.raw_calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NOT_FOUND
                }),
            )
            .route(
                "/v1/file/upload/status",
                post(|| async {
                    Json(FileUploadStatusResponse {
                        ok: true,
                        upload_id: "legacy-upload".to_string(),
                        received_bytes: 0,
                        total_size: 3,
                        path: "legacy.bin".to_string(),
                    })
                }),
            )
            .route(
                "/v1/file/upload/chunk",
                post(
                    |State(state): State<LegacyUploadState>,
                     Json(payload): Json<FileUploadChunkRequest>| async move {
                        state.json_calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(payload.upload_id, "legacy-upload");
                        assert_eq!(payload.offset, 0);
                        assert_eq!(payload.data_b64, BASE64.encode(b"abc"));
                        Json(FileUploadChunkResponse {
                            ok: true,
                            upload_id: payload.upload_id,
                            received_bytes: 3,
                        })
                    },
                ),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let auth = ResolvedClientAuth {
            server: format!("http://{address}"),
            token: "test-token".to_string(),
            socks5_hostname: None,
            request_timeout_seconds: 5,
            connect_retries: 0,
            connect_retry_delay_ms: 0,
            tls_ca_cert: None,
            tls_insecure_skip_verify: false,
            header: Vec::new(),
            agent_id: "test-agent".to_string(),
        };
        let client = build_http_client(&auth)?;
        let mut transport = UploadTransportArg::Auto;

        let response = upload_chunk_with_recovery(
            &client,
            &auth,
            "legacy-upload",
            0,
            b"abc",
            None,
            None,
            &mut transport,
            3,
        )
        .await?;
        server.abort();

        assert_eq!(response.received_bytes, 3);
        assert_eq!(transport, UploadTransportArg::Json);
        assert_eq!(observed.raw_calls.load(Ordering::SeqCst), 1);
        assert_eq!(observed.json_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn auto_upload_does_not_expand_raw_413_into_json() -> Result<()> {
        let state = LegacyUploadState::default();
        let observed = state.clone();
        let app = Router::new()
            .route(
                "/v1/file/upload/chunk/raw/{upload_id}/{offset}",
                post(|State(state): State<LegacyUploadState>| async move {
                    state.raw_calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::PAYLOAD_TOO_LARGE
                }),
            )
            .route(
                "/v1/file/upload/chunk",
                post(|State(state): State<LegacyUploadState>| async move {
                    state.json_calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let auth = ResolvedClientAuth {
            server: format!("http://{address}"),
            token: "test-token".to_string(),
            socks5_hostname: None,
            request_timeout_seconds: 5,
            connect_retries: 0,
            connect_retry_delay_ms: 0,
            tls_ca_cert: None,
            tls_insecure_skip_verify: false,
            header: Vec::new(),
            agent_id: "test-agent".to_string(),
        };
        let client = build_http_client(&auth)?;
        let mut transport = UploadTransportArg::Auto;

        let error = upload_chunk_with_recovery(
            &client,
            &auth,
            "oversized-upload",
            0,
            b"abc",
            None,
            None,
            &mut transport,
            3,
        )
        .await
        .expect_err("raw 413 must not fall back to larger JSON/base64");
        server.abort();

        assert!(error.to_string().contains("413"));
        assert_eq!(observed.raw_calls.load(Ordering::SeqCst), 1);
        assert_eq!(observed.json_calls.load(Ordering::SeqCst), 0);
        Ok(())
    }
}
