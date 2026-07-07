mod common;

use std::time::Duration;

use agentplane::protocol::{
    FileDeleteRequest, FileFindRequest, FileListRequest, FileReadRequest, FileStatRequest,
    FileUploadChunkRequest, FileUploadFinishRequest, FileUploadInitRequest,
    FileUploadStatusRequest, FileWriteRequest, SyncSessionInitRequest, SyncSessionReleaseRequest,
    SyncSessionStatusRequest,
};
use agentplane::server::{
    ServerState, handle_file_delete, handle_file_find, handle_file_list, handle_file_read,
    handle_file_stat, handle_file_upload_chunk, handle_file_upload_finish, handle_file_upload_init,
    handle_file_upload_status, handle_file_write, handle_sync_session_init,
    handle_sync_session_release, handle_sync_session_status,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::*;

#[tokio::test]
async fn sync_session_locks_remote_root_until_release() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    let first = handle_sync_session_init(
        &state,
        SyncSessionInitRequest {
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-a".to_string(),
            ttl_seconds: Some(60),
            lock_key: None,
        },
    )
    .await?;
    let second = handle_sync_session_init(
        &state,
        SyncSessionInitRequest {
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-b".to_string(),
            ttl_seconds: Some(60),
            lock_key: None,
        },
    )
    .await;
    assert!(second.is_err());
    assert!(format!("{:#}", second.unwrap_err()).contains("agent=agent-a"));

    let recovered = handle_sync_session_init(
        &state,
        SyncSessionInitRequest {
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-a".to_string(),
            ttl_seconds: Some(60),
            lock_key: None,
        },
    )
    .await?;
    assert_eq!(recovered.sync_session_id, first.sync_session_id);
    assert_eq!(recovered.lock_token, first.lock_token);

    handle_sync_session_release(
        &state,
        SyncSessionReleaseRequest {
            sync_session_id: recovered.sync_session_id,
            lock_token: recovered.lock_token,
        },
    )
    .await?;
    handle_sync_session_init(
        &state,
        SyncSessionInitRequest {
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-b".to_string(),
            ttl_seconds: Some(60),
            lock_key: None,
        },
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn sync_session_status_recovers_and_refreshes_existing_owner_only() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    let first = handle_sync_session_init(
        &state,
        SyncSessionInitRequest {
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-a".to_string(),
            ttl_seconds: Some(60),
            lock_key: Some("upload-key".to_string()),
        },
    )
    .await?;
    let recovered = handle_sync_session_status(
        &state,
        SyncSessionStatusRequest {
            sync_session_id: first.sync_session_id.clone(),
            lock_token: first.lock_token.clone(),
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-a".to_string(),
            lock_key: Some("upload-key".to_string()),
        },
    )
    .await?;
    assert_eq!(recovered.sync_session_id, first.sync_session_id);
    assert_eq!(recovered.lock_token, first.lock_token);
    assert_eq!(recovered.agent_id, "agent-a");
    assert_eq!(recovered.lock_key, "upload-key");

    let wrong_agent = handle_sync_session_status(
        &state,
        SyncSessionStatusRequest {
            sync_session_id: recovered.sync_session_id,
            lock_token: recovered.lock_token,
            remote_root: remote_root.path().display().to_string(),
            agent_id: "agent-b".to_string(),
            lock_key: Some("upload-key".to_string()),
        },
    )
    .await;
    assert!(wrong_agent.is_err());
    assert!(format!("{:#}", wrong_agent.unwrap_err()).contains("agent mismatch"));
    Ok(())
}

#[tokio::test]
async fn file_operations_round_trip_cover_write_read_list_find_delete_and_guards() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let other_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    handle_file_write(
        &state,
        FileWriteRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/bin/tool.sh".to_string(),
            content_b64: BASE64.encode("#!/usr/bin/env bash\necho hi\n"),
            executable: true,
            mode: None,
            create_parents: true,
            atomic: false,
            preserve_mode: false,
            checksum_sha256: None,
        },
    )
    .await?;
    handle_file_write(
        &state,
        FileWriteRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/readme.txt".to_string(),
            content_b64: BASE64.encode("hello world\n"),
            executable: false,
            mode: None,
            create_parents: true,
            atomic: false,
            preserve_mode: false,
            checksum_sha256: None,
        },
    )
    .await?;

    let read = handle_file_read(
        &state,
        FileReadRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/bin/tool.sh".to_string(),
        },
    )
    .await?;
    assert_eq!(
        decode_b64(&read.content_b64)?,
        "#!/usr/bin/env bash\necho hi\n"
    );
    assert!(read.executable);

    let list = handle_file_list(
        &state,
        FileListRequest {
            remote_root: remote_root.path().display().to_string(),
            path: Some("nested".to_string()),
        },
    )
    .await?;
    assert_eq!(list.entries.len(), 2);
    assert_eq!(list.entries[0].path, "nested/bin");
    assert!(list.entries[0].is_dir);
    assert_eq!(list.entries[1].path, "nested/readme.txt");
    assert!(!list.entries[1].is_dir);

    let find = handle_file_find(
        &state,
        FileFindRequest {
            remote_root: remote_root.path().display().to_string(),
            pattern: "tool".to_string(),
        },
    )
    .await?;
    assert_eq!(find.matches, vec!["nested/bin/tool.sh".to_string()]);

    handle_file_delete(
        &state,
        FileDeleteRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/bin".to_string(),
        },
    )
    .await?;
    assert!(!remote_root.path().join("nested/bin").exists());

    assert!(
        handle_file_write(
            &state,
            FileWriteRequest {
                remote_root: remote_root.path().display().to_string(),
                path: "../escape.txt".to_string(),
                content_b64: BASE64.encode("x"),
                executable: false,
                mode: None,
                create_parents: true,
                atomic: false,
                preserve_mode: false,
                checksum_sha256: None,
            },
        )
        .await
        .is_err()
    );

    assert!(
        handle_file_read(
            &state,
            FileReadRequest {
                remote_root: other_root.path().display().to_string(),
                path: "nope.txt".to_string(),
            },
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn file_upload_resume_round_trip_preserves_content_and_mode() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    let content = b"chunk-one\nchunk-two\n";
    let checksum = test_sha256_hex(content);

    let init = handle_file_upload_init(
        &state,
        FileUploadInitRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/upload.txt".to_string(),
            total_size: u64::try_from(content.len())?,
            chunk_size: 5,
            executable: true,
            create_parents: true,
            atomic: true,
            mode: Some(0o700),
            preserve_mode: false,
            checksum_sha256: checksum.clone(),
            resume: false,
            sync_session_id: None,
            lock_token: None,
        },
    )
    .await?;
    assert_eq!(init.received_bytes, 0);

    let first = &content[..5];
    let first_chunk = handle_file_upload_chunk(
        &state,
        FileUploadChunkRequest {
            upload_id: init.upload_id.clone(),
            offset: 0,
            data_b64: BASE64.encode(first),
            chunk_checksum_sha256: Some(test_sha256_hex(first)),
            sync_session_id: None,
            lock_token: None,
        },
    )
    .await?;
    assert_eq!(first_chunk.received_bytes, 5);

    let resumed = handle_file_upload_init(
        &state,
        FileUploadInitRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/upload.txt".to_string(),
            total_size: u64::try_from(content.len())?,
            chunk_size: 5,
            executable: true,
            create_parents: true,
            atomic: true,
            mode: Some(0o700),
            preserve_mode: false,
            checksum_sha256: checksum.clone(),
            resume: true,
            sync_session_id: None,
            lock_token: None,
        },
    )
    .await?;
    assert_eq!(resumed.received_bytes, 5);
    assert_eq!(resumed.upload_id, init.upload_id);

    let status = handle_file_upload_status(
        &state,
        FileUploadStatusRequest {
            upload_id: init.upload_id.clone(),
            sync_session_id: None,
            lock_token: None,
        },
    )
    .await?;
    assert_eq!(status.received_bytes, 5);

    let second = &content[5..];
    handle_file_upload_chunk(
        &state,
        FileUploadChunkRequest {
            upload_id: init.upload_id.clone(),
            offset: 5,
            data_b64: BASE64.encode(second),
            chunk_checksum_sha256: Some(test_sha256_hex(second)),
            sync_session_id: None,
            lock_token: None,
        },
    )
    .await?;
    handle_file_upload_finish(
        &state,
        FileUploadFinishRequest {
            upload_id: init.upload_id,
            sync_session_id: None,
            lock_token: None,
        },
    )
    .await?;

    let read = handle_file_read(
        &state,
        FileReadRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/upload.txt".to_string(),
        },
    )
    .await?;
    assert_eq!(
        decode_b64(&read.content_b64)?,
        String::from_utf8_lossy(content)
    );
    assert!(read.executable);

    let stat = handle_file_stat(
        &state,
        FileStatRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/upload.txt".to_string(),
        },
    )
    .await?;
    assert_eq!(stat.sha256.as_deref(), Some(checksum.as_str()));
    assert!(stat.executable);
    Ok(())
}

