use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

use super::ServerState;
use super::auth::ExecutionLease;
use crate::protocol::{
    CommandResult, FileDeleteRequest, FileFindRequest, FileFindResponse, FileListEntry,
    FileListRequest, FileListResponse, FileReadRequest, FileReadResponse, FileStatRequest,
    FileStatResponse, FileUploadChunkRequest, FileUploadChunkResponse, FileUploadFinishRequest,
    FileUploadInitRequest, FileUploadInitResponse, FileUploadStatusRequest,
    FileUploadStatusResponse, FileWrite, FileWriteRequest, ResourceClaim, SimpleResponse, SyncMode,
    SyncPayload, SyncReport, SyncResponse, SyncSessionInitRequest, SyncSessionInitResponse,
    SyncSessionReleaseRequest, SyncSessionStatusRequest, infer_gpu_resource_claims_from_sync_env,
    merge_resource_claims, relative_path_matches_preserve_path,
};

const DEFAULT_SYNC_RUN_OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_SYNC_SESSION_TTL_SECONDS: u64 = 300;
const DEFAULT_SYNC_SESSION_HEARTBEAT_SECONDS: u64 = 30;

#[derive(Debug, Clone)]
pub struct UploadSession {
    pub upload_id: String,
    remote_root: String,
    path: String,
    target_path: PathBuf,
    staging_path: PathBuf,
    total_size: u64,
    chunk_size: u64,
    checksum_sha256: String,
    create_parents: bool,
    atomic: bool,
    mode: Option<u32>,
    executable: bool,
    preserve_mode: bool,
    sync_session_id: Option<String>,
    lock_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SyncSession {
    pub sync_session_id: String,
    lock_token: String,
    agent_id: String,
    remote_root: PathBuf,
    lock_key: String,
    ttl_seconds: u64,
    expires_at: SystemTime,
}

pub async fn handle_sync_run(state: &ServerState, payload: SyncPayload) -> Result<SyncResponse> {
    handle_sync_run_with_lease(state, payload, None).await
}

pub async fn handle_sync_run_with_lease(
    state: &ServerState,
    payload: SyncPayload,
    execution_lease: Option<&ExecutionLease>,
) -> Result<SyncResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    validate_optional_sync_session(
        state,
        &remote_root,
        payload.sync_session_id.as_deref(),
        payload.lock_token.as_deref(),
    )
    .await?;
    tokio::fs::create_dir_all(&remote_root).await?;
    let report = match payload.sync_mode {
        SyncMode::WorktreeDelta => {
            apply_changes(
                &remote_root,
                &payload.writes,
                &payload.deletes,
                SyncWriteOptions::from_payload(&payload),
            )
            .await?
        }
        SyncMode::RefSnapshot => {
            apply_snapshot(
                &remote_root,
                &payload.writes,
                &payload.preserve_paths,
                SyncWriteOptions::from_payload(&payload),
            )
            .await?
        }
    };
    let write_count = report.created.len() + report.updated.len();
    let delete_count = report.deleted.len();
    let result = run_command(
        state,
        &remote_root,
        payload.command.clone(),
        payload.timeout_seconds,
        payload.env.unwrap_or_default(),
        payload.claims,
        execution_lease,
    )
    .await?;

    Ok(SyncResponse {
        ok: result.exit_code == 0,
        remote_root: remote_root.display().to_string(),
        write_count,
        delete_count,
        report,
        source_ref: payload.source_ref,
        preserve_paths: payload.preserve_paths,
        result,
    })
}

pub async fn handle_sync_session_init(
    state: &ServerState,
    payload: SyncSessionInitRequest,
) -> Result<SyncSessionInitResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let lock_key = payload
        .lock_key
        .clone()
        .unwrap_or_else(|| remote_root.display().to_string());
    let ttl_seconds = payload
        .ttl_seconds
        .unwrap_or(DEFAULT_SYNC_SESSION_TTL_SECONDS)
        .max(1);
    let now = SystemTime::now();
    let mut sessions = state.sync_sessions.lock().await;
    purge_expired_sync_sessions(&mut sessions, now);
    if let Some(existing) = sessions
        .values()
        .find(|session| session.lock_key == lock_key && session.expires_at > now)
    {
        if existing.agent_id == payload.agent_id && existing.remote_root == remote_root {
            let sync_session_id = existing.sync_session_id.clone();
            let session = sessions
                .get_mut(&sync_session_id)
                .ok_or_else(|| anyhow!("sync session disappeared during recovery"))?;
            session.expires_at = now + StdDuration::from_secs(session.ttl_seconds);
            return Ok(sync_session_response(session));
        }
        bail!(
            "sync lock held by agent={} session={} lock_key={} expires_at_unix_ms={}",
            existing.agent_id,
            existing.sync_session_id,
            existing.lock_key,
            unix_ms(existing.expires_at)
        );
    }

    let sync_session_id = Uuid::new_v4().to_string();
    let lock_token = Uuid::new_v4().to_string();
    let expires_at = now + StdDuration::from_secs(ttl_seconds);
    let session = SyncSession {
        sync_session_id: sync_session_id.clone(),
        lock_token: lock_token.clone(),
        agent_id: payload.agent_id.clone(),
        remote_root: remote_root.clone(),
        lock_key: lock_key.clone(),
        ttl_seconds,
        expires_at,
    };
    sessions.insert(sync_session_id.clone(), session);
    Ok(SyncSessionInitResponse {
        ok: true,
        sync_session_id,
        lock_token,
        agent_id: payload.agent_id,
        remote_root: remote_root.display().to_string(),
        lock_key,
        expires_unix_ms: unix_ms(expires_at),
        heartbeat_seconds: DEFAULT_SYNC_SESSION_HEARTBEAT_SECONDS,
    })
}

