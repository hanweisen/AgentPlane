mod common;

use std::thread;
use std::time::{Duration, Instant};

use agentplane::protocol::{AgentMode, LeaseReleaseRequest, LeaseRenewRequest, ModeSwitchRequest};
use agentplane::server::{
    ServerState, handle_lease_release, handle_lease_renew, handle_mode_get, handle_mode_switch,
};
use common::*;

#[tokio::test]
async fn agent_mode_switch_lease_renew_release_and_recover_workflow() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    let mode = handle_mode_get(&state).await?;
    assert_eq!(mode.current_mode, AgentMode::Single);
    assert!(mode.leases.is_empty());

    let switched = handle_mode_switch(
        &state,
        ModeSwitchRequest {
            mode: AgentMode::Shared,
            task_id: Some("task-1".to_string()),
            lease_id: Some("lease-1".to_string()),
            ttl_seconds: Some(30),
            heartbeat_seconds: Some(3),
            max_renewals: Some(2),
        },
    )
    .await?;
    assert_eq!(switched.current_mode, AgentMode::Shared);
    let lease = switched.lease.expect("lease");
    assert_eq!(lease.task_id, "task-1");
    assert_eq!(lease.lease_id, "lease-1");
    assert_eq!(lease.renewals, 0);
    assert_eq!(lease.max_renewals, 2);

    let renewed = handle_lease_renew(
        &state,
        LeaseRenewRequest {
            task_id: "task-1".to_string(),
            lease_id: "lease-1".to_string(),
        },
    )
    .await?;
    assert_eq!(renewed.lease.renewals, 1);

    let renewed_again = handle_lease_renew(
        &state,
        LeaseRenewRequest {
            task_id: "task-1".to_string(),
            lease_id: "lease-1".to_string(),
        },
    )
    .await?;
    assert_eq!(renewed_again.lease.renewals, 2);

    let expired = handle_lease_renew(
        &state,
        LeaseRenewRequest {
            task_id: "task-1".to_string(),
            lease_id: "lease-1".to_string(),
        },
    )
    .await;
    assert!(expired.is_err());

    let released = handle_lease_release(
        &state,
        LeaseReleaseRequest {
            task_id: "task-1".to_string(),
            lease_id: "lease-1".to_string(),
        },
    )
    .await?;
    assert_eq!(
        released.lease.status,
        agentplane::protocol::LeaseStatus::Released
    );

    let back_to_single = handle_mode_switch(
        &state,
        ModeSwitchRequest {
            mode: AgentMode::Single,
            task_id: None,
            lease_id: None,
            ttl_seconds: None,
            heartbeat_seconds: None,
            max_renewals: None,
        },
    )
    .await?;
    assert_eq!(back_to_single.current_mode, AgentMode::Single);
    assert_eq!(back_to_single.leases.len(), 1);
    Ok(())
}

#[tokio::test]
async fn agent_lease_ttl_expiry_blocks_renewal() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );

    handle_mode_switch(
        &state,
        ModeSwitchRequest {
            mode: AgentMode::Shared,
            task_id: Some("ttl-task".to_string()),
            lease_id: Some("ttl-lease".to_string()),
            ttl_seconds: Some(0),
            heartbeat_seconds: Some(1),
            max_renewals: Some(10),
        },
    )
    .await?;

    let expired = handle_lease_renew(
        &state,
        LeaseRenewRequest {
            task_id: "ttl-task".to_string(),
            lease_id: "ttl-lease".to_string(),
        },
    )
    .await;
    assert!(expired.is_err());

    let mode = handle_mode_get(&state).await?;
    assert_eq!(
        mode.leases[0].status,
        agentplane::protocol::LeaseStatus::Expired
    );
    Ok(())
}

