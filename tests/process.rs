mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use agentplane::protocol::{
    ProcessCleanupRequest, ProcessGetRequest, ProcessOutputStream, ProcessReadRequest,
    ProcessStartRequest, ProcessTerminateRequest, ProcessWriteRequest,
};
use agentplane::server::{
    ServerLimits, ServerState, handle_process_cleanup, handle_process_get, handle_process_list,
    handle_process_read, handle_process_start, handle_process_terminate, handle_process_write,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::*;

#[tokio::test]
async fn process_session_round_trip_supports_stdin_env_cwd_and_timeout() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    tokio::fs::create_dir_all(remote_root.path().join("nested")).await?;

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "stdin-cat".to_string(),
            command: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "printf '%s:%s:' \"$PWD\" \"$DEMO_FLAG\" && cat".to_string(),
            ],
            cwd: Some("nested".to_string()),
            env: Some(
                std::iter::once(("DEMO_FLAG".to_string(), Some("set".to_string()))).collect(),
            ),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: None,
            save_output_path: None,
            pipe_stdin: true,
            kill_tree_on_terminate: false,
        },
    )
    .await?;

    handle_process_write(
        &state,
        ProcessWriteRequest {
            process_id: "stdin-cat".to_string(),
            data_b64: BASE64.encode("hello\n"),
            close_stdin: true,
        },
    )
    .await?;

    let mut combined_stdout = String::new();
    let mut exited = false;
    for _ in 0..40 {
        let response = handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: "stdin-cat".to_string(),
                after_seq: None,
                max_bytes: None,
                wait_ms: Some(100),
            },
        )
        .await?;
        combined_stdout.push_str(&decode_process_chunks(
            &response.chunks,
            ProcessOutputStream::Stdout,
        )?);
        exited = response.exited;
        if exited {
            assert_eq!(response.exit_code, Some(0));
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(exited);
    assert!(combined_stdout.contains("/nested:set:hello"));

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "timeout".to_string(),
            command: vec!["bash".to_string(), "-lc".to_string(), "sleep 2".to_string()],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(1),
            output_bytes_limit: None,
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let timed_out = handle_process_read(
        &state,
        ProcessReadRequest {
            process_id: "timeout".to_string(),
            after_seq: None,
            max_bytes: None,
            wait_ms: Some(100),
        },
    )
    .await?;
    assert!(timed_out.exited);
    assert_eq!(timed_out.exit_code, Some(124));
    assert_eq!(
        timed_out.failure.as_deref(),
        Some("process timed out after 1 seconds")
    );

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "terminate-me".to_string(),
            command: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "sleep 30".to_string(),
            ],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: None,
            output_bytes_limit: None,
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    handle_process_terminate(
        &state,
        ProcessTerminateRequest {
            process_id: "terminate-me".to_string(),
            tree: false,
        },
    )
    .await?;
    let terminated = handle_process_read(
        &state,
        ProcessReadRequest {
            process_id: "terminate-me".to_string(),
            after_seq: None,
            max_bytes: None,
            wait_ms: Some(100),
        },
    )
    .await?;
    assert!(terminated.exited);
    assert_eq!(terminated.exit_code, Some(1));
    assert_eq!(
        terminated.failure.as_deref(),
        Some("process terminated by client")
    );
    assert!(
        handle_process_terminate(
            &state,
            ProcessTerminateRequest {
                process_id: "terminate-me".to_string(),
                tree: false,
            },
        )
        .await
        .is_ok()
    );

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "write-after-exit".to_string(),
            command: vec!["bash".to_string(), "-lc".to_string(), "cat".to_string()],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: None,
            save_output_path: None,
            pipe_stdin: true,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    handle_process_write(
        &state,
        ProcessWriteRequest {
            process_id: "write-after-exit".to_string(),
            data_b64: BASE64.encode("done\n"),
            close_stdin: true,
        },
    )
    .await?;
    for _ in 0..20 {
        let response = handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: "write-after-exit".to_string(),
                after_seq: None,
                max_bytes: None,
                wait_ms: Some(50),
            },
        )
        .await?;
        if response.exited {
            break;
        }
    }
    assert!(
        handle_process_write(
            &state,
            ProcessWriteRequest {
                process_id: "write-after-exit".to_string(),
                data_b64: BASE64.encode("again"),
                close_stdin: false,
            },
        )
        .await
        .is_err()
    );

    assert!(
        handle_process_start(
            &state,
            ProcessStartRequest {
                remote_root: remote_root.path().display().to_string(),
                process_id: "empty".to_string(),
                command: Vec::new(),
                cwd: None,
                env: Some(Default::default()),
                claims: Vec::new(),
                timeout_seconds: None,
                output_bytes_limit: None,
                save_output_path: None,
                pipe_stdin: false,
                kill_tree_on_terminate: false,
            },
        )
        .await
        .is_err()
    );

    assert!(
        handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: "missing".to_string(),
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            },
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn process_cleanup_dry_run_reports_without_signaling_and_kill_requires_signal() -> Result<()>
{
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    let marker = format!("cc_cleanup_marker_{}", std::process::id());
    let mut child = Command::new("python3")
        .args(["-c", "import time; time.sleep(30)", &marker])
        .spawn()?;

    let matched = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let response = handle_process_cleanup(
                &state,
                ProcessCleanupRequest {
                    process_match: marker.clone(),
                    dry_run: true,
                    kill: false,
                    signal: None,
                },
            )
            .await?;
            if response
                .matched
                .iter()
                .any(|process| process.pid == child.id() as i32)
            {
                break response;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("cleanup dry-run did not match test process");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    assert!(matched.dry_run);
    assert!(child.try_wait()?.is_none());
    assert!(matched.agent_hint.contains("Dry run only"));

    let missing_signal = handle_process_cleanup(
        &state,
        ProcessCleanupRequest {
            process_match: marker.clone(),
            dry_run: false,
            kill: true,
            signal: None,
        },
    )
    .await;
    assert!(missing_signal.is_err());
    assert!(child.try_wait()?.is_none());

    let killed = handle_process_cleanup(
        &state,
        ProcessCleanupRequest {
            process_match: marker,
            dry_run: false,
            kill: true,
            signal: Some("TERM".to_string()),
        },
    )
    .await?;
    assert!(!killed.dry_run);
    assert!(
        killed
            .signaled
            .iter()
            .any(|process| process.pid == child.id() as i32)
    );
    wait_for_child_exit(&mut child)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_process_cleanup_dry_run_and_explicit_term_signal() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let marker = format!("cc_cleanup_cli_marker_{}", std::process::id());
    let mut child = Command::new("python3")
        .args(["-c", "import time; time.sleep(30)", &marker])
        .spawn()?;
    let child_pid = child.id() as i64;

    let result = (|| -> Result<()> {
        let mut dry_run_body = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let dry_run = run_cli(&[
                "process-cleanup",
                "--server",
                &harness.base_url,
                "--token",
                token,
                "--match",
                &marker,
                "--dry-run",
            ])?;
            assert!(
                dry_run.status.success(),
                "stderr: {}",
                String::from_utf8_lossy(&dry_run.stderr)
            );
            let body: serde_json::Value = serde_json::from_slice(&dry_run.stdout)?;
            if body["matched"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|process| process["pid"].as_i64() == Some(child_pid))
            {
                dry_run_body = Some(body);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let body = dry_run_body
            .ok_or_else(|| anyhow::anyhow!("process-cleanup dry-run did not match test process"))?;
        assert_eq!(body["dry_run"], true);
        assert!(body["signaled"].as_array().unwrap().is_empty());
        assert!(child.try_wait()?.is_none());

        let dry_run_text = run_cli(&[
            "process-cleanup",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--match",
            &marker,
            "--dry-run",
            "--text",
        ])?;
        assert!(dry_run_text.status.success());
        let dry_run_stdout = String::from_utf8(dry_run_text.stdout)?;
        assert!(dry_run_stdout.contains("Dry run: no processes were signaled."));
        assert!(dry_run_stdout.contains(&format!("pid={child_pid}")));
        assert!(child.try_wait()?.is_none());

        let missing_signal = run_cli(&[
            "process-cleanup",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--match",
            &marker,
            "--kill",
        ])?;
        assert!(!missing_signal.status.success());
        assert!(
            String::from_utf8_lossy(&missing_signal.stderr).contains("requires --signal"),
            "stderr: {}",
            String::from_utf8_lossy(&missing_signal.stderr)
        );
        assert!(child.try_wait()?.is_none());

        let killed = run_cli(&[
            "process-cleanup",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--match",
            &marker,
            "--kill",
            "--signal",
            "TERM",
        ])?;
        assert!(
            killed.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&killed.stderr)
        );
        let killed_body: serde_json::Value = serde_json::from_slice(&killed.stdout)?;
        assert_eq!(killed_body["dry_run"], false);
        assert_eq!(killed_body["signal"], "TERM");
        assert!(
            killed_body["signaled"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|process| process["pid"].as_i64() == Some(child_pid))
        );
        wait_for_child_exit(&mut child)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[tokio::test]
async fn process_read_supports_sequence_cursor_wait_and_binary_output() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "chunked".to_string(),
            command: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "printf 'first\\n'; sleep 0.2; printf 'second\\n'; sleep 0.2".to_string(),
            ],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: None,
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;

    let first = handle_process_read(
        &state,
        ProcessReadRequest {
            process_id: "chunked".to_string(),
            after_seq: None,
            max_bytes: Some(1024),
            wait_ms: Some(1000),
        },
    )
    .await?;
    assert!(!first.chunks.is_empty());
    let first_stdout = decode_process_chunks(&first.chunks, ProcessOutputStream::Stdout)?;
    assert!(first_stdout.contains("first"));
    let first_next = first.next_seq;

    let second = handle_process_read(
        &state,
        ProcessReadRequest {
            process_id: "chunked".to_string(),
            after_seq: Some(first_next),
            max_bytes: Some(1024),
            wait_ms: Some(1000),
        },
    )
    .await?;
    let second_stdout = decode_process_chunks(&second.chunks, ProcessOutputStream::Stdout)?;
    assert!(second_stdout.contains("second"));
    assert!(second.next_seq >= first_next);
    assert_eq!(
        second.next_seq,
        second
            .chunks
            .last()
            .map(|chunk| chunk.seq + 1)
            .unwrap_or(first_next)
    );

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "binary".to_string(),
            command: vec![
                "python3".to_string(),
                "-c".to_string(),
                "import sys; sys.stdout.buffer.write(b'\\xff\\x00A')".to_string(),
            ],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: None,
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    let binary = handle_process_read(
        &state,
        ProcessReadRequest {
            process_id: "binary".to_string(),
            after_seq: None,
            max_bytes: Some(1024),
            wait_ms: Some(1000),
        },
    )
    .await?;
    let raw = binary
        .chunks
        .iter()
        .find(|chunk| chunk.stream == ProcessOutputStream::Stdout)
        .map(|chunk| BASE64.decode(chunk.data_b64.as_bytes()))
        .transpose()?
        .unwrap_or_default();
    assert_eq!(raw, vec![0xff, 0x00, b'A']);
    Ok(())
}

#[tokio::test]
async fn process_output_limit_truncates_old_chunks_and_reports_cursor_expiry() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "truncated".to_string(),
            command: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "for _ in 1 2 3 4 5; do printf '1234567890123456'; sleep 0.05; done".to_string(),
            ],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(8),
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;

    let mut response = None;
    for _ in 0..20 {
        let current = handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: "truncated".to_string(),
                after_seq: Some(0),
                max_bytes: Some(4096),
                wait_ms: Some(100),
            },
        )
        .await?;
        if current.truncated && current.cursor_expired {
            response = Some(current);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let response = response.expect("expected truncated output state");
    assert!(response.truncated);
    assert!(response.cursor_expired);
    assert!(response.available_from_seq > 0);
    let stdout = decode_process_chunks(&response.chunks, ProcessOutputStream::Stdout)?;
    assert!(!stdout.is_empty());
    Ok(())
}