#[tokio::test]
async fn file_write_supports_atomic_mode_create_parents_and_checksum() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    assert!(
        handle_file_write(
            &state,
            FileWriteRequest {
                remote_root: remote_root.path().display().to_string(),
                path: "missing/parent.txt".to_string(),
                content_b64: BASE64.encode("parent\n"),
                executable: false,
                mode: None,
                create_parents: false,
                atomic: false,
                preserve_mode: false,
                checksum_sha256: None,
            },
        )
        .await
        .is_err()
    );

    handle_file_write(
        &state,
        FileWriteRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "missing/parent.txt".to_string(),
            content_b64: BASE64.encode("parent\n"),
            executable: false,
            mode: None,
            create_parents: true,
            atomic: false,
            preserve_mode: false,
            checksum_sha256: Some(format!("sha256:{}", test_sha256_hex(b"parent\n"))),
        },
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("missing/parent.txt"))?,
        "parent\n"
    );

    assert!(
        handle_file_write(
            &state,
            FileWriteRequest {
                remote_root: remote_root.path().display().to_string(),
                path: "bad-checksum.txt".to_string(),
                content_b64: BASE64.encode("content\n"),
                executable: false,
                mode: None,
                create_parents: true,
                atomic: true,
                preserve_mode: false,
                checksum_sha256: Some(test_sha256_hex(b"different\n")),
            },
        )
        .await
        .is_err()
    );
    assert!(!remote_root.path().join("bad-checksum.txt").exists());

    handle_file_write(
        &state,
        FileWriteRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "bin/tool".to_string(),
            content_b64: BASE64.encode("tool\n"),
            executable: false,
            mode: Some(0o700),
            create_parents: true,
            atomic: true,
            preserve_mode: false,
            checksum_sha256: Some(test_sha256_hex(b"tool\n")),
        },
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(remote_root.path().join("bin/tool"))?,
        "tool\n"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(remote_root.path().join("bin/tool"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    Ok(())
}

#[tokio::test]
async fn file_stat_reports_missing_file_directory_size_and_executable() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    let missing = handle_file_stat(
        &state,
        FileStatRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "missing.txt".to_string(),
        },
    )
    .await?;
    assert!(missing.ok);
    assert!(!missing.exists);
    assert_eq!(missing.file_type, "missing");
    assert_eq!(missing.size, None);
    assert_eq!(missing.modified_unix_ms, None);
    assert!(!missing.executable);
    assert_eq!(missing.sha256, None);

    handle_file_write(
        &state,
        FileWriteRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/bin/tool.sh".to_string(),
            content_b64: BASE64.encode("#!/bin/sh\necho hi\n"),
            executable: true,
            mode: None,
            create_parents: true,
            atomic: false,
            preserve_mode: false,
            checksum_sha256: None,
        },
    )
    .await?;

    let file = handle_file_stat(
        &state,
        FileStatRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/bin/tool.sh".to_string(),
        },
    )
    .await?;
    assert!(file.exists);
    assert_eq!(file.file_type, "file");
    assert_eq!(file.size, Some(18));
    assert!(file.modified_unix_ms.is_some());
    assert!(file.executable);
    assert_eq!(file.sha256, Some(test_sha256_hex(b"#!/bin/sh\necho hi\n")));

    let directory = handle_file_stat(
        &state,
        FileStatRequest {
            remote_root: remote_root.path().display().to_string(),
            path: "nested/bin".to_string(),
        },
    )
    .await?;
    assert!(directory.exists);
    assert_eq!(directory.file_type, "directory");
    assert!(directory.size.is_some());

    assert!(
        handle_file_stat(
            &state,
            FileStatRequest {
                remote_root: remote_root.path().display().to_string(),
                path: "../escape.txt".to_string(),
            },
        )
        .await
        .is_err()
    );
    Ok(())
}

