mod common;

use std::time::{Duration, Instant};

use common::*;

#[test]
fn cli_profile_supplies_connection_remote_root_and_gateway_headers() -> Result<()> {
    let gate = HeaderGateHarness::start("x-agentplane-gateway", "profile")?;
    let profile_dir = tempfile::tempdir()?;
    let profile_path = profile_dir.path().join("agentplane.env");
    std::fs::write(
        &profile_path,
        format!(
            "\
AP_SERVER={}
AP_TOKEN=test-token
AP_REMOTE_ROOT=/workspace/project
AP_HEADER_1=X-AgentPlane-Gateway: profile
AP_CONNECT_RETRIES=1
AP_CONNECT_RETRY_DELAY_MS=10
",
            gate.base_url
        ),
    )?;

    let process_list = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-list",
    ])?;
    assert!(
        process_list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&process_list.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&process_list.stdout)?;
    assert_eq!(body["ok"], true);

    let file_list = run_cli(&[
        "--env-file",
        &profile_path.display().to_string(),
        "file-list",
        "--path",
        ".",
    ])?;
    assert!(
        file_list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&file_list.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&file_list.stdout)?;
    assert_eq!(body["entries"][0]["path"], "README.md");
    Ok(())
}

#[test]
fn cli_profile_does_not_accept_legacy_cc_keys() -> Result<()> {
    let profile_dir = tempfile::tempdir()?;
    let profile_path = profile_dir.path().join("legacy.env");
    std::fs::write(
        &profile_path,
        "\
CC_SERVER=http://127.0.0.1:1
CC_TOKEN=test-token
CC_REMOTE_ROOT=/workspace/project
",
    )?;

    let output = run_cli(&[
        "--profile",
        &profile_path.display().to_string(),
        "process-list",
    ])?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("AP_SERVER"), "stderr: {stderr}");
    Ok(())
}

#[test]
fn cli_custom_headers_enable_gateway_health_and_process_list() -> Result<()> {
    let harness = HeaderGateHarness::start("x-agentplane-gateway", "ok")?;

    let health_without_header = run_cli(&["health", "--server", &harness.base_url])?;
    assert!(!health_without_header.status.success());

    let health_with_header = run_cli(&[
        "health",
        "--server",
        &harness.base_url,
        "--header",
        "X-AgentPlane-Gateway: ok",
    ])?;
    assert!(health_with_header.status.success());
    let health_body: serde_json::Value = serde_json::from_slice(&health_with_header.stdout)?;
    assert_eq!(health_body["ok"], true);

    let list_with_headers = run_cli(&[
        "process-list",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--header",
        "X-AgentPlane-Gateway: ok",
    ])?;
    assert!(list_with_headers.status.success());
    let list_body: serde_json::Value = serde_json::from_slice(&list_with_headers.stdout)?;
    assert_eq!(list_body["ok"], true);
    assert_eq!(list_body["processes"].as_array().map(|v| v.len()), Some(0));
    Ok(())
}

#[test]
fn cli_custom_header_rejects_invalid_format() -> Result<()> {
    let output = run_cli(&[
        "health",
        "--server",
        "http://127.0.0.1:1",
        "--header",
        "broken-header",
    ])?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("invalid --header"));
    Ok(())
}

#[test]
fn cli_custom_headers_decode_gzip_json_responses() -> Result<()> {
    let harness = HeaderGateHarness::start("x-agentplane-gateway", "ok")?;

    let file_list = run_cli(&[
        "file-list",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        "/workspace/project",
        "--path",
        ".",
        "--header",
        "X-AgentPlane-Gateway: ok",
    ])?;
    assert!(file_list.status.success());
    let body: serde_json::Value = serde_json::from_slice(&file_list.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["entries"][0]["path"], "README.md");
    Ok(())
}

#[test]
fn cli_health_retries_connect_failures() -> Result<()> {
    let harness = DelayedHealthHarness::start(Duration::from_millis(600))?;
    let health = run_cli(&[
        "health",
        "--server",
        &harness.base_url,
        "--connect-retries",
        "5",
        "--request-timeout-seconds",
        "5",
    ])?;
    assert!(health.status.success());
    let body: serde_json::Value = serde_json::from_slice(&health.stdout)?;
    assert_eq!(body["ok"], true);
    Ok(())
}

#[test]
fn cli_process_start_retries_retryable_gateway_statuses() -> Result<()> {
    let harness = FlakyProcessStartHarness::start(2)?;
    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        "/workspace/project",
        "--process-id",
        "retry-job",
        "--connect-retries",
        "3",
        "--connect-retry-delay-ms",
        "25",
        "--",
        "bash",
        "-lc",
        "echo ok",
    ])?;
    assert!(process_start.status.success());
    let body: serde_json::Value = serde_json::from_slice(&process_start.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["process_id"], "retry-job");
    assert_eq!(harness.request_count(), 3);
    Ok(())
}

#[test]
fn cli_process_start_honors_retry_delay_between_attempts() -> Result<()> {
    let harness = FlakyProcessStartHarness::start(1)?;
    let started = Instant::now();
    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        "/workspace/project",
        "--process-id",
        "retry-job",
        "--connect-retries",
        "2",
        "--connect-retry-delay-ms",
        "400",
        "--",
        "bash",
        "-lc",
        "echo ok",
    ])?;
    let elapsed = started.elapsed();
    assert!(process_start.status.success());
    assert_eq!(harness.request_count(), 2);
    assert!(
        elapsed >= Duration::from_millis(350),
        "retry delay was too short: {:?}",
        elapsed
    );
    Ok(())
}

#[test]
fn cli_process_start_recovers_after_dropped_response_if_process_exists() -> Result<()> {
    let harness = DroppedConnectionRecoveryHarness::start()?;
    let process_start = run_cli(&[
        "process-start",
        "--server",
        &harness.base_url,
        "--token",
        "test-token",
        "--remote-root",
        "/workspace/project",
        "--process-id",
        "recover-after-drop",
        "--connect-retries",
        "2",
        "--connect-retry-delay-ms",
        "50",
        "--output-bytes-limit",
        "1048576",
        "--",
        "bash",
        "-lc",
        "echo ok",
    ])?;
    assert!(process_start.status.success());
    let body: serde_json::Value = serde_json::from_slice(&process_start.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["process_id"], "recover-after-drop");
    assert_eq!(body["created"], false);
    assert_eq!(body["already_exists"], true);
    Ok(())
}
