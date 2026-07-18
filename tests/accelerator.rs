mod common;

use std::process::Command;

use agentplane::protocol::{AcceleratorKind, AcceleratorStatusRequest, ProcessCleanupRequest};
use agentplane::server::{ServerState, handle_accelerator_status, handle_process_cleanup};
use common::*;

#[tokio::test]
async fn accelerator_status_reports_unavailable_gpu_without_failing() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let mut state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    state.nvidia_smi_path = Some(remote_root.path().join("missing-nvidia-smi"));

    let status = handle_accelerator_status(
        &state,
        AcceleratorStatusRequest {
            kind: AcceleratorKind::Gpu,
            gpus: None,
            process_match: None,
        },
    )
    .await?;
    assert!(status.ok);
    assert!(!status.available);
    assert_eq!(status.provider, None);
    assert_eq!(status.reason.as_deref(), Some("nvidia-smi not found"));
    assert!(status.agent_hint.contains("Do not retry GPU status checks"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn accelerator_status_parses_mocked_nvidia_gpu_and_processes() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let mock = remote_root.path().join("nvidia-smi");
    let current_pid = std::process::id();
    write_mock_nvidia_smi(
        &mock,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 128, 81920, 7, P0, 101.50, 42'\n    printf '%s\\n' '1, NVIDIA A800, GPU-one, 0, 81920, 0, P8, 50.00, 35'\n    ;;\n  --query-compute-apps=*)\n    printf '%s\\n' 'GPU-zero, {current_pid}, 256'\n    printf '%s\\n' 'GPU-one, 999999, 512'\n    ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )?;
    let mut state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    state.nvidia_smi_path = Some(mock);

    let status = handle_accelerator_status(
        &state,
        AcceleratorStatusRequest {
            kind: AcceleratorKind::Gpu,
            gpus: Some("0".to_string()),
            process_match: None,
        },
    )
    .await?;
    assert!(status.available);
    assert_eq!(status.provider.as_deref(), Some("nvidia"));
    assert_eq!(status.devices.len(), 1);
    assert_eq!(status.devices[0].index, 0);
    assert_eq!(status.devices[0].memory_total_mib, Some(81920));
    assert_eq!(status.devices[0].power_draw_milliwatts, Some(101500));
    assert_eq!(status.processes.len(), 1);
    assert_eq!(status.processes[0].pid, current_pid as i32);
    assert_eq!(status.processes[0].gpu_index, Some(0));
    assert_eq!(status.processes[0].used_memory_mib, Some(256));
    Ok(())
}

#[test]
fn cli_gpu_status_reports_unavailable_without_failing() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let missing = remote_root.path().join("missing-nvidia-smi");
    let missing_arg = missing.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &missing_arg],
    )?;

    let output = run_cli(&[
        "gpu-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["ok"], true);
    assert_eq!(body["kind"], "gpu");
    assert_eq!(body["available"], false);
    assert_eq!(body["reason"], "nvidia-smi not found");
    assert!(
        body["agent_hint"]
            .as_str()
            .unwrap_or_default()
            .contains("Do not retry GPU status checks")
    );

    let text = run_cli(&[
        "gpu-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--text",
    ])?;
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("No GPU accelerator detected"));
    Ok(())
}