#[test]
fn cli_health_and_file_round_trip_over_self_signed_https() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start_with_args_tls(remote_root.path(), token, &[], true)?;
    let ca_cert = harness
        .ca_cert_path
        .as_ref()
        .expect("missing ca cert path for tls harness");

    let health = run_cli(&[
        "health",
        "--server",
        &harness.base_url,
        "--tls-ca-cert",
        &ca_cert.display().to_string(),
    ])?;
    if !health.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&health.stderr));
    }
    assert!(health.status.success());

    let file_write = run_cli(&[
        "file-write",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--tls-ca-cert",
        &ca_cert.display().to_string(),
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "tls.txt",
        "--content",
        "secure",
    ])?;
    assert!(file_write.status.success());

    let file_read = run_cli(&[
        "file-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--tls-ca-cert",
        &ca_cert.display().to_string(),
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "tls.txt",
    ])?;
    assert!(file_read.status.success());
    let file_read_body: serde_json::Value = serde_json::from_slice(&file_read.stdout)?;
    assert_eq!(
        decode_b64(file_read_body["content_b64"].as_str().unwrap_or_default())?,
        "secure"
    );
    Ok(())
}

#[test]
fn cli_process_and_file_round_trip() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let file_write = run_cli(&[
        "file-write",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/input.txt",
        "--content",
        "from-cli",
    ])?;
    assert!(file_write.status.success());

    let file_list = run_cli(&[
        "file-list",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested",
    ])?;
    assert!(file_list.status.success());
    let file_list_body: serde_json::Value = serde_json::from_slice(&file_list.stdout)?;
    assert_eq!(file_list_body["entries"][0]["path"], "nested/input.txt");

    let file_read = run_cli(&[
        "file-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/input.txt",
    ])?;
    assert!(file_read.status.success());
    let file_read_body: serde_json::Value = serde_json::from_slice(&file_read.stdout)?;
    assert_eq!(
        decode_b64(file_read_body["content_b64"].as_str().unwrap_or_default())?,
        "from-cli"
    );

    let file_read_text = run_cli(&[
        "file-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/input.txt",
        "--text",
    ])?;
    assert!(file_read_text.status.success());
    assert_eq!(String::from_utf8(file_read_text.stdout)?, "from-cli");

    let local_upload = remote_root.path().join("local-upload.bin");
    std::fs::write(&local_upload, b"\xffbinary\n")?;
    let local_upload_sha = test_sha256_hex(b"\xffbinary\n");
    let file_write_local = run_cli(&[
        "file-write",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/upload.bin",
        "--from-local",
        &local_upload.display().to_string(),
        "--atomic",
        "--mode",
        "700",
        "--checksum",
        &format!("sha256:{local_upload_sha}"),
    ])?;
    assert!(file_write_local.status.success());
    let file_stat_upload = run_cli(&[
        "file-stat",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/upload.bin",
    ])?;
    assert!(file_stat_upload.status.success());
    let file_stat_upload_body: serde_json::Value =
        serde_json::from_slice(&file_stat_upload.stdout)?;
    assert_eq!(file_stat_upload_body["sha256"], local_upload_sha);
    assert!(
        file_stat_upload_body["executable"]
            .as_bool()
            .unwrap_or(false)
    );

    let file_find = run_cli(&[
        "file-find",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--pattern",
        "input",
    ])?;
    assert!(file_find.status.success());
    let file_find_body: serde_json::Value = serde_json::from_slice(&file_find.stdout)?;
    assert_eq!(file_find_body["matches"][0], "nested/input.txt");

    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-cat",
        "--cwd",
        "nested",
        "--env",
        "DEMO_FLAG=cli",
        "--output-bytes-limit",
        "4096",
        "--pipe-stdin",
        "--",
        "bash",
        "-lc",
        "printf '%s:' \"$DEMO_FLAG\" && cat input.txt -",
    ])?;
    assert!(process_start.status.success());

    let process_write = run_cli(&[
        "process-write",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "cli-cat",
        "--data",
        ":stdin",
        "--close-stdin",
    ])?;
    assert!(process_write.status.success());

    let mut process_stdout = String::new();
    let mut exited = false;
    let mut after_seq = 0u64;
    for _ in 0..40 {
        let process_read = run_cli(&[
            "process-read",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "cli-cat",
            "--after-seq",
            &after_seq.to_string(),
            "--wait-ms",
            "100",
        ])?;
        assert!(process_read.status.success());
        let process_read_body: serde_json::Value = serde_json::from_slice(&process_read.stdout)?;
        for chunk in process_read_body["chunks"].as_array().into_iter().flatten() {
            if chunk["stream"] == "stdout" {
                process_stdout
                    .push_str(&decode_b64(chunk["data_b64"].as_str().unwrap_or_default())?);
            }
        }
        after_seq = process_read_body["next_seq"].as_u64().unwrap_or(after_seq);
        exited = process_read_body["exited"].as_bool().unwrap_or(false);
        if exited {
            assert_eq!(process_read_body["exit_code"], 0);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited);
    assert!(process_stdout.contains("cli:from-cli:stdin"));

    let follow_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-follow",
        "--output-bytes-limit",
        "4096",
        "--",
        "bash",
        "-lc",
        "printf 'one\\n'; sleep 0.2; printf 'two\\n'",
    ])?;
    assert!(follow_start.status.success());

    let follow_read = run_cli(&[
        "process-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "cli-follow",
        "--follow",
        "--text",
        "--wait-ms",
        "100",
    ])?;
    assert!(follow_read.status.success());
    let follow_stdout = String::from_utf8(follow_read.stdout)?;
    assert!(follow_stdout.contains("one"));
    assert!(follow_stdout.contains("two"));

    let terminate_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-sleep",
        "--",
        "bash",
        "-lc",
        "sleep 30",
    ])?;
    assert!(terminate_start.status.success());

    let terminate = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "cli-sleep",
    ])?;
    assert!(terminate.status.success());

    let file_delete = run_cli(&[
        "file-delete",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/input.txt",
    ])?;
    assert!(file_delete.status.success());
    assert!(!remote_root.path().join("nested/input.txt").exists());
    Ok(())
}

