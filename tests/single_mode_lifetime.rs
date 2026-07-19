mod common;

use std::process::{Command, Output};
use std::time::{Duration, Instant};

use common::*;

fn assert_success(output: &Output, operation: &str) -> Result<()> {
    anyhow::ensure!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn assert_server_alive(
    harness: &mut CliServerHarness,
    server_pid: u32,
    profile: &str,
    checkpoint: &str,
) -> Result<()> {
    anyhow::ensure!(
        harness.process.try_wait()?.is_none(),
        "AgentPlane server {server_pid} exited after {checkpoint}"
    );
    let health = run_cli(&["--profile", profile, "health"])?;
    assert_success(&health, &format!("health after {checkpoint}"))?;
    let mode = run_cli(&["--profile", profile, "mode-get"])?;
    assert_success(&mode, &format!("mode-get after {checkpoint}"))?;
    let body: serde_json::Value = serde_json::from_slice(&mode.stdout)?;
    anyhow::ensure!(
        body["current_mode"] == "single",
        "server left single mode after {checkpoint}: {body}"
    );
    Ok(())
}

fn wait_for_path(path: &std::path::Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::bail!("path did not appear: {}", path.display())
}

#[cfg(unix)]
fn process_group_id(pid: u32) -> Result<i32> {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()?;
    assert_success(&output, "ps process group lookup")?;
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}

#[cfg(unix)]
#[test]
fn single_mode_capabilities_do_not_exit_the_server() -> Result<()> {
    let remote_root = tempfile::tempdir()?;
    let local_dir = tempfile::tempdir()?;
    let profile_dir = tempfile::tempdir()?;
    let run_dir = tempfile::tempdir()?;
    let token = "single-mode-lifetime-token";
    let mut harness = CliServerHarness::start(remote_root.path(), token)?;
    let server_pid = harness.process.id();
    let server = harness.base_url.clone();
    let root = remote_root.path().display().to_string();
    let profile_path = profile_dir.path().join("single.env");
    std::fs::write(
        &profile_path,
        format!(
            "AP_SERVER={server}\nAP_TOKEN={token}\nAP_REMOTE_ROOT={root}\nAP_LABEL=single-lifetime\n"
        ),
    )?;
    let profile = profile_path.display().to_string();

    assert_server_alive(&mut harness, server_pid, &profile, "startup")?;
    let mode_switch = run_cli(&["--profile", &profile, "mode-switch", "--mode", "single"])?;
    assert_success(&mode_switch, "mode-switch single")?;
    assert_server_alive(&mut harness, server_pid, &profile, "single mode operations")?;

    let process_group_check = format!(
        "server_pgid=$(ps -o pgid= -p {server_pid} | tr -d ' '); child_pgid=$(ps -o pgid= -p $$ | tr -d ' '); test \"$server_pgid\" != \"$child_pgid\"; printf 'process-run-ok\\n'"
    );
    let process_run = run_cli_with_env(
        &[
            "--profile",
            &profile,
            "process-run",
            "--process-id",
            "single-lifetime-run",
            "--run-id",
            "single-lifetime",
            "--",
            "sh",
            "-c",
            &process_group_check,
        ],
        &[("AP_PROCESS_TRANSPORT", "websocket")],
    )?;
    assert_success(&process_run, "process-run websocket")?;

    let term_marker = remote_root.path().join("managed-term-received");
    let ready_marker = remote_root.path().join("managed-term-ready");
    let term_script = r#"
import pathlib
import signal
import sys
import time

term_path = pathlib.Path(sys.argv[1])
ready_path = pathlib.Path(sys.argv[2])

def handle_term(_signum, _frame):
    term_path.write_text("term")
    raise SystemExit(0)

signal.signal(signal.SIGTERM, handle_term)
ready_path.write_text("ready")
while True:
    time.sleep(0.05)
"#;
    let process_start = run_cli(&[
        "--profile",
        &profile,
        "process-start",
        "--process-id",
        "single-lifetime-managed",
        "--run-id",
        "single-lifetime",
        "--",
        "python3",
        "-c",
        term_script,
        &term_marker.display().to_string(),
        &ready_marker.display().to_string(),
    ])?;
    assert_success(&process_start, "process-start")?;
    wait_for_path(&ready_marker)?;
    for command in ["process-get", "process-list", "process-status"] {
        let output = if command == "process-list" {
            run_cli(&["--profile", &profile, command])?
        } else {
            run_cli(&[
                "--profile",
                &profile,
                command,
                "--process-id",
                "single-lifetime-managed",
            ])?
        };
        assert_success(&output, command)?;
        if command == "process-status" {
            let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            let managed_pid = body["process"]["pid"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("process-status did not return a PID: {body}"))?;
            anyhow::ensure!(
                process_group_id(server_pid)? != process_group_id(managed_pid as u32)?,
                "managed process inherited the AgentPlane server process group"
            );
        }
    }
    let terminate = run_cli(&[
        "--profile",
        &profile,
        "process-terminate",
        "--process-id",
        "single-lifetime-managed",
    ])?;
    assert_success(&terminate, "process-terminate")?;
    wait_for_path(&term_marker)?;
    assert_server_alive(&mut harness, server_pid, &profile, "process operations")?;

    let cleanup_marker = format!("single_lifetime_cleanup_{}", std::process::id());
    let mut cleanup_target = Command::new("sh")
        .args([
            "-c",
            "trap 'exit 0' TERM; while :; do sleep 1; done",
            &cleanup_marker,
        ])
        .spawn()?;
    let cleanup_result = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let dry_run = run_cli(&[
                "--profile",
                &profile,
                "process-cleanup",
                "--match",
                &cleanup_marker,
                "--dry-run",
            ])?;
            assert_success(&dry_run, "process-cleanup dry-run")?;
            let body: serde_json::Value = serde_json::from_slice(&dry_run.stdout)?;
            if body["matched"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item["pid"].as_u64() == Some(u64::from(cleanup_target.id())))
            }) {
                break;
            }
            if Instant::now() >= deadline {
                anyhow::bail!("cleanup target was not matched");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let killed = run_cli(&[
            "--profile",
            &profile,
            "process-cleanup",
            "--match",
            &cleanup_marker,
            "--kill",
            "--signal",
            "TERM",
        ])?;
        assert_success(&killed, "process-cleanup TERM")?;
        let body: serde_json::Value = serde_json::from_slice(&killed.stdout)?;
        anyhow::ensure!(body["verified"] == true, "cleanup was not verified: {body}");
        wait_for_child_exit(&mut cleanup_target)?;
        Ok(())
    })();
    if cleanup_result.is_err() {
        let _ = cleanup_target.kill();
        let _ = cleanup_target.wait();
    }
    cleanup_result?;

    let listen_port = server
        .rsplit(':')
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing listen port in {server}"))?;
    let self_match = format!("--port {listen_port} --allow-root");
    let self_match_arg = format!("--match={self_match}");
    let self_cleanup = run_cli(&[
        "--profile",
        &profile,
        "process-cleanup",
        &self_match_arg,
        "--kill",
        "--signal",
        "TERM",
    ])?;
    assert_success(&self_cleanup, "process-cleanup server self-protection")?;
    let body: serde_json::Value = serde_json::from_slice(&self_cleanup.stdout)?;
    anyhow::ensure!(
        body["signaled"].as_array().is_some_and(Vec::is_empty),
        "server self-cleanup signaled a process: {body}"
    );
    anyhow::ensure!(
        body["skipped"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["pid"].as_u64() == Some(u64::from(server_pid)))),
        "server PID was not explicitly skipped: {body}"
    );
    assert_server_alive(&mut harness, server_pid, &profile, "cleanup operations")?;

    let file_write = run_cli(&[
        "--profile",
        &profile,
        "file-write",
        "--path",
        "lifetime/input.txt",
        "--content",
        "single-mode-file",
        "--atomic",
    ])?;
    assert_success(&file_write, "file-write")?;
    for args in [
        vec!["file-read", "--path", "lifetime/input.txt", "--text"],
        vec!["file-stat", "--path", "lifetime/input.txt"],
        vec!["file-list", "--path", "lifetime"],
        vec!["file-find", "--pattern", "input.txt"],
        vec![
            "file-wait",
            "--path",
            "lifetime/input.txt",
            "--min-bytes",
            "1",
            "--stable-ms",
            "0",
            "--timeout-seconds",
            "2",
        ],
    ] {
        let mut command = vec!["--profile", profile.as_str()];
        command.extend(args);
        let output = run_cli(&command)?;
        assert_success(&output, command[2])?;
    }

    let local_upload = local_dir.path().join("upload.bin");
    std::fs::write(&local_upload, b"binary-upload-content")?;
    let local_upload_arg = local_upload.display().to_string();
    let upload = run_cli_with_env(
        &[
            "--profile",
            &profile,
            "file-upload",
            "--path",
            "lifetime/upload.bin",
            "--from-local",
            &local_upload_arg,
            "--chunk-size",
            "5",
            "--atomic",
        ],
        &[("AP_UPLOAD_TRANSPORT", "binary")],
    )?;
    assert_success(&upload, "file-upload binary")?;
    let copy = run_cli(&[
        "file-copy",
        "--from-profile",
        &profile,
        "--from-path",
        "lifetime/upload.bin",
        "--to-profile",
        &profile,
        "--to-path",
        "lifetime/copied.bin",
        "--chunk-size",
        "4",
        "--checksum",
    ])?;
    assert_success(&copy, "file-copy")?;
    let delete = run_cli(&[
        "--profile",
        &profile,
        "file-delete",
        "--path",
        "lifetime/input.txt",
    ])?;
    assert_success(&delete, "file-delete")?;
    assert_server_alive(&mut harness, server_pid, &profile, "file operations")?;

    let repo = init_repo()?;
    std::fs::write(repo.path().join("sync.txt"), "single-mode-sync\n")?;
    git(repo.path(), &["add", "sync.txt"])?;
    git(repo.path(), &["commit", "-m", "single mode lifetime"])?;
    let repo_arg = repo.path().display().to_string();
    let sync_root = remote_root.path().join("sync-root").display().to_string();
    let sync_init = run_cli(&[
        "--profile",
        &profile,
        "sync-init",
        "--repo",
        &repo_arg,
        "--remote-root",
        &sync_root,
        "--checksum",
    ])?;
    assert_success(&sync_init, "sync-init")?;
    let sync_group_check = format!(
        "test \"$(cat sync.txt)\" = single-mode-sync; server_pgid=$(ps -o pgid= -p {server_pid} | tr -d ' '); child_pgid=$(ps -o pgid= -p $$ | tr -d ' '); test \"$server_pgid\" != \"$child_pgid\""
    );
    let sync_run = run_cli(&[
        "--profile",
        &profile,
        "sync-run",
        "--repo",
        &repo_arg,
        "--remote-root",
        &sync_root,
        "--checksum",
        "--command",
        &sync_group_check,
    ])?;
    assert_success(&sync_run, "sync-run")?;
    assert_server_alive(&mut harness, server_pid, &profile, "sync operations")?;

    for args in [
        vec!["accelerator-status", "--kind", "gpu"],
        vec!["accelerator-status", "--kind", "npu"],
        vec!["gpu-status"],
        vec!["npu-status"],
    ] {
        let mut command = vec!["--profile", profile.as_str()];
        command.extend(args);
        let output = run_cli(&command)?;
        assert_success(&output, command[2])?;
    }
    for args in [
        vec!["accelerator-preflight", "--kind", "gpu"],
        vec!["gpu-preflight"],
        vec![
            "accelerator-wait-idle",
            "--kind",
            "gpu",
            "--stable-seconds",
            "0",
            "--timeout-seconds",
            "1",
            "--poll-ms",
            "10",
        ],
        vec![
            "gpu-wait-idle",
            "--stable-seconds",
            "0",
            "--timeout-seconds",
            "1",
            "--poll-ms",
            "10",
        ],
    ] {
        let mut command = vec!["--profile", profile.as_str()];
        command.extend(args);
        let output = run_cli(&command)?;
        anyhow::ensure!(
            output.status.code().is_some(),
            "{} did not terminate normally",
            command[2]
        );
        assert_server_alive(&mut harness, server_pid, &profile, command[2])?;
    }

    let run_dir_arg = run_dir.path().display().to_string();
    let run_show = Command::new(build_binary()?)
        .args([
            "run-show",
            "single-lifetime",
            "--profile",
            &profile,
            "--rebuild",
        ])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert_success(&run_show, "run-show")?;
    let run_manifest = Command::new(build_binary()?)
        .args(["run-manifest", "single-lifetime", "--profile", &profile])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert_success(&run_manifest, "run-manifest")?;
    assert_server_alive(&mut harness, server_pid, &profile, "run aggregation")?;

    Ok(())
}