#[tokio::test]
async fn process_save_output_path_preserves_full_output_and_validates_config() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "printf 'stdout-full-line-0001\\n'; printf 'stderr-full-line-0002\\n' >&2; \
         for i in $(seq 1 20); do printf 'payload-%04d-xxxxxxxxxxxxxxxx\\n' \"$i\"; done"
            .to_string(),
    ];

    let first = handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "saved-output".to_string(),
            command: command.clone(),
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(32),
            save_output_path: Some("logs/full.log".to_string()),
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    assert!(first.created);

    let mut exited = false;
    let mut saw_truncation = false;
    for _ in 0..40 {
        let read = handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: "saved-output".to_string(),
                after_seq: Some(0),
                max_bytes: Some(4096),
                wait_ms: Some(100),
            },
        )
        .await?;
        saw_truncation |= read.truncated;
        exited = read.exited;
        if exited {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(exited);
    assert!(saw_truncation);

    let saved = tokio::fs::read_to_string(remote_root.path().join("logs/full.log")).await?;
    assert!(saved.contains("stdout-full-line-0001"));
    assert!(saved.contains("stderr-full-line-0002"));
    assert!(saved.contains("payload-0020-xxxxxxxxxxxxxxxx"));

    let info = handle_process_get(
        &state,
        ProcessGetRequest {
            process_id: "saved-output".to_string(),
        },
    )
    .await?;
    assert_eq!(
        info.process.save_output_path.as_deref(),
        Some("logs/full.log")
    );

    let second = handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "saved-output".to_string(),
            command: command.clone(),
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(32),
            save_output_path: Some("logs/full.log".to_string()),
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    assert!(second.already_exists);
    let still_saved = tokio::fs::read_to_string(remote_root.path().join("logs/full.log")).await?;
    assert_eq!(saved, still_saved);

    let changed_save_path = handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "saved-output".to_string(),
            command,
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(32),
            save_output_path: Some("logs/other.log".to_string()),
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await;
    assert!(changed_save_path.is_err());

    let escaped = handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "escaped-output".to_string(),
            command: vec!["bash".to_string(), "-lc".to_string(), "true".to_string()],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(32),
            save_output_path: Some("../escape.log".to_string()),
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await;
    assert!(escaped.is_err());
    assert!(!remote_root.path().join("../escape.log").exists());
    Ok(())
}