pub async fn handle_sync_session_status(
    state: &ServerState,
    payload: SyncSessionStatusRequest,
) -> Result<SyncSessionInitResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let lock_key = payload
        .lock_key
        .clone()
        .unwrap_or_else(|| remote_root.display().to_string());
    let now = SystemTime::now();
    let mut sessions = state.sync_sessions.lock().await;
    purge_expired_sync_sessions(&mut sessions, now);
    let Some(session) = sessions.get_mut(&payload.sync_session_id) else {
        bail!(
            "sync session not found or expired: {}",
            payload.sync_session_id
        );
    };
    if session.lock_token != payload.lock_token {
        bail!("sync session lock token mismatch");
    }
    if session.agent_id != payload.agent_id {
        bail!(
            "sync session agent mismatch: expected {}, got {}",
            session.agent_id,
            payload.agent_id
        );
    }
    if session.remote_root != remote_root {
        bail!(
            "sync session {} does not own remote root {}",
            session.sync_session_id,
            remote_root.display()
        );
    }
    if session.lock_key != lock_key {
        bail!(
            "sync session lock key mismatch: expected {}, got {}",
            session.lock_key,
            lock_key
        );
    }
    session.expires_at = now + StdDuration::from_secs(session.ttl_seconds);
    Ok(sync_session_response(session))
}

pub async fn handle_sync_session_release(
    state: &ServerState,
    payload: SyncSessionReleaseRequest,
) -> Result<SimpleResponse> {
    let mut sessions = state.sync_sessions.lock().await;
    let Some(session) = sessions.get(&payload.sync_session_id) else {
        return Ok(SimpleResponse {
            ok: true,
            error: None,
        });
    };
    if session.lock_token != payload.lock_token {
        bail!("sync session lock token mismatch");
    }
    sessions.remove(&payload.sync_session_id);
    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

fn sync_session_response(session: &SyncSession) -> SyncSessionInitResponse {
    SyncSessionInitResponse {
        ok: true,
        sync_session_id: session.sync_session_id.clone(),
        lock_token: session.lock_token.clone(),
        agent_id: session.agent_id.clone(),
        remote_root: session.remote_root.display().to_string(),
        lock_key: session.lock_key.clone(),
        expires_unix_ms: unix_ms(session.expires_at),
        heartbeat_seconds: DEFAULT_SYNC_SESSION_HEARTBEAT_SECONDS,
    }
}

pub async fn handle_file_read(
    state: &ServerState,
    payload: FileReadRequest,
) -> Result<FileReadResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let path = safe_join(&remote_root, &payload.path)?;
    let content = tokio::fs::read(&path).await?;
    let executable = is_executable(&path).await?;
    Ok(FileReadResponse {
        ok: true,
        path: payload.path,
        content_b64: BASE64.encode(content),
        executable,
    })
}

pub async fn handle_file_stat(
    state: &ServerState,
    payload: FileStatRequest,
) -> Result<FileStatResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let path = safe_join(&remote_root, &payload.path)?;
    let metadata = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileStatResponse {
                ok: true,
                path: payload.path,
                exists: false,
                file_type: "missing".to_string(),
                size: None,
                modified_unix_ms: None,
                executable: false,
                sha256: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let file_type = file_type_name(&metadata).to_string();
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis());
    Ok(FileStatResponse {
        ok: true,
        path: payload.path,
        exists: true,
        file_type,
        size: Some(metadata.len()),
        modified_unix_ms,
        executable: metadata_executable(&metadata),
        sha256: if metadata.is_file() {
            Some(hash_file_sha256(&path).await?)
        } else {
            None
        },
    })
}

pub async fn handle_file_write(
    state: &ServerState,
    payload: FileWriteRequest,
) -> Result<SimpleResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let path = safe_join(&remote_root, &payload.path)?;
    let content = BASE64
        .decode(payload.content_b64.as_bytes())
        .context("failed to decode base64 content")?;
    let options = WriteFileOptions {
        create_parents: payload.create_parents,
        atomic: payload.atomic,
        mode: payload.mode,
        executable: payload.executable,
        preserve_mode: payload.preserve_mode,
        expected_sha256: payload.checksum_sha256,
        skip_if_same: false,
    };
    write_file(&path, &content, &options).await?;
    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