#[test]
fn cli_shared_mode_requires_active_lease_for_process_start() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;

    let switched = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "task-cli",
        "--lease-id",
        "lease-cli",
        "--ttl-seconds",
        "60",
    ])?;
    assert!(switched.status.success());

    let missing_lease = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "missing-lease",
        "--cwd",
        &remote_root.path().display().to_string(),
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!missing_lease.status.success());
    let missing_error = String::from_utf8_lossy(&missing_lease.stderr);
    assert!(missing_error.contains("shared mode requires lease headers"));

    let wrong_lease = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "wrong-lease",
        "--cwd",
        &remote_root.path().display().to_string(),
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: task-cli",
        "--header",
        "x-agentplane-lease-id: missing",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!wrong_lease.status.success());
    let wrong_error = String::from_utf8_lossy(&wrong_lease.stderr);
    assert!(wrong_error.contains("unknown lease"));

    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root.path().display().to_string(),
        "--process-id",
        "with-lease",
        "--cwd",
        &remote_root.path().display().to_string(),
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: task-cli",
        "--header",
        "x-agentplane-lease-id: lease-cli",
        "--",
        "/bin/sh",
        "-lc",
        "echo lease-ok",
    ])?;
    assert!(started.status.success());
    Ok(())
}

#[test]
fn cli_file_upload_still_works_when_server_is_in_shared_mode() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    let switched = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "task-file",
        "--lease-id",
        "lease-file",
        "--ttl-seconds",
        "60",
    ])?;
    assert!(switched.status.success());

    let local_upload = remote_root.path().join("shared-upload.bin");
    std::fs::write(&local_upload, b"shared-mode-upload")?;

    let uploaded = run_cli(&[
        "file-upload",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--path",
        "shared/upload.bin",
        "--from-local",
        &local_upload.display().to_string(),
        "--chunk-size",
        "4",
        "--resume",
    ])?;
    assert!(uploaded.status.success());

    let read = run_cli(&[
        "file-read",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--path",
        "shared/upload.bin",
        "--text",
    ])?;
    assert!(read.status.success());
    assert_eq!(String::from_utf8(read.stdout)?, "shared-mode-upload");
    Ok(())
}

#[test]
fn cli_multi_agent_shared_mode_keeps_active_agent_resources_isolated() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    for (task_id, lease_id) in [("agent-a", "lease-a"), ("agent-b", "lease-b")] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            "60",
        ])?;
        assert!(switched.status.success());
    }

    let mode = run_cli(&[
        "mode-get",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
    ])?;
    assert!(mode.status.success());
    let mode_text = String::from_utf8_lossy(&mode.stdout);
    assert!(mode_text.contains("\"current_mode\": \"shared\""));
    assert!(mode_text.contains("\"task_id\": \"agent-a\""));
    assert!(mode_text.contains("\"task_id\": \"agent-b\""));

    for (task_id, lease_id, process_id, marker) in [
        ("agent-a", "lease-a", "agent-a-proc", "AGENT_A_OK"),
        ("agent-b", "lease-b", "agent-b-proc", "AGENT_B_OK"),
    ] {
        let started = run_cli(&[
            "process-start",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--remote-root",
            &remote_root_str,
            "--process-id",
            process_id,
            "--cwd",
            &remote_root_str,
            "--header",
            "x-agentplane-agent-mode: shared",
            "--header",
            &format!("x-agentplane-task-id: {task_id}"),
            "--header",
            &format!("x-agentplane-lease-id: {lease_id}"),
            "--",
            "/bin/sh",
            "-lc",
            &format!("echo {marker}"),
        ])?;
        assert!(started.status.success());
    }

    let process_id_collision = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "agent-a-proc",
        "--cwd",
        &remote_root_str,
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: agent-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "echo AGENT_B_COLLISION",
    ])?;
    assert!(!process_id_collision.status.success());
    let collision_error = String::from_utf8_lossy(&process_id_collision.stderr);
    assert!(collision_error.contains("process_id already exists"));

    let released_a = run_cli(&[
        "lease-release",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--task-id",
        "agent-a",
        "--lease-id",
        "lease-a",
    ])?;
    assert!(released_a.status.success());

    let mode_after_a_release = run_cli(&[
        "mode-get",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
    ])?;
    assert!(mode_after_a_release.status.success());
    let mode_after_a_release_text = String::from_utf8_lossy(&mode_after_a_release.stdout);
    assert!(mode_after_a_release_text.contains("\"current_mode\": \"shared\""));
    assert!(mode_after_a_release_text.contains("\"lease_id\": \"lease-a\""));
    assert!(mode_after_a_release_text.contains("\"status\": \"released\""));
    assert!(mode_after_a_release_text.contains("\"lease_id\": \"lease-b\""));
    assert!(mode_after_a_release_text.contains("\"status\": \"active\""));

    let released_agent_cannot_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "released-agent-a",
        "--cwd",
        &remote_root_str,
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: agent-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "echo RELEASED_AGENT_SHOULD_NOT_RUN",
    ])?;
    assert!(!released_agent_cannot_start.status.success());
    let released_agent_error = String::from_utf8_lossy(&released_agent_cannot_start.stderr);
    assert!(released_agent_error.contains("lease is not active"));

    let missing_lease_after_a_release = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "no-lease-while-b-active",
        "--cwd",
        &remote_root_str,
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!missing_lease_after_a_release.status.success());

    let b_still_runs = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "agent-b-after-a-release",
        "--cwd",
        &remote_root_str,
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: agent-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "echo AGENT_B_STILL_OK",
    ])?;
    assert!(b_still_runs.status.success());

    let released_b = run_cli(&[
        "lease-release",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--task-id",
        "agent-b",
        "--lease-id",
        "lease-b",
    ])?;
    assert!(released_b.status.success());

    let mode_after_b_release = run_cli(&[
        "mode-get",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
    ])?;
    assert!(mode_after_b_release.status.success());
    let mode_after_b_release_text = String::from_utf8_lossy(&mode_after_b_release.stdout);
    assert!(mode_after_b_release_text.contains("\"current_mode\": \"single\""));

    let single_after_all_released = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "single-after-all-released",
        "--cwd",
        &remote_root_str,
        "--",
        "/bin/sh",
        "-lc",
        "echo SINGLE_AFTER_ALL_RELEASED_OK",
    ])?;
    assert!(single_after_all_released.status.success());
    Ok(())
}

