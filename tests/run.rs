mod common;

use common::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RunManifest {
    run_id: String,
    nodes: Vec<RunNode>,
}

#[derive(Debug, Deserialize)]
struct RunNode {
    label: Option<String>,
    server: String,
    profile: Option<std::path::PathBuf>,
    processes: Vec<serde_json::Value>,
}

fn write_profile(
    path: &std::path::Path,
    server: &str,
    token: &str,
    remote_root: &std::path::Path,
    label: &str,
) -> anyhow::Result<()> {
    std::fs::write(
        path,
        format!(
            "AP_SERVER={server}\nAP_TOKEN={token}\nAP_REMOTE_ROOT={root}\nAP_LABEL={label}\n",
            root = remote_root.display()
        ),
    )?;
    Ok(())
}

#[test]
fn cli_run_show_aggregates_across_two_profiles() -> Result<()> {
    let token = "test-token";
    let root_a = tempfile::tempdir()?;
    let root_b = tempfile::tempdir()?;
    let profiles = tempfile::tempdir()?;
    // Isolate the manifest cache so the test does not touch the user's home.
    let run_dir = tempfile::tempdir()?;
    let run_dir_arg = run_dir.path().display().to_string();

    let harness_a = CliServerHarness::start(root_a.path(), token)?;
    let harness_b = CliServerHarness::start(root_b.path(), token)?;
    let profile_a = profiles.path().join("node14.env");
    let profile_b = profiles.path().join("node13.env");
    write_profile(
        &profile_a,
        &harness_a.base_url,
        token,
        root_a.path(),
        "node14",
    )?;
    write_profile(
        &profile_b,
        &harness_b.base_url,
        token,
        root_b.path(),
        "node13",
    )?;

    // Start one process per node with the same run_id.
    let started_a = run_cli(&[
        "--profile",
        &profile_a.display().to_string(),
        "process-start",
        "--process-id",
        "run42-producer",
        "--run-id",
        "run42",
        "--save-output-path",
        "runs/run42/producer.log",
        "--output-bytes-limit",
        "1048576",
        "--",
        "bash",
        "-lc",
        "echo producer && sleep 30",
    ])?;
    assert!(
        started_a.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&started_a.stderr)
    );
    let started_b = run_cli(&[
        "--profile",
        &profile_b.display().to_string(),
        "process-start",
        "--process-id",
        "run42-consumer",
        "--run-id",
        "run42",
        "--save-output-path",
        "runs/run42/consumer.log",
        "--output-bytes-limit",
        "1048576",
        "--",
        "bash",
        "-lc",
        "echo consumer && sleep 30",
    ])?;
    assert!(started_b.status.success());

    let show = std::process::Command::new(common::build_binary()?)
        .args([
            "run-show",
            "run42",
            "--profile",
            &profile_a.display().to_string(),
            "--profile",
            &profile_b.display().to_string(),
        ])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert!(
        show.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let manifest: RunManifest = serde_json::from_slice(&show.stdout)?;
    assert_eq!(manifest.run_id, "run42");
    assert_eq!(manifest.nodes.len(), 2, "should aggregate two nodes");

    // Each node should report exactly its own process, joined with save_output_path.
    let mut labels = manifest
        .nodes
        .iter()
        .map(|node| (node.label.as_deref().unwrap_or("?"), node.processes.len()))
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, vec![("node13", 1), ("node14", 1)]);

    for node in &manifest.nodes {
        assert_eq!(node.processes.len(), 1);
        assert!(!node.server.is_empty(), "server should be recorded");
        let proc = &node.processes[0];
        let pid = proc["process_id"].as_str().unwrap_or_default();
        assert!(pid.starts_with("run42-"), "process_id={pid}");
        assert_eq!(proc["run_id"], "run42");
        let save = proc["save_output_path"].as_str().unwrap_or_default();
        assert!(save.starts_with("runs/run42/"), "save_output_path={save}");
        // The manifest must record which profile produced each node.
        assert!(node.profile.is_some(), "profile path should be recorded");
    }

    // The manifest cache file should now exist.
    let cache = run_dir.path().join("run42.json");
    assert!(cache.exists(), "manifest cache should be written");

    // run-manifest emits the same JSON (no --profile: uses the cache).
    let manifest_cmd = std::process::Command::new(common::build_binary()?)
        .args(["run-manifest", "run42"])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert!(
        manifest_cmd.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&manifest_cmd.stderr)
    );
    let manifest2: RunManifest = serde_json::from_slice(&manifest_cmd.stdout)?;
    assert_eq!(manifest2.run_id, "run42");
    assert_eq!(manifest2.nodes.len(), 2);

    // run-show --text prints a readable per-node table.
    let text_cmd = std::process::Command::new(common::build_binary()?)
        .args(["run-show", "run42", "--text"])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert!(text_cmd.status.success());
    let text = String::from_utf8(text_cmd.stdout)?;
    assert!(text.contains("run run42"), "text: {text}");
    assert!(
        text.contains("[node13]") && text.contains("[node14]"),
        "text: {text}"
    );
    assert!(
        text.contains("run42-producer") && text.contains("run42-consumer"),
        "text: {text}"
    );

    // --rebuild reconstructs from server state, ignoring the cache.
    let rebuild_cmd = std::process::Command::new(common::build_binary()?)
        .args([
            "run-show",
            "run42",
            "--rebuild",
            "--profile",
            &profile_a.display().to_string(),
            "--profile",
            &profile_b.display().to_string(),
        ])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert!(rebuild_cmd.status.success());
    let manifest3: RunManifest = serde_json::from_slice(&rebuild_cmd.stdout)?;
    assert_eq!(manifest3.nodes.len(), 2);

    // Cleanup.
    let _ = run_cli(&[
        "--profile",
        &profile_a.display().to_string(),
        "process-terminate",
        "--process-id",
        "run42-producer",
    ])?;
    let _ = run_cli(&[
        "--profile",
        &profile_b.display().to_string(),
        "process-terminate",
        "--process-id",
        "run42-consumer",
    ])?;
    let _ = root_a;
    let _ = root_b;
    Ok(())
}

#[test]
fn cli_run_show_empty_run_is_valid_not_an_error() -> Result<()> {
    let token = "test-token";
    let root_a = tempfile::tempdir()?;
    let profiles = tempfile::tempdir()?;
    let run_dir = tempfile::tempdir()?;
    let run_dir_arg = run_dir.path().display().to_string();

    let harness = CliServerHarness::start(root_a.path(), token)?;
    let profile = profiles.path().join("node14.env");
    write_profile(&profile, &harness.base_url, token, root_a.path(), "node14")?;

    // A run_id nobody started -> empty manifest, not an error.
    let show = std::process::Command::new(common::build_binary()?)
        .args([
            "--profile",
            &profile.display().to_string(),
            "run-show",
            "nonexistent-run",
        ])
        .env("AP_RUN_DIR", &run_dir_arg)
        .output()?;
    assert!(
        show.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let manifest: RunManifest = serde_json::from_slice(&show.stdout)?;
    assert_eq!(manifest.run_id, "nonexistent-run");
    assert_eq!(manifest.nodes.len(), 1);
    assert!(manifest.nodes[0].processes.is_empty());
    Ok(())
}