pub async fn handle_file_upload_init(
    state: &ServerState,
    payload: FileUploadInitRequest,
) -> Result<FileUploadInitResponse> {
    let checksum_sha256 = normalize_sha256(&payload.checksum_sha256);
    validate_sha256(&checksum_sha256)?;
    if payload.chunk_size == 0 {
        bail!("chunk_size must be greater than zero");
    }
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    validate_optional_sync_session(
        state,
        &remote_root,
        payload.sync_session_id.as_deref(),
        payload.lock_token.as_deref(),
    )
    .await?;
    let target_path = safe_join(&remote_root, &payload.path)?;
    let staging_path = upload_staging_path(&target_path, &checksum_sha256, payload.atomic)?;

    if payload.create_parents
        && let Some(parent) = staging_path.parent()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut uploads = state.uploads.lock().await;
    let mut existing_upload_id = None;
    for (upload_id, session) in uploads.iter() {
        if session.remote_root == payload.remote_root
            && session.path == payload.path
            && session
                .checksum_sha256
                .eq_ignore_ascii_case(&checksum_sha256)
            && session.sync_session_id == payload.sync_session_id
            && session.lock_token == payload.lock_token
        {
            existing_upload_id = Some(upload_id.clone());
            break;
        }
    }

    let session = if let Some(upload_id) = existing_upload_id {
        let session = uploads
            .get_mut(&upload_id)
            .ok_or_else(|| anyhow!("upload session disappeared during init"))?;
        ensure_matching_upload_init(session, &payload, &checksum_sha256)?;
        session.chunk_size = payload.chunk_size;
        session.clone()
    } else {
        let upload_id = Uuid::new_v4().to_string();
        let session = UploadSession {
            upload_id: upload_id.clone(),
            remote_root: payload.remote_root.clone(),
            path: payload.path.clone(),
            target_path,
            staging_path,
            total_size: payload.total_size,
            chunk_size: payload.chunk_size,
            checksum_sha256: checksum_sha256.clone(),
            create_parents: payload.create_parents,
            atomic: payload.atomic,
            mode: payload.mode,
            executable: payload.executable,
            preserve_mode: payload.preserve_mode,
            sync_session_id: payload.sync_session_id.clone(),
            lock_token: payload.lock_token.clone(),
        };
        uploads.insert(upload_id, session.clone());
        session
    };
    drop(uploads);

    let received_bytes = if payload.resume {
        upload_current_size(&session).await?
    } else {
        reset_upload_contents(&session).await?;
        0
    };
    if received_bytes > session.total_size {
        bail!(
            "partial upload for {} exceeds declared size",
            session.target_path.display()
        );
    }

    Ok(FileUploadInitResponse {
        ok: true,
        upload_id: session.upload_id,
        received_bytes,
        total_size: session.total_size,
        chunk_size: session.chunk_size,
    })
}

pub async fn handle_file_upload_chunk(
    state: &ServerState,
    payload: FileUploadChunkRequest,
) -> Result<FileUploadChunkResponse> {
    let session = get_upload_session(state, &payload.upload_id).await?;
    validate_upload_session_lock(
        state,
        &session,
        &payload.sync_session_id,
        &payload.lock_token,
    )
    .await?;
    let data = BASE64
        .decode(payload.data_b64.as_bytes())
        .context("failed to decode base64 upload chunk")?;
    if let Some(chunk_checksum_sha256) = payload.chunk_checksum_sha256.as_deref() {
        let chunk_checksum_sha256 = normalize_sha256(chunk_checksum_sha256);
        validate_sha256(&chunk_checksum_sha256)?;
        let actual = sha256_hex(&data);
        if !actual.eq_ignore_ascii_case(&chunk_checksum_sha256) {
            bail!("upload chunk checksum mismatch for {}", session.path);
        }
    }
    let current_size = upload_current_size(&session).await?;
    if current_size != payload.offset {
        bail!(
            "upload offset mismatch for {}: expected {}, got {}",
            session.path,
            current_size,
            payload.offset
        );
    }
    let next_size = current_size
        .checked_add(u64::try_from(data.len()).context("upload chunk too large")?)
        .ok_or_else(|| anyhow!("upload size overflow for {}", session.path))?;
    if next_size > session.total_size {
        bail!(
            "upload chunk exceeds declared size for {}",
            session.target_path.display()
        );
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&session.staging_path)
        .await
        .with_context(|| format!("failed to open {}", session.staging_path.display()))?;
    file.write_all(&data)
        .await
        .with_context(|| format!("failed to append {}", session.staging_path.display()))?;
    file.flush().await?;

    Ok(FileUploadChunkResponse {
        ok: true,
        upload_id: session.upload_id,
        received_bytes: next_size,
    })
}

pub async fn handle_file_upload_status(
    state: &ServerState,
    payload: FileUploadStatusRequest,
) -> Result<FileUploadStatusResponse> {
    let session = get_upload_session(state, &payload.upload_id).await?;
    validate_upload_session_lock(
        state,
        &session,
        &payload.sync_session_id,
        &payload.lock_token,
    )
    .await?;
    let received_bytes = upload_current_size(&session).await?;
    Ok(FileUploadStatusResponse {
        ok: true,
        upload_id: session.upload_id,
        received_bytes,
        total_size: session.total_size,
        path: session.path,
    })
}