#[test]
fn cli_shared_gpu_claim_blocks_other_active_lease() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    for (task_id, lease_id) in [("gpu-a", "lease-a"), ("gpu-b", "lease-b")] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            "60",
        ])?;
        assert!(switched.status.success());
    }

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "gpu-holder-a",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: gpu-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    let blocked = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "gpu-holder-b",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: gpu-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!blocked.status.success());
    let blocked_error = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_error.contains("GPU 0 is reserved by active lease gpu-a/lease-a"));
    assert!(blocked_error.contains("reservation disappears automatically"));
    Ok(())
}

#[test]
fn cli_expired_lease_gpu_claim_no_longer_protects_running_process() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    let expired_switched = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "expired-task",
        "--lease-id",
        "expired-lease",
        "--ttl-seconds",
        "1",
        "--heartbeat-seconds",
        "1",
    ])?;
    assert!(expired_switched.status.success());

    let active_switched = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "active-task",
        "--lease-id",
        "active-lease",
        "--ttl-seconds",
        "60",
    ])?;
    assert!(active_switched.status.success());

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "expired-holder",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: expired-task",
        "--header",
        "x-agentplane-lease-id: expired-lease",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    thread::sleep(Duration::from_millis(1500));

    let holder_state = run_cli(&[
        "process-get",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--process-id",
        "expired-holder",
    ])?;
    assert!(holder_state.status.success());
    let holder_state_text = String::from_utf8_lossy(&holder_state.stdout);
    assert!(holder_state_text.contains("\"exited\": false"));

    let mode = run_cli(&[
        "mode-get",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
    ])?;
    assert!(mode.status.success());
    let mode_text = String::from_utf8_lossy(&mode.stdout);
    assert!(mode_text.contains("\"task_id\": \"expired-task\""));
    assert!(mode_text.contains("\"status\": \"expired\""));

    let expired_lease_cannot_restart = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "expired-holder-retry",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: expired-task",
        "--header",
        "x-agentplane-lease-id: expired-lease",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!expired_lease_cannot_restart.status.success());
    let expired_lease_error = String::from_utf8_lossy(&expired_lease_cannot_restart.stderr);
    assert!(expired_lease_error.contains("lease expired"));
    assert!(
        expired_lease_error
            .contains("resource protection from this lease has already been removed")
    );

    let other_agent_can_claim = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "new-holder",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: active-task",
        "--header",
        "x-agentplane-lease-id: active-lease",
        "--",
        "/bin/sh",
        "-lc",
        "echo claim-ok",
    ])?;
    assert!(other_agent_can_claim.status.success());
    Ok(())
}