#[tokio::test]
async fn process_resource_limits_reject_excessive_usage_and_prune_finished_entries() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let limits = ServerLimits {
        max_processes: 1,
        max_zombie_processes: 1,
        default_process_output_limit_bytes: 1024,
        max_process_output_limit_bytes: 2048,
        default_process_read_max_bytes: 128,
        max_process_read_max_bytes: 256,
        max_stdin_write_bytes: 4,
        max_process_timeout_seconds: 2,
        zombie_ttl_seconds: 0,
        default_kill_tree_on_terminate: false,
    };
    let state = ServerState::with_limits(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
        limits,
    );

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "one".to_string(),
            command: vec!["bash".to_string(), "-lc".to_string(), "sleep 1".to_string()],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(1),
            output_bytes_limit: Some(128),
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;

    assert!(
        handle_process_start(
            &state,
            ProcessStartRequest {
                remote_root: remote_root.path().display().to_string(),
                process_id: "two".to_string(),
                command: vec!["bash".to_string(), "-lc".to_string(), "sleep 1".to_string()],
                cwd: None,
                env: Some(Default::default()),
                claims: Vec::new(),
                timeout_seconds: Some(1),
                output_bytes_limit: Some(128),
                save_output_path: None,
                pipe_stdin: false,
                kill_tree_on_terminate: false,
            },
        )
        .await
        .is_err()
    );

    tokio::time::sleep(Duration::from_millis(1200)).await;
    let _ = handle_process_read(
        &state,
        ProcessReadRequest {
            process_id: "one".to_string(),
            after_seq: None,
            max_bytes: Some(4096),
            wait_ms: Some(100),
        },
    )
    .await?;

    handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "three".to_string(),
            command: vec!["bash".to_string(), "-lc".to_string(), "cat".to_string()],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(1),
            output_bytes_limit: Some(128),
            save_output_path: None,
            pipe_stdin: true,
            kill_tree_on_terminate: false,
        },
    )
    .await?;

    assert!(
        handle_process_write(
            &state,
            ProcessWriteRequest {
                process_id: "three".to_string(),
                data_b64: BASE64.encode("12345"),
                close_stdin: false,
            },
        )
        .await
        .is_err()
    );

    assert!(
        handle_process_start(
            &state,
            ProcessStartRequest {
                remote_root: remote_root.path().display().to_string(),
                process_id: "too-long-timeout".to_string(),
                command: vec!["bash".to_string(), "-lc".to_string(), "true".to_string()],
                cwd: None,
                env: Some(Default::default()),
                claims: Vec::new(),
                timeout_seconds: Some(10),
                output_bytes_limit: Some(128),
                save_output_path: None,
                pipe_stdin: false,
                kill_tree_on_terminate: false,
            },
        )
        .await
        .is_err()
    );

    Ok(())
}

