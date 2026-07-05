mod common;

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