pub async fn handle_file_upload_finish(
    state: &ServerState,
    payload: FileUploadFinishRequest,
) -> Result<SimpleResponse> {
    let session = get_upload_session(state, &payload.upload_id).await?;
    validate_upload_session_lock(
        state,
        &session,
        &payload.sync_session_id,
        &payload.lock_token,
    )
    .await?;
    let received_bytes = upload_current_size(&session).await?;
    if received_bytes != session.total_size {
        bail!(
            "upload for {} is incomplete: have {}, need {}",
            session.path,
            received_bytes,
            session.total_size
        );
    }

    let actual_sha256 = hash_file_sha256(&session.staging_path).await?;
    if !actual_sha256.eq_ignore_ascii_case(&session.checksum_sha256) {
        bail!("final checksum mismatch for {}", session.path);
    }

    let existing_mode = metadata_mode(&session.target_path).await?;
    if session.atomic {
        if let Some(parent) = session.target_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&session.staging_path, &session.target_path)
            .await
            .with_context(|| {
                format!(
                    "failed to publish {} to {}",
                    session.staging_path.display(),
                    session.target_path.display()
                )
            })?;
    }

    let mode = resolve_write_mode(
        &WriteFileOptions {
            create_parents: session.create_parents,
            atomic: session.atomic,
            mode: session.mode,
            executable: session.executable,
            preserve_mode: session.preserve_mode,
            expected_sha256: None,
            skip_if_same: false,
        },
        existing_mode,
    );
    apply_write_mode(&session.target_path, mode, session.executable).await?;

    let mut uploads = state.uploads.lock().await;
    uploads.remove(&payload.upload_id);
    drop(uploads);

    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

pub async fn handle_file_upload_abort(
    state: &ServerState,
    payload: crate::protocol::FileUploadAbortRequest,
) -> Result<SimpleResponse> {
    let session = get_upload_session(state, &payload.upload_id).await?;
    validate_upload_session_lock(
        state,
        &session,
        &payload.sync_session_id,
        &payload.lock_token,
    )
    .await?;
    let mut uploads = state.uploads.lock().await;
    uploads.remove(&payload.upload_id);
    drop(uploads);

    match tokio::fs::remove_file(&session.staging_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

fn ensure_matching_upload_init(
    session: &UploadSession,
    payload: &FileUploadInitRequest,
    checksum_sha256: &str,
) -> Result<()> {
    if session.total_size != payload.total_size
        || session.atomic != payload.atomic
        || session.mode != payload.mode
        || session.executable != payload.executable
        || session.preserve_mode != payload.preserve_mode
        || session.create_parents != payload.create_parents
        || !session
            .checksum_sha256
            .eq_ignore_ascii_case(checksum_sha256)
    {
        bail!(
            "resume parameters do not match existing upload for {}",
            session.path
        );
    }
    Ok(())
}

async fn get_upload_session(state: &ServerState, upload_id: &str) -> Result<UploadSession> {
    let uploads = state.uploads.lock().await;
    uploads
        .get(upload_id)
        .cloned()
        .ok_or_else(|| anyhow!("upload not found: {upload_id}"))
}

async fn validate_upload_session_lock(
    state: &ServerState,
    upload: &UploadSession,
    sync_session_id: &Option<String>,
    lock_token: &Option<String>,
) -> Result<()> {
    if upload.sync_session_id.is_none() {
        return Ok(());
    }
    if &upload.sync_session_id != sync_session_id || &upload.lock_token != lock_token {
        bail!("upload sync session mismatch for {}", upload.path);
    }
    validate_required_sync_session(
        state,
        &upload.target_path,
        sync_session_id.as_deref(),
        lock_token.as_deref(),
    )
    .await
}

async fn validate_optional_sync_session(
    state: &ServerState,
    remote_root: &Path,
    sync_session_id: Option<&str>,
    lock_token: Option<&str>,
) -> Result<()> {
    match (sync_session_id, lock_token) {
        (Some(sync_session_id), Some(lock_token)) => {
            validate_required_sync_session(
                state,
                remote_root,
                Some(sync_session_id),
                Some(lock_token),
            )
            .await
        }
        (None, None) => Ok(()),
        _ => bail!("sync session id and lock token must be provided together"),
    }
}

async fn validate_required_sync_session(
    state: &ServerState,
    remote_root_or_child: &Path,
    sync_session_id: Option<&str>,
    lock_token: Option<&str>,
) -> Result<()> {
    let sync_session_id = sync_session_id.ok_or_else(|| anyhow!("sync session id is required"))?;
    let lock_token = lock_token.ok_or_else(|| anyhow!("sync session lock token is required"))?;
    let now = SystemTime::now();
    let mut sessions = state.sync_sessions.lock().await;
    purge_expired_sync_sessions(&mut sessions, now);
    let Some(session) = sessions.get_mut(sync_session_id) else {
        bail!("sync session not found or expired: {sync_session_id}");
    };
    if session.lock_token != lock_token {
        bail!("sync session lock token mismatch");
    }
    if !(remote_root_or_child == session.remote_root
        || remote_root_or_child.starts_with(&session.remote_root))
    {
        bail!(
            "sync session {} does not own remote root {}",
            sync_session_id,
            remote_root_or_child.display()
        );
    }
    session.expires_at = now + StdDuration::from_secs(session.ttl_seconds);
    Ok(())
}

fn purge_expired_sync_sessions(sessions: &mut BTreeMap<String, SyncSession>, now: SystemTime) {
    sessions.retain(|_, session| session.expires_at > now);
}

fn unix_ms(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| StdDuration::from_secs(0))
        .as_millis()
}

fn upload_staging_path(target_path: &Path, checksum_sha256: &str, atomic: bool) -> Result<PathBuf> {
    if !atomic {
        return Ok(target_path.to_path_buf());
    }
    let parent = target_path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", target_path.display()))?;
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "target path has invalid file name: {}",
                target_path.display()
            )
        })?;
    Ok(parent.join(format!(
        ".{file_name}.{}.upload.part",
        &checksum_sha256[..12]
    )))
}