#[tokio::test]
async fn process_start_is_idempotent_and_supports_recovery_listing() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    let first = handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "recoverable".to_string(),
            command: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "printf 'boot\\n'; sleep 0.2; printf 'done\\n'".to_string(),
            ],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(1024),
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    assert!(first.created);
    assert!(!first.already_exists);

    let second = handle_process_start(
        &state,
        ProcessStartRequest {
            remote_root: remote_root.path().display().to_string(),
            process_id: "recoverable".to_string(),
            command: vec![
                "bash".to_string(),
                "-lc".to_string(),
                "printf 'boot\\n'; sleep 0.2; printf 'done\\n'".to_string(),
            ],
            cwd: None,
            env: Some(Default::default()),
            claims: Vec::new(),
            timeout_seconds: Some(5),
            output_bytes_limit: Some(1024),
            save_output_path: None,
            pipe_stdin: false,
            kill_tree_on_terminate: false,
        },
    )
    .await?;
    assert!(!second.created);
    assert!(second.already_exists);

    assert!(
        handle_process_start(
            &state,
            ProcessStartRequest {
                remote_root: remote_root.path().display().to_string(),
                process_id: "recoverable".to_string(),
                command: vec![
                    "bash".to_string(),
                    "-lc".to_string(),
                    "echo changed".to_string()
                ],
                cwd: None,
                env: Some(Default::default()),
                claims: Vec::new(),
                timeout_seconds: Some(5),
                output_bytes_limit: Some(1024),
                save_output_path: None,
                pipe_stdin: false,
                kill_tree_on_terminate: false,
            },
        )
        .await
        .is_err()
    );

    let listed = handle_process_list(&state).await?;
    assert_eq!(listed.processes.len(), 1);
    assert_eq!(listed.processes[0].process_id, "recoverable");

    let fetched = handle_process_get(
        &state,
        ProcessGetRequest {
            process_id: "recoverable".to_string(),
        },
    )
    .await?;
    assert_eq!(fetched.process.process_id, "recoverable");
    assert_eq!(
        fetched.process.command[2],
        "printf 'boot\\n'; sleep 0.2; printf 'done\\n'"
    );

    let mut after_seq = 0;
    let mut stdout = String::new();
    let mut exited = false;
    for _ in 0..40 {
        let read = handle_process_read(
            &state,
            ProcessReadRequest {
                process_id: "recoverable".to_string(),
                after_seq: Some(after_seq),
                max_bytes: Some(1024),
                wait_ms: Some(100),
            },
        )
        .await?;
        stdout.push_str(&decode_process_chunks(
            &read.chunks,
            ProcessOutputStream::Stdout,
        )?);
        after_seq = read.next_seq;
        exited = read.exited;
        if exited {
            break;
        }
    }
    assert!(exited);
    assert!(stdout.contains("boot"));
    assert!(stdout.contains("done"));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let finished = handle_process_get(
        &state,
        ProcessGetRequest {
            process_id: "recoverable".to_string(),
        },
    )
    .await?;
    assert!(finished.process.exited);
    assert!(finished.process.output_retained);
    Ok(())
}

#[test]
fn cli_process_run_streams_output_and_returns_remote_exit_code() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let profile_dir = tempfile::tempdir()?;
    let profile_path = profile_dir.path().join("cc.env");
    std::fs::write(
        &profile_path,
        format!(
            "\
AP_SERVER={}
AP_TOKEN={}
AP_REMOTE_ROOT={}
",
            harness.base_url,
            token,
            remote_root.path().display()
        ),
    )?;

    let output = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-run",
        "--process-id",
        "cli-process-run",
        "--wait-ms",
        "100",
        "--",
        "bash",
        "-lc",
        "printf 'run-ok\\n'; exit 7",
    ])?;
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(String::from_utf8(output.stdout)?, "run-ok\n");
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn cli_process_help_recommends_run_vs_start_usage() -> Result<()> {
    let start_help = run_cli(&["process-start", "--help"])?;
    assert!(start_help.status.success());
    let start_stdout = String::from_utf8(start_help.stdout)?;
    assert!(start_stdout.contains("Use process-start for long-running producers"));
    assert!(start_stdout.contains("Use process-run for short build/check commands"));

    let run_help = run_cli(&["process-run", "--help"])?;
    assert!(run_help.status.success());
    let run_stdout = String::from_utf8(run_help.stdout)?;
    assert!(run_stdout.contains("Use process-run for short build/check commands"));
    assert!(run_stdout.contains("Use process-start for long-running producers"));
    Ok(())
}

#[test]
fn cli_process_run_save_output_path_writes_full_output() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let output = run_cli(&[
        "process-run",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-process-run-save-output",
        "--wait-ms",
        "100",
        "--save-output-path",
        "logs/run.log",
        "--",
        "bash",
        "-lc",
        "printf 'cli-stdout\\n'; printf 'cli-stderr\\n' >&2; exit 7",
    ])?;
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(String::from_utf8(output.stdout)?, "cli-stdout\n");
    assert_eq!(String::from_utf8(output.stderr)?, "cli-stderr\n");

    let saved = std::fs::read_to_string(remote_root.path().join("logs/run.log"))?;
    assert!(saved.contains("cli-stdout"));
    assert!(saved.contains("cli-stderr"));
    Ok(())
}