#[test]
fn cli_file_upload_transfers_large_local_file_in_chunks() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let local_upload = remote_root.path().join("large-local-upload.bin");
    let content = b"0123456789abcdef0123456789abcdef";
    std::fs::write(&local_upload, content)?;
    let checksum = test_sha256_hex(content);

    let file_upload = run_cli(&[
        "file-upload",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/chunked.bin",
        "--from-local",
        &local_upload.display().to_string(),
        "--chunk-size",
        "7",
        "--atomic",
        "--mode",
        "700",
        "--checksum",
        &format!("sha256:{checksum}"),
        "--resume",
        "--lock-key",
        "nested/chunked.bin",
    ])?;
    assert!(file_upload.status.success());

    let file_read = run_cli(&[
        "file-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/chunked.bin",
    ])?;
    assert!(file_read.status.success());
    let file_read_body: serde_json::Value = serde_json::from_slice(&file_read.stdout)?;
    assert_eq!(
        decode_b64(file_read_body["content_b64"].as_str().unwrap_or_default())?,
        String::from_utf8_lossy(content)
    );

    let file_stat = run_cli(&[
        "file-stat",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "nested/chunked.bin",
    ])?;
    assert!(file_stat.status.success());
    let file_stat_body: serde_json::Value = serde_json::from_slice(&file_stat.stdout)?;
    assert_eq!(file_stat_body["sha256"], checksum);
    assert!(file_stat_body["executable"].as_bool().unwrap_or(false));
    Ok(())
}

