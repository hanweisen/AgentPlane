use std::path::PathBuf;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli_client::{post_json, process_error_response};
use crate::config::ResolvedClientAuth;
use crate::protocol::{
    SimpleResponse, SyncSessionInitRequest, SyncSessionInitResponse, SyncSessionReleaseRequest,
    SyncSessionStatusRequest,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSyncSession {
    server: String,
    agent_id: String,
    remote_root: String,
    lock_key: String,
    sync_session_id: String,
    lock_token: String,
}

pub(super) async fn acquire_sync_session(
    auth: &ResolvedClientAuth,
    remote_root: &str,
    lock_key: Option<&str>,
) -> Result<SyncSessionInitResponse> {
    let resolved_lock_key = lock_key.unwrap_or(remote_root);
    let cache_path = cache_path(auth, remote_root, resolved_lock_key);
    if let Some(cached) = load_cached_session(&cache_path)? {
        match refresh_cached_session(auth, remote_root, resolved_lock_key, &cached).await {
            Ok(session) => {
                save_cached_session(&cache_path, auth, &session)?;
                return Ok(session);
            }
            Err(_) => {
                remove_cached_session(&cache_path);
            }
        }
    }

    let response = post_json(
        auth,
        "/v1/sync/session/init",
        &SyncSessionInitRequest {
            remote_root: remote_root.to_string(),
            agent_id: auth.agent_id.clone(),
            ttl_seconds: None,
            lock_key: lock_key.map(ToString::to_string),
        },
        false,
    )
    .await?;
    if response.status() == StatusCode::OK {
        let session = response.json().await?;
        save_cached_session(&cache_path, auth, &session)?;
        return Ok(session);
    }
    Err(process_error_response(response).await)
}

pub(super) async fn release_sync_session(
    auth: &ResolvedClientAuth,
    session: &SyncSessionInitResponse,
) -> Result<()> {
    let response = post_json(
        auth,
        "/v1/sync/session/release",
        &SyncSessionReleaseRequest {
            sync_session_id: session.sync_session_id.clone(),
            lock_token: session.lock_token.clone(),
        },
        false,
    )
    .await?;
    if response.status() == StatusCode::OK {
        let _body: SimpleResponse = response.json().await?;
        let cache_path = cache_path(auth, &session.remote_root, &session.lock_key);
        remove_cached_session(&cache_path);
        return Ok(());
    }
    Err(process_error_response(response).await)
}

async fn refresh_cached_session(
    auth: &ResolvedClientAuth,
    remote_root: &str,
    lock_key: &str,
    cached: &CachedSyncSession,
) -> Result<SyncSessionInitResponse> {
    if cached.server != auth.server
        || cached.agent_id != auth.agent_id
        || cached.remote_root != remote_root
        || cached.lock_key != lock_key
    {
        anyhow::bail!("cached sync session key mismatch");
    }
    let response = post_json(
        auth,
        "/v1/sync/session/status",
        &SyncSessionStatusRequest {
            sync_session_id: cached.sync_session_id.clone(),
            lock_token: cached.lock_token.clone(),
            remote_root: remote_root.to_string(),
            agent_id: auth.agent_id.clone(),
            lock_key: Some(lock_key.to_string()),
        },
        true,
    )
    .await?;
    if response.status() == StatusCode::OK {
        return Ok(response.json().await?);
    }
    Err(process_error_response(response).await)
}

fn load_cached_session(path: &PathBuf) -> Result<Option<CachedSyncSession>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read sync session cache {}", path.display()));
        }
    };
    let session = match serde_json::from_str(&text) {
        Ok(session) => session,
        Err(_) => {
            remove_cached_session(path);
            return Ok(None);
        }
    };
    Ok(Some(session))
}

fn save_cached_session(
    path: &PathBuf,
    auth: &ResolvedClientAuth,
    session: &SyncSessionInitResponse,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create sync session cache {}", parent.display()))?;
    }
    let cached = CachedSyncSession {
        server: auth.server.clone(),
        agent_id: auth.agent_id.clone(),
        remote_root: session.remote_root.clone(),
        lock_key: session.lock_key.clone(),
        sync_session_id: session.sync_session_id.clone(),
        lock_token: session.lock_token.clone(),
    };
    let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp_path, serde_json::to_vec(&cached)?)
        .with_context(|| format!("failed to write sync session cache {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to publish sync session cache {}", path.display()))
}

fn remove_cached_session(path: &PathBuf) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

fn cache_path(auth: &ResolvedClientAuth, remote_root: &str, lock_key: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(auth.server.as_bytes());
    hasher.update([0]);
    hasher.update(auth.agent_id.as_bytes());
    hasher.update([0]);
    hasher.update(remote_root.as_bytes());
    hasher.update([0]);
    hasher.update(lock_key.as_bytes());
    let digest = hasher.finalize();
    let mut name = String::with_capacity(digest.len() * 2 + ".json".len());
    for byte in digest {
        name.push_str(&format!("{byte:02x}"));
    }
    name.push_str(".json");
    std::env::temp_dir()
        .join("agentplane")
        .join("sync-sessions")
        .join(name)
}