#[test]
fn cli_process_run_tail_on_error_prints_retained_tail() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let command = "for i in $(seq 1 200); do printf 'line-%05d\\n' \"$i\"; done";
    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-process-run-expired-cursor",
        "--output-bytes-limit",
        "64",
        "--",
        "bash",
        "-lc",
        command,
    ])?;
    assert!(
        started.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    std::thread::sleep(Duration::from_millis(300));

    let output = run_cli(&[
        "process-run",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-process-run-tail",
        "--wait-ms",
        "100",
        "--json",
        "--tail-on-error",
        "32",
        "--",
        "bash",
        "-lc",
        "printf 'alpha\\nbeta\\ngamma\\n'; exit 7",
    ])?;
    assert_eq!(output.status.code(), Some(7));
    let stdout_body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(stdout_body["exit_code"], 7);
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("tail-on-error"));
    assert!(stderr.contains("gamma"));
    Ok(())
}

#[test]
fn cli_process_run_warns_when_output_cursor_expires() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &[
            "--default-process-output-limit-bytes",
            "64",
            "--max-process-output-limit-bytes",
            "64",
            "--default-process-read-max-bytes",
            "64",
            "--max-process-read-max-bytes",
            "64",
        ],
    )?;

    let command = "for i in $(seq 1 20); do printf 'line-%05d-%080d\\n' \"$i\" 0; sleep 0.02; done";
    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-process-run-expired-cursor",
        "--output-bytes-limit",
        "64",
        "--",
        "bash",
        "-lc",
        command,
    ])?;
    assert!(
        started.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut cursor_expired = false;
    while Instant::now() < deadline {
        let read = run_cli(&[
            "process-read",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "cli-process-run-expired-cursor",
            "--after-seq",
            "0",
            "--max-bytes",
            "64",
        ])?;
        assert!(
            read.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&read.stderr)
        );
        let body: serde_json::Value = serde_json::from_slice(&read.stdout)?;
        if body["cursor_expired"].as_bool() == Some(true) {
            cursor_expired = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        cursor_expired,
        "process output cursor did not expire before process-run"
    );

    let output = run_cli(&[
        "process-run",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-process-run-expired-cursor",
        "--wait-ms",
        "100",
        "--output-bytes-limit",
        "64",
        "--max-bytes",
        "64",
        "--",
        "bash",
        "-lc",
        command,
    ])?;
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("cursor expired"), "stderr: {stderr}");
    assert!(
        stderr.contains("--output-bytes-limit"),
        "stderr should include retention hint: {stderr}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_process_terminate_tree_kills_background_child_without_touching_other_groups() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let child_pid_file = remote_root.path().join("tree-child.pid");

    let tree_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "tree-wrapper",
        "--kill-tree-on-terminate",
        "--",
        "bash",
        "-lc",
        "bash -c 'trap \"\" TERM; echo $$ > tree-child.pid; sleep 30' & wait",
    ])?;
    assert!(
        tree_start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tree_start.stderr)
    );

    let other_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "other-wrapper",
        "--",
        "bash",
        "-lc",
        "sleep 30",
    ])?;
    assert!(other_start.status.success());

    let child_pid = wait_for_pid_file(&child_pid_file)?;
    let tree_get = run_cli(&[
        "process-get",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "tree-wrapper",
    ])?;
    assert!(tree_get.status.success());
    let tree_body: serde_json::Value = serde_json::from_slice(&tree_get.stdout)?;
    assert_eq!(tree_body["process"]["kill_tree_on_terminate"], true);
    assert!(tree_body["process"]["process_group_id"].as_i64().is_some());

    let terminate = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "tree-wrapper",
        "--tree",
    ])?;
    assert!(
        terminate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&terminate.stderr)
    );
    assert_process_exits(child_pid)?;

    let other_get = run_cli(&[
        "process-get",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "other-wrapper",
    ])?;
    assert!(other_get.status.success());
    let other_body: serde_json::Value = serde_json::from_slice(&other_get.stdout)?;
    assert_eq!(other_body["process"]["exited"], false);

    let _ = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "other-wrapper",
    ])?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_process_get_reports_children_running_after_wrapper_exits() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let child_pid_file = remote_root.path().join("exited-wrapper-child.pid");

    let start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "exited-wrapper",
        "--kill-tree-on-terminate",
        "--",
        "bash",
        "-lc",
        "sleep 30 & echo $! > exited-wrapper-child.pid; exit 0",
    ])?;
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let child_pid = wait_for_pid_file(&child_pid_file)?;
    assert_process_running(child_pid)?;

    let mut process_body = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let get = run_cli(&[
            "process-get",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "exited-wrapper",
        ])?;
        assert!(get.status.success());
        let body: serde_json::Value = serde_json::from_slice(&get.stdout)?;
        if body["process"]["exited"] == true {
            process_body = Some(body);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let body = process_body.ok_or_else(|| anyhow::anyhow!("wrapper did not exit"))?;
    assert_eq!(body["process"]["kill_tree_on_terminate"], true);
    assert_eq!(body["process"]["children_running"], true);

    let terminate = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "exited-wrapper",
        "--tree",
    ])?;
    assert!(
        terminate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&terminate.stderr)
    );
    assert_process_exits(child_pid)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_server_default_kill_tree_on_terminate_cleans_process_group() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--default-kill-tree-on-terminate"],
    )?;
    let child_pid_file = remote_root.path().join("default-tree-child.pid");

    let start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "default-tree-wrapper",
        "--",
        "bash",
        "-lc",
        "sleep 30 & echo $! > default-tree-child.pid; wait",
    ])?;
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let child_pid = wait_for_pid_file(&child_pid_file)?;
    let get = run_cli(&[
        "process-get",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "default-tree-wrapper",
    ])?;
    assert!(get.status.success());
    let body: serde_json::Value = serde_json::from_slice(&get.stdout)?;
    assert_eq!(body["process"]["kill_tree_on_terminate"], true);
    assert!(body["process"]["process_group_id"].as_i64().is_some());

    let terminate = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "default-tree-wrapper",
    ])?;
    assert!(
        terminate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&terminate.stderr)
    );
    assert_process_exits(child_pid)?;
    Ok(())
}