#[test]
fn cli_file_stat_and_wait_cover_min_bytes_stable_and_timeout() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let missing_stat = run_cli(&[
        "file-stat",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "missing.txt",
    ])?;
    assert!(missing_stat.status.success());
    let missing_body: serde_json::Value = serde_json::from_slice(&missing_stat.stdout)?;
    assert_eq!(missing_body["exists"], false);
    assert_eq!(missing_body["file_type"], "missing");

    let delayed_path = remote_root.path().join("delayed.txt");
    let delayed_writer = std::thread::spawn(move || -> Result<()> {
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(delayed_path, "abc")?;
        Ok(())
    });
    let wait_min_bytes = run_cli(&[
        "file-wait",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "delayed.txt",
        "--min-bytes",
        "3",
        "--timeout-seconds",
        "2",
    ])?;
    delayed_writer.join().expect("delayed writer panicked")?;
    assert!(
        wait_min_bytes.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait_min_bytes.stderr)
    );
    let wait_body: serde_json::Value = serde_json::from_slice(&wait_min_bytes.stdout)?;
    assert_eq!(wait_body["exists"], true);
    assert_eq!(wait_body["file_type"], "file");
    assert_eq!(wait_body["size"], 3);

    std::fs::write(remote_root.path().join("stable.txt"), "a")?;
    let stable_path = remote_root.path().join("stable.txt");
    let stable_writer = std::thread::spawn(move || -> Result<()> {
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(stable_path, "abcdef")?;
        Ok(())
    });
    let wait_stable = run_cli(&[
        "file-wait",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "stable.txt",
        "--stable-ms",
        "300",
        "--timeout-seconds",
        "2",
    ])?;
    stable_writer.join().expect("stable writer panicked")?;
    assert!(
        wait_stable.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait_stable.stderr)
    );
    let stable_body: serde_json::Value = serde_json::from_slice(&wait_stable.stdout)?;
    assert_eq!(stable_body["size"], 6);

    let timeout = run_cli(&[
        "file-wait",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--path",
        "never.txt",
        "--timeout-seconds",
        "0",
    ])?;
    assert_eq!(timeout.status.code(), Some(1));
    let timeout_stderr = String::from_utf8(timeout.stderr)?;
    assert!(timeout_stderr.contains("file-wait timed out"));
    assert!(timeout_stderr.contains("\"file_type\": \"missing\""));
    Ok(())
}
