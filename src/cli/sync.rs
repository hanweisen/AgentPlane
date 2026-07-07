use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::cli_client::{
    build_http_client, post_json, post_json_with_client, print_error_response,
    process_error_response,
};
use crate::config::{ClientProfile, ResolvedClientAuth, resolve_remote_root};
use crate::git::{
    collect_repo_changes, collect_repo_changes_between_refs, collect_repo_snapshot,
    collect_repo_worktree_snapshot, parse_env_pairs, resolve_ref, resolve_repo_root,
};
use crate::protocol::{
    FileListEntry, FileListRequest, FileListResponse, FileStatRequest, FileStatResponse, FileWrite,
    SyncMode, SyncPayload, SyncResponse, SyncSessionInitResponse, parse_resource_claim_specs,
    relative_path_matches_preserve_path,
};

use super::file::{UploadBytesOptions, upload_bytes};
use super::sync_session::{acquire_sync_session, release_sync_session};
use super::{SyncInitArgs, SyncRunArgs};

pub(super) async fn sync_run(args: SyncRunArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let repo_root = resolve_repo_root(&args.repo)?;
    let (mut writes, mut deletes, sync_mode, source_ref, exact_sync) = if let Some(git_ref) =
        &args.git_ref
    {
        let resolved_ref = resolve_ref(&repo_root, git_ref)?;
        if let Some(base_ref) = &args.base_ref {
            let resolved_base_ref = resolve_ref(&repo_root, base_ref)?;
            let (writes, deletes) =
                collect_repo_changes_between_refs(&repo_root, &resolved_base_ref, &resolved_ref)?;
            (
                writes,
                deletes,
                SyncMode::WorktreeDelta,
                Some(format!("{resolved_base_ref}..{resolved_ref}")),
                false,
            )
        } else {
            let writes = collect_repo_snapshot(&repo_root, &resolved_ref)?;
            (
                writes,
                Vec::new(),
                SyncMode::RefSnapshot,
                Some(resolved_ref),
                true,
            )
        }
    } else {
        if args.base_ref.is_some() {
            return Err(anyhow!("--base-ref requires --ref"));
        }
        let (writes, deletes) = collect_repo_changes(&repo_root)?;
        (
            writes,
            deletes,
            SyncMode::WorktreeDelta,
            None,
            args.exact_sync,
        )
    };
    if exact_sync && (!args.include.is_empty() || !args.exclude_from.is_empty()) {
        bail!(
            "--include/--exclude-from are not supported with exact --ref sync; use --base-ref for filtered deltas"
        );
    }
    let exclude_patterns = load_exclude_patterns(&args.exclude_from)?;
    filter_sync_paths(&mut writes, &mut deletes, &args.include, &exclude_patterns);
    let env = parse_env_pairs(&args.env)?;
    let claims = parse_resource_claim_specs(&args.claims)?;
    let dry_run_delete_preview = if args.dry_run && matches!(sync_mode, SyncMode::RefSnapshot) {
        Some(
            preview_ref_snapshot_deletes(
                &auth,
                &remote_root.display().to_string(),
                &writes,
                &args.preserve_path,
            )
            .await?,
        )
    } else {
        None
    };
    let remote_root_string = remote_root.display().to_string();
    let payload = SyncPayload {
        remote_root: remote_root_string.clone(),
        writes,
        deletes,
        sync_mode,
        source_ref,
        preserve_paths: args.preserve_path.clone(),
        command: args.command.clone(),
        timeout_seconds: args.timeout_seconds,
        env: Some(env),
        claims,
        checksum: args.checksum,
        preserve_mode: args.preserve_mode,
        atomic_files: args.atomic_files,
        sync_session_id: None,
        lock_token: None,
    };

    if args.dry_run {
        let write_details = payload
            .writes
            .iter()
            .map(|item| {
                serde_json::json!({
                    "path": item.path,
                    "executable": item.executable,
                    "mode": item.mode.map(|mode| format!("{mode:o}")),
                    "sha256": if args.checksum { item.checksum_sha256.clone() } else { None },
                })
            })
            .collect::<Vec<_>>();
        let preview = serde_json::json!({
            "repo_root": repo_root.display().to_string(),
            "sync_mode": payload.sync_mode,
            "source_ref": payload.source_ref,
            "preserve_paths": payload.preserve_paths,
            "exact_sync": exact_sync,
            "checksum": payload.checksum,
            "preserve_mode": payload.preserve_mode,
            "atomic_files": payload.atomic_files,
            "include": args.include,
            "exclude_from": args.exclude_from.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "write_count": payload.writes.len(),
            "delete_count": dry_run_delete_preview
                .as_ref()
                .map(|preview| preview.len())
                .unwrap_or(payload.deletes.len()),
            "writes": payload.writes.iter().map(|item| item.path.clone()).collect::<Vec<_>>(),
            "write_details": write_details,
            "deletes": dry_run_delete_preview.unwrap_or(payload.deletes),
            "command": payload.command,
            "env": payload.env,
            "claims": payload.claims,
        });
        println!("{}", serde_json::to_string_pretty(&preview)?);
        return Ok(ExitCode::SUCCESS);
    }

    send_locked_sync_payload(
        &auth,
        &remote_root_string,
        payload,
        args.upload_chunk_size,
        args.checksum,
        args.preserve_mode,
    )
    .await
}