#[test]
fn cli_large_stdin_and_large_stdout_round_trip() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let large_input = "abc123XYZ\n".repeat(6000);
    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cli-large-io",
        "--output-bytes-limit",
        "2097152",
        "--pipe-stdin",
        "--",
        "bash",
        "-lc",
        "cat >/tmp/in.txt; wc -c /tmp/in.txt; yes PAYLOAD | head -n 20000",
    ])?;
    assert!(process_start.status.success());

    let process_write = run_cli(&[
        "process-write",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "cli-large-io",
        "--data",
        &large_input,
        "--close-stdin",
    ])?;
    assert!(process_write.status.success());

    let expected_input_len = large_input.len().to_string();
    let mut stdout = String::new();
    let mut after_seq = 0u64;
    let mut exited = false;
    let mut saw_cursor_advance = false;
    for _ in 0..120 {
        let process_read = run_cli(&[
            "process-read",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "cli-large-io",
            "--after-seq",
            &after_seq.to_string(),
            "--max-bytes",
            "32768",
            "--wait-ms",
            "100",
        ])?;
        assert!(process_read.status.success());
        let body: serde_json::Value = serde_json::from_slice(&process_read.stdout)?;
        let next_seq = body["next_seq"].as_u64().unwrap_or(after_seq);
        if next_seq > after_seq {
            saw_cursor_advance = true;
        }
        after_seq = next_seq;
        for chunk in body["chunks"].as_array().into_iter().flatten() {
            if chunk["stream"] == "stdout" {
                stdout.push_str(&decode_b64(chunk["data_b64"].as_str().unwrap_or_default())?);
            }
        }
        exited = body["exited"].as_bool().unwrap_or(false);
        if exited {
            assert_eq!(body["exit_code"], 0);
            break;
        }
    }

    assert!(exited);
    assert!(saw_cursor_advance);
    assert!(stdout.contains(&expected_input_len));
    assert!(stdout.matches("PAYLOAD\n").count() >= 20_000);
    Ok(())
}

#[test]
fn cli_large_output_cursor_reads_without_skipping_chunks() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "cursor-job",
        "--output-bytes-limit",
        "2097152",
        "--",
        "bash",
        "-lc",
        "for i in $(seq 1 20000); do printf 'PAYLOAD\\n'; done",
    ])?;
    assert!(process_start.status.success());

    let mut stdout = String::new();
    let mut after_seq = 0u64;
    let mut exited = false;
    for _ in 0..120 {
        let process_read = run_cli(&[
            "process-read",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "cursor-job",
            "--after-seq",
            &after_seq.to_string(),
            "--max-bytes",
            "32768",
            "--wait-ms",
            "100",
        ])?;
        assert!(process_read.status.success());
        let body: serde_json::Value = serde_json::from_slice(&process_read.stdout)?;
        for chunk in body["chunks"].as_array().into_iter().flatten() {
            if chunk["stream"] == "stdout" {
                stdout.push_str(&decode_b64(chunk["data_b64"].as_str().unwrap_or_default())?);
            }
        }
        let next_seq = body["next_seq"].as_u64().unwrap_or(after_seq);
        assert!(next_seq >= after_seq);
        after_seq = next_seq;
        exited = body["exited"].as_bool().unwrap_or(false);
        if exited {
            break;
        }
    }

    assert!(exited);
    assert_eq!(stdout.matches("PAYLOAD\n").count(), 20_000);
    Ok(())
}

