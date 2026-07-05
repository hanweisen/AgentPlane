use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::StatusCode;

use crate::cli_client::{post_json, print_error_response};
use crate::config::{ClientProfile, ResolvedClientAuth, resolve_remote_root};
use crate::protocol::{
    FileDeleteRequest, FileFindRequest, FileListRequest, FileReadRequest, FileReadResponse,
    FileStatRequest, FileStatResponse, FileWriteRequest, SimpleResponse,
};

use super::{
    FileDeleteArgs, FileFindArgs, FileListArgs, FileReadArgs, FileStatArgs, FileWaitArgs,
    FileWriteArgs,
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
            eprintln!(
                "file-wait timed out after {} seconds; last observed state:",
                args.timeout_seconds
            );
            eprintln!("{}", serde_json::to_string_pretty(&stat)?);
            return Ok(ExitCode::from(1));
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(FILE_WAIT_POLL_MS))).await;
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

fn stat_ready(stat: &FileStatResponse, min_bytes: Option<u64>) -> bool {
    if !stat.exists {
        return false;
    }
    if let Some(min_bytes) = min_bytes {
        return stat.size.is_some_and(|size| size >= min_bytes);
    }
    true
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