#[tokio::test]
async fn accelerator_status_reports_unavailable_npu_without_failing() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let mut state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    state.npu_smi_path = Some(remote_root.path().join("missing-npu-smi"));

    let status = handle_accelerator_status(
        &state,
        AcceleratorStatusRequest {
            kind: AcceleratorKind::Npu,
            gpus: None,
            process_match: None,
        },
    )
    .await?;
    assert!(status.ok);
    assert!(!status.available);
    assert_eq!(status.provider, None);
    assert_eq!(status.reason.as_deref(), Some("npu-smi not found"));
    assert!(status.agent_hint.contains("Do not retry NPU status checks"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn accelerator_status_parses_mocked_huawei_npu_and_processes() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let mock = remote_root.path().join("npu-smi");
    let current_pid = std::process::id();
    write_mock_executable(
        &mock,
        &format!(
            "#!/bin/sh\ncat <<'INFO'\n+------------------------------------------------------------------------------------------------+\n| NPU     Name      | Health          | Power(W)     Temp(C)           Hugepages-Usage(page)     |\n| Chip    Device    | Bus-Id          | AICore(%)    Memory-Usage(MB)                             |\n+===================+=================+==========================================================+\n| 0       910B      | OK              | 91.8         42                0    / 0                   |\n| 0       0         | 0000:C1:00.0    | 5            1024 / 65536                                 |\n+===================+=================+==========================================================+\n| 1       910B      | OK              | 80.0         39                0    / 0                   |\n| 0       1         | 0000:C2:00.0    | 0            0 / 65536                                    |\n+-------------------+-----------------+----------------------------------------------------------+\n| NPU     Chip      | Process id      | Process name             | Process memory(MB)      |\n+===================+=================+==========================================================+\n| 0       0         | {current_pid}   | agentplane               | 2048                    |\nINFO\n"
        ),
    )?;
    let mut state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    state.npu_smi_path = Some(mock);

    let status = handle_accelerator_status(
        &state,
        AcceleratorStatusRequest {
            kind: AcceleratorKind::Npu,
            gpus: Some("0".to_string()),
            process_match: None,
        },
    )
    .await?;
    assert!(status.available);
    assert_eq!(status.provider.as_deref(), Some("huawei-ascend"));
    assert_eq!(status.devices.len(), 1);
    assert_eq!(status.devices[0].index, 0);
    assert_eq!(status.devices[0].name, "910B");
    assert_eq!(status.devices[0].memory_used_mib, Some(1024));
    assert_eq!(status.devices[0].memory_total_mib, Some(65536));
    assert_eq!(status.devices[0].utilization_percent, Some(5));
    assert_eq!(status.processes.len(), 1);
    assert_eq!(status.processes[0].pid, current_pid as i32);
    assert_eq!(status.processes[0].gpu_index, Some(0));
    assert_eq!(status.processes[0].used_memory_mib, Some(2048));
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_npu_status_reads_mocked_huawei_npu() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let mock = remote_root.path().join("npu-smi");
    write_mock_executable(
        &mock,
        "#!/bin/sh\nif [ \"$1\" = 'info' ] && [ \"$2\" = '-t' ]; then\n  exit 0\nfi\ncat <<'INFO'\n+------------------------------------------------------------------------------------------------+\n| NPU     Name      | Health          | Power(W)     Temp(C)           Hugepages-Usage(page)     |\n| Chip    Device    | Bus-Id          | AICore(%)    Memory-Usage(MB)                             |\n+===================+=================+==========================================================+\n| 0       910B      | OK              | 91.8         42                0    / 0                   |\n| 0       0         | 0000:C1:00.0    | 5            1024 / 65536                                 |\nINFO\n",
    )?;
    let mock_arg = mock.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--npu-smi-path", &mock_arg],
    )?;

    let output = run_cli(&[
        "npu-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--json",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["available"], true);
    assert_eq!(body["provider"], "huawei-ascend");
    assert_eq!(body["kind"], "npu");
    assert_eq!(body["devices"].as_array().unwrap().len(), 1);
    assert_eq!(body["devices"][0]["index"], 0);

    let text = run_cli(&[
        "npu-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--text",
    ])?;
    assert!(text.status.success());
    let text_stdout = String::from_utf8_lossy(&text.stdout);
    assert!(text_stdout.contains("NPU accelerator provider: huawei-ascend"));
    assert!(text_stdout.contains("NPU 0: 910B"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_accelerator_status_reads_mocked_nvidia_gpu() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let mock = remote_root.path().join("nvidia-smi");
    let current_pid = std::process::id();
    write_mock_nvidia_smi(
        &mock,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 128, 81920, 7, P0, 101.50, 42'\n    printf '%s\\n' '1, NVIDIA A800, GPU-one, 0, 81920, 0, P8, 50.00, 35'\n    ;;\n  --query-compute-apps=*)\n    printf '%s\\n' 'GPU-zero, {current_pid}, 256'\n    ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )?;
    let mock_arg = mock.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &mock_arg],
    )?;

    let output = run_cli(&[
        "accelerator-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--kind",
        "gpu",
        "--gpus",
        "0",
        "--json",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["available"], true);
    assert_eq!(body["provider"], "nvidia");
    assert_eq!(body["devices"].as_array().unwrap().len(), 1);
    assert_eq!(body["devices"][0]["index"], 0);
    assert_eq!(body["processes"].as_array().unwrap().len(), 1);
    assert_eq!(body["processes"][0]["gpu_index"], 0);

    let text = run_cli(&[
        "gpu-status",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--text",
    ])?;
    assert!(text.status.success());
    let text_stdout = String::from_utf8_lossy(&text.stdout);
    assert!(text_stdout.contains("GPU accelerator provider: nvidia"));
    assert!(text_stdout.contains("GPU 0: NVIDIA A800"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_gpu_preflight_passes_when_selected_gpu_is_idle() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let mock = remote_root.path().join("nvidia-smi");
    write_mock_nvidia_smi(
        &mock,
        "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 0, 81920, 0, P8, 50.00, 35'\n    ;;\n  --query-compute-apps=*)\n    exit 0\n    ;;\n  *) exit 1 ;;\nesac\n",
    )?;
    let mock_arg = mock.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &mock_arg],
    )?;

    let output = run_cli(&[
        "gpu-preflight",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--max-memory-mib",
        "256",
        "--max-util-percent",
        "5",
        "--json",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["passed"], true);
    assert_eq!(body["blockers"].as_array().unwrap().len(), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_gpu_preflight_fails_with_threshold_and_forbidden_process_blockers() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let marker = remote_root.path().join("vllm-marker");
    std::os::unix::fs::symlink("/bin/sleep", &marker)?;
    let mut child = Command::new(&marker).arg("30").spawn()?;
    let child_pid = child.id();
    let mock = remote_root.path().join("nvidia-smi");
    write_mock_nvidia_smi(
        &mock,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 512, 81920, 9, P0, 101.50, 42'\n    ;;\n  --query-compute-apps=*)\n    printf '%s\\n' 'GPU-zero, {child_pid}, 512'\n    ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )?;
    let mock_arg = mock.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &mock_arg],
    )?;

    let output = run_cli(&[
        "gpu-preflight",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--max-memory-mib",
        "256",
        "--max-util-percent",
        "5",
        "--forbid-match",
        "sleep|vllm|xgl",
        "--json",
    ]);
    let _ = child.kill();
    let _ = child.wait();
    let output = output?;

    assert!(!output.status.success());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["passed"], false);
    let blocker_kinds = body["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|blocker| blocker["kind"].as_str())
        .collect::<Vec<_>>();
    assert!(blocker_kinds.contains(&"memory_above_threshold"));
    assert!(blocker_kinds.contains(&"utilization_above_threshold"));
    assert!(blocker_kinds.contains(&"forbidden_process"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("gpu-preflight failed"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_gpu_preflight_fails_when_gpu_is_unavailable() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let missing = remote_root.path().join("missing-nvidia-smi");
    let missing_arg = missing.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &missing_arg],
    )?;

    let output = run_cli(&[
        "gpu-preflight",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--json",
    ])?;
    assert!(!output.status.success());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["available"], false);
    assert_eq!(body["blockers"][0]["kind"], "accelerator_unavailable");
    assert!(
        body["agent_hint"]
            .as_str()
            .unwrap_or_default()
            .contains("Do not retry GPU status checks")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_gpu_preflight_reports_requested_gpu_missing_without_no_gpu_hint() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let mock = remote_root.path().join("nvidia-smi");
    write_mock_nvidia_smi(
        &mock,
        "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 0, 81920, 0, P8, 50.00, 35'\n    ;;\n  --query-compute-apps=*)\n    exit 0\n    ;;\n  *) exit 1 ;;\nesac\n",
    )?;
    let mock_arg = mock.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &mock_arg],
    )?;

    let output = run_cli(&[
        "gpu-preflight",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "99",
        "--json",
    ])?;
    assert!(!output.status.success());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["available"], true);
    assert_eq!(body["provider"], "nvidia");
    assert_eq!(body["blockers"].as_array().unwrap().len(), 1);
    assert_eq!(body["blockers"][0]["kind"], "gpu_missing");
    assert_eq!(body["blockers"][0]["gpu_index"], 99);
    assert!(
        body["blockers"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("requested but not reported")
    );
    assert!(
        !body["agent_hint"]
            .as_str()
            .unwrap_or_default()
            .contains("No GPU detected")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_gpu_wait_idle_waits_until_gpu_is_stably_under_threshold() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let mock = remote_root.path().join("nvidia-smi");
    let state_file = remote_root.path().join("nvidia-smi-count");
    write_mock_nvidia_smi(
        &mock,
        &format!(
            "#!/bin/sh\nSTATE='{state}'\ncase \"$1\" in\n  --query-gpu=*)\n    count=$(cat \"$STATE\" 2>/dev/null || echo 0)\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$STATE\"\n    if [ \"$count\" -lt 3 ]; then\n      printf '%s\\n' '0, NVIDIA A800, GPU-zero, 512, 81920, 9, P0, 101.50, 42'\n    else\n      printf '%s\\n' '0, NVIDIA A800, GPU-zero, 0, 81920, 0, P8, 50.00, 35'\n    fi\n    ;;\n  --query-compute-apps=*)\n    exit 0\n    ;;\n  *) exit 1 ;;\nesac\n",
            state = state_file.display()
        ),
    )?;
    let mock_arg = mock.display().to_string();
    let harness = CliServerHarness::start_with_args(
        remote_root.path(),
        token,
        &["--nvidia-smi-path", &mock_arg],
    )?;

    let output = run_cli(&[
        "gpu-wait-idle",
        "--server",
        &harness.base_url,
        "--token",
        token,
        "--gpus",
        "0",
        "--max-memory-mib",
        "256",
        "--max-util-percent",
        "5",
        "--stable-seconds",
        "1",
        "--timeout-seconds",
        "5",
        "--poll-ms",
        "100",
        "--json",
    ])?;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(body["passed"], true);
    assert_eq!(body["stable_seconds"], 1);
    assert!(body["observed_stable_seconds"].as_u64().unwrap_or(0) >= 1);
    Ok(())
}

#[tokio::test]
async fn process_cleanup_dry_run_attaches_gpu_occupancy_for_matched_pids() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    // Spawn a child whose command contains the marker so scan_cleanup_processes
    // can find it; long sleep so it survives until the assertion.
    let marker = format!("accel_cleanup_marker_{}", std::process::id());
    let mut child = Command::new("python3")
        .args(["-c", "import time; time.sleep(30)", &marker])
        .spawn()?;
    let child_pid = child.id() as i32;

    let result: Result<()> = async {
        // The child PID holds 256 MiB on device 0; a second compute process on
        // a different PID (999999) must be filtered out of the summary because
        // it is not in the matched set.
        let mock = remote_root.path().join("nvidia-smi");
        write_mock_nvidia_smi(
            &mock,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 128, 81920, 7, P0, 101.50, 42'\n    ;;\n  --query-compute-apps=*)\n    printf '%s\\n' 'GPU-zero, {child_pid}, 256'\n    printf '%s\\n' 'GPU-zero, 999999, 512'\n    ;;\n  *) exit 1 ;;\nesac\n"
            ),
        )?;
        let mut state = ServerState::new(
            "test-token".to_string(),
            vec![remote_root.path().to_path_buf()],
        );
        state.nvidia_smi_path = Some(mock);

        // Wait for the matcher to see the child.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let response = handle_process_cleanup(
                &state,
                ProcessCleanupRequest {
                    process_match: marker.clone(),
                    dry_run: true,
                    kill: false,
                    signal: None,
                    reconfirm: false,
                    reconfirm_wait_ms: None,
                    accelerator_summary: Some(AcceleratorKind::Gpu),
                },
            )
            .await?;
            if response
                .matched
                .iter()
                .any(|process| process.pid == child_pid)
            {
                assert!(response.dry_run);
                let summary = response
                    .accelerator_summary
                    .as_ref()
                    .expect("accelerator_summary should be attached");
                assert!(summary.available, "summary should be available");
                assert_eq!(summary.kind, AcceleratorKind::Gpu);
                // Only the child PID should appear; the 999999 PID must be filtered.
                let holding: Vec<_> = summary
                    .processes
                    .iter()
                    .map(|process| process.pid)
                    .collect();
                assert!(holding.contains(&child_pid), "child pid missing: {holding:?}");
                assert!(
                    !holding.contains(&999999),
                    "unmatched pid leaked into summary: {holding:?}"
                );
                let child_occ = summary
                    .processes
                    .iter()
                    .find(|process| process.pid == child_pid)
                    .expect("child occupancy present");
                assert_eq!(child_occ.device_index, Some(0));
                assert_eq!(child_occ.device_name.as_deref(), Some("NVIDIA A800"));
                assert_eq!(child_occ.used_memory_mib, Some(256));
                assert_eq!(child_occ.memory_total_mib, Some(81920));
                break;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("cleanup dry-run never matched child pid {child_pid}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
    .await;

    let _ = child.kill();
    let _ = child.wait();
    result
}

#[tokio::test]
async fn process_cleanup_dry_run_uses_default_gpu_smi_path() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let marker = format!("accel_cleanup_default_smi_{}", std::process::id());
    let mut child = Command::new("python3")
        .args(["-c", "import time; time.sleep(30)", &marker])
        .spawn()?;
    let child_pid = child.id() as i32;

    let result: Result<()> = async {
        let state = ServerState::new(
            "test-token".to_string(),
            vec![remote_root.path().to_path_buf()],
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let response = handle_process_cleanup(
                &state,
                ProcessCleanupRequest {
                    process_match: marker.clone(),
                    dry_run: true,
                    kill: false,
                    signal: None,
                    reconfirm: false,
                    reconfirm_wait_ms: None,
                    accelerator_summary: Some(AcceleratorKind::Gpu),
                },
            )
            .await?;
            if response
                .matched
                .iter()
                .any(|process| process.pid == child_pid)
            {
                let summary = response
                    .accelerator_summary
                    .as_ref()
                    .expect("accelerator_summary should be attached");
                assert_ne!(
                    summary.reason.as_deref(),
                    Some("nvidia-smi path not configured")
                );
                break;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("cleanup dry-run never matched child pid {child_pid}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
    .await;

    let _ = child.kill();
    let _ = child.wait();
    result
}

#[tokio::test]
async fn process_cleanup_dry_run_attaches_npu_occupancy_for_matched_pids() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let mut child = Command::new("python3")
        .args([
            "-c",
            "import time; time.sleep(30)",
            &format!("accel_cleanup_npu_{}", std::process::id()),
        ])
        .spawn()?;
    let child_pid = child.id() as i32;
    let marker = format!("accel_cleanup_npu_{}", std::process::id());

    let result: Result<()> = async {
        let mock = remote_root.path().join("npu-smi");
        write_mock_executable(
            &mock,
            &format!(
                "#!/bin/sh\ncat <<'INFO'\n+------------------------------------------------------------------------------------------------+\n| NPU     Name      | Health          | Power(W)     Temp(C)           Hugepages-Usage(page)     |\n| Chip    Device    | Bus-Id          | AICore(%)    Memory-Usage(MB)                             |\n+===================+=================+==========================================================+\n| 0       910B      | OK              | 91.8         42                0    / 0                   |\n| 0       0         | 0000:C1:00.0    | 5            1024 / 65536                                 |\n+-------------------+-----------------+----------------------------------------------------------+\n| NPU     Chip      | Process id      | Process name             | Process memory(MB)      |\n+===================+=================+==========================================================+\n| 0       0         | {child_pid}   | agentplane               | 2048                    |\nINFO\n"
            ),
        )?;
        let mut state = ServerState::new(
            "test-token".to_string(),
            vec![remote_root.path().to_path_buf()],
        );
        state.npu_smi_path = Some(mock);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let response = handle_process_cleanup(
                &state,
                ProcessCleanupRequest {
                    process_match: marker.clone(),
                    dry_run: true,
                    kill: false,
                    signal: None,
                    reconfirm: false,
                    reconfirm_wait_ms: None,
                    accelerator_summary: Some(AcceleratorKind::Npu),
                },
            )
            .await?;
            if response
                .matched
                .iter()
                .any(|process| process.pid == child_pid)
            {
                let summary = response
                    .accelerator_summary
                    .as_ref()
                    .expect("npu summary should be attached");
                assert!(summary.available);
                assert_eq!(summary.kind, AcceleratorKind::Npu);
                assert_eq!(summary.processes.len(), 1);
                assert_eq!(summary.processes[0].pid, child_pid);
                assert_eq!(summary.processes[0].device_index, Some(0));
                assert_eq!(summary.processes[0].device_name.as_deref(), Some("910B"));
                assert_eq!(summary.processes[0].used_memory_mib, Some(2048));
                assert_eq!(summary.processes[0].memory_total_mib, Some(65536));
                break;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("cleanup dry-run never matched child pid {child_pid}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
    .await;

    let _ = child.kill();
    let _ = child.wait();
    result
}

#[tokio::test]
async fn process_cleanup_dry_run_omits_summary_by_default() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let mock = remote_root.path().join("nvidia-smi");
    write_mock_nvidia_smi(&mock, "#!/bin/sh\nexit 0\n")?;
    let mut state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    state.nvidia_smi_path = Some(mock);

    let response = handle_process_cleanup(
        &state,
        ProcessCleanupRequest {
            process_match: "definitely-not-running-zzz".to_string(),
            dry_run: true,
            kill: false,
            signal: None,
            reconfirm: false,
            reconfirm_wait_ms: None,
            accelerator_summary: None,
        },
    )
    .await?;
    assert!(response.dry_run);
    // No summary requested -> field must be absent (None), not an empty object.
    assert!(response.accelerator_summary.is_none());
    Ok(())
}

#[tokio::test]
async fn process_cleanup_dry_run_skips_smi_when_no_processes_match() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let invoked = remote_root.path().join("smi-invoked");
    let mock = remote_root.path().join("nvidia-smi");
    write_mock_nvidia_smi(
        &mock,
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", invoked.display()),
    )?;
    let mut state = ServerState::new(
        "test-token".to_string(),
        vec![remote_root.path().to_path_buf()],
    );
    state.nvidia_smi_path = Some(mock);

    let response = handle_process_cleanup(
        &state,
        ProcessCleanupRequest {
            process_match: "definitely-not-running-zzz".to_string(),
            dry_run: true,
            kill: false,
            signal: None,
            reconfirm: false,
            reconfirm_wait_ms: None,
            accelerator_summary: Some(AcceleratorKind::Gpu),
        },
    )
    .await?;
    let summary = response
        .accelerator_summary
        .as_ref()
        .expect("summary should be attached for an empty match set");
    assert!(summary.available);
    assert!(summary.reason.is_none());
    assert!(summary.processes.is_empty());
    assert!(
        !invoked.exists(),
        "nvidia-smi should not run for no matches"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_process_cleanup_reports_gpu_occupancy_in_json_and_text() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let marker = format!("cli_accel_cleanup_{}", std::process::id());
    let mut child = Command::new("python3")
        .args(["-c", "import time; time.sleep(30)", &marker])
        .spawn()?;
    let child_pid = child.id() as i64;

    let result = (|| -> Result<()> {
        let mock = remote_root.path().join("nvidia-smi");
        write_mock_nvidia_smi(
            &mock,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  --query-gpu=*)\n    printf '%s\\n' '0, NVIDIA A800, GPU-zero, 128, 81920, 7, P0, 101.50, 42'\n    ;;\n  --query-compute-apps=*)\n    printf '%s\\n' 'GPU-zero, {child_pid}, 256'\n    ;;\n  *) exit 1 ;;\nesac\n"
            ),
        )?;
        let mock_arg = mock.display().to_string();
        let harness = CliServerHarness::start_with_args(
            remote_root.path(),
            token,
            &["--nvidia-smi-path", &mock_arg],
        )?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let body = loop {
            let output = run_cli(&[
                "process-cleanup",
                "--server",
                &harness.base_url,
                "--token",
                token,
                "--match",
                &marker,
                "--dry-run",
                "--accelerator-summary",
                "gpu",
                "--json",
            ])?;
            assert!(
                output.status.success(),
                "stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if body["matched"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|process| process["pid"].as_i64() == Some(child_pid))
            {
                break body;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("cleanup CLI never matched child pid {child_pid}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        assert_eq!(body["accelerator_summary"]["available"], true);
        assert_eq!(
            body["accelerator_summary"]["processes"][0]["pid"],
            child_pid
        );
        assert_eq!(
            body["accelerator_summary"]["processes"][0]["device_name"],
            "NVIDIA A800"
        );
        assert_eq!(
            body["accelerator_summary"]["processes"][0]["used_memory_mib"],
            256
        );
        assert_eq!(
            body["accelerator_summary"]["processes"][0]["memory_total_mib"],
            81920
        );

        let text = run_cli(&[
            "process-cleanup",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--match",
            &marker,
            "--dry-run",
            "--accelerator-summary",
            "gpu",
            "--text",
        ])?;
        assert!(text.status.success());
        let stdout = String::from_utf8(text.stdout)?;
        assert!(stdout.contains("Accelerator summary: GPU, 1 process(es)"));
        assert!(stdout.contains(&format!("pid={child_pid} device=0")));
        assert!(stdout.contains("device_name=NVIDIA A800"));
        assert!(stdout.contains("used_memory=256 MiB total_memory=81920 MiB"));
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(unix)]
#[test]
fn cli_process_cleanup_warns_when_gpu_summary_is_unavailable() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let token = "test-token";
    let marker = format!("cli_accel_unavailable_{}", std::process::id());
    let mut child = Command::new("python3")
        .args(["-c", "import time; time.sleep(30)", &marker])
        .spawn()?;
    let child_pid = child.id() as i64;

    let result = (|| -> Result<()> {
        let missing = remote_root.path().join("missing-nvidia-smi");
        let missing_arg = missing.display().to_string();
        let harness = CliServerHarness::start_with_args(
            remote_root.path(),
            token,
            &["--nvidia-smi-path", &missing_arg],
        )?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let body = loop {
            let output = run_cli(&[
                "process-cleanup",
                "--server",
                &harness.base_url,
                "--token",
                token,
                "--match",
                &marker,
                "--dry-run",
                "--accelerator-summary",
                "gpu",
                "--json",
            ])?;
            assert!(output.status.success());
            let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            if body["matched"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|process| process["pid"].as_i64() == Some(child_pid))
            {
                break body;
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("cleanup CLI never matched child pid {child_pid}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };

        assert_eq!(body["accelerator_summary"]["available"], false);
        assert!(
            body["agent_hint"]
                .as_str()
                .unwrap_or_default()
                .contains("WARNING: Accelerator status unavailable")
        );

        let text = run_cli(&[
            "process-cleanup",
            "--server",
            &harness.base_url,
            "--token",
            token,
            "--match",
            &marker,
            "--dry-run",
            "--accelerator-summary",
            "gpu",
            "--text",
        ])?;
        assert!(text.status.success());
        let stdout = String::from_utf8(text.stdout)?;
        assert!(stdout.contains("Accelerator summary: GPU unavailable"));
        assert!(stdout.contains("WARNING: Accelerator status unavailable"));
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    result
}