#[test]
fn cli_reconnect_reuses_same_process_without_duplicate_execution() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let first_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "reconnect-job",
        "--",
        "bash",
        "-lc",
        "sleep 2; printf 'done\\n'",
    ])?;
    assert!(first_start.status.success());
    let first_body: serde_json::Value = serde_json::from_slice(&first_start.stdout)?;
    assert_eq!(first_body["created"], true);
    assert_eq!(first_body["already_exists"], false);

    let timed_out_read = run_cli(&[
        "process-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "reconnect-job",
        "--wait-ms",
        "5000",
        "--request-timeout-seconds",
        "1",
    ])?;
    if !timed_out_read.status.success() {
        let timeout_stderr = String::from_utf8(timed_out_read.stderr)?;
        assert!(timeout_stderr.contains("timed out"));
    } else {
        let timed_out_body: serde_json::Value = serde_json::from_slice(&timed_out_read.stdout)?;
        assert_eq!(timed_out_body["process_id"], "reconnect-job");
    }

    let second_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "reconnect-job",
        "--",
        "bash",
        "-lc",
        "sleep 2; printf 'done\\n'",
    ])?;
    assert!(second_start.status.success());
    let second_body: serde_json::Value = serde_json::from_slice(&second_start.stdout)?;
    assert_eq!(second_body["created"], false);
    assert_eq!(second_body["already_exists"], true);

    let process_list = run_cli(&[
        "process-list",
        "--server",
        &harness.base_url,
        "--token",
        token,
    ])?;
    assert!(process_list.status.success());
    let process_list_body: serde_json::Value = serde_json::from_slice(&process_list.stdout)?;
    assert_eq!(
        process_list_body["processes"].as_array().map(|v| v.len()),
        Some(1)
    );

    let mut stdout = String::new();
    let mut after_seq = 0u64;
    let mut exited = false;
    for _ in 0..80 {
        let process_read = run_cli(&[
            "process-read",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "reconnect-job",
            "--after-seq",
            &after_seq.to_string(),
            "--max-bytes",
            "1024",
            "--wait-ms",
            "100",
        ])?;
        assert!(process_read.status.success());
        let body: serde_json::Value = serde_json::from_slice(&process_read.stdout)?;
        for chunk in body["chunks"].as_array().into_iter().flatten() {
            if chunk["stream"] == "stdout" {
                stdout.push_str(&decode_b64(chunk["data_b64"].as_str().unwrap_or_default())?);
            }
        }
        after_seq = body["next_seq"].as_u64().unwrap_or(after_seq);
        exited = body["exited"].as_bool().unwrap_or(false);
        if exited {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(exited);
    assert_eq!(stdout.matches("done\n").count(), 1);

    let process_get = run_cli(&[
        "process-get",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "reconnect-job",
    ])?;
    assert!(process_get.status.success());
    let process_get_body: serde_json::Value = serde_json::from_slice(&process_get.stdout)?;
    assert_eq!(process_get_body["process"]["exited"], true);
    assert_eq!(process_get_body["process"]["output_retained"], true);
    Ok(())
}

#[test]
fn cli_large_output_respects_server_retention_and_resume_cursor() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &[
            "--default-process-output-limit-bytes",
            "65536",
            "--max-process-output-limit-bytes",
            "65536",
        ],
    )?;

    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "truncated-job",
        "--output-bytes-limit",
        "65536",
        "--",
        "bash",
        "-lc",
        "for i in $(seq 1 12000); do printf 'line-%05d\\n' \"$i\"; done",
    ])?;
    assert!(process_start.status.success());

    let mut last_body = None;
    let mut after_seq = 0u64;
    for _ in 0..120 {
        let process_read = run_cli(&[
            "process-read",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            "truncated-job",
            "--after-seq",
            &after_seq.to_string(),
            "--max-bytes",
            "8192",
            "--wait-ms",
            "100",
        ])?;
        assert!(process_read.status.success());
        let body: serde_json::Value = serde_json::from_slice(&process_read.stdout)?;
        let exited = body["exited"].as_bool().unwrap_or(false);
        after_seq = body["next_seq"].as_u64().unwrap_or(after_seq);
        last_body = Some(body);
        if exited {
            break;
        }
    }

    let final_body = last_body.expect("missing process-read body");
    assert_eq!(final_body["exited"], true);
    assert_eq!(final_body["truncated"], true);
    let available_from_seq = final_body["available_from_seq"]
        .as_u64()
        .expect("available_from_seq should exist");
    assert!(available_from_seq > 0);

    let stale_read = run_cli(&[
        "process-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "truncated-job",
        "--after-seq",
        "0",
        "--max-bytes",
        "8192",
    ])?;
    assert!(stale_read.status.success());
    let stale_body: serde_json::Value = serde_json::from_slice(&stale_read.stdout)?;
    assert_eq!(stale_body["cursor_expired"], true);

    let resume_read = run_cli(&[
        "process-read",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "truncated-job",
        "--after-seq",
        &available_from_seq.to_string(),
        "--max-bytes",
        "8192",
    ])?;
    assert!(resume_read.status.success());
    let resume_body: serde_json::Value = serde_json::from_slice(&resume_read.stdout)?;
    assert_eq!(resume_body["cursor_expired"], false);
    let mut stdout = String::new();
    for chunk in resume_body["chunks"].as_array().into_iter().flatten() {
        if chunk["stream"] == "stdout" {
            stdout.push_str(&decode_b64(chunk["data_b64"].as_str().unwrap_or_default())?);
        }
    }
    assert!(stdout.contains("line-"));
    assert!(!stdout.contains("line-00001"));
    Ok(())
}

#[test]
fn cli_process_status_single_running_process() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "status-running",
        "--",
        "bash",
        "-lc",
        "echo ready && sleep 2",
    ])?;
    assert!(
        started.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started.stderr)
    );

    // Wait for output to be produced
    std::thread::sleep(Duration::from_millis(500));

    let status = run_cli(&[
        "process-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "status-running",
    ])?;
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(body["ok"], true);
    let process = &body["process"];
    assert_eq!(process["process_id"], "status-running");
    assert_eq!(process["status"], "running");
    assert!(
        process["pid"].as_i64().is_some(),
        "pid should be present for running process"
    );
    assert!(
        process["elapsed_ms"].as_u64().is_some(),
        "elapsed_ms should be present"
    );
    assert!(
        process["last_output_at_unix_ms"].as_u64().is_some(),
        "last_output_at_unix_ms should be present after output"
    );

    // Wait for process to exit
    std::thread::sleep(Duration::from_millis(2000));

    let status_after = run_cli(&[
        "process-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "status-running",
    ])?;
    assert!(status_after.status.success());
    let body_after: serde_json::Value = serde_json::from_slice(&status_after.stdout)?;
    let process_after = &body_after["process"];
    assert_eq!(process_after["status"], "exited");
    assert_eq!(process_after["exit_code"], 0);
    assert_eq!(process_after["exited"], true);

    let _ = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "status-running",
    ])?;
    Ok(())
}