pub(super) async fn sync_init(args: SyncInitArgs, profile: &ClientProfile) -> Result<ExitCode> {
    let auth = args.auth.resolve(profile)?;
    let remote_root = resolve_remote_root(args.remote_root.as_ref(), profile)?;
    let repo_root = resolve_repo_root(&args.repo)?;
    let writes = collect_repo_worktree_snapshot(&repo_root)?;
    let remote_root_string = remote_root.display().to_string();
    let dry_run_delete_preview = if args.dry_run {
        Some(
            preview_ref_snapshot_deletes(&auth, &remote_root_string, &writes, &args.preserve_path)
                .await?,
        )
    } else {
        None
    };

    let payload = SyncPayload {
        remote_root: remote_root_string.clone(),
        writes,
        deletes: Vec::new(),
        sync_mode: SyncMode::RefSnapshot,
        source_ref: Some("worktree".to_string()),
        preserve_paths: args.preserve_path.clone(),
        command: None,
        timeout_seconds: 0,
        env: Some(Default::default()),
        claims: Vec::new(),
        checksum: args.checksum,
        preserve_mode: args.preserve_mode,
        atomic_files: args.atomic_files,
        sync_session_id: None,
        lock_token: None,
    };

    if args.dry_run {
        let write_details = payload
            .writes
            .iter()
            .map(|item| {
                serde_json::json!({
                    "path": item.path,
                    "executable": item.executable,
                    "mode": item.mode.map(|mode| format!("{mode:o}")),
                    "sha256": if args.checksum { item.checksum_sha256.clone() } else { None },
                })
            })
            .collect::<Vec<_>>();
        let preview = serde_json::json!({
            "repo_root": repo_root.display().to_string(),
            "sync_mode": payload.sync_mode,
            "source_ref": payload.source_ref,
            "preserve_paths": payload.preserve_paths,
            "exact_sync": true,
            "checksum": payload.checksum,
            "preserve_mode": payload.preserve_mode,
            "atomic_files": payload.atomic_files,
            "write_count": payload.writes.len(),
            "delete_count": dry_run_delete_preview
                .as_ref()
                .map(|preview| preview.len())
                .unwrap_or_default(),
            "writes": payload.writes.iter().map(|item| item.path.clone()).collect::<Vec<_>>(),
            "write_details": write_details,
            "deletes": dry_run_delete_preview.unwrap_or_default(),
            "command": payload.command,
            "env": payload.env,
            "claims": payload.claims,
        });
        println!("{}", serde_json::to_string_pretty(&preview)?);
        return Ok(ExitCode::SUCCESS);
    }

    send_locked_sync_payload(
        &auth,
        &remote_root_string,
        payload,
        args.upload_chunk_size,
        args.checksum,
        args.preserve_mode,
    )
    .await
}

async fn send_locked_sync_payload(
    auth: &ResolvedClientAuth,
    remote_root: &str,
    mut payload: SyncPayload,
    upload_chunk_size: usize,
    checksum: bool,
    preserve_mode: bool,
) -> Result<ExitCode> {
    let session = acquire_sync_session(auth, remote_root, None).await?;
    payload.sync_session_id = Some(session.sync_session_id.clone());
    payload.lock_token = Some(session.lock_token.clone());

    let result = async {
        preupload_sync_writes(
            auth,
            remote_root,
            &mut payload.writes,
            upload_chunk_size,
            checksum,
            preserve_mode,
            &session,
        )
        .await?;
        post_sync_payload(auth, &payload).await
    }
    .await;

    if let Err(error) = release_sync_session(auth, &session).await {
        eprintln!("warning: failed to release sync session: {error:#}");
    }
    result
}