async fn upload_current_size(session: &UploadSession) -> Result<u64> {
    match tokio::fs::metadata(&session.staging_path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

async fn reset_upload_contents(session: &UploadSession) -> Result<()> {
    if session.create_parents
        && let Some(parent) = session.staging_path.parent()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&session.staging_path, [])
        .await
        .with_context(|| format!("failed to reset {}", session.staging_path.display()))
}

pub async fn handle_file_delete(
    state: &ServerState,
    payload: FileDeleteRequest,
) -> Result<SimpleResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let path = safe_join(&remote_root, &payload.path)?;
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_dir() => {
            tokio::fs::remove_dir_all(&path).await?;
        }
        Ok(_) => {
            tokio::fs::remove_file(&path).await?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(SimpleResponse {
        ok: true,
        error: None,
    })
}

pub async fn handle_file_find(
    state: &ServerState,
    payload: FileFindRequest,
) -> Result<FileFindResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let mut matches = Vec::new();
    find_matches(&remote_root, &remote_root, &payload.pattern, &mut matches).await?;
    Ok(FileFindResponse { ok: true, matches })
}

pub async fn handle_file_list(
    state: &ServerState,
    payload: FileListRequest,
) -> Result<FileListResponse> {
    let remote_root = resolve_remote_root(&state.allow_roots, &payload.remote_root)?;
    let target = match payload.path {
        Some(path) => safe_join(&remote_root, &path)?,
        None => remote_root.clone(),
    };
    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(&target).await?;
    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        let relative = path
            .strip_prefix(&remote_root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let metadata = entry.metadata().await?;
        entries.push(FileListEntry {
            path: relative,
            is_dir: metadata.is_dir(),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(FileListResponse { ok: true, entries })
}

async fn find_matches(
    remote_root: &Path,
    current: &Path,
    pattern: &str,
    matches: &mut Vec<String>,
) -> Result<()> {
    let mut stack = vec![current.to_path_buf()];
    while let Some(dir_path) = stack.pop() {
        let mut dir = tokio::fs::read_dir(&dir_path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            let relative = path
                .strip_prefix(remote_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let metadata = entry.metadata().await?;
            if relative.contains(pattern) {
                matches.push(relative.clone());
            }
            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }
    matches.sort();
    Ok(())
}

pub(super) fn resolve_remote_root(allow_roots: &[PathBuf], remote_root: &str) -> Result<PathBuf> {
    let remote_root = canonicalize_like(Path::new(remote_root))?;
    ensure_allowed_root(&remote_root, allow_roots)?;
    Ok(remote_root)
}

fn canonicalize_like(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        Ok(std::fs::canonicalize(path)?)
    } else if let Some(parent) = path.parent() {
        let canonical_parent = std::fs::canonicalize(parent)?;
        Ok(canonical_parent.join(path.file_name().unwrap_or_default()))
    } else {
        Ok(path.to_path_buf())
    }
}

fn ensure_allowed_root(remote_root: &Path, allow_roots: &[PathBuf]) -> Result<()> {
    let allowed = allow_roots.iter().any(|root| {
        let normalized_root = canonicalize_like(root).unwrap_or_else(|_| root.clone());
        remote_root == normalized_root
            || remote_root.starts_with(&normalized_root)
            || normalized_root.starts_with(remote_root)
    });
    if allowed {
        Ok(())
    } else {
        bail!("remote_root is not allowed: {}", remote_root.display())
    }
}

#[derive(Debug, Clone, Copy)]
struct SyncWriteOptions {
    checksum: bool,
    preserve_mode: bool,
    atomic_files: bool,
}

impl SyncWriteOptions {
    fn from_payload(payload: &SyncPayload) -> Self {
        Self {
            checksum: payload.checksum,
            preserve_mode: payload.preserve_mode,
            atomic_files: payload.atomic_files,
        }
    }
}

#[derive(Debug)]
struct WriteFileOptions {
    create_parents: bool,
    atomic: bool,
    mode: Option<u32>,
    executable: bool,
    preserve_mode: bool,
    expected_sha256: Option<String>,
    skip_if_same: bool,
}

async fn apply_changes(
    remote_root: &Path,
    writes: &[FileWrite],
    deletes: &[String],
    options: SyncWriteOptions,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    for relative in deletes {
        let target = safe_join(remote_root, relative)?;
        match tokio::fs::metadata(&target).await {
            Ok(metadata) if metadata.is_dir() => {
                tokio::fs::remove_dir_all(&target).await?;
                report.deleted.push(relative.clone());
            }
            Ok(_) => {
                tokio::fs::remove_file(&target).await?;
                report.deleted.push(relative.clone());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    for write in writes {
        let target = safe_join(remote_root, &write.path)?;
        if write.preuploaded {
            let outcome = validate_preuploaded_file(&target, write, &options).await?;
            match outcome {
                WriteOutcome::Skipped => report.skipped.push(write.path.clone()),
                WriteOutcome::Written if write.preupload_existed => {
                    report.updated.push(write.path.clone());
                }
                WriteOutcome::Written => report.created.push(write.path.clone()),
            }
            continue;
        }

        let content = BASE64
            .decode(write.content_b64.as_bytes())
            .context("failed to decode base64 content")?;
        let existed = tokio::fs::metadata(&target).await.is_ok();
        let expected_sha256 = if options.checksum {
            Some(
                write
                    .checksum_sha256
                    .clone()
                    .unwrap_or_else(|| sha256_hex(&content)),
            )
        } else {
            write.checksum_sha256.clone()
        };
        let outcome = write_file(
            &target,
            &content,
            &WriteFileOptions {
                create_parents: true,
                atomic: options.atomic_files,
                mode: if options.preserve_mode {
                    write.mode
                } else {
                    None
                },
                executable: write.executable,
                preserve_mode: false,
                expected_sha256,
                skip_if_same: options.checksum,
            },
        )
        .await?;
        match outcome {
            WriteOutcome::Skipped => report.skipped.push(write.path.clone()),
            WriteOutcome::Written if existed => report.updated.push(write.path.clone()),
            WriteOutcome::Written => report.created.push(write.path.clone()),
        }
    }

    Ok(report)
}

async fn validate_preuploaded_file(
    target: &Path,
    write: &FileWrite,
    options: &SyncWriteOptions,
) -> Result<WriteOutcome> {
    let metadata = tokio::fs::metadata(target)
        .await
        .with_context(|| format!("preuploaded file is missing: {}", target.display()))?;
    if !metadata.is_file() {
        bail!(
            "preuploaded path is not a regular file: {}",
            target.display()
        );
    }
    let expected_sha256 = write
        .checksum_sha256
        .as_deref()
        .ok_or_else(|| anyhow!("preuploaded file is missing checksum: {}", write.path))
        .map(normalize_sha256)?;
    validate_sha256(&expected_sha256)?;
    let actual_sha256 = hash_file_sha256(target).await?;
    if !actual_sha256.eq_ignore_ascii_case(&expected_sha256) {
        bail!("preuploaded checksum mismatch for {}", write.path);
    }

    apply_write_mode(
        target,
        if options.preserve_mode {
            write.mode
        } else {
            None
        },
        write.executable,
    )
    .await?;

    if write.preupload_skipped {
        Ok(WriteOutcome::Skipped)
    } else {
        Ok(WriteOutcome::Written)
    }
}

async fn apply_snapshot(
    remote_root: &Path,
    writes: &[FileWrite],
    preserve_paths: &[String],
    options: SyncWriteOptions,
) -> Result<SyncReport> {
    let keep = writes
        .iter()
        .map(|write| write.path.clone())
        .collect::<BTreeSet<_>>();
    let deleted_paths =
        remove_non_snapshot_entries(remote_root, remote_root, &keep, preserve_paths).await?;
    let mut report = apply_changes(remote_root, writes, &[], options).await?;
    report.deleted = deleted_paths;
    prune_empty_directories(remote_root, remote_root, preserve_paths).await?;
    Ok(report)
}

async fn remove_non_snapshot_entries(
    remote_root: &Path,
    current: &Path,
    keep: &BTreeSet<String>,
    preserve_paths: &[String],
) -> Result<Vec<String>> {
    let mut stack = vec![current.to_path_buf()];
    let mut directories = Vec::new();
    let mut deleted = Vec::new();

    while let Some(dir_path) = stack.pop() {
        directories.push(dir_path.clone());
        let mut dir = match tokio::fs::read_dir(&dir_path).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            let relative = path
                .strip_prefix(remote_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            if relative_path_matches_preserve_path(&relative, preserve_paths) {
                continue;
            }

            let metadata = entry.metadata().await?;
            if metadata.is_dir() {
                stack.push(path);
            } else if !keep.contains(&relative) {
                tokio::fs::remove_file(&path).await?;
                deleted.push(relative);
            }
        }
    }

    for dir_path in directories.into_iter().rev() {
        if dir_path == remote_root
            || directory_contains_preserved_entries(remote_root, &dir_path, preserve_paths)?
        {
            continue;
        }
        let mut nested = match tokio::fs::read_dir(&dir_path).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if nested.next_entry().await?.is_none() {
            tokio::fs::remove_dir(&dir_path).await?;
        }
    }

    deleted.sort();
    Ok(deleted)
}

async fn prune_empty_directories(
    remote_root: &Path,
    current: &Path,
    preserve_paths: &[String],
) -> Result<()> {
    let mut stack = vec![current.to_path_buf()];
    let mut directories = Vec::new();

    while let Some(dir_path) = stack.pop() {
        directories.push(dir_path.clone());
        let mut dir = match tokio::fs::read_dir(&dir_path).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            let metadata = entry.metadata().await?;
            if !metadata.is_dir() {
                continue;
            }
            let relative = path
                .strip_prefix(remote_root)
                .unwrap_or(&path)
                .display()
                .to_string();
            if relative_path_matches_preserve_path(&relative, preserve_paths) {
                continue;
            }
            stack.push(path);
        }
    }

    for dir_path in directories.into_iter().rev() {
        if dir_path == remote_root {
            continue;
        }
        let relative = dir_path
            .strip_prefix(remote_root)
            .unwrap_or(&dir_path)
            .display()
            .to_string();
        if relative_path_matches_preserve_path(&relative, preserve_paths) {
            continue;
        }
        let mut nested = match tokio::fs::read_dir(&dir_path).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if nested.next_entry().await?.is_none() {
            tokio::fs::remove_dir(&dir_path).await?;
        }
    }

    Ok(())
}

fn directory_contains_preserved_entries(
    remote_root: &Path,
    current: &Path,
    preserve_paths: &[String],
) -> Result<bool> {
    let relative = current
        .strip_prefix(remote_root)
        .unwrap_or(current)
        .display()
        .to_string();
    Ok(preserve_paths.iter().any(|preserve| {
        preserve == &relative
            || preserve
                .strip_prefix(&relative)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }))
}

pub(super) fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        bail!("path escapes remote root: {relative}");
    }

    let mut target = root.to_path_buf();
    for component in relative_path.components() {
        match component {
            std::path::Component::Normal(part) => target.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => bail!("path escapes remote root: {relative}"),
        }
    }
    Ok(target)
}

pub(super) fn resolve_cwd(remote_root: &Path, cwd: &str) -> Result<PathBuf> {
    let candidate = if Path::new(cwd).is_absolute() {
        canonicalize_like(Path::new(cwd))?
    } else {
        safe_join(remote_root, cwd)?
    };
    if candidate.starts_with(remote_root) {
        Ok(candidate)
    } else {
        bail!("cwd must be within remote root: {cwd}")
    }
}

async fn is_executable(path: &Path) -> Result<bool> {
    let metadata = tokio::fs::metadata(path).await?;
    Ok(metadata_executable(&metadata))
}

fn metadata_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn file_type_name(metadata: &std::fs::Metadata) -> &'static str {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        "file"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteOutcome {
    Written,
    Skipped,
}

async fn write_file(
    path: &Path,
    content: &[u8],
    options: &WriteFileOptions,
) -> Result<WriteOutcome> {
    let expected_sha256 = options.expected_sha256.as_deref().map(normalize_sha256);
    if let Some(expected) = expected_sha256.as_deref() {
        validate_sha256(expected)?;
        let actual = sha256_hex(content);
        if !actual.eq_ignore_ascii_case(expected) {
            bail!(
                "content checksum mismatch before write for {}",
                path.display()
            );
        }
    }

    let existing_mode = metadata_mode(path).await?;
    if options.skip_if_same
        && let Some(expected) = expected_sha256.as_deref()
        && tokio::fs::metadata(path).await.is_ok()
        && hash_file_sha256(path).await?.eq_ignore_ascii_case(expected)
        && mode_already_satisfied(path, options, existing_mode).await?
    {
        return Ok(WriteOutcome::Skipped);
    }

    if options.create_parents
        && let Some(parent) = path.parent()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mode = resolve_write_mode(options, existing_mode);
    if options.atomic {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("target path has invalid file name: {}", path.display()))?;
        let temp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
        let write_result = async {
            tokio::fs::write(&temp_path, content).await?;
            apply_write_mode(&temp_path, mode, options.executable).await?;
            tokio::fs::rename(&temp_path, path).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if write_result.is_err() {
            let _ = tokio::fs::remove_file(&temp_path).await;
        }
        write_result?;
    } else {
        tokio::fs::write(path, content).await?;
        apply_write_mode(path, mode, options.executable).await?;
    }

    if let Some(expected) = expected_sha256.as_deref() {
        let actual = hash_file_sha256(path).await?;
        if !actual.eq_ignore_ascii_case(expected) {
            bail!(
                "content checksum mismatch after write for {}",
                path.display()
            );
        }
    }

    Ok(WriteOutcome::Written)
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

async fn mode_already_satisfied(
    path: &Path,
    options: &WriteFileOptions,
    existing_mode: Option<u32>,
) -> Result<bool> {
    if options.preserve_mode {
        return Ok(true);
    }
    if let Some(expected_mode) = options.mode {
        return Ok(existing_mode.is_some_and(|mode| mode & 0o7777 == expected_mode & 0o7777));
    }
    Ok(is_executable(path).await? == options.executable)
}

fn resolve_write_mode(options: &WriteFileOptions, existing_mode: Option<u32>) -> Option<u32> {
    if let Some(mode) = options.mode {
        return Some(mode);
    }
    if options.preserve_mode {
        return existing_mode;
    }
    None
}

async fn apply_write_mode(path: &Path, mode: Option<u32>, executable: bool) -> Result<()> {
    if let Some(mode) = mode {
        set_mode(path, mode).await
    } else {
        set_executable(path, executable).await
    }
}

async fn metadata_mode(path: &Path) -> Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match tokio::fs::metadata(path).await {
            Ok(metadata) => Ok(Some(metadata.permissions().mode() & 0o7777)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

async fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if mode > 0o7777 {
            bail!("mode must be an octal permission value no greater than 7777");
        }
        let mut permissions = tokio::fs::metadata(path).await?.permissions();
        permissions.set_mode(mode);
        tokio::fs::set_permissions(path, permissions).await?;
    }

    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }

    Ok(())
}

async fn hash_file_sha256(path: &Path) -> Result<String> {
    let content = tokio::fs::read(path).await?;
    Ok(sha256_hex(&content))
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

async fn set_executable(path: &Path, executable: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = tokio::fs::metadata(path).await?;
        let mut permissions = metadata.permissions();
        let mode = permissions.mode();
        let updated = if executable {
            mode | 0o111
        } else {
            mode & !0o111
        };
        permissions.set_mode(updated);
        tokio::fs::set_permissions(path, permissions).await?;
    }

    #[cfg(not(unix))]
    {
        let _ = (path, executable);
    }

    Ok(())
}

async fn run_command(
    state: &ServerState,
    remote_root: &Path,
    command: Option<String>,
    timeout_seconds: u64,
    env: BTreeMap<String, String>,
    claims: Vec<ResourceClaim>,
    execution_lease: Option<&ExecutionLease>,
) -> Result<CommandResult> {
    let Some(command_text) = command.clone() else {
        return Ok(CommandResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            command: None,
        });
    };

    let claimed_resources = if execution_lease.is_some() {
        merge_resource_claims(&claims, &infer_gpu_resource_claims_from_sync_env(&env))?
    } else {
        Vec::new()
    };
    let claim_process_id = execution_lease.map(|lease| {
        format!(
            "sync-command:{}/{}/{}",
            lease.task_id,
            lease.lease_id,
            remote_root.display()
        )
    });
    if let (Some(lease), Some(process_id)) = (execution_lease, claim_process_id.as_deref()) {
        let mut modes = state.modes.lock().await;
        modes.claim_resources(
            &lease.task_id,
            &lease.lease_id,
            process_id,
            &claimed_resources,
        )?;
    }

    let mut child = Command::new("bash");
    child.arg("-lc").arg(&command_text).current_dir(remote_root);
    child.envs(env);
    child.stdout(std::process::Stdio::piped());
    child.stderr(std::process::Stdio::piped());

    let mut child = match child.spawn().context("failed to spawn command") {
        Ok(child) => child,
        Err(error) => {
            if let Some(process_id) = claim_process_id.as_deref() {
                let mut modes = state.modes.lock().await;
                modes.release_process_resource_claims(process_id);
            }
            return Err(error);
        }
    };
    let stdout = child.stdout.take().context("missing child stdout")?;
    let stderr = child.stderr.take().context("missing child stderr")?;

    let stdout_task = tokio::spawn(async move {
        read_stream_limited(stdout, DEFAULT_SYNC_RUN_OUTPUT_LIMIT_BYTES).await
    });
    let stderr_task = tokio::spawn(async move {
        read_stream_limited(stderr, DEFAULT_SYNC_RUN_OUTPUT_LIMIT_BYTES).await
    });

    let status = match timeout(Duration::from_secs(timeout_seconds), child.wait()).await {
        Ok(result) => result.context("failed waiting for child process")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            if let Some(process_id) = claim_process_id.as_deref() {
                let mut modes = state.modes.lock().await;
                modes.release_process_resource_claims(process_id);
            }
            bail!("command timed out after {timeout_seconds} seconds");
        }
    };

    let (stdout_bytes, stdout_truncated) =
        stdout_task.await.context("stdout task join failed")??;
    let (stderr_bytes, stderr_truncated) =
        stderr_task.await.context("stderr task join failed")??;

    if let Some(process_id) = claim_process_id.as_deref() {
        let mut modes = state.modes.lock().await;
        modes.release_process_resource_claims(process_id);
    }

    Ok(CommandResult {
        exit_code: status.code().unwrap_or(1),
        stdout: render_limited_output(&stdout_bytes, stdout_truncated, "stdout"),
        stderr: render_limited_output(&stderr_bytes, stderr_truncated, "stderr"),
        command: Some(command_text),
    })
}

async fn read_stream_limited<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let mut local = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut local).await?;
        if n == 0 {
            break;
        }
        let remaining = limit.saturating_sub(buffer.len());
        if remaining > 0 {
            let copy_len = remaining.min(n);
            buffer.extend_from_slice(&local[..copy_len]);
        }
        if buffer.len() >= limit && n > remaining {
            truncated = true;
        }
        if buffer.len() >= limit {
            truncated = true;
        }
    }
    Ok((buffer, truncated))
}

fn render_limited_output(bytes: &[u8], truncated: bool, stream_name: &str) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if truncated {
        text.push_str(&format!(
            "\n[agentplane] {stream_name} truncated after {DEFAULT_SYNC_RUN_OUTPUT_LIMIT_BYTES} bytes"
        ));
    }
    text
}
