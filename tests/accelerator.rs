mod common;

use std::process::Command;

use agentplane::protocol::{AcceleratorKind, AcceleratorStatusRequest};
use agentplane::server::{ServerState, handle_accelerator_status};
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