async fn post_sync_payload(auth: &ResolvedClientAuth, payload: &SyncPayload) -> Result<ExitCode> {
    let response = post_json(auth, "/v1/sync-run", payload, false).await?;
    if response.status() == StatusCode::OK {
        let body: SyncResponse = response.json().await?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(if body.ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }
    print_error_response(response).await
}

async fn preupload_sync_writes(
    auth: &ResolvedClientAuth,
    remote_root: &str,
    writes: &mut [FileWrite],
    chunk_size: usize,
    checksum: bool,
    preserve_mode: bool,
    session: &SyncSessionInitResponse,
) -> Result<()> {
    if chunk_size == 0 {
        bail!("--upload-chunk-size must be greater than zero");
    }
    if writes.is_empty() {
        return Ok(());
    }

    let client = build_http_client(auth)?;
    for write in writes {
        let stat = read_remote_stat(&client, auth, remote_root, &write.path).await?;
        let content = BASE64
            .decode(write.content_b64.as_bytes())
            .with_context(|| format!("failed to decode sync content for {}", write.path))?;
        let checksum_sha256 = write
            .checksum_sha256
            .clone()
            .unwrap_or_else(|| sha256_hex(&content));
        let preupload_existed = stat.exists && stat.file_type == "file";
        let preupload_skipped = checksum
            && preupload_existed
            && stat
                .sha256
                .as_deref()
                .is_some_and(|actual| actual.eq_ignore_ascii_case(&checksum_sha256))
            && stat_mode_satisfied(&stat, write, preserve_mode);

        upload_bytes(
            &client,
            auth,
            remote_root,
            &write.path,
            &content,
            &UploadBytesOptions {
                chunk_size,
                resume: true,
                executable: write.executable,
                create_parents: true,
                atomic: true,
                mode: if preserve_mode { write.mode } else { None },
                preserve_mode: false,
                checksum_sha256: checksum_sha256.clone(),
                sync_session_id: Some(session.sync_session_id.clone()),
                lock_token: Some(session.lock_token.clone()),
            },
        )
        .await?;

        write.content_b64.clear();
        write.checksum_sha256 = Some(checksum_sha256);
        write.preuploaded = true;
        write.preupload_existed = preupload_existed;
        write.preupload_skipped = preupload_skipped;
    }

    Ok(())
}

async fn read_remote_stat(
    client: &reqwest::Client,
    auth: &ResolvedClientAuth,
    remote_root: &str,
    path: &str,
) -> Result<FileStatResponse> {
    let response = post_json_with_client(
        client,
        auth,
        "/v1/file/stat",
        &FileStatRequest {
            remote_root: remote_root.to_string(),
            path: path.to_string(),
        },
        true,
    )
    .await?;
    if response.status() == StatusCode::OK {
        return Ok(response.json().await?);
    }
    Err(process_error_response(response).await)
}

fn stat_mode_satisfied(stat: &FileStatResponse, write: &FileWrite, preserve_mode: bool) -> bool {
    if preserve_mode {
        return false;
    }
    stat.executable == write.executable
}

fn sha256_hex(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn load_exclude_patterns(paths: &[PathBuf]) -> Result<Vec<String>> {
    let mut patterns = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read exclude file {}", path.display()))?;
        for line in text.lines() {
            let pattern = line.trim();
            if pattern.is_empty() || pattern.starts_with('#') {
                continue;
            }
            patterns.push(pattern.to_string());
        }
    }
    Ok(patterns)
}

fn filter_sync_paths(
    writes: &mut Vec<FileWrite>,
    deletes: &mut Vec<String>,
    include_patterns: &[String],
    exclude_patterns: &[String],
) {
    writes.retain(|write| path_selected(&write.path, include_patterns, exclude_patterns));
    deletes.retain(|path| path_selected(path, include_patterns, exclude_patterns));
}

fn path_selected(path: &str, include_patterns: &[String], exclude_patterns: &[String]) -> bool {
    let included = include_patterns.is_empty()
        || include_patterns
            .iter()
            .any(|pattern| path_pattern_matches(pattern, path));
    let excluded = exclude_patterns
        .iter()
        .any(|pattern| path_pattern_matches(pattern, path));
    included && !excluded
}

fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    let path = path.trim_start_matches("./");
    if let Some(prefix) = pattern.strip_suffix('/') {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return path == pattern || path.starts_with(&format!("{pattern}/"));
    }
    wildcard_matches(pattern.as_bytes(), path.as_bytes())
}

fn wildcard_matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut p, mut v) = (0usize, 0usize);
    let mut star = None;
    let mut star_value = 0usize;
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_value = v;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            star_value += 1;
            v = star_value;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

async fn preview_ref_snapshot_deletes(
    auth: &ResolvedClientAuth,
    remote_root: &str,
    writes: &[FileWrite],
    preserve_paths: &[String],
) -> Result<Vec<String>> {
    let keep = writes
        .iter()
        .map(|write| write.path.clone())
        .collect::<BTreeSet<_>>();
    let mut deletes = Vec::new();
    let mut stack = vec![None];
    while let Some(path) = stack.pop() {
        let entries = list_remote_entries(auth, remote_root, path).await?;
        for entry in entries {
            if entry.is_dir {
                stack.push(Some(entry.path));
                continue;
            }
            if keep.contains(&entry.path)
                || relative_path_matches_preserve_path(&entry.path, preserve_paths)
            {
                continue;
            }
            deletes.push(entry.path);
        }
    }
    deletes.sort();
    Ok(deletes)
}

async fn list_remote_entries(
    auth: &ResolvedClientAuth,
    remote_root: &str,
    path: Option<String>,
) -> Result<Vec<FileListEntry>> {
    let payload = FileListRequest {
        remote_root: remote_root.to_string(),
        path,
    };
    let response = post_json(auth, "/v1/file/list", &payload, true).await?;
    if response.status() == StatusCode::OK {
        let body: FileListResponse = response.json().await?;
        return Ok(body.entries);
    }
    if response.status() == StatusCode::BAD_REQUEST {
        let text = response.text().await.unwrap_or_default();
        if text.contains("No such file or directory")
            || text.contains("NotFound")
            || text.contains("os error 2")
        {
            return Ok(Vec::new());
        }
        return Err(anyhow!("request failed with status 400: {}", text));
    }
    Err(process_error_response(response).await)
}