#[test]
fn cli_sync_run_command_respects_active_gpu_claim() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();
    let repo = init_repo()?;
    std::fs::write(repo.path().join("tracked.txt"), "hello\n")?;
    git(repo.path(), &["add", "tracked.txt"])?;
    git(repo.path(), &["commit", "-m", "init"])?;

    for (task_id, lease_id) in [("sync-a", "lease-a"), ("sync-b", "lease-b")] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            "60",
        ])?;
        assert!(switched.status.success());
    }

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "sync-gpu-holder",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: sync-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    let blocked = run_cli(&[
        "sync-run",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--repo",
        &repo.path().display().to_string(),
        "--remote-root",
        &remote_root_str,
        "--command",
        "echo should-not-run",
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: sync-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
    ])?;
    assert!(!blocked.status.success());
    let blocked_error = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_error.contains("GPU 0 is reserved by active lease sync-a/lease-a"));
    Ok(())
}

#[test]
fn cli_explicit_gpu_claim_blocks_without_cuda_visible_devices() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    for (task_id, lease_id) in [("claim-a", "lease-a"), ("claim-b", "lease-b")] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            "60",
        ])?;
        assert!(switched.status.success());
    }

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "explicit-gpu-holder",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: claim-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    let blocked = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "explicit-gpu-competitor",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: claim-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!blocked.status.success());
    let blocked_error = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_error.contains("GPU 0 is reserved by active lease claim-a/lease-a"));
    Ok(())
}

#[test]
fn cli_recovered_existing_process_reclaims_explicit_resource_claim() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    for (task_id, lease_id, ttl_seconds) in [
        ("recover-a", "lease-a", "1"),
        ("recover-b", "lease-b", "60"),
    ] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            ttl_seconds,
        ])?;
        assert!(switched.status.success());
    }

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "recover-holder",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: recover-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    thread::sleep(Duration::from_millis(1500));

    let recovered_lease = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "recover-a",
        "--lease-id",
        "lease-a",
        "--ttl-seconds",
        "60",
    ])?;
    assert!(recovered_lease.status.success());

    let reconnected = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "recover-holder",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: recover-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(reconnected.status.success());
    let reconnected_text = String::from_utf8_lossy(&reconnected.stdout);
    assert!(reconnected_text.contains("\"already_exists\": true"));

    let blocked = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "recover-competitor",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: recover-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!blocked.status.success());
    let blocked_error = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_error.contains("GPU 0 is reserved by active lease recover-a/lease-a"));
    Ok(())
}

#[test]
fn cli_recovered_existing_process_fails_when_claim_was_taken() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    for (task_id, lease_id, ttl_seconds) in
        [("take-a", "lease-a", "1"), ("take-b", "lease-b", "60")]
    {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            ttl_seconds,
        ])?;
        assert!(switched.status.success());
    }

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "take-holder",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: take-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    thread::sleep(Duration::from_millis(1500));

    let competitor = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "take-competitor",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: take-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(competitor.status.success());

    let recovered_lease = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "take-a",
        "--lease-id",
        "lease-a",
        "--ttl-seconds",
        "60",
    ])?;
    assert!(recovered_lease.status.success());

    let reconnected = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "take-holder",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: take-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(!reconnected.status.success());
    let reconnect_error = String::from_utf8_lossy(&reconnected.stderr);
    assert!(reconnect_error.contains("GPU 0 is reserved by active lease take-b/lease-b"));
    Ok(())
}

#[test]
fn cli_explicit_gpu_claim_must_match_cuda_visible_devices() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    let switched = run_cli(&[
        "mode-switch",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--mode",
        "shared",
        "--task-id",
        "match-task",
        "--lease-id",
        "lease-a",
        "--ttl-seconds",
        "60",
    ])?;
    assert!(switched.status.success());

    let mismatched = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "mismatch-gpu-claim",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:1",
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: match-task",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!mismatched.status.success());
    let mismatch_error = String::from_utf8_lossy(&mismatched.stderr);
    assert!(
        mismatch_error.contains(
            "explicit resource claim gpu:1 conflicts with environment-inferred claim gpu:0"
        )
    );
    Ok(())
}