#[test]
fn cli_process_status_list_with_limit() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    // Start first process
    let started1 = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "status-list-1",
        "--",
        "bash",
        "-lc",
        "echo first && sleep 5",
    ])?;
    assert!(started1.status.success());
    std::thread::sleep(Duration::from_millis(300));

    // Start second process slightly later
    let started2 = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "status-list-2",
        "--",
        "bash",
        "-lc",
        "echo second && sleep 5",
    ])?;
    assert!(started2.status.success());
    std::thread::sleep(Duration::from_millis(300));

    // Request only 1 most-recent process
    let status = run_cli(&[
        "process-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--limit",
        "1",
    ])?;
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(body["ok"], true);
    let processes = body["processes"]
        .as_array()
        .expect("processes should be an array");
    assert_eq!(
        processes.len(),
        1,
        "limit=1 should return exactly 1 process"
    );
    // The most recently active should be status-list-2 (started later)
    assert_eq!(processes[0]["process_id"], "status-list-2");

    // Cleanup
    for pid in &["status-list-1", "status-list-2"] {
        let _ = run_cli(&[
            "process-terminate",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--process-id",
            pid,
        ])?;
    }
    Ok(())
}

#[test]
fn cli_process_start_response_contains_next_commands_without_token() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "secret-token-value-123";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "next-cmd-test",
        "--",
        "bash",
        "-lc",
        "echo hello && exit 0",
    ])?;
    assert!(
        started.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&started.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["process_id"], "next-cmd-test");

    let next = &body["next_commands"];
    assert!(
        next.is_object(),
        "next_commands should be an object: {next}"
    );
    assert!(
        next["status"]
            .as_str()
            .is_some_and(|s| s.contains("process-status")),
        "next_commands.status should contain process-status: {next}"
    );
    assert!(
        next["read"]
            .as_str()
            .is_some_and(|s| s.contains("process-read")),
        "next_commands.read should contain process-read: {next}"
    );
    assert!(
        next["terminate"]
            .as_str()
            .is_some_and(|s| s.contains("process-terminate")),
        "next_commands.terminate should contain process-terminate: {next}"
    );
    // All three should reference the process id
    for field in &["status", "read", "terminate"] {
        assert!(
            next[*field]
                .as_str()
                .is_some_and(|s| s.contains("next-cmd-test")),
            "next_commands.{field} should contain the process id"
        );
    }
    // Token must NOT appear anywhere in the response
    let raw = serde_json::to_string(&body)?;
    assert!(
        !raw.contains(token),
        "token value must not appear in process-start response"
    );
    assert!(
        !raw.contains("secret-token-value"),
        "token substring must not appear in process-start response"
    );
    // next_commands should use <token> placeholder
    assert!(
        next["status"]
            .as_str()
            .is_some_and(|s| s.contains("<token>")),
        "next_commands should use <token> placeholder"
    );
    assert!(
        next["status"]
            .as_str()
            .is_some_and(|s| !s.contains("--json")),
        "process-status next command must not include unsupported --json"
    );

    let _ = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "next-cmd-test",
    ])?;
    Ok(())
}

#[test]
fn cli_process_status_reports_failed_status() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;

    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "status-failed",
        "--",
        "sh",
        "-c",
        "exit 7",
    ])?;
    assert!(
        started.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started.stderr)
    );

    // Wait for the process to exit
    std::thread::sleep(Duration::from_millis(1000));

    let status = run_cli(&[
        "process-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "status-failed",
    ])?;
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(body["ok"], true);
    let process = &body["process"];
    assert_eq!(process["process_id"], "status-failed");
    assert_eq!(process["status"], "failed");
    assert_eq!(process["exit_code"], 7);
    assert_eq!(process["exited"], true);

    let _ = run_cli(&[
        "process-terminate",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--process-id",
        "status-failed",
    ])?;
    Ok(())
}

#[test]
fn cli_process_status_profile_label_surfaces_in_text_and_json() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let harness = CliServerHarness::start(remote_root.path(), token)?;
    let profile_dir = tempfile::tempdir()?;
    let profile_path = profile_dir.path().join("node13.env");
    std::fs::write(
        &profile_path,
        format!(
            "AP_SERVER={}\nAP_TOKEN={token}\nAP_REMOTE_ROOT={}\nAP_LABEL=node13\n",
            harness.base_url,
            remote_root.path().display()
        ),
    )?;

    let started = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-start",
        "--process-id",
        "label-running",
        "--",
        "bash",
        "-lc",
        "echo label-output && sleep 5",
    ])?;
    assert!(
        started.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    std::thread::sleep(Duration::from_millis(400));

    // Text mode: a metadata header identifies the node before the process lines.
    let status_text = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-status",
        "--text",
        "--process-id",
        "label-running",
    ])?;
    assert!(
        status_text.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status_text.stderr)
    );
    let text = String::from_utf8(status_text.stdout)?;
    assert!(
        text.contains("# label=node13 server="),
        "text header missing: {text}"
    );
    assert!(
        text.contains("label-running"),
        "process line missing: {text}"
    );

    // JSON mode: label and server merge in without changing ok semantics.
    let status_json = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-status",
        "--process-id",
        "label-running",
    ])?;
    assert!(status_json.status.success());
    let body: serde_json::Value = serde_json::from_slice(&status_json.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["label"], "node13");
    assert_eq!(body["server"], harness.base_url);
    assert_eq!(body["process"]["process_id"], "label-running");

    // List mode JSON also carries the identity for multi-node disambiguation.
    let status_list = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-status",
        "--label",
        "node13-dr",
    ])?;
    assert!(status_list.status.success());
    let list_body: serde_json::Value = serde_json::from_slice(&status_list.stdout)?;
    assert_eq!(list_body["label"], "node13-dr");
    assert_eq!(list_body["server"], harness.base_url);

    let _ = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-terminate",
        "--process-id",
        "label-running",
    ])?;
    Ok(())
}