#[test]
fn cli_claims_are_ignored_outside_shared_mode() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    let started = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "single-mode-claim",
        "--cwd",
        &remote_root_str,
        "--claim",
        "gpu:1",
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--",
        "/bin/sh",
        "-lc",
        "echo single-mode-ok",
    ])?;
    assert!(started.status.success());
    Ok(())
}

#[test]
fn cli_sync_run_generic_resource_claim_blocks_other_active_lease() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();
    let repo = init_repo()?;
    std::fs::write(repo.path().join("tracked.txt"), "hello\n")?;
    git(repo.path(), &["add", "tracked.txt"])?;
    git(repo.path(), &["commit", "-m", "init"])?;

    for (task_id, lease_id) in [("port-a", "lease-a"), ("port-b", "lease-b")] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            "60",
        ])?;
        assert!(switched.status.success());
    }

    let holder = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "port-holder",
        "--cwd",
        &remote_root_str,
        "--claim",
        "port:6006",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: port-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 30",
    ])?;
    assert!(holder.status.success());

    let blocked = run_cli(&[
        "sync-run",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--repo",
        &repo.path().display().to_string(),
        "--remote-root",
        &remote_root_str,
        "--command",
        "echo should-not-run",
        "--claim",
        "port:6006",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: port-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
    ])?;
    assert!(!blocked.status.success());
    let blocked_error = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_error.contains("port:6006 is reserved by active lease port-a/lease-a"));
    Ok(())
}

#[test]
fn cli_gpu_claim_survives_wrapper_exit_until_child_group_finishes() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let harness = CliServerHarness::start(remote_root.path(), "test-token")?;
    let remote_root_str = remote_root.path().display().to_string();

    for (task_id, lease_id) in [("wrap-a", "lease-a"), ("wrap-b", "lease-b")] {
        let switched = run_cli(&[
            "mode-switch",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--mode",
            "shared",
            "--task-id",
            task_id,
            "--lease-id",
            lease_id,
            "--ttl-seconds",
            "60",
        ])?;
        assert!(switched.status.success());
    }

    let wrapper = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "wrapper-holder",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--kill-tree-on-terminate",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: wrap-a",
        "--header",
        "x-agentplane-lease-id: lease-a",
        "--",
        "/bin/sh",
        "-lc",
        "sleep 5 & echo wrapper-exit",
    ])?;
    assert!(wrapper.status.success());

    thread::sleep(Duration::from_millis(1200));

    let wrapper_state = run_cli(&[
        "process-get",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--process-id",
        "wrapper-holder",
    ])?;
    assert!(wrapper_state.status.success());
    let wrapper_state_text = String::from_utf8_lossy(&wrapper_state.stdout);
    assert!(wrapper_state_text.contains("\"exited\": true"));
    assert!(wrapper_state_text.contains("\"children_running\": true"));

    let blocked = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        &remote_root_str,
        "--process-id",
        "wrapper-competitor-1",
        "--cwd",
        &remote_root_str,
        "--env",
        "CUDA_VISIBLE_DEVICES=0",
        "--header",
        "x-agentplane-agent-mode: shared",
        "--header",
        "x-agentplane-task-id: wrap-b",
        "--header",
        "x-agentplane-lease-id: lease-b",
        "--",
        "/bin/sh",
        "-lc",
        "echo should-not-run",
    ])?;
    assert!(!blocked.status.success());
    let blocked_error = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_error.contains("GPU 0 is reserved by active lease wrap-a/lease-a"));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut succeeded = false;
    while Instant::now() < deadline {
        let retry = run_cli(&[
            "process-start",
            "--server",
            &harness.base_url,
            "--token",
            "test-token",
            "--remote-root",
            &remote_root_str,
            "--process-id",
            "wrapper-competitor-2",
            "--cwd",
            &remote_root_str,
            "--env",
            "CUDA_VISIBLE_DEVICES=0",
            "--header",
            "x-agentplane-agent-mode: shared",
            "--header",
            "x-agentplane-task-id: wrap-b",
            "--header",
            "x-agentplane-lease-id: lease-b",
            "--",
            "/bin/sh",
            "-lc",
            "echo now-ok",
        ])?;
        if retry.status.success() {
            succeeded = true;
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    assert!(
        succeeded,
        "GPU claim did not release after background child exited"
    );
    Ok(())
}
